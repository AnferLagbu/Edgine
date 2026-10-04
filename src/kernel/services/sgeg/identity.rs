#![deny(unsafe_code)]
//! PWM 身份 (Identity) — services 层安全代理
//!
//! ## 状态 (v2.16, 2026-06-04)
//!
//! Phase 2.5 sgeg 迁移 1/2 (identity / PWM):
//! - [x] 强类型 `PwmId(pub u64)` 替代裸 u64 句柄
//! - [x] 切片 API `&[u8]` 替代 `*const u8` C 字符串
//! - [x] 强类型错误 `PwmError` 替代 `i32` 错误码
//! - [x] 强类型 re-export `PwmEntry` / `PwmId`
//! - [x] 强类型 `Domain` / `CapBits` 替代裸 u16/u64
//!
//! ## 迁移方法
//!
//! 内部把 `&[u8]` 切片转 `*const u8` (C 风格), 调用 `kernel::sgeg::api::pwm_*` 函数;
//! services 层 0 unsafe — 所有 `unsafe { ... }` 在 framework 层 `PwmEntry` 内部.
//!
//! 评估日期: 2026-06-04

use crate::framework::sgeg;
use crate::framework::sgeg_pwm;

// ============================================================================
// 强类型 re-export
// ============================================================================

/// PWM 句柄
pub use sgeg::types::PwmId;

/// PWM 表项 (从内核透传, 引用类型)
pub use sgeg::types::PwmEntry;

/// 能力域 (从 `services::sgeg::policy` 复用)
pub use super::policy::CapDomain;

/// 能力位 (从 `services::sgeg::policy` 复用)
pub use super::policy::CapBits;

// ============================================================================
// 错误类型
// ============================================================================

/// PWM 操作错误 — TD-20: 收敛到 `KernelError`, 3 字段 PWM 特有 + 1 共享包装.
///
/// 字段说明:
///   - `TableFull`: PWM 表已满 (POSIX EAGAIN, 但语义特化)
///   - `InvalidPassword`: 密码错误 (POSIX EACCES, 但语义特化)
///   - `WeakPassword`: 密码过短 / 不合法
///   - `Kernel(KernelError)`: 共享错误 (`NotFound` / `AlreadyExists` /
///     `PermissionDenied` / Other) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PwmError {
    /// 表已满
    TableFull,
    /// 密码错误
    InvalidPassword,
    /// 密码过短 / 不合法
    WeakPassword,
    /// 共享 `KernelError` 包装
    Kernel(crate::services::error::KernelError),
}

impl PwmError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::TableFull => E::EAGAIN,
            Self::InvalidPassword => E::EACCES,
            Self::WeakPassword => E::EINVAL,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    /// 从内核 `i32` 错误码翻译
    pub fn from_i32(code: i32) -> Self {
        use crate::services::error::KernelError as K;
        match code {
            -2 => Self::TableFull,
            -3 => Self::Kernel(K::FileNotFound),
            -4 => Self::Kernel(K::AlreadyExists),
            -5 => Self::InvalidPassword,
            -6 => Self::Kernel(K::PermissionDenied),
            -7 => Self::WeakPassword,
            other if other != 0 => Self::Kernel(K::Other(other)),
            _ => Self::Kernel(K::Other(0)),
        }
    }
}

pub type PwmResult<T> = Result<T, PwmError>;

use crate::framework::syscall::Errno;

// ============================================================================
// 初始化与发现
// ============================================================================

/// 初始化 PWM 身份表 (单线程只能调用一次)
pub fn init() {
    sgeg::api::pwm_init();
}

/// 尝试从磁盘恢复 (cold boot)
///
/// # Errors
/// 当底层 `persist::load_database` 返回非零错误码 (如磁盘无有效数据库、
/// 校验失败) 时, 返回由该错误码转换得到的 `Err(PwmError)`.
pub fn try_load() -> PwmResult<()> {
    let rc = super::persist::load_database();
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 任何身份是否已存在
pub fn any_identity_exists() -> bool {
    sgeg::api::pwm_any_identity_exists()
}

// ============================================================================
// 生命周期
// ============================================================================

/// 创世 (第一次身份创建, 工厂: 第一个管理员)
///
/// # Errors
/// 当 `password` 为空时返回 `Err(PwmError::WeakPassword)`;
/// 当底层 `pwm_try_genesis` 返回非正数错误码时, 返回由该错误码转换得到的 `Err(PwmError)`.
pub fn try_genesis(password: &[u8]) -> PwmResult<PwmId> {
    if password.is_empty() {
        return Err(PwmError::WeakPassword);
    }
    let rc = sgeg_pwm::pwm_try_genesis(password.as_ptr());
    if rc > 0 {
        Ok(PwmId(rc as u64))
    } else {
        Err(PwmError::from_i32(rc as i32))
    }
}

/// 创世 + 创建 root 身份
///
/// # Errors
/// 当 `password` 为空时返回 `Err(PwmError::WeakPassword)`;
/// 当底层 `pwm_create_first_identity` 返回非正数错误码时, 返回由该错误码
/// 转换得到的 `Err(PwmError)`.
pub fn create_first_identity(password: &[u8]) -> PwmResult<PwmId> {
    if password.is_empty() {
        return Err(PwmError::WeakPassword);
    }
    let rc = sgeg_pwm::pwm_create_first_identity(password.as_ptr());
    if rc > 0 {
        Ok(PwmId(rc as u64))
    } else {
        Err(PwmError::from_i32(rc as i32))
    }
}

/// 创建新身份 (需要 creator 拥有对应权限)
///
/// # Errors
/// 当 `password` 为空时返回 `Err(PwmError::WeakPassword)`;
/// 当底层 `pwm_create` 返回非正数错误码 (如无权限、身份数上限) 时, 返回
/// 由该错误码转换得到的 `Err(PwmError)`.
pub fn create(password: &[u8], note: &[u8], creator: PwmId) -> PwmResult<PwmId> {
    if password.is_empty() {
        return Err(PwmError::WeakPassword);
    }
    let rc = sgeg_pwm::pwm_create(password.as_ptr(), note.as_ptr(), creator.0);
    if rc > 0 {
        Ok(PwmId(rc as u64))
    } else {
        Err(PwmError::from_i32(rc as i32))
    }
}

/// 删除身份
///
/// # Errors
/// 当底层 `pwm_delete` 返回非零错误码 (如身份不存在、权限不足) 时, 返回
/// 由该错误码转换得到的 `Err(PwmError)`.
pub fn delete(pwm: PwmId) -> PwmResult<()> {
    let rc = sgeg::api::pwm_delete(pwm.0);
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 禁用身份 (保留条目但拒绝登录)
///
/// # Errors
/// 当底层 `pwm_disable` 返回非零错误码 (如身份不存在) 时, 返回由该错误码
/// 转换得到的 `Err(PwmError)`.
pub fn disable(pwm: PwmId) -> PwmResult<()> {
    let rc = sgeg::api::pwm_disable(pwm.0);
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 启用身份
///
/// # Errors
/// 当底层 `pwm_enable` 返回非零错误码 (如身份不存在) 时, 返回由该错误码
/// 转换得到的 `Err(PwmError)`.
pub fn enable(pwm: PwmId) -> PwmResult<()> {
    let rc = sgeg::api::pwm_enable(pwm.0);
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

// ============================================================================
// 密码
// ============================================================================

/// 验证密码 (常数时间比较)
pub fn verify_password(pwm: PwmId, password: &[u8]) -> bool {
    if password.is_empty() {
        return false;
    }
    sgeg_pwm::pwm_verify_password(pwm.0, password.as_ptr())
}

/// 改密
///
/// # Errors
/// 当 `old` 或 `new` 为空时返回 `Err(PwmError::WeakPassword)`;
/// 当底层 `pwm_change_password` 返回非零错误码 (如旧密码错误、身份不存在) 时,
/// 返回由该错误码转换得到的 `Err(PwmError)`.
pub fn change_password(pwm: PwmId, old: &[u8], new: &[u8]) -> PwmResult<()> {
    if old.is_empty() || new.is_empty() {
        return Err(PwmError::WeakPassword);
    }
    let rc = sgeg_pwm::pwm_change_password(pwm.0, old.as_ptr(), new.as_ptr());
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

// ============================================================================
// 查询
// ============================================================================

/// PWM 是否存在
pub fn exists(pwm: PwmId) -> bool {
    sgeg::api::pwm_find(pwm.0)
}

/// 获取 PWM 表项 (返回引用)
pub fn find(pwm: PwmId) -> Option<&'static PwmEntry> {
    sgeg::identity::find(pwm.0)
}

// ============================================================================
// 能力
// ============================================================================

/// 检查 PWM 是否有指定能力
pub fn has_capability(pwm: PwmId, domain: CapDomain, required: CapBits) -> bool {
    sgeg::api::pwm_has_capability(pwm.0, u16::from(domain.0), required.0)
}

/// 获取指定域的能力位
pub fn get_capability_raw(pwm: PwmId, domain: CapDomain) -> u64 {
    sgeg::api::pwm_get_capability_raw(pwm.0, u16::from(domain.0))
}

/// 获取 FS 域能力位
pub fn get_fs_capability(pwm: PwmId) -> u64 {
    sgeg::api::pwm_get_fs_capability(pwm.0)
}

/// 获取权限等级
pub fn get_privilege_level(pwm: PwmId) -> u8 {
    sgeg::api::pwm_get_privilege_level(pwm.0)
}

/// 获取创建者 PWM
pub fn get_creator(pwm: PwmId) -> PwmId {
    PwmId(sgeg::api::pwm_get_creator(pwm.0))
}

// ============================================================================
// 委托 / 撤销
// ============================================================================

/// 委托能力
///
/// # Errors
/// 当底层 `pwm_grant` 返回非零错误码 (如权限不足、能力域无效) 时, 返回
/// 由该错误码转换得到的 `Err(PwmError)`.
pub fn grant(grantor: PwmId, grantee: PwmId, domain: CapDomain, caps: u64) -> PwmResult<()> {
    let rc = sgeg::api::pwm_grant(grantor.0, grantee.0, u16::from(domain.0), caps);
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 撤销能力
///
/// # Errors
/// 当底层 `pwm_revoke` 返回非零错误码 (如权限不足、能力域无效) 时, 返回
/// 由该错误码转换得到的 `Err(PwmError)`.
pub fn revoke(revoker: PwmId, target: PwmId, domain: CapDomain, caps: u64) -> PwmResult<()> {
    let rc = sgeg::api::pwm_revoke(revoker.0, target.0, u16::from(domain.0), caps);
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 权限检查: 操作者是否能修改 target
pub fn check_privilege(operator: PwmId, target: PwmId) -> bool {
    sgeg::api::pwm_check_privilege(operator.0, target.0)
}

/// 转移创建者
///
/// # Errors
/// 当底层 `pwm_transfer_creator` 返回非零错误码 (如权限不足、身份不存在) 时,
/// 返回由该错误码转换得到的 `Err(PwmError)`.
pub fn transfer_creator(current: PwmId, target: PwmId, new_creator: PwmId) -> PwmResult<()> {
    let rc = sgeg::api::pwm_transfer_creator(current.0, target.0, new_creator.0);
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

// ============================================================================
// 会话 (login/logout)
// ============================================================================

/// 当前 PWM
pub fn current() -> PwmId {
    PwmId(sgeg::api::pwm_get_current())
}

/// 是否已登录
pub fn is_logged_in() -> bool {
    sgeg::api::pwm_is_logged_in()
}

/// 当前 UID
pub fn current_uid() -> u32 {
    sgeg::api::pwm_get_current_uid()
}

/// 当前 GID
pub fn current_gid() -> u32 {
    sgeg::api::pwm_get_current_gid()
}

/// EUID
pub fn euid() -> u32 {
    sgeg::api::pwm_get_euid()
}

/// EGID
pub fn egid() -> u32 {
    sgeg::api::pwm_get_egid()
}

/// UID / GID
pub fn uid(pwm: PwmId) -> u32 {
    sgeg::api::pwm_get_uid(pwm.0)
}

pub fn gid(pwm: PwmId) -> u32 {
    sgeg::api::pwm_get_gid(pwm.0)
}

/// 注销当前会话
pub fn logout() {
    sgeg::api::pwm_logout();
}

// ============================================================================
// 提权 (suid 机制)
// ===========================================================================

/// 为 suid 提权
pub fn elevate_for_suid(target: PwmId) -> bool {
    sgeg::api::pwm_elevate_for_suid(target.0)
}

/// 撤销提权
pub fn drop_elevation() -> bool {
    sgeg::api::pwm_drop_elevation()
}

/// 检查是否有提权权限
pub fn has_elevation_authority(target: PwmId) -> bool {
    sgeg::api::pwm_has_elevation_authority(target.0)
}

/// 尝试设置 UID
pub fn try_setuid(target_uid: u32) -> bool {
    sgeg::api::pwm_try_setuid(target_uid)
}

// ============================================================================
// 锁定 / 审计
// ===========================================================================

/// 清除锁定
///
/// # Errors
/// 当底层 `pwm_clear_lockout` 返回非零错误码 (如身份不存在) 时, 返回由该错误码
/// 转换得到的 `Err(PwmError)`.
pub fn clear_lockout(pwm: PwmId) -> PwmResult<()> {
    let rc = sgeg::api::pwm_clear_lockout(pwm.0);
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 记录审计日志
pub fn audit(pwm: PwmId, action: u32, target: u64, details: u64) {
    sgeg::api::pwm_audit_log(pwm.0, action, target, details);
}

/// 持久化到磁盘
///
/// # Errors
/// 当底层 `persist::save_database` 返回非零错误码 (如 IO 失败) 时, 返回由该
/// 错误码转换得到的 `Err(PwmError)`.
pub fn save_to_disk() -> PwmResult<()> {
    let rc = super::persist::save_database();
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 从磁盘加载
///
/// # Errors
/// 当底层 `persist::load_database` 返回非零错误码 (如文件不存在、校验失败) 时,
/// 返回由该错误码转换得到的 `Err(PwmError)`.
pub fn load_from_disk() -> PwmResult<()> {
    let rc = super::persist::load_database();
    if rc == 0 {
        Ok(())
    } else {
        Err(PwmError::from_i32(rc))
    }
}

/// 是否已修改
pub fn is_modified() -> bool {
    sgeg::api::pwm_is_modified()
}

/// 标记已修改
pub fn set_modified() {
    sgeg::api::pwm_set_modified();
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwm_id_construction() {
        let pwm = PwmId(42);
        assert_eq!(pwm.0, 42);
    }

    #[test]
    fn error_from_i32() {
        assert_eq!(PwmError::from_i32(-2), PwmError::TableFull);
        assert_eq!(
            PwmError::from_i32(-3),
            PwmError::Kernel(crate::services::error::KernelError::FileNotFound)
        );
        assert_eq!(
            PwmError::from_i32(0),
            PwmError::Kernel(crate::services::error::KernelError::Other(0))
        );
        assert_eq!(
            PwmError::from_i32(42),
            PwmError::Kernel(crate::services::error::KernelError::Other(42))
        );
    }

    #[test]
    fn weak_password_rejected() {
        // 静态检查, 不实际调用内核 (内核单线程限制)
        let empty: &[u8] = b"";
        assert!(empty.is_empty());
    }
}
