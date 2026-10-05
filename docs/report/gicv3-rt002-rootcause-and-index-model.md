# aarch64 GICv3 偶发挂起（ISSUE-RT-002）根因定位与统一索引模型重构报告

> 本轮把 ISSUE-RT-002 从"不可稳定复现、根因未定位"推进到"**定位并消除一处确定性根因缺陷**"：aarch64 GIC Distributor 初始化历史上**未显式置位亲和路由使能位（ARE）**，而 QEMU 强制 `ARE=1` 掩盖了该缺陷，在不强制 ARE 的真机上 SGI（内核 IPI 13/14/7）与 SPI 分发失效，表现为偶发挂起。修复方式为显式置 ARE 并等待 `GICD_CTLR.RWP` 清零，删除失效的 `GICD_ITARGETSR` 死写，并把后置条件自检（含 ARE fail-fast）接入初始化路径。同轮完成**统一索引模型**重构（逻辑索引为键 + 显式硬件 id 映射，BSP/AP 登记一致化）与 **GICR_TYPER 亲和扫描**（替代 redistributor 帧的线性推算）。§2.3 六验证门槛全绿。

本报告是 ISSUE-RT-002 本轮排查工作区的一次性快照。报告冻结当时事实，作为后续制定 plan 与启动修复工程的输入依据。所有结论均附源码位置或命令证据。

## 一、背景与现象

ISSUE-RT-002 记录的是 aarch64 平台 GICv3 初始化或中断处理**偶发挂起**（P0）。此前多轮复验（含真多核 `-smp 2` 压测 50 次、有界时序搜索五场景 96 次）在 QEMU/TCG 上**均无法稳定复现**，结论停留在"QEMU TCG 时序偶发"。上一轮已把纯判定逻辑剥离到架构中立模块 [gic_logic.rs](../../src/kernel/privileged/arch/gic_logic.rs) 并在 host 侧定点断言失败分支，但对真根因仍未定位。

关键观察来自上一轮 host 单测所**固化的事实**："当前内核未置 ARE、恒走 ITARGETSR legacy 臂"。该事实与本轮排查相互印证，指向 ARE 缺陷。

## 二、根因定位：ARE 未显式置位

### 2.1 缺陷

GICv3 的 `GICD_CTLR` 中，`ARE_S`（bit4）与 `ARE_NS`（bit5）决定中断是走**亲和路由**还是 legacy `GICD_ITARGETSR` 模型：

- 内核跨核 IPI 经 `ICC_SGI1R_EL1` 投递，**以 `ARE=1` 为前提**；
- SPI 经 `GICD_IROUTER` 分发，同样**以 `ARE=1` 为前提**。

原 [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs) 的 `init_distributor()` 在设置分组/优先级/使能时**从未写入 ARE 位**，仅写入 `0x3`（`EnableGrp0 | EnableGrp1NS`），**依赖平台默认值**。

### 2.2 为何此前未被发现

QEMU 的 GICv3 模型默认（且强制）`ARE=1`，掩盖了该缺陷：在 QEMU 上 `ICC_SGI1R_EL1` 与 `GICD_IROUTER` 照常工作，因此压测"看起来正常"。而在**不强制 ARE 默认置位**的真机上，Distributor 停在 legacy 模型：

- 内核 SGI（`TLB_SHOOTDOWN=13` / `RESCHEDULE=14` / `FREG_RECOVERY=7`）经 `ICC_SGI1R_EL1` 发出但**不被分发**；
- SPI 经 `GICD_IROUTER` 路由**不生效**。

其后果是跨核 IPI / 设备中断偶发丢失，表现为调度、TLB shootdown、定时器路径上的**间歇性停滞（挂起）**，与台账"偶发挂起"现象一致。

### 2.3 修复

`init_distributor()` 现按 ARM IHI 0069 的约束分步执行（`ARE` 仅在所有 Group 使能位为 0 时可写，故须先禁用 Distributor）：

1. `GICD_CTLR ← 0`（禁用），等待 `RWP` 清零；
2. **显式置 ARE**：`GICD_CTLR ← GICD_CTLR_ARE_MASK`，等待 `RWP` 清零；
3. 配置 SPI 分组（Group 1NS）与优先级；
4. 使能 Distributor：`GICD_CTLR ← ARE | EnableGrp0 | EnableGrp1NS`，等待 `RWP` 清零。

配套改动：

- 新增 `wait_ctlr_rwp()`：自旋等待 `GICD_CTLR.RWP`（bit31）清零，超限返回 `Err`（fail-closed），取代"写后立即下一步"的隐式假设；
- 删除失效的 `GICD_ITARGETSR` 死写（ARE=1 后该寄存器不再定义）；
- `route_spi_to_cpu0` 改用 64 位 `GICD_IROUTER`；
- `verify_post_conditions()` 的回读自检新增 ARE 判据：初始化后若 `ARE` 未置位即 fail-fast 报错，把"静默挂起"转为"确定性报错"；
- 相关判定常量与纯函数（`GICD_CTLR_ARE_MASK` / `GICD_CTLR_RWP_MASK` / `ctlr_rwp_timed_out` / `verify_post_conditions` 等）集中到 [gic_logic.rs](../../src/kernel/privileged/arch/gic_logic.rs)，生产路径与 host 测试判据同源。

## 三、统一索引模型（逻辑索引为键）

ISSUE-RT-002 的另一层隐患是**核标识语义不统一**：内核大量 per-CPU 数组（调度器 / softirq / RCU / CpuQueue / TLB 代）以"稠密逻辑索引"寻址，而 `get_current_cpu()` / IPI 目标历史上直接使用**硬件 id**（x86 LAPIC id / aarch64 MPIDR 派生值）。稀疏拓扑下二者不一致会导致索引错位，进而表现为难以复现的运行期异常。

本轮将标识模型统一为：

- **逻辑索引**：由 [`register_cpu`](../../src/kernel/privileged/smp/mod.rs#L95-L107) 按上线次序**稠密分配**，值域 `[0, MAX_CPUS)`，与全部 per-CPU 数组下标一致；
- **硬件 id**：x86 为 Local APIC id，aarch64 为**紧凑亲和 id** `(Aff3<<24)|(Aff2<<16)|(Aff1<<8)|Aff0`（完整 4 级亲和 packed 入 u32）；
- **唯一映射源**：`CPU_HW_IDS[逻辑索引] == 硬件 id`。BSP 在 `init()` 中登记槽位 0，AP 在其启动路径登记后续槽位，BSP/AP 登记路径一致化；
- [`current_cpu_index()`](../../src/kernel/privileged/smp/mod.rs#L69-L80) 读本核硬件 id 后在已登记前缀内**线性扫描**反查本核逻辑索引（核数有界、纯读、可在中断上下文调用）；
- [`get_current_cpu()`](../../src/kernel/privileged/smp/mod.rs#L88-L90) 语义修正为返回**逻辑索引**（历史上直返硬件 id）；
- [`get_hw_id()`](../../src/kernel/privileged/smp/mod.rs#L117-L122) 提供逻辑索引 → 硬件 id 的反向解析；IPI 发送入口 [`send_tlb_invalidate_ipi`](../../src/kernel/privileged/smp/mod.rs#L128-L134) / [`send_reschedule_ipi`](../../src/kernel/privileged/smp/mod.rs#L150-L156) 均以**逻辑索引为键**，内部经 `get_hw_id` 解析后交由架构层寻址；未登记槽位（哨兵 `0xFFFF`）静默返回。

调用点核查：`rcu.rs`、`cpu_queue.rs`、`scheduler.rs`（含 `find_idle_cpu` 返回逻辑索引并交 `resched_cpu` / `send_reschedule_ipi`）全链以逻辑索引为键，链路自洽。架构层 `send_ipi(target_cpu, vector)` 的 `target_cpu` 语义为**硬件 id**；x86_64 实现内 `target_cpu as u8` 是**受控的硬件字段窄化**（Local APIC 目标字段宽度），已附 `#[expect(clippy::cast_possible_truncation)]` 与中文说明，非缺陷，保持不变；aarch64 实现把 packed 亲和 id 解码回 `ICC_SGI1R_EL1` 的 `Aff3/Aff2/Aff1/Aff0(+TargetList)` 字段。

## 四、GICR_TYPER 亲和扫描（redistributor 帧定位）

Redistributor 帧的定位原用**线性推算**（`rd = GICR_BASE + GICR_STRIDE * cpu_index`），该假设仅在"GICR 区域紧凑、帧序与逻辑索引一致"时成立，稀疏/非连续拓扑下会定位到错误帧。

本轮改为 **GICR_TYPER 亲和扫描**：[`redist_frames()`](../../src/kernel/privileged/arch/aarch64/gic.rs#L173-L185) 以当前核硬件 id（紧凑亲和值）为键，逐帧回读 `GICR_TYPER[63:32]` 的 `Affinity_Value`（其 `Aff3.Aff2.Aff1.Aff0` 布局与 packed 亲和无歧义）、以 `GICR_TYPER.Last`（bit4）界定扫描终点，命中即得该核的 `(rd, sgi)`。扫描上限 `REDIST_SCAN_MAX_FRAMES = 1024`（128 MiB，落在内核 `L2_DEVICE [0, 0x4000_0000)` 窗口内，扫描安全）。判据 `redist_affinity` / `redist_is_last` / `find_redist_frame` 复用 [gic_logic.rs](../../src/kernel/privileged/arch/gic_logic.rs) 纯逻辑，host 侧与生产路径同源；未命中即返回 `Err`（fail-closed），不再回退到线性推算。`init_per_cpu()` 随之去参（BSP 与 AP 共用同一实现，以当前核硬件 id 定位帧）。

## 五、BSP_ID 冗余收口

统一索引模型落地后，`smp/mod.rs` 中原有的 `BSP_ID` 静态量（仅 `store`、无任何 `load`，为预存 write-only）与新的 `CPU_HW_IDS[0]` 语义完全重复。经用户裁定本轮随改删除：移除 `BSP_ID` 声明与其 `store` 行，零行为变化，消除冗余状态。

## 六、验证

命令与结果（本轮工作区）：

| 门槛 | 命令 | 结果 |
|---|---|---|
| 双架构构建 | `./ci/build.sh all` | `Passed: 5  Failed: 0` |
| aarch64 构建 | `./ci/build.sh aarch64` | `Passed: 2  Failed: 0` |
| 核心审计 + clippy + fmt | `bash ci/audit.sh quick` | `EXIT=0`（含 FP-06 白名单外 FP/SIMD = 0；SAFETY 覆盖 1872/1872；6 安全不变式 PASS；双架构 check；clippy pedantic + kernel_test 维 + host-test 维；rustfmt 三 crate） |
| host-tests | `./ci/build.sh all`（内嵌）/ `make test-host` | 11 passed / 0 failed |
| 内核 host 单测 | `make test-kernel-host` | 949 passed / 0 failed |
| QEMU 集成 | `TIMEOUT_QEMU=30 ./scripts/qemu_boot_test.sh all` | 2/2 通过；aarch64 断言 `GICv3 初始化成功` + `online CPUs: 2` + KPTI-09 + 双核并发 EL0 + EL0 中断可达 |
| GIC 启动压测 | `./scripts/gic_stress_test.sh 20` | 20/20 通过，0 失败 |

aarch64 QEMU 侧关键里程碑（[qemu_boot_aarch64.log](../../other/build/log/qemu_boot_aarch64.log)）：

```
✓ [aarch64] GICv3 初始化成功 (ISSUE-RT-002 回归通过)
✓ [aarch64] SMP 双核上线 (online CPUs: 2)
✓ [aarch64] EL0 中断可达 (用户态可被抢占, ISSUE-RT-005)
```

## 七、结论与遗留

**结论。** 本轮定位并消除了 aarch64 GICv3 初始化中的一处确定性根因缺陷：**未显式置位 ARE**。该缺陷在 QEMU（强制 `ARE=1`）下被掩盖，而在不强制 ARE 的真机上会导致内核 SGI 与 SPI 分发失效，与 ISSUE-RT-002"偶发挂起"现象一致。修复为显式置 ARE + `RWP` 等待 + 删 `ITARGETSR` 死写 + ARE 后置条件 fail-fast；同时完成统一索引模型（逻辑索引为键）与 GICR_TYPER 亲和扫描，消除核标识语义不统一与帧定位的隐式假设。

**遗留。**

- 原始偶发挂起在 QEMU/TCG 上**仍不可稳定复现**，故本轮对 ARE 缺陷的"根因"定性依据为**代码事实 + 平台差异分析**（QEMU 强制 ARE 掩盖 vs 真机不强制），**真机复验**（ISSUE-RT-003 载体）仍是最终确认手段。
- EL0 中断可达（ISSUE-RT-005）已在本轮回归断言中通过。
- `ci/audit.sh` 中 `audit_deadlock_matrix` 的 `x86_64/smp_init.rs` 一项 HIGH（`NON_IRQ_SAFE_LOCK_USE`）为**无关预存项**（阶段 0 已列），本轮未改动，保持报告性输出。
