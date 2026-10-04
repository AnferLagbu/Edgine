#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//!
//! ARC (Adaptive Replacement Cache) trait 抽象 — LEGACY-5.8
//!
//! ## 架构
//!
//! ```text
//! ArcCache trait (framework/unkfs 抽象接口)
//!   ├── StandardArc (services/unkfs, 0 unsafe, UnkfsArc 包装)
//!   └── MockArc   (host-test 用)
//! ```
//!
//! ## TCB 减负
//!
//! 原 `UnkfsArc` 8 个公共方法 (`init/lookup/insert/release/mark_dirty/flush_dirty`/...) 直接暴露.
//! 提取 trait 后:
//! - DMU/SPA 调用方依赖 trait object / 泛型, 不再绑死 `UnkfsArc`
//! - 单元测试可注入 `MockArc`, 验证缓存替换策略 (LRU/LFU/ARC) 行为
//!
//! ## 与 LEGACY-5.1-5.5/5.7 范式一致
//!
//! 注: `UnkfsArcKey` 包含 `vdev_id/offset/birth_txg`, 用于唯一标识缓存条目.

use super::arc::{UnkfsArc, UnkfsArcBufType, UnkfsArcKey};

// ============================================================================
// ArcCache trait — 自适应替换缓存接口
// ============================================================================

/// ARC 缓存 trait
///
/// `UnkfsArc` 的核心方法 (init/lookup/insert/release) 抽象为 trait,
/// 让 DMU/SPA 等调用方依赖抽象而非具体类型, 便于单元测试注入 mock 实现.
///
/// # Safety
///
/// - `init` 应只调用一次 (初始化 `hash_table` + `max_size`)
/// - `lookup` 返回 true 表示命中, false 表示未命中
/// - `insert` 在容量满时会按 ARC 策略淘汰
/// - `release` 减少 `ref_count`, 0 时可淘汰
pub trait ArcCache: Send + Sync {
    /// 初始化缓存
    fn init(&self, max_size: usize);

    /// 是否已初始化
    fn is_initialized(&self) -> bool;

    /// 查找 key
    /// 返回 true = 命中, false = 未命中
    fn lookup(&self, key: &UnkfsArcKey) -> bool;

    /// 插入 key + data
    /// 返回 true = 成功, false = 错误
    fn insert(&self, key: UnkfsArcKey, data: &[u8], buf_type: UnkfsArcBufType) -> bool;

    /// 释放 key (引用计数 -1, 0 时可淘汰)
    fn release(&self, key: &UnkfsArcKey);

    /// 当前缓存总大小
    fn current_size(&self) -> u64;

    /// MRU 列表条目数 (与 `max_size` 同量纲)
    fn mru_size(&self) -> u64;

    /// MFU 列表条目数 (与 `max_size` 同量纲)
    fn mfu_size(&self) -> u64;

    /// 最大容量 (条目数, ARC 淘汰判据的单位)
    fn max_size(&self) -> u64;

    /// 命中次数
    fn hit_count(&self) -> u64;

    /// 未命中次数
    fn miss_count(&self) -> u64;

    /// 淘汰次数
    fn evict_count(&self) -> u64;

    /// 命中率 (千分比, 0..=1000)
    fn hit_rate(&self) -> u64;
}

// ============================================================================
// StandardArc — 默认 ARC 实现 (UnkfsArc 包装)
// ============================================================================

/// 标准 ARC 实现 — 包装 `UnkfsArc`, 委托公共方法
///
/// 0 unsafe, 0 thunk, 编译期类型安全.
/// 单元测试可注入 `MockArc` 替代本实现.
pub struct StandardArc(pub UnkfsArc);

impl StandardArc {
    /// 构造新实例 (未初始化)
    pub fn new() -> Self {
        Self(UnkfsArc::new())
    }

    /// 访问内部 `UnkfsArc` (向后兼容)
    pub fn inner(&self) -> &UnkfsArc {
        &self.0
    }
}

impl Default for StandardArc {
    fn default() -> Self {
        Self::new()
    }
}

impl ArcCache for StandardArc {
    fn init(&self, max_size: usize) {
        self.0.init(max_size);
    }

    fn is_initialized(&self) -> bool {
        self.0.is_initialized()
    }

    fn lookup(&self, key: &UnkfsArcKey) -> bool {
        self.0.lookup(key).is_some()
    }

    fn insert(&self, key: UnkfsArcKey, data: &[u8], buf_type: UnkfsArcBufType) -> bool {
        self.0.insert(key, data, buf_type).is_some()
    }

    fn release(&self, key: &UnkfsArcKey) {
        self.0.release(key);
    }

    fn current_size(&self) -> u64 {
        self.0.current_size()
    }

    fn mru_size(&self) -> u64 {
        self.0.mru_size()
    }

    fn mfu_size(&self) -> u64 {
        self.0.mfu_size()
    }

    fn max_size(&self) -> u64 {
        self.0.max_size()
    }

    fn hit_count(&self) -> u64 {
        self.0.hit_count()
    }

    fn miss_count(&self) -> u64 {
        self.0.miss_count()
    }

    fn evict_count(&self) -> u64 {
        self.0.evict_count()
    }

    fn hit_rate(&self) -> u64 {
        let hits = self.hit_count();
        let total = hits + self.miss_count();
        if total > 0 { hits * 1000 / total } else { 0 }
    }
}

// ============================================================================
// 单元测试 — ArcCache trait 契约
// ============================================================================
//
// 验证 StandardArc 的 12 个 trait 方法 + 缓存命中/淘汰语义.

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// 1. new + uninitialized
    #[test]
    fn test_arc_uninitialized() {
        let arc = StandardArc::new();
        assert!(!arc.is_initialized());
        // 未 init 时, stats 全 0
        assert_eq!(arc.current_size(), 0);
        assert_eq!(arc.hit_count(), 0);
        assert_eq!(arc.miss_count(), 0);
    }

    /// 2. init: 设置 max_size
    #[test]
    fn test_arc_init() {
        let arc = StandardArc::new();
        arc.init(100);
        assert!(arc.is_initialized());
        assert_eq!(arc.max_size(), 100);
    }

    /// 3. init 0 → 默认值
    #[test]
    fn test_arc_init_zero_uses_default() {
        let arc = StandardArc::new();
        arc.init(0);
        // init(0) 应用 HV_ARC_DEFAULT_SIZE
        assert!(arc.max_size() > 0);
    }

    /// 4. lookup miss: 计数增加
    #[test]
    fn test_arc_lookup_miss() {
        let arc = StandardArc::new();
        arc.init(100);
        let key = UnkfsArcKey::new(0, 0, 0);
        let hit = arc.lookup(&key);
        assert!(!hit);
        assert_eq!(arc.miss_count(), 1);
    }

    /// 5. insert 后 lookup hit
    #[test]
    fn test_arc_insert_lookup_hit() {
        let arc = StandardArc::new();
        arc.init(100);
        let key = UnkfsArcKey::new(0, 0, 0);
        let data = vec![0xAA; 64];
        assert!(arc.insert(key, &data, UnkfsArcBufType::Data));
        // 再 lookup → hit
        assert!(arc.lookup(&key));
    }

    /// 6. insert 数据类型区分
    #[test]
    fn test_arc_buf_types() {
        let arc = StandardArc::new();
        arc.init(100);
        // insert data 类型
        let k1 = UnkfsArcKey::new(0, 0, 1);
        arc.insert(k1, &[1u8; 32], UnkfsArcBufType::Data);
        // insert meta 类型
        let k2 = UnkfsArcKey::new(0, 0, 2);
        arc.insert(k2, &[2u8; 32], UnkfsArcBufType::Metadata);
        // 两个都应存在
        assert!(arc.lookup(&k1));
        assert!(arc.lookup(&k2));
    }

    /// 7. hit_rate 计算
    #[test]
    fn test_arc_hit_rate() {
        let arc = StandardArc::new();
        arc.init(100);
        // 0 命中率
        assert_eq!(arc.hit_rate(), 0);
        // 插 1 个, 查 2 次 (1 hit + 1 miss)
        let k = UnkfsArcKey::new(0, 0, 0);
        arc.insert(k, &[0u8; 32], UnkfsArcBufType::Data);
        arc.lookup(&k); // hit
        let miss_key = UnkfsArcKey::new(0, 0, 99);
        arc.lookup(&miss_key); // miss
        // hit_rate = 1/2 → 千分比 500
        assert_eq!(arc.hit_rate(), 500);
    }

    /// 8. release: 释放 key (引用计数)
    #[test]
    fn test_arc_release() {
        let arc = StandardArc::new();
        arc.init(100);
        let k = UnkfsArcKey::new(0, 0, 0);
        arc.insert(k, &[0u8; 32], UnkfsArcBufType::Data);
        // release 不抛错
        arc.release(&k);
    }

    /// 9. mru_size / mfu_size: 列表大小
    #[test]
    fn test_arc_mru_mfu() {
        let arc = StandardArc::new();
        arc.init(100);
        // 初始: mru/mfu 都 0
        assert_eq!(arc.mru_size(), 0);
        assert_eq!(arc.mfu_size(), 0);
        // insert 后 (具体大小取决于 ARC 内部策略)
        let k = UnkfsArcKey::new(0, 0, 0);
        arc.insert(k, &[0u8; 32], UnkfsArcBufType::Data);
        // 不变量: mru + mfu <= max_size
        let mru = arc.mru_size();
        let mfu = arc.mfu_size();
        assert!(mru + mfu <= arc.max_size());
    }

    /// 10. trait 对象分发 (dyn ArcCache)
    #[test]
    fn test_arc_trait_object() {
        let arc: alloc::boxed::Box<dyn ArcCache> = alloc::boxed::Box::new(StandardArc::new());
        arc.init(50);
        assert!(arc.is_initialized());
        let k = UnkfsArcKey::new(0, 0, 0);
        assert!(!arc.lookup(&k)); // miss
        assert!(arc.insert(k, &[0u8; 16], UnkfsArcBufType::Data));
    }

    /// 11. integration: 容量满触发淘汰
    #[test]
    fn test_arc_capacity_eviction() {
        let arc = StandardArc::new();
        arc.init(3); // 容量 3
        // 插 5 个
        for i in 0..5 {
            let k = UnkfsArcKey::new(0, i, 0);
            arc.insert(k, &[0u8; 32], UnkfsArcBufType::Data);
        }
        // 至少应有一次淘汰
        assert!(arc.evict_count() > 0, "容量 3 插 5 应有淘汰");
    }

    /// 12. integration: ARC 模拟 SPA/DMU 场景
    #[test]
    fn test_arc_spa_simulation() {
        let arc = StandardArc::new();
        arc.init(10);
        // 模拟 SPA 缓存 dataset 元数据
        let datasets: Vec<UnkfsArcKey> = (0..5).map(|i| UnkfsArcKey::new(0, i * 4096, 0)).collect();
        for (i, k) in datasets.iter().enumerate() {
            arc.insert(*k, &vec![i as u8; 64], UnkfsArcBufType::Metadata);
        }
        // 验证
        for (i, k) in datasets.iter().enumerate() {
            if arc.lookup(k) {
                // 命中
                let _ = i;
            }
        }
        // 不变量
        assert!(arc.mru_size() + arc.mfu_size() <= arc.max_size());
    }
}
