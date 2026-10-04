#![deny(unsafe_code)]
//! SGEG 认证策略 — PWM 登录/登出/创建/删除/验证/授权
//!
//! 从 framework/syscall/mod.rs 迁移的策略代码:
//! - auth_login_syscall: PWM 登录
//! - auth_logout_syscall: PWM 登出
//! - auth_create_syscall: PWM 创建身份
//! - auth_delete_syscall: PWM 删除身份
//! - auth_info_syscall: 查询身份信息
//! - auth_changepw_syscall: 修改密码
//! - auth_verify_syscall: 验证密码
//! - auth_create_first_syscall: 创建首个身份
//! - auth_grant_syscall: 授权
//! - auth_revoke_syscall: 撤销授权
//! - auth_check_cap_syscall: 检查能力
//! - auth_get_caps_syscall: 获取能力
//! - capget_syscall: Linux `capget` ABI 映射 (SYSTEM 域 64 位能力 → `cap_data`)
//! - capset_syscall: Linux `capset` ABI 映射 (受约束子集写回, 禁提权)
//! - pwm_get_syscall: 获取当前 PWM
//! - pwm_set_syscall: 设置当前 PWM
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 通过 framework::sgeg 公开 API 访问
//! - 无 unsafe, 无裸指针

use crate::framework::syscall::Errno;

/// `auth_login(password`, note) 策略
pub fn auth_login_syscall(password_ptr: u64, note_ptr: u64) -> i64 {
    crate::framework::sgeg::pwm_login(note_ptr as *const u8, password_ptr as *const u8)
}

/// `auth_logout()` 策略
pub fn auth_logout_syscall() -> i64 {
    crate::framework::sgeg::pwm_logout();
    0
}

/// `auth_create(password`, note, level) 策略
pub fn auth_create_syscall(password_ptr: u64, note_ptr: u64, _level: u8) -> i64 {
    let creator = crate::framework::sgeg::pwm_get_current();
    crate::framework::sgeg::pwm_create(password_ptr as *const u8, note_ptr as *const u8, creator)
}

/// `auth_delete(target)` 策略
pub fn auth_delete_syscall(target: u64) -> i64 {
    i64::from(crate::framework::sgeg::pwm_delete(target))
}

/// `auth_info(target)` 策略
pub fn auth_info_syscall(target: u64) -> i64 {
    i64::from(crate::framework::sgeg::pwm_get_privilege_level(target))
}

/// `auth_changepw(old_pw`, `new_pw`) 策略
pub fn auth_changepw_syscall(old_pw_ptr: u64, new_pw_ptr: u64) -> i64 {
    let pwm = crate::framework::sgeg::pwm_get_current();
    i64::from(crate::framework::sgeg::pwm_change_password(
        pwm,
        old_pw_ptr as *const u8,
        new_pw_ptr as *const u8,
    ))
}

/// `auth_verify(password)` 策略
pub fn auth_verify_syscall(password_ptr: u64) -> i64 {
    let pwm = crate::framework::sgeg::pwm_get_current();
    i64::from(crate::framework::sgeg::pwm_verify_password(
        pwm,
        password_ptr as *const u8,
    ))
}

/// `auth_create_first(password)` 策略
pub fn auth_create_first_syscall(password_ptr: u64) -> i64 {
    if password_ptr == 0 {
        return Errno::EINVAL.as_ret();
    }
    crate::framework::sgeg::pwm_create_first_identity(password_ptr as *const u8)
}

/// `auth_grant(grantor`, grantee, domain, caps) 策略
pub fn auth_grant_syscall(grantor: u64, grantee: u64, domain: u16, caps: u64) -> i64 {
    i64::from(crate::framework::sgeg::pwm_grant(
        grantor, grantee, domain, caps,
    ))
}

/// `auth_revoke(revoker`, target, domain, caps) 策略
pub fn auth_revoke_syscall(revoker: u64, target: u64, domain: u16, caps: u64) -> i64 {
    i64::from(crate::framework::sgeg::pwm_revoke(
        revoker, target, domain, caps,
    ))
}

/// `auth_check_cap(pwm`, domain, required) 策略
pub fn auth_check_cap_syscall(pwm: u64, domain: u16, required: u64) -> i64 {
    i64::from(crate::framework::sgeg::pwm_has_capability(
        pwm, domain, required,
    ))
}

/// `auth_get_caps(pwm`, domain) 策略
pub fn auth_get_caps_syscall(pwm: u64, domain: u16) -> i64 {
    crate::framework::sgeg::pwm_get_capability_raw(pwm, domain) as i64
}

/// Linux `_LINUX_CAPABILITY_VERSION_3` — 每个能力集 64 位 (2 个 `cap_data` 字)
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// Linux `struct __user_cap_header_struct` (8 字节)
#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxCapHeader {
    version: u32,
    pid: i32,
}

/// Linux `struct __user_cap_data_struct` (12 字节)
#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxCapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// 校验并读取 `capget`/`capset` 的公共头部 (`version` + `pid`).
///
/// 返回 `Ok(header)` 或已转换的负值 errno.
///
/// 版本不受支持时按 Linux 语义把受支持版本回写给用户并返回 `EINVAL`.
/// 仅负责版本协商, `pid` 语义由调用方决定 (capget → `ESRCH`, capset → `EPERM`).
fn cap_read_header(header: u64) -> Result<LinuxCapHeader, i64> {
    if header == 0 {
        return Err(Errno::EFAULT.as_ret());
    }
    let mut hdr = LinuxCapHeader { version: 0, pid: 0 };
    if !crate::framework::syscall::api::read_struct_from_user(header, &mut hdr) {
        return Err(Errno::EFAULT.as_ret());
    }
    if hdr.version != LINUX_CAPABILITY_VERSION_3 {
        let fixed = LinuxCapHeader {
            version: LINUX_CAPABILITY_VERSION_3,
            pid: hdr.pid,
        };
        let _ = crate::framework::syscall::api::write_struct_to_user(header, &fixed);
        return Err(Errno::EINVAL.as_ret());
    }
    Ok(hdr)
}

/// `pid` 字段是否指向当前进程 (Linux 中 `0` 表示自身).
fn cap_pid_is_self(pid: i32) -> bool {
    pid == 0 || pid as u32 == crate::framework::proc::process_get_current_pid()
}

/// `capget(header, data)` 策略 — Edgine SYSTEM 域能力导出为 Linux cap ABI
///
/// SIMPLIFIED: 仅支持 `_LINUX_CAPABILITY_VERSION_3` 且仅映射 SYSTEM 域
/// (sgeg 无 Linux 细粒度 cap 划分, permitted/effective 同值, inheritable 恒 0);
/// 影响面: v1/v2 版本请求返回 `EINVAL`, 其余域能力不经此 ABI 暴露;
/// 何时需扩展: 用户态出现按 Linux cap 位逐个协商的需求时补域/位映射表.
///
/// `header.pid == 0` 或等于当前进程 pid; `data == 0` 时仅做版本协商 (返回 0).
///
/// # Errors
///
/// - 用户指针非法 → `EFAULT`
/// - 版本不受支持 → `EINVAL` (并回写受支持版本)
/// - `pid` 指向他进程 → `ESRCH`
pub fn capget_syscall(header: u64, data: u64) -> i64 {
    let hdr = match cap_read_header(header) {
        Ok(h) => h,
        Err(ret) => return ret,
    };
    if !cap_pid_is_self(hdr.pid) {
        return Errno::ESRCH.as_ret();
    }
    if data == 0 {
        return 0;
    }

    let pwm = crate::framework::sgeg::pwm_get_current();
    let caps = crate::framework::sgeg::pwm_get_capability_raw(
        pwm,
        crate::framework::sgeg::CAP_DOMAIN_SYSTEM,
    );
    let lo = LinuxCapData {
        effective: caps as u32,
        permitted: caps as u32,
        inheritable: 0,
    };
    let hi = LinuxCapData {
        effective: (caps >> 32) as u32,
        permitted: (caps >> 32) as u32,
        inheritable: 0,
    };

    let Some(data1) = data.checked_add(core::mem::size_of::<LinuxCapData>() as u64) else {
        return Errno::EFAULT.as_ret();
    };
    if !crate::framework::syscall::api::write_struct_to_user(data, &lo)
        || !crate::framework::syscall::api::write_struct_to_user(data1, &hi)
    {
        return Errno::EFAULT.as_ret();
    }
    0
}

/// `capset(header, data)` 策略 — Linux cap ABI 写回 Edgine SYSTEM 域能力
///
/// 取 Linux `permitted & effective` 作为请求集合, 交由 framework 受约束 setter
/// 落地 (仅允许**当前值子集**且不得破坏域能力下限), 否则 `EPERM`.
/// `inheritable` 被忽略 (sgeg 无继承能力概念).
///
/// SIMPLIFIED: 仅支持 `_LINUX_CAPABILITY_VERSION_3` 且仅作用于 SYSTEM 域;
/// 影响面: 无法通过此 ABI 授予新能力 (需走 `auth_grant_syscall`), 其余域不可写;
/// 何时需扩展: 用户态需要按 Linux cap 位粒度提权时改为经 `grant` 路径校验.
///
/// # Errors
///
/// - 用户指针非法 → `EFAULT`
/// - 版本不受支持 → `EINVAL` (并回写受支持版本)
/// - `pid` 指向他进程 → `EPERM` (禁止跨进程能力篡改)
/// - 请求集合超出当前值或破坏下限 → `EPERM`
pub fn capset_syscall(header: u64, data: u64) -> i64 {
    let hdr = match cap_read_header(header) {
        Ok(h) => h,
        Err(ret) => return ret,
    };
    if !cap_pid_is_self(hdr.pid) {
        return Errno::EPERM.as_ret();
    }
    if data == 0 {
        return Errno::EINVAL.as_ret();
    }

    let mut lo = LinuxCapData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    };
    let mut hi = LinuxCapData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    };
    let Some(data1) = data.checked_add(core::mem::size_of::<LinuxCapData>() as u64) else {
        return Errno::EFAULT.as_ret();
    };
    if !crate::framework::syscall::api::read_struct_from_user(data, &mut lo)
        || !crate::framework::syscall::api::read_struct_from_user(data1, &mut hi)
    {
        return Errno::EFAULT.as_ret();
    }

    let permitted = u64::from(lo.permitted) | (u64::from(hi.permitted) << 32);
    let effective = u64::from(lo.effective) | (u64::from(hi.effective) << 32);
    let requested = permitted & effective;

    let ret = crate::framework::sgeg::pwm_set_current_capability_raw(
        crate::framework::sgeg::CAP_DOMAIN_SYSTEM,
        requested,
    );
    if ret < 0 {
        return Errno::EPERM.as_ret();
    }
    0
}

/// `pwm_get()` 策略
pub fn pwm_get_syscall() -> i64 {
    crate::framework::sgeg::pwm_get_current() as i64
}

/// `pwm_set(pwm)` 策略
///
/// B07-05 (DECISION-078): 设置进程 PWM 属身份安全关键操作, 调用方必须持有
/// SYSTEM 域 `SET_PWM` 能力位 (PWM 0/bootstrap 经 `engine::check` 恒全权).
/// 原实现无任何校验, 任意进程可把自身 PWM 改为 root, 绕过全部 UID/GID 检查
/// (任意提权漏洞 P0-07).
pub fn pwm_set_syscall(pwm: u64) -> i64 {
    use crate::services::sgeg::capability::{CAP_DOMAIN_SYSTEM, SYSTEM_CAP_SET_PWM};
    let current = crate::framework::sgeg::pwm_get_current();
    if !crate::framework::sgeg::pwm_has_capability(current, CAP_DOMAIN_SYSTEM, SYSTEM_CAP_SET_PWM) {
        return Errno::EPERM.as_ret();
    }
    let pid = crate::framework::proc::process_get_current_pid();
    i64::from(crate::framework::proc::proc_set_pwm(pid, pwm))
}
