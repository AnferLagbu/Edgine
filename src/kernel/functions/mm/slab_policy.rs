#![deny(unsafe_code)]
//! Slab 策略 — functions 层
//!
//! ## 框架责任分离
//!
//! - **privileged**: slab 页面分配/释放, 位图操作, 链表管理, 锁操作 (机制)
//! - **functions** (本模块): 缓存大小选择, 对象数计算, 分配优先级, 大小限制 (策略)
//!
//! ## 策略表
//!
//! | 策略 | 默认行为 | 可调参数 |
//! |------|---------|---------|
//! | find_cache_index | 线性扫描 cache_sizes, 取首个 >= size | cache_sizes 数组 |
//! | calculate_objects_per_slab | (slab_size - header - bitmap) / obj_size | slab_size |
//! | select_alloc_source | partial → free → new | 优先级顺序 |
//! | normalize_object_size | max(requested, MIN_SIZE), 限制 MAX_SIZE | MIN/MAX |
//!
//! ## 关联
//!
//! - T2-3: Slab 策略提取 (2026-06-19)
//! - 互补: pmm_trait::PmmPolicy (物理页分配策略)

use crate::privileged::config::{SLAB_MAX_OBJECT_SIZE, SLAB_MIN_OBJECT_SIZE};
use crate::privileged::mm::slab_trait::{SlabAllocSource, SlabPolicy, SlabPolicyContext};

// ============================================================================
// 默认 Slab 策略 — 标准 slab 分配器行为
// ============================================================================

/// 默认 Slab 策略 — 与原 privileged/mm/slab.rs 硬编码行为一致
///
/// 在 `functions::mm::init()` 中通过 `register_slab_policy()` 注册.
pub struct DefaultSlabPolicy;

impl SlabPolicy for DefaultSlabPolicy {
    /// 缓存大小选择: 线性扫描, 取首个 >= 请求大小的缓存
    fn find_cache_index(&self, size: usize, cache_sizes: &[usize]) -> Option<usize> {
        for (i, &cache_size) in cache_sizes.iter().enumerate() {
            if size <= cache_size {
                return Some(i);
            }
        }
        None
    }

    /// 计算每个 Slab 可容纳的对象数
    ///
    /// 公式: (`slab_size` - `header_size` - `bitmap_bytes`) / `object_size`
    /// 其中 `bitmap_bytes` = `estimated_objects.div_ceil(8)`
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

    /// Slab 选择策略: partial → free → new
    ///
    /// 优先从 partial 链表分配 (减少碎片),
    /// 其次从 free 链表分配 (复用空闲 Slab),
    /// 最后新建 Slab (扩展缓存).
    fn select_alloc_source(&self, ctx: SlabPolicyContext) -> SlabAllocSource {
        if ctx.partial_slabs > 0 {
            SlabAllocSource::Partial
        } else if ctx.free_slabs > 0 {
            SlabAllocSource::Free
        } else {
            SlabAllocSource::NewSlab
        }
    }

    /// 对象大小规范化
    ///
    /// - 请求 0 或超过 MAX → None (回退堆分配器)
    /// - 请求 < MIN → 提升到 MIN (16 字节)
    fn normalize_object_size(&self, requested_size: usize) -> Option<usize> {
        if requested_size == 0 || requested_size > SLAB_MAX_OBJECT_SIZE {
            return None;
        }
        Some(requested_size.max(SLAB_MIN_OBJECT_SIZE))
    }
}

/// 注册默认 Slab 策略到 privileged
///
/// 由 `functions::mm::init()` 调用. 只能注册一次.
///
/// # Errors
///
/// 当 Slab 策略已被注册时返回 `Err(())`.
pub fn register_default_slab_policy() -> Result<(), ()> {
    static POLICY: DefaultSlabPolicy = DefaultSlabPolicy;
    crate::privileged::mm::register_slab_policy(&POLICY).map_err(|_| ())
}

// ============================================================================
// 单元测试 — Slab 策略契约
// ============================================================================
//
// 验证 DefaultSlabPolicy 的 4 个核心方法:
// - find_cache_index: 缓存大小选择 (匹配/无匹配/精确边界)
// - calculate_objects_per_slab: 对象数计算 (含 header/bitmap 扣除)
// - select_alloc_source: 分配源选择 (partial → free → new)
// - normalize_object_size: 大小规范化 (0/超 MAX/小于 MIN)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::mm::slab_trait::{SlabAllocSource, SlabPolicyContext};

    /// 1. find_cache_index: 命中首个 >= size 的 cache
    #[test]
    fn test_slab_find_cache_index() {
        let policy = DefaultSlabPolicy;
        let sizes = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
        // 8 → 16 (index 0)
        assert_eq!(policy.find_cache_index(8, &sizes), Some(0));
        // 16 → 16 (index 0, 精确匹配)
        assert_eq!(policy.find_cache_index(16, &sizes), Some(0));
        // 17 → 32 (index 1)
        assert_eq!(policy.find_cache_index(17, &sizes), Some(1));
        // 100 → 128 (index 3)
        assert_eq!(policy.find_cache_index(100, &sizes), Some(3));
        // 1024 → 1024 (index 6, 精确匹配)
        assert_eq!(policy.find_cache_index(1024, &sizes), Some(6));
        // 4096 → 4096 (index 8, 精确匹配)
        assert_eq!(policy.find_cache_index(4096, &sizes), Some(8));
        // 5000 → None (超所有 cache)
        assert_eq!(policy.find_cache_index(5000, &sizes), None);
        // 空 cache_sizes → None
        assert_eq!(policy.find_cache_index(16, &[]), None);
    }

    /// 2. calculate_objects_per_slab: 公式 (slab - header - bitmap) / obj
    #[test]
    fn test_slab_calculate_objects() {
        let policy = DefaultSlabPolicy;
        // 标准场景: slab=4096, header=128, obj=32
        // usable = 4096 - 128 = 3968
        // estimated = 3968 / 32 = 124
        // bitmap = 124 / 8 = 16 (向上)
        // actual_usable = 3968 - 16 = 3952
        // objects = 3952 / 32 = 123
        assert_eq!(policy.calculate_objects_per_slab(4096, 128, 32), 123);
        // 小对象: obj=16, slab=4096, header=64
        // usable = 4032, estimated = 4032/16 = 252
        // bitmap = 252/8 = 32, actual_usable = 4000
        // objects = 4000/16 = 250
        assert_eq!(policy.calculate_objects_per_slab(4096, 64, 16), 250);
        // 大对象: obj=2048, slab=4096, header=128
        // usable = 3968, estimated = 3968/2048 = 1
        // bitmap = 1/8 = 1, actual_usable = 3967
        // objects = 3967/2048 = 1
        assert_eq!(policy.calculate_objects_per_slab(4096, 128, 2048), 1);
        // 边界: obj == slab - header (无空间)
        // 可用 = 总 - 头, 预估 = 1, 位图 = 1, 实际可用 = 总 - 头 - 1
        // objects = (slab - header - 1) / obj, obj=slab-header 时为 0
        assert_eq!(policy.calculate_objects_per_slab(1024, 512, 512), 0);
    }

    /// 3. select_alloc_source: partial 优先
    #[test]
    fn test_slab_select_alloc_source() {
        let policy = DefaultSlabPolicy;
        // partial > 0 → Partial (优先)
        let ctx = SlabPolicyContext {
            object_size: 32,
            objects_per_slab: 100,
            partial_slabs: 5,
            free_slabs: 0,
            total_slabs: 10,
        };
        assert_eq!(policy.select_alloc_source(ctx), SlabAllocSource::Partial);
        // partial > 0, free > 0 → Partial (仍优先)
        let ctx = SlabPolicyContext {
            object_size: 32,
            objects_per_slab: 100,
            partial_slabs: 1,
            free_slabs: 3,
            total_slabs: 5,
        };
        assert_eq!(policy.select_alloc_source(ctx), SlabAllocSource::Partial);
        // partial = 0, free > 0 → Free
        let ctx = SlabPolicyContext {
            object_size: 32,
            objects_per_slab: 100,
            partial_slabs: 0,
            free_slabs: 3,
            total_slabs: 5,
        };
        assert_eq!(policy.select_alloc_source(ctx), SlabAllocSource::Free);
        // partial = 0, free = 0 → NewSlab
        let ctx = SlabPolicyContext {
            object_size: 32,
            objects_per_slab: 100,
            partial_slabs: 0,
            free_slabs: 0,
            total_slabs: 0,
        };
        assert_eq!(policy.select_alloc_source(ctx), SlabAllocSource::NewSlab);
    }

    /// 4. normalize_object_size: 0/超 MAX → None, < MIN → MIN
    #[test]
    fn test_slab_normalize_object_size() {
        let policy = DefaultSlabPolicy;
        // 0 → None
        assert_eq!(policy.normalize_object_size(0), None);
        // > MAX → None
        assert_eq!(policy.normalize_object_size(SLAB_MAX_OBJECT_SIZE + 1), None);
        assert_eq!(policy.normalize_object_size(SLAB_MAX_OBJECT_SIZE * 2), None);
        // == MIN → MIN
        assert_eq!(
            policy.normalize_object_size(SLAB_MIN_OBJECT_SIZE),
            Some(SLAB_MIN_OBJECT_SIZE)
        );
        // < MIN → 提升到 MIN
        assert_eq!(policy.normalize_object_size(1), Some(SLAB_MIN_OBJECT_SIZE));
        assert_eq!(policy.normalize_object_size(8), Some(SLAB_MIN_OBJECT_SIZE));
        // MIN < size <= MAX → 原值
        assert_eq!(policy.normalize_object_size(64), Some(64));
        assert_eq!(policy.normalize_object_size(1024), Some(1024));
        assert_eq!(
            policy.normalize_object_size(SLAB_MAX_OBJECT_SIZE),
            Some(SLAB_MAX_OBJECT_SIZE)
        );
    }

    /// 5. integration: 完整分配流程
    #[test]
    fn test_slab_allocation_flow() {
        let policy = DefaultSlabPolicy;
        // 1. 请求 100 字节 → 选 cache index (128)
        let sizes = [16, 32, 64, 128, 256, 512, 1024];
        assert_eq!(policy.find_cache_index(100, &sizes), Some(3));
        // 2. 规范化 100 字节 → 仍为 100 (>= MIN, <= MAX)
        let norm = policy.normalize_object_size(100).unwrap();
        assert_eq!(norm, 100);
        // 3. 计算 objects per slab (4096 slab, 128 header, 100 obj)
        // 可用 = 3968, 预估 = 39, 位图 = 5, 实际 = 3963, 对象 = 39
        let objects = policy.calculate_objects_per_slab(4096, 128, 100);
        assert_eq!(objects, 39);
        // 4. partial > 0 → 选 Partial
        let ctx = SlabPolicyContext {
            object_size: 100,
            objects_per_slab: objects,
            partial_slabs: 2,
            free_slabs: 1,
            total_slabs: 4,
        };
        assert_eq!(policy.select_alloc_source(ctx), SlabAllocSource::Partial);
    }
}
