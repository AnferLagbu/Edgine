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
//! 另有一条**只由 aarch64 调用**的窄支路 [`break_cow_for_kernel_uaccess`]: ARM 的页写保护
//! 位对 EL1 同样生效, 故 fork 的 COW 写保护会让内核写用户页的 store 一起故障, 该支路负责
//! 把这一种 (且仅这一种) 核态故障破 COW 后重试。
//!
//! ## SAFETY
//!
//! - 本模块在 #PF 中断上下文中运行。所有操作必须无阻塞。
//! - PMM 分配的物理页通过 `KERNEL_BASE` 转为有效虚拟地址后清零，
//!   确保用户态不会看到脏数据（信息泄漏防护）。
//! - `PAGE_FAULT_COUNT` 使用 `AtomicU64`, 无竞争条件。

#![deny(missing_docs)]

use super::page_fault_policy::current_page_fault_policy;
use super::pmm;
use super::vma::{MmStruct, Vma, VmaType};
use super::vmm;
use super::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use core::sync::atomic::{AtomicU64, Ordering};

/// 缺页处理结果: 指示 #PF 处理器应采取的处置动作.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PfResult {
    /// 缺页已修复，处理器可直接返回用户态。
    Fixed = 0,
    /// 需向进程投递 `SIGSEGV`。
    SignalSegv = 1,
    /// 需向进程投递 `SIGBUS`。
    SignalBus = 2,
    /// 物理内存不足，缺页无法修复。
    Oom = 3,
    /// 未识别的缺页，交由上层兜底处理。
    Unhandled = 4,
    /// userfaultfd: 缺页已登记事件并入队, 当前进程需转入阻塞等待用户态提供页
    ///
    /// 处理落点见 `idt::handlers::PageFaultHandler`: 仅标记 `Blocked` + 置
    /// `need_reschedule` 后 iretq (见 `privileged::mm::uffd` 模块文档).
    UffdWait = 5,
}

/// 缺页错误码中的访问属性 (W/R、U/S、I/D 位)
#[derive(Debug, Clone, Copy)]
pub struct AccessFlags {
    /// 错误码 W/R 位：是否为写访问。
    pub write: bool,
    /// 错误码 U/S 位：是否来自用户态。
    pub user: bool,
    /// 错误码 I/D 位：是否为取指访问。
    pub instruction: bool,
}

/// 缺页错误码中的页状态 (P、RSVD 位)
#[derive(Debug, Clone, Copy)]
pub struct FaultPageStatus {
    /// 错误码 P 位：页是否已存在。
    pub present: bool,
    /// 错误码 RSVD 位：是否使用保留位。
    pub reserved: bool,
}

/// 缺页错误信息: 故障地址与错误码解码后的标志位.
#[derive(Debug, Clone, Copy)]
pub struct PageFaultInfo {
    /// 触发缺页的虚拟地址（CR2）。
    pub fault_addr: u64,
    /// 访问属性标志 (写 / 用户态 / 取指)。
    pub access: AccessFlags,
    /// 页状态标志 (已存在 / 保留位)。
    pub page: FaultPageStatus,
}

impl PageFaultInfo {
    /// 由 x86_64 缺页错误码解码出各标志位。
    #[must_use]
    pub fn from_error_code(fault_addr: u64, error_code: u64) -> Self {
        Self {
            fault_addr,
            access: AccessFlags {
                write: error_code & 0x02 != 0,
                user: error_code & 0x04 != 0,
                instruction: error_code & 0x10 != 0,
            },
            page: FaultPageStatus {
                present: error_code & 0x01 != 0,
                reserved: error_code & 0x08 != 0,
            },
        }
    }

    /// 由 aarch64 `ESR_EL1` + `FAR_EL1` 解码, 产出与 x86 `from_error_code` **同一结构**.
    ///
    /// 位段: `EC = ESR[31:26]` 定异常来源异常级与取指/数据; `WnR = ISS[6]` 定读写;
    /// `ISS[5:0]` 的高 4 位经 [`A64FaultClass::from_esr`] 归类后决定 `page.present`.
    ///
    /// `page.reserved` 恒 `false`: aarch64 stage-1 翻译没有"访问了保留位"这一独立故障类
    /// (描述符里的非法位组合由硬件按翻译/权限故障上报), 故 `PfResult::SignalBus` 分支在
    /// 本架构上不可达.
    #[must_use]
    pub fn from_esr(esr: u64, fault_addr: u64) -> Self {
        let ec = esr >> 26;
        let class = A64FaultClass::from_esr(esr);
        Self {
            fault_addr,
            access: AccessFlags {
                write: esr & (1 << A64_WNR_BIT) != 0,
                user: matches!(ec, EC_INSTR_ABORT_LOWER_EL | EC_DATA_ABORT_LOWER_EL),
                instruction: matches!(ec, EC_INSTR_ABORT_SAME_EL | EC_INSTR_ABORT_LOWER_EL),
            },
            page: FaultPageStatus {
                // 只有翻译故障意味着"描述符不存在"; 权限/AF 故障的页在表里, 外部/对齐
                // 一类不参与恢复 (由调用方按 `A64FaultClass::is_demand_pageable` 拦下).
                present: !matches!(class, A64FaultClass::Translation),
                reserved: false,
            },
        }
    }
}

/// aarch64 `ESR_EL1` 故障类 (按数据异常 `DFSC` 归类).
///
/// 供 `arch::aarch64::exception::sync_exception_handler` 判定"本次异常是否落在可按
/// 缺页恢复的类别"里: 只有 [`Self::Translation`] (未映射) 与 [`Self::Permission`]
/// (写保护) 进入恢复路径. 故障**层级** (L0..L3, `ISS[1:0]`) 不参与判定, 故不入枚举.
///
/// 归类只在 `EC` 为**数据异常** (`0x24` 低 EL / `0x25` 同 EL) 时给出非
/// [`Self::Other`] 的类别: 取指异常 (`0x20`/`0x21`) 的 IFSC 在"翻译 (`0b0001LL`)"与
/// "AF (`0b0010LL`)"两段与 DFSC **同形**, 须先按 `EC` 排除, 见 [`Self::from_esr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A64FaultClass {
    /// 翻译故障 (`DFSC[5:2] == 0b0001`): 路径上缺描述符 ⇒ `page.present = false`,
    /// 对应匿名页首次访问 (按需分页) 与用户栈增长.
    Translation,
    /// 访问标志故障 (`0b0010`): 描述符存在但 AF 未置. 本内核建映射恒置 AF,
    /// 出现即属非预期形态, 不恢复.
    AccessFlag,
    /// 权限故障 (`0b0011`): 描述符存在而访问权限不符 ⇒ `page.present = true`,
    /// 对应 COW 写保护页被写.
    Permission,
    /// 其余故障类 (地址大小 / 外部同步 / 对齐 / TLB 冲突 / 取指权限 / 实现定义):
    /// 不属缺页射程.
    Other,
}

impl A64FaultClass {
    /// 由 `ESR_EL1` 归类: 先看 `EC` 是否**数据异常**, 再看 `ISS[5:0]` (DFSC) 高 4 位.
    ///
    /// `EC` 前置判不可省: 取指异常的 IFSC 与数据异常的 DFSC 在"翻译 (`0b0001LL`)"与
    /// "AF (`0b0010LL`)"两段**同形** —— 只看 ISS 会把取指故障误判成可恢复, 而本内核不为
    /// 取指故障建页 (用户 PC 落在未映射区必须按原有语义终止).
    #[must_use]
    pub fn from_esr(esr: u64) -> Self {
        let ec = esr >> 26;
        if !matches!(ec, EC_DATA_ABORT_LOWER_EL | EC_DATA_ABORT_SAME_EL) {
            return Self::Other;
        }
        match (esr & 0x3F) >> 2 {
            0b0001 => Self::Translation,
            0b0010 => Self::AccessFlag,
            0b0011 => Self::Permission,
            _ => Self::Other,
        }
    }

    /// 是否落在缺页恢复射程内 (翻译 / 权限故障).
    #[must_use]
    pub fn is_demand_pageable(self) -> bool {
        matches!(self, Self::Translation | Self::Permission)
    }
}

/// `ISS[6]` (WnR): 置位 = 写访问.
const A64_WNR_BIT: u64 = 6;
// `EC` 段的 "同 EL / 低 EL" 分配是**反直觉**的一侧: ARMv8 把 `0b1000xx` / `0b1001xx`
// 的**偶数**编码给"来自低异常级", **奇数**编码给"异常级不变" —— 与位序直觉相反.
// 实机读数钉住口径 (aarch64 QEMU, init 的 KPTI 高半区探针与核态 uaccess 写各一条):
// - EL0 取内核高半区 → `ESR=0x92000006` (`EC=0x24`, ELR 是**用户** PC, SPSR.M=EL0t);
// - 内核写用户 COW 页 → `ESR=0x9600004F` (`EC=0x25`, ELR 是**内核** .text, SPSR.M=EL1h).
// 两组读数的 ELR/SPSR 与这里的命名互相印证, 写反即 `user` 位与恢复射程整体倒置.
/// `EC` 0x20: 低异常级 (EL0) 取指异常.
const EC_INSTR_ABORT_LOWER_EL: u64 = 0x20;
/// `EC` 0x21: 同异常级取指异常.
const EC_INSTR_ABORT_SAME_EL: u64 = 0x21;
/// `EC` 0x24: 低异常级 (EL0) 数据异常.
const EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
/// `EC` 0x25: 同异常级 (内核态) 数据异常 —— 核态 uaccess 写命中 COW 只读即此编码.
const EC_DATA_ABORT_SAME_EL: u64 = 0x25;

// 用户栈扩展参数已策略化 (page_fault_policy trait, §6.3 拆分接口).
// #PF 中断上下文: current_page_fault_policy() 是纯决策读取, 无阻塞/分配.

/// 栈保护页结束地址 (栈底向下 guard 页边界), 缺页地址低于此值即 SIGSEGV
#[inline]
fn stack_guard_end() -> u64 {
    let p = current_page_fault_policy();
    let base = p.stack_top() - p.stack_default_size();
    base + p.stack_guard_pages() * PAGE_SIZE
}

/// 基于给定地址空间 (`MmStruct`) 处理一次缺页, 返回处置结果.
pub fn handle_page_fault(mm: &MmStruct, info: PageFaultInfo) -> PfResult {
    let addr = crate::privileged::glue::usize_from_u64(info.fault_addr);

    if info.page.reserved {
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

    if info.access.user && is_stack_expansion_candidate(addr) {
        return handle_stack_expansion(mm, addr, user_cr3);
    }

    PfResult::SignalSegv
}

/// 用户态缺页简化入口 (无需传递 MmStruct，直接分配并映射)
///
/// 从汇编层保存的 `USER_CR3_SAVE` 读取用户页表 PML4 物理地址.
/// 汇编在 KPTI 切换前将硬件 CR3 写入此变量.
pub fn handle_user_page_fault(info: PageFaultInfo) -> PfResult {
    let user_cr3 = super::read_user_cr3_asm();
    let addr = crate::privileged::glue::usize_from_u64(info.fault_addr);

    if info.page.reserved {
        return PfResult::SignalBus;
    }

    // Swap-in: PTE 为 swap entry (present=0 但非零)
    if !info.page.present && info.access.user {
        let pml4 = user_cr3;
        if let Some(pte) = super::Vmm::get_pte_value(pml4, VirtAddr(info.fault_addr))
            && super::swap::is_swap_pte(pte)
        {
            let result = super::swap::handle_swap_fault(pml4, info.fault_addr);
            if result == PfResult::Fixed {
                PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
            }
            return result;
        }
    }

    // P0-I-26 / B13-FL-01 修复: 先查 VMA, 确认地址合法性后再处理 COW.
    // 若 COW 路径先于 VMA 查找, 已 munmap 但 PTE 未刷新的地址会被错误地
    // 授予 WRITABLE 权限 (延迟 TLB 刷新场景下存在安全风险).
    let mm = super::vma::get_current_mm();
    if let Some(mm) = mm
        && let Some(vma) = mm.find_vma(addr)
    {
        if vma.is_guard() {
            return PfResult::SignalSegv;
        }
        return handle_vma_fault_with_mm(mm, &vma, &info, user_cr3);
    }

    // COW: 写已存在但只读的页 (VMA 未覆盖的 fallback 路径,
    // 例如 fork 后子进程写 COW 页但 VMA 尚未同步)
    if info.access.write && info.page.present {
        let pml4 = user_cr3;
        return match super::cow::cow_handle_fault(pml4, info.fault_addr) {
            Some(_) => {
                PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
                PfResult::Fixed
            }
            None => PfResult::SignalSegv,
        };
    }

    if info.access.user && is_stack_expansion_candidate(addr) {
        return handle_stack_expansion_simple(addr, user_cr3);
    }

    PfResult::SignalSegv
}

fn handle_stack_expansion_simple(addr: usize, user_cr3: u64) -> PfResult {
    let page_aligned = addr & !(crate::privileged::glue::usize_from_u64(PAGE_SIZE) - 1);

    if (page_aligned as u64) < stack_guard_end() {
        return PfResult::SignalSegv;
    }

    let pmm_inst = pmm::get_pmm();
    let Some(phys) = pmm_inst.alloc_page() else {
        return PfResult::Oom;
    };

    let phys_virt = phys.to_virt();
    // SAFETY: PMM 分配的有效页
    unsafe {
        core::ptr::write_bytes(
            phys_virt.0 as *mut u8,
            0,
            crate::privileged::glue::usize_from_u64(PAGE_SIZE),
        );
    }

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    crate::privileged::mm::Vmm::map_page_in_table(
        user_cr3,
        VirtAddr(page_aligned as u64),
        phys,
        flags,
    );

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

fn handle_vma_fault_with_mm(
    mm: &MmStruct,
    vma: &Vma,
    info: &PageFaultInfo,
    user_cr3: u64,
) -> PfResult {
    let aligned = crate::privileged::glue::usize_from_u64(info.fault_addr)
        & !(crate::privileged::glue::usize_from_u64(PAGE_SIZE) - 1);

    // ── userfaultfd (T1 G4): 已注册区间的匿名页缺页交由用户态处理 ──
    // 仅拦截 not-present 的用户态缺页 (MISSING 模式); COW/文件映射不受影响.
    if info.access.user && !info.page.present {
        let uffd_flags = if info.access.write {
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
    if vma.kind == VmaType::FileBacked && vma.inode_id != 0 {
        return handle_file_fault(mm, vma, info, aligned, user_cr3);
    }

    // ── COW: 写入只读页 ──
    if info.access.write && !vma.flags.contains(PageFlags::WRITABLE) {
        return do_cow_copy_with_mm(mm, vma, aligned, user_cr3);
    }

    // ── 普通匿名页分配 ──
    let pmm_inst = pmm::get_pmm();
    let Some(phys) = pmm_inst.alloc_page() else {
        return PfResult::Oom;
    };

    let phys_virt = phys.to_virt();
    // SAFETY: PMM 分配的有效页, 清零防信息泄漏
    unsafe {
        core::ptr::write_bytes(
            phys_virt.0 as *mut u8,
            0,
            crate::privileged::glue::usize_from_u64(PAGE_SIZE),
        );
    }

    let flags = vma.flags | PageFlags::PRESENT;
    let pml4 = user_cr3;

    crate::privileged::mm::Vmm::map_page_in_table(pml4, VirtAddr(aligned as u64), phys, flags);

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

/// userfaultfd: 用户态已提供数据的页 → 分配物理页 + 填充 + 按 VMA 权限映射
///
/// 由 `handle_vma_fault_with_mm` 在 `UffdFaultOutcome::Ready` 时调用.
fn handle_uffd_provided(vma: &Vma, aligned: usize, user_cr3: u64) -> PfResult {
    let pmm_inst = pmm::get_pmm();
    let Some(phys) = pmm_inst.alloc_page() else {
        return PfResult::Oom;
    };

    if !super::uffd::fill_provided_page(aligned as u64, phys) {
        // 状态在分配与填充之间被改变 (如 UFFDIO_WAKE): 释放物理页并退回等待
        pmm_inst.free_page(phys);
        return PfResult::UffdWait;
    }

    let flags = vma.flags | PageFlags::PRESENT;
    crate::privileged::mm::Vmm::map_page_in_table(user_cr3, VirtAddr(aligned as u64), phys, flags);

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

/// 文件映射缺页处理: 从 Page Cache 获取/创建缓存页
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
    let Some((cache_phys, newly_inserted)) =
        super::pcache::pcache_acquire_for_va(user_cr3, aligned as u64, vma.inode_id, page_index)
    else {
        return PfResult::Oom;
    };

    // Demand Paging (B2 第三步真语义): 新插入的条目内容仍是零页 ⇒ 同步从 vfs 读
    // 4KB 填 pcache. 与传统 demand paging 一致: 用户访问哪页才读哪页, 不预先读全部.
    if newly_inserted {
        // pwm 取自 Vma 记录的创建者凭证, 保证权限校验正确
        // P3-I-19: 传入 mount_idx (vma.mount_idx, mmap 时记录的挂载点)
        // 让 vfs_pread_inode 能走 FileSystem trait 分发而非硬编码 RamFS.
        let file_off = vma.offset + (aligned - vma.start) as u64;
        let mut page_buf = [0u8; crate::privileged::glue::usize_from_u64(PAGE_SIZE)];
        let n = crate::privileged::fs::current_vfs_ops().pread_inode(
            vma.mount_idx,
            vma.inode_id,
            file_off,
            &mut page_buf,
            vma.file_pwm,
        );
        if n > 0 {
            super::pcache::pcache_fill(
                vma.inode_id,
                page_index,
                &page_buf[..crate::privileged::glue::usize_from_i32(n)],
            );
        }
        // n <= 0 (EOF / 文件短): 保持插入时的零页 (POSIX: mmap 文件尾零填充)
    }

    let pml4 = user_cr3;

    if vma.shared {
        // MAP_SHARED: 可写映射, 写入回写 Page Cache
        let flags = vma.flags | PageFlags::PRESENT | PageFlags::WRITABLE;
        crate::privileged::mm::Vmm::map_page_in_table(
            pml4,
            VirtAddr(aligned as u64),
            PhysAddr(cache_phys),
            flags,
        );

        // 写入时标记脏页
        if info.access.write {
            super::pcache::pcache_mark_dirty(vma.inode_id, page_index);
        }
    } else {
        // MAP_PRIVATE: 只读映射, 写入时触发 COW
        let flags = (vma.flags | PageFlags::PRESENT) & !PageFlags::WRITABLE;
        crate::privileged::mm::Vmm::map_page_in_table(
            pml4,
            VirtAddr(aligned as u64),
            PhysAddr(cache_phys),
            flags,
        );

        // 写入时 COW: 分配新页, 复制数据, 可写映射
        if info.access.write {
            let pmm_inst = pmm::get_pmm();
            let Some(new_phys) = pmm_inst.alloc_page() else {
                return PfResult::Oom;
            };

            // 从缓存页复制数据到新页
            let src_virt = PhysAddr(cache_phys).to_virt();
            let dst_virt = new_phys.to_virt();
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src_virt.0 as *const u8,
                    dst_virt.0 as *mut u8,
                    crate::privileged::glue::usize_from_u64(PAGE_SIZE),
                );
            }

            // 注销本 VA 对 Page Cache 的持有: 上面已确认该 VA 正持有此缓存页
            // (acquire 的登记/幂等判据), 故此处直接 pcache_put, 无需再判 PTE.
            super::pcache::pcache_put(vma.inode_id, page_index);

            // 用新页替换映射 (可写)
            let cow_flags = vma.flags | PageFlags::PRESENT | PageFlags::WRITABLE;
            crate::privileged::mm::Vmm::map_page_in_table(
                pml4,
                VirtAddr(aligned as u64),
                new_phys,
                cow_flags,
            );

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

fn handle_stack_expansion(mm: &MmStruct, addr: usize, user_cr3: u64) -> PfResult {
    let page_aligned = addr & !(crate::privileged::glue::usize_from_u64(PAGE_SIZE) - 1);

    if (page_aligned as u64) < stack_guard_end() {
        return PfResult::SignalSegv;
    }

    if mm.find_vma(page_aligned).is_some() {
        return PfResult::SignalSegv;
    }

    let pmm_inst = pmm::get_pmm();
    let Some(phys) = pmm_inst.alloc_page() else {
        return PfResult::Oom;
    };

    let phys_virt = phys.to_virt();
    // SAFETY: PMM 分配的有效页, 清零防信息泄漏
    unsafe {
        core::ptr::write_bytes(
            phys_virt.0 as *mut u8,
            0,
            crate::privileged::glue::usize_from_u64(PAGE_SIZE),
        );
    }

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    crate::privileged::mm::Vmm::map_page_in_table(
        user_cr3,
        VirtAddr(page_aligned as u64),
        phys,
        flags,
    );

    let stack_vma = Vma::new(
        page_aligned,
        page_aligned + crate::privileged::glue::usize_from_u64(PAGE_SIZE),
        flags,
        VmaType::Stack,
    );
    if mm.insert_vma(stack_vma).is_err() {
        // insert_vma 失败: unmap 刚映射的页面, 防止无 VMA 跟踪的孤儿映射
        // (后续 munmap/mprotect 无法覆盖此区域).
        // 帧的归还由 unmap 统一承担 (§8.1 规则 3: 每 USER leaf 拆除即 frame_dec,
        // 归零才延迟释放) —— 此处不得再显式 free_page, 否则同一帧被注销两次.
        #[cfg(target_arch = "x86_64")]
        vmm::get_vmm().unmap_page_in_table(user_cr3, VirtAddr(page_aligned as u64));
        #[cfg(target_arch = "aarch64")]
        vmm::Aarch64Vmm::unmap_page_in_table(user_cr3, VirtAddr(page_aligned as u64));
        return PfResult::Oom;
    }

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

// ── COW (Copy-on-Write) ──

fn do_cow_copy_with_mm(_mm: &MmStruct, _vma: &Vma, addr: usize, user_cr3: u64) -> PfResult {
    let pml4 = user_cr3;

    let Some(old_phys) = super::Vmm::get_physical_in_pml4(pml4, VirtAddr(addr as u64)) else {
        return PfResult::SignalSegv;
    };

    let pmm_inst = pmm::get_pmm();
    let Some(new_phys) = pmm_inst.alloc_page() else {
        return PfResult::Oom;
    };

    let old_virt = old_phys.to_virt();
    let new_virt = new_phys.to_virt();
    // SAFETY: old_phys/new_phys 均为有效物理页, KERNEL_BASE 映射可用;
    // 两个 4KB 区域不重叠 (PMM 保证)
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_virt.0 as *const u8,
            new_virt.0 as *mut u8,
            crate::privileged::glue::usize_from_u64(PAGE_SIZE),
        );
    }

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    crate::privileged::mm::Vmm::map_page_in_table(pml4, VirtAddr(addr as u64), new_phys, flags);

    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    PfResult::Fixed
}

// ── 核态 uaccess 写命中 COW 只读 ──

/// 纯解码判据: 本次故障能否被解释成"内核写用户态的 COW 只读页". 不碰页表, 便于逐项证伪.
///
/// 四条全中才成立, 其余情形一律属内核缺陷, 必须保持原有停机语义:
/// - 故障类必须是 [`A64FaultClass::Permission`] —— 页在表里, 只是权限不符。翻译故障意味着
///   内核访问了根本没映射的地址, 与 COW 无关。
/// - 必须是**写** (`WnR=1`)。COW 只读形态 (AP[2] 置位) 不挡 EL1 读, 读故障没有 COW 解释。
/// - FAR 必须落**用户半区** (`< USER_ADDR_MAX`)。内核高半区故障即内核映射坏了。
/// - `EC` 还须是**同异常级**数据异常: [`PageFaultInfo::from_esr`] 的 `user` 位即由 EC 给出,
///   故 `!user` 等价于"不是 EL0 来源" —— EL0 故障走 [`handle_user_page_fault`], 不得由本
///   支路代答 (它按核态此刻实际翻译的根改表, 与 EL0 侧的"真用户根"口径不同)。
#[must_use]
fn kernel_uaccess_cow_candidate(esr: u64, fault_addr: u64) -> bool {
    let info = PageFaultInfo::from_esr(esr, fault_addr);
    matches!(A64FaultClass::from_esr(esr), A64FaultClass::Permission)
        && info.access.write
        && !info.access.user
        && fault_addr < crate::privileged::constants::limits::USER_ADDR_MAX
}

/// 内核态写用户页被 COW 写保护挡住时的窄恢复支路: 破 COW 后返回 `true`, 调用方直接返回
/// 重试那条 store; 返回 `false` 表示不属本支路射程, 调用方按原有语义处置 (核态故障停机).
///
/// **本支路存在的全部理由**: ARM stage-1 的 `AP[2]` 同时决定 EL1 与 EL0 的写权限 ——
/// `AP[2:1] = 0b01` 是双方皆可写, `0b11` 是双方皆只读, 编码里**没有**"EL0 只读 / EL1 可写"
/// 这一组合。故 fork 的 COW 写保护 (置 `AP[2]`, 见 `mm::cow::mark_cow_readonly`) 会把
/// **内核**写用户页的那条 store 一起挡下来。`x86_64` 同形写保护没有这个问题 (ring0 写忽略
/// PTE 的 WRITABLE 位), 所以这条支路只在 aarch64 被调用; 实机签名即
/// `ESR=0x9600004F FAR=<用户地址> ELR=<kernel .text>` 的 `write_struct_to_user` 那条
/// `str` (2026-11-07 归因, 由本轮启用置只读暴露, 此前被"不置只读"的临时口径掩盖)。
///
/// 恢复动作 = [`super::cow::cow_handle_fault`]: 帧持有计数 1 则就地恢复可写, >1 则复制新页
/// 并注销旧引用 —— 与 EL0 侧 COW 同一入口, 故记账口径 (§8.1 规则 3) 不分叉。
///
/// **COW 的证据取自「用户 leaf + 描述符只读 + 本帧带 COW 待破标」三条, 不取自帧计数,
/// 也不取自 VMA**:
/// - **用户 leaf**: 本支路只管用户半区的 4KB leaf 描述符; supervisor 页与大页/表项描述符
///   落在射程外 (内核写坏它们是内核缺陷, 必须保持停机)。
/// - **描述符只读**: 与「权限故障 + 写」的解码读数互相印证。
/// - **本帧带 COW 待破标** (`pmm::frame_cow_test`, 由 fork 在真正置只读的那一刻登记,
///   见 `mm::cow::clone_user_page_table_cow_inner`): 这是唯一能把 **COW 只读** 与
///   **真·只读映射** 分开的证据 —— 描述符分不开 (`AP_BOTH_RO` 与 fork 的置位形态逐位同形),
///   帧计数也分不开 (子进程退出后计数回到 1, 父侧 leaf 仍是 COW 待破; 2026-11-07 真机
///   读数 `count=1 far=0x409000` 后紧跟 `SYNC! ESR=0x9600004F`, 6 次启动命中 5 次)。
///   计数 1 的真·只读页 (如 `PROT_READ` 的 `.text`) 内核写不得把它升成可写 (I2/I4),
///   故无标记即拒绝。
///
/// 不用 VMA 作判据的理由 (真机取证): `CURRENT_MM` 只在 `execve` 装载 ELF 时写一次
/// (`proc::elf` 是唯一调用点), 既非 per-CPU 也不随 fork/调度更新 ⇒ fork 出的子进程、以及
/// 双核并发时都在读"最后一个 exec 的地址空间"。以它为准会把"页其实合法"误判成"页不属任何
/// VMA"而拒绝恢复 (2026-11-07 本轮第一版即栽在此: `FAR=0x40A0D0` 连续两次停机)。该口径
/// 缺陷同样影响 [`handle_user_page_fault`] 的 VMA 优先路, 已单列为预存问题登记, 本批不擅动。
///
/// 页表根取 `vmm::get_current_pml4()` 而非 [`super::read_user_cr3_asm`]: 触发本次翻译的
/// 就是内核此刻 TTBR0 里那张表 (KPTI 开时是本进程 EL1 视图根, 其下级表与用户表共享 ⇒
/// 改同一 L3 描述符, 两视图不分叉; KPTI 关时即用户表)。
#[must_use]
pub fn break_cow_for_kernel_uaccess(esr: u64, fault_addr: u64) -> bool {
    if !kernel_uaccess_cow_candidate(esr, fault_addr) {
        return false;
    }

    let user_cr3 = vmm::get_current_pml4();
    let aligned = fault_addr & !(PAGE_SIZE - 1);
    // 判据 1: 用户 leaf (裸 u64 判据按架构分派位布局, 与 fork/拆除两侧同一函数).
    let Some(pte) = super::Vmm::get_pte_value(user_cr3, VirtAddr(aligned)) else {
        return false;
    };
    if !super::is_user_leaf(pte) {
        return false;
    }
    // 判据 2: 描述符确实只读. 越界/未映射 (MMIO 一类) 在此一并落回停机语义.
    let Some(leaf) = super::Vmm::translate_in_pml4(user_cr3, VirtAddr(aligned)) else {
        return false;
    };
    if leaf.writable {
        return false;
    }
    // 判据 3: 本帧带 COW 待破标. 非 PMM 分配的物理地址 (MMIO) 由 `frame_cow_test`
    // fail-closed 报 false ⇒ 同样落在射程外.
    if !pmm::get_pmm().frame_cow_test(PhysAddr(leaf.phys.as_u64() & 0x000F_FFFF_FFFF_F000)) {
        return false;
    }

    let broken = super::cow::cow_handle_fault(user_cr3, fault_addr).is_some();
    if broken {
        PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    broken
}

// ── 统计 ──

/// 累计缺页处理次数。
pub static PAGE_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);

/// 返回累计缺页处理次数.
pub fn page_fault_count() -> u64 {
    PAGE_FAULT_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    // stack_top/stack_default_size 是 PageFaultPolicy 的 trait 方法, 需显式引入 trait.
    use crate::privileged::mm::page_fault_policy::PageFaultPolicy;

    #[test]
    fn test_pf_info_from_error_code() {
        let info = PageFaultInfo::from_error_code(0x4000, 0x06);
        assert_eq!(info.fault_addr, 0x4000);
        assert!(info.access.write);
        assert!(info.access.user);
        assert!(!info.page.present);
        assert!(!info.page.reserved);
        assert!(!info.access.instruction);
    }

    #[test]
    fn test_pf_info_not_present() {
        let info = PageFaultInfo::from_error_code(0x1000, 0x00);
        assert!(!info.page.present);
        assert!(!info.access.write);
        assert!(!info.access.user);
    }

    #[test]
    fn test_stack_expansion_candidate() {
        // 回退策略与历史硬编码值一致: 栈顶 = USER_ADDR_MAX, 默认 8MB, guard 1 页
        let p = crate::privileged::mm::page_fault_policy::FallbackPageFaultPolicy;
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

    /// aarch64 `ESR_EL1` → 故障类归类表: 恢复射程内 (翻译/权限) 与射程外 (AF/其余) 双向覆盖,
    /// 且**取指异常一律射程外** (与数据异常共用的 IFSC/DFSC 子码须由 `EC` 挡掉).
    ///
    /// 编码: `EC = ESR[31:26]`、`IL = ESR[25]` ⇒ 数据异常高字节 `0x92` = EL0 (`EC=0x24`),
    /// `0x96` = 同 EL (`EC=0x25`); 取指异常高字节 `0x82` = EL0 (`EC=0x20`), `0x86` = 同 EL
    /// (`EC=0x21`). `WnR = ISS[6]`.
    #[test]
    fn test_a64_fault_class() {
        let cases: [(u64, A64FaultClass, bool); 10] = [
            // 读 level-3 翻译故障 / 写 level-3 权限故障 (COW 写保护的真题读数, EL0 与同 EL 各一对)
            (0x9200_0007, A64FaultClass::Translation, true),
            (0x9200_004F, A64FaultClass::Permission, true),
            (0x9600_0007, A64FaultClass::Translation, true),
            (0x9600_004F, A64FaultClass::Permission, true),
            // 层级子码 (ISS[1:0]) 不改归类: 翻译 level-0 / 权限 level-0
            (0x9200_0004, A64FaultClass::Translation, true),
            (0x9200_000C, A64FaultClass::Permission, true),
            // AF 故障与射程外各类 (地址大小 / 对齐 / 外部同步)
            (0x9200_000A, A64FaultClass::AccessFlag, false),
            (0x9200_0001, A64FaultClass::Other, false),
            (0x9200_0061, A64FaultClass::Other, false),
            (0x9200_0014, A64FaultClass::Other, false),
        ];
        for (esr, want, want_pageable) in cases {
            let got = A64FaultClass::from_esr(esr);
            assert_eq!(got, want, "ESR={:#X}", esr);
            assert_eq!(got.is_demand_pageable(), want_pageable, "ESR={:#X}", esr);
        }
        // 取指异常**射程外**: 翻译 (`IFSC=0b000110`) 与 AF (`IFSC=0b0010LL`) 两类子码和 DFSC
        // 同名段**完全同形**, 只有 `EC` 能区分 —— 若去掉 `EC` 前置判, 取指翻译故障会被读成
        // `Translation` 而进恢复路径, 而本内核不为取指故障建页 (重试再故障 ⇒ 缺页风暴).
        // 取指权限故障用另一段 (`0b0111LL`), 数据侧同值不属任何已归类故障, 靠段表本身即落在
        // 射程外 —— 本条形态存在是为了锁住"取指一律射程外"这个口径, 而非只锁偶然正确.
        let ifetch: [(u64, &str); 4] = [
            (0x8200_0006, "EL0 取指翻译故障"),
            (0x8200_001F, "EL0 取指权限故障"),
            (0x8600_0006, "同 EL 取指翻译故障"),
            (0x8600_001F, "同 EL 取指权限故障"),
        ];
        for (esr, label) in ifetch {
            assert_eq!(
                A64FaultClass::from_esr(esr),
                A64FaultClass::Other,
                "{label} 须落在射程外 (ESR={esr:#X})"
            );
        }
    }

    /// aarch64 `ESR_EL1` → `PageFaultInfo`: 与 x86 `from_error_code` 同结构, 且
    /// `present` 只由故障类决定 (`reserved` 恒 false).
    #[test]
    fn test_pf_info_from_esr() {
        // EL0 写权限故障 (COW): present + write + user ⇒ 命中 cow fallback
        let cow = PageFaultInfo::from_esr(0x9200_004F, 0x8000_0000);
        assert_eq!(cow.fault_addr, 0x8000_0000);
        assert!(cow.page.present);
        assert!(cow.access.write);
        assert!(cow.access.user);
        assert!(!cow.access.instruction);

        // EL0 读翻译故障 (匿名页首次访问): 非 present ⇒ 不命中 COW, 走 VMA/栈路径
        let first_touch = PageFaultInfo::from_esr(0x9200_0007, 0x4000);
        assert!(!first_touch.page.present);
        assert!(!first_touch.page.reserved);
        assert!(!first_touch.access.write);
        assert!(first_touch.access.user);

        // 同 EL (内核态) 写权限故障 (EC=0x25): 无 U/S ⇒ user=false ⇒ 走核态 uaccess 支路
        let el1 = PageFaultInfo::from_esr(0x9600_004F, 0x40A0D0);
        assert!(el1.access.write);
        assert!(!el1.access.user);
        assert!(el1.page.present);

        // 取指故障 (EL0: EC=0x20 / 同 EL: EC=0x21): instruction 位置位, user 按 EC 分档
        let ifetch_low = PageFaultInfo::from_esr(0x8200_001F, 0x1000);
        assert!(ifetch_low.access.instruction);
        assert!(ifetch_low.access.user);
        let ifetch_cur = PageFaultInfo::from_esr(0x8600_001F, 0x1000);
        assert!(ifetch_cur.access.instruction);
        assert!(!ifetch_cur.access.user);
    }

    /// 核态 uaccess 破 COW 支路的准入判据: 只有"同 EL + 写 + 权限故障 + FAR 落用户半区"
    /// 四条全中才成立, 每条各有一条形形态必须被挡 (负控).
    ///
    /// 真题读数 (`kernel_uaccess_cow_candidate` 的成立例): `ESR=0x9600004F
    /// FAR=0x40A0D0 ELR=<kernel .text>` —— 即 `write_struct_to_user` 写 fork 后共享页.
    #[test]
    fn test_kernel_uaccess_cow_candidate_scope() {
        // 成立: 内核写用户 COW 只读页
        assert!(kernel_uaccess_cow_candidate(0x9600_004F, 0x40A0_D0));

        // EL0 来源 (EC=0x24) 不属本支路 —— 由 handle_user_page_fault 负责
        assert!(!kernel_uaccess_cow_candidate(0x9200_004F, 0x40A0_D0));
        // 翻译故障 (未映射) 是内核缺陷, 不得借 COW 名义建页
        assert!(!kernel_uaccess_cow_candidate(0x9600_0007, 0x40A0_D0));
        // 读权限故障 (WnR=0) 无 COW 解释: AP[2] 只挡 EL1 写, 不挡 EL1 读
        assert!(!kernel_uaccess_cow_candidate(0x9600_000F, 0x40A0_D0));
        // FAR 在内核高半区 ⇒ 内核映射坏了, 保持停机
        assert!(!kernel_uaccess_cow_candidate(
            0x9600_004F,
            crate::privileged::constants::limits::USER_ADDR_MAX
        ));
        // 取指异常即使子码同形也不进射程 (由 EC 前置判挡下)
        assert!(!kernel_uaccess_cow_candidate(0x8600_000F, 0x40A0_D0));
    }

    /// 准入判据不成立时**必须在碰页表之前**返回 `false` (fail-closed, 不得"假装已恢复").
    ///
    /// 判据成立之后的动作依赖真页表 (`vmm::get_current_pml4` 读 CR3/TTBR0), host (std) 下
    /// 不可调用 —— 与 `handle_user_page_fault` 的 host 不可测同因 (见
    /// `host-tests/tests/demand_paging_test.rs` 头注释). 故本用例只证伪"准入面"; 成立后的
    /// 真链 (改表 + 重试成功) 由 aarch64 真启动腿判 (见 `scripts/aarch64_cow_boot_test.sh`).
    #[test]
    fn test_break_cow_for_kernel_uaccess_rejects_before_touching_tables() {
        // EL0 来源: 本支路不代答 (走 handle_user_page_fault)
        assert!(!break_cow_for_kernel_uaccess(0x9200_004F, 0x40A0_D0));
        // 翻译故障: 真·未映射, 是内核缺陷
        assert!(!break_cow_for_kernel_uaccess(0x9600_0007, 0x40A0_D0));
        // 读权限故障: 无 COW 解释
        assert!(!break_cow_for_kernel_uaccess(0x9600_000F, 0x40A0_D0));
        // FAR 落内核高半区
        assert!(!break_cow_for_kernel_uaccess(
            0x9600_004F,
            crate::privileged::constants::limits::USER_ADDR_MAX
        ));
    }
}
