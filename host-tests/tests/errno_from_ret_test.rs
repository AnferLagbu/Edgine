//! Errno::from_ret 映射完整性契约测试 (B05-04)
//!
//! 权威实现: `framework::errno::Errno` 的 `from_ret` (DECISION-J 2026-09-13:
//! syscall/types 迁回 framework, services 侧 re-export 兼容). 本测试经
//! services 公共 API 直接验证内核真实实现:
//! 1. 所有已定义的 `Errno` 变体编号都能被 `from_ret` 正确往返映射
//!    (返回的枚举编号与输入负返回码绝对值一致)
//! 2. 未知错误码回退 `EINVAL` (POSIX 约定)
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `mirror_from_ret` 手工映射表 (78 项平行实现), 改引
//! `queenx::kernel::services::syscall::types::Errno::from_ret` 真实实现.
//! 原镜像表与内核 `framework/errno.rs::from_ret` 一一对应, 不再需要双维护.

use queenx::kernel::services::syscall::types::Errno;

/// 内核 `Errno` 枚举中已定义的全部编号 (B05-04 验收: 这些必须可往返)
const ALL_DEFINED_ERRNOS: &[i32] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 60, 61, 62, 63, 64, 71, 74,
    75, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107,
    108, 110, 111, 112, 113, 114, 115,
];

#[test]
fn all_defined_errnos_round_trip() {
    // B05-04: 每个已定义 Errno 变体 must 可往返 (from_ret(-n) == n)
    for &n in ALL_DEFINED_ERRNOS {
        let mapped = Errno::from_ret(-(n as i64)).as_i32();
        assert_eq!(
            mapped, n,
            "B05-04: from_ret(-{}) 应映射回 {} (Errno 变体), 实际 = {}",
            n, n, mapped
        );
    }
}

#[test]
fn all_defined_errnos_round_trip_negative_input() {
    // 输入必须是负数 (POSIX -errno 约定); 非负数无意义
    for &n in ALL_DEFINED_ERRNOS {
        // from_ret 对负数输入取绝对值映射
        let mapped = Errno::from_ret(-(n as i64)).as_i32();
        assert!(mapped > 0, "B05-04: 映射结果必须为正 errno, 输入 -{}", n);
    }
}

#[test]
fn unknown_errno_falls_back_to_einval() {
    // 未定义错误码回退 EINVAL (22)
    for unknown in [0i64, 44, 45, 65, 76, 109, 116, 1000, i64::MAX] {
        let mapped = Errno::from_ret(-unknown).as_i32();
        assert_eq!(
            mapped, 22,
            "B05-04: from_ret(-{}) 应回退 EINVAL(22), 实际 = {}",
            unknown, mapped
        );
    }
}

#[test]
fn specific_errno_values_match_linux() {
    // 抽查关键 errno 编号与 Linux x86_64 一致 (DECISION-037: 0-299 直接 Linux ABI)
    assert_eq!(Errno::from_ret(-1).as_i32(), 1, "EPERM");
    assert_eq!(Errno::from_ret(-38).as_i32(), 38, "ENOSYS");
    assert_eq!(Errno::from_ret(-13).as_i32(), 13, "EACCES");
    assert_eq!(Errno::from_ret(-95).as_i32(), 95, "ENOTSUP");
    assert_eq!(Errno::from_ret(-98).as_i32(), 98, "EADDRINUSE");
    assert_eq!(Errno::from_ret(-111).as_i32(), 111, "ECONNREFUSED");
    assert_eq!(Errno::from_ret(-115).as_i32(), 115, "EINPROGRESS");
}

#[test]
fn enum_variant_values_match_linux() {
    // 抽查 Errno 枚举判别值直接等于 POSIX 编号 (repr(i32))
    assert_eq!(Errno::EPERM.as_i32(), 1);
    assert_eq!(Errno::EINVAL.as_i32(), 22);
    assert_eq!(Errno::EINPROGRESS.as_i32(), 115);
}
