//! SGEG 身份与权限框架 API 层
//!
//! 统一的身份管理 (PWM) / 能力矩阵 / 会话管理 / 审计入口,
//! 是 `Edgine` 安全子系统的对外契约面。
//!
//! ## 调用方契约
//! - `syscall::mod` —— `SYS_SGEG_LOGIN/LOGOUT/CREATE/DELETE/GRANT/REVOKE/CHECK_CAP` 等
//! - `fs::vfs` —— `vfs_open/write` 等文件操作前调用 `pwm_get_current()` 获取权限上下文
//! - `proc::api` —— 进程创建时分配 PWM, 销毁时回收
//! - `net::init` —— socket 操作前的权限校验
//! - `freg::recovery` —— 会话状态纳入恢复域
//! - `console::gfx_console` —— 登录交互
//!
//! ## 内部接口
//! - `identity.rs` —— `IdentityTable`, PWM 生命周期管理
//! - `engine.rs` —— 能力检查引擎 (`check`/`get_caps`/`grant`/`revoke`)
//! - `session.rs` —— 会话管理器
//! - `audit.rs` —— 审计日志
//! - `capability.rs` —— `CapDomain` / `CapBits` 能力矩阵
//!
//! ## 安全约束
//! - `pwm_init()` 必须单线程调用且只能调用一次 (`AtomicBool` 保护)
//! - 所有 `pwm_*` 函数内部使用 `identity::get_table()` 获取全局单例
//! - 密码传递走 `*const u8` C 风格字符串, 在入口处做 null 检查
//! - 能力检查在 engine 层用位运算, 无锁 (`AtomicU64` 矩阵)
//!
//! ## 性能特征
//! - 能力检查: O(1) 位运算, ≤ 5ns
//! - 身份查找: O(1) 哈希表
//! - 密码验证: SHA-256 计算, ~1μs
//!
//! ## 设计理念
//! - 无 Root 概念, 细粒度 `CapDomain` 矩阵
//! - 支持委托 (grant) 与撤销 (revoke)
//! - 完整审计追踪
//!
//! 所有公开函数使用 `#[no_mangle]` 以保证跨模块符号名稳定。

#![deny(missing_docs)]

use super::audit;
use super::engine;
use super::identity;
use super::session;
use super::types::{AuditAction, CapBits, CapDomain, PwmEntry};
use crate::privileged::glue::CStrExt;

macro_rules! klog_pwm {
    ($($arg:tt)*) => {
        $crate::klog_ffi!(klog_ffi_warn, $($arg)*)
    };
}

static INITIALIZED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// 初始化 PWM 身份子系统 (仅首次调用生效, 重复调用直接返回)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_init() {
    if INITIALIZED
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        return;
    }
    let t = identity::get_table();
    t.init();
    klog_pwm!("PWM v5 initialized");
}

/// 查询身份表中是否已存在任何身份 (用于判定是否需要 genesis 引导)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_any_identity_exists() -> bool {
    identity::get_table().any_identity_exists()
}

/// 以给定密码创建首个 root 身份, 返回新 PWM (失败返回负错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_try_genesis(password: *const u8) -> i64 {
    let pwd = password.as_kstr();
    match identity::get_table().bootstrap(pwd, "root") {
        Ok(pwm) => pwm.cast_signed(),
        Err(e) => i64::from(e.as_i32()),
    }
}

/// 创建新身份 (密码 / 备注 / 创建者 PWM), 返回新 PWM (失败返回负错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_create(password: *const u8, note: *const u8, creator_pwm: u64) -> i64 {
    let pwd = password.as_kstr();
    let note_kstr = note.as_kstr();
    match identity::get_table().create(pwd, note_kstr, creator_pwm) {
        Ok(pwm) => pwm.cast_signed(),
        Err(e) => i64::from(e.as_i32()),
    }
}

/// 删除指定 PWM 的身份 (0 = 成功, 非零 = 错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_delete(pwm: u64) -> i32 {
    match identity::get_table().delete(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 停用指定 PWM 的身份 (0 = 成功, 非零 = 错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_disable(pwm: u64) -> i32 {
    match identity::get_table().disable(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 重新启用指定 PWM 的身份 (0 = 成功, 非零 = 错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_enable(pwm: u64) -> i32 {
    match identity::get_table().enable(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 校验指定 PWM 的密码 (密码指针为空直接判失败)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_verify_password(pwm: u64, password: *const u8) -> bool {
    if password.is_null() {
        return false;
    }
    let pwd = password.as_kstr();
    identity::get_table().verify_password(pwm, pwd)
}

/// 修改指定 PWM 的密码 (旧密码校验通过后写入新密码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_change_password(pwm: u64, old: *const u8, new: *const u8) -> i32 {
    let o = old.as_kstr();
    let n = new.as_kstr();
    match identity::get_table().change_password(pwm, o, n) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 判定指定 PWM 是否存在于身份表
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_find(pwm: u64) -> bool {
    identity::find(pwm).is_some()
}

/// 取得指定 PWM 的身份条目只读指针 (不存在时返回空指针)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_find_entry(pwm: u64) -> *const PwmEntry {
    identity::find(pwm).map_or(core::ptr::null(), core::ptr::from_ref::<PwmEntry>)
}

/// 取指定 PWM 在指定能力域上的全部能力位 (原始 u64 形式)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_has_cap_raw(pwm: u64, domain: u16, _cap_bit: u8) -> u64 {
    engine::get_caps(pwm, CapDomain(domain)).as_u64()
}

/// 以给定密码创建首个身份, 返回新 PWM (genesis 的别名入口)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_create_first_identity(password: *const u8) -> i64 {
    let pwd = password.as_kstr();
    match identity::get_table().bootstrap(pwd, "root") {
        Ok(pwm) => pwm.cast_signed(),
        Err(e) => i64::from(e.as_i32()),
    }
}

/// 取指定 PWM 在文件系统能力域上的能力位
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_fs_capability(pwm: u64) -> u64 {
    engine::get_caps(pwm, CapDomain::FS).as_u64()
}

/// 判定指定 PWM 在指定域上是否具备所要求的能力位
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_has_capability(pwm: u64, domain: u16, required: u64) -> bool {
    engine::check(pwm, CapDomain(domain), CapBits(required))
}

/// 取指定 PWM 在指定能力域上的能力位 (原始 u64 形式)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_capability_raw(pwm: u64, domain: u16) -> u64 {
    engine::get_caps(pwm, CapDomain(domain)).as_u64()
}

/// 原子设置**当前** PWM 在指定域的能力位 (`capset` 机制入口).
///
/// 安全约束 (无提权 + 不破下限):
/// 1. 新值必须是当前值的**子集** — 出现当前值之外的位即视为提权尝试, 返回 -1;
/// 2. 新值不得低于 `VIABLE_FLOOR` 域下限 (与 `identity::revoke` 同口径), 返回 -1.
///
/// 仅作用于当前会话 PWM (不接受 pwm 参数), 杜绝跨进程能力篡改.
#[must_use]
pub fn pwm_set_current_capability_raw(domain: u16, caps: u64) -> i32 {
    if domain >= 16 {
        return -1;
    }
    let d = CapDomain(domain);
    let pwm = session::get_current_pwm();
    let Some(entry) = identity::find(pwm) else {
        return -1;
    };
    let current = entry.load_caps(d).as_u64();
    // 禁止提权: 新值必须是当前值子集.
    if caps & !current != 0 {
        return -1;
    }
    // 不得破坏能力下限.
    let floor = super::capability::VIABLE_FLOOR[domain as usize];
    if caps & floor != floor {
        return -1;
    }
    entry.store_caps(d, CapBits(caps));
    0
}

/// 取指定 PWM 的权限等级
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_privilege_level(pwm: u64) -> u8 {
    engine::get_privilege_level(pwm)
}

/// 取指定 PWM 的创建者 PWM (无创建者时返回 0)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_creator(pwm: u64) -> u64 {
    engine::get_creator(pwm)
}

/// 由授权方向受让方授予指定能力 (0 = 成功, 非零 = 错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_grant(grantor_pwm: u64, grantee_pwm: u64, domain: u16, caps: u64) -> i32 {
    match identity::get_table().grant(grantor_pwm, grantee_pwm, CapDomain(domain), CapBits(caps)) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 由撤销方回收目标方的指定能力 (0 = 成功, 非零 = 错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_revoke(revoker_pwm: u64, target_pwm: u64, domain: u16, caps: u64) -> i32 {
    match identity::get_table().revoke(revoker_pwm, target_pwm, CapDomain(domain), CapBits(caps)) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 转移指定身份的创建者归属 (0 = 成功, 非零 = 错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_transfer_creator(current_creator: u64, target: u64, new_creator: u64) -> i32 {
    match identity::get_table().transfer_creator(current_creator, target, new_creator) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 判定操作方对目标方是否具备管辖权限
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_check_privilege(operator: u64, target: u64) -> bool {
    engine::check_privilege(operator, target)
}

/// 以备注与密码登录会话, 返回登录后的 PWM (失败返回负错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_login(note: *const u8, password: *const u8) -> i64 {
    let n = note.as_kstr();
    let p = password.as_kstr();
    match session::login(n, p) {
        Ok(pwm) => pwm.cast_signed(),
        Err(e) => i64::from(e.as_i32()),
    }
}

/// 注销当前会话
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_logout() {
    session::logout();
}

/// 取当前会话的 PWM (未登录返回 0)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current() -> u64 {
    session::get_current_pwm()
}

/// 取当前会话身份条目的只读指针 (未登录返回空指针)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current_entry() -> *const PwmEntry {
    session::get_current_entry()
}

/// 判定当前是否已登录
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_is_logged_in() -> bool {
    session::is_logged_in()
}

/// 取当前会话的 UID
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current_uid() -> u32 {
    session::get_current_uid()
}

/// 取当前会话的 GID
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current_gid() -> u32 {
    session::get_current_gid()
}

/// 取当前会话的有效 UID
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_euid() -> u32 {
    session::get_euid()
}

/// 取当前会话的有效 GID
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_egid() -> u32 {
    session::get_egid()
}

/// 为 set-uid 场景临时提升到目标身份的权力
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_elevate_for_suid(target_pwm: u64) -> bool {
    session::elevate_for_suid(target_pwm)
}

/// 放弃 set-uid 场景的临时提权
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_drop_elevation() -> bool {
    session::drop_elevation()
}

/// 判定当前会话是否具备对目标身份的提权权限
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_has_elevation_authority(target_pwm: u64) -> bool {
    session::has_elevation_authority(target_pwm)
}

/// 尝试把当前会话切换到目标 UID
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_try_setuid(target_uid: u32) -> bool {
    session::try_setuid(target_uid)
}

/// 取指定 PWM 的 UID (身份不存在时返回 `0xFFFFFFFF`)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_uid(pwm: u64) -> u32 {
    identity::find(pwm).map_or(0xFFFF_FFFF, PwmEntry::get_uid)
}

/// 取指定 PWM 的 GID (身份不存在时返回 `0xFFFFFFFF`)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_gid(pwm: u64) -> u32 {
    identity::find(pwm).map_or(0xFFFF_FFFF, PwmEntry::get_gid)
}

/// 清除指定 PWM 的登录锁定状态 (0 = 成功, 非零 = 错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_clear_lockout(pwm: u64) -> i32 {
    match session::clear_lockout(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

/// 判定身份表自上次持久化以来是否被修改
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_is_modified() -> bool {
    identity::get_table().is_modified()
}

/// 标记身份表已被修改, 触发下一次持久化
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_set_modified() {
    identity::get_table().set_modified();
}

// ============================================================================
// 文件创建掩码 umask (单一全局值)
// ============================================================================

use core::sync::atomic::{AtomicU32, Ordering};

static UMASK: AtomicU32 = AtomicU32::new(0o022);

/// 设置进程 umask, 返回旧值
pub fn umask_set(new_mask: u32) -> u32 {
    UMASK.swap(new_mask & 0o777, Ordering::SeqCst)
}

/// 取当前 umask
pub fn umask_get() -> u32 {
    UMASK.load(Ordering::SeqCst)
}

/// 按动作编号写入一条 PWM 审计记录
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_audit_log(pwm: u64, action: u32, target: u64, details: u64) {
    let act = match action {
        1 => AuditAction::Login,
        2 => AuditAction::Logout,
        3 => AuditAction::Create,
        4 => AuditAction::Delete,
        8 => AuditAction::PasswordChange,
        10 => AuditAction::Grant,
        11 => AuditAction::Revoke,
        12 => AuditAction::TransferCreator,
        13 => AuditAction::FirstTokenGrant,
        _ => AuditAction::Modify,
    };
    audit::log(pwm, act, target, 0, details);
}

/// 输出全部审计记录
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_audit_dump() {
    audit::dump();
}

/// 以首个身份凭据恢复身份表, 返回恢复出的 PWM (失败返回负错误码)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_recover_first(password: *const u8, note: *const u8) -> i64 {
    let p = password.as_kstr();
    let n = note.as_kstr();
    match identity::get_table().recover_with_first(p, n) {
        Ok(pwm) => pwm.cast_signed(),
        Err(e) => i64::from(e.as_i32()),
    }
}
