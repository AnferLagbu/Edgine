# EEVDF 调度器工程（独立大工程）

> **定位**：独立工程，与 [多线程模型工程](./multithreading-project.md) 同级。EEVDF（Earliest Eligible Virtual Deadline First，最早合格虚拟截止期优先）不是 CFS 的参数调优，而是把 CFS 的「最小 vruntime 选择」替换为「lag 判定资格 + 虚拟截止期选择 + 请求切片」三件套；改动面横跨进程调度字段、每核运行队列、tick 记账、唤醒/迁移、负载均衡与 syscall 权重链路，需要独立设计、分阶段实施与专项取证。
>
> **目标**：在保持 IC1/IC2/IC3、P-21 投送屏障与 RqClaim 跨核迁移协议的前提下，把进程级 CFS 演进为 EEVDF：睡眠任务按欠账获得**有界**补偿，权重通过虚拟截止期体现，切片到期与唤醒抢占各有明确判据；网络 e2e、SMP 迁移、双架构门禁零回归。
>
> **来源**：用户决策——EEVDF 调度算法实现，独立任务工程，重要性等同多线程工程。
>
> **关联**：[multithreading-project.md](./multithreading-project.md)（K-04 线程维度迁移）、[net-e2e-tcp-echo-flakiness.md](./net-e2e-tcp-echo-flakiness.md)（调度离核窗口 P-17/P-21/P-26 取证）、[ref-lock-order.md](../explain/ref-lock-order.md)（rq 锁序）、[spec-engineering.md](../explain/spec-engineering.md)（编码规范）。

## 1. 决策记录

| 决策点 | 选择 | 说明 |
|---|---|---|
| D1 工程形态 | **独立大工程** | 用户已确认；不与多线程工程合并，不拆成分册子项 |
| D2 目标算法 | **EEVDF** | 用户已确认；以 lag + 虚拟截止期 + 请求切片为核心 |
| D3 重要性 | **与多线程工程同级** | 用户已确认；调度器是 CPU 分配的唯一裁决点 |
| D4 实施深度 | **B 相对完整** | 用户已裁定；路径/边界/错误分支闭合、具备单测覆盖、可长期稳定运行，不追求对齐 Linux 全量语义 |
| D5 与 K-04 次序 | **① 先进程级 EEVDF，后线程维度迁移** | 用户已裁定；EEVDF 先落在 `Process`（即每线程 TCB，见多线程工程 D7），K-04 只做线程组语义收口与接线，不做调度字段的结构迁移 |
| D6 CFS 去留 | **① 直接替换** | 用户已裁定；EEVDF 上线即替换旧 CFS 选择/切片判据，不保留回退开关；旧函数经真实使用路径替换后删除，不得以死代码抑制保留（F9） |
| D7 队列结构 | **混合：单 `BTreeSet` 为唯一真相 + 资格前缀线性筛选** | 用户已裁定；合格判定 `vr_i * W <= sum_w_vr` 在 vr 序上恰好是一段前缀，用 `tree.range(..=bound)` 截取后线性取最小 `(vd, pid)`；不建第二索引，避免再引入一处会背离的账目 |
| D8 切片来源 | **① 默认按队列总权重动态 + ③ `sched_setattr` 请求值覆盖** | 用户已裁定；默认切片由 `TARGET_LATENCY` 与 `W` 派生；③ 是覆盖通道而非默认来源，请求的真实切片夹取到 `[MIN_GRANULARITY, TARGET_LATENCY]` |
| D9 syscall 面 | **③ 全通：`nice`/`setpriority` + `sched_setscheduler`/`getscheduler` + `sched_setattr`/`getattr`** | 用户已裁定；先修「nice 不改变 CFS 权重」的断链，再补策略查询与请求切片 |
| D10 虚拟时间精度 | **② Q16.16 定点** | 用户已裁定；`vr` 以 `1/65536` tick 为单位，增量 `((NICE0_WEIGHT << 16) / w).max(1)`，floor/阈值/测试断言全域同步换算。现行整数路径 `(NICE0_WEIGHT / w).max(1)` 对**所有 `w >= NICE0_WEIGHT`（nice ≤ 0）**都产出 1，nice -20 与 nice 0 在 vruntime 上完全同速；这不是仅 `w > NICE0_WEIGHT` 的边界 |

> D4-D10 已由用户裁定（见 §8 变更历史），E-01 已把 §3 公式冻结为 [host-tests/src/eevdf_model.rs](../../host-tests/src/eevdf_model.rs) 可执行参考模型（配套 `tests/eevdf_reference_model_test.rs`），E-03 起实现只许按模型口径落地；D7 的备选（双索引）仅在 E-03 基准证明单树前缀扫描不可接受时另开决策，实现中不得私自改道。

## 2. 调研结论（源码依据）

- **单轨、进程级调度**：当前生产主路径是 `privileged/proc/scheduler.rs` 的 PID 级 DL → RT → CFS（[scheduler.rs:1155](../../src/kernel/privileged/proc/scheduler.rs#L1155)）。旧的并行调度器 `scheduler_ex.rs` 已由 P-29 route C 删除（commit `96ee9beb`），多线程工程 §2 的「双轨调度器」是历史快照，不作为本工程的现状前提。
- **CFS 队列与不变式**：每核 `CfsRunQueue` 以 `(vruntime, pid)` 为 `BTreeSet` 键（[cfs.rs:161](../../src/kernel/privileged/proc/cfs.rs#L161)），`min_vruntime` 作单调下界，IC1 单一时基 / IC2 结构即真相 / IC3 floor 单调；入队落点由 `placement` 钳制并回写任务自身 `cfs_vruntime`（[cfs.rs:226](../../src/kernel/privileged/proc/cfs.rs#L226)），`total_weight` 以 `cfs_on_rq` 为记账凭证（[cfs.rs:237](../../src/kernel/privileged/proc/cfs.rs#L237)）。
- **运行任务在树外、仍在账内**：`pick_next` 只摘树节点、不动权重记账（[cfs.rs:289](../../src/kernel/privileged/proc/cfs.rs#L289)），运行任务由 tick 推进 vruntime 并同步 floor（[scheduler.rs:1957-1990](../../src/kernel/privileged/proc/scheduler.rs#L1957-L1990)）。EEVDF 的加权平均虚拟时间 `V` 必须覆盖「树节点 + 运行任务」，否则运行任务的权重会从平均中消失。
- **对称多处理迁移协议**：投送屏障 `cfs_on_cpu` + `RqClaim` 认领 + 源/目的双 rq 锁（按 CPU 索引升序）+ `reap_off_cpu` 补投，见 [scheduler.rs:720-860](../../src/kernel/privileged/proc/scheduler.rs#L720-L860)、[scheduler.rs:1463](../../src/kernel/privileged/proc/scheduler.rs#L1463) 与 [scheduler.rs:2217-2340](../../src/kernel/privileged/proc/scheduler.rs#L2217-L2340)。任何新增的 `V`/权重账目都必须在这套临界区内更新，不能引入新锁或改变锁序。
- **tick 与重调度**：定时器频率 1000 Hz（[pit.rs:42](../../src/kernel/privileged/timer/pit.rs#L42)），`Scheduler::tick`（[scheduler.rs:1808](../../src/kernel/privileged/proc/scheduler.rs#L1808)）驱动 `tick_current_task`（[scheduler.rs:1893](../../src/kernel/privileged/proc/scheduler.rs#L1893)）；重调度经 `mark_resched_pending_local` / `run_pending_resched` 在中断退出路径执行（[cpu_queue.rs:193](../../src/kernel/privileged/proc/cpu_queue.rs#L193)）。
- **切片未生效**：`CfsRunQueue::calc_time_slice`（[cfs.rs:313](../../src/kernel/privileged/proc/cfs.rs#L313)）只有测试调用，生产路径没有按切片到期抢占；当前 CFS 抢占只依据 vruntime 差值 `cfs_should_preempt`（[cfs.rs:439](../../src/kernel/privileged/proc/cfs.rs#L439)）与 `vr > floor + TARGET_LATENCY_TICKS` 的让出判据（[scheduler.rs:1990](../../src/kernel/privileged/proc/scheduler.rs#L1990)）。
- **nice 权重链路断开**：`nice`/`getpriority`/`setpriority` 只写粗粒度 `ProcessPriority`（[priority.rs:23-105](../../src/kernel/functions/proc/priority.rs#L23-L105)），不更新 `Process::nice` 与 `cfs_weight`；`Scheduler::set_nice`（[scheduler.rs:613](../../src/kernel/privileged/proc/scheduler.rs#L613)）在 functions 层无生产调用者。因此当前用户改 nice 不会改变 CFS 权重，EEVDF 的权重语义在此缺口闭合前无法落地。
- **虚拟时间整数截断**：`calc_vruntime_delta = (NICE0_WEIGHT / weight).max(1)`（[cfs.rs:421-429](../../src/kernel/privileged/proc/cfs.rs#L421-L429)）。`NICE0_WEIGHT = 1024`，所有 `weight >= 1024` 的任务每 tick 都只增加 1 个虚拟 tick ⇒ nice 0 与 nice -20 在 vruntime 上无差别。EEVDF 若直接复用该增量，权重只在「低权重侧」生效。
- **pick 不比较 current**：`pick_next_task` 只从 DL/RT/CFS 树取候选（[scheduler.rs:1155-1251](../../src/kernel/privileged/proc/scheduler.rs#L1155-L1251)），运行任务在树外；现在没有「唤醒候选 vs 当前任务」的截止期比较，也没有切片到期这一周期性抢占源。EEVDF 需要把唤醒抢占做成显式判据，而不是每次 `need_reschedule` 都无条件切换。
- **syscall 面窄**：仅有 `sched_yield`（[lifecycle.rs:29](../../src/kernel/functions/proc/lifecycle.rs#L29)）与 `sched_{set,get}affinity`（[sched.rs:20-60](../../src/kernel/functions/proc/sched.rs#L20-L60)）；全仓无 `sched_setscheduler`/`sched_getscheduler`/`sched_setattr`/`sched_getattr`。
- **线程维度尚未存在**：`privileged/proc/thread.rs` 与 `privileged/syscall/clone.rs` 均已不在现树，`CLONE_THREAD` 只在 functions 层校验标志（[clone.rs:24-47](../../src/kernel/functions/proc/clone.rs#L24-L47)）。多线程工程 K-04 未实施，本工程不得假设线程级调度字段已就位。
- **现有回归与基准锚点**：`host-tests/tests/cfs_single_time_base_test.rs`（IC1/IC2/IC3）、`smp_rq_migration_test.rs`（RqClaim/单树/屏障）、`sched_offcore_placement_barrier_test.rs`（P-21 接线）、`sched_current_type_consistency_test.rs`、`td10_utime_stime_test.rs`；性能基准 `host-tests/tests/cfs_btreemap_bench_test.rs` 只测模拟 `BTreeMap`，尚未覆盖内核队列的选择路径。

## 3. 设计模型与冻结口径

> 本节是 E-01 冻结口径（D4-D10 已裁定）。所有公式、舍入方向与阈值以 [host-tests/src/eevdf_model.rs](../../host-tests/src/eevdf_model.rs) 为可执行单源；实现与文档出现分歧时以模型 + 对拍测试为准，改口径必须同批改模型、测试与本节。

### 3.1 每任务状态

沿用现有 `Process` 调度字段（[process.rs:198-233](../../src/kernel/privileged/proc/process.rs#L198-L233)）：

- `cfs_vruntime`：虚拟运行时间（与 rq 同一时基）。
- `cfs_weight`：由 nice 派生的权重。
- `cfs_on_rq` / `cfs_rq_cpu` / `cfs_on_cpu`：可运行记账、归属核、投送屏障。

EEVDF 新增字段（D10② 后所有虚拟时间字段均为 Q16.16 定点）：

- `eevdf_vdeadline`：本轮的虚拟截止期 `vd = vr + slice_virt`，用于跨任务比较与唤醒抢占。
- `eevdf_slice_virt`：本任务本轮的虚拟切片 `Δvr(w) * slice_real`；默认真实切片由 rq 总权重动态计算（§3.3），`sched_setattr` 路线下按 D8③ 覆盖。
- `eevdf_slice_left`：本轮剩余的**真实切片**（tick）。

切片剩余量**必须落在任务自身**，不得由每核 `PerCpuSched` 的当前槽位持有：任务在 `Migrate`（跨核迁移）与 `Requeue`（被抢占后重投）时要携带预算，放在核槽位会在迁移/换出时丢失且引入跨核写同一字段的竞态。睡眠唤醒（`Wake`）时预算重置，睡眠不累积切片。

`Process` 同时是 EEVDF 的调度实体与（多线程工程 D7 之后的）每线程 TCB；`vr/weight/slice/vd` 的写入者唯一，仍由 rq 锁或 `PROCESS_TABLE` 的既定锁序保护。

### 3.2 每核状态与新增不变式

在 `CfsRunQueue` 现有 `tree` / `min_vruntime` / `total_weight` 基础上按「树账 + 运行账」分解：

- `tree_weight: u64` / `tree_sum_w_vr: u128`：树内（`cfs_on_rq == true` 且不在本核运行）任务的权重和与 `w * vr` 和，只在 rq 锁内由入队/出队/迁移路径更新。
- 每核 `PerCpuSched` 的运行槽增加 `running_weight: u64` / `running_sum_w_vr: u128`：当前运行 CFS 任务的两个分量，由 tick 以增量方式更新（`+w*Δvr`），不触碰树账。
- `W = tree_weight + running_weight`，`S = tree_sum_w_vr + running_sum_w_vr`；`V = S / W`（u128 商，饱和回 Q16.16 u64）。
- `total_weight` 由上述两项派生（`get_weighted_load` 返回 `W`），不再作为独立真相维护；`min_vruntime`/floor 保留并继续单调推进，但其用途降为 IC3/IE4 取证与空合格集合的兜底参照，**不再参与唤醒落点或选择**。

拆分的理由是热路径与迁移路径的写者不同：tick 只改本核运行账（无跨核争用），入队/出队只在 rq 锁内改树账；若把两者合成一个 `sum_w_vr`，tick 每毫秒都要写共享账目，且迁移必须同时搬运运行分量，临界区与出错面都会扩大。运行任务的权重仍在 `W`/`S` 中，不会从平均虚拟时间里消失。

新增不变式（与 IC1-IC3 并行）：

- **IE1 同域**：`V`、`vr`、`floor`、`vd`、`slice_virt` 全部使用 Q16.16 同一虚拟时间单位与同一时基。
- **IE2 账目完备**：每个 `cfs_on_rq == true` 的任务恰好被计入一次——在树上的计入 `tree_*`，在本核运行的计入 `running_*`；`W`/`S` 等于可运行集的全量。`total_weight` 若保留为派生读取，必须与 `W` 恒等。
- **IE3 有界信用 + 欠账保留**：`Wake` 后恒有 `lag = V_before - vr <= slice_virt`；若 `vr_raw <= V_before` 则 `lag >= 0`（睡眠信用有界），若 `vr_raw > V_before` 则 `lag < 0`（保留欠账，不补偿、不制造信用）。睡眠不产生无界信用，也不清空既有欠账。
- **IE4 floor 单调**：`min_vruntime` 仍只增不减（IC3 不因 EEVDF 改变），但 `V` 不要求单调（低 vr 任务入队会拉低平均）。
- **IE5 迁移守恒**：跨核迁移在源/目的双持锁内完成「源撤账 + 目的记账」，两核的 `tree_weight`/`tree_sum_w_vr` 变化量互为相反数；`Migrate` 原样携带 `vr`、`slice_virt`、`slice_left`、`vd`，不得重算落点。
- **IE6 切片生命周期**：`Wake` 重置预算，`Migrate` 携带预算，`Requeue` 仅在预算耗尽时补充；三种入队语义不得混用。

### 3.3 核心公式（E-01 冻结）

- 单位：`F = 16`，所有虚拟时间量 `X_v = X_real << 16`；`Δvr`、`vr`、`V`、`vd`、`slice_virt` 一律为 Q16.16 整数，除法运算一律向下取整。
- 虚拟时间推进：`Δvr = max(1, (NICE0_WEIGHT << 16) / w)`（`w == 0` 按 `NICE0_WEIGHT` 退化）；`vr' = vr + Δvr`。nice -20..19 全 40 档的 `Δvr` 从 756 到 4_473_924 随权重严格递减，不再出现「nice ≤ 0 全为 1」的截断。
- 加权平均：`W = tree_weight + running_weight`，`S = tree_sum_w_vr + running_sum_w_vr`，`V = S / W`（`W == 0` 时 `V = 0`；商溢出饱和到 `u64`）。
- 欠账：`lag_i = V - vr_i`；**合格** ⇔ `lag_i >= 0` ⇔ `vr_i * W <= S`（u128 交叉乘法；不用除法比较，避免 floor 改变符号）。
- 默认真实切片：`slice_real = max(MIN_GRANULARITY, min(TARGET_LATENCY, TARGET_LATENCY * w_i / W_inc))`，其中 `W_inc` 是**把本任务计入后**的可运行集权重和——`Wake` 用 `W_before + w_i`，`Requeue` 用当前已含本任务的 `W`。
- 虚拟切片：`slice_virt = Δvr(w_i) * slice_real`。该式与逐 tick 记账严格对齐：切片耗尽时 `vr` 恰好推进一个 `slice_virt`。**不采用** `slice_virt = slice_real * NICE0 / w_i` 的连续比值——它与整数 `Δvr` 的 floor 不同源，会产生约一个虚拟 tick 的漂移。
- 虚拟截止期：`vd_i = vr_i + slice_virt_i`，在建账时写入；运行期间 `vd` 固定，恒有 `vd_i - vr_i = Δvr(w_i) * slice_left_i`（IE6 的联合检查式）。
- D8③ 请求切片：`sched_runtime`（ns）先换算真实 tick 并夹取到 `[MIN_GRANULARITY, TARGET_LATENCY]`，再按同一 `slice_virt = Δvr * slice_real` 换算；请求只在下次建账（`Wake`，或切片耗尽后的 `Requeue`）生效，不打断当前切片；清除请求后回到默认值。
- 选择：合格集合 = 树内合格前缀（`vr <= V`）+ 合格 current，取 `(vd, pid)` 最小者；无合格者时回退 `(vr, pid)` 最小并与 current 比较（`current.vr <= candidate.vr` 时保持 current，**current-aware fallback**）。
- 入队三分语义：
  - `Wake`：重算 `slice_real/slice_virt`，`vr = max(vr_raw, V_before - slice_virt)`，`slice_left = slice_real`，`vd = vr + slice_virt`。睡眠信用上界为一个 `slice_virt`；`vr_raw > V_before` 时保留负 lag（欠账），不制造信用。
  - `Migrate`：原样携带 `vr/slice_real/slice_virt/slice_left/vd`，不套用任何落点钳制（迁移不得奖励 vr，也不得重算切片）。
  - `Requeue`：`slice_left > 0` 时原样重投，保留 `vr` 与预算；仅当 `slice_left == 0`（切片耗尽/被抢占）时重算 `slice_real/slice_virt` 并令 `vd = vr + slice_virt`，不改 `vr`。
- 唤醒抢占：入队成功与抢占请求是两件事；仅当新任务合格、当前任务是 CFS 且 `vd_new < vd_current` 时请求重调度，无 current 时直接请求。当前任务非 CFS 时按类间优先级处理。唤醒路径不再使用 floor 或固定 vr 差值。

### 3.4 选择路径与 current

运行任务保持在树外（与现状一致），但新增三项判据：

- **切片到期**：tick 递减任务的 `eevdf_slice_left`；归零时把当前任务以 `Requeue` 语义重新入树（补充切片、重算 `vd`），并设置本核重调度标记。重调度仍只在中断退出路径执行，不在 tick 内直接切换。
- **唤醒抢占**：在 `unblock` / 投送路径比较候选与当前任务的 `vd`，只有候选合格且 `vd` 更早才设置重调度标记，避免每次唤醒都抖动。
- **当前任务兜底**：`pick_next_task` 的 CFS 分支由 `pick_eevdf` 替换；无合格任务时，树内最小 vr 候选必须与 current 比较，current 更靠前则保持 current。boot 期「从未入过 CFS 队列」的 keep-current 路径保持原样。

`pick_eevdf` 的输入是「树 + 当前运行任务」的合并可运行集：当前任务在树外，但通过 `running_*` 账目参与 `V` 与兜底比较；它若在切片到期后被 `Requeue` 入树，则回到正常选择路径。

**E-01 数学结论**：只要可运行集非空且 IE2 账目完备，就有 `min(vr) <= S / W`，故 `min(vr) <= floor(S / W) = V`，即合格集合恒非空，「全体 lag < 0」不可达。current-aware fallback 因此只是防御性路径；必须锁死的是**账目完备**——运行任务一旦漏出 `W/S`，该证明立即失效。

### 3.5 时基、溢出与精度

- `sum_w_vr` 用 `u128` 防止 `w * vr` 在长 uptime 下溢出；`V` 取商时饱和到 Q16.16 `u64`。
- `W` 最大约 `255 * 88_761 ≈ 22.6M`；`S` 的乘积与合格判定的交叉乘法（`vr * W`）均在 `u128` 内完成。
- Q16.16 的增量上界为 nice 19（`w = 15`）：`1024 * 65536 / 15 ≈ 4.47M` 单位/tick；按 1000 Hz 计，`u64` 的 vr 时间轴可用约 `10^9` 秒量级，满足工程寿命。
- 所有常量（`TARGET_LATENCY`、`MIN_GRANULARITY`、`NICE0_WEIGHT`）、floor、切片、测试断言一次性换算到 Q16.16；**禁止两套单位并存**。换算口径固定在 E-01 参考模型中，实现只引用模型导出的常量。
- 所有入队/出队/迁移路径必须**幂等**更新对应账目（树账或运行账），并用「随机操作序列后与暴力重算对账」的属性测试兜底。

### 3.6 与 DL/RT、idle、负载均衡的关系

- EEVDF 只替换 `SchedPolicy::Normal` 的 CFS 选择规则；DL → RT → CFS 的类间优先级不变。
- idle 任务的策略仍为 `SchedPolicy::Idle`，不进入 CFS 选择；boot 期「从未入过 CFS 队列」的 keep-current 路径（[scheduler.rs:1225-1248](../../src/kernel/privileged/proc/scheduler.rs#L1225-L1248)）必须保留，且不得被 EEVDF 的 `V` 计算误判为空闲。
- 负载均衡的候选选择与搬动记账必须与 EEVDF 账目同源：搬运树内任务时，源/目的两棵树的 `tree_weight`/`tree_sum_w_vr` 与任务自身的 `vr/slice_left/vd` 都要在双锁内同步；运行账不参与偷取（候选只来自树）。
- 当前 `steal_locked` 的候选集已收敛为「合格前缀（`vr <= V`，最多 `LOAD_BALANCE_MAX_MOVE` 个）」，并按「窗口内最大有效 `vd`（最不急迫）」选取（E-05b 评估结论）。两种策略的候选集合相同，故不改变合格性与迁移守恒；差异只在同批次的搬动顺序：最左端 vr 优先搬走源核最急需的任务且忽略截止期，最大 `vd` 则降低迁移对目的核的即时抢占概率、把更紧迫任务留给源核。迁移全程原样携带 `vr/slice/vd`，且不做目的核 floor 钳制（否则会改写全局 `S`）。
- `cfs_should_preempt` 与 `vr > floor + TARGET_LATENCY_TICKS` 的旧判据由 D6① 直接替换：生产调用点改为 EEVDF 的 `vd`/切片判据后删除函数与对应测试，不保留兼容阈值或二级保险。

### 3.7 与多线程工程的边界

- **EEVDF = Normal 类内部的分配策略；K-04 = 调度单位从 Process 迁到 Thread**。两者正交，但共享同一组 `vr/weight/slice/vd` 字段。
- D5=① 且多线程工程 D7 采用「Process 即每线程 TCB（PID=TID）」后，**不存在结构迁移**：EEVDF 字段先落在现 `Process`，K-04 只补线程组语义（tgid/ThreadGroup/组退出/信号投递）与进程级捷径审计。E-08 据此改为语义对齐阶段，不再描述「字段机械迁移」。
- 若后续改变实体口径（例如引入独立 Thread 结构），才需要把字段搬到新实体；该变更必须另立决策，不能由实现顺手完成。
- 本工程**不新增**「调度实体 trait」抽象来同时适配 Process/Thread；那是为假想未来预留的复杂度，按 §11.3 简约原则由 D5 裁定后直接落字段。

### 3.8 E-01 结论（原开放议题收口）

- **睡眠欠账上界**：结论为一个 `slice_virt`（`Wake` 落点钳制），由 D8① 默认值或 D8③ 请求值决定；信用与欠账两侧均已由随机序列（2_000 轮）验证：`lag <= slice_virt` 恒成立，`lag >= 0` 当且仅当 `vr_raw <= V_before`，负 lag 作为欠账原样保留。
- **新 fork 任务的初始 vr**：走 `Wake` 入队钳制（继承父 vr，但在 `V - slice_virt` 之上），不单独设计「初始 vr」分支；exec 保持当前调度实体与 vr（同一任务继续执行）。
- **低/高权重切片上下限**：真实切片恒在 `[MIN_GRANULARITY, TARGET_LATENCY]`；虚拟切片一律 `Δvr * slice_real`。E-01 已覆盖 nice -20..19 全 40 档与单任务/多任务边界，舍入方向固定为 floor。
- **饥饿证明**：数学上非空可运行集必有合格任务（`min(vr) <= V`），全体 lag < 0 不可达；current-aware fallback 仅防御。adversarial「全体树任务欠账 + 探针每 7 tick 连续唤醒」序列下，基础任务首次服务均 < 1000 tick 且全部获得服务。
- **抢占延迟**：频繁唤醒的高权重短切片任务每周期最多消费一个请求切片，份额被限制在 750..850‰，不能按权重垄断 CPU；CPU-bound 高权重任务的长期份额贴近权重比（约 98%），不会因真实切片上限而被人为压低。用户可感知的抢占间隔上界为一个切片（默认或请求值）。
- **`cfs_should_preempt` 的关系**：D6① 直接替换，生产调用点改完后删除，不保留兼容阈值。
- **`V` 的表示**：内部 Q16.16 字段 + 测试/调试访问器，本工程不新增 procfs 暴露。
- **回退**：D6① 不设回退开关；若未来需要回退，另立工程并走真实可达路径，不得以死代码或 `cfg` 空壳保留。
- **E-01 交付物**：[host-tests/src/eevdf_model.rs](../../host-tests/src/eevdf_model.rs) 参考模型 + [tests/eevdf_reference_model_test.rs](../../host-tests/tests/eevdf_reference_model_test.rs) 14 项对拍，覆盖常量单源、40 档 `Δvr`、切片/请求夹取、IE3/IE4/IE5/IE6、255 边界、合格集合、adversarial 序列与随机序列暴力对账。生产实现不得自行改公式；口径变更必须同批修改模型、测试与本节。

## 4. 实施阶段（循序渐进，每阶段 = 状态 [X] + 验证门槛）

- **E-01. 设计与口径定稿**
  - 描述：D4-D10 已裁定；把 §3 的 Q16.16 单位、树账/运行账分解、`Wake/Migrate/Requeue` 三分入队、合格/截止期/切片公式、current-aware fallback 与唤醒抢占判据固化为参考模型与属性测试；本阶段不改生产代码。
  - 方案：host 侧纯函数模型（Q16.16 整数运算）+ 随机操作序列与暴力重算对账（`W`/`S`/`V`/资格/切片/迁移守恒）+ 边界表（255 任务、nice -20..19、切片上下限、全体 lag 为负）；参考模型输出作为 E-02 起实现的唯一口径。
  - 状态：[X]
  - 详情：参考模型落 `host-tests/src/eevdf_model.rs`，对拍落 `host-tests/tests/eevdf_reference_model_test.rs`（14 项全绿）；冻结结论见 §3.3 与 §3.8。修正点：`slice_virt = Δvr * slice_real`（不再用连续比值）、IE3 改为「信用有界 + 欠账保留」、证明非空可运行集必有合格任务（fallback 降为防御路径）。
- **E-02. 权重链路打通与调度字段审计**（依赖 E-03 的账目入口）
  - 描述：修复 `nice`/`setpriority` 只写 `ProcessPriority`、不写 `nice`/`cfs_weight` 的既有缺口；让 `Scheduler::set_nice` 进入生产调用路径；核对 `Process::new` / init 默认值（[process.rs:424-430](../../src/kernel/privileged/proc/process.rs#L424-L430)）。
  - 方案：在 functions 层经 privileged 安全 API 更新 `nice` + `cfs_weight`；任务在 rq 上时，经 E-03 提供的权重变更入口在**同一 rq 锁内**先撤旧账（旧 `w` 与旧 `w*vr`）再按新 `w` 记入，并重算 `slice_virt/vd`；不新增锁、不改锁序。补 nice → weight → Q16.16 增量的全 40 档单测与负控制。
  - 状态：[X]
  - 详情：`Scheduler::set_nice` 改为「`RqClaim` 认领 → 取所属 rq 锁 → 写 nice/cfs_weight → `reweight` → 重建 `slice_real/slice_virt/slice_left/vd`」；新增 `claim_rq_blocking`（只在系统调用上下文、不持 rq 锁时等待迁移完成，持锁路径仍用有界 `claim_rq`），锁序保持 `cfs_rq -> PROCESS_TABLE`。权重变更按重建本轮账目处理（`slice_left` 置为新 `slice_real`，不做剩余切片折算，`vd = vr + slice_virt` 恒成立）；在 E-04 切换时基前，`vd` 用 `cfs_vruntime << 16` 桥接，桥接点已注释标明。functions 侧：`getpriority` 返回真实 nice；`setpriority`/`nice` 统一走 `Scheduler::set_nice`，负数 nice 不再被误判为错误（新增 `apply_nice`），粗粒度 `ProcessPriority` 同步维护；`priority_to_nice` 无调用已删除（F9）。进程字段：新增 `Process::get_nice` 与 `glue::low_i8_from_u32`，修正 `Process::nice` 的编码注释；`Process::new` 默认值（nice=0 / weight=1024 / EEVDF 全 0）与 `user_proc` 委托路径已核对。回归锁 [eevdf_weight_chain_test.rs](../../host-tests/tests/eevdf_weight_chain_test.rs) 9 项（40 档模型对拍 + 边界/负控制 + syscall 端到端 + 在队 reweight/切片/vd 真实 rq 验证 + 源码契约）。**审计登记**：fork 的 `inherit_parent_state` 未继承父进程 nice/cfs_weight/ProcessPriority（POSIX 缺口，host 无可行直测路径），归 E-06 处理，不在本阶段静默修复。
- **E-03. EEVDF 队列机制与纯函数**（先于 E-02 落地，消除账目循环依赖）
  - 描述：为 `CfsRunQueue` 引入树账/运行账分解（`tree_weight`/`tree_sum_w_vr` + 每核 `running_*`）、`V` 计算、合格前缀判定、虚拟截止期与 `pick_eevdf`；把切片计算从「仅测试调用」变为生产切片来源；保留 IC1-IC3 与幂等入队/撤账语义。
  - 方案：先实现不接生产 tick 的纯函数与属性测试（暴力重算/资格集合/无重复节点/权重守恒/运行账并入）；`pick_eevdf` 按 D7 用单树合格前缀 + 线性 `(vd, pid)` 选择；提供 rq 锁内的权重变更入口供 E-02 调用；所有写路径在同一 rq 锁内更新对应账目。
  - 状态：[X]
  - 详情：`CfsRunQueue` 增树账（`tree_ledger` + `tree_weight`/`tree_sum_w_vr`）与运行账（`running_ledger` + `running_weight`/`running_sum_w_vr`），`total_weight` 由两账同步派生；新增 rq 锁内入口 `reweight` / `advance_running_vruntime` 供 E-02/E-04 使用，`pick_eevdf` 按 D7 做单树合格前缀筛选 + current-aware 兜底 + 最小 `(vd, pid)`；9 个纯函数（Q16.16 增量、默认/请求切片、`V`、资格、`vd`、唤醒钳制）落地且全部与 E-01 模型对拍。`Process` 增 `eevdf_vdeadline` / `eevdf_slice_real` / `eevdf_slice_virt` / `eevdf_slice_left` / `eevdf_requested_slice` 五字段（0 = 未构建/无请求）。回归锁：[eevdf_queue_mechanism_test.rs](../../host-tests/tests/eevdf_queue_mechanism_test.rs) 8 项（模型对拍 / 2000 步随机账目暴力对账 / 800 组选择对拍 / 迁移守恒 / 255 边界 / stale 凭证），与 `cfs_single_time_base_test`、`smp_rq_migration_test`、`cfs_btreemap_bench_test` 同树全绿；本阶段不接生产 tick，E-02/E-04 起消费上述入口。
- **E-04. tick 记账、切片预算与抢占接线**
  - 描述：运行任务的 vruntime 推进时以增量维护本核 `running_weight/running_sum_w_vr`；tick 递减 `eevdf_slice_left` 并在到期时按 `Requeue` 语义重投入树、请求重调度；用 EEVDF 判据替换或收编现有 `should_yield`。
  - 方案：保持 `advance_floor` 与 IC1/IC3；tick 热路径只做 O(1) 运行账增量，不写树账；重调度仍走 `mark_resched_pending_local` / `run_pending_resched`，不在中断上下文直接切换。
  - 状态：[X]
  - 详情：tick 的 CFS 分支收口为 `Scheduler::tick_current_cfs`，vruntime 推进、运行账增量（`advance_running_vruntime`）、floor 推进（`advance_floor`）与切片状态机在同一 rq 锁内完成，锁序仍为 `cfs_rq -> PROCESS_TABLE`。切片三态：`slice_left == 0` 为 Build（只建本轮切片与 `vd`，不请求抢占）、`> 0` 为 Advance（递减、不写树）、归零为 Expire（`rebuild_eevdf_round` 后按 `Requeue` 原位 `enqueue` 并请求重调度）。重调度只置 `need_reschedule`，实际切换仍在中断退出路径执行。Requeue 仅对「运行账在册或已在树」的任务执行，boot 期 keep-current 任务不入树（避免 `NO_RQ_CPU` 裸节点被偷取后双重记账）；到期后 `eevdf_current_view` 因 current 在树上而返回 None，由 `pick_eevdf` 走树→运行账的激活路径。本阶段同批删除旧判据：`cfs_should_preempt`、`vr > floor + TARGET_LATENCY_TICKS` 与 `vr << VIRT_SHIFT` 换算桥全部移除，`placement` 改用 `MIN_GRANULARITY_VIRT`，全链路单一时基 Q16.16。唤醒抢占（`vd_new < vd_current`）按依赖归 E-05a，本阶段以「切片到期必重选」给出有界时延。回归锁：[eevdf_tick_accounting_test.rs](../../host-tests/tests/eevdf_tick_accounting_test.rs) 5 项（真实 rq 上 Build/Advance/Expire 两轮切片 + 源码契约 + 旧判据负控制），`cfs_single_time_base_test` 同步为 E-04 契约；旧 `tick` 分支的行为断言改由新用例承接。
- **E-05a. 入队、唤醒与 prev 重投**
  - 描述：实现 `Wake/Migrate/Requeue` 三分语义；`unblock`、prev 重投（`requeue_prev`）、block/dequeue 路径维护 EEVDF 账目与切片预算；实现 `vd` 比较的唤醒抢占。
  - 方案：沿用现有投送路径与锁序，不新增锁；在 `sched_current_type_consistency_test` 与新增 host 用例上覆盖三分语义、睡眠欠账上界、唤醒抢占下界；迁移暂只保证不破坏旧行为，完整迁移账目在 E-05b 收口。
  - 状态：[X]
  - 详情：入队语义收口为 `PlacementKind::{Wake,Requeue,Migrate}` + 纯函数 `plan_placement`：Wake 以 `max(vr_raw, V_before - slice_virt)` 把睡眠信用钳到至多一个虚拟切片，并按**入队后** `W` 重建 `slice_real/slice_virt/vd`；Requeue 在 `slice_left > 0` 时原样携带本轮切片与 `vd`、耗尽时按已含自身的 `W` 重建；Migrate 原样携带全部字段（生产接线与源/目的账目守恒归 E-05b，本阶段未改 `steal_locked`）。`place_locked` 统一消费计划：同一任务已在目的或源队列记账时 Wake 降级 `Requeue`（重复唤醒不刷新预算/信用）；屏障分支在同一 `PROCESS_TABLE` 临界区完成 `cfs_on_cpu` 复核、`Ready` 翻转与落点/切片/`vd` 写入；IC1 floor 钳制只改落点，携带切片的 `vd` 按 `placed - vr` 平移以保持 `vd - vr`。接线：`unblock` 与 `add`/`add_to_run_queue` 走 Wake，`requeue_prev`/`reap_off_cpu`/RT 降级走 Requeue；`unblock` 按 `PlacementOutcome` 仅在实际落队后请求重调度。唤醒抢占 `wake_preempts`：无 current、current 非 `Running`/`Ready`、或 current 为 Idle 时允许；current 为 Normal 时要求新任务在**入队后**账目合格且 `vd_new < vd_current`（current 未建账时按 `vd = vr`），RT/DL 一律不抢占；仍只置 `need_reschedule`，不在投送路径直接切换。回归锁：[eevdf_wake_requeue_test.rs](../../host-tests/tests/eevdf_wake_requeue_test.rs) 8 项（睡眠信用上界、晚截止期/不合格不抢占、重复 Wake 降级、prev 携带未耗尽切片/耗尽重建、三分接线契约、门闸源码契约与负控制），并同步 `eevdf_tick_accounting_test` / `cfs_single_time_base_test` / `smp_rq_migration_test` / `sched_offcore_placement_barrier_test` 的 E-05a 契约。
- **E-05b. 跨核迁移与负载均衡账目**
  - 描述：`RqClaim` + `cfs_on_cpu` 屏障 + 升序双锁下的跨核迁移同步维护源/目的树账；`steal_locked` 候选收敛到合格前缀，并完成「最左端 vr vs 最不急迫 vd」的评估。
  - 方案：迁移全程携带 `vr/slice_virt/slice_left/vd`（`Migrate` 语义）；在 `smp_rq_migration_test` / `sched_offcore_placement_barrier_test` 上扩展 IE5 守恒断言；评估结论落 E-01 参考模型的候选选择对照，不改锁序。
  - 状态：[X]
  - 详情：`CfsRunQueue::migrate` 作为迁移记账入口落地——先撤源树账/摘节点，再按**原落点**记入目的树账并同步权重和，**不做**目的核 IC1 floor 钳制（钳制会凭空制造 `w * (placed - vr)` 的 S 漂移），调用方回写 `cfs_vruntime` 且 `placed == vr`（IE5）。`steal_locked` 改走该入口：`src.dequeue` → `dst.migrate` → 同一 `PROCESS_TABLE` 临界区回写 `cfs_vruntime` / `cfs_on_rq` / `eevdf_vdeadline`，切片三字段不重写（Migrate 携带本轮预算与截止期）；认领、`current`、亲和、屏障与可调度判据全部保持原序，锁序不变。候选来源由 `leftmost_pids` 收敛为 `eligible_pids`（取 `vr <= V` 的合格前缀，最多 `LOAD_BALANCE_MAX_MOVE` 个；`V` 含运行账），再按新增 `effective_deadline`（`vd == 0 || vd < vr` 退化为 `vr`，与 `pick_eevdf` / 唤醒抢占同式）降序取最大者——即窗口内**最不急迫**的合格任务。**评估结论**：最左端 vr 会优先搬走源核最急需执行的任务且完全忽略截止期；在候选集合相同（故合格性与 IE5 不变）的前提下，最大有效 `vd` 只会降低迁移对目的核的即时抢占概率、把更紧迫任务留给源核，故选定后者。回归锁：新增 [eevdf_migration_test.rs](../../host-tests/tests/eevdf_migration_test.rs) 2 项（32 任务随机迁移：原样携带、每次搬动树账全额守恒、重复迁移幂等；400 组模型对照：两策略候选集相同、全部合格、最不急迫者 `vd` 为窗口最大、移出后源核非空时 `V` 不降），`smp_rq_migration_test` 扩为 11 项（合格前缀边界、偷取源码契约、旧 `leftmost_pids` 负控制），`sched_offcore_placement_barrier_test` 增「屏障拒搬早于撤账」断言；`cfs.rs` 内联补 2 项（`migrate` 原样记账、`eligible_pids` 覆盖全可运行集）。
- **E-06. functions/syscall 面与可观测**
  - 描述：按 D9③ 打通 `nice`/`setpriority`、新增 `sched_setscheduler`/`getscheduler` 与 `sched_setattr`/`getattr`；`sched_setattr` 的请求切片按 D8③ 夹取后经 E-03 入口生效；必要时暴露 `V`/lag/切片统计供测试读取。E-02 审计登记：fork 子进程须按 POSIX 继承父进程 nice/cfs_weight/ProcessPriority（当前 `inherit_parent_state` 未覆盖）。
  - 方案：ABI 常量落 `edgine-abi` 单源；functions 层只调用 privileged 安全 API（F1/F2）；权限与 `SCHED_NORMAL` 之外策略的返回码逐项测试；不在 functions 层复制调度公式；`sched_setattr` 的其他字段（affinity/deadline）明确返回 `EINVAL` 而非静默忽略；fork 继承经 `Process::new` 默认值之上的显式字段复制实现，并补继承语义测试。
  - 状态：[X]
  - 详情：ABI 单源落 `src/rust/edgine-abi/src/sched.rs`（syscall 编号 144/145/314/315、策略值 `SCHED_OTHER=0`/FIFO/RR/BATCH/IDLE=5/DEADLINE=6、`SCHED_RESET_ON_FORK`、七个 `SCHED_FLAG_*`、`SCHED_ATTR_SIZE_VER0=48`、`MIN_NICE/MAX_NICE`、`SchedParam`/`SchedAttr` 布局，含偏移与尺寸单测）；privileged 在 `syscall::types` 以 `pub use` 转出，functions 经既有 `functions::syscall::types` glob 代理消费同一套数值（不新增 crate 依赖、不触 F2 黑名单）。`tick::ns_to_ticks` 补齐纳秒→tick 的向上取整换算。privileged 新增 `Scheduler::set_requested_slice`：0 表示清除、非 0 按 D8③ `clamp_requested_slice` 夹取；经 `claim_rq_blocking` 认领后，未入账者只记录请求，在树/在运行者在同一 rq 锁内 `rebuild_eevdf_round` 重建 `slice_real`/`slice_virt`/`slice_left`/`vd`（锁序仍 `cfs_rq -> PROCESS_TABLE`）。functions 新增 [sched_attr.rs](../../src/kernel/functions/proc/sched_attr.rs)：`sched_setscheduler` 只接受 `SCHED_OTHER` + `sched_priority == 0` 且只作用于自身（策略检查先于指针读取，负 pid `EINVAL` → 目标缺失 `ESRCH` → 外来 `EPERM`），内部 RT/DL/Idle 策略返回 `EINVAL`；`sched_getscheduler` 经显式 `policy_to_abi` 映射（内部 Idle=3/Deadline=4 与 ABI 5/6 不同域），查询不设 self-only 限制；`sched_setattr` 要求 `size == 48`、策略 `SCHED_OTHER`、`sched_flags`/`sched_priority`/`sched_deadline`/`sched_period` 均为 0、nice ∈ `[-20,19]`，否则逐项 `EINVAL`（不静默忽略），nice 复用 `setpriority_for_pid`（`priority.rs` 提升为 `pub(crate)`，保持单一写路径），`sched_runtime` 经 `ns_to_ticks` → `set_requested_slice` 生效；`sched_getattr` 接受 `size >= 48` 并只回写 48 字节 VER0（未支持字段置 0，`sched_runtime` 经 `ticks_to_ns` 还原）。四条编号接入 `functions/syscall/dispatch.rs`。fork 的 `inherit_parent_state` 复制父进程 `nice`/`cfs_weight`/`ProcessPriority`（nice 与权重成对复制，避免子进程以默认权重稀释父份额）；每轮 EEVDF 账目与请求切片不继承，由入队建账重新起算。可观测面未新增特权接口：测试直读 `Process` 原子字段，用户面由 `sched_getattr` 往返。回归锁：[eevdf_sched_syscall_test.rs](../../host-tests/tests/eevdf_sched_syscall_test.rs) 12 项（ABI 布局/常量与内核再导出同源、策略映射五值、setscheduler 幂等与非零 priority/EFAULT、非 OTHER 策略与跨进程/负 pid/缺失目标的码序、在队 setattr 的切片夹取与 vd 重建、off-rq 只记属性、未支持字段逐项 EINVAL、target 错误码顺序、getattr 往返与更大容量只写 VER0、跨进程查询、dispatch 接线、functions 层无调度公式源码契约），`proc_ops.rs` 内联 1 项 fork 继承语义（权重链路复制、vruntime/vd/请求切片不继承）。
- **E-07. 回归锁、性能基线与文档收口**
  - 描述：把 EEVDF 语义固化为 host-tests 回归锁；补内核队列选择路径的基准并与 `cfs_btreemap_bench_test` 对照；跑 QEMU 双架构与网络 e2e；按 D6 决定 CFS 旧路径的去留并同步 docs/explain。
  - 方案：每阶段跑 §2.3 六门槛；E-07 末尾跑全量 `bash ci/audit.sh full` + `./ci/build.sh all` + `make test-host` + `make test-kernel-host` + `make -f Makefile.ci ci-rustdoc` + QEMU；`host-tests/benches/baseline.json` 按 §8 更新；删除旧路径时不得用 `#[allow(dead_code)]` 等抑制（F9）。
  - 状态：[X]
  - 详情：D6① 旧路径退役：删除 `CfsRunQueue::pick_next`（最小 vruntime 选择器），全部测试调用点改写为 `pick_eevdf(None, |_| None)`（合格前缀必含树最左端，行为等价），结构体/`leftmost_vruntime`/`eligible_pids` 文档同步；`DlRunQueue::pick_next` 仍为生产入口（`scheduler.rs` 的 DL 选路），不在退役范围。新增 [cfs_single_time_base_test.rs](../../host-tests/tests/cfs_single_time_base_test.rs) 回归锁 `cfs_legacy_selector_is_retired`：源码级禁止旧选择器复活，并断言生产选路 `pick_next_task → pick_cfs_task → pick_eevdf`（该文件 10 项全绿）。性能基线：[framekernel_bench.rs](../../host-tests/src/framekernel_bench.rs) 新增 `eevdf_pick_requeue`（255 任务稳态，每次迭代 = `pick_eevdf` + Requeue 重投，覆盖树账/运行账与合格前缀筛选），`baseline.json` 按 §8 重录为 24 条，读数 550.37 ns/op；同机 release 下 `cfs_btreemap_bench_test` 的手工 `BTreeMap`（1000 任务 enqueue+pick）为 ~48 ns/op — 生产队列约 11 倍，差异来自双账维护与 D7 合格前缀线性扫描（规模以 `MAX_TASKS=255` 为界），非静默放宽，`check_bench_regression.py` 通过。文档同步：[guide-dev.md](../explain/guide-dev.md)、[explain-framekernel.md](../explain/explain-framekernel.md)、[linux-compat-philosophy.md](../explain/linux-compat-philosophy.md) 的 CFS/MLFQ 表述改为 EEVDF，并写明调度策略因运行队列与进程表/调度器共享锁按 DECISION-J 留 privileged。验证：`./ci/build.sh all` 双架构 0w0e、`bash ci/audit.sh full` 全绿、`make test-host` 全绿、`make test-kernel-host` 1097 passed/0 failed、`make -f Makefile.ci ci-rustdoc` 双架构 0 link 错误、QEMU `all` 2/2（P-62 4/4）且 x86_64/aarch64 网络 e2e 均回显通过。
- **E-08. 线程维度语义对齐（依赖多线程 K-04）**
  - 描述：K-04 按多线程工程 D7 只做线程组语义收口后，确认 `vr/weight/slice/vd` 仍落在每线程 TCB（现 `Process`），调度实体、current 语义、tick 记账与迁移协议均无需结构迁移；把 EEVDF 文档与多线程文档的实体口径对齐。
  - 方案：与 K-04 同批评审「Process 即每线程 TCB」的每一条进程级捷径（children/wait、fd 关闭、cwd、信号投递、exit_group），补充线程级回归；本工程不重复实现线程组语义，也不在新实体上复制一套调度字段。
  - 状态：[]

### 依赖关系

```
E-01 → E-03 → E-02 → E-04 → E-05a → E-05b → E-06 → E-07 → E-08
```

- **E-03 先于 E-02**：`set_nice` 在任务已入队时必须同步 `tree_weight`/`tree_sum_w_vr`，该入口属 E-03；先做 E-02 会形成「权重已改、账目未建」的循环依赖。
- E-02 可与 E-03 的纯函数设计并行，但 E-02 的接线必须等 E-03 的账目入口就绪；E-04 必须在两者之后（否则权重语义仍断链）。
- E-05a 依赖 E-03 的账目与 E-04 的切片判据；E-05b 依赖 E-05a 的三分语义；E-06 依赖 E-05a/E-05b 的唤醒/切片/迁移语义。
- E-08 依赖本工程 E-07 与多线程工程 K-04；两条工程线可独立推进，交接点只做语义对齐，不做字段迁移。
- E-01 未完成前，E-02 起不得开工。

## 5. 验证门槛（每阶段不可豁免）

- §2.3 六条门槛全量通过：双架构构建、clippy/fmt、核心审计、host-tests、QEMU、host 侧内核单元测试；审计档位必须显式 `full`。
- 调度专项回归：`cfs_single_time_base_test`、`smp_rq_migration_test`、`sched_offcore_placement_barrier_test`、`sched_current_type_consistency_test`、`td10_utime_stime_test` 全绿。
- 新增属性测试至少覆盖：树账/运行账与暴力重算一致、合格集合与 lag 符号一致、无重复节点、权重守恒、迁移前后两核账目守恒（IE5）、`Wake` 欠账有界、切片到期必请求重调度、三分入队语义不混用（IE6）。
- Q16.16 专项：nice -20..19 全 40 档的 `Δvr` 严格随权重递减（不再全为 1）；所有阈值/floor/切片断言使用同一单位；参考模型与生产实现逐用例对拍。
- 选择专项：非空可运行集必有合格任务（`min(vr) <= V`）且 `pick_eevdf` 结果等于合格集合最小 `(vd, pid)`；current-aware fallback 仅为防御路径；唤醒抢占只在 `vd_new < vd_current` 时发生；`Migrate` 不改变 `vr`/预算。
- 旧判据退役：`cfs_should_preempt` 与 `vr > floor + TARGET_LATENCY_TICKS` 的调用点全部改完后，函数与测试删除，无死代码抑制。
- 网络 e2e：沿用 `E2E_REPEATS`（[qemu_boot_test.sh:43](../../scripts/qemu_boot_test.sh#L43)）与 [qemu_liveness_probe.sh](../../scripts/qemu_liveness_probe.sh) 的既有回归；如需 1000-boot soak，按 P-29/P-30 的手工复跑方式执行并把读数落档，不把未落脚本的流程写成现成门禁。
- 性能：E-03 建立队列选择基准，E-07 与基线对照；任何回退必须给出根因与后续计划，不得静默放宽。
- F9：旧 CFS 选择/切片函数若被替换，必须删除或经真实使用路径消费；不得新增 `#[allow(dead_code)]`、`#[allow(clippy::…)]` 或构建参数逃逸。

## 6. 风险与边界

- **架构关键路径**：调度器改动直接决定所有进程的 CPU 分配与网络唤醒时延；D4-D10 已裁定，实施仍须按 E-01 冻结口径，禁止「边写边改公式」。
- **SMP 竞态**：树账与运行账必须在各自的写入临界区内更新；跨核迁移沿用双持锁与 RqClaim，任何新字段都不能绕过 `cfs_on_cpu` 屏障。
- **Q16.16 迁移**：所有阈值、floor、placement、切片与测试断言必须一次换算，禁止两套单位并存；换算由 E-01 参考模型单源导出。
- **切片归属与迁移**：`slice_left/vd` 必须随任务迁移/重投携带；若误放每核槽位，会造成迁移后预算丢失或跨核写同一字段。
- **切片语义**：默认切片随总权重变化（D8①），请求切片夹取到固定区间（D8③）；不得把默认值写死为常量。
- **唤醒抖动**：若唤醒抢占判据过宽，网络 e2e 会出现频繁切换；若过窄，短请求延迟上升；必须用 e2e 与基准双读数裁定。
- **与 K-04 的范围冲突**：EEVDF 不实现线程组、CLONE_THREAD、共享资源语义；K-04 不重新设计 EEVDF 公式。两工程不得互相吞并阶段。
- **TCB 边界**：队列状态与选择算法留在 `privileged/`（需要原子/锁/裸指针语义）；functions 只经安全 API 调用，保持 F1/F2。
- **回退与死代码**：D6 若保留回退路径，必须是真实可达路径（运行期策略位或 feature 构建），不能只是「注释掉 + `#[allow(dead_code)]`」。

## 7. 与多线程工程的协同

- 两个工程同级、独立立项：EEVDF 解决「Normal 类怎么分配 CPU」，多线程工程解决「调度单位是进程还是线程」。
- D5=①：EEVDF 的 E-01..E-07 先独立完成；K-04 按多线程 D7 只做线程组语义收口，E-08 只做口径对齐与进程级捷径审计。
- 任一顺序都要求：调度字段只有一个写入者（rq 锁或 PROCESS_TABLE 的既定锁序）、迁移协议只有一套、回归锁不重复实现。

## 8. 变更历史

- 创建本工程：用户决策 EEVDF 调度算法实现为独立大工程，与多线程工程同级；登记现状调研、设计草案、开放决策 D4-D10、阶段 E-01..E-08 与验证门槛。
- 记录 D4-D10 裁定：D4=B / D5=① / D6=① / D7=混合单树资格前缀 / D8=①+③ / D9=③ / D10=② Q16.16；修正 D10 截断口径为「所有 `w >= NICE0_WEIGHT` 均退化为 1」。
- 设计口径修正：切片剩余量改为每任务持有；账目拆为树账 + 每核运行账；唤醒落点改用 `V - slice_virt`（floor 不再参与）；三分入队语义 `Wake/Migrate/Requeue`；无合格任务改 current-aware fallback；`steal_locked` 候选收敛到合格前缀。
- 阶段修正：E-03 前置于 E-02 以消除账目循环依赖；E-05 拆为 E-05a（入队/唤醒/prev 重投）与 E-05b（迁移/负载均衡）；E-08 由「字段机械迁移」改为线程组语义对齐（因多线程 D7 的 Process=每线程 TCB 口径）。
- E-01 完成：新增 host 参考模型（Q16.16 纯整数）与 14 项对拍测试；冻结 `slice_virt = Δvr * slice_real`、`W_inc` 口径与三分入队语义；修正 IE3 为「信用有界 + 欠账保留」；证明非空可运行集必有合格任务（current-aware fallback 降为防御路径）；§3.8 开放议题全部收口，E-01 状态置 `[X]`。
- E-03 完成：`CfsRunQueue` 落树账/运行账分解与 `pick_eevdf`（单树合格前缀 + current-aware + 最小 `(vd, pid)`），提供 `reweight` / `advance_running_vruntime` 两个 rq 锁内入口；9 个纯函数与 E-01 模型对拍，`Process` 增五个 EEVDF 字段；本阶段不接生产 tick（切片生效与抢占归 E-04，权重链路归 E-02），状态置 `[X]`。
- E-02 完成：`Scheduler::set_nice` 进入 setpriority/nice 生产路径，按「认领 → rq 锁 → 写 nice/权重 → `reweight` → 重建切片/vd」收口，新增不持锁等待的 `claim_rq_blocking`；functions 侧 `getpriority` 返回真实 nice、负数 nice 误判修复；新增 `Process::get_nice` 与 9 项回归锁。fork 未继承 nice/权重的审计发现登记至 E-06，状态置 `[X]`。
- E-04 完成：tick CFS 分支收口为 `tick_current_cfs`，在同一 rq 锁内推进 vruntime/运行账/floor，并执行切片 Build/Advance/Expire 状态机；到期按 `Requeue` 原位入树并置 `need_reschedule`，切换仍由中断退出路径执行；Requeue 仅限运行账在册或已在树的任务，boot keep-current 不入树。同批删除 `cfs_should_preempt`、`floor + TARGET_LATENCY_TICKS` 旧判据与 `vr << VIRT_SHIFT` 桥，全链路统一 Q16.16；唤醒抢占归 E-05a。新增 5 项 tick 回归锁，状态置 `[X]`。
- E-05a 完成：`PlacementKind` 三分入队语义与 `plan_placement` 纯函数落地，`place_locked` 升为唯一投送主体（Wake 睡眠信用上界 + 入队后 `W` 重建切片、Requeue 携带/耗尽重建、重复 Wake 降级、屏障分支在 `PROCESS_TABLE` 同一临界区原子落账）；`unblock`/`add`/`add_to_run_queue` 接 Wake，`requeue_prev`/`reap_off_cpu`/RT 降级接 Requeue；唤醒抢占按入队后资格与 `vd_new < vd_current` 判定且仍只置 `need_reschedule`。Migrate 生产接线与跨核账目守恒留 E-05b。新增 8 项回归锁并同步四份既有契约，状态置 `[X]`。
- E-05b 完成：`CfsRunQueue::migrate` 落原样迁移入口（撤源账 + 目的按原落点记账，不做目的核 floor 钳制），`steal_locked` 改走 `src.dequeue → dst.migrate` 并在同一临界区回写 vr/`cfs_on_rq`/`vd`，切片预算随任务携带（IE5/IE6）；候选由 `leftmost_pids` 收敛为 `eligible_pids` 合格前缀，再按共享 `effective_deadline` 取窗口内最大 `vd`（最不急迫）。评估结论：同候选集合下，最大 `vd` 相比最左端只把搬动顺序从「源核最急需」改为「目的核最不急迫」，不改变资格与守恒。新增 2 项迁移回归锁 + 2 项 `cfs` 内联用例，`smp_rq_migration_test` 扩为 11 项、屏障契约补撤账次序断言，状态置 `[X]`。
- E-06 完成：`edgine-abi::sched` 固化调度 syscall 常量与 `sched_attr` 布局，privileged `syscall::types` 转出、functions 经 `syscall::types` 代理消费（单一权威源，无新依赖、无 F2 穿透）；`Scheduler::set_requested_slice` 按 D8③ 夹取请求切片并在同一 rq 锁内重建在队任务的本轮切片与 `vd`；`sched_setscheduler`/`getscheduler`/`setattr`/`getattr` 四条编号接线，策略只接受 `SCHED_OTHER`、修改限自身（码序 EINVAL→ESRCH→EPERM）、未支持字段逐项 `EINVAL`；fork 补 nice/`cfs_weight`/`ProcessPriority` 继承而每轮账目与请求切片不继承。新增 12 项 syscall 回归锁 + 1 项 fork 继承内联用例，状态置 `[X]`。
- E-07 完成：按 D6① 删除 `CfsRunQueue::pick_next` 旧选择器（`DlRunQueue::pick_next` 保留，属 DL 生产选路），测试调用点统一改走 `pick_eevdf`，新增源码契约锁禁止旧选择器复活；新增生产队列 `eevdf_pick_requeue` 基准（255 任务稳态 550.37 ns/op），与手工 `BTreeMap` 基准（~48 ns/op）对照并归因于双账维护与合格前缀扫描，baseline 重录为 24 条；docs/explain 三处 CFS/MLFQ 表述同步为 EEVDF；双架构构建/全量审计/host+内核 host 测试/rustdoc/QEMU + 双架构网络 e2e 全绿，状态置 `[X]`。
