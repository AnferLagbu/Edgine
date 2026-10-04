//! 块设备抽象层
//!
//! 提供统一的 `BlockDevice` trait, 由所有存储驱动 (ATA, AHCI, NVMe, virtio-blk) 实现.
//!
//! ## EGDF 统一架构
//!
//! EGDF 是唯一的设备驱动框架. 块设备通过 `proto_block::register_block_device`
//! 注册到 EGDF, UNKFS 通过 `egdf_blk_read/write` 直接 I/O.
//!
//! `BlockDevice` trait 定义在 egdf (设备框架) 中, 本模块 re-export.
//! `hdd_*` 函数提供向后兼容的 EGDF 代理.

use alloc::vec::Vec;

// ── BlockDevice Trait (定义在 egdf, 此处 re-export) ──

pub use crate::framework::egdf::BlockDevice;

// ── UNKFS bridge (EGDF 代理) ──
//
// 所有 hdd_* 函数现在委托给 EGDF 统一 I/O 路径。
// 这确保 UNKFS 的所有块设备访问都经过 EGDF。

pub fn hdd_read_sector(drive: u8, sector: u64, buf: &mut [u8]) -> i32 {
    crate::framework::egdf::egdf_blk_read(drive, sector, buf)
}

pub fn hdd_write_sector(drive: u8, sector: u64, buf: &[u8]) -> i32 {
    crate::framework::egdf::egdf_blk_write(drive, sector, buf)
}

pub fn hdd_is_present(drive: u8) -> bool {
    crate::framework::egdf::egdf_blk_is_present(drive)
}

pub fn hdd_total_sectors(drive: u8) -> u64 {
    crate::framework::egdf::egdf_blk_total_sectors(drive)
}

pub fn block_device_name(drive: u8) -> Option<&'static str> {
    crate::framework::egdf::egdf_blk_name(drive)
}

pub fn block_device_info(drive: u8) -> (&'static str, bool, u64) {
    crate::framework::egdf::egdf_blk_info(drive)
}

pub fn block_device_count() -> usize {
    crate::framework::egdf::egdf_blk_count()
}

pub fn block_device_list() -> Vec<(usize, &'static str, u64)> {
    let devices = crate::framework::egdf::EGDF_DEVICES.lock();
    devices
        .iter()
        .enumerate()
        .filter(|(_, d)| d.proto == crate::framework::egdf::EGDFProto::Block)
        .map(|(i, d)| {
            let sectors = d.block_dev.as_ref().map_or(0, |bd| bd.blk_total_sectors());
            (i, d.name, sectors)
        })
        .collect()
}

/// 查询块设备状态 (热插拔 syscall ABI)。
///
/// 返回 `(present, removing, io_count)`:
/// - `present`: 该索引处存在 `Ready` 的块设备且硬件在位
/// - `removing`: 该索引处的块设备已被墓碑化移除
/// - `io_count`: 恒为 0 — EGDF 块设备 I/O 在 `EGDF_DEVICES` 锁内同步
///   完成, 不存在"静默在途 I/O", 故无需引用计数; 保留字段以维持 ABI 稳定。
pub fn block_device_state(drive: u8) -> (bool, bool, u32) {
    let present = hdd_is_present(drive);
    let removing = crate::framework::egdf::egdf_blk_is_removed(drive);
    (present, removing, 0)
}
