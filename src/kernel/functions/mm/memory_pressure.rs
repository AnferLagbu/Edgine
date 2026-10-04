#![deny(unsafe_code)]
//! 内存压力策略 (Memory Pressure) — functions 层
//!
//! ## 框架责任分离 (DECISION-O ② 2026-09-13)
//!
//! - **privileged** (`privileged::mm::pressure`): MemoryPressure 类型, 压力状态
//!   (CURRENT/PREV), `update_pressure` 包装, 分级策略注册口
//! - **functions** (本模块): 分级阈值, 级别判定算法 (`classify_pressure`),
//!   压力感知分配策略 (`PressureAwareAllocPolicy`)
//!
//! ## 策略表 (与 Linux mempressure 对照)
//!
//! | 级别 | 阈值 (free_pages 绝对值) | 阈值 (free_ratio 百分比) | 动作 |
//! |------|------|------|------|
//! | Normal | >256 | >25% | 正常运行 |
//! | Warning | 64-256 | 10-25% | 通知进程释放 page cache |
//! | Critical | 16-64 | 3-10% | 阻塞新 mmap, 降 RSS Top-3 优先级 |
//! | Emergency | ≤16 | ≤3% | SIGTERM → 最大 RSS 进程, 5s → SIGKILL |
//!
//! ## 与 Linux 差异
//!
//! - Linux cgroup-v2 memory.pressure: usec 粒度, 无固定阈值表
//! - 本项目: 嵌入式轻量基线, 4 级 + 绝对/相对双阈值
//! - 阈值可通过 `set_thresholds` 动态调整 (但必须保持 warn > crit > emer)
//!
//! ## 关联
//!
//! - 移出: privileged::mm::pressure (2026-06-11, P1-I-01 D9) → DECISION-O ②
//!   反转归位 (2026-09-13); 类型/状态/读取/包装经下方 re-export 保持 API 兼容
//!   (functions→privileged 合法方向)

pub use crate::privileged::mm::pressure::{
    MemoryPressure, current_pressure, previous_pressure, update_pressure,
};

use core::sync::atomic::{AtomicU64, Ordering};

// ============================================================================
// 阈值 (可调, 默认保守值适合 256MB 内嵌)
// ============================================================================

static FREE_PAGES_THRESHOLD_WARNING: AtomicU64 = AtomicU64::new(256);
static FREE_PAGES_THRESHOLD_CRITICAL: AtomicU64 = AtomicU64::new(64);
static FREE_PAGES_THRESHOLD_EMERGENCY: AtomicU64 = AtomicU64::new(16);

/// 设置阈值. 仅当 `warning > critical > emergency` 时生效 (配置合法性).
pub fn set_thresholds(warning: u64, critical: u64, emergency: u64) {
    if warning > critical && critical > emergency {
        FREE_PAGES_THRESHOLD_WARNING.store(warning, Ordering::Release);
        FREE_PAGES_THRESHOLD_CRITICAL.store(critical, Ordering::Release);
        FREE_PAGES_THRESHOLD_EMERGENCY.store(emergency, Ordering::Release);
    }
}

/// 分级判定 — 纯策略函数 (注册进 `privileged::mm::pressure`, 由 OOMD tick 经
/// `update_pressure` 间接调用; 状态交换由 privileged 完成)
///
/// 策略: 4 级双阈值状态机 (绝对值 + 百分比).
fn classify_pressure(free_pages: u64, total_pages: u64) -> MemoryPressure {
    let ratio = if total_pages > 0 {
        free_pages * 100 / total_pages
    } else {
        100
    };

    let (thr_warn, thr_crit, thr_emer) = (
        FREE_PAGES_THRESHOLD_WARNING.load(Ordering::Acquire),
        FREE_PAGES_THRESHOLD_CRITICAL.load(Ordering::Acquire),
        FREE_PAGES_THRESHOLD_EMERGENCY.load(Ordering::Acquire),
    );

    if free_pages <= thr_emer || ratio <= 3 {
        MemoryPressure::Emergency
    } else if free_pages <= thr_crit || ratio <= 10 {
        MemoryPressure::Critical
    } else if free_pages <= thr_warn || ratio <= 25 {
        MemoryPressure::Warning
    } else {
        MemoryPressure::Normal
    }
}

/// 注册分级策略到 privileged (由 `functions::mm::init` 调用). 只能注册一次.
///
/// # Errors
///
/// 当分级策略已被注册时返回 `Err(())`.
pub fn register_pressure_classifier() -> Result<(), ()> {
    crate::privileged::mm::pressure::register_pressure_classifier(classify_pressure).map_err(|_| ())
}

pub fn is_pressure_critical() -> bool {
    current_pressure().is_critical()
}

pub fn is_pressure_emergency() -> bool {
    current_pressure().is_emergency()
}

// ============================================================================
// T-02: 压力感知分配策略 — functions 层策略主体
// ============================================================================

use crate::privileged::mm::alloc_trait::{AllocContext, AllocDecision, FrameAllocDecision};

/// 压力感知分配策略 — functions 层安全实现
///
/// 根据内存压力级别决定是否允许分配:
/// - Normal/Warning: 允许
/// - Critical: 仅允许小分配 (< 4 页)
/// - Emergency: 拒绝所有分配, 建议回收后重试
///
/// 在 `functions::mm::init()` 中通过 `register_alloc_decision()` 注册.
pub struct PressureAwareAllocPolicy;

impl FrameAllocDecision for PressureAwareAllocPolicy {
    fn decide_alloc(&self, ctx: AllocContext) -> AllocDecision {
        let pressure = current_pressure();
        match pressure {
            MemoryPressure::Normal | MemoryPressure::Warning => AllocDecision::Allow,
            MemoryPressure::Critical => {
                // 严重压力下仅允许小分配
                if ctx.requested_pages <= 4 {
                    AllocDecision::Allow
                } else {
                    AllocDecision::Deny
                }
            }
            MemoryPressure::Emergency => AllocDecision::RetryAfterReclaim,
        }
    }

    fn on_alloc_failed(&self, ctx: AllocContext) -> AllocDecision {
        let pressure = current_pressure();
        match pressure {
            MemoryPressure::Emergency => AllocDecision::RetryAfterReclaim,
            _ => {
                // 非紧急压力下分配失败, 建议回收后重试一次
                if ctx.free_pages < ctx.requested_pages as u64 {
                    AllocDecision::RetryAfterReclaim
                } else {
                    AllocDecision::Deny
                }
            }
        }
    }

    fn select_numa_node(&self, _ctx: AllocContext) -> Option<u8> {
        // 当前单节点系统, 不做 NUMA 选择
        None
    }
}

/// 注册压力感知分配策略到 privileged
///
/// 由 `functions::mm::init()` 调用. 只能注册一次.
///
/// # Errors
///
/// 当分配策略已被注册时返回 `Err(())`.
pub fn register_pressure_aware_policy() -> Result<(), ()> {
    static POLICY: PressureAwareAllocPolicy = PressureAwareAllocPolicy;
    crate::privileged::mm::register_alloc_decision(&POLICY).map_err(|_| ())
}
