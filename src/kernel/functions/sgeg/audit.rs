#![deny(unsafe_code)]
//! 审计日志 — 仅追加 / 哈希链 / 不可篡改 (functions 层)
//!
//! ## 框内核中的表达
//!
//! 审计是 functions 层的**外部安全观察**. TCB 提供原子写入原语,
//! functions 在其上构建哈希链与查询接口.
//!
//! ```text
//! audit.append(event)
//!   ↓
//!   record { prev_hash, payload, hash }
//!   ↓
//!   privileged::sgeg::atomic_write
//!     ↓
//!   functions::sgeg::audit::store[index]  ← 环形缓冲
//! ```
//!
//! ## @SAFE
//! 本文件不含 `unsafe`. 哈希由 privileged 提供.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// 审计事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEventKind {
    LoginSuccess,
    LoginFailed,
    Logout,
    CapabilityGranted,
    CapabilityRevoked,
    SessionKilled,
    PolicyViolation,
    FREGRecovery,
    FREGReset,
    Custom(u8),
}

impl AuditEventKind {
    /// 数字标签 (用于哈希)
    pub const fn tag(self) -> u8 {
        match self {
            Self::LoginSuccess => 0x01,
            Self::LoginFailed => 0x02,
            Self::Logout => 0x03,
            Self::CapabilityGranted => 0x04,
            Self::CapabilityRevoked => 0x05,
            Self::SessionKilled => 0x06,
            Self::PolicyViolation => 0x07,
            Self::FREGRecovery => 0x08,
            Self::FREGReset => 0x09,
            Self::Custom(v) => v,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditEvent {
    pub kind: AuditEventKind,
    pub tick: u64,
    pub pwm: u64,
    pub session_id: u32,
    pub domain_id: u8,
    pub bits: u64,
    pub result: i32,
}

/// 哈希链节点
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashChainNode {
    pub index: u32,
    pub event: AuditEvent,
    pub prev_hash: u64,
    pub hash: u64,
}

/// 审计日志 (环形缓冲 + 哈希链)
pub struct AuditLog {
    /// 物理存储
    buffer: [HashChainNode; AUDIT_BUFFER_SIZE],
    /// 已写入索引 (单调递增, 不重用)
    next_index: AtomicU32,
    /// 当前环形写入位置
    write_pos: AtomicU32,
    /// 已丢弃数量 (覆盖时)
    dropped: AtomicU64,
    /// 最近 hash
    last_hash: AtomicU64,
    /// 事件总数 (含被覆盖的)
    total: AtomicU64,
}

pub const AUDIT_BUFFER_SIZE: usize = 1024;

impl AuditLog {
    #[expect(
        clippy::large_stack_arrays,
        reason = "large_stack_arrays: 大栈数组是性能权衡 (避免堆分配); 当前优先 expect"
    )]
    pub const fn new() -> Self {
        const EMPTY_NODE: HashChainNode = HashChainNode {
            index: 0,
            event: AuditEvent {
                kind: AuditEventKind::Custom(0),
                tick: 0,
                pwm: 0,
                session_id: 0,
                domain_id: 0,
                bits: 0,
                result: 0,
            },
            prev_hash: 0,
            hash: 0,
        };
        Self {
            buffer: [EMPTY_NODE; AUDIT_BUFFER_SIZE],
            next_index: AtomicU32::new(0),
            write_pos: AtomicU32::new(0),
            dropped: AtomicU64::new(0),
            last_hash: AtomicU64::new(0),
            total: AtomicU64::new(0),
        }
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 追加事件
    ///
    /// # Errors
    /// 当追加失败 (如审计缓冲区不可用或校验失败) 时返回 `Err(AuditError)`.
    pub fn append(&mut self, event: AuditEvent) -> Result<u32, AuditError> {
        let idx = self.next_index.fetch_add(1, Ordering::AcqRel);
        let pos = self.write_pos.load(Ordering::Acquire) as usize;
        if self.buffer[pos].index != 0
            && self.buffer[pos].index < idx
            && self.next_index.load(Ordering::Acquire) as usize > AUDIT_BUFFER_SIZE
        {
            // 覆盖位置, 计数
            self.dropped.fetch_add(1, Ordering::AcqRel);
        }

        let prev = self.last_hash.load(Ordering::Acquire);
        let hash = compute_hash(prev, &event);
        let node = HashChainNode {
            index: idx,
            event,
            prev_hash: prev,
            hash,
        };
        self.buffer[pos] = node;
        self.write_pos
            .store(((pos + 1) % AUDIT_BUFFER_SIZE) as u32, Ordering::Release);
        self.last_hash.store(hash, Ordering::Release);
        self.total.fetch_add(1, Ordering::AcqRel);
        Ok(idx)
    }

    /// 按 index 查找 (包括已被覆盖的, 需 index < `next_index`)
    pub fn get(&self, index: u32) -> Option<&HashChainNode> {
        if index >= self.next_index.load(Ordering::Acquire) {
            return None;
        }
        for n in &self.buffer {
            if n.index == index {
                return Some(n);
            }
        }
        None
    }

    /// 验证哈希链完整性
    ///
    /// 返回: (ok, 第一个被破坏的 index)
    ///
    /// 只校验仍保留在环形缓冲内的区间 `[next - BUFFER_SIZE, next)`;
    /// 更早的条目已被覆盖, 其前驱不在缓冲中, 无法参与链校验.
    /// 原实现遍历整个 buffer 收集节点, 未写槽位 (`EMPTY_NODE`, index=0, hash=0)
    /// 会覆盖 `nodes[0]` 的真实节点, 导致 verify 恒返回 false (B-8; 2026-09-24 UT-06 修复).
    pub fn verify(&self) -> (bool, Option<u32>) {
        let next = self.next_index.load(Ordering::Acquire);
        // 环形缓冲中仍保留的最早 index
        let start = next.saturating_sub(AUDIT_BUFFER_SIZE as u32);
        // 首个可校验节点无前驱作参照, 故 prev 用 Option 表示"尚无期望前驱哈希"
        let mut prev: Option<u64> = None;
        for i in start..next {
            let slot = (i as usize) % AUDIT_BUFFER_SIZE;
            let n = &self.buffer[slot];
            // 槽位内容必须确为 index i (被同槽更新条目替换 / 未写则跳过)
            if n.index != i {
                continue;
            }
            if let Some(prev_hash) = prev {
                if n.prev_hash != prev_hash {
                    return (false, Some(n.index));
                }
            }
            let expected = compute_hash(n.prev_hash, &n.event);
            if expected != n.hash {
                return (false, Some(n.index));
            }
            prev = Some(n.hash);
        }
        (true, None)
    }

    /// 查询某 PWM 的事件
    pub fn query_pwm(&self, pwm: u64) -> [Option<HashChainNode>; 32] {
        let mut out: [Option<HashChainNode>; 32] = [None; 32];
        let mut idx = 0;
        for n in &self.buffer {
            if idx >= 32 {
                break;
            }
            if n.event.pwm == pwm {
                out[idx] = Some(*n);
                idx += 1;
            }
        }
        out
    }

    /// 按类型查询
    pub fn query_kind(&self, kind: AuditEventKind) -> [Option<HashChainNode>; 32] {
        let mut out: [Option<HashChainNode>; 32] = [None; 32];
        let mut idx = 0;
        for n in &self.buffer {
            if idx >= 32 {
                break;
            }
            if n.event.kind == kind {
                out[idx] = Some(*n);
                idx += 1;
            }
        }
        out
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Acquire)
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Acquire)
    }
}

/// 审计错误 — TD-20: 收敛到 `KernelError`, 1 字段 audit 特有 + 1 共享包装 (预留).
///
/// 字段说明:
///   - `Full`: 审计缓冲区已满 (理论上环形不会发生, 但保留)
///   - `Kernel(KernelError)`: 共享错误 (预留扩展, 当前未使用)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditError {
    /// 缓冲区已满 (应该不会发生, 环形)
    Full,
    /// 共享 `KernelError` 包装
    Kernel(crate::functions::error::KernelError),
}

impl AuditError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::Full => E::ENOSPC,
            Self::Kernel(e) => e.as_errno(),
        }
    }
}

use crate::privileged::syscall::Errno;

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// FNV-1a 64 位哈希 (functions 层, 用于审计链)
fn compute_hash(prev: u64, event: &AuditEvent) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut h = prev ^ OFFSET;
    h = (h ^ (event.tick as u64)).wrapping_mul(PRIME);
    h = (h ^ event.pwm).wrapping_mul(PRIME);
    h = (h ^ u64::from(event.session_id)).wrapping_mul(PRIME);
    h = (h ^ u64::from(event.domain_id)).wrapping_mul(PRIME);
    h = (h ^ event.bits).wrapping_mul(PRIME);
    h = (h ^ (event.result as u64)).wrapping_mul(PRIME);
    h = (h ^ u64::from(event.kind.tag())).wrapping_mul(PRIME);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_deterministic() {
        let e = AuditEvent {
            kind: AuditEventKind::LoginSuccess,
            tick: 100,
            pwm: 1,
            session_id: 42,
            domain_id: 0,
            bits: 0,
            result: 0,
        };
        let h1 = compute_hash(0, &e);
        let h2 = compute_hash(0, &e);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_changes_with_prev() {
        let e = AuditEvent {
            kind: AuditEventKind::LoginSuccess,
            tick: 100,
            pwm: 1,
            session_id: 42,
            domain_id: 0,
            bits: 0,
            result: 0,
        };
        let h1 = compute_hash(0, &e);
        let h2 = compute_hash(12345, &e);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_changes_with_event() {
        let e1 = AuditEvent {
            kind: AuditEventKind::LoginSuccess,
            tick: 100,
            pwm: 1,
            session_id: 42,
            domain_id: 0,
            bits: 0,
            result: 0,
        };
        let e2 = AuditEvent {
            kind: AuditEventKind::LoginFailed,
            ..e1
        };
        assert_ne!(compute_hash(0, &e1), compute_hash(0, &e2));
    }

    #[test]
    fn append_and_get() {
        let mut log = AuditLog::new();
        let e = AuditEvent {
            kind: AuditEventKind::LoginSuccess,
            tick: 100,
            pwm: 1,
            session_id: 42,
            domain_id: 0,
            bits: 0,
            result: 0,
        };
        let idx = log.append(e).unwrap();
        let got = log.get(idx).unwrap();
        assert_eq!(got.event, e);
        assert_eq!(got.index, idx);
        assert_eq!(got.prev_hash, 0);
        assert_ne!(got.hash, 0);
    }

    #[test]
    fn verify_chain_ok() {
        let mut log = AuditLog::new();
        for i in 0..10 {
            log.append(AuditEvent {
                kind: AuditEventKind::LoginSuccess,
                tick: i,
                pwm: i,
                session_id: 0,
                domain_id: 0,
                bits: 0,
                result: 0,
            })
            .unwrap();
        }
        let (ok, bad) = log.verify();
        assert!(ok, "expected ok, got bad={:?}", bad);
    }

    #[test]
    fn verify_chain_detects_tamper() {
        let mut log = AuditLog::new();
        for i in 0..5 {
            log.append(AuditEvent {
                kind: AuditEventKind::LoginSuccess,
                tick: i,
                pwm: i,
                session_id: 0,
                domain_id: 0,
                bits: 0,
                result: 0,
            })
            .unwrap();
        }
        // 篡改: 直接修改 buffer 中第 2 个节点
        log.buffer[2].event.bits = 0xDEADBEEF;
        let (ok, _) = log.verify();
        assert!(!ok);
    }

    #[test]
    fn query_pwm() {
        let mut log = AuditLog::new();
        log.append(AuditEvent {
            kind: AuditEventKind::LoginSuccess,
            tick: 1,
            pwm: 7,
            session_id: 0,
            domain_id: 0,
            bits: 0,
            result: 0,
        })
        .unwrap();
        log.append(AuditEvent {
            kind: AuditEventKind::LoginFailed,
            tick: 2,
            pwm: 8,
            session_id: 0,
            domain_id: 0,
            bits: 0,
            result: 0,
        })
        .unwrap();
        log.append(AuditEvent {
            kind: AuditEventKind::Logout,
            tick: 3,
            pwm: 7,
            session_id: 0,
            domain_id: 0,
            bits: 0,
            result: 0,
        })
        .unwrap();
        let q = log.query_pwm(7);
        // 前两个应是 pwm=7 的事件
        assert!(q[0].is_some());
        assert_eq!(q[0].unwrap().event.tick, 1);
        assert_eq!(q[1].unwrap().event.tick, 3);
    }

    #[test]
    fn query_kind() {
        let mut log = AuditLog::new();
        log.append(AuditEvent {
            kind: AuditEventKind::LoginSuccess,
            tick: 1,
            pwm: 1,
            session_id: 0,
            domain_id: 0,
            bits: 0,
            result: 0,
        })
        .unwrap();
        log.append(AuditEvent {
            kind: AuditEventKind::LoginFailed,
            tick: 2,
            pwm: 2,
            session_id: 0,
            domain_id: 0,
            bits: 0,
            result: 0,
        })
        .unwrap();
        let q = log.query_kind(AuditEventKind::LoginSuccess);
        assert!(q[0].is_some());
        assert_eq!(q[0].unwrap().event.pwm, 1);
    }

    #[test]
    fn total_count() {
        let mut log = AuditLog::new();
        assert_eq!(log.total(), 0);
        log.append(AuditEvent {
            kind: AuditEventKind::LoginSuccess,
            tick: 1,
            pwm: 1,
            session_id: 0,
            domain_id: 0,
            bits: 0,
            result: 0,
        })
        .unwrap();
        log.append(AuditEvent {
            kind: AuditEventKind::Logout,
            tick: 2,
            pwm: 1,
            session_id: 0,
            domain_id: 0,
            bits: 0,
            result: 0,
        })
        .unwrap();
        assert_eq!(log.total(), 2);
    }
}
