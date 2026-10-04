// ============================================================================
// P2-I-30: 凭证会话上下文改为 per-process
// ============================================================================
//
// 改造前: `static GLOBAL_SESSION: Mutex<SessionManager>` 是 UnsafeCell 全局单例,
//         SMP 下所有 CPU 共享同一会话上下文, 进程切换时存在身份/权限串台风险.
//
// 改造后: PwmContext (uid/gid/euid/egid/saved_*/domain/elevation) 与 SUID 提权栈
//         均绑定到 `Process` 结构体 (framework::proc::process::Process::session /
//         session_elev_stack / session_elev_depth). 进程退出时随 Process 回收.
//         公开 API 签名不变, 内部实现改为: 取 current pid → PROCESS_TABLE 查
//         Process → 访问该进程的 session 字段.
//
// 线程安全: 每次公开调用都先取 current pid 再访问 Process, 锁粒度与原版一致
//           (PwmContext 一把 spinlock, 提权栈一把 spinlock). 公开调用本身不持
//           PROCESS_TABLE 锁超过单条闭包, 不会与其它子系统争用.
//
// SAFETY: 所有 UnsafeCell 已删除. 不再有 `unsafe impl Send/Sync for SessionManager`.
//         Process 本身已通过 `unsafe impl Send/Sync` 标注 (process.rs 中),
//         `Mutex<...>` 字段提供内部可变性.
use super::identity;
use super::types::{AuditAction, DomainId, PwmContext, PwmEntry, PwmError, PwmFlags, PwmId};
use crate::framework::proc::PROCESS_TABLE;
use crate::framework::proc::process_get_current_pid;
use core::sync::atomic::Ordering;

const MAX_ELEVATION_DEPTH: isize = 8;
const MAX_LOGIN_ATTEMPTS: u32 = 5;
const LOCKOUT_DURATION_SECS: u64 = 300;

// 进程级 SUID 提权栈容量, 与 MAX_ELEVATION_DEPTH 一致.
// 必须为 const, 用于 Process::new 中 `[T; N]` 数组初始化.
pub const SESSION_ELEV_STACK_CAP: usize = 8;

// ============================================================================
// 内部辅助: 在当前进程上执行 PwmContext 闭包
// ============================================================================

/// 取出当前 pid 对应 Process 的 `PwmContext`, 在其上执行 f.
#[inline]
fn with_current_ctx<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut PwmContext) -> R,
{
    let pid = process_get_current_pid();
    if pid == 0 {
        return None;
    }
    PROCESS_TABLE.with_process(pid, |p| {
        let mut guard = p.session.lock();
        f(&mut guard)
    })
}

/// 取出当前 pid 对应 Process 的 `PwmContext`, 复制返回.
#[inline]
fn read_current_ctx() -> Option<PwmContext> {
    with_current_ctx(|ctx| *ctx)
}

/// 当前 pid 不存在 (启动早期 / 调度器空) 时调用, 写操作返回默认值静默失败.
#[inline]
fn try_with_current_ctx<F, R>(default: R, f: F) -> R
where
    F: FnOnce(&mut PwmContext) -> R,
{
    with_current_ctx(f).unwrap_or(default)
}

// ============================================================================
// 登录 / 登出
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 使用 note 与密码执行登录, 成功后把会话信息写入当前进程的 `PwmContext`。
/// # Errors
/// 当前进程不存在、note 对应的 PWM 不存在、身份被禁用、处于锁定期或密码错误时返回 Err。
pub fn login(note: &str, password: &str) -> Result<u64, PwmError> {
    // 提前取 current pid, 因 PwmId 等类型不是 Send, 不能跨闭包捕获.
    let pid = process_get_current_pid();
    if pid == 0 {
        return Err(PwmError::NotFound);
    }

    let t = identity::get_table();
    let entry = match t.find_by_note(note) {
        Some(e) => e,
        None => return Err(PwmError::NotFound),
    };

    if entry.has_flag(PwmFlags::DISABLED) {
        return Err(PwmError::Disabled);
    }

    let now = super::bootstrap::pwm_now();
    let lockout = entry.lockout_until.load(Ordering::Acquire);
    if lockout > 0 && now < lockout {
        return Err(PwmError::Disabled);
    }

    let pwm = entry.pwm.load(Ordering::Acquire);

    if !t.verify_password(pwm, password) {
        let attempts = entry.failed_attempts.fetch_add(1, Ordering::AcqRel) + 1;
        if attempts >= MAX_LOGIN_ATTEMPTS {
            entry
                .lockout_until
                .store(now + LOCKOUT_DURATION_SECS * 1_000_000, Ordering::Release);
            entry.add_flags(PwmFlags::LOCKED);
        }
        return Err(PwmError::PasswordIncorrect);
    }

    entry.failed_attempts.store(0, Ordering::Release);
    entry.remove_flags(PwmFlags::LOCKED);
    entry.last_login_time.store(now, Ordering::Release);

    let uid = entry.get_uid();
    let gid = entry.get_gid();

    // 将 login 结果写入当前进程的 PwmContext.
    let wrote = PROCESS_TABLE.with_process(pid, |p| {
        let mut ctx = p.session.lock();
        ctx.current_entry = entry;
        ctx.session_pwm = PwmId(pwm);
        ctx.cached_uid = uid;
        ctx.cached_gid = gid;
        ctx.euid = uid;
        ctx.egid = gid;
        ctx.saved_euid = uid;
        ctx.saved_egid = gid;
        ctx.active_domain_id = DomainId::from_uid(uid);
        ctx.elevation_granted_pwm = PwmId::ZERO;
        // 新登录重置 SUID 提权栈, 避免跨会话残留.
        p.session_elev_depth.store(0, Ordering::Release);
        true
    });
    if wrote.is_none() {
        return Err(PwmError::NotFound);
    }

    super::audit::log(pwm, AuditAction::Login, pwm, 0, 0);

    Ok(pwm)
}

pub fn logout() {
    let pid = process_get_current_pid();
    if pid == 0 {
        return;
    }
    let pwm = PROCESS_TABLE
        .with_process(pid, |p| {
            let mut ctx = p.session.lock();
            let saved = ctx.session_pwm.as_u64();
            ctx.current_entry = core::ptr::null();
            ctx.session_pwm = PwmId::ZERO;
            ctx.cached_uid = 0;
            ctx.cached_gid = 0;
            ctx.euid = 0;
            ctx.egid = 0;
            ctx.saved_euid = 0;
            ctx.saved_egid = 0;
            ctx.active_domain_id = DomainId::ZERO;
            ctx.elevation_granted_pwm = PwmId::ZERO;
            p.session_elev_depth.store(0, Ordering::Release);
            saved
        })
        .unwrap_or(0);
    super::audit::log(pwm, AuditAction::Logout, pwm, 0, 0);
}

// ============================================================================
// 只读访问器
// ============================================================================

pub fn get_current_pwm() -> u64 {
    with_current_ctx(|ctx| ctx.session_pwm.as_u64()).unwrap_or(0)
}

pub fn get_current_entry() -> *const PwmEntry {
    with_current_ctx(|ctx| ctx.current_entry).unwrap_or(core::ptr::null())
}

pub fn get_current_uid() -> u32 {
    with_current_ctx(|ctx| ctx.cached_uid).unwrap_or(0)
}

pub fn get_current_gid() -> u32 {
    with_current_ctx(|ctx| ctx.cached_gid).unwrap_or(0)
}

pub fn get_euid() -> u32 {
    with_current_ctx(|ctx| ctx.euid).unwrap_or(0)
}

pub fn get_egid() -> u32 {
    with_current_ctx(|ctx| ctx.egid).unwrap_or(0)
}

pub fn get_saved_euid() -> u32 {
    with_current_ctx(|ctx| ctx.saved_euid).unwrap_or(0)
}

pub fn get_saved_egid() -> u32 {
    with_current_ctx(|ctx| ctx.saved_egid).unwrap_or(0)
}

pub fn get_current_domain_id() -> u64 {
    with_current_ctx(|ctx| ctx.active_domain_id.as_u64()).unwrap_or(0)
}

pub fn is_logged_in() -> bool {
    get_current_pwm() != 0
}

/// 清除指定 PWM 的登录锁定状态 (锁定截止时间与失败计数)。
/// # Errors
/// 指定 PWM 不存在时返回 Err。
pub fn clear_lockout(pwm: u64) -> Result<(), PwmError> {
    let entry = identity::find(pwm).ok_or(PwmError::NotFound)?;
    entry.lockout_until.store(0, Ordering::Release);
    entry.failed_attempts.store(0, Ordering::Release);
    entry.remove_flags(PwmFlags::LOCKED);
    Ok(())
}

// ============================================================================
// SUID 提权 (per-process 栈)
// ============================================================================

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn elevate_for_suid(target_pwm: u64) -> bool {
    let pid = process_get_current_pid();
    if pid == 0 {
        return false;
    }
    let target_entry = match identity::find(target_pwm) {
        Some(e) => e,
        None => return false,
    };
    let target_uid = target_entry.get_uid();
    let target_gid = target_entry.get_gid();

    let result = PROCESS_TABLE.with_process(pid, |p| {
        let depth = p.session_elev_depth.load(Ordering::Acquire);
        if depth >= MAX_ELEVATION_DEPTH {
            return (false, 0u64);
        }
        let session_pwm: u64;
        let snapshot = {
            let mut ctx = p.session.lock();
            session_pwm = ctx.session_pwm.as_u64();
            ctx.euid = target_uid;
            ctx.egid = target_gid;
            ctx.saved_euid = target_uid;
            ctx.saved_egid = target_gid;
            ctx.active_domain_id = DomainId::from_uid(target_uid);
            ctx.elevation_granted_pwm = PwmId(target_pwm);
            *ctx
        };
        // 推入提权栈.
        {
            let mut stack = p.session_elev_stack.lock();
            stack[depth as usize] = snapshot;
        }
        p.session_elev_depth.store(depth + 1, Ordering::Release);
        (true, session_pwm)
    });
    match result {
        Some((ok, session_pwm)) => {
            if ok {
                super::audit::log(session_pwm, AuditAction::Grant, target_pwm, 0, 1);
            }
            ok
        }
        None => false,
    }
}

pub fn drop_elevation() -> bool {
    let pid = process_get_current_pid();
    if pid == 0 {
        return false;
    }
    PROCESS_TABLE
        .with_process(pid, |p| {
            let depth = p.session_elev_depth.load(Ordering::Acquire);
            if depth == 0 {
                return false;
            }
            let saved = {
                let stack = p.session_elev_stack.lock();
                stack[(depth - 1) as usize]
            };
            {
                let mut ctx = p.session.lock();
                *ctx = saved;
            }
            p.session_elev_depth.store(depth - 1, Ordering::Release);
            true
        })
        .unwrap_or(false)
}

pub fn has_elevation_authority(target_pwm: u64) -> bool {
    with_current_ctx(|ctx| ctx.elevation_granted_pwm == PwmId(target_pwm)).unwrap_or(false)
}

// ============================================================================
// POSIX setuid / setgid / setreuid / setregid 系列调用
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn try_setuid(target_uid: u32) -> bool {
    let table = identity::get_table();
    let target_entry = match table.find_by_uid(target_uid) {
        Some(e) => e,
        None => return false,
    };
    let target_pwm = target_entry.get_pwm().0;

    let current_pwm = read_current_ctx().map_or(0, |c| c.session_pwm.as_u64());

    if super::engine::check_privilege(target_pwm, current_pwm)
        || has_elevation_authority(target_pwm)
    {
        elevate_for_suid(target_pwm)
    } else {
        false
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn try_setgid(target_gid: u32) -> bool {
    let egid = get_egid();
    if target_gid == egid {
        return true;
    }

    let table = identity::get_table();
    let target_entry = match table.find_by_gid(target_gid) {
        Some(e) => e,
        None => return false,
    };
    let target_pwm = target_entry.get_pwm().0;

    let (current_pwm, cached_gid, saved_egid) = match read_current_ctx() {
        Some(c) => (c.session_pwm.as_u64(), c.cached_gid, c.saved_egid),
        None => return false,
    };

    if super::engine::check_privilege(target_pwm, current_pwm) {
        try_with_current_ctx(false, |ctx| {
            ctx.egid = target_gid;
            ctx.saved_egid = target_gid;
            true
        })
    } else if target_gid == cached_gid || target_gid == saved_egid {
        try_with_current_ctx(false, |ctx| {
            ctx.egid = target_gid;
            true
        })
    } else {
        false
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn try_seteuid(target_euid: u32) -> bool {
    let euid = get_euid();
    if target_euid == euid {
        return true;
    }

    let table = identity::get_table();
    let target_entry = match table.find_by_uid(target_euid) {
        Some(e) => e,
        None => return false,
    };
    let target_pwm = target_entry.get_pwm().0;

    let (current_pwm, cached_uid, saved_euid) = match read_current_ctx() {
        Some(c) => (c.session_pwm.as_u64(), c.cached_uid, c.saved_euid),
        None => return false,
    };

    if super::engine::check_privilege(target_pwm, current_pwm) {
        try_with_current_ctx(false, |ctx| {
            ctx.euid = target_euid;
            ctx.saved_euid = target_euid;
            true
        })
    } else if target_euid == cached_uid || target_euid == saved_euid {
        try_with_current_ctx(false, |ctx| {
            ctx.euid = target_euid;
            true
        })
    } else {
        false
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn try_setegid(target_egid: u32) -> bool {
    let egid = get_egid();
    if target_egid == egid {
        return true;
    }

    let table = identity::get_table();
    let target_entry = match table.find_by_gid(target_egid) {
        Some(e) => e,
        None => return false,
    };
    let target_pwm = target_entry.get_pwm().0;

    let (current_pwm, cached_gid, saved_egid) = match read_current_ctx() {
        Some(c) => (c.session_pwm.as_u64(), c.cached_gid, c.saved_egid),
        None => return false,
    };

    if super::engine::check_privilege(target_pwm, current_pwm) {
        try_with_current_ctx(false, |ctx| {
            ctx.egid = target_egid;
            ctx.saved_egid = target_egid;
            true
        })
    } else if target_egid == cached_gid || target_egid == saved_egid {
        try_with_current_ctx(false, |ctx| {
            ctx.egid = target_egid;
            true
        })
    } else {
        false
    }
}

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
pub fn try_setreuid(target_ruid: u32, target_euid: u32) -> bool {
    #[inline]
    fn has_uid_privilege(table: &identity::IdentityTable, uid: u32, current_pwm: u64) -> bool {
        table.find_by_uid(uid).map_or(false, |entry| {
            super::engine::check_privilege(entry.get_pwm().0, current_pwm)
        })
    }

    let (current_pwm, old_cached_uid, old_euid, old_saved_euid) = match read_current_ctx() {
        Some(c) => (c.session_pwm.as_u64(), c.cached_uid, c.euid, c.saved_euid),
        None => return false,
    };

    let ruid_is_set = target_ruid != u32::MAX;
    let euid_is_set = target_euid != u32::MAX;

    let new_ruid = if ruid_is_set {
        target_ruid
    } else {
        old_cached_uid
    };
    let new_euid = if euid_is_set { target_euid } else { old_euid };

    if new_ruid == old_cached_uid && new_euid == old_euid {
        return true;
    }

    let table = identity::get_table();

    if ruid_is_set && new_ruid != old_cached_uid {
        let ok = has_uid_privilege(table, new_ruid, current_pwm) || new_ruid == old_euid;
        if !ok {
            return false;
        }
    }

    if euid_is_set && new_euid != old_euid {
        let ok = has_uid_privilege(table, new_euid, current_pwm)
            || new_euid == old_cached_uid
            || new_euid == old_saved_euid;
        if !ok {
            return false;
        }
    }

    let saved_euid_should_update = ruid_is_set || (euid_is_set && new_euid != old_cached_uid);
    try_with_current_ctx(false, |ctx| {
        ctx.cached_uid = new_ruid;
        ctx.euid = new_euid;
        if saved_euid_should_update {
            ctx.saved_euid = new_euid;
        }
        true
    })
}

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
pub fn try_setregid(target_rgid: u32, target_egid: u32) -> bool {
    #[inline]
    fn has_gid_privilege(table: &identity::IdentityTable, gid: u32, current_pwm: u64) -> bool {
        table.find_by_gid(gid).map_or(false, |entry| {
            super::engine::check_privilege(entry.get_pwm().0, current_pwm)
        })
    }

    let (current_pwm, old_cached_gid, old_egid, old_saved_egid) = match read_current_ctx() {
        Some(c) => (c.session_pwm.as_u64(), c.cached_gid, c.egid, c.saved_egid),
        None => return false,
    };

    let rgid_is_set = target_rgid != u32::MAX;
    let egid_is_set = target_egid != u32::MAX;

    let new_rgid = if rgid_is_set {
        target_rgid
    } else {
        old_cached_gid
    };
    let new_egid = if egid_is_set { target_egid } else { old_egid };

    if new_rgid == old_cached_gid && new_egid == old_egid {
        return true;
    }

    let table = identity::get_table();

    if rgid_is_set && new_rgid != old_cached_gid {
        let ok = has_gid_privilege(table, new_rgid, current_pwm) || new_rgid == old_egid;
        if !ok {
            return false;
        }
    }

    if egid_is_set && new_egid != old_egid {
        let ok = has_gid_privilege(table, new_egid, current_pwm)
            || new_egid == old_cached_gid
            || new_egid == old_saved_egid;
        if !ok {
            return false;
        }
    }

    let saved_egid_should_update = rgid_is_set || (egid_is_set && new_egid != old_cached_gid);
    try_with_current_ctx(false, |ctx| {
        ctx.cached_gid = new_rgid;
        ctx.egid = new_egid;
        if saved_egid_should_update {
            ctx.saved_egid = new_egid;
        }
        true
    })
}
