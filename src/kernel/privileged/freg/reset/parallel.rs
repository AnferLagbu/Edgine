//! # 并发回滚机制
//!
//! 支持 SMP 并行回滚多个独立的恢复域

#![deny(missing_docs)]

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::config;
use crate::privileged::freg::DomainState;
use crate::privileged::freg::RECOVERY_MANAGER;

/// 依赖分层允许的最大层数。
pub const MAX_DEPENDENCY_LAYERS: usize = 8;
/// 单个依赖层内可容纳的最大恢复域数。
pub const MAX_DOMAINS_PER_LAYER: usize = 16;

/// 单个依赖层级: 同一层内的恢复域互相无依赖, 可并行回滚。
#[derive(Debug, Clone, Copy)]
pub struct DependencyLayer {
    /// 本层内已登记的恢复域 ID 列表（前 `count` 项有效）。
    pub domains: [u64; MAX_DOMAINS_PER_LAYER],
    /// 本层已登记的恢复域数量。
    pub count: usize,
    /// 本层在依赖拓扑中的层级号（从 0 起）。
    pub level: usize,
}

impl DependencyLayer {
    /// 构造指定层级的空依赖层。
    #[must_use]
    pub const fn new(level: usize) -> Self {
        Self {
            domains: [0; MAX_DOMAINS_PER_LAYER],
            count: 0,
            level,
        }
    }

    /// 向本层追加一个恢复域 ID；层已满时返回 `false`。
    pub fn add(&mut self, domain_id: u64) -> bool {
        if self.count >= MAX_DOMAINS_PER_LAYER {
            return false;
        }
        self.domains[self.count] = domain_id;
        self.count += 1;
        true
    }
}

/// 按依赖拓扑分层后的恢复域集合, 层级由低到高排列。
#[derive(Debug)]
pub struct DependencyLayers {
    /// 各层级的依赖层数组（前 `count` 层有效）。
    pub layers: [DependencyLayer; MAX_DEPENDENCY_LAYERS],
    /// 已使用的层数。
    pub count: usize,
}

impl Default for DependencyLayers {
    fn default() -> Self {
        Self::new()
    }
}

impl DependencyLayers {
    /// 构造空的依赖分层集合。
    #[must_use]
    pub const fn new() -> Self {
        const EMPTY_LAYER: DependencyLayer = DependencyLayer::new(0);
        Self {
            layers: [EMPTY_LAYER; MAX_DEPENDENCY_LAYERS],
            count: 0,
        }
    }

    /// 把恢复域 ID 放入指定层级；层级越界时返回 `false`。
    pub fn add_to_layer(&mut self, level: usize, domain_id: u64) -> bool {
        if level >= MAX_DEPENDENCY_LAYERS {
            return false;
        }
        if self.layers[level].count == 0 && self.count <= level {
            self.count = level + 1;
        }
        self.layers[level].add(domain_id)
    }
}

/// 根据各恢复域的 `depends_on` 关系计算依赖分层 (拓扑排序)。
pub fn compute_dependency_layers() -> DependencyLayers {
    let manager = RECOVERY_MANAGER.lock();
    let domain_count = manager.count.load(Ordering::SeqCst) as usize;

    let mut layers = DependencyLayers::new();
    let mut visited = [false; 32];
    let mut domain_levels = [0u32; 32];

    for _ in 0..domain_count {
        for i in 0..domain_count {
            if visited[i] {
                continue;
            }

            if let Some(domain) = &manager.domains[i] {
                let deps = domain.depends_on.lock();
                let mut all_deps_visited = true;
                let mut max_dep_level = 0u32;

                for slot in deps.iter() {
                    if let Some(dep_id) = *slot {
                        for j in 0..domain_count {
                            if let Some(d) = &manager.domains[j]
                                && d.id() == dep_id
                            {
                                if visited[j] {
                                    max_dep_level = max_dep_level.max(domain_levels[j] + 1);
                                } else {
                                    all_deps_visited = false;
                                }
                                break;
                            }
                        }
                    }
                }

                if all_deps_visited {
                    visited[i] = true;
                    domain_levels[i] = max_dep_level;
                    let level = max_dep_level as usize;
                    layers.add_to_layer(level, domain.id());
                }
            }
        }
    }

    layers
}

/// 串行回滚单层内全部恢复域, 返回累计回滚的撤销条目数。
pub fn rollback_layer_serial(layer: &DependencyLayer) -> usize {
    let mut total_rolled = 0usize;
    let manager = RECOVERY_MANAGER.lock();

    for &domain_id in &layer.domains[..layer.count] {
        if let Some(domain) = manager.find(domain_id) {
            let mut undo = domain.undo.lock();
            let rolled = undo.rollback_to(0);
            total_rolled += rolled;
            drop(undo);
            domain.freg_generation.store(1, Ordering::SeqCst);
            domain.set_state(DomainState::Active, Ordering::SeqCst);
        }
    }

    total_rolled
}

/// 按 worker 分片并行回滚单层的一个分片, 返回该分片回滚的撤销条目数。
pub fn rollback_layer_parallel(layer: &DependencyLayer, worker_id: usize) -> usize {
    let manager = RECOVERY_MANAGER.lock();
    let count = layer.count;

    if count == 0 {
        return 0;
    }

    let max_workers = config::RECOVERY_CONFIG.parallel_max_workers as usize;
    let chunk_size = count.div_ceil(max_workers);
    let start = worker_id * chunk_size;
    let end = (start + chunk_size).min(count);

    if start >= count {
        return 0;
    }

    let mut total_rolled = 0usize;

    for &domain_id in &layer.domains[start..end] {
        if let Some(domain) = manager.find(domain_id) {
            let mut undo = domain.undo.lock();
            let rolled = undo.rollback_to(0);
            total_rolled += rolled;
            drop(undo);
            domain.freg_generation.store(1, Ordering::SeqCst);
            domain.set_state(DomainState::Active, Ordering::SeqCst);
        }
    }

    total_rolled
}

/// 按依赖分层并行回滚全部恢复域, 返回累计回滚的撤销条目数。
pub fn rollback_all_parallel() -> usize {
    config::PARALLEL_ROLLBACK_ACTIVE.store(true, Ordering::SeqCst);

    let layers = compute_dependency_layers();
    let mut total_rolled = 0usize;

    for layer in &layers.layers[..layers.count] {
        if layer.count <= 1 {
            total_rolled += rollback_layer_serial(layer);
        } else {
            let max_workers = config::RECOVERY_CONFIG.parallel_max_workers as usize;
            let mut worker_results = [0usize; 4];

            for (worker_id, result) in worker_results
                .iter_mut()
                .enumerate()
                .take(max_workers.min(4))
            {
                *result = rollback_layer_parallel(layer, worker_id);
            }

            total_rolled += worker_results.iter().sum::<usize>();
        }
    }

    config::PARALLEL_ROLLBACK_ACTIVE.store(false, Ordering::SeqCst);
    total_rolled
}

/// 回滚全部恢复域: 依据配置选择并行分层回滚或串行回滚到初始态。
pub fn rollback_all() -> usize {
    if config::RECOVERY_CONFIG.is_parallel() {
        rollback_all_parallel()
    } else {
        super::bsr::rollback_to_init(&RECOVERY_MANAGER.lock())
    }
}

/// 并行回滚累计执行次数统计。
pub static PARALLEL_ROLLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
/// 并行回滚累计耗时统计（单位由调用方约定）。
pub static PARALLEL_ROLLBACK_TIME: AtomicU32 = AtomicU32::new(0);

/// 读取并行回滚统计 (累计回滚次数, 耗时)。
pub fn get_parallel_stats() -> (usize, u32) {
    (
        PARALLEL_ROLLBACK_COUNT.load(Ordering::SeqCst),
        PARALLEL_ROLLBACK_TIME.load(Ordering::SeqCst),
    )
}

// UT-07 (2026-09-26): 原 `#[cfg(feature = "kernel_test")] pub mod tests { pub fn .. -> bool }`
// 形态改写为源侧 `#[cfg(test)] #[test]`; 注册侧 `privileged/tests/reset.rs::freg::parallel`
// 的薄包装 `check!(tests::test_*())` 随之删除.
#[cfg(test)]
mod tests {
    // J-01 (2026-09-08): wildcard_imports 清理 — 显式列出本模块使用的 super 符号
    use super::{DependencyLayer, DependencyLayers, compute_dependency_layers};

    #[test]
    fn dependency_layer() {
        let mut layer = DependencyLayer::new(0);
        layer.add(1);
        layer.add(2);
        assert_eq!(layer.count, 2, "单层依赖计数");
    }

    #[test]
    fn dependency_layers() {
        let mut layers = DependencyLayers::new();
        layers.add_to_layer(0, 1);
        layers.add_to_layer(1, 2);
        assert_eq!(layers.count, 2, "分层依赖计数");
    }

    #[test]
    fn compute_layers() {
        // J-01 (2026-09-08): 原 `layers.count > 0 || true` 恒真 (overly_complex_bool_expr) —
        // 语义为"compute_dependency_layers 可调用不 panic", 化简并丢弃返回值
        let _ = compute_dependency_layers();
    }
}
