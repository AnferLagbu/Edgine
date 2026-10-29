use crate::klog_info;
use crate::klog_warn;
use crate::privileged::egdf::{
    BlockDevice, EGDFProto, egdf_blk_is_present, egdf_blk_read, egdf_blk_total_sectors,
    egdf_blk_write, egdf_find_by_id, egdf_find_by_name,
};
use crate::privileged::egdf::{
    devtree_children, devtree_get_node, devtree_walk, register_block_device,
};
use crate::privileged::error::KernelError;
use core::sync::atomic::{AtomicU32, Ordering};

const MAX_COMPOSITE_CHILDREN: usize = 8;
const MIN_STRIPE_SIZE: u64 = 512;

/// 复合块设备类型 (RAID0 条带 / RAID1 镜像).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeType {
    /// RAID0 条带 (无冗余, 容量累加).
    Raid0,
    /// RAID1 镜像 (全冗余, 容量取最小值).
    Raid1,
}

impl CompositeType {
    /// 返回设备树 compatible 字符串 (`eg,raid0` / `eg,raid1`).
    pub fn compatible_str(self) -> &'static str {
        match self {
            Self::Raid0 => "eg,raid0",
            Self::Raid1 => "eg,raid1",
        }
    }

    /// 按 compatible 字符串解析复合类型 (`eg,raid1` 与 `eg,mirror` 均视为 RAID1).
    pub fn from_compatible(compat: &str) -> Option<Self> {
        match compat {
            "eg,raid0" => Some(Self::Raid0),
            "eg,raid1" | "eg,mirror" => Some(Self::Raid1),
            _ => None,
        }
    }

    /// 返回用于设备名与日志的显示名 (`RAID0` / `RAID1`).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Raid0 => "RAID0",
            Self::Raid1 => "RAID1",
        }
    }
}

/// 由多个子块设备组成的复合块设备 (RAID0 条带 / RAID1 镜像).
pub struct CompositeBlockDevice {
    device_type: CompositeType,
    child_drives: [u8; MAX_COMPOSITE_CHILDREN],
    child_count: u8,
    stripe_sectors: u64,
    total_sectors: u64,
    read_round_robin: AtomicU32,
}

// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe impl Send for CompositeBlockDevice {}
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe impl Sync for CompositeBlockDevice {}

impl Clone for CompositeBlockDevice {
    fn clone(&self) -> Self {
        Self {
            device_type: self.device_type,
            child_drives: self.child_drives,
            child_count: self.child_count,
            stripe_sectors: self.stripe_sectors,
            total_sectors: self.total_sectors,
            read_round_robin: AtomicU32::new(self.read_round_robin.load(Ordering::Relaxed)),
        }
    }
}

impl CompositeBlockDevice {
    /// 创建复合设备并计算总容量.
    ///
    /// RAID0 容量 = 各子盘对齐到条带后的最小值 × 子盘数; RAID1 容量 = 各子盘最小值。
    /// 子盘数量为 0、超过上限、任一子盘容量不足时返回 `None`。
    pub fn new(device_type: CompositeType, child_drives: &[u8], stripe_size: u64) -> Option<Self> {
        if child_drives.is_empty() || child_drives.len() > MAX_COMPOSITE_CHILDREN {
            return None;
        }

        let mut drives: [u8; MAX_COMPOSITE_CHILDREN] = [0; MAX_COMPOSITE_CHILDREN];
        drives[..child_drives.len()].copy_from_slice(child_drives);

        let stripe_size = stripe_size.max(MIN_STRIPE_SIZE);
        let stripe_sectors = stripe_size.div_ceil(512);

        let count = child_drives.len() as u64;

        let total: u64 = match device_type {
            CompositeType::Raid0 => {
                let mut min_sectors = u64::MAX;
                for &drive in child_drives {
                    let sectors = egdf_blk_total_sectors(drive);
                    if sectors < stripe_sectors {
                        return None;
                    }
                    let aligned = (sectors / stripe_sectors) * stripe_sectors;
                    if aligned < min_sectors {
                        min_sectors = aligned;
                    }
                }
                min_sectors.checked_mul(count)?
            }
            CompositeType::Raid1 => {
                let mut min_sectors = u64::MAX;
                for &drive in child_drives {
                    let sectors = egdf_blk_total_sectors(drive);
                    if sectors == 0 {
                        return None;
                    }
                    if sectors < min_sectors {
                        min_sectors = sectors;
                    }
                }
                min_sectors
            }
        };

        Some(Self {
            device_type,
            child_drives: drives,
            child_count: crate::privileged::lib::low_u8_from_usize(child_drives.len()),
            stripe_sectors,
            total_sectors: total,
            read_round_robin: AtomicU32::new(0),
        })
    }

    #[inline]
    fn map_raid0_sector(&self, logical: u64) -> (u8, u64) {
        let stripe = logical / self.stripe_sectors;
        let child_idx =
            crate::privileged::lib::low_u8_from_u64(stripe % u64::from(self.child_count));
        let stripe_row = stripe / u64::from(self.child_count);
        let offset = logical % self.stripe_sectors;
        let physical = stripe_row * self.stripe_sectors + offset;
        (child_idx, physical)
    }
}

impl BlockDevice for CompositeBlockDevice {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
        let num_sectors = (buf.len() / 512) as u64;
        if num_sectors == 0 {
            return KernelError::InvalidArgument.as_i32();
        }
        if sector + num_sectors > self.total_sectors {
            return KernelError::InvalidArgument.as_i32();
        }

        match self.device_type {
            CompositeType::Raid0 => {
                for s in 0..num_sectors {
                    let chunk = &mut buf[crate::privileged::lib::usize_from_u64(s * 512)
                        ..crate::privileged::lib::usize_from_u64((s + 1) * 512)];
                    let (drive_idx, phys_sector) = self.map_raid0_sector(sector + s);
                    let ret = egdf_blk_read(drive_idx, phys_sector, chunk);
                    if ret != 0 {
                        return ret;
                    }
                }
                0
            }
            CompositeType::Raid1 => {
                for s in 0..num_sectors {
                    let chunk = &mut buf[crate::privileged::lib::usize_from_u64(s * 512)
                        ..crate::privileged::lib::usize_from_u64((s + 1) * 512)];
                    let sector_off = sector + s;

                    let start = self.read_round_robin.fetch_add(1, Ordering::Relaxed) as usize
                        % self.child_count as usize;

                    let mut ok = false;
                    for offset in 0..self.child_count as usize {
                        let idx = (start + offset) % self.child_count as usize;
                        let ret = egdf_blk_read(self.child_drives[idx], sector_off, chunk);
                        if ret == 0 {
                            ok = true;
                            break;
                        }
                    }
                    if !ok {
                        return KernelError::Io.as_i32();
                    }
                }
                0
            }
        }
    }

    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
        let num_sectors = (buf.len() / 512) as u64;
        if num_sectors == 0 {
            return KernelError::InvalidArgument.as_i32();
        }
        if sector + num_sectors > self.total_sectors {
            return KernelError::InvalidArgument.as_i32();
        }

        match self.device_type {
            CompositeType::Raid0 => {
                for s in 0..num_sectors {
                    let chunk = &buf[crate::privileged::lib::usize_from_u64(s * 512)
                        ..crate::privileged::lib::usize_from_u64((s + 1) * 512)];
                    let (drive_idx, phys_sector) = self.map_raid0_sector(sector + s);
                    let ret = egdf_blk_write(drive_idx, phys_sector, chunk);
                    if ret != 0 {
                        return ret;
                    }
                }
                0
            }
            CompositeType::Raid1 => {
                for s in 0..num_sectors {
                    let chunk = &buf[crate::privileged::lib::usize_from_u64(s * 512)
                        ..crate::privileged::lib::usize_from_u64((s + 1) * 512)];
                    let sector_off = sector + s;

                    for i in 0..self.child_count as usize {
                        let ret = egdf_blk_write(self.child_drives[i], sector_off, chunk);
                        if ret != 0 {
                            return ret;
                        }
                    }
                }
                0
            }
        }
    }

    fn blk_is_present(&self) -> bool {
        match self.device_type {
            CompositeType::Raid0 => {
                for i in 0..self.child_count as usize {
                    if !egdf_blk_is_present(self.child_drives[i]) {
                        return false;
                    }
                }
                true
            }
            CompositeType::Raid1 => {
                for i in 0..self.child_count as usize {
                    if egdf_blk_is_present(self.child_drives[i]) {
                        return true;
                    }
                }
                false
            }
        }
    }

    fn blk_total_sectors(&self) -> u64 {
        self.total_sectors
    }
}

/// 遍历设备树, 为匹配 `eg,raid0`/`eg,raid1` 的节点创建并注册复合块设备, 返回创建数量.
pub fn devtree_probe_composites() -> usize {
    let composite_compatibles: &[&str] = &["eg,raid0", "eg,raid1"];

    let mut candidate_nodes: alloc::vec::Vec<(u32, CompositeType)> = alloc::vec::Vec::new();

    devtree_walk(|node| {
        for &compat in composite_compatibles {
            if node.matches_compatible(compat) {
                if let Some(ct) = CompositeType::from_compatible(compat) {
                    candidate_nodes.push((node.id, ct));
                }
                break;
            }
        }
    });

    if candidate_nodes.is_empty() {
        klog_info!(Driver, "EGDF: no composite device tree nodes found");
        return 0;
    }

    let mut created = 0usize;

    for (node_id, composite_type) in candidate_nodes {
        let children = devtree_children(node_id);

        if children.is_empty() {
            klog_warn!(
                Driver,
                "EGDF: composite node {} has no children, skipping",
                node_id
            );
            continue;
        }

        let child_drives: alloc::vec::Vec<u8> = collect_composite_child_drives(&children);

        if child_drives.is_empty() {
            klog_warn!(
                Driver,
                "EGDF: composite node {} has no valid block children",
                node_id
            );
            continue;
        }

        let stripe_size: u64 = {
            let Some(node) = devtree_get_node(node_id) else {
                continue;
            };
            node.get_prop("stripe_size")
                .and_then(super::devtree::PropertyValue::as_u64)
                .unwrap_or(65536)
        };

        let orig = stripe_size;
        let stripe_size = stripe_size.max(MIN_STRIPE_SIZE);
        if orig < MIN_STRIPE_SIZE {
            klog_info!(
                Driver,
                "EGDF: stripe_size {} < 512, clamped to {}",
                orig,
                stripe_size
            );
        }

        let dev_name = alloc::format!(
            "{}-{}",
            composite_type.display_name().to_lowercase(),
            created
        );
        // EGDF register API 要求设备名为 `&'static str`.
        // 这是有界分配 —— 初始化时每个复合设备一份.
        let name_leaked: &'static str = dev_name.leak();

        let Some(composite) = CompositeBlockDevice::new(composite_type, &child_drives, stripe_size)
        else {
            klog_warn!(
                Driver,
                "EGDF: failed to create composite device '{}'",
                name_leaked
            );
            continue;
        };

        let total_mb = composite.total_sectors / 2048;
        register_block_device(name_leaked, composite, None);

        klog_info!(
            Driver,
            "EGDF: {} virtual device '{}' registered ({} children, {} MB)",
            composite_type.display_name(),
            name_leaked,
            child_drives.len(),
            total_mb
        );

        created += 1;
    }

    created
}

/// 收集复合节点下所有可用块设备在 EGDF 注册表中的索引 (`u8` 有界).
///
/// 跳过子节点不存在 / 非块设备 / 未注册 / 索引超 `u8` 范围的情况, 各打一行告警.
fn collect_composite_child_drives(children: &[u32]) -> alloc::vec::Vec<u8> {
    let mut child_drives: alloc::vec::Vec<u8> = alloc::vec::Vec::new();

    for &child_id in children {
        let Some(child_node) = devtree_get_node(child_id) else {
            klog_warn!(
                Driver,
                "EGDF: composite child node {} not found, skipping",
                child_id
            );
            continue;
        };

        if child_node.proto != EGDFProto::Block {
            klog_warn!(
                Driver,
                "EGDF: composite child '{}' is not a block device, skipping",
                child_node.name
            );
            continue;
        }

        let idx: usize = {
            if let Some(dev_id) = child_node.device_id {
                if let Some(i) = egdf_find_by_id(dev_id) {
                    i
                } else {
                    klog_warn!(
                        Driver,
                        "EGDF: child '{}' device_id={} not in registry",
                        child_node.name,
                        dev_id
                    );
                    continue;
                }
            } else {
                if let Some(i) = egdf_find_by_name(child_node.name) {
                    i
                } else {
                    klog_warn!(
                        Driver,
                        "EGDF: child '{}' not found in EGDF registry",
                        child_node.name
                    );
                    continue;
                }
            }
        };

        if idx > u8::MAX as usize {
            klog_warn!(
                Driver,
                "EGDF: child '{}' index {} exceeds u8 range, skipping",
                child_node.name,
                idx
            );
            continue;
        }

        child_drives.push(crate::privileged::lib::low_u8_from_usize(idx));
    }

    child_drives
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 入口: 探测并注册设备树中的复合块设备, 返回创建数量.
pub extern "C" fn composite_probe() -> u32 {
    crate::privileged::lib::low_u32_from_usize(devtree_probe_composites())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raid0_sector_mapping() {
        let dev = CompositeBlockDevice {
            device_type: CompositeType::Raid0,
            child_drives: [0, 1, 0, 0, 0, 0, 0, 0],
            child_count: 2,
            stripe_sectors: 8,
            total_sectors: 16,
            read_round_robin: AtomicU32::new(0),
        };

        assert_eq!(dev.map_raid0_sector(0), (0, 0));
        assert_eq!(dev.map_raid0_sector(7), (0, 7));
        assert_eq!(dev.map_raid0_sector(8), (1, 0));
        assert_eq!(dev.map_raid0_sector(15), (1, 7));
    }

    #[test]
    fn test_raid0_sector_beyond_total() {
        let dev = CompositeBlockDevice {
            device_type: CompositeType::Raid0,
            child_drives: [0, 1, 0, 0, 0, 0, 0, 0],
            child_count: 2,
            stripe_sectors: 8,
            total_sectors: 16,
            read_round_robin: AtomicU32::new(0),
        };

        let mut buf = [0u8; 512];
        // 超出 total_sectors 返回 -EINVAL (KernelError::InvalidArgument),
        // 原断言 -1 是旧语义残留 (2026-09-24 UT-06 实测修正).
        assert_eq!(dev.clone().blk_read(16, &mut buf), -22);
    }
}
