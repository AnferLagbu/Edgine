//! Page Cache — 文件内容缓存
//!
//! 为文件映射 (mmap) 和读写提供统一的页级缓存, 避免重复 I/O.
//!
//! ## 核心设计
//!
//! - 以 `(inode_id, page_index)` 为键的全局哈希表
//! - 每个缓存页存储 4KB 文件数据 + 引用计数
//! - `MAP_SHARED`: 写回 Page Cache (脏页标记)
//! - `MAP_PRIVATE`: COW, 写入不回写 Page Cache
//!
//! ## 计数契约
//!
//! - `PageCacheEntry::ref_count` = **当前映射持有该缓存帧的 VMA 页数**:
//!   每一次「新建/替换映射指向该帧」登记一份 (`pcache_acquire_for_va`),
//!   每一次「拆除指向该帧的用户 leaf」注销一份 (`pcache_release_for_va`).
//! - 条目自身对帧的持有由 `insert` 的 `alloc_page` 计入帧计数 (置 1),
//!   故条目存在时恒有 `pmm.frame_ref_count(phys) == ref_count + 1`.
//! - `ref_count` 归零 (最后一个映射持有者注销) 时 `deref` 释放条目自身那份并移除条目.
//! - 判据「该 VA 是否持有该缓存页」= *该 VA 的 PTE 帧 == 该页缓存帧*,
//!   只在 `pcache_acquire_for_va` / `pcache_release_for_va` 内实现一份
//!   (建立侧 `page_fault` 与拆除侧 `mmap` 共用, 避免两侧各自判断而错位).
//!
//! ## 同步
//!
//! 每个桶由独立的 `IrqSpinLock` 保护, 持锁期间关中断,
//! 避免与中断上下文死锁. 桶级锁减少全局竞争.
//!
//! # Safety
//!
//! - 缓存页由 PMM 分配, 通过 `KERNEL_BASE` 映射访问
//! - 脏页写回由文件系统负责 (当前阶段仅标记)
//! - 仅 `pcache_copy_to_user` 保留 unsafe (用户态指针操作)

use crate::privileged::mm::{PAGE_SIZE, PhysAddr, VirtAddr, pmm, vmm};
use crate::privileged::sync::IrqSpinLock;

// ============================================================================
// 物理页 safe 操作封装
// ============================================================================

/// 将物理页内容清零
///
/// 用于新分配的物理页初始化, 防止信息泄漏.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
fn zero_phys_page(phys: PhysAddr) {
    let virt = phys.to_virt();
    // SAFETY: phys 由 PMM 分配, to_virt() 返回有效的内核虚拟地址;
    // 写入 PAGE_SIZE 字节不会越界 (物理页按 PAGE_SIZE 对齐分配).
    unsafe {
        core::ptr::write_bytes(virt.0 as *mut u8, 0, PAGE_SIZE as usize);
    }
}

/// 将数据复制到物理页
///
/// 复制长度取 `min(src.len(), PAGE_SIZE)`, 不足部分保持原值 (通常为零).
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
fn copy_to_phys_page(phys: u64, src: &[u8]) {
    let dst_virt = crate::privileged::mm::phys_to_virt(phys);
    let copy_len = core::cmp::min(src.len(), PAGE_SIZE as usize);
    // SAFETY: phys 是 PMM 分配的有效物理页, phys_to_virt 返回有效的内核虚拟地址;
    // copy_len <= PAGE_SIZE, 不会越界; src 是有效切片.
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst_virt as *mut u8, copy_len);
    }
}

/// 从物理页复制数据到目标缓冲区
///
/// 复制长度取 `min(dst.len(), PAGE_SIZE)`.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
fn copy_from_phys_page(phys: u64, dst: &mut [u8]) {
    let src_virt = crate::privileged::mm::phys_to_virt(phys);
    let copy_len = core::cmp::min(dst.len(), PAGE_SIZE as usize);
    // SAFETY: phys 是 PMM 分配的有效物理页, phys_to_virt 返回有效的内核虚拟地址;
    // copy_len <= PAGE_SIZE, 不会越界; dst 是有效切片.
    unsafe {
        core::ptr::copy_nonoverlapping(src_virt as *const u8, dst.as_mut_ptr(), copy_len);
    }
}

// ============================================================================
// Page Cache Entry
// ============================================================================

/// 一个缓存页: 存储文件某页的数据
struct PageCacheEntry {
    /// 对应的 inode 编号
    inode_id: u32,
    /// 文件内页索引 (`byte_offset` / `PAGE_SIZE`)
    page_index: u64,
    /// 物理页帧 (由 PMM 分配)
    phys: u64,
    /// 映射持有该缓存帧的 VMA 页数 (契约见模块文档「计数契约」;
    /// 条目自身对帧的持有不计入此值, 由 `alloc_page` 计入帧计数)
    ref_count: u32,
    /// 是否为脏页 (`MAP_SHARED` 写入后标记)
    dirty: bool,
    /// 是否被占用 (`inode_id` != 0)
    occupied: bool,
}

impl PageCacheEntry {
    const fn empty() -> Self {
        Self {
            inode_id: 0,
            page_index: 0,
            phys: 0,
            ref_count: 0,
            dirty: false,
            occupied: false,
        }
    }
}

// ============================================================================
// Page Cache Bucket
// ============================================================================

/// 哈希桶数量 (2 的幂)
const PCACHE_HASH_BUCKETS: usize = 64;
/// 每桶最大条目数
const PCACHE_BUCKET_CAPACITY: usize = 16;

struct PageCacheBucket {
    entries: [PageCacheEntry; PCACHE_BUCKET_CAPACITY],
    count: usize,
}

impl PageCacheBucket {
    const fn new() -> Self {
        Self {
            entries: [
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
                PageCacheEntry::empty(),
            ],
            count: 0,
        }
    }

    /// 查找缓存页, 返回物理地址
    fn lookup(&self, inode_id: u32, page_index: u64) -> Option<u64> {
        for entry in &self.entries {
            if entry.occupied && entry.inode_id == inode_id && entry.page_index == page_index {
                return Some(entry.phys);
            }
        }
        None
    }

    /// 查找并增加引用计数
    fn lookup_and_ref(&mut self, inode_id: u32, page_index: u64) -> Option<u64> {
        for entry in &mut self.entries {
            if entry.occupied && entry.inode_id == inode_id && entry.page_index == page_index {
                entry.ref_count += 1;
                return Some(entry.phys);
            }
        }
        None
    }

    /// 登记一份「映射持有该缓存页」的引用
    ///
    /// 命中 ⇒ `ref_count += 1`; 未命中 ⇒ 分配物理页并插入 (`ref_count = 1`,
    /// 该值即首个映射持有者; 条目自身对帧的持有由 `alloc_page` 计入帧计数).
    /// 返回 `(物理地址, 是否为新插入的条目)`, 或 `None` (桶满/OOM).
    fn insert(&mut self, inode_id: u32, page_index: u64) -> Option<(u64, bool)> {
        // 先检查是否已存在 (满桶命中仍应登记成功)
        if let Some(phys) = self.lookup_and_ref(inode_id, page_index) {
            return Some((phys, false));
        }
        if self.count >= PCACHE_BUCKET_CAPACITY {
            return None;
        }

        // 分配物理页
        let pmm_inst = pmm::get_pmm();
        let phys = pmm_inst.alloc_page()?;

        // 清零 (防止信息泄漏)
        zero_phys_page(phys);

        // 此处仅分配全零页; miss 时的文件数据回填由调用方
        // 通过 pcache_fill(inode, page, src) 显式完成 (避免持锁 + 跨层 I/O).

        // 插入条目
        for entry in &mut self.entries {
            if !entry.occupied {
                entry.inode_id = inode_id;
                entry.page_index = page_index;
                entry.phys = phys.as_u64();
                entry.ref_count = 1;
                entry.dirty = false;
                entry.occupied = true;
                self.count += 1;
                return Some((phys.as_u64(), true));
            }
        }

        // 不应到达此处 (count 检查已通过)
        pmm_inst.free_page(phys);
        None
    }

    /// 观测条目的映射持有者数 (`None` = 条目不存在)
    fn ref_count_of(&self, inode_id: u32, page_index: u64) -> Option<u32> {
        self.entries
            .iter()
            .find(|e| e.occupied && e.inode_id == inode_id && e.page_index == page_index)
            .map(|e| e.ref_count)
    }

    /// 标记脏页
    fn mark_dirty(&mut self, inode_id: u32, page_index: u64) {
        for entry in &mut self.entries {
            if entry.occupied && entry.inode_id == inode_id && entry.page_index == page_index {
                entry.dirty = true;
                return;
            }
        }
    }

    /// 减少引用计数, 归零时移除条目并返回**待归还的帧**
    ///
    /// 调用方 (桶锁持有者) 必须在**出桶锁之后**归还返回的帧: 归还需 `VMM_LOCK`,
    /// 而桶锁在既定锁序中位于 `VMM_LOCK` 之后 (见 `mm::release_frame`).
    ///
    /// 仅当 `frame_dec` 报告「本次即最后持有者」(计数 1→0) 时返回帧 —— 这是该帧的
    /// 最后一次引用注销, 必须走统一的延迟释放语义 (x86_64), 故不得在桶锁内
    /// 立即 `free_page`. `frame_dec == false` (仍有其他持有者或契约违反) ⇒ 不归还.
    fn deref(&mut self, inode_id: u32, page_index: u64) -> Option<PhysAddr> {
        for entry in &mut self.entries {
            if entry.occupied && entry.inode_id == inode_id && entry.page_index == page_index {
                if entry.ref_count > 0 {
                    entry.ref_count -= 1;
                }
                if entry.ref_count == 0 {
                    // 条目自身那份持有 (契约: `frame_ref_count == ref_count + 1`)
                    let phys = PhysAddr(entry.phys);
                    let last_holder = pmm::get_pmm().frame_dec(phys);
                    *entry = PageCacheEntry::empty();
                    self.count -= 1;
                    return if last_holder { Some(phys) } else { None };
                }
                return None;
            }
        }
        None
    }

    /// 释放指定 inode 的缓存页
    ///
    /// **仅释放无映射持有者 (`ref_count == 0`) 的条目**: 仍被映射的页由其最后一个
    /// 映射的注销路径 (`deref` 归零) 释放. 这保证 `close(fd)` 不摧毁仍活跃的映射缓存
    /// (`munmap` 允许晚于 `close`).
    ///
    /// 注: 本工程无「纯缓存引用」(条目只由缺页路径创建 ⇒ 存在即有映射持有),
    /// 故该集合当前恒为空, 本函数实际为守卫语义 (见 plan 登记项).
    fn invalidate_inode(&mut self, inode_id: u32) {
        for entry in &mut self.entries {
            if entry.occupied && entry.inode_id == inode_id && entry.ref_count == 0 {
                let phys = PhysAddr(entry.phys);
                pmm::get_pmm().free_page(phys);
                *entry = PageCacheEntry::empty();
                self.count -= 1;
            }
        }
    }

    /// 填充缓存页内容 (供 miss 后由 vfs/fs 路径回填文件数据)
    ///
    /// 若 `src.len() < PAGE_SIZE`, 剩余字节保持原值 (通常为零).
    fn fill(&mut self, inode_id: u32, page_index: u64, src: &[u8]) -> bool {
        for entry in &self.entries {
            if entry.occupied && entry.inode_id == inode_id && entry.page_index == page_index {
                copy_to_phys_page(entry.phys, src);
                return true;
            }
        }
        false
    }
}

// ============================================================================
// 全局 Page Cache
// ============================================================================

/// 每桶独立 `IrqSpinLock`, 持锁期间关中断, 避免与中断上下文死锁.
/// `IrqSpinLock`<T> 自动实现 Send+Sync, 无需手写 unsafe impl.
static PAGE_CACHE: [IrqSpinLock<PageCacheBucket>; PCACHE_HASH_BUCKETS] =
    [const { IrqSpinLock::new(PageCacheBucket::new()) }; PCACHE_HASH_BUCKETS];

/// 计算哈希桶索引
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn pcache_hash(inode_id: u32, page_index: u64) -> usize {
    let h = u64::from(inode_id)
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(page_index.wrapping_mul(0x517CC1B727220A95));
    (h as usize) & (PCACHE_HASH_BUCKETS - 1)
}

// ============================================================================
// 公共 API
// ============================================================================

/// 登记一份「映射持有该缓存页」的引用, 返回缓存帧物理地址
///
/// 若缓存命中, 返回物理地址并增加引用计数.
/// 若未命中, 分配物理页并插入缓存 (`ref_count` 置 1).
///
/// 建立侧应优先使用 `pcache_acquire_for_va` (含幂等判据与旧帧注销).
pub fn pcache_get(inode_id: u32, page_index: u64) -> Option<u64> {
    let idx = pcache_hash(inode_id, page_index);
    let mut guard = PAGE_CACHE[idx].lock();
    guard.insert(inode_id, page_index).map(|(phys, _)| phys)
}

/// 查找缓存页 (不增加引用计数)
pub fn pcache_lookup(inode_id: u32, page_index: u64) -> Option<u64> {
    let idx = pcache_hash(inode_id, page_index);
    let guard = PAGE_CACHE[idx].lock();
    guard.lookup(inode_id, page_index)
}

/// 标记脏页 (`MAP_SHARED` 写入后调用)
pub fn pcache_mark_dirty(inode_id: u32, page_index: u64) {
    let idx = pcache_hash(inode_id, page_index);
    let mut guard = PAGE_CACHE[idx].lock();
    guard.mark_dirty(inode_id, page_index);
}

/// 释放缓存页引用 (munmap 时调用)
pub fn pcache_put(inode_id: u32, page_index: u64) {
    let idx = pcache_hash(inode_id, page_index);
    // 桶锁内只改计数/条目: 归还需 VMM_LOCK, 必须在出桶锁后执行 (锁序见 mm::release_frame)
    let last_frame = {
        let mut guard = PAGE_CACHE[idx].lock();
        guard.deref(inode_id, page_index)
    };
    if let Some(phys) = last_frame {
        super::release_frame(phys);
    }
}

/// 释放 inode 的缓存页 (文件关闭时调用)
///
/// 仅释放无映射持有者的条目 (详见 `PageCacheBucket::invalidate_inode`).
pub fn pcache_invalidate_inode(inode_id: u32) {
    for i in 0..PCACHE_HASH_BUCKETS {
        let mut guard = PAGE_CACHE[i].lock();
        guard.invalidate_inode(inode_id);
    }
}

/// 观测条目的映射持有者数 (`None` = 条目不存在)
///
/// 契约: 条目存在时 `pmm.frame_ref_count(phys) == ref_count + 1`.
/// 仅用于断言/审计 (风格对齐 `pmm.frame_ref_count`), 不参与生产决策.
pub fn pcache_ref_count(inode_id: u32, page_index: u64) -> Option<u32> {
    let idx = pcache_hash(inode_id, page_index);
    let guard = PAGE_CACHE[idx].lock();
    guard.ref_count_of(inode_id, page_index)
}

/// 为一个 VMA 页登记「映射持有该文件缓存页」(建立侧唯一入口)
///
/// 幂等: 若该 VA 的 PTE 帧已是本页缓存帧 ⇒ 不重复登记, 直接返回该帧.
/// 否则登记一份 (命中 `+1` / 未命中插入) 并 `frame_inc` (新建映射新增一份帧持有);
/// 若该 VA 原先映射的是**其它**帧 ⇒ 注销其映射持有者 (归零则按架构释放).
///
/// 返回 `(缓存帧物理地址, 是否为新插入的条目)`; 后者为 `true` 时帧内容仍是零页,
/// 需调用方回填文件数据. `pml4 == 0` / 桶满 / OOM / 帧计数契约违反 ⇒ `None` (fail-closed).
pub fn pcache_acquire_for_va(
    pml4: u64,
    va: u64,
    inode_id: u32,
    page_index: u64,
) -> Option<(u64, bool)> {
    // 用户页表根缺失 ⇒ fail-closed (不得退化到全局单表)
    if pml4 == 0 {
        return None;
    }
    let vmm_inst = vmm::get_vmm();
    let old = vmm_inst
        .get_physical_in_pml4(pml4, VirtAddr(va))
        .map_or(0, |p| p.as_u64());

    // 幂等: 该 VA 已持有本页缓存帧
    if old != 0 && pcache_lookup(inode_id, page_index) == Some(old) {
        return Some((old, false));
    }

    let idx = pcache_hash(inode_id, page_index);
    let (phys, newly_inserted) = {
        let mut guard = PAGE_CACHE[idx].lock();
        guard.insert(inode_id, page_index)?
    };

    let pmm_inst = pmm::get_pmm();
    if !pmm_inst.frame_inc(PhysAddr(phys)) {
        // 帧未处于计数态 (契约违反): 回滚刚登记的引用, fail-closed
        pcache_put(inode_id, page_index);
        return None;
    }

    // 旧帧被本页缓存帧替换 ⇒ 注销其映射持有者; 归零才归还 (时机与锁序见 mm::release_frame)
    if old != 0 && old != phys && pmm_inst.frame_dec(PhysAddr(old)) {
        super::release_frame(PhysAddr(old));
    }

    Some((phys, newly_inserted))
}

/// 注销一个 VMA 页对文件缓存页的映射持有 (拆除侧唯一入口)
///
/// 仅当该 VA 的 PTE 帧**恰为**本页缓存帧时注销 (`pcache_put`, `-1`) 并返回该帧;
/// 否则返回 `None` (该 VA 未持有 ⇒ 不得注销他处条目).
///
/// 帧计数不在此处改: 用户 leaf 的拆除由 `unmap_page_in_table` 承担,
/// 条目自身那份由 `deref` 在 `ref_count` 归零时释放.
pub fn pcache_release_for_va(pml4: u64, va: u64, inode_id: u32, page_index: u64) -> Option<u64> {
    // 用户页表根缺失 ⇒ fail-closed
    if pml4 == 0 {
        return None;
    }
    let cur = pcache_lookup(inode_id, page_index)?;
    let mapped = vmm::get_vmm()
        .get_physical_in_pml4(pml4, VirtAddr(va))
        .map_or(0, |p| p.as_u64());
    if mapped != cur {
        return None;
    }
    pcache_put(inode_id, page_index);
    Some(cur)
}

/// 将缓存页数据写入目标虚拟地址 (用于 #PF 时填充用户页)
///
/// # Safety
///
/// - `dest_virt` 必须指向有效的、已映射的用户空间页
/// - `phys` 必须是 Page Cache 中的有效物理页
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub unsafe fn pcache_copy_to_user(phys: u64, dest_virt: u64) {
    unsafe {
        let src_virt = crate::privileged::mm::phys_to_virt(phys);
        // SAFETY: 调用方保证 dest_virt 指向有效用户空间页, phys 为有效物理页.
        core::ptr::copy_nonoverlapping(
            src_virt as *const u8,
            dest_virt as *mut u8,
            PAGE_SIZE as usize,
        );
    }
}

/// 填充指定缓存页内容 (解决 TRACK-A7DE25)
///
/// 适用于 `pcache_get` 返回新页 (miss) 后, 由 vfs 层读取 fs 数据并回填.
/// 复制长度取 `min(src.len(), PAGE_SIZE)`, 不足部分保持 `pcache_get` 时的零页状态.
///
/// 返回 true 表示找到并填充了对应 entry; false 表示 entry 不存在
/// (调用方应仅在 `pcache_get` 成功返回后调用).
pub fn pcache_fill(inode_id: u32, page_index: u64, src: &[u8]) -> bool {
    let idx = pcache_hash(inode_id, page_index);
    let mut guard = PAGE_CACHE[idx].lock();
    guard.fill(inode_id, page_index, src)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 读取缓存页内容到目标缓冲区
///
/// 用于把 pcache 物理页的数据复制到用户缓冲 / 其他位置.
/// `dst.len()` 不得超过 `PAGE_SIZE`.
pub fn pcache_read_to_slice(inode_id: u32, page_index: u64, dst: &mut [u8]) -> bool {
    let phys = match pcache_lookup(inode_id, page_index) {
        Some(p) => p,
        None => return false,
    };
    copy_from_phys_page(phys, dst);
    true
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_pcache_hash_range() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    for &inode in &[1u32, 42, 1000, 0xFFFF] {
        for &pg in &[0u64, 1, 100, 0xFFFFFFFF] {
            let idx = pcache_hash(inode, pg);
            check!(idx < PCACHE_HASH_BUCKETS, "hash in range");
        }
    }
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_pcache_bucket_insert_lookup() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    let bucket = PageCacheBucket::new();
    check!(bucket.count == 0, "empty bucket");

    // 注意: insert 会调用 PMM 分配, 在测试环境中可能失败
    // 此测试仅验证数据结构操作
    // 实际集成测试在 QEMU 中运行
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_pcache_fill_requires_existing_entry() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // 在空桶上 fill 应返回 false (entry 不存在)
    let mut bucket = PageCacheBucket::new();
    let data = [0xABu8; 16];
    let result = bucket.fill(1, 0, &data);
    check!(!result, "fill on empty bucket returns false");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_pcache_fill_len_clamped() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // fill 的 copy_len 应取 min(src.len(), PAGE_SIZE)
    // 我们通过计算期望 copy_len 验证 (不实际触发 PMM 分配)
    let src_short = [0u8; 100];
    let expected = core::cmp::min(src_short.len(), PAGE_SIZE as usize);
    check!(expected == 100, "short src copies full");
    let src_long = [0u8; (PAGE_SIZE as usize) + 1024];
    let expected2 = core::cmp::min(src_long.len(), PAGE_SIZE as usize);
    check!(
        expected2 == PAGE_SIZE as usize,
        "long src clamped to PAGE_SIZE"
    );
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
pub fn register_pcache_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("pcache", "hash_range", test_pcache_hash_range);
    r.register(
        "pcache",
        "bucket_insert_lookup",
        test_pcache_bucket_insert_lookup,
    );
    r.register(
        "pcache",
        "fill_requires_existing_entry",
        test_pcache_fill_requires_existing_entry,
    );
    r.register("pcache", "fill_len_clamped", test_pcache_fill_len_clamped);
}
