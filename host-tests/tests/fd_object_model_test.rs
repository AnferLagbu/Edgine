//! fd 对象模型阶段 2 契约: `File` trait 抽象落地与 `OpenFile` 解耦.
//!
//! - 行为面: host 侧直接驱动 `functions::fs::file` / `OpenFileTable`;
//! - 分发面: 源码形状锁固定 syscall / VFS 消费点经 `File` 对象分发.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use edgine::kernel::functions::fs::file::{File, MmapBacking, PollEvents, RegularFile};
use edgine::kernel::functions::fs::inode::{AnonymousInode, Inode};
use edgine::kernel::functions::fs::open_file_table::OpenFileTable;
use edgine::kernel::functions::fs::vfs_types::{
    KernelError, KernelResult, OpenFile, VfsFileType, VfsSeekWhence, VfsStat,
};

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

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// 最小 `Inode` 实现: stat 报告的节点号 / 挂载点 / 类型可配置.
struct StubInode {
    node_id: u32,
    mount_idx: u32,
    file_type: VfsFileType,
}

impl Inode for StubInode {
    fn read(&self, _offset: u64, _buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        Ok(0)
    }

    fn write(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        Ok(buf.len())
    }

    fn stat(&self, _pwm: u64) -> KernelResult<VfsStat> {
        Ok(VfsStat {
            node_id: self.node_id,
            file_type: self.file_type.as_u8(),
            ..VfsStat::default()
        })
    }

    fn truncate(&self, _size: u64, _pwm: u64) -> KernelResult<()> {
        Ok(())
    }

    fn seek(&self, _offset: i64, _whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        Ok(current_offset)
    }

    fn is_dir(&self) -> bool {
        self.file_type == VfsFileType::Dir
    }

    fn node_id(&self) -> u32 {
        self.node_id
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
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

    fn as_any(&self) -> &dyn core::any::Any {
        self
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
    assert_eq!(of.inode_id(), None, "非 inode 对象 inode_id 必须为 None");
    assert!(
        of.file().mmap_backing().is_none(),
        "默认对象未声明映射后端, 不可作文件映射"
    );
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
    assert_eq!(of.inode_id(), Some(7), "as_inode 桥接回 Inode::node_id");
    assert_eq!(
        of.file().mmap_backing(),
        Some(MmapBacking {
            inode_id: 7,
            mount_idx: None,
        }),
        "映射后端由 Inode 派生; 匿名 inode 的 u32::MAX 挂载哨兵必须收口为 None"
    );
    assert!(of.as_inode().is_some(), "RegularFile 必须桥接回 Inode");
}

#[test]
fn regular_file_mmap_backing_gates_type_and_mount_sentinel() {
    let regular = RegularFile::new(
        Arc::new(StubInode {
            node_id: 42,
            mount_idx: 3,
            file_type: VfsFileType::File,
        }),
        0,
    );
    assert_eq!(
        regular.mmap_backing(),
        Some(MmapBacking {
            inode_id: 42,
            mount_idx: Some(3),
        }),
        "常规文件必须声明映射后端并透传挂载点"
    );

    for file_type in [VfsFileType::Dir, VfsFileType::Dev, VfsFileType::Symlink] {
        let non_file = RegularFile::new(
            Arc::new(StubInode {
                node_id: 7,
                mount_idx: 0,
                file_type,
            }),
            0,
        );
        assert_eq!(
            non_file.mmap_backing(),
            None,
            "非普通文件不可作页缓存后端: {file_type:?}"
        );
    }
}

#[test]
fn regular_file_declares_namespace_id_for_all_real_inode_types() {
    // inotify 的 IN_CLOSE_* 对目录同样有意义, 故命名空间键不按 VfsFileType
    // 收窄 (那会误伤目录); 与 mmap_backing 的「仅普通文件可映射」判据分层.
    for file_type in [
        VfsFileType::File,
        VfsFileType::Dir,
        VfsFileType::Dev,
        VfsFileType::Symlink,
    ] {
        let regular = RegularFile::new(
            Arc::new(StubInode {
                node_id: 9,
                mount_idx: 0,
                file_type,
            }),
            0,
        );
        assert_eq!(
            regular.inode_namespace_id(),
            Some(9),
            "真实 inode 均为 pcache / inotify 命名空间成员: {file_type:?}"
        );
    }
}

#[test]
fn inode_id_is_option_not_zero_sentinel() {
    // 0 是 pcache / inotify 命名空间的真实键. 特殊对象 (eventfd / signalfd)
    // 曾以 0 哨兵冒充 inode, 使 VFS 读快路径命中 inode 0 的页缓存 — 特殊
    // 对象读会把他人文件数据当成自己的读结果返回. 类型化 Option 后所有
    // inode 命名空间消费点被编译器强制处理 None.
    let vfs_types = read_file("functions/fs/vfs_types.rs");
    assert!(
        vfs_types.contains("pub fn inode_id(&self) -> Option<u32>"),
        "inode_id 必须返回 Option<u32> (0 不可作「无 inode」哨兵)"
    );
    assert!(
        !vfs_types.contains("pub fn inode_id(&self) -> u32"),
        "不得回退到裸 u32 + 0 哨兵"
    );

    let handle = read_file("functions/fs/handle.rs");
    assert!(
        handle.contains("pcache_fast_target(open_file.file().inode_namespace_id(), count, offset)"),
        "pcache 快路径必须经对象声明的 inode 命名空间键 (Option 解构)"
    );
    assert!(
        handle.contains("fn pcache_fast_target(node_id: Option<u32>"),
        "快路径判据签名必须接收 Option<u32>"
    );
    assert!(
        !handle.contains(".unwrap_or((0, 0))"),
        "close 快照不得把无 inode 退化为 0 (旧哨兵形态)"
    );
}

#[test]
fn inode_namespace_key_is_object_declared() {
    // 管道端点的 node_id 是合成值 (0xF000_0000 + key), 但 as_inode() = Some,
    // 所以 OpenFile::inode_id() 对它返回 Some(伪值). pcache / inotify 的键必须
    // 由对象自行声明命名空间成员身份, 不能以「有 Inode」推断.
    let file = read_file("functions/fs/file.rs");
    assert!(
        file.contains("fn inode_namespace_id(&self) -> Option<u32> {"),
        "File trait 必须暴露 inode 命名空间键 (默认不声明)"
    );
    assert!(
        file.contains("Some(self.inode.node_id())"),
        "RegularFile 的真实 inode 必须声明命名空间键"
    );

    let handle = read_file("functions/fs/handle.rs");
    assert!(
        handle.contains("of.file().inode_namespace_id(), of.get_flags()"),
        "close 的 pcache 失效 / inotify 键必须来自对象声明"
    );
    assert_eq!(
        handle
            .matches("let node_id = open_file.file().inode_namespace_id();")
            .count(),
        2,
        "write / pwrite 的 inotify IN_MODIFY 键必须来自对象声明"
    );
    assert!(
        !handle.contains("open_file.inode_id()") && !handle.contains("of.inode_id()"),
        "命名空间消费者不得回退到 OpenFile::inode_id (伪 inode 会混入命名空间)"
    );

    for special in [
        "functions/fs/pipe_fd.rs",
        "functions/fs/eventfd.rs",
        "functions/fs/signalfd.rs",
    ] {
        assert!(
            !read_file(special).contains("fn inode_namespace_id"),
            "伪 inode 对象不得声明命名空间键: {special}"
        );
    }
}

#[test]
fn inotify_parent_lookup_has_no_zero_fallback() {
    // 父目录解析失败曾以 0 兜底继续发 inotify: 0 是真实 inode 键, 会把
    // IN_CREATE / IN_DELETE 错误投递给 inode 0 的 watch. 解析失败必须不发事件.
    for path in ["functions/fs/vfs_path.rs", "functions/fs/handle.rs"] {
        let src = read_file(path);
        assert!(
            !src.contains("fs_resolve_path(parent_path).unwrap_or(0)"),
            "父目录 inotify 键不得以 0 兜底: {path}"
        );
        assert!(
            src.contains("if let Some(parent_ino) = ") || src.contains("fs_opt.and_then("),
            "父目录解析失败分支必须跳过 inotify: {path}"
        );
    }
}

#[test]
fn mmap_requires_object_declared_backing() {
    // 管道 / eventfd / signalfd 同样报告 VfsFileType::File, 仅凭 file_type 无法
    // 把伪 inode 命名空间挡在 pcache 之外. 判据改为对象自行声明的映射后端
    // 凭据: 未声明 = 不可映射 (ENODEV), 而非由 fd 数值区间或类型猜测.
    let mmap = read_file("functions/mm/mmap.rs");
    assert!(
        mmap.contains("pub fn fd_mmap_backing(fd: i32) -> Result<MmapBacking, Errno>"),
        "mmap 的映射后端来源必须是对象声明的 MmapBacking"
    );
    assert!(
        mmap.contains("let backing = fd_mmap_backing(req.fd)?;"),
        "mmap 文件映射必须经凭据解析, 不可绕过对象声明"
    );
    assert!(
        mmap.contains(".ok_or(Errno::Enodev)"),
        "对象未声明映射后端时必须 ENODEV (对齐 Linux 非普通文件语义)"
    );
    assert!(
        !mmap.contains("fd_to_inode_id") && !mmap.contains("fd_to_mount_idx"),
        "旧的裸 inode / 挂载点直读入口必须删除 (无法区分伪 inode 命名空间)"
    );

    let file = read_file("functions/fs/file.rs");
    assert!(
        file.contains("fn mmap_backing(&self) -> Option<MmapBacking>"),
        "File trait 必须暴露映射后端凭据 (默认不可映射)"
    );
    assert!(
        file.contains("self.file_type != VfsFileType::File"),
        "RegularFile 只放行常规文件内容"
    );
    assert!(
        file.contains("if mount == u32::MAX"),
        "Inode::mount_idx 的 u32::MAX 哨兵必须在凭据处收口为 None"
    );

    for special in [
        "functions/fs/pipe_fd.rs",
        "functions/fs/eventfd.rs",
        "functions/fs/signalfd.rs",
    ] {
        assert!(
            !read_file(special).contains("fn mmap_backing"),
            "特殊对象不得声明映射后端 (伪 inode 会污染 pcache 命名空间): {special}"
        );
    }
}

#[test]
fn dup2_release_shares_close_chain_with_close() {
    let handle = read_file("functions/fs/handle.rs");
    assert!(
        handle.contains("fn release_open_file(handle_id: u32, fd_idx: u32)"),
        "关闭副作用必须收口到共享入口 release_open_file"
    );
    assert!(
        handle.contains("release_open_file(handle_id, fd_idx);"),
        "close 路径必须走共享关闭链"
    );

    // dup2 覆盖分支: 必须复用关闭链, 不得直连 OpenFileTable::close
    let dup2_start = handle
        .find("pub extern \"C\" fn vfs_dup2")
        .expect("vfs_dup2 必须存在");
    let dup2_body = &handle[dup2_start..];
    let next_fn = dup2_body[40..]
        .find("pub fn ")
        .or_else(|| dup2_body[40..].find("pub extern \"C\" fn "))
        .map_or(dup2_body.len(), |i| 40 + i);
    let dup2_body = &dup2_body[..next_fn];
    assert!(
        dup2_body.contains("release_open_file(prev_hid, newfd);"),
        "dup2 覆盖旧 fd 必须经共享关闭链 (pcache/inotify/fd_notify)"
    );
    assert!(
        !dup2_body.contains("OPEN_FILE_TABLE.close(prev_hid)"),
        "dup2 不得直连 OpenFileTable::close 绕过关闭副作用"
    );
}

#[test]
fn vma_inode_id_is_option_not_zero_sentinel() {
    // 0 是 inode 命名空间的真实键, 故 VMA 的文件后端同为 Option:
    // 匿名 VMA 的「无后端」由类型表达, 消费点经编译器强制解构.
    let vma = read_file("privileged/mm/vma.rs");
    assert!(
        vma.contains("pub inode_id: Option<u32>"),
        "Vma 文件后端必须用 Option<u32> (0 不可作「无后端」哨兵)"
    );
    assert!(
        vma.contains("inode_id: None,"),
        "匿名 VMA 构造必须显式无文件后端"
    );
    assert!(
        vma.contains("inode_id: Some(inode_id),"),
        "file_backed 构造器必须把 inode 包成 Some"
    );
    assert!(
        !vma.contains("pub fn with_offset("),
        "with_offset 是唯一不带 inode 的 FileBacked 构造器, 已删除"
    );

    let page_fault = read_file("privileged/mm/page_fault.rs");
    assert!(
        page_fault.contains("let Some(inode_id) = vma.inode_id"),
        "文件缺页分发必须以 None 判非文件后端 (不得沿用 != 0)"
    );
    assert!(
        page_fault.contains("handle_file_fault(mm, vma, inode_id, info, aligned, user_cr3)"),
        "handle_file_fault 必须接收已解构的 inode_id (内部不再读字段)"
    );

    let mmap = read_file("functions/mm/mmap.rs");
    assert!(
        mmap.contains("let Some(inode_id) = vma.inode_id else {"),
        "munmap 释放路径必须解构 Option, 不得保留 == 0 防御哨兵"
    );
    assert!(!mmap.contains("vma.inode_id == 0"), "不得回退到 0 哨兵判据");
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
        "fn mmap_backing(",
        "fn inode_namespace_id(",
        "fn as_any(",
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
        src.contains("pub use file::{File, MmapBacking, PollEvents, RegularFile};"),
        "File/MmapBacking/PollEvents/RegularFile 必须顶层 re-export"
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

// ============================================================================
// 阶段 3 第 2 个迁移子系统: SignalFd — 行为面与源码形状锁
// ============================================================================

#[test]
fn signalfd_object_implements_file_contract() {
    use edgine::kernel::functions::fs::signalfd::{
        SFD_IOC_SETMASK, SIGNALFD_SIGINFO_SIZE, SignalFdFile,
    };

    // host 侧无进程上下文 (current pid == 0), 与绑定 pid 0 相同 ⇒ pending 恒 0
    let sfd = SignalFdFile::new(0, 0);
    assert_eq!(
        sfd.io_control(
            SFD_IOC_SETMASK,
            (1usize << 9) | (1usize << 8) | (1usize << 18)
        ),
        Ok(0),
        "改掩码命令应成功 (不可屏蔽信号在对象内过滤)"
    );
    assert!(
        sfd.poll(PollEvents::IN).is_empty(),
        "无 pending 信号时不可读"
    );
    assert_eq!(
        sfd.poll(PollEvents::ERR),
        PollEvents::empty(),
        "绑定进程内不附加 ERR"
    );
    assert_eq!(
        sfd.write_at(0, &[0u8; SIGNALFD_SIGINFO_SIZE], 0),
        Err(KernelError::InvalidArgument),
        "signalfd 只读, 写入返回 EINVAL"
    );
    assert_eq!(
        sfd.read_at(0, &mut [0u8; 64], 0),
        Err(KernelError::InvalidArgument),
        "缓冲区小于 signalfd_siginfo → EINVAL"
    );
    assert_eq!(
        sfd.io_control(0x1234_5678, 0),
        Err(KernelError::NotSupported),
        "未知控制命令回落默认策略"
    );
    assert!(sfd.stream(), "signalfd 不可定位 (lseek → ESPIPE)");
    assert!(sfd.as_inode().is_none(), "signalfd 非 inode 承载");
    assert_eq!(sfd.file_type(), VfsFileType::File);

    let foreign = SignalFdFile::new(1 << 9, 42);
    assert_eq!(
        foreign.poll(PollEvents::IN | PollEvents::ERR),
        PollEvents::ERR,
        "非绑定进程 (fork 继承 fd) 按无效 fd 报 ERR"
    );
}

#[test]
fn signalfd_created_via_open_file_table_not_fdplan() {
    let src = read_file("functions/fs/signalfd.rs");
    assert!(
        src.contains("impl File for SignalFdFile"),
        "SignalFdFile 必须实现统一 File 接口"
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
    assert!(!src.contains("SFD_TABLE"), "迁移后不得保留子系统槽位表");
    let old = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/privileged/syscall/signalfd.rs");
    assert!(
        !old.exists(),
        "privileged 旧槽位实现必须删除 (阶段 3 迁移完成)"
    );
}

#[test]
fn signalfd_special_object_dispatch_sites() {
    let io = read_file("functions/fs/io.rs");
    assert!(
        io.contains("vfs_is_special_fd"),
        "写循环必须用 vfs_is_special_fd 识别特殊对象并短写即停"
    );
    assert!(
        !io.contains("is_signalfd_fd") && !io.contains("sys_signalfd_read"),
        "io.rs 不得残留 signalfd 数值特判"
    );

    let epoll = read_file("privileged/syscall/epoll.rs");
    assert!(
        epoll.contains("fd_poll_special"),
        "epoll 必须经 VfsOps::fd_poll_special 求值特殊对象就绪位"
    );
    assert!(
        !epoll.contains("is_signalfd_fd"),
        "epoll 不得残留 signalfd 数值分支"
    );

    let dispatch = read_file("functions/syscall/dispatch.rs");
    assert!(
        dispatch.contains("functions::fs::signalfd::signalfd_syscall"),
        "SYS_SIGNALFD/SYS_SIGNALFD4 必须分发到对象模型实现"
    );

    let sendfile = read_file("functions/fs/sendfile.rs");
    assert!(
        sendfile.contains("of.as_inode().is_some()"),
        "sendfile 必须按 as_inode 判据排除非文件对象"
    );
    assert!(
        !sendfile.contains("220..236"),
        "sendfile 不得残留 signalfd 数值区间"
    );

    let fs_mod = read_file("functions/fs/mod.rs");
    assert!(
        fs_mod.contains("pub mod signalfd;"),
        "对象实现必须落在 functions/fs"
    );
    let sync_mod = read_file("functions/sync/mod.rs");
    assert!(
        !sync_mod.contains("pub mod signalfd;"),
        "旧 functions/sync 代理必须删除"
    );
    let priv_mod = read_file("privileged/syscall/mod.rs");
    assert!(
        !priv_mod.contains("pub mod signalfd;"),
        "privileged 旧 syscall 模块必须摘除"
    );

    let kernel_tests = read_file("kernel_tests/mod.rs");
    assert!(
        kernel_tests.contains("test_signalfd::register_signalfd_tests()"),
        "对象级内核测试必须注册到 glue 侧统一 runner"
    );
    let priv_tests = read_file("privileged/tests/mod.rs");
    assert!(
        !priv_tests.contains("register_signalfd_tests"),
        "旧 privileged 注册点必须删除"
    );
}

// ============================================================================
// 阶段 3 第 3 个迁移子系统: Smoltcp socket — 行为面与源码形状锁
// ============================================================================

#[test]
fn socket_object_implements_file_contract() {
    use edgine::kernel::functions::net::socket_file::SocketFile;

    let sf = SocketFile::new(11);
    assert_eq!(sf.slot(), 11, "槽位编号原样保存");
    assert!(sf.stream(), "socket 不可定位 (lseek → ESPIPE)");
    assert!(sf.as_inode().is_none(), "socket 非 inode 承载");
    assert_eq!(sf.file_type(), VfsFileType::File);
    assert_eq!(
        sf.io_control(0x1234_5678, 0),
        Err(KernelError::NotSupported),
        "socket 对象不处理通用 ioctl (回落默认策略)"
    );

    // syscall 层经 as_any 下转型取槽位; 挂表后 OpenFile 仍可往返.
    let of = OpenFile::from_file(Arc::new(SocketFile::new(11)), 0, 0);
    let recovered = of
        .file()
        .as_any()
        .downcast_ref::<SocketFile>()
        .expect("SocketFile 必须可下转型");
    assert_eq!(recovered.slot(), 11);
    assert!(!of.is_nonblock(), "OpenFile 缺省阻塞");
    of.set_nonblock(true);
    assert!(
        of.is_nonblock(),
        "F_SETFL 的非阻塞位由 OpenFile 单点承载 (socket 对象不另存)"
    );
    assert!(
        of.as_inode().is_none(),
        "socket 不得经 as_inode 混入 pcache / inotify 命名空间"
    );
}

#[test]
fn socket_created_via_open_file_table_not_fdplan() {
    let src = read_file("functions/net/socket_file.rs");
    assert!(
        src.contains("impl File for SocketFile"),
        "SocketFile 必须实现统一 File 接口"
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
        src.contains("net_socket::sm_net_close(slot)"),
        "挂表失败必须显式归还协议栈槽位"
    );
    assert!(
        src.contains("OPEN_FILE_TABLE.close(handle_id)"),
        "fd 表耗尽必须经关闭链触发 release 归还槽位"
    );
    assert!(
        src.contains("net_socket::sm_net_close(self.slot)"),
        "release 必须关闭协议栈槽位 (最后一个 fd 引用归零)"
    );
    assert!(
        !src.contains("FdPlan"),
        "迁移后不得引用全局 FD 编号区间 (FdPlan)"
    );

    let sys = read_file("functions/net/syscall.rs");
    assert!(
        !sys.contains("FdPlan") && !sys.contains("sm_slot"),
        "socket syscall 层不得残留 FD 编号区间 / fd→槽位换算"
    );

    // 槽位分配器归 privileged; sm_socket / sm_accept / sm_close 经其生灭.
    let slots = read_file("privileged/net/slots.rs");
    assert!(
        slots.contains("pub fn alloc()") && slots.contains("pub fn free(slot: usize)"),
        "槽位分配器必须暴露 alloc / free"
    );
    let fi = read_file("privileged/net/init/sm_fi.rs");
    assert!(
        fi.contains("slots::alloc()"),
        "sm_socket / sm_accept 必须经槽位分配器取槽位"
    );
    assert!(fi.contains("slots::free(slot)"), "sm_close 必须归还槽位");
}

#[test]
fn socket_dispatch_sites_resolve_object_and_pass_slot() {
    let sys = read_file("functions/net/syscall.rs");
    assert!(
        sys.contains("fn resolve(fd: i32) -> Result<Self, Errno>"),
        "socket syscall 必须以用户 fd 解析对象"
    );
    assert!(
        sys.contains("file.as_any().downcast_ref::<SocketFile>()"),
        "解析必须经 as_any 下转型到 SocketFile"
    );
    assert!(
        sys.contains("fn blocks(&self, flags: i32) -> bool"),
        "阻塞语义必须由对象非阻塞位与 MSG_DONTWAIT 合成"
    );
    assert!(
        sys.contains("impl Drop for SocketObject"),
        "系统调用结束必须释放对象引用 (槽位不被并发 close 回收)"
    );

    // UDS 专属选项 (SO_PASSCRED) 在对象解析后按对象类型分流 (阶段 3).
    let so = sys.find("fn setsockopt_syscall").expect("setsockopt 入口");
    let body = &sys[so..];
    let resolve_at = body
        .find("SocketObject::resolve(fd)")
        .expect("setsockopt 必须解析 socket 对象");
    let uds_at = body
        .find("obj.unix_idx()")
        .expect("SO_PASSCRED 必须按 UDS 对象分流");
    assert!(
        resolve_at < uds_at,
        "SO_PASSCRED 的 UDS 分流必须建立在 socket 对象解析之上"
    );

    // privileged 入口参数域为槽位编号, 且不再持有 per-slot 阻塞标志.
    let fw = read_file("privileged/net/syscall.rs");
    for needle in [
        "pub fn bind_syscall(slot_id: i32",
        "pub fn listen_syscall(slot_id: i32",
        "pub fn accept_syscall(slot_id: i32",
        "pub fn connect_syscall(slot_id: i32",
    ] {
        assert!(
            fw.contains(needle),
            "privileged 入口必须接收槽位编号: {needle}"
        );
    }
    assert!(
        !fw.contains("sm_set_nonblocking"),
        "非阻塞状态归 functions 对象, privileged 不再持 per-slot 标志"
    );

    // 通用 poll / close 分发不得残留 socket 数值分支.
    let file_ops = read_file("functions/fs/file_ops.rs");
    assert!(
        !file_ops.contains("Smoltcp") && !file_ops.contains("sm_socket_poll"),
        "poll 必须走 File::poll 统一对象分发"
    );
    let open = read_file("functions/fs/open.rs");
    assert!(
        !open.contains("sm_close"),
        "close 必须经 OpenFileTable 引用归零回调, 不得数值分支直关"
    );
    let io = read_file("functions/fs/io.rs");
    assert!(
        io.contains("of.is_pipe_endpoint() || of.as_inode().is_none()"),
        "F_SETFL 必须覆盖非 inode 特殊对象与管道端点"
    );
    assert!(
        !io.contains("Smoltcp") && !io.contains("SMOLTCP"),
        "io.rs 不得残留 socket 数值特判"
    );
}

// ============================================================================
// 阶段 3 第 4 个迁移子系统: UDS (AF_UNIX) — 行为面与源码形状锁
// ============================================================================

#[test]
fn unix_socket_object_implements_file_contract() {
    use edgine::kernel::functions::net::unix_socket_file::UnixSocketFile;

    // 槽位下标不触碰 UDS 策略表 (read/poll 会访问, 此处仅验证纯对象面)
    let uf = UnixSocketFile::new(3);
    assert_eq!(uf.idx(), 3, "策略表槽位下标原样保存");
    assert!(uf.stream(), "socket 不可定位 (lseek → ESPIPE)");
    assert!(uf.as_inode().is_none(), "UDS socket 非 inode 承载");
    assert!(uf.mmap_backing().is_none(), "UDS socket 不可作 mmap 后端");
    assert!(
        uf.inode_namespace_id().is_none(),
        "UDS socket 不属于 inode 命名空间"
    );
    assert_eq!(uf.file_type(), VfsFileType::File);
    assert_eq!(
        uf.io_control(0x1234_5678, 0),
        Err(KernelError::NotSupported),
        "UDS 对象不处理通用 ioctl (回落默认策略)"
    );

    let of = OpenFile::from_file(Arc::new(UnixSocketFile::new(3)), 0, 0);
    let recovered = of
        .file()
        .as_any()
        .downcast_ref::<UnixSocketFile>()
        .expect("UnixSocketFile 必须可下转型");
    assert_eq!(recovered.idx(), 3);
    assert!(!of.is_nonblock(), "OpenFile 缺省阻塞");
    of.set_nonblock(true);
    assert!(
        of.is_nonblock(),
        "F_SETFL 的非阻塞位由 OpenFile 单点承载 (UDS 对象不另存)"
    );
}

#[test]
fn unix_socket_created_via_open_file_table_not_fdplan() {
    let src = read_file("functions/net/unix_socket_file.rs");
    for needle in [
        "impl File for UnixSocketFile",
        "OpenFile::from_file",
        "OPEN_FILE_TABLE.alloc",
        "t.alloc_fd(handle_id, cloexec)",
        "OPEN_FILE_TABLE.close(handle_id)",
        "uds::uds_free(self.idx)",
        "uds::uds_free(idx)",
    ] {
        assert!(src.contains(needle), "UDS 创建/释放路径必须含: {needle}");
    }
    assert!(
        !src.contains("FdPlan") && !src.contains("fd_alloc::"),
        "迁移后不得引用全局 FD 编号区间 / fd_alloc 位图"
    );

    let policy = read_file("functions/net/unix.rs");
    assert!(
        !policy.contains("UDS_FD_BASE")
            && !policy.contains("FdPlan")
            && !policy.contains("fd_alloc::"),
        "UDS 策略表只暴露槽位下标, 不得残留 FD 编号区间 / 位图分配"
    );

    let sys = read_file("functions/net/syscall.rs");
    assert!(
        sys.contains("Self::Unix {") && sys.contains("fn unix_idx(&self) -> Option<u8>"),
        "socket syscall 层必须把 UDS 对象纳入统一解析与按类型分流"
    );
    assert!(
        !sys.contains("is_uds_fd") && !sys.contains("uds::uds_create"),
        "socket syscall 层不得残留 UDS 数值 fd 分流"
    );
}
