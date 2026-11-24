# 定时器终态架构落地

> 来源：[timer-subsystem-design.md](../design/timer-subsystem-design.md)。本册把终态设计拆成可执行批次；设计主张以该册为准，本册只记录施工目标、验收与状态。

## 一、问题与根因

取消协议已经收口，但承载结构仍是中间态：

- `HrTimer` 只有 `Inactive` / `Pending` / `Running`，`closed` 是独立旁路位，`init` 与 `shutdown` 的串行化依赖调用契约；
- `hrtimer_start*` 用 `0` 同时表示“框架未就绪”“未 init”“已 shutdown”，调用方无法区分；
- 全局 `HRTIMER_QUEUE: Mutex<Vec<TimerEntry>>` 让所有 CPU 串行，民用多核下不可作为终态；
- `timerfd` 仍是 `TFD_SLOTS: [TimerFdSlot; 16]` + 全局 `TFD_LOCK`，回调线性反查，且周期重挂在回调内；
- tickless 取全局最早到期，忽略 `cpu_id`；
- CPU 上下线没有 timer 侧 quiesce/迁移协议。

根因不是单点实现错误，而是“取消协议已终端化、载体仍按单队列/静态槽位/回调重挂组织”。本册按依赖顺序逐段替换载体，保持取消 API 不变。

## 二、目标与边界

目标：

- 保持 `hrtimer_cancel` 返回即可释放、`try_cancel` 三态、`shutdown` 不复活三条契约；
- 把 `HrTimer` 状态机、返回值和时钟中断接线收成终态形态；
- 把队列换成每 CPU `TimerBase`，tickless 按 CPU 取下一次到期；
- 把 timerfd 换成 per-context 对象，周期重挂移到 `read` 路径；
- 给 CPU 上下线提供 timer 侧 quiesce/强制迁移/online 握手；
- 锁序、静态审计、host 测试与民用多核验收同步。

不在本册：

- 平台级 CPU 上下线流程、ACPI/MP table、物理带电插拔；
- `NO_HZ_FULL`、软定时器、deferrable/slack、CPU 热插拔策略；
- 时钟基扩展（`CLOCK_REALTIME` 仍按设计册边界处理）；
- epoll 全局表、fd 表、分配器等整机扩展性改造，除 timerfd 唤醒 token 化这一必要交叉项。

## 三、条目

### 条目 1・HrTimer 五态化与 closed 收口

描述：把 `HrTimerState` 扩为 `Uninit` / `Inactive` / `Pending` / `Running` / `Shutdown`，删除独立 `closed: AtomicBool`；`init` 只允许在 `Uninit` / `Inactive` / `Shutdown` 上调用，`Pending` / `Running` 上调用在 debug 构建断言；`start` 拒绝 `Uninit` / `Shutdown`；`shutdown` 置 `Shutdown` 而不是覆盖回 `Inactive`。

方案：状态编码按 `Uninit=0, Inactive=1, Pending=2, Running=3, Shutdown=4`；`dispatch_expired` 的关闭分支保留 `Shutdown`，迟到回调不得把 `Shutdown` 收敛成 `Inactive`；Periodic 重挂失败时同样不得覆盖 `Shutdown`。唯一会在活动期调 `init` 的网络 poll timer 改为仅 `Uninit`/`Shutdown` 时初始化，其余路径靠 `start` 替换激活。

状态：[x] 已落地。`closed` 字段删除；状态编码与 `uninit`/`init`/`shutdown` 测试更新；新增“未 init 的 start 被拒绝”回归；hrtimer host 24 passed（含条目 2 新增例）、kernel host 614 passed、host 测试通过、`ci/audit.sh quick` 全绿、`ci/audit.sh full` 全绿（17/17 腿，QEMU 2/2）。

### 条目 2・StartOutcome 与 init 严格化

描述：把 `hrtimer_start*` 的 `0` 哨兵改为显式 `StartOutcome`（`Started(seq)` / `NotReady` / `NotInitialized` / `Shutdown`），让调用方区分“框架未就绪”“未 init”“已 shutdown”；把 `init` 的 debug 断言升级为队列锁内的运行时拒绝：`try_init` 返回 `InitError`，`init` 对活动定时器 panic。

方案：保留 `u64` 入队序号在 `Started` 内，并保证恒非 0；`hrtimer_start_rel` / `periodic` 同步返回 `StartOutcome`；不保留 `into_u64()` 适配层，所有内部调用点强制消费枚举。`timerfd` 的 `set_time` 改为 `Result<TimerTime, SetTimeError>`，初始化/入队失败回滚 armed 并映射 `EIO`；`itimer` 的设防失败回滚槽位并返回 `KernelError`，系统调用映射 `EIO`；`posix_timer` 失败撤销 armed/interval 并返回 `EIO`；`net_poll` 失败清截止期；`hrtimer_sleep` 失败直接返回，不再自旋到超时后假成功。

状态：[x] 已落地。`StartOutcome` 四态与 `InitError` 三态、`try_init` 队列锁内检查、`init` 运行时 panic 均已合入；全部 `start` 调用点已改判，无 `== 0` 哨兵残留；新增 NotReady / NotInitialized / Shutdown / init 活动拒绝回归；hrtimer 24 passed、kernel host 614 passed、host 测试通过、`ci/audit.sh quick` 全绿、`ci/audit.sh full` 全绿（17/17 腿、QEMU 2/2）。

### 条目 3・每 CPU TimerBase 与 tickless

描述：删除全局 `HRTIMER_QUEUE`，改为 `TIMER_BASES: [TimerBase; MAX_CPUS]`；每 base 一把锁、侵入式有序树、`Running` 计数；`hrtimer_start` 按当前 CPU/`base_id` 绑核；`hrtimer_run_queues(cpu)` 只处理本地 base；`hrtimer_next_expiry(cpu)` 供 tickless 按 CPU 装填。

方案：`HrTimer` 增加内嵌链节点与 `base_id`；`start` 在 base 锁内摘旧条目、发新 `seq`、置 `Pending`；`try_cancel` 跨 CPU 只锁目标 base；`cancel` 锁外等待 `in_flight`。`hrtimer_run_queues` 采用“锁内摘一个、锁外回调、预算耗尽立即重装”的无分配分发。`tickless` 的 `get_next_timer_expiry(cpu_id)` 真实传参。

状态：[x] 已落地。全局 `HRTIMER_QUEUE`/`Vec<TimerEntry>` 已删除；`HrTimer` 内嵌 `RbNode<(expiry, seq)>` + `base_id`，`TIMER_BASES: [TimerBase; MAX_CPUS]` 每 CPU 一把 `IrqSpinLock`；`hrtimer_start` 对 `Inactive` 绑当前 CPU，对 `Pending/Running` 用已有 `base_id`，不跨 CPU 搬迁；`run_queues(cpu)` 锁内摘一个、锁外回调、每轮最多 64 个；`hrtimer_next_expiry(cpu)` 只扫本 base，`tickless` 已传真实 CPU。`TIMER_INIT_LOCK` 串行化 `try_init` 与 `Inactive→Pending` 首次入队。tree 单测 3 passed（含插入/正序/逆序/中序删除红黑性质校验）、hrtimer 26 passed（含双 base 隔离与离线/越界 CPU 无后备拒绝）、kernel host 619 passed、host 测试通过、`ci/audit.sh quick` 全绿、`ci/audit.sh full` 全绿（17/17 腿、QEMU 2/2）。实现期发现 1024 个常驻 base 把 x86 镜像顶过 `USER_CODE_BASE`（K-08），将每 base `running` 从 `usize` 收为 `u32` 后恢复。

### 条目 4・timerfd per-context 对象与 read 路径重挂

描述：删除 `TFD_SLOTS` / `TFD_LOCK`，改为 `Arc<TimerFdContext>`；回调用 `container_of` O(1) 定位，只累加 ticks、置 expired、唤醒，不再在回调内重挂；周期重挂移到 `read` 路径；动态对象替代固定 16 槽。

方案：`TimerFdContext` 首字段放 `HrTimer`，其余用 `ticks/interval/armed/generation/last_expiry` 原子量；`set_time` 用 per-context 控制面占位，等待在途回调不持任何回调需要的锁；`read` 消费 ticks 后按 `last_expiry + interval` 重挂；`release` 先 `hrtimer_shutdown` 再释放 `Arc`，`Drop` 兜底。回调与 `poll` 不取控制面占位，避免 `epoll_pwake → timerfd poll` 回取。

状态：[x] 已落地。`TFD_SLOTS` / `TFD_LOCK` / `TFD_MAX_SLOTS` 已删除，动态 `Arc<TimerFdContext>` 替代 16 槽静态表；回调经 `container_of`（`timer` 为 `#[repr(C)]` 首字段）O(1) 定位，只累计 ticks、置 `armed=false`、`wake_fd` 唤醒并返回 `OneShot`，不在回调内重挂；周期由 `take_expiry`/`read` 按 `last_expiry + interval` 重挂，落后当前时刻则从当前时刻起算，未读对象只产生首个 tick。`release` 走 `hrtimer_shutdown` 等待在途回调，`TimerFdContext::drop` 兜底 shutdown；回调与 `poll` 均不取 `control_busy`。`CLOCK_REALTIME` 改为 `ENOTSUP`，不再静默按单调基计时；未知 clockid 仍 `EINVAL`。timerfd 数量只受 fd 表/堆内存限制。验证：timerfd privileged 单测 4 passed、functions 单测 6 passed、kernel host 1445 passed（kernel 622 / functions 811 / privileged 12）、host 1652 passed、fd 形状锁 43 passed、`ci/audit.sh quick` 全绿、`ci/audit.sh full` 全绿（17/17 + 存量 11/11 审计腿、Lockbud 4/4、QEMU 2/2）；锁序表删除 `TFD_LOCK`。

### 条目 5・CPU 上下线 quiesce/迁移

描述：给 `TimerBase` 增加 `Offline` / `Online` / `Quiescing` 生命周期；平台 offline 前调用 `hrtimer_cpu_quiesce(cpu, target)`，源 base 拒绝新回调、等待 `running` 与 `migrations` 归零、把 Pending timer 强制迁到目标 base；CPU online 后 `hrtimer_cpu_online(cpu)` 复位，不做反向迁移。

方案：迁移用 `migrating` 瞬态；源 base 每摘下一个待迁条目就 `migrations += 1`，目标插入后才回落，`quiesce` 必须同时看到 `running==0 && migrations==0` 才置 `Offline`。`try_cancel` 在迁移中返回 `Busy`，`cancel` 等待 `migrating` 清零；`Inactive` timer 在 quiesce 期间直接绑 `target`，`Pending`/`Running` 由 `start_bound` 自迁移到 `target`，只有 timer 已处 `migrating` 时才返回 `NotReady`。迁移只锁一个 base，禁止源/目标双持。最后一个在线 CPU 不允许下线；`target` 在 quiesce 期间保持在线。`BaseInner` 的 `running/migrations/quiesce_target` 收为 `u16`，维持 24 字节预算（K-08）。`hrtimer_init` 按实际在线 CPU 复位 base，AP 在 `register_cpu` 后调用 `hrtimer_cpu_online`。

状态：[x] 已落地。`BaseState::Quiescing`、源 base `quiesce_target`、`HrTimer::migrating`、`ONLINE_BASE_COUNT` 与 `CpuTimerError` 已实现；Pending 迁移保留 `expiry_ns`/`seq`，不双持源/目标 base 锁；quiesce 等待 `running` + `migrations` 双归零；`hrtimer_try_cancel` 迁移期返回 `Busy` 并在 `base_id` 变化时要求重试；`hrtimer_shutdown` 等迁移瞬态后再摘链；`run_queues`/`next_expiry` 跳过 `migrating` 节点。x86_64/aarch64 AP 在 `register_cpu` 后接线 `hrtimer_cpu_online`，boot `hrtimer_init` 只把实际在线 CPU 置 `Online`。新增 quiesce/迁移/last-CPU/target 离线/并发 start/cancel/shutdown/周期重挂 10 例及 `BaseInner<=24` 预算断言。验证：hrtimer 36 passed、kernel host 1455 passed（kernel 632 / functions 811 / privileged 12）、host 1652 passed、`ci/audit.sh quick` 全绿、QEMU x86_64 + aarch64 2/2 通过（K-08 镜像未回退）、`ci/audit.sh full` 全绿（14/14 自测、17/17 审计腿 + 存量腿、QEMU 2/2）。

### 条目 6・唤醒 token 化、静态槽清理与审计

描述：timerfd 不再保存裸 `wake_fd`，改为对象级稳定 token；epoll 兴趣按 `(handle_id, token)` 登记，回调按 token 唤醒所有兴趣（覆盖 dup 别名与多个 epoll）；删除静态槽残余，锁序表登记 `TFD_CTX.tfd_ctx` / `TIMER_BASE`，静态锁序、形状锁、死锁矩阵和 full 审计同步。

方案：`File::wake_token()` 默认 `0`，`TimerFdFile` 返回 `TimerFdContext` 的地址令牌（非 0，只作身份、不解引用）。`VfsOps` 增加 `fd_watch_token(fd)` 与 `open_file_poll(handle_id, events)`；epoll `EpollItem` 持 `handle_id`/`wake_token`，`epoll_ctl(ADD)` 登记，`epoll_wait` 对 token 兴趣走对象 `File::poll`。回调只调 `epoll_pwake_token(token)`，扫描所有实例按 token 入 `EPOLLIN`，不在中断上下文回查 VFS。最后一个 fd 关闭时 `TimerFdContext::release` 在 `hrtimer_shutdown` 后调 `epoll_forget_token`，移除所有兴趣与就绪项，避免地址复用误唤醒。`ReadyEvent` 带 token，`EPOLL_CTL_DEL` 按 `(data, token)` 精确清理。`control_busy` 换成 `TFD_CTX.tfd_ctx` 控制锁，回调与 `poll` 不取。锁序表以 `TFD_CTX.tfd_ctx`(11) / `TIMER_BASE`(14) 替换 `TFD_LOCK` / `HRTIMER_QUEUE`；新增“回调不取控制锁、token 多 epoll 唤醒、release 清兴趣、timerfd 未读不风暴”的形状/行为测试。

状态：[x] 已落地。`TimerFdContext` 删除 `wake_fd`/`set_wake_fd`/`control_busy`，新增 `wake_token()` 与 `tfd_ctx`；`File`/`VfsOps`/epoll 三条链路接通；新增 `epoll_pwake_token`/`epoll_forget_token`/`check_item_ready`；形状锁断言 timerfd 无旧裸 fd API、epoll 存在 token/handle API。已知余项：`epoll_forget_token` 清理后不主动唤醒阻塞在 `epoll_wait(-1)` 的等待者；timerfd 阻塞 `read` 仍无 `waiters` 队列，属后续条目。验证：timerfd privileged 5 passed、functions 6 passed、kernel host 1456 passed（kernel 633 / functions 811 / privileged 12）、host 1653 passed、`ci/audit.sh quick` 全绿、`ci/audit.sh full` 全绿（未执行腿 0、QEMU 2/2）、QEMU x86_64 + aarch64 2/2、静态死锁矩阵 0 项。

### 条目 7・timerfd 阻塞 read 与关闭唤醒

描述：timerfd 的 `read` 在无到期计数时，阻塞 fd 必须睡到下一次到期或关闭，而不是直接返回 `EAGAIN`；同一个 `TimerFdContext` 的等待队列由回调、`release`/`Drop` 和 epoll 关闭路径共同唤醒。`epoll_forget_token` 移除最后一个兴趣后，也要唤醒阻塞在 `epoll_wait(-1)` 的等待者，避免关闭 timerfd 后等待者永久睡眠。

方案：`TimerFdContext` 增加 `waiters: IrqSpinLock<WaitQueue>` 与 `closed: AtomicBool`，锁类登记 `TFD_WAIT.waiters`(11)。`TimerFdContext::wait_for_ticks()` 在 `TFD_WAIT` 内检查 `closed`/`ticks`、入队，再按 futex 的锁域顺序在同一临界区内 `process_block_prepare` 置 Blocked，释放锁后 `process_block_finish` 切换；被回调或关闭唤醒后重查 `closed`/`ticks`，无 ticks 则由 syscall 重试并重新入队。回调在累加 ticks 后 drain 等待队列并 `scheduler_unblock`，不在 `TFD_CTX` 内唤醒；`release` 先 shutdown/清状态、置 `closed`，再 drain 并唤醒，最后 `epoll_forget_token`。`read_syscall` 捕获 timerfd 的 `EAGAIN` 后调 `functions::fs::timerfd::wait_readable(fd)`：按 `OpenFile` 的 nonblock 快照短路，取 context `Arc` 后释放 `OpenFileTable` 锁再睡，禁止持表锁阻塞。`epoll_forget_token` 在清理兴趣/就绪项后对受影响实例 `wake_one` + `scheduler_unblock`。

状态：[x] 已落地。`TimerFdContext` 新增 `waiters: IrqSpinLock<WaitQueue>`、`closed: AtomicBool` 与 `TFD_WAIT.waiters`（11）；`wait_for_ticks` 在 `TFD_WAIT` 内检查 `closed`/`ticks`、入队并在同一临界区 `process_block_prepare` 置 Blocked，锁外 `process_block_finish` 切换，伪唤醒无 tick 时重新入队；`read_syscall` 在 timerfd `EAGAIN` 时经 `TimerFdFile::wait_readable(fd)` 按 `OpenFile::nonblock` 短路，取 context `Arc` 后释放 `OpenFileTable` 锁再睡。`timerfd_callback` 累加 ticks 后 drain 等待者并 `scheduler_unblock`；`release`/`Drop` 置 `closed`、drain 后唤醒，再 `epoll_forget_token`；`epoll_forget_token` 移除兴趣后唤醒该实例的 epoll 等待者。静态锁序新增 `TFD_WAIT.waiters`(11)；耦合基线 `privileged proc↔syscall` 2→3 已显式更新（阻塞原语依赖 proc，方向不新增环）。验证：timerfd 13 passed（functions 6 / privileged 7）、kernel host 1458 passed（kernel 635 / functions 811 / privileged 12）、host 1653 passed、`ci/audit.sh quick` 全绿、`ci/audit.sh full` 全绿（未执行腿 0、QEMU 2/2）、静态死锁矩阵 0 项、耦合审计通过。
