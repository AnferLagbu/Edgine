//! EGDF块设备协议 (EGDF Block Protocol)
//!
//! 块设备注册路径 — 统一使用 `BlockDevice` trait.
//!
//! `egdf_blk_read/write` 直接调用 `&mut dyn BlockDevice::blk_read/blk_write`,
//! 0 unsafe, 编译期类型安全.

use crate::privileged::egdf::BlockDevice;

/// 注册块设备到 EGDF
///
/// 直接传 `&'static mut dyn BlockDevice`:
/// - `egdf_blk_read/write` 直接 trait dispatch
/// - 编译期类型安全
///
/// # 内存
///
/// `dev: impl BlockDevice + 'static` 会被 `Box::leak` 为静态内存, 由 EGDF 全权管理.
/// 这意味着注册的块设备**永不释放** (适用 OS 生命周期). 如需动态卸载, 请
/// 显式管理并使用 `egdf_unregister` + drop.
///
/// 返回设备在 `EGDF_DEVICES` 中的索引 (用作 `drive_id`)。
pub fn register_block_device(
    name: &'static str,
    dev: impl BlockDevice + 'static,
    io_base: Option<u64>,
) -> u32 {
    let leaked: &'static mut dyn BlockDevice = alloc::boxed::Box::leak(alloc::boxed::Box::new(dev));
    crate::privileged::egdf::egdf_register_block_dev(name, io_base, None, leaked)
}
