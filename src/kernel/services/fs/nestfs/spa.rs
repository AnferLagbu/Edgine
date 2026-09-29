#![deny(unsafe_code)]
use crate::framework::driver::block;
use crate::services::fs::KernelError;
use crate::services::fs::nestfs::arc::NestArc;
use crate::services::fs::nestfs::bp::{HV_DVA_MAX, NestBlockPointer, NestCksumType, NestCompType};
use crate::services::fs::nestfs::checksum::NestChecksum;
use crate::services::fs::nestfs::dva::NestDva;
use crate::services::fs::nestfs::metaslab::NestMetaslab;
use crate::services::fs::nestfs::vdev::{NestVdev, NestVdevConfig, NestVdevState};
use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

pub const HV_SPA_MAGIC: u32 = 0x48564653;
pub const HV_UBERBLOCK_COUNT: usize = 128;
pub const HV_UBERBLOCK_SECTOR: u32 = 0;
pub const HV_VDEV_LABEL_SIZE: u64 = 262144;
pub const HV_POOL_MAX_NAME: usize = 64;

pub const HV_POOL_BLOCK_SIZE: u64 = 4096;
pub const HV_POOL_METASLAB_SHIFT: u8 = 24;
pub const HV_POOL_METASLAB_SIZE: u64 = 1 << HV_POOL_METASLAB_SHIFT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestPoolState {
    Uninit = 0,
    Active = 1,
    Exported = 2,
    Destroyed = 3,
    Suspended = 4,
    ReadOnly = 5,
}

#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
#[repr(C)]
pub struct NestUberblock {
    pub txg: u64,
    pub root_bp: NestBlockPointer,
    pub timestamp: u64,
    pub root_dataset_obj: u64,
    pub pool_guid: u64,
    pub checkpoint_txg: u64,
    pub checksum: [u64; 4],
    pub magic: u32,
    pub pwm_domain_id: u16,
    pub _pad: [u8; 2],
}

impl NestUberblock {
    pub const fn null() -> Self {
        Self {
            txg: 0,
            root_bp: NestBlockPointer::null(),
            timestamp: 0,
            root_dataset_obj: 0,
            pool_guid: 0,
            checkpoint_txg: 0,
            checksum: [0; 4],
            magic: 0,
            pwm_domain_id: 0,
            _pad: [0; 2],
        }
    }

    pub fn is_valid(&self) -> bool {
        self.magic == HV_SPA_MAGIC
    }

    pub fn compute_checksum(&mut self) {
        self.checksum = [0; 4];
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, self.as_bytes());
        self.checksum = ck.value;
    }

    pub fn verify_checksum(&self) -> bool {
        let mut copy = *self;
        let saved = copy.checksum;
        copy.checksum = [0; 4];
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, copy.as_bytes());
        ck.value == saved
    }

    /// E6-6: 使用 `IntoBytes` + Immutable derive 编译期验证无 padding, `as_bytes` 为 safe 方法
    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }

    /// E6-6: safe 反序列化, 逐字段读取替代 unsafe `read_unaligned`
    pub fn from_bytes_unaligned(bytes: &[u8]) -> Option<Self> {
        let size = core::mem::size_of::<Self>();
        if bytes.len() < size {
            return None;
        }
        let mut off = 0usize;
        let txg = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        off += 8;
        let root_bp = NestBlockPointer::from_bytes(&bytes[off..off + NestBlockPointer::BYTES])?;
        off += NestBlockPointer::BYTES;
        let timestamp = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        off += 8;
        let root_dataset_obj = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        off += 8;
        let pool_guid = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        off += 8;
        let checkpoint_txg = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        off += 8;
        let mut checksum = [0u64; 4];
        for i in 0..4 {
            checksum[i] = u64::from_le_bytes(bytes[off + i * 8..off + i * 8 + 8].try_into().ok()?);
        }
        off += 32;
        let magic = u32::from_le_bytes(bytes[off..off + 4].try_into().ok()?);
        off += 4;
        let pwm_domain_id = u16::from_le_bytes(bytes[off..off + 2].try_into().ok()?);
        Some(Self {
            txg,
            root_bp,
            timestamp,
            root_dataset_obj,
            pool_guid,
            checkpoint_txg,
            checksum,
            magic,
            pwm_domain_id,
            _pad: [0; 2],
        })
    }
}

const _UBERBLOCK_MAX_SIZE: usize = 512;
const _ASSERT_UBERBLOCK_FITS: () =
    assert!(core::mem::size_of::<NestUberblock>() <= _UBERBLOCK_MAX_SIZE);

pub struct NestSpaConfig {
    pub name: [u8; HV_POOL_MAX_NAME],
    pub guid: u64,
    pub ashift: u8,
    pub block_size: u32,
    pub max_vdevs: u16,
    pub readonly: bool,
}

impl NestSpaConfig {
    pub fn new(name: &str) -> Self {
        let mut n = [0u8; HV_POOL_MAX_NAME];
        let b = name.as_bytes();
        let len = b.len().min(HV_POOL_MAX_NAME - 1);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            name: n,
            guid: 0,
            ashift: 12,
            block_size: HV_POOL_BLOCK_SIZE as u32,
            max_vdevs: 8,
            readonly: false,
        }
    }
}

pub struct NestSpa {
    pub config: Mutex<NestSpaConfig>,
    pub state: AtomicU8,
    pub uberblock: Mutex<NestUberblock>,
    pub vdevs: Mutex<Vec<NestVdev>>,
    pub metaslabs: Mutex<Vec<NestMetaslab>>,
    pub arc: NestArc,
    pub txg_current: AtomicU64,
    pub txg_syncing: AtomicBool,
    pub alloc_count: AtomicU64,
    pub free_count: AtomicU64,
    pub read_count: AtomicU64,
    pub write_count: AtomicU64,
    pub initialized: AtomicBool,
    pub disk_present: AtomicBool,
    pub formatted: AtomicBool,
    pub last_sync_time: AtomicU64,
    pub scrub_in_progress: AtomicBool,
    pub scrub_last_txg: AtomicU64,
    pub partition_start: AtomicU32,
}

// SAFETY (Framekernel P2.2.2): NestSpa 全部字段 (Mutex<T>, Atomic*, Vec) 自动 Send + Sync。

impl NestSpa {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(NestSpaConfig::new("")),
            state: AtomicU8::new(NestPoolState::Uninit as u8),
            uberblock: Mutex::new(NestUberblock::null()),
            vdevs: Mutex::new(Vec::new()),
            metaslabs: Mutex::new(Vec::new()),
            arc: NestArc::new(),
            txg_current: AtomicU64::new(0),
            txg_syncing: AtomicBool::new(false),
            alloc_count: AtomicU64::new(0),
            free_count: AtomicU64::new(0),
            read_count: AtomicU64::new(0),
            write_count: AtomicU64::new(0),
            initialized: AtomicBool::new(false),
            disk_present: AtomicBool::new(false),
            formatted: AtomicBool::new(false),
            last_sync_time: AtomicU64::new(0),
            scrub_in_progress: AtomicBool::new(false),
            scrub_last_txg: AtomicU64::new(0),
            partition_start: AtomicU32::new(0),
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    fn generate_guid() -> u64 {
        let t = crate::arch!(timestamp());
        let mut h: u64 = 14695981039346656037;
        h ^= t;
        h = h.wrapping_mul(1099511628211);
        h ^= t.rotate_left(17);
        h = h.wrapping_mul(1099511628211);
        h | 1
    }

    /// 获取第一个 vdev 的磁盘 ID (无 vdev 时返回 0)
    fn vdev_0_drive_id(&self) -> u8 {
        self.vdevs
            .lock()
            .first()
            .map_or(0, |v| v.config.vdev_id as u8)
    }

    fn read_sector(&self, sector: u32, buf: &mut [u8]) -> i32 {
        if buf.len() < 512 {
            return KernelError::InvalidArgument.as_i32();
        }
        let phys = sector + self.partition_start.load(Ordering::Acquire);
        let drive = self.vdev_0_drive_id();
        block::hdd_read_sector(drive, u64::from(phys), buf)
    }

    fn write_sector(&self, sector: u32, buf: &[u8]) -> i32 {
        if buf.len() < 512 {
            return KernelError::InvalidArgument.as_i32();
        }
        let phys = sector + self.partition_start.load(Ordering::Acquire);
        let drive = self.vdev_0_drive_id();
        block::hdd_write_sector(drive, u64::from(phys), buf)
    }

    pub fn init(&self, name: &str) {
        {
            let mut cfg = self.config.lock();
            let mut new_cfg = NestSpaConfig::new(name);
            new_cfg.guid = Self::generate_guid();
            *cfg = new_cfg;
        }
        self.arc.init(256);
        self.txg_current.store(1, Ordering::Release);
        self.state
            .store(NestPoolState::Active as u8, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
        {
            let mut ub = self.uberblock.lock();
            ub.magic = HV_SPA_MAGIC;
            ub.txg = 1;
            ub.pool_guid = self.config.lock().guid;
            ub.root_dataset_obj = 0;
        }
    }

    pub fn add_vdev(&self, config: NestVdevConfig) -> bool {
        let mut vdevs = self.vdevs.lock();
        let max_vdevs = self.config.lock().max_vdevs;
        if vdevs.len() >= max_vdevs as usize {
            return false;
        }
        let mut vdev = NestVdev::new(config);
        vdev.state = NestVdevState::Healthy;
        let vdev_id = vdev.config.vdev_id;
        let asize = vdev.config.asize;
        vdevs.push(vdev);
        drop(vdevs);
        if asize > 0 {
            let mut ms_list = self.metaslabs.lock();
            let n_ms = asize.div_ceil(HV_POOL_METASLAB_SIZE) as u32;
            for i in 0..n_ms {
                let ms_start = u64::from(i) * HV_POOL_METASLAB_SIZE + HV_VDEV_LABEL_SIZE;
                let ms_size = if i < n_ms - 1 {
                    HV_POOL_METASLAB_SIZE
                } else {
                    asize - ms_start + HV_VDEV_LABEL_SIZE
                };
                let ms = NestMetaslab::new(ms_list.len() as u32, vdev_id, ms_start, ms_size);
                ms_list.push(ms);
            }
        }
        true
    }

    pub fn allocate(
        &self,
        size: u64,
        kind: NestCksumType,
        comp: NestCompType,
        txg: u64,
    ) -> Option<NestBlockPointer> {
        let rounded = size.div_ceil(HV_POOL_BLOCK_SIZE) * HV_POOL_BLOCK_SIZE;
        let mut ms_list = self.metaslabs.lock();
        let mut best_vdev_id: u16 = 0;
        let mut best_weight: u64 = 0;
        let mut best_ms_idx: Option<usize> = None;
        for (i, ms) in ms_list.iter().enumerate() {
            if !ms.is_available() {
                continue;
            }
            if ms.free_space.load(Ordering::Relaxed) < rounded {
                continue;
            }
            if ms.weight > best_weight {
                best_weight = ms.weight;
                best_vdev_id = ms.vdev_id;
                best_ms_idx = Some(i);
            }
        }
        let ms_idx = best_ms_idx?;
        let offset = ms_list[ms_idx].alloc(rounded)?;
        drop(ms_list);
        let dva = NestDva::new(best_vdev_id, offset, rounded as u32);
        let mut bp = NestBlockPointer::null();
        bp.set_dva(0, dva);
        bp.prop.set_cksum_type(kind);
        bp.prop.set_comp_type(comp);
        bp.prop.logical_size = size as u32;
        bp.prop.physical_size = rounded as u32;
        bp.set_birth(txg);
        self.alloc_count.fetch_add(1, Ordering::Relaxed);
        Some(bp)
    }

    pub fn free(&self, bp: &NestBlockPointer, _txg: u64) {
        for i in 0..HV_DVA_MAX {
            if let Some(dva) = bp.get_dva(i) {
                let mut ms_list = self.metaslabs.lock();
                for ms in ms_list.iter_mut() {
                    if ms.vdev_id == dva.vdev_id {
                        let rel = dva.offset.saturating_sub(ms.start);
                        if rel < ms.size {
                            ms.free(dva.offset, u64::from(dva.asize));
                            break;
                        }
                    }
                }
            }
        }
        self.free_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn read_bp(&self, bp: &NestBlockPointer, buf: &mut [u8]) -> i32 {
        for i in 0..HV_DVA_MAX {
            if let Some(dva) = bp.get_dva(i) {
                let mut vdevs = self.vdevs.lock();
                if let Some(vdev) = vdevs.iter_mut().find(|v| v.config.vdev_id == dva.vdev_id) {
                    let sector = dva.offset / 512;
                    let count = dva.asize.div_ceil(512);
                    let result = vdev.read_sectors(sector, count, buf);
                    if result == 0 {
                        self.read_count.fetch_add(1, Ordering::Relaxed);
                        return 0;
                    }
                }
            }
        }
        -1
    }

    pub fn write_bp(&self, bp: &NestBlockPointer, buf: &[u8]) -> i32 {
        for i in 0..HV_DVA_MAX {
            if let Some(dva) = bp.get_dva(i) {
                let mut vdevs = self.vdevs.lock();
                if let Some(vdev) = vdevs.iter_mut().find(|v| v.config.vdev_id == dva.vdev_id) {
                    let sector = dva.offset / 512;
                    let count = dva.asize.div_ceil(512);
                    let result = vdev.write_sectors(sector, count, buf);
                    if result != 0 {
                        return result;
                    }
                }
            }
        }
        self.write_count.fetch_add(1, Ordering::Relaxed);
        0
    }

    pub fn write_uberblock_to_disk(&self) {
        let ub = self.uberblock.lock();
        if !ub.is_valid() {
            return;
        }
        let mut copy = *ub;
        copy.compute_checksum();
        let ub_bytes = copy.as_bytes();
        let ub_sector =
            (self.txg_current.load(Ordering::Relaxed) as u32) % HV_UBERBLOCK_COUNT as u32;
        let sector = HV_UBERBLOCK_SECTOR + ub_sector;
        let mut sector_buf = [0u8; 512];
        let copy_len = ub_bytes.len().min(512);
        sector_buf[..copy_len].copy_from_slice(&ub_bytes[..copy_len]);
        let _ = self.write_sector(sector, &sector_buf);
        self.last_sync_time
            .store(crate::arch!(timestamp()), Ordering::Relaxed);
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn read_uberblock_from_disk(&self) -> Option<NestUberblock> {
        for i in (0..HV_UBERBLOCK_COUNT as u32).rev() {
            let sector = HV_UBERBLOCK_SECTOR + i;
            let mut sector_buf = [0u8; 512];
            if self.read_sector(sector, &mut sector_buf) != 0 {
                continue;
            }
            let ub = match NestUberblock::from_bytes_unaligned(&sector_buf) {
                Some(u) => u,
                None => continue,
            };
            if ub.is_valid() && ub.verify_checksum() {
                return Some(ub);
            }
        }
        None
    }

    pub fn sync_uberblock(&self) {
        self.write_uberblock_to_disk();
    }

    pub fn get_stats(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.alloc_count.load(Ordering::Relaxed),
            self.free_count.load(Ordering::Relaxed),
            self.read_count.load(Ordering::Relaxed),
            self.write_count.load(Ordering::Relaxed),
            self.txg_current.load(Ordering::Relaxed),
        )
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    pub fn is_disk_present(&self) -> bool {
        self.disk_present.load(Ordering::Acquire)
    }

    pub fn is_formatted(&self) -> bool {
        self.formatted.load(Ordering::Acquire)
    }

    pub fn advance_txg(&self) -> u64 {
        self.txg_current.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn current_txg(&self) -> u64 {
        self.txg_current.load(Ordering::Acquire)
    }
}
