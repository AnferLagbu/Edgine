#![deny(unsafe_code)]
#![deny(missing_docs)]
//! 元数据块分配器 (metaslab), 以位图管理一段连续的物理块区间.
//!
//! 每个 metaslab 负责一个 vdev 上的一段连续空间, 通过位图跟踪块的空闲状态,
//! 并以权重 (`weight`) 为上层选择分配目标提供依据。
use crate::functions::fs::unkfs::spa::HV_POOL_BLOCK_SIZE;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// metaslab 中单个块的大小 (字节), 与存储池块大小一致.
pub const HV_MS_BLOCK_SIZE: u64 = HV_POOL_BLOCK_SIZE;
/// 单个 metaslab 可管理的最大块数.
pub const HV_MS_MAX_BLOCKS: u32 = 16384;
/// 块大小对应的位移位数 (14 → 16 KiB).
pub const HV_MS_SHIFT: u8 = 14;

/// metaslab 的运行状态.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnkfsMsState {
    /// 尚未初始化.
    Uninit = 0,
    /// 可正常分配.
    Active = 1,
    /// 空闲空间耗尽.
    Full = 2,
}

/// 元数据块分配器: 管理 `[start, start + size)` 区间内的块位图与统计量.
pub struct UnkfsMetaslab {
    /// metaslab 编号.
    pub id: u32,
    /// 所属 vdev 编号.
    pub vdev_id: u16,
    /// 区间在 vdev 内的起始偏移 (字节).
    pub start: u64,
    /// 区间总大小 (字节).
    pub size: u64,
    /// 当前状态.
    pub state: UnkfsMsState,
    /// 累计已分配字节数.
    pub allocated: AtomicU64,
    /// 累计已释放字节数.
    pub freed: AtomicU64,
    /// 总空间 (字节), 构造后不变, 仅用于统计.
    pub space: AtomicU64,
    /// 当前空闲空间 (字节).
    pub free_space: AtomicU64,
    /// 块位图, 1 表示空闲, 0 表示已占用; 每 `u64` 覆盖 64 块.
    pub bitmap: Vec<u64>,
    /// 位图覆盖的块总数.
    pub max_block: u32,
    /// 分配权重, 空闲空间越多权重越高.
    pub weight: u64,
    /// 是否已加载到内存.
    pub loaded: bool,
    /// 是否正在整理碎片.
    pub condensing: bool,
}

// SAFETY: UnkfsMetaslab uses Mutex for bitmap and plain Copy types for other fields.
// SAFETY (Framekernel P2.2.2): UnkfsMetaslab 全部字段 (Mutex<T>, [T; N]) 自动 Send + Sync。

impl UnkfsMetaslab {
    /// 构造一个处于 [`UnkfsMsState::Active`] 状态的 metaslab, 位图全部标记为空闲.
    #[must_use]
    pub fn new(id: u32, vdev_id: u16, start: u64, size: u64) -> Self {
        let nblocks = crate::functions::intcast::low_u32_from_u64(size / HV_MS_BLOCK_SIZE);
        let bitmap_len = (nblocks as usize).div_ceil(64);
        let mut bitmap = Vec::with_capacity(bitmap_len);
        bitmap.resize(bitmap_len, !0u64);
        let free = size;
        Self {
            id,
            vdev_id,
            start,
            size,
            state: UnkfsMsState::Active,
            allocated: AtomicU64::new(0),
            freed: AtomicU64::new(0),
            space: AtomicU64::new(size),
            free_space: AtomicU64::new(free),
            bitmap,
            max_block: nblocks,
            weight: size,
            loaded: true,
            condensing: false,
        }
    }

    /// 申请 `size` 字节的连续块, 成功时返回区间在 vdev 内的起始偏移.
    ///
    /// `size` 为 0、超过 metaslab 容量或找不到足够长的连续空闲块时返回 `None`。
    pub fn alloc(&mut self, size: u64) -> Option<u64> {
        if size == 0 || size > self.size {
            return None;
        }
        let nblocks = crate::functions::intcast::low_u32_from_u64(size.div_ceil(HV_MS_BLOCK_SIZE));
        if nblocks > self.max_block {
            return None;
        }
        let start = self.find_contiguous(nblocks)?;
        for b in start..start + nblocks {
            self.clear_bit(b);
        }
        let offset = self.start + u64::from(start) * HV_MS_BLOCK_SIZE;
        self.allocated
            .fetch_add(u64::from(nblocks) * HV_MS_BLOCK_SIZE, Ordering::Relaxed);
        self.free_space
            .fetch_sub(u64::from(nblocks) * HV_MS_BLOCK_SIZE, Ordering::Relaxed);
        self.update_weight();
        Some(offset)
    }

    /// 释放 `[offset, offset + size)` 区间, 将覆盖的块重新标记为空闲.
    ///
    /// 越界的块标记会被忽略; `offset` 小于区间起点时按起点处理。
    pub fn free(&mut self, offset: u64, size: u64) {
        let rel = offset.saturating_sub(self.start);
        let start_block = crate::functions::intcast::low_u32_from_u64(rel / HV_MS_BLOCK_SIZE);
        let nblocks = crate::functions::intcast::low_u32_from_u64(size.div_ceil(HV_MS_BLOCK_SIZE));
        for b in start_block..start_block + nblocks {
            if (b as usize) < self.max_block as usize {
                self.set_bit(b);
            }
        }
        self.freed
            .fetch_add(u64::from(nblocks) * HV_MS_BLOCK_SIZE, Ordering::Relaxed);
        self.free_space
            .fetch_add(u64::from(nblocks) * HV_MS_BLOCK_SIZE, Ordering::Relaxed);
        self.update_weight();
    }

    fn find_contiguous(&self, nblocks: u32) -> Option<u32> {
        let mut found = 0u32;
        let mut start = 0u32;
        for i in 0..self.max_block {
            if self.get_bit(i) {
                if found == 0 {
                    start = i;
                }
                found += 1;
                if found >= nblocks {
                    return Some(start);
                }
            } else {
                found = 0;
            }
        }
        None
    }

    fn get_bit(&self, block: u32) -> bool {
        let idx = (block as usize) / 64;
        let bit = (block as usize) % 64;
        if idx >= self.bitmap.len() {
            return false;
        }
        (self.bitmap[idx] >> bit) & 1 == 1
    }

    fn set_bit(&mut self, block: u32) {
        let idx = (block as usize) / 64;
        let bit = (block as usize) % 64;
        if idx < self.bitmap.len() {
            self.bitmap[idx] |= 1u64 << bit;
        }
    }

    fn clear_bit(&mut self, block: u32) {
        let idx = (block as usize) / 64;
        let bit = (block as usize) % 64;
        if idx < self.bitmap.len() {
            self.bitmap[idx] &= !(1u64 << bit);
        }
    }

    fn update_weight(&mut self) {
        let free = self.free_space.load(Ordering::Relaxed);
        self.weight = if free > self.size / 2 { free } else { free * 2 };
    }

    /// 判断当前是否可接受分配 (状态为 Active 且仍有空闲空间).
    pub fn is_available(&self) -> bool {
        self.state == UnkfsMsState::Active && self.free_space.load(Ordering::Relaxed) > 0
    }

    /// 返回空闲空间占比 (0-100 的百分比), 数值越大空间越充裕.
    pub fn fragmentation(&self) -> u8 {
        let free = self.free_space.load(Ordering::Relaxed);
        if self.size == 0 {
            return 0;
        }
        crate::functions::intcast::low_u8_from_u64((free * 100) / self.size)
    }

    /// 依据空闲空间刷新状态: 无空闲空间置为 [`UnkfsMsState::Full`], 否则置为 [`UnkfsMsState::Active`].
    pub fn sync(&mut self) {
        let free = self.free_space.load(Ordering::Relaxed);
        if free == 0 {
            self.state = UnkfsMsState::Full;
        } else {
            self.state = UnkfsMsState::Active;
        }
    }
}
