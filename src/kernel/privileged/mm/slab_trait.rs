//! Slab 策略决策 trait — 策略-机制分离接口
//!
//! T2-3: Slab 内部策略 (缓存大小选择/对象数计算/Slab 选择优先级/大小限制)
//! 由 functions 实现, privileged 仅保留 slab 页面分配/释放/位图操作机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackSlabPolicy`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_slab_policy()` 注册自己的策略实现
//!
//! ## 与 KmemCache 的关系
//!
//! - `KmemCache` (slab.rs): buddy 页面分配/释放, 位图操作, 链表管理 (机制)
//! - `SlabPolicy` (本模块): 缓存大小选择, 对象数计算, 分配优先级 (策略)

use crate::privileged::config::{SLAB_MAX_OBJECT_SIZE, SLAB_MIN_OBJECT_SIZE};

/// Slab 策略上下文 — 传递给策略决策的只读信息
#[derive(Debug, Clone, Copy)]
pub struct SlabPolicyContext {
    /// 当前缓存的对象大小
    pub object_size: usize,
    /// 每个 Slab 的对象数
    pub objects_per_slab: u32,
    /// partial 链表中的 Slab 数
    pub partial_slabs: u32,
    /// free 链表中的 Slab 数
    pub free_slabs: u32,
    /// 总 Slab 数
    pub total_slabs: u32,
}

/// Slab 分配来源 — 策略决策返回值
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlabAllocSource {
    /// 从 partial 链表分配 (优先)
    Partial,
    /// 从 free 链表分配
    Free,
    /// 新建 Slab 分配
    NewSlab,
}

/// Slab 策略接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait SlabPolicy: Send + Sync {
    /// 缓存大小选择: 将请求大小映射到通用缓存索引
    ///
    /// 返回 `Some(idx)` 表示使用第 idx 个通用缓存,
    /// 返回 `None` 表示超出 slab 范围, 应回退到堆分配器.
    fn find_cache_index(&self, size: usize, cache_sizes: &[usize]) -> Option<usize>;

    /// 计算每个 Slab 可容纳的对象数
    ///
    /// `slab_size` 为 Slab 页大小 (通常 4096),
    /// `header_size` 为 `SlabHeader` 大小,
    /// `object_size` 为单个对象大小.
    fn calculate_objects_per_slab(
        &self,
        slab_size: usize,
        header_size: usize,
        object_size: usize,
    ) -> u32;

    /// Slab 选择策略: 决定从哪个来源分配对象
    ///
    /// 默认策略: partial → free → new.
    fn select_alloc_source(&self, ctx: SlabPolicyContext) -> SlabAllocSource;

    /// 对象大小规范化: 将请求大小调整为实际分配大小
    ///
    /// 例如: 请求 8 字节 → 规范化为 `MIN_OBJECT_SIZE` (16 字节).
    /// 请求超过 `MAX_OBJECT_SIZE` → 返回 None (回退到堆分配器).
    fn normalize_object_size(&self, requested_size: usize) -> Option<usize>;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略 — 标准 slab 分配器行为
///
/// 在 functions 注册策略之前, Slab 使用此策略.
/// 逻辑与原 `slab.rs` 硬编码行为一致.
pub struct FallbackSlabPolicy;

impl SlabPolicy for FallbackSlabPolicy {
    fn find_cache_index(&self, size: usize, cache_sizes: &[usize]) -> Option<usize> {
        for (i, &cache_size) in cache_sizes.iter().enumerate() {
            if size <= cache_size {
                return Some(i);
            }
        }
        None
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn calculate_objects_per_slab(
        &self,
        slab_size: usize,
        header_size: usize,
        object_size: usize,
    ) -> u32 {
        let usable_space = slab_size - header_size;
        let estimated_objects = usable_space / object_size;
        let bitmap_bytes = estimated_objects.div_ceil(8);
        let actual_usable = usable_space - bitmap_bytes;
        (actual_usable / object_size) as u32
    }

    fn select_alloc_source(&self, ctx: SlabPolicyContext) -> SlabAllocSource {
        if ctx.partial_slabs > 0 {
            SlabAllocSource::Partial
        } else if ctx.free_slabs > 0 {
            SlabAllocSource::Free
        } else {
            SlabAllocSource::NewSlab
        }
    }

    fn normalize_object_size(&self, requested_size: usize) -> Option<usize> {
        if requested_size == 0 || requested_size > SLAB_MAX_OBJECT_SIZE {
            return None;
        }
        Some(requested_size.max(SLAB_MIN_OBJECT_SIZE))
    }
}

static FALLBACK_SLAB_POLICY: FallbackSlabPolicy = FallbackSlabPolicy;

/// 全局策略注册表 — functions 通过 `register_slab_policy` 注册
static SLAB_POLICY: crate::privileged::sync::OnceLock<&'static dyn SlabPolicy> =
    crate::privileged::sync::OnceLock::new();

/// 注册 Slab 策略 (由 `functions::mm::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_slab_policy(
    policy: &'static dyn SlabPolicy,
) -> Result<(), &'static dyn SlabPolicy> {
    match SLAB_POLICY.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的 Slab 策略 (未注册时返回内建回退)
#[inline]
pub fn current_slab_policy() -> &'static dyn SlabPolicy {
    match SLAB_POLICY.get() {
        Some(&p) => p,
        None => &FALLBACK_SLAB_POLICY,
    }
}
