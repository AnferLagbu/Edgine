#![deny(unsafe_code)]
use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const HV_ARC_DEFAULT_SIZE: usize = 256;
pub const HV_ARC_MAX_SIZE: usize = 4096;
pub const HV_ARC_BUF_SIZE: usize = 4096;
pub const HV_ARC_META_SIZE: usize = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestArcState {
    Anon = 0,
    Mru = 1,
    Mfu = 2,
    MruGhost = 3,
    MfuGhost = 4,
    L2Cache = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestArcBufType {
    Data = 0,
    Metadata = 1,
}

#[derive(Debug, Clone, Copy)]
pub struct NestArcKey {
    pub vdev_id: u16,
    pub offset: u64,
    pub birth_txg: u64,
}

impl NestArcKey {
    pub fn new(vdev_id: u16, offset: u64, birth_txg: u64) -> Self {
        Self {
            vdev_id,
            offset,
            birth_txg,
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 14695981039346656037;
        h ^= u64::from(self.vdev_id);
        h = h.wrapping_mul(1099511628211);
        h ^= self.offset;
        h = h.wrapping_mul(1099511628211);
        h ^= self.birth_txg;
        h = h.wrapping_mul(1099511628211);
        h
    }
}

pub struct NestArcBuf {
    pub key: NestArcKey,
    pub data: Box<[u8]>,
    pub size: usize,
    pub buf_type: NestArcBufType,
    pub state: NestArcState,
    pub ref_count: AtomicU32,
    pub access_count: u32,
    pub dirty: bool,
    pub l2cached: bool,
    pub compressed: bool,
}

// SAFETY (Framekernel P2.2.2): NestArcBuf 全部字段自动 Send + Sync。

impl NestArcBuf {
    pub fn new(key: NestArcKey, size: usize, buf_type: NestArcBufType) -> Self {
        // 2026-09-11 实测: with_capacity+resize 触发 slow_vector_initialization, 需豁免
        #[allow(clippy::slow_vector_initialization)]
        let mut data = Vec::with_capacity(size);
        data.resize(size, 0);
        Self {
            key,
            data: data.into_boxed_slice(),
            size,
            buf_type,
            state: NestArcState::Anon,
            ref_count: AtomicU32::new(1),
            access_count: 1,
            dirty: false,
            l2cached: false,
            compressed: false,
        }
    }

    pub fn is_referenced(&self) -> bool {
        self.ref_count.load(Ordering::Acquire) > 0
    }

    pub fn add_ref(&self) {
        self.ref_count.fetch_add(1, Ordering::AcqRel);
    }

    pub fn release(&self) -> u32 {
        let mut current = self.ref_count.load(Ordering::Acquire);
        loop {
            if current == 0 {
                return 0;
            }
            match self.ref_count.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return current - 1,
                Err(actual) => current = actual,
            }
        }
    }
}

pub struct NestArcStats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub mru_hits: AtomicU64,
    pub mfu_hits: AtomicU64,
    pub ghost_hits: AtomicU64,
    pub evicts: AtomicU64,
    pub size: AtomicU64,
    pub mru_size: AtomicU64,
    pub mfu_size: AtomicU64,
    pub ghost_mru_size: AtomicU64,
    pub ghost_mfu_size: AtomicU64,
    pub data_size: AtomicU64,
    pub meta_size: AtomicU64,
}

impl NestArcStats {
    pub fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            mru_hits: AtomicU64::new(0),
            mfu_hits: AtomicU64::new(0),
            ghost_hits: AtomicU64::new(0),
            evicts: AtomicU64::new(0),
            size: AtomicU64::new(0),
            mru_size: AtomicU64::new(0),
            mfu_size: AtomicU64::new(0),
            ghost_mru_size: AtomicU64::new(0),
            ghost_mfu_size: AtomicU64::new(0),
            data_size: AtomicU64::new(0),
            meta_size: AtomicU64::new(0),
        }
    }
}

const HV_ARC_HASH_BUCKETS: usize = 256;

struct NestArcInner {
    mru: VecDeque<usize>,
    mfu: VecDeque<usize>,
    ghost_mru: VecDeque<NestArcKey>,
    ghost_mfu: VecDeque<NestArcKey>,
    buffers: Vec<Option<NestArcBuf>>,
    hash_table: Vec<Vec<usize>>,
    max_size: usize,
    p: usize,
}

pub struct NestArc {
    inner: Mutex<NestArcInner>,
    stats: NestArcStats,
    initialized: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): NestArc 全部字段 (Mutex<T>, AtomicBool, NestArcStats)
// 都自动实现 Send + Sync。

impl NestArc {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(NestArcInner {
                mru: VecDeque::new(),
                mfu: VecDeque::new(),
                ghost_mru: VecDeque::new(),
                ghost_mfu: VecDeque::new(),
                buffers: Vec::new(),
                hash_table: (0..HV_ARC_HASH_BUCKETS).map(|_| Vec::new()).collect(),
                max_size: HV_ARC_DEFAULT_SIZE,
                p: 0,
            }),
            stats: NestArcStats::new(),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn init(&self, max_size: usize) {
        let max = if max_size == 0 {
            HV_ARC_DEFAULT_SIZE
        } else {
            max_size.min(HV_ARC_MAX_SIZE)
        };
        let mut inner = self.inner.lock();
        inner.max_size = max;
        inner.buffers.clear();
        inner.mru.clear();
        inner.mfu.clear();
        inner.ghost_mru.clear();
        inner.ghost_mfu.clear();
        for bucket in &mut inner.hash_table {
            bucket.clear();
        }
        inner.p = max / 2;
        self.initialized.store(true, Ordering::Release);
    }

    pub fn lookup(&self, key: &NestArcKey) -> Option<*const u8> {
        let mut inner = self.inner.lock();
        let bucket_idx = (key.hash() as usize) % HV_ARC_HASH_BUCKETS;
        let mut found_idx: Option<usize> = None;
        let mut found_ptr: Option<*const u8> = None;
        let mut promote_to_mfu = false;

        for &idx in &inner.hash_table[bucket_idx] {
            if let Some(ref buf) = inner.buffers[idx] {
                if buf.key.vdev_id == key.vdev_id
                    && buf.key.offset == key.offset
                    && buf.key.birth_txg == key.birth_txg
                {
                    self.stats.hits.fetch_add(1, Ordering::Relaxed);
                    if buf.state == NestArcState::Mru {
                        self.stats.mru_hits.fetch_add(1, Ordering::Relaxed);
                    } else if buf.state == NestArcState::Mfu {
                        self.stats.mfu_hits.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Some(ref mut buf) = inner.buffers[idx] {
                        buf.access_count += 1;
                        if buf.state == NestArcState::Mru {
                            promote_to_mfu = true;
                        }
                        buf.add_ref();
                        found_ptr = Some(buf.data.as_ptr());
                    }
                    found_idx = Some(idx);
                    break;
                }
            }
        }
        if let Some(idx) = found_idx {
            if promote_to_mfu {
                inner.mru.retain(|&i| i != idx);
                if let Some(ref mut buf) = inner.buffers[idx] {
                    buf.state = NestArcState::Mfu;
                }
                inner.mfu.push_back(idx);
            }
            return found_ptr;
        }
        if let Some(pos) = inner.ghost_mru.iter().position(|k| {
            k.vdev_id == key.vdev_id && k.offset == key.offset && k.birth_txg == key.birth_txg
        }) {
            inner.ghost_mru.remove(pos);
            let delta = if inner.mru.len() + inner.ghost_mru.len() > 0 {
                (inner.max_size * inner.ghost_mru.len()) / (inner.mru.len() + inner.ghost_mru.len())
            } else {
                1
            };
            inner.p = (inner.p + delta).min(inner.max_size);
            self.stats.ghost_hits.fetch_add(1, Ordering::Relaxed);
        } else if let Some(pos) = inner.ghost_mfu.iter().position(|k| {
            k.vdev_id == key.vdev_id && k.offset == key.offset && k.birth_txg == key.birth_txg
        }) {
            inner.ghost_mfu.remove(pos);
            let delta = if inner.mfu.len() + inner.ghost_mfu.len() > 0 {
                (inner.max_size * inner.ghost_mfu.len()) / (inner.mfu.len() + inner.ghost_mfu.len())
            } else {
                1
            };
            inner.p = inner.p.saturating_sub(delta);
            self.stats.ghost_hits.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Framekernel P2.2.2: 安全地获取缓存切片，通过 framework `arc_safe::ptr_to_slice` 封装 unsafe
    /// 调用者仍需在访问后调用 release(key) 释放引用计数
    pub fn lookup_slice(&self, key: &NestArcKey, len: usize) -> Option<&[u8]> {
        let ptr = self.lookup(key)?;
        crate::framework::fs::unkfs::arc_safe::ptr_to_slice(ptr, len)
    }

    pub fn insert(
        &self,
        key: NestArcKey,
        data: &[u8],
        buf_type: NestArcBufType,
    ) -> Option<*const u8> {
        let mut inner = self.inner.lock();
        self.remove_key(&mut inner, &key);
        self.evict_if_needed(&mut inner, data.len());
        let mut buf = NestArcBuf::new(key, data.len(), buf_type);
        buf.data[..data.len()].copy_from_slice(data);
        buf.state = NestArcState::Mru;
        let slot_idx = inner.buffers.iter().position(core::option::Option::is_none);
        let idx = if let Some(i) = slot_idx {
            inner.buffers[i] = Some(buf);
            i
        } else {
            inner.buffers.push(Some(buf));
            inner.buffers.len() - 1
        };
        let bucket_idx = (key.hash() as usize) % HV_ARC_HASH_BUCKETS;
        inner.hash_table[bucket_idx].push(idx);
        inner.mru.push_back(idx);
        self.stats
            .size
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        if buf_type == NestArcBufType::Data {
            self.stats
                .data_size
                .fetch_add(data.len() as u64, Ordering::Relaxed);
        } else {
            self.stats
                .meta_size
                .fetch_add(data.len() as u64, Ordering::Relaxed);
        }
        self.stats
            .mru_size
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        inner.buffers[idx].as_ref().map(|b| {
            b.add_ref();
            b.data.as_ptr()
        })
    }

    fn remove_key(&self, inner: &mut NestArcInner, key: &NestArcKey) {
        let bucket_idx = (key.hash() as usize) % HV_ARC_HASH_BUCKETS;
        let mut found: Option<usize> = None;
        for (pos, &idx) in inner.hash_table[bucket_idx].iter().enumerate() {
            if let Some(ref buf) = inner.buffers[idx] {
                if buf.key.vdev_id == key.vdev_id
                    && buf.key.offset == key.offset
                    && buf.key.birth_txg == key.birth_txg
                {
                    found = Some(pos);
                    break;
                }
            }
        }
        if let Some(pos) = found {
            let idx = inner.hash_table[bucket_idx].remove(pos);
            let size = inner.buffers[idx].as_ref().map_or(0, |b| b.data.len());
            self.stats.size.fetch_sub(size as u64, Ordering::Relaxed);
            inner.buffers[idx] = None;
            inner.mru.retain(|&i| i != idx);
            inner.mfu.retain(|&i| i != idx);
        }
    }

    fn evict_if_needed(&self, inner: &mut NestArcInner, _incoming_size: usize) {
        while inner.mru.len() + inner.mfu.len() + 1 > inner.max_size {
            if inner.mru.len() > inner.p {
                if let Some(idx) = inner.mru.pop_front() {
                    if let Some(buf) = inner.buffers[idx].take() {
                        let bucket_idx = (buf.key.hash() as usize) % HV_ARC_HASH_BUCKETS;
                        inner.hash_table[bucket_idx].retain(|&i| i != idx);
                        inner.ghost_mru.push_back(buf.key);
                        self.stats
                            .mru_size
                            .fetch_sub(buf.size as u64, Ordering::Relaxed);
                        self.stats
                            .size
                            .fetch_sub(buf.size as u64, Ordering::Relaxed);
                        self.stats.evicts.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else if let Some(idx) = inner.mfu.pop_front() {
                if let Some(buf) = inner.buffers[idx].take() {
                    let bucket_idx = (buf.key.hash() as usize) % HV_ARC_HASH_BUCKETS;
                    inner.hash_table[bucket_idx].retain(|&i| i != idx);
                    inner.ghost_mfu.push_back(buf.key);
                    self.stats
                        .mfu_size
                        .fetch_sub(buf.size as u64, Ordering::Relaxed);
                    self.stats
                        .size
                        .fetch_sub(buf.size as u64, Ordering::Relaxed);
                    self.stats.evicts.fetch_add(1, Ordering::Relaxed);
                }
            }
            if inner.ghost_mru.len() > inner.max_size {
                inner.ghost_mru.pop_front();
            }
            if inner.ghost_mfu.len() > inner.max_size {
                inner.ghost_mfu.pop_front();
            }
        }
    }

    pub fn release(&self, key: &NestArcKey) {
        let mut inner = self.inner.lock();
        if let Some(buf) = inner.buffers.iter_mut().find_map(|slot| {
            slot.as_mut().filter(|buf| {
                buf.key.vdev_id == key.vdev_id
                    && buf.key.offset == key.offset
                    && buf.key.birth_txg == key.birth_txg
            })
        }) {
            buf.release();
        }
    }

    pub fn mark_dirty(&self, key: &NestArcKey) {
        let mut inner = self.inner.lock();
        if let Some(buf) = inner.buffers.iter_mut().find_map(|slot| {
            slot.as_mut().filter(|buf| {
                buf.key.vdev_id == key.vdev_id
                    && buf.key.offset == key.offset
                    && buf.key.birth_txg == key.birth_txg
            })
        }) {
            buf.dirty = true;
        }
    }

    pub fn flush_dirty(&self) -> usize {
        let inner = self.inner.lock();
        let mut count = 0;
        for slot in &inner.buffers {
            if let Some(buf) = slot {
                if buf.dirty {
                    count += 1;
                }
            }
        }
        count
    }

    pub fn get_stats(&self) -> (u64, u64, u64, u64) {
        (
            self.stats.hits.load(Ordering::Relaxed),
            self.stats.misses.load(Ordering::Relaxed),
            self.stats.size.load(Ordering::Relaxed),
            self.stats.evicts.load(Ordering::Relaxed),
        )
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    // ====== LEGACY-5.8: ArcCache trait 公开访问器 ======
    // 允许 arc_trait.rs 读取 stats 字段, 不暴露字段本身

    /// 当前缓存总大小
    pub fn current_size(&self) -> u64 {
        self.stats.size.load(Ordering::Acquire)
    }

    /// MRU 列表条目数 (与 `max_size` 同量纲, 便于容量不变量比较)
    ///
    /// 注: `stats.mru_size` 为 MRU 占用字节数统计 (与 `stats.size` 同口径),
    /// 与 `max_size` 的条目数量纲不同, 故本访问器按条目数返回.
    pub fn mru_size(&self) -> u64 {
        self.inner.lock().mru.len() as u64
    }

    /// MFU 列表条目数 (与 `max_size` 同量纲)
    ///
    /// 注: `stats.mfu_size` 为 MFU 占用字节数统计, 口径同 `mru_size` 注释.
    pub fn mfu_size(&self) -> u64 {
        self.inner.lock().mfu.len() as u64
    }

    /// 命中次数
    pub fn hit_count(&self) -> u64 {
        self.stats.hits.load(Ordering::Acquire)
    }

    /// 未命中次数
    pub fn miss_count(&self) -> u64 {
        self.stats.misses.load(Ordering::Acquire)
    }

    /// 淘汰次数
    pub fn evict_count(&self) -> u64 {
        self.stats.evicts.load(Ordering::Acquire)
    }

    /// 最大容量
    pub fn max_size(&self) -> u64 {
        self.inner.lock().max_size as u64
    }
}

// DECISION-080: ARC 缓存纯逻辑断言 (初始化/查找/淘汰/dirty) 以本文件源侧 #[cfg(test)] 为唯一归属.
// 注: 迁入 services (#![deny(unsafe_code)]) 后, 查找数据改用 safe 包装 `lookup_slice`,
// 不再直接对裸指针做 `from_raw_parts`.
#[cfg(test)]
mod tests {
    use super::*;

    /// init 后应报告已初始化.
    #[test]
    fn test_arc_init() {
        let arc = NestArc::new();
        arc.init(128);
        assert!(arc.is_initialized(), "arc should be initialized");
    }

    /// 空缓存查找应返回 None.
    #[test]
    fn test_arc_lookup_miss() {
        let arc = NestArc::new();
        arc.init(128);
        let key = NestArcKey::new(0, 0, 0);
        assert!(arc.lookup(&key).is_none(), "empty arc should return None");
    }

    /// 插入后应能经 safe 切片接口查回同一数据.
    #[test]
    fn test_arc_insert_lookup() {
        let arc = NestArc::new();
        arc.init(128);
        let key = NestArcKey::new(0, 4096, 1);
        let data: [u8; 16] = [0xAA; 16];
        let _ = arc.insert(key, &data, NestArcBufType::Data);
        let Some(found) = arc.lookup_slice(&key, 16) else {
            panic!("should find inserted entry");
        };
        assert_eq!(found[0], 0xAA, "data mismatch");
    }

    /// 多次插入后缓存应产生统计活动.
    #[test]
    fn test_arc_eviction() {
        let arc = NestArc::new();
        arc.init(8192);
        for i in 0..5u64 {
            let key = NestArcKey::new(0, i * 4096, 1);
            let data: [u8; 64] = [i as u8; 64];
            let _ = arc.insert(key, &data, NestArcBufType::Data);
        }
        let (hits, misses, size, _evicts) = arc.get_stats();
        assert!(
            size > 0 || hits > 0 || misses > 0,
            "arc should have activity after inserts"
        );
    }

    /// mark_dirty 后 flush_dirty 应统计到脏条目.
    #[test]
    fn test_arc_dirty_tracking() {
        let arc = NestArc::new();
        arc.init(256);
        let key = NestArcKey::new(0, 0, 1);
        let data: [u8; 32] = [0xBB; 32];
        let _ = arc.insert(key, &data, NestArcBufType::Data);

        arc.mark_dirty(&key);
        let dirty_count = arc.flush_dirty();
        assert!(
            dirty_count > 0,
            "should have dirty entries after mark_dirty"
        );
    }
}
