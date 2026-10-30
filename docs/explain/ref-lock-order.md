# 全局锁顺序

> Edgine 内核锁获取的全局顺序约定。违反此顺序将导致 AB-BA 死锁。
> 运行时检测由 `privileged/sync/lockdep.rs`（Lockdep）提供。

## 锁层级（从高到低）

获取锁时必须按层级编号从小到大获取。同一层级内的锁可以任意顺序获取。

层级编号的**方向语义**：0 是最靠内核底层的域（中断安全、恢复路径），14 是最靠外设的域。
"从小到大"等价于"先取高层级域、再取低层级域"；一旦持有了编号大的锁，就不得回头去取编号小的锁。
唯一成体系的例外是调度域，见下文「调度域的锁域所有权」。

```
层级 0: FREG / Recovery（中断安全）
  RECOVERY_MANAGER, ROLLBACK_LOG, RESET_AUDIT_LOG, DEVICE_SNAPSHOTS

层级 1: PMM（物理内存）
  PMM_SNAPSHOT, buddy allocator 内部锁

层级 2: Slab / Kmalloc（内核堆）
  slab free-list 锁, kmalloc 内部锁

层级 3: VMM / VMA（虚拟内存）
  VMM 页表锁, VMA 链表锁（MmStruct::vmas）

层级 4: Page Cache（页缓存）
  PAGE_CACHE[hash_bucket] 锁

层级 5: COW（写时复制）
  COW_REFS 锁

层级 6: Swap（换页）
  SWAP.lock

层级 7: Process Table（进程表）
  PROC_SNAPSHOT, ProcessTable::processes 锁

层级 8: Scheduler（调度器）
  PER_CPU_SCHED[idx] 锁, CFS/RT/DL 队列锁

层级 9: 进程属性
  sigaction_table, rlimit_table, seccomp.filters, namespaces, numa_policy, children

层级 10: POSIX Timer
  TIMER_MANAGER 锁

层级 11: VFS / FS（文件系统）
  VFS 全局锁, 文件系统特定锁（EGFS, ramfs, devfs）

层级 12: Network（网络）
  NET_LOCK, socket 锁, NET_SNAPSHOT_LOCK

层级 13: Driver（驱动）
  设备特定锁（AHCI/NVMe/E1000/VirtIO/Keyboard 等）

层级 14: Block Device Registry（块设备注册表）
  REGISTRY, DEVICE_NAMES, IO_REFS, REMOVING
```

## 规则

1. **层级递增**：获取锁时，层级编号必须 >= 当前持有的最高层级锁。
2. **中断上下文**：仅 SpinLock 和 IrqSpinLock 可在中断上下文获取。Mutex/PiMutex 可能 yield，禁止在中断中使用。
3. **递归禁止**：同一线程不可重复获取同一非递归锁（SpinLock/Mutex/RwLock）。
4. **持锁禁分配**：自旋锁持有期间禁止内存分配（kmalloc/slab）和调度（yield/sleep）。

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

## 常见路径示例

### 缺页处理（Page Fault）

```
层级 3: MmStruct::vmas.lock()  →  查找 VMA
层级 1: PMM 分配页帧           →  buddy allocator
层级 3: VMM 映射页表           →  页表锁
层级 4: Page Cache 插入         →  PAGE_CACHE[hash].lock()
```

### 进程创建（Fork）

```
层级 7: ProcessTable::processes.lock()  →  分配 PID
层级 3: MmStruct::vmas.lock()          →  克隆 VMA
层级 1: PMM 分配页帧                    →  COW 页表克隆
层级 8: scheduler.lock()                →  注册到运行队列
层级 9: parent.children.lock()          →  添加子进程
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

### 跨核搬动（负载均衡 / 唤醒投送）

```
层级 8: per_cpu_for(min).cfs_rq.lock()   →  归属核先取 (cpu index 升序)
层级 8: per_cpu_for(max).cfs_rq.lock()   →  第二把同层级锁, 定序见上节
层级 7: PROCESS_TABLE.with_process()     →  借引用读快照 / 写 Ready (rq → PT)
层级 8: 提交归属, 释放两把 rq 锁          →  变更全程不放锁
```

## Lockdep 集成

Lockdep（`privileged/sync/lockdep.rs`）在 `debug_assertions` 或 `feature = "lockdep"` 启用时，
跟踪每个锁的获取/释放，构建锁序图，检测：

- **AB-BA 死锁**：线程 A 持锁 L1 再获取 L2，线程 B 持锁 L2 再获取 L1
- **中断上下文睡眠**：在硬中断中获取 Mutex
- **递归获取**：同一线程对同一锁重复 lock
- **释放未持有的锁**：unlock 时当前线程并非持有者

Lockdep 类通过 `SpinLock::named()` / `Mutex::named()` / `RwLock::named()` 自动注册，
无需手动维护锁类 ID。

## 参考

- Linux kernel `Documentation/locking/lockdep-design.txt`
- FreeBSD witness（`sys/kern/subr_witness.c`）
- `privileged/sync/lockdep.rs`（运行时检测器实现）
