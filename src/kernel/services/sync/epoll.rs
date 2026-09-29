#![deny(unsafe_code)]
//! epoll — services 层安全代理
//!
//! 为 epoll 系统调用提供参数验证和类型安全封装:
//! - `epoll_create_syscall`: 验证 size > 0
//! - `epoll_ctl_syscall`: 验证 op 有效, fd 非负
//! - `epoll_wait_syscall`: 验证 maxevents > 0
//!
//! ## 安全边界
//!
//! - services 层验证标量参数 (size/op/fd/maxevents)
//! - 原始指针 (`*const EpollEvent` / `*mut EpollEvent`) 委托给 framework 层
//!   (指针合法性由 syscall 入口 `check_user_ptr` 保证)

use crate::framework::syscall::Errno;

/// `epoll_create` 安全代理
///
/// 验证: size > 0
///
/// # Errors
///
/// - `size <= 0` → `EINVAL`
/// - 底层 `sys_epoll_create` 返回负值时转换为对应的 `Errno`
pub fn epoll_create_syscall(size: i32) -> Result<usize, Errno> {
    if size <= 0 {
        return Err(Errno::EINVAL);
    }
    let ret = crate::framework::syscall::epoll::sys_epoll_create(size);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `epoll_ctl` 安全代理
///
/// 验证: op 有效 (ADD/DEL/MOD), fd >= 0
///
/// # Errors
///
/// - `op` 不是 ADD/DEL/MOD 之一 → `EINVAL`
/// - `fd < 0` → `EBADF`
/// - ADD/MOD 操作 `event` 为空指针 → `EFAULT`
/// - 底层 `sys_epoll_ctl` 返回负值时转换为对应的 `Errno`
pub fn epoll_ctl_syscall(
    epfd: i64,
    op: i32,
    fd: i32,
    event: u64, // 原始指针, 委托 framework 处理
) -> Result<usize, Errno> {
    // 验证 op
    const EPOLL_CTL_ADD: i32 = 1;
    const EPOLL_CTL_DEL: i32 = 2;
    const EPOLL_CTL_MOD: i32 = 3;
    if op != EPOLL_CTL_ADD && op != EPOLL_CTL_DEL && op != EPOLL_CTL_MOD {
        return Err(Errno::EINVAL);
    }
    // 验证 fd
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    // DEL 操作允许 event 为 null
    if op == EPOLL_CTL_DEL && event == 0 {
        let ret = crate::framework::syscall::epoll::sys_epoll_ctl(epfd, op, fd, core::ptr::null());
        return if ret < 0 {
            Err(Errno::from_ret(ret))
        } else {
            Ok(0)
        };
    }
    // ADD/MOD: event 必须非 null
    if event == 0 {
        return Err(Errno::EFAULT);
    }
    let ret = crate::framework::syscall::epoll::sys_epoll_ctl(
        epfd,
        op,
        fd,
        event as *const crate::framework::syscall::epoll::EpollEvent,
    );
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `epoll_wait` 安全代理
///
/// 验证: maxevents > 0
///
/// # Errors
///
/// - `maxevents <= 0` → `EINVAL`
/// - `events` 为空指针 → `EFAULT`
/// - 底层 `sys_epoll_wait` 返回负值时转换为对应的 `Errno`
pub fn epoll_wait_syscall(
    epfd: i64,
    events: u64, // 原始指针, 委托 framework 处理
    maxevents: i32,
    timeout: i32,
) -> Result<usize, Errno> {
    if maxevents <= 0 {
        return Err(Errno::EINVAL);
    }
    if events == 0 {
        return Err(Errno::EFAULT);
    }
    let ret = crate::framework::syscall::epoll::sys_epoll_wait(
        epfd,
        events as *mut crate::framework::syscall::epoll::EpollEvent,
        maxevents,
        timeout,
    );
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `epoll_pwait` 安全代理 (T1 G5 实装)
///
/// `epoll_wait` + 临时信号屏蔽字 (Linux ABI 第 5/6 参数 `sigmask`/`sigsetsize`):
/// 等待期间替换屏蔽字, 返回前恢复原值 (见
/// `services::proc::signal::with_temporary_sigmask`).
///
/// # Errors
///
/// - 继承 [`epoll_wait_syscall`] 的校验结果 (`maxevents <= 0` → `EINVAL`,
///   `events` 为空指针 → `EFAULT`)
/// - `sigsetsize != 8` → `EINVAL`; `sigmask` 用户内存不可读 → `EFAULT`
pub fn epoll_pwait_syscall(
    epfd: i64,
    events: u64, // 原始指针, 委托 framework 处理
    maxevents: i32,
    timeout: i32,
    sigmask_ptr: u64,
    sigsetsize: u64,
) -> Result<usize, Errno> {
    crate::services::proc::signal::with_temporary_sigmask(sigmask_ptr, sigsetsize, || {
        epoll_wait_syscall(epfd, events, maxevents, timeout)
    })
}

// ============================================================================
// 单元测试 (DECISION-080 双轨: 纯逻辑测试归源侧 #[cfg(test)])
// ============================================================================

#[cfg(test)]
mod tests {
    use super::epoll_pwait_syscall;
    use crate::framework::syscall::Errno;

    /// 越界用户指针 (`>= USER_ADDR_MAX`): 被 `check_user_buf` 拒绝而非解引用
    const BAD_USER_PTR: u64 = 0x8000_0000_0000_0000;

    /// `epoll_pwait` 参数校验与 `epoll_wait` 委托 (sigmask == NULL)
    #[test]
    fn test_epoll_pwait_validation() {
        // maxevents <= 0 → EINVAL (委托 epoll_wait 校验)
        match epoll_pwait_syscall(-1, 0x1000, 0, 0, 0, 0) {
            Err(Errno::EINVAL) => {}
            _ => panic!("maxevents=0 应返回 EINVAL"),
        }
        // maxevents 合法 + epfd 非法 → EINVAL (framework 层 epfd <= 0)
        match epoll_pwait_syscall(-1, 0x1000, 1, 0, 0, 0) {
            Err(Errno::EINVAL) => {}
            _ => panic!("epfd<0 应返回 EINVAL"),
        }
        // sigmask 越界指针 → 错误先于等待返回
        assert!(
            epoll_pwait_syscall(-1, 0x1000, 1, 0, BAD_USER_PTR, 8).is_err(),
            "非法 sigmask 指针应返回错误"
        );
    }
}
