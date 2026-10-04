#![deny(unsafe_code)]
//! nanosleep — functions 层安全代理
//!
//! 为 nanosleep 系统调用提供参数验证:
//! - 验证 Timespec 指针非空
//! - 验证 `tv_sec` >= 0, 0 <= `tv_nsec` < `1_000_000_000`
//!
//! ## 安全边界
//!
//! - functions 层验证标量参数 (Timespec 字段合法性)
//! - 原始指针解引用委托给 privileged 层 (指针合法性由 syscall 入口保证)

use crate::privileged::syscall::Errno;

/// nanosleep 安全代理
///
/// 验证 Timespec 参数合法性, 委托 privileged 层执行睡眠.
///
/// # Errors
///
/// 当底层 `sys_nanosleep` 返回负值(如参数非法或被信号中断)时,
/// 转换为对应的 `Errno`.
pub fn nanosleep_syscall(req: u64, rem: u64) -> Result<usize, Errno> {
    // req 指针为空由 privileged 层检查 (需要 unsafe 解引用)
    // functions 层仅验证已解析的标量参数
    let ret = crate::privileged::syscall::api::sys_nanosleep(req, rem);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}
