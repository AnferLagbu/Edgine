#![deny(unsafe_code)]
//! 会话生命周期 (functions 层)
//!
//! ## 框内核中的表达
//!
//! 会话 (Session) 是**已认证的活跃登录状态**, 与 PWM (持久身份) 分离.
//!
//! ```text
//! Login:
//!   password.verify(privileged::sgeg::password)
//!     ↓
//!   policy.check(privileged::sgeg::matrix)
//!     ↓
//!   Session::new(pwm, cap_snapshot, expiry)  ← 本文件
//!     ↓
//!   写入 SessionTable
//!
//! Logout:
//!   Session::end()
//!     ↓
//!   撤销临时 capability grant
//!     ↓
//!   审计 (functions::sgeg::audit)
//! ```
//!
//! ## @SAFE
//! 本文件不含 `unsafe`.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::policy::{CAP_DOMAINS, CapBits, CapDomain, PolicyEngine, PolicyResult};

/// 最大并发会话数 (B07-16: 改名消除与 `config::MAX_SESSIONS`(16, POSIX
/// 进程会话表) 的同名冲突 — 认证会话 vs 进程会话是不同职责)
pub const SGEG_MAX_SESSIONS: usize = 64;

/// 会话状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 活跃
    Active,
    /// 已过期
    Expired,
    /// 已显式登出
    LoggedOut,
    /// 被管理员强制终止
    Killed,
    /// 因异常被隔离
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub pwm: u64,
    /// 创建 tick
    pub created_tick: u64,
    /// 最后活跃 tick
    pub last_active_tick: u64,
    /// 过期 tick (0 = 永不过期)
    pub expires_tick: u64,
    /// 当前能力快照 (16 域)
    pub caps: [CapBits; CAP_DOMAINS],
    /// 状态
    pub state: SessionState,
    /// 失败登录次数
    pub failed_attempts: u32,
    /// 会话关联进程 PID (0 = 内核态)
    pub pid: u32,
}

/// 会话表
pub struct SessionTable {
    records: [Option<Session>; SGEG_MAX_SESSIONS],
    next_id: AtomicU32,
    active: AtomicU64,
    next_slot: AtomicU32,
}

impl SessionTable {
    pub const fn new() -> Self {
        const NONE: Option<Session> = None;
        const EMPTY_CAPS: [CapBits; CAP_DOMAINS] = [CapBits(0); CAP_DOMAINS];
        let _ = EMPTY_CAPS;
        Self {
            records: [NONE; SGEG_MAX_SESSIONS],
            next_id: AtomicU32::new(1),
            active: AtomicU64::new(0),
            next_slot: AtomicU32::new(0),
        }
    }

    /// 创建新会话
    ///
    /// # Errors
    /// 当同一 PWM 的并发活动会话数达到上限 (8) 时返回
    /// `Err(SessionError::Kernel(KernelError::WouldBlock))`;
    /// 当会话表已满 (无空闲槽位) 时返回 `Err(SessionError::TableFull)`.
    pub fn create(
        &mut self,
        pwm: u64,
        caps: [CapBits; CAP_DOMAINS],
        current_tick: u64,
        expires_tick: u64,
        pid: u32,
    ) -> Result<SessionId, SessionError> {
        // 限额: 同一 PWM 最多 N 个并发会话
        let per_pwm: u32 = 8;
        let mut count: u32 = 0;
        for slot in &self.records {
            if let Some(s) = slot {
                if s.pwm == pwm && matches!(s.state, SessionState::Active) {
                    count += 1;
                    if count >= per_pwm {
                        return Err(SessionError::Kernel(
                            crate::functions::error::KernelError::WouldBlock,
                        ));
                    }
                }
            }
        }
        // 查找空槽
        let start = self.next_slot.load(Ordering::Acquire) as usize;
        for i in 0..SGEG_MAX_SESSIONS {
            let idx = (start + i) % SGEG_MAX_SESSIONS;
            if self.records[idx].is_none() {
                let id = SessionId(self.next_id.fetch_add(1, Ordering::AcqRel));
                self.records[idx] = Some(Session {
                    id,
                    pwm,
                    created_tick: current_tick,
                    last_active_tick: current_tick,
                    expires_tick,
                    caps,
                    state: SessionState::Active,
                    failed_attempts: 0,
                    pid,
                });
                self.next_slot
                    .store(((idx + 1) % SGEG_MAX_SESSIONS) as u32, Ordering::Release);
                self.active.fetch_add(1, Ordering::AcqRel);
                return Ok(id);
            }
        }
        Err(SessionError::TableFull)
    }

    /// 查找会话
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        for s in &self.records {
            if let Some(s) = s {
                if s.id == id {
                    return Some(s);
                }
            }
        }
        None
    }

    /// 标记活跃 (heartbeat)
    ///
    /// # Errors
    /// 当会话不存在时返回 `Err(SessionError::Kernel(KernelError::FileNotFound))`;
    /// 当会话不处于活跃状态 (如已结束) 时返回 `Err(SessionError::NotActive)`.
    pub fn heartbeat(&mut self, id: SessionId, current_tick: u64) -> Result<(), SessionError> {
        for slot in &mut self.records {
            if let Some(s) = slot {
                if s.id == id {
                    if !matches!(s.state, SessionState::Active) {
                        return Err(SessionError::NotActive);
                    }
                    s.last_active_tick = current_tick;
                    return Ok(());
                }
            }
        }
        Err(SessionError::Kernel(
            crate::functions::error::KernelError::FileNotFound,
        ))
    }

    /// 结束会话
    ///
    /// # Errors
    /// 当会话不存在时返回 `Err(SessionError::Kernel(KernelError::FileNotFound))`.
    pub fn end(&mut self, id: SessionId, target: SessionState) -> Result<Session, SessionError> {
        for slot in &mut self.records {
            if let Some(s) = slot {
                if s.id == id {
                    let prev = *s;
                    s.state = target;
                    self.active.fetch_sub(1, Ordering::AcqRel);
                    *slot = None; // 释放槽
                    return Ok(prev);
                }
            }
        }
        Err(SessionError::Kernel(
            crate::functions::error::KernelError::FileNotFound,
        ))
    }

    /// 强制结束某 PWM 的所有会话
    pub fn end_all_for(&mut self, pwm: u64, target: SessionState) -> usize {
        let mut count = 0;
        for slot in &mut self.records {
            if let Some(s) = slot {
                if s.pwm == pwm {
                    s.state = target;
                    *slot = None;
                    count += 1;
                }
            }
        }
        if count > 0 {
            self.active.fetch_sub(count as u64, Ordering::AcqRel);
        }
        count
    }

    /// 清理过期会话
    pub fn gc_expired(&mut self, current_tick: u64) -> usize {
        let mut count = 0;
        for slot in &mut self.records {
            if let Some(s) = slot {
                if s.expires_tick != 0 && current_tick >= s.expires_tick {
                    s.state = SessionState::Expired;
                    *slot = None;
                    count += 1;
                }
            }
        }
        if count > 0 {
            self.active.fetch_sub(count as u64, Ordering::AcqRel);
        }
        count
    }

    pub fn active_count(&self) -> u64 {
        self.active.load(Ordering::Acquire)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.records.iter().filter_map(|s| s.as_ref())
    }
}

/// Session 错误 — TD-20: 收敛到 `KernelError`, 2 字段 session 特有 + 1 共享包装.
///
/// 字段说明:
///   - `TableFull`: Session 表已满 (POSIX EAGAIN, 但语义特化)
///   - `NotActive`: Session 状态非激活 (POSIX EINVAL, 但语义特化)
///   - `Kernel(KernelError)`: 共享错误 (`NotFound` / TooManySessions→WouldBlock / Other) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    TableFull,
    NotActive,
    /// 共享 `KernelError` 包装
    Kernel(crate::functions::error::KernelError),
}

impl SessionError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::TableFull => E::EAGAIN,
            Self::NotActive => E::EINVAL,
            Self::Kernel(e) => e.as_errno(),
        }
    }
}

use crate::privileged::syscall::Errno;

/// 登录认证结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginResult {
    Success(SessionId),
    Denied(LoginDeny),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginDeny {
    InvalidCredentials,
    AccountLocked,
    CapabilityDenied(PolicyResult),
    TooManySessions,
}

/// 登录管理器
pub struct SessionManager<'a> {
    pub table: &'a mut SessionTable,
    pub policy: &'a PolicyEngine,
    pub max_failed_attempts: u32,
}

impl<'a> SessionManager<'a> {
    pub fn new(table: &'a mut SessionTable, policy: &'a PolicyEngine) -> Self {
        Self {
            table,
            policy,
            max_failed_attempts: 5,
        }
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    #[expect(
        clippy::match_wildcard_for_single_variants,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    /// 登录流程 (functions 层抽象, 实际密码验证由 `privileged::sgeg::password` 提供)
    pub fn login(
        &mut self,
        pwm: u64,
        password_ok: bool, // 来自 privileged::sgeg::password
        required: CapBits,
        domain: CapDomain,
        current_tick: u64,
        expires_tick: u64,
        pid: u32,
        matrix_caps: [CapBits; CAP_DOMAINS],
    ) -> LoginResult {
        if !password_ok {
            return LoginResult::Denied(LoginDeny::InvalidCredentials);
        }
        // 失败计数: 简化处理 (实际应有 PWM 关联的失败计数表)
        // 检查策略
        let matrix = InMemoryCaps(matrix_caps);
        match self.policy.check(&matrix, domain, required) {
            PolicyResult::Allow => {
                let id = match self
                    .table
                    .create(pwm, matrix_caps, current_tick, expires_tick, pid)
                {
                    Ok(id) => id,
                    Err(SessionError::Kernel(crate::functions::error::KernelError::WouldBlock)) => {
                        return LoginResult::Denied(LoginDeny::TooManySessions);
                    }
                    Err(SessionError::TableFull) => {
                        return LoginResult::Denied(LoginDeny::TooManySessions);
                    }
                    Err(_) => return LoginResult::Denied(LoginDeny::AccountLocked),
                };
                LoginResult::Success(id)
            }
            other => LoginResult::Denied(LoginDeny::CapabilityDenied(other)),
        }
    }
}

/// 内部: 复用 `InMemoryMatrix` 模式但传入 [`CapBits`; 16]
struct InMemoryCaps([CapBits; CAP_DOMAINS]);

impl super::policy::CapabilityMatrix for InMemoryCaps {
    fn get(&self, domain: CapDomain) -> Option<CapBits> {
        if !domain.is_valid() {
            return None;
        }
        Some(self.0[domain.0 as usize])
    }
    fn set(&self, _domain: CapDomain, _bits: CapBits) -> Result<CapBits, ()> {
        Err(()) // 只读
    }
    fn compare_exchange(
        &self,
        domain: CapDomain,
        _current: CapBits,
        _new: CapBits,
    ) -> Result<CapBits, CapBits> {
        Err(self.0[domain.0 as usize]) // 只读
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // get/set 是 CapabilityMatrix 的 trait 方法, 需显式引入 trait 才能调用.
    use crate::functions::sgeg::policy::CapabilityMatrix;

    fn default_caps() -> [CapBits; CAP_DOMAINS] {
        [CapBits(0); CAP_DOMAINS]
    }

    #[test]
    fn session_create_and_get() {
        let mut t = SessionTable::new();
        let id = t.create(1, default_caps(), 100, 0, 0).unwrap();
        let s = t.get(id).unwrap();
        assert_eq!(s.pwm, 1);
        assert_eq!(s.state, SessionState::Active);
    }

    #[test]
    fn session_end_marks_inactive() {
        let mut t = SessionTable::new();
        let id = t.create(1, default_caps(), 100, 0, 0).unwrap();
        assert_eq!(t.active_count(), 1);
        let _ = t.end(id, SessionState::LoggedOut).unwrap();
        assert_eq!(t.active_count(), 0);
        assert!(t.get(id).is_none());
    }

    #[test]
    fn session_heartbeat() {
        let mut t = SessionTable::new();
        let id = t.create(1, default_caps(), 100, 200, 0).unwrap();
        assert!(t.heartbeat(id, 150).is_ok());
        let s = t.get(id).unwrap();
        assert_eq!(s.last_active_tick, 150);
    }

    #[test]
    fn session_gc_expired() {
        let mut t = SessionTable::new();
        let id1 = t.create(1, default_caps(), 100, 200, 0).unwrap();
        let _id2 = t.create(2, default_caps(), 100, 500, 0).unwrap();
        assert_eq!(t.active_count(), 2);
        let n = t.gc_expired(300);
        assert_eq!(n, 1);
        assert_eq!(t.active_count(), 1);
        assert!(t.get(id1).is_none());
    }

    #[test]
    fn session_end_all_for() {
        let mut t = SessionTable::new();
        for _ in 0..3 {
            t.create(1, default_caps(), 100, 0, 0).unwrap();
        }
        t.create(2, default_caps(), 100, 0, 0).unwrap();
        assert_eq!(t.active_count(), 4);
        let killed = t.end_all_for(1, SessionState::Killed);
        assert_eq!(killed, 3);
        assert_eq!(t.active_count(), 1);
    }

    #[test]
    fn session_too_many() {
        let mut t = SessionTable::new();
        for _ in 0..8 {
            t.create(1, default_caps(), 100, 0, 0).unwrap();
        }
        // 第 9 个应被拒
        let r = t.create(1, default_caps(), 100, 0, 0);
        assert_eq!(
            r,
            Err(SessionError::Kernel(
                crate::functions::error::KernelError::WouldBlock
            ))
        );
    }

    #[test]
    fn session_table_full() {
        let mut t = SessionTable::new();
        for i in 0..SGEG_MAX_SESSIONS {
            t.create(i as u64, default_caps(), 100, 0, 0).unwrap();
        }
        let r = t.create(999, default_caps(), 100, 0, 0);
        assert_eq!(r, Err(SessionError::TableFull));
    }

    #[test]
    fn login_invalid_creds() {
        let mut t = SessionTable::new();
        let p = PolicyEngine::new();
        let mut mgr = SessionManager::new(&mut t, &p);
        let caps = default_caps();
        let r = mgr.login(1, false, CapBits(0), CapDomain::FS, 100, 0, 0, caps);
        assert_eq!(r, LoginResult::Denied(LoginDeny::InvalidCredentials));
    }

    #[test]
    fn login_capability_denied() {
        let mut t = SessionTable::new();
        let p = PolicyEngine::new();
        let mut mgr = SessionManager::new(&mut t, &p);
        let mut caps = default_caps();
        // 不给 FS 任何能力, 但要求 0b0001
        caps[CapDomain::FS.0 as usize] = CapBits(0);
        let r = mgr.login(1, true, CapBits(0b0001), CapDomain::FS, 100, 0, 0, caps);
        assert!(matches!(
            r,
            LoginResult::Denied(LoginDeny::CapabilityDenied(_))
        ));
    }

    #[test]
    fn login_success() {
        let mut t = SessionTable::new();
        let p = PolicyEngine::new();
        let mut mgr = SessionManager::new(&mut t, &p);
        let mut caps = default_caps();
        caps[CapDomain::FS.0 as usize] = CapBits(0xFF);
        let r = mgr.login(1, true, CapBits(0b1000), CapDomain::FS, 100, 0, 0, caps);
        assert!(matches!(r, LoginResult::Success(_)));
    }

    #[test]
    fn caps_helper() {
        let mut caps = default_caps();
        caps[CapDomain::FS.0 as usize] = CapBits(0x42);
        let m = InMemoryCaps(caps);
        assert_eq!(m.get(CapDomain::FS), Some(CapBits(0x42)));
        assert_eq!(m.get(CapDomain::NET), Some(CapBits::NONE));
    }
}
