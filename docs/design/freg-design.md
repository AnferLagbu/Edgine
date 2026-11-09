# FREG架构设计

> FREG（Fault Recovery Stack）的目标架构设计：从"恢复型FREG"演进到"遏制 + 重建型FREG"，在标配硬件之上获得 MINIX 级的服务故障隔离能力。本文是设计立场文档，记录诊断结论、理论基础、架构形态与裁定记录；立场为**依赖标配硬件、不依赖特殊硬件**。

## 最初的想法与目标

### 缘起：为什么会有FREG

FREG的起点，来自 Edgine 对嵌入式 / IoT 场景的故障恢复诉求：**系统必须"跑起来就不宕机"**。初期的FREG选择了宏内核下最务实的路径——模块级 undo 回滚：用 `RecoverableMutex` 在加锁时记录旧值，panic 时把域回滚到屏障点。它证明了单地址空间内做模块级恢复可行，也让"故障恢复"成了 Edgine 的一等子系统。

但"预览版"的三个问题把我们推向了重新思考：**恢复天花板**（固有 bug 的终态是整机宕机）、**undo 的尽力而为本质**、以及"**没有遏制，恢复就无根基**"。

### 目标的演进

最初的追求是"能恢复"；现在的追求是一组更强的组合：

- **MINIX 级安全能力**：服务故障不宕机，系统降级继续跑；
- **宏内核级性能**：热路径零锁、零分配、零 TCB 调用；
- **依赖标配硬件、不依赖特殊硬件**：用 MMU / 页表、中断控制器、架构计时器、NMI·FIQ、IPI、watchdog、页表基址切换（CR3 / TTBR）等通用能力；不用 PKU·MPK、MTE、CHERI、专用隔离核、硬件 TEE 等专属扩展；
- **子系统本身坚固**：恢复机制有界、可审计、可证明。

这组目标的核心认知跃迁是：**恢复不是主机制，遏制才是地基；恢复干净性必须由结构保证，而非恢复代码写得对。** 由此FREG从"恢复型"演进为"遏制 + 重建型"。

### 原创主张：一个尚未被占住的组合

调研现有工作（MINIX 3、Singularity、RedLeaf、Theseus、CuriOS、Asterinas 等）后，我们确认以下组合在学术界仍是空白：

> **语言遏制 + 框架内核 + 依赖标配硬件、不依赖特殊硬件 + 微内核级故障重生 + 恢复范围可证封闭。**

这个组合蕴含三块原创性：

1. **理论主张（最硬）**：**恢复范围封闭**——遏制是编译期不变式，跨域状态互不可达 ⇒ 服务故障只可能损坏该服务自身状态 ⇒ 胶囊重建的恢复范围可证封闭。这把恢复从"尽力而为"升级为"有界保证"。该定理的显式前提是：**只有 TCB 可作共享页授予方，服务域之间永不授予**（见"L1 设计研讨"章的第三种通信形态）。若未来允许服务域互授，定理须重新证明，不能沿用当前结论。
2. **机制组合（完整）**：create 化重建主路径 + BSR + BHR 仅 TCB 的系统性、可审计恢复子系统（undo 已废弃）。
3. **故障消费纪律（最难）**：单地址空间内"域死亡 == 干净状态全清"的结构化并发设计。

成为真正"先河"的前提是三条都立得住：理论论证严谨、fault injection 实证恢复语义成立、性能数据证明宏内核级开销。

**强度说明（立场修正后）**：旧口径里"不需要硬件前提"是本组合最醒目的差异点；改为"依赖标配硬件、不依赖特殊硬件"后，这个差异点不再成立——多数现代内核都依赖 MMU 与中断。剩余的差异点是"语言遏制 + 框架内核 + 微内核级重生 + 恢复范围可证封闭"的组合，以及"只用标配硬件、不引入特殊隔离扩展"。前者更可验证，后者更窄；是否仍构成"学术界空白"，须以文献复核为准，本文不再把它当作已证结论。

### 立场修正：依赖标配硬件、不依赖特殊硬件

早先的立场把 FREG 当成"不需要硬件前提"的软件方案。这个立场过于天真：它把正确性押在程序的自律性上——跨域零共享、安全点纪律、TCB 不腐坏，全都只能靠"代码写对了"，没有运行时可验证的强制面。对内核这种长期演进、多人维护的代码，这不是可审计的正确性论证。

现在的立场是**依赖标配硬件，但不依赖特殊硬件**：

- **标配硬件（可依赖）**：MMU / 页表、本地与外部中断控制器、架构计时器、NMI / FIQ、IPI、watchdog、页表基址切换（CR3 / TTBR）。
- **特殊硬件（不依赖）**：PKU / MPK、MTE、CHERI 之类内存标签、专用隔离核、硬件 TEE / 机密计算，以及任何非通用专有扩展。这些只能作为可选增强档位，不能作为正确性前提。

为什么这样改：服务↔服务的语义隔离做成了编译期不变式，这部分仍然成立；但"故障能否被发现、裁决者能否入场、执行流能否被强制复位"这三件事，仅靠代码自律没有结构保证。标配硬件提供的正是这三件事的强制面：计时器 / watchdog 让死循环可被发现，NMI / FIQ 让不可屏蔽事件能打断，IPI 让核间复位可传达，MMU / 页表基址切换让裁决者与故障域在地址空间上分开。把正确性建立在这些通用能力上，比"假设代码自律"更可验证。

代价：FREG 不再是"零硬件前提"的方案。平台标配能力不全时必须降档，且降档路径沿用既有的"事件源硬件确认"表与"能力适配"节，不新造机制：无 NMI / FIQ 时退到 watchdog 硬复位；单核无扩展时同核原地裁决，连 NMI 也缺失则 watchdog 硬复位。降档意味着隔离强度或恢复粒度下降，这是接受硬件前提后必须如实写出的边界。

修正收益：新口径消除了旧表述的内部矛盾——半单地址空间本来就要用 MMU（裁决核心的 recovery 页表根、共享页的标准映射），旧的"不需要硬件前提"与它自相矛盾；事件源章（NMI / FIQ / watchdog / IPI）本来就把标配硬件当底座，旧口径也与之冲突。改为"依赖标配、不依赖特殊"后，地址空间设计与事件源设计都回到同一立场上。

## 问题定义与现状诊断

### 现状：恢复型FREG

FREG当前是**恢复型**子系统：不做遏制，只做恢复。机制层在 `privileged/freg/`（`RecoveryDomain`、`UndoLog` 字节级回滚、屏障快照、BBR/BSR/BHR 分层恢复），策略层在 `functions/freg/`（`RecoveryPolicy` 决策矩阵、故障归属、健康监测、级联恢复）。

恢复的运作方式：域通过 `RecoverableMutex` 在加锁时把受保护值记入撤销日志（[recoverable.rs:60-73](src/kernel/privileged/freg/recoverable.rs)），后台 tick 推进屏障快照，panic 时定位域并级联回滚到目标代，恢复失败逐层升级 BBR → BSR → BHR（[layered.rs](src/kernel/privileged/freg/reset/layered.rs)）。

现状定位：这只是**预览版**。它证明了单地址空间内做模块级恢复可行，但没有遏制能力，安全性与 MINIX 级相去甚远。

### 恢复天花板：固有 bug 的终态是整机宕机

回滚只能吸收**瞬态故障**（竞态、时序、偶发输入）。若域存在**固有 bug**——同一代码路径每次必然复现——回滚后重跑必然再次 panic。此时机制的行为（[domain.rs:107-140](src/kernel/privileged/freg/domain.rs)）：

- 连续失败 ≥5 次 → 置 `Quarantined`，返回 false；
- 指纹判重：`prev_fp == crash_fingerprint` → 直接把 failures 置 5 → 置 `Quarantined`，返回 false。

指纹判重本是防崩溃循环的设计（对固有 bug，重试无意义且有 crash-loop 风险），但它的后果是：级联回滚找不到可回滚的域 → `recovery_try_recover_from_idt` 返回 -1（[api.rs:149](src/kernel/privileged/freg/api.rs)）→ IDT handler 触发 `kernel_panic("Domain recovery failed")`（[table.rs:749](src/kernel/privileged/idt/table.rs)）→ BSR 失败 → BHR 键盘复位 / 三重故障（[bhr.rs:38-53](src/kernel/privileged/freg/reset/bhr.rs)），整机宕掉。

结论：**`Quarantined` 只阻止了"再尝试回滚"，没有消费这次 panic。** 一个服务域固有 bug 的终态不是"该域牺牲、系统降级继续跑"，而是整机宕机——这正是与 MINIX 的差距所在。

### undo 回滚的固有局限

- 只回滚注册过的字段（加锁时记录的旧值），未记录的状态与外部副作用（设备寄存器、IPC 副作用、已落盘数据）不可回滚；
- 回滚写回带校验和判断（[undo_log.rs:85-107](src/kernel/privileged/freg/undo_log.rs)），尽力而为，不保证语义一致；
- 恢复正确性依赖"恢复代码写得对"，属于**靠代码尽力**，而非**靠结构保证**。

## 实证调研：可变性分布与恢复覆盖率的真实缺口

"恢复范围封闭"要成为工程现实，先得回答一个诚实的问题：**当前代码里，到底有多少状态被 undo 日志覆盖？** 为回答它，我们对可变性做了两层审计——第一层数总量，第二层按"锁 vs 原子 vs 内部可变性"的交叉切面分类。结果直接改写了原始策略：**"全遏制"内部可变性既不可行，也不必要。**

### 第一层：可变性总量分布（functions 层口径）

| 可变性形态 | 数量 | 分布 | 说明 |
|---|---|---|---|
| 原子类型使用点（`Atomic*`） | 610 | 53 文件 | 含 import 与字段；**静态原子全局 21 处** |
| 锁原语使用点（`IrqSpinLock`/`SpinLock`/`Mutex`/`RwLock`） | 425 | 61 文件 | 含 sync 模块自身定义；**静态锁容器 16 处** |
| 内部可变性载体（`Cell`/`RefCell`/`OnceCell`/`UnsafeCell`） | 84 | 36 文件 | functions 仅 9 处，多为 `OnceCell`（写一次） |
| `static mut` 实际声明 | 0（functions）/ 1（privileged） | — | privileged 1 处为 `smp_init` 的 `AP_PER_CPU`（TCB 启动期） |
| `RecoverableMutex` | 定义 1 处 / **使用 0 处** | — | 仅 privileged 定义 + re-export |

计数口径：本次 grep 的 functions 层全量匹配；`Cell<`/`RefCell<` 为子串匹配，含 `OnceCell`/`UnsafeCell` 等衍生类型。

### 第二层：三类结构性发现

**发现一：锁保护类型内的内部可变性（undo 覆盖漏洞候选）**

锁只保护"结构性元数据"（槽位、表项），值本身走无锁原子——这是性能刻意为之，但 undo 日志（加锁时记录旧值）**无法捕获这些原子写**：

- [sysctl.rs:112-121](src/kernel/functions/config/sysctl.rs)：`SysctlEntry` 的 `AtomicI64/AtomicU64/AtomicBool` 在 `IrqSpinLock<[Option<...>; 32]>` 内，锁只管槽位分配，读写路径无锁；
- [iouring.rs:120-131](src/kernel/functions/io/iouring.rs)：`RingBuffer<T>` 的 `head/tail: AtomicU32` 在 `IrqSpinLock` 内——SPSC 环形队列的头尾指针是**语义性无锁状态**；同文件 `IoUring.flags: AtomicU32` 同理。

**发现二：无锁共享状态（越出恢复范围候选）——21 处静态原子全局，分四类**

| 类别 | 实例 | 丢失 / 回退语义 |
|---|---|---|
| FREG自身恢复控制 | [reset_config.rs:108-114](src/kernel/functions/freg/reset_config.rs) 的 `CURRENT_LAYER`/`RESET_IN_PROGRESS`/`BBR/BSR/BHR_ATTEMPT_COUNT`/`LAST_RESET_TICK`/`PARALLEL_ROLLBACK_ACTIVE`（7 处） | 不能回滚——它们是回滚动作的输入 |
| ID / 统计计数器 | `NEXT_NS_ID`/`NEXT_URING_ID`/`NEXT_CGROUP_ID`/`FLOCK_OPS`/`POSIX_LOCK_OPS`（5 处） | 丢失可容忍（ID 用新值继续） |
| 生命周期标志 | `NET_READY`/`NET_CONFIGURED`/`CGROUP_INITIALIZED`（3 处） | 重建时重置为已知初值 |
| 语义性无锁状态 | [memory_pressure.rs:93-98](src/kernel/functions/mm/memory_pressure.rs) 压力级 / 阈值（5 处）；`KASLR_BASE_OFFSET`（启动期定值） | 有协议保证，undo 会破坏其语义 |

**发现三（最关键）：`RecoverableMutex` 全内核无使用点。**

预览版 undo 回滚的注册覆盖率是 **0**——没有任何服务域的状态被登记进撤销日志。这意味着"恢复范围封闭"在当前代码里**不成立**：不是"记录不完整"，而是"根本没记录"。这是"预览版"最硬的一条实证：undo 机制存在，但从未接入真实业务状态。

### 策略修订：原子三级分类取代"全遏制"

"全遏制"（所有内部可变性都纳入 undo）**不可行且不必要**：

- **不可行**：发现一/二中的语义性无锁状态有明确无锁协议（SPSC 头尾、单写多读原子值），undo 回滚会破坏它们的语义；
- **不必要**：纯计数器丢失无害，生命周期标志 / 阈值在重建时可重置为初值。

唯一必须"遏制"的是 **C 类**——锁写路径上的普通字段（锁保护的 `&mut` 数据）。由此修订为三级分类策略：

| 级 | 类别 | 例 | 恢复归属 |
|---|---|---|---|
| A | 纯计数器 / 统计 | 尝试计数、锁操作统计、ID 分配器 | 不纳入恢复；回退 / 重置无害 |
| B | 语义性无锁状态 | SPSC `head/tail`、sysctl 原子值、压力阈值、生命周期标志、启动期定值 | 不纳入恢复；胶囊重建时重置为已知初值，语义由协议保证 |
| C | 锁写路径的普通字段 | 锁保护数据结构中的非原子字段 | **唯一纳入恢复的范围**：结构性强制（整块 capsule 重建） |

### FREG自身状态：越出恢复范围的登记

`reset_config.rs` 的原子变量是FREG**自身的恢复控制面**——它们是回滚动作的输入，若被回滚则自指（回滚动作自己需要被回滚）。处理方式不是"遏制"，而是**登记为越出恢复范围**：

- 语义被限制为简单原子 RMW（无复杂不变式），正确性靠构造保证；
- 作为控制面状态由 recovery 路径直接读写，生命周期与 TCB 一致；
- 对它们的破坏 = TCB 级故障 → BHR，与"TCB 受信任基座"的边界一致。

### 修订的恢复范围封闭定理

> 封闭对**锁写路径**成立（结构性强制：所有由锁保护的普通字段都落在某个 capsule buffer 内，恢复时整块清零重建；undo 注册路径已随 T-01 B 关闭）；对**已登记的越出恢复范围的无锁状态**（A/B 类 + FREG控制面）不提供恢复保证，但保证它们与恢复**隔离**——恢复不回滚它们，它们也不参与恢复正确性论证。

这把"恢复范围封闭"从一句口号，落成一条可审计的边界：**C 类写路径必须能证明封闭，A/B 类必须在登记表上，二者之间不允许第三类状态。**

### 推倒重来预案：必要时实现可从零重建

预览版的存在不等于新架构必须兼容它。**已定决策：在必要情况下，FREG实现允许推倒重来**——现有代码（`RecoveryDomain` / `UndoLog` 字节级回滚 / `RecoverableMutex` / 分层恢复）是"预览版"的证明性实现，不是约束。

- **触发条件（必要情况）**：旧实现与目标架构冲突，且改造面 ≥ 重建面。具体：若 C 类锁写路径的类型强制、胶囊重建、结构化并发所需的原语无法在旧实现上自然生长（旧接口绑定了 undo 世界观），则推倒重来。
- **判定口径**：**结构保证 > 代码复用**。能证明"恢复范围封闭"的形态优先于保留旧代码；复用旧代码若削弱结构性保证，则不复用。
- **保留清单**（推倒重来不丢的）：理论主张（封闭定理）、已定决策（方案 A、三级分类、单主路径、TCB 受信任基座）、检测与策略理念（BBR/BSR/BHR 分层、健康监测、故障归属）——可移植、可保留。
- **丢弃清单**：以 undo 为中心的机制本体（`RecoverableMutex` 注册、`UndoLog` 字节回滚）——若与胶囊重建的结构性强制冲突；已被审计证明 0 覆盖率的设施没有兼容义务。
- **推倒重来的边界**：TCB 边界不变；privileged 不可因FREG重建而扩大 unsafe 面；functions 层 0 unsafe、F1-F9 硬规则不变。

## 设计目标与边界

### 目标

对标 MINIX 微内核的安全能力：**服务域故障不导致整机宕机**，而是"该域牺牲/降级、系统其余部分继续运行"，同时满足：

- **保证性能**：热路径零锁、零分配、零 TCB 调用；
- **依赖标配硬件、不依赖特殊硬件**：两条路径都用标配能力（MMU / 页表、中断控制器、架构计时器、NMI / FIQ、IPI、watchdog、CR3 / TTBR 切换）；路径 1 不使用特殊隔离硬件（PKU / MTE / CHERI / 专用隔离核 / TEE），共享页授予走标准 MMU 映射；路径 2 额外依赖多核 + 每核 MMU + SMP + cache coherent（见"架构形态：Isolated 独立空间裁决核心"章）；
- **子系统本身坚固**：恢复机制自身有界、可审计、可证明。

### 诚实边界：TCB 是受信任基座

仅靠语言层遏制无法约束 privileged 的 unsafe 代码——TCB 内存一旦损坏，恢复机制本身运行在已损坏的内存上，不可信。因此语言隔离只作用于 functions 层；privileged（TCB）是**受信任基座**，TCB bug → BHR 是可接受的终态。依赖标配硬件不等于 TCB 受硬件约束：页表、中断向量、IPI 路由仍由 TCB 配置；TCB 损坏时，这些标配能力同样不可信。硬件前提约束的是"故障域与裁决者之间的强制面"，不是"TCB 一定正确"。

这与 MINIX 一致：MINIX 的微内核同样受信任，其安全来自"内核之外的一切被隔离且可重启"。对应到 Edgine：**TCB（privileged）受信任 + functions 软件隔离**，而非把整个内核都塞进软件遏制。

## 理论基础：两个先例与机制替代

### MINIX 进程重生

MINIX 的机制是**进程重生**：MMU 强制地址空间隔离（遏制）；进程死亡时内核拆除其地址空间、线程、栈、锁、IPC 端点（**状态与执行上下文全清**）；Reincarnation Server 重启驱动（逐维对照与双层映射见"对照参照：MINIX 3 进程重生与双层 FREG 映射"章）。

关键：一个机制（进程 + MMU）同时解决遏制与重启，干净性由硬件结构保证，无需恢复代码"写得对"。

### Singularity OS

Microsoft Research 的单地址空间 OS：不依赖 MMU，用类型安全语言（Sing#）做隔离——软件隔离进程（SIP）+ 类型化信道通信 + 系统验证器。

它证明了**"纯软件隔离"在理论上成立**：隔离可以由编译器强制，而非硬件。但它也划出了边界——语言能强制"不越界"，不能强制"故障可被发现、裁决者可入场、执行流可被强制复位"；这三件事仍要靠标配硬件。Singularity 最终未成为产品（性能与实用性的教训），其"语言强制隔离 + 显式通信"仍是本设计的原型，但本设计不再把正确性只押在语言上。

### 机制替代

| MINIX 靠硬件做到 | FREG 的替代（标配硬件 + 语言） | 代价 |
|---|---|---|
| 遏制：MMU 隔离地址空间 | 服务↔服务：所有权 / 类型系统强制（functions 100% safe Rust + 跨域零共享可变状态）；裁决者↔服务：标准 MMU 独立地址空间 | 编译期不变式零运行时开销；裁决侧用标准 MMU |
| 重启：进程拆除 | create 化重建（清零 capsule + `new()` + gen++，不跑 Drop） | 逐域改造 |
| 通信：IPC 过内核 | 内核内显式消息传递（无特权级 / 地址空间切换） | 低于真微内核 |
| 检测：硬件异常 | 标配事件源（NMI / FIQ / watchdog / IPI / 架构计时器）+ 软件策略（CPU 配额 / 心跳 / 不变式断言） | 需服务层配合；无 NMI / FIQ 时降档 |

### 核心命题

- **遏制 = 编译期不变式（服务↔服务）+ 标准 MMU（裁决者↔服务）**。MMU 在运行时拦截越界；所有权在编译期就保证不越界。前者有硬件开销，后者零开销——这是"保证性能"的答案；两条路径都不依赖特殊隔离扩展。
- **重启干净性 = 结构保证**。恢复单位是 privileged 静态分配的整块 capsule buffer：重建时 `memset` 清零 + 重调 `Domain::new()` + 锁内 `gen++`，**不跑任何 Drop**。若域状态只通过胶囊可达（无逃逸引用），则"整个 buffer 就是恢复边界"——等价于 MMU 回收地址空间；路径 1 的重建干净性由"类型系统 + 静态分配边界"承担，不要求重建时换页表，但裁决者侧的隔离仍由标准 MMU 提供。

> **［用户裁定］create 即总回收（T-01 B）**。`no_std` panic=abort 语境下 Drop 在故障现场不可靠（析构可能再拿锁 / 嵌套 panic / 依赖已破坏的不变式），因此恢复动作不执行析构，收敛为"清零整块 capsule buffer + 重调 `Domain::new()` + 锁内改代"。遏制命题不受影响，仍然成立；代价是放弃 Drop 式资源释放，外部资源改由 BCB 账本代管（见"全 create 化恢复模型"章）。

## Rust 特性利用：把遏制 / 回收 / 故障变成语言保证

Rust 支撑本设计的关键，在于它把"遏制、回收、故障"三件事从**运行时机制**变成**编译期或结构保证**。逐条映射到设计支柱。

### 遏制（语义隔离）：编译期不变式

| 特性 | 用法 | 落到设计哪里 |
|---|---|---|
| **所有权 + 移动语义** | 一个值只有一个拥有者；转移所有权零成本 | 胶囊状态唯一拥有权；域间传数据用 move（转移）而非共享 |
| **借用检查（`&mut` 独占 / `&` 共享不可变）** | 编译期禁止跨域共享可变状态 | "遏制 = 编译期不变式"的直接实现者——域 A 若持域 B 状态的可变借用，编译器就拒绝 B 同时触碰 |
| **模块可见性（`pub` / 私有）** | 私有字段不可命名、不可触碰 | 域内部状态对域外不可见 |
| **`Send` / `Sync` auto traits** | 编译器判定值能否跨线程转移 / 共享 | 跨域共享的编译器门禁——胶囊内部类型标 `!Sync`，编译器强制无人能共享它 |
| **内部可变性（`Cell` / `RefCell` / `Mutex`）** | 借用的"逃生门"，运行时检查 | 域内部可自由用；域边界必须禁用，由 API 设计 + 审计强制 |

### 胶囊重建（总回收）：create 即总回收

> ［用户裁定］本节路径为 T-01 B：**不执行析构**，恢复动作是"清零 capsule buffer + `Domain::new()` + 锁内 `gen++`"。Drop / RAII 路径已废弃，仅作为历史实现记录在"单主路径"章。

| 特性 | 用法 | 落到设计哪里 |
|---|---|---|
| **静态 capsule buffer** | privileged 预分配、定长，恢复单位 = buffer 边界 | 恢复范围由分配划定，不由代码行为决定 |
| **移动语义** | 域状态只通过胶囊可达（无逃逸引用） | 清零整块 buffer 即状态总清——不依赖析构遍历 |
| **显式资源账本（BCB）** | 外部资源（设备句柄、页、fd）不归域 Drop | 重建后由 BCB 批量代管 / 回收；不执行域析构 |

### 故障消费：panic + 错误传播

| 特性 | 用法 | 落到设计哪里 |
|---|---|---|
| **`panic`（内核通常 `panic=abort`）** | 域内 panic = "我坏了"的故障信号 | 现有FREG正是这么接的（IDT 路径）；把 abort 改造成"胶囊重建入口"：panic handler → FREG → 清零 capsule → 重建 |
| **`Result<T,E>` + `?`** | 错误沿调用栈显式传播 | 域死亡的消费者——调用方拿到 `Err` 而非 kernel panic；域边界的返回类型即故障契约 |
| **线程模型 + `Send`** | 线程对象可被拥有 / 转移 | 每域静态栈池 + SP 重置；线程只是槽位内的任务记录，重建不依赖栈回收（T-07 A） |

### 域间通信与性能

| 特性 | 用法 | 落到设计哪里 |
|---|---|---|
| **移动语义（零拷贝转移）** | 传数据 = 转移所有权，非复制 | 内核内 IPC 的零拷贝方案 |
| **`trait` 接口（含 trait object）** | 把域边界协议形式化为接口 | 胶囊接口 = trait；调用点只依赖 trait 而非内部实现 |
| **泛型 + 静态分发** | 编译期单态化，无虚表 | 热路径无虚表、无动态分发开销的直接答案 |
| **`Arc` / `Weak`** | 只读共享所有权 | 跨域共享只读数据（如配置）时可用——但需纪律：共享可变 / 引用计数会破坏"总回收"，是语义隔离的缺口 |

### 纪律与审计（让上述保证可维护）

| 特性 | 用法 |
|---|---|
| **`unsafe` 的封装性** | 只允许 privileged 用 unsafe 并封装为 safe API；TCB 边界本身就是遏制边界 |
| **宏 / derive** | 把"域边界纪律"声明式化（自动生成胶囊骨架、IPC 协议），减少手写错误 |
| **`cfg` 条件编译** | fault injection / 测试钩子（项目已有 `kernel_test` / `fault_injection` feature） |
| **typestate / PhantomData（类型状态）** | 编码胶囊生命周期状态机（Running → Faulted → Rebuilt），非法转移在编译期拒绝 |

### 三个关键杠杆

1. **所有权 + 借用**：把"遏制 = 编译期不变式"从口号变成编译器强制——这是"恢复范围封闭"理论能成立的语言学基础。
2. **静态 capsule buffer**：把"重启干净性 = 结构保证"变成确定性语义——清零 buffer + `new()` + gen++ 即总回收（不依赖 Drop）。
3. **`panic` + `Result`**：把"故障消费"变成可表达的协议——panic 是自杀信号，`Result` 是死亡通知。

### 诚实的边界

- **内部可变性、`Arc`、`unsafe`** 是三个破坏上述保证的逃生门，不能消灭，只能**用纪律约束**（API 边界 + 审计脚本）。
- **unwind vs abort**：`no_std` 内核默认 abort，无法用 `catch_unwind` 优雅吞掉 panic——所以"域死亡"必须走**胶囊重建**路径（abort 信号 → FREG接管）。这是本设计与用户态 try/catch 思路的分水岭。

## 单主路径：create 化重建（undo 已废弃）

- **唯一主路径 = create 化重建**：结构保证的干净重启，对标 MINIX 进程重生；恢复动作 = 清零 capsule buffer + `Domain::new()` + 锁内 `gen++`。
- **undo 回滚 = 已废弃的历史实现**：它只吸收瞬态故障，且外部副作用不可回滚、恢复范围依赖记录覆盖、恢复正确性不可证明。`UndoLog` / `RecoverableMutex` / `DomainState<T>` 强制注册一系机制整体退出目标架构，不再作为"兜底"或"默认"路径。

> **［用户裁定］create 化取代 undo（T-01 B）**：恢复收敛为**单主路径**（裁决 → create 化重建）。命门①"undo 注册的类型系统强制"随之关闭——不再需要证明"哪些写被登记"，只需要划对"哪块 buffer 属于该域"；三级分类的作用从"定义 undo 覆盖"收敛为"定义 buffer 边界与跨域存活状态的登记"。undo 预览版仅保留为可行性证据；`undo_log.rs` / `recoverable.rs` 的删除前提（S4）已成立。

## 四个深层子问题

### 语义隔离

"内存安全"（safe Rust 已保证不越界）与**语义隔离**（一个域的 bug 连语义上都碰不到另一个域的状态）是两回事。后者要求**跨域零共享可变状态**、全部显式通信。

语义隔离是恢复封闭性的前提——只有跨域状态互不可达，"胶囊重建只重建该域状态"才是可证明的。

### 故障消费

单地址空间里"杀死一个域的线程"：栈与锁的所有权、被阻塞调用方唤醒。MINIX 靠进程拆除免费获得；单地址空间需显式设计（结构化并发）。若不做故障消费，panic 冒泡 → BHR——正是当前现状。

### 检测：标配硬件事件源 + 软件策略

检测建立在标配事件源上，软件只负责判据与策略：

- 死循环 → 架构计时器 / watchdog（标配）；
- 不可屏蔽事件 → NMI / FIQ（标配；无此能力的平台降档，见"能力适配"节）；
- CPU 占用 → 配额（现有 `cpu_quota_max` / `check_quota`，[domain.rs:286-296](src/kernel/privileged/freg/domain.rs)）；
- 心跳（现有 `heartbeat_max_gap` / `check_health`，[domain.rs:302-313](src/kernel/privileged/freg/domain.rs)）；
- 不变式断言（I1-I6，见 [explain-framekernel.md](./explain-framekernel.md)）。

硬件负责"事件能到达裁决者"，软件负责"事件是否构成故障"；无 NMI / FIQ 时退到 watchdog 硬复位，这是能力下界而非设计缺口。

### 恢复正确性论证

因为遏制是编译期不变式，"服务故障只可能损坏该服务自身状态"**可证明** → 胶囊重建的恢复范围**封闭**。这把恢复从"尽力而为"升级为"有界保证"。

封闭的工程边界由实证调研钉死（见"实证调研"章）：封闭对**锁写路径**成立（C 类结构性强制）；对**已登记的越出恢复范围的无锁状态**（A/B 类 + FREG控制面）只保证隔离、不保证恢复。任何既不在 C 类写路径、又不在登记表上的状态，都是封闭性缺口，须在设计中消除或登记。

## 架构形态：域 = 胶囊

域 = **所有权作用域（胶囊）+ 显式 IPC**。域拥有状态（数据结构）；执行回收的**理论终态是方案 A（完整胶囊）**——域同时拥有线程组，经协作取消（安全点）+ 每域静态栈池的 SP 重置完成执行流回收（见下"域边界模型"）。

分层恢复形态：

```text
L0 遏制（所有权/类型系统）        ← 本设计的根本，编译期不变式
L1 检测与裁决（健康/心跳/配额/仲裁） ← 纯函数策略，零热路径开销
L2 create 化重建（固有 bug 主路径）  ← 结构保证的干净重启
L3 BSR（设备软重置）              ← 现有能力
L4 BHR（仅 TCB）                 ← 受信任基座故障才允许整机宕机
```

### 域边界模型：执行回收的终态是方案 A

"域是否拥有线程"是执行回收的分叉点。所有权只保证**状态总回收**，不保证**执行总回收**——在共享地址空间里中止一个正在跑的线程是硬问题。谱系：

| 方案 | 形态 | 故障时执行流处理 | 改造面 |
|---|---|---|---|
| B 半胶囊 | 域只拥有状态，线程归调度器 | RCU 式排空（grace period） | 最小 |
| B+ 域 worker 池 | 域拥有固定 worker 池 | 整池重建 + 有界泄漏 | 中 |
| **A 完整胶囊（终态）** | 域拥有状态 + 线程组 | 协作取消（安全点）+ 静态栈池 SP 重置 | 最大 |

**已定决策：第一条路径（共享地址空间一体式FREG，语言遏制为主）的理论终态 = A。** 含义：

- **定义**：域胶囊拥有「状态 + 线程组」；故障时经协作取消（安全点）+ 静态栈池 SP 重置完成执行流回收，向 MINIX 进程模型对齐。
- **地基**：crash-only 段纪律——安全点之间的代码段只动域局部状态，这是"中止线程是否安全"的前提。
- **实施路线（已记：方案之一 = 形态 A + 推倒重来）**：面向 A 设计；施工上允许推倒重来——弃旧 undo 预览版（`RecoverableMutex` / `UndoLog` / `DomainState<T>`），直接引入静态 capsule buffer 池与每域静态栈池，不背兼容包袱（见"推倒重来预案"）。B/B+ 不作为默认路线，仅作渐进验证台阶。
- **分工边界**：A 管"域怎么死"；Isolated（独立空间裁决核心，另章讨论）管"谁裁决 + 谁能强制复位"，两者正交。

**诚实的上限**：A 是共享地址空间内、仅靠语言约束与静态栈池做执行回收的极限表达；它只解决"共享空间内的执行回收"，不解决"任意时刻拆除执行流 + TCB 故障也能裁决"——后者是 Isolated 路径（第二条路）的地盘。

域粒度、与现有 `RecoveryDomain`（恢复域）的关系均为开放议题（恢复范围的工程强制已由"create 化 = buffer 边界"承接，见下）。

### 命门决策：命门①随 create 化关闭，命门②保留

方案 A 能否成立，原取决于两条工程命门。T-01 B 裁定后，命门①被 create 化直接消解，命门②仍需成立。

**命门①：undo 注册的类型系统强制 → 已关闭（［用户裁定］T-01 B）。** 恢复单位改为 privileged 静态分配的整块 capsule buffer，"恢复范围"由 buffer 边界划定，不再需要"加锁即登记旧值"的 `DomainState<T>` 受控锁原语。A/B 类无锁状态仍按登记表管理，但只用于"是否跨代存活"的边界判定，不参与 undo 覆盖论证。

**命门②：crash-only 段的强制方式 → 是。** 以结构强制为主：作用域 / typestate 让跨安全点的跨域引用在编译期非法，审计脚本兜底。随方案 A 的胶囊原语一并设计，不单独前置。

实现载体与推倒重来一致：弃旧 `RecoverableMutex` / `UndoLog` / `DomainState<T>`，在干净地面引入静态 capsule buffer 池与每域静态栈池（T-07 A）。命门②的代价由三级分类收窄为"buffer 边界 + 跨代存活登记"，仍是可审计、可落地的工程决定。

### BCB：FREG全局控制块（全局唯一）

BCB（FREG Control Block）是FREG的**全局唯一控制中枢**——类比内核的进程表 / TCB 目录：一个结构承载FREG对全系统域的完整控制视图，替代散落各处的域元数据与FREG控制面。

**动机**：路径 1（胶囊模型）与路径 2（Isolated 仲裁）都需要"对每个域看全、看一致"，而现状要跨 privileged 层（`RecoveryDomain` / `RecoveryManager`）与策略层（`FaultAttribution` / `DomainTopology` / `HealthMonitor`）多个结构拼接。BCB 提供统一的**域控制事实源**，两条路径共享同一份元数据视图。

**职责边界**：BCB 只做**控制面**（元数据）；域的实际业务状态在**胶囊**（数据面，create 化总回收）。类比：TCB 只含线程元数据，不含线程栈。这条分离是 BCB 不膨胀、不引入热路径开销的前提。

#### 功能区与内容（五区）

| 区 | 内容 | 落位 / 替代 |
|---|---|---|
| ① 全局身份与版本 | `magic` / `version`（防误用、防布局漂移）；裁决维度 `adjudication: AdjudicationMode`（Inline / Isolated，静态配置，非派生）+ 派生健康 `SystemHealth`（`Degraded` 仅为派生聚合值，顶层状态机无此状态）；域级"结构已恢复、设备未复位"由 `DomainEntry.phase` 表达（T-02 B / T-11 A） | 新建 |
| ② FREG控制面 | `current_layer` / `reset_in_progress` / `bbr/bsr/bhr_attempt_count` / `last_reset_tick` / `parallel_rollback_active` / 全局健康水位 | 归位 [config.rs:144-156](src/kernel/privileged/freg/reset/config.rs) 的静态原子；`functions/freg/reset_config.rs` 实测仅 12 行、纯 re-export 壳，不是定义处 |
| ③ 域注册表 | 域槽位数组 + 空闲位图 + 查找索引；每域 `DomainEntry`（状态机 / 代 / 配额 / 心跳 / 恢复等级 / 是否 TCB 边界标记）；拓扑依赖 | 替代 `RecoveryManager` + `RecoveryDomain` 控制面部分 |
| ④ 恢复范围登记表 | 每域三级分类 A/B/C 归属；已登记的无锁共享状态清单（A/B 类）；越出恢复范围的FREG自身状态标记；胶囊重建现场（重建语义 / 重置函数指针） | 三级分类与封闭定理的**工程落点** |
| ⑤ 仲裁现场 | 现场快照入口（Inline 层维护、Isolated 层只读）；裁定结果 / 复位裁定历史 | 服务路径 2 的仲裁输入 |

#### 域注册表 vs 域目录（命名决策）

命名承载设计承诺：

- **目录 Directory** = 固定槽位的预分配实体，无生命周期，查不到就是没有——与现有 `RecoveryDomain` 静态预分配一致，但**与胶囊模型冲突**（胶囊的核心是域的创建 / 重建 / 销毁）；
- **注册表 Registry** = 有生命周期的成员（`register` / `unregister` / `iter`）——贴合路径 1 方案 A 的胶囊重建语义，且与④"恢复范围**登记**表"用词同源（同是 register 语义），与路径 2 的成员遍历（仲裁需列出所有域现场）兼容。

**已定：域注册表 DomainRegistry**。语义上是注册表（对外生命周期接口），物理上是目录（固定槽位 + 空闲位图，保留 O(1) 查找与紧凑性，不引入链表遍历开销）：

```rust
pub struct DomainRegistry {
    // 物理：固定槽位 + 空闲位图（目录形态，O(1) 查找）
    slots: [Option<DomainEntry>; MAX_DOMAINS],
    free_bitmap: AtomicU64,
    // 语义：register() / unregister() / iter() 生命周期接口（对外呈现注册表）
}
```

#### 域注册契约：`Domain::new()` 是最小初始化器

域注册即契约。注册一个域，同时承诺它的 `Domain::new()` 满足两条：

- **由 TCB 提供**：`Domain::new()` 不是域自行编写的任意业务入口，而是由 TCB 提供、按域声明的静态初值与字段布局落地的最小初始化器；
- **不得执行业务逻辑**：初始化器只允许做"字段置初值 / 寄存器与静态状态复位 / 句柄表清空"，不得发起 I/O、加锁、分配、调用其他域、读取故障现场，也不得依赖任何外部服务。

理由：恢复路径上唯一会执行的域侧代码就是 `Domain::new()`。若它允许执行业务逻辑，"恢复不执行故障现场的析构与可变状态"就不再可自证——初始化器本身可能依赖已损坏的状态或外部服务。把约束收窄到"最小初始化器"，恢复动作的执行面才收敛为"清零 + 置初值"，与故障域的行为无关。

注册即契约（S3 才实施）：违反契约的域不具备注册资格。本条在文档中只作契约条款，不展开实施细节。

#### 并发与性能

- **全局唯一但不全局锁**：每域槽位独立锁 / 原子 + 目录元数据单独锁（读路径近似 RCU 无锁）；
- **热路径不碰 BCB**——服务调用走胶囊（数据面），BCB 只在登记 / 健康采样 / 故障仲裁时访问，避免 BCB 成为锁竞争点。

#### 初步骨架

```rust
/// BCB：FREG全局控制块（控制面事实源）
///
/// 布局：目录元数据自旋锁保护；每域槽位独立原子 / 锁；
/// 热路径（服务调用）不碰 BCB，仅在登记 / 健康采样 / 仲裁时访问。
pub struct FREGControlBlock {
    // ① 身份与裁决维度
    magic: u32,
    version: u32,
    adjudication: AdjudicationMode,  // Inline | Isolated（静态，非派生）

    // ② 控制面（原 reset_config.rs 静态原子归位）
    current_layer: AtomicU32,
    reset_in_progress: AtomicBool,
    bbr_attempt: AtomicU32,
    bsr_attempt: AtomicU32,
    bhr_attempt: AtomicU32,
    last_reset_tick: AtomicU64,
    parallel_rollback_active: AtomicBool,

    // ③ 域注册表
    registry: SpinLock<DomainRegistry>,

    // ④ 恢复范围登记表
    scope_table: SpinLock<ScopeTable>,   // 每域 A/B/C 归属 + 无锁状态登记

    // ⑤ 仲裁现场
    arbiter_view: ArbiterView,           // Inline 写 / Isolated 只读
}
```

#### 与既有代码的替换映射

| 现状 | BCB 落位 |
|---|---|
| [reset_config.rs:108-114](src/kernel/functions/freg/reset_config.rs) 7 个静态原子 | ② 控制面 |
| [mod.rs:99](src/kernel/privileged/freg/mod.rs) `RecoveryManager` | ③ 域注册表 |
| [domain.rs:11](src/kernel/privileged/freg/domain.rs) `RecoveryDomain` 控制面部分 | ③ `DomainEntry`（undo 时代回调字段已废弃，T-01 B） |
| 三级分类 + 封闭定理（设计层） | ④ 登记表（工程落点） |
| Isolated 仲裁输入（设计层） | ⑤ 仲裁现场 |

#### 单点风险（可接受终态）

BCB 是FREG控制面的单点——被破坏 = TCB 级故障 → BHR。这与既有"FREG控制面越出恢复范围、破坏即 TCB 级"的边界一致（见"实证调研"章），不引入新的信任缺口。

## 架构形态：Isolated 独立空间裁决核心（第二条路径）

### 定位：回答路径 1 的两个上限

路径 1（域 = 胶囊）是共享地址空间内、仅靠语言约束与静态栈池做执行回收的极限表达，但它有两个诚实的上限：

- **任意时刻拆除执行流**：协作取消依赖安全点，被取消的线程必须恰好停在安全点；共享地址空间里"强制中止一个正在跑的线程"没有硬件背书——这正是路径 2 要用标配硬件（多核 + 每核 MMU + IPI + NMI / FIQ）补上的强制面；
- **TCB 故障也能裁决**：路径 1 的裁决者（FREG自身）与故障域同处一个地址空间，若 TCB 内存损坏，裁决本身不可信。

第二条路径用**独立空间裁决核心**（Isolated）回答这两个上限：把"谁裁决 + 谁能强制复位"从故障发生的地址空间中挪出去，放在一个隔离的裁决环境（独立地址空间；执行手段见"隔离手段光谱"）。

分工边界：**A 管"域怎么死"；Isolated 管"谁裁决 + 谁能强制复位"**。两者正交——路径 1 负责域内的干净回收语义，路径 2 负责域外的仲裁权威与强制力。

**半单地址空间定位**：路径 2 处于单地址空间（路径 1）与全多地址空间（MINIX）之间——**主体服务间仍单地址空间**（软件遏制，零锁、零分配、零 TCB 调用），**只有裁决核心那一半独立出去**（隔离地址空间）。隔离是**非对称**的：服务与服务的遏制靠语言（编译期、零成本），裁决者对服务的隔离靠硬件（最强、冷路径）。对应性能：**热路径比 MINIX 更快**（无特权级/地址空间切换），**冷路径安全效果比肩 MINIX**（仲裁权威性相当，且裁决环境不参与服务运行、不被故障域拖累）。

> **口径澄清**："半"按**主体**划分，不按时间划分。主体服务域——无论健康还是故障——都留在同一个内核地址空间；只有裁决核心拥有独立页表根（预置 recovery 页表，平时在所有核的页表中不映射，事件时才切入）。因此"FREG 没有独立地址空间"只在"服务域"这一侧成立；"FREG 有独立地址空间"只在"裁决核心"这一侧成立。非对称隔离的方向也不是"故障域 vs 健康域"，而是"服务↔服务"（语言）与"裁决者↔服务"（硬件）。
>
> **［用户裁定］** FREG **不按需为故障域新建独立地址空间**：被恢复域仍在原 capsule buffer 上 `memset + new() + gen++`，只有裁决核心持有独立 recovery 页表根。这是半单地址空间的边界，不是待议选项；代价见"对照参照"章的"取舍"段。
>
> **与新立场的衔接**：这一口径与"依赖标配硬件、不依赖特殊硬件"一致——半单地址空间本来就要用标准 MMU（recovery 页表根与共享页映射），旧的"不需要硬件前提"在此自相矛盾；新口径把这一点显式化，属于修正收益。

### 双层结构

第二条路径是双层FREG：

| 层 | 位置 | 职责 | 路径 |
|---|---|---|---|
| Inline 层 | 与内核同地址空间 | 热路径：健康监测采样、只读快照发布、gen-check | 每次服务调用都经过，须零 / 极低开销 |
| Isolated 层 | 事件时借核（主推）或专用核（档位之一），独立地址空间 | 冷路径：故障仲裁、裁决是否重建、强制复位 | 仅故障时进入，开销不敏感 |

Inline 层不引入额外隔离，保持宏内核热路径的性能形态；Isolated 层用独立地址空间把裁决核心与故障域隔开——故障即使击穿 Inline 层的一切遏制，也无法直接污染裁决者。

### 硬件依赖论证

FREG 整体依赖**现代处理器普遍具备的标准能力**，而非特殊硬件；两条路径的依赖深度不同：

- **MMU / 页表**（两条路径都用）：共享页授予走标准 MMU 映射；Isolated 层用它给裁决者分配独立地址空间；
- **中断控制器 / 架构计时器 / NMI·FIQ / watchdog / IPI**（两条路径都用）：检测与裁决的入场手段；无 NMI / FIQ 时按"能力适配"降档；
- **页表基址切换（CR3 / TTBR）**（Isolated 层用）：事件时切入 recovery 页表；
- **多核 + 每核 MMU + SMP + cache coherent**（路径 2 额外需要）：存在可被指派或借用的核，裁决者可读取隔离核的现场。

跨架构支持：

| 架构 | 支撑能力 | 说明 |
|---|---|---|
| x86_64 | 多核 + 每核 CR3 页表 | 通用 |
| aarch64 | 多核 + TTBR0/1 + ASID | 通用 |
| RISC-V | 多核 + SATP + SBI + PMP | **最优**：SBI 提供核间管理与复位接口，PMP 可作物理内存隔离的又一重保障 |
| LoongArch | 多核 + 每核页表 | 通用 |

与路径 1 的关系：路径 1 同样依赖标配硬件——共享页授予走标准 MMU 映射，检测依赖架构计时器 / 看门狗 / 中断，只是不需要多核；它不使用特殊隔离硬件（PKU / MTE 等专属扩展），单核也能结构隔离。路径 2 在此基础上额外要求多核 + 每核 MMU，把"多核 + 每核 MMU"这些标准能力用作裁决者的隔离手段，而非依赖专属加速特性。二者共享"依赖标配、不依赖特殊"的立场，区别在于路径 2 需要多核才能获得真隔离。

### 隔离手段光谱：消除"扣核"尴尬的三个档位

"裁决者必须在故障域之外"有三种硬隔离手段，专用核只是其一。三者构成**三角张力**：**不扣核 ∧ 效果一致（隔离裁决者）∧ 不依赖特殊硬件扩展，只能取其二**。

| 档位 | 隔离本质 | 硬件依赖 | 扣核 | 适用 |
|---|---|---|---|---|
| **按需借核（主推）** | 空间隔离（独立地址空间按需映射） | 多核 + 每核页表（标准） | 仅事件时瞬时 | 所有多核设备 |
| 专用核 | 空间隔离（常驻独立地址空间） | 多核 + 每核 MMU（标准） | 持续 1 核 | 有富余核设备 |
| 特权层守护 | 特权隔离（HYP / EL3 / HS） | 虚拟化/安全特权扩展 | 无 | 有安全扩展设备（不限核数，单核可用） |

**按需借核（任何核皆可借）**是消除"专用核扣一颗大脑且平时不干活"尴尬的主推方案：

- **平时**：所有核自由运行任务，零扣核；冷路径FREG驻守独立地址空间，且**该空间平时在所有核的页表中不映射**——任何代码（含故障 TCB）物理上不可达；
- **事件时**：事件源（硬件异常 / NMI / FIQ / 看门狗，现代平台标配）投递到任意核 → 该核"变身"裁决核（保存上下文 → 注入冷路径空间映射 / 切换页表 → 执行裁决）；
- **结束后**：撤销映射、恢复原上下文，核归还。

配套机制（四件）：① **事件源 = 标配硬件**（见下）；② **最小切换代码**：每核只读段静态存在，TCB 级信任（保存上下文 + 切页表 + 跳转，极小）；③ **短暂扣核的锁纪律**：裁决逻辑不能依赖被挂起任务的锁状态（被变身核原任务被挂起微秒级）；④ **并发变身协议**：多事件同时到达需"主导-协作"仲裁。

**事件源硬件确认**（现代平台标配，非特殊硬件）：

| 架构 | 事件源 |
|---|---|
| x86_64 | NMI（LINT1）、APIC、Machine Check、watchdog |
| aarch64 | FIQ（GIC）、SError/SEA、generic timer、watchdog |
| RISC-V | NMI / SBI、PLIC/CLINT、IPI、watchdog |
| LoongArch | NMI、IPI、watchdog |

这些均属"现代处理器标配能力"，不依赖 SGX / TrustZone / 专属加速器等特殊硬件；RISC-V 若缺 NMI 可降级用 IPI + 最高优先级 IRQ。这张表就是新立场的直接体现：事件源全部是标配能力，不是特殊硬件；FREG 的检测与裁决入场依赖它们，检测由此明确建立在硬件事件源之上。

### 能力适配

按硬件配置选档：

- **有安全特权扩展**（虚拟化 / EL3 / HS，不限核数）→ 特权层守护（最优雅，不扣核，**单核也可用**）；
- **无扩展 + 多核** → 按需借核（主推，不常驻扣核）；
- **无扩展 + 有富余核且接受扣 1 核** → 专用核档；
- **无扩展 + 单核** → 按"是否存在不可屏蔽中断"分叉：x86_64 / aarch64 的 NMI / FIQ **软件关不掉**（IF=0 拦不住），watchdog 事件照样能打开场——同核原地裁决（NMI handler 变身 → 裁决 → create 化 → 改写中断返回帧重定向到新入口），外科式恢复仍成立，代价是全系统冻结数十 µs（结构段 ≤50µs、整体 ≤100µs 硬顶，见"性能论证"章）；只有连 NMI 也缺失 / 可屏蔽的 RISC-V 低端单核 MCU，才落到"裁决者无入场机会、看门狗硬复位"——那已是硬件能力下界，任何软件方案（含 MINIX）在此都只能复位。

任一配置下都有确定的恢复语义，只是隔离强度、扣核形态与恢复粒度不同。

### 架构抽象

Isolated 层所需的架构差异（专用核指派、地址空间切换、核间同步、复位）收敛到一个**窄架构抽象层**，复用现有 `arch!` 宏系统——把每架构不同的 3~5 个原语（如 `allocate_isolated_core`、`switch_isolated_address_space`、`signal_isolated_core`、`reset_isolated_core`）作为宏分支，其余仲裁逻辑架构无关、用纯 Rust 单份实现。仲裁逻辑本体保持单份，跨架构只换薄薄的适配面。

### 已定决策

- **双层结构**（Inline 热路径 + Isolated 冷路径仲裁）已定；
- **［用户裁定］半单地址空间 + 非对称隔离**定位（主体单地址空间、裁决核心独立；热路径超越 MINIX、冷路径比肩）已定。**被恢复域不新建独立地址空间**：重建是在原 capsule buffer 上 `memset + new() + gen++`，全程沿用内核共享地址空间；只有裁决核心有独立 recovery 页表根。**不采用** MINIX 的"整进程换新地址空间"。见"对照参照：MINIX 3 进程重生与双层 FREG 映射"章的取舍条；
- **隔离手段光谱**（按需借核主推 / 专用核 / 特权层守护），专用核降级为手段之一，已定；
- **按需借核**（任何核皆可借 + 冷路径空间平时不映射 + 事件时按需变身 + 结束归还）作为主推档位，已定；
- **事件源 = 标配硬件**（NMI / FIQ / 看门狗 / IPI，跨架构确认），依赖标配硬件、不依赖特殊硬件，已定；
- **能力适配**（特权扩展含单核 / 多核借核 / 富余核专用 / 单核无扩展看门狗回退）已定；
- **［用户裁定］FREG 语义 crate（方案 B）**：整体而非只切裁决核心——leaf crate `kernel-freg` 承载状态机 / 注册表不变量 / 仲裁 / 策略 / 检测聚合 / 审计事件类型，零依赖 + `forbid(unsafe_code)`；全局状态、锁、`no_mangle` 符号、硬件装配留 `kernel-privileged`，启动期注入 `FregEnv` / `FregHw`。见"FREG 语义 crate（方案 B）"章；
- **［用户裁定］全 create 化恢复（T-01 B）**：弃 undo / 弃 Drop，恢复动作 = `memset` capsule + `Domain::new()` + 锁内 `gen++`；外部资源由 BCB 账本代管。undo 路径废弃，S4 删除 `undo_log.rs` / `recoverable.rs` 的前提成立。见"全 create 化恢复模型"章；
- **［用户裁定］三量状态模型（T-02 B / T-11 A）**：`AdjudicationMode`（静态）+ `SystemHealth`（派生）+ `DomainEntry.phase ∈ { Structural, Device }`（仅 `Rebuilding` 内有效）；不设第八态，`Degraded` 不再作为顶层状态名（同名只保留为 `SystemHealth` 的派生聚合值：存在 `Quarantined` / `Faulted` 且无 `Rebuilding`）；
- **［用户裁定］权威三源与 PMM 归属（T-03 A）**：权威输入 = 持久存储 ∨ 静态硬件事实 ∨ TCB 影子账本；**PMM 暂列 TCB 机制**（故障 → BHR），留复议条件（EGFS 依赖实测后若证明恢复收益显著，再议"影子账本权威域"）；
- **［用户裁定］第三种通信形态（T-04 A）**：TCB 中介共享页授予（写权归授予方 / 硬件，TCB 按 `gen` 单方撤销）；EGFS↔PMM 回收环拆为 `evict → free → done` 两条单向消息，不保留回调；
- **［用户裁定］`kernel-freg` 规模口径（T-12 A）**：放弃行数承诺，保留 crate 名 `kernel-freg`；"小到可穷尽审查"由"零依赖 + 零 unsafe + 纯函数化"三项性质承担；
- **［用户裁定］world-stop 双层口径（T-08 C / T-09 A）**：结构段 `t_entry → t_rebuilt` ≤50µs（能力档内：≤8 核 / PCID·ASID / 并行 IPI 前提），整体 ≤100µs 硬顶；三段计时 `t_entry → t_adjudicated → t_rebuilt`；Path1 / Path2 分别出实测分项；
- **［用户裁定］硬件等待硬超时（T-06 D）**：结构段同步，有界等待 100µs 硬顶，设备复位后台异步；C3 工期 3–5 周；
- **［用户裁定］栈模型（T-07 A）**：每域静态栈池 + SP 重置，不采用动态栈 / 栈回收；
- **［用户裁定］Path 判据（T-14 A）**：故障类型为主判据，硬件能力决定可用档位，严重度只影响重试 / 隔离阈值；
- **［用户裁定］`recovery_freg_maintenance`（T-05 B）**：删除该零调用入口；删除前核对函数体，独有检查并入 `detect` / `policy`；
- **［用户裁定］数值阈值（T-10 A）**：先取保守默认值，fault injection 后调参；
- **拉取式失效 + Handle+Gen 值语义跨域引用**已定（Inline 域间；Inline↔Isolated 不传引用，只传只读快照，见"核间通信协议"条）；
- **正确性四件套**（Park-and-Ack 静默协议 / InitCtx 受限 token / 裁决页表自笼头 / 二级 watchdog）已定，见"正确性补全协议"章；
- **两拍重建**（结构恢复 µs 级同步 + 设备复位后台异步；结构段 ≤50µs、整体 ≤100µs 硬顶）已定；
- **故障注入首选模型 = livelock + watchdog NMI**（全链路一次性覆盖：不可屏蔽检测 → park-and-ack → 变身 → 重建 → 拉取式失效 → 归还），验收以 100% 不变式而非恢复率计——初步解，见"故障注入实证"章。

### 议题状态（已闭合）

- **Isolated 层内部结构** → 已收窄：裁决逻辑下沉 leaf crate `kernel-freg`（见"FREG 语义 crate（方案 B）"章），执行权留 privileged；crate 规模不以行数为承诺（［用户裁定］T-12 A），"小到可穷尽审查"的性质由"零依赖 + `forbid(unsafe_code)` + 纯函数化"承担；
- **被变身核持锁时的裁决纪律** → 已闭环：控制面全原子化（裁决路径这辈子不拿锁）+ Park-and-Ack 静默协议（见"正确性补全协议"章）；
- **并发变身的主导-协作仲裁** → 已闭环：单核 CAS 抢占 `arbiter_claim`，wait-free；单机广播事件最多一个赢家，无需比 CAS 更强的共识；
- **核间通信协议** → 已闭环：不建通道——Inline 层写原子槽位 / seq 快照，Isolated 层只读采集为 `ArbiterView`；
- **实现期细节（不再进裁定表）**：事件源降级的工程细节（无 NMI / FIQ 时最高优先级 IRQ + IPI 的可靠性边界）、变身上下文保存区的具体实现。

## 对照参照：MINIX 3 进程重生与双层 FREG 映射

本章补齐 MINIX 3 的逐维对照。此前的 MINIX 内容散在"理论基础：两个先例与机制替代"与"副作用一致性·与 MINIX 的持平声明"两处，本章不重复那些论证，只把**地址空间、重建动作、失效传播**三件事按双层结构对齐。

### MINIX 3 的事实基线

以下为公开资料的概括，**未在本仓核对**（本仓没有 MINIX 源码或实验环境）：

- **微内核 + 用户态服务**：驱动、文件系统、网络等服务运行在用户态独立进程；微内核只保留 IPC、调度、内存等最小机制，自身受信任（本设计多处引用的口径是约 6K 行）。
- **RS（Reincarnation Server）**：用户态服务管理器，监控服务状态并负责重启；服务可登记依赖，RS 可按依赖顺序重启（具体策略随版本）。
- **DS（Data Store）**：用户态键值存储服务，供服务保存配置 / 状态。重启后的状态恢复靠服务重新读取 DS、设备或对端协议，**不是内核把旧进程的内存灌回新进程**。
- **IPC 端点**：服务通过 endpoint 标识通信；endpoint 含进程槽位与代际信息，槽位复用后旧 endpoint 失效（具体位布局随版本）。
- **失效表现**：目标服务死亡时，发往该 endpoint 的调用收到 `EDEADSRC` 一类错误；调用方必须重连、重查或向上返回错误。
- **停摆范围**：一次服务重启只影响该服务及其直接调用方，其余服务继续运行。RS / DS / 微内核自身不在"可重生"集合内——它们是恢复基础设施。

> 可靠性分级：“驱动在用户态”“RS 负责重启”“服务死亡使调用方收到 dead-source 错误”属高置信公开事实；“endpoint 含代际”“DS 用于状态恢复”“依赖顺序重启”的细节随 MINIX 版本而异，本仓无法逐条核对。后一类在本章只作对照参照，不作为 FREG 设计的证据链。

### 逐维对照

| 维度 | MINIX 3 进程重生 | FREG Inline 层 | FREG Isolated 层 |
|---|---|---|---|
| 位置 | 服务进程在用户态独立地址空间；RS 在另一进程 | 域胶囊在内核共享地址空间 | 裁决核心在独立 recovery 地址空间（平时不映射） |
| 隔离来源 | MMU 地址空间隔离（硬件） | 语言 / 所有权 / 模块可见性（编译期）+ Handle+gen 值语义 | 页表切换（CR3 / TTBR1）+ 最小映射面（硬件） |
| 失效触发 | 进程退出 / RS 心跳失联 | panic / IDT 异常 / 心跳 / 配额（本核或近端检测） | NMI / FIQ / watchdog / IPI 远程发现（含关中断死循环） |
| 重建动作 | 拆进程（AS / 线程 / 栈 / 端点）→ spawn → execve → 重 init | memset capsule + `new()` + gen++；不建进程、不换 AS | 与 Inline 重建动作相同，但由域外裁决核执行；含 park-and-ack |
| 状态来源 | 新进程重新初始化；从 DS / 设备 / 对端协议重建 | `new()` 从持久层 + 跨代账本（BCB / 硬件环）+ 对端协议重建（R1 / R5） | 同 Inline；裁决者只读 `ArbiterView` 决定是否重建 |
| 陈旧引用处理 | endpoint 失效，调用方收 `EDEADSRC`，重连 / 重查 | `Handle{id,gen}` 值语义；lookup 校验 gen，不匹配 `Err(StaleGeneration)` | gen++ 后不推送；消费者拉取式发现失效 |
| 停摆范围 | 该服务及其直接调用方；其余服务继续 | 该域（结构第一拍 µs 级）；其余域不停 | 多核：被 park 的核短暂停，其余域不停；单核：全系统冻结几十 µs |
| 调用方容忍机制 | 处理 `EDEADSRC`，重试 / 重连 / 降级 | `Err(StaleGeneration)` + deadline；不阻塞等死域 | 同 Inline；裁决期间可能看到短暂 `Deferred` |

### 层间映射：角色对应，机制不同构

**角色映射**：MINIX 的重生由两段组成——RS 在故障进程之外决定并启动重启，重生侧执行"把对象重新初始化"。FREG 的双层结构正好拆成对应的两段：

- **Isolated 裁决段 ≈ MINIX 的 RS 侧**：都在故障对象之外，决定"是否重启、何时重启"，并拥有独立的执行环境（MINIX 是独立进程的 AS，FREG 是独立 recovery 页表）；
- **Inline 重建段 ≈ MINIX 的重生侧**：都执行"把对象重新初始化"的动作。

**机制不同构**：MINIX 的重生侧是"拆旧进程 + 新建独立 AS + spawn/execve + 重 init"；FREG 的重建侧是"原 capsule buffer 上 `memset + new() + gen++`"，**不新建 AS、不换页表、不重建进程**。Inline 重建段在角色上对应 MINIX 的重生侧，在机制上没有 MINIX 对应物——这是 FREG 独有的形态，也是"零锁、零分配、零 TCB 调用"与"亚进程级粒度"的来源。

> ［用户裁定］FREG **不采用 MINIX 的整进程换新地址空间**：被恢复域不新建独立 AS，只有裁决核心持有独立 recovery 页表根。这条裁定关闭了"每个故障域一份页表 / Isolated 层为域新建 AS"的替代方向。

### 取舍：选 A 的代价

选"维持半单地址空间"不是没有代价，如实记录三条：

- **失去地址空间级的陈旧引用撤销**。MINIX 换新 AS 后，旧进程的映射随地址空间拆除而自然失效；FREG 的被恢复域仍在原 AS，旧引用不会因页表拆除而失效，**只能靠 gen + Handle 的代际校验**来拒绝。gen 校验是软件路径，漏检一处就是一个悬挂引用。
- **失去"整进程全清"的彻底性**。MINIX 的重生把栈、线程、锁、IPC 端点、地址空间全部拆掉，干净性由结构保证；FREG 靠"状态只经 capsule 可达"的不变式 + `memset` 清零逼近同等效果，**对逃逸引用、静态状态、硬件副作用不提供同等保证**。
- **换来的是零锁、零分配、零 TCB 调用与亚进程级粒度**：不换 AS、不 spawn/execve、不重建进程，才有"同空间调用 + µs 级重建"。这是取舍，不是无偿获益。

### 该学的四条教训

1. **失效是协议结果，不是内核事件。** MINIX 调用方收到 `EDEADSRC` 后重连 / 降级，而不是等内核 panic。落到本设计：跨域引用一律 `Handle{id,gen}`，失效返回 `Err(StaleGeneration)`（C6 / "全 create 化恢复模型"章的拉取式失效条）；所有跨域阻塞等待加 deadline（C3 第 3 点）。MINIX 的 `EDEADSRC` 是"已死端点"的显式结果；FREG 的 `StaleGeneration` 是"代不匹配"的显式结果——两者都把失效从"异常"降级为"可处理返回值"，这是调用方能继续运行的前提。
2. **重建者必须在故障对象之外。** MINIX 的 RS 不在服务进程内；FREG 的裁决核在独立 recovery 页表内，且该页表平时不映射。落到本设计：Isolated 层 + park-and-ack 静默协议（"正确性补全协议"章①）。只靠 Inline 层无法裁决"关中断死循环"的核——这正是 Isolated 路径存在的理由。
3. **状态恢复靠外部权威，不靠内存回灌。** MINIX 重启后由服务重新读 DS / 设备 / 对端协议；FREG 的 `new()` 只从持久层 + 跨代账本（BCB / 硬件环）+ 对端协议重建（R1 / R5）。这条纪律的直接产物是"权威账本不得住在可重生胶囊里"——它同时解释了为什么 MINIX 重启会丢全部内存态，而 FREG 敢承诺"已到达未处理不丢、已落盘可判定"。
4. **恢复基础设施自身必须在恢复范围之外。** MINIX 的 RS / DS / 微内核不参与"可重生"集合；FREG 的 TCB（privileged）是受信任基座，TCB bug → BHR 是可接受终态（"诚实边界"章）。这不是回避，而是任何恢复系统的共同边界：恢复者不能是自己要恢复的对象。

### MINIX 做不到而 FREG 想做的

- **内核内细粒度**：MINIX 的重生单位是进程，一次重启丢掉该进程的全部内存态；FREG 的恢复单位是域胶囊，可以只重建一个结构，跨代账本与硬件环跨代存活。
- **基础设施域可自修**：FREG 想把 EGFS / proc / net 作为可恢复域（PMM 已裁定降为 TCB 机制，T-03 A），用 BCB 影子账本 + 静态 buffer 降低重启代价；MINIX 的 RS 主要面向驱动与普通服务，PM / VFS / RS / DS 这类基础设施通常不按普通服务处理（据公开资料，未在本仓核对）。
- **零锁、零分配、零 TCB 调用**：MINIX 每次跨服务调用都要 IPC + 特权级 / 地址空间切换；FREG 同空间调用只有一次快照 `store` + gen-check（"性能论证与速率预算"章），这是"半单地址空间"换来的。
- **TCB 逻辑故障的第二道防线**：MINIX 微内核故障即整机故障；FREG 的 Isolated 层试图在 Inline FREG / TCB 逻辑故障时仍能执裁。但这条有诚实边界：TCB 内存损坏仍只保 BHR（"诚实边界"章），Isolated 层不是"TCB 任意损坏都能恢复"的承诺。

## 无条件执裁：裁决者不依赖"故障代码写没写对"

MINIX 对本设计的根本诘难是：MMU 不管你的代码写没写对，越界就是硬件拦截；FREG能否做到"不管代码写没写对，都能执裁"？答案分两层。

**遏制层：对 functions 而言，100% safe Rust 不比 MMU 弱，甚至更强。**

| 破坏性操作 | MINIX / MMU | functions 0 unsafe（编译器） |
|---|---|---|
| 越界读写 | 运行时 page fault 拦截 | 编译期拒绝，该代码不存在 |
| use-after-free | MMU + 引用计数 | 所有权系统编译期拒绝 |
| 数据竞态 | 进程隔离 + IPC 串行化 | Send/Sync + `&mut` 独占，编译期拒绝 |
| 触碰裁决者内存 | ring 特权级不可达 | 裁决者字段跨 crate 不可见、不可命名、不可调用 |

MMU 的保证是"你写错了运行时拦你"，Rust safe 代码的保证是"你写错了根本编译不出来"。对"functions 代码写没写对"这个命题，编译期不变式是严格更强的断言。**所以执裁不需要假设故障域配合**——其破坏力在编译期已被消灭。

**执裁层：残余的两个依赖，以及各自的消解方式。**

1. **裁决者自身不能有 bug** → 以"小到可穷尽审查"消解：FREG 语义整体下沉零依赖 leaf crate（见下章），`forbid(unsafe_code)`、无分配、无锁、纯函数化；规模不以行数为承诺，"小"由这三项性质定义，人工可按模块穷尽审查。MINIX 敢信任它的微内核，理由同构——不是魔法，是性质可穷尽；
2. **恢复不执行故障现场的析构与可变状态** → 以全 create 化（［用户裁定］T-01 B）消解（见再下章）：不跑 Drop、不 undo，裁决者的全部动作是"清零一块 buffer + 调一次 `new()` + 锁内改代"。`Domain::new()` 必须由 TCB 提供最小初始化器，域不得在其中执行业务逻辑（契约见"域注册表 vs 域目录"之后的"域注册契约"小节）；因此恢复路径上唯一执行的域侧代码是"字段置初值 + 寄存器 / 静态状态复位"级别的初始化，不触碰故障现场的析构与可变状态，也就无需假设故障域"写没写对"。

两个消解完成后，FREG执裁的依赖集收敛为：**编译器 + 硬件 + 一个零依赖语义 crate**——与 MINIX 的"硬件 + 微内核"假设数相同。真正的差距从来不在"functions 代码对不对"（编译器管了），而在"裁决者自己够不够小"——这一点由 crate 化 + 纯函数化自己掌控，不依赖任何外部资源。

## FREG 语义 crate（方案 B）：边界、接口与迁移

［用户裁定］FREG 按**整体 crate** 落地，采纳方案 B：新建零依赖 leaf crate `kernel-freg`，承载 FREG 的全部**语义**——状态机、域注册表不变量、仲裁、失败分类、策略矩阵、检测聚合、审计事件类型、对外 API 契约；全局状态、锁、`no_mangle` 符号、硬件装配全部留在 `kernel-privileged`，启动期以 `FregEnv` / `FregHw` 两个 trait 注入。本章写到"照着能开工"的颗粒度。

### 依赖图（含 glue 的位置）

```text
                    ┌──────────────────────────────┐
                    │  kernel（glue）              │
                    │  staticlib + rlib            │
                    │  启动编排 / panic 兜底 / 分配器 │
                    └───────┬───────────┬──────────┘
               pub use      │           │      pub use
                            ▼           ▼
             kernel-privileged      kernel-functions
              （TCB，unsafe）         （100% safe）
                            │           │
                            │           │ 无直接依赖边；
                            │           │ 仅经 kernel_privileged::freg 的
                            │           │ re-export 使用语义类型
                            │ path dep  │
                            ▼           │
             ┌──────────────────────────────────┐
             │  kernel-freg（leaf）             │
             │  no_std + forbid(unsafe_code)    │
             │  [dependencies] 为空              │
             └──────────────────────────────────┘
```

- 唯一边方向：`kernel-privileged → kernel-freg`。`kernel-freg` 的 `Cargo.toml` 没有 `[dependencies]`，编译器物理禁止它看见 privileged / functions / glue。
- `kernel-functions` 不直接依赖 `kernel-freg`：functions 只经 `kernel_privileged::freg` 的 re-export 使用语义类型，避免 `audit_functions_boundary.py` 的 `SAFE_PRIVILEGED_APIS` 白名单再开一个入口。
- `kernel` 仍是 glue，只 `pub use kernel_privileged as privileged;` / `pub use kernel_functions as functions;`，不新增 FREG 逻辑。
- **`forbid(unsafe_code)` 在 `kernel-freg`**；**`no_mangle` 在 `kernel-privileged`**。这切开了"零 unsafe 的编译期证明"与"唯一 FFI 符号壳"：`#[unsafe(no_mangle)]` 在 Edition 2024 是 unsafe 属性，只能留在允许 unsafe 的 TCB。

### crate 目录结构

```text
src/kernel/freg/
├── mod.rs            crate 根：no_std / forbid(unsafe_code) / deny(missing_docs) / 模块声明
├── state.rs          FregState / DomainEntry / DomainState / 派生健康度
├── registry.rs       注册、注销、依赖无环检查、槽位索引（纯数据操作）
├── arbiter.rs        仲裁纯函数：ArbiterView → Verdict
├── env.rs            FregEnv / FregHw trait + FregError
├── api.rs            对外唯一调用面（薄，不持状态）
├── detect.rs         检测聚合：DomainObservation → DetectionSummary（承接 health_monitor）
├── policy.rs         故障类型 × 域状态 × 动作矩阵（承接 recovery_policy）
├── cascade.rs        拓扑级联（承接 cascade）
├── audit.rs          AuditEvent / AuditRing（定长）
└── tests.rs          host 侧纯函数测试（#[cfg(test)]）
```

`[lib] name = "kernel_freg"`、`path = "mod.rs"`，与 P-71 的 `privileged/mod.rs` / `functions/mod.rs` 同形；`src/kernel/Cargo.toml` 的 `[workspace] members` 由 `["privileged"]` 改为 `["privileged", "freg"]`（`functions` 仍是 root package 的 path 依赖，由 cargo 自动纳入）。

### `FregState`：字段与定长化

```rust
// state.rs — 全部定长；无 alloc、无锁、无 unsafe
#[repr(C)]
pub struct FregState {
    pub abi: AbiHeader,                     // magic: u32 / version: u16 / layout: u16
    pub config: FregConfig,                 // 阈值、预算、上限
    pub slots: [DomainEntry; MAX_DOMAINS],  // MAX_DOMAINS = 32
    pub occupied: u32,                      // 槽位位图（32 槽恰好一 u32）
    pub next_gen: u64,                      // 全局代，只增
    pub adjudication: AdjudicationMode,     // 配置维度（Inline / Isolated）
    pub health: SystemHealth,               // 派生聚合量（Normal / Recovering / Degraded）；Degraded 不在顶层状态机，仅为"存在 Quarantined/Faulted 且无 Rebuilding"的派生聚合值
    pub stats: [DomainStats; MAX_DOMAINS],
    pub audit: AuditRing,                   // { events: [AuditEvent; 128], head: u16, len: u16 }
}

#[repr(C)]
pub struct DomainEntry {
    pub id: DomainId,                  // u32
    pub state: DomainState,            // repr(u8) 七态
    pub phase: RebuildPhase,           // repr(u8)：Structural | Device，仅 Rebuilding 有效（T-11 A）
    pub gen: u64,
    pub severity: Severity,            // repr(u8)
    pub tier: CapTier,                 // repr(u8)
    pub retries: u8,
    pub max_retries: u8,               // 默认 5，沿用 MAX_CONSECUTIVE_FAILURES
    pub flags: DomainFlags,            // u16：TCB 边界 / 可隔离 / 有外部权威
    pub quota: QuotaWindow,            // 4 × u64
    pub heartbeat: HeartbeatWindow,    // 2 × u64
    pub deps: [DomainId; MAX_DEPS],    // MAX_DEPS = 8
    pub dep_len: u8,
    pub init_kind: InitKind,           // repr(u8)：仅枚举，不持函数指针
}

#[repr(u8)]
pub enum RebuildPhase {
    Structural = 0,  // 结构已恢复（memset + new() + gen++ 完成）
    Device = 1,      // 设备复位后台进行中
}
```

定长化方案（逐条都是硬约束）：

- **无堆**：crate 不依赖 `alloc`，全部用 `[T; N]` 与定长枚举；`DomainId = u32`、`Handle { id: u32, gen: u64 }`。
- **枚举窄化**：`DomainState` / `RebuildPhase` / `Severity` / `AdjudicationMode` / `SystemHealth` / `InitKind` 全部 `#[repr(u8)]`，布局可预测。
- **容量沿用现有上限**：`MAX_DOMAINS = 32` 对齐 [types.rs:9](src/kernel/privileged/freg/types.rs) 的 `MAX_RECOVERY_DOMAINS`；`MAX_DEPS = 8` 对齐 [types.rs:11](src/kernel/privileged/freg/types.rs) 的 `MAX_DOMAIN_DEPENDENCIES`。
- **总预算**：`const _: () = assert!(core::mem::size_of::<FregState>() <= 12 * 1024);`。超预算时先缩 `AuditRing`，不缩域数——域数是语义上限，环只是审计深度。
- **无函数指针**：`DomainEntry` 不含 `fn()`；`init_kind` 是枚举，实际 `init_fn` 只活在 privileged 的 capsule 池表里，经 `FregHw` 调用。
- **环形审计**：`AuditEvent` 只含 `u64` / `u32` / `u8` 与枚举，不含 `String`；格式化在 privileged / functions 的**非恢复路径**做。

### `FregEnv`：每方法签名与失败语义

```rust
pub trait FregEnv {
    /// 当前 tick。未注册 tick 源时返回 0（与 tick_query::current_tick 的既有语义一致）。
    fn now_ticks(&self) -> u64;
    /// 本核编号。不可知时返回 u16::MAX。
    fn cpu_id(&self) -> u16;
    /// 在线核数。不可知时返回 1。
    fn cpu_count(&self) -> u16;
    /// panic PC → 域。未命中返回 None（调用方按 TCB 故障处理）。
    fn lookup_by_pc(&self, pc: usize) -> Option<DomainId>;
    /// 域的代码范围。未登记返回 None。
    fn code_range(&self, id: DomainId) -> Option<CodeRange>;
    /// 审计日志。best-effort：不得分配、不得取共享锁、不得 panic；失败静默丢弃。
    fn log(&self, event: &AuditEvent);
    /// 发布 Inline 快照到原子槽位。槽位不可写返回 Err(EnvUnavailable)。
    fn publish_view(&self, view: &ArbiterView) -> Result<(), FregError>;
}
```

失败语义的选择原则：**读类不返回 `Result`**（用 0 / `None` / 哨兵表达"不可用"，避免恢复路径上多一层分支）；**写类返回 `Result`**（失败必须让调用方决定降级还是 BHR）。`log` 是唯一允许"静默失败"的方法——日志不能成为恢复路径的故障源。

### `FregHw`：每方法签名与失败语义

```rust
pub trait FregHw {
    /// 清零该域的 capsule buffer。未注册 → Err(NotRegistered)；buffer 不可达 → Err(EnvUnavailable)。
    fn zero_capsule(&self, id: DomainId) -> Result<(), FregError>;
    /// 切入预置 recovery 页表（x86 CR3+PCID / aarch64 TTBR1+ASID）。
    fn enter_recovery_space(&self, id: DomainId) -> Result<(), FregError>;
    /// 切回原地址空间。失败 → Err(HwFailure)，调用方走 BHR。
    fn leave_recovery_space(&self, id: DomainId) -> Result<(), FregError>;
    /// 第二拍设备复位（后台）。失败 → Err(HwFailure)，域保持 `Rebuilding(phase = Device)`。
    fn reset_device(&self, id: DomainId) -> Result<(), FregError>;
    /// 定向 park IPI。核不在线 → Err(NotRegistered)。
    fn send_park_ipi(&self, cpu: u16, id: DomainId) -> Result<(), FregError>;
    /// 有界等待 park_ack。超预算 → Err(Timeout)。
    fn wait_park_ack(&self, cpu: u16, budget_ticks: u32) -> Result<(), FregError>;
    /// 硬复位（BHR）。不返回。
    fn hard_reset(&self, reason: ResetReason) -> !;
    /// 内存屏障（跨核可见性）。
    fn fence(&self);
}
```

`FregHw` 的每个方法在 privileged 侧都含 unsafe（memset / 页表寄存器 / IPI / MMIO），但 trait 本身是 safe 的——unsafe 被封装在实现里，`kernel-freg` 侧看到的是一组可能失败的 safe 调用。这就是"unsafe 留 TCB、语义进 leaf"的具体切法。

### privileged 侧四文件职责

| 文件 | 职责 | 明确不做 |
|---|---|---|
| `state.rs` | `static FREG_STATE: FregCell<FregState>`；`try_lock` 守卫；每核 recovery 栈；`arbiter_claim` 的 `AtomicU32` 抢占；`#[repr(C)]` 布局 | 不做状态迁移判断、不做策略选择 |
| `env.rs` | `struct KernelEnv; impl FregEnv`：`now_ticks` 转 `tick_query::current_tick`、`cpu_id` 转 SMP 编号、`lookup_by_pc` 转既有地址范围表、`log` 转 `klog_*!`、`publish_view` 写原子槽位 | 不持有语义状态 |
| `hw.rs` | `struct KernelHw; impl FregHw`：memset、CR3/TTBR1 切换、IPI、设备复位、BHR；每个 unsafe 块配 `// SAFETY:` | 不做仲裁 |
| `api.rs` | 唯一 `no_mangle` 面：`recovery_try_recover_from_idt` / `recovery_register_domain`；参数转换 + 调 `kernel_freg::api::*` + 错误码回填。`recovery_freg_maintenance` 按 T-05 B 删除，独有检查先并入 `detect` / `policy` | 不写业务分支 |

### panic 壳与 `try_lock` 失败分支

```rust
#[unsafe(no_mangle)]
pub extern "C" fn recovery_try_recover_from_idt() -> i32 {
    // 旧契约保留：重入 / 递归 panic 由 RECOVERY_ATTEMPTED 直接拒绝（api.rs:138-140）。
    if RECOVERY_ATTEMPTED.swap(true, Ordering::SeqCst) {
        return -2;
    }
    let Some(mut guard) = FREG_STATE.try_lock() else {
        return crate::privileged::freg::state::on_lock_contended();
    };
    match kernel_freg::api::on_panic(&mut guard, &KERNEL_ENV, &KERNEL_HW) {
        Ok(RecoveryOutcome::Recovered) => 0,
        Ok(RecoveryOutcome::Deferred) => 0,       // 第二拍后台，结构恢复已完成
        Err(FregError::NotRegistered) => -1,      // 旧契约：无可恢复域，IDT 侧继续 BSR
        Err(FregError::IllegalTransition) => -4,  // 新增：状态机拒绝
        Err(_) => -1,
    }
}
```

FFI 错误码沿用 [api.rs:133](src/kernel/privileged/freg/api.rs) 的既有契约并**只增不改**：`0 = 成功`、`-1 = 无可恢复域`、`-2 = 已尝试（重入）`、`-3 = 管理器忙`、`-4 = 状态机拒绝（新增）`。IDT 侧对任何负值都走 `kernel_panic`（[table.rs:742-750](src/kernel/privileged/idt/table.rs)）；若要区分 `-4` 直接 BHR，只需在 IDT 侧加一个分支，不改旧码语义。

`try_lock` 失败按三种情形分流（`on_lock_contended` 的全部逻辑）：

1. **另一核在裁决中**：本核不是赢家。有界轮询 `next_gen`（预算 `PARK_ACK_BUDGET_TICKS`），观察到代前进 → 返回 0（视作已被恢复）；预算耗尽 → `hard_reset(ResetReason::ArbiterStalled)`。**不等待锁**——持锁者可能正是故障核。
2. **本核重入**：由 `RECOVERY_ATTEMPTED` 在进入锁之前拦下，返回 `-2`（保持旧契约），IDT 侧按负值走 BSR/BHR。
3. **锁状态不可判定**：按情形 1 处理，预算后 BHR。三种情形都不做无界自旋。

`IrqSpinLock::try_lock` 已在 [irq_spinlock.rs:232](src/kernel/privileged/sync/irq_spinlock.rs) 存在；`lock()` 的关中断路径 [:171](src/kernel/privileged/sync/irq_spinlock.rs) 不在恢复路径使用。

### 六步迁移：改动面与验证判据

| 步 | 改动面 | 验证判据 | 回滚 |
|---|---|---|---|
| **S1 纯类型外移** | 新建 `src/kernel/freg/`（`mod.rs` / `state.rs` / `env.rs` / `audit.rs`）；把 `types.rs` 的纯类型、`recovery_policy.rs`、`cascade.rs` 移入（`UndoEntry` 不搬，留待 S4 删除）；`Cargo.toml` 加 workspace 成员与 path 依赖；调用方路径改 re-export | 六门槛全绿；**零行为变化**（纯移动 + re-export） | 单 commit revert |
| **S2 定长 state + 纯状态机** | 新增 `registry.rs` / `arbiter.rs` / `detect.rs` / `policy.rs`；privileged 新增 `state.rs` 持有 `FregState`；旧 `RECOVERY_MANAGER` 保持权威（shadow 双跑） | 状态迁移表 unit test 覆盖全部合法/非法边；shadow 对比无差异；`make test-kernel-host` | 删除新模块，旧路径不动 |
| **S3 env/hw 注入 + 单点切流** | 新增 `KernelEnv` / `KernelHw`；`recovery_try_recover_from_idt` 改走 `kernel_freg`，由 `feature = "freg"` 门控 | `KERNEL_FEATURES=freg` 真构建 + QEMU 真启动（P-62）；panic 注入用例；feature 关闭时旧路径仍绿 | 关闭 feature 即回旧路径 |
| **S4 删 undo 时代文件** | 删 `undo_log.rs`；剥 `types.rs` 的 `UndoEntry`；确认 `recoverable.rs` / `audit_export.rs` / `reset_config.rs` 已在 S1–S3 删除；更新审计基线 | `audit_dead_code_comment.py` 零新增；全仓零引用；六门槛全绿 | 单 commit revert |
| **S5 functions 收口** | `attribution.rs` 拆分（纯范围判定进 crate，SGEG 能力降级留 functions）；`health_monitor.rs` 由 `detect.rs` 承接后删除；`functions/freg/mod.rs` 退为薄 re-export | `audit_functions_boundary.py` 白名单同步；host-tests 全绿 | 按文件 revert |
| **S6 审计/CI 登记** | 见下表 | `bash ci/audit.sh full` 全绿；fmt / lockbud / 文档债册一致 | 脚本改动可独立 revert |

**S4 的前置条件已成立**：T-01 B（create 化）已由用户裁定，S4 不再待命；`undo_log.rs` / `recoverable.rs` 的删除不再依赖未决项。

### 审计脚本需要登记的改动点（逐脚本）

| 脚本 / 文件 | 行 | 改动 | 原因 |
|---|---|---|---|
| `src/kernel/Cargo.toml` | 98 | `members = ["privileged"]` → `["privileged", "freg"]` | 新成员纳入 `--workspace` |
| `src/kernel/privileged/Cargo.toml` | 17–19 | 在 `[dependencies]` 加 `kernel-freg = { path = "../freg" }` | 唯一依赖边 |
| `ci/audit.sh` | 563 | 不改（`--workspace` 自动纳入新成员） | 双架构 clippy |
| `ci/audit.sh` | 670 / 673 | fmt 命令加 `-p kernel-freg`（错误提示同步） | fmt 维显式点名三成员，需变四成员 |
| `ci/audit.sh` | 859 / 877 | `LOCKBUD_CRATES` 加 `kernel-freg` | lockbud 显式 crate 清单 |
| `scripts/audit_tcb_ratio.py` | 18–19 | 加 `FREG = BASE / 'src' / 'kernel' / 'freg'` | 新树必须进统计，否则分子分母同减、占比失真 |
| `scripts/audit_tcb_ratio.py` | 153–154 | `total_loc = fw_loc + sv_loc + freg_loc`；`freg` 不计入分子 | 新 crate 零 unsafe，属非 TCB 侧 |
| `scripts/audit_coupling.py` | 51–52 | 加 `FREG_BASE = Path('src/kernel/freg')` | 第三棵树纳入耦合扫描 |
| `scripts/audit_coupling.py` | 624 / 636 / 651 / 688 | 扫描与基线循环加 `freg` 标签 | 同上 |
| `scripts/audit_missing_docs_debt.py` | 70 | `PACKAGES` 加 `"kernel-freg"` | rustc `missing_docs` 取数覆盖新成员 |
| `scripts/audit_missing_docs_debt.py` | 239 | 基线口径文本改"四成员" | 与 `PACKAGES` 同步 |
| `scripts/audit_dead_code_comment.py` | 53 | 不改（`src/kernel` 已覆盖） | 新 crate 自动进 F9 射程 |
| `scripts/audit_implicit_deps.baseline.json` | 35 | 删 `src/kernel/functions/freg/audit_export.rs` 条目（`--update-baseline`） | 文件被删，基线须收缩 |
| `scripts/audit_functions_boundary.py` | 344–345 | 删 `reset_config.rs` 的豁免行 | 文件被删 |
| `scripts/audit_safety_coverage.py` | 19 | 不改（根仍是 `privileged`） | 新 crate 零 unsafe，由 `forbid` 证明 |
| `scripts/audit_public_api_docs.py` | 49 | 不改（F8 语言面只判 `privileged/`） | 新 crate 用 `#![deny(missing_docs)]` 自证 |
| `scripts/audit_once_cell.py` | 32–36 / 59 | 不改（`src/kernel` 根已覆盖新 crate） | 四树扫描自动纳入 |
| `scripts/audit_deadlock_matrix.py` | 30–31 | 不改（crate 无锁） | 纯函数 crate |
| `scripts/audit_feature_semantics.py` | 37–38 | 不改（crate 无 feature） | 避免为审计加特征位 |

**编译期证明的落点**：`kernel-freg` 的 crate 根写 `#![no_std]` + `#![forbid(unsafe_code)]` + `#![deny(missing_docs)]`；`kernel-privileged` 保留其既有的 `deny + 点白名单` 形态（它本来就要 unsafe），`no_mangle` 全部集中在 `api.rs`。这样"零 unsafe"由编译器证明，"符号壳"由 TCB 承担，两者不再混在一个 crate 里。

> **溯源说明**：本章的总体边界（整体 crate 拆分、全局状态/锁/符号/硬件留 privileged、env/hw 注入）是［用户裁定］；其下的接口签名、定长预算、六步切分与审计登记点属执行方设计推演（［研讨结论］），在 S1 骨架评审时仍需确认，不得当作已定实现规格。

## 既有实现的针对性修剪清单

［用户裁定］四个重点文件按 v2 设计路径做**针对性修剪 / 删除**，不是一律删。判据不是"当前是否零调用"，而是"v2 是否有对应需求、需求落在哪个新层"。下表覆盖四个重点文件与此前判定为删除 / 合并的全部条目；"改写进 v2"的条目给出新落点。行数为实测（`wc -l`）。

### 四个重点文件

| 文件 / 片段 | v2 是否有对应需求 | 处置 | 前置条件 | 改写落点 |
|---|---|---|---|---|
| [health_monitor.rs](src/kernel/functions/freg/health_monitor.rs)（298 行，全仓零调用） | **有**："检测聚合 → 策略决策"是 v2 的检测面（`DomainHealth → FaultSignal → RecoveryPolicy::decide`，:24-138；检测源见"检测源"表） | **改写进 v2**，不保留原文件 | 无（可先写新实现再删旧文件） | `kernel-freg/src/detect.rs`：无状态纯函数 `aggregate(&[DomainObservation; N], &FregConfig) -> [DetectionSummary; N]`。`DomainObservation { id, last_tick, failures, tier, quota_exceeded }` 由 `on_tick` 从 `FregState.stats` 填充；聚合结果交 `policy.rs::decide`。原 `HealthMonitor` 的 `last_check_tick` / `consecutive_failures` 上移为 `FregState.stats`，不再是独立结构；`to_fault_signal` 折叠进 `DetectionSummary.severity`；容量统一到 `MAX_DOMAINS = 32` |
| [audit_export.rs](src/kernel/functions/freg/audit_export.rs)（264 行） | **无**：导出的是 undo 时代的 `ROLLBACK_LOG`（:16），v2 审计是 `AuditEvent` / `AuditRing` | **立即删** | privileged 侧 `ROLLBACK_LOG` 先移除或确认无其他消费者 | 无；审计格式化留在 privileged / functions 的非恢复路径 |
| [reset_config.rs](src/kernel/functions/freg/reset_config.rs)（实测 **12 行**） | **无独立需求**：全文只有 `pub use crate::privileged::freg::reset::config::*;`（:12） | **立即删** | 确认零外部引用（实测零外部引用；`privileged/tests/reset.rs:9/16/22` 直接走 privileged 路径） | 常量/类型进 `kernel-freg` 的 `FregConfig`；7 个静态原子（[config.rs:144-156](src/kernel/privileged/freg/reset/config.rs)）进 `privileged/freg/state.rs` |
| [recoverable.rs](src/kernel/privileged/freg/recoverable.rs)（83 行） | **无**：`RecoverableMutex` 全仓 0 使用（:51-52 两处 `unsafe impl`） | **立即删**（不依赖 create 化裁定） | 无 | 无 |

### 此前判定为删除 / 合并的其它条目

| 文件 / 片段 | v2 是否有对应需求 | 处置 | 前置条件 | 改写落点 |
|---|---|---|---|---|
| [undo_log.rs](src/kernel/privileged/freg/undo_log.rs)（252 行） | **无**：undo 兜底已随 T-01 B 废弃 | **S4 删除** | ［用户裁定］T-01 B 已成立 | 无；裸指针读写（:210-251）不进 crate |
| [types.rs](src/kernel/privileged/freg/types.rs)（127 行） | 状态/常量有；`UndoEntry` 无 | **拆分** | 无 | `DomainState` / `MAX_*` → `state.rs`；`RollbackEvent` → `audit.rs`；`UndoEntry`（:94-95）删 |
| [domain.rs](src/kernel/privileged/freg/domain.rs)（415 行） | 有（域元数据、配额、心跳） | **拆分** | 无 | 元数据 / 配额 / 心跳 → `DomainEntry` + `DomainStats`；`IrqSpinLock<Option<unsafe fn()>>`（:61-64）与 unsafe 调用 → `privileged/freg/slot.rs`；`cpu_quota_max` / `consume_quota_tick` / `check_quota` / `reset_quota`（:42-48 / :281-289 / :354-368）变 `FregState` 上的纯函数 |
| [manager.rs](src/kernel/privileged/freg/manager.rs)（413 行） | 有（注册、查找、拓扑） | **拆分** | 无 | 注册 / 查找 / 无环检查 → `registry.rs`；`unsafe { f() }`（:398-407）与静态 `RECOVERY_MANAGER`（`mod.rs:109-121`）→ privileged |
| [recovery.rs](src/kernel/privileged/freg/recovery.rs)（314 行） | 有（恢复编排、级联可达性） | **拆分** | 无 | 图可达性纯算法（`alloc::vec!` :201-228 改定长）→ `registry.rs` / `cascade.rs`；`RECOVERY_REGISTRY.lock()`（:243/:253）与 `unsafe fn()`（:293-313）→ privileged |
| [snapshot.rs](src/kernel/privileged/freg/snapshot.rs)（424 行） | 有（设备快照，第二拍） | **保留并改写** | 第二拍设备复位 落地 | 快照机制留 privileged `hw.rs`；元数据类型进 crate 可选的 `DeviceSnapshotMeta` |
| [degrade_policy.rs](src/kernel/privileged/freg/degrade_policy.rs)（162 行） | 有（能力降级策略） | **拆分** | 无 | `OnceLock<&'static dyn FREGDegradePolicy>` 留 privileged；策略判定类型 → `policy.rs` |
| [fault_inject.rs](src/kernel/privileged/freg/fault_inject.rs)（25 行） | 有（故障注入实证） | **保留** | 无 | privileged（`panic!` 注入是 feature gate，不进 leaf crate） |
| [api.rs](src/kernel/privileged/freg/api.rs)（335 行） | 有（唯一 FFI 面） | **改写** | S3 切流完成 | 保留 `#[unsafe(no_mangle)]` + `try_lock`；语义调 crate；`recovery_freg_maintenance`（:52-62，全仓零调用）**删除**（［用户裁定］T-05 B），删除前核对函数体，独有检查并入 `detect` / `policy` |
| [mod.rs](src/kernel/privileged/freg/mod.rs)（128 行） | 有（静态全局 + wiring） | **改写** | S1 | privileged 壳；语义类型 re-export 自 `kernel-freg` |
| [reset/config.rs](src/kernel/privileged/freg/reset/config.rs)（261 行） | 有（配置与恢复层级） | **拆分** | 无 | 常量/类型 → `FregConfig`；7 个静态原子（:144-156）→ `privileged/freg/state.rs` |
| [reset/audit.rs](src/kernel/privileged/freg/reset/audit.rs)（213 行） | 有（审计事件） | **拆分** | 无 | 事件类型 → `audit.rs`；ring buffer 与 tick 读取留 privileged |
| [reset/bbr.rs](src/kernel/privileged/freg/reset/bbr.rs)（163 行） | 有（Path1 执行） | **改写** | S3 | 裁决在 `policy.rs` / `arbiter.rs`；执行机制留 privileged |
| [reset/bsr.rs](src/kernel/privileged/freg/reset/bsr.rs)（159 行） | 有（Path2 执行） | **改写** | S3 | 同上 |
| [reset/bhr.rs](src/kernel/privileged/freg/reset/bhr.rs)（116 行） | 有（最后手段） | **保留** | 无 | privileged（`outb` / `halt` / `lidt` / `int 3` 的硬件性不可下沉，`forbid` 冲突即源于此） |
| [reset/parallel.rs](src/kernel/privileged/freg/reset/parallel.rs)（274 行） | 有（并发编排） | **拆分** | 无 | 并发状态机 → `arbiter.rs`；IPI / 核同步 → privileged `hw.rs` |
| [reset/layered.rs](src/kernel/privileged/freg/reset/layered.rs)（136 行） | 有（层级选择） | **拆分** | 无 | 层级选择 → `policy.rs`；`unreachable_unchecked`（:33-36/77-80）留 privileged |
| [reset/mod.rs](src/kernel/privileged/freg/reset/mod.rs)（58 行） | 有（编排入口） | **改写** | S3 | privileged 编排入口 |
| [attribution.rs](src/kernel/functions/freg/attribution.rs)（552 行） | 有（panic PC 归属） | **拆分**（不整体迁移） | 无 | 纯 `panic_rip → 域` 判定 → `policy.rs::attribute_pc`；`functions/sgeg::policy` 依赖（:35）的能力降级留 functions |
| [recovery_policy.rs](src/kernel/functions/freg/recovery_policy.rs)（287 行） | 有（策略决策） | **迁移** | S1 | `policy.rs`；动作枚举重写为 v2 的 `Verdict` / `RecoveryAction` |
| [cascade.rs](src/kernel/functions/freg/cascade.rs)（385 行） | 有（拓扑级联） | **迁移** | S1 | `cascade.rs` |
| [mod.rs](src/kernel/functions/freg/mod.rs)（44 行） | 有（模块薄壳） | **改写** | S5 | 薄 re-export + 保留 capability degrade |

### 与 v2 的对应关系（汇总）

- **删除 4 个**：`recoverable.rs` / `audit_export.rs` / `reset_config.rs` 立即删；`undo_log.rs` 随 S4 删（［用户裁定］T-01 B，前置条件已成立）。
- **改写进 v2 1 个**：`health_monitor.rs` → `detect.rs`（检测聚合）与 `policy.rs`（策略决策）两层，不是保留原文件。
- **拆分 / 改写 12 个**：`types` / `domain` / `manager` / `recovery` / `degrade_policy` / `reset/config` / `reset/audit` / `reset/parallel` / `reset/layered` / `attribution` / `api` / `mod`。
- **迁移进 crate 3 个**：`recovery_policy` / `cascade` / `recovery.rs` 的纯图算法部分（并入 `registry.rs` / `cascade.rs`）。
- **原样留 privileged 2 个 + 机制保留 1 个**：`bhr` / `fault_inject`；`snapshot` 只保留机制部分。另有新建 `state` / `env` / `hw` / `api` 四个壳文件。
- **v2 新增模块**：`state.rs` / `registry.rs` / `arbiter.rs` / `env.rs` / `api.rs` / `detect.rs` / `policy.rs` / `audit.rs`（crate 侧），以及 privileged 侧 `state.rs` / `env.rs` / `hw.rs` / `api.rs` / `slot.rs`。

> **溯源说明**："按 v2 路径修剪而非按零调用删除"是［用户裁定］；上表逐文件的处置判断（拆分 / 迁移 / 保留参考）是执行方对 v2 语义落点的推演（［研讨结论］）。与 create 化绑定的条目已由［用户裁定］T-01 B 关闭（B 采用，`undo_log.rs` 删除）。

## 全 create 化恢复模型（已定）

［用户裁定］T-01 B：放弃修复式恢复（undo / Drop），恢复语义收敛为"整块清零 + 重新 create"。它与已废弃的"胶囊重建：确定性析构"路径共用"遏制 = 编译期不变式"，差异只在重建动作是否执行析构；本路径不执行析构。

```text
rebuild(id):                          ← privileged 执行；arbiter 只给 Verdict::Rebuild
    memset(CAPSULE_POOL[id], 0)       ← 静态 buffer 池，零分配
    fresh = (CAPSULE_POOL[id].init_fn)() ← 唯一入口 Domain::new()；init_fn 只活在 privileged 池表，不进 FregState
    bcb_release_external(id)          ← 代管资源（fd / slot）批量回收，不等域 Drop
    state.slots[id].gen = state.next_gen; state.next_gen += 1
                                       ← 锁内改代；这一刻 = 所有旧引用失效的时刻
```

四个支柱性质：

- **恢复范围不再需要论证**。恢复边界 = buffer 边界，由 privileged 静态分配时划定，不由代码行为决定。封闭定理的工程形态从"强制 C 类写路径登记"升级为更简单的结构——**整个 capsule 就是恢复单位**，不存在"漏登记"这个概念。
- **不跑 Drop**（abort without unwind）。故障域的析构器永不在裁决上下文执行，panic=abort 下的 Drop soundness 难题被移除出关键路径。
- **拉取式失效 + Handle+Gen**。裁决者 gen++ 后不通知任何消费者。跨域引用一律值语义 `Handle { id, gen }`（禁裸指针与长生命周期引用），消费者经 privileged lookup 时检查 gen，不匹配返回 `Err(StaleGeneration)`——safe Rust 的 match 穷尽性把"处理域重建"变成编译期义务，想漏都漏不掉。失效传播是惰性、按需、强制的，这是"拉取式"相对"推送式回调"的核心优势：裁决者不需要知道谁在引用故障域。
- **外部资源代管**。域不向外申请可变资源；fd 表、socket buffer 等由 privileged 经 BCB 代管，生命周期归 BCB 不归域 Drop，create 后可批量回收。

## 副作用一致性：三通道与设计规则（初步解）

对"整块清零 + 重新 create"的第一个严肃反驳是**数据错位**：胶囊的记忆回到了初始态，但外部世界已经前进了——已发送/接收的网络包、已写入磁盘的数据都不会跟着 gen++ 回滚。本节是初步研讨得到的解，有待 fault-injection 实证检验。基本立场先立住：

> **重建不解决数据错位，任何内核内恢复机制都不解决。全 create 化的进步不在于消灭错位，而在于不假装能消灭错位——把错位显式地关进三条各有标准解药的通道。undo 流派的问题恰是"日志回滚不了副作用"却暗示自己能回滚。**

### 三个通道分别算账

| 通道 | 错位形态 | 解药 | FREG承诺 |
|---|---|---|---|
| 已发送（不可逆） | 对端收到重复/半截交互；本端连接态随胶囊死亡 | 对端视角"胶囊重生 ≡ 网络分区"：超时重传、seq 去重、应用层重连——分布式协议的日常业务 | 不承诺 exactly-once，只承诺"故障表现归入协议已训练的等价类" |
| 已接收（真窗口） | 包已从硬件环取走、处理了、结果只存胶囊内存 → 重生即丢 | DMA 环 / RX 队列在 privileged 代管区**跨代存活**，新胶囊从 head/tail 当前值继续排空；"取走未下沉"窗口用延迟消费语义压到单事务原子性尺度 | 已到达未处理不丢；丢的窗口有界且等于正常并发问题量级 |
| 已落盘（可判定） | journal 半提交 / torn write / 在途块请求 | 磁盘状态机本来就有标准答案：FS 域的 new() 就是 mount + journal replay（重放已提交 / 丢弃未提交 / checksum 检出 torn 页）；在途 IO 靠第二拍设备复位 + 超时重试 | 落盘一致性由日志层担保，与不崩溃系统同构 |

不可幂等的对外动作（控制命令、执行器指令）内核救不了，只能靠应用层幂等键/序列号——这是 End-to-End 论证的教科书案例，不是FREG的缺陷，是分布式的物理事实。

### 设计规则：R1 落点分层 与 R5 完备性检验

> **R1（落点分层原则）**：已被外部世界确认的数据，其落点不得住在可重生的胶囊里。胶囊准入标准：**只住可再生的派生态**（缓存、索引、状态机）；权威账本住跨代存活层（journal / 硬件环 / BCB 代管区）。

> **R5（new() 完备性检验）**：若 `Domain::new()` 不能只从"持久层 + 跨代账本 + 硬件环 + 对端协议"重建出语义有效的运行态，则该域与外部世界的边界就划错了——它把不该住进胶囊的东西（账本）住进去了。审计一个域，只看它 new() 的输入来源清单：来源全是跨代层 → 错位免疫；缺了一块内存态 → 那块内存就是下一个 bug。

两条规则合起来，把"数据错位怎么办"从无穷发散问题收敛为**可检查的设计约束**：错位窗口不再靠枚举场景堵漏，而是被 R1 的边界划定直接排除在恢复范围之外。

### 与 MINIX 的持平声明

发送侧连接死亡、接收侧环形缓冲存活、磁盘侧 journal 重放——三通道上FREG与 MINIX 的解药完全同构（MINIX 的 INET server 重生同样不承诺 exactly-once，同样靠协议幂等与 journal）。差异在反方向：MINIX 的代价是"全有或全无"（整个 server 陪葬全部连接与缓冲），FREG靠 R1 落点控制 + 胶囊边界可把陪葬范围设计得小一到两个数量级。**在副作用一致性上FREG不声称超越 MINIX，声称的是：同样的外物（journal/硬件环/协议幂等），更小的牺牲半径。**

## 正确性补全协议：四件

全 create 化 + 独立裁决者模型在推演中暴露出四个正确性缺口，均以小机制钉死：

**① Park-and-Ack 静默协议（多核）**。Pause 清 run bit 只防"将来被调度"，防不了"正在执行的这个时间片"——若故障核仍在自旋读写 capsule，memset 就是数据竞争。协议：裁决核向故障核发定向 NMI → 故障核 park handler（十几行、0 unsafe：置位 + 自旋）置 `park_ack` → 裁决核有界等待（~100µs），等不到（连 NMI 都进不去 = TCB 级损坏）则降级 BHR；恢复完成后 IPI 唤醒 parked 核重定向到新入口。单核场景该协议天然退化掉：NMI 已物理抢占故障代码，静默自动成立。

**② InitCtx 受限 token**。`Domain::new()` 在关中断的恢复上下文运行，必须禁拿共享锁 / 禁分配 / 禁碰他域——不靠纪律靠类型：init_fn 只收 privileged 构造的 `&mut InitCtx<'_>`，token 只暴露"读 static 配置、写自己域的 slot 与 BCB 状态位"，不携带任何全局锁入口。**API 面即权限面**，new() 在恢复上下文死锁在结构上不可能。

**③ 裁决页表自笼头**。裁决页表只映射裁决必需的最小面：TCB text/rodata、本核 recovery 栈、串口 / local APIC、BCB 原子槽位区、故障域 capsule buffer（详见"L1 设计研讨·C4 上下文保存区"）。健康域内存不在其中：即使裁决逻辑里真藏了 bug，想碰健康域的内存 = page fault——**静默损坏变成响亮失败**。这里 MMU 不做遏制（遏制归编译器），做的是对裁决者自身错误的第二道防线。

**④ 二级 watchdog**。裁决流程自身也是一段会执行的程序：为其设第二级 watchdog 窗口，裁决超时（例如被 TCB 级损坏拖死）→ 硬复位。BHR 终态，语义仍然确定。

## 执行形态：单核与多核

"按需借核 + 变身 + 归还"的形态与 Intel SMM（SMI → 进 SMRAM 隔离空间 → 执行 → RSM 归还）同构——区别在本设计把这套硬件范式**用标配硬件能力在内核层重演**：NMI + CR3/TTBR0 切换 + 一套平时不映射的页表，不依赖任何固件或平台特权。这是"依赖标配、不依赖特殊硬件"立场的落地关键：需要的是硬件*能力*（标配），不是硬件*设施*（专属）。

| 场景 | 恢复路径 | 粒度与代价 |
|---|---|---|
| 多核（任意架构） | 借核变身裁决，故障核 park-and-ack | 外科手术；其余域全程不停 |
| 单核 x86_64 / aarch64 | NMI / FIQ 不可屏蔽 → 同核原地裁决，改写中断返回帧重定向新入口 | 外科手术；全系统冻结数十 µs（结构段 ≤50µs、整体 ≤100µs 硬顶） |
| 单核 RISC-V 低端（无可屏蔽中断） | 无入场机会 → watchdog 硬复位 | 整机冷启动；create 化保证重启后重建语义确定，配合日志文件系统恢复快 |

## 性能论证与速率预算

**热路径**：**零锁、零分配、零 TCB 调用**——所有权是编译期检查，跨域调用不进任何锁、不做堆分配、不调 TCB 入口。实际成本是一次快照 `store(Release)` ≈ 10-20ns + lookup 的 gen-check ≈ 1-5ns，合计 **<30ns**——约等于一次函数调用加一条分支。附注：所谓"零开销"指上述三类开销为零，不是字面上的 0 ns；唯一记账就是这一次快照 store + gen-check。对照 MINIX 跨服务 IPC 往返 1-5µs，**快 50-100 倍**。

**机制本体**（多核借核裁决，不含域自身 new()）：

| 阶段 | 延迟 | 主要成本 |
|---|---|---|
| park 握手 | ~0.3-1µs | 一次跨核缓存行转移 |
| 变身（CR3 切换 + 冷启动） | 1-5µs | TLB refill / ICache 冷 |
| arbitrate 纯函数 | 1-3µs | 读若干原子槽位（远端脏行每个 ~200ns） |
| memset buffer | 1-10µs | 4KB≈1µs，64KB≈10µs |
| BCB 回收 + gen++ + 唤醒 | ~2µs | 原子批量 + 一次 IPI |
| **合计** | **~10-20µs** | 大变量只剩 new() 本身 |

**端到端 MTTR**：panic / 不变式类故障 = 检测 <1µs + 机制 ~20µs + new()，合计几十~百 µs 量级；livelock 类 = watchdog 策略窗口主导（10ms-1s 可调），机制成本是零头；驱动域设备复位（ms~s，NVMe / PHY 有规格书下限）是 MINIX / Linux 同样要付的物理成本，FREG不在此维度作弊。

**横比**：

| 系统 | 一次服务级故障恢复 | 恢复粒度 |
|---|---|---|
| Linux panic | 秒~分钟（整机重启 + fsck） | 整机 |
| MINIX 3 重生 | ~1-10ms（拆进程 + spawn + execve + 重 init） | 进程级，全状态丢失 |
| FREG（目标态） | 计算域 ~10-100µs；驱动域 ms 级（贴硬件下限） | 亚进程级，可只重建单个结构 |

比 MINIX 快 1-2 个数量级的来源很朴素：MINIX 重生要**造一个新进程**（地址空间构建 + ELF 装载 + 调度入队），FREG重建只是 memset 一块早就在那的静态 buffer + 调一次 new()。

**两拍重建（world-stop 双层承诺）**：rebuild 拆两拍——第一拍在裁决上下文内只做结构部分（memset + new() 结构初始化 + gen++，µs 级），域以 `Rebuilding(phase = Device)` 上线；第二拍设备复位序列在后台 work queue 跑完，原子置 `Healthy`（消费者 gen-check 顺带查 health 位）。由此结构段 `t_entry → t_rebuilt` ≤50µs（能力档内），整体 ≤100µs 为硬顶；单核场景也不被 ms 级设备重置拖成长冻结。

**内核内 IPC**：无硬件切换（无 syscall / 异常），低于真微内核的跨组件 IPC 成本。

## 故障注入实证：模型选择与验收标准（初步解）

"fault-injection 实证恢复语义成立"是三条先河前提中唯一可由单人 + QEMU + 零外部许可闭环的一条。本节是初步研讨得到的实证方案，有待实际执行检验。

### 把"恢复语义成立"拆成三个可分离命题

| 命题 | 内容 | 检验手段 | fault injection 的角色 |
|---|---|---|---|
| A 遏制层 | 故障域损坏不出自身 capsule | **静态**：functions 0 unsafe 审计 + R5 边界审计 | 不归它管——遏制是编译期不变式，对抗性注入无法反驳一段编译不出来的代码 |
| B 机制活性 | 任意故障后裁决→重建→归还全流程跑通 | 受控注入（非随机采样） | 主场 |
| C 语义正确性 | 重建后的世界 == 新启动 | 注入 + oracle | 主场，全部难度浓缩在 oracle 设计 |

随机注入在故障空间（Σ 域 × 操作边界 × 故障类型）里覆盖率趋近于零，B 的正确做法是**受控选择**：按故障模型逐个定向触发，每个模型至少一个确定性用例进 CI。

### 首选模型：livelock + watchdog NMI

四种故障模型的验证效力不对等：

| 模型 | 注入手段 | 覆盖面 | 判定 |
|---|---|---|---|
| **Livelock（关中断死循环）** | 域内 `loop { spin_loop() }` 不返回 + watchdog NMI 触发 | **全链路一次覆盖**：不可屏蔽检测 → park-and-ack → 借核变身 → arbitrate → memset+new()+gen++ → 拉取式失效 → 归还核 | **首选** |
| Panic/abort | 现有 `maybe_inject_fault` 概率 panic | 只测恢复段（走 IDT 本核入口），不触发远程裁决 | 语义等价 oracle 的低成本载体，进 CI 每 PR 跑 |
| 越界写 | guard page 外显式写 | 只测 Verdict=Pass 短路分支 | 补充覆盖 |
| 静默数据损坏 | QEMU monitor `memwrite` 位翻转 | 检测层盲区 | **不投入**——safe Rust 下不崩溃的"算错"不在恢复机制管辖内，注入它只能暴露检测无能，无正向验证力 |

livelock 之所以是首选，因为 **Path 2 相对普通 panic recovery 的全部设计增量都集中在"关中断的死循环核如何被另一颗核远程仲裁并恢复"这条路径上**。其余模型各测一段或测不了；livelock 单用例跑通 = 三 crate 分工、静默协议、全 create 化、拉取式失效全部被真实执行路径覆盖。

```rust
// SIMPLIFIED: livelock 最小注入用例骨架; 单域单故障; 待扩至多域并发/多架构
#[cfg(feature = "fault_injection")]
fn livelock_inject() -> ! {
    loop { core::hint::spin_loop(); }   // 不 panic、不返回、纯忙等
}
// watchdog NMI（另一核，或单核同核）→ arbiter 变身 → Rebuild
// 断言: 恢复后经陈旧 Handle 的 lookup 返回 Err(StaleGeneration)
```

### Oracle 四类

"恢复语义成立"的重音在"成立"——凭什么说恢复了。重生流派的 oracle 是：**"域的正确状态"不是故障前那一刻，而是 fresh boot 后应有的状态**。全 create 化把这个哲学承诺压缩成机械可判定的不变式：*重建完成瞬间，capsule buffer 内容 == 纯 `new()` 产出的内容——内存相等，无需语义理解*。

| # | Oracle | 检验 | 实现成本 |
|---|---|---|---|
| 1 | **等价类**：恢复后 dump capsule buffer，与无故障对照 run 逐字节比对（哈希即可） | 命题 C 核心"恢复 == 新启动"；buffer 不等即暴露 R5 审计漏的状态逃逸 | 极低（buffer 是 privileged 静态分配，内核自 dump） |
| 2 | **不变式**：陈旧 Handle lookup 必 `Err(StaleGeneration)`、gen 恰 +1、InitCtx token 未逃逸、未登记外部资源归 BCB 队列 | 信任链每一环 | 低（原子读检） |
| 3 | **FS 差分**：每笔写 n 个边界逐点崩溃，恢复后跑文件一致性校验 | 通道 3（new() = journal replay） | 中（`host-tests/src/fsx.rs` 是现成模型，扩展"中途崩溃+恢复"即可） |
| 4 | **网络分区模拟**：注入后掐断/延迟对端流量，验证会话重连 | 通道 1（已发送 ≡ 分区） | 中（smoltcp vendored 自带 phy 层 `FaultInjector`） |

### 验收标准：不变式而非恢复率

现有 chaos harness 的 "recovery rate ≥ 80%" 是 undo/补偿流派的统计式验收（多数故障被救活即通过）。FREG的宣称是结构式的（恢复边界 = buffer 边界），结构式宣称要么 100% 要么设计有洞——**验收标准随之换为四条 100% 不变式**：

1. 重建后 buffer 与纯 new() 等价（oracle 1）；
2. 陈旧引用必拒（oracle 2）；
3. gen 每重建恰好 +1，不多次不跳号；
4. world-stop 结构段在 ≤50µs 预算内、整体在 ≤100µs 硬顶内实测（速率预算章的兑现检验）。

任一不变式违反 = 设计有洞，立即停工修复；统计式百分比反而掩盖结构缺陷。原 `tests/chaos/` harness 属 undo 时代遗产，已随遗留测试目录清理一并删除；其验收接线随全 create 化落地时以新装具重写（已隐含在该决策的迁移成本中）。

### 实证的管辖边界（诚实声明）

fault injection 证不了遏制层（那是编译器的活，命题 A 静态审计），也证不了静默损坏（那是类型系统的活）。它证的是**恢复机制的活性（B）+ 重建语义的等价性（C）**。"实证恢复语义成立"的精确含义即此，不多承诺。

## L1 设计研讨：状态机、域粒度、结构化并发、上下文保存、失败矩阵与 API 契约

本章是主控立场（C1–C6）与执行方红队逐条对撞的结果。每条给出结论、理由、证据与替代方案；同意不粉饰，反对不回避。溯源等级逐条标注，C 编号保留以便与研讨记录对齐。

### C1 状态机：单一 `DomainState` 与派生 mode

**结论：部分同意；三量拆分已由用户裁定采用。**［用户裁定］T-02 B / T-11 A：七态、六事件、三守卫与 `Faulted` 不得直连 `Healthy` 全部采纳；单一顶层 mode 量拆为 `AdjudicationMode`（静态）+ `SystemHealth`（派生）+ `DomainEntry.phase`（域级），不设第八态。

同意的部分：

- 单一 `DomainState` 枚举替代现有 `DomainState` 与 `RecoveryLayer` 两套状态名并存；
- 事件六类 `InitOk / InitFail / Fault(kind) / RebuildOk / RebuildFail / Timeout`；
- 守卫三条：故障类型为主判据决定 Path1（Inline）/ Path2（Isolated），硬件能力定可用档位，严重度只调重试 / 隔离阈值（T-14 A）；重建重试上限 N 次后转 `Quarantined`；`Quarantined` 只能显式退出；
- 硬不变式：`Faulted` 不得直连 `Healthy`。实现上由状态迁移函数返回 `Err(IllegalTransition)`，不靠约定。

反对的理由：

- "功能区"表的裁决维度（`Inline / Isolated`）不是健康度，而是**静态裁决路径 / 能力档**（由配置与硬件能力决定，见"已定决策"的隔离手段光谱与按需借核两条，以及"执行形态"表的单核 / 多核分档）。把它压成一个派生量，会丢掉"当前故障走哪条裁决路径"这一恢复语义必需的信息。
- 两拍重建期间域处于 `Rebuilding(phase = Device)`（"执行形态"表），这是**域级相位**，不是全局 mode。七态里没有 `Degraded`，说明状态机缺少"结构已恢复、设备未复位"的相位；该相位由 `phase` 字段表达，不增设第八态（T-11 A）。
- 若 `Quarantined` 被算进 `Recovering`，恢复态永不结束；派生规则必须把它排除。

三量模型（［用户裁定］T-02 B）：

- `AdjudicationMode { Inline, Isolated }`：静态配置维度，非派生；
- `SystemHealth { Normal, Recovering, Degraded }`：派生聚合量——全 `Healthy`/`Quarantined` 且无 `Rebuilding` → `Normal`；存在 `Rebuilding` → `Recovering`；存在 `Quarantined` 或 `Faulted` 且无 `Rebuilding` → `Degraded`。这里的 `Degraded` 是系统级聚合值，不是域状态名，也**不在顶层状态机的状态集合里**——顶层状态机无 `Degraded`，同名仅作派生聚合值，判定条件即前句的"存在 `Quarantined` / `Faulted` 且无 `Rebuilding`"；
- `DomainEntry.phase ∈ { Structural, Device }`：仅当 `state == Rebuilding` 有效，表达两拍（T-11 A，不设第八态）。

状态迁移表：

```text
Uninitialized --InitOk--> Healthy
Uninitialized --InitFail--> Faulted
Healthy       --Fault--> Faulted
Faulted       --Fault(TCB 范围 / 不可恢复类型)--> 系统 BHR（非域状态，不进入 Rebuilding）
Faulted       --RebuildStart--> Rebuilding
Rebuilding    --RebuildOk(等价 && gen 恰 +1)--> Healthy
Rebuilding    --RebuildFail(retries < N)--> Faulted
Rebuilding    --RebuildFail(retries >= N)--> Quarantined
Faulted/Rebuilding --Timeout--> Faulted（计入 retries；达上限转 Quarantined）
Quarantined   --ExplicitRelease--> Uninitialized（唯一出口）
任意          --ExplicitRetire--> Retired
Retired       无出边
```

`Rebuilding → Healthy` 的两个条件必须同时满足：重建等价（由 oracle 检验）与 `gen` 恰 +1（不是 +N、不是回绕）。任一不满足 → `RebuildFail`。

### C2 域粒度与 DAG

**结论：部分同意；权威判据放宽与第三形态已由用户裁定。**［用户裁定］T-03 A / T-04 A："域 = 独占状态的可重建单元""非线程非模块""上限 32""注册期无环检查""禁止双向共享可变状态"全部采纳；"权威来源"判据放宽为三源，通信形态补足三种。

反对判据：按"状态必须能从一个权威来源重建，且该权威本身可用"的原判据，PMM 的位图 / 自由链表没有外部权威（影子账本在 TCB），proc 的 fd / IPC 状态也依赖 TCB 影子账本——两者都会被判为"非法域"。这不符合 v2 需要的基础设施域现实（EGFS / proc / net；PMM 已裁定为 TCB 机制，T-03 A）。

［用户裁定］T-03 A：**权威输入 = 持久存储 ∨ 静态硬件事实 ∨ TCB 维护的影子账本（不依赖该域自身执行）**。影子账本由 TCB 持有，TCB bug 导致其损坏时按 BHR 处理——与"TCB 是受信任基座"一致（见"诚实边界"章）。**PMM 暂列 TCB 机制**（故障 → BHR），留复议条件（见下）。

耦合覆盖面检验（逐对）：

| 耦合对 | 真实形态 | 消息 + 快照是否够 | 结论 |
|---|---|---|---|
| EGFS ↔ PMM | page cache 的 pin / 分配、页回收 | **不够** | 需要 TCB 中介的共享页授予（只读 / DMA，带 gen + revoke） |
| net ↔ proc | socket / fd 所有权 | 够（TCB fd 表 + `Handle`） | 消息 + 快照成立 |
| net ↔ PMM | DMA buffer | **不够** | 共享页授予（否则零拷贝退化为全拷贝） |
| proc ↔ PMM | 地址空间 / 页表 | 不属域间共享 | 页表本就在 TCB，proc 只是策略层 |

由此补**第三种通信形态**（［用户裁定］T-04 A）：**TCB 中介的共享页授予**——授权方（PMM）经 TCB 把只读页或 DMA 窗口授予使用方，携带 `gen`。授权协议：授予方向 TCB 提交 `{page, len, gen, 访问权}`，TCB 在页表中建立受限映射；写权只归授予方或硬件，使用方只有受限访问权。撤销协议：域故障时 TCB 按该域 `gen` 单方撤销映射与访问权，使用方下一次访问得到 `Err(StaleGeneration)` 或 page fault；授予方不参与撤销，也不被回调。它不是"双向共享可变状态"。**前提固定**：只有 TCB 可作共享页授予方，服务域之间永不授予。封闭定理（"原创主张"章）依赖这条前提；若未来允许服务域互授，定理须重新证明，不得沿用。

DAG 环的破法（［用户裁定］T-04 A 子项 4a）：EGFS ↔ PMM 存在回收回调环（PMM 回收页 → 询问 EGFS）。把回收拆为**两条单向消息**：`EGFS → PMM: evict(page)`（EGFS 声明候选页、放弃引用）与 `PMM → EGFS: done(page)`（页已 `free` 并完成记账，单向通知）；PMM 永不反向同步调用 EGFS。注册期无环检查可通过。

PMM 归属已由［用户裁定］T-03 A 关闭：PMM 从"可恢复域"降为 **TCB 机制**（其故障 → BHR）。复议条件：EGFS 依赖实测后，若证明 PMM 恢复收益显著（页缓存重建代价远高于 TCB 机制故障的整机风险），再议"影子账本权威域"，但重建需扫描页表（慢路径）。

### C3 结构化并发

**结论：部分同意；反对"1–2 周"与"不需要被阻塞调用方唤醒协议"。**［研讨结论 / 估算］

同意：放弃"杀线程"；协作取消（CancellationToken + 有界步长）适用于 CPU 长循环、可切块 I/O、调度器阻塞等待；域故障时"置取消位 + 换代"使旧任务的外发引用全部失效。这一方向正确，也是把关键路径压短的主要杠杆。

反对的三点：

1. **不可切分的路径确实存在。** 以下函数 / 路径无法只靠 token 切成有界步长，必须逐个处理：
   - `IrqSpinLock::lock()` 的自旋获取（[irq_spinlock.rs:171](src/kernel/privileged/sync/irq_spinlock.rs)；raw 自旋在 `spinlock.rs`）——不可取消，且可能无界等待；
   - `wait_park_ack` / IPI 收敛等待（设计给的是 ~100µs 有界；"有界"是前提，不是结论）；
   - 驱动轮询：AHCI / virtio / e1000 / UART 的"写寄存器 → 轮询状态位"循环，凡无硬超时者取消延迟无上界；
   - 文件系统 journal 提交与块设备在途 IO（`functions/fs/` 写路径；第二拍设备复位 依赖它）；
   - `net` 的 smoltcp poll 循环（包数有界，但单包处理可能长）。
2. **取消延迟 = 最长不可中断步。** 若某硬件轮询超时是 ms 级，worst-case 就是 ms 级，结构段 `≤50µs` 不成立。T-06 D：结构段同步，所有硬件等待加 **100µs 硬顶**（超时即 `Err(Timeout)` → 域故障），设备复位一律移出 world-stop（第二拍后台异步）；整体 world-stop ≤100µs 硬顶，结构段 ≤50µs（能力档内）。
3. **被阻塞调用方仍需要协议。** "不需要唤醒协议"只对**域内 worker** 成立；跨域调用方阻塞在域的消息队列上时，域死亡后它必须收到 `Err(StaleGeneration)` 或超时。最简做法：所有跨域阻塞等待**加 deadline**，每步检查 `Handle.gen`；这不需要显式唤醒，但延迟 = deadline，且它本身就是一种超时协议，不能省略。

栈的补充（［用户裁定］T-07 A）：不杀线程 ≠ 不需要栈。动态栈在重建时无法安全释放，因此不采用；采用**每域槽位静态预置栈**（与 capsule buffer 同池），线程只是槽位内的任务记录，重建时重置 SP。该模型不依赖栈回收。

与"按需借核"（"执行形态"章）的冲突判断：借出的核在执行域任务时必须能被 NMI 抢占；NMI 不可屏蔽，**入场不冲突**。冲突在"parked 核持锁不放"——需要 Park-and-Ack（"正确性补全协议"章）兜底，且恢复完成前该锁被视为"已放弃"。结论：不冲突，但依赖 Park-and-Ack 与静态栈两个前提。

工期判断：1–2 周不成立。［用户裁定］T-06 D：上述路径的清点、逐个加超时、静态栈改造、取消安全点标注，按 **3–5 周**（估算，非承诺）。

### C4 上下文保存区

**结论：部分同意；计时与上限口径已由用户裁定。**［用户裁定］T-08 C / T-09 A：帧布局与栈大小可给草案；结构段 ≤50µs（能力档内）、整体 ≤100µs 硬顶成立；"world-stop 终点 = 裁决首条指令"的计时定义反对。

先反对计时定义：终点定在"裁决首条指令"会把仲裁、memset、`new()`、`gen++` 全部排除在预算外，承诺就失去意义；"执行形态"章的两拍承诺明确包含结构部分（memset + 结构初始化 + gen++）。［用户裁定］T-09 A 改为三段计时：

- `t_entry → t_adjudicated`：NMI/FIQ 入口 → 保存上下文 → 切 recovery 页表 → 收敛（Path2 含 IPI）→ 仲裁；
- `t_adjudicated → t_rebuilt`：memset + `new()` + `gen++`（结构第一拍）；
- `t_rebuilt → t_resume`：唤醒 / 归还（可与第二拍重叠，不计入结构段）。

结构段预算 = `t_entry → t_rebuilt` ≤50µs（能力档内），整体 world-stop ≤100µs 硬顶（T-08 C）。Path1（同核 Inline，无 IPI 收敛）与 Path2（多核借核，含收敛）共用上限，但分别出实测分项。

x86_64 帧草案（NMI + IST）：

```text
[硬件] SS, RSP, RFLAGS, CS, RIP        跨环 5 × 8 = 40 B（同环 3 × 8 = 24 B）
[软件] RAX..R15                        15 × 8 = 120 B
[可选] XSAVE 区                        512 B（legacy）/ ~2.5 KB（AVX-512）
[元数据] 故障核号、原因、CR3、gen       ~32 B
合计                                   ≈ 704 B（legacy）/ ≈ 2.7 KB（AVX-512）
```

- 每核 recovery 栈 **16 KiB**（约 20+ 个 legacy 帧）。依据：单帧 ~704 B × 嵌套 4 层 + 仲裁局部量；另加 1 页 guard。
- 预置 recovery 页表只映射 TCB text/rodata、本核 recovery 栈、串口 MMIO、local APIC、目标 capsule buffer；CR3 切换用 PCID，避免全 TLB 冲刷。共享页授予不使用 recovery 页表，走标准 MMU 映射（与"设计目标与边界"章的路径 1 口径一致）。
- `#DF` 仅作 BHR 最后手段，不作常规入口；NMI 用独立 IST。

aarch64 帧草案（FIQ / SError，EL1）：

```text
[硬件] SPSR_EL1, ELR_EL1               2 × 8 = 16 B（EL1→EL1）
[软件] x0..x30                         31 × 8 = 248 B
[软件] SP_EL1, ESR_EL1, FAR_EL1        3 × 8 = 24 B
[可选] FP/SIMD q0..q31 + FPCR/FPSR     32 × 16 + 8 = 520 B
合计                                   ≈ 288 B（GPR）/ ≈ 808 B（含 FP/SIMD）
```

- SVE 向量长度可变（2048-bit VL 时 32 个 Z 寄存器 + 16 个 P 寄存器约 9 KB），恢复路径必须**禁用 SVE 或惰性保存**，否则帧大小无界。
- 每核独立 EL1 recovery 栈 **16 KiB**；TTBR1 + ASID 切 recovery 空间；ASID 避免全 TLB 冲刷；TTBR0（用户空间）不动。

结构段 50µs 量化判断（［估算］，能力前提：≤8 核 / PCID·ASID / 并行 IPI）：

| 阶段 | ≤8 核 | 16 核 |
|---|---|---|
| 入口 + 保存 + 切页表（PCID/ASID） | 5–15µs | 5–15µs |
| IPI 收敛（并行） | 10–30µs | 15–40µs |
| 仲裁纯函数 | 1–3µs | 1–3µs |
| memset 4KB + 结构 new + gen++ | 2–8µs | 2–8µs |
| 合计 | **18–56µs** | 23–66µs（串行 IPI 可到 80–150µs） |

> 注：≤8 核合计上界估算 56µs，已略高于结构段 50µs 预算；该行是"能力档内"的估算上限，不是承诺值，最终以实测分项为准（若实测超 50µs，按 T-08 C 降档或走整体 100µs 硬顶）。16 核列不纳入结构段承诺，仅作整体 100µs 硬顶参考。

结论：三条并存，不互相替代——

1. **结构段预算 50µs**：`t_entry → t_rebuilt` 的预算值，能力前提为 ≤8 核 / PCID·ASID / 并行 IPI；这是预算，不是已实现读数，Path1 / Path2 分别记录实测分项（T-08 C / T-09 A）；
2. **≤8 核估算上界 56µs**：上表合计的算术上界是 56µs，已略高于 50µs 预算；该值保留在文档里，不被预算抹掉，也不被当作承诺值；
3. **超预算按能力档降级**：实测若超出 50µs，按 T-08 C 降低能力档；整体仍以 100µs 为硬顶，超出硬顶或无法降档时按超时 → BHR。

16 核以上或串行 IPI 不保证结构段，只保证整体 100µs 硬顶。硬件侧还有 SMM / 虚拟化对 NMI 的阻塞（可达数百 µs），必须实测。故 50µs 的准确表述是"结构段、≤8 核、PCID/ASID、并行 IPI 的前提**预算**"，不是"50µs 已实现"；整体上限统一为 100µs 硬顶。

### C5 失败分类与策略矩阵

**结论：同意可做；补充 `Quarantined` 与 `IllegalTransition` 行；数值阈值按 T-10 A 先取保守默认值。**［研讨结论］

| 故障类型 | 检测源 | 触发前状态 | 动作 | 阈值 | 验收（oracle） |
|---|---|---|---|---|---|
| 非法指令 #UD | IDT | Healthy | Path1 Inline → Rebuild | 立即 | 1 + 2 |
| 非法地址 #PF（域范围） | IDT | Healthy | Path1 → Rebuild；地址在 TCB 范围 → BHR | 立即 | 1 + 2 |
| 非法地址 #PF（TCB 范围） | IDT | 任意 | BHR | 立即 | 无（终态） |
| 断言失败 / panic | panic 壳 | Healthy | Path1 → Rebuild | 立即 | 1 + 2 |
| 锁超时 | 仪器化锁 + tick | Healthy | Path2 远程 → park → Rebuild | 保守默认 N ticks，故障注入后调参（T-10 A） | 1 + 2 + 4 |
| 心跳丢失 | `detect.rs` | Healthy | Path2 → Rebuild | `heartbeat_max_gap`（默认沿用现有字段） | 1 + 2 |
| 配额超限 | `policy.rs` | Healthy | 降级能力；达上限 → Quarantined | `cpu_quota_max` / period（0 = 不限） | 能力位翻转 |
| 外部故障 MCE / SError | MCE / SError 异常向量 | 任意 | 域范围 → Path2；TCB/硬件 → BHR | 立即 | 1 + 2（域）/ 无（BHR） |
| 主动 panic（注入） | `fault_inject` | Healthy | Path1 → Rebuild | 立即 | 1 + 2 |
| 初始化失败 | 注册 / Init | Uninitialized | Faulted → 重试 → Quarantined | N 次（默认 5） | 2 |
| 状态机非法迁移 | 状态机 | 任意 | `Err(IllegalTransition)` → BHR | 立即 | 无 |
| 陈旧 Handle | lookup | 任意（消费者侧） | `Err(StaleGeneration)`，不触发恢复 | gen 不匹配 | 2 |
| `Quarantined` 中再次故障 | IDT / tick | Quarantined | 不重试；保持隔离，记录审计 | 立即 | 2 |

与四条不变式对齐：

- 重建等价 → 每次 `RebuildOk` 的 oracle 1 验收；
- 陈旧引用必拒 → 每次 `gen` 变化后的 lookup；
- `gen` 恰 +1 → 每次 `RebuildOk` 的锁内改代（`next_gen` 加一，无回绕）；
- world-stop 结构段 ≤50µs（能力档内）/ 整体 ≤100µs 硬顶 → 仅 Path1 / Path2 的机制段（结构第一拍 + 硬顶计时）。

与 oracle 四类（"故障注入实证"章的 1–4 类）的映射写在表最后一列。阈值来源：重试上限沿用 [types.rs:17](src/kernel/privileged/freg/types.rs) 的 `MAX_CONSECUTIVE_FAILURES = 5`；心跳沿用 [domain.rs:54](src/kernel/privileged/freg/domain.rs) 的 `last_heartbeat` / `heartbeat_max_gap`；配额沿用 [domain.rs:42-48](src/kernel/privileged/freg/domain.rs)。心跳窗口 / 锁超时 / 配额周期具体值由［用户裁定］T-10 A 先取保守默认，故障注入后调参。

### C6 API 与 panic 契约

**结论：同意六条契约；补固定签名与错误码枚举。**［研讨结论］

六条契约与强制手段：

1. **不分配**：crate 不依赖 `alloc`，`[dependencies]` 为空；`FregState` 定长。
2. **不取锁，只用 `try_lock`**：crate 无 `Mutex` / `OnceLock`；状态由调用方以 `&mut FregState` 传入；privileged 侧只用 `try_lock`。
3. **不再 panic**：`#![deny(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]`；Kani harness 验证无 panic 路径。
4. **不递归**：privileged 的 `arbiter_claim` 用 `compare_exchange`；crate 状态机无回调、无重入。
5. **迭代有界**：所有循环上界为 `MAX_DOMAINS` / `MAX_DEPS` / `MAX_AUDIT` 常量。
6. **日志无锁 best-effort**：`FregEnv::log` 契约；实现侧只写无锁环或串口，失败静默。

固定签名：

```rust
pub fn register(state: &mut FregState, req: &RegisterRequest, env: &dyn FregEnv)
    -> Result<DomainId, FregError>;
pub fn unregister(state: &mut FregState, id: DomainId) -> Result<(), FregError>;
pub fn on_panic(state: &mut FregState, ctx: &PanicCtx, env: &dyn FregEnv, hw: &dyn FregHw)
    -> Result<RecoveryOutcome, FregError>;
pub fn on_tick(state: &mut FregState, now: u64, env: &dyn FregEnv)
    -> Result<TickOutcome, FregError>;
pub fn arbitrate(view: &ArbiterView, cfg: &FregConfig) -> Verdict;
pub fn lookup(state: &FregState, handle: Handle) -> Result<&DomainEntry, FregError>;
pub fn audit_snapshot(state: &FregState, out: &mut AuditBuffer) -> Result<usize, FregError>;
```

`AuditBuffer` 是调用方提供的定长输出缓冲，crate 不分配。所有签名都不接收全局对象，也不返回堆对象。

错误码枚举：

```rust
pub enum FregError {
    InvalidDomain,
    DuplicateDomain,
    DependencyCycle,
    CapacityExhausted,
    NotRegistered,
    IllegalTransition,
    InvalidHandle,
    StaleGeneration,
    EnvUnavailable,
    HwFailure,
    Timeout,
}
```

panic 入口的错误码映射：`NotRegistered → -1`（保持旧契约）、`IllegalTransition → -4`（新增，与既有 `-2 = 已尝试` 不冲突）、其余 `→ -1`。

## 议题状态（已闭合）

- **域间通信形态** → ［用户裁定］T-04 A：三形态成立（单向有界消息 + 拉取式只读快照 + TCB 中介共享页授予）；
- **域粒度与现有模块映射** → EGFS / proc / net 为初始域；［用户裁定］T-03 A：PMM 为 TCB 机制（留复议条件）；
- **结构化并发** → ［用户裁定］T-06 D / T-07 A：协作取消 + 静态栈池 + 有界等待；设备复位后台异步；工期 3–5 周；
- **检测粒度** → 已落到 `detect.rs` 纯函数 + `policy.rs` 矩阵；［用户裁定］T-10 A：先取保守默认值，fault injection 后调参。

## 裁定记录（已闭合）

以下 13 项本轮裁定均经［用户裁定］，采纳此前建议；T-13 为前章已闭合项，一并列在表末以便追溯"当初为什么这么定"。本表不再作为待决策项。

| 编号 | 裁定 | 一句话理由 | 落点 |
|---|---|---|---|
| T-01 | **B**：create 即总回收 | panic=abort 下 Drop 在故障现场不可靠；恢复单位改为静态 capsule buffer，`memset + new() + gen++` 即总回收，undo 随之废弃、S4 解锁 | "核心命题" / "单主路径" / "全 create 化恢复模型"章 |
| T-02 | **B**：拆三量 | 单一顶层 mode 量会丢"裁决路径"信息；`AdjudicationMode` 静态、`SystemHealth` 派生、`Rebuilding.phase` 域级 | C1 / "已定决策" |
| T-03 | **A**：权威三源，PMM 列 TCB 机制 | 只认外部权威会让 PMM / proc 不合法；影子账本可作权威，PMM 暂降 TCB 机制并留复议条件 | C2 / "已定决策" |
| T-04 | **A**：第三种通信形态 + 子项 4a 拆两条单向消息 | 仅消息 + 快照覆盖不了 page cache 与 DMA；共享页授予须由 TCB 按 gen 单方撤销，EGFS↔PMM 回收环拆为 `evict → free → done` | C2 / "已定决策" |
| T-05 | **B**：删 `recovery_freg_maintenance` | 全仓零调用；删除前核对函数体，独有检查并入 `detect` / `policy`，避免 F9 存量零引用 | C5 / 修剪清单 |
| T-06 | **D**：结构段 50µs、有界等待 100µs 硬顶、设备复位后台异步；C3 工期 3–5 周 | 硬件轮询若留在 world-stop 内，取消延迟无上界；分层后才能同时保住结构段与整体上限 | C3 / "性能论证"章 |
| T-07 | **A**：每域静态栈池 + SP 重置 | "不杀线程"模型下动态栈会泄漏；静态池与 capsule 同池，重建只重置 SP | C3 / "域边界模型" |
| T-08 | **C**：双层上限 | 结构段 50µs（能力档内）+ 整体 100µs 硬顶；保留实测上界与能力前提（≤8 核 / PCID·ASID / 并行 IPI） | C4 / "性能论证"章 |
| T-09 | **A**：三段计时 | "终点 = 裁决首条指令"排除了仲裁与重建；`t_entry → t_adjudicated → t_rebuilt`，Path1 / Path2 分别出实测分项 | C4 / "性能论证"章 |
| T-10 | **A**：先取保守默认值 | 现有字段无统一口径；故障注入后调参比先验拍定更可靠 | C5 |
| T-11 | **A**：用 `Rebuilding.phase` | 七态缺"结构已恢复、设备未复位"；用相位字段表达，不增设第八态 | C1 |
| T-12 | **A**：保留 `kernel-freg`，放弃行数承诺 | 行数不是可审查性的可靠代理；改用"零依赖 + 零 unsafe + 纯函数化"三项性质 | B / "FREG 语义 crate"章 |
| T-13 | **A**：维持半单地址空间 | 被恢复域不新建独立 AS，只有裁决核心持有 recovery 页表根；代价见"对照参照"章 | 前章已闭合 |
| T-14 | **A**：故障类型为主判据 | 旧口径让严重度选 Path、与"能力决定档位"冲突；改为故障类型选 Path、能力定档位、严重度只调重试 / 隔离阈值 | C1 / C5 |

> T-13 的裁定与代价记录见"架构形态：Isolated 独立空间裁决核心"章的"已定决策"与"对照参照：MINIX 3 进程重生与双层 FREG 映射"章的"取舍"段；本表保留一行以保持 13 项之外的连续性。

## 交叉引用

- [explain-framekernel.md](./explain-framekernel.md)：框内核架构、privileged/functions 边界、6 条安全不变式——本设计的地基。
- [vision-hope.md](./vision-hope.md)：Edgine 项目愿景，FREG故障恢复是嵌入式 / IoT 场景的契合点。
- `src/kernel/privileged/freg/`：FREG机制层（RecoveryDomain / 分层恢复；UndoLog 为 undo 时代历史实现，随 S4 删除）。
- `src/kernel/functions/freg/`：FREG策略层（RecoveryPolicy / 故障归属 / 健康监测 / 级联）。
