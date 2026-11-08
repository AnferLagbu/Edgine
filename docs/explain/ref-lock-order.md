# 全局锁顺序

> Edgine 内核锁获取的全局顺序约定。违反此顺序将导致 AB-BA 死锁。
> 运行时检测由 `privileged/sync/lockdep.rs`（Lockdep）提供。

## 锁层级（从高到低）

获取锁时必须按层级编号从小到大获取。同一层级内的锁可以任意顺序获取。

层级编号的**方向语义**：0 是最靠内核底层的域（中断安全、恢复路径），14 是最靠外设的域。
"从小到大"等价于"先取高层级域、再取低层级域"；一旦持有了编号大的锁，就不得回头去取编号小的锁。
成体系的例外登记在序表的 `exceptions` 数组里，见「调度域的锁域所有权」与「日志与控制台发射锁」两节。

```
层级 0: FREG / Recovery（中断安全）
  RECOVERY_MANAGER, FREG.ROLLBACK_LOG, FREG.PANIC_MSG,
  FREG.domain.capture_cb, FREG.domain.rollback_cb, FREG.domain.undo,
  FREG.domain.depends_on, FREG.domain.depended_by,
  FREG.domain.freg_stack, FREG.domain.addr_ranges,
  RESET_AUDIT_LOG, DEVICE_SNAPSHOTS

层级 1: PMM（物理内存）
  PMM_SNAPSHOT, PhysicalMemoryManager::lock（buddy 分配器面；帧持有计数与 COW 待破标
  FRAME_COW 簿记区同在 PhysicalMemoryManager::lock 这一面内，不另开锁类）

层级 2: Slab / Kmalloc（内核堆）
  GENERAL_CACHES, GLOBAL_KMALLOC

层级 3: VMM / VMA（虚拟内存）
  VMM_LOCK, MmStruct::vmas

层级 4: Page Cache（页缓存）
  PAGE_CACHE[hash_bucket]

层级 5: COW（写时复制）
  （原 COW_REFS 锁已删除：帧持有计数并入层级 1 的 PMM 计数面）

层级 6: Swap（换页）
  SWAP.lock

层级 7: Process Table（进程表）
  PROCESS_TABLE.processes, PROC_SNAPSHOT

层级 8: Scheduler（调度器）
  cfs_rq（per-CPU 运行队列锁；跨核搬动按 cpu index 升序取两把）

层级 9: 进程属性
  sigaction_table, rlimit_table, seccomp.filters, namespaces, numa_policy,
  thread_group.members（线程组成员账）, thread_group.children（子进程组账）,
  fd_table.entries / fd_table.cloexec（fd 表）,
  fs_context.cwd / fs_context.root（进程 FS 上下文; umask 为原子量）

层级 10: POSIX Timer
  TIMER_MANAGER

层级 11: VFS / FS（文件系统）
  DCACHE, ICACHE, RAMFS_DATA（文件系统特定锁的代表条目；EGFS / devfs 走各自对象锁）

层级 12: Network（网络）
  NET_STATE（原 NET_LOCK 与 12 个 static mut 的替代单源）, NET_SNAPSHOT_LOCK,
  NET_STACK_INSTANCE（smoltcp 栈锁）, UDS_STATE（unix socket 表）

层级 13: Driver（驱动）
  UEFI.gop_mode, UEFI.memory_map, UEFI.variables,
  KEYBOARD_DEVICE, GLOBAL_FRAMEBUFFER（设备特定锁的代表条目；AHCI / NVMe / E1000 /
  VirtIO 走各自设备锁）

层级 14: Block Device Registry（块设备注册表）
  NVME_CONTROLLERS, AHCI_CONTROLLERS, ATA_CONTROLLERS, HOTPLUG_MANAGER
```

本表不再是手写清单：它的**机器可读单源**是 `scripts/lock_class_order.json`（层级定义 + 每条锁类的锚点与归属层级），`scripts/audit_deadlock_matrix.py` 的第 (6) 项检查对本表做双向对账并把四条判据执法——① 代码里 `::named` / `::named_lazy` 注册的锁类若序表未声明即红（新锁类必须先入表）；② 序表声明的 `lockdep` 类在代码里已无注册点、或 `symbol` 锚在代码里定位不到即红（本表因此能抓住改名与删锁，序表对账首跑即抓出 `COW_REFS` 已删、`NET_LOCK` 已改名 `NET_STATE`、层级 14 四名从无实现）；③ 本表的条目集合与序表逐层级双向不符即红（括号内的说明文字不参与判定）；④ 容量对账（A-127c）：序表 `lockdep` 条目数与代码具名类数任一到达 `limits.rs` 的 `MAX_LOCK_CLASSES` 实值即红，上限不在脚本里抄数、单源不可解析即 fail-closed。判据 ④ 之所以必要：类表是定长数组，到顶之后的第 N+1 个类在运行时只表现为「注册被丢弃 + 一条粘滞失效读数」，静态侧必须当场拦住而不是等读数行。`MAX_HELD_LOCKS` 与本表层级数的差只进摘要读数不进取决（当前 8 vs 15，即「一条合法长链可每层持一把、而持有栈只跟得住 8 把」）——抬高上限牵动 boot BSS 定长数组尺寸，属另一轮裁定，溢出本身由运行时粘滞读数执法。改锁的顺序归属 = 改序表 + 改本表，两处同批提交，红在门禁而不是在下一次读文档时。第 (7) 项检查是静态倒序检测（见下节「静态锁序倒序检测」），它消费同一份序表与 `exceptions` 数组。

序表的锚点有两种口径：`lockdep` 锚要求名称与代码类名字面量完全一致（16 个，见下文 Lockdep 集成；该计数**必须整文件匹配**——`IrqSpinLock::named_lazy(` 与类名字面量分行的 4 处 FREG 域内锁，逐行扫会看成「代码里没注册」而同时漏掉判据 ① 与表条目，注释行则整体跳过）；`symbol` 锚只要求该标识符仍能在 `src/kernel` 定位，用于尚未具名化的锁——`PhysicalMemoryManager::lock` 与 `VMM_LOCK` 是 `AtomicBool` 手递自旋标志、不进 lockdep 类表，`PAGE_CACHE` / `PROC_SNAPSHOT` / `TIMER_MANAGER` 等仍是未具名 `static` 构造。全仓未具名构造点共 242 处，一律不被具名化纳入本表（2026-11-05 裁定：先建表再改名，避免把改名噪声与表条目失效混在首轮读数里）。

## 规则

1. **层级递增**：获取锁时，层级编号必须 >= 当前持有的最高层级锁。
2. **中断上下文**：仅 SpinLock 和 IrqSpinLock 可在中断上下文获取。Mutex/PiMutex 可能 yield，禁止在中断中使用。
3. **递归禁止**：同一线程不可重复获取同一非递归锁（SpinLock/Mutex/RwLock）。
4. **持锁禁分配**：自旋锁持有期间禁止内存分配（kmalloc/slab）和调度（yield/sleep）。

## 静态锁序倒序检测（A-3b）

层级表本身只是文档约定；把它变成门禁的是 `scripts/audit_deadlock_matrix.py` 的第 (7) 项检查。
运行时的 AB-BA 检测（lockdep）只在真取锁时记账，静态检测补的是先验：持有一把已声明层级的
guard 期间取了层级更小的锁（或取了层级为 `null` 的最内层锁）即判红，不必真的跑到那个交错。

派生的取锁组合有三条形态：

- **词法嵌套**：同一函数内，外层 guard 的生命周期覆盖内层取锁点。
- **一跳跨函数**：持锁期间直接调用的被调函数，其自身每一个已知类的取锁点都视为在外层 guard
  覆盖下（被调者内部中途是否已释放这把锁不追——保守方向）。
- **回调包装**：被调函数取到自己的锁后调用传入的闭包（`with_process` / `with_process_mut`
  这类），闭包实参里的取锁点按"外层 guard → 包装者入口锁 → 闭包内取锁"两级展开。

guard 生命周期按 Rust 规则取：`let` 绑定的 guard 存活到块尾（显式 `drop(name)` 处截断）；
非绑定的临时 guard 只到语句尾；`if let` / `while let` / `match` 的 scrutinee 临时 guard 存活到
整个构造结束。反例同时钉进自测：`let v = *m.lock()` 绑定的是值不是 guard，
`m.lock().clone()` 同理，二者的临时 guard 都只在语句内有效。

序表可以登记**例外**（`exceptions` 数组，`outer` / `inner` 各须是已声明的类）。现存两条：

- `cfs_rq → PROCESS_TABLE.processes`：调度域锁域所有权，见「调度域的锁域所有权」。
- `LOG_EMIT_LOCK → LOG_SINKS`：最内层发射锁唯一允许的内层，见 `LOG_EMIT_LOCK` 一节。

例外表是机器可读的：新增一条降序路径只能靠改序表放行，不能在脚本里写死文件名。

未声明锁类的取锁点（接收者末段名映射不到序表条目，`self.inner.lock()` 这类局部变量是主体）
不进倒序判据，改由**覆盖率棘轮**执法：`scripts/lock_order_static.baseline.json` 按
（文件, 接收者末段名）冻结当前存量，新增或计数上涨即红；`--update-lock-order-baseline` 是
唯一写者，基线缺失按 fail-closed 判红。这份快照不是锁序判据——它只保证"新取锁点必须先被
看见"，倒序判定仍只走 `scripts/lock_class_order.json`。

刻意的覆盖边界（只进摘要读数，不判红）：多跳传播（第二跳起不再追）、函数指针与 trait 对象
动态派发、宏展开体、闭包被存储后延迟执行、同名函数导致的歧义调用。这些形态在当前内核里
要么不承载锁嵌套，要么静态不可判；运行时 lockdep 仍是它们的执法者。

当前读数：取锁点 1244（已分类 396 · 未分类 848）/ 函数 9761 / 组合边 14 / 例外 2 / 违规 0。

## 日志与控制台发射锁（`LOG_EMIT_LOCK`）

这把锁不在上面的层级表里，它是**最内层**的锁：任何上下文都可以取它，但取到之后不
得再取任何其他锁——临界区内只允许串口逐字节写出与日志环形缓冲区推进，唯一的例外是
`LOG_SINKS`（sink 注册表自身的锁，嵌套方向固定为 `LOG_EMIT_LOCK` → `LOG_SINKS`）。
反过来也不成立：持有任何层级锁期间都可以直接写日志，因此它不参与「层级递增」判定。

串口与环形缓冲区是全局资源，而 `IrqSpinLock` 加锁只屏蔽本核中断，所以跨核独占必须由
这把锁承担。它同时定义了**行原子契约**：一条控制台输出必须在一次持锁内整段写完，段
边界取换行符，段长上限 `klog::CONSOLE_EMIT_MAX`（256 输入字节；换行按 `\r\n` 展开后写
出量最多翻倍，故最坏 ≈ 44ms、每段一个换行时 ≈ 22ms @115200）——该上界就是「本核中断
被屏蔽、其余核自旋等同锁」的最坏时长上界。

契约的射程止于**一次持锁段**，不等于一次逻辑行：由多次 `write` 拼出的行（如 `print` +
`print_dec` + `println`）仍可能在调用之间被别的写者插入，实测即 `[KPTI] EL0 kernel
high-half access denied (pid=` 与尾部 `8)` 被内核日志分开。整行原性由写者负责合成单次
写出——用户库 `println` 已如此，逐个片段打印的调用点需自行拼行（`init` 的
`thread_fail` 即先把整行拼进栈缓冲再一次 `write`）。

契约的消费点：klog 的整行发射在锁内组装并一次写出；用户态写 fd 1/2 与 `/dev/console`
走 `klog::console_write_bytes`（同一个锁、同一套切段规则）；用户库的 `println` 把正文
与换行合成一次 `write`，使一行落进一个持锁段。

有意留在锁外的写出方：`serial_write_bytes`（它是锁内 sink 广播的写出原语，`IrqSpinLock`
不可重入，在临界区内再取即本核自旋死锁）、`src/kernel/lib.rs` 的 panic 路径与 early
boot（SMP AP 上线、NUMA、cgroup/session 初始化）。panic 可能正发生在持锁期间，此时
取锁会自旋不返，所以这些路径宁可牺牲行原子也要保证能落地——它们的输出交错只可能出现在
崩溃转储里，而崩溃转储本来就是逐行独立可读的。

## 调度域的锁域所有权

层级表给的是**跨域**的粗粒度定序，它假设"先取的锁授权后取的锁"。调度域不满足这个假设：
一个任务的可运行状态由两份数据共同表达——进程控制块里的队列三元组（`cfs_on_rq` /
`cfs_vruntime` / `cfs_rq_cpu`），以及它所属 CPU 运行队列里的树节点与权重账。两者的**权威
保护者是归属核的 rq 锁**，不是进程表锁。进程表锁在这里只是取得 `Process` 引用的入场券：
`PROCESS_TABLE.with_process()` 借用引用、在闭包结束时即释放，它不参与"谁有权变更队列成员
身份"的裁决。因此本域内的既定取锁方向是

```
cfs_rq（层级 8）  →  PROCESS_TABLE（层级 7）
```

这个嵌套看起来像"回头取编号小的锁"，但它不构成层级倒退违规：被保护的不变式（账随任务走、
同一任务不双挂）由 rq 锁单方面确立，进程表锁只是访问手段。反过来，**严禁在持进程表锁的
临界区内获取任何 rq 锁**——那是同一条不变式的第二个授权者，两个授权者按相反方向取锁即
AB-BA。此禁令当前在全内核成立（`with_process` 闭包体内不出现运行队列操作），新增调度路径
必须维持它；需要变更归属时，把 `with_process` 挪进 rq 临界区，而不是相反。

同一份锁域所有权还解释了搬定协议的两个形状约束：归属字段 `cfs_rq_cpu` 的写入必须经认领凭证
（`RqClaim`）单一入口，以及任何一次队列成员变更都必须在 rq 临界区内闭合——先认领、再搬账、
后提交归属，中间不放锁。历史上"摘源 → 放锁 → 投目的"的两段式正是把这个不变式拆成两次上锁，
留下了任务"既不在树上又已清凭证"的真空窗口（详见 `docs/plan/net-e2e-tcp-echo-flakiness.md`）。

### 双 rq 锁的同层级定序

跨核搬动要同时持有源核与目的核两把 rq 锁。它们同属层级 8，层级表对此沉默，于是必须由本域
自行定序：**按 cpu index 升序取锁**，`index` 相同时（同核，或本就无归属）只取一把——嵌套同一
把 `IrqSpinLock` 即自死锁。两个方向相反的搬动者若各按"先源后目"取锁，就会精确构成 AB-BA。
这条定序与 Linux `double_rq_lock()` 用 `cpu_to_rq(order)` 排序是同一个做法，也允许更省事的等价
形式：目的核恒先取（`rq_lock(rq, &rf)` 的 `rq_next` 语义）。本工程的实现选择是升序取锁，见
`Scheduler::cfs_place_on` 与 `Scheduler::load_balance` 的分支。

### 周期扫描的持锁口径

调度器里有两条按固定节拍跑全表的扫描：`tick_sleep_wake` 收到期睡眠者，`tick_reap_zombies` 收孤儿僵尸组。
它们曾写成 `for pid in 1..=255` 逐槽 `with_process`，而 `with_process` 在判槽位空否之前就先取 `processes`
锁，于是**空槽也计一次有名锁获取**——每 tick 每在线核固定 255 次，与在册任务数无关（lockdep 启动读数里
这一形态占 `acquire_ops` 的 98%）。现在的形态是**一次表锁内的批量读**：`ProcessTable::scan_into` 在一
段临界区内按槽位升序把满足判据的条目收进调用方的定长缓冲，返回实收条数；`for_each` 与 `scan_into` 共用
同一个私有取锁点 `walk_locked`，所以"持表锁遍历"在内核里只有一处形态，契约也只需写一次。

这次改造压掉的是**op 次数**（由"tick 数 × 槽数"变成"tick 数"），代价是单次持锁时长由"一个槽的判据读取"
扩成"一轮扫描"。两种形态的上界不同源：旧形态的持锁窗口与在册数无关而次数与槽数成正比，新形态恰好相反，
所以窗口长度只由谓词的工作量决定，而谓词被钉成四律——只读 `Process` 的原子字段与内联标量、不得回调
`PROCESS_TABLE` 的任何方法、不得分配、不得打印、不得获取任何运行队列锁。第三条里的"不得打印"与本文件
`LOG_EMIT_LOCK` 一节同源：一次串口写出可拖到毫秒级，把它放进表锁等于把整核中断屏蔽窗口撑在层级 7 的临界
区内。契约写在 [`ProcessTable::scan_into`](../../src/kernel/privileged/proc/process.rs) 自己的文档注释
上，形状与措辞由 [host-tests/tests/sched_tick_scan_m7_test.rs](../../host-tests/tests/sched_tick_scan_m7_test.rs)
钉住（含"空槽不计遍历次数""缓冲写满即停止调用谓词"两条双向核对），因此谓词一旦被写成回调表方法或就地
取 rq 锁，红在测试而不是只靠 review 拦住。

对上一节的锁域所有权，结论是**没有引入新的取锁方向**：批量读只读 TCB 内的原子量，不参与"谁有权变更队列
成员身份"的裁决，`cfs_rq → PROCESS_TABLE` 的单向例外既未扩大也未违反。需要再取锁的动作一律排在快照之后
释放表锁的那一刻——唤醒走 `Scheduler::unblock`（层级 8）、僵尸回收的父进程存活查询与
`group_zombie_exit_code` / `reap_zombie_group`（再次取层级 7）都在锁外的结果集上完成。这正是"锁内收、
锁外做"的分工：把判据所需的最小读取留在临界区，把可能嵌套任何锁的后续动作全部推出去。

一处**可观察语义的偏离**记在这里以免被读成等价改造：旧形态的回收上限 16 数的是"选中"的线程组（就绪判据
在锁外逐项完成），批量形态只能在锁内数"候选"，故候选窗口取 `REAP_CANDIDATE_WINDOW = 32`（去重在锁内按
tgid 完成，选择上限本身仍是 16、选中次序仍是槽位升序）。只有当同一轮里存在多于 32 个不同的孤儿僵尸组、
且前 32 个里不足 16 个通过锁外就绪判据时，未进窗口的组延后一轮（约 1 秒）回收。睡眠唤醒一侧没有这个张力：
8 个名额的截断点与旧形态"命中数达上限即 break"同形，且截断发生在谓词之前而非之后。

本口径不是扫描算法的终局形态。按到期时间组织的睡眠链（A-15 方案甲）能把扫描本身从 O(在册数) 降到
O(到期数)，而本节消掉的是"线性扫描必须付几次锁"这个独立于扫描算法的代价——两者可以叠加，互不冲突。

## 常见路径示例

### 缺页处理（Page Fault）

```
层级 7: ProcessTable::with_process()  →  读当前任务 Process.mm (A-16 甲; 闭包返回前释放)
层级 3: MmStruct::vmas.lock()         →  查找 VMA
层级 1: PMM 分配页帧                  →  buddy allocator
层级 3: VMM 映射页表                  →  页表锁
层级 4: Page Cache 插入                →  PAGE_CACHE[hash].lock()
```

层级 7 与层级 3 在此**顺序发生而非嵌套**：表锁闭包内只做一次原子读，返回后调用方
才取 `MmStruct::vmas`。缺页入口只面向 EL0 故障（进入内核时本 CPU 不持任何内核锁），
故不存在"已持表锁再触发缺页"的自死锁面；内核态 uaccess 故障不走此路（见
`break_cow_for_kernel_uaccess` 的 per-frame COW 判据，不取当前任务 mm）。

### 进程创建（Fork）

```
层级 7: ProcessTable::processes.lock()  →  分配 PID
层级 3: MmStruct::vmas.lock()          →  克隆 VMA
层级 1: PMM 分配页帧                    →  COW 页表克隆
层级 8: scheduler.lock()                →  注册到运行队列
层级 9: thread_group.children.lock()    →  添加子进程组 (挂父组 tgid; 线程不入账)
```

### 文件写入

```
层级 7: ProcessTable::with_process()    →  获取进程
层级 11: VFS open → inode lock          →  文件系统操作
层级 4: Page Cache 获取页               →  PAGE_CACHE[hash].lock()
层级 1: PMM 分配页帧（if cache miss）    →  buddy allocator
```

### 网络收包

```
层级 13: 网卡中断 → spin_lock_irqsave  →  设备锁
层级 12: NET_LOCK.lock()                →  协议栈处理
层级 12: socket.lock()                  →  投递到 socket buffer
```

### 进程退出（Exit）

```
层级 7: PROCESS_TABLE.with_process()     →  退出引用 / 置 exiting
层级 9: fd_table.release_user()          →  最后持有者判定 (K-03a)
层级 11: close_all_fds()                 →  逐句柄递减 OpenFile 引用 (仅最后持有者)
层级 9: thread_group.members.lock()      →  移除本 TID; 组空则置退出状态
层级 7: PROCESS_TABLE.with_process()     →  置 Zombie / 处理子进程
层级 9: thread_group.children.lock()     →  组最后成员退出时改挂/清空子进程组账
```

`exit_group` 的退出请求发布落在层级 7 的表锁之内，并与 `clone` 的入组共用层级 9 的成员表锁，使「入组」与「组退出」不可交错（K-06）：

```
层级 7: PROCESS_TABLE.with_process()     →  取组 Arc 并在同一闭包内发布退出请求
层级 9: thread_group.members.lock()      →  request_exit 置位 / join 复查后拒绝入组
层级 7: PROCESS_TABLE.with_process(tid)  →  逐成员挂 SIGKILL（成员快照已在锁外遍历）
层级 8: Scheduler::unblock(tid)          →  睡眠成员搬回运行队列（不与层级 7 嵌套）
```

### 跨核搬动（负载均衡 / 唤醒投送）

```
层级 8: per_cpu_for(min).cfs_rq.lock()   →  归属核先取 (cpu index 升序)
层级 8: per_cpu_for(max).cfs_rq.lock()   →  第二把同层级锁, 定序见上节
层级 7: PROCESS_TABLE.with_process()     →  借引用读快照 / 写 Ready (rq → PT)
层级 8: 提交归属, 释放两把 rq 锁          →  变更全程不放锁
```

### 周期扫描（tick 睡眠唤醒 / 僵尸回收）

```
层级 7: PROCESS_TABLE.scan_into()             →  一次表锁内收候选, 谓词只读原子量
(锁外) 层级 8: Scheduler::unblock(pid)        →  唤醒只作用于快照结果
(锁外) 层级 7: with_process(parent)           →  父组存活判据, 与 scan_into 不嵌套
(锁外) 层级 7: group_zombie_exit_code(tgid)   →  就绪复查
(锁外) 层级 7: reap_zombie_group(tgid)        →  真回收, 内部另有分配与成员表遍历
```

括号里的"锁外"是这条路径的全部要点：它表示后四行不与第一行嵌套，因而不是层级倒退。旧形态把判据与动作写在
同一次持锁里、每槽一次，读起来像"每行都在锁内"，所以层级表无法表达它的真实持锁集合；改成两段式之后，本示
例的每一行都能单独核对。第一行的持锁时长上界是"一轮扫描 × 谓词工作量"，谓词的禁止项见上节《周期扫描的持
锁口径》。

## Lockdep 集成

Lockdep（`privileged/sync/lockdep.rs`）在 `debug_assertions` 或 `feature = "lockdep"` 启用时，
跟踪每个锁的获取/释放，构建锁序图，检测以下问题（`lockdep` 自 A-1（2026-11-08 裁定甲「全量
常开」）起在默认 features 里，出货的 release 构建默认开启检测，`--no-default-features` 是
唯一关闭形态）：

- **AB-BA 死锁**：线程 A 持锁 L1 再获取 L2，线程 B 持锁 L2 再获取 L1
- **中断上下文睡眠**：在硬中断中获取 Mutex
- **递归获取**：同一线程对同一锁重复 lock
- **释放未持有的锁**：unlock 时当前线程并非持有者

Lockdep 类由构造点自动注册，无需手动维护锁类 ID：`SpinLock::named()` / `Mutex::named()` /
`RwLock::named()` / `PiMutex::named()` 在创建时注册；`IrqSpinLock` 走 const 构造 + 首次
`lock()` 按名惰性注册（机制 A）。五把锁的门控掩码同为 `any(debug_assertions, feature = "lockdep")`
——只写 `debug_assertions` 的锁在 release + `KERNEL_FEATURES=lockdep` 构建里会静默不跟踪，
锁序图缺边（由 `host-tests/tests/lockdep_cfg_mask_test.rs` 拦回归）。

记账路径的开销形态（低开销化首批 M1+M2+M5+M6）：一次有名锁获取只进入**一次** `LOCK_DEP_MAP`
全局临界区（检查 1/2/2.5/3 与压栈同临界区完成）、只开一个 per-CPU 持有栈的关中断窗口，遍历持有栈
直读切片而非 `to_vec()` 堆分配，AB-BA 的 BFS 只对**首次出现**的边执行（已知边在 `add_dependency`
开头短路）。违规报告一律登记进栈上定长队列 `PendingReports`，`LOCK_DEP_MAP` guard 释放后才发射，
因此 lockdep 不再在全局锁临界区内写控制台——与 `LOG_EMIT_LOCK` 的「发射点不持其它全局锁」立场一致。
`stats()`（经 `functions::sync::lockdep` 安全代理转发）给出六个读数：`acquire_ops` / `map_sections` /
`edge_attempts` / `new_edges` 四个记账读数加两个**粘滞失效读数** `class_table_dropped` /
`held_stack_overflowed`（只增不减，只在 `LOCK_DEP_MAP` guard 内更新）。前两者相等即「每 op 单次临界区」
未退化，`new_edges` 远小于 `edge_attempts` 即「稳态不进 BFS」未退化；末两项非零即「本轮 lockdep 覆盖面
有损」——类表满丢弃注册与持有栈溢出都不再静默（A-1 前置 ①/③ 已裁为「显式失败」），但显式失败的出口是
**读数而不是 panic**：锁的语义与返回值一律不受影响。结构判据由 `host-tests/tests/lockdep_low_overhead_test.rs`
锁住（源码契约面，非计时读数）。

消费面（A-1 前置 ② 已裁为「接一个真实消费者」）：内核在进用户态前的最后一点——`boot_syscall_and_launch`
里晚于 boot 栈 canary 复核、早于 `launch_first_user_process`——由 `report_lockdep_coverage()` 打一行
`[LOCKDEP] coverage classes=… violations=… detected=… acquire_ops=… new_edges=… class_table_dropped=…
held_stack_overflowed=…`，取值同样走 `functions::sync::lockdep` 安全代理。`scripts/qemu_boot_test.sh`
双架构分支各消费一次，判据是**四项硬零**（`detected=false` / `violations=0` / 两个粘滞失效读数为 0），
非零或读数行缺失即 `RESULT=1` 且不受 `FAIL_OK` 放宽。`classes=` 与两个记账计数不参与判定：出货的
默认 release 构建（A-1 常开，含 lockdep）与 `debug_assertions` 构建里 `classes` 大于 0，仅
`--no-default-features` 的 release 构建恒为 0，同一硬零判据要在两种形态下同形。接线形状由
`host-tests/tests/lockdep_coverage_boot_line_test.rs` 锁住。

门控关闭的收益只在「调用点不编译」这一层，**体积不省**：lockdep 模块本体仍参与编译（内部只有
per-CPU 持栈与依赖图段被同一掩码 cfg 掉），而内核链接是 `--whole-archive` 且 `LDFLAGS` 无
`--gc-sections`，归档目标文件全量入镜像——A-1 常开后该形态只由 `--no-default-features` 构建取得，
其归档里 `lockdep::acquire` 仍是 `T` 导出符号。实测读数与后续处置见
`docs/plan/multithreading-project.md` 的 A-1 详情。

## 参考

- Linux kernel `Documentation/locking/lockdep-design.txt`
- FreeBSD witness（`sys/kern/subr_witness.c`）
- `privileged/sync/lockdep.rs`（运行时检测器实现）
