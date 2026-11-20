//! `sendmsg`/`recvmsg` 逐 iov 地址校验回归锁 (安全缺口 ㉛)
//!
//! privileged 侧 `sm_sendmsg`/`sm_recvmsg` 直接对 `iov_base` 做 `copy_nonoverlapping`
//! (源或目的). 修复前 functions 层只校验 `msg` 与 iov **数组** 的范围, 不校验每条
//! iov 的 `base`/`len`, 用户可把 `iov_base` 指向内核地址从而读写内核内存 (违反 I4).
//!
//! 本测试经真实 syscall 入口 `sendmsg_syscall` / `recvmsg_syscall` 验证: 指向内核地址
//! 的 iov 在进入 privileged 裸搬运前即被 `EFAULT` 拒绝. 两个入口都在解析 fd 之前做
//! iov 校验, 故无需真实 socket 即可覆盖该判据.

use edgine::kernel::functions::net::syscall::{recvmsg_syscall, sendmsg_syscall};
use edgine::kernel::privileged::syscall::Errno;

/// 构造一个 `msghdr` 缓冲: `msg_iov` 指向给定 iov 数组, `msg_iovlen` 给定条数.
///
/// Linux `msghdr` 布局: `msg_name`(0) / `msg_namelen`(8) / `msg_iov`(16) /
/// `msg_iovlen`(24) / `msg_control`(32) / `msg_controllen`(40) / `msg_flags`(48),
/// 共 56 字节.
fn msghdr_with_iov(iov_ptr: u64, iovlen: u64) -> [u8; 56] {
    let mut msg = [0u8; 56];
    msg[16..24].copy_from_slice(&iov_ptr.to_ne_bytes());
    msg[24..32].copy_from_slice(&iovlen.to_ne_bytes());
    msg
}

#[test]
fn sendmsg_rejects_kernel_iov_base() {
    // iov_base 取内核高半区地址 (≥ USER_ADDR_MAX), len 非零.
    let iov: [[u64; 2]; 1] = [[0xFFFF_8000_0000_0000, 8]];
    let msg = msghdr_with_iov(iov.as_ptr() as u64, 1);
    let ret = sendmsg_syscall(0, msg.as_ptr() as u64, 0);
    assert!(
        matches!(ret, Err(Errno::Efault)),
        "sendmsg 应在裸搬运前以 EFAULT 拒绝内核 iov_base, 实际 {ret:?}"
    );
}

#[test]
fn recvmsg_rejects_kernel_iov_base() {
    // recvmsg 把接收数据写回 iov_base; 指向内核地址即为内核内存写.
    let iov: [[u64; 2]; 1] = [[0xFFFF_8000_0000_0000, 8]];
    let msg = msghdr_with_iov(iov.as_ptr() as u64, 1);
    let ret = recvmsg_syscall(0, msg.as_ptr() as u64, 0);
    assert!(
        matches!(ret, Err(Errno::Efault)),
        "recvmsg 应在裸搬运前以 EFAULT 拒绝内核 iov_base, 实际 {ret:?}"
    );
}
