//! `rseq(2)` — 注册/注销当前线程的 rseq 用户区 (functions 层策略)
//!
//! 参数语义与 Linux 原始 32 字节 ABI 一致:
//! - `flags = 0`: 注册; 已注册时同参返回 `EBUSY`, 指针/长度不同返回
//!   `EINVAL`, 签名不同返回 `EPERM`
//! - `flags = RSEQ_FLAG_UNREGISTER`: 注销; 指针/长度/签名必须与注册值一致
//! - `len` 当前只接受 32; 指针必须 32 字节对齐
//!
//! 机制 (用户内存写入/进程状态) 全部委托 `privileged::proc::rseq`.
#![deny(unsafe_code)]

use crate::privileged::errno::Errno;

/// `rseq(rseq, rseq_len, flags, sig)`.
///
/// # Errors
///
/// 透传机制层错误: 非法参数/对齐 `EINVAL`, 重复注册 `EBUSY`, 签名不符
/// `EPERM`, 用户区不可写 `EFAULT`, 当前进程不存在 `ESRCH`.
pub fn rseq_syscall(rseq: u64, rseq_len: u32, flags: i32, sig: u32) -> Result<usize, Errno> {
    crate::privileged::proc::rseq::rseq_syscall(rseq, rseq_len, flags, sig)?;
    Ok(0)
}
