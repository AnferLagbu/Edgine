//! I4 用户指针校验面收口回归锁 (安全缺口 ㉚ ㉜ ㊾)
//!
//! 锁定的契约:
//! - `validate_user_buf` 是完整判据: NULL / 上界 / `checked_add` 回绕 / 已映射;
//! - `validate_user_aligned::<T>` 额外要求 `align_of::<T>()` 对齐;
//! - `net::syscall` 六个入口 (accept / recvfrom / sendmsg / recvmsg /
//!   getsockname / getpeername) 在进入 sm 裸搬运前以 `EFAULT` 拒绝内核地址;
//! - `raw` 读写助手自带判据, 越界/未对齐指针不产生用户内存访问.
//!
//! 真用户内存搬运需 ring-0 SMAP/页表环境, 归 privileged 单测与 QEMU/e2e 腿;
//! host 腿只锁判据与拒绝路径 (见 unresolved-issues-2026-08-09.md 的 CR4 口径).

use edgine::kernel::privileged::net::syscall::{
    accept_syscall, getpeername_syscall, getsockname_syscall, recvfrom_syscall, recvmsg_syscall,
    sendmsg_syscall,
};
use edgine::kernel::privileged::syscall::Errno;
use edgine::kernel::privileged::syscall::api::{read_u64_from_user, write_u64_to_user};
use edgine::kernel::privileged::syscall::raw as syscall_raw;
use edgine::kernel::privileged::userptr::{validate_user_aligned, validate_user_buf};

/// 取源码中某函数从声明起到配对右花括号的切片.
///
/// 用于 ㉜ 的接线锁: SMAP 包裹只在 ring-0 + CR4.SMAP=1 时才有可观测差异,
/// host 腿无法执行该路径, 故以源码契约锁定每个用户访问助手都带
/// `smap_begin` / `smap_end`.
fn fn_body<'a>(src: &'a str, marker: &str) -> &'a str {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("源码中未找到 {marker}"));
    let open = start + src[start..].find('{').expect("函数缺少函数体");
    let mut depth = 0usize;
    for (i, b) in src[open..].bytes().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[start..=open + i];
                }
            }
            _ => {}
        }
    }
    panic!("函数体未闭合: {marker}")
}

/// 内核高半区地址: `validate_user_buf` 与所有入口都必须拒绝.
const KERNEL_PTR: u64 = 0xFFFF_8000_0000_0000;

#[test]
fn validate_user_buf_rejects_wrapping_range() {
    // 合法基址 + 巨长 len → `ptr + len` 回绕到小值, 未检查加法会同时绕过上界判据.
    assert!(!validate_user_buf(0x1000, u64::MAX));
    assert!(!validate_user_buf(0x1000, u64::MAX - 0x1000));
    // 恰好越过上界也必须拒绝.
    assert!(!validate_user_buf(0x0000_7FFF_FFFF_F000 - 1, 2));
    // 正常区间在 host 腿 (映射面跳过) 仍为真, 保证判据没有被写成恒假.
    assert!(validate_user_buf(0x1000, 8));
}

#[test]
fn validate_user_aligned_requires_alignment() {
    assert!(!validate_user_aligned::<u64>(0x1001));
    assert!(validate_user_aligned::<u64>(0x1000));
    assert!(!validate_user_aligned::<u32>(0x1002));
    assert!(validate_user_aligned::<u32>(0x1004));
}

#[test]
fn raw_u64_helpers_reject_invalid_pointers() {
    // 内核地址: 未映射/越界, 不得读取.
    assert!(syscall_raw::read_u64(KERNEL_PTR).is_none());
    assert!(!syscall_raw::write_u64(KERNEL_PTR, 1));
    // 未对齐: `read_volatile` 的前置条件不成立, 必须以判据拒绝而非执行.
    assert!(syscall_raw::read_u64(0x1001).is_none());
    assert!(!syscall_raw::write_u64(0x1001, 1));
}

#[test]
fn api_u64_helpers_reject_kernel_pointer() {
    assert!(read_u64_from_user(KERNEL_PTR).is_none());
    assert!(!write_u64_to_user(KERNEL_PTR, 1));
}

#[test]
fn net_entrypoints_reject_kernel_pointer() {
    // sendmsg/recvmsg 用越界槽位: 判据在槽位分发之前, 校验缺失时只会落到
    // `-EBADF` (干净断言失败), 不会进入 sm 层碰真实状态.
    const INVALID_SLOT: i32 = 100_000;
    let e = Errno::Efault.as_ret();
    assert_eq!(accept_syscall(0, KERNEL_PTR, 0, false), e);
    assert_eq!(accept_syscall(0, 0, KERNEL_PTR, false), e);
    assert_eq!(recvfrom_syscall(0, 0x1000, 16, 0, KERNEL_PTR, 0, false), e);
    assert_eq!(recvfrom_syscall(0, 0x1000, 16, 0, 0, KERNEL_PTR, false), e);
    assert_eq!(sendmsg_syscall(INVALID_SLOT, KERNEL_PTR, 0, false), e);
    assert_eq!(recvmsg_syscall(INVALID_SLOT, KERNEL_PTR, 0, false), e);
    assert_eq!(getsockname_syscall(0, KERNEL_PTR, 0), e);
    assert_eq!(getsockname_syscall(0, 0, KERNEL_PTR), e);
    assert_eq!(getpeername_syscall(0, KERNEL_PTR, 0), e);
    assert_eq!(getpeername_syscall(0, 0, KERNEL_PTR), e);
}

#[test]
fn smap_wrapping_is_wired_in_user_access_helpers() {
    let net = std::fs::read_to_string("../src/kernel/privileged/net/syscall.rs").unwrap();
    for f in [
        "fn raw_read_u32",
        "fn raw_write_u32",
        "fn raw_read_sun_family",
    ] {
        let body = fn_body(&net, f);
        assert!(
            body.contains("smap_begin()") && body.contains("smap_end()"),
            "㉜: {f} 缺少 SMAP 包裹"
        );
    }
    let syscall = std::fs::read_to_string("../src/kernel/privileged/syscall/mod.rs").unwrap();
    for f in [
        "pub fn read_u64(",
        "pub fn write_u64(",
        "pub fn write_u64_pair(",
        "pub fn write_struct<",
        "pub fn read_u64_from_user(",
        "pub fn read_struct_from_user<",
    ] {
        let body = fn_body(&syscall, f);
        assert!(
            body.contains("smap_begin()") && body.contains("smap_end()"),
            "㉜: {f} 缺少 SMAP 包裹"
        );
    }
    let sm = std::fs::read_to_string("../src/kernel/privileged/net/init/sm_fi.rs").unwrap();
    for f in [
        "pub(crate) unsafe fn write_sockaddr(",
        "pub unsafe extern \"C\" fn sm_sendmsg(",
        "pub unsafe extern \"C\" fn sm_recvmsg(",
    ] {
        let body = fn_body(&sm, f);
        assert!(
            body.contains("smap_begin()") && body.contains("smap_end()"),
            "㉜: {f} 缺少 SMAP 包裹"
        );
    }
}
