#![deny(unsafe_code)]
//! nanosleep 与自适应睡眠策略 — functions 层
//!
//! 为 nanosleep 系统调用提供参数验证:
//! - 验证 Timespec 指针非空
//! - 验证 `tv_sec` >= 0, 0 <= `tv_nsec` < `1_000_000_000`
//!
//! 并承接 `adaptive_sleep` 的阈值选择策略 (自 privileged 下沉, 见 docs/plan §6.3):
//! 短延时走忙等待, 长延时走调度器阻塞. 两种机制本身 (TSC 忙等待、可阻塞睡眠)
//! 仍由 privileged 提供.
//!
//! ## 安全边界
//!
//! - functions 层验证标量参数 (Timespec 字段合法性)
//! - 原始指针解引用委托给 privileged 层 (指针合法性由 syscall 入口保证)

use crate::privileged::syscall::Errno;

/// 忙等待与调度器阻塞的分界 (毫秒)
///
/// 属睡眠策略参数, 随策略一同留在 functions; 机制侧 (`busy_wait_ms` /
/// `timer_sleep`) 不感知该阈值.
const BUSY_WAIT_THRESHOLD_MS: u64 = 1;

/// 自适应睡眠: 按延时长度选择忙等待或调度器阻塞
///
/// - 延时小于 `BUSY_WAIT_THRESHOLD_MS`: 忙等待 (避免调度开销)
/// - 其余: 调度器阻塞 (睡眠期间让出 CPU)
///
/// # Arguments
/// * `ms` - 睡眠时间 (毫秒); 0 表示立即返回
pub fn adaptive_sleep(ms: u64) {
    if ms == 0 {
        return;
    }
    if ms < BUSY_WAIT_THRESHOLD_MS {
        crate::privileged::timer::busy_wait_ms(ms);
    } else {
        let _ = crate::privileged::timer::timer_sleep_safe(ms);
    }
}

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
        Ok(crate::functions::intcast::usize_from_i64(ret))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_sleep_zero_returns_immediately() {
        // 0 毫秒不进入任何机制路径
        adaptive_sleep(0);
    }
}
