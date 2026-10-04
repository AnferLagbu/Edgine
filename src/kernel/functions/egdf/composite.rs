#![deny(unsafe_code)]
//! 复合块设备 (Composite Block) — functions 层安全代理
//!
//! ## 状态 (v2.10, 2026-06-04)
//!
//! Phase 2.4 net/egdf 4/4 子系统迁移收尾: 封装 `kernel::egdf::composite::*`:
//! - [x] `devtree_probe_composites` — 扫描设备树, 创建 RAID0/RAID1 复合块设备
//! - [x] `composite_probe` — 顶层探测入口
//!
//! ## 备注
//!
//! 复合设备 (RAID) 实际使用由 driver 内部完成, functions 层只暴露探测入口。
//! `CompositeType`/`CompositeBlockDevice` 是 privileged 内部结构, 不直接暴露给用户态。
//!
//! 评估日期: 2026-06-04

use crate::privileged::egdf::composite;

// ============================================================================
// 顶层 API
// ============================================================================

/// 探测设备树中的复合块设备节点 (compatible: "eg,raid0" / "eg,raid1")
///
/// # 返回
/// 成功创建的复合设备数量 (0 表示未发现兼容节点)
pub fn probe() -> usize {
    composite::devtree_probe_composites()
}

/// 顶层探测入口 (供系统初始化调用)
pub fn probe_init() -> u32 {
    composite::composite_probe()
}
