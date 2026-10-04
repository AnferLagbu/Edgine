#![deny(unsafe_code)]
//! 身份与权限 — PWM/能力矩阵/会话 (services 层)
//!
//! ## 框内核中的 PWID 表达
//!
//! ```text
//! framework/sgeg/                  ← TCB (unsafe 允许)
//!   ├─ atomic_matrix.rs              ← 16×64 AtomicU64 物理存储
//!   ├─ password.rs                   ← SHA-256 + 常数时间比较
//!   └─ persist.rs                    ← 磁盘序列化
//!
//! services/sgeg/ (本模块)         ← 100% safe Rust
//!   ├─ policy.rs                     ← 能力检查策略 (本文件)
//!   ├─ grants.rs                     ← 委托规则
//!   ├─ sessions.rs                   ← 会话生命周期
//!   └─ audit.rs                      ← 审计日志生成
//! ```
//!
//! ## @SAFE
//! 本文件不含 `unsafe`。所有硬件交互通过 `framework::sgeg` 的安全 API。
//!
//! CI 由 `tools/check_tcb.sh` 通过 `grep -rP 'unsafe\s*(\{|fn |impl)'` 实际校验,
//! 不用编译期假属性 (历史上曾误用 `//! #![@SAFE]` 注释伪装, 已删除)。

use core::sync::atomic::{AtomicU64, Ordering};

/// 16 个能力域
pub const CAP_DOMAINS: usize = 16;
/// 每域 64 位
pub const CAP_BITS_PER_DOMAIN: u64 = 64;

/// 域 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CapDomain(pub u8);

impl CapDomain {
    pub const SYSTEM: Self = Self(0);
    pub const FS: Self = Self(1);
    pub const NET: Self = Self(2);
    pub const PROC: Self = Self(3);
    pub const DEVICE: Self = Self(4);
    pub const USER_MGMT: Self = Self(5);
    pub const IPC: Self = Self(6);
    pub const MEM: Self = Self(7);
    pub const TIME: Self = Self(8);
    pub const FREG: Self = Self(9);
    pub const SIGNAL: Self = Self(10);
    pub const SHM: Self = Self(11);
    pub const SEM: Self = Self(12);
    pub const MSGQ: Self = Self(13);
    pub const DMA: Self = Self(14);
    pub const RESERVED: Self = Self(15);

    pub fn is_valid(self) -> bool {
        (self.0 as usize) < CAP_DOMAINS
    }
}

/// 能力位
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CapBits(pub u64);

impl CapBits {
    pub const NONE: Self = Self(0);
    /// 全能力 (所有 64 位)
    pub const ALL: Self = Self(u64::MAX);

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub fn diff(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl core::ops::BitOr for CapBits {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for CapBits {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl core::ops::Not for CapBits {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

/// 策略结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyResult {
    /// 允许
    Allow,
    /// 拒绝 + 原因
    Deny(DenyReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// 域 ID 非法
    InvalidDomain,
    /// PWM 未找到
    UnknownPwm,
    /// 域内无此能力
    NoAuthority,
    /// 试图撤销可行下界
    FloorProtected,
    /// 域被禁用
    Disabled,
}

/// 委托结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantResult {
    Granted,
    NoAuthority,
    InvalidDomain,
    Empty,
}

/// 撤销结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeResult {
    Revoked,
    NoChange,
    InvalidDomain,
    FloorProtected,
}

/// 能力矩阵抽象 (services 调用 framework TCB)
///
/// `framework::sgeg::AtomicMatrix` 实现此 trait,
/// 通过 lock-free 路径提供能力读写。
pub trait CapabilityMatrix {
    /// 读取某域的能力位
    fn get(&self, domain: CapDomain) -> Option<CapBits>;
    /// 原子设置某域能力
    ///
    /// # Errors
    /// 当设置失败 (如原子操作冲突) 时返回 `Err(())`.
    fn set(&self, domain: CapDomain, bits: CapBits) -> Result<CapBits, ()>;
    /// 比较并交换 (用于 lock-free grant)
    ///
    /// # Errors
    /// 当比较并交换失败 (当前值已与 `current` 不一致) 时, 返回 `Err(实际当前值)`.
    fn compare_exchange(
        &self,
        domain: CapDomain,
        current: CapBits,
        new: CapBits,
    ) -> Result<CapBits, CapBits>;
}

/// 内存实现 (单线程 fallback, 测试用)
pub struct InMemoryMatrix {
    rows: [AtomicU64; CAP_DOMAINS],
}

/// 能力位图快照 (按域) — 用于快照/审计/降级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapMatrix {
    bits: [u64; CAP_DOMAINS],
}

impl CapMatrix {
    /// 空能力位图 (所有域均为 NONE)
    pub const fn empty() -> Self {
        Self {
            bits: [0u64; CAP_DOMAINS],
        }
    }
    /// 全能力位图
    pub const fn all() -> Self {
        Self {
            bits: [u64::MAX; CAP_DOMAINS],
        }
    }
    /// 从域位图构造
    pub const fn from_bits(bits: [u64; CAP_DOMAINS]) -> Self {
        Self { bits }
    }
    /// 取某域位图
    pub fn get(&self, domain: CapDomain) -> CapBits {
        CapBits(self.bits[domain.0 as usize])
    }
}

impl InMemoryMatrix {
    pub const fn new() -> Self {
        Self {
            rows: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }
}

impl CapabilityMatrix for InMemoryMatrix {
    fn get(&self, domain: CapDomain) -> Option<CapBits> {
        if !domain.is_valid() {
            return None;
        }
        Some(CapBits(
            self.rows[domain.0 as usize].load(Ordering::Acquire),
        ))
    }

    fn set(&self, domain: CapDomain, bits: CapBits) -> Result<CapBits, ()> {
        if !domain.is_valid() {
            return Err(());
        }
        let old = self.rows[domain.0 as usize].swap(bits.0, Ordering::AcqRel);
        Ok(CapBits(old))
    }

    fn compare_exchange(
        &self,
        domain: CapDomain,
        current: CapBits,
        new: CapBits,
    ) -> Result<CapBits, CapBits> {
        if !domain.is_valid() {
            return Err(current);
        }
        self.rows[domain.0 as usize]
            .compare_exchange(current.0, new.0, Ordering::AcqRel, Ordering::Acquire)
            .map(CapBits)
            .map_err(CapBits)
    }
}

/// 可行性下界 (viability floor)
///
/// 系统保留的最低能力:
/// - FS: READ | EXEC
/// - PROC: 派生 (FORK) | 执行 (EXEC)
/// - `USER_MGMT`: LIST
pub const VIABLE_FLOOR: [u64; CAP_DOMAINS] = {
    let mut f = [0u64; CAP_DOMAINS];
    f[CapDomain::FS.0 as usize] = (1 << 0) | (1 << 2); // READ | EXEC
    f[CapDomain::PROC.0 as usize] = (1 << 0) | (1 << 1); // FORK | EXEC
    f[CapDomain::USER_MGMT.0 as usize] = 1 << 0; // LIST
    f
};

/// 策略引擎 (services 层)
pub struct PolicyEngine {
    // 当前实现: 无状态, 仅依赖全局常量 VIABLE_FLOOR
    // 未来可扩展: 域间约束表 / 临时禁令 / 时段限制
}

impl PolicyEngine {
    pub const fn new() -> Self {
        Self {}
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 检查 (matrix, domain, required) 是否被允许
    pub fn check<M: CapabilityMatrix>(
        &self,
        matrix: &M,
        domain: CapDomain,
        required: CapBits,
    ) -> PolicyResult {
        if !domain.is_valid() {
            return PolicyResult::Deny(DenyReason::InvalidDomain);
        }
        if required.is_empty() {
            return PolicyResult::Allow;
        }
        let Some(owned) = matrix.get(domain) else {
            return PolicyResult::Deny(DenyReason::UnknownPwm);
        };
        if !owned.contains(required) {
            return PolicyResult::Deny(DenyReason::NoAuthority);
        }
        // 不能撤销可行下界 (仅对设有非零下界的域生效; 零下界域不受此约束)
        let floor = CapBits(VIABLE_FLOOR[domain.0 as usize]);
        if !floor.is_empty() && required.contains(floor) {
            return PolicyResult::Deny(DenyReason::FloorProtected);
        }
        PolicyResult::Allow
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 委托 (使用 CAS 保证并发安全)
    pub fn grant<M: CapabilityMatrix>(
        &self,
        from: &M,
        to: &M,
        domain: CapDomain,
        bits: CapBits,
    ) -> GrantResult {
        if !domain.is_valid() {
            return GrantResult::InvalidDomain;
        }
        if bits.is_empty() {
            return GrantResult::Empty;
        }
        let Some(owned) = from.get(domain) else {
            return GrantResult::NoAuthority;
        };
        if !owned.contains(bits) {
            return GrantResult::NoAuthority;
        }
        // 循环 CAS 重试
        loop {
            let to_current = to.get(domain).unwrap_or(CapBits::NONE);
            let to_new = to_current | bits;
            match to.compare_exchange(domain, to_current, to_new) {
                Ok(_) => return GrantResult::Granted,
                Err(_) => {} // 重试
            }
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 撤销 (保护可行下界)
    pub fn revoke<M: CapabilityMatrix>(
        &self,
        matrix: &M,
        domain: CapDomain,
        bits: CapBits,
    ) -> RevokeResult {
        if !domain.is_valid() {
            return RevokeResult::InvalidDomain;
        }
        if bits.is_empty() {
            return RevokeResult::NoChange;
        }
        let floor = CapBits(VIABLE_FLOOR[domain.0 as usize]);
        let revocable = bits.diff(floor);
        if revocable.is_empty() {
            return RevokeResult::FloorProtected;
        }
        loop {
            let current = matrix.get(domain).unwrap_or(CapBits::NONE);
            let new = current.diff(revocable);
            match matrix.compare_exchange(domain, current, new) {
                Ok(_) => return RevokeResult::Revoked,
                Err(_) => {}
            }
        }
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_matrix() -> InMemoryMatrix {
        InMemoryMatrix::new()
    }

    #[test]
    fn policy_basic_check() {
        let m = make_matrix();
        m.set(CapDomain::FS, CapBits(0xFF)).unwrap();

        let p = PolicyEngine::new();
        // 0x08 (bit3) 不含 FS 的可行性下界 (READ|EXEC = 0b101), 故仅需矩阵授权即 Allow
        // (原用 0x0F 含下界位, 必被 FloorProtected 拒绝; 2026-09-24 UT-06 修正)
        assert_eq!(
            p.check(&m, CapDomain::FS, CapBits(0x08)),
            PolicyResult::Allow
        );
        assert_eq!(
            p.check(&m, CapDomain::FS, CapBits(0x100)),
            PolicyResult::Deny(DenyReason::NoAuthority)
        );
    }

    #[test]
    fn policy_invalid_domain() {
        let m = make_matrix();
        let p = PolicyEngine::new();
        assert_eq!(
            p.check(&m, CapDomain(20), CapBits(0x01)),
            PolicyResult::Deny(DenyReason::InvalidDomain)
        );
    }

    #[test]
    fn policy_empty_required() {
        let m = make_matrix();
        let p = PolicyEngine::new();
        // 无要求即通过
        assert_eq!(
            p.check(&m, CapDomain::FS, CapBits::NONE),
            PolicyResult::Allow
        );
    }

    #[test]
    fn policy_floor_protection() {
        let m = make_matrix();
        m.set(CapDomain::FS, CapBits(0xFF)).unwrap();
        let p = PolicyEngine::new();
        // FS 底限 = READ | EXEC = 0b0101
        // 试图"撤销" 0b0101 应被 FloorProtected 拒绝
        // 实际语义: check(FS, required=0b0101) — 但 required=owned ⊆ owned 总是 true
        // 我们要求 required 包含 floor bits → 拒绝
        let result = p.check(&m, CapDomain::FS, CapBits(0b0101));
        assert_eq!(result, PolicyResult::Deny(DenyReason::FloorProtected));
    }

    #[test]
    fn policy_zero_floor_domain_allowed() {
        // 回归 (零下界域恒拒): 曾因 floor == 0 使 required.contains(floor) 恒真,
        // 导致 SYSTEM/NET 等 13 个零下界域一律被 FloorProtected 拒绝.
        let m = make_matrix();
        m.set(CapDomain::NET, CapBits(0x03)).unwrap();
        m.set(CapDomain::SYSTEM, CapBits(0x01)).unwrap();
        let p = PolicyEngine::new();
        assert_eq!(
            p.check(&m, CapDomain::NET, CapBits(0x03)),
            PolicyResult::Allow
        );
        assert_eq!(
            p.check(&m, CapDomain::SYSTEM, CapBits(0x01)),
            PolicyResult::Allow
        );
    }

    #[test]
    fn grant_basic() {
        let from = make_matrix();
        let to = make_matrix();
        from.set(CapDomain::FS, CapBits(0b1111)).unwrap();

        let p = PolicyEngine::new();
        let r = p.grant(&from, &to, CapDomain::FS, CapBits(0b1000));
        assert_eq!(r, GrantResult::Granted);
        assert_eq!(to.get(CapDomain::FS), Some(CapBits(0b1000)));
    }

    #[test]
    fn grant_no_authority() {
        let from = make_matrix();
        let to = make_matrix();
        let p = PolicyEngine::new();
        let r = p.grant(&from, &to, CapDomain::FS, CapBits(0b0001));
        assert_eq!(r, GrantResult::NoAuthority);
    }

    #[test]
    fn grant_invalid_domain() {
        let from = make_matrix();
        let to = make_matrix();
        let p = PolicyEngine::new();
        let r = p.grant(&from, &to, CapDomain(20), CapBits(0b0001));
        assert_eq!(r, GrantResult::InvalidDomain);
    }

    #[test]
    fn revoke_basic() {
        let m = make_matrix();
        m.set(CapDomain::FS, CapBits(0b1111)).unwrap();
        let p = PolicyEngine::new();
        let r = p.revoke(&m, CapDomain::FS, CapBits(0b1000));
        assert_eq!(r, RevokeResult::Revoked);
        assert_eq!(m.get(CapDomain::FS), Some(CapBits(0b0111)));
    }

    #[test]
    fn revoke_floor_protected() {
        let m = make_matrix();
        m.set(CapDomain::FS, CapBits(0b1111)).unwrap();
        let p = PolicyEngine::new();
        // 试图撤销 floor (READ|EXEC = 0b0101)
        let r = p.revoke(&m, CapDomain::FS, CapBits(0b0101));
        assert_eq!(r, RevokeResult::FloorProtected);
    }

    #[test]
    fn revoke_invalid_domain() {
        let m = make_matrix();
        let p = PolicyEngine::new();
        let r = p.revoke(&m, CapDomain(20), CapBits(0b0001));
        assert_eq!(r, RevokeResult::InvalidDomain);
    }

    #[test]
    fn revoke_empty() {
        let m = make_matrix();
        let p = PolicyEngine::new();
        let r = p.revoke(&m, CapDomain::FS, CapBits::NONE);
        assert_eq!(r, RevokeResult::NoChange);
    }

    /// 跨域独立性
    #[test]
    fn domains_independent() {
        let m = make_matrix();
        m.set(CapDomain::FS, CapBits::ALL).unwrap();
        m.set(CapDomain::NET, CapBits::ALL).unwrap();
        assert_eq!(m.get(CapDomain::FS), Some(CapBits::ALL));
        assert_eq!(m.get(CapDomain::NET), Some(CapBits::ALL));
        assert_eq!(m.get(CapDomain::PROC), Some(CapBits::NONE));
    }

    // UT-07 (2026-09-26): 注册侧 pwm::policy 组全量迁入 —
    // framework/tests/test_sgeg.rs 的 23 例 (CapBits 位运算语义 +
    // CapMatrix 快照 + InMemoryMatrix 可变矩阵 + 域边界) 逐例归并等价判据,
    // 断言数净增不净减; 该注册载体随后整体删除.

    use crate::services::sgeg::capability::{
        FS_CAP_CHOWN, FS_CAP_DELETE, FS_CAP_EXECUTE, FS_CAP_READ, FS_CAP_WRITE, PROC_CAP_EXEC,
        PROC_CAP_FORK, PROC_CAP_KILL, SYS_CAP_ALL,
    };

    #[test]
    fn cap_bits_has() {
        let cb = CapBits(FS_CAP_READ | FS_CAP_WRITE);
        assert!(cb.contains(CapBits(FS_CAP_READ)), "应含 READ");
        assert!(cb.contains(CapBits(FS_CAP_WRITE)), "应含 WRITE");
        assert!(!cb.contains(CapBits(FS_CAP_EXECUTE)), "不应含 EXEC");
    }

    #[test]
    fn cap_bits_grant() {
        let cb = CapBits(FS_CAP_READ) | CapBits(FS_CAP_WRITE);
        assert!(cb.contains(CapBits(FS_CAP_READ)), "应含 READ");
        assert!(cb.contains(CapBits(FS_CAP_WRITE)), "应含 WRITE");
    }

    #[test]
    fn cap_bits_revoke() {
        let cb = CapBits(FS_CAP_READ | FS_CAP_WRITE).diff(CapBits(FS_CAP_READ));
        assert!(!cb.contains(CapBits(FS_CAP_READ)), "READ 已撤销");
        assert!(cb.contains(CapBits(FS_CAP_WRITE)), "WRITE 保留");
    }

    #[test]
    fn cap_bits_superset() {
        let full = CapBits(FS_CAP_READ | FS_CAP_WRITE | FS_CAP_EXECUTE);
        let partial = CapBits(FS_CAP_READ);
        assert!(full.contains(partial), "全集包含子集");
        assert!(!partial.contains(full), "子集不包含全集");
    }

    #[test]
    fn cap_matrix_new_empty() {
        let cm = InMemoryMatrix::new();
        assert_eq!(cm.get(CapDomain::FS), Some(CapBits::NONE), "FS 为空");
        assert_eq!(cm.get(CapDomain::PROC), Some(CapBits::NONE), "PROC 为空");
    }

    #[test]
    fn cap_matrix_grant_revoke() {
        let cm = InMemoryMatrix::new();
        cm.set(CapDomain::FS, CapBits(FS_CAP_READ | FS_CAP_WRITE))
            .unwrap();
        assert!(
            cm.get(CapDomain::FS)
                .unwrap()
                .contains(CapBits(FS_CAP_READ)),
            "FS 应含 READ"
        );
        assert!(
            cm.get(CapDomain::FS)
                .unwrap()
                .contains(CapBits(FS_CAP_WRITE)),
            "FS 应含 WRITE"
        );
        cm.set(CapDomain::FS, CapBits(FS_CAP_READ)).unwrap();
        assert!(
            cm.get(CapDomain::FS)
                .unwrap()
                .contains(CapBits(FS_CAP_READ)),
            "FS 仍含 READ"
        );
        assert!(
            !cm.get(CapDomain::FS)
                .unwrap()
                .contains(CapBits(FS_CAP_WRITE)),
            "FS 已失去 WRITE"
        );
    }

    #[test]
    fn cap_matrix_all() {
        let cm = CapMatrix::all();
        for d in 0..16u8 {
            assert_eq!(cm.get(CapDomain(d)), CapBits::ALL, "全域均为 ALL");
        }
    }

    #[test]
    fn cap_matrix_viable() {
        let cm = CapMatrix::from_bits(VIABLE_FLOOR);
        assert!(
            cm.get(CapDomain::FS).contains(CapBits(FS_CAP_READ)),
            "FS 下界含 READ"
        );
        assert!(
            cm.get(CapDomain::FS).contains(CapBits(FS_CAP_EXECUTE)),
            "FS 下界含 EXEC"
        );
        assert!(
            !cm.get(CapDomain::FS).contains(CapBits(FS_CAP_WRITE)),
            "FS 下界不含 WRITE"
        );
        assert!(
            cm.get(CapDomain::PROC).contains(CapBits(PROC_CAP_FORK)),
            "PROC 下界含 FORK"
        );
        assert!(
            cm.get(CapDomain::PROC).contains(CapBits(PROC_CAP_EXEC)),
            "PROC 下界含 EXEC"
        );
    }

    #[test]
    fn cap_matrix_superset() {
        let parent = CapMatrix::all();
        let child = CapMatrix::from_bits(VIABLE_FLOOR);
        for d in 0..16u8 {
            assert!(
                parent.get(CapDomain(d)).contains(child.get(CapDomain(d))),
                "父集包含子集"
            );
            assert!(
                !child.get(CapDomain(d)).contains(parent.get(CapDomain(d))),
                "子集不包含父集"
            );
        }
    }

    #[test]
    fn cap_matrix_out_of_range() {
        let cm = InMemoryMatrix::new();
        assert!(!CapDomain(16).is_valid(), "16 非法");
        assert!(!CapDomain(255).is_valid(), "255 非法");
        assert_eq!(cm.get(CapDomain(16)), None, "get 16 为 None");
        assert_eq!(cm.get(CapDomain(255)), None, "get 255 为 None");
    }

    #[test]
    fn cap_bits_empty_has_nothing() {
        let cb = CapBits::NONE;
        assert!(!cb.contains(CapBits(FS_CAP_READ)), "NONE 不含 READ");
        assert!(!cb.contains(CapBits(FS_CAP_WRITE)), "NONE 不含 WRITE");
        assert!(!cb.contains(CapBits(SYS_CAP_ALL)), "NONE 不含 SYS_ALL");
    }

    #[test]
    fn cap_bits_grant_all_then_revoke_one() {
        let cb = CapBits::ALL;
        assert!(cb.contains(CapBits(FS_CAP_READ)), "ALL 含 READ");
        assert!(cb.contains(CapBits(SYS_CAP_ALL)), "ALL 含 SYS_ALL");
        let cb = cb.diff(CapBits(FS_CAP_READ));
        assert!(!cb.contains(CapBits(FS_CAP_READ)), "READ 已撤销");
        assert!(cb.contains(CapBits(FS_CAP_WRITE)), "WRITE 保留");
    }

    #[test]
    fn cap_bits_revoke_nonexistent_is_noop() {
        let cb = CapBits(FS_CAP_READ).diff(CapBits(FS_CAP_WRITE));
        assert!(cb.contains(CapBits(FS_CAP_READ)), "READ 保留");
        assert!(!cb.contains(CapBits(FS_CAP_WRITE)), "WRITE 本就不存在");
    }

    #[test]
    fn cap_bits_grant_idempotent() {
        let cb = CapBits(FS_CAP_READ) | CapBits(FS_CAP_READ);
        assert!(cb.contains(CapBits(FS_CAP_READ)), "应含 READ");
        assert_eq!(cb, CapBits(FS_CAP_READ), "幂等值不变");
    }

    #[test]
    fn cap_matrix_delegation_chain() {
        let root = CapMatrix::all();
        let admin = InMemoryMatrix::new();
        admin
            .set(
                CapDomain::FS,
                CapBits(FS_CAP_READ | FS_CAP_WRITE | FS_CAP_EXECUTE | (1 << 3) | (1 << 4)),
            )
            .unwrap();
        admin
            .set(
                CapDomain::PROC,
                CapBits(PROC_CAP_FORK | PROC_CAP_EXEC | PROC_CAP_KILL),
            )
            .unwrap();
        let user = InMemoryMatrix::new();
        user.set(CapDomain::FS, CapBits(FS_CAP_READ | FS_CAP_EXECUTE))
            .unwrap();
        user.set(CapDomain::PROC, CapBits(PROC_CAP_FORK | PROC_CAP_EXEC))
            .unwrap();
        assert!(
            root.get(CapDomain::FS)
                .contains(admin.get(CapDomain::FS).unwrap()),
            "root 包含 admin"
        );
        assert!(
            admin
                .get(CapDomain::FS)
                .unwrap()
                .contains(user.get(CapDomain::FS).unwrap()),
            "admin 包含 user"
        );
        assert!(
            !user
                .get(CapDomain::FS)
                .unwrap()
                .contains(admin.get(CapDomain::FS).unwrap()),
            "user 不包含 admin"
        );
    }

    #[test]
    fn cap_matrix_revocation_partial() {
        let all = CapMatrix::all();
        let fs_bits = all
            .get(CapDomain::FS)
            .diff(CapBits(FS_CAP_DELETE | FS_CAP_CHOWN));
        assert!(fs_bits.contains(CapBits(FS_CAP_READ)), "含 READ");
        assert!(fs_bits.contains(CapBits(FS_CAP_WRITE)), "含 WRITE");
        assert!(!fs_bits.contains(CapBits(FS_CAP_DELETE)), "不含 DELETE");
        assert!(!fs_bits.contains(CapBits(FS_CAP_CHOWN)), "不含 CHOWN");
    }

    #[test]
    fn cap_matrix_viable_is_not_all() {
        let viable = CapMatrix::from_bits(VIABLE_FLOOR);
        let all = CapMatrix::all();
        assert!(
            all.get(CapDomain::FS).contains(viable.get(CapDomain::FS)),
            "ALL 包含下界"
        );
        assert!(
            !viable.get(CapDomain::FS).contains(all.get(CapDomain::FS)),
            "下界不包含 ALL"
        );
    }

    #[test]
    fn cap_matrix_grant_out_of_range_silent() {
        let cm = InMemoryMatrix::new();
        assert!(cm.set(CapDomain(16), CapBits(0xFF)).is_err(), "16 set 报错");
        assert!(
            cm.set(CapDomain(255), CapBits(0xFF)).is_err(),
            "255 set 报错"
        );
        assert_eq!(cm.get(CapDomain(16)), None, "16 仍为 None");
        assert_eq!(cm.get(CapDomain(255)), None, "255 仍为 None");
    }

    #[test]
    fn cap_matrix_revoke_out_of_range_silent() {
        // 内核 set 对非法域返回 Err, 矩阵内容不变 (等价原 revoke 静默失败)
        let cm = InMemoryMatrix::new();
        assert!(cm.set(CapDomain(16), CapBits::ALL).is_err(), "16 set 报错");
        assert!(
            cm.set(CapDomain(255), CapBits::ALL).is_err(),
            "255 set 报错"
        );
        for d in 0..16u8 {
            assert_eq!(cm.get(CapDomain(d)), Some(CapBits::NONE), "全为 NONE");
        }
    }

    #[test]
    fn cap_matrix_cross_domain_isolation() {
        let cm = InMemoryMatrix::new();
        cm.set(CapDomain::FS, CapBits(FS_CAP_READ)).unwrap();
        assert_eq!(cm.get(CapDomain::PROC), Some(CapBits::NONE), "PROC 为空");
        assert_eq!(cm.get(CapDomain::NET), Some(CapBits::NONE), "NET 为空");
    }

    #[test]
    fn cap_matrix_empty_not_superset_of_viable() {
        let empty = CapMatrix::empty();
        let viable = CapMatrix::from_bits(VIABLE_FLOOR);
        assert!(
            !empty.get(CapDomain::FS).contains(viable.get(CapDomain::FS)),
            "空矩阵不包含下界"
        );
    }

    #[test]
    fn cap_bits_superset_reflexive() {
        let cb = CapBits(FS_CAP_READ | FS_CAP_WRITE);
        assert!(cb.contains(cb), "自反");
    }

    #[test]
    fn cap_matrix_superset_reflexive() {
        let cm = CapMatrix::from_bits(VIABLE_FLOOR);
        assert!(
            cm.get(CapDomain::FS).contains(cm.get(CapDomain::FS)),
            "自反"
        );
    }
}
