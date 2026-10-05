# aarch64 TLB shootdown 发送路径工程（deferred-free 公共层）

> 本文件是 [aarch64-smp-bringup.md](./aarch64-smp-bringup.md)（DECISION-082）**范围声明之外**的后续独立工程。
>
> **前提订正（施工期实证）**：本文初稿曾以「DECISION-082 使 aarch64 具备『多核在线 + 可收 SGI 13』」为前提。施工期 QEMU 实证**证伪**了「可收 SGI 13」——
> - 次核上线（`online CPUs: 2`）成立；但 **SGI 13/14 从未在 per-CPU `GICR_ISENABLER0` 使能**（该寄存器为每核 Redistributor 私有状态，复位值 0），接收侧 `intid == 13` 分支**永不触发**；
> - 且 `arch/aarch64/mod.rs::send_ipi` 的目标编码把 Aff0 误写入 `ICC_SGI1R_EL1` 的 Aff1 字段（`1 << (16+aff0)`），**SGI 永不投递**。
>
> 二者使"发送路径"在接收侧静默失效（延迟释放帧永久滞留）。本工程据此新增 **ST-09**（每核 SGI 使能收敛 + SGI 目标编码修复）作为 ST-05/ST-06 生效的前置条件。
>
> 目标：把 x86_64 私有的「代发布 + 批次链 + 延迟释放」机制**抽取为架构无关公共层**，aarch64 侧接入同一实现（**不写平行实现**，对齐项目硬约束「内核内部并行实现必须统一为单一规范实现」）。
>
> 关联：`docs/plan/tlb-shootdown-epoch.md`（epoch 框架权威）、`docs/plan/smp-ipi-protocol.md`（IPI 骨架）。

## 1. 根因与性质

**根因**：aarch64 的页表修改只用 `tlbi vaae1is`（**IS 后缀 = Inner Shareable 广播**）完成跨核 TLB 失效，随后**立即**归还页表页与数据帧；而 x86_64 通过「批次链 + TLB 代计数 + 延迟释放」保证"全部在线核已失效后才归还"。

**为什么广播 TLBI 不足以替代延迟释放**：ARM 下 broadcast TLBI 完成后，目标 PE 的**后续**遍历不再命中旧条目；但**已经开始的 in-flight 页表遍历**不受保证（需目标 PE 执行上下文同步）。页表页被立即 `free_page` 后若被并发遍历读到，即为 UAF。故 aarch64 与 x86_64 一样需要"驱动目标核执行 `dsb` 并声明追平代"的发送路径 —— 这正是 SGI 13 接收分支（`smp::tlb_catch_up_local` + `tlb_probe_report`）的用途。

**性质判定**：**能力级缺口 + 结构性不一致**。aarch64 单核下无害（无远程核）；多核下页表拆除存在 in-flight 遍历窗口。同时 `mm::release_frame_locked` 存在架构分叉（x86 延迟 / aarch64 立即），属"平行实现"形态。

**前提变更**：`tlb-shootdown-epoch.md` 的 D7 与超范围项（L21、L219）以「aarch64 无 AP 上线路径 ⇒ `SMP_ENABLED` 恒 false ⇒ 发送侧不可运行验证」为由排除 aarch64 发送侧。该前提**已被 DECISION-082 解除**，仅「不建平行实现」约束仍有效。

## 2. 实证结论（源码复核）

| # | 命题 | 复核结论 |
|---|---|---|
| E1 | x86_64 发送点在 `release_lock` 尾部 | **成立**。`vmm_x86_64.rs:2075` 的 `release_lock` 在**仍持锁**时读-清 `TLB_SHOOTDOWN_NEEDED`(:2078) 并摘走 `BATCH_HEAD`(:2082)，解锁后 `tlb_gen_publish_and_shoot()`(:2097)，再 `drain_pending`/`settle_batch`(:2110-2115) |
| E2 | 全仓 `tlb_gen_publish_and_shoot()` 调用者仅 x86_64 | **成立**。仅 `vmm_x86_64.rs:2097`（生产）与 :2354（探针自检）—— aarch64 无调用者 |
| E3 | 发送原语本身已架构无关 | **成立（但接线有缺陷，见订正）**。`smp::tlb_gen_publish_and_shoot()` 内部走 `send_tlb_invalidate_ipi`→`arch!(send_ipi(cpu,0xFD))`；接收侧 `intid == 13` 派发 `tlb_catch_up_local`。**无需新增发送原语**。订正：初稿称 aarch64 `send_ipi` 已正确编入 `ICC_SGI1R_EL1` —— 实测其目标编码有误（Aff0 误置入 Aff1 字段），且接收侧 SGI 13 根本未使能；二者由 ST-09 修复 |
| E4 | 延迟释放机制全在 `vmm_x86_64.rs` 私有 | **成立**。静态量(:65/:71/:76/:82/:89/:99)、`frame_link_*`(:157)、`free_chain`(:167)、`settle_batch`(:186)、`drain_pending`(:214)、`defer_free`(:2230) 均在 x86_64 专属文件 |
| E5 | aarch64 无任何 shootdown/deferred 逻辑 | **成立**。`vmm_aarch64.rs:284` 的 `release_lock` 仅清锁 + 恢复中断；`:362` 的 `free_table` 直接 `get_pmm().free_page()` |
| E6 | 架构分叉已落在 `mm/mod.rs` | **成立**。`release_frame_locked`(:306-311) 为 `#[cfg(target_arch="x86_64")] defer_free` / `#[cfg(target_arch="aarch64")] free_page`，注释即 D7 落地 |
| E7 | 帧归还入口已收敛为单点 | **成立**。x86_64 的 `unmap_page_in_table`/`destroy_page_table` 与架构无关的 `cow.rs:439/506` 一律经 `super::release_frame_locked`；aarch64 侧同经该入口（`vmm_aarch64.rs:858/1480`）。故**统一该单点即可覆盖绝大部分帧归还** |
| E8 | aarch64 页表页释放另有独立路径 | **成立**。aarch64 的中间页表页经私有 `free_table`（`vmm_aarch64.rs:875/888/904/970/982/983/1023/1024/1025/1085/1132/1133/1433/1448/1463/1484`），**不**经 `release_frame_locked` ⇒ 需一并接入延迟释放 |
| E9 | 编号可用 | **成立**。现存最新裁定为 DECISION-082，本工程取 **DECISION-083** |

## DECISION-083（aarch64 TLB shootdown 发送路径实施裁定）

- **描述**：aarch64 需要发送路径，但机制已存在且属 x86_64 私有；施工前需裁定抽取方式与验证强度。
- **方案（用户裁定）**：
  1. **实现路径 = 方案 B（相对完整，抽公共层）**：把 E4 的机制从 `vmm_x86_64.rs` 迁入**架构无关公共模块**，x86_64 与 aarch64 共用同一实现；**不**在 aarch64 复制一份（违反「禁止内核平行实现」硬约束）。
  2. **不新增发送原语**（E3）：aarch64 复用 `smp::tlb_gen_publish_and_shoot()` 与既有 SGI 13 接收分支。
  3. **验证强度**：满足 §2.3 六门槛；专项判据 = QEMU `-smp 2` 下 aarch64 出现 `[SMP] TLB shootdown #N gen=… targets=…` 日志且启动无回归；host-tests 增加公共层契约测试。
  4. **前置修复授权（施工期追加）**：用户裁定「取长期最优」→ 将 SGI 13/14/7 的使能**收敛到每核唯一入口** `gic::init_per_cpu`（消除分散使能点），并修复 `send_ipi` 的 `ICC_SGI1R_EL1` 目标编码（见 ST-09）。
- **状态**：[X]
- **详情（范围）**：本工程含"发送侧 + 页表页/帧延迟回收接入"；**不含**页级 shootdown（地址载荷）扩展（`tlb-shootdown-epoch.md` 已登记为扩展项）。

## 任务清单

- **ST-01. 抽取公共层 `mm/deferred_free.rs`**
  - 描述：把 E4 的机制迁入架构无关模块，x86_64 行为**不变**（纯重构）。
  - 方案：新建 `src/kernel/privileged/mm/deferred_free.rs`，迁入静态量、`frame_link_*`、`free_chain`、`settle_batch`、`drain_pending`；对外暴露 `defer_free(frame)` / `mark_remote_shootdown()` / `take_batch()` / `clear_shootdown_flag()` / `release_tail(shootdown_needed, batch)` / `admitted()` / `released()` / `pending_nonempty()`。
  - 状态：[X]
- **ST-02. x86_64 接线到公共层**
  - 描述：`vmm_x86_64.rs` 删除本地副本，改调公共 API（行为等价）。
  - 方案：`release_lock` 尾部改调 `deferred_free::release_tail(...)`；`flush_tlb_remote` 的置位改 `mark_remote_shootdown()`；`defer_free` 方法转调；计数读取改 `deferred_free::admitted()`。
  - 状态：[X]（实测锚点：`vmm_x86_64.rs:1881/1885/1896` 摘批次与 `release_tail`；`:1135/:1967` `mark_remote_shootdown`；本地副本已删，F9 无残留）
- **ST-03. aarch64 帧归还统一走延迟释放**
  - 描述：消除 E6 的架构分叉。
  - 方案：`mm/mod.rs` 的 `release_frame_locked` 去掉 `#[cfg]` 分叉，统一调 `deferred_free::defer_free`；更新函数文档（含锁序契约）。
  - 状态：[X]（实测锚点：`mm/mod.rs:310-312` 无分叉，单一 `deferred_free::defer_free`）
- **ST-04. aarch64 页表页接入延迟释放**
  - 描述：E8 的私有 `free_table` 路径不入批，须一并接入。
  - 方案：`vmm_aarch64.rs` 的 `free_table` 改为经 `deferred_free` 入批（保持"持 VMM_LOCK"前提）；**精准分派**（用户裁定）：16 处调用点中 7 处不持 `VMM_LOCK`，不得整体改道（否则违反锁内单写者前提）；仅对**持锁**调用点走 `deferred_free::defer_free`，其余保持立即释放并登记。
  - 状态：[X]（实测锚点：`vmm_aarch64.rs:402` 入批实现；`mark_remote_shootdown` 于 `:823/:967/:1513`）
- **ST-05. aarch64 `release_lock` 尾部接入发送**
  - 描述：让 aarch64 真正发布代 + 广播 SGI 13。
  - 方案：`vmm_aarch64.rs` 的 `release_lock` 对齐 x86_64 形态（仍持锁时读-清标志 + 摘批次 → 解锁 → `release_tail`）。
  - 状态：[X]（实测锚点：`vmm_aarch64.rs:287/291/303`）
- **ST-06. aarch64 页表修改登记远程失效**
  - 描述：aarch64 的 `tlbi vaae1is` 处需登记"本临界区需远程失效"。
  - 方案：在 `unmap_page_in_table` 等"替换/覆盖既有翻译"的路径调用 `mark_remote_shootdown()`；纯新建映射不登记（对齐 x86_64 S-9 语义）。
  - 状态：[X]
- **ST-07. host-tests 契约 + QEMU `-smp 2` 验证**
  - 描述：静态契约 + 运行期判据。
  - 方案：host-tests 增加公共层行为契约（如 `defer_free`/`release_tail` 的入链与计数、越界/空链 fail-closed）；`scripts/qemu_boot_test.sh` aarch64 分支在既有断言之上增加 `TLB shootdown` 里程碑（fail-closed）。
  - 状态：[X]（实测：`host-tests/tests/mm_deferred_free_contract_test.rs` 全过；`qemu_boot_test.sh` aarch64 分支新增 `[SMP] TLB shootdown` 断言）
- **ST-08. 文档同步**
  - 描述：消除失效前提与分叉注释。
  - 方案：更新 `tlb-shootdown-epoch.md` D7 与超范围项（L21/L219）；更新 `mm/mod.rs` 与 `vmm_aarch64.rs` 的架构分叉注释；在 `unresolved-issues-2026-08-09.md` 登记本工程 + 新增 ST-09 的 SGI 缺陷订正。
  - 状态：[X]
- **ST-09. 每核 SGI 使能收敛 + `ICC_SGI1R_EL1` 目标编码修复（施工期新增，用户裁定「取长期最优」）**
  - 描述：ST-05/ST-06 的发送路径在接收侧静默失效，根因二：① SGI 13/14/7 未在 per-CPU `GICR_ISENABLER0` 使能；② `send_ipi` 目标编码把 Aff0 误置入 Aff1。
  - 方案：
    1. SGI 编号集中定义于 `gic.rs`（`TLB_SHOOTDOWN_SGI`/`RESCHEDULE_SGI`/`FREG_RECOVERY_SGI`），供 `exception`（接收路由）与 `freg`（触发）复用，消除编号分散；
    2. 新增通用原语 `gic::enable_sgi`，在**每核唯一中断入口** `gic::init_per_cpu` 内使能全部内核 SGI（BSP 经 `gic::init`、AP 经 `ap_main` 共用）——确保每核 Redistributor 私有使能位均置位；
    3. 修复 `arch/aarch64/mod.rs::send_ipi`：目标 Aff0 编码为 `TargetList` 的对应位（`1 << aff0`），修正初稿误置 `1 << (16+aff0)`（Aff1）；
    4. 删除因收敛而成为死代码的 `gic::gicr_sgi_write` 与 `freg::enable_freg_sgi`（F9）。
  - 状态：[X]（实测：QEMU `-smp 2` 出现 `IRQ: intid=13 count=2..5` 与 `[SMP] TLB shootdown #1/#2/#3 gen=1/2/3`；host 契约测试 `per_cpu_gic_enables_all_kernel_sgis()` 8/8）

## 施工规划

### 批次与依赖

| 批次 | 含任务 | 依赖 | 出口判据 | 状态 |
|---|---|---|---|---|
| **1A** | ST-01 + ST-02 | 无 | 纯重构，x86_64 行为不变；§2.3 六门槛全过（含 QEMU x86_64 TLB shootdown 日志与探针） | [X] |
| **1B** | ST-03..ST-06 + ST-09 | 1A | aarch64 接线；§2.3 六门槛全过；`-smp 2` 出现 `[SMP] TLB shootdown` 且启动至 EL0 无回归 | [X] |
| **1C** | ST-07 + ST-08 | 1B | host-tests 新契约全过；QEMU 双架构断言全过；文档前提订正 | [X] |

依赖关系：`1A → 1B → 1C`。1A 可独立验证与回退（x86_64 行为等价）。**ST-09 并入 1B**：其缺陷使 1B 的发送路径在接收侧静默失效，须与 ST-05/06 同批修复并同批验证（否则 `[SMP] TLB shootdown` 断言不成立）。

### 关键约束

- **不写平行实现**：aarch64 只做"接线 + 置位点"，机制必须单一实现在 `mm/deferred_free.rs`。
- **锁序**：`BATCH_HEAD` 单写者前提 = 持 `VMM_LOCK`；`release_tail` 必须在**锁外、中断开启**后调用（对齐 x86_64 :2088-2092）。
- **同一帧只能入链一次**（`defer_free` 以帧前 16 字节为节点）——aarch64 页表页接入时须保证同一页不重复入批。
- **F9**：迁移后 x86_64 侧不得残留未使用的静态量/函数。

## 验证门槛

- AGENTS §2.3 六条门槛全过（双架构 0w0e / clippy 0 warning / 核心审计 / `make test-host` / `make test-kernel-host` / QEMU 双架构）。
- 专项：aarch64 QEMU `-smp 2` 出现 `[SMP] TLB shootdown #N gen=… targets=…`；x86_64 QEMU 的 `[VMM] deferred-free admitted_total=N released_total=N pending=false` 行为不回归。
- 架构合规：新增 unsafe 仅限 `privileged/`；`// SAFETY:` 100% 覆盖（F4）；中文注释（F7）。

## 风险与回退

- **重构波及 x86_64 生产路径**：1A 必须保证行为等价（同序、同内存序）；回退 = 恢复 `vmm_x86_64.rs` 私有副本。
- **aarch64 `free_table` 调用点上下文不一**（已按 ST-04 精准分派化解）：16 处调用点中 7 处不持 `VMM_LOCK`，保持立即释放（不改道，避免违反锁内单写者前提）；持锁调用点走 `deferred_free::defer_free`。
- **延迟释放引入滞留**：若某核无法追平代，帧滞留 pending 链（`DEFERRED_FREE_RELEASED` 不增长）——按 x86_64 既有诊断文案观测，不引入新机制。**滞留的结构性预期**：排空点唯一（各架构 `release_lock` 出口的 `release_tail`），若某轮 shootdown 后不再有新的页表拆除触发 `release_tail`，末批 admitted 帧会保留在 pending 链至下一次排空——与 x86_64 canonical 行为一致，非缺陷。
- **无地址载荷**：本工程沿用"远程失效 = 全量 `tlb_flush_all`"简化（对齐 x86_64 SIMPLIFIED 项），不含页级载荷。
- **同族编码缺陷（§12.5 报告后已处置）**：`arch/aarch64/freg/mod.rs::freg_trigger_recovery` 原以 `1u64 << 16` 编码目标（注释误称 "TargetList: Aff0=0"），与已确认的 `ICC_SGI1R_EL1` 布局（TargetList=[15:0]，Aff1=[23:16]）不符 —— 属与 ST-09 同类的编码缺陷（因 `freg=off` 未触发）。经用户裁定「本轮一并修复」：改为按**当前核 Aff0** 编码 TargetList（`1u64 << (cpu_id() & 0xF)`），保 `isb` 不变。
- **过期 lint expectation（§12.5 报告后已处置）**：`arch/aarch64/exception.rs` 第二处 `#[expect(clippy::borrow_as_ptr)]` 在 aarch64 clippy `-D warnings` 下报 unfulfilled（`cargo check`/CI 不查 aarch64 clippy，故长期潜伏，HEAD 即存在）。经用户裁定「本轮一并清理」：删除该过期 `#[expect]`（保留已 fulfilled 的另一处）。