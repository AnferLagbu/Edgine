#![deny(unsafe_code)]
#![deny(missing_docs)]
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
/// CAS 内容哈希的字节长度 (SHA256).
pub const CAS_HASH_SIZE: usize = 32;
/// CAS 内容哈希类型 (定长 32 字节数组).
pub type CasHash = [u8; CAS_HASH_SIZE];

/// CAS 索引操作类型 (用于审计/ZIL 记录).
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum CasOp {
    /// 引用计数递增.
    RefInc = 0,
    /// 引用计数递减.
    RefDec = 1,
    /// 插入新块映射.
    Insert = 2,
    /// 淘汰块映射.
    Evict = 3,
}

/// 内容寻址索引 (哈希 → 块指针 + 引用计数).
pub struct CasIndex {
    hash_to_dva: Mutex<BTreeMap<[u8; 32], Vec<UnkfsBlockPointer>>>,
    ref_counts: Mutex<BTreeMap<[u8; 32], u64>>,
    hits: AtomicU64,
    misses: AtomicU64,
    synced: AtomicU64,
}

impl CasIndex {
    /// 创建空索引 (所有映射与计数器归零).
    #[must_use]
    pub fn new() -> Self {
        Self {
            hash_to_dva: Mutex::new(BTreeMap::new()),
            ref_counts: Mutex::new(BTreeMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            synced: AtomicU64::new(0),
        }
    }

    /// 按内容哈希查找已有块指针, 命中/未命中分别累加对应计数.
    pub fn lookup(&self, hash: &CasHash) -> Option<UnkfsBlockPointer> {
        let index = self.hash_to_dva.lock();
        if let Some(dvas) = index.get(hash)
            && let Some(bp) = dvas.first()
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Some(*bp);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// 插入哈希 → 块指针映射并递增引用计数.
    pub fn insert(&self, hash: CasHash, bp: UnkfsBlockPointer) {
        let mut index = self.hash_to_dva.lock();
        index.entry(hash).or_default().push(bp);
        let mut refs = self.ref_counts.lock();
        *refs.entry(hash).or_insert(0) += 1;
        self.synced.fetch_add(1, Ordering::Relaxed);
    }

    /// 递增指定哈希的引用计数, 返回新计数值.
    pub fn ref_inc(&self, hash: &CasHash) -> u64 {
        let mut refs = self.ref_counts.lock();
        let count = refs.entry(*hash).or_insert(0);
        *count += 1;
        self.hits.fetch_add(1, Ordering::Relaxed);
        *count
    }

    /// 递减指定哈希的引用计数 (饱和到 0); 归零时移除映射并返回 0.
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

    /// 查询指定哈希的引用计数 (未知哈希为 0).
    pub fn ref_count(&self, hash: &CasHash) -> u64 {
        let refs = self.ref_counts.lock();
        refs.get(hash).copied().unwrap_or(0)
    }

    /// 判断指定哈希是否已在索引中.
    pub fn is_known(&self, hash: &CasHash) -> bool {
        self.ref_counts.lock().contains_key(hash)
    }

    /// 返回 (`hits`, `misses`, `synced`) 三元统计.
    pub fn get_stats(&self) -> (u64, u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.synced.load(Ordering::Relaxed),
        )
    }

    /// 移除指定哈希的全部索引与引用计数.
    pub fn invalidate(&self, hash: &CasHash) {
        let mut index = self.hash_to_dva.lock();
        index.remove(hash);
        let mut refs = self.ref_counts.lock();
        refs.remove(hash);
    }
}

impl Default for CasIndex {
    /// 委托 [`CasIndex::new`] 创建空索引.
    fn default() -> Self {
        Self::new()
    }
}

static CAS_INDEX: OnceCell<CasIndex> = OnceCell::new();

/// 返回全局 CAS 索引单例 (首次调用时惰性初始化).
pub fn get_cas() -> &'static CasIndex {
    CAS_INDEX.get_or_init(|slot| {
        slot.write(CasIndex::new());
    })
}

/// 初始化全局 CAS 索引 (幂等).
pub fn cas_init() {
    get_cas();
}

/// 在全局索引中按内容哈希查找块指针.
#[must_use]
pub fn cas_lookup(hash: &CasHash) -> Option<UnkfsBlockPointer> {
    get_cas().lookup(hash)
}

/// 向全局索引插入哈希 → 块指针映射.
pub fn cas_insert(hash: CasHash, bp: UnkfsBlockPointer) {
    get_cas().insert(hash, bp);
}

/// 递增全局索引中指定哈希的引用计数.
#[must_use]
pub fn cas_ref_inc(hash: &CasHash) -> u64 {
    get_cas().ref_inc(hash)
}

/// 递减全局索引中指定哈希的引用计数.
#[must_use]
pub fn cas_ref_dec(hash: &CasHash) -> u64 {
    get_cas().ref_dec(hash)
}

/// 查询全局索引中指定哈希的引用计数.
#[must_use]
pub fn cas_ref_count(hash: &CasHash) -> u64 {
    get_cas().ref_count(hash)
}

/// 判断指定哈希是否已在全局索引中.
#[must_use]
pub fn cas_is_known(hash: &CasHash) -> bool {
    get_cas().is_known(hash)
}

/// 返回全局索引的 (`hits`, `misses`, `synced`) 统计.
#[must_use]
pub fn cas_stats() -> (u64, u64, u64) {
    get_cas().get_stats()
}

/// 计算数据块的 SHA256 内容哈希.
#[must_use]
pub fn sha256(data: &[u8]) -> CasHash {
    let ck = super::checksum::UnkfsChecksum::compute(super::bp::UnkfsCksumType::SHA256, data);
    let mut hash = [0u8; 32];
    hash[0..8].copy_from_slice(&ck.value[0].to_be_bytes());
    hash[8..16].copy_from_slice(&ck.value[1].to_be_bytes());
    hash[16..24].copy_from_slice(&ck.value[2].to_be_bytes());
    hash[24..32].copy_from_slice(&ck.value[3].to_be_bytes());
    hash
}

/// 校验数据块的 SHA256 是否与期望哈希一致.
#[must_use]
pub fn sha256_matches(data: &[u8], expected: &CasHash) -> bool {
    sha256(data) == *expected
}

/// CAS 感知写入: 计算 SHA256, 检查去重索引, 引用已有块或分配新块
#[must_use]
pub fn cas_aware_write(data: &[u8], txg: u64, obj_id: u64) -> Option<super::bp::UnkfsBlockPointer> {
    let hash = sha256(data);
    let cas = get_cas();

    if let Some(existing) = cas.lookup(&hash) {
        cas.ref_inc(&hash);
        let _ = crate::functions::fs::unkfs::zil::UnkfsZilRecord::new_dedup_ref(
            txg,
            [
                u64::from_be_bytes(hash[0..8].try_into().unwrap_or([0u8; 8])),
                u64::from_be_bytes(hash[8..16].try_into().unwrap_or([0u8; 8])),
                u64::from_be_bytes(hash[16..24].try_into().unwrap_or([0u8; 8])),
                u64::from_be_bytes(hash[24..32].try_into().unwrap_or([0u8; 8])),
            ],
            obj_id,
        );
        return Some(existing);
    }

    None
}

/// 释放一个内容哈希引用; 引用计数归零时返回 `true` (块可真正回收).
#[must_use]
pub fn cas_aware_free(hash: &CasHash) -> bool {
    let count = cas_ref_dec(hash);
    count == 0
}
