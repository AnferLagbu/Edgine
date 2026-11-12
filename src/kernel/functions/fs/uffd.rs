#![deny(unsafe_code)]
//! userfaultfd — fd 对象与 ioctl ABI 层 (fd 对象模型阶段 3 第 8 个迁移子系统)
//!
//! ## fd 对象模型
//!
//! `UserFaultFdFile { idx }` 挂在全局 `OpenFileTable`, 用户 fd 由 per-process
//! `FdTable` 分配; 实例表 / #PF 拦截 / 物理页填充仍归 `privileged::mm::uffd`
//! (槽位下标域), 对象只持槽位下标. 由此获得 close/dup/fork/CLOEXEC/epoll 的
//! 统一语义; 对象不实现 `Inode` (`as_inode() == None`).
//!
//! ## 阻塞 read 的落点
//!
//! `File::read_at` 在 `OpenFileTable` 自旋锁内执行, 不可睡眠; 因此阻塞 `read()`
//! 在 syscall 层按**对象类型**路由 (fd → `UserFaultFdFile` 下转型) 后调用
//! [`read_event`], 与 socket syscall 的对象解析模式一致. 对象自身的 `read_at`
//! 为非阻塞快照 (无事件 → `EAGAIN`), 供 poll / 非阻塞调用方使用.
//!
//! ## 框内核边界
//!
//! - 100% safe Rust: 用户内存拷贝走 `syscall::api` 安全代理
//! - 实例表/#PF 拦截/物理页填充均在 privileged (0 unsafe 于本文件)

use alloc::sync::Arc;

use super::file::{File, PollEvents};
use super::open_file_table::OPEN_FILE_TABLE;
use super::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::privileged::mm::uffd::{
    UFFD_MSG_SIZE, UFFDIO_API, UFFDIO_COPY, UFFDIO_REGISTER, UFFDIO_UNREGISTER, UFFDIO_WAKE,
    UFFDIO_ZEROPAGE, UffdIoApi, UffdIoCopy, UffdIoRange, UffdIoRegister, UffdIoZeropage,
    UffdMsgPagefault,
};
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::api::{read_struct_from_user, write_struct_to_user};

/// `O_CLOEXEC`
const O_CLOEXEC: u32 = 0o200_0000;
/// `O_NONBLOCK`
const O_NONBLOCK: u32 = 0o4000;

// ============================================================================
// fd 对象
// ============================================================================

/// userfaultfd fd 对象 — 只持 privileged 实例槽位下标
pub struct UserFaultFdFile {
    /// 实例槽位下标
    idx: usize,
}

impl UserFaultFdFile {
    /// 构造 userfaultfd 对象
    ///
    /// 调用方保证 `idx` 对应一个已创建的 privileged 实例 (创建路径
    /// [`userfaultfd_syscall`] 负责).
    #[must_use]
    pub fn new(idx: usize) -> Self {
        Self { idx }
    }

    /// 实例槽位下标
    #[must_use]
    pub fn idx(&self) -> usize {
        self.idx
    }
}

/// 把 `UFFD_EVENT_PAGEFAULT` 事件按 ABI 布局 (32 字节) 写入切片.
fn encode_event(ev: &UffdMsgPagefault, buf: &mut [u8]) {
    buf[0] = ev.event;
    buf[1] = ev.reserved1;
    buf[2..4].copy_from_slice(&ev.reserved2.to_ne_bytes());
    buf[4..8].copy_from_slice(&ev.reserved3.to_ne_bytes());
    buf[8..16].copy_from_slice(&ev.flags.to_ne_bytes());
    buf[16..24].copy_from_slice(&ev.address.to_ne_bytes());
    buf[24..28].copy_from_slice(&ev.ptid.to_ne_bytes());
    buf[28..32].copy_from_slice(&ev.reserved4.to_ne_bytes());
}

impl File for UserFaultFdFile {
    fn read_at(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        if buf.len() < UFFD_MSG_SIZE {
            return Err(KernelError::InvalidArgument);
        }
        if !crate::privileged::mm::uffd_is_open(self.idx) {
            return Err(KernelError::BadFd);
        }
        match crate::privileged::mm::pop_event(self.idx) {
            Some(ev) => {
                encode_event(&ev, &mut buf[..UFFD_MSG_SIZE]);
                Ok(UFFD_MSG_SIZE)
            }
            // 非阻塞快照: 阻塞 read 由 syscall 层对象路由承担 (见模块文档)
            None => Err(KernelError::WouldBlock),
        }
    }

    fn write_at(&self, _offset: u64, _buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        Err(KernelError::InvalidArgument)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        match crate::privileged::mm::poll_ready(self.idx) {
            Some(true) => events & PollEvents::IN,
            _ => PollEvents::empty(),
        }
    }

    fn io_control(&self, cmd: u32, arg: usize) -> KernelResult<usize> {
        let ret = ioctl_uffd(
            self.idx,
            u64::from(cmd),
            crate::functions::intcast::u64_from_usize(arg),
        );
        if ret >= 0 {
            Ok(0)
        } else {
            Err(KernelError::from_i32(Errno::from_ret(ret).as_i32()))
        }
    }

    fn release(&self) {
        let _ = crate::privileged::mm::uffd_release(self.idx);
    }

    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn stream(&self) -> bool {
        // POSIX: userfaultfd 不可定位 (lseek → ESPIPE)
        true
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// fd → userfaultfd 对象快照 `(槽位下标, 是否非阻塞)`; 非 uffd 对象返回 `None`.
///
/// 阻塞 read / ioctl 在 syscall 层经此解析后调用, 避免在 `OpenFileTable`
/// 自旋锁内睡眠或执行用户内存拷贝.
#[must_use]
pub fn resolve_uffd(fd: i32) -> Option<(usize, bool)> {
    let handle_id = super::vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))?;
    OPEN_FILE_TABLE
        .with_file(handle_id, |of| {
            of.file()
                .as_any()
                .downcast_ref::<UserFaultFdFile>()
                .map(|f| (f.idx, of.is_nonblock()))
        })
        .flatten()
}

// ============================================================================
// syscall 与 ABI 策略
// ============================================================================

/// `userfaultfd(flags)` 策略 — 创建实例并挂 per-process fd 表
///
/// `O_NONBLOCK` 落 `OpenFile` 非阻塞位 (`read()` 无事件时返回 `EAGAIN`).
#[must_use]
pub fn userfaultfd_syscall(flags: u32) -> i64 {
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return Errno::Einval.as_ret();
    }
    let nonblock = flags & O_NONBLOCK != 0;
    let cloexec = flags & O_CLOEXEC != 0;

    // 实例按创建者线程组归属 (缺页路径按 tgid 匹配)
    let tgid = crate::privileged::proc::process_get_current_tgid();
    let Some(idx) = crate::privileged::mm::uffd_create(tgid) else {
        return Errno::Emfile.as_ret();
    };

    let pwm = crate::privileged::sgeg::pwm_get_current();
    let open_file = OpenFile::from_file(Arc::new(UserFaultFdFile::new(idx)), 0, pwm);
    open_file.set_nonblock(nonblock);

    let Some(handle_id) = OPEN_FILE_TABLE.alloc(open_file) else {
        // 对象未挂表 (release 不会触发), 显式归还实例
        let _ = crate::privileged::mm::uffd_release(idx);
        return Errno::Emfile.as_ret();
    };
    // 无进程上下文 / fd 表耗尽: 关闭 handle 触发 release (内部归还实例)
    let Some(fd_idx) =
        crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, cloexec))
            .flatten()
    else {
        OPEN_FILE_TABLE.close(handle_id);
        return Errno::Emfile.as_ret();
    };
    i64::from(crate::functions::intcast::low_i32_from_usize(fd_idx))
}

/// `UFFDIO_*` ioctl 策略 (fd 已确认为 userfaultfd)
///
/// 返回值语义与 Linux 一致: 成功返回 0, `UFFDIO_COPY` / `UFFDIO_ZEROPAGE` 的
/// 实际拷贝长度通过回填用户结构体的 `copy` / `zeropage` 字段返回.
#[must_use]
pub fn ioctl_uffd(idx: usize, request: u64, arg: u64) -> i64 {
    match request {
        UFFDIO_API => uffd_negotiate(idx, arg),
        UFFDIO_REGISTER => uffd_register(idx, arg),
        UFFDIO_UNREGISTER => uffd_unregister(idx, arg),
        UFFDIO_WAKE => uffd_wake(idx, arg),
        UFFDIO_COPY => uffd_copy(idx, arg),
        UFFDIO_ZEROPAGE => uffd_zeropage(idx, arg),
        _ => Errno::Enotty.as_ret(),
    }
}

/// `UFFDIO_API`: 协商 API 版本与特性, 成功时回填用户结构体
fn uffd_negotiate(idx: usize, arg: u64) -> i64 {
    let mut io = UffdIoApi::default();
    if !read_struct_from_user(arg, &mut io) {
        return Errno::Efault.as_ret();
    }
    match crate::privileged::mm::api_negotiate(idx, io.api, io.features) {
        Ok(out) => {
            if !write_struct_to_user(arg, &out) {
                return Errno::Efault.as_ret();
            }
            0
        }
        Err(e) => e.as_ret(),
    }
}

/// `UFFDIO_REGISTER`: 注册缺页区间, 成功时回填已实装 ioctl 位图
fn uffd_register(idx: usize, arg: u64) -> i64 {
    let mut reg = UffdIoRegister::default();
    if !read_struct_from_user(arg, &mut reg) {
        return Errno::Efault.as_ret();
    }
    match crate::privileged::mm::uffd_register(idx, reg.range.start, reg.range.len, reg.mode) {
        Ok(()) => {
            // Linux 回填 ioctls 支持位图; 本实装注册成功即支持全部已实装 ioctl
            reg.ioctls =
                UFFDIO_REGISTER | UFFDIO_UNREGISTER | UFFDIO_WAKE | UFFDIO_COPY | UFFDIO_ZEROPAGE;
            if !write_struct_to_user(arg, &reg) {
                return Errno::Efault.as_ret();
            }
            0
        }
        Err(e) => e.as_ret(),
    }
}

/// `UFFDIO_UNREGISTER`: 取消注册缺页区间
fn uffd_unregister(idx: usize, arg: u64) -> i64 {
    let mut range = UffdIoRange::default();
    if !read_struct_from_user(arg, &mut range) {
        return Errno::Efault.as_ret();
    }
    match crate::privileged::mm::uffd_unregister(idx, range.start, range.len) {
        Ok(()) => 0,
        Err(e) => e.as_ret(),
    }
}

/// `UFFDIO_WAKE`: 唤醒区间内的缺页等待者
fn uffd_wake(idx: usize, arg: u64) -> i64 {
    let mut range = UffdIoRange::default();
    if !read_struct_from_user(arg, &mut range) {
        return Errno::Efault.as_ret();
    }
    match crate::privileged::mm::uffd_wake(idx, range.start, range.len) {
        Ok(_) => 0,
        Err(e) => e.as_ret(),
    }
}

/// `UFFDIO_COPY`: 将用户提供的页内容填入缺页地址
fn uffd_copy(idx: usize, arg: u64) -> i64 {
    let mut cp = UffdIoCopy::default();
    if !read_struct_from_user(arg, &mut cp) {
        return Errno::Efault.as_ret();
    }
    if crate::functions::intcast::usize_from_u64(cp.len)
        != crate::functions::intcast::usize_from_u64(crate::privileged::mm::PAGE_SIZE)
    {
        return Errno::Einval.as_ret();
    }
    if !crate::privileged::syscall::api::validate_user_buf(cp.src, cp.len) {
        return Errno::Efault.as_ret();
    }
    let mut page =
        [0u8; crate::functions::intcast::usize_from_u64(crate::privileged::mm::PAGE_SIZE)];
    let page_len = page.len();
    match crate::privileged::mm::copy_user::copy_from_user(&mut page, cp.src, page_len) {
        Ok(n) if n == page_len => {}
        _ => return Errno::Efault.as_ret(),
    }
    match crate::privileged::mm::provide_page(idx, cp.dst, Some(&page)) {
        Ok(()) => {
            cp.copy = cp.len.cast_signed();
            if !write_struct_to_user(arg, &cp) {
                return Errno::Efault.as_ret();
            }
            0
        }
        Err(e) => e.as_ret(),
    }
}

/// `UFFDIO_ZEROPAGE`: 以零页填入缺页地址
fn uffd_zeropage(idx: usize, arg: u64) -> i64 {
    let mut zp = UffdIoZeropage::default();
    if !read_struct_from_user(arg, &mut zp) {
        return Errno::Efault.as_ret();
    }
    if crate::functions::intcast::usize_from_u64(zp.range.len)
        != crate::functions::intcast::usize_from_u64(crate::privileged::mm::PAGE_SIZE)
        || zp.range.start % crate::privileged::mm::PAGE_SIZE != 0
    {
        return Errno::Einval.as_ret();
    }
    match crate::privileged::mm::provide_page(idx, zp.range.start, None) {
        Ok(()) => {
            zp.zeropage = zp.range.len.cast_signed();
            if !write_struct_to_user(arg, &zp) {
                return Errno::Efault.as_ret();
            }
            0
        }
        Err(e) => e.as_ret(),
    }
}

/// uffd `read()` 策略: 返回一个 `UFFD_EVENT_PAGEFAULT` 事件
///
/// `blocking == true` 时无就绪事件则阻塞当前线程 (`scheduler_block` + 让出
/// CPU), 由 `fault_notify` 入队事件后唤醒; `blocking == false`
/// (`O_NONBLOCK`) 时无事件返回 `EAGAIN`.
///
/// # Errors
/// - `count < 32` (单事件大小) → `EINVAL`
/// - 写入用户缓冲区失败 → `EFAULT`; fd 已关闭 → `EBADF`; 非阻塞空读 → `EAGAIN`
pub fn read_event(idx: usize, buf: u64, count: u64, blocking: bool) -> Result<usize, Errno> {
    if count < UFFD_MSG_SIZE as u64 {
        return Err(Errno::Einval);
    }
    let pid = crate::privileged::proc::process_get_current_pid();
    crate::privileged::mm::set_reader(idx, pid);
    loop {
        if let Some(ev) = crate::privileged::mm::pop_event(idx) {
            crate::privileged::mm::clear_reader(idx, pid);
            if !write_struct_to_user(buf, &ev) {
                return Err(Errno::Efault);
            }
            return Ok(UFFD_MSG_SIZE);
        }
        // fd 在阻塞期间被关闭 → 立即返回错误 (避免永久挂起)
        if !crate::privileged::mm::uffd_is_open(idx) {
            crate::privileged::mm::clear_reader(idx, pid);
            return Err(Errno::Ebadf);
        }
        if !blocking {
            crate::privileged::mm::clear_reader(idx, pid);
            return Err(Errno::Eagain);
        }
        // 无就绪事件: 阻塞当前线程, 由 fault_notify 唤醒
        crate::privileged::proc::scheduler_block(
            crate::privileged::proc::BlockReason::WaitingForIo,
        );
        crate::privileged::proc::scheduler_yield_ex();
    }
}

// ============================================================================
// host 侧单元测试 (cargo test; 含 syscall 级 fd 分配/dup/close 闭环)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::intcast::{low_u32_from_usize, usize_from_i32};
    use crate::privileged::proc::PROCESS_TABLE;
    use crate::privileged::proc::raw;
    use crate::privileged::proc::scheduler::Scheduler;

    /// 临时安装当前进程 (含独立 FdTable), 执行闭包后无条件拆除现场
    fn with_temp_process<F: FnOnce()>(name: &str, f: F) {
        let Some(pid) = PROCESS_TABLE.allocate_pid() else {
            panic!("临时进程: 无空闲 pid");
        };
        let proc_ptr = raw::alloc_process(pid, name, None);
        if !PROCESS_TABLE.insert(proc_ptr) {
            raw::drop_boxed_process(proc_ptr);
            PROCESS_TABLE.free_pid(pid);
            panic!("临时进程: 插入进程表失败");
        }

        let prev_current = Scheduler::current();
        Scheduler::set_current(pid);

        f();

        Scheduler::set_current(prev_current.unwrap_or(0));
        let _ = PROCESS_TABLE.remove(pid);
        raw::drop_boxed_process(proc_ptr);
        PROCESS_TABLE.free_pid(pid);
    }

    fn close_fd(fd: usize) -> i32 {
        crate::functions::fs::api::vfs_close_safe(low_u32_from_usize(fd))
    }

    #[test]
    fn object_file_contract() {
        let file = UserFaultFdFile::new(15);
        assert_eq!(file.idx(), 15);
        assert!(file.stream(), "uffd 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "uffd 非 inode 承载");
        assert!(file.mmap_backing().is_none(), "uffd 不可作 mmap 后端");
        assert!(
            file.inode_namespace_id().is_none(),
            "uffd 不属于 inode 命名空间"
        );
        assert_eq!(file.file_type(), VfsFileType::File);
        assert_eq!(
            file.read_at(0, &mut [0u8; UFFD_MSG_SIZE - 1], 0),
            Err(KernelError::InvalidArgument),
            "短缓冲读 → EINVAL"
        );
        assert_eq!(
            file.read_at(0, &mut [0u8; UFFD_MSG_SIZE], 0),
            Err(KernelError::BadFd),
            "无效槽位读 → EBADF"
        );
        assert!(file.poll(PollEvents::IN).is_empty(), "无效槽位 poll 不就绪");
        assert_eq!(
            file.write_at(0, &[0u8; 4], 0),
            Err(KernelError::InvalidArgument),
            "uffd 只读 (写 → EINVAL)"
        );
        assert_eq!(
            file.io_control(crate::functions::intcast::low_u32_from_u64(UFFDIO_API), 0),
            Err(KernelError::from_i32(Errno::Efault.as_i32())),
            "UFFDIO_API 用户指针为空 → EFAULT"
        );
    }

    #[test]
    fn object_downcast() {
        let file = UserFaultFdFile::new(7);
        let erased: &dyn File = &file;
        assert_eq!(
            erased
                .as_any()
                .downcast_ref::<UserFaultFdFile>()
                .map(UserFaultFdFile::idx),
            Some(7),
            "UserFaultFdFile 必须可下转型"
        );
    }

    #[test]
    fn event_encoding_matches_abi_layout() {
        let ev = UffdMsgPagefault {
            event: 0x12,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            flags: 0x0002,
            address: 0x0000_0000_4000_0000,
            ptid: 42,
            reserved4: 0,
        };
        let mut buf = [0xFFu8; UFFD_MSG_SIZE];
        encode_event(&ev, &mut buf);
        assert_eq!(buf[0], 0x12, "event 偏移 0");
        assert_eq!(&buf[8..16], &0x0002u64.to_ne_bytes(), "flags 偏移 8");
        assert_eq!(
            &buf[16..24],
            &0x0000_0000_4000_0000u64.to_ne_bytes(),
            "address 偏移 16"
        );
        assert_eq!(&buf[24..28], &42u32.to_ne_bytes(), "ptid 偏移 24");
    }

    #[test]
    fn syscall_create_dup_close_lifecycle() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("uffd-t", || {
            let fd = userfaultfd_syscall(O_CLOEXEC | O_NONBLOCK);
            assert!(fd >= 3, "uffd 由 per-process fd 表分配 (>= FIRST_USER_FD)");
            let fd_usize = crate::functions::intcast::usize_from_i64(fd);

            let (idx, nonblock) =
                resolve_uffd(crate::functions::intcast::low_i32_from_usize(fd_usize))
                    .expect("fd 应解析为 uffd 对象");
            assert!(nonblock, "O_NONBLOCK 应落 OpenFile 非阻塞位");
            assert!(crate::privileged::mm::uffd_is_open(idx), "实例应在打开状态");

            let handle = crate::functions::fs::vfs_get_fd_handle(fd_usize).expect("OpenFile");
            assert_eq!(
                crate::privileged::proc::with_current_fd_table(|t| t.is_cloexec(fd_usize)),
                Some(true),
                "O_CLOEXEC 应落 per-process fd 表"
            );
            assert_eq!(
                OPEN_FILE_TABLE.with_file(handle, |of| of.file().poll(PollEvents::IN)),
                Some(PollEvents::empty()),
                "无事件时 uffd 不就绪"
            );
            assert_eq!(
                OPEN_FILE_TABLE.with_file(handle, |of| of.file().read_at(
                    0,
                    &mut [0u8; UFFD_MSG_SIZE],
                    0
                )),
                Some(Err(KernelError::WouldBlock)),
                "非阻塞空读 → EAGAIN"
            );

            let dup_fd = crate::functions::fs::vfs_dup(low_u32_from_usize(fd_usize));
            assert!(dup_fd >= 0, "dup 应成功");
            let dup_idx = usize_from_i32(dup_fd);
            assert_ne!(dup_idx, fd_usize, "dup 分配新 fd");
            assert_eq!(
                crate::functions::fs::vfs_get_fd_handle(dup_idx),
                Some(handle),
                "dup 共享同一 OpenFile"
            );

            assert_eq!(close_fd(fd_usize), 0);
            assert_eq!(
                resolve_uffd(dup_fd).map(|(i, _)| i),
                Some(idx),
                "dup 仍解析同一实例"
            );
            assert_eq!(close_fd(dup_idx), 0);
            assert!(resolve_uffd(dup_fd).is_none(), "最后关闭后对象退场");
            assert!(
                !crate::privileged::mm::uffd_is_open(idx),
                "最后关闭必须释放实例"
            );
        });
    }

    #[test]
    fn syscall_rejects_invalid_flags() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("uffd-t2", || {
            assert_eq!(userfaultfd_syscall(0o10), Errno::Einval.as_ret());
        });
    }
}
