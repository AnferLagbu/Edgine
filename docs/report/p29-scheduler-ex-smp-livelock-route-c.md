# P-29：Mode X 真冻结根因坐实与 SchedulerEx 并行实现移除（route C）

> 定位：本文是 P-29 工程的结果快照——记录 `net-e2e-tcp-echo-flakiness` Mode X「关中断自旋死锁」的**真根因**如何在前序 PROCESS_TABLE/NET_STATE 判读被否证后收敛到「全局单例无锁 `SCHEDULER_EX` 被每核 tick 并发驱动 → `boost_all` livelock」，以及据此做出的结构性修复（route C：彻底移除并行调度器实现）与其 1000-boot soak 验效。发布即冻结，作为后续工程（含新立 P-30）的输入依据。
>
> 关联：`docs/report/p28-mode-x-freeze-verdict.md`（判型产物，本文上游）、`docs/report/p27-e2e-flake-forensic-report.md`（取证产物）、`docs/plan/net-e2e-tcp-echo-flakiness.md`（P-29 台账闭合 + P-30 登记）。

## 一、从误判到真因：判读演化链

P-28 用 QEMU HMP monitor 现场快照把 Mode X 主形态判为「假说③ 关中断自旋死锁」，并经 DWARF addr2line 把自旋点收敛到 `schedule()` → `reap_off_cpu` → `ProcessTable::with_process` 取 **PROCESS_TABLE** 锁。这条判读随后被 P-29 的多轮取证**否证并两次改道**：

1. **PROCESS_TABLE 持锁者假设不成立**。monitor 现场反复显示：所谓「持 PROCESS_TABLE 不放的核」并不在任何运行核的 RIP 上，且 schedule 入口即关中断、取锁前后的快照无法自洽指向某个长期持锁者。更关键的是，用 tick 计数双趟读（`TICK_IRQ_COUNT` 二次采样求 delta）复核时，多数 PROCESS_TABLE/NET_STATE 现场的 **delta ≠ 0**——时基仍在推进，系统其实活着，那只是锁**竞争**（瞬态），不是**冻结**（真死锁）。把 delta≠0 的竞争当冻结立项，是 P-29 前段的最大方法论错误。

2. **NET_STATE「n=2 逐字节同形」亦是同族误判**。boot14/boot172 两个 NET_STATE Mutex 现场同样落在 delta≠0 的竞争带，被误当成独立病灶统计。

3. 只有把判据严格钉在 **delta = 0**（全核时基停摆）后，真冻结样本才浮出水面——boot23 与 boot594，二者栈形一致，自旋/空转点指向**旧 `SCHEDULER_EX` 全局线程级调度器**，而非 PROCESS_TABLE/NET_STATE。

方法论教训固化为一条：**monitor 快照判死的唯一硬判据是 tick 计数 delta 是否为 0；delta≠0 一律降级为竞争，不得立项为冻结**。前序所有 PROCESS_TABLE/NET_STATE 结论均据此重估。

## 二、根因：全局单例无锁 RunQueue 的每核并发 livelock

坐实的真因结构如下。旧实现存在一个进程级调度器之外的并行线程级单例 `SCHEDULER_EX`，其 RunQueue 由 `AtomicU64 head` + `AtomicU32 count` 构成，但 `next`/`prev` 指针**非原子**。`Scheduler::tick` 首行 `disable_interrupts` 只能防**同核重入**，无法提供**跨核串行**。

在 SMP（本项目取证用 `-smp 4`）下，**每个核的 timer tick 都会并发驱动这同一个全局 RunQueue**。一旦 `boost_all` 例程进入 `while let Some(t) = pop_front() { push_back(t) }` 的重排循环，多个核同时改写同一条非原子链表，指针相对次序被并发撕裂，循环**永不收敛**——它不 panic、不死锁于某把锁，而是**空转 livelock**：持有 tick 的核被这条永不返回的循环占死，时基因此停摆（表现为 delta = 0），整系统在「活着」的假象下冻结。

这解释了为何 PROCESS_TABLE 锁序审计（六条阻塞循环 / `poll_network` / `*_locked` 均无持锁再入或阻塞边）与 ABBA 跨核互等（单自旋核 + 唤醒在锁外）都被逐一否证——真正的病灶根本不在锁序图上，而在**无锁结构的并发重排不收敛**。

## 三、修复形状：route C —— 移除并行实现，而非给 livelock 打补丁

面向「给 `SCHEDULER_EX` 的 RunQueue 补跨核串行」与「彻底移除这条并行实现」两条路，用户裁定后者（route C）。判据是架构性的：项目终态是**单一 per-CPU 进程级调度器**（`Scheduler`），`SCHEDULER_EX` 是历史遗留的线程级并行实现，二者职责重叠正是并发撕裂的土壤；保留它并加固，等于维护第二套需要独立证明正确的调度机制。移除并行实现同时消除 livelock 根因与长期双实现漂移风险，与 `eliminate-parallel-implementations` 的工程主张一致。

route C 的实施要点（收敛为一处 per-CPU 时间基）：

- **删两文件**：`privileged/proc/scheduler_ex.rs` 与 `privileged/proc/thread.rs` 整体移除。
- **迁记账**：原先仅由 `SCHEDULER_EX.tick_accounting()` 驱动的进程 user/sys 时间记账，改由 per-CPU `Scheduler::tick` 直接调 `proc_account_tick(in_kern)` 承担（`in_kern` 经 `proc_get_in_kern()` 取）。此迁移不新增任何锁，未触及 tick 内既有的 freg `RECOVERY_MANAGER` 取锁点。
- **重定向入口**：`sched_ops` / `pi_mutex` 等对 `SCHEDULER_EX`/`Thread` 的 5 处引用重定向到进程级等价 API，删除相应 import。
- **切除孤儿策略层**：`SCHEDULER_EX` 删除后，`current_sched_decision()` 失去唯一消费者，整套 `SchedDecision` 策略层（`sched_trait` / `DefaultPolicy` / `FallbackPolicy` / `register_default_policy` 及其 boot 死接线）连同线程模型遗留词汇 `ThreadPriority` / `ThreadState` 一并孤儿化。用户裁定「一并切除」而非留作休眠 API——符合 §5 F9 死代码零容忍。
- **清死谓词**：`ProcessState::is_runnable` / `can_freeze` 此前仅被已删的 thread/scheduler_ex 消费，`audit_unwired --strict` 报 R1 HIGH，据此移除（不扩台账、不留投机 API）。

## 四、验证：静态门槛 + 1000-boot soak

**静态门槛（§2.3）**：双架构 `./ci/build.sh all` 0 error / 0 warning（含 `--features lockdep` 路径）、`cargo clippy -D warnings`、`cargo fmt --check`、核心审计（boundary/safety/deadlock/coupling/comment/invariants/once_cell/c_naming/repr_c/volatile/static_mut/orphaned_init/unwired `--strict` 全 R1/R2=0）、host-tests 全绿、`make test-kernel-host` 985 全过；QEMU x86_64 单 boot 冒烟含 e2e 完整成功。三个因删文件而失效的 host-test（`safety_boilerplate` / `sched_current_type_consistency` / `td10_utime_stime`）分别改写为「并行实现不得重现」的回归守卫、scheduler.rs 重定向、以及 `Scheduler::tick` 调用 `proc_account_tick` 的结构断言。

**1000-boot soak（CONC=1，非侵入 monitor 快照，`-smp 4`）**：

| 类别 | 数量 | 判读 |
|---|---|---|
| 正常 boot | 989 | 无 freeze 事件 |
| freeze 现场抓取 | 11 | 客户端失败且 QEMU 存活 |
| └ 瞬态竞争（delta ≠ 0） | 10 | 时基仍推进，属 net recv-timeout flake（P-27 家族），非冻结 |
| └ 真冻结（delta = 0） | 1 | **boot284，非 `SCHEDULER_EX` 签名** |

结论两条：

1. **route C 的靶心已闭合**——旧 `boost_all` livelock 签名（全局无锁 RunQueue 每核并发重排不收敛）在 1000 boot 中**零复发**；且该代码路径已被物理删除，不存在复现土壤（scheduler.rs 中仅余解释性注释提及旧名，无代码引用）。

2. **唯一的真冻结（boot284）是一类正交病灶**——两核在 `Scheduler::tick` 路径上关中断自旋于 `SpinLock::raw_lock`：一核取 freg `RECOVERY_MANAGER` 锁、一核走 `load_balance` 取 `cfs_rq` 锁，串口显示触发链为 `[tcp] Connection accepted` → `[FREG] Rollback result=-1` → `GPF idt index=0`（一次罕见的 FREG BSR 升级/GPF）。经 HEAD 对拍，`Scheduler::tick` 内的 `RECOVERY_MANAGER.lock()` 在 route C 之前即存在，route C 对 tick 的唯一改动是记账替换（不加锁），故该冻结**既非 route C 引入、也非其靶心**，判为预存在的跨核锁纪律问题，另立 **P-30** 单开取证。样本 n=1 且泛型锁类型受 ICF 折叠不可单独尽信，P-30 复现与坐实前不预设修复形状。

## 五、遗留与后续

- **P-30**（新立）：boot284 型 freg `RECOVERY_MANAGER` × `cfs_rq` 跨核自旋冻结，1/1000 复现率，需按「delta=0 硬判据 + 连贯栈回溯 + 锁序审计」路线坐实持锁者与取锁序，再裁定修复深度。
- **取证工具的地址新鲜度陷阱**（本轮踩坑，记为经验）：非侵入 monitor 快照工具（dumper/scanner）硬编码的符号绝对 VA 会随任何改变 `.data`/`.text` 布局的重建而漂移，失效后 `xp` 恒读 0、伪报 delta=0。**每次重建后必须从 `nm kernel.bin` 重校正符号地址**，并以单 boot 的 delta（应为正常推进量级，非 0）验证工具可用后方可批量采样。
