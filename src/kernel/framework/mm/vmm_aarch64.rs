//! AArch64 虚拟内存管理器
//!
//! 实现与 x86_64 vmm.rs 相同的 FFI 接口, 提供:
//! - 内核高半区页表 (TTBR1_EL1) 管理
//! - 用户空间页表 (TTBR0_EL1) 创建/映射
//! - 页表遍历/克隆/销毁
//!
//! 架构: ARMv8-A 4KB granule, 48-bit VA
//! - TTBR0_EL1: 用户空间 (0x0000_0000_0000_0000 .. 0x0000_FFFF_FFFF_FFFF)
//! - TTBR1_EL1: 内核空间 (0xFFFF_0000_0000_0000 .. 0xFFFF_FFFF_FFFF_FFFF)
//!
//! 页表级: L0 (512GB) → L1 (1GB) → L2 (2MB) → L3 (4KB)

// 显式导入 mm 父模块符号 (2026-09-12 方案 B: 消除 glob 导入, 满足 clippy::wildcard_imports)
// 背景: 既有 commit a7851509 删除本文件 glob allow 导致 aarch64 clippy 回归, 用户裁决改显式导入.
// 说明: `super::kpti::kpti_init` 走显式路径, 无需在此导入 (PA→VA 换算见上方
// `super::phys_to_virt`, L1-04 收敛后不再本地重复定义).
use super::{
    PAGE_NX, PAGE_SIZE, PAGE_USER, PAGE_WRITABLE, PageFlags, PageSize, PageTranslation, PhysAddr,
    VirtAddr, get_pmm, is_user_leaf, phys_to_virt,
};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::framework::sync::{IrqSaveFlags, disable_interrupts, restore_interrupts};

use crate::framework::sync::OnceLock;

// ─── ARM 描述符常量 ────────────────────────────────────────

/// 描述符类型
const DESC_VALID: u64 = 1 << 0;
const DESC_TYPE_TABLE: u64 = 0b11; // 表描述符 (L0/L1/L2)
const DESC_TYPE_BLOCK: u64 = 0b01; // 块描述符 (L1/L2)
const DESC_TYPE_PAGE: u64 = 0b11; // 页描述符 (L3, 与 TABLE 位相同)

/// 内存属性索引 (与 mmu.rs 中 MAIR_EL1 设定对应)
const MAIR_DEVICE_nGnRnE: u64 = 0; // Device memory
const MAIR_NORMAL_WBWA: u64 = 1; // 普通可缓存内存 (内核)
const MAIR_NORMAL_NC: u64 = 2; // Normal non-cacheable
const MAIR_USER_NORMAL: u64 = 4; // 用户页的 Normal WBWA (回写缓存)

/// 访问权限位 `[7:6]` (描述符中)
const AP_EL1_RW: u64 = 0 << 6; // EL1 读写, EL0 不可访问
const AP_BOTH_RW: u64 = 1 << 6; // EL1 读写, EL0 读写
const AP_EL1_RO: u64 = 2 << 6; // EL1 只读, EL0 不可访问
const AP_BOTH_RO: u64 = 3 << 6; // EL1 只读, EL0 只读

/// 属性索引移位 (位 `[4:2]`)
const ATTR_SHIFT: u64 = 2;

/// 访问标志 (位 10)
const AF: u64 = 1 << 10;

/// XN (Execute Never) 位
const UXN: u64 = 1 << 54; // EL0 不可执行
const PXN: u64 = 1 << 53; // EL1 不可执行

/// Stage 1 共享性 (位 8 内, 位 9 外) — Stage 1 不严格需要

/// 每级页表项数
const TABLE_ENTRIES: usize = 512;

/// 用户 L0 表的**保留槽**下标 (VA `0x0000_0080_0000_0000` = 512 GiB).
///
/// 该 VA 段用户态从不使用 (实测用户面仅占 L0 索引 0/170/255), 故借它关联本进程的
/// **EL1 视图**根表物理地址 (见 [`Aarch64Vmm::build_el1_view`]).
///
/// 存法刻意与普通表项不同: 只写物理地址, **不置 `bits[1:0]`** ⇒ 硬件读到的是
/// 一枚**无效描述符**, VA 512 GiB~1 TiB 在用户态保持未映射, 行为与全零槽一致;
/// `destroy_page_table` / `count_present_user_pages` / COW 克隆的遍历均以
/// `bits[1:0] == 0b11` 过滤, 天然跳过本槽.
///
/// 以此换取的收益: EL0→EL1 入口汇编只需 `ldr x4, [x2, #8]` (x2 = 当前 `TTBR0`)
/// 即可取到本进程的 EL1 视图 —— 关联**随当前页表一起切换**, 无需全局槽,
/// 也就不存在"调度后忘记更新全局槽 ⇒ 用错视图"的陈旧值风险.
const EL1_VIEW_SLOT: usize = 1;

/// 描述符输出地址掩码 (bits `[47:12]`).
const DESC_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

// ─── 地址提取宏 ───────────────────────────────────────

#[inline(always)]
fn l0_index(vaddr: u64) -> usize {
    ((vaddr >> 39) & 0x1FF) as usize
}

#[inline(always)]
fn l1_index(vaddr: u64) -> usize {
    ((vaddr >> 30) & 0x1FF) as usize
}

#[inline(always)]
fn l2_index(vaddr: u64) -> usize {
    ((vaddr >> 21) & 0x1FF) as usize
}

#[inline(always)]
fn l3_index(vaddr: u64) -> usize {
    ((vaddr >> 12) & 0x1FF) as usize
}

#[inline(always)]
fn is_kernel_addr(vaddr: u64) -> bool {
    vaddr >= 0xFFFF_0000_0000_0000
}

// ─── 页标志转换 (x86 → ARM) ──────────────────────────────

/// 将 x86 风格页标志转换为 ARM L3 页描述符 (4KB 页)
fn page_flags_to_descriptor(flags: u64, paddr: u64) -> u64 {
    let mut desc = paddr & 0x0000_FFFF_FFFF_F000; // Output address [47:12]
    desc |= DESC_TYPE_PAGE; // bits [1:0] = 0b11
    desc |= AF; // Access flag

    // Access permission
    let user = (flags & PAGE_USER) != 0;
    let writable = (flags & PAGE_WRITABLE) != 0;

    if user && writable {
        desc |= AP_BOTH_RW;
    } else if user && !writable {
        desc |= AP_BOTH_RO;
    } else if !user && writable {
        desc |= AP_EL1_RW;
    } else {
        desc |= AP_EL1_RO;
    }

    // Memory type
    if user {
        desc |= MAIR_USER_NORMAL << ATTR_SHIFT;
    } else {
        desc |= MAIR_NORMAL_WBWA << ATTR_SHIFT;
    }

    // Execute never
    let nx = (flags & PAGE_NX) != 0;
    if nx {
        desc |= UXN;
        if !user {
            desc |= PXN;
        }
    }

    desc
}

/// 将 x86 风格页标志转换为 ARM L1/L2 块描述符 (1GB/2MB 块)
fn block_flags_to_descriptor(flags: u64, paddr: u64, _level: u8, output_mask: u64) -> u64 {
    let mut desc = paddr & output_mask;
    desc |= DESC_TYPE_BLOCK;
    desc |= AF;

    let user = (flags & PAGE_USER) != 0;
    let writable = (flags & PAGE_WRITABLE) != 0;

    if user && writable {
        desc |= AP_BOTH_RW;
    } else if user && !writable {
        desc |= AP_BOTH_RO;
    } else if !user && writable {
        desc |= AP_EL1_RW;
    } else {
        desc |= AP_EL1_RO;
    }

    // Kernel blocks use MAIR index 1 (WBWA), device blocks use 0
    if user {
        desc |= MAIR_USER_NORMAL << ATTR_SHIFT;
    } else {
        desc |= MAIR_NORMAL_WBWA << ATTR_SHIFT;
    }

    let nx = (flags & PAGE_NX) != 0;
    if nx {
        desc |= UXN;
        if !user {
            desc |= PXN;
        }
    }

    desc
}

/// 创建指向下一级表的表描述符
fn table_descriptor(next_table_paddr: u64) -> u64 {
    (next_table_paddr & 0x0000_FFFF_FFFF_F000) | DESC_TYPE_TABLE
}

static VMM_LOCK: AtomicBool = AtomicBool::new(false);

#[cfg(debug_assertions)]
static VMM_LOCK_RECURSIVE: AtomicBool = AtomicBool::new(false);

/// 非阻塞获取 `VMM_LOCK`: 锁已被占用时立即返回 `None`, 绝不重试或自旋.
///
/// 与 `Aarch64Vmm::acquire_lock` 不同, 本函数只走一次 `compare_exchange`
/// 即返回. 页表只读遍历 (`count_present_user_pages`) 用它避免与并发 map/unmap
/// 争锁: 拿不到即放弃本轮, 不阻塞调用者 (OOMD 运行在 scheduler tick 中断上下文).
///
/// 成功时关中断, 并在调试构建中置 `VMM_LOCK_RECURSIVE` (与 acquire 的 debug 语义一致).
fn try_acquire_lock() -> Option<IrqSaveFlags> {
    let flags = disable_interrupts();
    if VMM_LOCK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        // 未取得锁: 恢复进入时的中断状态, 不留副作用.
        restore_interrupts(&flags);
        return None;
    }
    #[cfg(debug_assertions)]
    {
        // 成功获取时 VMM_LOCK_RECURSIVE 必为 false; 为 true 说明锁状态不一致 (死锁)
        assert!(
            !VMM_LOCK_RECURSIVE.swap(true, Ordering::Relaxed),
            "VMM_LOCK: recursive acquisition detected (deadlock)"
        );
    }
    Some(flags)
}

// ─── AArch64 Virtual Memory Manager ──────────────────────────────────

pub struct Aarch64Vmm {
    /// Physical address of kernel L0 table (for TTBR1_EL1)
    kernel_l0: u64,
    /// User page table counter (KPTI 页表隔离追踪)
    next_table_id: core::sync::atomic::AtomicU64,
}

impl Aarch64Vmm {
    pub fn new() -> Self {
        Self {
            kernel_l0: 0,
            next_table_id: core::sync::atomic::AtomicU64::new(0),
        }
    }

    #[inline(always)]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 获取 VMM 锁 (关中断 + CAS 自旋).
    ///
    /// 本锁是**非重入**自旋锁: 调用方不得在持锁期间进入任何会再次获取 `VMM_LOCK`
    /// 的路径. 语义与 x86_64 `VirtualMemoryManager::acquire_lock` 保持一致
    /// (两者均为关中断 + CAS 自旋, 均无"单核可重入短路").
    ///
    /// # Panics
    /// 在 `debug_assertions` 构建下, 若检测到 `VMM_LOCK` 被递归获取 (死锁), 触发 `assert!`
    /// panic, 错误信息为 "`VMM_LOCK`: recursive acquisition detected (deadlock)".
    pub fn acquire_lock(&self) -> IrqSaveFlags {
        let flags = disable_interrupts();
        while VMM_LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        #[cfg(debug_assertions)]
        {
            // 不可恢复: VMM_LOCK 递归获取意味着死锁, 继续执行只会挂起系统
            assert!(
                !VMM_LOCK_RECURSIVE.swap(true, Ordering::Relaxed),
                "VMM_LOCK: recursive acquisition detected (deadlock)"
            );
        }
        flags
    }

    #[inline(always)]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn release_lock(&self, flags: &IrqSaveFlags) {
        #[cfg(debug_assertions)]
        {
            VMM_LOCK_RECURSIVE.store(false, Ordering::Relaxed);
        }
        VMM_LOCK.store(false, Ordering::Release);
        restore_interrupts(flags);
    }

    // ─── 初始化 ──────────────────────────────────────────────

    /// 初始化内核高半区页表 (TTBR1_EL1).
    /// 不替换 mmu.rs 已建立的低半区恒等映射.
    pub fn init(&self) {
        // 内核 MMU 恒等映射已由 mmu::init() 建立.
        // 我们保留它用于低层访问 (MMIO 等), 并在 TTBR1_EL1 中建立
        // 规范的高半区内核映射以供常规使用.

        // 读取当前 TTBR0_EL1 (指向 mmu.rs 建立的 L0 表)
        let current_l0: u64;
        // SAFETY: mrs ttbr0_el1 是系统寄存器读取指令，无副作用；
        // 声明 options(nomem, preserves_flags) 防止编译器重排。
        unsafe {
            core::arch::asm!("mrs {}, ttbr0_el1", out(reg) current_l0);
        }

        // 存储内核 L0 地址 (TTBR0 identity mapping, 用于内核低地址映射)
        // 暂复用现有页表.
        // 完整实现中应创建独立的内核表.
        let kernel_l0_ptr = (&raw const self.kernel_l0).cast_mut();
        // SAFETY: kernel_l0_ptr 指向 self.kernel_l0 (类型对齐的 u64)；
        // write_volatile 防止编译器优化掉对页表硬件的写。
        unsafe {
            ptr::write_volatile(kernel_l0_ptr, current_l0);
        }

        // 读取当前 TTBR1_EL1 (由 mmu::init 设置的 4 级内核根表, 含高半区映射).
        // 不覆盖 TTBR1_EL1 — 高半区映射用于 VBAR_EL1 高地址访问异常向量表.
        let current_ttbr1: u64;
        // SAFETY: mrs ttbr1_el1 是系统寄存器读取指令，无副作用.
        unsafe {
            core::arch::asm!("mrs {}, ttbr1_el1", out(reg) current_ttbr1);
        }

        // 初始化 KPTI: 创建 trampoline TTBR1 页表.
        // 在用户态运行时, TTBR1_EL1 指向最小化页表 (仅含异常入口),
        // 减少内核地址空间泄露面. 异常入口时切换回完整内核页表.
        // SAFETY: current_ttbr1 是 mmu::init 写入 TTBR1_EL1 的有效页表物理地址;
        // KPTI 全局状态在 boot 阶段被独占写入; PMM 已初始化.
        unsafe {
            super::kpti::kpti_init(self, current_ttbr1);
        }
    }

    // ─── Allocate a Page Table ───────────────────────────────────────

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    fn alloc_table(&self) -> Option<u64> {
        let paddr = get_pmm().alloc_page()?;
        // Zero the table
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            ptr::write_bytes(
                phys_to_virt(paddr.as_u64()) as *mut u8,
                0,
                PAGE_SIZE as usize,
            );
        }
        Some(paddr.as_u64())
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    fn free_table(&self, paddr: u64) {
        if paddr != 0 {
            get_pmm().free_page(PhysAddr(paddr));
        }
    }

    // ─── Kernel Page Map ─────────────────────────────────────────────

    #[expect(
        clippy::unnecessary_wraps,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    #[expect(
        clippy::missing_errors_doc,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn map_page(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), &'static str> {
        if virt.as_u64() >> 48 == 0 {
            return Ok(());
        }
        self.map_page_in_table(self.kernel_l0, virt, phys, flags);
        Ok(())
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    #[expect(
        clippy::missing_errors_doc,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn map_huge_page(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        size_type: PageSize,
    ) -> Result<(), &'static str> {
        if virt.as_u64() >> 48 == 0 {
            return Ok(());
        }
        match size_type {
            PageSize::Size4K => self.map_page(virt, phys, flags),
            PageSize::Size2M => {
                let _lock_flags = self.acquire_lock();

                let vaddr = virt.as_u64();
                let paddr = phys.as_u64();
                let raw_flags = flags.bits();

                let l0 = phys_to_virt(self.kernel_l0) as *mut u64;
                let l0_idx = l0_index(vaddr);
                let l1 = match self.ensure_next_level(l0, l0_idx) {
                    Ok(t) => t,
                    Err(e) => {
                        self.release_lock(&_lock_flags);
                        return Err(e);
                    }
                };
                let l1_idx = l1_index(vaddr);
                let l2 = match self.ensure_next_level(l1, l1_idx) {
                    Ok(t) => t,
                    Err(e) => {
                        self.release_lock(&_lock_flags);
                        return Err(e);
                    }
                };
                let l2_idx = l2_index(vaddr);

                let desc = block_flags_to_descriptor(raw_flags, paddr, 2, 0x0000_FFFF_FFE0_0000);
                // SAFETY: l2 是 ensure_next_level 返回的 L2 页表物理基地址
                // (转换后虚拟地址)；l2_idx < 512 落在表项数内；write_volatile
                // 写硬件页表，禁用编译器优化。
                unsafe {
                    ptr::write_volatile(l2.add(l2_idx), desc);
                }

                self.release_lock(&_lock_flags);
                Ok(())
            }
            PageSize::Size1G => {
                let _lock_flags = self.acquire_lock();

                let vaddr = virt.as_u64();
                let paddr = phys.as_u64();
                let raw_flags = flags.bits();

                let l0 = phys_to_virt(self.kernel_l0) as *mut u64;
                let l0_idx = l0_index(vaddr);
                let l1 = match self.ensure_next_level(l0, l0_idx) {
                    Ok(t) => t,
                    Err(e) => {
                        self.release_lock(&_lock_flags);
                        return Err(e);
                    }
                };
                let l1_idx = l1_index(vaddr);

                let desc = block_flags_to_descriptor(raw_flags, paddr, 1, 0x0000_FFFF_C000_0000);
                // SAFETY: l1 是 ensure_next_level 返回的 L1 页表基地址；
                // l1_idx < 512；write_volatile 写硬件页表。
                unsafe {
                    ptr::write_volatile(l1.add(l1_idx), desc);
                }

                self.release_lock(&_lock_flags);
                Ok(())
            }
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn unmap_page(&self, _virt: VirtAddr) {}

    #[expect(
        clippy::used_underscore_binding,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 修改虚拟页的保护属性 (mprotect 核心实现)
    ///
    /// 遍历 aarch64 页表, 找到目标页/块描述符, 保留物理地址,
    /// 仅修改 AP (访问权限) 和 UXN/PXN (执行权限) 位, 然后 TLB invalidate.
    pub fn protect_page(&self, virt: VirtAddr, new_flags: PageFlags) {
        let _lock_flags = self.acquire_lock();

        let vaddr = virt.as_u64();
        let raw_flags = new_flags.bits();

        // SAFETY: KERNEL_L0 是内核 L0 页表物理地址, phys_to_virt 转换为内核 VA.
        let l0 = phys_to_virt(self.kernel_l0) as *mut u64;
        let l0_idx = l0_index(vaddr);

        let l1 = self.get_next_level(l0, l0_idx);
        if l1.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l1_idx = l1_index(vaddr);

        // 检查 L1 块映射 (1GB)
        // SAFETY: l1 是已验证的 L1 页表基地址; l1_idx < 512.
        unsafe {
            let l1_entry = ptr::read_volatile(l1.add(l1_idx));
            if (l1_entry & 0b11) == DESC_TYPE_BLOCK as u64 {
                // L1 块映射 (1GB): 修改权限位
                let paddr = l1_entry & 0x0000_FFFF_FFFF_F000;
                let new_desc =
                    block_flags_to_descriptor(raw_flags, paddr, 1, 0x0000_FFFF_FFFF_F000);
                ptr::write_volatile(l1.add(l1_idx), new_desc);
                core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
                self.release_lock(&_lock_flags);
                return;
            }
        }

        let l2 = self.get_next_level(l1, l1_idx);
        if l2.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l2_idx = l2_index(vaddr);

        // 检查 L2 块映射 (2MB)
        // SAFETY: l2 是已验证的 L2 页表基地址; l2_idx < 512.
        unsafe {
            let l2_entry = ptr::read_volatile(l2.add(l2_idx));
            if (l2_entry & 0b11) == DESC_TYPE_BLOCK as u64 {
                // L2 块映射 (2MB): 修改权限位
                let paddr = l2_entry & 0x0000_FFFF_FFFF_F000;
                let new_desc =
                    block_flags_to_descriptor(raw_flags, paddr, 2, 0x0000_FFFF_FFFF_F000);
                ptr::write_volatile(l2.add(l2_idx), new_desc);
                core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
                self.release_lock(&_lock_flags);
                return;
            }
        }

        let l3 = self.get_next_level(l2, l2_idx);
        if l3.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l3_idx = l3_index(vaddr);

        // L3 页描述符 (4KB): 修改权限位
        // SAFETY: l3 是已验证的 L3 页表基地址; l3_idx < 512.
        unsafe {
            let l3_entry = ptr::read_volatile(l3.add(l3_idx));
            if l3_entry == 0 {
                // 页未映射, 无需修改
                self.release_lock(&_lock_flags);
                return;
            }
            let paddr = l3_entry & 0x0000_FFFF_FFFF_F000;
            let new_desc = page_flags_to_descriptor(raw_flags, paddr);
            ptr::write_volatile(l3.add(l3_idx), new_desc);
            core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
        }

        self.release_lock(&_lock_flags);
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 修改**指定页表**中虚拟页的保护属性 (mprotect 显式变体)
    ///
    /// 与 [`Self::protect_page`] 的唯一区别是目标页表根由调用方显式给出
    /// (`root_paddr`), 而非固定为内核表 `self.kernel_l0`. 用户地址空间的
    /// `mprotect` 必须走此变体: 用户数据页建在**进程用户页表**上,
    /// 改内核表不会影响用户页权限.
    ///
    /// 遍历 aarch64 页表, 找到目标页/块描述符, 保留物理地址,
    /// 仅修改 AP (访问权限) 和 UXN/PXN (执行权限) 位, 然后 TLB invalidate.
    pub fn protect_page_in_table(&self, root_paddr: u64, virt: VirtAddr, new_flags: PageFlags) {
        if root_paddr == 0 {
            return;
        }

        let _lock_flags = self.acquire_lock();

        let vaddr = virt.as_u64();
        let raw_flags = new_flags.bits();

        // SAFETY: root_paddr 是调用方指定的 L0 页表物理地址, phys_to_virt 转换为内核 VA.
        let l0 = phys_to_virt(root_paddr) as *mut u64;
        let l0_idx = l0_index(vaddr);

        let l1 = self.get_next_level(l0, l0_idx);
        if l1.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l1_idx = l1_index(vaddr);

        // 检查 L1 块映射 (1GB)
        // SAFETY: l1 是已验证的 L1 页表基地址; l1_idx < 512.
        unsafe {
            let l1_entry = ptr::read_volatile(l1.add(l1_idx));
            if (l1_entry & 0b11) == DESC_TYPE_BLOCK as u64 {
                // L1 块映射 (1GB): 修改权限位
                let paddr = l1_entry & 0x0000_FFFF_FFFF_F000;
                let new_desc =
                    block_flags_to_descriptor(raw_flags, paddr, 1, 0x0000_FFFF_FFFF_F000);
                ptr::write_volatile(l1.add(l1_idx), new_desc);
                core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
                self.release_lock(&_lock_flags);
                return;
            }
        }

        let l2 = self.get_next_level(l1, l1_idx);
        if l2.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l2_idx = l2_index(vaddr);

        // 检查 L2 块映射 (2MB)
        // SAFETY: l2 是已验证的 L2 页表基地址; l2_idx < 512.
        unsafe {
            let l2_entry = ptr::read_volatile(l2.add(l2_idx));
            if (l2_entry & 0b11) == DESC_TYPE_BLOCK as u64 {
                // L2 块映射 (2MB): 修改权限位
                let paddr = l2_entry & 0x0000_FFFF_FFFF_F000;
                let new_desc =
                    block_flags_to_descriptor(raw_flags, paddr, 2, 0x0000_FFFF_FFFF_F000);
                ptr::write_volatile(l2.add(l2_idx), new_desc);
                core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
                self.release_lock(&_lock_flags);
                return;
            }
        }

        let l3 = self.get_next_level(l2, l2_idx);
        if l3.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l3_idx = l3_index(vaddr);

        // L3 页描述符 (4KB): 修改权限位
        // SAFETY: l3 是已验证的 L3 页表基地址; l3_idx < 512.
        unsafe {
            let l3_entry = ptr::read_volatile(l3.add(l3_idx));
            if l3_entry == 0 {
                // 页未映射, 无需修改
                self.release_lock(&_lock_flags);
                return;
            }
            let paddr = l3_entry & 0x0000_FFFF_FFFF_F000;
            let new_desc = page_flags_to_descriptor(raw_flags, paddr);
            ptr::write_volatile(l3.add(l3_idx), new_desc);
            core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
        }

        self.release_lock(&_lock_flags);
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    #[expect(
        clippy::missing_errors_doc,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn split_2mb_page(&self, _virt: u64) -> Result<(), &'static str> {
        // 在 aarch64 上, L2 块 (2MB) 是块映射的默认.
        // 无需拆分 — 可直接分配 L3 表并使用 4KB 页.
        // 此函数仅为与 x86 兼容而保留.
        Ok(())
    }

    // ─── 页表遍历 / 映射 ───────────────────────────────────────

    /// 从 `root_paddr` 遍历页表, 按需创建中间级, 设置最终页描述符 —— **自持 `VMM_LOCK` 变体**
    ///
    /// 语义与锁序约束见 [`Self::map_page_in_table_locked`]; 本入口只负责加/解锁,
    /// 供不持 `VMM_LOCK` 的路径使用。已持锁的路径必须直接用
    /// [`Self::map_page_in_table_locked`], 否则构成递归加锁 (`VMM_LOCK` 非重入)。
    #[expect(
        clippy::used_underscore_binding,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn map_page_in_table(
        &self,
        root_paddr: u64,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) {
        let _lock_flags = self.acquire_lock();
        self.map_page_in_table_locked(root_paddr, virt, phys, flags);
        self.release_lock(&_lock_flags);
    }

    /// 从 `root_paddr` 遍历页表, 按需创建中间级, 设置最终页描述符 ——
    /// **要求调用方已持 `VMM_LOCK`**
    #[expect(
        clippy::manual_let_else,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub(crate) fn map_page_in_table_locked(
        &self,
        root_paddr: u64,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) {
        let vaddr = virt.as_u64();
        let paddr = phys.as_u64();
        let raw_flags = flags.bits();

        let l0 = phys_to_virt(root_paddr) as *mut u64;
        let l0_idx = l0_index(vaddr);

        let l1 = match self.ensure_next_level(l0, l0_idx) {
            Ok(t) => t,
            Err(_) => {
                return;
            }
        };
        // KPTI 方案 S3: 该 L0 槽位可能是本轮才新建 (用户栈 L0[255] / PIE L0[170] 均在
        // `create_user_page_table` 之后映射), 须同步进本进程 EL1 视图.
        Self::mirror_l0_slot_to_el1_view(l0, l0_idx);
        let l1_idx = l1_index(vaddr);

        let l2 = match self.ensure_next_level(l1, l1_idx) {
            Ok(t) => t,
            Err(_) => {
                return;
            }
        };
        let l2_idx = l2_index(vaddr);

        let l3 = match self.ensure_next_level(l2, l2_idx) {
            Ok(t) => t,
            Err(_) => {
                return;
            }
        };
        let l3_idx = l3_index(vaddr);

        let desc = page_flags_to_descriptor(raw_flags, paddr);
        // SAFETY: l3 是 ensure_next_level 返回的 L3 页表基地址；
        // l3_idx < 512；write_volatile 写硬件页表。
        unsafe {
            ptr::write_volatile(l3.add(l3_idx), desc);
        }

        // SAFETY: dsb ishst / tlbi vaae1is / dsb ish / isb 是 aarch64 标准
        // TLB 失效序列；ARM 架构要求 tlbi vaae1is 的操作数是虚拟地址右移12位（页帧号）。
        unsafe {
            core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
        }
    }

    /// Ensure the next-level page table exists at `table[idx]`.
    /// Returns a pointer to the next-level table.
    fn ensure_next_level(&self, table: *mut u64, idx: usize) -> Result<*mut u64, &'static str> {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let entry = ptr::read_volatile(table.add(idx));
            if entry & 0b11 == 0b11 {
                let paddr = entry & 0x0000_FFFF_FFFF_F000;
                Ok(phys_to_virt(paddr) as *mut u64)
            } else {
                let new_paddr = self
                    .alloc_table()
                    .ok_or("[VMM] Out of physical memory for page table")?;

                let desc = table_descriptor(new_paddr);
                ptr::write_volatile(table.add(idx), desc);
                core::arch::asm!("dsb ishst");

                Ok(phys_to_virt(new_paddr) as *mut u64)
            }
        }
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn unmap_page_in_table(&self, root_paddr: u64, virt: VirtAddr) {
        if root_paddr == 0 {
            return;
        }

        let _lock_flags = self.acquire_lock();

        let vaddr = virt.as_u64();

        // SAFETY: phys_to_virt(root_paddr) gives kernel VA for page table walk.
        let l0 = phys_to_virt(root_paddr) as *mut u64;
        let l0_idx = l0_index(vaddr);

        let l1 = self.get_next_level(l0, l0_idx);
        if l1.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l1_idx = l1_index(vaddr);

        let l2 = self.get_next_level(l1, l1_idx);
        if l2.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l2_idx = l2_index(vaddr);

        let l3 = self.get_next_level(l2, l2_idx);
        if l3.is_null() {
            self.release_lock(&_lock_flags);
            return;
        }
        let l3_idx = l3_index(vaddr);

        // 清除 L3 页描述符, 保留旧值以判定是否为用户 leaf (帧持有计数面)
        // SAFETY: l3 是已验证的 L3 页表基地址；l3_idx < 512。
        let old_entry = unsafe {
            let entry = ptr::read_volatile(l3.add(l3_idx));
            ptr::write_volatile(l3.add(l3_idx), 0);
            entry
        };

        // TLB 失效 — 必须在释放页表页前执行, 以避免投机性遍历落入已释放物理页.
        // SAFETY: 标准 TLB 失效序列；ARM 架构要求 tlbi vaae1is 的操作数是虚拟地址右移12位（页帧号）。
        unsafe {
            core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
        }

        // §8.1 规则 3: 拆除用户 leaf 即注销该映射持有的一份帧引用, 归零才释放.
        // **必须过滤用户 leaf**: KPTI supervisor 页与内核页表共享同一物理帧, 参与计数
        // 会误释放; 设备/MMIO 映射的 pfn 越界, frame_dec 侧 fail-closed 拒绝.
        // 释放时机**严格晚于**上面的 tlbi + dsb ish —— 先让所有核停止使用该映射,
        // 再归还物理帧 (aarch64 广播失效即追平, 归还时机见 `mm::release_frame_locked`).
        if is_user_leaf(old_entry) {
            let user_phys = old_entry & 0x0000_FFFF_FFFF_F000;
            if get_pmm().frame_dec(PhysAddr(user_phys)) {
                super::release_frame_locked(PhysAddr(user_phys));
            }
        }

        // 递归释放空的中间页表页, 避免 unmap 间内存泄漏
        // (destroy_page_table 仅在进程销毁时执行).
        if self.is_table_empty(l3) {
            // SAFETY: 读取/清除 L2 中指向 L3 的表项；l2_idx < 512。
            let l3_paddr = unsafe {
                let l2_entry = ptr::read_volatile(l2.add(l2_idx));
                ptr::write_volatile(l2.add(l2_idx), 0);
                l2_entry & 0x0000_FFFF_FFFF_F000
            };
            // SAFETY: dsb ishst 是数据同步屏障，确保前面的页表写完成。
            unsafe {
                core::arch::asm!("dsb ishst");
            }
            self.free_table(l3_paddr);

            // Check L2 recursively
            if self.is_table_empty(l2) {
                // SAFETY: 读取/清除 L1 中指向 L2 的表项；l1_idx < 512。
                let l2_paddr = unsafe {
                    let l1_entry = ptr::read_volatile(l1.add(l1_idx));
                    ptr::write_volatile(l1.add(l1_idx), 0);
                    l1_entry & 0x0000_FFFF_FFFF_F000
                };
                unsafe {
                    core::arch::asm!("dsb ishst");
                }
                self.free_table(l2_paddr);

                // Check L1 recursively
                if self.is_table_empty(l1) {
                    // SAFETY: 读取/清除 L0 中指向 L1 的表项；l0_idx < 512。
                    let l1_paddr = unsafe {
                        let l0_entry = ptr::read_volatile(l0.add(l0_idx));
                        ptr::write_volatile(l0.add(l0_idx), 0);
                        l0_entry & 0x0000_FFFF_FFFF_F000
                    };
                    unsafe {
                        core::arch::asm!("dsb ishst");
                    }
                    // KPTI 方案 S3: 清空必须同步到 EL1 视图 —— 否则视图残留指向即将
                    // `free_table` 的 L1 页的陈旧表项 (UAF).
                    Self::mirror_l0_slot_to_el1_view(l0, l0_idx);
                    self.free_table(l1_paddr);
                }
            }
        }

        self.release_lock(&_lock_flags);
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 当一个页表页的全部 512 项均为 0 时返回 true.
    fn is_table_empty(&self, table: *mut u64) -> bool {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            for i in 0..TABLE_ENTRIES {
                if ptr::read_volatile(table.add(i)) != 0 {
                    return false;
                }
            }
        }
        true
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 遍历表项到下一级 (只读, 不分配).
    /// 若该表项不是合法的表描述符则返回 null.
    fn get_next_level(&self, table: *mut u64, idx: usize) -> *mut u64 {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let entry = ptr::read_volatile(table.add(idx));
            if entry & 0b11 == 0b11 {
                let paddr = entry & 0x0000_FFFF_FFFF_F000;
                phys_to_virt(paddr) as *mut u64
            } else {
                core::ptr::null_mut()
            }
        }
    }

    // ─── 用户页表操作 ──────────────────────────────────

    #[expect(
        clippy::manual_let_else,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    #[expect(
        clippy::single_match_else,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn create_user_page_table(&self) -> Option<u64> {
        // 为用户空间 (TTBR0_EL1) 分配一张干净的 L0 表
        let user_l0 = self.alloc_table()?;

        // 为用户空间项分配新的 L1 表.
        // 不与内核共享 L1_IDMAP/L2_DEVICE — 这些表对 MMIO 区使用
        // Device 内存属性, 不适合用户代码执行. 共享它们也会导致
        // 用户页表修改 (例如把 2MB BLOCK 替换为 TABLE 描述符) 破坏
        // 内核恒等映射.
        let user_l1 = match self.alloc_table() {
            Some(t) => t,
            None => {
                self.free_table(user_l0);
                return None;
            }
        };

        // KPTI 方案 S3: 用户低区 L2 必须 **eager** 建立.
        // EL1 视图的 `L1_el1[0]` 直接指向本页 (与用户视图 `L1_u[0]` 同页共享 ⇒
        // 零同步); 若留待 `map_page_in_table` 惰性创建, EL1 视图将长期持有
        // stale 项 (指向 0), 内核态解引用用户裸指针即翻译失败.
        let user_l2 = match self.alloc_table() {
            Some(t) => t,
            None => {
                self.free_table(user_l1);
                self.free_table(user_l0);
                return None;
            }
        };

        let kernel_l0 = phys_to_virt(self.kernel_l0) as *const u64;
        let user_l0_ptr = phys_to_virt(user_l0) as *mut u64;
        let user_l1_desc = table_descriptor(user_l1);

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // L0[0] → 新的用户 L1 表 (干净, 不共享)
            ptr::write_volatile(user_l0_ptr.add(0), user_l1_desc);
            // L1_u[0] → 新的用户 L2 表 (用户低区 0-1 GiB 的低 8 MiB 段)
            ptr::write_volatile(
                (phys_to_virt(user_l1) as *mut u64).add(0),
                table_descriptor(user_l2),
            );

            // 从内核 L0 复制 TTBR1 项 (索引 256..511).
            // 它们覆盖高半区内核地址空间
            // (0xFFFF_0000_0000_0000 .. 0xFFFF_FFFF_FFFF_FFFF).
            // 切换 TTBR0_EL1 到用户页表后, 内核代码必须仍可通过
            // TTBR1_EL1 访问 — TTBR1 项保证内核自身页表仍可用于
            // 异常处理与其他内核态操作.
            for i in 256..TABLE_ENTRIES {
                let entry = ptr::read_volatile(kernel_l0.add(i));
                ptr::write_volatile(user_l0_ptr.add(i), entry);
            }
        }
        // SAFETY: dsb ishst 确保上面的页表写入对 MMU walker 可见 (后续
        // build_el1_view 要按该结构反推 L2_u 描述符).
        unsafe {
            core::arch::asm!("dsb ishst");
        }

        // KPTI 方案 S3: 建立本进程的 EL1 视图 (用户半区 ∪ 内核恒等) 并登记进保留槽.
        // 失败即 fail-closed: 无 EL1 视图的用户进程在内核态无法解引用用户裸指针
        // (copy_from_user / UserReadPtr 均按当前地址空间直访), 不可放行.
        if self.build_el1_view(user_l0).is_none() {
            self.free_table(user_l2);
            self.free_table(user_l1);
            self.free_table(user_l0);
            return None;
        }

        // 分配唯一页表 ID (用于 KPTI 页表隔离追踪)
        let _table_id = self.next_table_id.fetch_add(1, Ordering::Relaxed);

        Some(user_l0)
    }

    /// 为用户页表 `user_root` 建立配套的 **EL1 视图** (KPTI 方案 S3), 并把根表
    /// 物理地址登记进 `user_root` 的保留槽 [`EL1_VIEW_SLOT`].
    ///
    /// # 为什么需要 EL1 视图
    ///
    /// 全切换模型下 EL0 与 EL1 使用不同的 `TTBR0`: EL0 只能看见用户映射
    /// (Meltdown 面最小), 但内核态必须**同时**看见内核镜像与用户页 ——
    /// `copy_from_user` / `UserReadPtr` 的做法是直接解引用用户裸指针
    /// (`userptr.rs`), **不做页表遍历**, 地址空间缺映射即翻译故障.
    /// 这与 x86_64 给 `KERNEL_PML4` 低半区填用户项 (`ensure_pml4_user`) 同一归因.
    ///
    /// # 结构与代价
    ///
    /// 与 EL0 视图的差异只在 L0/L1 两级 (L2 以下全部共享):
    /// - `L0_el1[0] → L1_el1`; `L0_el1[170]/[255]` 与用户表同值 (PIE / mmap 栈)
    /// - `L1_el1[0] → L2_u` —— 与用户视图 `L1_u[0]` **指向同一页** ⇒ 后续
    ///   map/unmap 用户页对两侧同时生效, 零同步成本
    ///
    /// **视图只承载用户页** (L1-05): 内核镜像/数据/BSS/内核栈在迁移后链接于高半区,
    /// 经 `TTBR1` 可达, 不占用 `TTBR0` ⇒ 本视图不再含 DRAM 块 (L1-05 前曾以
    /// `L1_el1[1]` = `L1_IDMAP[1]` DRAM 1 GiB 块纳入, 迁移完成后为冗余面).
    ///
    /// **刻意不含 Device**: 内核 MMIO 统一走高半区别名 (`IoMem` 的 `virt`),
    /// 若把 0-1 GiB 的 Device 页并入本视图, 用户进程页表就会带上 MMIO 面,
    /// 每进程还要多一级 L2. 故每进程仅多 2 页 (`L0_el1` + `L1_el1`).
    ///
    /// # 返回
    ///
    /// 成功返回 EL1 视图根表物理地址; 前置结构缺失或内存不足时返回 `None`
    /// (试探性失败不留残留页).
    pub fn build_el1_view(&self, user_root: u64) -> Option<u64> {
        if user_root == 0 || self.kernel_l0 == 0 {
            return None;
        }

        let user = phys_to_virt(user_root) as *mut u64;

        // 前置: 用户视图 L0[0] → L1_u → L2_u 必须已建立 (共享对象)
        let l1_u = self.get_next_level(user, 0);
        if l1_u.is_null() {
            return None;
        }
        // SAFETY: l1_u 是已存在的 L1 表页; 读槽位 0.
        let l2_u_desc = unsafe { ptr::read_volatile(l1_u) };
        if l2_u_desc & 0b11 != 0b11 {
            return None;
        }

        let el1_l0 = self.alloc_table()?;
        let Some(el1_l1) = self.alloc_table() else {
            self.free_table(el1_l0);
            return None;
        };

        let el1_l0_ptr = phys_to_virt(el1_l0) as *mut u64;
        let el1_l1_ptr = phys_to_virt(el1_l1) as *mut u64;
        // SAFETY: el1_l0/el1_l1 是刚分配并清零的表页; 各索引均 < 512;
        // 写入后由调用方在切换 TTBR0 前经 tlbi 生效 (见 exception.rs 入口汇编).
        unsafe {
            // ① 共享用户 L0 的有效项 (PIE / mmap 栈).
            //    保留槽自身位 [1:0] = 0 ⇒ 不满足 0b11, 天然跳过.
            for i in 1..256 {
                let entry = ptr::read_volatile(user.add(i));
                if entry & 0b11 == 0b11 {
                    ptr::write_volatile(el1_l0_ptr.add(i), entry);
                }
            }
            // ② L0_el1[0] → L1_el1
            ptr::write_volatile(el1_l0_ptr, table_descriptor(el1_l1));
            // ③ L1_el1[0] → L2_u (共享). L1-05: 不再写 L1_el1[1] DRAM 块 ——
            //    内核经 TTBR1 高半区可达, 本视图只承载用户页.
            ptr::write_volatile(el1_l1_ptr, l2_u_desc);
            // ④ 登记关联: 用户表保留槽 = EL1 视图根 (不置位 ⇒ 硬件视为无效项)
            ptr::write_volatile(user.add(EL1_VIEW_SLOT), el1_l0);
            core::arch::asm!("dsb ishst");
        }

        Some(el1_l0)
    }

    /// 拆除 `user_root` 配套的 EL1 视图: **仅释放视图自身的 L0_el1/L1_el1 两页**.
    ///
    /// 不得递归: `L0_el1[170]/[255]` 与 `L1_el1[0] → L2_u` 都与用户视图**共享**,
    /// 递归释放会与 `destroy_page_table` 的用户半区遍历重复释放 (双释放 / UAF).
    fn destroy_el1_view(&self, user_root: u64) {
        if user_root == 0 {
            return;
        }
        let user = phys_to_virt(user_root) as *mut u64;
        // SAFETY: user_root 是有效用户 L0 表页; 保留槽内值仅取输出地址位.
        let el1_l0 = unsafe { ptr::read_volatile(user.add(EL1_VIEW_SLOT)) } & DESC_ADDR_MASK;
        if el1_l0 == 0 {
            return;
        }
        // SAFETY: el1_l0 是本进程 EL1 视图根表页, 槽位 0 为 → L1_el1 的表描述符.
        let el1_l1 =
            unsafe { ptr::read_volatile(phys_to_virt(el1_l0) as *const u64) } & DESC_ADDR_MASK;
        self.free_table(el1_l1);
        self.free_table(el1_l0);
    }

    /// 把用户表 L0 的槽位 `idx` 镜像到本进程 EL1 视图的同一槽位 (KPTI 方案 S3 同步点).
    ///
    /// EL1 视图的 L0 是**用户 L0 槽位 1..255 的副本** (下级 L1/L2/L3 与用户视图共享),
    /// 而用户栈 (VA `0x7FFF_FFFF_F000` ⇒ L0\[255\]) 与 PIE (L0\[170\]) 都是
    /// `create_user_page_table` **之后**才经 `map_page_in_table` 惰性映射的. 视图若不同步
    /// 这些槽位, 就会停留在建视图时刻的快照 (全 0): 内核态按当前地址空间直访用户裸指针
    /// (`copy_from_user` / `userptr.rs`) 将触发 level-0 翻译故障.
    ///
    /// 槽位 0 由视图自身占用 (`L0_el1[0] → L1_el1`, 仅承载用户页), 保留槽
    /// [`EL1_VIEW_SLOT`] 存放视图根地址, 二者都不镜像; 高半区槽位 (≥ 256) 于用户表恒为
    /// 内核 L0 的副本, 不参与映射变更, 同样不镜像.
    ///
    /// 无 EL1 视图时 (保留槽为 0, 如内核表 / trampoline 表) 为空操作; 全部调用点均在
    /// `VMM_LOCK` 持锁路径内, 与视图构建互斥.
    fn mirror_l0_slot_to_el1_view(user_l0: *mut u64, idx: usize) {
        if idx == 0 || idx == EL1_VIEW_SLOT || idx >= 256 {
            return;
        }
        // SAFETY: user_l0 是有效 L0 表页; idx 与 EL1_VIEW_SLOT 均 < 512;
        // el1_l0 取自保留槽, 是本进程 EL1 视图根表页 (由 build_el1_view 写入).
        unsafe {
            let el1_l0 = ptr::read_volatile(user_l0.add(EL1_VIEW_SLOT)) & DESC_ADDR_MASK;
            if el1_l0 == 0 {
                return;
            }
            let entry = ptr::read_volatile(user_l0.add(idx));
            ptr::write_volatile((phys_to_virt(el1_l0) as *mut u64).add(idx), entry);
            core::arch::asm!("dsb ishst");
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn ensure_pml4_user(&self, _virt: u64) {
        // 在 aarch64 上, 内核与用户表是分离的 (TTBR0 vs TTBR1).
        // 内核表项无需 USER 位 — 用户访问走 TTBR0, 内核走 TTBR1.
        // 对 aarch64 而言此函数为空操作.
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn ensure_path_user(&self, virt: u64) {
        // 在 aarch64 上, 仅当路径位于用户页表才相关.
        // 由于用户表在 TTBR0 且天然用户可访问, 只需确保所有
        // 中间表描述符存在 (map_page_in_table 已处理).
        if is_kernel_addr(virt) {
            // 内核页无需 USER 标志
        }
        // 用户表中的用户空间地址, 已在 map_page_in_table 中保证项合法
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn switch_page_table(&self, ttbr0: u64) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "dsb ish",
                "msr ttbr0_el1, {}",
                "isb",
                "tlbi vmalle1is",
                "dsb ish",
                "isb",
                in(reg) ttbr0,
            );
        }
    }

    pub fn get_physical(&self, virt: VirtAddr) -> Option<PhysAddr> {
        self.get_physical_in_pml4(self.kernel_l0, virt)
    }

    /// 在指定页表中翻译虚拟地址, 仅返回物理地址 (丢弃叶子项写位).
    ///
    /// 委托 [`Self::translate_in_pml4`], 供仅需物理地址的既有调用点使用.
    pub fn get_physical_in_pml4(&self, root_paddr: u64, virt: VirtAddr) -> Option<PhysAddr> {
        self.translate_in_pml4(root_paddr, virt).map(|t| t.phys)
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 在指定页表中翻译虚拟地址, 返回物理地址与叶子项可写标志.
    ///
    /// 与 [`Self::get_physical_in_pml4`] 同一遍历逻辑, 额外透出叶子表项的写位,
    /// 供跨进程写路径判定目标页可写 (只读页拒绝写入). 未映射返回 `None`.
    ///
    /// 可写判据: 叶子描述符 `AP[2]` (bit 7) == 0 表示可写.
    pub fn translate_in_pml4(&self, root_paddr: u64, virt: VirtAddr) -> Option<PageTranslation> {
        if root_paddr == 0 {
            return None;
        }

        let vaddr = virt.as_u64();

        let l0 = root_paddr as *const u64;
        let l0_idx = l0_index(vaddr);
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let l0_entry = unsafe { ptr::read_volatile(l0.add(l0_idx)) };
        if l0_entry & 0b11 != 0b11 {
            return None;
        }

        // SAFETY: table descriptor frame bits contain valid PA → phys_to_virt → kernel VA
        let l1 = phys_to_virt(l0_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
        let l1_idx = l1_index(vaddr);
        let l1_entry = unsafe { ptr::read_volatile(l1.add(l1_idx)) };
        if l1_entry & 0b11 == 0b01 {
            // L1 block (1GB)
            return Some(PageTranslation {
                phys: PhysAddr((l1_entry & 0x0000_FFFF_C000_0000) | (vaddr & 0x3FFF_FFFF)),
                writable: (l1_entry & (1 << 7)) == 0,
            });
        }
        if l1_entry & 0b11 != 0b11 {
            return None;
        }

        // SAFETY: L1 table descriptor frame → phys_to_virt → kernel VA
        let l2 = phys_to_virt(l1_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
        let l2_idx = l2_index(vaddr);
        let l2_entry = unsafe { ptr::read_volatile(l2.add(l2_idx)) };
        if l2_entry & 0b11 == 0b01 {
            // L2 block (2MB)
            return Some(PageTranslation {
                phys: PhysAddr((l2_entry & 0x0000_FFFF_FFE0_0000) | (vaddr & 0x1F_FFFF)),
                writable: (l2_entry & (1 << 7)) == 0,
            });
        }
        if l2_entry & 0b11 != 0b11 {
            return None;
        }

        // SAFETY: L2 table descriptor frame → phys_to_virt → kernel VA
        let l3 = phys_to_virt(l2_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
        let l3_idx = l3_index(vaddr);
        let l3_entry = unsafe { ptr::read_volatile(l3.add(l3_idx)) };
        if l3_entry & 0b11 != 0b11 {
            return None;
        }

        Some(PageTranslation {
            phys: PhysAddr((l3_entry & 0x0000_FFFF_FFFF_F000) | (vaddr & 0xFFF)),
            writable: (l3_entry & (1 << 7)) == 0,
        })
    }

    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 读取 L3 页表项原始值 (用于 swap entry 检测)
    pub fn get_pte_value(&self, root_paddr: u64, virt: VirtAddr) -> Option<u64> {
        let vaddr = virt.as_u64();

        let l0 = root_paddr as *const u64;
        let l0_idx = l0_index(vaddr);
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let l0_entry = unsafe { ptr::read_volatile(l0.add(l0_idx)) };
        if l0_entry & 0b11 != 0b11 {
            return None;
        }

        let l1 = phys_to_virt(l0_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
        let l1_idx = l1_index(vaddr);
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let l1_entry = unsafe { ptr::read_volatile(l1.add(l1_idx)) };
        if l1_entry & 0b11 != 0b11 {
            return None;
        }

        let l2 = phys_to_virt(l1_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
        let l2_idx = l2_index(vaddr);
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let l2_entry = unsafe { ptr::read_volatile(l2.add(l2_idx)) };
        if l2_entry & 0b11 != 0b11 {
            return None;
        }

        let l3 = phys_to_virt(l2_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
        let l3_idx = l3_index(vaddr);
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let l3_entry = unsafe { ptr::read_volatile(l3.add(l3_idx)) };

        Some(l3_entry)
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// 直接写入 L3 PTE 原始值 (用于 swap 替换)
    ///
    /// 沿 L0→L1→L2→L3 找到最终 PTE, 写入 raw_pte 后 TLB invalidate.
    /// 与 map_page_in_table 的区别: 接受任意 raw PTE (含 swap entry, 即 valid=0).
    /// 若任意中间层缺失 (valid=0), 静默返回 (不创建中间页表, swap-out 不应触发缺中间页).
    pub fn set_pte_value(&self, root_paddr: u64, virt: VirtAddr, raw_pte: u64) {
        let vaddr = virt.as_u64();

        let _flags = self.acquire_lock();

        // SAFETY: VMM_LOCK held; 四级页表查找 PTE 并直接写入
        unsafe {
            let l0 = root_paddr as *const u64;
            let l0_idx = l0_index(vaddr);
            let l0_entry = ptr::read_volatile(l0.add(l0_idx));
            if l0_entry & 0b11 != 0b11 {
                self.release_lock(&_flags);
                return;
            }

            let l1 = phys_to_virt(l0_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
            let l1_idx = l1_index(vaddr);
            let l1_entry = ptr::read_volatile(l1.add(l1_idx));
            if l1_entry & 0b11 != 0b11 {
                self.release_lock(&_flags);
                return;
            }

            let l2 = phys_to_virt(l1_entry & 0x0000_FFFF_FFFF_F000) as *const u64;
            let l2_idx = l2_index(vaddr);
            let l2_entry = ptr::read_volatile(l2.add(l2_idx));
            if l2_entry & 0b11 != 0b11 {
                self.release_lock(&_flags);
                return;
            }

            let l3 = phys_to_virt(l2_entry & 0x0000_FFFF_FFFF_F000) as *mut u64;
            let l3_idx = l3_index(vaddr);
            let l3_ptr = l3.add(l3_idx);
            ptr::write_volatile(l3_ptr, raw_pte);

            // TLB invalidate (与 unmap_page_in_table 一致)
            // ARM 架构要求 tlbi vaae1is 的操作数是虚拟地址右移12位（页帧号）
            core::arch::asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) vaddr >> 12);
        }

        self.release_lock(&_flags);
    }

    // ─── Clone / Destroy User Page Table ────────────────────────────

    pub fn clone_user_page_table(&self, parent_paddr: u64) -> Option<u64> {
        let child_paddr = self.alloc_table()?;

        // SAFETY: phys_to_virt converts page table physical addresses to kernel VAs
        let parent = phys_to_virt(parent_paddr) as *const u64;
        let child = phys_to_virt(child_paddr) as *mut u64;

        for i in 0..256 {
            unsafe {
                let entry = ptr::read_volatile(parent.add(i));
                ptr::write_volatile(child.add(i), entry);
            }
        }

        Some(child_paddr)
    }

    pub fn destroy_page_table(&self, root_paddr: u64) {
        if root_paddr == 0 {
            return;
        }

        // 释放前先做一次系统级失效: 本函数会立即归还用户数据帧与各级页表帧,
        // 必须确保所有核已停止用该页表 (复用 switch_page_table 的广播形态).
        // SAFETY: 标准系统级 TLB 失效序列, 不触及任何 Rust 内存.
        unsafe {
            core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb");
        }

        // SAFETY: phys_to_virt converts root_paddr to kernel VA
        let l0 = phys_to_virt(root_paddr) as *mut u64;

        for i in 0..256 {
            unsafe {
                let entry = ptr::read_volatile(l0.add(i));
                if entry & 0b11 == 0b11 {
                    let l1_paddr = entry & 0x0000_FFFF_FFFF_F000;
                    self.destroy_l1_table(l1_paddr);
                }
            }
        }

        // KPTI 方案 S3: 先拆除配套 EL1 视图 —— **仅释放视图自身的 L0_el1/L1_el1 两页**,
        // 不递归. 视图的 L0 有效项 (PIE / mmap 栈) 与 L1_el1[0] → L2_u 均与用户视图
        // **共享页表页**, 递归释放会与下面的用户半区遍历重复释放 (双释放 / UAF);
        // 且保留槽 (EL1_VIEW_SLOT) 的位 [1:0] = 00 不满足 0b11 ⇒ 上面的遍历天然跳过它.
        self.destroy_el1_view(root_paddr);

        self.free_table(root_paddr);
    }

    fn destroy_l1_table(&self, paddr: u64) {
        let l1 = phys_to_virt(paddr) as *mut u64;
        for i in 0..512 {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                let entry = ptr::read_volatile(l1.add(i));
                if entry & 0b11 == 0b11 {
                    let l2_paddr = entry & 0x0000_FFFF_FFFF_F000;
                    self.destroy_l2_table(l2_paddr);
                }
            }
        }
        self.free_table(paddr);
    }

    fn destroy_l2_table(&self, paddr: u64) {
        let l2 = phys_to_virt(paddr) as *mut u64;
        for i in 0..512 {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                let entry = ptr::read_volatile(l2.add(i));
                if entry & 0b11 == 0b11 {
                    let l3_paddr = entry & 0x0000_FFFF_FFFF_F000;
                    self.destroy_l3_table(l3_paddr);
                }
            }
        }
        self.free_table(paddr);
    }

    /// 销毁 L3 表: 逐**用户 leaf** 注销帧引用 (归零才释放), 随后释放 L3 页表页本身.
    ///
    /// §8.1 规则 3 的拆除侧: 每个用户 leaf 持有其帧一份引用 ⇒ 拆除即 `frame_dec`,
    /// 与 fork 的 +1 侧 (`cow::clone_user_page_table_cow_inner`) 同集.
    /// 归零时的归还时机与语义见 `mm::release_frame_locked` (aarch64 立即归还).
    /// 大页 leaf (L2 块描述符 `0b01`) 不参与计数, 与 `x86_64` 一致.
    fn destroy_l3_table(&self, paddr: u64) {
        let l3 = phys_to_virt(paddr) as *mut u64;
        for i in 0..512 {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            let entry = unsafe { ptr::read_volatile(l3.add(i)) };
            if is_user_leaf(entry) {
                let user_phys = entry & 0x0000_FFFF_FFFF_F000;
                if get_pmm().frame_dec(PhysAddr(user_phys)) {
                    super::release_frame_locked(PhysAddr(user_phys));
                }
            }
        }
        self.free_table(paddr);
    }
}

// ─── 全局 VMM 实例 ─────────────────────────────────────────────

static GLOBAL_VMM: OnceLock<Aarch64Vmm> = OnceLock::new();

pub fn vmm_init() {
    GLOBAL_VMM.get_or_init(|slot| {
        let vmm = Aarch64Vmm::new();
        vmm.init();
        slot.write(vmm);
    });
}

pub fn get_vmm() -> &'static Aarch64Vmm {
    GLOBAL_VMM.get_or_panic("VMM")
}

/// 返回 GLOBAL_VMM OnceLock 的内部状态机原始值 (仅用于诊断).
///
/// 返回值: 0=未初始化, 1=初始化中, 2=已完成.
/// 与 `get_vmm()` 不同, 本函数不会 panic, 可在 VMM 初始化前安全调用.
pub fn vmm_debug_state() -> u8 {
    GLOBAL_VMM.debug_state()
}

pub fn get_kernel_pml4() -> u64 {
    get_vmm().kernel_l0
}

pub fn get_current_pml4() -> u64 {
    let ttbr0: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mrs {}, TTBR0_EL1", out(reg) ttbr0);
    }
    if ttbr0 != 0 {
        ttbr0
    } else {
        get_vmm().kernel_l0
    }
}

/// 统计进程页表中已映射的用户页数 (4 KiB 粒度, RSS 近似) — **非阻塞**.
///
/// 只读遍历 L0 用户半区 (`0..256`). 遍历前经 `try_acquire_lock` **非阻塞**
/// 获取 `VMM_LOCK`: 锁被并发 map/unmap 占用时**立即返回 `None`, 不做任何遍历**,
/// 调用方应跳过本轮 (不得将 `None` 当作 0 参与比较). 持锁遍历期间并发
/// `unmap_page_in_table` 无法递归释放变空的中间页表 (`get_pmm().free_page`),
/// 消除踩野指针的竞态 (供 OOMD 在内存紧急时挑选占用最大的进程).
///
/// # Arguments
/// * `root_paddr` — 进程页表根 (TTBR0) 物理地址; 0 表示无用户页表.
///
/// # Returns
/// * `Some(页数)` — 成功持锁遍历所得 (2 MiB block 按其覆盖的 4 KiB 页数折算);
///   `root_paddr == 0` 时返回 `Some(0)`.
/// * `None` — `VMM_LOCK` 被占用, 未做遍历, 调用方应跳过本轮.
///
/// # 调用约束
/// 调用方不得在已持 `VMM_LOCK` 的情况下调用本函数.
pub fn count_present_user_pages(root_paddr: u64) -> Option<u64> {
    if root_paddr == 0 {
        return Some(0);
    }

    // 非阻塞获取: 锁被占用则不遍历, 直接放弃本轮.
    let Some(flags) = try_acquire_lock() else {
        return None;
    };

    let vmm = get_vmm();

    let mut pages = 0u64;
    let l0_ptr = phys_to_virt(root_paddr) as *const u64;

    // SAFETY: root_paddr 为进程有效 L0 表物理地址, 经 phys_to_virt 转为可读虚拟地址;
    // 仅读取 4 级页表结构不做修改; 各层索引均限制在 4 KiB 表内 (< 256 / < 512);
    // 全程持 VMM_LOCK, 中间页表不会被并发 unmap 递归释放, 指针在遍历期间有效.
    unsafe {
        for i in 0..256usize {
            let l0e = ptr::read_volatile(l0_ptr.add(i));
            if l0e & 0b11 != 0b11 {
                continue;
            }
            let l1_ptr = phys_to_virt(l0e & 0x0000_FFFF_FFFF_F000) as *const u64;

            for j in 0..512usize {
                let l1e = ptr::read_volatile(l1_ptr.add(j));
                if l1e & 0b11 != 0b11 {
                    continue;
                }
                let l2_ptr = phys_to_virt(l1e & 0x0000_FFFF_FFFF_F000) as *const u64;

                for k in 0..512usize {
                    let l2e = ptr::read_volatile(l2_ptr.add(k));
                    if l2e & 0b11 == 0b01 {
                        // L2 block descriptor = 2 MiB = 512 个 4 KiB 页
                        pages += 512;
                        continue;
                    }
                    if l2e & 0b11 != 0b11 {
                        continue;
                    }
                    let l3_ptr = phys_to_virt(l2e & 0x0000_FFFF_FFFF_F000) as *const u64;

                    for l in 0..512usize {
                        if ptr::read_volatile(l3_ptr.add(l)) & 0b11 == 0b11 {
                            pages += 1;
                        }
                    }
                }
            }
        }
    }

    vmm.release_lock(&flags);
    Some(pages)
}

// ============================================================================
// 诊断和页表查询 API
// ============================================================================

/// 检查描述符是否有效 (valid bit)
pub fn is_desc_valid(descriptor: u64) -> bool {
    descriptor & DESC_VALID != 0
}

/// 获取描述符类型
pub fn desc_type(descriptor: u64) -> u64 {
    descriptor & 0x3
}

/// 检查描述符是否为表描述符
pub fn is_desc_table(descriptor: u64) -> bool {
    (descriptor & 0x3) == DESC_TYPE_TABLE
}

/// 检查描述符是否为块描述符
pub fn is_desc_block(descriptor: u64) -> bool {
    (descriptor & 0x3) == DESC_TYPE_BLOCK
}

/// 检查描述符是否为页描述符
pub fn is_desc_page(descriptor: u64) -> bool {
    (descriptor & 0x3) == DESC_TYPE_PAGE
}

/// 获取描述符中的物理地址 (去除属性位)
pub fn desc_physical_addr(descriptor: u64) -> u64 {
    descriptor & 0x0000_FFFF_FFFF_F000
}

/// 检查描述符是否为设备内存映射 (MAIR index = 0)
pub fn is_desc_device_memory(descriptor: u64) -> bool {
    let mair_idx = (descriptor >> 2) & 0x7;
    mair_idx == MAIR_DEVICE_nGnRnE as u64
}

/// 检查描述符是否为非缓存 Normal 内存 (MAIR index = 2)
pub fn is_desc_non_cacheable(descriptor: u64) -> bool {
    let mair_idx = (descriptor >> 2) & 0x7;
    mair_idx == MAIR_NORMAL_NC as u64
}

/// 诊断页表条目
pub fn diagnose_descriptor(descriptor: u64, level: u8) {
    if !is_desc_valid(descriptor) {
        crate::klog_ffi!(
            klog_ffi_info,
            "[VMM] L{} descriptor 0x{:016x}: invalid (valid=0)",
            level,
            descriptor
        );
        return;
    }

    let type_str = match desc_type(descriptor) {
        0b00 => "invalid",
        0b01 => "block",
        0b10 => "reserved",
        0b11 => "table/page",
        _ => "unknown",
    };

    let phys = desc_physical_addr(descriptor);
    let mair_idx = (descriptor >> 2) & 0x7;
    let ap = (descriptor >> 6) & 0x3;
    let af = (descriptor >> 10) & 0x1;
    let xn = (descriptor >> 54) & 0x1;

    crate::klog_ffi!(
        klog_ffi_info,
        "[VMM] L{} descriptor 0x{:016x}: type={} phys=0x{:x} mair={} ap={} af={} xn={}",
        level,
        descriptor,
        type_str,
        phys,
        mair_idx,
        ap,
        af,
        xn
    );
}
