//! 线程族系统调用常量（clone / 线程组信号 / futex）.
//!
//! 内核 `privileged::syscall` 的 FFI 分派与用户态 `userlib::sys` 的 wrapper
//! 均引用本模块，两侧数值由结构保证一致（同 `sched` / `socket` 模块的单源口径）.
//!
//! 值域基准: Linux x86_64 `<linux/sched.h>`（`CLONE_*`）与 `<linux/futex.h>`
//! （`FUTEX_*`）。aarch64 与 x86_64 同用 asm-generic 值域，故双架构一致.

/// `SYS_CLONE`: 创建进程或线程（可指定共享资源）.
pub const SYS_CLONE: u64 = 56;

/// `SYS_KILL`: 向进程/线程组投递信号.
pub const SYS_KILL: u64 = 62;

/// `SYS_GETTID`: 读取当前线程 ID.
pub const SYS_GETTID: u64 = 186;

/// `SYS_FUTEX`: futex 等待/唤醒.
pub const SYS_FUTEX: u64 = 202;

/// `SYS_EXIT_GROUP`: 终止整个线程组.
pub const SYS_EXIT_GROUP: u64 = 231;

/// `SYS_TGKILL`: 向指定线程组的指定线程投递信号.
pub const SYS_TGKILL: u64 = 234;

// ==================== `clone` 标志位 ====================

/// `CLONE_VM`: 共享地址空间（线程的必要条件）.
pub const CLONE_VM: u64 = 0x0000_0100;

/// `CLONE_FS`: 共享 cwd / 根前缀 / umask.
pub const CLONE_FS: u64 = 0x0000_0200;

/// `CLONE_FILES`: 共享文件描述符表.
pub const CLONE_FILES: u64 = 0x0000_0400;

/// `CLONE_SIGHAND`: 共享信号动作表（`CLONE_THREAD` 的前置）.
pub const CLONE_SIGHAND: u64 = 0x0000_0800;

/// `CLONE_THREAD`: 与调用方同一线程组（子 TID 而非子进程）.
pub const CLONE_THREAD: u64 = 0x0001_0000;

/// `CLONE_SETTLS`: 第 5 个入参为新线程的 TLS 基址.
pub const CLONE_SETTLS: u64 = 0x0008_0000;

/// `CLONE_PARENT_SETTID`: 把子 TID 写入调用方给的 `parent_tidptr`.
pub const CLONE_PARENT_SETTID: u64 = 0x0010_0000;

/// `CLONE_CHILD_CLEARTID`: 子线程退出时清零 `child_tidptr` 并 futex 唤醒.
pub const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;

/// `CLONE_CHILD_SETTID`: 把子 TID 写入子线程自己的 `child_tidptr`.
pub const CLONE_CHILD_SETTID: u64 = 0x0100_0000;

// ==================== futex 操作码 ====================

/// `FUTEX_WAIT`: 值不等立即返回，相等则入睡.
pub const FUTEX_WAIT: i32 = 0;

/// `FUTEX_WAKE`: 唤醒至多 `val` 个等待者.
pub const FUTEX_WAKE: i32 = 1;

/// `FUTEX_REQUEUE`: 把 `val` 个等待者改投到 `timeout_or_uaddr2` 指向的地址.
pub const FUTEX_REQUEUE: i32 = 3;

/// `FUTEX_WAIT_BITSET`: 带位集过滤的等待.
pub const FUTEX_WAIT_BITSET: i32 = 9;

/// `FUTEX_WAKE_BITSET`: 带位集过滤的唤醒.
pub const FUTEX_WAKE_BITSET: i32 = 10;

/// `FUTEX_PRIVATE_FLAG`: 私有 futex 位（同地址空间内等待者）.
pub const FUTEX_PRIVATE_FLAG: i32 = 128;
