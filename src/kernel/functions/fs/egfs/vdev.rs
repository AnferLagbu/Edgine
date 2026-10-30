#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::KernelError;
use crate::privileged::driver::block;
use alloc::vec::Vec;

/// 单个存储池可注册的 vdev 上限.
pub const HV_VDEV_MAX: usize = 8;
/// vdev 类型: 单盘.
pub const HV_VDEV_TYPE_DISK: u8 = 0;
/// vdev 类型: 镜像.
pub const HV_VDEV_TYPE_MIRROR: u8 = 1;
/// vdev 类型: 单校验 RAID-Z.
pub const HV_VDEV_TYPE_RAIDZ1: u8 = 2;
/// vdev 类型: 双校验 RAID-Z.
pub const HV_VDEV_TYPE_RAIDZ2: u8 = 3;
/// vdev 类型: 三校验 RAID-Z.
pub const HV_VDEV_TYPE_RAIDZ3: u8 = 4;
/// vdev 类型: 热备.
pub const HV_VDEV_TYPE_SPARE: u8 = 5;
/// vdev 类型: 独立日志.
pub const HV_VDEV_TYPE_LOG: u8 = 6;
/// vdev 类型: L2 缓存.
pub const HV_VDEV_TYPE_L2CACHE: u8 = 7;

/// 探测不到实际容量时使用的默认 vdev 可分配大小 (32 MiB).
pub const HV_VDEV_ASIZE_DEFAULT: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// vdev 生命周期与健康状态.
pub enum EgfsVdevState {
    /// 未知状态.
    Unknown = 0,
    /// 已关闭.
    Closed = 1,
    /// 离线.
    Offline = 2,
    /// 已移除.
    Removed = 3,
    /// 无法打开.
    CantOpen = 4,
    /// 故障.
    Faulted = 5,
    /// 降级 (部分成员不可用).
    Degraded = 6,
    /// 健康.
    Healthy = 7,
}

impl EgfsVdevState {
    /// 由编码值还原状态, 未知值归一为 `Unknown`.
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Closed,
            2 => Self::Offline,
            3 => Self::Removed,
            4 => Self::CantOpen,
            5 => Self::Faulted,
            6 => Self::Degraded,
            7 => Self::Healthy,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
/// vdev 配置: 标识、类型、路径、扇区位移与子成员数.
pub struct EgfsVdevConfig {
    /// vdev 编号 (单盘时对应驱动器号).
    pub vdev_id: u16,
    /// vdev 类型 (`HV_VDEV_TYPE_*`).
    pub vdev_type: u8,
    /// 全局唯一标识.
    pub guid: u64,
    /// 设备路径 (NUL 填充的定长字节数组).
    pub path: [u8; 64],
    /// 扇区大小位移 (log2 字节数).
    pub ashift: u8,
    /// 可分配大小 (字节).
    pub asize: u64,
    /// 校验盘数量.
    pub nparity: u8,
    /// 子 vdev 数量.
    pub children: u16,
    /// 是否作为独立日志设备.
    pub is_log: bool,
    /// 扇区总数.
    pub sector_count: u64,
    /// 分区起始扇区.
    pub partition_start: u32,
}

impl EgfsVdevConfig {
    /// 构造单盘 vdev 配置, 初始容量取默认值、扇区数待探测.
    #[must_use]
    pub fn new_disk(vdev_id: u16, path: &str, ashift: u8) -> Self {
        let mut p = [0u8; 64];
        let b = path.as_bytes();
        let len = b.len().min(63);
        p[..len].copy_from_slice(&b[..len]);
        Self {
            vdev_id,
            vdev_type: HV_VDEV_TYPE_DISK,
            guid: 0,
            path: p,
            ashift,
            asize: HV_VDEV_ASIZE_DEFAULT,
            nparity: 0,
            children: 1,
            is_log: false,
            sector_count: 0,
            partition_start: 0,
        }
    }
}

/// vdev 运行时实例: 配置、健康状态、子成员关系与读写计数.
pub struct EgfsVdev {
    /// 静态配置.
    pub config: EgfsVdevConfig,
    /// 当前状态.
    pub state: EgfsVdevState,
    /// 父 vdev 编号.
    pub parent_id: Option<u16>,
    /// 子 vdev 编号列表.
    pub child_ids: Vec<u16>,
    /// 元数据区数量.
    pub ms_count: u32,
    /// 已激活的元数据区数量.
    pub ms_active: u32,
    /// 是否正在初始化.
    pub initializing: bool,
    /// 是否支持 TRIM.
    pub trim_supported: bool,
    /// 累计读次数.
    pub total_reads: u64,
    /// 累计写次数.
    pub total_writes: u64,
}

// SAFETY: EgfsVdev contains only Copy/primitive types and Mutex-protected fields.
// SAFETY (Framekernel P2.2.2): EgfsVdev 全部字段 (Mutex<T>, Atomic*, [T; N]) 自动 Send + Sync。

impl EgfsVdev {
    /// 以配置构造 vdev, 初始状态为已关闭.
    #[must_use]
    pub fn new(config: EgfsVdevConfig) -> Self {
        Self {
            config,
            state: EgfsVdevState::Closed,
            parent_id: None,
            child_ids: Vec::new(),
            ms_count: 0,
            ms_active: 0,
            initializing: false,
            trim_supported: false,
            total_reads: 0,
            total_writes: 0,
        }
    }

    /// 探测指定驱动器的容量 (字节): 优先读注册表, 回退二分扫描, 失败取默认值.
    #[must_use]
    pub fn probe_disk_size(drive: u8) -> u64 {
        if !block::hdd_is_present(drive) {
            return 0;
        }

        // 优先采用 BlockDevice 的 total_sectors (避免二分查找)
        let from_registry = block::hdd_total_sectors(drive);
        if from_registry > 0 {
            return from_registry * 512;
        }

        // 回退方案: 二分查找最高可读扇区
        let mut lo: u32 = 0;
        let mut hi: u32 = 0xFFFF;
        let mut buf = [0u8; 512];
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if block::hdd_read_sector(drive, u64::from(mid), &mut buf) >= 0 {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return HV_VDEV_ASIZE_DEFAULT;
        }
        let detected = u64::from(lo) * 512;
        if detected > 0 {
            detected
        } else {
            HV_VDEV_ASIZE_DEFAULT
        }
    }

    /// 是否处于健康状态.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.state == EgfsVdevState::Healthy
    }

    /// 是否可读写 (健康或降级).
    #[must_use]
    pub fn is_available(&self) -> bool {
        matches!(self.state, EgfsVdevState::Healthy | EgfsVdevState::Degraded)
    }

    /// 打开 vdev: 单盘探测到设备则置健康并回填容量/扇区数, 否则置无法打开.
    pub fn open(&mut self) {
        if self.config.vdev_type == HV_VDEV_TYPE_DISK {
            let present = block::hdd_is_present(crate::functions::intcast::low_u8_from_u16(
                self.config.vdev_id,
            ));
            if present {
                self.state = EgfsVdevState::Healthy;
                if self.config.asize == 0 {
                    self.config.asize = Self::probe_disk_size(
                        crate::functions::intcast::low_u8_from_u16(self.config.vdev_id),
                    );
                }
                if self.config.asize > 0 {
                    self.config.sector_count = self.config.asize / 512;
                }
            } else {
                self.state = EgfsVdevState::CantOpen;
            }
        }
    }

    /// 按扇区读取数据 (扣除分区偏移), 缓冲区不足返回 `InvalidArgument`.
    pub fn read_sectors(&mut self, sector: u64, count: u32, buf: &mut [u8]) -> i32 {
        let need_bytes = u64::from(count) * 512;
        if buf.len() < crate::functions::intcast::usize_from_u64(need_bytes) {
            return KernelError::InvalidArgument.as_i32();
        }
        let part_start = u64::from(self.config.partition_start);
        for i in 0..count {
            let offset = crate::functions::intcast::usize_from_u64(u64::from(i) * 512);
            if offset + 512 > buf.len() {
                break;
            }
            let result = block::hdd_read_sector(
                crate::functions::intcast::low_u8_from_u16(self.config.vdev_id),
                part_start + sector + u64::from(i),
                &mut buf[offset..],
            );
            if result < 0 {
                return result;
            }
        }
        self.total_reads += 1;
        0
    }

    /// 按扇区写入数据 (扣除分区偏移), 缓冲区不足返回 `InvalidArgument`.
    pub fn write_sectors(&mut self, sector: u64, count: u32, buf: &[u8]) -> i32 {
        let need_bytes = u64::from(count) * 512;
        if buf.len() < crate::functions::intcast::usize_from_u64(need_bytes) {
            return KernelError::InvalidArgument.as_i32();
        }
        let part_start = u64::from(self.config.partition_start);
        for i in 0..count {
            let offset = crate::functions::intcast::usize_from_u64(u64::from(i) * 512);
            if offset + 512 > buf.len() {
                break;
            }
            let result = block::hdd_write_sector(
                crate::functions::intcast::low_u8_from_u16(self.config.vdev_id),
                part_start + sector + u64::from(i),
                &buf[offset..],
            );
            if result < 0 {
                return result;
            }
        }
        self.total_writes += 1;
        0
    }
}
