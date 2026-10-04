//! Swap 策略决策 trait — 策略-机制分离接口
//!
//! T2-4: Swap 内部策略 (LRU 管理/回收决策/kswapd 触发)
//! 由 functions 实现, privileged 仅保留 swap 区 I/O 和 PTE 操作机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackSwapPolicy`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_swap_policy()` 注册自己的策略实现
//!
//! ## 与 SwapArea 的关系
//!
//! - `SwapArea` (swap.rs): slot 分配/释放, 数据读写 (机制, 含 unsafe)
//! - `SwapPolicy` (本模块): LRU 管理, 回收决策, kswapd 触发 (策略)

/// Swap 策略上下文 — 传递给策略决策的只读信息
#[derive(Debug, Clone, Copy)]
pub struct SwapPolicyContext {
    /// 总 swap slot 数
    pub total_slots: u64,
    /// 已用 swap slot 数
    pub used_slots: u64,
    /// LRU active 链表条目数
    pub active_count: usize,
    /// LRU inactive 链表条目数
    pub inactive_count: usize,
    /// 当前空闲物理页数
    pub free_pages: u64,
    /// 总物理页数
    pub total_pages: u64,
}

/// LRU 页面信息 — 策略决策的输入
#[derive(Debug, Clone, Copy)]
pub struct LruPageInfo {
    /// 所属进程的 PML4 (CR3)
    pub pml4: u64,
    /// 虚拟地址
    pub virt_addr: u64,
    /// 物理地址
    pub phys_addr: u64,
    /// 是否为脏页
    pub dirty: bool,
    /// 是否被 mlock 锁定
    pub locked: bool,
}

/// Swap 策略接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait SwapPolicy: Send + Sync {
    /// kswapd 每次唤醒回收的批量大小
    ///
    /// 返回值决定 `reclaim_pages(max_count)` 中 `max_count` 的默认值.
    fn reclaim_batch_size(&self, ctx: SwapPolicyContext) -> u32;

    /// 是否应该唤醒 kswapd
    ///
    /// 基于当前内存压力和 swap 使用率决定.
    fn should_wakeup_kswapd(&self, ctx: SwapPolicyContext) -> bool;

    /// LRU 降级阈值: active 链表满时是否应降级最旧条目到 inactive
    ///
    /// `active_count` 为当前 active 链表条目数,
    /// `capacity` 为链表容量.
    fn should_demote_active(&self, active_count: usize, capacity: usize) -> bool;

    /// inactive 链表满时是否应丢弃最旧的非锁定条目
    fn should_evict_inactive(&self, inactive_count: usize, capacity: usize) -> bool;

    /// 从 inactive 链表中选择回收候选
    ///
    /// `entries` 为 inactive 链表中所有条目 (按插入顺序).
    /// 返回被选中的条目索引, 或 None 表示不回收.
    ///
    /// 默认策略: 选择第一个非 locked 条目.
    fn select_victim(&self, entries: &[Option<LruPageInfo>]) -> Option<usize>;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略 — 标准 swap 行为
///
/// 在 functions 注册策略之前, Swap 使用此策略.
/// 逻辑与原 `swap.rs` 硬编码行为一致.
pub struct FallbackSwapPolicy;

impl SwapPolicy for FallbackSwapPolicy {
    fn reclaim_batch_size(&self, _ctx: SwapPolicyContext) -> u32 {
        8 // RECLAIM_BATCH
    }

    fn should_wakeup_kswapd(&self, ctx: SwapPolicyContext) -> bool {
        // 空闲页低于总页数的 10% 或 swap 使用率超过 80% (整数比较, 无浮点)
        let free_low = ctx.total_pages > 0 && ctx.free_pages * 10 < ctx.total_pages;
        let swap_high = ctx.total_slots > 0 && ctx.used_slots * 10 > ctx.total_slots * 8;
        free_low || swap_high
    }

    fn should_demote_active(&self, active_count: usize, capacity: usize) -> bool {
        active_count >= capacity
    }

    fn should_evict_inactive(&self, inactive_count: usize, capacity: usize) -> bool {
        inactive_count >= capacity
    }

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

static FALLBACK_SWAP_POLICY: FallbackSwapPolicy = FallbackSwapPolicy;

/// 全局策略注册表 — functions 通过 `register_swap_policy` 注册
static SWAP_POLICY: crate::privileged::sync::OnceLock<&'static dyn SwapPolicy> =
    crate::privileged::sync::OnceLock::new();

/// 注册 Swap 策略 (由 `functions::mm::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_swap_policy(
    policy: &'static dyn SwapPolicy,
) -> Result<(), &'static dyn SwapPolicy> {
    match SWAP_POLICY.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的 Swap 策略 (未注册时返回内建回退)
#[inline]
pub fn current_swap_policy() -> &'static dyn SwapPolicy {
    match SWAP_POLICY.get() {
        Some(&p) => p,
        None => &FALLBACK_SWAP_POLICY,
    }
}
