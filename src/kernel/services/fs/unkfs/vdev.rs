#![deny(unsafe_code)]
use crate::framework::driver::block;
use crate::services::fs::KernelError;
use alloc::vec::Vec;

pub const HV_VDEV_MAX: usize = 8;
pub const HV_VDEV_TYPE_DISK: u8 = 0;
pub const HV_VDEV_TYPE_MIRROR: u8 = 1;
pub const HV_VDEV_TYPE_RAIDZ1: u8 = 2;
pub const HV_VDEV_TYPE_RAIDZ2: u8 = 3;
pub const HV_VDEV_TYPE_RAIDZ3: u8 = 4;
pub const HV_VDEV_TYPE_SPARE: u8 = 5;
pub const HV_VDEV_TYPE_LOG: u8 = 6;
pub const HV_VDEV_TYPE_L2CACHE: u8 = 7;

pub const HV_VDEV_ASIZE_DEFAULT: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestVdevState {
    Unknown = 0,
    Closed = 1,
    Offline = 2,
    Removed = 3,
    CantOpen = 4,
    Faulted = 5,
    Degraded = 6,
    Healthy = 7,
}

impl NestVdevState {
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
pub struct NestVdevConfig {
    pub vdev_id: u16,
    pub vdev_type: u8,
    pub guid: u64,
    pub path: [u8; 64],
    pub ashift: u8,
    pub asize: u64,
    pub nparity: u8,
    pub children: u16,
    pub is_log: bool,
    pub sector_count: u64,
    pub partition_start: u32,
}

impl NestVdevConfig {
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

pub struct NestVdev {
    pub config: NestVdevConfig,
    pub state: NestVdevState,
    pub parent_id: Option<u16>,
    pub child_ids: Vec<u16>,
    pub ms_count: u32,
    pub ms_active: u32,
    pub initializing: bool,
    pub trim_supported: bool,
    pub total_reads: u64,
    pub total_writes: u64,
}

// SAFETY: NestVdev contains only Copy/primitive types and Mutex-protected fields.
// SAFETY (Framekernel P2.2.2): NestVdev 全部字段 (Mutex<T>, Atomic*, [T; N]) 自动 Send + Sync。

impl NestVdev {
    pub fn new(config: NestVdevConfig) -> Self {
        Self {
            config,
            state: NestVdevState::Closed,
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

    pub fn is_healthy(&self) -> bool {
        self.state == NestVdevState::Healthy
    }

    pub fn is_available(&self) -> bool {
        matches!(self.state, NestVdevState::Healthy | NestVdevState::Degraded)
    }

    pub fn open(&mut self) {
        if self.config.vdev_type == HV_VDEV_TYPE_DISK {
            let present = block::hdd_is_present(self.config.vdev_id as u8);
            if present {
                self.state = NestVdevState::Healthy;
                if self.config.asize == 0 {
                    self.config.asize = Self::probe_disk_size(self.config.vdev_id as u8);
                }
                if self.config.asize > 0 {
                    self.config.sector_count = self.config.asize / 512;
                }
            } else {
                self.state = NestVdevState::CantOpen;
            }
        }
    }

    pub fn read_sectors(&mut self, sector: u64, count: u32, buf: &mut [u8]) -> i32 {
        let need_bytes = u64::from(count) * 512;
        if buf.len() < need_bytes as usize {
            return KernelError::InvalidArgument.as_i32();
        }
        let mut offset = 0usize;
        let mut sec = sector;
        let part_start = u64::from(self.config.partition_start);
        for _ in 0..count {
            if offset + 512 > buf.len() {
                break;
            }
            let result = block::hdd_read_sector(
                self.config.vdev_id as u8,
                part_start + sec,
                &mut buf[offset..],
            );
            if result < 0 {
                return result;
            }
            sec += 1;
            offset += 512;
        }
        self.total_reads += 1;
        0
    }

    pub fn write_sectors(&mut self, sector: u64, count: u32, buf: &[u8]) -> i32 {
        let need_bytes = u64::from(count) * 512;
        if buf.len() < need_bytes as usize {
            return KernelError::InvalidArgument.as_i32();
        }
        let mut offset = 0usize;
        let mut sec = sector;
        let part_start = u64::from(self.config.partition_start);
        for _ in 0..count {
            if offset + 512 > buf.len() {
                break;
            }
            let result = block::hdd_write_sector(
                self.config.vdev_id as u8,
                part_start + sec,
                &buf[offset..],
            );
            if result < 0 {
                return result;
            }
            sec += 1;
            offset += 512;
        }
        self.total_writes += 1;
        0
    }
}
