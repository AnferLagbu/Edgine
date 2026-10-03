//! PMM 策略决策 trait — 策略-机制分离接口
//!
//! T2-2: PMM 内部策略 (阶数选择、碎片化评估、回收阈值、水位线)
//! 由 services 实现, framework 仅保留 buddy 分配器机制.
//!
//! ## 设计
//!
//! - trait 定义在 framework (引用 framework 类型)
//! - 实现在 services (100% safe Rust, `#![deny(unsafe_code)]`)
//! - framework 提供默认回退策略 (`FallbackPmmPolicy`), 早期启动阶段使用
//! - services 在 `init()` 中通过 `register_pmm_policy()` 注册自己的策略实现
//!
//! ## 与 FrameAllocDecision 的关系
//!
//! - `FrameAllocDecision` (alloc_trait.rs): 分配前决策 (允许/拒绝/回收后重试)
//! - `PmmPolicy` (本模块): buddy 分配器内部策略 (阶数选择/碎片化/水位线)
//!
//! 两者互补, 共同构成完整的 PMM 策略面.

/// PMM 策略上下文 — 传递给策略决策的只读信息
#[derive(Debug, Clone, Copy)]
pub struct PmmPolicyContext {
    /// 当前空闲页数
    pub free_pages: u64,
    /// 总页数
    pub total_pages: u64,
    /// 累计分配失败次数
    pub failed_allocs: u64,
    /// 累计分配总次数
    pub total_allocs: u64,
}

/// 内存水位线 — 三级阈值
///
/// 参考 Linux watermark 设计:
/// - **high**: kswapd 停止回收的阈值 (空闲页高于此值时无需回收)
/// - **low**: kswapd 开始后台回收的阈值
/// - **min**: 紧急阈值 (低于此值时阻塞分配, 等待回收完成)
#[derive(Debug, Clone, Copy)]
pub struct Watermarks {
    pub high: u64,
    pub low: u64,
    pub min: u64,
}

/// PMM 策略接口 — services 实现, framework 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait PmmPolicy: Send + Sync {
    /// Buddy 阶数选择策略: 将请求页数转换为 buddy 阶数
    ///
    /// `max_order` 为 buddy 分配器支持的最大阶数 (当前为 9, 即 2MB).
    /// 策略可在此约束下自由决定阶数, 默认实现为向上取整到 2^n.
    fn count_to_order(&self, count: usize, max_order: u8) -> u8;

    /// 碎片化评估: 返回千分比 (0..=1000) 的碎片化程度
    ///
    /// 0 = 无碎片, 1000 = 严重碎片化.
    /// 评估依据: 空闲比例、分配失败率等.
    fn fragmentation_score(&self, ctx: PmmPolicyContext) -> u64;

    /// 空闲页面回收阈值: 当空闲页低于此值时触发 kswapd 回收
    fn reclaim_threshold_pages(&self, total_pages: u64) -> u64;

    /// 内存水位线计算: 返回 (high, low, min) 三级水位线
    fn watermarks(&self, total_pages: u64) -> Watermarks;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, services 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略 — 标准 buddy 分配器行为
///
/// 在 services 注册策略之前, PMM 使用此策略.
/// 逻辑与原 `pmm.rs` 硬编码行为一致.
pub struct FallbackPmmPolicy;

impl PmmPolicy for FallbackPmmPolicy {
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn count_to_order(&self, count: usize, max_order: u8) -> u8 {
        if count <= 1 {
            return 0;
        }
        let order = (count - 1).bit_width() as u8;
        if order > max_order { max_order } else { order }
    }

    fn fragmentation_score(&self, ctx: PmmPolicyContext) -> u64 {
        if ctx.total_pages == 0 {
            return 0;
        }
        // 千分比整数运算 (0..=1000), 避免内核侧浮点 (见 docs/plan/aarch64-kernel-fp-free.md)
        let free_permille = ctx.free_pages * 1000 / ctx.total_pages;
        let fail_permille = if ctx.total_allocs > 0 {
            ctx.failed_allocs * 1000 / ctx.total_allocs
        } else {
            0
        };
        // 碎片化评分: 空闲比例低 + 失败率高 = 高碎片化
        // (1 - free_ratio) × 0.7 + fail_ratio × 0.3 的整数展开
        ((1000 - free_permille) * 7 + fail_permille * 3) / 10
    }

    fn reclaim_threshold_pages(&self, total_pages: u64) -> u64 {
        // 默认: 当空闲页低于总页数的 10% 时触发回收
        (total_pages * 10 / 100).max(64)
    }

    fn watermarks(&self, total_pages: u64) -> Watermarks {
        // Linux 风格水位线: min ≈ 1.25% × total, low = 1.5× min, high = 2× min
        let min = (total_pages * 125 / 10000).max(16);
        let low = min * 3 / 2;
        let high = min * 2;
        Watermarks { high, low, min }
    }
}

static FALLBACK_PMM_POLICY: FallbackPmmPolicy = FallbackPmmPolicy;

/// 全局策略注册表 — services 通过 `register_pmm_policy` 注册
static PMM_POLICY: crate::framework::sync::OnceLock<&'static dyn PmmPolicy> =
    crate::framework::sync::OnceLock::new();

/// 注册 PMM 策略 (由 `services::mm::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
/// # Errors
/// 策略已被注册过时返回 Err。
pub fn register_pmm_policy(policy: &'static dyn PmmPolicy) -> Result<(), &'static dyn PmmPolicy> {
    match PMM_POLICY.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的 PMM 策略 (未注册时返回内建回退)
#[inline]
pub fn current_pmm_policy() -> &'static dyn PmmPolicy {
    match PMM_POLICY.get() {
        Some(&p) => p,
        None => &FALLBACK_PMM_POLICY,
    }
}
