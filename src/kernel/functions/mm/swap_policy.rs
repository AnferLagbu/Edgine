#![deny(unsafe_code)]
//! Swap 策略 — functions 层
//!
//! ## 框架责任分离
//!
//! - **privileged**: swap 区 I/O, slot 分配/释放, PTE 操作, 页面换出/换入 (机制)
//! - **functions** (本模块): LRU 管理决策, 回收决策, kswapd 触发策略 (策略)
//!
//! ## 策略表
//!
//! | 策略 | 默认行为 | 可调参数 |
//! |------|---------|---------|
//! | reclaim_batch_size | 8 页/次 | 批量大小 |
//! | should_wakeup_kswapd | free<10% 或 swap>80% | 阈值 |
//! | should_demote_active | active >= capacity | 容量 |
//! | should_evict_inactive | inactive >= capacity | 容量 |
//! | select_victim | 首个非 locked 条目 | 选择算法 |
//!
//! ## 关联
//!
//! - T2-4: Swap 策略完整迁移 (2026-06-19)
//! - 互补: pmm_trait::PmmPolicy (物理页分配策略)

use crate::privileged::mm::swap_trait::{LruPageInfo, SwapPolicy, SwapPolicyContext};

// ============================================================================
// 默认 Swap 策略 — 标准 swap 行为
// ============================================================================

/// 默认 Swap 策略 — 与原 privileged/mm/swap.rs 硬编码行为一致
///
/// 在 `functions::mm::init()` 中通过 `register_swap_policy()` 注册.
pub struct DefaultSwapPolicy;

impl SwapPolicy for DefaultSwapPolicy {
    /// kswapd 每次唤醒回收 8 页
    fn reclaim_batch_size(&self, _ctx: SwapPolicyContext) -> u32 {
        8
    }

    /// 唤醒条件: 空闲页 < 10% 或 swap 使用率 > 80%
    fn should_wakeup_kswapd(&self, ctx: SwapPolicyContext) -> bool {
        // 整数比较 (无浮点): free/total < 1/10 或 used/total > 8/10
        let free_low = ctx.total_pages > 0 && ctx.free_pages * 10 < ctx.total_pages;
        let swap_high = ctx.total_slots > 0 && ctx.used_slots * 10 > ctx.total_slots * 8;
        free_low || swap_high
    }

    /// active 链表满时降级最旧条目
    fn should_demote_active(&self, active_count: usize, capacity: usize) -> bool {
        active_count >= capacity
    }

    /// inactive 链表满时丢弃最旧非锁定条目
    fn should_evict_inactive(&self, inactive_count: usize, capacity: usize) -> bool {
        inactive_count >= capacity
    }

    /// 选择第一个非 locked 的 inactive 条目作为回收候选
    fn select_victim(&self, entries: &[Option<LruPageInfo>]) -> Option<usize> {
        for (i, entry) in entries.iter().enumerate() {
            if let Some(e) = entry {
                if !e.locked {
                    return Some(i);
                }
            }
        }
        None
    }
}

/// 注册默认 Swap 策略到 privileged
///
/// 由 `functions::mm::init()` 调用. 只能注册一次.
///
/// # Errors
///
/// 当 Swap 策略已被注册时返回 `Err(())`.
pub fn register_default_swap_policy() -> Result<(), ()> {
    static POLICY: DefaultSwapPolicy = DefaultSwapPolicy;
    crate::privileged::mm::register_swap_policy(&POLICY).map_err(|_| ())
}

// ============================================================================
// 单元测试 — Swap 策略契约
// ============================================================================
//
// 验证 DefaultSwapPolicy 的 5 个核心方法:
// - reclaim_batch_size: 固定 8
// - should_wakeup_kswapd: free<10% 或 swap>80%
// - should_demote_active / should_evict_inactive: 容量满时
// - select_victim: 首个非 locked 条目

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::mm::swap_trait::{LruPageInfo, SwapPolicyContext};
    use alloc::vec;
    use alloc::vec::Vec;

    fn make_ctx(
        free_pages: u64,
        total_pages: u64,
        used_slots: u64,
        total_slots: u64,
    ) -> SwapPolicyContext {
        SwapPolicyContext {
            total_slots,
            used_slots,
            active_count: 0,
            inactive_count: 0,
            free_pages,
            total_pages,
        }
    }

    fn make_entry(pml4: u64, locked: bool) -> Option<LruPageInfo> {
        Some(LruPageInfo {
            pml4,
            virt_addr: 0x1000,
            phys_addr: 0x2000,
            dirty: false,
            locked,
        })
    }

    /// 1. reclaim_batch_size: 固定 8
    #[test]
    fn test_swap_reclaim_batch_size() {
        let policy = DefaultSwapPolicy;
        assert_eq!(policy.reclaim_batch_size(make_ctx(0, 0, 0, 0)), 8);
        assert_eq!(policy.reclaim_batch_size(make_ctx(100, 1000, 50, 1000)), 8);
    }

    /// 2. should_wakeup_kswapd: free<10% 或 swap>80%
    #[test]
    fn test_swap_should_wakeup_kswapd() {
        let policy = DefaultSwapPolicy;
        // 空闲充分, swap 没用 → 不唤醒
        let ctx = make_ctx(500, 1000, 0, 1000);
        assert!(!policy.should_wakeup_kswapd(ctx));
        // 空闲 9% (free/total=90/1000) < 10% → 唤醒
        let ctx = make_ctx(90, 1000, 0, 1000);
        assert!(policy.should_wakeup_kswapd(ctx));
        // 空闲 10% (恰好 100/1000) → 不唤醒 (边界)
        let ctx = make_ctx(100, 1000, 0, 1000);
        assert!(!policy.should_wakeup_kswapd(ctx));
        // 空闲 5%, swap 0% → 唤醒 (free 触发)
        let ctx = make_ctx(50, 1000, 0, 1000);
        assert!(policy.should_wakeup_kswapd(ctx));
        // 空闲 50%, swap 81% → 唤醒 (swap 触发)
        let ctx = make_ctx(500, 1000, 810, 1000);
        assert!(policy.should_wakeup_kswapd(ctx));
        // 空闲 50%, swap 80% (恰好) → 不唤醒 (边界)
        let ctx = make_ctx(500, 1000, 800, 1000);
        assert!(!policy.should_wakeup_kswapd(ctx));
        // total=0 → free_ratio=1.0, swap_usage=0.0 → 不唤醒 (避免除零)
        let ctx = make_ctx(0, 0, 0, 0);
        assert!(!policy.should_wakeup_kswapd(ctx));
    }

    /// 3. should_demote_active: 容量满时
    #[test]
    fn test_swap_should_demote_active() {
        let policy = DefaultSwapPolicy;
        // 0/100 → false
        assert!(!policy.should_demote_active(0, 100));
        // 99/100 → false
        assert!(!policy.should_demote_active(99, 100));
        // 100/100 → true (容量满)
        assert!(policy.should_demote_active(100, 100));
        // 150/100 → true (超过容量)
        assert!(policy.should_demote_active(150, 100));
        // capacity=0 → active_count >= 0 永远 true (异常但安全)
        assert!(policy.should_demote_active(0, 0));
    }

    /// 4. should_evict_inactive: 同 demote 逻辑
    #[test]
    fn test_swap_should_evict_inactive() {
        let policy = DefaultSwapPolicy;
        assert!(!policy.should_evict_inactive(50, 100));
        assert!(policy.should_evict_inactive(100, 100));
        assert!(policy.should_evict_inactive(101, 100));
    }

    /// 5. select_victim: 首个非 locked 条目
    #[test]
    fn test_swap_select_victim() {
        let policy = DefaultSwapPolicy;
        // 全空 → None
        let entries: Vec<Option<LruPageInfo>> = vec![None, None, None];
        assert_eq!(policy.select_victim(&entries), None);
        // 首个 unlocked → 0
        let entries = vec![
            make_entry(1, false),
            make_entry(2, true),
            make_entry(3, true),
        ];
        assert_eq!(policy.select_victim(&entries), Some(0));
        // 首个 locked, 第二个 unlocked → 1
        let entries = vec![
            make_entry(1, true),
            make_entry(2, false),
            make_entry(3, true),
        ];
        assert_eq!(policy.select_victim(&entries), Some(1));
        // 全 locked → None
        let entries = vec![
            make_entry(1, true),
            make_entry(2, true),
            make_entry(3, true),
        ];
        assert_eq!(policy.select_victim(&entries), None);
        // 混合 None + locked + unlocked
        let entries = vec![
            None,
            make_entry(1, true),
            None,
            make_entry(2, false),
            make_entry(3, true),
        ];
        assert_eq!(policy.select_victim(&entries), Some(3));
    }

    /// 6. integration: 内存压力场景
    #[test]
    fn test_swap_memory_pressure() {
        let policy = DefaultSwapPolicy;
        // 模拟 OOM: 0 空闲页, 0 总页 (除零保护)
        let ctx = make_ctx(0, 0, 0, 0);
        assert!(
            !policy.should_wakeup_kswapd(ctx),
            "total=0 时不应唤醒 (除零保护)"
        );

        // 模拟内存不足: 1% 空闲
        let ctx = make_ctx(10, 1000, 0, 1000);
        assert!(policy.should_wakeup_kswapd(ctx), "1% 空闲应唤醒 kswapd");

        // LRU 满 → 应驱逐
        assert!(policy.should_evict_inactive(100, 100));
        assert!(policy.should_demote_active(100, 100));
    }
}
