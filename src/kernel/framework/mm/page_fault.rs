//! Demand Paging — 按需分页子系统
//!
//! 与 VMA、VMM、PMM 协作实现真正的按需分页：
//!
//! ## 处理流程
//!
//! ```text
//! #PF 异常 → handle_page_fault(mm, info)
//!   ├── find_vma(addr)
//!   │   ├── Found → handle_vma_fault
//!   │   │   ├── Write+ReadOnly → COW copy
//!   │   │   └── Normal → alloc + map
//!   │   └── Not Found
//!   │       ├── Stack region → handle_stack_expansion
//!   │       └── Else → SIGSEGV
//!   └── Return PfResult
//! ```
//!
//! ## SAFETY
//!
//! - 本模块在 #PF 中断上下文中运行。所有操作必须无阻塞。
//! - PMM 分配的物理页通过 `KERNEL_BASE` 转为有效虚拟地址后清零，
//!   确保用户态不会看到脏数据（信息泄漏防护）。
//! - `PAGE_FAULT_COUNT` 使用 `AtomicU64`, 无竞争条件。

use super::page_fault_policy::current_page_fault_policy;
use super::pmm;
use super::vma::{MmStruct, Vma, VmaType};
use super::vmm;
use super::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use core::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PfResult {
    Fixed = 0,
    SignalSegv = 1,
    SignalBus = 2,
    Oom = 3,
    Unhandled = 4,
    /// userfaultfd: 缺页已登记事件并入队, 当前进程需转入阻塞等待用户态提供页
    ///
    /// 处理落点见 `idt::handlers::PageFaultHandler`: 仅标记 `Blocked` + 置
    /// `need_reschedule` 后 iretq (见 `framework::mm::uffd` 模块文档).
    UffdWait = 5,
}

#[derive(Debug, Clone, Copy)]
pub struct PageFaultInfo {
    pub fault_addr: u64,
    pub present: bool,
    pub write: bool,
    pub user: bool,
    pub reserved: bool,
    pub instruction: bool,
}

impl PageFaultInfo {
    pub fn from_error_code(fault_addr: u64, error_code: u64) -> Self {
        Self {
            fault_addr,
            present: error_code & 0x01 != 0,
            write: error_code & 0x02 != 0,
            user: error_code & 0x04 != 0,
            reserved: error_code & 0x08 != 0,
            instruction: error_code & 0x10 != 0,
        }
    }
}

// 用户栈扩展参数已策略化 (page_fault_policy trait, §6.3 拆分接口).
// #PF 中断上下文: current_page_fault_policy() 是纯决策读取, 无阻塞/分配.

/// 栈保护页结束地址 (栈底向下 guard 页边界), 缺页地址低于此值即 SIGSEGV
#[inline]
fn stack_guard_end() -> u64 {
    let p = current_page_fault_policy();
    let base = p.stack_top() - p.stack_default_size();
    base + p.stack_guard_pages() * PAGE_SIZE
}

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn handle_page_fault(mm: &MmStruct, info: PageFaultInfo) -> PfResult {
    let addr = info.fault_addr as usize;

    if info.reserved {
        return PfResult::SignalBus;
    }

    // 内核内部路径: 调用方保证 CR3 正确, 使用硬件 CR3
    let user_cr3 = vmm::get_current_pml4();

    if let Some(vma) = mm.find_vma(addr) {
        if vma.is_guard() {
            return PfResult::SignalSegv;
        }
        return handle_vma_fault_with_mm(mm, &vma, &info, user_cr3);
    }

    if info.user && is_stack_expansion_candidate(addr) {
        return handle_stack_expansion(mm, addr, user_cr3);
    }

    PfResult::SignalSegv
}

/// 用户态缺页简化入口 (无需传递 MmStruct，直接分配并映射)
///
/// 从汇编层保存的 `USER_CR3_SAVE` 读取用户页表 PML4 物理地址.
/// 汇编在 KPTI 切换前将硬件 CR3 写入此变量.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn handle_user_page_fault(info: PageFaultInfo) -> PfResult {
    let user_cr3 = super::read_user_cr3_asm();
    let addr = info.fault_addr as usize;

    if info.reserved {
        return PfResult::SignalBus;
    }

    // Swap-in: PTE 为 swap entry (present=0 但非零)
    if !info.present && info.user {
        let pml4 = user_cr3;
        let vmm_inst = vmm::get_vmm();
        if let Some(pte) = vmm_inst.get_pte_value(pml4, VirtAddr(info.fault_addr)) {
            if super::swap::is_swap_pte(pte) {
                let result = super::swap::handle_swap_fault(pml4, info.fault_addr);
                if result == PfResult::Fixed {
                    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                return result;
            }
        }
    }

    // P0-I-26 / B13-FL-01 修复: 先查 VMA, 确认地址合法性后再处理 COW.
    // 若 COW 路径先于 VMA 查找, 已 munmap 但 PTE 未刷新的地址会被错误地
    // 授予 WRITABLE 权限 (延迟 TLB 刷新场景下存在安全风险).
    let mm = super::vma::get_current_mm();
    if let Some(mm) = mm {
        if let Some(vma) = mm.find_vma(addr) {
            if vma.is_guard() {
                return PfResult::SignalSegv;
            }
            return handle_vma_fault_with_mm(mm, &vma, &info, user_cr3);
        }
    }

    // COW: 写已存在但只读的页 (VMA 未覆盖的 fallback 路径,
    // 例如 fork 后子进程写 COW 页但 VMA 尚未同步)
    if info.write && info.present {
        let pml4 = user_cr3;
        return match super::cow::cow_handle_fault(pml4, info.fault_addr) {
            Some(_) => {
                PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
                PfResult::Fixed
            }
            None => PfResult::SignalSegv,
        };
    }

    if info.user && is_stack_expansion_candidate(addr) {
        return handle_stack_expansion_simple(addr, user_cr3);
    }

    PfResult::SignalSegv
}

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn handle_stack_expansion_simple(addr: usize, user_cr3: u64) -> PfResult {
    let page_aligned = addr & !(PAGE_SIZE as usize - 1);

    if (page_aligned as u64) < stack_guard_end() {
        return PfResult::SignalSegv;
    }

    let pmm_inst = pmm::get_pmm();
    let phys = match pmm_inst.alloc_page() {
        Some(p) => p,
        None => return PfResult::Oom,
    };

    let phys_virt = phys.to_virt();
    // SAFETY: PMM 分配的有效页
    unsafe {
        core::ptr::write_bytes(phys_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
    }

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    let vmm_inst = vmm::get_vmm();
    vmm_inst.map_page_in_table(user_cr3, VirtAddr(page_aligned as u64), phys, flags);

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn handle_vma_fault_with_mm(
    mm: &MmStruct,
    vma: &Vma,
    info: &PageFaultInfo,
    user_cr3: u64,
) -> PfResult {
    let aligned = (info.fault_addr as usize) & !(PAGE_SIZE as usize - 1);

    // ── userfaultfd (T1 G4): 已注册区间的匿名页缺页交由用户态处理 ──
    // 仅拦截 not-present 的用户态缺页 (MISSING 模式); COW/文件映射不受影响.
    if info.user && !info.present {
        let uffd_flags = if info.write {
            super::uffd::UFFD_PAGEFAULT_FLAG_WRITE
        } else {
            0
        };
        match super::uffd::fault_notify(aligned as u64, uffd_flags) {
            super::uffd::UffdFaultOutcome::NotRegistered => {}
            super::uffd::UffdFaultOutcome::Waiting => return PfResult::UffdWait,
            super::uffd::UffdFaultOutcome::Ready => {
                return handle_uffd_provided(vma, aligned, user_cr3);
            }
        }
    }

    // ── FileBacked VMA: 从 Page Cache 获取缓存页 ──
    if vma.vma_type == VmaType::FileBacked && vma.inode_id != 0 {
        return handle_file_fault(mm, vma, info, aligned, user_cr3);
    }

    // ── COW: 写入只读页 ──
    if info.write && !vma.flags.contains(PageFlags::WRITABLE) {
        return do_cow_copy_with_mm(mm, vma, aligned, user_cr3);
    }

    // ── 普通匿名页分配 ──
    let pmm_inst = pmm::get_pmm();
    let phys = match pmm_inst.alloc_page() {
        Some(p) => p,
        None => return PfResult::Oom,
    };

    let phys_virt = phys.to_virt();
    // SAFETY: PMM 分配的有效页, 清零防信息泄漏
    unsafe {
        core::ptr::write_bytes(phys_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
    }

    let flags = vma.flags | PageFlags::PRESENT;
    let vmm_inst = vmm::get_vmm();
    let pml4 = user_cr3;

    vmm_inst.map_page_in_table(pml4, VirtAddr(aligned as u64), phys, flags);

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

/// userfaultfd: 用户态已提供数据的页 → 分配物理页 + 填充 + 按 VMA 权限映射
///
/// 由 `handle_vma_fault_with_mm` 在 `UffdFaultOutcome::Ready` 时调用.
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn handle_uffd_provided(vma: &Vma, aligned: usize, user_cr3: u64) -> PfResult {
    let pmm_inst = pmm::get_pmm();
    let phys = match pmm_inst.alloc_page() {
        Some(p) => p,
        None => return PfResult::Oom,
    };

    if !super::uffd::fill_provided_page(aligned as u64, phys) {
        // 状态在分配与填充之间被改变 (如 UFFDIO_WAKE): 释放物理页并退回等待
        pmm_inst.free_page(phys);
        return PfResult::UffdWait;
    }

    let flags = vma.flags | PageFlags::PRESENT;
    vmm::get_vmm().map_page_in_table(user_cr3, VirtAddr(aligned as u64), phys, flags);

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

/// 文件映射缺页处理: 从 Page Cache 获取/创建缓存页
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn handle_file_fault(
    _mm: &MmStruct,
    vma: &Vma,
    info: &PageFaultInfo,
    aligned: usize,
    user_cr3: u64,
) -> PfResult {
    let page_index = ((aligned - vma.start) as u64 + vma.offset) / PAGE_SIZE;

    // 建立侧唯一入口 (§8.1 规则 2): 登记「本 VA 映射持有该文件缓存页」.
    // 入口内含幂等判据 (该 VA 已持有 ⇒ 不重复登记) 与被替换旧帧的注销,
    // 与拆除侧 `pcache_release_for_va` 共用同一判据实现.
    let (cache_phys, newly_inserted) = match super::pcache::pcache_acquire_for_va(
        user_cr3,
        aligned as u64,
        vma.inode_id,
        page_index,
    ) {
        Some(v) => v,
        None => return PfResult::Oom,
    };

    // Demand Paging (B2 第三步真语义): 新插入的条目内容仍是零页 ⇒ 同步从 vfs 读
    // 4KB 填 pcache. 与传统 demand paging 一致: 用户访问哪页才读哪页, 不预先读全部.
    if newly_inserted {
        // pwm 取自 Vma 记录的创建者凭证, 保证权限校验正确
        // P3-I-19: 传入 mount_idx (vma.mount_idx, mmap 时记录的挂载点)
        // 让 vfs_pread_inode 能走 FileSystem trait 分发而非硬编码 RamFS.
        let file_off = vma.offset + (aligned - vma.start) as u64;
        let mut page_buf = [0u8; PAGE_SIZE as usize];
        let n = crate::framework::fs::current_vfs_ops().pread_inode(
            vma.mount_idx,
            vma.inode_id,
            file_off,
            &mut page_buf,
            vma.file_pwm,
        );
        if n > 0 {
            super::pcache::pcache_fill(vma.inode_id, page_index, &page_buf[..n as usize]);
        }
        // n <= 0 (EOF / 文件短): 保持插入时的零页 (POSIX: mmap 文件尾零填充)
    }

    let vmm_inst = vmm::get_vmm();
    let pml4 = user_cr3;

    if vma.shared {
        // MAP_SHARED: 可写映射, 写入回写 Page Cache
        let flags = vma.flags | PageFlags::PRESENT | PageFlags::WRITABLE;
        vmm_inst.map_page_in_table(pml4, VirtAddr(aligned as u64), PhysAddr(cache_phys), flags);

        // 写入时标记脏页
        if info.write {
            super::pcache::pcache_mark_dirty(vma.inode_id, page_index);
        }
    } else {
        // MAP_PRIVATE: 只读映射, 写入时触发 COW
        let flags = (vma.flags | PageFlags::PRESENT) & !PageFlags::WRITABLE;
        vmm_inst.map_page_in_table(pml4, VirtAddr(aligned as u64), PhysAddr(cache_phys), flags);

        // 写入时 COW: 分配新页, 复制数据, 可写映射
        if info.write {
            let pmm_inst = pmm::get_pmm();
            let new_phys = match pmm_inst.alloc_page() {
                Some(p) => p,
                None => return PfResult::Oom,
            };

            // 从缓存页复制数据到新页
            let src_virt = PhysAddr(cache_phys).to_virt();
            let dst_virt = new_phys.to_virt();
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src_virt.0 as *const u8,
                    dst_virt.0 as *mut u8,
                    PAGE_SIZE as usize,
                );
            }

            // 注销本 VA 对 Page Cache 的持有: 上面已确认该 VA 正持有此缓存页
            // (acquire 的登记/幂等判据), 故此处直接 pcache_put, 无需再判 PTE.
            super::pcache::pcache_put(vma.inode_id, page_index);

            // 用新页替换映射 (可写)
            let cow_flags = vma.flags | PageFlags::PRESENT | PageFlags::WRITABLE;
            vmm_inst.map_page_in_table(pml4, VirtAddr(aligned as u64), new_phys, cow_flags);

            // §8.1 规则 3: 该 VA 的旧映射被替换 ⇒ 注销它持有的那份缓存帧引用.
            // (pcache 条目自身那份持有在 ref_count 归零时由 deref 释放, 已含在上面
            // 的 pcache_put 内). 归零才归还, 归还时机与锁序见 `mm::release_frame`.
            if pmm_inst.frame_dec(PhysAddr(cache_phys)) {
                super::release_frame(PhysAddr(cache_phys));
            }
        }
    }

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

fn is_stack_expansion_candidate(addr: usize) -> bool {
    let p = current_page_fault_policy();
    let a = addr as u64;
    (p.stack_top() - p.stack_default_size()..p.stack_top()).contains(&a)
}

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn handle_stack_expansion(mm: &MmStruct, addr: usize, user_cr3: u64) -> PfResult {
    let page_aligned = addr & !(PAGE_SIZE as usize - 1);

    if (page_aligned as u64) < stack_guard_end() {
        return PfResult::SignalSegv;
    }

    if mm.find_vma(page_aligned).is_some() {
        return PfResult::SignalSegv;
    }

    let pmm_inst = pmm::get_pmm();
    let phys = match pmm_inst.alloc_page() {
        Some(p) => p,
        None => return PfResult::Oom,
    };

    let phys_virt = phys.to_virt();
    // SAFETY: PMM 分配的有效页, 清零防信息泄漏
    unsafe {
        core::ptr::write_bytes(phys_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
    }

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    let vmm_inst = vmm::get_vmm();
    vmm_inst.map_page_in_table(user_cr3, VirtAddr(page_aligned as u64), phys, flags);

    let stack_vma = Vma::new(
        page_aligned,
        page_aligned + PAGE_SIZE as usize,
        flags,
        VmaType::Stack,
    );
    if mm.insert_vma(stack_vma).is_err() {
        // insert_vma 失败: unmap 刚映射的页面, 防止无 VMA 跟踪的孤儿映射
        // (后续 munmap/mprotect 无法覆盖此区域).
        // 帧的归还由 unmap 统一承担 (§8.1 规则 3: 每 USER leaf 拆除即 frame_dec,
        // 归零才延迟释放) —— 此处不得再显式 free_page, 否则同一帧被注销两次.
        vmm_inst.unmap_page_in_table(user_cr3, VirtAddr(page_aligned as u64));
        return PfResult::Oom;
    }

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

// ── COW (Copy-on-Write) ──

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn do_cow_copy_with_mm(_mm: &MmStruct, _vma: &Vma, addr: usize, user_cr3: u64) -> PfResult {
    let vmm_inst = vmm::get_vmm();
    let pml4 = user_cr3;

    let old_phys = match vmm_inst.get_physical_in_pml4(pml4, VirtAddr(addr as u64)) {
        Some(p) => p,
        None => return PfResult::SignalSegv,
    };

    let pmm_inst = pmm::get_pmm();
    let new_phys = match pmm_inst.alloc_page() {
        Some(p) => p,
        None => return PfResult::Oom,
    };

    let old_virt = old_phys.to_virt();
    let new_virt = new_phys.to_virt();
    // SAFETY: old_phys/new_phys 均为有效物理页, KERNEL_BASE 映射可用;
    // 两个 4KB 区域不重叠 (PMM 保证)
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_virt.0 as *const u8,
            new_virt.0 as *mut u8,
            PAGE_SIZE as usize,
        );
    }

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    vmm_inst.map_page_in_table(pml4, VirtAddr(addr as u64), new_phys, flags);

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

// ── 统计 ──

pub static PAGE_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn page_fault_count() -> u64 {
    PAGE_FAULT_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    // stack_top/stack_default_size 是 PageFaultPolicy 的 trait 方法, 需显式引入 trait.
    use crate::framework::mm::page_fault_policy::PageFaultPolicy;

    #[test]
    fn test_pf_info_from_error_code() {
        let info = PageFaultInfo::from_error_code(0x4000, 0x06);
        assert_eq!(info.fault_addr, 0x4000);
        assert!(info.write);
        assert!(info.user);
        assert!(!info.present);
        assert!(!info.reserved);
        assert!(!info.instruction);
    }

    #[test]
    fn test_pf_info_not_present() {
        let info = PageFaultInfo::from_error_code(0x1000, 0x00);
        assert!(!info.present);
        assert!(!info.write);
        assert!(!info.user);
    }

    #[test]
    fn test_stack_expansion_candidate() {
        // 回退策略与历史硬编码值一致: 栈顶 = USER_ADDR_MAX, 默认 8MB, guard 1 页
        let p = crate::framework::mm::page_fault_policy::FallbackPageFaultPolicy;
        let inside = (p.stack_top() - PAGE_SIZE) as usize;
        assert!(is_stack_expansion_candidate(inside));

        let outside = (p.stack_top() - p.stack_default_size() - PAGE_SIZE) as usize;
        assert!(!is_stack_expansion_candidate(outside));

        assert!(!is_stack_expansion_candidate(0x1000));
    }

    #[test]
    fn test_pf_result_values() {
        assert_eq!(PfResult::Fixed as u8, 0);
        assert_eq!(PfResult::SignalSegv as u8, 1);
        assert_eq!(PfResult::Oom as u8, 3);
    }
}
