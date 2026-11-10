# aarch64 `reap-timeout` 偶发根因工程

> 立项依据：2026-11-10 用户裁定「reap-timeout 立项根治」（四组裁定中的登记小项之一）。
> 关联登记：`multithreading-project.md` K-07a（线程探针）、P-78（`wait_reap` 真阻塞等待）、`bounded-wait-deadline.md`（等待语义改墙钟截止）、`net-e2e-tcp-echo-flakiness.md` P-80（独立观察项，不合并归因）。

## 1. 现象与既有证据

- **失败签名**：aarch64 `[THREAD] FAIL: reap-timeout code=0`，随后同轮 `[tcp] FAIL: socket()=-22` 级联，`thread_stress_test.sh` 单轮判 FAIL，full 档步骤 4 以 exit=1 终止。
- **复现率**：登记口径约 1/8–1/11（每次 aarch64 单轮）。2026-11-10 full 首跑命中 1 次（3 轮中第 2 轮），同日单独复跑 6/6 绿。
- **现场**（`other/build/log/thread_stress_aarch64_fail_2.log`）：
  - `[REAP] tgid=13 unreaped pid=13 state=Running/Ready` 交替约 0.8–1.07 s，此后同签名去重不再打印；
  - 截止时刻（`THREAD_WAIT_BUDGET_MS = 5000`，见 `src/user/init/src/main.rs`）到达时组长仍非 Zombie，`group_exit_leg` 报 `reap-timeout`；
  - 同轮 `[SCHED] SNAP END ... conserve=ok`、`[SIG] PASS`、`[COW] PASS` 均正常 ⇒ 不是整机冻结；
  - 历史同族现场（`bounded-wait-deadline.md` §5 归因段）为 `[REAP] tgid=13 unreaped pid=14 state=Blocked` ⇒ 同族至少两种现场形态。
- **探针逻辑**：`group_exit_leg` 以 `WNOHANG` + 1 ms `nanosleep` 轮询子组退出码，5 s 截止；子进程 `group_child_body` 建 helper 线程 → 等 helper 报到（同一截止）→ `exit_group(7)`；helper 睡在 `GROUP_HOLD` 的 futex 上，唯一合法唤醒源是组退出路径（SIGKILL + unblock）。
- **内核路径**：P-78 后 `wait_reap` 的阻塞等待已修；本探针走 `WNOHANG` 非阻塞路径，嫌疑面因此在**组退出/收割链**（`exit_group` → 成员落 Zombie → `process_group_exit_code` 可见），而非父侧等待机制。
- **跨架构旁证（x86_64，-smp 4，2026-11-10 接线 D-3 liveness 腿时发现）**：`scripts/qemu_liveness_probe.sh` 在 `SMP=4` 下间歇出现 `[IDT] user exception: vec=13 err=0x0 rip=0x404302 rsp=0x408040` 的**同一 pid 反复死亡**循环（样本：3 s 内 235 次 `exit: pid=12 code=1`；另一 12 s 样本 1011 次），而 `SMP=2` 的 boot 门禁与 liveness 腿均为 0 次。`rip=0x404302` 经 `objdump` 定位为 `userlib::sys::thread_trampoline` 的 `hlt`（`call thread_body` → `syscall exit` → `hlt` 的死循环兜底）——即**线程走到 `exit` 之后没有终止、返回用户态执行了 `hlt`**，`#GP` 杀进程后同一 pid 被反复重建/重投。间歇性：5 次 `-smp 4` 运行中 3 次复现、1 次全程干净（另 1 次为旧镜像）。候选机理（与本册 H1/H2 同族，待 R-3 探针分辨）：① `exit` 系统调用被信号/IPI 可中断返回而未达终态；② 杀死的任务未从运行队列摘下、被再次调度。现行 §2.3 QEMU 腿只用 `-smp 2`，故该签名不可见。证据日志：`other/build/log/liveness_smp4_evidence/boot1.log`（干净样本）与接线当轮的 `vec=13` 计数记录。

## 2. 待验证假设（按优先级）

- **H1 组退出链未达终态**：`exit_requested` 已置真（`[REAP]` 行只在置真后打印）但组长自身或 helper 仍有成员未落 Zombie。需确认 helper 是否已 Zombie、组长停在退出流程的哪一步。
- **H2 丢唤醒**：组退出侧的 SIGKILL + unblock 对 helper 的唤醒丢失，helper 永睡在 futex，组无法整体落 Zombie。P-78 修的是父侧等待，不覆盖此路径。
- **H3 跨核内存序**：aarch64 弱内存序下 `exit_requested` / 组退出码 / 成员状态的跨核读顺序不一致，`process_group_exit_code` 或 Zombie 判定延迟/漏判。
- **H4 调度饿死**：helper 或组长在 aarch64 EEVDF 合格集上饿死（与 P-78 同族但不同路径）。
- **H0 前置**：先区分「真挂死」与「宿主过载导致 5 s 预算内未完成」——取证轮须记录宿主负载与 tick 速率。

## 3. 施工条目

### R-1 取证 harness（零生产改动）

- 描述：固定 aarch64 单架构多轮（建议 100–300 轮，SMP=2）复现，逐轮保留完整串口日志与 `[REAP]`/`[SCHED]`/`[SMP]` 行；命中即快照。
- 方案：复用 `scripts/thread_stress_test.sh` 的镜像与 QEMU 参数；新增诊断脚本 `scripts/reap_timeout_soak.sh`（不入 CI 门禁），命中轮落日志并记录镜像 sha256；输出命中率与每轮截止余量。跨架构旁证腿可选：同一 harness 以 `SMP=4` 跑 x86_64 `scripts/qemu_liveness_probe.sh`，按 `vec=13 rip=0x404302` / 同 pid `exit code=1` 计数统计复现率（见 §1 旁证条）。
- 状态：[X]
- 详情：2026-11-10 以既有装备收口，未新增独立脚本：复现与取证复用 `scripts/thread_stress_test.sh` 多轮（aarch64 单架构 + 逐轮日志留存），命中样本 `thread_stress_aarch64_fail_25.log` 已给出 `[REAP]` / `[REDISPATCH]` 直证；根因（R-2）定死后独立 soak harness 不再有增量信息，按简约原则不另建装备。

### R-2 静态审计：组退出/收割链

- 描述：逐行审计 `exit_group` → `wake_child_waiters` → `process_group_exit_code` → `reap_group` 的状态机与锁边界，产出「终态定义 + 每步不变式」清单。
- 方案：读 `privileged/proc/scheduler.rs`（`wake_child_waiters` / `Scheduler::exit`）、`functions/proc/wait4.rs`、`privileged/proc/process.rs`（`diag_unreaped_group` / 组状态查询）；列出全部成员状态迁移与唤醒点，标注跨核读写与内存序。
- 状态：[X]
- 详情：2026-11-10 审计结论 —— **终态定义**：组可收割 = `has_exited()` **且组内全部 TCB `state == Zombie`**（`group_zombie_exit_code` / `reap_zombie_group`，`process.rs:1142-1161`、`1222-1238`）；`exit_requested` 只是"退出已请求"，不等于终态。**断点链**：① `Scheduler::exit` 自述"正常退出时状态为 Running/Ready"（`scheduler.rs:2297-2298`）；② 落 Zombie 的唯一写点 `let _ = proc.set_state_safe(ProcessState::Zombie)`（`scheduler.rs:2342`）静默丢弃转换失败；③ 状态机允许表只有 `Running/Blocked→Zombie`，缺 `Ready→Zombie` 与 `Frozen→Zombie`（`process.rs:679-704`）⇒ 以 Ready 进入 exit 的 TCB 永远不会成为僵尸；④ 失败后 TCB 仍 Ready 且留在 EEVDF 可运行集（`pick_eevdf_task` 只排除 Blocked/Zombie，`scheduler.rs:1424-1437`），`exit` 末尾 `Self::schedule()`（`2402`）可经 `next == current_pid` 快路径原地返回（`1489-1494`）⇒ **exit 返回用户态**；即便被切走，`prev_state_code==2` 也走 `requeue_prev` 重投（`1563-1578`、`1712-1802`）。**实证**（`other/build/log/thread_stress_aarch64_fail_25.log`）：L213-217 同一 pid 两次 `exit: pid=14 code=7`，中间 `[REAP] ... state=Ready`（`process.rs:1209`）与 `[REDISPATCH] via=exit pid=14 ... state=Ready gexit=1`（`scheduler.rs:2299`）；L247 于 5.445 s 仍有 `via=requeue ... state=Ready gexit=1`。§1 的 x86_64 `vec=13 rip=thread_trampoline hlt` 反复死亡即同一条"exit 返回用户态"链的旁证。**范围**：本结论证实 H1 的 `state=Ready/Frozen` 形态；`bounded-wait-deadline.md` §5 的 `state=Blocked` 历史样本属 H2（未走到退出收尾）候选，R-1/R-3 仍需覆盖。**候选 Ready 写入者（未定罪，交 R-3）**：`place_locked` 的 `set_state_safe(Ready)`（`scheduler.rs:1066`，无 Running 守卫）、`signal.rs` 的 `Cont`/`Stop` 直接 store（`787-788`、`808-809`）、`schedule` 快路径不回写 Running 使 Ready 粘滞（`1489-1494`）。

### R-3 靶向探针

- 描述：为 H1/H2/H3 各设计一条最小可观测证据（组退出时的逐成员状态快照、helper 唤醒计数、`exit_requested` 前后的序）。
- 方案：优先扩展既有 `[REAP]` 取证行（组终态快照），沿用 §6③a 的「锁外发射」纪律，避免持表锁打印。
- 状态：[X]
- 详情：2026-11-10 以既有取证行收口，未新增探针：`[REAP]`（组非僵尸成员）与 `[REDISPATCH]`（`state=Ready gexit=1` 反复重排）的时序已足以把 H1 的 `Ready/Frozen` 形态定死（同 pid 两次 `exit:` 之间夹 `state=Ready`），靶向探针无增量判据；H2/H3 仍按 R-2 的范围界定留待后续样本，不在本批施工面。

### R-4 修复与回归锁

- 描述：按 R-2/R-3 结论修根因；**修前不改断言、不放宽 5 s 预算**。
- 方案：若为丢唤醒，补「置状态后复核 + 唤醒」闭窗（与 P-78 父侧同构）；若为内存序，补 acquire/release 配对；若为调度饿死，单独归因 EEVDF。回归锁优先落 host 侧状态机契约 + QEMU 多轮腿。
- 状态：[X]
- 详情：2026-11-10 按用户裁定「B 相对完整」三项施工。**① 不可拒绝终态写点**：新增 `Process::mark_zombie`（任意存活态一律落 `Zombie`；已 `Terminated` 不动，`Zombie` 幂等），`Scheduler::exit` 改经它落终态，替掉被状态机拒绝且 `let _` 静默吞错的 `set_state_safe(Zombie)`。**② `exiting` 全面排除**：拣选面 `pick_eevdf_task` / `pick_deadline_task` / RT 拣选 / `keep_current` / `eevdf_current_view` / `steal_locked` / `ready_orphan_due`；投送面 `place_locked` 把 `exiting` 与 `Zombie`/`Terminated` 同价拒收（`gone`）；补投面 `reap_off_cpu` 排除 `was_exiting`；重排面 `schedule()` 的 prev 收尾对 `exiting` 不调 `requeue_prev`，改 `eevdf_deactivate` 撤账。**为何不会饿死退出中的任务**：内核态不可异步抢占（x86_64 `IA32_SFMASK` 在 syscall 入口清 IF、aarch64 异常入口屏蔽 DAIF.I，`run_pending_resched` 全部调用点都在 IF=0 处），`exiting` 只在任务自己的最后一次 `schedule()` 被观察到，退出链中途不会被切走。**③ 回归锁**：新增 `host-tests/tests/reap_terminal_state_test.rs` 8 例——状态机行为契约（`mark_zombie` 全存活态 + `Terminated` 保护 + 旧白名单仍拒 `Ready/Frozen→Zombie`）、逐站点形状谓词、11 条合成回退负控制；`eevdf_thread_entity_test` / `thread_group_k04_test` 两处针由旧机制 `set_state_safe(Zombie)` 改为 `mark_zombie()`（E-08 / K-06 契约语义不变）。**验证**：双架构 `./ci/build.sh all` 通过；`make test-host` 189 套件全绿、`make test-kernel-host` 513 passed；其余门槛与双架构多轮压力见 R-5。

### R-5 门槛与文档

- 描述：§2.3 全门槛 + thread-stress 多轮全绿；同步本册与 `multithreading-project.md` / `bounded-wait-deadline.md` 的偶发家族口径。
- 方案：判据 = `bash ci/audit.sh full` 全绿 + `THREAD_STRESS_ARCH=both` 多轮 0 失败；文档只更新状态与结论，不改历史正文。
- 状态：[X]
- 详情：2026-11-10 收口读数（全部串行执行）——`./ci/build.sh all` Passed 6 / Failed 0；`bash ci/audit.sh full` exit 0（含 clippy/fmt、2d/2e（10/10 腿无静默跳过）/2f、Lockbud、QEMU 双架构 2/2、P-62 feature 运行维 4/4、线程压力 3 轮 × 2 架构全过）；`make test-host` exit 0（189 个测试套件全 ok、无 FAILED）；`make test-kernel-host` exit 0（513 passed + 637 passed 两腿）；`make -f Makefile.ci ci-rustdoc` 双架构 exit 0。**加压腿**：`THREAD_STRESS_ARCH=both scripts/thread_stress_test.sh 20`（每架构 20 轮，逐轮留痕）= **40/40 通过、0 失败**；对照该签名历史命中量级（默认构建 aarch64 约 1/8–1/11 单轮）本批零命中。**判据未放宽**：`THREAD_WAIT_BUDGET_MS = 5000`、`reap-timeout` 判据与两处旧形状针的契约语义逐字未动。**文档**：本册 R-1–R-5 状态与详情；`multithreading-project.md` §8.1「§6③a」行与 §6 风险条 ③ 追加结论（历史正文未改）；`bounded-wait-deadline.md` §5 原「属既有偶发家族」表述仍成立（现已定因并修复），不回填。

## 4. 边界

- 修前不改 `[THREAD] FAIL: reap-timeout` 判据与 5 s 预算；不为消红延长预算。
- 与 P-80（`[NET] Step1` 静默）分属两个观察项，不合并归因；签名重合 ≠ 同病因。
- 属内核线程/进程路径改动，触及 TCB 边界时须逐项对照 §4.2 I1–I6 与 §11.3。
