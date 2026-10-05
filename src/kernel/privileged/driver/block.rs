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

pub use crate::privileged::egdf::BlockDevice;

// ── UNKFS bridge (EGDF 代理) ──
//
// 所有 hdd_* 函数现在委托给 EGDF 统一 I/O 路径。
// 这确保 UNKFS 的所有块设备访问都经过 EGDF。

/// 从指定块设备读取一个扇区到缓冲区, 返回 0 表示成功
pub fn hdd_read_sector(drive: u8, sector: u64, buf: &mut [u8]) -> i32 {
    crate::privileged::egdf::egdf_blk_read(drive, sector, buf)
}

/// 向指定块设备写入一个扇区, 返回 0 表示成功
pub fn hdd_write_sector(drive: u8, sector: u64, buf: &[u8]) -> i32 {
    crate::privileged::egdf::egdf_blk_write(drive, sector, buf)
}

/// 查询指定块设备是否在位
pub fn hdd_is_present(drive: u8) -> bool {
    crate::privileged::egdf::egdf_blk_is_present(drive)
}

/// 查询指定块设备的总扇区数
pub fn hdd_total_sectors(drive: u8) -> u64 {
    crate::privileged::egdf::egdf_blk_total_sectors(drive)
}

/// 查询指定块设备的名称; 设备不存在时返回 `None`
pub fn block_device_name(drive: u8) -> Option<&'static str> {
    crate::privileged::egdf::egdf_blk_name(drive)
}

/// 查询指定块设备的 (名称, 是否在位, 总扇区数)
pub fn block_device_info(drive: u8) -> (&'static str, bool, u64) {
    crate::privileged::egdf::egdf_blk_info(drive)
}

/// 查询当前已注册的块设备数量
pub fn block_device_count() -> usize {
    crate::privileged::egdf::egdf_blk_count()
}

/// 列出所有块设备, 每项为 (设备索引, 名称, 总扇区数)
pub fn block_device_list() -> Vec<(usize, &'static str, u64)> {
    let devices = crate::privileged::egdf::EGDF_DEVICES.lock();
    devices
        .iter()
        .enumerate()
        .filter(|(_, d)| d.proto == crate::privileged::egdf::EGDFProto::Block)
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
    let removing = crate::privileged::egdf::egdf_blk_is_removed(drive);
    (present, removing, 0)
}
