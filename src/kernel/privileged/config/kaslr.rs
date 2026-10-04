//! KASLR 配置 — privileged 机制常量与全局状态
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量与全局状态 (`KASLR_BASE_OFFSET` AtomicU64) 于 T6-9 (2026-06-16) 迁至
//! `functions::config::kaslr`, 本文件仅 re-export。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：`KASLR_BASE_OFFSET` 是
//! 机制持有的全局状态, 被 privileged config/caps `get_config_summary` 机制消费 —
//! 属机制项, 迁回。`validate_kaslr_offset` (启动自检, 返回 functions `KernelError`)
//! 为策略校验, 留在 functions。
//!
//! functions 侧改 re-export + 本地保留 `validate_kaslr_offset` 保持 API 兼容。
//! 本文件 0 unsafe (纯常量 + AtomicU64 全局状态 + 无锁读写).

use core::sync::atomic::{AtomicU64, Ordering};

/// KASLR 是否启用 (派生自 Cargo feature `kaslr`).
pub const KASLR_ENABLED: bool = cfg!(feature = "kaslr");

/// 偏移对齐粒度 (2MB, 与 2M-huge-page 对齐, 与 `x86_64/aarch64` linker 一致).
pub const KASLR_ALIGN: u64 = 0x200000;

/// 默认偏移 — 未启用 KASLR 时为 0, 等同"加载到 linker 脚本指定的地址".
pub const KASLR_DEFAULT_OFFSET: u64 = 0;

/// 最大允许偏移 (1 GB). 超过此值可能侵入其他子系统地址空间.
pub const KASLR_MAX_OFFSET: u64 = 0x4000_0000;

/// 实际加载时由 bootloader/entry 写入的偏移量.
///
/// 默认值为 0, 含义是"未应用 KASLR". 当 `KASLR_ENABLED` 为 true 时, 该值
/// 在启动早期应被设置为一个对齐到 `KASLR_ALIGN` 的非零值.
pub static KASLR_BASE_OFFSET: AtomicU64 = AtomicU64::new(KASLR_DEFAULT_OFFSET);

/// 设置运行时 KASLR 基址偏移 (由 bootloader/entry 调用).
///
/// 写指针不要求互斥 (`AtomicU64` 自然线程安全); 但只在启动极早期调用一次,
/// 之后多核并发读.
pub fn set_kaslr_offset(offset: u64) {
    KASLR_BASE_OFFSET.store(offset, Ordering::Release);
}

/// 获取当前 KASLR 基址偏移.
pub fn get_kaslr_offset() -> u64 {
    KASLR_BASE_OFFSET.load(Ordering::Acquire)
}

/// 检查 `offset` 是否满足 KASLR 对齐要求.
#[inline]
pub fn is_aligned(offset: u64) -> bool {
    (offset & (KASLR_ALIGN - 1)) == 0
}
