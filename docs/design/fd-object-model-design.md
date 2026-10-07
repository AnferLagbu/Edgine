# fd 对象模型设计立场文档

> 本文是 Edgine fd（file descriptor）子系统的设计立场文档：回答"fd 要演化成什么、为什么这么选"，不是任务清单。可执行分解见 `docs/plan/fd-object-model-unification.md`；现状机制说明见 `docs/explain/` 相关章节。根因与证据快照见 `docs/report/context-consistency-audit.md`。

## 一、问题陈述

Edgine 当前的 fd 是**全局编号 + 数值区间分段**的过渡模型：`FdPlan`（`privileged/proc/fd_alloc.rs`）为 8 个非 VFS 子系统（Smoltcp/UDS/EventFd/SignalFd/Inotify/TimerFd/PidFd/UserFaultFd）各划一段全局 fd 区间，syscall 层靠 `subsystem_of(fd)` 的数值区间判定来分发。而文件系统侧走的是另一套 per-process 模型：`FdTable`（每进程 `[0, 64)`）→ handle → `OpenFileTable` → `OpenFile { Arc<dyn Inode>, offset, flags, ... }`。两套模型并存造成一组结构性问题：

- **边界松动（I3/I4 类）**：非 VFS fd 全局有效且与进程无关——任何进程可引用任何其他进程的 socket fd，dup 到 socket、CLOEXEC、进程退出回收全部缺失。
- **分发散点化**：每个 syscall（close/ioctl/poll/fcntl/read/write…）都要手写一遍区间判定；方案 C（Smoltcp 段基址 64）之后 `subsystem_of` 判定开始趋于一致，但本质上是把"数值即类型"的耦合固化。
- **对象语义缺位**：socket/eventfd 等对象没有统一的 read/write/poll/ioctl/release 接口，`Inode` trait 是文件系统中心的（stat/truncate/readdir 对 socket 毫无意义），无法直接复用。

终态目标一句话：**fd 是进程私有句柄，指向一个实现统一对象 trait 的内核对象；分发按对象能力而非 fd 数值区间**。这与 Linux 的 `file_operations`、Asterinas 的 `File` trait 同构。

## 二、核心抽象：`File` trait

fd 对象模型的中心的单一 trait，落在 `functions::fs`（100% safe 子树），命名 `File`：

```rust
/// 统一 fd 对象接口 — 与文件系统解耦.
pub trait File: Send + Sync + 'static {
    /// 从对象读取. 非字节流对象返回 Err(EBADF)/按对象语义解释 offset.
    fn read_at(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize>;
    /// 向对象写入.
    fn write_at(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize>;
    /// 就绪查询: 输入请求位, 返回就绪位 (非阻塞, 可重复调用).
    fn poll(&self, events: PollEvents) -> PollEvents;
    /// 对象控制 (ioctl / socket option 统一入口).
    fn io_control(&self, cmd: u32, arg: usize) -> KernelResult<usize>;
    /// 最后一个引用关闭时回收对象资源.
    fn release(&self) {}
    /// 对象类型 (fstat 的 st_mode 派生 + /proc 展示).
    fn file_type(&self) -> FileType;
    /// 名字/描述 (fstat 可读回; socket 为 "socket:[ino]" 形态).
    fn name(&self) -> Option<&str> { None }
    /// 前进型读取 (管道/uart 类一次性流; 默认 `None` = 走 `read_at`).
    /// 吸收 Asterinas `read_direct`/pipe 语义: 有 `Some` 实现时 read 不走 offset.
    fn read_forward(&self, buf: &mut [u8]) -> Option<KernelResult<usize>> { None }
    /// 非 seekable 判据: 为 true 时 `lseek` 返回 ESPIPE.
    fn stream(&self) -> bool { false }
}
```

设计立场与取舍：

- **offset 语义归对象**。`OpenFile` 继续持有共享 `offset`（dup 共享、fork 不共享的 POSIX 语义），但 offset 只对"有位置概念"的对象有意义；socket/pipe 类实现 `stream() = true`，`lseek` 统一在分发层返回 ESPIPE。不为每个对象单独造位置状态。
- **不引入 vtable 裸指针**。Linux 用 C 函数指针表是历史形态；Edgine 用 `Arc<dyn File>` 的动态分发，对象注册即 `Arc::new`，引用计数复用现有 `OpenFile.refcount`，不需要 `get_file/put_file` 生命周期仪式。
- **`Inode` 与 `File` 的关系：叠加适配，不做继承**。文件系统对象经一个薄适配器（`RegularFile(Arc<dyn Inode>, ...)`）实现 `File`（read_at/write_at 直转 Inode，poll 恒就绪，io_control 走 FS_IOC 子集）；`Inode` trait 本身不动——它的目录/元数据语义仍归 VFS 中心。这比"给 Inode 加 poll/ioctl 默认方法"干净：socket 不需要被迫携带 20 多个文件系统方法的默认实现。
- **凭证裁剪**。`read_at/write_at` 保留 `pwm` 参数是 VFS 权限检查的需要；socket 类对象忽略该参数（socket 的访问控制由创建权天然给出，与 Linux 一致）。这是 trait 签名上唯一的妥协点，接受它换取单一入口。

## 三、句柄结构：`OpenFile` 泛化

`OpenFile` 从 `inode: Arc<dyn Inode>` 改为持有 `Arc<dyn File>`：

```
fd (per-process, u32)
 └─ FdTable[fd] = (handle_id, cloexec)
     └─ OpenFileTable[handle_id] = OpenFile { file: Arc<dyn File>, offset, flags, pwm, refcount, file_type }
```

要点：

- **per-process fd 空间统一**。全部对象共用 `FdTable`，fd 数值只在所属进程内有意义；`FdPlan`/`FdSubsystem`/`subsystem_of`/`idx_of` 及 `sm_slot` 类适配层整体退役。全局位图分配器随最后一个非 VFS 子系统迁移后删除。
- **对象索引不再编码进 fd 数值**。socket 的内部槽位（smoltcp `SocketSet` 索引等）是对象自身的私有状态，由 `SocketFile` 结构体持有（`Arc<SocketFile { slot: usize, ... }>`），syscall 层永远看不到槽位。方案 C 引入的 `sm_slot()` 单点换算正是这一迁移的过渡桥：槽位空间已经收敛到单函数，退役它只需替换该函数。
- **`file_type` 由对象提供**（`file_type()` 方法派生），不再由分配点手写常量；`fstat` 对全部 fd 类型统一可用（socket/事件对象给出合法的 `S_IFSOCK`/`S_IFCHR` 形态）。
- **cloexec / dup / fork 矩阵**：

| 操作 | 文件对象 | socket 及其他非文件对象 | 立场依据 |
|---|---|---|---|
| `dup/dup2/dup3` | 共享 OpenFile（offset 共享） | 同一语义，直接支持 | POSIX；终态下"一切皆 fd 对象"，dup 无需按类型分支 |
| `execve` + CLOEXEC | 按 cloexec 位关闭 | 同一机制 | fd 表机制统一后天然获得 |
| `fork` | 复制 fd 表项，共享 OpenFile | 同一机制（socket 随进程继承，Linux 默认语义） | `FdTable::copy_from` 已存在，终态只是覆盖面扩展 |
| 进程退出 | 逐项 close → refcount 归零 → `release()` | 同一机制 | 修复"socket 全局表泄漏至对端进程"的根因路径 |
| `lseek` | 正常 | ESPIPE（`stream()` 判据） | POSIX |

## 四、poll 事件模型

`File::poll` 是统一分发点的关键方法，事件位采用 Linux `<poll.h>` 位值（POLLIN=1 / POLLPRI=2 / POLLOUT=4 / POLLERR=8 / POLLHUP=16 / POLLNVAL=32），理由：**用户态 ABI 兼容优先**——真实用户态软件（curl、busybox）直接按位值使用，任何内部编码都要在 syscall 边界翻译，不如统一采用事实标准。内核内部以 newtype 包裹避免裸 i16 蔓延：

```rust
bitflags::bitflags! { pub struct PollEvents: i16 { const IN = 1; const OUT = 4; /* ... */ } }
```

契约（写进 `File::poll` 的文档注释，实现方必须遵守）：

1. **非阻塞、可重复**：`poll` 只做就绪查询，绝不睡眠；`events` 输入是请求位掩码，返回值必须是其子集（POLLERR/POLLHUP 例外，可无条件附加）。
2. **无副作用**：poll 不得消费数据、不得推进状态机（TCP 状态观察除外）。
3. **锁纪律**：`poll` 内部可短暂持对象自有锁（如 NET_STATE），但不得持锁跨 syscall 返回；poll 等待（睡眠）不属于本方法，归 P5 阻塞语义的等待队列工程——`File::poll` 就是届时"唤醒后重查"的谓词。

P4 的 `sm_socket_poll(fd, events) -> i16`（D8b 方案）是本模型的**第一块垫脚石**：签名即 `File::poll` 的"数值 fd 前影"，位值、契约完全一致。终态迁移时 `poll_syscall` 的改动只有一处——把"区间判定 → `poll_fd`"换成"fd 表查找 → `file.poll()`"，事件模型与调用面零改写。

## 五、业界对照与独立取舍

| 维度 | Linux `file_operations` | Asterinas `File` trait | Edgine 立场 |
|---|---|---|---|
| 分发形态 | C 函数指针表 + `inode->i_fop` | Rust `Arc<dyn File>` + `FileLike` 双层 | 取 Asterinas 形态（类型安全、无生命周期仪式）；不取 Linux 的"read 与 read_iter 双轨"（当前无 iocb 批量异步需求，保留 offset 参数化即可） |
| 与文件系统关系 | 文件与 pipe/socket 同为 `struct file` | `File`（字节流对象）与 `Inode` 分离，`GenericFile` 桥接 | 同 Asterinas：`Inode` 保持文件系统中心，`File` 为 fd 对象统一面，薄适配器桥接（`RegularFile`） |
| poll | `poll_table` + 等待队列回调 | `poll_mask`（无回调，快照式） | 取 Asterinas 快照式——Edgine 无 poll_table 基础设施，快照式与 P5 等待队列可独立组合 |
| ioctl | `unlocked_ioctl`/`compat_ioctl` 双入口 | `IoctlCmd` trait 化命令号 | 单一 `io_control`；命令号按 Linux ABI 取值，`compat` 不存在（无 32 位兼容层计划） |
| 生命周期 | f_count + `release` 回调 | `Arc<dyn File>` 引用计数 + `Drop` | Arc 计数 + 显式 `release()`（POSIX close 错误语义需要"关闭动作"而非仅 Drop；Drop 不返回错误） |

判据来源区分清楚：位值与错误码是**用户态 ABI 事实标准**（必须兼容）；trait 方法集与桥接结构是**内部工程选择**（可独立演化）。Edgine 不追求 `file_operations` 的全量对齐（如 mmap/fsync/fasync 按需求驱动后置），方法集以 P4-P6 已暴露的语义需求 + dup/fork/exec 矩阵为下限。

## 六、演化路径与开放议题

落地分四阶段（任务分解与验收见 plan 文档）：**设计定稿（本文档）→ 抽象落地（`File` trait + `RegularFile` 适配器 + VFS 单分发点）→ 8 个子系统逐个迁移（每子系统一 PR + 回归）→ FdPlan/换算层退役**。顺序立场：VFS 先行迁移（存量最大、语义最标准，作为抽象的试金石），随后按"迁移成本低→高"排 UDS/EventFd/…/Smoltcp——socket 最后是因为它是唯一带全局槽位表 + 中断侧 poll 的子系统，改造面最大。

开放议题（实施到对应阶段前须由用户决策，此处只锚定问题）：

- **socket 的 fd 号可见性剧变**。终态下 socket fd 进入 per-process `[0,64)`，现有 e2e 断言与用户态程序对 fd 数值的假设（如 65 形态）全部失效——测试判据必须从"具体数值"改为"去重/区间无关"表述（P3 的 fd 去重断言已是此形态）。
- **`MAX_FDS_PER_PROCESS = 64` 是否够用**。全局位图退役后 fd 上限收敛为每进程 64；业界通常 1024 起步。扩容涉及 FdTable 静态数组 → 动态结构的机制改动，归 privileged。
- **epoll 与 `File::poll` 的关系**。快照式 poll 支撑 edge-triggered epoll 需要注册表（关注列表 + 唤醒回调），是独立子系统工程，不在本抽象内预支。
- **`Arc<dyn File>` 的 no_std 依赖**。对象表全面依赖堆（Arc/Vec），现行 `OpenFileTable` 已如此，无新增风险，但 `Box`/`Arc` 分配失败路径（fd 耗尽 vs OOM）语义需在阶段 2 明确。
- **与 P5 阻塞语义的组合**。`File::poll` 契约为等待-重查谓词，等待队列归 P5；两工程交界面只有这一个谓词，耦合面刻意最小。
