# 定时器子系统终态架构

> Edgine 定时器的终态不是 Linux hrtimer 的功能复制，而是：**保留两档取消协议，把载体换成 per-CPU、无分配、CPU 亲和、硬中断回调的定时器；timerfd 改为 per-context 对象、read 路径重挂、按对象注册唤醒。** 取消协议已经接近终态，本文主要设计其承载结构与 timerfd 语义。
>
> **项目级约束**：Edgine 是实验性内核，但实验性不降低硬件可用性要求。终态必须在民用 x86_64 / aarch64 多核 CPU 上可长期运行；因此 per-CPU 定时器、SMP 初始化、tickless 空闲管理是**硬约束**，不是规模变大后的优化。

## 一、终态的定义与边界

**终态**指：在 Edgine 的目标规模与上下文模型下，不需要因为当前实现约束再改 API 语义、生命周期契约或锁域模型。它不意味着永远不再增加功能，也不意味着对齐 Linux 的全部定时器特性。

### 项目级约束

- **民用硬件可用性**：目标不是“单核实验板”，而是普通消费级 x86_64 / aarch64 多核 CPU；包括 SMP、SMT、异构大小核和不同逻辑核数量的机器。
- **不依赖特殊定时器硬件**：只使用标配时钟事件源（x86_64 LAPIC timer、aarch64 `CNTP_CVAL` 等）；不引入专用定时器芯片、隔离核或实时扩展。
- **空闲可用**：消费级 CPU 在空闲时不能靠全局周期 tick 维持；`NO_HZ_IDLE` 与每 CPU 下一次到期装填是可用性的一部分，不只是省电优化。
- **不要求 Linux 全功能**：不因“民用硬件”就承诺 `NO_HZ_FULL`、平台级 CPU 上下线流程、定时器迁移或全部 POSIX 时钟语义；这些是独立能力，不是硬件可用的前置条件。
- CPU 上下线（online/offline）的**定时器侧协议**纳入本文（见第五节）；平台级 ACPI/MP table 启动、物理带电插拔、CPU 上下线策略不在本文。正常运行时定时器不做跨 CPU 迁移/负载均衡；CPU offline 的强制迁移是唯一例外。

本文覆盖：

- `hrtimer` 核心：定时器状态、队列、取消、重挂、时钟中断接线。
- `timerfd`：对象生命周期、周期语义、read/poll 唤醒。
- 与 `tickless`、`epoll`、fd 对象模型的接口边界。
- 锁序、内存序、容量与错误模型。

本文不覆盖，且不纳入 Edgine 定时器终态：

- 软中断定时器、deferrable timer、timer slack、信号驱动的 posix timer 全量语义。
- 平台级 CPU 上下线流程（ACPI/MP table 启动、offline 策略、RAS）、物理带电插拔、每 CPU 动态扩容。定时器侧只提供 `quiesce`/`migrate`/`online` 握手；平台级流程与物理插拔不在本设计范围。民用硬件可用不要求这些能力；若后续产品目标包含笔记本睡眠，需另立时钟恢复协议。
- `CLOCK_REALTIME` 时钟基。它不属于 CPU 硬件可用性门槛；但当前 `timerfd_create` 接受该 clockid 却按单调时基计时，这是必须消除的静默错语义。时钟基抽象就位前，显式返回 `ENOTSUP` 是终态的一部分；若要对齐 Linux 用户态的 `CLOCK_REALTIME` timerfd，另立时钟基工程。
- 以定时器回调承担长任务。回调仍要求短小、不可睡眠。

### 容量包络与民用/商用边界

“民用/商用”不是可靠的设计判据，核心数只是代理指标，而且两端重叠：96 核 Threadripper 属于消费/准专业，64 核 EPYC 属于商用；64 核至强与 32 核桌面也可能落在同一数量级。定时器终态应按**部署包络**约束，而不是按市场标签：

| 维度 | 终态 v1 包络 | 超出包络后的代价 |
|---|---|---|
| 逻辑 CPU | `MAX_CPUS = 1024` 以内；启动时校验，超限明确拒绝 | 提高编译期上限或改动态 per-CPU base；队列 API 不用改 |
| socket / NUMA | 不假设单 socket；per-CPU base 与 socket 解耦 | NUMA 主要影响对象放置和跨节点取消延迟，不改变队列正确性 |
| CPU 上下线（online/offline） | timer 侧支持 quiesce/强制迁移/online；平台级 offline 流程另计 | 平台必须先调用 timer quiesce，再下线 CPU；物理带电插拔不在范围 |
| suspend/resume | 不支持 | 必须增加每 CPU base 冻结/恢复与错过到期补偿 |
| 每 CPU 活跃 timer | 不设硬上限，按内存增长 | 红黑树保持 O(log n)；内存与回调预算是真正的约束 |
| timerfd 数量 | 按 fd 表和堆内存，不再有 16 槽硬上限 | 受 fd limit/内存限制，不回到固定槽位 |
| 中断率 | 由回调预算限制 | 预算过小增加延迟，过大占用硬中断；需按机器标定 |

结论是：**per-CPU + 每 CPU 有序树本身是规模中性的，4 核到数百核都成立；真正的硬边界是 `MAX_CPUS`、平台级 CPU 上下线和 suspend，而不是“民用/商用”标签。** 若未来要覆盖超过 `MAX_CPUS` 的逻辑 CPU，不能只调大队列；必须把 `TimerBase` 的生命周期从“启动时静态初始化”扩展为可挂接/可排空的 per-CPU 资源。

超多核上还要防止把定时器设计当成整机可用的充分条件：即使每 CPU 队列无全局锁，epoll 的全局实例表、fd 表、分配器和调度器仍可能成为瓶颈。定时器终态只保证自己这一层的锁域与生命周期，不替其他子系统背书。

## 二、当前形态与差距

当前实现已经完成了取消协议的语义收口，但承载结构仍是单队列 + 静态槽位的中间态：

| 部件 | 当前形态 | 终态差距 |
|---|---|---|
| 取消协议 | `try_cancel` 三态、阻塞 `cancel` 锁外等待、`shutdown` 关闭位；`hrtimer_start*` 返回入队序号 | 协议可保留；`closed` 需并入状态机，`start` 的 `0` 哨兵需改为显式结果 |
| 队列 | 单一全局 `HRTIMER_QUEUE: Mutex<Vec<TimerEntry>>`，按到期时间排序 | 改为每 CPU 一个 `TimerBase`，队列操作不再全局串行 |
| 队列操作 | `Vec` 的 `retain` / 插入 / 排序，入队和分发都有 O(n) 扫描 | 改为侵入式有序结构，入队/出队 O(log n)，回调路径不分配 |
| 定时器状态 | `Inactive` / `Pending` / `Running` 三态 + 独立 `closed: AtomicBool` | 增加 `Uninit` / `Shutdown`，关闭位成为状态而不是旁路原子量 |
| 生命周期 | `in_flight` + 调用方保活契约；静态槽位保证地址稳定 | 取消后即可释放；对象所有权 + 不可移动契约承担生命周期 |
| timerfd 槽位 | `TFD_SLOTS: [TimerFdSlot; 16]`，全局 `TFD_LOCK`，回调线性反查 | 改为 per-context `Arc` 对象，回调 `container_of` O(1) 定位 |
| timerfd 周期 | 回调内直接 `hrtimer_start` 自重挂 | 回调只累加 ticks 并唤醒；周期重挂移到 `read` 路径 |
| timerfd 唤醒 | 保存单个数值 `wake_fd`，`epoll_pwake(fd)` | 改为按 epoll 兴趣/对象注册唤醒，避免 fd 复用误唤醒；单一 fd 无法覆盖多 epoll 兴趣 |
| tickless | `get_next_timer_expiry(_cpu_id)` 忽略 CPU，取全局最早到期 | 改为每 CPU 查询自己的 `TimerBase`，各自装填下一次中断 |
| 多核可用性 | 所有 CPU 共用一把 `HRTIMER_QUEUE` 锁和一个最早到期读数 | 民用多核下必须 per-CPU；否则核间竞争、错核唤醒和空闲功耗都不可接受 |
| 锁域 | `TFD_LOCK`（控制面）+ `HRTIMER_QUEUE`（全局队列） | `TFD_CTX` 控制面 + 每 CPU `TIMER_BASE`，不再有全局定时器锁 |

当前代码事实来源：

- `HrTimer` 结构、三态、`in_flight`、`closed`、全局队列与取消协议见 [hrtimer.rs](../../src/kernel/privileged/timer/hrtimer.rs)。
- `TFD_SLOTS` / `TFD_LOCK` / 回调重挂见 [timerfd.rs](../../src/kernel/privileged/syscall/timerfd.rs)。
- timerfd 对象层与 read/poll 语义见 [functions/fs/timerfd.rs](../../src/kernel/functions/fs/timerfd.rs)。
- tickless 取下一次到期见 [tickless.rs](../../src/kernel/functions/timer/tickless.rs)。
- 定时器中断统一入口见 [tick.rs](../../src/kernel/privileged/timer/tick.rs)。

## 三、终态不变量

无论底层结构如何替换，以下契约不再改变：

1. **取消返回即可释放**：`hrtimer_cancel` 返回后，回调不再访问该 timer；`try_cancel` 返回 `Cancelled` 时，队列不再持有该 timer 指针。`Busy` 只表示“尚未安全”，不是“取消失败”。
2. **取消等待不持锁**：等待在途回调期间，不持队列锁、不持调用方控制锁，也不持回调可能需要的锁。
3. **回调硬中断化**：hrtimer 回调只在硬中断上下文执行，不睡眠、不分配、不做长任务；软定时器不在本终态内。
4. **CPU 亲和**：timer 在首次 `start` 时绑定到当前 CPU；运行期间不迁移。取消可以从任意 CPU 发起。
5. **无分配队列**：入队、出队、分发、周期重挂都不走堆分配；`HrTimer` 的链节点内嵌在对象里。
6. **O(1) 归属定位**：timerfd 回调从 `HrTimer` 直接定位宿主 context，不做槽位表线性扫描。
7. **状态机唯一**：`Uninit` / `Inactive` / `Pending` / `Running` / `Shutdown` 是唯一状态来源；不再有独立 `closed` 旁路位。
8. **timerfd 未读不风暴**：周期 timerfd 在无人 `read` 时不得持续重挂回调；重挂由 `read` 驱动。
9. **锁序可审计**：每个锁类有唯一层级；回调不直接持控制面锁；静态锁序与运行时 lockdep 都能检查。

## 四、HrTimer 状态机与 API

终态状态集合：

| 状态 | 含义 |
|---|---|
| `Uninit` | 刚构造，未设置回调 |
| `Inactive` | 已初始化，未入队，无在途回调 |
| `Pending` | 已入队，等待到期；可能同时存在一个旧回调在途（由 `in_flight` 标记） |
| `Running` | 当前激活的回调正在执行 |
| `Shutdown` | 关闭位生效，拒绝再次入队；等待在途回调后可重新 `init` |

状态迁移：

| From | 事件 | To | 约束 |
|---|---|---|---|
| `Uninit` / `Shutdown` | `init(callback)` | `Inactive` | 必须无 `in_flight` |
| `Inactive` | `init(callback)` | `Inactive` | 用于构造期换回调；不得在 `Pending/Running` 上调用 |
| `Inactive` | `start` | `Pending` | 绑定当前 CPU 的 `TimerBase` |
| `Pending` / `Running` | `start` | `Pending` | 替换激活：摘旧条目、发新序号；旧回调若在途，靠序号作废 |
| `Pending` | `try_cancel` / `cancel` | `Inactive` | `in_flight` 为真时 `try_cancel` 返回 `Busy`，`cancel` 等待后重试 |
| `Pending` | `run_queues` 收集 | `Running` | 队列锁内置 `Running` + `in_flight=true` |
| `Running` | 回调返回 `OneShot` | `Inactive` | 清 `in_flight` |
| `Running` | 回调返回 `Periodic` | `Pending` | 框架在清 `in_flight` 前重挂；若已 `Shutdown` 则不重挂 |
| 任意 | `shutdown` | `Shutdown` | 摘条目、置关闭状态、等待 `in_flight` 清零 |

`Inactive` 上允许 `init` 是构造期便利，不表示可以在 `Pending/Running` 上换回调。`Pending` 和 `Running` 上的 `init` 必须被 debug 断言或运行时错误挡住；否则回调指针与在途回调会产生别名语义。

终态 API 轮廓：

```rust
pub enum StartOutcome {
    Started(u64),
    NotReady,
    Shutdown,
}

pub enum CancelOutcome {
    Cancelled,
    NotPending,
    Busy,
}

pub fn hrtimer_start(timer: &HrTimer, expiry_ns: u64) -> StartOutcome;
pub fn hrtimer_start_periodic(timer: &HrTimer, delay_ns: u64, interval_ns: u64) -> StartOutcome;
pub fn hrtimer_try_cancel(timer: &HrTimer) -> CancelOutcome;
pub fn hrtimer_cancel(timer: &HrTimer) -> bool;
pub fn hrtimer_shutdown(timer: &HrTimer);
pub fn hrtimer_next_expiry(cpu: u32) -> Option<u64>;
pub fn hrtimer_run_queues(cpu: u32);
```

`0` 不再同时表示“框架未就绪”和“已 shutdown”。迁移期可以保留 `0` 适配层，但终态调用方必须消费 `StartOutcome`，否则关闭位与未初始化会被继续混为一谈。

`hrtimer_start` 在 `Pending/Running` 上的替换语义保留，因为网络 poll timer、POSIX timer 等需要在运行中改截止期；但“替换”只保证旧激活不再产生有效回调，不保证旧回调已经退出。需要“替换且确保旧回调退出”的调用方，先 `cancel` 再 `start`。

## 五、每 CPU TimerBase

终态队列形状：

```text
enum BaseState { Offline, Online, Quiescing }

TIMER_BASES: [TimerBase; MAX_CPUS]
  └── TimerBase
        ├── lock: IrqSpinLock<BaseInner>
        └── BaseInner
              ├── state: BaseState
              ├── quiesce_target: u32
              ├── root: *mut HrTimer          (侵入式有序树根)
              ├── active: usize
              ├── running: usize
              └── next_seq: u64
```

`HrTimer` 内嵌树节点：

```rust
struct HrTimer {
    expiry_ns: AtomicU64,
    interval_ns: AtomicU64,
    callback: AtomicUsize,
    state: AtomicU8,
    base_id: AtomicU32,
    in_flight: AtomicBool,
    migrating: AtomicBool,          // CPU offline 强制迁移的瞬态
    links: UnsafeCell<TimerLinks>,  // 只在对应 TimerBase 锁内访问
}
```

具体有序结构选**侵入式红黑树**：无堆分配、最坏 O(log n)、支持 IRQ 内插入删除。排序键为 `(expiry_ns, seq)`，`seq` 用于同到期时间的稳定顺序。若后续因实现成本降级为有序侵入链表，那只是过渡实现，不改变本文的锁模型、状态机和 API 结论，也不把它称作终态。

当前 `MAX_CPUS` 是 1024（见 [capacity.rs](../../src/kernel/privileged/config/capacity.rs)），`TimerBase` 静态数组按该上限留容量；单 base 只含锁、树根和计数，不随 timer 数量增长。消费级 CPU 的逻辑核数（含 SMT 线程）远低于该上限。启动时只初始化实际在线的 CPU base，未上线 CPU 的 base 保持空；没有“全局后备队列”作为降级路径，否则多核可用性会重新退化为单锁。

### 启动与绑核

`hrtimer_start` 的绑核规则：

1. 读 `state`。`Shutdown` 直接返回；`Inactive` 选择当前 CPU 作为目标 base；若当前 base 处于 `Quiescing`/`Offline`，改用 `quiesce_target` 的在线 base；`Pending/Running` 读 `base_id`，锁定原 base。
2. 锁定目标 `TimerBase` 后复查 `base_id`、`migrating` 与状态；若期间已被别的 CPU 改变，或 `base_id` 处于迁移中，释放锁并返回 `NotReady`，由调用方重试。
3. 在 base 锁内摘除同一 timer 的旧树节点（若有），分配新 `seq`，写 `expiry_ns`，置 `Pending`，插入树。
4. 若旧激活的回调仍在途，保留 `in_flight=true`；新条目在 `in_flight` 清零前不会被 `run_queues` 收集。

这样 `start` 不需要等待旧回调，也不会让同一 timer 的两个回调并发；旧回调通过 `seq` 作废。

`hrtimer_start` 不主动把已绑定 timer 搬到别的 CPU。唯一例外是 CPU offline 的强制迁移：当 base 进入 `Quiescing`，新 `start` 只能落到 `quiesce_target`；已有 Pending timer 由 quiesce 流程搬走。迁移中的 timer 对 `start` 表现为 `NotReady`，不出现“半在源 base、半在目标 base”的可见状态。

### 取消与关闭

`hrtimer_try_cancel`：

1. 若 `state` 为 `Shutdown` 或 `Inactive` 且 `in_flight=false`、`migrating=false`，返回 `NotPending`。
2. 若 `in_flight=true` 或 `migrating=true`，返回 `Busy`，不改队列。
3. 否则锁定 `base_id` 对应 base，摘除树节点，置 `Inactive`，清 `seq`，返回 `Cancelled`。

`hrtimer_cancel` 在 `Busy` 时释放所有 base 锁后自旋等待 `in_flight` 清零且 `migrating` 清零，再回环重试。等待方不得持有回调可能获取的锁；这条和当前协议一致。

`hrtimer_shutdown`：

1. 若 `migrating=true`，先等待迁移完成；否则可能锁到错误的 base。
2. 锁定当前 `base_id`，置 `Shutdown`，摘除树节点，释放锁。
3. 等待 `in_flight` 清零。
4. 返回后 timer 可释放；`init` 是唯一允许离开 `Shutdown` 的入口。

`shutdown` 与 `start` 的关闭位判定必须在同一个 base 锁内完成：要么 `start` 先入队、`shutdown` 随后摘除；要么 `shutdown` 先置状态、`start` 看到 `Shutdown` 后拒绝。

### 分发

`hrtimer_run_queues(cpu)` 只处理当前 CPU 的 base：

1. 若 base 不是 `Online`，不收集新回调；`Quiescing` 的剩余条目由 CPU offline 流程迁移，`Offline` 的 base 应为空。
2. 锁定 base，找到最早到期且 `state==Pending && !in_flight && !migrating` 的节点。
3. 摘除节点，置 `Running`、`in_flight=true`、`running += 1`，释放锁。
4. 在锁外执行回调；回调返回 `Periodic` 时，框架在清 `in_flight` 前重新插入同一 base（若 base 已 `Quiescing`，改插 `quiesce_target`）。
5. 清 `in_flight`、`running -= 1`，继续下一项；单次中断设置回调预算，预算耗尽则把本 CPU 的下一次中断装填为“立即”，把剩余工作留给下一次中断。

预算的意义是避免短周期 timer 在硬中断里无限占用 CPU；它是终态的一部分，不是可选优化。

### tickless 接线

`tickless` 的 `get_next_timer_expiry(cpu_id)` 必须把 `cpu_id` 传给 `hrtimer_next_expiry(cpu)`，只查询该 CPU 的 base。当前实现忽略 CPU id、取全局最早到期，在 per-CPU 终态下会变成“CPU0 的定时器唤醒 CPU1”的错配。

`on_timer_interrupt` 读取当前 CPU id 后调用 `hrtimer_run_queues(cpu)`；不再存在全局 `run_queues()`。

消费级 CPU 的空闲功耗依赖这条 per-CPU 装填：如果仍用全局最早到期，一个 CPU 的 timer 会唤醒另一个空闲 CPU，笔电/桌面空载会出现无谓唤醒和错核延迟。终态不要求 `NO_HZ_FULL`，但 `NO_HZ_IDLE` 与每 CPU 下一次到期必须成立。

### CPU 上下线协议（timer 侧）

CPU 上下线是 `TimerBase` 生命周期事件，不是普通负载均衡。定时器终态向平台提供两个入口：

```rust
pub enum CpuTimerError {
    NoOnlineTarget,
    LastOnlineCpu,
    Busy,
}

pub fn hrtimer_cpu_online(cpu: u32);
pub fn hrtimer_cpu_quiesce(cpu: u32, target: u32) -> Result<(), CpuTimerError>;
```

`hrtimer_cpu_quiesce` 的语义：

1. 校验 `target` 是另一个在线 CPU；最后一个在线 CPU 不允许下线，返回 `LastOnlineCpu`。
2. 在源 base 锁内把状态置 `Quiescing`，记录 `quiesce_target`；此后源 base 不再收集新回调，当前 CPU 上新的 `start` 改绑 `target`。
3. 等待源 base 的 `running` 归零；在途回调返回 `Periodic` 时，框架把它重挂到 `target`，不再回源 base。
4. 逐个迁移源树中的 Pending timer：在源 base 锁内摘除、置 `migrating=true`，释放源锁；锁 `target` base 插入、更新 `base_id`、清 `migrating`。迁移保留原 `expiry_ns` 与 `seq`，不产生新的激活语义。
5. 源树清空且 `running==0` 后，状态置 `Offline`，`hrtimer_next_expiry(cpu)` 返回 `None`；平台随后可以执行 CPU offline。

迁移期间 `try_cancel` 返回 `Busy`，`cancel` 等待 `migrating` 清零后重试；`start` 返回 `NotReady`，由调用方重试。这样不会出现“源 base 已摘除、目标 base 未插入”时被取消或重挂的窗口。

`hrtimer_cpu_online` 在 CPU 启动、开始调度前调用：清空树、复位 `running`/`active`、状态置 `Online`。**不做反向迁移**：CPU 重新上线后，原先迁走的 timer 不会自动搬回；新 `start` 才按当前 CPU 绑定。

平台必须保证：

- 同一时刻只有一个 CPU 执行 offline quiesce，且 `target` 在 quiesce 期间保持在线；`quiesce` 只在状态变更前做校验并可能返回 `Busy`，进入迁移后不得失败。若平台中途把 `target` 下线，属于协议违约，timer 不做半迁移回滚。
- CPU offline 流程先完成 task/IRQ 迁移，再调用 timer quiesce，最后才真正停核；timer quiesce 不替代平台的任务迁移。若 offline 中止，平台调用 `hrtimer_cpu_online(cpu)` 复位 base；已迁走的 timer 不自动迁回。
- `hrtimer_cpu_quiesce` 只能由平台 offline 线程在进程上下文调用，不得在中断、回调或持锁上下文中调用；它会等待源 base 的 `running` 归零，期间不得阻塞目标 base 的回调。
- 物理带电插拔 CPU、ACPI/MP table 的 CPU 枚举与 RAS 流程不在本文范围；本文只定义定时器侧的 base 状态与迁移语义。

timerfd 不受 CPU 上下线影响：`TimerFdContext` 仍由 fd 引用持有，迁移只改变其 `HrTimer` 的 base；回调在目标 CPU 执行，读取同一 context。若 quiesce 期间 `set_time` 到达，`start` 要么落到 `target`，要么返回 `NotReady` 让 syscall 重试，不会把 timer 留在即将下线的 CPU 上。

## 六、timerfd 终态

### 对象形状

timerfd 不再有全局槽位表和槽位下标。终态对象：

```rust
struct TimerFdContext {
    timer: HrTimer,                 // 必须是首字段，便于 container_of
    ticks: AtomicU64,
    interval_ns: AtomicU64,
    armed: AtomicBool,
    generation: AtomicU64,
    waker: WakeRegistration,        // 不是数值 fd
    waiters: WaitQueue,
}

struct TimerFdFile {
    ctx: Arc<TimerFdContext>,
    nonblock: bool,
}
```

`TimerFdFile` 挂在 `OpenFileTable`，fd 由 per-process `FdTable` 分配；对象生命周期由 `Arc` 与 fd 引用计数承担。`release` 只在最后一个 fd 引用关闭时调用，先 `cancel`/`shutdown`，确认回调退出后再释放 `Arc`。

回调通过 `container_of` 从 `HrTimer` 指针定位 `TimerFdContext`，不再线性扫描静态数组。`HrTimer` 在 `TimerFdContext` 中地址稳定；`TimerFdContext` 一旦发布到 fd 表，不允许移动。

### 周期语义

终态采用 Linux 形态的“回调只记 tick，read 重挂”：

- 回调：校验 `generation`，`ticks.fetch_add(1)`，单次定时器置 `armed=false`；周期定时器**不重挂**；然后按 `waker` 唤醒 epoll/等待队列，返回 `OneShot`。
- `read`：交换 `ticks` 为 0；无 tick 返回 `EAGAIN`/`WouldBlock`；有 tick 且 `interval_ns > 0` 时，以 `last_expiry + interval` 或 `now + interval` 为下一次到期，调用 `hrtimer_start` 重挂，写入新 `generation`。
- 这样无人 `read` 的周期 timerfd 在首次到期后自然停住，不会在硬中断里持续点火。当前回调内自重挂的形态必须删除。

`set_time` 只启动第一次到期，不启动“回调自动续期”。周期续期由 `read` 驱动。

### read / poll / gettime

- `read_at` 保持非阻塞快路径：缓冲区小于 8 字节返回 `EINVAL`；无 tick 返回 `WouldBlock`。阻塞 `read` 由 fd/syscall 层的等待队列承担：在 `!nonblock` 时检查 `ticks`，为 0 则挂入 `waiters`，回调唤醒后重试。
- `poll` 只读 `ticks > 0`，不取 `TFD_CTX` 控制锁；否则回调持控制锁唤醒 epoll 时，epoll 的 `check_fd_ready → timerfd::poll` 会回取同一把锁，重新制造自死锁。
- `gettime` 返回 `(interval, remaining)`；`armed=false` 时 remaining 为 0，对应“已到期但尚未 read 重挂”的 Linux 语义。
- `set_time` 先在控制面占位，取状态快照，**释放状态锁后再 `hrtimer_cancel`**；等待返回后重新取状态锁，清 ticks、写 interval、启动一次到期。控制面占位保证等待期间对象不被释放。
- `release` 先 `shutdown` 或 `cancel`，再清 `waker`、ticks、armed；回调退出后才允许 `Arc` 归零。

### 唤醒注册

当前 `wake_fd` 是单个数值 fd，`epoll_pwake(fd)` 按 fd 唤醒。这在 dup、close、fd 复用下不成立：timerfd 关闭后，同一个数值 fd 可能已属于另一个对象；timerfd 也可能同时被多个 epoll 实例关注。

终态要求：

- `TimerFdContext` 不保存裸 fd，而保存 epoll 兴趣注册返回的 `WakeRegistration`（token 或弱引用）。
- 一个 timerfd 支持多个 epoll 兴趣；回调按注册集合唤醒，而不是按一个 fd 号。
- epoll 的 timerfd 就绪查询走对象 `poll`，不走数值 fd 分支。

这是 timerfd 终态的前置条件。若 epoll 仍以数值 fd 为键，timerfd 可以完成对象化，但不能宣称完成“fd 复用安全”。

### clockid 与 flags

终态 v1 只支持 `CLOCK_MONOTONIC`。`CLOCK_REALTIME` 在时钟基抽象落地前返回 `ENOTSUP`，不再接受后静默按单调时基计时。未知 flags、未知 clockid 继续 `EINVAL`。`TFD_TIMER_ABSTIME` 保留，按对应时钟基解释；单调基下即绝对单调纳秒。

## 七、锁序与内存序

终态锁类：

| 锁类 | 层级 | 作用 | 约束 |
|---|---:|---|---|
| `TFD_CTX` | 11 | timerfd 控制面：set_time/release 的复合状态 | 等待在途回调前先释放；回调不取 |
| `TIMER_BASE` | 14 | 每 CPU 定时器队列 | 只允许持有一个 base 锁；锁内不执行回调、不等待 |
| `EPOLL` | 既有层级 | epoll 实例与等待队列 | 回调唤醒时取；不得在持 `TIMER_BASE` 时取 |

约束：

- 不存在 `TIMER_BASE(cpu0) → TIMER_BASE(cpu1)` 的嵌套。任何跨 CPU 操作都只锁目标 base，释放后再重试或等待。CPU offline 迁移也是“锁源 base 摘除 → 放锁 → 锁目标 base 插入”，不持两把 base 锁。
- `TFD_CTX` 内可以短临界区调 `hrtimer_start`（11 → 14），但等待 `hrtimer_cancel` 前必须释放 `TFD_CTX` 状态锁，只保留 `control_busy` 占位。
- timerfd 回调不取 `TFD_CTX`；`poll` 不取 `TFD_CTX`；否则 `epoll_pwake` 的间接 poll 会回取控制锁。
- `hrtimer_run_queues` 在 `TIMER_BASE` 锁外执行回调；周期重挂在清 `in_flight` 前重新取 base 锁，允许回调内调用 `hrtimer_start`，但不允许回调假设自己仍持 base 锁。

内存序：

- `seq` / `generation` 用 Release 写、Acquire 读，保证回调看到的激活序号与状态写入有序。
- `in_flight` 用 Release 置位、Acquire 读；清位必须在状态收敛或重挂写入之后，否则取消方可能提前释放。
- `state` 用 AcqRel；树链指针只在 base 锁内读写，不做无锁遍历。
- `TimerFdContext` 的 `ticks` 可用 AcqRel 交换；`poll` 只需 Acquire 读，允许瞬时旧值。

## 八、生命周期与错误模型

- `HrTimer` 发布（首次 `start`）后不得移动。终态通过 `!Unpin` 或类型契约保证；`HrTimer` 离开 `Pending/Running` 前不得 drop。
- `TimerFdContext` 由 `Arc` 持有；回调只借 `&HrTimer`，不克隆 `Arc`。`release` 的 `cancel` 保证回调退出后才释放最后一个 `Arc`。
- `hrtimer_start` 不再用 `0` 表达失败，返回 `StartOutcome`；`NotReady` 与 `Shutdown` 分离。
- `hrtimer_cancel` 仍返回“是否摘除了一个 Pending 激活”，不返回错误；上下文违规是编程错误，由文档、debug 断言和静态审计承担，不做运行时可恢复错误。
- `timerfd_create` 的失败面：fd 表耗尽 `EMFILE`，堆分配失败 `ENOMEM`；不再有固定槽位耗尽 `EMFILE` 的伪容量限制。
- 回调预算耗尽不是错误，而是分发策略：剩余到期项通过立即重装中断继续处理。

## 九、终态验收判据

以下判据不依赖具体实现细节，可作为“是否到达终态”的检查表：

1. **取消契约**：任意非回调上下文调用 `hrtimer_cancel`，返回后立刻释放 timer，ASan/形状测试无 UAF；`try_cancel` 在回调在途时返回 `Busy`。
2. **关闭契约**：`shutdown` 后 `start` 返回 `Shutdown`；回调自重挂与框架 Periodic 重挂都不复活；`init` 后可以重新启动。
3. **每 CPU 隔离**：CPU0 的 timer 不会进入 CPU1 的队列；`hrtimer_next_expiry(cpu)` 只返回本 CPU 最早到期；tickless 的 `cpu_id` 参数被真实消费。
4. **民用 CPU 矩阵**：x86_64 4/8/16 逻辑核与 aarch64 4/8 逻辑核机器上完成 SMP 启动、每核 tickless idle、并发 start/cancel/read；`MAX_CPUS` 有足够余量，单 base 不随 timer 数增长。
5. **无分配**：入队、取消、分发、Periodic 重挂的路径没有堆分配；回调路径没有锁分配或动态容器增长。
6. **timerfd 不风暴**：周期 timerfd 在无人 `read` 时只产生首次到期，不持续重挂；`read` 后按 interval 重挂。
7. **timerfd 生命周期**：`dup`/`close`/`fork` 下对象引用正确；fd 复用不会误唤醒新的 timerfd；多个 epoll 兴趣都能被唤醒。
8. **锁序**：`TFD_CTX` / `TIMER_BASE` 进入锁序表；回调不直接取控制锁；死锁矩阵 0 问题；锁序文档与实际一致。
9. **可扩展性**：多核并发 start/cancel/read 的基准不随全局锁串行；单 CPU 活跃 timer 增加时入队/取消保持对数级，不出现全队列扫描。
10. **旧语义回归**：itimer、POSIX timer、网络 poll timer、`hrtimer_sleep` 在 per-CPU 队列上保持原有可观察行为。
11. **容量边界**：`cpu_count > MAX_CPUS` 时启动明确拒绝并给出可诊断错误；不超过时每 CPU base 正常初始化；若未来提高上限，需重测静态 per-CPU 数组的内存与启动时间，不能静默截断 CPU。
12. **CPU 上下线**：quiesce 时存在 Pending、Running、Periodic 回调与并发 cancel，迁移后源 base 为空、目标 base 可正常触发；timerfd 在迁移期间 set_time/read 不丢对象、不落回离线 base；online 后新 start 正常绑定；offline 中止时 `hrtimer_cpu_online` 可复位。

## 十、开放议题

- **有序结构实现成本**：侵入式红黑树是无分配 O(log n) 的正解，但 Rust 侵入式树的安全边界与调试成本高。若首轮实现降级为有序侵入链表，只能作为过渡，不得把 O(n) 链表写成终态。
- **回调预算值**：预算过小会拉长到期延迟，过大则硬中断占用上升；需用真实 timerfd/网络负载标定，不在设计文档里拍一个未经测量的数。
- **epoll 唤醒 token 化**：timerfd 的多兴趣唤醒依赖 epoll 从“数值 fd 键”迁移到“对象/兴趣 token”。这是跨子系统前置条件，未完成前 timerfd 终态只能算“对象化完成、唤醒安全未完成”。
- **阻塞 read 接入**：`File::read_at` 当前是非阻塞快路径；阻塞语义需要 syscall/fd 层与 `WaitQueue` 对接。若暂不接入，应明确保留 `WouldBlock` 行为，不能宣称 Linux 完整阻塞语义。
- **`CLOCK_REALTIME`**：需要时钟基抽象与实时钟偏移维护；在此之前显式 `ENOTSUP` 是终态的一部分，不是临时缺陷。它不是 CPU 硬件可用性门槛，但会影响 Linux 用户态兼容度。
- **suspend/resume**：民用笔记本的睡眠/唤醒需要冻结并恢复每 CPU `TimerBase`、重新装填空闲到期、处理休眠期间错过的 monotonic 时间。本终态不承诺 suspend；若产品目标包含笔记本睡眠，必须另立时钟恢复协议。
- **CPU 上下线的平台接入**：timer 侧协议已定义（quiesce/强制迁移/online）；仍需平台在 offline 前调用 quiesce、保持 target 在线、串行化 offline。未接入前不能宣称整机支持 CPU offline。物理带电插拔不在本设计范围，其风险与逻辑上下线不同。
- **超过 `MAX_CPUS` 的机器**：逻辑 CPU 超过 1024 时，静态 `[TimerBase; MAX_CPUS]` 不再成立。提高上限会放大 per-CPU 静态数组，改动态 base 则需要 per-CPU 分配器与启动顺序支持；两条路都不改队列 API，但都属于系统容量设计，不是定时器局部优化。
- **`hrtimer_sleep` 的等待方式**：当前是自旋等待 tick；per-CPU 队列不改变这一点。若未来允许睡眠等待，需要单独的调度器接口，不与硬中断定时器终态混在一起。

## 十一、与现有计划的关系

取消协议的可执行收口记录在 [hrtimer-cancel-protocol.md](../plan/hrtimer-cancel-protocol.md)；本文不重复其状态字段，也不把本文变成任务清单。锁序的人读表见 [ref-lock-order.md](../explain/ref-lock-order.md)，机器单源见 [lock_class_order.json](../../scripts/lock_class_order.json)。

本文描述的是目标形态。落地顺序、批次拆分、每批验收与回滚条件归 `plan/` 管理；本文只提供判断“某个实现是否仍在中间态”的标尺。
