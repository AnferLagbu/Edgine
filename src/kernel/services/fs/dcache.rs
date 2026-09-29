//! 目录项缓存 (dcache) + inode 缓存 (icache) — framework 层完整实现
//!
//! ## B09-12/DECISION-H13 P1-B1 迁移记录 (2026-08-31)
//!
//! dcache/icache 是 VFS 缓存机制, 按"机制归 framework"原则从
//! `services::fs::dcache` 迁回本文件. 0 语义变更, 仅锁路径由
//! `services::sync::irq_lock::IrqSpinLock` 改为 `framework::sync::IrqSpinLock`
//! (两者同一类型, 前者是后者的 type alias).
//! `services::fs::dcache` 改为 re-export 本文件保持调用方兼容.
//!
//! ## 动机
//!
//! 当前 `resolve_path` 每次从根目录逐级线性扫描目录项, O(n) 复杂度.
//! 对于 `/usr/bin/ls` 这样的路径, 需要 3 次目录扫描, 每次遍历所有目录项.
//! dcache 将 (`fs_id`, `parent_ino`, name) → inode 的映射缓存起来, 将路径
//! 解析从 O(depth × `entries_per_dir`) 降至 O(depth).
//!
//! ## 架构
//!
//! ```text
//! dcache: (fs_id, parent_ino, name) → DCacheEntry { ino, file_type, valid }
//! icache: (fs_id, ino) → ICacheEntry { ino, file_type, size, perm, valid }
//! ```
//!
//! ## 设计决策
//!
//! - **开放寻址哈希表**: 固定大小数组, 无堆分配, 适合 `no_std` 内核
//! - **Robin Hood 哈希**: 减少探查链长度, 查找方差小
//! - **负缓存**: 查找失败也记录, 避免重复扫描不存在的路径
//! - **fs 实例命名空间**: 键首维为文件系统实例标识 `fs_id`. 多个 `RamFsData`
//!   实例 (全局 `RAMFS_DATA` / overlay 内嵌 upper/work 层 / `SafeRamFs` /
//!   `TmpFsData`) 根 inode 同为 1、节点号重叠, 若不带实例维度则跨实例相互
//!   污染 (负缓存串扰 / 命中他实例 ino). `fs_id` 由实例挂载时分配, 0 表示
//!   未挂载.
//! - **简单失效**: 文件创建/删除/重命名时按 (`fs_id`, `parent_ino`) 失效相关条目
//! - **单核假设**: 当前用 `IrqSpinLock` 保护, 后续 per-CPU 时可去锁
//!
//! ## 与 Linux 的差异
//!
//! Linux dcache 是复杂的 LRU + RCU + dentry 父子指针树.
//! `QueenX` 当前是单核 + `RamFs`, 采用扁平哈希表 + 简单失效,
//! 功能等价但复杂度低两个数量级. 后续多核时再引入 per-CPU dcache.
//!
//! ## 安全契约
//!
//! - 全局状态由 `IrqSpinLock` 守护
//! - 所有公开函数接受 `&self` / `&mut self`
//! - 缓存条目可被失效, 失效后回退到原始路径解析
//! - 本文件 0 unsafe

#![deny(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use crate::framework::sync::IrqSpinLock;

// ============================================================================
// 常量
// ============================================================================

/// dcache 条目数 (素数, 减少哈希冲突)
const DCACHE_SIZE: usize = 127;
/// icache 条目数
const ICACHE_SIZE: usize = 127;
/// 名称最大长度 (与 `VFS_MAX_NAME` 一致)
const DCACHE_NAME_LEN: usize = 64;
/// 哈希表空槽标记
const EMPTY_INO: u32 = u32::MAX;
/// 负缓存标记: 表示该 (parent, name) 不存在
const NEGATIVE_INO: u32 = u32::MAX - 1;

// ============================================================================
// dcache 条目
// ============================================================================

/// 目录项缓存条目
#[derive(Clone, Copy)]
struct DCacheEntry {
    /// 文件系统实例标识 (命名空间, 0 = 未挂载)
    fs_id: u32,
    /// 父目录 inode 号
    parent_ino: u32,
    /// 目录项名称
    name: [u8; DCACHE_NAME_LEN],
    /// 名称有效长度
    name_len: u8,
    /// 子 inode 号 (`NEGATIVE_INO` = 负缓存)
    ino: u32,
    /// 文件类型 (`VfsFileType` as u8)
    file_type: u8,
    /// 有效标志
    valid: bool,
    /// 探查距离 (Robin Hood)
    probe_distance: u8,
}

impl Default for DCacheEntry {
    fn default() -> Self {
        Self {
            fs_id: 0,
            parent_ino: EMPTY_INO,
            name: [0; DCACHE_NAME_LEN],
            name_len: 0,
            ino: EMPTY_INO,
            file_type: 0,
            valid: false,
            probe_distance: 0,
        }
    }
}

// ============================================================================
// icache 条目
// ============================================================================

/// inode 缓存条目
#[derive(Clone, Copy)]
struct ICacheEntry {
    /// 文件系统实例标识 (命名空间, 0 = 未挂载)
    fs_id: u32,
    /// inode 号
    ino: u32,
    /// 文件类型
    file_type: u8,
    /// 权限
    perm: u16,
    /// 文件大小
    size: u32,
    /// 修改时间
    mtime: u64,
    /// 创建时间
    ctime: u64,
    /// 所有者 PWM
    owner_pwm: u64,
    /// 组 PWM
    group_pwm: u64,
    /// 有效标志
    valid: bool,
    /// 引用计数 (被 dentry 引用次数)
    ref_count: u32,
    /// 探查距离 (Robin Hood)
    probe_distance: u8,
}

impl Default for ICacheEntry {
    fn default() -> Self {
        Self {
            fs_id: 0,
            ino: EMPTY_INO,
            file_type: 0,
            perm: 0,
            size: 0,
            mtime: 0,
            ctime: 0,
            owner_pwm: 0,
            group_pwm: 0,
            valid: false,
            ref_count: 0,
            probe_distance: 0,
        }
    }
}

// ============================================================================
// 全局缓存实例
// ============================================================================

/// 全局 dcache
static DCACHE: IrqSpinLock<DCache> = IrqSpinLock::new(DCache::new());
/// 全局 icache
static ICACHE: IrqSpinLock<ICache> = IrqSpinLock::new(ICache::new());

/// 统计: dcache 查找次数
static DCACHE_LOOKUPS: AtomicU64 = AtomicU64::new(0);
/// 统计: dcache 命中次数
static DCACHE_HITS: AtomicU64 = AtomicU64::new(0);
/// 统计: icache 查找次数
static ICACHE_LOOKUPS: AtomicU64 = AtomicU64::new(0);
/// 统计: icache 命中次数
static ICACHE_HITS: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// DCache 实现
// ============================================================================

struct DCache {
    entries: [DCacheEntry; DCACHE_SIZE],
    count: usize,
}

impl DCache {
    const fn new() -> Self {
        Self {
            entries: [DCacheEntry {
                fs_id: 0,
                parent_ino: EMPTY_INO,
                name: [0; DCACHE_NAME_LEN],
                name_len: 0,
                ino: EMPTY_INO,
                file_type: 0,
                valid: false,
                probe_distance: 0,
            }; DCACHE_SIZE],
            count: 0,
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// FNV-1a 哈希: (`fs_id`, `parent_ino`, name) → u64
    fn hash_key(fs_id: u32, parent_ino: u32, name: &str) -> u64 {
        let mut h: u64 = 14695981039346656037;
        // 混入 fs_id (实例命名空间)
        for &b in &fs_id.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1099511628211);
        }
        // 混入 parent_ino
        for &b in &parent_ino.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1099511628211);
        }
        // 混入 name
        for &b in name.as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1099511628211);
        }
        h
    }

    /// 查找 (`fs_id`, `parent_ino`, name) → (ino, `file_type`)
    ///
    /// 返回:
    /// - `Some((ino, file_type))`: 正缓存命中
    /// - `Some((NEGATIVE_INO, _))`: 负缓存命中 (该路径不存在)
    /// - `None`: 缓存未命中
    fn lookup(&self, fs_id: u32, parent_ino: u32, name: &str) -> Option<(u32, u8)> {
        if name.is_empty() || name.len() > DCACHE_NAME_LEN {
            return None;
        }

        let hash = Self::hash_key(fs_id, parent_ino, name);
        let start = (hash % DCACHE_SIZE as u64) as usize;

        for distance in 0..DCACHE_SIZE {
            let idx = (start + distance) % DCACHE_SIZE;
            let entry = &self.entries[idx];

            // 空槽: 未命中
            if !entry.valid && entry.parent_ino == EMPTY_INO {
                return None;
            }

            // Robin Hood: 如果当前条目的探查距离小于我们已走的距离, 不可能再找到
            if entry.probe_distance < distance as u8 {
                return None;
            }

            // 匹配检查
            if entry.valid
                && entry.fs_id == fs_id
                && entry.parent_ino == parent_ino
                && entry.name_len as usize == name.len()
                && &entry.name[..entry.name_len as usize] == name.as_bytes()
            {
                return Some((entry.ino, entry.file_type));
            }
        }

        None
    }

    /// 插入 (`fs_id`, `parent_ino`, name) → (ino, `file_type`)
    ///
    /// ino = `NEGATIVE_INO` 表示负缓存
    fn insert(&mut self, fs_id: u32, parent_ino: u32, name: &str, ino: u32, file_type: u8) {
        if name.is_empty() || name.len() > DCACHE_NAME_LEN {
            return;
        }

        let hash = Self::hash_key(fs_id, parent_ino, name);
        let start = (hash % DCACHE_SIZE as u64) as usize;

        let mut new_entry = DCacheEntry {
            fs_id,
            parent_ino,
            name: [0; DCACHE_NAME_LEN],
            name_len: name.len() as u8,
            ino,
            file_type,
            valid: true,
            probe_distance: 0,
        };
        new_entry.name[..name.len()].copy_from_slice(name.as_bytes());

        let mut pos = start;
        let mut distance = 0u8;

        for _ in 0..DCACHE_SIZE {
            let idx = pos % DCACHE_SIZE;
            let entry = &mut self.entries[idx];

            // 空槽或无效条目: 直接插入
            if !entry.valid || entry.parent_ino == EMPTY_INO {
                new_entry.probe_distance = distance;
                *entry = new_entry;
                self.count += 1;
                return;
            }

            // 已存在相同 key: 更新
            if entry.fs_id == fs_id
                && entry.parent_ino == parent_ino
                && entry.name_len as usize == name.len()
                && &entry.name[..entry.name_len as usize] == name.as_bytes()
            {
                entry.ino = ino;
                entry.file_type = file_type;
                entry.valid = true;
                return;
            }

            // Robin Hood: 如果已有条目的探查距离更短, 换位
            if entry.probe_distance < distance {
                new_entry.probe_distance = distance;
                distance = entry.probe_distance;
                core::mem::swap(entry, &mut new_entry);
            }

            pos = (pos + 1) % DCACHE_SIZE;
            distance += 1;
        }

        // 表满: 丢弃新条目 (不应发生, DCACHE_SIZE 足够大)
    }

    /// 失效指定实例下某父目录的所有条目
    ///
    /// 文件创建/删除/重命名时调用, 确保一致性.
    fn invalidate_parent(&mut self, fs_id: u32, parent_ino: u32) {
        for entry in &mut self.entries {
            if entry.valid && entry.fs_id == fs_id && entry.parent_ino == parent_ino {
                entry.valid = false;
                entry.parent_ino = EMPTY_INO;
                self.count -= 1;
            }
        }
    }

    /// 尝试释放无效的缓存条目
    ///
    /// 在内存压力或缓存驱逐时调用.
    fn try_evict_entries(&mut self) -> usize {
        let mut evicted = 0;
        for entry in &mut self.entries {
            if !entry.valid && entry.parent_ino != EMPTY_INO {
                entry.parent_ino = EMPTY_INO;
                evicted += 1;
            }
        }
        evicted
    }

    /// 清空所有缓存
    fn flush(&mut self) {
        for entry in &mut self.entries {
            *entry = DCacheEntry::default();
        }
        self.count = 0;
    }

    /// 缓存条目数
    fn len(&self) -> usize {
        self.count
    }
}

// ============================================================================
// ICache 实现
// ============================================================================

struct ICache {
    entries: [ICacheEntry; ICACHE_SIZE],
    count: usize,
}

impl ICache {
    const fn new() -> Self {
        Self {
            entries: [ICacheEntry {
                fs_id: 0,
                ino: EMPTY_INO,
                file_type: 0,
                perm: 0,
                size: 0,
                mtime: 0,
                ctime: 0,
                owner_pwm: 0,
                group_pwm: 0,
                valid: false,
                ref_count: 0,
                probe_distance: 0,
            }; ICACHE_SIZE],
            count: 0,
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// FNV-1a 哈希: (`fs_id`, ino) → u64
    fn hash_key(fs_id: u32, ino: u32) -> u64 {
        let mut h: u64 = 14695981039346656037;
        for &b in &fs_id.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1099511628211);
        }
        for &b in &ino.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1099511628211);
        }
        h
    }

    /// 查找 inode 缓存
    fn lookup(&self, fs_id: u32, ino: u32) -> Option<ICacheEntry> {
        if ino == EMPTY_INO || ino == NEGATIVE_INO {
            return None;
        }

        let hash = Self::hash_key(fs_id, ino);
        let start = (hash % ICACHE_SIZE as u64) as usize;

        for distance in 0..ICACHE_SIZE {
            let idx = (start + distance) % ICACHE_SIZE;
            let entry = &self.entries[idx];

            if !entry.valid && entry.ino == EMPTY_INO {
                return None;
            }

            if entry.probe_distance < distance as u8 {
                return None;
            }

            if entry.valid && entry.fs_id == fs_id && entry.ino == ino {
                return Some(*entry);
            }
        }

        None
    }

    /// 插入/更新 inode 缓存
    #[expect(
        clippy::too_many_arguments,
        reason = "too_many_arguments: icache 需完整 VfsStat 字段, 拆结构体收益不抵改动面; 当前优先 expect"
    )]
    fn insert(
        &mut self,
        fs_id: u32,
        ino: u32,
        file_type: u8,
        perm: u16,
        size: u32,
        mtime: u64,
        ctime: u64,
        owner_pwm: u64,
        group_pwm: u64,
    ) {
        if ino == EMPTY_INO || ino == NEGATIVE_INO {
            return;
        }

        let hash = Self::hash_key(fs_id, ino);
        let start = (hash % ICACHE_SIZE as u64) as usize;

        let mut new_entry = ICacheEntry {
            fs_id,
            ino,
            file_type,
            perm,
            size,
            mtime,
            ctime,
            owner_pwm,
            group_pwm,
            valid: true,
            ref_count: 0,
            probe_distance: 0,
        };

        let mut pos = start;
        let mut distance = 0u8;

        for _ in 0..ICACHE_SIZE {
            let idx = pos % ICACHE_SIZE;
            let entry = &mut self.entries[idx];

            if !entry.valid || entry.ino == EMPTY_INO {
                new_entry.probe_distance = distance;
                // 继承引用计数 (如果之前有同 ino 的条目)
                new_entry.ref_count = entry.ref_count;
                *entry = new_entry;
                self.count += 1;
                return;
            }

            // 已存在: 更新
            if entry.fs_id == fs_id && entry.ino == ino {
                entry.file_type = file_type;
                entry.perm = perm;
                entry.size = size;
                entry.mtime = mtime;
                entry.valid = true;
                return;
            }

            // Robin Hood 换位
            if entry.probe_distance < distance {
                new_entry.probe_distance = distance;
                distance = entry.probe_distance;
                core::mem::swap(entry, &mut new_entry);
            }

            pos = (pos + 1) % ICACHE_SIZE;
            distance += 1;
        }
    }

    /// 失效指定实例的 inode
    fn invalidate(&mut self, fs_id: u32, ino: u32) {
        if ino == EMPTY_INO {
            return;
        }

        let hash = Self::hash_key(fs_id, ino);
        let start = (hash % ICACHE_SIZE as u64) as usize;

        for distance in 0..ICACHE_SIZE {
            let idx = (start + distance) % ICACHE_SIZE;
            let entry = &mut self.entries[idx];

            if !entry.valid && entry.ino == EMPTY_INO {
                return;
            }

            if entry.probe_distance < distance as u8 {
                return;
            }

            if entry.valid && entry.fs_id == fs_id && entry.ino == ino {
                entry.valid = false;
                entry.ino = EMPTY_INO;
                self.count -= 1;
                return;
            }
        }
    }

    /// 增加引用计数
    fn ref_inc(&mut self, fs_id: u32, ino: u32) {
        if let Some(idx) = self.lookup_index(fs_id, ino) {
            self.entries[idx].ref_count = self.entries[idx].ref_count.saturating_add(1);
        }
    }

    /// 减少引用计数
    fn ref_dec(&mut self, fs_id: u32, ino: u32) {
        if let Some(idx) = self.lookup_index(fs_id, ino) {
            self.entries[idx].ref_count = self.entries[idx].ref_count.saturating_sub(1);
        }
    }

    /// 检查引用计数是否为零
    fn is_ref_zero(&self, fs_id: u32, ino: u32) -> bool {
        self.lookup_index(fs_id, ino)
            .is_none_or(|idx| self.entries[idx].ref_count == 0)
    }

    /// 获取引用计数
    fn get_ref_count(&self, fs_id: u32, ino: u32) -> u32 {
        self.lookup_index(fs_id, ino)
            .map_or(0, |idx| self.entries[idx].ref_count)
    }

    /// 可变查找 (返回索引)
    fn lookup_index(&self, fs_id: u32, ino: u32) -> Option<usize> {
        if ino == EMPTY_INO || ino == NEGATIVE_INO {
            return None;
        }

        let hash = Self::hash_key(fs_id, ino);
        let start = (hash % ICACHE_SIZE as u64) as usize;

        for distance in 0..ICACHE_SIZE {
            let idx = (start + distance) % ICACHE_SIZE;
            let entry = &self.entries[idx];

            if !entry.valid && entry.ino == EMPTY_INO {
                return None;
            }

            if entry.probe_distance < distance as u8 {
                return None;
            }

            if entry.valid && entry.fs_id == fs_id && entry.ino == ino {
                return Some(idx);
            }
        }

        None
    }

    /// 尝试释放引用计数为零的缓存条目
    ///
    /// 在内存压力或缓存驱逐时调用.
    fn try_evict_entries(&mut self) -> usize {
        let mut evicted = 0;
        for entry in &mut self.entries {
            if entry.valid && entry.ref_count == 0 {
                entry.valid = false;
                entry.ino = EMPTY_INO;
                self.count -= 1;
                evicted += 1;
            }
        }
        evicted
    }

    /// 清空所有缓存
    fn flush(&mut self) {
        for entry in &mut self.entries {
            *entry = ICacheEntry::default();
        }
        self.count = 0;
    }

    /// 缓存条目数
    fn len(&self) -> usize {
        self.count
    }
}

// ============================================================================
// 公开 API
// ============================================================================

/// dcache 查找结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DCacheResult {
    /// 正缓存命中: 找到 inode
    Hit { ino: u32, file_type: u8 },
    /// 负缓存命中: 该路径不存在
    Negative,
    /// 缓存未命中
    Miss,
}

/// icache 查找结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ICacheResult {
    pub ino: u32,
    pub file_type: u8,
    pub perm: u16,
    pub size: u32,
    pub mtime: u64,
    pub ctime: u64,
    pub owner_pwm: u64,
    pub group_pwm: u64,
}

/// dcache 查找
pub fn dcache_lookup(fs_id: u32, parent_ino: u32, name: &str) -> DCacheResult {
    DCACHE_LOOKUPS.fetch_add(1, Ordering::Relaxed);

    let dcache = DCACHE.lock();
    match dcache.lookup(fs_id, parent_ino, name) {
        Some((ino, ft)) => {
            DCACHE_HITS.fetch_add(1, Ordering::Relaxed);
            if ino == NEGATIVE_INO {
                DCacheResult::Negative
            } else {
                DCacheResult::Hit { ino, file_type: ft }
            }
        }
        None => DCacheResult::Miss,
    }
}

/// dcache 插入 (正缓存)
pub fn dcache_insert(fs_id: u32, parent_ino: u32, name: &str, ino: u32, file_type: u8) {
    let mut dcache = DCACHE.lock();
    dcache.insert(fs_id, parent_ino, name, ino, file_type);
    // 同时更新 icache 引用计数
    let mut icache = ICACHE.lock();
    icache.ref_inc(fs_id, ino);
}

/// dcache 插入 (负缓存: 该路径不存在)
pub fn dcache_insert_negative(fs_id: u32, parent_ino: u32, name: &str) {
    let mut dcache = DCACHE.lock();
    dcache.insert(fs_id, parent_ino, name, NEGATIVE_INO, 0);
}

/// dcache 失效: 指定实例下父目录的所有条目
pub fn dcache_invalidate_parent(fs_id: u32, parent_ino: u32) {
    let mut dcache = DCACHE.lock();
    dcache.invalidate_parent(fs_id, parent_ino);
}

/// dcache 清空
pub fn dcache_flush() {
    let mut dcache = DCACHE.lock();
    // 先尝试驱逐无效条目
    dcache.try_evict_entries();
    dcache.flush();
}

/// icache 查找
pub fn icache_lookup(fs_id: u32, ino: u32) -> Option<ICacheResult> {
    ICACHE_LOOKUPS.fetch_add(1, Ordering::Relaxed);

    let mut icache = ICACHE.lock();
    icache.lookup(fs_id, ino).map(|entry| {
        ICACHE_HITS.fetch_add(1, Ordering::Relaxed);
        // 增加引用计数
        icache.ref_inc(fs_id, ino);
        ICacheResult {
            ino: entry.ino,
            file_type: entry.file_type,
            perm: entry.perm,
            size: entry.size,
            mtime: entry.mtime,
            ctime: entry.ctime,
            owner_pwm: entry.owner_pwm,
            group_pwm: entry.group_pwm,
        }
    })
}

/// icache 插入/更新
pub fn icache_insert(
    fs_id: u32,
    ino: u32,
    file_type: u8,
    perm: u16,
    size: u32,
    mtime: u64,
    ctime: u64,
    owner_pwm: u64,
    group_pwm: u64,
) {
    let mut icache = ICACHE.lock();
    icache.insert(
        fs_id, ino, file_type, perm, size, mtime, ctime, owner_pwm, group_pwm,
    );
}

/// icache 失效
pub fn icache_invalidate(fs_id: u32, ino: u32) {
    let mut icache = ICACHE.lock();
    // 减少引用计数
    icache.ref_dec(fs_id, ino);
    // 如果引用计数为零, 可以安全失效
    if icache.is_ref_zero(fs_id, ino) {
        icache.invalidate(fs_id, ino);
    }
}

/// 获取 icache 条目的引用计数 (诊断接口)
pub fn icache_get_ref_count(fs_id: u32, ino: u32) -> u32 {
    let icache = ICACHE.lock();
    icache.get_ref_count(fs_id, ino)
}

/// icache 清空
pub fn icache_flush() {
    let mut icache = ICACHE.lock();
    // 先尝试驱逐引用计数为零的条目
    icache.try_evict_entries();
    icache.flush();
}

/// 同时清空 dcache + icache
pub fn flush_all() {
    dcache_flush();
    icache_flush();
}

// ============================================================================
// 统计
// ============================================================================

/// dcache 命中率
pub fn dcache_hit_rate() -> (u64, u64) {
    let lookups = DCACHE_LOOKUPS.load(Ordering::Relaxed);
    let hits = DCACHE_HITS.load(Ordering::Relaxed);
    (hits, lookups)
}

/// icache 命中率
pub fn icache_hit_rate() -> (u64, u64) {
    let lookups = ICACHE_LOOKUPS.load(Ordering::Relaxed);
    let hits = ICACHE_HITS.load(Ordering::Relaxed);
    (hits, lookups)
}

/// dcache 条目数
pub fn dcache_count() -> usize {
    let dcache = DCACHE.lock();
    dcache.len()
}

/// icache 条目数
pub fn icache_count() -> usize {
    let icache = ICACHE.lock();
    icache.len()
}

/// 重置统计计数器
pub fn reset_stats() {
    DCACHE_LOOKUPS.store(0, Ordering::Relaxed);
    DCACHE_HITS.store(0, Ordering::Relaxed);
    ICACHE_LOOKUPS.store(0, Ordering::Relaxed);
    ICACHE_HITS.store(0, Ordering::Relaxed);
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dcache_insert_lookup() {
        let mut dcache = DCache::new();
        dcache.insert(0, 1, "bin", 10, 1);
        assert!(matches!(dcache.lookup(0, 1, "bin"), Some((10, 1))));
    }

    #[test]
    fn test_dcache_miss() {
        let dcache = DCache::new();
        assert!(dcache.lookup(0, 1, "nonexist").is_none());
    }

    #[test]
    fn test_dcache_negative() {
        let mut dcache = DCache::new();
        dcache.insert(0, 1, "gone", NEGATIVE_INO, 0);
        assert!(matches!(
            dcache.lookup(0, 1, "gone"),
            Some((NEGATIVE_INO, 0))
        ));
    }

    #[test]
    fn test_dcache_invalidate_parent() {
        let mut dcache = DCache::new();
        dcache.insert(0, 1, "a", 10, 0);
        dcache.insert(0, 1, "b", 20, 0);
        dcache.insert(0, 2, "c", 30, 0);
        dcache.invalidate_parent(0, 1);
        assert!(dcache.lookup(0, 1, "a").is_none());
        assert!(dcache.lookup(0, 1, "b").is_none());
        assert!(matches!(dcache.lookup(0, 2, "c"), Some((30, 0))));
    }

    #[test]
    fn test_dcache_update() {
        let mut dcache = DCache::new();
        dcache.insert(0, 1, "file", 10, 0);
        dcache.insert(0, 1, "file", 20, 1);
        assert!(matches!(dcache.lookup(0, 1, "file"), Some((20, 1))));
    }

    /// 回归: 不同 fs 实例的相同 (parent_ino, name) 必须互不干扰
    ///
    /// 修复前全局 dcache 键为 (`parent_ino`, name), 多 `RamFsData` 实例根 inode
    /// 同为 1、节点号重叠时会跨实例污染 (负缓存串扰 / 命中他实例 ino).
    #[test]
    fn test_dcache_fs_instance_isolation() {
        let mut dcache = DCache::new();
        // 实例 1: /dir 存在; 实例 2: 同名路径为负缓存
        dcache.insert(1, 1, "dir", 10, 1);
        dcache.insert(2, 1, "dir", NEGATIVE_INO, 0);

        assert!(matches!(dcache.lookup(1, 1, "dir"), Some((10, 1))));
        assert!(matches!(
            dcache.lookup(2, 1, "dir"),
            Some((NEGATIVE_INO, 0))
        ));
        // 未插入的实例 3 应未命中
        assert!(dcache.lookup(3, 1, "dir").is_none());

        // 失效实例 1 不影响实例 2
        dcache.invalidate_parent(1, 1);
        assert!(dcache.lookup(1, 1, "dir").is_none());
        assert!(matches!(
            dcache.lookup(2, 1, "dir"),
            Some((NEGATIVE_INO, 0))
        ));
    }

    #[test]
    fn test_icache_insert_lookup() {
        let mut icache = ICache::new();
        icache.insert(0, 10, 1, 0o755, 4096, 1000, 1000, 0, 0);
        let entry = icache.lookup(0, 10).unwrap();
        assert_eq!(entry.ino, 10);
        assert_eq!(entry.file_type, 1);
        assert_eq!(entry.size, 4096);
    }

    /// 回归: 不同 fs 实例的同号 inode 必须互不干扰
    #[test]
    fn test_icache_fs_instance_isolation() {
        let mut icache = ICache::new();
        icache.insert(1, 10, 1, 0o644, 512, 1000, 1000, 0, 0);
        icache.insert(2, 10, 2, 0o755, 4096, 2000, 2000, 0, 0);

        assert_eq!(icache.lookup(1, 10).unwrap().size, 512);
        assert_eq!(icache.lookup(2, 10).unwrap().size, 4096);

        icache.invalidate(1, 10);
        assert!(icache.lookup(1, 10).is_none());
        assert_eq!(icache.lookup(2, 10).unwrap().size, 4096);
    }

    #[test]
    fn test_icache_invalidate() {
        let mut icache = ICache::new();
        icache.insert(0, 10, 1, 0o755, 4096, 1000, 1000, 0, 0);
        icache.invalidate(0, 10);
        assert!(icache.lookup(0, 10).is_none());
    }

    #[test]
    fn test_dcache_flush() {
        let mut dcache = DCache::new();
        dcache.insert(0, 1, "a", 10, 0);
        dcache.insert(0, 1, "b", 20, 0);
        dcache.flush();
        assert_eq!(dcache.len(), 0);
        assert!(dcache.lookup(0, 1, "a").is_none());
    }

    #[test]
    fn test_many_entries() {
        let mut dcache = DCache::new();
        // 插入足够多的条目验证 Robin Hood 行为
        for i in 0..50u32 {
            let name = alloc::format!("file_{}", i);
            dcache.insert(0, 1, &name, 100 + i, 0);
        }
        for i in 0..50u32 {
            let name = alloc::format!("file_{}", i);
            assert!(matches!(
                dcache.lookup(0, 1, &name),
                Some((ino, 0)) if ino == 100 + i
            ));
        }
    }
}
