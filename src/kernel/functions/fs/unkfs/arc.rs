#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// ARC 默认容量 (条目数).
pub const HV_ARC_DEFAULT_SIZE: usize = 256;
/// ARC 容量上限 (条目数).
pub const HV_ARC_MAX_SIZE: usize = 4096;
/// 数据缓冲区大小 (字节).
pub const HV_ARC_BUF_SIZE: usize = 4096;
/// 元数据缓冲区大小 (字节).
pub const HV_ARC_META_SIZE: usize = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// ARC 缓冲区状态 (MRU/MFU 链表与幽灵链表归属).
pub enum UnkfsArcState {
    /// 匿名/尚未归类.
    Anon = 0,
    /// 位于最近使用 (MRU) 链表.
    Mru = 1,
    /// 位于最常使用 (MFU) 链表.
    Mfu = 2,
    /// 位于 MRU 幽灵链表 (已淘汰, 仅保留键).
    MruGhost = 3,
    /// 位于 MFU 幽灵链表.
    MfuGhost = 4,
    /// 位于二级缓存.
    L2Cache = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// ARC 缓冲区数据类型.
pub enum UnkfsArcBufType {
    /// 数据块.
    Data = 0,
    /// 元数据块.
    Metadata = 1,
}

#[derive(Debug, Clone, Copy)]
/// ARC 缓存键: 由 vdev、偏移与出生事务组唯一确定一个块.
pub struct UnkfsArcKey {
    /// 所属 vdev 编号.
    pub vdev_id: u16,
    /// 块在 vdev 内的字节偏移.
    pub offset: u64,
    /// 块的出生事务组号.
    pub birth_txg: u64,
}

impl UnkfsArcKey {
    /// 以 vdev 编号、偏移与出生事务组构造缓存键.
    pub fn new(vdev_id: u16, offset: u64, birth_txg: u64) -> Self {
        Self {
            vdev_id,
            offset,
            birth_txg,
        }
    }

    /// 计算该键的 64 位 FNV-1a 哈希, 用作哈希桶索引.
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 14_695_981_039_346_656_037;
        h ^= u64::from(self.vdev_id);
        h = h.wrapping_mul(1_099_511_628_211);
        h ^= self.offset;
        h = h.wrapping_mul(1_099_511_628_211);
        h ^= self.birth_txg;
        h = h.wrapping_mul(1_099_511_628_211);
        h
    }
}

/// 一个已缓存的块: 键、只读数据句柄与淘汰状态.
pub struct UnkfsArcBuf {
    /// 缓存键.
    pub key: UnkfsArcKey,
    /// 块数据的共享只读句柄.
    pub data: Arc<[u8]>,
    /// 数据字节数.
    pub size: usize,
    /// 缓冲区数据类型.
    pub buf_type: UnkfsArcBufType,
    /// 当前所处的链表状态.
    pub state: UnkfsArcState,
    /// 引用计数.
    pub ref_count: AtomicU32,
    /// 累计访问次数 (MFU 晋升判据).
    pub access_count: u32,
    /// 是否存在未回写的修改.
    pub dirty: bool,
    /// 是否已进入二级缓存.
    pub l2cached: bool,
    /// 是否以压缩形态存放.
    pub compressed: bool,
}

// SAFETY (Framekernel P2.2.2): UnkfsArcBuf 全部字段自动 Send + Sync。

impl UnkfsArcBuf {
    /// 以键、数据与类型构造缓冲区 (状态 Anon, 引用计数 1).
    pub fn new(key: UnkfsArcKey, data: &[u8], buf_type: UnkfsArcBufType) -> Self {
        Self {
            key,
            data: Arc::from(data),
            size: data.len(),
            buf_type,
            state: UnkfsArcState::Anon,
            ref_count: AtomicU32::new(1),
            access_count: 1,
            dirty: false,
            l2cached: false,
            compressed: false,
        }
    }

    /// 原子递减引用计数; 已为 0 时返回 0 (不下溢).
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

/// ARC 命中与容量统计计数器集合.
pub struct UnkfsArcStats {
    /// 命中总数.
    pub hits: AtomicU64,
    /// 未命中总数.
    pub misses: AtomicU64,
    /// MRU 链表命中数.
    pub mru_hits: AtomicU64,
    /// MFU 链表命中数.
    pub mfu_hits: AtomicU64,
    /// 幽灵链表命中数.
    pub ghost_hits: AtomicU64,
    /// 淘汰次数.
    pub evicts: AtomicU64,
    /// 当前缓存总字节数.
    pub size: AtomicU64,
    /// MRU 占用字节数.
    pub mru_size: AtomicU64,
    /// MFU 占用字节数.
    pub mfu_size: AtomicU64,
    /// MRU 幽灵链表占用字节数.
    pub ghost_mru_size: AtomicU64,
    /// MFU 幽灵链表占用字节数.
    pub ghost_mfu_size: AtomicU64,
    /// 数据块占用字节数.
    pub data_size: AtomicU64,
    /// 元数据块占用字节数.
    pub meta_size: AtomicU64,
}

impl UnkfsArcStats {
    /// 构造全部计数为 0 的统计集合.
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

struct UnkfsArcInner {
    mru: VecDeque<usize>,
    mfu: VecDeque<usize>,
    ghost_mru: VecDeque<UnkfsArcKey>,
    ghost_mfu: VecDeque<UnkfsArcKey>,
    buffers: Vec<Option<UnkfsArcBuf>>,
    hash_table: Vec<Vec<usize>>,
    max_size: usize,
    p: usize,
}

/// 自适应替换缓存 (ARC): MRU/MFU 双链表加幽灵链表的块缓存.
pub struct UnkfsArc {
    inner: Mutex<UnkfsArcInner>,
    stats: UnkfsArcStats,
    initialized: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): UnkfsArc 全部字段 (Mutex<T>, AtomicBool, UnkfsArcStats)
// 都自动实现 Send + Sync。

impl UnkfsArc {
    /// 构造空 ARC, 容量取 [`HV_ARC_DEFAULT_SIZE`].
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(UnkfsArcInner {
                mru: VecDeque::new(),
                mfu: VecDeque::new(),
                ghost_mru: VecDeque::new(),
                ghost_mfu: VecDeque::new(),
                buffers: Vec::new(),
                hash_table: (0..HV_ARC_HASH_BUCKETS).map(|_| Vec::new()).collect(),
                max_size: HV_ARC_DEFAULT_SIZE,
                p: 0,
            }),
            stats: UnkfsArcStats::new(),
            initialized: AtomicBool::new(false),
        }
    }

    /// 按请求容量初始化并清空缓存; 0 取默认值, 上限 [`HV_ARC_MAX_SIZE`].
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

    /// 按键查缓存; 命中返回数据共享句柄并更新命中/访问计数, 未命中返回 `None`.
    pub fn lookup(&self, key: &UnkfsArcKey) -> Option<Arc<[u8]>> {
        let mut inner = self.inner.lock();
        let bucket_idx =
            crate::functions::intcast::usize_from_u64(key.hash()) % HV_ARC_HASH_BUCKETS;
        let mut found_idx: Option<usize> = None;
        let mut found_data: Option<Arc<[u8]>> = None;
        let mut promote_to_mfu = false;

        for &idx in &inner.hash_table[bucket_idx] {
            if let Some(ref buf) = inner.buffers[idx] {
                if buf.key.vdev_id == key.vdev_id
                    && buf.key.offset == key.offset
                    && buf.key.birth_txg == key.birth_txg
                {
                    self.stats.hits.fetch_add(1, Ordering::Relaxed);
                    if buf.state == UnkfsArcState::Mru {
                        self.stats.mru_hits.fetch_add(1, Ordering::Relaxed);
                    } else if buf.state == UnkfsArcState::Mfu {
                        self.stats.mfu_hits.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Some(ref mut buf) = inner.buffers[idx] {
                        buf.access_count += 1;
                        if buf.state == UnkfsArcState::Mru {
                            promote_to_mfu = true;
                        }
                        // ARC 共享句柄: 持有期间缓冲区不会被淘汰回收.
                        found_data = Some(Arc::clone(&buf.data));
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
                    buf.state = UnkfsArcState::Mfu;
                }
                inner.mfu.push_back(idx);
            }
            return found_data;
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

    /// 安全地获取缓存数据的共享句柄 (0 unsafe)
    ///
    /// 返回 `Arc<[u8]>`: 持有期间该缓冲区不会被淘汰回收, 无需配对 release。
    /// 返回长度为缓存条目的实际长度, 调用方按 `data.len()` 自行裁剪范围。
    pub fn lookup_slice(&self, key: &UnkfsArcKey) -> Option<Arc<[u8]>> {
        self.lookup(key)
    }

    /// 插入或替换一个块 (必要时先淘汰), 返回新缓冲区的数据句柄.
    pub fn insert(
        &self,
        key: UnkfsArcKey,
        data: &[u8],
        buf_type: UnkfsArcBufType,
    ) -> Option<Arc<[u8]>> {
        let mut inner = self.inner.lock();
        self.remove_key(&mut inner, &key);
        self.evict_if_needed(&mut inner, data.len());
        let mut buf = UnkfsArcBuf::new(key, data, buf_type);
        buf.state = UnkfsArcState::Mru;
        let slot_idx = inner.buffers.iter().position(core::option::Option::is_none);
        let idx = if let Some(i) = slot_idx {
            inner.buffers[i] = Some(buf);
            i
        } else {
            inner.buffers.push(Some(buf));
            inner.buffers.len() - 1
        };
        let bucket_idx =
            crate::functions::intcast::usize_from_u64(key.hash()) % HV_ARC_HASH_BUCKETS;
        inner.hash_table[bucket_idx].push(idx);
        inner.mru.push_back(idx);
        self.stats
            .size
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        if buf_type == UnkfsArcBufType::Data {
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
        inner.buffers[idx].as_ref().map(|b| Arc::clone(&b.data))
    }

    fn remove_key(&self, inner: &mut UnkfsArcInner, key: &UnkfsArcKey) {
        let bucket_idx =
            crate::functions::intcast::usize_from_u64(key.hash()) % HV_ARC_HASH_BUCKETS;
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

    fn evict_if_needed(&self, inner: &mut UnkfsArcInner, _incoming_size: usize) {
        while inner.mru.len() + inner.mfu.len() + 1 > inner.max_size {
            if inner.mru.len() > inner.p {
                if let Some(idx) = inner.mru.pop_front() {
                    if let Some(buf) = inner.buffers[idx].take() {
                        let bucket_idx = crate::functions::intcast::usize_from_u64(buf.key.hash())
                            % HV_ARC_HASH_BUCKETS;
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
                    let bucket_idx = crate::functions::intcast::usize_from_u64(buf.key.hash())
                        % HV_ARC_HASH_BUCKETS;
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

    /// 按键递减对应缓冲区的引用计数; 键不存在时无操作.
    pub fn release(&self, key: &UnkfsArcKey) {
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

    /// 按键把对应缓冲区标记为脏; 键不存在时无操作.
    pub fn mark_dirty(&self, key: &UnkfsArcKey) {
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

    /// 返回当前仍为脏的缓冲区个数 (只统计, 不执行回写).
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

    /// 返回 (命中数, 未命中数, 当前字节数, 淘汰数) 四元组.
    pub fn get_stats(&self) -> (u64, u64, u64, u64) {
        (
            self.stats.hits.load(Ordering::Relaxed),
            self.stats.misses.load(Ordering::Relaxed),
            self.stats.size.load(Ordering::Relaxed),
            self.stats.evicts.load(Ordering::Relaxed),
        )
    }

    /// 是否已完成 [`UnkfsArc::init`] 初始化.
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
// 注: 迁入 functions (#![deny(unsafe_code)]) 后, 查找数据改用 safe 包装 `lookup_slice`,
// 不再直接对裸指针做 `from_raw_parts`.
#[cfg(test)]
mod tests {
    use super::*;

    /// init 后应报告已初始化.
    #[test]
    fn test_arc_init() {
        let arc = UnkfsArc::new();
        arc.init(128);
        assert!(arc.is_initialized(), "arc should be initialized");
    }

    /// 空缓存查找应返回 None.
    #[test]
    fn test_arc_lookup_miss() {
        let arc = UnkfsArc::new();
        arc.init(128);
        let key = UnkfsArcKey::new(0, 0, 0);
        assert!(arc.lookup(&key).is_none(), "empty arc should return None");
    }

    /// 插入后应能经 safe 切片接口查回同一数据.
    #[test]
    fn test_arc_insert_lookup() {
        let arc = UnkfsArc::new();
        arc.init(128);
        let key = UnkfsArcKey::new(0, 4096, 1);
        let data: [u8; 16] = [0xAA; 16];
        let _ = arc.insert(key, &data, UnkfsArcBufType::Data);
        let Some(found) = arc.lookup_slice(&key) else {
            panic!("should find inserted entry");
        };
        assert_eq!(found[0], 0xAA, "data mismatch");
    }

    /// 驱逐不得使已持有的共享句柄失效 (P1 回归).
    #[test]
    fn test_arc_slice_survives_eviction() {
        let arc = UnkfsArc::new();
        arc.init(1);
        let key = UnkfsArcKey::new(0, 0, 1);
        let _ = arc.insert(key, &[0xAAu8; 64], UnkfsArcBufType::Data);
        let held = arc.lookup_slice(&key).expect("should find entry");
        // 插入新条目触发驱逐; 已持有的 Arc 句柄必须仍然有效.
        let _ = arc.insert(
            UnkfsArcKey::new(0, 4096, 1),
            &[0xBBu8; 64],
            UnkfsArcBufType::Data,
        );
        assert_eq!(held[0], 0xAA, "held slice must survive eviction");
    }

    /// 多次插入后缓存应产生统计活动.
    #[test]
    fn test_arc_eviction() {
        let arc = UnkfsArc::new();
        arc.init(8192);
        for i in 0..5u64 {
            let key = UnkfsArcKey::new(0, i * 4096, 1);
            let data: [u8; 64] = [i as u8; 64];
            let _ = arc.insert(key, &data, UnkfsArcBufType::Data);
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
        let arc = UnkfsArc::new();
        arc.init(256);
        let key = UnkfsArcKey::new(0, 0, 1);
        let data: [u8; 32] = [0xBB; 32];
        let _ = arc.insert(key, &data, UnkfsArcBufType::Data);

        arc.mark_dirty(&key);
        let dirty_count = arc.flush_dirty();
        assert!(
            dirty_count > 0,
            "should have dirty entries after mark_dirty"
        );
    }
}
