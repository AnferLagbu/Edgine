#![deny(unsafe_code)]
//! userfaultfd 系统调用与 ioctl ABI 层 (T1 G4 实装)
//!
//! ## 职责
//!
//! - `userfaultfd(flags)`: 校验 flags 并创建实例 (机制在 `privileged::mm::uffd`)
//! - `UFFDIO_*` ioctl: 用户结构体解析/回填 + 参数校验, 委托 privileged 机制
//! - `read()`: 弹出 `UFFD_EVENT_PAGEFAULT` 事件 (无事件时阻塞等待)
//!
//! ## 框内核边界
//!
//! - 100% safe Rust: 用户内存拷贝走 `syscall::api` 安全代理
//! - 实例表/#PF 拦截/物理页填充均在 privileged (0 unsafe 于本文件)

use crate::privileged::mm::uffd::{
    UFFD_MSG_SIZE, UFFDIO_API, UFFDIO_COPY, UFFDIO_REGISTER, UFFDIO_UNREGISTER, UFFDIO_WAKE,
    UFFDIO_ZEROPAGE, UffdIoApi, UffdIoCopy, UffdIoRange, UffdIoRegister, UffdIoZeropage,
};
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::api::{read_struct_from_user, write_struct_to_user};

/// `O_CLOEXEC`
const O_CLOEXEC: u32 = 0o2000000;
/// `O_NONBLOCK`
const O_NONBLOCK: u32 = 0o4000;

/// `userfaultfd(flags)` 策略
///
/// SIMPLIFIED: `O_NONBLOCK` 被接受但无行为差异 — 本实装的 `read()` 语义为
/// "无事件则阻塞等待" (Linux `O_NONBLOCK` 下应为 `EAGAIN`). 影响面: 非阻塞调用方
/// 会在无事件时挂起. 何时需扩展: 引入 fd 级状态标志位后按 `O_NONBLOCK` 分支返回 `EAGAIN`.
pub fn userfaultfd_syscall(flags: u32) -> i64 {
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return Errno::EINVAL.as_ret();
    }
    let pid = crate::privileged::proc::process_get_current_pid();
    match crate::privileged::mm::uffd_create(pid) {
        Some(fd) => i64::from(fd),
        None => Errno::EMFILE.as_ret(),
    }
}

/// `UFFDIO_*` ioctl 策略 (fd 已确认为 userfaultfd)
///
/// 返回值语义与 Linux 一致: 成功返回 0, `UFFDIO_COPY` / `UFFDIO_ZEROPAGE` 的
/// 实际拷贝长度通过回填用户结构体的 `copy` / `zeropage` 字段返回.
pub fn ioctl_uffd(fd: i32, request: u64, arg: u64) -> i64 {
    match request {
        UFFDIO_API => {
            let mut io = UffdIoApi::default();
            if !read_struct_from_user(arg, &mut io) {
                return Errno::EFAULT.as_ret();
            }
            match crate::privileged::mm::api_negotiate(fd, io.api, io.features) {
                Ok(out) => {
                    if !write_struct_to_user(arg, &out) {
                        return Errno::EFAULT.as_ret();
                    }
                    0
                }
                Err(e) => e.as_ret(),
            }
        }
        UFFDIO_REGISTER => {
            let mut reg = UffdIoRegister::default();
            if !read_struct_from_user(arg, &mut reg) {
                return Errno::EFAULT.as_ret();
            }
            match crate::privileged::mm::uffd_register(fd, reg.range.start, reg.range.len, reg.mode)
            {
                Ok(()) => {
                    // Linux 回填 ioctls 支持位图; 本实装注册成功即支持全部已实装 ioctl
                    reg.ioctls = UFFDIO_REGISTER
                        | UFFDIO_UNREGISTER
                        | UFFDIO_WAKE
                        | UFFDIO_COPY
                        | UFFDIO_ZEROPAGE;
                    if !write_struct_to_user(arg, &reg) {
                        return Errno::EFAULT.as_ret();
                    }
                    0
                }
                Err(e) => e.as_ret(),
            }
        }
        UFFDIO_UNREGISTER | UFFDIO_WAKE => {
            let mut range = UffdIoRange::default();
            if !read_struct_from_user(arg, &mut range) {
                return Errno::EFAULT.as_ret();
            }
            let result = if request == UFFDIO_UNREGISTER {
                crate::privileged::mm::uffd_unregister(fd, range.start, range.len)
            } else {
                crate::privileged::mm::uffd_wake(fd, range.start, range.len).map(|_| ())
            };
            match result {
                Ok(()) => 0,
                Err(e) => e.as_ret(),
            }
        }
        UFFDIO_COPY => {
            let mut cp = UffdIoCopy::default();
            if !read_struct_from_user(arg, &mut cp) {
                return Errno::EFAULT.as_ret();
            }
            if cp.len as usize != crate::privileged::mm::PAGE_SIZE as usize {
                return Errno::EINVAL.as_ret();
            }
            if !crate::privileged::syscall::raw::check_user_buf(cp.src, cp.len) {
                return Errno::EFAULT.as_ret();
            }
            let mut page = [0u8; crate::privileged::mm::PAGE_SIZE as usize];
            let page_len = page.len();
            match crate::privileged::mm::copy_user::copy_from_user(&mut page, cp.src, page_len) {
                Ok(n) if n == page_len => {}
                _ => return Errno::EFAULT.as_ret(),
            }
            match crate::privileged::mm::provide_page(fd, cp.dst, Some(&page)) {
                Ok(()) => {
                    cp.copy = cp.len as i64;
                    if !write_struct_to_user(arg, &cp) {
                        return Errno::EFAULT.as_ret();
                    }
                    0
                }
                Err(e) => e.as_ret(),
            }
        }
        UFFDIO_ZEROPAGE => {
            let mut zp = UffdIoZeropage::default();
            if !read_struct_from_user(arg, &mut zp) {
                return Errno::EFAULT.as_ret();
            }
            if zp.range.len as usize != crate::privileged::mm::PAGE_SIZE as usize
                || zp.range.start % crate::privileged::mm::PAGE_SIZE != 0
            {
                return Errno::EINVAL.as_ret();
            }
            match crate::privileged::mm::provide_page(fd, zp.range.start, None) {
                Ok(()) => {
                    zp.zeropage = zp.range.len as i64;
                    if !write_struct_to_user(arg, &zp) {
                        return Errno::EFAULT.as_ret();
                    }
                    0
                }
                Err(e) => e.as_ret(),
            }
        }
        _ => Errno::ENOTTY.as_ret(),
    }
}

/// uffd `read()` 策略: 返回一个 `UFFD_EVENT_PAGEFAULT` 事件
///
/// 无就绪事件时阻塞当前线程 (`scheduler_block` + 让出 CPU), 由 `fault_notify`
/// 入队事件后唤醒 (见 `privileged::mm::uffd` 模块文档的时序图).
///
/// # Errors
/// - `count < 32` (单事件大小) → `EINVAL`
/// - 写入用户缓冲区失败 → `EFAULT`; fd 已关闭 → `EBADF`
pub fn read_event(fd: i32, buf: u64, count: u64) -> Result<usize, Errno> {
    if count < UFFD_MSG_SIZE as u64 {
        return Err(Errno::EINVAL);
    }
    let pid = crate::privileged::proc::process_get_current_pid();
    crate::privileged::mm::set_reader(fd, pid);
    loop {
        if let Some(ev) = crate::privileged::mm::pop_event(fd) {
            crate::privileged::mm::clear_reader(fd, pid);
            if !write_struct_to_user(buf, &ev) {
                return Err(Errno::EFAULT);
            }
            return Ok(UFFD_MSG_SIZE);
        }
        // fd 在阻塞期间被关闭 → 立即返回错误 (避免永久挂起)
        if !crate::privileged::mm::uffd_is_open(fd) {
            crate::privileged::mm::clear_reader(fd, pid);
            return Err(Errno::EBADF);
        }
        // 无就绪事件: 阻塞当前线程, 由 fault_notify 唤醒
        crate::privileged::proc::scheduler_block(
            crate::privileged::proc::BlockReason::WaitingForIo,
        );
        crate::privileged::proc::scheduler_yield_ex();
    }
}
