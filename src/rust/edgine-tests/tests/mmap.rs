//! mmap/munmap 系统调用服务层参数验证测试
//!
//! 覆盖 services/mm/mmap.rs 的标量验证逻辑:
//! - mmap: size > 0, prot 合法, MAP_SHARED/MAP_PRIVATE 二选一
//! - munmap: addr != 0 && size > 0

use edgine_tests::*;

// ============================================================================
// mmap
// ============================================================================

#[test]
fn test_mmap_zero_size_rejected() {
    // size == 0 POSIX 非法
    assert_eq!(mmap_validate(0, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Err(Errno::EINVAL));
}

#[test]
fn test_mmap_typical_anon() {
    // 匿名私有映射
    assert_eq!(mmap_validate(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Ok(()));
}

#[test]
fn test_mmap_invalid_prot() {
    // prot 0x10 非法 (PROT_GROWSDOWN 等不在标准位)
    assert_eq!(mmap_validate(0, 4096, 0x10, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Err(Errno::EINVAL));
    // 0xFF 显然非法
    assert_eq!(mmap_validate(0, 4096, 0xFF, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Err(Errno::EINVAL));
}

#[test]
fn test_mmap_valid_prot_values() {
    // PROT_NONE
    assert_eq!(mmap_validate(0, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Ok(()));
    // PROT_READ
    assert_eq!(mmap_validate(0, 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Ok(()));
    // PROT_READ | PROT_WRITE
    assert_eq!(mmap_validate(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Ok(()));
    // PROT_READ | PROT_EXEC
    assert_eq!(mmap_validate(0, 4096, PROT_READ | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Ok(()));
    // PROT_READ | PROT_WRITE | PROT_EXEC
    assert_eq!(mmap_validate(0, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Ok(()));
}

#[test]
fn test_mmap_missing_share_flag() {
    // 没有任何 MAP_SHARED/MAP_PRIVATE → 非法
    assert_eq!(mmap_validate(0, 4096, PROT_READ, MAP_ANONYMOUS, -1, 0), Err(Errno::EINVAL));
    // 0 flags
    assert_eq!(mmap_validate(0, 4096, PROT_READ, 0, -1, 0), Err(Errno::EINVAL));
}

#[test]
fn test_mmap_conflict_share_flags() {
    // 同时 SHARED + PRIVATE 冲突
    assert_eq!(mmap_validate(0, 4096, PROT_READ, MAP_SHARED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Err(Errno::EINVAL));
}

#[test]
fn test_mmap_shared_anon() {
    // 共享匿名 (BSD 扩展)
    assert_eq!(mmap_validate(0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0), Ok(()));
}

#[test]
fn test_mmap_with_fixed_addr() {
    // MAP_FIXED 可与 SHARED/PRIVATE 组合
    assert_eq!(mmap_validate(0x1000_0000, 4096, PROT_READ, MAP_PRIVATE | MAP_FIXED, -1, 0), Ok(()));
}

#[test]
fn test_mmap_large_size() {
    // 大映射
    assert_eq!(mmap_validate(0, 0x1_0000_0000, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), Ok(()));
}
