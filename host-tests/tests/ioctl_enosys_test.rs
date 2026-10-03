//! sys_ioctl 行为契约测试 (P1-I-39)
//!
//! 验证:
//! 1. TCGETS stub 必须返回 -ENOSYS 而非假装成功
//! 2. 未知 ioctl 命令返回 -ENOTTY
//! 3. arg=0 返回 -EINVAL
//! 4. TIOCGWINSZ 返回 0 (真实实现, 填 ws 结构)
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `sys_ioctl_contract` / `Winsize` / 错误码常量平行镜像, 改引内核
//! 真实源码 `queenx::kernel::services::fs::file_ops::ioctl_syscall` (services 层,
//! 100% safe, pub fn, host 可直接调用).
//! - TIOCGWINSZ 命令常量 (0x5413) / TCGETS (0x5401) 为内核私有常量, 测试侧
//!   保留镜像并标注同步 (参考 lib_string_strlen_safe 的 STRLEN_MAX 做法).
//! - 错误码不再硬编码, 统一经 `Errno::as_ret()` 引用内核 errno 编码.
//! - 用户指针校验 (USER_ADDR_MAX) 是内核真实行为: host 用堆 (Box) 分配的
//!   Winsize 缓冲区地址 < USER_ADDR_MAX 可通过校验; 栈地址会因超界返回 EFAULT.

use queenx::kernel::framework::syscall::Errno;
use queenx::kernel::services::fs::file_ops::ioctl_syscall;

/// 内核私有命令常量镜像 (services/fs/file_ops.rs, 非 pub)
/// 与内核 `const TIOCGWINSZ: u64 = 0x5413` 同步; 若内核改值需同步.
const TIOCGWINSZ: u64 = 0x5413;
/// 内核私有命令常量镜像 (services/fs/file_ops.rs)
const TCGETS: u64 = 0x5401;
/// 未知命令 (验证回退 ENOTTY)
const TIOCSETAF: u64 = 0x5404;
/// 未知命令 (验证回退 ENOTTY)
const FIONREAD: u64 = 0x541B;

/// 与内核 `ioctl_syscall` TIOCGWINSZ 分支相同的 `Winsize` 布局 (repr(C))
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[test]
fn tcgets_stub_returns_enosys_not_zero() {
    // P1-I-39 验收: TCGETS 必须返回 ENOSYS, 不能假装成功
    let mut ws = Box::new(Winsize::default());
    let arg = &mut *ws as *mut Winsize as u64;
    let ret = ioctl_syscall(1, TCGETS, arg);
    assert_eq!(
        ret,
        Errno::ENOSYS.as_ret(),
        "P1-I-39: TCGETS stub 必须返回 -ENOSYS, 实际 = {}",
        ret
    );
    // termios 缓冲区不被填充, 也不被破坏 (内核端 stub 路径无副作用)
    assert_eq!(ws.ws_row, 0, "P1-I-39: ENOSYS 路径不应修改 termios 缓冲区");
}

#[test]
fn tcgets_returns_enosys_for_any_fd() {
    // P1-I-39 验收: 任意 fd 调用 TCGETS 返回 ENOSYS
    for fd in [0i32, 1, 2, 100, -1] {
        let ws = Box::new(Winsize::default());
        let ret = ioctl_syscall(fd, TCGETS, &*ws as *const Winsize as u64);
        assert_eq!(
            ret,
            Errno::ENOSYS.as_ret(),
            "P1-I-39: fd={} 调用 TCGETS 必须返回 -ENOSYS",
            fd
        );
    }
}

#[test]
fn unknown_ioctl_returns_enotty() {
    // 未知命令必须返回 ENOTTY (POSIX 约定)
    let ws = Box::new(Winsize::default());
    let arg = &*ws as *const Winsize as u64;
    let ret = ioctl_syscall(0, FIONREAD, arg);
    assert_eq!(
        ret,
        Errno::ENOTTY.as_ret(),
        "P1-I-39: 未知 ioctl 必须返回 -ENOTTY"
    );
    let ret = ioctl_syscall(0, TIOCSETAF, arg);
    assert_eq!(
        ret,
        Errno::ENOTTY.as_ret(),
        "P1-I-39: 未知 ioctl 必须返回 -ENOTTY"
    );
}

#[test]
fn arg_zero_returns_einval() {
    // arg=0 是无效指针, 必须返回 EINVAL
    let ret = ioctl_syscall(0, TIOCGWINSZ, 0);
    assert_eq!(
        ret,
        Errno::EINVAL.as_ret(),
        "P1-I-39: arg=0 必须返回 -EINVAL"
    );
}

#[test]
fn tiocgwinsz_real_impl_fills_winsize() {
    // TIOCGWINSZ 是真实实现, 返回 0 并填充 ws 结构.
    // host 侧用 Box 分配的缓冲区: 地址 < USER_ADDR_MAX, 内核用户指针校验通过.
    let mut ws = Box::new(Winsize::default());
    let ret = ioctl_syscall(1, TIOCGWINSZ, &mut *ws as *mut Winsize as u64);
    assert_eq!(ret, 0, "P1-I-39: TIOCGWINSZ 应返回 0");
    assert_eq!(ws.ws_row, 25, "P1-I-39: 终端行数应为 25");
    assert_eq!(ws.ws_col, 80, "P1-I-39: 终端列数应为 80");
}

#[test]
fn tiocgwinsz_user_ptr_oob_returns_efault() {
    // 内核真实行为: 用户指针校验 (USER_ADDR_MAX = 0x0000_7FFF_FFFF_F000) 是
    // 框架层强制的. 内核空间地址 (> USER_ADDR_MAX) 会被判为非法用户指针 →
    // write_struct_to_user 返回 false → ioctl_syscall 返回 EFAULT.
    // 此断言固化内核用户指针代理边界 (I4). 注: host 栈/堆地址可能落在
    // USER_ADDR_MAX 内 (返回 0), 故使用确定性的内核空间地址.
    let kernel_addr: u64 = 0xFFFF_8000_DEAD_BEEF; // > USER_ADDR_MAX
    let ret = ioctl_syscall(1, TIOCGWINSZ, kernel_addr);
    assert_eq!(
        ret,
        Errno::EFAULT.as_ret(),
        "P1-I-39: 超 USER_ADDR_MAX 的用户指针必须返回 -EFAULT"
    );
}

#[test]
fn isatty_simulation_via_ioctl_return_code() {
    // P1-I-39 验收: isatty() 在非 tty fd 上正确返回 0 (不假设是终端).
    // isatty() = (ioctl(TCGETS) == 0); 修复后 TCGETS 返回 ENOSYS, 故 isatty() 正确返回 0.
    let ws = Box::new(Winsize::default());
    let fd = 99; // 非 tty fd
    let rc = ioctl_syscall(fd, TCGETS, &*ws as *const Winsize as u64);
    let isatty_result = if rc == 0 { 1 } else { 0 };
    assert_eq!(
        isatty_result, 0,
        "P1-I-39: ioctl 失败时 isatty() 必须返回 0 (非终端)"
    );
}
