//! # 并发回滚机制
//!
//! 支持 SMP 并行回滚多个独立的恢复域

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::config;
use crate::framework::freg::DomainState;
use crate::framework::freg::RECOVERY_MANAGER;

pub const MAX_DEPENDENCY_LAYERS: usize = 8;
pub const MAX_DOMAINS_PER_LAYER: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct DependencyLayer {
    pub domains: [u64; MAX_DOMAINS_PER_LAYER],
    pub count: usize,
    pub level: usize,
}

impl DependencyLayer {
    pub const fn new(level: usize) -> Self {
        Self {
            domains: [0; MAX_DOMAINS_PER_LAYER],
            count: 0,
            level,
        }
    }

    pub fn add(&mut self, domain_id: u64) -> bool {
        if self.count >= MAX_DOMAINS_PER_LAYER {
            return false;
        }
        self.domains[self.count] = domain_id;
        self.count += 1;
        true
    }
}

#[derive(Debug)]
pub struct DependencyLayers {
    pub layers: [DependencyLayer; MAX_DEPENDENCY_LAYERS],
    pub count: usize,
}

impl DependencyLayers {
    pub const fn new() -> Self {
        const EMPTY_LAYER: DependencyLayer = DependencyLayer::new(0);
        Self {
            layers: [EMPTY_LAYER; MAX_DEPENDENCY_LAYERS],
            count: 0,
        }
    }

    pub fn add_to_layer(&mut self, level: usize, domain_id: u64) -> bool {
        if level >= MAX_DEPENDENCY_LAYERS {
            return false;
        }
        if self.layers[level].count == 0 {
            if self.count <= level {
                self.count = level + 1;
            }
        }
        self.layers[level].add(domain_id)
    }
}

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
                            if let Some(d) = &manager.domains[j] {
                                if d.id == dep_id {
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
                }

                if all_deps_visited {
                    visited[i] = true;
                    domain_levels[i] = max_dep_level;
                    let level = max_dep_level as usize;
                    layers.add_to_layer(level, domain.id);
                }
            }
        }
    }

    layers
}

pub fn rollback_layer_serial(layer: &DependencyLayer) -> usize {
    let mut total_rolled = 0usize;
    let manager = RECOVERY_MANAGER.lock();

    for i in 0..layer.count {
        let domain_id = layer.domains[i];
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

    for i in start..end {
        let domain_id = layer.domains[i];
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

pub fn rollback_all_parallel() -> usize {
    config::PARALLEL_ROLLBACK_ACTIVE.store(true, Ordering::SeqCst);

    let layers = compute_dependency_layers();
    let mut total_rolled = 0usize;

    for layer_idx in 0..layers.count {
        let layer = &layers.layers[layer_idx];

        if layer.count <= 1 {
            total_rolled += rollback_layer_serial(layer);
        } else {
            let max_workers = config::RECOVERY_CONFIG.parallel_max_workers as usize;
            let mut worker_results = [0usize; 4];

            for worker_id in 0..max_workers.min(4) {
                worker_results[worker_id] = rollback_layer_parallel(layer, worker_id);
            }

            total_rolled += worker_results.iter().sum::<usize>();
        }
    }

    config::PARALLEL_ROLLBACK_ACTIVE.store(false, Ordering::SeqCst);
    total_rolled
}

pub fn rollback_all() -> usize {
    if config::RECOVERY_CONFIG.is_parallel() {
        rollback_all_parallel()
    } else {
        super::bsr::rollback_to_init()
    }
}

pub static PARALLEL_ROLLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static PARALLEL_ROLLBACK_TIME: AtomicU32 = AtomicU32::new(0);

pub fn get_parallel_stats() -> (usize, u32) {
    (
        PARALLEL_ROLLBACK_COUNT.load(Ordering::SeqCst),
        PARALLEL_ROLLBACK_TIME.load(Ordering::SeqCst),
    )
}

// UT-07 (2026-09-26): 原 `#[cfg(feature = "kernel_test")] pub mod tests { pub fn .. -> bool }`
// 形态改写为源侧 `#[cfg(test)] #[test]`; 注册侧 `framework/tests/reset.rs::freg::parallel`
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
