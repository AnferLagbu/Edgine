//! Swap — 页面换出/换入与回收
//!
//! 当物理内存不足时, 将不活跃的页面换出到 swap 区, 释放物理页.
//! 当访问换出页面时, 通过 #PF 触发换入.
//!
//! ## 核心设计
//!
//! - **Swap 区**: 概念性块设备区域, 按 slot 索引 (每个 slot = 4KB)
//! - **Swap Entry**: 64 位编码, 存储在页表项中, 标识换出页面的位置
//!   - Bit 0: present = 0 (标识为 swap entry 而非 PTE)
//!   - Bit 1: swap 类型 (0 = 普通 swap)
//!   - Bits 2-55: swap slot 索引
//!   - Bits 56-63: 保留
//! - **LRU 链表**: active + inactive 双链表, 近似 LRU 回收
//! - **kswapd**: 内核线程, 周期性扫描 inactive 链表回收页面
//!
//! ## 换出流程
//!
//! ```text
//! kswapd 扫描 inactive 链表
//!   → 选中页面
//!   → 分配 swap slot
//!   → 写入 swap 区 (当前阶段: 复制到预留内存区域)
//!   → 更新 PTE 为 swap entry
//!   → 释放物理页
//! ```
//!
//! ## 换入流程
//!
//! ```text
//! #PF → 检测 PTE 为 swap entry
//!   → 分配新物理页
//!   → 从 swap slot 读取数据
//!   → 更新 PTE 为正常映射
//!   → 释放 swap slot
//! ```
//!
//! # Safety
//!
//! - Swap 位图由自旋锁保护
//! - 换出/换入操作在 #PF 上下文中执行, 必须无阻塞
//! - 当前阶段 swap 区使用预留内存区域模拟, 后续集成块设备

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::privileged::irq::{self, SoftirqVec};
use crate::privileged::mm::PfResult;
use crate::privileged::mm::{PAGE_SIZE, PhysAddr, VirtAddr, pmm, vmm};
use crate::privileged::sync::IrqSpinLock;

// ============================================================================
// Swap Entry 编码
// ============================================================================

/// Swap entry: 存储在页表项中, 标识换出页面
///
/// 编码格式 (64 位):
/// - Bit 0: 0 (present = 0, 区分正常 PTE)
/// - Bit 1: swap 类型 (0 = 普通 swap)
/// - Bits 2-55: swap slot 索引
/// - Bits 56-63: 保留
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapEntry(u64);

impl SwapEntry {
    /// 创建 swap entry
    pub fn new(slot: u64) -> Self {
        // Bit 0 = 0 (not present), Bit 1 = 0 (type 0)
        // Slot 存储在 bits 2-55
        Self((slot & 0x003F_FFFF_FFFF_FFFF) << 2)
    }

    /// 从 PTE 值解析 swap entry
    pub fn from_pte(pte: u64) -> Option<Self> {
        // present = 0 且非零值
        if pte & 1 != 0 || pte == 0 {
            return None;
        }
        // 检查是否为有效的 swap entry (bit 1 = 0)
        if pte & 0x2 != 0 {
            return None;
        }
        Some(Self(pte))
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取 swap slot 索引
    pub fn slot(&self) -> u64 {
        (self.0 >> 2) & 0x003F_FFFF_FFFF_FFFF
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 转换为 PTE 值
    pub fn to_pte(&self) -> u64 {
        self.0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 是否为有效的 swap entry
    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }
}

// ============================================================================
// Swap 区管理
// ============================================================================

/// Swap 区最大 slot 数 (当前阶段: 4096 slots = 16MB)
const SWAP_MAX_SLOTS: usize = 4096;

/// Swap slot 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SlotState {
    Free = 0,
    Used = 1,
}

/// Swap 区: 使用预留内存区域模拟
struct SwapArea {
    /// Slot 分配位图
    bitmap: [u8; SWAP_MAX_SLOTS],
    /// 已使用的 slot 数
    used_count: u64,
    /// Swap 数据存储区虚拟地址 (预留内存)
    storage_virt: u64,
    /// B03-03: swap 存储区物理基址 (用于 deinit 时 unreserve)
    storage_phys: PhysAddr,
    /// B03-03: swap 存储区字节数
    storage_size: usize,
    /// 是否已初始化
    initialized: bool,
}

impl SwapArea {
    const fn new() -> Self {
        Self {
            bitmap: [SlotState::Free as u8; SWAP_MAX_SLOTS],
            used_count: 0,
            storage_virt: 0,
            storage_phys: PhysAddr(0),
            storage_size: 0,
            initialized: false,
        }
    }

    /// 初始化 swap 区: 分配预留内存
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn init(&mut self) -> bool {
        if self.initialized {
            return true;
        }

        // B03-03 + DECISION-050: 改用 find_contig_range + reserve_range
        // 走 PMM "reserved 独占" 语义, 不走 alloc_page 簿记 (避免永久泄漏).
        // swap slot_addr 假设物理连续 (storage_virt + slot * PAGE_SIZE),
        // 因此必须保证基址连续范围.
        let pmm_inst = pmm::get_pmm();
        let swap_bytes = SWAP_MAX_SLOTS * PAGE_SIZE as usize;

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let Some(phys_base) = pmm_inst.find_contig_range(swap_bytes) else {
            crate::klog_warn!(
                Swap,
                "[SWAP] init: no {} MB contig range (degraded mode)",
                swap_bytes / (1024 * 1024)
            );
            return false;
        };

        if let Err(e) = pmm_inst.reserve_range(phys_base, swap_bytes) {
            crate::klog_warn!(Swap, "[SWAP] init: reserve_range failed: {}", e);
            return false;
        }

        // 清零存储区
        let virt_base = phys_base.to_virt().0;
        // SAFETY: virt_base 指向 reserve_range 保证独占的连续物理范围
        unsafe {
            core::ptr::write_bytes(virt_base as *mut u8, 0, swap_bytes);
        }

        self.storage_virt = virt_base;
        self.storage_phys = phys_base;
        self.storage_size = swap_bytes;
        self.initialized = true;
        crate::klog_info!(
            Swap,
            "[SWAP] Initialized: {} slots ({} MB) at phys 0x{:X}",
            SWAP_MAX_SLOTS,
            swap_bytes / (1024 * 1024),
            phys_base.as_u64()
        );
        true
    }

    /// B03-03 回滚路径: unreserve 存储区, 恢复 PMM 簿记 (供测试与将来热卸载).
    /// 当前未在生产路径调用 (swap 无卸载场景), 但保证函数可用于 host-tests。
    fn deinit(&mut self) -> bool {
        if !self.initialized {
            return true;
        }
        // 清零自身状态
        self.storage_virt = 0;
        self.initialized = false;
        // unreserve: 重置 PMM 簿记 (注意 reserve_range 已 set_bit, 需反向 clear_bit)
        let pmm_inst = pmm::get_pmm();
        if let Err(e) = pmm_inst.unreserve_range(self.storage_phys, self.storage_size) {
            crate::klog_warn!(Swap, "[SWAP] deinit: unreserve_range failed: {}", e);
            return false;
        }
        self.storage_phys = PhysAddr(0);
        self.storage_size = 0;
        true
    }

    /// 分配一个空闲 slot, 返回 slot 索引
    fn alloc_slot(&mut self) -> Option<u64> {
        for (i, slot) in self.bitmap.iter_mut().enumerate() {
            if *slot == SlotState::Free as u8 {
                *slot = SlotState::Used as u8;
                self.used_count += 1;
                return Some(i as u64);
            }
        }
        None
    }

    /// 释放 slot
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn free_slot(&mut self, slot: u64) {
        let idx = slot as usize;
        if idx < SWAP_MAX_SLOTS && self.bitmap[idx] == SlotState::Used as u8 {
            self.bitmap[idx] = SlotState::Free as u8;
            self.used_count -= 1;
        }
    }

    /// 获取 slot 对应的虚拟地址
    fn slot_addr(&self, slot: u64) -> u64 {
        self.storage_virt + slot * PAGE_SIZE
    }

    /// 将数据写入 swap slot
    ///
    /// # Safety
    ///
    /// - `src_virt` 必须指向有效的 4KB 数据源
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn write_slot(&self, slot: u64, src_virt: u64) {
        if !self.initialized {
            return;
        }
        let dst = self.slot_addr(slot) as *mut u8;
        // SAFETY: dst 指向 swap 存储区 (已分配并映射), src 为有效用户页
        unsafe {
            core::ptr::copy_nonoverlapping(src_virt as *const u8, dst, PAGE_SIZE as usize);
        }
    }

    /// 从 swap slot 读取数据
    ///
    /// # Safety
    ///
    /// - `dst_virt` 必须指向有效的 4KB 目标页
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn read_slot(&self, slot: u64, dst_virt: u64) {
        if !self.initialized {
            return;
        }
        let src = self.slot_addr(slot) as *const u8;
        // SAFETY: src 指向 swap 存储区, dst 为有效物理页
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst_virt as *mut u8, PAGE_SIZE as usize);
        }
    }

    /// 空闲 slot 数
    fn free_slots(&self) -> u64 {
        SWAP_MAX_SLOTS as u64 - self.used_count
    }
}

// ============================================================================
// LRU 链表 (简化版)
// ============================================================================

/// LRU 页面跟踪
///
/// 使用固定大小数组跟踪最近访问的页面,
/// 按访问时间排序 (新访问的页移到尾部).
struct LruList {
    /// Active 链表: 最近被访问的页面
    active: [LruEntry; LRU_CAPACITY],
    active_count: usize,
    /// Inactive 链表: 较久未访问的页面 (回收候选)
    inactive: [LruEntry; LRU_CAPACITY],
    inactive_count: usize,
}

const LRU_CAPACITY: usize = 256;

#[derive(Clone, Copy)]
struct LruEntry {
    /// 所属进程的 PML4 (CR3), 用于 swap-out 时写 PTE 为 swap entry
    pml4: u64,
    virt_addr: u64,
    phys_addr: u64,
    /// 是否为脏页
    dirty: bool,
    /// 是否被占用
    occupied: bool,
    /// 是否被 mlock 锁定 (锁定页不参与 swap/reclaim, 跳过回收候选)
    /// 设置时机: `set_page_locked()` 或 `lru_touch` 时由 `vma_flags.MLOCKED` 推导
    locked: bool,
}

impl LruEntry {
    const fn empty() -> Self {
        Self {
            pml4: 0,
            virt_addr: 0,
            phys_addr: 0,
            dirty: false,
            occupied: false,
            locked: false,
        }
    }
}

impl LruList {
    const fn new() -> Self {
        Self {
            active: [LruEntry::empty(); LRU_CAPACITY],
            active_count: 0,
            inactive: [LruEntry::empty(); LRU_CAPACITY],
            inactive_count: 0,
        }
    }

    /// 添加页面到 active 链表 (页面被访问时调用)
    ///
    /// pml4 为该虚拟地址所属进程的 CR3, 用于 swap-out 时写 PTE 为 swap entry.
    /// locked 表示该页是否被 mlock 锁定, 由调用方根据 VMA `vm_flags.MLOCKED` 推导.
    fn add_active(&mut self, pml4: u64, virt_addr: u64, phys_addr: u64, dirty: bool, locked: bool) {
        // 先检查是否已在 inactive 链表中, 若是则提升
        for i in 0..LRU_CAPACITY {
            if self.inactive[i].occupied && self.inactive[i].virt_addr == virt_addr {
                // 从 inactive 移除, 加入 active (保留 locked 标志)
                let mut entry = self.inactive[i];
                self.inactive[i] = LruEntry::empty();
                self.inactive_count -= 1;
                entry.locked = locked || entry.locked;
                entry.dirty |= dirty;
                self.push_active(
                    entry.pml4,
                    entry.virt_addr,
                    entry.phys_addr,
                    entry.dirty,
                    entry.locked,
                );
                return;
            }
        }

        // 不在 inactive 中, 直接加入 active
        self.push_active(pml4, virt_addr, phys_addr, dirty, locked);
    }

    fn push_active(
        &mut self,
        pml4: u64,
        virt_addr: u64,
        phys_addr: u64,
        dirty: bool,
        locked: bool,
    ) {
        // 检查是否已在 active 中
        for i in 0..LRU_CAPACITY {
            if self.active[i].occupied && self.active[i].virt_addr == virt_addr {
                self.active[i].dirty = dirty;
                // locked 单调: 一旦锁定不可解除 (仅清 VMA flag 不影响 LRU 锁)
                if locked {
                    self.active[i].locked = true;
                }
                return;
            }
        }

        // T2-4: 降级决策委托给 SwapPolicy
        if super::swap_trait::current_swap_policy()
            .should_demote_active(self.active_count, LRU_CAPACITY)
        {
            self.demote_oldest();
        }

        // 添加到 active 尾部
        for i in 0..LRU_CAPACITY {
            if !self.active[i].occupied {
                self.active[i] = LruEntry {
                    pml4,
                    virt_addr,
                    phys_addr,
                    dirty,
                    occupied: true,
                    locked,
                };
                self.active_count += 1;
                return;
            }
        }
    }

    /// 将 active 链表最旧的条目降级到 inactive
    fn demote_oldest(&mut self) {
        // 找到 active 中第一个条目 (最旧)
        for i in 0..LRU_CAPACITY {
            if self.active[i].occupied {
                let entry = self.active[i];
                self.active[i] = LruEntry::empty();
                self.active_count -= 1;

                // T2-4: inactive 满时丢弃决策委托给 SwapPolicy
                if super::swap_trait::current_swap_policy()
                    .should_evict_inactive(self.inactive_count, LRU_CAPACITY)
                {
                    // 移除最旧的 inactive 条目 (跳过 locked 优先保护)
                    for j in 0..LRU_CAPACITY {
                        if self.inactive[j].occupied && !self.inactive[j].locked {
                            self.inactive[j] = LruEntry::empty();
                            self.inactive_count -= 1;
                            break;
                        }
                    }
                }

                // 添加到 inactive
                for j in 0..LRU_CAPACITY {
                    if !self.inactive[j].occupied {
                        self.inactive[j] = entry;
                        self.inactive_count += 1;
                        return;
                    }
                }
                return;
            }
        }
    }

    /// 从 inactive 链表获取回收候选
    ///
    /// T2-4: 选择策略委托给 `SwapPolicy::select_victim`.
    /// **跳过 locked 页**: mlock 锁定的页保留在链表中, 不被回收.
    fn get_victim(&mut self) -> Option<LruEntry> {
        // 构建候选视图供策略决策
        let candidates: [Option<super::swap_trait::LruPageInfo>; LRU_CAPACITY] = {
            let mut arr = [None; LRU_CAPACITY];
            for i in 0..LRU_CAPACITY {
                if self.inactive[i].occupied {
                    arr[i] = Some(super::swap_trait::LruPageInfo {
                        pml4: self.inactive[i].pml4,
                        virt_addr: self.inactive[i].virt_addr,
                        phys_addr: self.inactive[i].phys_addr,
                        dirty: self.inactive[i].dirty,
                        locked: self.inactive[i].locked,
                    });
                }
            }
            arr
        };

        let idx = super::swap_trait::current_swap_policy().select_victim(&candidates)?;
        let entry = self.inactive[idx];
        self.inactive[idx] = LruEntry::empty();
        self.inactive_count -= 1;
        Some(entry)
    }

    /// 标记某虚拟地址对应的 LRU 条目为 locked (mlock)
    ///
    /// 用于 mlock 系统调用: 锁定 VMA 范围时遍历 VMA 内每个触达页.
    /// 若该页未在 LRU 跟踪 (未被触达过), 跳过 — locked 状态由 VMA `vm_flags` 承载.
    fn set_locked(&mut self, virt_addr: u64, locked: bool) -> bool {
        for i in 0..LRU_CAPACITY {
            if self.active[i].occupied && self.active[i].virt_addr == virt_addr {
                self.active[i].locked = locked;
                return true;
            }
        }
        for i in 0..LRU_CAPACITY {
            if self.inactive[i].occupied && self.inactive[i].virt_addr == virt_addr {
                self.inactive[i].locked = locked;
                return true;
            }
        }
        false
    }

    /// 查询某虚拟地址的 LRU 锁定状态
    fn is_locked(&self, virt_addr: u64) -> Option<bool> {
        for i in 0..LRU_CAPACITY {
            if self.active[i].occupied && self.active[i].virt_addr == virt_addr {
                return Some(self.active[i].locked);
            }
        }
        for i in 0..LRU_CAPACITY {
            if self.inactive[i].occupied && self.inactive[i].virt_addr == virt_addr {
                return Some(self.inactive[i].locked);
            }
        }
        None
    }
}

// ============================================================================
// 全局 Swap 状态
// ============================================================================

struct SwapState {
    lock: IrqSpinLock<()>,
    area: UnsafeCell<SwapArea>,
    lru: UnsafeCell<LruList>,
}

// SAFETY: SwapState 由自旋锁保护
unsafe impl Sync for SwapState {}
unsafe impl Send for SwapState {}

static SWAP: SwapState = SwapState {
    lock: IrqSpinLock::new(()),
    area: UnsafeCell::new(SwapArea::new()),
    lru: UnsafeCell::new(LruList::new()),
};

// ============================================================================
// 公共 API
// ============================================================================

/// 初始化 swap 子系统
pub fn swap_init() -> bool {
    let _guard = SWAP.lock.lock();
    // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
    let area = unsafe { &mut *SWAP.area.get() };
    area.init()
}

/// B03-03 + DECISION-050: swap 子系统卸载/回滚入口
///
/// 释放 swap 存储区回 PMM 池 (unreserve_range), 清零状态。
/// 当前生产路径无调用方 (swap 是持久子系统), 但保证可用于:
/// - host-tests 的 init/deinit 配对测试
/// - 将来 swap 模块热卸载
pub fn swap_deinit() -> bool {
    let _guard = SWAP.lock.lock();
    // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
    let area = unsafe { &mut *SWAP.area.get() };
    area.deinit()
}

/// 换出页面: 将物理页写入 swap, 返回 swap entry
///
/// # Safety
///
/// - `virt_addr` 必须是已映射的用户空间虚拟地址
/// - `phys_addr` 必须是对应的有效物理地址
/// - **不更新 PTE** — 调用方需自行将 PTE 替换为 swap entry 或 unmap
pub fn swap_out(virt_addr: u64, phys_addr: u64, _dirty: bool) -> Option<SwapEntry> {
    let _guard = SWAP.lock.lock();
    let area = unsafe { &mut *SWAP.area.get() };

    if !area.initialized {
        return None;
    }

    let slot = area.alloc_slot()?;

    // 将页面数据写入 swap slot
    let src_virt = phys_addr + crate::privileged::mm::KERNEL_BASE;
    area.write_slot(slot, src_virt);

    let entry = SwapEntry::new(slot);

    crate::klog_debug!(
        Swap,
        "[SWAP] Out: vaddr={:#x} paddr={:#x} -> slot={}",
        virt_addr,
        phys_addr,
        slot
    );

    Some(entry)
}

/// 换出页面: 完整实现 — 分配 slot + 写入数据 + 替换 PTE 为 swap entry
///
/// 返回 (`SwapEntry`, `old_phys`).
/// 若 slot 分配失败或 PTE 替换失败, 返回 None.
///
/// ## 完整实现要点
/// 1. 从 LRU inactive 链表中选 victim
/// 2. `alloc_slot` + `write_slot` (`swap_out` 内)
/// 3. `vmm.set_pte_value(pml4`, virt, `swap_entry.to_pte()`) — PTE 替换
/// 4. `pmm.free_page(phys)` — 释放物理页
///
/// # Safety
///
/// - `virt_addr` 必须属于由 pml4 标识的地址空间
/// - pml4 必须是有效的页表根 (CR3 值)
/// - `virt_addr` 对应的 PTE 当前必须是 present 页
pub fn swap_out_to_pte(pml4: u64, virt_addr: u64) -> Option<SwapEntry> {
    // 1. 读取当前 PTE 获取物理地址
    let vmm_inst = vmm::get_vmm();
    let pte = vmm_inst.get_pte_value(pml4, VirtAddr(virt_addr))?;
    if (pte & 1) == 0 {
        // 已是非 present 页, 不应再换出
        return None;
    }
    let phys_addr = pte & 0x000F_FFFF_FFFF_F000;

    // 2. swap_out (分配 slot + 写入)
    let entry = swap_out(virt_addr, phys_addr, false)?;

    // 3. 替换 PTE 为 swap entry
    vmm_inst.set_pte_value(pml4, VirtAddr(virt_addr), entry.to_pte());

    crate::klog_debug!(
        Swap,
        "[SWAP] Out→PTE: vaddr={:#x} pml4={:#x} slot={}",
        virt_addr,
        pml4,
        entry.slot()
    );

    Some(entry)
}

/// 换入页面: 从 swap slot 读取数据到新物理页
///
/// # Safety
///
/// - 调用者确保 `entry` 是有效的 swap entry
/// - 返回新分配的物理地址
pub fn swap_in(entry: SwapEntry) -> Option<PhysAddr> {
    let slot = entry.slot();

    let _guard = SWAP.lock.lock();
    let area = unsafe { &mut *SWAP.area.get() };

    if !area.initialized {
        return None;
    }

    // 分配新物理页
    let pmm_inst = pmm::get_pmm();
    let new_phys = pmm_inst.alloc_page()?;

    // 从 swap slot 读取数据
    let dst_virt = new_phys.to_virt().0;
    area.read_slot(slot, dst_virt);

    // 释放 swap slot
    area.free_slot(slot);

    crate::klog_debug!(
        Swap,
        "[SWAP] In: slot={} -> paddr={:#x}",
        slot,
        new_phys.as_u64()
    );

    Some(new_phys)
}

/// 释放 swap slot (页面被修改写回时调用)
pub fn swap_free(entry: SwapEntry) {
    let slot = entry.slot();
    let _guard = SWAP.lock.lock();
    // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
    let area = unsafe { &mut *SWAP.area.get() };
    area.free_slot(slot);
}

/// 检测 PTE 是否为 swap entry
pub fn is_swap_pte(pte: u64) -> bool {
    SwapEntry::from_pte(pte).is_some()
}

/// 记录页面访问 (添加到 LRU active 链表)
///
/// pml4 必须为该虚拟地址所属进程的 CR3, 用于 swap-out 时写 PTE 为 swap entry.
/// `locked` 表示该页是否被 mlock 锁定 (由调用方根据 VMA `vm_flags.MLOCKED` 推导).
pub fn lru_touch(pml4: u64, virt_addr: u64, phys_addr: u64, dirty: bool, locked: bool) {
    let _guard = SWAP.lock.lock();
    // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
    let lru = unsafe { &mut *SWAP.lru.get() };
    lru.add_active(pml4, virt_addr, phys_addr, dirty, locked);
}

/// 设置某虚拟地址对应 LRU 条目的 mlock 锁定状态
///
/// 返回 true 表示 LRU 中有该条目 (并已更新), false 表示该页未在 LRU 跟踪
/// (尚未触达, locked 状态由 VMA `vm_flags` 承载, 触达时会通过 `lru_touch` 锁定).
///
/// # 用法
///
/// - `mlock` 系统调用: 对 VMA 内每页调用 `set_page_locked(virt, true)`
/// - `munlock` 系统调用: 对 VMA 内每页调用 `set_page_locked(virt, false)`
/// - 保留的 locked 状态对 `get_victim` 不可见, 不会被 swap-out
pub fn set_page_locked(virt_addr: u64, locked: bool) -> bool {
    let _guard = SWAP.lock.lock();
    // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
    let lru = unsafe { &mut *SWAP.lru.get() };
    lru.set_locked(virt_addr, locked)
}

/// 查询某虚拟地址对应 LRU 条目的 mlock 锁定状态
///
/// 返回 Some(true/false) 表示 LRU 中存在该条目, None 表示该页未在 LRU 跟踪.
pub fn is_page_locked(virt_addr: u64) -> Option<bool> {
    let _guard = SWAP.lock.lock();
    // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
    let lru = unsafe { &*SWAP.lru.get() };
    lru.is_locked(virt_addr)
}

/// 回收页面 (从 LRU inactive 链表选取并换出)
///
/// 返回换出的页面数
pub fn reclaim_pages(max_count: u32) -> u32 {
    let mut reclaimed = 0u32;

    while reclaimed < max_count {
        let victim = {
            let _guard = SWAP.lock.lock();
            // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
            let lru = unsafe { &mut *SWAP.lru.get() };
            lru.get_victim()
        };

        match victim {
            Some(entry) => {
                // 完整 swap-out 路径: 写 PTE 为 swap entry, #PF 时通过 handle_swap_fault 换入
                if entry.pml4 != 0 {
                    // 有 pml4 记录: 走 swap_out_to_pte 完整路径
                    if let Some(_swap_entry) = swap_out_to_pte(entry.pml4, entry.virt_addr) {
                        // PTE 已替换为 swap entry; 释放物理页
                        let pmm_inst = pmm::get_pmm();
                        pmm_inst.free_page(PhysAddr(entry.phys_addr));
                        reclaimed += 1;
                    } else {
                        break;
                    }
                } else {
                    // 无 pml4 记录 (老 LRU 条目): 退化路径, 走 unmap
                    if swap_out(entry.virt_addr, entry.phys_addr, entry.dirty).is_some() {
                        let vmm_inst = vmm::get_vmm();
                        vmm_inst.unmap_page(VirtAddr(entry.virt_addr));
                        let pmm_inst = pmm::get_pmm();
                        pmm_inst.free_page(PhysAddr(entry.phys_addr));
                        reclaimed += 1;
                    } else {
                        break;
                    }
                }
            }
            None => break,
        }
    }

    if reclaimed > 0 {
        crate::klog_debug!(Swap, "[SWAP] Reclaimed {} pages", reclaimed);
    }

    reclaimed
}

/// 获取 swap 信息
pub fn swap_info() -> (u64, u64) {
    let _guard = SWAP.lock.lock();
    // SAFETY: `SWAP` 由调用方保证为有效指针; 只读访问
    let area = unsafe { &mut *SWAP.area.get() };
    let total = SWAP_MAX_SLOTS as u64;
    let free = area.free_slots();
    (total * PAGE_SIZE, free * PAGE_SIZE)
}

// ============================================================================
// #PF Swap Entry 检测
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 处理 swap-in 缺页
///
/// 当 #PF 检测到 PTE 为 swap entry 时调用.
/// 分配新物理页, 从 swap 读取数据, 重新建立映射.
pub fn handle_swap_fault(pml4: u64, fault_addr: u64) -> PfResult {
    let vmm_inst = vmm::get_vmm();

    // 读取当前 PTE
    let pte = match vmm_inst.get_pte_value(pml4, VirtAddr(fault_addr)) {
        Some(v) => v,
        None => return PfResult::SignalSegv,
    };

    let entry = match SwapEntry::from_pte(pte) {
        Some(e) => e,
        None => return PfResult::SignalSegv,
    };

    // 换入
    let new_phys = match swap_in(entry) {
        Some(p) => p,
        None => return PfResult::Oom,
    };

    // 重新建立映射
    let flags = crate::privileged::mm::PageFlags::PRESENT
        | crate::privileged::mm::PageFlags::WRITABLE
        | crate::privileged::mm::PageFlags::USER;
    vmm_inst.map_page_in_table(pml4, VirtAddr(fault_addr), new_phys, flags);

    PfResult::Fixed
}

// ============================================================================
// kswapd: 内存回收/页面换出 (softirq 驱动)
// ============================================================================
//
// ## 设计
//
// Edgine 当前没有 kthread 抽象, 因此 kswapd 走 softirq 路径:
//   1. `kswapd_init()`: 注册 Kswapd softirq handler, 调度器 tick 周期触发
//   2. `kswapd_wakeup()`: 立即 raise_softirq, 异步执行 reclaim_pages
//   3. `kswapd_softirq_handler()`: softirq 上下文执行 reclaim_pages
//
// ## 触发源
//
// - scheduler.tick 周期触发 (例: 每 100 ticks)
// - pressure 级别跃迁 (Warning/Critical/Emergency) 触发
// - mmap 失败 (可选, 当前未启用)
//
// ## 限制
//
// - softirq 上下文: 不可睡眠, 不可长时间持锁
// - reclaim_pages 当前实现持 SWAP.lock; 锁粒度已细化, 软中断可接受
// - 单 CPU 串行: 多核并行 swap 由各 CPU softirq 自行触发

/// kswapd softirq 触发状态
static KSWAPD_PENDING: AtomicBool = AtomicBool::new(false);

/// kswapd softirq handler — 在 softirq 上下文执行
///
/// 每次唤醒回收 `RECLAIM_BATCH` 个页面. 软中断上下文不可睡眠,
/// 但 `reclaim_pages` 的所有子操作 (`swap_out` / `vmm.set_pte_value` / `pmm.free_page`)
/// 均为非阻塞原子操作, 满足软中断约束.
fn kswapd_softirq_handler() {
    // 原子清除 pending 标志
    KSWAPD_PENDING.store(false, Ordering::Release);

    // T2-4: 回收批量大小委托给 SwapPolicy
    let ctx = {
        let _guard = SWAP.lock.lock();
        // SAFETY: SWAP.lock 已加锁, area 和 lru 的内部可变性受锁保护
        let area = unsafe { &*SWAP.area.get() };
        let lru = unsafe { &*SWAP.lru.get() };
        super::swap_trait::SwapPolicyContext {
            total_slots: SWAP_MAX_SLOTS as u64,
            used_slots: area.used_count,
            active_count: lru.active_count,
            inactive_count: lru.inactive_count,
            free_pages: pmm::get_pmm().get_free_pages(),
            total_pages: pmm::get_pmm().get_total_pages(),
        }
    };

    let batch = super::swap_trait::current_swap_policy().reclaim_batch_size(ctx);
    let reclaimed = reclaim_pages(batch);
    if reclaimed > 0 {
        crate::klog_debug!(Swap, "[KSWAPD] softirq reclaimed {} pages", reclaimed);
    }
}

/// 初始化 kswapd: 注册 Kswapd softirq handler
///
/// 必须在 irq 子系统初始化 (`interrupt_late_init`) 之后调用.
pub fn kswapd_init() {
    irq::open_softirq(SoftirqVec::Kswapd, kswapd_softirq_handler);
    crate::klog_info!(Swap, "[KSWAPD] softirq handler registered");
}

/// 唤醒 kswapd: 立即 raise Kswapd softirq
///
/// 由 scheduler.tick 周期调用, 或 pressure 跃迁调用.
/// 重复唤醒是幂等的 (`raise_softirq` 仅设置 pending bit).
///
/// T2-4: 触发条件委托给 `SwapPolicy::should_wakeup_kswapd`.
pub fn kswapd_wakeup() {
    // 快速路径: 若已 pending, 不重复触发
    if KSWAPD_PENDING.load(Ordering::Acquire) {
        return;
    }

    // 策略决策: 是否应该唤醒
    let ctx = {
        let _guard = SWAP.lock.lock();
        // SAFETY: SWAP.lock 已加锁, area 和 lru 的内部可变性受锁保护
        let area = unsafe { &*SWAP.area.get() };
        let lru = unsafe { &*SWAP.lru.get() };
        super::swap_trait::SwapPolicyContext {
            total_slots: SWAP_MAX_SLOTS as u64,
            used_slots: area.used_count,
            active_count: lru.active_count,
            inactive_count: lru.inactive_count,
            free_pages: pmm::get_pmm().get_free_pages(),
            total_pages: pmm::get_pmm().get_total_pages(),
        }
    };

    if !super::swap_trait::current_swap_policy().should_wakeup_kswapd(ctx) {
        return;
    }

    KSWAPD_PENDING.store(true, Ordering::Release);
    irq::raise_softirq(SoftirqVec::Kswapd);
}

/// 检查 kswapd 是否处于 pending (诊断接口)
pub fn kswapd_is_pending() -> bool {
    KSWAPD_PENDING.load(Ordering::Acquire)
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_swap_entry_encoding() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};

    let entry = SwapEntry::new(42);
    assert_eq_test!(entry.slot(), 42, "slot decode");

    let pte = entry.to_pte();
    assert_eq_test!(pte & 1, 0, "not present");
    check!(SwapEntry::from_pte(pte).is_some(), "from_pte valid");

    // present=1 的 PTE 不应被解析为 swap entry
    check!(
        SwapEntry::from_pte(0x1001).is_none(),
        "present pte not swap"
    );
    // 零值不应被解析
    check!(SwapEntry::from_pte(0).is_none(), "zero not swap");

    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_swap_entry_large_slot() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};

    let entry = SwapEntry::new(0x003F_FFFF_FFFF_FFFF);
    assert_eq_test!(entry.slot(), 0x003F_FFFF_FFFF_FFFF, "max slot");

    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
pub fn register_swap_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("swap", "entry_encoding", test_swap_entry_encoding);
    r.register("swap", "entry_large_slot", test_swap_entry_large_slot);
}
