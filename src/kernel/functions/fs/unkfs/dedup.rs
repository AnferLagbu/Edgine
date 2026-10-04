#![deny(unsafe_code)]
//! Content-Addressable Storage (CAS) — 内容寻址块去重
//!
//! 块索引按内容哈希 (SHA256) 寻址。相同内容的块自动去重。
//!
//! ## 工作流
//!
//! ```text
//! 写 /data/config.json (4KB)
//!   → SHA256(content) = 0xDEAD...BEEF
//!   → 查询 CAS 索引: 已存在 (refcount=3)
//!   → refcount++, 不写新块
//!   → ZIL 记录: DedupRef
//!
//! 删除 → refcount-- → refcount=0 时真正释放
//! ```

use super::bp::UnkfsBlockPointer;
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::functions::sync::once::OnceCell;
pub const CAS_HASH_SIZE: usize = 32;
pub type CasHash = [u8; CAS_HASH_SIZE];

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum CasOp {
    RefInc = 0,
    RefDec = 1,
    Insert = 2,
    Evict = 3,
}

pub struct CasIndex {
    hash_to_dva: Mutex<BTreeMap<[u8; 32], Vec<UnkfsBlockPointer>>>,
    ref_counts: Mutex<BTreeMap<[u8; 32], u64>>,
    hits: AtomicU64,
    misses: AtomicU64,
    synced: AtomicU64,
}

impl CasIndex {
    pub fn new() -> Self {
        Self {
            hash_to_dva: Mutex::new(BTreeMap::new()),
            ref_counts: Mutex::new(BTreeMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            synced: AtomicU64::new(0),
        }
    }

    pub fn lookup(&self, hash: &CasHash) -> Option<UnkfsBlockPointer> {
        let index = self.hash_to_dva.lock();
        if let Some(dvas) = index.get(hash) {
            if let Some(bp) = dvas.first() {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Some(*bp);
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    pub fn insert(&self, hash: CasHash, bp: UnkfsBlockPointer) {
        let mut index = self.hash_to_dva.lock();
        index.entry(hash).or_default().push(bp);
        let mut refs = self.ref_counts.lock();
        *refs.entry(hash).or_insert(0) += 1;
        self.synced.fetch_add(1, Ordering::Relaxed);
    }

    pub fn ref_inc(&self, hash: &CasHash) -> u64 {
        let mut refs = self.ref_counts.lock();
        let count = refs.entry(*hash).or_insert(0);
        *count += 1;
        self.hits.fetch_add(1, Ordering::Relaxed);
        *count
    }

    pub fn ref_dec(&self, hash: &CasHash) -> u64 {
        // 锁序统一: hash_to_dva → ref_counts (与 insert/invalidate 一致).
        // 回归背景 (第二十六批预存问题修复): 此前清零分支先持 ref_counts 再取
        // hash_to_dva, 与 insert 的持锁顺序相反, 并发交错即 ABBA 死锁
        // (host-tests unkfs_stress_test 两测试线程互等复现).
        let mut index = self.hash_to_dva.lock();
        let mut refs = self.ref_counts.lock();
        if let Some(count) = refs.get_mut(hash) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                refs.remove(hash);
                index.remove(hash);
                return 0;
            }
            return *count;
        }
        0
    }

    pub fn ref_count(&self, hash: &CasHash) -> u64 {
        let refs = self.ref_counts.lock();
        refs.get(hash).copied().unwrap_or(0)
    }

    pub fn is_known(&self, hash: &CasHash) -> bool {
        self.ref_counts.lock().contains_key(hash)
    }

    pub fn get_stats(&self) -> (u64, u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.synced.load(Ordering::Relaxed),
        )
    }

    pub fn invalidate(&self, hash: &CasHash) {
        let mut index = self.hash_to_dva.lock();
        index.remove(hash);
        let mut refs = self.ref_counts.lock();
        refs.remove(hash);
    }
}

static CAS_INDEX: OnceCell<CasIndex> = OnceCell::new();

pub fn get_cas() -> &'static CasIndex {
    CAS_INDEX.get_or_init(|slot| {
        slot.write(CasIndex::new());
    })
}

pub fn cas_init() {
    get_cas();
}

pub fn cas_lookup(hash: &CasHash) -> Option<UnkfsBlockPointer> {
    get_cas().lookup(hash)
}

pub fn cas_insert(hash: CasHash, bp: UnkfsBlockPointer) {
    get_cas().insert(hash, bp);
}

pub fn cas_ref_inc(hash: &CasHash) -> u64 {
    get_cas().ref_inc(hash)
}

pub fn cas_ref_dec(hash: &CasHash) -> u64 {
    get_cas().ref_dec(hash)
}

pub fn cas_ref_count(hash: &CasHash) -> u64 {
    get_cas().ref_count(hash)
}

pub fn cas_is_known(hash: &CasHash) -> bool {
    get_cas().is_known(hash)
}

pub fn cas_stats() -> (u64, u64, u64) {
    get_cas().get_stats()
}

pub fn sha256(data: &[u8]) -> CasHash {
    let ck = super::checksum::UnkfsChecksum::compute(super::bp::UnkfsCksumType::SHA256, data);
    let mut hash = [0u8; 32];
    hash[0..8].copy_from_slice(&ck.value[0].to_be_bytes());
    hash[8..16].copy_from_slice(&ck.value[1].to_be_bytes());
    hash[16..24].copy_from_slice(&ck.value[2].to_be_bytes());
    hash[24..32].copy_from_slice(&ck.value[3].to_be_bytes());
    hash
}

pub fn sha256_matches(data: &[u8], expected: &CasHash) -> bool {
    sha256(data) == *expected
}

/// CAS 感知写入: 计算 SHA256, 检查去重索引, 引用已有块或分配新块
pub fn cas_aware_write(data: &[u8], txg: u64, obj_id: u64) -> Option<super::bp::UnkfsBlockPointer> {
    let hash = sha256(data);
    let cas = get_cas();

    if let Some(existing) = cas.lookup(&hash) {
        cas.ref_inc(&hash);
        crate::functions::fs::unkfs::zil::UnkfsZilRecord::new_dedup_ref(
            txg,
            [
                u64::from_be_bytes(hash[0..8].try_into().unwrap_or_else(|_| [0u8; 8])),
                u64::from_be_bytes(hash[8..16].try_into().unwrap_or_else(|_| [0u8; 8])),
                u64::from_be_bytes(hash[16..24].try_into().unwrap_or_else(|_| [0u8; 8])),
                u64::from_be_bytes(hash[24..32].try_into().unwrap_or_else(|_| [0u8; 8])),
            ],
            obj_id,
        );
        return Some(existing);
    }

    None
}

pub fn cas_aware_free(hash: &CasHash) -> bool {
    let count = cas_ref_dec(hash);
    count == 0
}
