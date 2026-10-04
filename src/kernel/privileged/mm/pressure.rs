//! 内存压力机制 — privileged 机制层
//!
//! ## DECISION-O ② 归属反转记录 (2026-09-13)
//!
//! MemoryPressure 类型/压力状态/读取原语于 2026-06-11 (P1-I-01 D9, TCB 减面)
//! 移至 `functions::mm::memory_pressure`; DECISION-J (2026-09-12) 删除 privileged
//! 壳后 `privileged/proc/oomd.rs` 直连 functions — 形成 privileged→functions 反向
//! 依赖。按"机制持有的状态归 privileged"判据反转：压力级别状态是 OOMD (调度器
//! tick 直接驱动的机制组件, scheduler.rs `OOMD.tick()`) 的机制状态, 类型 + 状态
//! + `update_pressure` 包装归位本模块; 分级算法 (阈值/判定) 属策略, 留 functions
//! 并经注册注入 (同 `alloc_trait` 注册模式), privileged 生产代码零 functions 引用。
//!
//! ## 设计
//!
//! - 状态: `CURRENT_PRESSURE`/`PREV_PRESSURE` 原子量, privileged 持有
//! - 分级算法: functions 注册纯函数 `fn(free_pages, total_pages) -> MemoryPressure`
//! - 未注册 fallback: 返回 Normal (functions::mm::init 之前的早期启动窗口,
//!   同 `FallbackAllocPolicy` 风格)

use core::sync::atomic::{AtomicU8, Ordering};

use crate::privileged::sync::OnceLock;

// ============================================================================
// 内存压力级别
// ============================================================================

/// 内存压力级别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MemoryPressure {
    /// 正常: 内存充足
    Normal = 0,
    /// 警告: 建议进程主动释放 page cache
    Warning = 1,
    /// 严重: 阻塞新 mmap, 降 RSS Top-3 优先级
    Critical = 2,
    /// 紧急: SIGTERM → SIGKILL 序列
    Emergency = 3,
}

impl MemoryPressure {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Warning,
            2 => Self::Critical,
            3 => Self::Emergency,
            _ => Self::Normal,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_critical(&self) -> bool {
        matches!(self, Self::Critical | Self::Emergency)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_emergency(&self) -> bool {
        matches!(self, Self::Emergency)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn description(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Emergency => "emergency",
        }
    }
}

// ============================================================================
// 压力状态 (机制持有)
// ============================================================================

static CURRENT_PRESSURE: AtomicU8 = AtomicU8::new(MemoryPressure::Normal as u8);
/// 上一次压力级别 (由 `update_pressure` 在 swap 后写入)
static PREV_PRESSURE: AtomicU8 = AtomicU8::new(MemoryPressure::Normal as u8);

/// 获取当前压力级别
pub fn current_pressure() -> MemoryPressure {
    MemoryPressure::from_u8(CURRENT_PRESSURE.load(Ordering::SeqCst))
}

/// 读取上一次压力级别 (供调用方做日志比较)
pub fn previous_pressure() -> MemoryPressure {
    MemoryPressure::from_u8(PREV_PRESSURE.load(Ordering::SeqCst))
}

// ============================================================================
// 分级策略注册点 (机制留注册口, 算法在 functions)
// ============================================================================

/// 压力分级函数签名 — 纯策略函数, 由 functions 注册
pub type PressureClassifier = fn(u64, u64) -> MemoryPressure;

static CLASSIFIER: OnceLock<PressureClassifier> = OnceLock::new();

/// 注册压力分级策略 (由 `functions::mm::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
///
/// 分级策略已被注册过时返回 `Err`.
pub fn register_pressure_classifier(f: PressureClassifier) -> Result<(), PressureClassifier> {
    CLASSIFIER.set(f)
}

/// 更新压力级别 (传入当前 `free_pages` / `total_pages`)
///
/// 分级判定委托已注册的 functions 策略; 未注册 (早期启动窗口) 时 fallback
/// Normal. 状态交换 (含 `PREV_PRESSURE` 记录) 由本函数完成, 返回新级别供
/// 调用方 (OOMD tick) 决定机制动作.
pub fn update_pressure(free_pages: u64, total_pages: u64) -> MemoryPressure {
    let new_pressure = match CLASSIFIER.get() {
        Some(&f) => f(free_pages, total_pages),
        // fallback: functions::mm::init 注册前的启动窗口, 同 FallbackAllocPolicy 风格
        None => MemoryPressure::Normal,
    };
    let prev = CURRENT_PRESSURE.swap(new_pressure as u8, Ordering::SeqCst);
    PREV_PRESSURE.store(prev, Ordering::SeqCst);
    new_pressure
}
