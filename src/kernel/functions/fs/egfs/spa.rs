#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::KernelError;
use crate::functions::fs::egfs::arc::EgfsArc;
use crate::functions::fs::egfs::bp::{EgfsBlockPointer, EgfsCksumType, EgfsCompType, HV_DVA_MAX};
use crate::functions::fs::egfs::checksum::EgfsChecksum;
use crate::functions::fs::egfs::dva::EgfsDva;
use crate::functions::fs::egfs::metaslab::EgfsMetaslab;
use crate::functions::fs::egfs::vdev::{EgfsVdev, EgfsVdevConfig, EgfsVdevState};
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use crate::privileged::driver::block;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// SPA uberblock 魔数 (小端 "HVSF"), 用于识别本池的根元数据块.
pub const HV_SPA_MAGIC: u32 = 0x4856_4653;
/// 每个 vdev 标签内的 uberblock 槽位数, 写出时按 txg 取模环形复用.
pub const HV_UBERBLOCK_COUNT: usize = 128;
/// uberblock 区的起始扇区号.
pub const HV_UBERBLOCK_SECTOR: u32 = 0;
/// vdev 标签区字节数, 位于 metaslab 分配区之前.
pub const HV_VDEV_LABEL_SIZE: u64 = 262_144;
/// 池名缓冲区长度上限 (含结尾 NUL).
pub const HV_POOL_MAX_NAME: usize = 64;

/// 池内块大小的基线, 分配尺寸按此对齐.
pub const HV_POOL_BLOCK_SIZE: u64 = 4096;
/// 单个 metaslab 大小的移位量 (2^24 = 16 MiB).
pub const HV_POOL_METASLAB_SHIFT: u8 = 24;
/// 单个 metaslab 的字节数.
pub const HV_POOL_METASLAB_SIZE: u64 = 1 << HV_POOL_METASLAB_SHIFT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// 池的生命周期状态.
pub enum EgfsPoolState {
    /// 池尚未初始化.
    Uninit = 0,
    /// 池已导入并正常服务.
    Active = 1,
    /// 池已导出, 可在其他系统重新导入.
    Exported = 2,
    /// 池已销毁, 数据不可再使用.
    Destroyed = 3,
    /// 因 I/O 错误或管理操作暂停服务.
    Suspended = 4,
    /// 只读模式, 拒绝一切写入.
    ReadOnly = 5,
}

#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
#[repr(C)]
/// 池的根元数据块, 记录最近一次成功事务组的位置与校验信息.
pub struct EgfsUberblock {
    /// 该 uberblock 对应的事务组编号.
    pub txg: u64,
    /// 指向对象集根块树的块指针.
    pub root_bp: EgfsBlockPointer,
    /// 写入该 uberblock 的时间戳 (秒).
    pub timestamp: u64,
    /// 根数据集对象号.
    pub root_dataset_obj: u64,
    /// 池的全局唯一标识.
    pub pool_guid: u64,
    /// 最近一次 checkpoint 的事务组编号; 0 表示无.
    pub checkpoint_txg: u64,
    /// 覆盖本块其余字段的 4×64 位 Fletcher4 校验和.
    pub checksum: [u64; 4],
    /// 魔数; 等于 `HV_SPA_MAGIC` 时才视为有效.
    pub magic: u32,
    /// 归属的 PWM (功率/写管理) 域编号.
    pub pwm_domain_id: u16,
    /// 对齐填充, 使块尾保持 8 字节边界.
    pub pad: [u8; 2],
}

impl EgfsUberblock {
    /// 构造全零的无效 uberblock (magic 为 0).
    #[must_use]
    pub const fn null() -> Self {
        Self {
            txg: 0,
            root_bp: EgfsBlockPointer::null(),
            timestamp: 0,
            root_dataset_obj: 0,
            pool_guid: 0,
            checkpoint_txg: 0,
            checksum: [0; 4],
            magic: 0,
            pwm_domain_id: 0,
            pad: [0; 2],
        }
    }

    /// 魔数匹配时返回 `true`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.magic == HV_SPA_MAGIC
    }

    /// 以 Fletcher4 重算校验和并写回 (计算前先把校验和字段清零).
    pub fn compute_checksum(&mut self) {
        self.checksum = [0; 4];
        let ck = EgfsChecksum::compute(EgfsCksumType::Fletcher4, self.as_bytes());
        self.checksum = ck.value;
    }

    /// 按存储的校验和重算比对, 一致时返回 `true`.
    #[must_use]
    pub fn verify_checksum(&self) -> bool {
        let mut copy = *self;
        let saved = copy.checksum;
        copy.checksum = [0; 4];
        let ck = EgfsChecksum::compute(EgfsCksumType::Fletcher4, copy.as_bytes());
        ck.value == saved
    }

    /// E6-6: 使用 `IntoBytes` + Immutable derive 编译期验证无 padding, `as_bytes` 为 safe 方法
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }

    /// E6-6: safe 反序列化, 逐字段读取替代 unsafe `read_unaligned`
    #[must_use]
    pub fn from_bytes_unaligned(bytes: &[u8]) -> Option<Self> {
        let size = core::mem::size_of::<Self>();
        if bytes.len() < size {
            return None;
        }
        let mut off = 0usize;
        let txg = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        off += 8;
        let root_bp = EgfsBlockPointer::from_bytes(&bytes[off..off + EgfsBlockPointer::BYTES])?;
        off += EgfsBlockPointer::BYTES;
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
            pad: [0; 2],
        })
    }
}

// 编译期断言: EgfsUberblock 必须装入 512 字节的 uberblock 区 (UBERBLOCK_MAX_SIZE).
const _: () = assert!(core::mem::size_of::<EgfsUberblock>() <= 512);

/// 池的静态配置: 名称、GUID、块尺寸与只读标志.
pub struct EgfsSpaConfig {
    /// 池名, NUL 结尾.
    pub name: [u8; HV_POOL_MAX_NAME],
    /// 池的唯一标识; 0 表示尚未生成.
    pub guid: u64,
    /// 块大小移位量 (2^ashift 为最小块字节数).
    pub ashift: u8,
    /// 逻辑块字节数.
    pub block_size: u32,
    /// 允许加入的 vdev 数量上限.
    pub max_vdevs: u16,
    /// 是否以只读方式导入.
    pub readonly: bool,
}

impl EgfsSpaConfig {
    /// 由名称构造默认配置 (ashift=12, 4 KiB 块, 最多 8 个 vdev).
    #[must_use]
    pub fn new(name: &str) -> Self {
        let mut n = [0u8; HV_POOL_MAX_NAME];
        let b = name.as_bytes();
        let len = b.len().min(HV_POOL_MAX_NAME - 1);
        n[..len].copy_from_slice(&b[..len]);
        Self {
            name: n,
            guid: 0,
            ashift: 12,
            block_size: crate::functions::intcast::low_u32_from_u64(HV_POOL_BLOCK_SIZE),
            max_vdevs: 8,
            readonly: false,
        }
    }
}

/// 存储池抽象 (SPA), 聚合配置、vdev、metaslab、ARC 与事务组状态.
pub struct EgfsSpa {
    /// 池配置, 由自旋锁保护.
    pub config: Mutex<EgfsSpaConfig>,
    /// 池状态 (`EgfsPoolState` 的 u8 编码).
    pub state: AtomicU8,
    /// 当前生效的根元数据块.
    pub uberblock: Mutex<EgfsUberblock>,
    /// 已加入的虚拟设备列表.
    pub vdevs: Mutex<Vec<EgfsVdev>>,
    /// 全部 metaslab 分配单元.
    pub metaslabs: Mutex<Vec<EgfsMetaslab>>,
    /// 该池的读缓存.
    pub arc: EgfsArc,
    /// 当前事务组编号.
    pub txg_current: AtomicU64,
    /// 是否正在同步事务组.
    pub txg_syncing: AtomicBool,
    /// 累计分配次数.
    pub alloc_count: AtomicU64,
    /// 累计释放次数.
    pub free_count: AtomicU64,
    /// 累计块读取次数.
    pub read_count: AtomicU64,
    /// 累计块写入次数.
    pub write_count: AtomicU64,
    /// 池是否已完成初始化.
    pub initialized: AtomicBool,
    /// 底层磁盘是否存在.
    pub disk_present: AtomicBool,
    /// 磁盘上是否已有有效池格式.
    pub formatted: AtomicBool,
    /// 最近一次 uberblock 落盘的时间戳.
    pub last_sync_time: AtomicU64,
    /// 是否正在执行 scrub.
    pub scrub_in_progress: AtomicBool,
    /// 最近一次 scrub 覆盖的事务组.
    pub scrub_last_txg: AtomicU64,
    /// 池所在分区的起始扇区偏移.
    pub partition_start: AtomicU32,
}

// SAFETY (Framekernel P2.2.2): EgfsSpa 全部字段 (Mutex<T>, Atomic*, Vec) 自动 Send + Sync。

impl EgfsSpa {
    /// 构造未初始化的空池: 状态 Uninit, 计数与时间戳归零.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Mutex::new(EgfsSpaConfig::new("")),
            state: AtomicU8::new(EgfsPoolState::Uninit as u8),
            uberblock: Mutex::new(EgfsUberblock::null()),
            vdevs: Mutex::new(Vec::new()),
            metaslabs: Mutex::new(Vec::new()),
            arc: EgfsArc::new(),
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

    fn generate_guid() -> u64 {
        let t = crate::arch!(timestamp());
        let mut h: u64 = 14_695_981_039_346_656_037;
        h ^= t;
        h = h.wrapping_mul(1_099_511_628_211);
        h ^= t.rotate_left(17);
        h = h.wrapping_mul(1_099_511_628_211);
        h | 1
    }

    /// 获取第一个 vdev 的磁盘 ID (无 vdev 时返回 0)
    fn vdev_0_drive_id(&self) -> u8 {
        self.vdevs.lock().first().map_or(0, |v| {
            crate::functions::intcast::low_u8_from_u16(v.config.vdev_id)
        })
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

    /// 以给定名称初始化池: 生成 GUID、激活 ARC、置 Active 并写入首个 uberblock.
    pub fn init(&self, name: &str) {
        {
            let mut cfg = self.config.lock();
            let mut new_cfg = EgfsSpaConfig::new(name);
            new_cfg.guid = Self::generate_guid();
            *cfg = new_cfg;
        }
        self.arc.init(256);
        self.txg_current.store(1, Ordering::Release);
        self.state
            .store(EgfsPoolState::Active as u8, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
        {
            let mut ub = self.uberblock.lock();
            ub.magic = HV_SPA_MAGIC;
            ub.txg = 1;
            ub.pool_guid = self.config.lock().guid;
            ub.root_dataset_obj = 0;
        }
    }

    /// 加入 vdev 并按容量切分 metaslab; 超过 `max_vdevs` 时返回 `false`.
    pub fn add_vdev(&self, config: EgfsVdevConfig) -> bool {
        let mut vdevs = self.vdevs.lock();
        let max_vdevs = self.config.lock().max_vdevs;
        if vdevs.len() >= max_vdevs as usize {
            return false;
        }
        let mut vdev = EgfsVdev::new(config);
        vdev.state = EgfsVdevState::Healthy;
        let vdev_id = vdev.config.vdev_id;
        let asize = vdev.config.asize;
        vdevs.push(vdev);
        drop(vdevs);
        if asize > 0 {
            let mut ms_list = self.metaslabs.lock();
            let n_ms =
                crate::functions::intcast::low_u32_from_u64(asize.div_ceil(HV_POOL_METASLAB_SIZE));
            for i in 0..n_ms {
                let ms_start = u64::from(i) * HV_POOL_METASLAB_SIZE + HV_VDEV_LABEL_SIZE;
                let ms_size = if i < n_ms - 1 {
                    HV_POOL_METASLAB_SIZE
                } else {
                    asize - ms_start + HV_VDEV_LABEL_SIZE
                };
                let ms = EgfsMetaslab::new(
                    crate::functions::intcast::low_u32_from_usize(ms_list.len()),
                    vdev_id,
                    ms_start,
                    ms_size,
                );
                ms_list.push(ms);
            }
        }
        true
    }

    /// 在可用 metaslab 中按权重挑选最合适者, 分配对齐块并返回块指针.
    pub fn allocate(
        &self,
        size: u64,
        kind: EgfsCksumType,
        comp: EgfsCompType,
        txg: u64,
    ) -> Option<EgfsBlockPointer> {
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
        let dva = EgfsDva::new(
            best_vdev_id,
            offset,
            crate::functions::intcast::low_u32_from_u64(rounded),
        );
        let mut bp = EgfsBlockPointer::null();
        bp.set_dva(0, dva);
        bp.prop.set_cksum_type(kind);
        bp.prop.set_comp_type(comp);
        bp.prop.logical_size = crate::functions::intcast::low_u32_from_u64(size);
        bp.prop.physical_size = crate::functions::intcast::low_u32_from_u64(rounded);
        bp.set_birth(txg);
        self.alloc_count.fetch_add(1, Ordering::Relaxed);
        Some(bp)
    }

    /// 按块指针中的 DVA 把空间归还给对应的 metaslab.
    pub fn free(&self, bp: &EgfsBlockPointer, _txg: u64) {
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

    /// 按块指针把数据块读入 `buf`; 成功返回 0, 全部 DVA 失败返回 -1.
    pub fn read_bp(&self, bp: &EgfsBlockPointer, buf: &mut [u8]) -> i32 {
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

    /// 按块指针写入数据块; 成功返回 0, 否则返回底层错误码.
    pub fn write_bp(&self, bp: &EgfsBlockPointer, buf: &[u8]) -> i32 {
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

    /// 计算校验和, 把当前 uberblock 写入环形槽位, 并更新落盘时间戳.
    pub fn write_uberblock_to_disk(&self) {
        let ub = self.uberblock.lock();
        if !ub.is_valid() {
            return;
        }
        let mut copy = *ub;
        copy.compute_checksum();
        let ub_bytes = copy.as_bytes();
        let ub_sector =
            crate::functions::intcast::low_u32_from_u64(self.txg_current.load(Ordering::Relaxed))
                % crate::functions::intcast::low_u32_from_usize(HV_UBERBLOCK_COUNT);
        let sector = HV_UBERBLOCK_SECTOR + ub_sector;
        let mut sector_buf = [0u8; 512];
        let copy_len = ub_bytes.len().min(512);
        sector_buf[..copy_len].copy_from_slice(&ub_bytes[..copy_len]);
        let _ = self.write_sector(sector, &sector_buf);
        self.last_sync_time
            .store(crate::arch!(timestamp()), Ordering::Relaxed);
    }

    /// 从最新槽位向前扫描, 返回首个 magic 与校验和均有效的 uberblock.
    pub fn read_uberblock_from_disk(&self) -> Option<EgfsUberblock> {
        for i in (0..crate::functions::intcast::low_u32_from_usize(HV_UBERBLOCK_COUNT)).rev() {
            let sector = HV_UBERBLOCK_SECTOR + i;
            let mut sector_buf = [0u8; 512];
            if self.read_sector(sector, &mut sector_buf) != 0 {
                continue;
            }
            let Some(ub) = EgfsUberblock::from_bytes_unaligned(&sector_buf) else {
                continue;
            };
            if ub.is_valid() && ub.verify_checksum() {
                return Some(ub);
            }
        }
        None
    }

    /// 把当前 uberblock 同步落盘.
    pub fn sync_uberblock(&self) {
        self.write_uberblock_to_disk();
    }

    /// 返回计数器快照: (分配数, 释放数, 读次数, 写次数, 当前 txg).
    pub fn get_stats(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.alloc_count.load(Ordering::Relaxed),
            self.free_count.load(Ordering::Relaxed),
            self.read_count.load(Ordering::Relaxed),
            self.write_count.load(Ordering::Relaxed),
            self.txg_current.load(Ordering::Relaxed),
        )
    }

    /// 池是否已完成初始化.
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// 底层磁盘是否在位.
    pub fn is_disk_present(&self) -> bool {
        self.disk_present.load(Ordering::Acquire)
    }

    /// 磁盘上是否已有有效的池格式.
    pub fn is_formatted(&self) -> bool {
        self.formatted.load(Ordering::Acquire)
    }

    /// 事务组自增并返回新编号.
    pub fn advance_txg(&self) -> u64 {
        self.txg_current.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// 返回当前事务组编号.
    pub fn current_txg(&self) -> u64 {
        self.txg_current.load(Ordering::Acquire)
    }
}

impl Default for EgfsSpa {
    /// 委托 [`EgfsSpa::new`] 构造未初始化的空池.
    fn default() -> Self {
        Self::new()
    }
}

// DECISION-080: SPA 纯逻辑断言 (配置名 / uberblock) 以本文件源侧 #[cfg(test)] 为唯一归属.
#[cfg(test)]
mod tests {
    use super::*;

    // ==== SPA 配置 / Uberblock ====

    /// 池名写入应以 NUL 截断并保留前缀.
    #[test]
    fn test_spa_config_name() {
        let cfg = EgfsSpaConfig::new("test-pool");
        let name = core::str::from_utf8(&cfg.name)
            .unwrap_or("")
            .trim_end_matches('\0');
        assert!(name.starts_with("test-pool"), "expected test-pool prefix");
    }

    /// 空 uberblock magic 为 0, 应判定无效.
    #[test]
    fn test_spa_uberblock_null() {
        let ub = EgfsUberblock::null();
        assert!(!ub.is_valid(), "null uberblock should be invalid");
    }

    /// 计算校验和后应立即通过自校验.
    #[test]
    fn test_spa_uberblock_checksum() {
        let mut ub = EgfsUberblock {
            txg: 1,
            root_bp: EgfsBlockPointer::null(),
            timestamp: 100,
            root_dataset_obj: 0,
            pool_guid: 0xABCD,
            checkpoint_txg: 0,
            checksum: [0; 4],
            magic: HV_SPA_MAGIC,
            pwm_domain_id: 0,
            pad: [0; 2],
        };
        ub.compute_checksum();
        assert!(ub.verify_checksum(), "checksum should verify");
    }

    /// magic 被篡改后应判定无效.
    #[test]
    fn test_spa_uberblock_invalid_magic() {
        let mut ub = EgfsUberblock::null();
        ub.magic = 0xDEADBEEF;
        assert!(!ub.is_valid(), "wrong magic should be invalid");
    }
}
