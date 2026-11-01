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
| D4 实施深度 | 待裁：A 简约 / B 相对完整 | 涉及架构关键路径，按 §11.3 须由用户二选一后再施工 |
| D5 与 K-04 次序 | 待裁：① 先进程级 EEVDF，后线程维度迁移 ② 先 K-04 线程化，再做线程级 EEVDF ③ 两者合并 | 决定调度实体的落点（`Process` 还是 `Thread`）与回归面 |
| D6 CFS 去留 | 待裁：① 直接替换 ② 保留可回退开关 ③ 保留最小回退 | 关系 F9 死代码处置与回退风险 |
| D7 队列结构 | 待裁：① 单 `BTreeSet` + 线性筛选 ② 双索引（vr / vd）③ intrusive RB tree | `MAX_PROCESSES = 256`，候选集上限 255；先以基准读数裁定 |
| D8 切片来源 | 待裁：① 按队列总权重动态算 ② 每任务固定虚拟切片 ③ 开放 `sched_setattr` 请求值 | 决定默认时延、时延抖动与 syscall 面 |
| D9 syscall 面 | 待裁：① 只打通 `nice`/`setpriority` ② + `sched_setscheduler`/`getscheduler` ③ + `sched_setattr`/`getattr` | 至少须修复「nice 当前不改变 CFS 权重」的既有缺口 |
| D10 虚拟时间精度 | 待裁：① 维持整数 tick 粒度 ② 定点（如 Q16.16） | 现有整数截断使 `w > NICE0_WEIGHT` 的权重全部退化为 1 tick/tick |

> D4-D10 属决策灰色地带（§9.1）与架构关键路径（§11.3）。本工程**先立项、先设计**，未获用户授权前不进入生产代码施工。

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

## 3. 设计模型与开放议题

> 本节是 E-01 的输入草案，不是已定稿设计。所有公式与阈值须在 E-01 中逐项评审并由用户裁定 D4-D10 后固化。

### 3.1 每任务状态

沿用现有 `Process` 调度字段（[process.rs:198-233](../../src/kernel/privileged/proc/process.rs#L198-L233)）：

- `cfs_vruntime`：虚拟运行时间（与 rq 同一时基）。
- `cfs_weight`：由 nice 派生的权重。
- `cfs_on_rq` / `cfs_rq_cpu` / `cfs_on_cpu`：可运行记账、归属核、投送屏障。

EEVDF 新增候选字段：

- `eevdf_vdeadline`：本轮的虚拟截止期 `vd`，用于跨任务比较与唤醒抢占。
- `eevdf_slice_virt`：本任务请求的虚拟切片；默认由 rq 总权重动态计算，`sched_setattr` 路线下可覆盖。
- 运行切片剩余量**不落 `Process`**，由每核 `PerCpuSched` 的当前任务槽位持有（任务只在树外运行期间消耗），避免多核并发改同一字段。

### 3.2 每核状态与新增不变式

在 `CfsRunQueue` 现有 `tree` / `min_vruntime` / `total_weight` 之外增加：

- `sum_w_vr: u128`：所有 `cfs_on_rq` 任务的 `w * vr` 之和（含运行任务）。
- `V`：加权平均虚拟时间，`V = sum_w_vr / total_weight`；只作展示与调试字段，资格判定用交叉乘法避免除法误差。
- `running_slice_left`：当前运行任务剩余的真实切片（tick）。

新增不变式（与 IC1-IC3 并行，编号待 E-01 固定）：

- **IE1 同域**：`V`、`vr`、`floor`、`vd` 在同一虚拟时间单位与同一时基域。
- **IE2 账目完备**：每个 `cfs_on_rq == true` 的任务恰好被 `total_weight` 与 `sum_w_vr` 计入一次；运行任务计入但不在树。
- **IE3 有界欠账**：入队/唤醒时 `lag = V - vr` 被钳制在 `[0, slice_virt]`（或按 D8 的请求切片上界）；睡眠不产生无界信用。
- **IE4 floor 单调**：`min_vruntime` 的单调性不因 EEVDF 改变；`V` 不要求单调（低 vr 任务入队会拉低平均）。
- **IE5 迁移守恒**：跨核迁移在源/目的双持锁内完成「源撤账 + 目的记账」，两核的 `total_weight` 与 `sum_w_vr` 变化量互为相反数。

### 3.3 核心公式（提案）

- 虚拟时间推进（沿用现状或定点化，取决于 D10）：`vr' = vr + max(NICE0_WEIGHT / w, 1)`。
- 加权平均：`V = (Σ w_i * vr_i) / (Σ w_i)`。
- 欠账：`lag_i = V - vr_i`；**合格** ⇔ `lag_i >= 0`（等价 `vr_i * W <= sum_w_vr`）。
- 虚拟截止期：`vd_i = vr_i + slice_v_i`。
- 默认虚拟切片：`slice_v = max(1, TARGET_LATENCY_TICKS * NICE0_WEIGHT / W)`；对应真实切片 `slice_real = slice_v * w / NICE0_WEIGHT`，与现有 `calc_time_slice = TARGET_LATENCY * w / W` 在整数域同源。
- 选择：在合格集合中取 `(vd, pid)` 最小者；若无合格任务，退化为取 `(vr, pid)` 最小者（保证服务推进）。
- 唤醒落点：`vr_wake = max(vr_raw, V - slice_v)`，再计算 `vd`；`lag` 因此不超过一个切片。
- 唤醒抢占：仅当新任务合格且 `vd_new < vd_current`（或当前切片已耗尽）才请求重调度。

### 3.4 选择路径与 current

运行任务保持在树外（与现状一致），但新增两项判据：

- **切片到期**：`running_slice_left` 归零 ⇒ `need_reschedule`；任务重新入树时重算默认切片。
- **唤醒抢占**：在 `unblock` / 投送路径比较候选与当前任务的 `vd`，而不是只要唤醒就无条件重调度。

`pick_next_task` 仍只从树中选；区别是 CFS 分支由「最小 vr」改为 `pick_eevdf`。若当前任务不应被切走，唤醒侧就不该设置重调度标记；这样避免「每次唤醒都切换」的抖动。

### 3.5 时基、溢出与精度

- `sum_w_vr` 用 `u128` 防止 `w * vr` 在长 uptime 下溢出；`V` 取商时饱和到 `u64`。
- `total_weight` 最大约 `255 * 88_761 ≈ 22.6M`，`u64` 足够；`u128` 只用于乘积和。
- 定点路线（D10②）把 `vr` 单位放大 `2^F`，所有阈值、floor、测试断言同步换算；整数路线（D10①）须在文档中明确「w > NICE0 的权重退化」是已知边界，不能宣称完整 nice 语义。
- 所有入队/出队/迁移路径必须以**幂等**方式更新 `sum_w_vr`，并用「随机操作序列后与暴力重算对账」的属性测试兜底。

### 3.6 与 DL/RT、idle、负载均衡的关系

- EEVDF 只替换 `SchedPolicy::Normal` 的 CFS 选择规则；DL → RT → CFS 的类间优先级不变。
- idle 任务的策略仍为 `SchedPolicy::Idle`，不进入 CFS 选择；boot 期「从未入过 CFS 队列」的 keep-current 路径（[scheduler.rs:1225-1248](../../src/kernel/privileged/proc/scheduler.rs#L1225-L1248)）必须保留，且不得被 EEVDF 的 `V` 计算误判为空闲。
- 负载均衡的候选选择与搬动记账必须与 EEVDF 账目同源：搬运一个任务时，源/目的两棵树的 `V` 与 `sum_w_vr` 都要在双锁内更新；不得为了简化只搬树节点。
- 当前 `steal_locked` 以最左端 vr 为候选（[scheduler.rs:2280-2340](../../src/kernel/privileged/proc/scheduler.rs#L2280-L2340)）；EEVDF 下应评估改为「取最不急迫者（最大 `vd`）」是否更优，作为 E-05 的设计项，不在 E-03 预判。

### 3.7 与多线程工程的边界

- **EEVDF = Normal 类内部的分配策略；K-04 = 调度实体从 Process 迁到 Thread**。两者正交，但共享同一组 `vr/weight/slice/deadline` 字段。
- 若 D5 选①（先进程级）：EEVDF 先落在 `Process` 字段，K-04 落地时把这组字段机械迁移到线程实体；EEVDF 公式不变。
- 若 D5 选②（先 K-04）：EEVDF 直接在 Thread 上实现，避免一次迁移；代价是调度器演进的验证被多线程工程阻塞。
- 若 D5 选③（合并）：两个大工程的阶段表合并，风险与工期最大，不建议作为默认路径。
- 无论哪条路线，本工程**不新增**「调度实体 trait」抽象来同时适配 Process/Thread；那是为假想未来预留的复杂度，按 §11.3 简约原则由 D5 裁定后直接落字段。

### 3.8 开放议题（E-01 必须逐项给出结论）

- 睡眠任务的欠账上界：一个切片、`TARGET_LATENCY`、还是固定 tick 上限？
- 新 fork / exec 任务的初始 `vr`：继承父任务、落在 `V`、还是 `V - slice`？
- 低权重任务的切片下限与高权重任务的切片上限：`MIN_GRANULARITY` 是否继续作真实切片下限？
- 饥饿证明：在「合格集合为空」时退化到最小 vr，是否足以覆盖所有 lag 为负的极端场景？
- 抢占延迟：唤醒抢占只在 `vd` 更早时触发，是否会让高权重短切片任务在网络唤醒路径上被延迟？
- 与 `cfs_should_preempt` 的关系：直接删除、降级为兼容阈值、还是保留为 EEVDF 的二级保险？
- `V` 的表示：只作调试字段，还是需要在 `schedstat` / procfs 中暴露？
- 回退：D6 若选②/③，回退开关放编译期 feature 还是运行期策略位？回退路径不得引入死代码抑制。

## 4. 实施阶段（循序渐进，每阶段 = 状态 [X] + 验证门槛）

- **E-01. 设计与口径定稿**
  - 描述：裁定 D4-D10；固定 EEVDF 术语、公式、单位、舍入与边界；定义 IE1-IE5 与现有 IC1-IC3 的关系；把 §3 的开放议题逐项收口。
  - 方案：逐项列 A/B/C 方案与推荐，交用户裁定；用独立参考模型（host 侧纯函数）验证公式与边界；本阶段不改生产代码。
  - 状态：[]
- **E-02. 权重链路打通与调度字段审计**
  - 描述：修复 `nice`/`setpriority` 只写 `ProcessPriority`、不写 `nice`/`cfs_weight` 的既有缺口；让 `Scheduler::set_nice` 进入生产调用路径；核对 `Process::new` / `init_kernel_process_fields` 的默认值（[process.rs:424-426](../../src/kernel/privileged/proc/process.rs#L424-L426)、[user_proc.rs:590](../../src/kernel/privileged/proc/user_proc.rs#L590)）。
  - 方案：在 functions 层经 privileged 安全 API 更新 `nice` + `cfs_weight`；对已在 rq 上的任务同步修正 `total_weight` 与 `sum_w_vr`（按 D5 裁定的实体落点）；补 nice → weight → vruntime 增量的单测与负控制。
  - 状态：[]
- **E-03. EEVDF 队列机制与纯函数**
  - 描述：为 `CfsRunQueue` 增加 `sum_w_vr`、`V` 计算、合格判定、虚拟截止期与 `pick_eevdf`；把 `calc_time_slice` 从「仅测试调用」变为生产切片来源；保留 IC1-IC3 与现有幂等入队/撤账语义。
  - 方案：先实现不接生产 tick 的纯函数与属性测试（暴力重算/资格集合/无重复节点/权重守恒）；`pick_eevdf` 用单树线性筛选（D7①）还是双索引由 D7 裁定；所有写路径在同一 rq 锁内更新账目。
  - 状态：[]
- **E-04. tick 记账、切片预算与抢占接线**
  - 描述：运行任务的 vruntime 推进同时维护 `sum_w_vr`/`V`；`running_slice_left` 递减并在到期时请求重调度；用 EEVDF 判据替换或收编现有 `should_yield`。
  - 方案：保持 `advance_floor` 与 IC1/IC3；tick 热路径只做 O(1) 账目更新；重调度仍走 `mark_resched_pending_local` / `run_pending_resched`，不在中断上下文直接切换。
  - 状态：[]
- **E-05. 入队、唤醒、迁移与负载均衡**
  - 描述：将唤醒落点改为 `vr = max(vr_raw, V - slice_v)` 并写入 `vd`；实现唤醒抢占判据；block/unblock、prev 重投、跨核迁移与 `steal_locked` 全部同步维护 EEVDF 账目。
  - 方案：沿用 `RqClaim` + `cfs_on_cpu` 屏障 + 升序双锁，不新增锁；在 `smp_rq_migration_test` / `sched_offcore_placement_barrier_test` 上扩展 EEVDF 断言；负载均衡候选选择按 §3.6 的评估结论实施。
  - 状态：[]
- **E-06. functions/syscall 面与可观测**
  - 描述：按 D9 打通 nice/setpriority 的权重效果；视裁定增加 `sched_setscheduler`/`getscheduler` 或 `sched_setattr`/`getattr`；必要时暴露 `V`/lag/切片统计供 procfs 或测试读取。
  - 方案：ABI 常量落 `edgine-abi` 单源；functions 层只调用 privileged 安全 API（F1/F2）；权限与 `SCHED_NORMAL` 之外策略的返回码逐项测试；不在 functions 层复制调度公式。
  - 状态：[]
- **E-07. 回归锁、性能基线与文档收口**
  - 描述：把 EEVDF 语义固化为 host-tests 回归锁；补内核队列选择路径的基准并与 `cfs_btreemap_bench_test` 对照；跑 QEMU 双架构与网络 e2e；按 D6 决定 CFS 旧路径的去留并同步 docs/explain。
  - 方案：每阶段跑 §2.3 六门槛；E-07 末尾跑全量 `bash ci/audit.sh full` + `./ci/build.sh all` + `make test-host` + `make test-kernel-host` + `make -f Makefile.ci ci-rustdoc` + QEMU；`host-tests/benches/baseline.json` 按 §8 更新；删除旧路径时不得用 `#[allow(dead_code)]` 等抑制（F9）。
  - 状态：[]
- **E-08. 线程维度交接（依赖多线程 K-04）**
  - 描述：当多线程工程完成线程级调度实体后，把 `vr/weight/slice/vd` 从 `Process` 机械迁移到线程实体，保持 EEVDF 公式与阈值不变。
  - 方案：与 K-04 同一批次评审字段迁移、current 语义、tick 记账与迁移协议；本工程只提供公式与测试，不重复实现线程组语义。
  - 状态：[]

### 依赖关系

```
E-01 → E-02 → E-03 → E-04 → E-05 → E-06 → E-07 → E-08
```

- E-02 可与 E-03 的纯函数设计并行，但 E-04 必须在两者之后（否则权重语义仍断链）。
- E-05 依赖 E-03 的队列账目与 E-04 的切片判据；E-06 依赖 E-05 的唤醒/切片语义。
- E-08 依赖本工程 E-07 与多线程工程 K-04；两条工程线可独立推进，交接点只做字段迁移。
- E-01 未获 D4 裁定前，E-02 起不得开工。

## 5. 验证门槛（每阶段不可豁免）

- §2.3 六条门槛全量通过：双架构构建、clippy/fmt、核心审计、host-tests、QEMU、host 侧内核单元测试；审计档位必须显式 `full`。
- 调度专项回归：`cfs_single_time_base_test`、`smp_rq_migration_test`、`sched_offcore_placement_barrier_test`、`sched_current_type_consistency_test`、`td10_utime_stime_test` 全绿。
- 新增属性测试至少覆盖：`sum_w_vr` 与暴力重算一致、合格集合与 lag 符号一致、无重复节点、权重守恒、迁移前后两核账目守恒、唤醒欠账有界、切片到期必请求重调度。
- 网络 e2e：沿用 `E2E_REPEATS`（[qemu_boot_test.sh:43](../../scripts/qemu_boot_test.sh#L43)）与 [qemu_liveness_probe.sh](../../scripts/qemu_liveness_probe.sh) 的既有回归；如需 1000-boot soak，按 P-29/P-30 的手工复跑方式执行并把读数落档，不把未落脚本的流程写成现成门禁。
- 性能：E-03 建立队列选择基准，E-07 与基线对照；任何回退必须给出根因与后续计划，不得静默放宽。
- F9：旧 CFS 选择/切片函数若被替换，必须删除或经真实使用路径消费；不得新增 `#[allow(dead_code)]`、`#[allow(clippy::…)]` 或构建参数逃逸。

## 6. 风险与边界

- **架构关键路径**：调度器改动直接决定所有进程的 CPU 分配与网络唤醒时延；D4 未裁定前不施工，避免「先做后补设计」。
- **SMP 竞态**：`V`/`sum_w_vr` 与 `total_weight` 必须在同一 rq 锁域更新；跨核迁移沿用双持锁与 RqClaim，任何新字段都不能绕过 `cfs_on_cpu` 屏障。
- **整数/定点迁移**：D10① 的权重截断、D10② 的全域单位换算都会触碰 floor、placement、测试断言与 procfs 读数；必须一次定口径，禁止两套单位并存。
- **切片语义**：默认切片按总权重动态计算会让「任务数变化」改变单个任务的切片长度；D8 裁定前不得把它写死为常量。
- **唤醒抖动**：若唤醒抢占判据过宽，网络 e2e 会出现频繁切换；若过窄，短请求延迟上升；必须用 e2e 与基准双读数裁定。
- **与 K-04 的范围冲突**：EEVDF 不实现线程组、CLONE_THREAD、共享资源语义；K-04 不重新设计 EEVDF 公式。两工程不得互相吞并阶段。
- **TCB 边界**：队列状态与选择算法留在 `privileged/`（需要原子/锁/裸指针语义）；functions 只经安全 API 调用，保持 F1/F2。
- **回退与死代码**：D6 若保留回退路径，必须是真实可达路径（运行期策略位或 feature 构建），不能只是「注释掉 + `#[allow(dead_code)]`」。

## 7. 与多线程工程的协同

- 两个工程同级、独立立项：EEVDF 解决「Normal 类怎么分配 CPU」，多线程工程解决「调度单位是进程还是线程」。
- 推荐顺序由 D5 裁定；若选①，EEVDF 的 E-01..E-07 可先独立完成，K-04 落地时只迁移字段与 current 语义；若选②，本工程 E-08 前移并与 K-04 合并评审。
- 任一顺序都要求：调度字段只有一个写入者（rq 锁或 PROCESS_TABLE 的既定锁序）、迁移协议只有一套、回归锁不重复实现。

## 8. 变更历史

- 创建本工程：用户决策 EEVDF 调度算法实现为独立大工程，与多线程工程同级；登记现状调研、设计草案、开放决策 D4-D10、阶段 E-01..E-08 与验证门槛。
