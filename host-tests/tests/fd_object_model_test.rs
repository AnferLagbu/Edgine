//! fd 对象模型阶段 2 契约: `File` trait 抽象落地与 `OpenFile` 解耦.
//!
//! - 行为面: host 侧直接驱动 `functions::fs::file` / `OpenFileTable`;
//! - 分发面: 源码形状锁固定 syscall / VFS 消费点经 `File` 对象分发.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use edgine::kernel::functions::fs::file::{File, PollEvents, RegularFile};
use edgine::kernel::functions::fs::inode::AnonymousInode;
use edgine::kernel::functions::fs::open_file_table::OpenFileTable;
use edgine::kernel::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};

fn read_file(relative_path: &str) -> String {
    let path = format!(
        "{}/../src/kernel/{}",
        env!("CARGO_MANIFEST_DIR"),
        relative_path
    );
    fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {relative_path}"))
}

/// 最小 `File` 实现: 固定类型 + 可配置就绪位, 用于验证构造派生与默认方法.
struct StubFile {
    file_type: VfsFileType,
    poll_mask: PollEvents,
}

impl File for StubFile {
    fn read_at(&self, _offset: u64, _buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        Ok(0)
    }

    fn write_at(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        Ok(buf.len())
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        events & self.poll_mask
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn file_type(&self) -> VfsFileType {
        self.file_type
    }
}

/// 携带 `release` 计数的 `File` 实现, 验证引用归零回调.
struct ReleaseProbe {
    released: AtomicU32,
}

impl File for ReleaseProbe {
    fn read_at(&self, _offset: u64, _buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        Ok(0)
    }

    fn write_at(&self, _offset: u64, _buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        Ok(0)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        events
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn release(&self) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

// ============================================================================
// 行为面
// ============================================================================

#[test]
fn poll_event_bits_match_linux_poll_h() {
    // 用户态 ABI 兼容: 内部编码与 Linux <poll.h> 位值一致, syscall 边界零翻译.
    assert_eq!(PollEvents::IN.bits(), 1, "POLLIN");
    assert_eq!(PollEvents::PRI.bits(), 2, "POLLPRI");
    assert_eq!(PollEvents::OUT.bits(), 4, "POLLOUT");
    assert_eq!(PollEvents::ERR.bits(), 8, "POLLERR");
    assert_eq!(PollEvents::HUP.bits(), 16, "POLLHUP");
    assert_eq!(PollEvents::NVAL.bits(), 32, "POLLNVAL");
}

#[test]
fn open_file_from_file_derives_type_and_non_inode_sentinels() {
    let file = Arc::new(StubFile {
        file_type: VfsFileType::Dev,
        poll_mask: PollEvents::IN,
    });
    let of = OpenFile::from_file(file, 0, 0);
    assert_eq!(
        of.file_type,
        VfsFileType::Dev.as_u8(),
        "file_type 必须由对象 file_type() 派生"
    );
    assert_eq!(of.inode_id(), 0, "非文件对象 inode_id 走 0 哨兵");
    assert_eq!(of.mount_idx(), u32::MAX, "非文件对象挂载点走 u32::MAX 哨兵");
    assert!(of.as_inode().is_none(), "非文件对象 as_inode 必须为 None");
    assert!(!of.file().stream(), "默认对象可定位 (非流)");
    assert!(
        of.file().read_forward(&mut [0u8; 4]).is_none(),
        "默认对象不提供前进型读取"
    );
}

#[test]
fn open_file_new_wraps_regular_file_over_inode() {
    let of = OpenFile::new(Arc::new(AnonymousInode::new(7)), 0, 0);
    assert_eq!(
        of.file_type,
        VfsFileType::File.as_u8(),
        "RegularFile 类型由 Inode::stat 派生"
    );
    assert_eq!(of.inode_id(), 7, "as_inode 桥接回 Inode::node_id");
    assert_eq!(of.mount_idx(), u32::MAX, "匿名 inode 无挂载点");
    assert!(of.as_inode().is_some(), "RegularFile 必须桥接回 Inode");
}

#[test]
fn regular_file_poll_reports_requested_io_bits() {
    let rf = RegularFile::new(Arc::new(AnonymousInode::new(9)), 0);
    let ready = rf.poll(PollEvents::IN | PollEvents::OUT | PollEvents::ERR);
    assert_eq!(
        ready,
        PollEvents::IN | PollEvents::OUT,
        "常规对象内容常驻内存: 恒报请求的 IN|OUT, 不附加 ERR/HUP"
    );
}

#[test]
fn open_file_table_release_runs_once_after_last_ref() {
    let table = OpenFileTable::new();
    let probe = Arc::new(ReleaseProbe {
        released: AtomicU32::new(0),
    });
    let handle = table
        .alloc(OpenFile::from_file(probe.clone(), 0, 0))
        .expect("空表必须可分配 handle");
    table.inc_ref(handle);
    table.dec_ref(handle);
    assert_eq!(
        probe.released.load(Ordering::SeqCst),
        0,
        "仍有引用时不得调用 release"
    );
    table.dec_ref(handle);
    assert_eq!(
        probe.released.load(Ordering::SeqCst),
        1,
        "最后一个引用关闭时 release 恰一次"
    );
    table.dec_ref(handle);
    assert_eq!(
        probe.released.load(Ordering::SeqCst),
        1,
        "重复 close 不得二次 release"
    );
}

// ============================================================================
// 源码形状锁: 抽象定义与消费点
// ============================================================================

#[test]
fn file_trait_defined_with_full_contract() {
    let src = read_file("functions/fs/file.rs");
    assert!(
        src.contains("pub trait File: Send + Sync + 'static"),
        "File trait 必须定义在 functions/fs/file.rs"
    );
    for needle in [
        "fn read_at(",
        "fn write_at(",
        "fn poll(",
        "fn io_control(",
        "fn release(",
        "fn file_type(",
        "fn read_forward(",
        "fn stream(",
        "fn as_inode(",
    ] {
        assert!(src.contains(needle), "File trait 缺契约方法: {needle}");
    }
    assert!(
        src.contains("#![deny(unsafe_code)]"),
        "functions 侧 File trait 必须 #![deny(unsafe_code)]"
    );
}

#[test]
fn pipe_endpoints_implement_forward_stream_file() {
    let src = read_file("functions/fs/pipe_fd.rs");
    assert!(
        src.contains("impl File for PipeEndpointInode"),
        "管道端点必须实现统一 File 接口"
    );
    assert!(
        src.contains("fn read_forward("),
        "管道读必须提供前进型读取 (不依赖 offset)"
    );
    assert!(
        src.contains("fn stream(&self) -> bool"),
        "管道必须声明流式对象 (lseek → ESPIPE)"
    );
    assert!(
        src.contains("Some(Inode::read(self, 0, buf, 0))"),
        "管道 read_forward 必须直转 Inode::read"
    );
}

#[test]
fn syscall_dispatch_uses_file_object() {
    let ioctl = read_file("functions/fs/file_ops.rs");
    assert!(
        ioctl.contains("of.file().io_control("),
        "ioctl 的 VFS 分支必须经 File::io_control 分发"
    );
    assert!(
        ioctl.contains("KernelError::NotSupported"),
        "对象未实现 io_control 时必须回落默认策略"
    );
    assert!(
        ioctl.contains("of.file().poll("),
        "poll 的 VFS 分支必须经 File::poll 快照"
    );
    assert!(
        !ioctl.contains("POLLIN") && !ioctl.contains("POLLOUT"),
        "poll/ioctl 不得残留本地 POLL* 常量"
    );
}

#[test]
fn fs_mod_reexports_file_abstractions() {
    let src = read_file("functions/fs/mod.rs");
    assert!(src.contains("pub mod file;"), "file 模块必须注册");
    assert!(
        src.contains("pub use file::{File, PollEvents, RegularFile};"),
        "File/PollEvents/RegularFile 必须顶层 re-export"
    );
}

// ============================================================================
// 阶段 3: EventFd 迁移 — 行为面与源码形状锁
// ============================================================================

#[test]
fn eventfd_object_implements_file_contract() {
    use edgine::kernel::functions::fs::eventfd::EventFdFile;

    let efd = EventFdFile::new(5, false);
    let mut buf = [0u8; 8];
    assert_eq!(efd.read_at(0, &mut buf, 0), Ok(8));
    assert_eq!(u64::from_ne_bytes(buf), 5, "读返回计数并清零");
    assert_eq!(
        efd.read_at(0, &mut buf, 0),
        Err(KernelError::WouldBlock),
        "计数清零后读 EAGAIN"
    );

    assert_eq!(efd.write_at(0, &3u64.to_ne_bytes(), 0), Ok(8));
    assert_eq!(
        efd.poll(PollEvents::IN | PollEvents::OUT),
        PollEvents::IN | PollEvents::OUT,
        "计数 > 0 时可读可写"
    );
    assert_eq!(
        efd.write_at(0, &[0u8; 8], 0),
        Err(KernelError::InvalidArgument),
        "write(0) → EINVAL"
    );
    assert!(
        efd.stream(),
        "eventfd 不可定位 (lseek → ESPIPE), 必须声明流式对象"
    );
    assert!(efd.as_inode().is_none(), "eventfd 非 inode 承载");
    assert_eq!(efd.file_type(), VfsFileType::File);

    let sem = EventFdFile::new(3, true);
    for _ in 0..3 {
        assert_eq!(sem.read_at(0, &mut buf, 0), Ok(8));
        assert_eq!(u64::from_ne_bytes(buf), 1, "信号量模式每次读 1");
    }
    assert_eq!(sem.read_at(0, &mut buf, 0), Err(KernelError::WouldBlock));
}

#[test]
fn eventfd_created_via_open_file_table_not_fdplan() {
    let src = read_file("functions/fs/eventfd.rs");
    assert!(
        src.contains("impl File for EventFdFile"),
        "EventFdFile 必须实现统一 File 接口"
    );
    assert!(
        src.contains("OpenFile::from_file"),
        "创建路径必须构造 OpenFile 挂表"
    );
    assert!(
        src.contains("OPEN_FILE_TABLE.alloc"),
        "创建路径必须分配 OpenFileTable 槽位"
    );
    assert!(
        src.contains("t.alloc_fd(handle_id, cloexec)"),
        "用户 fd 必须由 per-process fd 表分配并携带 CLOEXEC"
    );
    assert!(
        !src.contains("FdPlan"),
        "迁移后不得引用全局 FD 编号区间 (FdPlan)"
    );
    assert!(!src.contains("EFD_TABLE"), "迁移后不得保留子系统槽位表");
    let old = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/privileged/syscall/eventfd.rs");
    assert!(
        !old.exists(),
        "privileged 旧槽位实现必须删除 (阶段 3 迁移完成)"
    );
}

#[test]
fn eventfd_special_object_dispatch_sites() {
    // 统一判据: as_inode() == None 的挂表对象走通用 VFS 分发路径.
    let io = read_file("functions/fs/io.rs");
    assert!(
        io.contains("vfs_is_special_fd"),
        "写循环必须用 vfs_is_special_fd 识别特殊对象并短写即停"
    );
    assert!(
        !io.contains("is_eventfd_fd") && !io.contains("EFD_FD_BASE"),
        "io.rs 不得残留 eventfd 数值特判"
    );

    let handle = read_file("functions/fs/handle.rs");
    assert!(
        handle.contains("pub fn vfs_is_special_fd"),
        "handle 必须暴露特殊对象判据"
    );
    assert!(
        handle.contains("fn fd_event_may_change"),
        "读写后唤醒必须覆盖非 inode 特殊对象"
    );
    assert!(
        handle.contains("OPEN_FILE_TABLE.close(handle_id)"),
        "close 必须经 OpenFileTable 回收槽位"
    );
    assert!(
        handle.contains("fd_notify::notify_fd_close"),
        "close 必须唤醒 epoll 等待者"
    );

    let epoll = read_file("privileged/syscall/epoll.rs");
    assert!(
        epoll.contains("fd_poll_special"),
        "epoll 必须经 VfsOps::fd_poll_special 求值特殊对象就绪位"
    );
    assert!(
        !epoll.contains("is_eventfd_fd"),
        "epoll 不得残留 eventfd 数值分支"
    );

    let sendfile = read_file("functions/fs/sendfile.rs");
    assert!(
        sendfile.contains("of.as_inode().is_some()"),
        "sendfile 必须按 as_inode 判据排除非文件对象"
    );
    assert!(
        !sendfile.contains("EFD_FD_BASE") && !sendfile.contains("200..216"),
        "sendfile 不得残留 eventfd 数值区间"
    );

    let ops = read_file("privileged/fs/vfs/ops_trait.rs");
    assert!(
        ops.contains("fn fd_poll_special"),
        "VfsOps 契约必须暴露 fd_poll_special"
    );

    let reexport = read_file("functions/fs/mod.rs");
    assert!(
        reexport.contains("fn fd_poll_special") && reexport.contains("of.file().poll("),
        "FunctionsVfsOps 必须把 fd_poll_special 接到 File::poll"
    );
}
