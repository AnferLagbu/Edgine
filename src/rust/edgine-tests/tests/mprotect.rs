//! mprotect 服务层参数验证测试
//!
//! 覆盖 functions/mm/mprotect.rs 的纯标量验证逻辑.

use edgine_tests::*;

#[test]
fn test_mprotect_valid_input() {
    // addr 页对齐, len > 0, prot 合法
    assert_eq!(mprotect_validate(0x1000, 0x2000, PROT_READ), Ok(()));
    assert_eq!(mprotect_validate(0x1000, 0x2000, PROT_READ | PROT_WRITE), Ok(()));
    assert_eq!(mprotect_validate(0x1000, 0x2000, PROT_READ | PROT_WRITE | PROT_EXEC), Ok(()));
    assert_eq!(mprotect_validate(0x1000, 0x2000, PROT_NONE), Ok(()));
}

#[test]
fn test_mprotect_unaligned_addr_rejected() {
    // addr 必须页对齐 (4KB)
    assert_eq!(mprotect_validate(0x1001, 0x2000, PROT_READ), Err(Errno::EINVAL));
    assert_eq!(mprotect_validate(0x1234, 0x2000, PROT_READ), Err(Errno::EINVAL));
    assert_eq!(mprotect_validate(0xFFF, 0x2000, PROT_READ), Err(Errno::EINVAL));
}

#[test]
fn test_mprotect_zero_len_rejected() {
    // len 必须 > 0
    assert_eq!(mprotect_validate(0x1000, 0, PROT_READ), Err(Errno::EINVAL));
}

#[test]
fn test_mprotect_invalid_prot_rejected() {
    // 非法 prot 位 (bit 3+)
    assert_eq!(mprotect_validate(0x1000, 0x2000, 0x08), Err(Errno::EINVAL));
    assert_eq!(mprotect_validate(0x1000, 0x2000, 0xFF), Err(Errno::EINVAL));
    assert_eq!(mprotect_validate(0x1000, 0x2000, 0x10), Err(Errno::EINVAL));
}

#[test]
fn test_mprotect_alignment_4k() {
    // 4KB 对齐的合法地址
    for addr in [0x1000u64, 0x2000, 0x1_000_000, 0x10_000_000] {
        assert_eq!(mprotect_validate(addr, 0x1000, PROT_READ), Ok(()));
    }
}

#[test]
fn test_mprotect_typical_user_scenarios() {
    // 典型用户场景: mmap 后设只读
    assert_eq!(mprotect_validate(0x7fff_ffff_f000, 0x1000, PROT_READ), Ok(()));
    // mprotect 让堆可执行 (JIT)
    assert_eq!(mprotect_validate(0x7fff_ffff_e000, 0x1000, PROT_READ | PROT_WRITE | PROT_EXEC), Ok(()));
    // 取消映射 (PROT_NONE)
    assert_eq!(mprotect_validate(0x7fff_ffff_d000, 0x2000, PROT_NONE), Ok(()));
}

#[test]
fn test_mprotect_boundary_values() {
    // 最小合法值
    assert_eq!(mprotect_validate(0x1000, 1, PROT_READ), Ok(()));
    // 大范围
    assert_eq!(mprotect_validate(0x1000, u64::MAX, PROT_READ), Ok(()));
}

#[test]
fn test_mprotect_alignment_at_page_boundary_minus_one() {
    // 0xFFF 边界 (不是页对齐)
    assert_eq!(mprotect_validate(0xFFF, 0x1000, PROT_READ), Err(Errno::EINVAL));
    // 0x1000 是页对齐
    assert_eq!(mprotect_validate(0x1000, 0x1000, PROT_READ), Ok(()));
}
