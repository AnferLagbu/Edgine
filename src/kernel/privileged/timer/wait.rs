//! 有界等待原语 (墙钟截止期)
//!
//! 驱动轮询的旧形态普遍是 `while timeout > 0 { ...; timeout -= 1 }`: 预算是
//! **迭代次数**, 每轮实际耗时随设备、主频、编译器优化浮动 —— 同一个上限在不同
//! 机器/负载下可能对应几毫秒, 也可能对应几十秒, 既可能把慢设备误判为超时,
//! 也可能让"有上限"形同虚设.
//!
//! 本模块把预算改写成**墙钟时长**, 与 Linux `read_poll_timeout` 同一立场:
//! 每轮读 [`tick::get_uptime_ns`] (硬件时基建立后由自由计数器派生, 自旋期间不
//! 依赖中断到达), 到期即判超时.
//!
//! 硬件时基未建立 (计数器频率未知) 时**回退为调用方原有的迭代上限** ——
//! 与 Linux `read_poll_timeout_atomic` 在 timekeeping 不可用时放弃时间维度、
//! 只按延时累计降级同构: 宁可等完旧上限, 也不编造频率去折算时长.
//!
//! 本原语是**忙等待**: 不睡眠、不调度, 供启动期/中断上下文/不可调度上下文使用;
//! 需要让出 CPU 的等待请用 [`super::sleep::wait_with_timeout`] (调度器路径).

use super::tick;

/// 墙钟截止期
///
/// 供自带循环体的轮询使用 (每轮需要硬件延时或复杂状态机, 收不进闭包):
///
/// ```rust,no_run
/// let mut deadline = Deadline::after_ms(100, 100_000);
/// loop {
///     if device_ready() { break; }
///     if deadline.expired() { return Err(Timeout); }
///     device_delay();
/// }
/// ```
///
/// `fallback_iters` 是硬件时基未建立时的迭代上限 (旧行为); 时基可用时不参与判定.
pub struct Deadline {
    start_ns: u64,
    budget_ns: u64,
    fallback_iters: u32,
    iters: u32,
    time_based: bool,
}

impl Deadline {
    /// 以纳秒预算构造 (时基是否可用在构造时快照)
    ///
    /// `budget_ns == 0` 表示非阻塞: 第一次 [`Self::expired`] 即判到期.
    #[must_use]
    pub fn after_ns(budget_ns: u64, fallback_iters: u32) -> Self {
        Self::with_mode(tick::timebase_is_hardware(), budget_ns, fallback_iters)
    }

    /// 以毫秒预算构造 (换算饱和, 极大值不会 panic)
    #[must_use]
    pub fn after_ms(budget_ms: u64, fallback_iters: u32) -> Self {
        Self::after_ns(budget_ms.saturating_mul(1_000_000), fallback_iters)
    }

    /// 记一轮轮询并判定是否到期
    ///
    /// 墙钟模式读当前时基; 回退模式按调用次数与 `fallback_iters` 比较.
    /// 同一次等待中每轮只应调用一次 (回退模式的计数即由此推进).
    pub fn expired(&mut self) -> bool {
        if !self.time_based {
            self.iters = self.iters.saturating_add(1);
            return self.iters >= self.fallback_iters.max(1);
        }
        self.expired_at(tick::get_uptime_ns())
    }

    fn with_mode(time_based: bool, budget_ns: u64, fallback_iters: u32) -> Self {
        Self {
            start_ns: if time_based { tick::get_uptime_ns() } else { 0 },
            budget_ns,
            fallback_iters,
            iters: 0,
            time_based,
        }
    }

    fn expired_at(&self, now_ns: u64) -> bool {
        now_ns.saturating_sub(self.start_ns) >= self.budget_ns
    }
}

/// 在墙钟预算内轮询条件, 满足返回 `true`, 到期返回 `false`
///
/// 语义与 Linux `read_poll_timeout` 对齐:
/// - 条件先于截止期判定 (设备在最后一次轮询就绪不会被误判);
/// - 判到期后再读一次条件 (恰在截止瞬间就绪同样算成功);
/// - `budget_ns == 0` 为非阻塞单次探测.
///
/// 硬件时基未建立时按 `fallback_iters` 轮回退 (旧的迭代上限语义).
#[must_use]
pub fn poll_until_ns<F>(budget_ns: u64, fallback_iters: u32, cond: F) -> bool
where
    F: FnMut() -> bool,
{
    poll_until(
        tick::timebase_is_hardware(),
        tick::get_uptime_ns,
        budget_ns,
        fallback_iters,
        cond,
    )
}

/// 毫秒版 [`poll_until_ns`] (换算饱和)
#[must_use]
pub fn poll_until_ms<F>(budget_ms: u64, fallback_iters: u32, cond: F) -> bool
where
    F: FnMut() -> bool,
{
    poll_until_ns(budget_ms.saturating_mul(1_000_000), fallback_iters, cond)
}

/// 时钟注入核心: `time_based` 与 `clock` 由调用方给定, 便于 host 单测
/// 在不触碰全局时基的前提下覆盖两条路径.
fn poll_until<C, F>(
    time_based: bool,
    mut clock: C,
    budget_ns: u64,
    fallback_iters: u32,
    mut cond: F,
) -> bool
where
    C: FnMut() -> u64,
    F: FnMut() -> bool,
{
    if budget_ns == 0 {
        return cond();
    }

    if time_based {
        let start = clock();
        loop {
            if cond() {
                return true;
            }
            if clock().saturating_sub(start) >= budget_ns {
                // Linux 同款: 到期后再读一次, 截止瞬间就绪的设备不被误判超时.
                return cond();
            }
            core::hint::spin_loop();
        }
    }

    // 回退模式: 每轮一次条件读, 轮数到达旧上限后做最后一次重读.
    let rounds = fallback_iters.max(1);
    for round in 0..rounds {
        if cond() {
            return true;
        }
        if round + 1 < rounds {
            core::hint::spin_loop();
        }
    }
    cond()
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn test_zero_budget_is_single_probe() {
        let calls = Cell::new(0u32);
        let hit = poll_until(
            false,
            || 0,
            0,
            1_000,
            || {
                calls.set(calls.get() + 1);
                false
            },
        );
        assert!(!hit);
        assert_eq!(calls.get(), 1, "零预算必须只探测一次");

        let hit = poll_until(false, || 0, 0, 1_000, || true);
        assert!(hit);
    }

    #[test]
    fn test_fallback_mode_bounded_by_iterations() {
        let calls = Cell::new(0u32);
        let hit = poll_until(
            false,
            || 0,
            u64::MAX,
            7,
            || {
                calls.set(calls.get() + 1);
                false
            },
        );
        assert!(!hit);
        assert_eq!(calls.get(), 8, "回退模式 = 7 轮 + 到期最后重读一次");
    }

    #[test]
    fn test_fallback_mode_succeeds_before_cap() {
        let calls = Cell::new(0u32);
        let hit = poll_until(
            false,
            || 0,
            u64::MAX,
            100,
            || {
                calls.set(calls.get() + 1);
                calls.get() == 3
            },
        );
        assert!(hit);
        assert_eq!(calls.get(), 3, "条件满足即返回, 不耗完迭代上限");
    }

    #[test]
    fn test_time_mode_uses_wall_clock_not_iteration_cap() {
        // 每次读钟 +1 ms, 条件恒 false; 预算 100 ms ⇒ 约 100 轮后到期,
        // 与 fallback_iters (此处 0) 无关.
        let now = Cell::new(0u64);
        let cond_calls = Cell::new(0u32);
        let hit = poll_until(
            true,
            || {
                now.set(now.get() + 1_000_000);
                now.get()
            },
            100_000_000,
            0,
            || {
                cond_calls.set(cond_calls.get() + 1);
                false
            },
        );
        assert!(!hit);
        assert!(
            (100..=102).contains(&cond_calls.get()),
            "墙钟预算应决定轮数, 实得 {}",
            cond_calls.get()
        );
    }

    #[test]
    fn test_time_mode_final_reread_success() {
        // 第 1 次条件读未命中且已到截止期: 第二次 (最后重读) 必须被给出,
        // 且命中即算成功 —— 截止瞬间才就绪的设备不被误判超时.
        let now = Cell::new(0u64);
        let cond_calls = Cell::new(0u32);
        let hit = poll_until(
            true,
            || {
                now.set(now.get() + 10_000_000);
                now.get()
            },
            5_000_000,
            0,
            || {
                cond_calls.set(cond_calls.get() + 1);
                cond_calls.get() >= 2
            },
        );
        assert!(hit, "到期时的最后重读命中必须返回成功");
        assert_eq!(cond_calls.get(), 2, "到期后只允许再读一次");
    }

    #[test]
    fn test_deadline_time_boundary() {
        let deadline = Deadline {
            start_ns: 5_000,
            budget_ns: 1_000,
            fallback_iters: 0,
            iters: 0,
            time_based: true,
        };
        assert!(!deadline.expired_at(5_999), "预算内不得到期");
        assert!(deadline.expired_at(6_000), "恰达预算即到期");
        assert!(deadline.expired_at(6_001), "超预算必须到期");
        // 计数器读数回退 (跨核不同步的防御) 按未到期处理, 不得提前误判.
        assert!(!deadline.expired_at(100));
    }

    #[test]
    fn test_deadline_fallback_counts_rounds() {
        let mut deadline = Deadline {
            start_ns: 0,
            budget_ns: 1_000_000,
            fallback_iters: 3,
            iters: 0,
            time_based: false,
        };
        assert!(!deadline.expired());
        assert!(!deadline.expired());
        assert!(deadline.expired(), "第三轮到达迭代上限");
    }
}
