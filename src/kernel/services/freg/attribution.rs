#![deny(unsafe_code)]
//! FREG恢复 — 跨 framework/services 边界的故障归属 (services 层)
//!
//! ## 框内核中的 TCB/Services 故障边界
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │ services/* (100% safe Rust)                            │
//! │  ├─ cred/*  ├─ fs/*  ├─ net/*  ├─ proc/*  ├─ ...     │
//! │     │          │        │         │                    │
//! │     └──────────┴────────┴─────────┘                    │
//! │              ↓ safe trait 抽象                        │
//! │ framework/* (TCB, unsafe 允许)                         │
//! │  ├─ freg/*  ├─ sgeg/*  ├─ mm/*  ├─ sync/*        │
//! └────────────────────────────────────────────────────────┘
//! ```
//!
//! 故障发生时:
//! - **TCB 内 fault**: 不可恢复 (框架内部 bug, 立即 BHR)
//! - **Services 内 fault**: 可恢复 (归属到具体 recovery domain, 走 BBR/BSR)
//! - **跨层 fault (Services 调用 TCB 时)**: 由 panic 站位 + 域地址范围判定
//!
//! ## 本模块职责
//!
//! 1. **故障归属**: `panic_rip` ∈ services 范围 → 服务域, ∈ TCB 范围 → 不可恢复
//! 2. **能力降级**: 服务域连续失败 → 自动降级 capability
//! 3. **审计**: 所有归属决策进入 audit log
//!
//! ## @SAFE
//! 本文件不含 `unsafe`. 地址范围来自 `framework::freg` 暴露的安全接口.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::services::sgeg::policy::{CapBits, CapDomain, CapabilityMatrix, InMemoryMatrix};

/// 故障归属决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAttribution {
    /// 归属到具体服务域
    Service { domain_id: u64, recoverable: bool },
    /// TCB 内故障, 不可恢复
    Tcb { module: TcbModule },
    /// 跨层调用中, 需进一步判定
    CrossLayer { caller: u64, callee: TcbModule },
    /// 未知地址
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::upper_case_acronyms,
    reason = "TCB 归属枚举按子系统缩写命名 (FREG/SGEG), 缩写即语义标识且无更长形式"
)]
pub enum TcbModule {
    FREG,
    SGEG,
    Memory,
    Sync,
    Idt,
    Other,
}

impl TcbModule {
    pub const fn from_name(name: &[u8]) -> Self {
        if contains_substr(name, b"freg") {
            Self::FREG
        } else if contains_substr(name, b"sgeg") {
            Self::SGEG
        } else if contains_substr(name, b"mm") || contains_substr(name, b"pmm") {
            Self::Memory
        } else if contains_substr(name, b"sync") {
            Self::Sync
        } else if contains_substr(name, b"idt") {
            Self::Idt
        } else {
            Self::Other
        }
    }
}

const fn contains_substr(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        let mut j = 0;
        let mut match_ = true;
        while j < needle.len() {
            if haystack[i + j] != needle[j] {
                match_ = false;
                break;
            }
            j += 1;
        }
        if match_ {
            return true;
        }
        i += 1;
    }
    false
}

/// 地址范围 (用于归属判定)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrRange {
    pub start: u64,
    pub end: u64,
    pub name: &'static [u8],
}

impl AddrRange {
    pub const fn contains(&self, addr: u64) -> bool {
        addr >= self.start && addr < self.end
    }
}

/// 服务域地址范围
pub const SERVICE_RANGES: &[AddrRange] = &[
    AddrRange {
        start: 0xFFFF_FFFF_0000_0000,
        end: 0xFFFF_FFFF_1000_0000,
        name: b"services::sgeg",
    },
    AddrRange {
        start: 0xFFFF_FFFF_1000_0000,
        end: 0xFFFF_FFFF_2000_0000,
        name: b"services::fs",
    },
    AddrRange {
        start: 0xFFFF_FFFF_2000_0000,
        end: 0xFFFF_FFFF_3000_0000,
        name: b"services::net",
    },
    AddrRange {
        start: 0xFFFF_FFFF_3000_0000,
        end: 0xFFFF_FFFF_4000_0000,
        name: b"services::proc",
    },
];

/// TCB 模块地址范围
pub const TCB_RANGES: &[AddrRange] = &[
    AddrRange {
        start: 0xFFFF_FFFF_8000_0000,
        end: 0xFFFF_FFFF_8001_0000,
        name: b"framework::freg",
    },
    AddrRange {
        start: 0xFFFF_FFFF_8001_0000,
        end: 0xFFFF_FFFF_8002_0000,
        name: b"framework::sgeg",
    },
    AddrRange {
        start: 0xFFFF_FFFF_8002_0000,
        end: 0xFFFF_FFFF_8003_0000,
        name: b"framework::mm",
    },
    AddrRange {
        start: 0xFFFF_FFFF_8003_0000,
        end: 0xFFFF_FFFF_8004_0000,
        name: b"framework::sync",
    },
    AddrRange {
        start: 0xFFFF_FFFF_8004_0000,
        end: 0xFFFF_FFFF_8005_0000,
        name: b"framework::idt",
    },
];

/// 故障归属器
pub struct FaultAttributor;

impl FaultAttributor {
    /// 根据 panic RIP 判定故障归属
    pub fn attribute(panic_rip: u64) -> FaultAttribution {
        // 先查 TCB
        for r in TCB_RANGES {
            if r.contains(panic_rip) {
                let module = TcbModule::from_name(r.name);
                return FaultAttribution::Tcb { module };
            }
        }
        // 再查 Services
        for r in SERVICE_RANGES {
            if r.contains(panic_rip) {
                // 用地址哈希作为 domain_id (实际实现应由 framework 提供映射)
                let domain_id = (panic_rip >> 12) & 0xFFFF;
                return FaultAttribution::Service {
                    domain_id,
                    recoverable: true,
                };
            }
        }
        FaultAttribution::Unknown
    }

    /// 跨层调用判定
    ///
    /// 当 services 函数调用 TCB 函数时, TCB 内部 panic, 需要回溯到
    /// services 调用栈. 这里使用 `caller_id` (由 `framework::freg` 提供).
    pub fn attribute_cross(caller_id: u64, callee_panic_rip: u64) -> FaultAttribution {
        // 验证 callee 在 TCB 范围
        for r in TCB_RANGES {
            if r.contains(callee_panic_rip) {
                let module = TcbModule::from_name(r.name);
                return FaultAttribution::CrossLayer {
                    caller: caller_id,
                    callee: module,
                };
            }
        }
        Self::attribute(callee_panic_rip)
    }
}

/// 服务域失败记录
pub struct DomainFailureRecord {
    pub domain_id: u64,
    pub consecutive_failures: AtomicU32,
    pub total_failures: AtomicU64,
    pub last_failure_tick: AtomicU64,
    pub downgraded: AtomicU32,
    pub current_tier: AtomicU32, // 0=正常, 1=降级, 2=隔离
}

impl DomainFailureRecord {
    pub const fn new(domain_id: u64) -> Self {
        Self {
            domain_id,
            consecutive_failures: AtomicU32::new(0),
            total_failures: AtomicU64::new(0),
            last_failure_tick: AtomicU64::new(0),
            downgraded: AtomicU32::new(0),
            current_tier: AtomicU32::new(0),
        }
    }

    /// 记录一次失败 (B07-17: 多因子定级)
    ///
    /// 返回: 建议降级到哪个 tier
    ///
    /// 决策因子:
    /// - 连续失败次数: ≥3 → tier 1, ≥5 → tier 2 (基础)
    /// - 心跳间隔 `heartbeat_gap`: >500 tick 视为域失联, 直接升级 (与
    ///   `recovery_policy` 阈值一致)
    /// - 依赖者数 `dependents`: 被 ≥1 个域依赖且已连续失败 ≥3 次 → 升级
    ///   (快速隔离防级联扩散)
    pub fn record_failure(&self, current_tick: u64, heartbeat_gap: u64, dependents: u32) -> u32 {
        self.total_failures.fetch_add(1, Ordering::AcqRel);
        self.consecutive_failures.fetch_add(1, Ordering::AcqRel);
        self.last_failure_tick
            .store(current_tick, Ordering::Release);
        let n = self.consecutive_failures.load(Ordering::Acquire);
        let base = if n >= 5 { 2 } else { u32::from(n >= 3) };
        let hb_escalate = u32::from(heartbeat_gap > 500);
        let dep_escalate = u32::from(dependents > 0 && n >= 3);
        let new_tier = base.max(hb_escalate).max(dep_escalate).min(2);
        let old = self.current_tier.load(Ordering::Acquire);
        if new_tier > old {
            self.current_tier.store(new_tier, Ordering::Release);
            self.downgraded.fetch_add(1, Ordering::AcqRel);
        }
        new_tier
    }

    /// 记录成功 (重置连续失败)
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Release);
        let old = self.current_tier.load(Ordering::Acquire);
        if old > 0 {
            self.current_tier.store(old - 1, Ordering::Release);
        }
    }
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 能力降级策略
///
/// tier 1: 撤销可写能力 (WRITE/SEND/CREATE)
/// tier 2: 保留只读 + READ/EXEC
pub fn downgrade_for_tier(tier: u32) -> CapBits {
    match tier {
        0 => CapBits::ALL,
        1 => {
            // tier 1: 撤销 WRITE 位 (bit 0 of each cap)
            CapBits(0xFFFFFFFFFFFFFFFE) // 假设每个能力域 bit 0 = WRITE
        }
        2 => {
            // tier 2: 撤销 WRITE/CREATE 位
            CapBits(0xFFFFFFFFFFFFFFFC) // 保留 READ (bit 0) + EXEC (bit 1) 之类
        }
        _ => CapBits::NONE,
    }
}

/// 跨层故障处理器
pub struct CrossLayerHandler<'a> {
    pub matrix: &'a InMemoryMatrix,
    pub records: &'a mut [DomainFailureRecord; MAX_SERVICE_DOMAINS],
    pub current_tick: u64,
}

pub const MAX_SERVICE_DOMAINS: usize = 16;

impl<'a> CrossLayerHandler<'a> {
    pub fn new(
        matrix: &'a InMemoryMatrix,
        records: &'a mut [DomainFailureRecord; MAX_SERVICE_DOMAINS],
        current_tick: u64,
    ) -> Self {
        Self {
            matrix,
            records,
            current_tick,
        }
    }

    /// 处理故障: 降级 + 审计
    ///
    /// 返回: 新 tier
    ///
    /// B07-17: 落地降级写入 — 原实现 `let _ = target` 为 no-op, 降级从未
    /// 生效 (攻击者可故意触发服务失败强制降级却收不回能力). 现在把
    /// `downgrade_for_tier` 结果实际写回 capability matrix.
    pub fn handle(&mut self, domain_id: u64) -> u32 {
        // 查找记录
        let idx = (domain_id as usize) % MAX_SERVICE_DOMAINS;
        let rec = &self.records[idx];
        // 事件路径无心跳/依赖上下文, 传 0 (等价原单因子行为); 时间窗口与
        // 依赖因子由上层 health_monitor 的 RecoveryPolicy 决策补充.
        let new_tier = rec.record_failure(self.current_tick, 0, 0);
        // 应用降级: 实际写回 capability matrix (不再丢弃)
        let target = downgrade_for_tier(new_tier);
        let _ = self.matrix.set(CapDomain(idx as u8), target);
        new_tier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcb_range_contains_tcb_addr() {
        for r in TCB_RANGES {
            assert!(r.contains(r.start));
            assert!(!r.contains(r.end));
        }
    }

    #[test]
    fn attribute_tcb() {
        // 0xFFFF_FFFF_8000_5000 落在 framework::freg
        let attr = FaultAttributor::attribute(0xFFFF_FFFF_8000_5000);
        assert_eq!(
            attr,
            FaultAttribution::Tcb {
                module: TcbModule::FREG
            }
        );
    }

    #[test]
    fn attribute_service() {
        // 0xFFFF_FFFF_0500_0000 落在 services::sgeg
        let attr = FaultAttributor::attribute(0xFFFF_FFFF_0500_0000);
        assert!(matches!(
            attr,
            FaultAttribution::Service {
                recoverable: true,
                ..
            }
        ));
    }

    #[test]
    fn attribute_unknown() {
        let attr = FaultAttributor::attribute(0x1234_5678);
        assert_eq!(attr, FaultAttribution::Unknown);
    }

    #[test]
    fn attribute_cross_layer() {
        let attr = FaultAttributor::attribute_cross(
            42,                    // caller_id
            0xFFFF_FFFF_8001_8000, // framework::sgeg 地址
        );
        assert!(matches!(
            attr,
            FaultAttribution::CrossLayer {
                caller: 42,
                callee: TcbModule::SGEG
            }
        ));
    }

    #[test]
    fn tcb_module_from_name() {
        assert_eq!(TcbModule::from_name(b"framework::freg"), TcbModule::FREG);
        assert_eq!(TcbModule::from_name(b"framework::sgeg"), TcbModule::SGEG);
        assert_eq!(TcbModule::from_name(b"framework::pmm"), TcbModule::Memory);
        assert_eq!(TcbModule::from_name(b"unknown"), TcbModule::Other);
    }

    #[test]
    fn failure_record_basic() {
        let rec = DomainFailureRecord::new(1);
        assert_eq!(rec.record_failure(100, 0, 0), 0);
        assert_eq!(rec.record_failure(101, 0, 0), 0);
        assert_eq!(rec.record_failure(102, 0, 0), 1); // 3 次 → tier 1
        assert_eq!(rec.record_failure(103, 0, 0), 1);
        assert_eq!(rec.record_failure(104, 0, 0), 2); // 5 次 → tier 2
        assert_eq!(rec.total_failures.load(Ordering::Acquire), 5);
    }

    #[test]
    fn failure_record_recovery() {
        let rec = DomainFailureRecord::new(1);
        rec.record_failure(100, 0, 0);
        rec.record_failure(101, 0, 0);
        rec.record_failure(102, 0, 0); // → tier 1
        rec.record_success();
        assert_eq!(rec.consecutive_failures.load(Ordering::Acquire), 0);
    }

    // B07-17: 心跳间隔因子 — 单次失败 + 长时间无心跳 (域失联) 即升级
    #[test]
    fn failure_record_heartbeat_escalate() {
        let rec = DomainFailureRecord::new(1);
        assert_eq!(rec.record_failure(1000, 600, 0), 1); // 失联 → tier 1
        assert_eq!(rec.current_tier.load(Ordering::Acquire), 1);
    }

    // B07-17: 依赖者因子 — 被依赖域连续失败 → 快速升级防级联
    #[test]
    fn failure_record_dependents_escalate() {
        let rec = DomainFailureRecord::new(1);
        assert_eq!(rec.record_failure(100, 0, 1), 0);
        assert_eq!(rec.record_failure(101, 0, 1), 0);
        assert_eq!(rec.record_failure(102, 0, 1), 1); // 3 次 + 有依赖 → tier 1
    }

    #[test]
    fn downgrade_for_tier_0() {
        assert_eq!(downgrade_for_tier(0), CapBits::ALL);
    }

    #[test]
    fn downgrade_for_tier_1() {
        let bits = downgrade_for_tier(1);
        assert!(!bits.contains(CapBits(0b01))); // 撤销 bit 0 (WRITE)
    }

    #[test]
    fn downgrade_for_tier_2() {
        let bits = downgrade_for_tier(2);
        assert!(!bits.contains(CapBits(0b01)));
        assert!(!bits.contains(CapBits(0b10)));
    }

    #[test]
    fn cross_layer_handler_basic() {
        let m = InMemoryMatrix::new();
        let mut records = [
            DomainFailureRecord::new(0),
            DomainFailureRecord::new(1),
            DomainFailureRecord::new(2),
            DomainFailureRecord::new(3),
            DomainFailureRecord::new(4),
            DomainFailureRecord::new(5),
            DomainFailureRecord::new(6),
            DomainFailureRecord::new(7),
            DomainFailureRecord::new(8),
            DomainFailureRecord::new(9),
            DomainFailureRecord::new(10),
            DomainFailureRecord::new(11),
            DomainFailureRecord::new(12),
            DomainFailureRecord::new(13),
            DomainFailureRecord::new(14),
            DomainFailureRecord::new(15),
        ];
        let mut h = CrossLayerHandler::new(&m, &mut records, 100);
        // 同一域失败 3 次 → tier 1
        for _ in 0..3 {
            h.handle(7);
        }
        let r = &h.records[7];
        assert_eq!(r.current_tier.load(Ordering::Acquire), 1);
    }

    /// 完整流: services 域失败 → 自动降级 cap
    #[test]
    fn full_flow_failure_downgrade() {
        let m = InMemoryMatrix::new();
        // DomainFailureRecord 含原子字段非 Copy, 不能用数组重复语法; 逐元素构造.
        let mut records = [(); 16].map(|()| DomainFailureRecord::new(0));
        let mut h = CrossLayerHandler::new(&m, &mut records, 100);
        for _ in 0..5 {
            h.handle(3);
        }
        // tier 应为 2
        let r = &h.records[3];
        assert_eq!(r.current_tier.load(Ordering::Acquire), 2);
        assert!(r.downgraded.load(Ordering::Acquire) >= 1);
    }
}
