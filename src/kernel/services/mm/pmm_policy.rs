#![deny(unsafe_code)]
//! PMM 策略 — services 层
//!
//! ## 框架责任分离
//!
//! - **framework**: buddy 位图操作、物理页分配/释放、锁操作 (机制)
//! - **services** (本模块): 阶数选择、碎片化评估、回收阈值、水位线 (策略)
//!
//! ## 策略表
//!
//! | 策略 | 默认行为 | 可调参数 |
//! |------|---------|---------|
//! | count_to_order | 向上取整到 2^n | max_order |
//! | fragmentation_score | (1-free_permille)×7/10 + fail_permille×3/10 (千分比 0..=1000) | 权重 |
//! | reclaim_threshold | max(total×10%, 64) | 百分比/最小值 |
//! | watermarks | min=1.25%×total, low=1.5×min, high=2×min | 比例系数 |
//!
//! ## 关联
//!
//! - T2-2: PMM 策略提取 (2026-06-19)
//! - 互补: alloc_trait::FrameAllocDecision (分配前决策)

use crate::framework::mm::pmm_trait::{PmmPolicy, PmmPolicyContext, Watermarks};

// ============================================================================
// 默认 PMM 策略 — 标准 buddy 分配器行为
// ============================================================================

/// 默认 PMM 策略 — 与原 framework/mm/pmm.rs 硬编码行为一致
///
/// 在 `services::mm::init()` 中通过 `register_pmm_policy()` 注册.
pub struct DefaultPmmPolicy;

impl PmmPolicy for DefaultPmmPolicy {
    /// Buddy 阶数选择: 向上取整到 2^n
    ///
    /// 阶数 = ceil(log2(count)) = floor(log2(count-1)) + 1
    /// 受 `max_order` 约束 (当前为 9, 即 2MB).
    fn count_to_order(&self, count: usize, max_order: u8) -> u8 {
        if count <= 1 {
            return 0;
        }
        let order = (count - 1).bit_width() as u8;
        if order > max_order { max_order } else { order }
    }

    /// 碎片化评估: 综合空闲比例和分配失败率
    ///
    /// 评分公式: (1 - `free_ratio`) × 0.7 + `fail_ratio` × 0.3 的千分比整数展开
    /// (返回 0..=1000, 0 = 无碎片, 1000 = 严重碎片化)
    /// - 空闲比例低 → 高碎片化 (权重 7/10)
    /// - 分配失败率高 → 高碎片化 (权重 3/10)
    fn fragmentation_score(&self, ctx: PmmPolicyContext) -> u64 {
        if ctx.total_pages == 0 {
            return 0;
        }
        let free_permille = ctx.free_pages * 1000 / ctx.total_pages;
        let fail_permille = if ctx.total_allocs > 0 {
            ctx.failed_allocs * 1000 / ctx.total_allocs
        } else {
            0
        };
        ((1000 - free_permille) * 7 + fail_permille * 3) / 10
    }

    /// 回收阈值: 当空闲页低于 max(total×10%, 64) 时触发 kswapd
    fn reclaim_threshold_pages(&self, total_pages: u64) -> u64 {
        (total_pages * 10 / 100).max(64)
    }

    /// 水位线计算: Linux 风格三级阈值
    ///
    /// - min ≈ 1.25% × total (最低 16 页)
    /// - low = 1.5 × min
    /// - high = 2 × min
    fn watermarks(&self, total_pages: u64) -> Watermarks {
        let min = (total_pages * 125 / 10000).max(16);
        let low = min * 3 / 2;
        let high = min * 2;
        Watermarks { high, low, min }
    }
}

/// 注册默认 PMM 策略到 framework
///
/// 由 `services::mm::init()` 调用. 只能注册一次.
///
/// # Errors
///
/// 当 PMM 策略已被注册时返回 `Err(())`.
pub fn register_default_pmm_policy() -> Result<(), ()> {
    static POLICY: DefaultPmmPolicy = DefaultPmmPolicy;
    crate::framework::mm::register_pmm_policy(&POLICY).map_err(|_| ())
}

// ============================================================================
// 单元测试 — PMM 策略契约
// ============================================================================
//
// 验证 DefaultPmmPolicy 的 4 个核心方法:
// - count_to_order: 阶数选择 (边界 + 0)
// - fragmentation_score: 碎片化评分 (千分比 0..=1000; 权重 7/10 与 3/10)
// - reclaim_threshold_pages: 回收阈值 (10% + 64 页最小)
// - watermarks: 三级水位线 (min/low/high 比例)
//
// 这些都是纯函数, 无状态依赖, 0 unsafe, 适合 unit test.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::mm::pmm_trait::PmmPolicyContext;

    /// 1. count_to_order: 边界 (0, 1, 2, 3, 4, 8, 16) + max_order 截断
    #[test]
    fn test_pmm_count_to_order_boundaries() {
        let policy = DefaultPmmPolicy;
        // count=0 / 1 → order 0 (最小)
        assert_eq!(policy.count_to_order(0, 9), 0);
        assert_eq!(policy.count_to_order(1, 9), 0);
        // 2 → 1, 4 → 2, 8 → 3, 16 → 4
        assert_eq!(policy.count_to_order(2, 9), 1);
        assert_eq!(policy.count_to_order(3, 9), 2); // 向上取整 (3-1=2, log2=1, +1=2)
        assert_eq!(policy.count_to_order(4, 9), 2);
        assert_eq!(policy.count_to_order(8, 9), 3);
        assert_eq!(policy.count_to_order(9, 9), 4);
        assert_eq!(policy.count_to_order(16, 9), 4);
        // 1024 = 2^10, 但 max_order=9 → 截断为 9
        // (原断言为 10, 与 max_order 截断语义矛盾; 2026-09-24 UT-06 修正)
        assert_eq!(policy.count_to_order(1024, 9), 9);
        // 截断: count 超出 max_order → 返回 max_order
        assert_eq!(policy.count_to_order(2048, 9), 9);
        assert_eq!(policy.count_to_order(1 << 20, 9), 9);
    }

    /// 2. fragmentation_score: 0% / 50% / 100% 空闲 + 0 / 50% / 100% 失败率
    #[test]
    fn test_pmm_fragmentation_score_basic() {
        let policy = DefaultPmmPolicy;
        // total=0 边界: 立即返回 0
        let ctx = PmmPolicyContext {
            total_pages: 0,
            free_pages: 0,
            total_allocs: 0,
            failed_allocs: 0,
        };
        assert_eq!(policy.fragmentation_score(ctx), 0);
        // 100% 空闲, 0 失败: (1-1)*7 + 0*3 = 0
        let ctx = PmmPolicyContext {
            total_pages: 100,
            free_pages: 100,
            total_allocs: 50,
            failed_allocs: 0,
        };
        assert_eq!(policy.fragmentation_score(ctx), 0);
        // 0% 空闲, 0 失败: (1-0)*7 + 0*3 = 700
        let ctx = PmmPolicyContext {
            total_pages: 100,
            free_pages: 0,
            total_allocs: 50,
            failed_allocs: 0,
        };
        assert_eq!(policy.fragmentation_score(ctx), 700);
        // 50% 空闲, 0 失败: (1-0.5)*7 = 350
        let ctx = PmmPolicyContext {
            total_pages: 100,
            free_pages: 50,
            total_allocs: 50,
            failed_allocs: 0,
        };
        assert_eq!(policy.fragmentation_score(ctx), 350);
    }

    /// 3. fragmentation_score: 失败率贡献 (权重 3/10)
    #[test]
    fn test_pmm_fragmentation_score_fail_ratio() {
        let policy = DefaultPmmPolicy;
        // 100% 空闲, 50% 失败: 0*7 + 500*3 = 1500, /10 = 150
        let ctx = PmmPolicyContext {
            total_pages: 100,
            free_pages: 100,
            total_allocs: 100,
            failed_allocs: 50,
        };
        assert_eq!(policy.fragmentation_score(ctx), 150);
        // 0% 空闲, 100% 失败: (1000-0)*7 + 1000*3 = 10000, /10 = 1000 (严重碎片化上限)
        let ctx = PmmPolicyContext {
            total_pages: 100,
            free_pages: 0,
            total_allocs: 100,
            failed_allocs: 100,
        };
        assert_eq!(policy.fragmentation_score(ctx), 1000);
        // total_allocs=0 时 fail_ratio=0 (避免除零)
        let ctx = PmmPolicyContext {
            total_pages: 100,
            free_pages: 0,
            total_allocs: 0,
            failed_allocs: 0,
        };
        assert_eq!(policy.fragmentation_score(ctx), 700);
    }

    /// 4. reclaim_threshold_pages: 10% 公式 + 64 页最小值
    #[test]
    fn test_pmm_reclaim_threshold() {
        let policy = DefaultPmmPolicy;
        // 小总量: 10% 不足 64, 取最小 64
        assert_eq!(policy.reclaim_threshold_pages(0), 64); // 0%*X = 0 → max(0, 64) = 64
        assert_eq!(policy.reclaim_threshold_pages(100), 64); // 10%*100=10 → max(10, 64) = 64
        assert_eq!(policy.reclaim_threshold_pages(640), 64); // 10%*640=64 → max(64, 64) = 64
        // 大总量: 10% 超过 64, 取 10%
        assert_eq!(policy.reclaim_threshold_pages(1000), 100); // 10%*1000=100
        assert_eq!(policy.reclaim_threshold_pages(10000), 1000);
        assert_eq!(policy.reclaim_threshold_pages(1 << 20), 104857); // ~104857 页 (~100 MB)
    }

    /// 5. watermarks: min/low/high 比例 + 16 页最小
    #[test]
    fn test_pmm_watermarks_basic() {
        let policy = DefaultPmmPolicy;
        // 小总量: min 受最小值 16 约束
        let w = policy.watermarks(0);
        assert_eq!(w.min, 16);
        assert_eq!(w.low, 24); // 16 * 3 / 2 = 24
        assert_eq!(w.high, 32); // 16 * 2 = 32
        // 1000 页: min = 1000 * 125 / 10000 = 12 → max(12, 16) = 16
        let w = policy.watermarks(1000);
        assert_eq!(w.min, 16);
        // 10000 页: min = 10000 * 125 / 10000 = 125
        let w = policy.watermarks(10000);
        assert_eq!(w.min, 125);
        assert_eq!(w.low, 187); // 125 * 3 / 2 = 187
        assert_eq!(w.high, 250); // 125 * 2 = 250
        // 不变量: high >= low >= min
        let w = policy.watermarks(100000);
        assert!(w.high >= w.low);
        assert!(w.low >= w.min);
    }

    /// 6. integration: 模拟 PMM 压力场景, 验证策略响应
    #[test]
    fn test_pmm_policy_under_pressure() {
        let policy = DefaultPmmPolicy;
        // 高碎片化 (0 空闲, 100% 失败) → 评分达到上限 1000
        let ctx = PmmPolicyContext {
            total_pages: 1024,
            free_pages: 0,
            total_allocs: 1000,
            failed_allocs: 1000,
        };
        let score = policy.fragmentation_score(ctx);
        assert!(score > 700, "高压力下应返回高碎片化评分 (got {})", score);
        // 回收阈值应被触发: free=0 < threshold=102 (10%*1024)
        let threshold = policy.reclaim_threshold_pages(1024);
        assert_eq!(threshold, 102);
    }
}
