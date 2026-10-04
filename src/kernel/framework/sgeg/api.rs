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

use super::audit;
use super::engine;
use super::identity;
use super::session;
use super::types::{AuditAction, CapBits, CapDomain, PwmEntry};
use crate::framework::lib::CStrExt;

macro_rules! klog_pwm {
    ($($arg:tt)*) => {
        $crate::klog_ffi!(klog_ffi_warn, $($arg)*)
    };
}

static INITIALIZED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_any_identity_exists() -> bool {
    identity::get_table().any_identity_exists()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_try_genesis(password: *const u8) -> i64 {
    let pwd = password.as_kstr();
    match identity::get_table().bootstrap(pwd, "root") {
        Ok(pwm) => pwm as i64,
        Err(e) => i64::from(e.as_i32()),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
pub extern "C" fn pwm_create(password: *const u8, note: *const u8, creator_pwm: u64) -> i64 {
    let pwd = password.as_kstr();
    let nte = note.as_kstr();
    match identity::get_table().create(pwd, nte, creator_pwm) {
        Ok(pwm) => pwm as i64,
        Err(e) => i64::from(e.as_i32()),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_delete(pwm: u64) -> i32 {
    match identity::get_table().delete(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_disable(pwm: u64) -> i32 {
    match identity::get_table().disable(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_enable(pwm: u64) -> i32 {
    match identity::get_table().enable(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_verify_password(pwm: u64, password: *const u8) -> bool {
    if password.is_null() {
        return false;
    }
    let pwd = password.as_kstr();
    identity::get_table().verify_password(pwm, pwd)
}

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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_find(pwm: u64) -> bool {
    identity::find(pwm).is_some()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
pub extern "C" fn pwm_find_entry(pwm: u64) -> *const PwmEntry {
    identity::find(pwm).map_or(core::ptr::null(), |e| e as *const PwmEntry)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_has_cap_raw(pwm: u64, domain: u16, _cap_bit: u8) -> u64 {
    engine::get_caps(pwm, CapDomain(domain)).as_u64()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_create_first_identity(password: *const u8) -> i64 {
    let pwd = password.as_kstr();
    match identity::get_table().bootstrap(pwd, "root") {
        Ok(pwm) => pwm as i64,
        Err(e) => i64::from(e.as_i32()),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_fs_capability(pwm: u64) -> u64 {
    engine::get_caps(pwm, CapDomain::FS).as_u64()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_has_capability(pwm: u64, domain: u16, required: u64) -> bool {
    engine::check(pwm, CapDomain(domain), CapBits(required))
}

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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_privilege_level(pwm: u64) -> u8 {
    engine::get_privilege_level(pwm)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_creator(pwm: u64) -> u64 {
    engine::get_creator(pwm)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_grant(grantor_pwm: u64, grantee_pwm: u64, domain: u16, caps: u64) -> i32 {
    match identity::get_table().grant(grantor_pwm, grantee_pwm, CapDomain(domain), CapBits(caps)) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_revoke(revoker_pwm: u64, target_pwm: u64, domain: u16, caps: u64) -> i32 {
    match identity::get_table().revoke(revoker_pwm, target_pwm, CapDomain(domain), CapBits(caps)) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_transfer_creator(current_creator: u64, target: u64, new_creator: u64) -> i32 {
    match identity::get_table().transfer_creator(current_creator, target, new_creator) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_check_privilege(operator: u64, target: u64) -> bool {
    engine::check_privilege(operator, target)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_login(note: *const u8, password: *const u8) -> i64 {
    let n = note.as_kstr();
    let p = password.as_kstr();
    match session::login(n, p) {
        Ok(pwm) => pwm as i64,
        Err(e) => i64::from(e.as_i32()),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_logout() {
    session::logout();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current() -> u64 {
    session::get_current_pwm()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current_entry() -> *const PwmEntry {
    session::get_current_entry()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_is_logged_in() -> bool {
    session::is_logged_in()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current_uid() -> u32 {
    session::get_current_uid()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_current_gid() -> u32 {
    session::get_current_gid()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_euid() -> u32 {
    session::get_euid()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_get_egid() -> u32 {
    session::get_egid()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_elevate_for_suid(target_pwm: u64) -> bool {
    session::elevate_for_suid(target_pwm)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_drop_elevation() -> bool {
    session::drop_elevation()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_has_elevation_authority(target_pwm: u64) -> bool {
    session::has_elevation_authority(target_pwm)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_try_setuid(target_uid: u32) -> bool {
    session::try_setuid(target_uid)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub extern "C" fn pwm_get_uid(pwm: u64) -> u32 {
    identity::find(pwm).map_or(0xFFFFFFFF, PwmEntry::get_uid)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub extern "C" fn pwm_get_gid(pwm: u64) -> u32 {
    identity::find(pwm).map_or(0xFFFFFFFF, PwmEntry::get_gid)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_clear_lockout(pwm: u64) -> i32 {
    match session::clear_lockout(pwm) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_is_modified() -> bool {
    identity::get_table().is_modified()
}

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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
pub extern "C" fn pwm_audit_log(pwm: u64, action: u32, target: u64, details: u64) {
    let act = match action {
        1 => AuditAction::Login,
        2 => AuditAction::Logout,
        3 => AuditAction::Create,
        4 => AuditAction::Delete,
        5 => AuditAction::Modify,
        8 => AuditAction::PasswordChange,
        10 => AuditAction::Grant,
        11 => AuditAction::Revoke,
        12 => AuditAction::TransferCreator,
        13 => AuditAction::FirstTokenGrant,
        _ => AuditAction::Modify,
    };
    audit::log(pwm, act, target, 0, details);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_audit_dump() {
    audit::dump();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pwm_recover_first(password: *const u8, note: *const u8) -> i64 {
    let p = password.as_kstr();
    let n = note.as_kstr();
    match identity::get_table().recover_with_first(p, n) {
        Ok(pwm) => pwm as i64,
        Err(e) => i64::from(e.as_i32()),
    }
}
