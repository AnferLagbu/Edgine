# aarch64 SMP bring-up 工程（AP 上线路径）

> 本文件是 aarch64 架构 **SMP 次核（AP）上线路径缺失** 的独立工程文档，与 [smp-ipi-protocol.md](./smp-ipi-protocol.md)、[tlb-shootdown-epoch.md](./tlb-shootdown-epoch.md) 同族。
>
> 目标：为 aarch64 补齐 PSCI `CPU_ON` 次核上电 → 次核入口 → GIC 次核初始化 → per-CPU 状态登记 → idle 的完整链路，使 `SMP_ENABLED` 在 aarch64 上可置真、`CPU_COUNT > 1`，并使既有 SGI（TLB shootdown / reschedule）在真多核下有可响应的核。
>
> 关联裁定：DECISION-082（本文件）。关联登记：[unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) ISSUE-RT-002（aarch64 真多核 GIC 压测待本工程落地后补）。
>
> 实现路径：**方案 B（相对完整）** —— 用户裁定（见 DECISION-082 裁定 1）。

## 1. 根因与性质

**根因**：aarch64 架构**根本不存在 AP 上线路径**，SMP 抽象层（`privileged/smp/mod.rs`，双架构共享）在 aarch64 上从未被激活。具体缺口有四处，层层递进：

1. **无 PSCI 上电原语**：`privileged/arch/aarch64/psci.rs` 仅有 `PSCI_SYSTEM_OFF` / `PSCI_SYSTEM_RESET` / `PSCI_VERSION` 三个函数 ID，**无 `PSCI_CPU_ON`**；且唯一底层封装 [`smc()`](../../src/kernel/privileged/arch/aarch64/psci.rs#L22-L33) 只传 1 个寄存器（x0=func），而 `CPU_ON` 需要 x0=func / x1=target_cpu / x2=entry_pa / x3=context_id 四个入参。
2. **无次核汇编入口**：`boot/aarch64/start.S` 是**纯单核引导**——仅 `_start`（BSP 入口）一条从 EL3→EL2→EL1→`entry()` 的路径，无任何次核分派。
3. **无次核 Rust 入口**：`arch/aarch64/` 下无 `smp_init.rs`（x86_64 的对应物在 `arch/x86_64/smp_init.rs`，含 trampoline 拷贝、`start_ap`、`ap_entry`）。
4. **连 BSP 都未接 `smp::init()`**：`arch/aarch64/mod.rs` 的 [`interrupt_early_init()`](../../src/kernel/privileged/arch/aarch64/mod.rs#L238-L240) 与 [`interrupt_late_init()`](../../src/kernel/privileged/arch/aarch64/mod.rs#L242-L244) **均为空函数**（注释称「GICv3 + VBAR_EL1 已由 entry.rs / bootloader 配置」）。对比 x86_64 的 [`interrupt_late_init()`](../../src/kernel/privileged/arch/x86_64/mod.rs#L324-L395) 末尾调用 `smp::init()` + `smp_init::init()`，aarch64 侧完全缺失该接线。

**复合后果**：`smp::init()` 从未被调用 ⇒ `BSP_ID` / `CPU_ONLINE[0]` / `CPU_COUNT` 从未被正确初始化（`CPU_COUNT` 恒为初值 1）；`smp::register_cpu()` 无调用者 ⇒ `SMP_ENABLED` 恒 false。因此 [smp-ipi-protocol.md](./smp-ipi-protocol.md) §2.6 明确登记「**无 AP 上线路径**」，其 §5.3 的 aarch64 SGI 处理分支（`exception.rs` 中 SGI 7/13/14）**仅编译验证、无运行验证载体**。

**性质判定**：**能力级缺口，非缺陷修复**。aarch64 当前单核语义"自洽"（所有依赖 `CPU_COUNT` 的路径在单核下退化为恒等操作），故本工程是**新增能力**而非修 bug；但其缺失使 aarch64 与 x86_64 的 SMP 抽象层长期处于"一半活、一半死"的分叉状态，且 ISSUE-RT-002（GIC 多核压测）等条目被该缺口阻塞。

**关键认知（范围边界）**：本工程 bring-up 范围 = **次核成功上线 + 既有 IPI 可达**。**不含**aarch64 侧 TLB shootdown 的**发送路径**实装——`exception.rs` 的 SGI 13 **响应**分支（`smp::tlb_catch_up_local()` + `tlb_probe_report()`）已存在，但 aarch64 无「本核修改页表后广播 SGI 13 给其他核」的发送点（x86_64 有 `smp::broadcast_tlb_invalidate()` 的调用面）。发送路径属独立工程，本文件仅在「风险与回退」登记其未覆盖事实。

## 2. 实证结论（源码复核，全部只读）

| # | 命题 | 复核结论 |
|---|---|---|
| E1 | aarch64 无 AP 上线路径 | **成立**。四处缺口见 §1（无 CPU_ON / 无次核汇编入口 / 无 `smp_init.rs` / BSP 未接 `smp::init()`） |
| E2 | SMP 抽象层可复用（双架构共享） | **成立**。`privileged/smp/mod.rs` 的 `init()` / `register_cpu()` / IPI 发送 / 代计数 / 探针全套与架构无关，仅 `arch!(cpu_id())` 经 `CoreArch` 抽象（aarch64 = `MPIDR_EL1` Aff0） |
| E3 | SGI 响应分发已存在 | **成立（但分发不等于可响应，见订正）**。`exception.rs::irq_handler()` 已分发 SGI 7（FREG恢复）/ SGI 13（TLB）/ SGI 14（reschedule）。订正（DECISION-083 施工期实证）：分发分支存在**并不等于** SGI 可达 —— SGI 13/14 从未在 per-CPU `GICR_ISENABLER0` 使能，接收侧 `intid == 13` 分支永不触发；且发送侧目标编码有误。二者由 DECISION-083 ST-09 修复（使能收敛到 `gic::init_per_cpu` + 修正 `ICC_SGI1R_EL1` 目标位） |
| E4 | GIC 初始化函数可参数化复用 | **成立但需改造**。`gic.rs` 的 `init_distributor()` / `init_redistributor()` / `init_cpu_interface()` / `enable_timer_ppi()` 已拆分良好，但 `init_redistributor()` **仅作用于当前核单一 `GICR_BASE` 静态量**（未按 CPU 号偏移），次核需按 `GICR stride = 128 KiB` 定位自己的 RD/SGI frame |
| E5 | BSP 运行时页表可供次核复用 | **成立**。`mmu.rs` 的运行时根表 [`L0_TABLE`](../../src/kernel/privileged/arch/aarch64/mmu.rs#L49) 链接于高半区，`virt_to_phys(L0_TABLE...)` 可得其**物理基址**（[`alloc_user_page_table()`](../../src/kernel/privileged/arch/aarch64/mmu.rs#L211-L217) 即此形态）。`start.S` 的引导页表 `_boot_l0`（`.bootbss`，VMA==LMA==PA）与运行时表映射等价 ⇒ 次核可复用 BSP 已建好的运行时表物理基址，**跳过 `mmu::init()`** |
| E6 | 次核 MPIDR 目标来源需新增 | **成立**。`privileged/dtb.rs` 的 [`DtbInfo`](../../src/kernel/privileged/dtb.rs#L62-L74) 仅含 memory / uart / gic_dist / gic_redist，**无 CPU 节点枚举能力**。次核目标 MPIDR 须新增 DTB `/cpus/cpu@N/reg` 解析，或回退 QEMU virt 约定 `mpidr = cpu_index`（Aff0 = 索引） |
| E7 | 次核所在异常级（EL）为假设项 | **已实证（SMP-03）**。`-smp 2` QEMU 实测次核经 `ap_entry_asm`（MMU 关态、直接装 TTBR/开 MMU）成功上线（`online CPUs: 2`，且未触发"responded but did not register"诊断分支）⇒ 次核进入级为 **EL1**（PSCI 已替其完成 EL 降级），**无需** EL2→EL1 降级序列 |
| E8 | 现有 QEMU 集成未启用多核 | **成立**。`scripts/qemu_boot_test.sh` aarch64 分支（[L228-L229](../../scripts/qemu_boot_test.sh#L228-L229)）的 `qemu-system-aarch64` 命令行**无 `-smp`**，默认单核 ⇒ 本工程需显式加 `-smp 2` 方可验证 |
| E9 | 编号可用 | **成立**。现存最新裁定为 DECISION-081（[framekernel-bench-measure-fix.md](./framekernel-bench-measure-fix.md)），本工程取 **DECISION-082** |
| E10 | 初始化时序支持接线 | **成立**。`lib.rs`（[L750](../../src/kernel/lib.rs#L750)）的 `interrupt_late_init()` 在 [`scheduler::init()`](../../src/kernel/lib.rs#L770-L771) **之前**调用；x86_64 亦然，且 AP 的 per-CPU 调度器初始化（`init_per_cpu_sched`）是 **per-CPU 独立**、不依赖全局 scheduler init ⇒ aarch64 的 `smp::init()` + `smp_init::init()` 应接在 aarch64 `interrupt_late_init()` 内，与 x86_64 同构 |

## DECISION-082（aarch64 SMP bring-up 实施裁定）

- **描述**：为 aarch64 补齐 AP 上线路径。施工前需裁定实现路径、次核入口所在 EL 的处理、GIC 复用范围与验证强度。
- **方案（用户裁定 + 本文件落实）**：
  1. **实现路径 = 方案 B（相对完整）**（用户裁定）：在方案 A（PSCI CPU_ON + 次核汇编入口 + 次核 Rust 入口 + GIC 次核初始化 + BSP 接线 + §2.3 六门槛）之上，加压：次核入口后置自检 fail-fast、`[SMP] online CPUs: N` 里程碑、PSCI 版本探测与 `CPU_ON` 错误码解析、host-tests 静态契约测试、QEMU 集成 `-smp 2` 断言。
  2. **次核入口 EL 处理 = 先实测后定**（E7）：`start.S` 次核 stub 首条即读 `CurrentEL` 打印；若实测为 EL1 则直入 Rust 入口，若为 EL2 则补 EL2→EL1 降级（复用 `start.S` 现有 `el2_entry` 的降级序列常量）。**不预设**，以实测为准。
  3. **页表 = 复用 BSP 运行时表物理基址**（E5）：次核 stub 从固定位置读取 BSP 写入的 `TTBR0_EL1` 值（= `virt_to_phys(L0_TABLE)`）装入自己的 `TTBR0_EL1`/`TTBR1_EL1`，并同装 `MAIR_EL1`/`TCR_EL1`，直接开 MMU 切高半区。**不**新建次核独立页表（避免双表并对齐 BSP 的恒等+高半区别名等价性）。
  4. **GIC = 参数化复用**（E4）：把 `init_redistributor()` 等改为按 CPU 号定位 `GICR` frame（stride 128 KiB），BSP 与 AP 共用同一代码路径。
  5. **验证强度**：满足 §2.3 六门槛；专项判据 = QEMU `-smp 2` 下打印 `[SMP] online CPUs: 2` 且 `smp_is_enabled() == true`。
- **状态**：[X]
- **详情（施工结论）**：批次 A–E 全部落地，SMP-01..SMP-10 全 `[X]` 收口；六门槛实测结果见 SMP-10 详情；专项判据 `[SMP] online CPUs: 2` 已进 CI（SMP-09）。本工程的范围声明（aarch64 TLB shootdown **发送路径**与 IPI/TLB 运行期验证不在内）仍如 §1 关键认知与裁定 5 详情。
- **详情（裁定 3 的理由）**：次核自建页表意味着要在汇编或早期 Rust 里重新构造两套等价映射，既放大出错面（E5 的历史教训：TTBR1 根表形态错误曾导致内核从未进过异常处理），又与 BSP 映射产生不一致风险。复用 BSP 运行时表物理基址把"页表正确性"收敛为**单点已验证事实**，符合 §12.3 简约默认。
- **详情（范围边界声明）**：aarch64 侧 **TLB shootdown 发送路径不在本工程范围**（见 §1 关键认知）。本工程交付后 aarch64 具备"多核在线"；"主动发 SGI 13 请求他核失效 TLB"的调用面仍缺，作为风险项登记。**订正（DECISION-083 施工期实证）**：初稿同时宣称"能收到 SGI" —— 该前提**不成立**，SGI 13/14 实际从未使能（接收侧静默）；发送路径 + SGI 使能/编码由后续工程 DECISION-083（[aarch64-tlb-shootdown-send.md](./aarch64-tlb-shootdown-send.md)）一并实装并运行验证。

## 任务清单

- **SMP-01. 计划与决策固化**
  - 描述：施工前把 aarch64 SMP 缺口实证与裁定写入专门计划文件（AGENTS §9.1 决策先记录）。
  - 方案：新建本文件；在 [smp-ipi-protocol.md](./smp-ipi-protocol.md) §2.6 / §5.3 补指向本工程的指针；在 [unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) ISSUE-RT-002 条目登记"真多核压测待本工程落地"。
  - 状态：[X]
  - 详情：（施工轮填写）本文件即交付物；[smp-ipi-protocol.md](./smp-ipi-protocol.md) §2.6 / §5.3 已补 DECISION-082 指向与订正（原「无 AP 上线路径 / `SMP_ENABLED` 恒 false」前提解除）；[unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) ISSUE-RT-002 已补「DECISION-082 订正」行（真多核 GIC 压测自此具备执行载体，条目本身仍不闭合）。

- **SMP-02. PSCI `CPU_ON` 封装 + 版本探测 + 错误码解析 + CPU 拓扑探测**
  - 描述：提供"上电指定 CPU 至指定入口"的 safe 原语，并解决次核目标 MPIDR 的来源（E1、E6）。
  - 方案：
    - `psci.rs` 新增 4 寄存器 `smc4(func, x1, x2, x3) -> i64`（`asm!("smc #0", in("x0") func, in("x1") x1, in("x2") x2, in("x3") x3, lateout("x0") ret, options(nostack))`，**含 `// SAFETY:` 注释**）。
    - 新增 `pub const PSCI_CPU_ON: u32 = 0xC400_0003`（SMC64）。
    - 新增 `pub enum PsciError { NotSupported, InvalidParameters, Denied, AlreadyOn, OnPending, InternalFailure, NotPresent, Disabled, Other(i64) }` 与 `from_i64`（映射 0=SUCCESS 以外各负码；见下）。
    - 新增 `pub fn cpu_on(mpidr: u64, entry_pa: u64, context_id: u64) -> Result<(), PsciError>`；返回 `Success` 前置校验 `psci_version()` 可用（`None` ⇒ `NotSupported`）。
    - CPU 拓扑探测：在 `dtb.rs` 增 `cpu_mpidrs`（`/cpus/cpu@N/reg` 首条目，`address-cells=1`）或等价最小能力；探测失败回退 QEMU virt 约定 `mpidr = cpu_index`（Aff0=索引）。
  - 状态：[X]
  - 详情（施工结论）：`psci.rs` 以 `conduit()` + `invoke()` 按设备树 `/psci.method` 分派 `hvc`/`smc`（QEMU virt = HVC，见本工程 conduit 子决策），`cpu_on()` / `psci_version()` / `system_off()` / `system_reset()` 统一走 `invoke()`；`dtb.rs` 新增 `PsciMethod` 枚举 + `DtbInfo.psci_method` + `PSCI_METHOD` 全局持久化 + `/psci` 节点 `method` 解析，`CpuTopology` 已由 `/cpus/cpu@N/reg` 枚举 MPIDR 并回退 Aff0=索引。host 侧 `parse_psci_method_smc` / `parse_psci_method_unknown_is_none` 通过。
  - 详情（错误码表，ARM PSCI v1.x）：`SUCCESS=0` / `NOT_SUPPORTED=-1` / `INVALID_PARAMETERS=-2` / `DENIED=-3` / `ALREADY_ON=-4` / `ON_PENDING=-5` / `INTERNAL_FAILURE=-6` / `NOT_PRESENT=-7` / `DISABLED=-8`。
  - 详情（现有函数 ID 复用）：`PSCI_VERSION = 0x84000000` 已存在，`psci_version()` 现为私有 —— 需升为 `pub(crate)` 或经 `cpu_on` 内部调用（不扩大外部 API 面）。
  - 详情（`ALREADY_ON` 语义）：下次核已上电时返回 `ALREADY_ON`，本工程视为**可继续**（幂等），但须打印告警以便与"真双核"区分。

- **SMP-03. 次核汇编入口（低半区 stub）**
  - 描述：`PSCI_CPU_ON` 的 entry point 必须是**物理地址**、且次核以 MMU **关**态进入 ⇒ 需一段 `VMA == LMA == PA` 的低半区 stub（E1、E7）。
  - 方案：
    - 在 `boot/aarch64/start.S` 新增 `.text.ap_entry`（或 `.section .text.boot`）段：开头 `mrs x0, CurrentEL` 打印/保存（EL 实证，E7）→ 从固定 `.bootbss` 槽读 BSP 预写的 `TTBR0_EL1`/`MAIR_EL1`/`TCR_EL1`/`ap_context` → 装 `TTBR0_EL1`/`TTBR1_EL1`（同值）、`MAIR_EL1`、`TCR_EL1` → 设次核栈（从 `ap_context` 读）→ `dsb sy; isb` → 开 MMU（`SCTLR_EL1` M/C/I）→ 分支高半区 Rust 入口 `ap_main`。
    - 该 stub 的链接地址须为**物理低半区**（`0x4008_XXXX`，与 `_start` 同 `.text.boot` 区间或独立 `.ap_entry` 段并保证 PA 可达）；链接脚本 `link/aarch64.ld` 需相应确保该段不落入高半区重定位。
    - EL 处理按裁定 2：实测 EL1 ⇒ 直接进；EL2 ⇒ 补 `el2→el1` 降级（复用 `el2_entry` 常量序列）。**以 SMP-03 实测结论为准，先打印再决定**。
  - 状态：[X]
  - 详情（施工结论）：`start.S` 新增低半区 `ap_entry_asm`（`VMA == LMA == PA`），自 `.bootbss` 的 `_ap_boot_info` 槽装载 TTBR0/TTBR1/MAIR/TCR/SCTLR/栈/入口 VA，经 TTBR1 高半区别名 `br` 跳 `ap_main`；`.bootbss` 已预留 80 字节启动槽。链接脚本新增 `ap_entry_asm_alias = _kernel_base + ap_entry_asm` 供 Rust 侧取高半区地址（避开低半区 adrp ±4GB 越界）。`-smp 2` 实测次核以 EL1 进入并成功上线（无需 EL2 降级）。
  - 详情（BSP 侧准备）：`smp_init::init()`（SMP-06）须在调用 `cpu_on` 前把 `TTBR0_EL1` 当前值、`MAIR_EL1`、`TCR_EL1` 与次核栈顶写入 `.bootbss` 的 fixed 槽（形态对齐 x86_64 `ApStartupInfo` 的 `#[repr(C, packed)]` + 编译期 `size_of` 断言）。
  - 详情（待实证项）：`_boot_stack_top` 只有 256 KiB 单核栈；次核栈须**另行分配**（BSP 侧从物理内存切分或复用既定 AP 栈池），不得共用 BSP 栈。

- **SMP-04. GIC 次核初始化参数化**
  - 描述：次核需唤醒并配置**自己的** Redistributor（E4）。
  - 方案：把 `gic.rs` 的 `init_redistributor()` / `init_cpu_interface()` / `enable_timer_ppi()` 改为接受 **CPU 索引**（或 `GICR` frame 物理基址），内部按 `GICR stride = 128 KiB` 计算 `RD` frame 与 `SGI` frame 基址（`GICR_SGI = GICR_RD + GICR_SGI_OFFSET(0x1_0000)`）。BSP 调用点传 0，AP 传自身 CPU 索引。
  - 状态：[X]
  - 详情（现有常量）：`GICR_SGI_OFFSET = 0x1_0000`（RD→SGI 相邻 64 KiB）；CPU 间 `GICR` stride = **128 KiB**（2 × 64 KiB，ARM GICv3 规定）。新增常量 `GICR_STRIDE: u64 = 0x2_0000` 并附中文说明。
  - 详情（Distributor 共享）：`GICD` 为全局共享，**仅 BSP 初始化一次**；AP **不得**重复初始化 `GICD`。`GICD_ITARGETSR = 0x0101_0101`（PPIs to CPU0）现为硬编码单一目标 —— SPI 亲和路由暂不随 CPU 数调整，登记为简化项（`// SIMPLIFIED:`）。
  - 详情（唤醒超时复用）：沿用既有 `REDIST_WAKE_SPIN_LIMIT`；超限返回 `Err("GICR_WAKER.ChildrenAsleep 未在限定自旋内清零")`，AP 侧据此 fail-fast（不登记该 CPU）。

- **SMP-05. 次核 Rust 入口（`smp_init.rs`）**
  - 描述：次核切高半区后进入 Rust，完成 per-CPU 初始化并登记上线（E2、E10）。
  - 方案：新建 `arch/aarch64/smp_init.rs`，镜像 x86_64 同名模块结构但按 aarch64 语义重写：
    - `pub fn init()`（BSP 侧）：`smp::init()` 之后调用；探测 CPU 数（`<=1` 则打印 `[SMP] Single-core system, skipping AP startup` 并置 `SMP_FULLY_INITIALIZED` 返回）→ 准备 `.bootbss` 槽（SMP-03 详情）→ 遍历次核 `psci::cpu_on(mpidr, ap_entry_pa, cpu_index)` → 等待上线（有超时）→ 打印 `[SMP] online CPUs: {}`。
    - `pub unsafe extern "C" fn ap_main(cpu_index: u64) -> !`（AP 侧）：`gic::init_per_cpu(cpu_index)`（SMP-04）→ **per-CPU 四连**（对齐 x86_64 `ap_entry`）：`proc::init_cpu_queue(cpu_index, 0)` / `proc::init_per_cpu_sched(cpu_index)` / `sync::rcu_alloc_cpu(cpu_index)` / `irq::softirq_alloc_cpu(cpu_index)`（任一失败 ⇒ **不登记**，打印后 `loop { arch!(halt()) }`）→ `smp::register_cpu(cpu_index)`（登记 `CPU_ONLINE` / `SMP_ENABLED`）→ `SCHEDULER.init_per_cpu_idle(cpu_index)` → 写 done 标志 → **开中断**（`msr daifclr, #2` 或等价）→ `loop { arch!(halt()) }`。
    - 次核入口后置自检（fail-fast，方案 B）：开中断前后各做一次可观测自检（如 `gic` 回读 `ICC_IGRPEN1_EL1`、`smp::is_enabled()`），任一不满足即 FATAL loop 并打印原因（对齐 `entry.rs` 的 GIC fail-fast 风格）。
    - 导出：`smp_init_bsp()` / `smp_ready()` / `smp_get_ap_count()`（与 x86_64 命名对齐，供 BSP 接线与自检）。
  - 状态：[X]
  - 详情（施工结论）：`arch/aarch64/smp_init.rs` 已落地 `init()`（拓扑 `<=1` ⇒ 单核退化；否则逐核 `start_ap`，含 80 字节 `ApBootInfo` + `clean_to_poc` + 有界自旋等 `done`）与 `ap_main(cpu_index)`（GIC per-CPU → 异常向量 → per-CPU 四连 → `smp::register_cpu` → `SCHEDULER.init_per_cpu_idle` → Release 置 `done` → 开中断 idle）。`mmu.rs` 补 `read_ttbr1()`。次核栈用 `alloc_zeroed` 分配（避免 `static mut`）。
  - 详情（与 x86_64 的差异）：x86_64 用 INIT-SIPI + trampoline + `ApStartupInfo`（`#[repr(C, packed)]`，54 字节）；aarch64 用 PSCI `CPU_ON`（entry=物理地址）+ `.bootbss` 固定槽。**不复制** x86_64 的 `ApStartupInfo` 结构体到 aarch64（语义不同），但沿用其"固定布局 + 编译期 `size_of` 断言 + host-tests 镜像测试"的契约化手法。
  - 详情（中断上下文约束）：AP 在**未开中断**期间完成 GIC Redistributor 唤醒与 CPU 接口使能，开中断后进入 idle；不得在持锁状态下开中断（对齐 AGENTS §11 中断上下文约束）。

- **SMP-06. BSP 接线（`interrupt_late_init`）**
  - 描述：让 aarch64 BSP 真正调用 SMP 抽象层与 AP 启动（E10，§1 缺口 4）。
  - 方案：在 [`arch/aarch64/mod.rs`](../../src/kernel/privileged/arch/aarch64/mod.rs#L242-L244) 的 `interrupt_late_init()` 内追加：`smp::init()` → `smp_init::init()`（对齐 x86_64 [`interrupt_late_init()`](../../src/kernel/privileged/arch/x86_64/mod.rs#L324-L395) 末尾）。在 `arch/aarch64/mod.rs` 顶部补 `mod smp_init;`（并按需 `pub(crate) use`）。
  - 状态：[X]
  - 详情（施工结论）：`arch/aarch64/mod.rs` 已加 `pub mod smp_init;`，`interrupt_late_init()` 内追加 `crate::privileged::smp::init();` + `smp_init::init();`（对齐 x86_64）。单核与 `-smp 2` 双路径 QEMU 均实测接线生效。
  - 详情（为何在此接线）：`interrupt_late_init()` 在 `scheduler::init()` 之前（`lib.rs` L750 vs L770，E10），与 x86_64 一致；AP 的 `init_per_cpu_sched` 是 per-CPU 独立初始化，不依赖全局 scheduler。接线点天然正确，**无需改 `lib.rs` 顺序**。
  - 详情（early_init 不动）：`interrupt_early_init()` 在 aarch64 保持空（GICv3 + VBAR_EL1 由 `entry.rs` 配置）；SMP 属"late"阶段，只动 `interrupt_late_init()`。

- **SMP-07. 里程碑与后置自检（`[SMP] online CPUs: N` + fail-fast）**
  - 描述：提供可被 QEMU 集成脚本断言的里程碑，并让上线失败**显式失败**而非静默退化（方案 B）。
  - 方案：
    - 成功里程碑：`smp_init::init()` 完成后打印 `[SMP] online CPUs: {CPU_COUNT}`（与 x86_64 同文案，供 `qemu_boot_test.sh` grep）。
    - 单核退化：打印 `[SMP] Single-core system, skipping AP startup`（与 x86_64 同文案）。
    - 次核失败：`cpu_on` 返回错误 ⇒ 打印 `[SMP] AP {i} CPU_ON failed: {err:?}`；超时未上线 ⇒ 打印 `[SMP] AP {i} did not come online within timeout`。**默认不 panic**（BSP 仍可单核运行），但 `SMP_FULLY_INITIALIZED` 仅在**期望的次核全部上线**时置真，使自检能区分"部分成功"。
  - 状态：[X]
  - 详情（施工结论）：单核 QEMU 打印 `[SMP] Single-core system, skipping AP startup`；`-smp 2` QEMU 打印 `[SMP] BSP initialized` + `[SMP] online CPUs: 2`（`online` 取 `smp::get_cpu_count()` 权威计数，仅 `register_cpu` 递增 ⇒ 次核确已上线登记，`online == expected` 时置 `SMP_FULLY_INITIALIZED`）。启动续行至 `VFS ready` / `Entering EL0` / KPTI-09 断言无回归。
  - 详情（自检判据）：`smp_ready()` 返回「`SMP_FULLY_INITIALIZED == true`」；BSP 在 `interrupt_late_init()` 返回后可据此打印一行汇总。QEMU 断言以 `online CPUs: 2` 为准。

- **SMP-08. host-tests 静态契约测试**
  - 描述：把"入口 ABI / 常量 / 错误码编码"从运行期依赖变为可 CI 强制的静态契约（对齐 `host-tests/tests/arch_apstartup_info_layout_test.rs` 范式）。
  - 方案：新建 `host-tests/tests/aarch64_smp_contract_test.rs`（或按既有命名风格），断言：
    - PSCI 函数 ID 编码：`PSCI_CPU_ON == 0xC400_0003`、`PSCI_VERSION == 0x8400_0000`、`SYSTEM_OFF/RESET` 值不变。
    - 错误码映射：`-1..=-8` 到 `PsciError` 各变体一一对应（可用 `#[cfg(target_arch="aarch64")]` 镜像常量，或经 `pub` 常量导出）。
    - GIC 常量：`GICR_SGI_OFFSET == 0x1_0000`、`GICR_STRIDE == 0x2_0000`。
    - 若 aarch64 侧引入 `#[repr(C, packed)]` 的 AP 槽结构体，则镜像其 `size_of` 与关键字段偏移（与 x86_64 测试同手法）。
  - 状态：[X]
  - 详情（施工结论）：新建 `host-tests/tests/aarch64_smp_contract_test.rs`（7 用例，全过）。覆盖：PSCI 四个函数 ID 编码；`PsciError` 变体集与负返回码 `0/-1..=-8/兜底` 映射；`ApBootInfo` 尺寸（`size_of == 80` 编译期断言 + `start.S` `.space 80`）与 10 字段顺序 ↔ 汇编字节偏移 `0x00..0x48` + 五个 `msr` 系统寄存器绑定；启动槽/入口 stub 低半区符号 + `aarch64.ld` 高半区别名；`GICR_SGI_OFFSET` / `GICR_STRIDE` 几何常量。
  - 详情（宿主可测性约束）：host 目标（x86_64）不能编译 aarch64 内联汇编，故测试**只做源码文本契约分析**，不调用 `smc`/`hvc`。**未采用"镜像常量/结构体"策略**（原方案列为可选）：在测试内复刻常量/结构体属内核逻辑的平行实现，与源码漂移时镜像端不会失败（自证式断言），违背项目「主机测试不得包含内核逻辑平行实现」硬约束；改用与批次 A `aarch64_gic_contract_test.rs` 同手法的文本契约，任何一侧漂移即失败。

- **SMP-09. QEMU 集成断言扩展（`-smp 2`）**
  - 描述：让 aarch64 启动测试真正跑到双核（E8）。
  - 方案：`scripts/qemu_boot_test.sh` aarch64 分支的 `qemu-system-aarch64` 命令行加 `-smp 2`；在既有断言（`GICv3 ready` / `VFS ready` / EL0 / KPTI）之上新增：grep `[SMP] online CPUs: 2`，命中 ⇒ `ok`，未命中 ⇒ `warn`/`err`（与现有 RT-002 断言风格一致）。
  - 状态：[X]
  - 详情（施工结论）：aarch64 分支 QEMU 参数加 `-smp 2`（置于 `-m 512` 之后），并在 `GICv3 ready` 断言块之后新增 `online CPUs: 2` 里程碑断言（fail-closed，未命中置 `RESULT=1`）。实跑 `./scripts/qemu_boot_test.sh aarch64` = 1/1 通过，逐项命中 `VFS ready` / `GICv3 ready` / **`SMP 双核上线 (online CPUs: 2)`** / virtio-net functions bridge / `Entering EL0` / KPTI-09，无回归。
  - 详情（回归衔接）：本工程落地后，ISSUE-RT-002 的"真多核 GIC 压测"已具备执行载体 —— 压测脚本 `scripts/gic_stress_test.sh` 已完成 `-smp 2` 升级（见 SMP-10 后续项 ②），并以真多核载体完成 50 次启动压测（50/50 通过，详见 ISSUE-RT-002 台账「真多核压测复验」行）。
  - 详情（x86_64 分支不动）：`-smp` 仅加在 aarch64 分支；x86_64 分支的 `-smp` 现状保持（避免无关改动，§12.2）。

- **SMP-10. §2.3 六门槛验证**
  - 描述：本工程交付的验收判据。
  - 方案：按序跑全部门槛并记录实测结果：
    1. `./ci/build.sh all`（双架构 0 error / 0 warning）
    2. `cargo clippy --release -- -D warnings`（0 warning）
    3. `./ci/audit.sh`（核心审计全过，含 `audit_functions_boundary.py` / `audit_safety_coverage.py`）
    4. `make test-host`
    5. `make test-kernel-host`（= `cd src/kernel && cargo test --features host-test --lib`）
    6. `./scripts/qemu_boot_test.sh all`（双架构；aarch64 侧须命中 `online CPUs: 2`）
  - 状态：[X]
  - 详情（施工结论）：六门槛实测 —— ① `./ci/build.sh all` = Passed 5 / Failed 0；② clippy pedantic 三维（lib / kernel_test / host-test）全过；③ `./ci/build.sh aarch64 && ./ci/audit.sh` = `AUDIT_EXIT=0`（functions 0 unsafe、6 不变式 PASS、privileged SAFETY 覆盖 1867/1867 缺 0、FP-06 PASS）；④ `make test-host` = 116 个测试二进制全 ok、0 failed（含 SMP-08 契约 7 用例）；⑤ `make test-kernel-host` = 949 passed / 0 failed；⑥ `./scripts/qemu_boot_test.sh all` = 2/2 通过（x86_64：`VFS ready`/e1000/Ring 3/KPTI-09；aarch64：`VFS ready`/`GICv3 ready`/`SMP 双核上线 (online CPUs: 2)`/virtio-net bridge/`Entering EL0`/KPTI-09）。
  - 详情（专项判据实测）：`[SMP] online CPUs: 2` 由 aarch64 `smp_init::init()` 打印（取权威计数 `smp::get_cpu_count()`）⇒ `CPU_COUNT == 2` 直接可观测；`smp_is_enabled()` 为真由构造蕴含 —— `SMP_ENABLED` 与 `CPU_COUNT` 由 `smp::register_cpu` 同一调用路径唯一置位（`CPU_COUNT.fetch_add` 后 `SMP_ENABLED.store(true)`），且 AP 的上线自检 `is_cpu_online(idx)` 未触发 fail-fast（否则 BSP 超时、`online` 不会为 2）⇒ `CPU_COUNT == 2` 必然蕴含 `SMP_ENABLED == true`。（未新增内核代码使该布尔直接打印：本批为验证收口，遵 §12.2/§12.3 简约路径；如需直接实测该行，可单开一行 BSP 汇总日志。）
  - 详情（F9/F4）：`PsciError` 各变体、`cpu_mpidrs`、`GICR_STRIDE` 均经真实使用路径消费（无 `#[allow(dead_code)]`）；新增 `invoke` / 次核入口 unsafe 块的 `// SAFETY:` 覆盖计入 1867/1867。
  - 详情（后续项，不阻塞收口）：① `kpti_aarch64.rs` 注释失真**已修正** + 全局量内存序**已复核**（结论见「风险与回退」与文末专项登记）；复核发现的结构性缺陷（入口/出口每核活跃值存单实例全局量）已**立项为独立专项（per-CPU 化）待排期**；② `scripts/gic_stress_test.sh` 的 `-smp` 升级**已完成** —— 该脚本默认以真多核（`-smp 2`）启动，里程碑由单一 `GICv3 ready` 升级为 `GICv3 ready` && `[SMP] online CPUs: <N>` 双断言（fail-closed，等同覆盖 BSP 与全部 AP 的 per-CPU GIC），并新增 `GIC_STRESS_SMP`（核数）/ `GIC_STRESS_QEMU_EXTRA`（附加 QEMU 参数）两个旋钮以变换核数与时序；③ ISSUE-RT-002 原始偶发挂起**根因仍未定位**（条目不闭合）—— 已借真多核载体完成**有界时序搜索**（5 场景合计 96 次启动，96/96 通过、0 次触发挂起：`-smp 2/4/8` × {50,15,10} + `tcg,thread=single` × 15 + `-icount` × 6），未复现，结论进一步支持「QEMU TCG 时序偶发、当前环境不可稳定复现」，保留待真机 / 具复现样本的环境。
  - 详情（F9 死代码防线）：新增的 `PsciError` 各变体、`cpu_mpidrs`、`GICR_STRIDE` 等必须有**真实使用路径**（错误码经 `cpu_on` 返回并被 BSP 打印；`cpu_mpidrs` 被 `cpu_on` 消费；`GICR_STRIDE` 被 per-CPU 基址计算消费），不得靠 `#[allow(dead_code)]` 保留。

## 施工规划（具体工程）

> 本章把 SMP-01..SMP-10 从"任务清单"细化为**可施工的批次、依赖、数据契约与代码骨架**。所有文件锚点、行号、函数签名均为 §10 源码复核（见 §2）实测所得；施工时以当轮实际文件为准（行号可能漂移）。**批次 A（SMP-04）、B（SMP-02+03+05+06+07）、C（SMP-09）、D（SMP-08）、E（SMP-10）均已落地并通过 §2.3 六门槛 —— 本工程 SMP-01..SMP-10 全部 `[X]` 收口。**

### 3.1 施工批次与依赖

将 SMP-02..SMP-10 编为 5 个可独立满足 §2.3 门槛的批次。F9（死代码零容忍）决定：**"新增 API 却无调用者"不可单独成批**，故 PSCI `CPU_ON` 必须与其调用链同批落地。

| 批次 | 含任务 | 依赖 | 出口判据（可验证） |
|---|---|---|---|
| **A** | SMP-04（GIC 参数化） | 无 | 双架构 0w0e / clippy / 核心审计 / `make test-host` 过；单核 QEMU `GICv3 ready` 仍命中（**单核零回归**） |
| **B** | SMP-02 + SMP-03 + SMP-05 + SMP-06 + SMP-07（一次落地） | A | 双架构 0w0e / clippy / 审计过；默认单核 QEMU 打印 `[SMP] Single-core system, skipping AP startup` 且启动至 EL0 无回归；手动 `-smp 2` 首次实证 E7（记录 `CurrentEL`）与 `online CPUs: N` |
| **C** | SMP-09（`-smp 2` 断言） | B | `./scripts/qemu_boot_test.sh aarch64` 命中 `[SMP] online CPUs: 2` ✅ 已达成 |
| **D** | SMP-08（host-tests 静态契约） | B（常量已冻结） | `make test-host` 全过（含新契约测试）✅ 已达成 |
| **E** | SMP-10（§2.3 六门槛 + 专项判据） | A+B+C+D | 六门槛全过 + `online CPUs: 2` && `smp_is_enabled()==true` ✅ 已达成 |

**批次 A 可先行**：GIC 参数化是**纯重构**（单例基址改为按 CPU 索引计算，BSP 仍传 0），不新增调用者 ⇒ 无死代码风险，单核行为不变 ⇒ 可独立验证与独立回退。

**批次 B 须一次落地**：`psci::cpu_on` / `ap_entry_asm` / `ap_main` / `interrupt_late_init` 接线构成一条不可分割的调用链；任一单独落地都不构成可验证交付（接线未通 = 功能未交付），故合并为一批形成"定义—调用—验证"闭环。

**依赖关系**：`A → B → {C, D} → E`（C 与 D 无相互依赖，可并行）。

### 3.2 数据契约：`.bootbss` 次核启动槽 `ApBootInfo`

次核以 **MMU 关、物理地址、低半区** 进入，故 BSP 与 AP 之间只能经 **`.bootbss` 物理固定槽** 传递启动参数（该段在 [aarch64.ld](../../src/kernel/privileged/link/aarch64.ld) 中 `NOLOAD` 且 `VMA == LMA == PA`）。契约手法对齐 x86_64 [`ApStartupInfo`](../../src/kernel/privileged/arch/x86_64/smp_init.rs)（固定布局 + 编译期 `size_of` 断言），但**语义不同，不复制其结构体**。

**三侧骨架**：

Rust 侧（`arch/aarch64/smp_init.rs`）：

```rust
/// 次核启动信息槽（BSP 写、AP 读；固定布局，映射 `.bootbss` 的 `_ap_boot_info`）。
/// 全字段 u64 ⇒ `repr(C)` 下自然 8 字节对齐，偏移即下标 × 8，无需 packed。
#[repr(C)]
struct ApBootInfo {
    ttbr0: u64,      // 0x00 BSP 运行时 TTBR0_EL1 值（= virt_to_phys(L0_TABLE)）
    ttbr1: u64,      // 0x08 BSP 运行时 TTBR1_EL1 值（当前与 ttbr0 同表）
    mair: u64,       // 0x10 MAIR_EL1
    tcr: u64,        // 0x18 TCR_EL1
    sctlr: u64,      // 0x20 SCTLR_EL1（含 M/C/I，即"开 MMU"后的目标值）
    stack_top: u64,  // 0x28 次核栈顶（高半区 VA，16 字节对齐，AAPCS64）
    entry_va: u64,   // 0x30 `ap_main` 的高半区 VA
    cpu_index: u64,  // 0x38 次核索引（= MPIDR_EL1 Aff0，亦作 PSCI context_id）
    done: u64,       // 0x40 AP 上线完成标志（AP 写 1）
    el: u64,         // 0x48 次核实测 CurrentEL（诊断，E7 实证落点）
}

// 编译期契约断言：布局变更必须在此显式更新（与汇编/链接脚本同步）。
const _: () = assert!(core::mem::size_of::<ApBootInfo>() == 80);
const DONE_OFFSET: usize = core::mem::offset_of!(ApBootInfo, done);
const EL_OFFSET: usize = core::mem::offset_of!(ApBootInfo, el);
```

汇编侧（`boot/aarch64/start.S`，追加到既有 `.bootbss` 段）：

```asm
.section .bootbss, "aw", %nobits
.align 3                       // 8 字节对齐
_ap_boot_info:
    .space 80                  // = sizeof(ApBootInfo)，与 Rust 侧断言一致
```

链接脚本侧（`link/aarch64.ld`，仿既有 `fdt_addr_ptr` 范式）：

```ld
/* 次核启动槽：低半区符号的高半区别名，供 Rust 侧以 VA 访问 */
ap_boot_info_ptr = _kernel_base + _ap_boot_info;
```

Rust 侧访问：

```rust
unsafe extern "C" { static ap_boot_info_ptr: u64; }
// 读：read_volatile(&raw const ap_boot_info_ptr) → 得高半区 VA → 转 *mut ApBootInfo
```

**缓存一致性（真机必需，QEMU 不建模）**：BSP 写满槽后，必须把该 80 字节范围**按 64 字节 cache line 逐行 `dc civac`（清理到 PoC）+ `dsb sy`**，AP（caches off）方能读到新值。缺此步在 QEMU 上可能"看起来正常"，但真机会读到陈旧 DRAM。等价于 Linux 的 `dcache_clean_inval_poc`。

**次核栈**：`_boot_stack_top` 仅 256 KiB 单核栈，**不得共用**。BSP 侧（`smp_init::init()`）为每个 AP `Box::new(ApStack)`（`#[repr(align(16))]`，建议 16 KiB），`stack_top` 取"高半区 VA + size"，写入槽。

### 3.3 分步施工细则

#### 3.3.1 SMP-04（批次 A）：GIC 参数化

- **文件锚点**：[gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs)
- **改动**：
  1. 新增 `pub const GICR_STRIDE: u64 = 0x2_0000;`（GICv3 规定每 CPU 的 RD+SGI 共 128 KiB）。
  2. 新增薄封装 `fn gicr_read_at(base: u64, off: u64) -> u32` / `gicr_write_at` / `gicr_sgi_read_at` / `gicr_sgi_write_at`（内部仍走既有 `ptr::read_volatile`/`write_volatile` + 既有 `// SAFETY:` 注释），既有 `gicr_read(off)` 等改为 `gicr_read_at(GICR_BASE, off)`（**最小改动面**，不改调用者语义）。
  3. 新增 `fn redist_frames(cpu_index: u32) -> (u64, u64)`：`rd = GICR_BASE + GICR_STRIDE * cpu_index`、`sgi = rd + GICR_SGI_OFFSET`。
  4. `init_redistributor()` / `enable_timer_ppi()` 改为按 `(rd, sgi)` 基址操作；`init_cpu_interface()` 不动（纯 ICC_* 系统寄存器，天然 per-CPU）。
  5. 新增 `pub fn init_per_cpu(cpu_index: u32) -> Result<(), &'static str>`：`init_redistributor(rd, sgi)?; init_cpu_interface(); enable_timer_ppi(sgi);`。
  6. `pub fn init()` 内 `init_redistributor()` 调用点改为 `init_per_cpu(0)?`；**`init_distributor()` 仍仅在 BSP 调用一次**（AP 不得重复初始化 `GICD`）。
  7. `GICD_ITARGETSR = 0x0101_0101` 保持硬编码 + 附 `// SIMPLIFIED: PPIs 固定亲和 CPU0；SPI 亲和路由未随 CPU 数调整；多核 SPI 负载均衡时需扩展`。
- **验证**：`./ci/build.sh all` → `./scripts/qemu_boot_test.sh aarch64`（须仍命中 `GICv3 ready`，无回归）。

#### 3.3.2 SMP-02（批次 B）：PSCI `CPU_ON` + 拓扑

- **文件锚点**：[psci.rs](../../src/kernel/privileged/arch/aarch64/psci.rs)、[dtb.rs](../../src/kernel/privileged/dtb.rs)
- **psci.rs 骨架**：

```rust
/// `CPU_ON` 的 SMC64 函数 ID（ARM PSCI v0.2+）。
pub const PSCI_CPU_ON: u32 = 0xC400_0003;

/// 4 寄存器 PSCI 调用（x0=func / x1..x3=参数，返回值取 x0）。
///
/// SAFETY: 仅在 EL1 及以下、由固件（PSCI 实现）提供 `smc` 服务时调用；
/// 调用方须保证 `entry_pa` 为物理地址且已按次核入口约定准备就绪。
unsafe fn smc4(func: u32, x1: u64, x2: u64, x3: u64) -> i64 {
    let ret: i64;
    // SAFETY: 见函数级 SAFETY 说明。
    unsafe {
        core::arch::asm!(
            "smc #0",
            in("x0") u64::from(func), in("x1") x1, in("x2") x2, in("x3") x3,
            lateout("x0") ret, options(nostack),
        );
    }
    ret
}

/// PSCI 返回码（0=SUCCESS 由 `cpu_on` 内部转为 `Ok(())`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsciError { NotSupported, InvalidParameters, Denied, AlreadyOn, OnPending, InternalFailure, NotPresent, Disabled, Other(i64) }

impl PsciError {
    /// 负数返回码 → 错误变体；0（SUCCESS）→ `None`。
    fn from_i64(code: i64) -> Option<Self> {
        match code {
            0 => None,
            -1 => Some(Self::NotSupported),
            -2 => Some(Self::InvalidParameters),
            -3 => Some(Self::Denied),
            -4 => Some(Self::AlreadyOn),
            -5 => Some(Self::OnPending),
            -6 => Some(Self::InternalFailure),
            -7 => Some(Self::NotPresent),
            -8 => Some(Self::Disabled),
            other => Some(Self::Other(other)),
        }
    }
}

/// 上电 `mpidr` 指定的 CPU 至物理地址 `entry_pa`，`context_id` 透传至入口 x0。
pub fn cpu_on(mpidr: u64, entry_pa: u64, context_id: u64) -> Result<(), PsciError> {
    if psci_version().is_none() {
        return Err(PsciError::NotSupported);
    }
    // SAFETY: entry_pa 为次核低半区 stub 的物理地址（见 SMP-03）。
    let code = unsafe { smc4(PSCI_CPU_ON, mpidr, entry_pa, context_id) };
    match PsciError::from_i64(code) { None => Ok(()), Some(e) => Err(e) }
}
```

  - `smc4` 与既有 `smc` 同带 `#[expect(clippy::cast_lossless, reason = "DECISION-043 pedantic 兜底...")]`（`u64::from(func)` 已避免一次转换，按实际 lint 结果决定是否仍需 expect）。
  - `psci_version()` 保持私有，仅 `cpu_on` 内部调用（不扩大外部 API 面）。
- **dtb.rs 骨架**：

```rust
/// `/cpus` 下可枚举的 CPU 上限（防 DTB 异常）。
pub const CPU_MPIDR_MAX: usize = 8;

/// CPU 拓扑（`/cpus/cpu@N/reg` 首条目）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTopology { pub count: u32, pub mpidrs: [u64; CPU_MPIDR_MAX] }
```

  - `DtbInfo` 增字段 `pub cpus: Option<CpuTopology>`。
  - parser 需为 `/cpus` 节点**单独采样** `#address-cells`（QEMU virt 下 `/cpus` 为 `#address-cells=1` / `#size-cells=0`，与根节点不同）⇒ 现有"全局单元格宽度"解码会误读，须在 `record_node` 针对 `/cpus` 分支读取其单元格属性。
  - **回退**：解析失败 ⇒ `cpus = None`，`smp_init` 回退 QEMU virt 约定 `mpidr = cpu_index`（Aff0=索引）。
- **验证**：`./ci/build.sh all`；配合 SMP-05 接线后由 QEMU 实测。

#### 3.3.3 SMP-03（批次 B）：次核汇编入口

- **文件锚点**：[start.S](../../src/kernel/privileged/boot/aarch64/start.S)（追加）、[aarch64.ld](../../src/kernel/privileged/link/aarch64.ld)（`ap_boot_info_ptr`）
- **关键更正**：**stub 放 `.text.boot` 即可物理可达**（该段 `VMA == LMA == PA`），**无需**新建段、无需改链接脚本段布局；仅需按 §3.2 加 `_ap_boot_info` 槽与 `ap_boot_info_ptr` 别名。
- **骨架**（`.text.boot` 段内）：

```asm
.global ap_entry_asm
.type ap_entry_asm, %function
// 次核入口：MMU 关 / 物理地址 / 低半区。PSCI cpu_on 的 entry point。
// context_id（x0）= cpu_index。
ap_entry_asm:
    adrp    x10, _ap_boot_info
    add     x10, x10, :lo12:_ap_boot_info
    mrs     x9, CurrentEL
    lsr     x9, x9, #2              // 0b01=EL1 / 0b10=EL2 / 0b11=EL3
    str     x9, [x10, #0x48]        // 记录实测 EL（诊断）
    cmp     x9, #1
    b.ne    ap_bad_el               // 非 EL1 先走失败/降级分支（见 E7）
    ldr     x11, [x10, #0x00]
    msr     ttbr0_el1, x11
    ldr     x11, [x10, #0x08]
    msr     ttbr1_el1, x11
    ldr     x11, [x10, #0x10]
    msr     mair_el1, x11
    ldr     x11, [x10, #0x18]
    msr     tcr_el1, x11
    tlbi    vmalle1
    dsb     sy
    isb
    ldr     x12, [x10, #0x28]
    mov     sp, x12                 // 次核独立栈（高半区 VA）
    ldr     x13, [x10, #0x20]
    msr     sctlr_el1, x13          // 开 MMU（M/C/I）
    dsb     sy
    isb
    ldr     x0, [x10, #0x38]        // cpu_index → ap_main 第 1 参
    ldr     x14, [x10, #0x30]       // ap_main 高半区 VA
    br      x14

ap_bad_el:
    mov     x15, #1
    str     x15, [x10, #0x40]       // 标记 done=1 以便 BSP 超时后读 EL 诊断
    b       ap_bad_el               // PSCI 已上电但环境不满足 ⇒ 自旋占位（BSP 侧超时兜底）
```

  - **EL 实测分支（E7，裁定 2）**：`ap_bad_el` 是**占位**。若首轮实测 `CurrentEL == EL2`，则在此处补 `el2 → el1` 降级序列（复用 `el2_entry` 既有常量：`HCR_EL2`、`SPSR_EL2`、`ELR_EL2`、`eret`），再跳回 EL1 主路径；若实测即 EL1，则删除该分支（保持 F9）。**未实测前不冻结此分支形态。**
  - `_ap_boot_info` 的 `adrp/add` 在 MMU 关态取得物理地址（低半区，`VMA==LMA==PA`）⇒ 开 MMU 后该地址仍在恒等映射内，`x10` 持续有效。
  - Rust 侧声明：`unsafe extern "C" { fn ap_entry_asm(); }`，`cpu_on` 的 `entry_pa = ap_entry_asm as usize as u64`（`.text.boot` 物理地址 == 链接地址）。
- **验证**：随批次 B 的 `-smp 2` 手动实测。

#### 3.3.4 SMP-05（批次 B）：次核 Rust 入口

- **文件锚点**：新建 `arch/aarch64/smp_init.rs`；[mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs)（补读 helper）；[exception.rs](../../src/kernel/privileged/arch/aarch64/exception.rs)（拆 `init_vectors`）
- **关键更正 1**：`mmu.rs` 公开读 helper 仅 `read_ttbr0()` / `read_far()`，**无** `read_mair` / `read_tcr` / `read_sctlr` ⇒ 需补三个 `pub fn read_mair() -> u64` / `read_tcr()` / `read_sctlr()`（各 `mrs` 一行，含 `// SAFETY:` 或以内联 asm 直接实现）。
- **关键更正 2**：`exception.rs` 现 `pub unsafe fn init()` = 设 `VBAR_EL1` + `daifclr #0xF`（**当场开中断**）。`VBAR_EL1` 是 **per-CPU** 寄存器 ⇒ 须拆出 `pub unsafe fn init_vectors()`（只 `msr vbar_el1` + `isb`），`init()` = `init_vectors()` + `daifclr #0xF`。AP 用 `init_vectors()`，**开中断延后**至 per-CPU 状态就绪。
- **骨架**：

```rust
pub fn init() {                       // BSP 侧（interrupt_late_init 内调用）
    smp::init();
    // 探测 CPU 数（dtb.cpus 或回退）；<=1 ⇒ 打印 Single-core 并置 SMP_FULLY_INITIALIZED，返回
    // 逐 AP：准备 ApBootInfo 槽 → dc civac + dsb sy → psci::cpu_on(mpidr, ap_entry_pa, idx) → 轮询 done
    // 收尾：klog_info!(Kernel, "[SMP] online CPUs: {}", smp::get_cpu_count());
}

/// 次核 Rust 入口（由 ap_entry_asm `br` 至此；x0 = cpu_index）。
pub unsafe extern "C" fn ap_main(cpu_index: u64) -> ! {
    let idx = cpu_index as u32;
    // 1) GIC 次核初始化（SMP-04），失败 ⇒ 不登记 + FATAL loop + 打印 reason
    // 2) exception::init_vectors()  // 只设 VBAR_EL1，不开中断
    // 3) per-CPU 四连（对齐 x86_64 ap_entry）：
    //    proc::init_cpu_queue(idx, 0) && proc::init_per_cpu_sched(idx)
    //      && sync::rcu_alloc_cpu(idx) && irq::softirq_alloc_cpu(idx)
    //    任一失败 ⇒ 不登记 + FATAL loop
    // 4) smp::register_cpu(idx)     // idx = Aff0，与 send_ipi 的 target_cpu & 0xF 一致
    // 5) SCHEDULER.init_per_cpu_idle(idx)
    // 6) 后置自检：smp::is_cpu_online(idx) 必须为真，否则 FATAL loop
    // 7) 写 done = 1（Release）
    // 8) arch!(interrupt_enable())  // 开中断（SMP-07 要求"延后开中断"）
    loop { arch!(halt()) }
}
```

  - **`register_cpu(idx)` 语义（更正 3）**：aarch64 `send_ipi` 用 `target_cpu & 0xF`（Aff0），故 `CPU_APIC_IDS` 槽应存 **`cpu_index`（= Aff0）**，保持与 x86_64 消费路径一致。
  - **SMP-07 里程碑/自检内嵌于本步**：成功 `[SMP] online CPUs: {n}`；单核 `[SMP] Single-core system, skipping AP startup`；`cpu_on` 失败 `[SMP] AP {i} CPU_ON failed: {err:?}`；超时 `[SMP] AP {i} did not come online within timeout`。`SMP_FULLY_INITIALIZED` 仅在**期望次核全部上线**时置真（区分"部分成功"）。
  - **中断上下文约束**：AP 在未开中断期间完成 GIC 唤醒与 CPU 接口使能，开中断后进 idle；不得持锁开中断（AGENTS §11）。
- **验证**：`./ci/build.sh all` + clippy；批次 B 的 `-smp 2` 手动实测。

#### 3.3.5 SMP-06（批次 B）：BSP 接线

- **文件锚点**：[arch/aarch64/mod.rs](../../src/kernel/privileged/arch/aarch64/mod.rs)
- **改动**：
  1. 模块声明区加 `mod smp_init;`。
  2. [`interrupt_late_init()`](../../src/kernel/privileged/arch/aarch64/mod.rs#L242-L244) 内追加 `smp::init(); smp_init::init();`（对齐 x86_64 [`interrupt_late_init()`](../../src/kernel/privileged/arch/x86_64/mod.rs#L324-L395) 末尾）。
  3. `interrupt_early_init()` 保持空（GICv3 + VBAR_EL1 由 `entry.rs` 配置）。
- **更正 4（无需改 `lib.rs`）**：`interrupt_late_init()`（[L750](../../src/kernel/lib.rs#L750)）在 `scheduler::init()`（[L770](../../src/kernel/lib.rs#L770-L771)）**之前**，且 AP 的 `init_per_cpu_sched` 是 per-CPU 独立初始化 ⇒ 接线点天然正确。
- **验证**：默认单核 QEMU 须打印 `[SMP] Single-core system, skipping AP startup` 且启动至 EL0 无回归。

#### 3.3.6 SMP-09（批次 C）：QEMU `-smp 2` 断言

- **文件锚点**：[qemu_boot_test.sh](../../scripts/qemu_boot_test.sh#L228-L229)
- **改动**：aarch64 分支 `qemu-system-aarch64` 命令行加 `-smp 2`；在既有断言（`GICv3 ready` / `VFS ready` / EL0 / KPTI）之上新增 grep `[SMP] online CPUs: 2`（命中 ⇒ `ok`，未命中 ⇒ 非零退出/`err`）。**x86_64 分支不动**（§12.2）。
- **验证**：`./scripts/qemu_boot_test.sh aarch64` 命中 `online CPUs: 2`。

#### 3.3.7 SMP-08（批次 D）：host-tests 静态契约

- **文件锚点**：新建 `host-tests/tests/aarch64_smp_contract_test.rs`（对齐既有 `host-tests/tests/arch_apstartup_info_layout_test.rs` 范式）
- **断言**：`PSCI_CPU_ON == 0xC400_0003` / `PSCI_VERSION == 0x8400_0000` / `SYSTEM_OFF`/`RESET` 不变；`-1..=-8` 错误码 → `PsciError` 变体一一对应；`GICR_SGI_OFFSET == 0x1_0000` / `GICR_STRIDE == 0x2_0000`；`ApBootInfo` 的 `size_of == 80` 与 `done@0x40` / `el@0x48` 偏移（镜像常量 + 编译期 assert）。
- **约束**：host 目标（x86_64）不能编译 aarch64 内联汇编 ⇒ 测试**只断言常量/布局**，不调用 `smc`。
- **验证**：`make test-host` 全过。

### 3.4 调研更正（施工前必读）

§10 源码复核相对上一版计划的三处**关键更正**（直接影响施工形态）：

1. **次核 stub 无需新建段/改段布局**：`.text.boot` 已满足 `VMA == LMA == PA`（[aarch64.ld](../../src/kernel/privileged/link/aarch64.ld) fail-closed 布局），stub 追加至该段即物理可达；只需在 `.bootbss` 加 `_ap_boot_info` 槽、在链接脚本加 `ap_boot_info_ptr` 别名。
2. **`exception.rs` 须拆 `init_vectors()`**：`init()` 现为"设 VBAR + 当场开中断"，而 AP 必须**延后**开中断（per-CPU 状态就绪后），故须拆出只设 `VBAR_EL1` 的 `init_vectors()`。
3. **`mmu.rs` 须补读 helper**：现有公开读 helper 仅 `read_ttbr0()` / `read_far()`，缺 `read_mair` / `read_tcr` / `read_sctlr`，SMP-05 准备槽时无值可读。

附加两项工程约束：

4. **缓存一致性**：BSP 写 `ApBootInfo` 槽后须 `dc civac`（按 64B line 清至 PoC）+ `dsb sy`，否则次核（caches off）可能读到陈旧 DRAM（QEMU 不建模，真机必挂）。
5. **`register_cpu` 参数**：aarch64 侧传 `cpu_index`（= MPIDR Aff0），与 `send_ipi` 的 `target_cpu & 0xF` 编码一致。

## 验证门槛

- **AGENTS §2.3 六条门槛全过**（双架构 0w0e / clippy 0 warning / 核心审计 / `make test-host` / `make test-kernel-host` / QEMU 双架构）—— 实测结果写入 SMP-10 详情。
- **专项（本工程成功标准）**：aarch64 QEMU `-smp 2` 启动日志出现 `[SMP] online CPUs: 2`，且 `smp_is_enabled() == true`、`CPU_COUNT == 2`。
- **回归**：aarch64 单核路径（若 QEMU 不带 `-smp`）仍打印 `[SMP] Single-core system, skipping AP startup` 并正常启动到 EL0（不因新增接线破坏单核）。
- **静态契约**：SMP-08 的 host-tests 全过（PSCI 编码 / 错误码映射 / GIC 常量）。
- **架构合规**：新增 unsafe 集中于 `privileged/`（`psci.rs` / `start.S` / `smp_init.rs`），`functions/` 保持 0 unsafe（F1）；`// SAFETY:` 100% 覆盖（F4）；中文注释（F7）；公共 API 中文文档（F8）。

## 风险与回退

- **次核入口 EL 级为假设项（E7）— 已解除**：`-smp 2` 实测次核经 `ap_entry_asm` 成功上线（`online CPUs: 2`），确认进入级为 **EL1**，无需 EL2→EL1 降级序列，stub 形态已冻结。
- **页表复用的一致性风险**：次核装入 BSP 的 `TTBR0_EL1` 物理基址。若 BSP 后续切换 `TTBR0`（用户态切换），次核的 `TTBR0` 与之脱钩 —— 但次核 idle 期间只跑内核高半区代码，`TTBR1` 覆盖内核地址空间 ⇒ 影响受限；**登记为简化点**（`// SIMPLIFIED:`），后续若需次核参与用户态运行须重审。
- **`mm/kpti_aarch64.rs` 单核假设 — 已复核（注释已订正 + 内存序结论）**：原注释「aarch64 无 SMP（`smp_init.rs` 仅 x86_64），故全局量用普通 `AtomicU64` 即可」已修正为「SMP 前提」章节（区分 boot 期发布字段与每核活跃值）。复核结论二分为：
  - **boot 期一次性发布、此后只读**（`ready` / `kernel_ttbr0` / `kernel_ttbr1` / `tramp_ttbr1`）：内存序**配对完整、通过** —— `kpti_init()` 以 `Release` 依次存三表后 `ready.store(1, Release)`；汇编表切换序列 `dsb ish → msr → isb → tlbi vmalle1is` 符合 ARM ARM。
  - **★ 每核活跃值**（`user_ttbr0` 偏移 24 / `tramp_save0` 40 / `tramp_save1` 48）：**结构性缺陷** —— 这三者属"每核当前活跃状态"，却存放于**单实例** `KPTI_GLOBALS`，且入口/出口汇编用普通 `str`/`ldr` 按固定偏移访问。多核并发 EL0 时，A 核入口保存的 `x3`/`x4`（用户栈/返回 PC 中转）与 B 核写入的用户页表会**跨核互相覆盖** ⇒ 用户寄存器损坏、以他核页表 `eret`。反证：同库 `mm/copy_user.rs` 的 `PER_CPU_EXCEPTION_CTX[cpu]` 已采用按核数组范式。**当前未爆发**：AP 上线后停在 idle、未调度用户任务，故无并发 EL0。**已立项为独立专项**（KPTI 入口/出口状态 per-CPU 化，见文末「后续专项登记」），本轮不改行为（AGENTS §9.1）。
- **aarch64 TLB shootdown 发送路径未覆盖**（§1 关键认知 / DECISION-082 范围声明）：本工程使 aarch64 具备多核在线，但"主动广播 SGI 13 请求他核失效 TLB"的发送调用面当时仍缺。在多核下若 aarch64 有页表修改而依赖该广播，**可能产生 TLB 陈旧条目**。**处置（已闭合）**：由后续独立工程 DECISION-083（[aarch64-tlb-shootdown-send.md](./aarch64-tlb-shootdown-send.md)）实装发送路径并接入架构无关公共层 `mm/deferred_free.rs`；其施工期实证同时修复了 SGI 13/14 未使能与 `send_ipi` 目标编码两处缺陷（ST-09）。QEMU `-smp 2` 已验证 `[SMP] TLB shootdown #N` 与 `IRQ: intid=13`。
- **QEMU PSCI 实现差异**：QEMU virt + `-cpu max` 的 PSCI 版本应为 v0.2/v1.x，`CPU_ON` 可用；若某环境返回 `NOT_SUPPORTED`，BSP 应打印并退化为单核（SMP-07 的失败路径），不得 panic。真机（若有）PSCI 差异须另行验证 —— 本工程验收以 QEMU 为准。
- **次核栈分配**：`_boot_stack_top` 仅 256 KiB 单核栈，次核栈须独立分配。若分配失败，`cpu_on` 前应检测并跳过对应 AP（fail-fast 打印）。
- **回退**：本工程按模块可独立回退 —— 撤 `interrupt_late_init()` 接线（SMP-06）+ 移除 `-smp 2`（SMP-09）即恢复单核行为；PSCI `CPU_ON` 封装、GIC 参数化、`smp_init.rs` 可保留为"未接线"状态（但须注意 F9：未接线的死代码不允许，故完整回退须整轮撤销）。

## 后续专项登记：KPTI 入口/出口状态 per-CPU 化

> 来源：本工程 SMP-10 收口时的 `mm/kpti_aarch64.rs` 内存序复核（见「风险与回退」末二条）。**本轮不改行为**（AGENTS §9.1 决策先记录再施工），仅登记立项待排期。
>
> **结案**：本专项已由后续独立工程 [smp-ap-user-scheduling.md](./smp-ap-user-scheduling.md)（**DECISION-084**，APS-04）落地 —— 该工程使双核并发 EL0 成为现实，故先修本缺陷。实现要点：每核活跃值（`user_ttbr0`/`tramp_save0`/`tramp_save1`）迁入 `KPTI_CPU_GLOBALS: KptiCpuStateArray`（按核数组，槽内偏移 0/8/16），槽基址经 `kpti_bind_cpu(cpu_index)` 写入 `TPIDR_EL1`（BSP 由 `kpti_init`、AP 由 `ap_main` 步骤 4.1 调用），入口/出口汇编一律 `mrs ... tpidr_el1` 取基址（不再现算 `MPIDR_EL1`，入口无空闲 GPR 可用）；boot 期只读字段仍留单实例 `KPTI_GLOBALS`。QEMU `-smp 2` 双核并发 EL0 通过，无跨核污染。

- **KPTI-PCPU-01. KPTI 入口/出口每核活跃状态 per-CPU 化**
  - 描述：aarch64 KPTI 的入口/出口路径中，`user_ttbr0`（偏移 24）、`tramp_save0`（40）、`tramp_save1`（48）三个字段承载**每核当前活跃状态**（本核在 EL0 期间的用户页表，以及入口中转保存的用户 `x3`/`x4`），却与 boot 期只读字段同置于**单实例** `KPTI_GLOBALS`。多核并发 EL0 时相互覆盖，须按核隔离。
  - 方案（待排期细化）：
    1. **首选 = 按核数组**：对齐同库 `mm/copy_user.rs` 的 `PER_CPU_EXCEPTION_CTX[cpu]` 范式，把上述三字段改为 `[AtomicU64; MAX_CPUS]`，入口/出口汇编按 `MPIDR_EL1 & 0xFF`（Aff0，与 `smp::register_cpu` 的 `cpu_index` 一致）索引；boot 期只读字段仍留单实例。
    2. **备选 = `TPIDR_EL1` per-CPU 基址**：为每核在 `TPIDR_EL1` 存一份 KPTI 状态基址（aarch64 无专门的 per-CPU 基址寄存器约定，`TPIDR_EL1` 当前未使用；`TPIDRRO_EL0` 仅作入口临时中转且被清零），入口/出口经 `mrs tpidr_el1` 取得基址后偏移寻址。优点 = 汇编零 MPIDR 计算、扩展不再受 `MAX_CPUS` 数组上限约束；缺点 = 需在次核上线路径与 BSP 初始化中额外维护基址，且与调度切换路径（`context.rs` 刷新 `user_ttbr0` 的两处 `str x4, [x3, #24]`）须同步改造。
  - 状态：[X]（由 DECISION-084 / APS-04 落地结案，见上「结案」）
  - 详情（根因）：复核对证据 —— `exception.rs` 的 `handle_el0_sync` / `handle_el0_irq` / `el0_return` / `kpti_enter_user_trampoline` 均以固定字节偏移访问 `KPTI_GLOBALS`（无按核索引）；`context.rs` 的内核续跑路径与 `.Lctx_enter_el0` 两处亦同。
  - 详情（影响面）：A 核入口保存的用户 `x3`/`x4` 被 B 核覆盖、或 A 核 `el0_return`/trampoline 读到 B 核写入的用户页表 ⇒ 用户寄存器损坏、以他核页表 `eret`（越权/崩溃）。**触发条件 = 两核同时处于 EL0**；当前 AP 上线后停在 idle、不调度用户任务，故潜伏未爆发。
  - 详情（验证门槛）：满足 §2.3 六门槛；**专项判据 = QEMU `-smp 2` 下双核并发 EL0**（需先具备"AP 参与用户态调度"能力，属本专项或其后继工程的前置），观测无跨核污染；静态侧 = 汇编偏移/索引与 Rust 布局的契约测试。
  - 详情（依赖 / 范围外）：① 前置依赖 = AP 能参与用户态调度（本工程仅 bring-up 到 idle）；② `context.rs` 的两处 `str ..., [x3, #24]` 属**同核**已处理路径（任务在核间迁移时作为任务恢复动作重写，语义正确），per-CPU 化后须复核其与按核隔离的交互；③ aarch64 无 per-CPU 基址寄存器约定，方案 2 的 `TPIDR_EL1` 占用须评估与既有约定（见本文件 §3.4 / `aarch64-kernel-fp-free.md`）的兼容性。
