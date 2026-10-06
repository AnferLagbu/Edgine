//! Copy-on-Write (COW) — fork 内存共享与写时复制
//!
//! ## 核心思想
//!
//! `fork()` 时父子进程**共享**物理页，均标记为只读。
//! 任意一方写入时触发 #PF → COW handler → 复制物理页。
//!
//! **架构边界**：置只读依赖缺页恢复路径。`x86_64` 侧完整；aarch64 无 page-fault
//! 处理器（EL0 非 SVC 同步异常径直停机），故 aarch64 的 `fork` **只登记帧持有计数、
//! 不置只读**（保持既有"共享写"语义）。补齐 aarch64 缺页路径是内核工程必需项，
//! 见 [cr3-lifetime-ownership.md](../../docs/plan/cr3-lifetime-ownership.md) D-13 登记项。
//!
//! ## 引用计数
//!
//! 计数面已下沉到 PMM（`frame_inc` / `frame_dec` / `frame_ref_count`，按 pfn 索引）：
//! - `alloc_page` 置 1（创建者即首个映射的持有者）；
//! - fork 共享页每 leaf 各 +1 ⇒ 计数 2；
//! - COW fault: 复制新页，旧帧 -1，归零才释放；
//! - munmap / exit: 每 USER leaf -1，归零才释放。
//!
//! 本模块不再持有独立计数表（原 `COW_REFS` 已删除）——两份重复计数收敛为 PMM 单一
//! 帧持有计数面，契约见 `docs/plan/cr3-lifetime-ownership.md` §8.1。
//!
//! ## SAFETY
//!
//! - 所有 `unsafe` 页表访问基于 PMM 分配的有效物理帧，通过 `KERNEL_BASE`
//!   转换为内核虚拟地址，不会产生悬垂指针。
//! - volatile 读写确保编译器不重排 MMIO 相关的页表操作。

use super::vmm;
use super::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr, is_user_leaf};

/// COW 标记: **用户可写** leaf → 只读; 其余情形**原值返回** (`x86_64` 非用户可写页,
/// 以及 aarch64 全量).
///
/// 置只读的前提是"首次写入能被缺页处理器复制恢复". aarch64 无 page-fault 处理器
/// (EL0 非 SVC 同步异常径直停机), 置只读会让 fork 后的首次写入挂死系统, 故 aarch64
/// 分支恒返回原值 —— 保持既有"共享写"语义 (见 `docs/plan/cr3-lifetime-ownership.md`
/// D-13 详情与登记项).
#[inline]
fn mark_cow_readonly(entry: u64) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // bit1 为可写位 (WRITABLE), bit2 为用户位 (USER)
        if entry & 0b10 != 0 && entry & 0b100 != 0 {
            entry & !0b10
        } else {
            entry
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SIMPLIFIED: aarch64 无缺页处理器 ⇒ 不置只读, 仅登记帧持有计数;
        // 影响面: fork 后父子为"共享写"而非真 COW, 跨进程写竞争需用户态自行避免;
        // 何时扩展: 补齐 aarch64 缺页/COW 恢复路径后, 此分支改为清 AP[2] (bit7).
        entry
    }
}

#[expect(
    clippy::used_underscore_binding,
    reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
)]
/// COW 感知的页表克隆: 共享用户空间物理页, 双方标记只读
/// 相比 deep copy 版本, 该版本不分配新物理页, 也不复制数据
///
/// # SMP Safety
/// 本函数持有 `VMM_LOCK` 保护所有页表修改, 确保多核并发安全。
/// 函数返回前刷新全部 TLB 条目使被清除 WRITABLE 位的 PTE 失效。
pub fn clone_user_page_table_cow(parent_pml4: u64) -> Option<u64> {
    let vmm_inst = vmm::get_vmm();
    let _vmm_flags = vmm_inst.acquire_lock();
    let result = clone_user_page_table_cow_inner(parent_pml4);
    vmm_inst.release_lock(&_vmm_flags);
    result
}

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn clone_user_page_table_cow_inner(parent_pml4: u64) -> Option<u64> {
    if parent_pml4 == 0 {
        return None;
    }

    let pmm = super::pmm::get_pmm();
    // 阶段 1 的首个分页帧: 此刻尚无任何已建子树, 失败无需回滚
    let Some(child_pml4_phys) = pmm.alloc_page() else {
        return None;
    };
    let kernel_pml4 = vmm::get_kernel_pml4();

    // SAFETY: child_pml4_phys 刚由 PMM 分配, 物理地址有效;
    // KERNEL_BASE 偏移后得到可访问的内核虚拟地址
    let child_pml4_virt = child_pml4_phys.to_virt().0 as *mut u64;
    unsafe {
        // B05-55C 根治: 必须 cast *mut u8, 否则 *mut u64 的 write_bytes 按
        // count × size_of::<u64>() 清零 32768 字节 (8 页), 覆盖后续物理页
        // (可能含 parent 页表结构页) → parent 页表被清 → fork 崩溃.
        core::ptr::write_bytes(child_pml4_virt as *mut u8, 0, PAGE_SIZE as usize);
    }

    // 装配内核高半区 (KPTI-08 统一入口):
    // - x86_64 且 KPTI 激活: 逐页装配"入口依赖面"必需内核页 (不再整段复制);
    // - x86_64 且 KPTI 未激活 / aarch64: 整段复制内核高半区条目 (行为同此前).
    #[cfg(target_arch = "x86_64")]
    // SAFETY: kernel_pml4 由 vmm_init 写入, child_pml4_phys 刚由 PMM 分配并已清零;
    // 本函数外层持 VMM_LOCK, 无并发修改子页表.
    unsafe {
        super::kpti::assemble_kernel_half(child_pml4_phys.as_u64(), kernel_pml4);
    }

    #[cfg(not(target_arch = "x86_64"))]
    // SAFETY: kernel_pml4 由 vmm_init 写入, 指向有效页表;
    // 复制高半区 (索引 256-511) 使子进程页表共享内核映射
    unsafe {
        let kernel_pml4_virt = PhysAddr(kernel_pml4).to_virt().0 as *const u64;
        core::ptr::copy_nonoverlapping(kernel_pml4_virt.add(256), child_pml4_virt.add(256), 256);
    }

    // SAFETY: parent_pml4 是已注册用户页表的物理地址
    let parent_pml4_virt = PhysAddr(parent_pml4).to_virt().0 as *const u64;

    for i in 0..256usize {
        // SAFETY: 索引 0-255 在 PML4 页范围内; volatile 确保读取真实的页表内容
        let parent_pml4e = unsafe { parent_pml4_virt.add(i).read_volatile() };
        if (parent_pml4e & 1) == 0 {
            continue;
        }

        // 分配失败即回滚: 释放已建的子树页表帧 (此刻父页表未被改动、未登记任何持有者)
        let Some(child_pdpt_phys) = pmm.alloc_page() else {
            free_child_page_table_tree(child_pml4_phys.as_u64());
            return None;
        };
        // SAFETY: 刚分配的页, 通过 KERNEL_BASE 映射有效
        let child_pdpt_virt = child_pdpt_phys.to_virt().0 as *mut u64;
        unsafe {
            // B05-55C 根治: cast *mut u8, 否则清零 8 页覆盖后续物理页
            core::ptr::write_bytes(child_pdpt_virt as *mut u8, 0, PAGE_SIZE as usize);
        }

        let mut child_pml4e = parent_pml4e;
        child_pml4e = (child_pml4e & 0xFFF) | (child_pdpt_phys.as_u64() & 0x000FFFFFFFFFF000);
        // SAFETY: child_pml4_virt 指向有效 PML4 页
        unsafe {
            child_pml4_virt.add(i).write_volatile(child_pml4e);
        }

        // SAFETY: parent_pml4e 已检验 present, phys_to_virt 映射有效
        let parent_pdpt_virt =
            PhysAddr(parent_pml4e & 0x000FFFFFFFFFF000).to_virt().0 as *const u64;

        for j in 0..512usize {
            // SAFETY: pdpt 索引在页范围内
            let parent_pdpte = unsafe { parent_pdpt_virt.add(j).read_volatile() };
            if (parent_pdpte & 1) == 0 || (parent_pdpte & 0x80) != 0 {
                continue;
            }

            let Some(child_pd_phys) = pmm.alloc_page() else {
                free_child_page_table_tree(child_pml4_phys.as_u64());
                return None;
            };
            // SAFETY: 刚分配的页
            let child_pd_virt = child_pd_phys.to_virt().0 as *mut u64;
            unsafe {
                // B05-55C 根治: cast *mut u8, 否则清零 8 页覆盖后续物理页
                core::ptr::write_bytes(child_pd_virt as *mut u8, 0, PAGE_SIZE as usize);
            }

            let mut child_pdpte = parent_pdpte;
            child_pdpte = (child_pdpte & 0xFFF) | (child_pd_phys.as_u64() & 0x000FFFFFFFFFF000);
            unsafe {
                child_pdpt_virt.add(j).write_volatile(child_pdpte);
            }

            let parent_pd_virt =
                PhysAddr(parent_pdpte & 0x000FFFFFFFFFF000).to_virt().0 as *const u64;

            for k in 0..512usize {
                // SAFETY: pd 索引在页范围内
                let parent_pde = unsafe { parent_pd_virt.add(k).read_volatile() };
                if (parent_pde & 1) == 0 {
                    continue;
                }

                if (parent_pde & 0x80) != 0 {
                    // 2MB huge page: 保持原样, 不参与 COW
                    let child_pde_v = parent_pde;
                    // SAFETY: child_pd_virt 指向有效 PD 页
                    unsafe {
                        child_pd_virt.add(k).write_volatile(child_pde_v);
                    }
                    continue;
                }

                // 分配失败即回滚: 释放已建的子树页表帧 (此刻父页表未被改动、未登记任何持有者)
                let Some(child_pt_phys) = pmm.alloc_page() else {
                    free_child_page_table_tree(child_pml4_phys.as_u64());
                    return None;
                };
                let child_pt_virt = child_pt_phys.to_virt().0 as *mut u64;
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    // B05-55C 根治: cast *mut u8, 否则清零 8 页覆盖后续物理页
                    core::ptr::write_bytes(child_pt_virt as *mut u8, 0, PAGE_SIZE as usize);
                }

                let mut child_pde = parent_pde;
                child_pde = (child_pde & 0xFFF) | (child_pt_phys.as_u64() & 0x000FFFFFFFFFF000);
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    child_pd_virt.add(k).write_volatile(child_pde);
                }

                // SAFETY: parent_pt 物理地址来自有效 PDE (阶段 1 只读该页)
                let parent_pt_virt =
                    PhysAddr(parent_pde & 0x000FFFFFFFFFF000).to_virt().0 as *const u64;

                // 阶段 1 只建结构: leaf PTE 逐项原值复制 —— 不改父页表、不改子 PTE、
                // 不登记任何持有者. 使本阶段任一分配失败只需释放已建子树页表帧,
                // 无需回滚页表内容与帧计数 (父 PTE 清 WRITABLE 与 frame_inc 都在
                // 阶段 2, 而阶段 2 无分配 ⇒ 不会失败).
                for l in 0..512usize {
                    // SAFETY: pt 索引在页范围内
                    let parent_pte = unsafe { parent_pt_virt.add(l).read_volatile() };
                    if (parent_pte & 1) == 0 {
                        continue;
                    }
                    // SAFETY: child_pt_virt 指向有效 PT 页, l 在页范围内
                    unsafe {
                        child_pt_virt.add(l).write_volatile(parent_pte);
                    }
                }
            }
        }
    }

    // 阶段 2: 无分配的"登记持有者 + 清 WRITABLE". 阶段 1 已保证所有页表帧分配成功,
    // 故阶段 2 不可能失败, 无需回滚路径.
    //
    // 计数面 (§8.1 规则 3 的 fork 侧): **每个用户 leaf** 在父子两侧各持一份引用,
    // 与拆除侧 (unmap / destroy_page_table 的逐用户 leaf frame_dec) 严格同集 ——
    // 判据统一走 `is_user_leaf` 分派, 不用裸位掩码.
    //
    // COW 标记只对"用户可写页"且只在具备缺页恢复路径的架构上做. 用户页表高半区还含
    // KPTI 入口依赖面映射的 supervisor 页 (USER_CR3_SAVE, SyscallPerCpu, GDT/IDT/TSS,
    // IST 栈, trampoline 栈, RSP0 栈), 这些页无 USER 位; 若对其清 WRITABLE, 用户态
    // 异常入口写 USER_CR3_SAVE → 写保护 #PF → 死循环/Triple Fault.
    for_each_cow_leaf(
        parent_pml4,
        child_pml4_phys.as_u64(),
        |parent_slot, child_slot, parent_pte| {
            if !is_user_leaf(parent_pte) {
                return;
            }
            // fork: 父子各持引用 (每 leaf 一次), PMM 帧持有计数 +1
            pmm.frame_inc(PhysAddr(parent_pte & 0x000FFFFFFFFFF000));

            // 仅当 COW 标记确有变化时才写回 (aarch64 分支恒为原值 ⇒ 两侧都不写)
            let ro_entry = mark_cow_readonly(parent_pte);
            if ro_entry != parent_pte {
                // SAFETY: 两槽位均为有效页表页内的 4KB leaf 槽位; 本函数外层持
                // VMM_LOCK, 这两个槽位由本函数独占访问
                unsafe {
                    parent_slot.write_volatile(ro_entry);
                    child_slot.write_volatile(ro_entry);
                }
            }
        },
    );

    // SMP: 刷新 TLB 使所有被清除 WRITABLE 位的 PTE 失效
    // 父进程可能在其他 CPU 上运行, 完整的 TLB shootdown 需要 IPI
    // 此处至少刷新本地 TLB 确保当前 CPU 的一致性
    crate::arch!(tlb_flush_all());

    Some(child_pml4_phys.as_u64())
}

/// 锁步遍历父子页表在 PD 级以下的 4KB leaf 槽位, 对每个 present leaf 回调
/// `(父槽位, 子槽位, 父 PTE 原值)`.
///
/// 跳过 PD 级大页 (`PS` 位) —— 大页不参与 COW.
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn for_each_cow_leaf_in_pd(
    parent_pd: *mut u64,
    child_pd: *mut u64,
    f: &mut impl FnMut(*mut u64, *mut u64, u64),
) {
    for k in 0..512usize {
        // SAFETY: k 在 PD 页范围内; 两端指针由调用方保证指向有效页表帧
        let pde = unsafe { parent_pd.add(k).read_volatile() };
        // PS 位 (0x80) 置位 = 2MB 大页, 无下级 PT
        if (pde & 1) == 0 || (pde & 0x80) != 0 {
            continue;
        }
        let child_pde = unsafe { child_pd.add(k).read_volatile() };
        if (child_pde & 1) == 0 {
            continue;
        }

        let parent_pt = PhysAddr(pde & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;
        let child_pt = PhysAddr(child_pde & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;

        for l in 0..512usize {
            // SAFETY: l 在 PT 页范围内
            let pte = unsafe { parent_pt.add(l).read_volatile() };
            if (pte & 1) == 0 {
                continue;
            }
            // SAFETY: 两端槽位均在有效 PT 页内; 写权限由调用方持有的 VMM_LOCK 保证
            unsafe {
                f(parent_pt.add(l), child_pt.add(l), pte);
            }
        }
    }
}

/// 锁步遍历父子页表低半区的 4KB leaf 槽位, 对每个 present 且非大页的 leaf 回调
/// `(父槽位, 子槽位, 父 PTE 原值)`.
///
/// 高半区 (索引 256-511, 内核映射) 不属于用户地址空间、不参与 COW, 故不在遍历范围.
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn for_each_cow_leaf(
    parent_root: u64,
    child_root: u64,
    mut f: impl FnMut(*mut u64, *mut u64, u64),
) {
    let parent_pml4 = PhysAddr(parent_root).to_virt().0 as *mut u64;
    let child_pml4 = PhysAddr(child_root).to_virt().0 as *mut u64;

    for i in 0..256usize {
        // SAFETY: i 在 PML4 低半区范围内; 两个根均由调用方保证为有效页表帧
        let pml4e = unsafe { parent_pml4.add(i).read_volatile() };
        if (pml4e & 1) == 0 {
            continue;
        }
        let child_pml4e = unsafe { child_pml4.add(i).read_volatile() };
        if (child_pml4e & 1) == 0 {
            continue;
        }

        let parent_pdpt = PhysAddr(pml4e & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;
        let child_pdpt = PhysAddr(child_pml4e & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;

        for j in 0..512usize {
            // SAFETY: j 在 PDPT 页范围内
            let pdpte = unsafe { parent_pdpt.add(j).read_volatile() };
            // PS 位 (0x80) 置位 = 1GB 大页, 无下级 PD
            if (pdpte & 1) == 0 || (pdpte & 0x80) != 0 {
                continue;
            }
            let child_pdpte = unsafe { child_pdpt.add(j).read_volatile() };
            if (child_pdpte & 1) == 0 {
                continue;
            }

            let parent_pd = PhysAddr(pdpte & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;
            let child_pd = PhysAddr(child_pdpte & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;

            for_each_cow_leaf_in_pd(parent_pd, child_pd, &mut f);
        }
    }
}

/// 释放克隆中途失败时已建成的子页表子树 (仅页表帧; 数据帧与内核映射不触碰).
///
/// 后序遍历: 先释放最底层页表帧, 最后释放 `root` 自身. 子页表页在分配时已被清零,
/// 故其低半区任何 present 槽位必为本次克隆所建; 高半区是内核页表的副本, 不在范围内.
///
/// 调用方须持有 `VMM_LOCK` (`defer_free` 的 `BATCH_HEAD` 单写者前提).
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn free_child_page_table_tree(root: u64) {
    if root == 0 {
        return;
    }
    let pml4 = PhysAddr(root).to_virt().0 as *mut u64;

    for i in 0..256usize {
        // SAFETY: i 在 PML4 低半区范围内; root 为 PMM 分配的有效页表帧
        let pml4e = unsafe { pml4.add(i).read_volatile() };
        if (pml4e & 1) == 0 {
            continue;
        }
        let pdpt = PhysAddr(pml4e & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;

        for j in 0..512usize {
            // SAFETY: j 在 PDPT 页范围内
            let pdpte = unsafe { pdpt.add(j).read_volatile() };
            // 大页 (PS) 无下级页表帧
            if (pdpte & 1) == 0 || (pdpte & 0x80) != 0 {
                continue;
            }
            let pd = PhysAddr(pdpte & 0x000FFFFFFFFFF000).to_virt().0 as *mut u64;

            for k in 0..512usize {
                // SAFETY: k 在 PD 页范围内
                let pde = unsafe { pd.add(k).read_volatile() };
                if (pde & 1) == 0 || (pde & 0x80) != 0 {
                    continue;
                }
                release_child_table_frame(pde & 0x000FFFFFFFFFF000);
            }
            release_child_table_frame(pdpte & 0x000FFFFFFFFFF000);
        }
        release_child_table_frame(pml4e & 0x000FFFFFFFFFF000);
    }
    release_child_table_frame(root);
}

/// 归还单个子页表帧.
///
/// 调用方须持 `VMM_LOCK`. 表帧与数据帧同属需计数帧 (经 `alloc_page` 计数 1), 故走
/// `mm::release_table_frame_locked` 的 `frame_dec` 单一归零门, 保证「同一表帧只入链
/// 一次」(归还时机与锁序见 `mm::release_table_frame_locked`).
fn release_child_table_frame(frame: u64) {
    super::release_table_frame_locked(PhysAddr(frame));
}

/// COW fault 处理: 为写入分配新页
///
/// # SMP Safety
/// 帧持有计数判定 (`frame_ref_count`) 与其后的映射/帧归还**在同一临界区内**完成
/// (`VMM_LOCK`), 消除"判定与映射之间被他核并发建映射改变帧计数"的 TOCTOU 窗口。
/// 已持锁的路径必须直接用 [`cow_handle_fault_locked`], 否则构成递归加锁
/// (`VMM_LOCK` 非重入)。
pub fn cow_handle_fault(pml4: u64, fault_addr: u64) -> Option<u64> {
    let vmm_inst = vmm::get_vmm();
    let lock_flags = vmm_inst.acquire_lock();
    let result = cow_handle_fault_locked(pml4, fault_addr);
    vmm_inst.release_lock(&lock_flags);
    result
}

/// COW fault 处理 —— **要求调用方已持 `VMM_LOCK`**
///
/// 判定与映射同处一个临界区, 故须用 `map_page_in_table_locked` (不自取锁) 与
/// `release_frame_locked` (不自取锁)。读路径 `get_physical_in_pml4` 只做只读遍历,
/// 不取 `VMM_LOCK`, 可在持锁下安全调用。
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn cow_handle_fault_locked(pml4: u64, fault_addr: u64) -> Option<u64> {
    let vmm_inst = vmm::get_vmm();
    let page_aligned = fault_addr & !(PAGE_SIZE - 1);

    let old_phys = vmm_inst.get_physical_in_pml4(pml4, VirtAddr(page_aligned))?;
    let old_frame = old_phys.as_u64() & 0x000FFFFFFFFFF000;

    let pmm_inst = super::pmm::get_pmm();

    // §8.1 判据: 计数 = 持有者数. <= 1 表示本映射即该帧唯一引用 (或该帧未计数,
    // 如设备/MMIO 映射) ⇒ 就地恢复可写, 无需复制.
    let should_reuse = pmm_inst.frame_ref_count(PhysAddr(old_frame)) <= 1;

    if should_reuse {
        // 唯一引用: 直接恢复 WRITABLE 位, 无需分配新页
        let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
        vmm_inst.map_page_in_table_locked(pml4, VirtAddr(page_aligned), old_phys, flags);
        return Some(old_phys.as_u64());
    }

    let new_phys = pmm_inst.alloc_page()?;
    let new_virt = new_phys.to_virt();

    let old_virt = PhysAddr(old_frame).to_virt();
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_virt.0 as *const u8,
            new_virt.0 as *mut u8,
            PAGE_SIZE as usize,
        );
    }

    // 计数归零 = 无其他映射持有旧帧, 可以归还
    if pmm_inst.frame_dec(PhysAddr(old_frame)) {
        // 归零不等于"远端核 TLB 已失效": 若他核曾运行本进程, 其 TLB 仍缓存旧映射,
        // 立即归还后该帧可被重分配 ⇒ 他核经陈旧映射访问他人物理页 (UAF).
        // 归还时机 (两架构统一走延迟释放) 与锁序见 `mm::release_frame_locked`.
        super::release_frame_locked(PhysAddr(old_frame));
    }

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    vmm_inst.map_page_in_table_locked(pml4, VirtAddr(page_aligned), new_phys, flags);

    Some(new_phys.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virt_index_functions() {
        // 低半区低地址: PML4 索引 0, PT 索引 1
        let low = VirtAddr(0x0000_0000_0000_1000u64);
        assert_eq!(low.pml4_idx(), 0);
        assert_eq!(low.pt_idx(), 1);

        // 低半区顶端: 0x7FFF_0000_0000 >> 39 = 0xFF ⇒ PML4 索引 255
        // (原用例对该地址断言 0, 与实现不符; UT-06 实测修正)
        let high = VirtAddr(0x0000_7FFF_0000_0000u64);
        assert_eq!(high.pml4_idx(), 255);
        assert_eq!(high.pt_idx(), 0);
    }
}
