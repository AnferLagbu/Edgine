#![deny(unsafe_code)]
use crate::functions::fs::unkfs::spa::HV_POOL_BLOCK_SIZE;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

pub const HV_MS_BLOCK_SIZE: u64 = HV_POOL_BLOCK_SIZE;
pub const HV_MS_MAX_BLOCKS: u32 = 16384;
pub const HV_MS_SHIFT: u8 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnkfsMsState {
    Uninit = 0,
    Active = 1,
    Full = 2,
}

pub struct UnkfsMetaslab {
    pub id: u32,
    pub vdev_id: u16,
    pub start: u64,
    pub size: u64,
    pub state: UnkfsMsState,
    pub allocated: AtomicU64,
    pub freed: AtomicU64,
    pub space: AtomicU64,
    pub free_space: AtomicU64,
    pub bitmap: Vec<u64>,
    pub max_block: u32,
    pub weight: u64,
    pub loaded: bool,
    pub condensing: bool,
}

// SAFETY: UnkfsMetaslab uses Mutex for bitmap and plain Copy types for other fields.
// SAFETY (Framekernel P2.2.2): UnkfsMetaslab 全部字段 (Mutex<T>, [T; N]) 自动 Send + Sync。

impl UnkfsMetaslab {
    pub fn new(id: u32, vdev_id: u16, start: u64, size: u64) -> Self {
        let nblocks = (size / HV_MS_BLOCK_SIZE) as u32;
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

    pub fn alloc(&mut self, size: u64) -> Option<u64> {
        if size == 0 || size > self.size {
            return None;
        }
        let nblocks = size.div_ceil(HV_MS_BLOCK_SIZE) as u32;
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

    pub fn free(&mut self, offset: u64, size: u64) {
        let rel = offset.saturating_sub(self.start);
        let start_block = (rel / HV_MS_BLOCK_SIZE) as u32;
        let nblocks = size.div_ceil(HV_MS_BLOCK_SIZE) as u32;
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

    pub fn is_available(&self) -> bool {
        self.state == UnkfsMsState::Active && self.free_space.load(Ordering::Relaxed) > 0
    }

    pub fn fragmentation(&self) -> u8 {
        let free = self.free_space.load(Ordering::Relaxed);
        if self.size == 0 {
            return 0;
        }
        ((free * 100) / self.size) as u8
    }

    pub fn sync(&mut self) {
        let free = self.free_space.load(Ordering::Relaxed);
        if free == 0 {
            self.state = UnkfsMsState::Full;
        } else {
            self.state = UnkfsMsState::Active;
        }
    }
}
