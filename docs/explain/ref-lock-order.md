# 全局锁顺序

> Edgine 内核锁获取的全局顺序约定。违反此顺序将导致 AB-BA 死锁。
> 运行时检测由 `privileged/sync/lockdep.rs`（Lockdep）提供。

## 锁层级（从高到低）

获取锁时必须按层级编号从小到大获取。同一层级内的锁可以任意顺序获取。

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
  VFS 全局锁, 文件系统特定锁（UNKFS, ramfs, devfs）

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
