// SPDX-License-Identifier: MPL-2.0
//! `T7-a` Linux syscall ABI 边界翻译锁.
//!
//! 内部 canonical 统一为 x86_64 Linux 编号; aarch64 入口/用户库门经
//! `edgine-abi::linux_syscall` 翻译. 本测试钉住:
//! 1. 常用公开号的 aarch64 映射与反向映射互逆;
//! 2. aarch64 无对应号的 x86_64 遗留号进 legacy 私有段, 不占 Linux 公开号;
//! 3. EG/SGEG 私有号原样透传且仍在 `PRIVATE_END` 以内;
//! 4. 未知号返回 `UNKNOWN`, 内核入口据此返回 `-ENOSYS`.

use edgine_abi::linux_syscall::{
    PRIVATE_BASE, PRIVATE_END, PRIVATE_LEGACY_BASE, UNKNOWN, aarch64_to_canonical,
    canonical_to_aarch64,
};

#[test]
fn common_linux_numbers_are_bijective() {
    for (canonical, aarch64) in [
        (0u64, 63u64), // read
        (1, 64),       // write
        (3, 57),       // close
        (9, 222),      // mmap
        (15, 139),     // rt_sigreturn
        (56, 220),     // clone
        (59, 221),     // execve
        (61, 260),     // wait4
        (72, 25),      // fcntl
        (218, 96),     // set_tid_address
        (257, 56),     // openat
        (262, 79),     // newfstatat
        (302, 261),    // prlimit64
    ] {
        assert_eq!(canonical_to_aarch64(canonical), aarch64);
        assert_eq!(aarch64_to_canonical(aarch64), canonical);
    }
}

#[test]
fn x86_only_legacy_numbers_use_private_legacy_segment() {
    for canonical in [
        2u64, 4, 6, 7, 21, 22, 23, 33, 34, 37, 57, 78, 82, 83, 84, 85, 86, 87, 88, 89, 90, 92,
    ] {
        let mapped = canonical_to_aarch64(canonical);
        assert_eq!(mapped, PRIVATE_LEGACY_BASE + canonical);
        assert_eq!(aarch64_to_canonical(mapped), canonical);
    }
}

#[test]
fn private_numbers_pass_through() {
    for old in [400u64, 423, 464, 500, 720, 766, 812, 816] {
        let nr = PRIVATE_BASE + old;
        assert_eq!(canonical_to_aarch64(nr), nr);
        assert_eq!(aarch64_to_canonical(nr), nr);
        assert!(nr < PRIVATE_END);
    }
}

#[test]
fn unknown_numbers_fail_closed() {
    assert_eq!(canonical_to_aarch64(0x3000), UNKNOWN);
    assert_eq!(canonical_to_aarch64(PRIVATE_END), UNKNOWN);
    assert_eq!(aarch64_to_canonical(9999), UNKNOWN);
    assert_eq!(aarch64_to_canonical(PRIVATE_END), UNKNOWN);
    // aarch64 rseq 公开号 293 尚未映射；必须保持 UNKNOWN -> 内核 -ENOSYS，
    // 不能误落到 canonical 侧任何已实现 syscall。
    assert_eq!(aarch64_to_canonical(293), UNKNOWN);
}
