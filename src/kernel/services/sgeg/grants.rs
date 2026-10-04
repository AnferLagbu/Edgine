#![deny(unsafe_code)]
//! 能力委托 — 链式委托 / 时间约束 / 撤销传播 (services 层)
//!
//! ## 框内核中的表达
//!
//! 委托是 services 层的**策略**, TCB 只提供"原子地 grant/revoke"操作.
//! 链式委托、时间约束、撤销传播都是策略层关注点.
//!
//! ## 数据结构
//!
//! ```text
//! GrantRecord {
//!   from_pwm: u64,        // 委托者
//!   to_pwm: u64,          // 受托者
//!   domain: CapDomain,    // 哪个能力域
//!   bits: CapBits,        // 授予的位
//!   created_tick: u64,    // 创建时间
//!   expires_tick: u64,    // 过期时间 (0 = 永久)
//!   generation: u32,      // 委托代数
//!   parent_gen: u32,      // 父委托 (链式)
//!   flags: GrantFlags,    // NON_DELEGATABLE / REVOKED
//! }
//! ```
//!
//! ## @SAFE
//! 本文件不含 `unsafe`. 时间由 `framework::time` 提供.

use super::policy::{CapBits, CapDomain, CapabilityMatrix, GrantResult, PolicyEngine};
use core::sync::atomic::{AtomicU32, Ordering};

bitflags::bitflags! {
    /// 委托标志
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GrantFlags: u32 {
        const NONE       = 0;
        /// 不可再委托
        const NON_DELEGATABLE = 1 << 0;
        /// 已撤销
        const REVOKED    = 1 << 1;
        /// 审计已记录
        const AUDITED    = 1 << 2;
    }
}

/// 委托记录
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantRecord {
    pub from_pwm: u64,
    pub to_pwm: u64,
    pub domain: CapDomain,
    pub bits: CapBits,
    pub created_tick: u64,
    pub expires_tick: u64,
    pub generation: u32,
    pub parent_gen: u32,
    pub flags: GrantFlags,
}

/// 委托表容量 (B07-16: 改名消除与 `types::MAX_GRANT_RECORDS`(1024,
/// framework 授权记录表) 的同名冲突 — 两者是分层不同职责)
pub const GRANT_TABLE_CAPACITY: usize = 256;

/// 委托表 (固定大小, 避免动态分配)
pub struct GrantTable {
    records: [GrantRecord; GRANT_TABLE_CAPACITY],
    next_gen: AtomicU32,
    used: AtomicU32,
}

impl GrantTable {
    pub const fn new() -> Self {
        const EMPTY: GrantRecord = GrantRecord {
            from_pwm: 0,
            to_pwm: 0,
            domain: CapDomain(0),
            bits: CapBits(0),
            created_tick: 0,
            expires_tick: 0,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        Self {
            records: [EMPTY; GRANT_TABLE_CAPACITY],
            next_gen: AtomicU32::new(1),
            used: AtomicU32::new(0),
        }
    }

    /// 添加委托记录
    pub fn add(&mut self, mut record: GrantRecord) -> Option<u32> {
        if self.used.load(Ordering::Acquire) as usize >= GRANT_TABLE_CAPACITY {
            return None;
        }
        let r#gen = self.next_gen.fetch_add(1, Ordering::AcqRel);
        record.generation = r#gen;
        // 线性查找空槽 (generation == 0)
        for i in 0..GRANT_TABLE_CAPACITY {
            if self.records[i].generation == 0 {
                self.records[i] = record;
                self.used.fetch_add(1, Ordering::AcqRel);
                return Some(r#gen);
            }
        }
        None
    }

    /// 按 generation 查找
    pub fn get(&self, r#gen: u32) -> Option<&GrantRecord> {
        if r#gen == 0 {
            return None;
        }
        for r in &self.records {
            if r.generation == r#gen {
                return Some(r);
            }
        }
        None
    }

    /// 标记撤销 (释放槽) — B07-16: 级联撤销所有以 `r#gen` 为祖先的后代委托.
    ///
    /// 撤销父委托时, 按 `parent_gen` 链递归收集全部后代一并撤销, 防止
    /// 子委托在父被撤销后仍有效 (权限放大).
    pub fn mark_revoked(&mut self, r#gen: u32) -> bool {
        // 撤销目标本身 + 全部后代 (先收集, 在父记录被清空前).
        let mut victims: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
        victims.push(r#gen);
        self.collect_descendants(r#gen, &mut victims);
        let mut revoked_any = false;
        for &g in &victims {
            for i in 0..GRANT_TABLE_CAPACITY {
                if self.records[i].generation == g {
                    if !self.records[i].flags.contains(GrantFlags::REVOKED) {
                        self.records[i].flags |= GrantFlags::REVOKED;
                        self.records[i].generation = 0; // 释放
                        self.used.fetch_sub(1, Ordering::AcqRel);
                        revoked_any = true;
                    }
                    break;
                }
            }
        }
        revoked_any
    }

    /// 递归收集 `parent` 的全部后代代 (含直接子与间接孙).
    fn collect_descendants(&self, parent: u32, out: &mut alloc::vec::Vec<u32>) {
        for r in &self.records {
            if r.generation != 0 && r.parent_gen == parent {
                out.push(r.generation);
                self.collect_descendants(r.generation, out);
            }
        }
    }

    /// 检查委托是否仍有效
    pub fn is_valid(&self, r#gen: u32, current_tick: u64) -> bool {
        match self.get(r#gen) {
            Some(r) => {
                if r.flags.contains(GrantFlags::REVOKED) {
                    return false;
                }
                if r.expires_tick != 0 && current_tick >= r.expires_tick {
                    return false;
                }
                true
            }
            None => false,
        }
    }

    /// 撤销某 PWM 发出的所有委托
    pub fn revoke_all_from(&mut self, pwm: u64) -> usize {
        let mut count = 0;
        let gens: [u32; GRANT_TABLE_CAPACITY] = {
            let mut g = [0u32; GRANT_TABLE_CAPACITY];
            let mut idx = 0;
            for r in &self.records {
                if r.generation != 0 && r.from_pwm == pwm && idx < GRANT_TABLE_CAPACITY {
                    g[idx] = r.generation;
                    idx += 1;
                }
            }
            g
        };
        for &r#gen in &gens {
            if r#gen == 0 {
                break;
            }
            if self.mark_revoked(r#gen) {
                count += 1;
            }
        }
        count
    }

    pub fn len(&self) -> usize {
        self.used.load(Ordering::Acquire) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 委托操作结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationResult {
    Granted { r#gen: u32 },
    Denied(DelegationDeny),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationDeny {
    NonDelegatable,
    TableFull,
    InvalidExpiry,
    SamePwm,
    Policy(GrantResult),
}

/// 委托引擎
pub struct DelegationEngine<'a> {
    pub table: &'a mut GrantTable,
    pub policy: &'a PolicyEngine,
}

impl<'a> DelegationEngine<'a> {
    pub fn new(table: &'a mut GrantTable, policy: &'a PolicyEngine) -> Self {
        Self { table, policy }
    }

    /// 委托
    ///
    /// `parent_gen`: 父委托的 generation (链式委托的父链, 0 = 根委托).
    /// B07-16: 记录父委托链, 使撤销父委托时级联撤销后代 (权限放大防护).
    pub fn delegate<M: CapabilityMatrix>(
        &mut self,
        from_matrix: &M,
        to_matrix: &mut M,
        from_pwm: u64,
        to_pwm: u64,
        domain: CapDomain,
        bits: CapBits,
        current_tick: u64,
        expires_tick: u64,
        parent_gen: u32,
        non_delegatable: bool,
    ) -> DelegationResult {
        if from_pwm == to_pwm {
            return DelegationResult::Denied(DelegationDeny::SamePwm);
        }
        if expires_tick != 0 && expires_tick <= current_tick {
            return DelegationResult::Denied(DelegationDeny::InvalidExpiry);
        }
        // 若声明父委托, 父必须存在且未被撤销 (防悬空父链).
        if parent_gen != 0 && !self.table.is_valid(parent_gen, current_tick) {
            return DelegationResult::Denied(DelegationDeny::InvalidExpiry);
        }
        let grant_res = self.policy.grant(from_matrix, to_matrix, domain, bits);
        match grant_res {
            GrantResult::Granted => {
                let flags = if non_delegatable {
                    GrantFlags::NON_DELEGATABLE
                } else {
                    GrantFlags::NONE
                };
                let rec = GrantRecord {
                    from_pwm,
                    to_pwm,
                    domain,
                    bits,
                    created_tick: current_tick,
                    expires_tick,
                    generation: 0,
                    parent_gen,
                    flags,
                };
                self.table.add(rec).map_or(
                    DelegationResult::Denied(DelegationDeny::TableFull),
                    |r#gen| DelegationResult::Granted { r#gen },
                )
            }
            other => DelegationResult::Denied(DelegationDeny::Policy(other)),
        }
    }

    /// 撤销指定 gen 的委托
    pub fn revoke<M: CapabilityMatrix>(&mut self, matrix: &mut M, r#gen: u32) -> bool {
        if let Some(rec) = self.table.get(r#gen).copied() {
            if rec.flags.contains(GrantFlags::REVOKED) {
                return false;
            }
            let _ = self.policy.revoke(matrix, rec.domain, rec.bits);
            self.table.mark_revoked(r#gen);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::policy::InMemoryMatrix;
    use super::*;

    fn make_matrix() -> InMemoryMatrix {
        InMemoryMatrix::new()
    }

    #[test]
    fn grant_table_basic_add() {
        let mut t = GrantTable::new();
        let rec = GrantRecord {
            from_pwm: 1,
            to_pwm: 2,
            domain: CapDomain::FS,
            bits: CapBits(0xFF),
            created_tick: 100,
            expires_tick: 0,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        let r#gen = t.add(rec).unwrap();
        assert!(r#gen > 0);
        assert_eq!(t.len(), 1);
        let got = t.get(r#gen).unwrap();
        assert_eq!(got.from_pwm, 1);
        assert_eq!(got.to_pwm, 2);
    }

    #[test]
    fn grant_table_full() {
        let mut t = GrantTable::new();
        for i in 0..GRANT_TABLE_CAPACITY {
            let rec = GrantRecord {
                from_pwm: 1,
                to_pwm: 2 + i as u64,
                domain: CapDomain::FS,
                bits: CapBits(0xFF),
                created_tick: 0,
                expires_tick: 0,
                generation: 0,
                parent_gen: 0,
                flags: GrantFlags::NONE,
            };
            assert!(t.add(rec).is_some(), "should add {}", i);
        }
        let rec = GrantRecord {
            from_pwm: 1,
            to_pwm: 999,
            domain: CapDomain::FS,
            bits: CapBits(0x01),
            created_tick: 0,
            expires_tick: 0,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        assert_eq!(t.add(rec), None);
    }

    #[test]
    fn grant_table_revoke_frees_slot() {
        let mut t = GrantTable::new();
        let rec = GrantRecord {
            from_pwm: 1,
            to_pwm: 2,
            domain: CapDomain::FS,
            bits: CapBits(0x01),
            created_tick: 0,
            expires_tick: 0,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        let r#gen = t.add(rec).unwrap();
        assert_eq!(t.len(), 1);
        assert!(t.mark_revoked(r#gen));
        assert_eq!(t.len(), 0);

        let rec2 = GrantRecord {
            from_pwm: 1,
            to_pwm: 3,
            domain: CapDomain::FS,
            bits: CapBits(0x02),
            created_tick: 0,
            expires_tick: 0,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        assert!(t.add(rec2).is_some());
    }

    #[test]
    fn grant_expiry() {
        let mut t = GrantTable::new();
        let rec = GrantRecord {
            from_pwm: 1,
            to_pwm: 2,
            domain: CapDomain::FS,
            bits: CapBits(0x01),
            created_tick: 100,
            expires_tick: 200,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        let r#gen = t.add(rec).unwrap();
        assert!(t.is_valid(r#gen, 150));
        assert!(!t.is_valid(r#gen, 200));
        assert!(!t.is_valid(r#gen, 300));
    }

    #[test]
    fn grant_permanent_never_expires() {
        let mut t = GrantTable::new();
        let rec = GrantRecord {
            from_pwm: 1,
            to_pwm: 2,
            domain: CapDomain::FS,
            bits: CapBits(0x01),
            created_tick: 0,
            expires_tick: 0,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        let r#gen = t.add(rec).unwrap();
        assert!(t.is_valid(r#gen, u64::MAX));
    }

    #[test]
    fn revoke_all_from() {
        let mut t = GrantTable::new();
        // 来自 pwm=1 的 3 个委托
        for i in 0..3 {
            let rec = GrantRecord {
                from_pwm: 1,
                to_pwm: 10 + i,
                domain: CapDomain::FS,
                bits: CapBits(0x01),
                created_tick: 0,
                expires_tick: 0,
                generation: 0,
                parent_gen: 0,
                flags: GrantFlags::NONE,
            };
            t.add(rec).unwrap();
        }
        // 来自 pwm=2 的 1 个委托
        let rec = GrantRecord {
            from_pwm: 2,
            to_pwm: 100,
            domain: CapDomain::FS,
            bits: CapBits(0x01),
            created_tick: 0,
            expires_tick: 0,
            generation: 0,
            parent_gen: 0,
            flags: GrantFlags::NONE,
        };
        t.add(rec).unwrap();

        assert_eq!(t.len(), 4);
        let revoked = t.revoke_all_from(1);
        assert_eq!(revoked, 3);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn delegate_basic() {
        let mut table = GrantTable::new();
        let policy = PolicyEngine::new();
        let from = make_matrix();
        let mut to = make_matrix();
        from.set(CapDomain::FS, CapBits(0xFF)).unwrap();

        let mut eng = DelegationEngine::new(&mut table, &policy);
        let r = eng.delegate(
            &from,
            &mut to,
            1,
            2,
            CapDomain::FS,
            CapBits(0b1000),
            100,
            0,
            0,
            false,
        );
        assert!(matches!(r, DelegationResult::Granted { .. }));
        assert_eq!(to.get(CapDomain::FS), Some(CapBits(0b1000)));
    }

    #[test]
    fn delegate_same_pwm() {
        let mut table = GrantTable::new();
        let policy = PolicyEngine::new();
        let from = make_matrix();
        let mut to = make_matrix();
        from.set(CapDomain::FS, CapBits(0xFF)).unwrap();
        let mut eng = DelegationEngine::new(&mut table, &policy);
        let r = eng.delegate(
            &from,
            &mut to,
            1,
            1,
            CapDomain::FS,
            CapBits(0b1000),
            100,
            0,
            0,
            false,
        );
        assert_eq!(r, DelegationResult::Denied(DelegationDeny::SamePwm));
    }

    #[test]
    fn delegate_invalid_expiry() {
        let mut table = GrantTable::new();
        let policy = PolicyEngine::new();
        let from = make_matrix();
        let mut to = make_matrix();
        from.set(CapDomain::FS, CapBits(0xFF)).unwrap();
        let mut eng = DelegationEngine::new(&mut table, &policy);
        // 过期刻度 (50) <= 当前刻度 (100)
        let r = eng.delegate(
            &from,
            &mut to,
            1,
            2,
            CapDomain::FS,
            CapBits(0b1000),
            100,
            50,
            0,
            false,
        );
        assert_eq!(r, DelegationResult::Denied(DelegationDeny::InvalidExpiry));
    }

    #[test]
    fn delegate_no_authority() {
        let mut table = GrantTable::new();
        let policy = PolicyEngine::new();
        let from = make_matrix();
        let mut to = make_matrix();
        let mut eng = DelegationEngine::new(&mut table, &policy);
        let r = eng.delegate(
            &from,
            &mut to,
            1,
            2,
            CapDomain::FS,
            CapBits(0b1000),
            100,
            0,
            0,
            false,
        );
        assert!(matches!(
            r,
            DelegationResult::Denied(DelegationDeny::Policy(_))
        ));
    }

    #[test]
    fn delegate_revoke() {
        let mut table = GrantTable::new();
        let policy = PolicyEngine::new();
        let from = make_matrix();
        let mut to = make_matrix();
        from.set(CapDomain::FS, CapBits(0xFF)).unwrap();
        let mut eng = DelegationEngine::new(&mut table, &policy);
        let r = eng.delegate(
            &from,
            &mut to,
            1,
            2,
            CapDomain::FS,
            CapBits(0b1000),
            100,
            0,
            0,
            false,
        );
        let r#gen = match r {
            DelegationResult::Granted { r#gen } => r#gen,
            _ => panic!("expected Granted"),
        };
        assert_eq!(to.get(CapDomain::FS), Some(CapBits(0b1000)));
        assert!(eng.revoke(&mut to, r#gen));
        // 0b1000 非 floor (FS floor = 0b0101), 应被撤销
        assert_eq!(to.get(CapDomain::FS), Some(CapBits::NONE));
    }

    // B07-16: 链式委托 — 撤销父委托级联撤销后代 (权限放大防护)
    #[test]
    fn delegate_cascade_revoke() {
        let mut table = GrantTable::new();
        let policy = PolicyEngine::new();
        let from = make_matrix();
        let mut to = make_matrix();
        let mut to2 = make_matrix();
        from.set(CapDomain::FS, CapBits(0xFF)).unwrap();
        to.set(CapDomain::FS, CapBits(0xFF)).unwrap();
        let mut eng = DelegationEngine::new(&mut table, &policy);

        // 根委托: A(1) → B(2)
        let r1 = eng.delegate(
            &from,
            &mut to,
            1,
            2,
            CapDomain::FS,
            CapBits(0b1000),
            100,
            0,
            0,
            false,
        );
        let gen1 = match r1 {
            DelegationResult::Granted { r#gen } => r#gen,
            _ => panic!("expected Granted"),
        };
        // 子委托: B(2) → C(3), parent_gen = gen1
        let r2 = eng.delegate(
            &to,
            &mut to2,
            2,
            3,
            CapDomain::FS,
            CapBits(0b0100),
            100,
            0,
            gen1,
            false,
        );
        let gen2 = match r2 {
            DelegationResult::Granted { r#gen } => r#gen,
            _ => panic!("expected Granted"),
        };
        // eng 仍持有 &mut table, 故经 eng.table 读取 (裸用 table 会触发 E0502).
        assert!(eng.table.is_valid(gen2, 100));

        // 撤销父委托 gen1 → 子委托 gen2 也应被级联撤销
        assert!(eng.revoke(&mut to, gen1));
        assert!(!table.is_valid(gen2, 100), "child grant must be cascaded");
        assert!(!table.is_valid(gen1, 100));
    }

    // B07-16: 悬空父链 — 父委托不存在时拒绝创建子委托
    #[test]
    fn delegate_dangling_parent_rejected() {
        let mut table = GrantTable::new();
        let policy = PolicyEngine::new();
        let from = make_matrix();
        let mut to = make_matrix();
        from.set(CapDomain::FS, CapBits(0xFF)).unwrap();
        let mut eng = DelegationEngine::new(&mut table, &policy);
        // parent_gen=999 不存在 → 拒绝
        let r = eng.delegate(
            &from,
            &mut to,
            1,
            2,
            CapDomain::FS,
            CapBits(0b1000),
            100,
            0,
            999,
            false,
        );
        assert!(matches!(
            r,
            DelegationResult::Denied(DelegationDeny::InvalidExpiry)
        ));
    }
}
