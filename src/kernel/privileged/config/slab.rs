//! Slab 分配器配置常量 — privileged 机制常量
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量于 T6-9 (2026-06-16) 迁至 `functions::config::slab`, 本文件仅 re-export。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：`SLAB_*` 被 privileged
//! mm/slab 机制 (per-cache 数组/对象上限) 直接消费 — 属机制常量, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::config::slab::*` 保持 API 兼容。
//! 本文件 0 unsafe (纯常量).

use super::memory::PAGE_SIZE;

/// Default Slab cache size (4 KiB = one page).
pub const SLAB_DEFAULT_SIZE: usize = PAGE_SIZE as usize;

/// Slab 对象最小尺寸 (字节).
pub const SLAB_MIN_OBJECT_SIZE: usize = 16;

/// Slab 对象最大尺寸 (字节).
pub const SLAB_MAX_OBJECT_SIZE: usize = 2048;

/// 通用 Slab 缓存数量.
pub const SLAB_GENERAL_CACHE_NUM: usize = 8;
