//! TD-26: access_syscall 能力制校验回归 (B06-04 / DECISION-077 方案 A)
//!
//! 原镜像内核 [src/kernel/services/fs/access.rs] 的 mode→FS_CAP 映射逻辑,
//! 现改引内核真实实现 (host-test feature 暴露), 验证:
//!   1. `F_OK` (mode=0) 不要求任何能力
//!   2. `R_OK`/`W_OK`/`X_OK` 正确映射到 `FS_CAP_READ/WRITE/EXECUTE`
//!   3. 组合 mode (位或) 映射为组合能力位
//!   4. 能力充足 → 放行; 能力不足 → EACCES (走内核 `framework::sgeg::pwm_has_capability`)
//!   5. mode 越界 → EINVAL (与内核 `0..=0o7` 校验一致)
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `FS_CAP_*` 常量表与 `pwm_has_capability` mock 实现, 改引:
//! - `framework::sgeg::pwm_has_capability` — 内核真实能力检查 (engine::check)
//! - `services::sgeg::capability::{FS_CAP_*, CAP_DOMAIN_FS}` — 能力位/域常量
//! - 测试身份经 `identity::get_table().create(..)` 真实注册 (初始 FS 能力 =
//!   `capability::VIABLE_FLOOR` = READ|EXECUTE), 能力授予以 `PwmEntry::fetch_or_caps` 完成.
//!
//! ## 因内核 host 不可测已移除
//! 原镜像的 `path_exists` 存在性检查 (对应内核 `access_syscall` 内
//! `vfs_stat_safe`) 已移除: VFS 全局状态 (VFS_MANAGER) 在 host 未初始化, 不可测.
//! 原镜像 `current_pwm()` 读取进程凭证部分 (内核 `access.rs::current_pwm` 为私有
//! 函数, 依赖 `sgeg::api::pwm_get_current()` 进程上下文) 亦不可测, 已移除.
//! 以上两部分的回归覆盖保留在 QEMU 集成测试.

use std::sync::OnceLock;

use edgine::kernel::framework::sgeg::identity;
use edgine::kernel::framework::sgeg::pwm_has_capability;
use edgine::kernel::services::sgeg::capability::{
    CAP_DOMAIN_FS, FS_CAP_EXECUTE, FS_CAP_READ, FS_CAP_WRITE,
};
use edgine::kernel::services::sgeg::types::{CapBits, CapDomain};

const EACCES: i32 = -13; // POSIX EACCES
const EINVAL: i32 = -22; // POSIX EINVAL

// 内核 [services/fs/access.rs] 的 R_OK/W_OK/X_OK/F_OK 常量
const F_OK: i32 = 0;
const R_OK: i32 = 4;
const W_OK: i32 = 2;
const X_OK: i32 = 1;

/// 注册并缓存测试身份 (creator=0 → 最高特权级). 新身份的初始 FS 能力 =
/// `services::sgeg::capability::VIABLE_FLOOR[FS]` = READ|EXECUTE (无 WRITE),
/// 即内核"可行下界"进程的权限形态.
fn test_pwm() -> u64 {
    static PWM: OnceLock<u64> = OnceLock::new();
    *PWM.get_or_init(|| {
        identity::get_table()
            .create("td26-pw", "td26-access-cap", 0)
            .expect("注册测试身份失败")
    })
}

/// 独立测试身份, 供"授予 WRITE"用例修改能力而不污染 `test_pwm`
/// (避免测试并行运行时互相干扰).
fn write_pwm() -> u64 {
    static PWM: OnceLock<u64> = OnceLock::new();
    *PWM.get_or_init(|| {
        identity::get_table()
            .create("td26-w", "td26-grant-write", 0)
            .expect("注册测试身份失败")
    })
}

/// 镜像 [services/fs/access.rs::access_syscall] 中 host 可测的判定片段:
/// mode→FS_CAP 映射 + 内核 `pwm_has_capability` 能力制校验.
/// 返回 None = 通过, Some(errno) = 拒绝.
/// (内核 access_syscall 的 current_pwm / vfs_stat_safe 依赖进程与 VFS 上下文,
/// host 不可测, 见文件头说明.)
fn access_check(pwm: u64, mode: i32) -> Option<i32> {
    // 越界校验 (与内核 `0..=0o7` 一致)
    if !(0..=0o7).contains(&mode) {
        return Some(EINVAL);
    }
    // 能力制校验: F_OK 不要求能力
    let mut required_caps: u64 = 0;
    if mode & R_OK != 0 {
        required_caps |= FS_CAP_READ;
    }
    if mode & W_OK != 0 {
        required_caps |= FS_CAP_WRITE;
    }
    if mode & X_OK != 0 {
        required_caps |= FS_CAP_EXECUTE;
    }
    if required_caps != 0 && !pwm_has_capability(pwm, CAP_DOMAIN_FS, required_caps) {
        return Some(EACCES);
    }
    None
}

#[test]
fn f_ok_requires_no_capability() {
    // F_OK (mode=0): 无能力进程也应通过 (仅存在性检查, 能力位为空)
    assert_eq!(access_check(test_pwm(), F_OK), None);
}

#[test]
fn viable_floor_process_can_read_execute() {
    // 内核 VIABLE_FLOOR (FS_CAP_READ|EXECUTE) 进程:
    // R_OK 与 X_OK 通过, W_OK 拒绝
    let pwm = test_pwm();
    assert_eq!(access_check(pwm, R_OK), None);
    assert_eq!(access_check(pwm, X_OK), None);
    assert_eq!(access_check(pwm, W_OK), Some(EACCES));
}

#[test]
fn r_ok_maps_to_fs_cap_read() {
    // R_OK → FS_CAP_READ (bit0): viable floor 进程有 READ → 通过
    let pwm = test_pwm();
    assert_eq!(access_check(pwm, R_OK), None);
}

#[test]
fn w_ok_maps_to_fs_cap_write() {
    // W_OK → FS_CAP_WRITE (bit1): 内核 VIABLE_FLOOR 默认进程无 WRITE → 拒绝
    let pwm = test_pwm();
    assert_eq!(access_check(pwm, W_OK), Some(EACCES));
}

#[test]
fn x_ok_maps_to_fs_cap_execute() {
    // X_OK → FS_CAP_EXECUTE (bit2): viable floor 进程有 EXECUTE → 通过
    let pwm = test_pwm();
    assert_eq!(access_check(pwm, X_OK), None);
}

#[test]
fn combined_mode_maps_to_combined_caps() {
    let pwm = test_pwm();
    // R_OK | X_OK → READ|EXECUTE: viable floor 全有 → 通过
    assert_eq!(access_check(pwm, R_OK | X_OK), None);
    // R_OK | W_OK → READ|WRITE: 无 WRITE → 拒绝
    assert_eq!(access_check(pwm, R_OK | W_OK), Some(EACCES));
    // R_OK | W_OK | X_OK → 同样拒绝
    assert_eq!(access_check(pwm, R_OK | W_OK | X_OK), Some(EACCES));
}

#[test]
fn grant_write_allows_w() {
    // 通过内核 PwmEntry::fetch_or_caps 真实授予 FS WRITE 后, W_OK 放行
    // (用独立身份 write_pwm, 不污染 test_pwm 的 viable floor 断言)
    let pwm = write_pwm();
    assert_eq!(access_check(pwm, W_OK), Some(EACCES));
    identity::find(pwm)
        .expect("测试身份应已注册")
        .fetch_or_caps(CapDomain::FS, CapBits(FS_CAP_WRITE));
    assert_eq!(access_check(pwm, W_OK), None);
    assert_eq!(access_check(pwm, R_OK | W_OK | X_OK), None);
}

#[test]
fn invalid_mode_rejected() {
    // mode 越界 (>= 0o10) → EINVAL, 与内核 `0..=0o7` 校验一致
    let pwm = test_pwm();
    assert_eq!(access_check(pwm, 0o10), Some(EINVAL));
    assert_eq!(access_check(pwm, 0o77), Some(EINVAL));
    assert_eq!(access_check(pwm, -1), Some(EINVAL));
}

#[test]
fn bootstrap_pwm_has_all_caps() {
    // 内核差异: pwm==0 是 bootstrap 全权身份 (engine::check 直接放行),
    // 与旧 mock "pwm==0 无能力" 相反. 以内核真实语义为准.
    assert_eq!(access_check(0, W_OK), None);
    assert_eq!(access_check(0, R_OK | W_OK | X_OK), None);
}

#[test]
fn unregistered_pwm_rejected() {
    // 未注册 pwm: 内核 pwm_has_capability → identity::find None → false → EACCES
    const UNREGISTERED: u64 = 0x1234_5678_9ABC_DEF0;
    assert!(identity::find(UNREGISTERED).is_none(), "该 pwm 不应已注册");
    assert_eq!(access_check(UNREGISTERED, R_OK), Some(EACCES));
    assert_eq!(access_check(UNREGISTERED, W_OK), Some(EACCES));
}
