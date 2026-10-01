//! KPTI (Kernel Page Table Isolation) — `x86_64`
//!
//! 抗 Meltdown 用户/内核页表隔离。
//!
//! # 设计目标
//!
//! 1. **双页表**: 一份仅含 entry/exit trampoline 所需最小内核数据 (`USER_PML4`),
//!    一份完整内核页表 (`KERNEL_PML4`)。CPU 在用户态运行 `USER_PML4`,
//!    进入内核后切换为 `KERNEL_PML4`。
//! 2. **安全**: 大部分内核页表条目移除 USER 位, 用户态无法访问内核 .text/.data/.bss/
//!    GDT/IDT/TSS/per-CPU 栈/页表本身。
//! 3. **可关闭**: 通过 `KernelCapabilities::kpti` 编译期开关, 调试时关闭可加快 TT 速度。
//!
//! # 现状 (本轮 PR)
//!
//! - **已完成**:
//!   - 用户页表内核映射装配: **逐页显式映射"入口依赖面"必需页** (KPTI-08 移除
//!     高半区整段复制), 见 `map_kernel_pages_in_user_pml4`
//!   - `switch_to_user_pml4` / `switch_to_kernel_pml4` CR3 切换原语
//!   - 公共 API: `kpti_init`, `kpti_is_active`, `kpti_user_pml4`, `kpti_enter_kernel`,
//!     `kpti_exit_to_user`
//!   - 与 `vmm::Vmm::init` 集成
//!
//! - **未完成 (本轮范围外, 在 `engineering-progress.md` §五 + roadmap Backlog 登记)**:
//!   - **汇编 trampoline 集成**: 当前 syscall 入口是 Rust `syscall_dispatch_from_frame`,
//!     KPTI 切换必须在汇编中做 (CPU 进入内核的第一条指令必须是 `mov cr3, kernel_pml4`,
//!     否则 CPU 仍按 `user_pml4` 寻址, 会因缺页 #PF panic)。
//!     需要新增 `entry_SYSCALL_64` / `swapgs_restore_regs_and_return_to_usermode` 汇编
//!     trampoline, 把当前 `syscall_dispatch_from_frame` 改造成可被 trampoline 调用。
//!   - **PCID/INVPCID 优化**: 当前每次切换 CR3 都 TLB 全清, 高频 syscall 性能损失 5-15%。
//!   - **aarch64 双 TTBR**: 需要在 `vmm_aarch64.rs` 实现 TTBR0 (用户) / TTBR1 (内核) 切换。
//!   - **可写 trampoline 页的 RO 化**: trampoline 代码需要 RO+NX 保护。

#![cfg(target_arch = "x86_64")]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::framework::mm::pmm_alloc_page;
// host-test 下 KERNEL_BASE 仅在 map_text_page (整函数门控排除) 中使用,
// 该符号改由真机分支单独导入 (避免 host-test 维 unused_imports).
#[cfg(not(feature = "host-test"))]
use crate::framework::mm::KERNEL_BASE;
use crate::framework::mm::{PAGE_SIZE, PhysAddr};

// ── PCID 常量 ─────────────────────────────────────────────────────
// PCID (Process-Context Identifier) 占 CR3 低 12 位, 用于 TLB 标记.
// 启用 PCID 后, CR3 切换不再隐式刷新全局 TLB, 改用 INVPCID 精确刷除.

/// 内核页表 PCID
pub const PCID_KERNEL: u64 = 1;
/// 用户页表 PCID
pub const PCID_USER: u64 = 2;

/// INVPCID 指令类型: 按 PCID 刷新 TLB
///
/// 供 VMM 页表修改 (COW/mprotect) 后刷除特定 PCID 的 TLB 条目.
const INVPCID_TYPE_SINGLE: u64 = 0;
/// INVPCID 指令类型: 刷新所有 TLB (包括 global 页)
const INVPCID_TYPE_ALL_INCL_GLOBAL: u64 = 2;

/// 执行 INVPCID 指令, 刷新指定 PCID 的 TLB 条目.
///
/// # Safety
///
/// 调用方保证 CPU 支持 INVPCID (通过 CPUID.07H:EBX.IVPCID 确认).
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn invpcid(pcid: u64, addr: u64, typ: u64) {
    // INVPCID 描述符: 16 字节, [0:7] PCID, [8:15] 线性地址
    // 在栈上构造描述符, 通过内存操作数传递给 INVPCID.
    // INVPCID 格式: invpcid r64, m128 — 第二操作数必须是内存引用.
    let desc: [u64; 2] = [pcid, addr];
    // SAFETY: 调用方保证 CPU 支持 INVPCID; desc 在栈上有效, 16 字节对齐.
    unsafe {
        core::arch::asm!(
            "invpcid {typ}, [{desc}]",
            typ = in(reg) typ,
            desc = in(reg) desc.as_ptr(),
            options(nostack, preserves_flags, readonly),
        );
    }
}

/// 刷新所有 PCID 的 TLB 条目 (不含 global 页).
///
/// # Safety
///
/// 调用方保证 CPU 支持 INVPCID (通过 CPUID.07H:EBX.IVPCID 确认).
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn invpcid_flush_all() {
    // SAFETY: 调用方保证 CPU 支持 INVPCID; type 2 刷新所有 TLB 条目是安全操作.
    unsafe {
        invpcid(0, 0, INVPCID_TYPE_ALL_INCL_GLOBAL);
    }
}

/// 按 PCID + 虚拟地址刷新单条 TLB 条目.
///
/// 用于 VMM COW/mprotect 的细粒度 TLB 失效, 避免全量刷新的性能损失.
///
/// # Safety
///
/// - `pcid` 必须是有效的 PCID (0-4095)
/// - `vaddr` 必须是页对齐的虚拟地址
/// - 调用方保证 vaddr 属于当前地址空间或已通过 CR3 切换访问
/// - CPU 必须支持 INVPCID (通过 CPUID.07H:EBX.IVPCID 确认)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn invpcid_flush_single(pcid: u16, vaddr: u64) {
    // SAFETY: INVPCID type 0 (by individual address + PCID).
    // 前提: pcid 有效 (0-4095), vaddr 页对齐.
    // 调用方保证: vaddr 属于调用方地址空间.
    // 硬件契约: INVPCID 指令在支持的 CPU 上原子刷新单条 TLB.
    unsafe {
        invpcid(u64::from(pcid), vaddr, INVPCID_TYPE_SINGLE);
    }
}

/// CR3 值中嵌入 PCID: PML4 物理地址 | PCID.
#[inline(always)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub const fn cr3_with_pcid(pml4_phys: u64, pcid: u64) -> u64 {
    (pml4_phys & 0x000FFFFFFFFFF000) | (pcid & 0xFFF)
}

/// 检查 CPU 是否支持 INVPCID.
#[inline]
pub fn has_invpcid() -> bool {
    crate::framework::cpu::get_cpu_info().is_some_and(|info| {
        info.features
            .contains(crate::framework::cpu::CpuFeatures::INVPCID)
    })
}

/// 检查 PCID 是否已启用 (CR4.PCIDE = 1).
#[inline]
pub fn pcid_is_enabled() -> bool {
    let cr4: u64;
    // SAFETY: 读取 CR4 是特权操作但无副作用, 仅检查 bit 17.
    unsafe {
        core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nostack, nomem));
    }
    (cr4 >> 17) & 1 == 1
}

// ── 链接脚本符号 (x86_64.ld) ──────────────────────────────────────
// KPTI 用户页表只需 `.text` 的**入口区段**: `_kernel_text_start ~ _kpti_trampoline_end`.
// 链接脚本把 `*(.kpti_trampoline)` 与 `build/isr.o(.text)` 排在 `_kpti_trampoline_end`
// 之前 ⇒ 全部入口 stub (isr0-31/irq0-15/isr_common/irq_common/syscall_entry)、
// `enter_user_asm` 与 `.kpti_trampoline` 内的 Rust 处理函数都在该区段内可取指.
// `_kernel_text_end` 仅用于诊断统计与"收窄不变式"断言 (不得作为用户页表映射上界).

// SAFETY: 链接脚本定义的符号, 地址有效 (只读引用).
// 符号桩化 (host-test): host 无 x86_64.ld 符号, 引用点 (`map_kernel_pages_in_user_pml4`)
// 受 not(host-test) 门控; 声明门控与之严格同构 (本模块已受 target_arch = "x86_64" 门控,
// 不重复 arch 条件).
#[cfg(not(feature = "host-test"))]
unsafe extern "C" {
    pub(super) static _kernel_text_start: u8;
    pub(super) static _kpti_trampoline_end: u8;
    pub(super) static _kernel_text_end: u8;
}

// ── 公共状态 ──────────────────────────────────────────────────────

/// KPTI 是否已初始化 (init 完成后置 true)。
static KPTI_READY: AtomicBool = AtomicBool::new(false);

/// `USER_PML4` 物理地址 (在 `vmm_init` 阶段被初始化)。
///
/// KPTI-08 后本表**不**复制 `KERNEL_PML4[256..511]`: 其高半区只包含
/// `map_kernel_pages_in_user_pml4` 逐页映射的"入口依赖面"必需页
/// (entry 区段代码 + USER_CR3_SAVE + per-CPU GDT/TSS/SyscallPerCpu 头页
/// + IDT + per-CPU IST/trampoline 栈顶页), 其余内核页对用户态完全不可见。
static USER_PML4: AtomicU64 = AtomicU64::new(0);

/// 上一份 PML4 物理地址 (供 `switch_to_kernel_pml4` 切回时使用)。
///
/// 注: 切回时直接用 `KERNEL_PML4` 而非此处保存的旧值, 因为 CPU 上一次在内核态
/// 使用的就是 `KERNEL_PML4`, 保留 `USER_PML4` 即可, 不需要 per-CPU 保存。
static LAST_KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

// ── 公开 API ──────────────────────────────────────────────────────

/// 返回 KPTI 是否已就绪 (init 调用完成)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn kpti_is_active() -> bool {
    KPTI_READY.load(Ordering::Acquire)
}

/// 返回 `USER_PML4` 物理地址 (供 COW fork 等路径构造子进程用户页表)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn kpti_user_pml4() -> u64 {
    USER_PML4.load(Ordering::Acquire)
}

/// 返回 `KERNEL_PML4` 物理地址 (避免其它模块读 `KERNEL_PML4` 内部符号)。
#[inline(always)]
pub fn kpti_kernel_pml4() -> u64 {
    LAST_KERNEL_PML4.load(Ordering::Acquire)
}

// ── [已移除] `kpti_sync_pml4_entry` (KPTI-08) ─────────────────────
//
// 原实现把 `KERNEL_PML4[pml4_idx]` (idx ≥ 256) 单项复制到共享 `USER_PML4`,
// 用于"内核高半区新增映射 (如帧缓冲 MMIO) 后让 user CR3 也能访问"。
//
// KPTI-08 移除高半区整段复制后, 该操作的前提整体失效且**危险**:
// - 它复制的是 PML4 顶层指针, 使 `USER_PML4` 与 `KERNEL_PML4` 共享该 PML4 项下的
//   整棵子树 (PDPT/PD/PT), 等于把内核高半区的一整段映射面重新注入用户页表 ——
//   正是本工程要消除的 Meltdown 隔离缺口;
// - 用户页表已不再需要内核高半区 MMIO: 内核访问 MMIO 全部发生在内核 CR3 下
//   (syscall/中断入口在第一条指令即切 `KERNEL_PML4`), 而用户态访问帧缓冲走
//   低半区**用户**映射 (`fb_mmap_syscall` → `map_page_in_table`, 受"安全门 1"
//   约束, 不复用内核高半区条目)。
//
// 原两处调用点 (`map_2mb_page` / `map_1gb_page`) 已同步删除其同步分支。

// ── 切换原语 (entry/exit trampoline 调用) ────────────────────────

/// 进入内核态: CR3 切换 `USER_PML4` → `KERNEL_PML4`。
///
/// **调用方**: 必须在 CPU 刚进入 ring 0 的第一条指令 (syscall/iret 入口汇编 trampoline)。
/// **当前实现**: 该函数可用作逻辑参考, 但**真正的集成需要在汇编 trampoline 中**,
/// Rust 函数调用栈一旦建立, 切 CR3 就会因旧栈页不可见导致立即 #PF。
///
/// # Safety
///
/// 调用方必须是 CPU 入口 trampoline (栈尚未建立 / 栈为 trampoline 专用页)。
#[inline(never)]
pub unsafe fn kpti_enter_kernel() {
    let kernel_pml4 = LAST_KERNEL_PML4.load(Ordering::Acquire);
    if kernel_pml4 == 0 {
        return;
    }
    // SAFETY: CR3 write is privileged; kernel_pml4 来自 init 阶段的可信来源。
    unsafe {
        core::arch::asm!(
            "mov cr3, {pml4}",
            pml4 = in(reg) kernel_pml4,
            options(nostack, preserves_flags),
        );
    }
}

/// 返回用户态: CR3 切换 `KERNEL_PML4` → `USER_PML4`。
///
/// **调用方**: 必须在 CPU 即将 iretq/sysret 之前的最后一条指令 (exit trampoline)。
///
/// # Safety
///
/// 调用方必须是 exit trampoline (即将 iretq/sysret, 已恢复用户态寄存器)。
#[inline(never)]
pub unsafe fn kpti_exit_to_user() {
    let user_pml4 = USER_PML4.load(Ordering::Acquire);
    if user_pml4 == 0 {
        return;
    }
    // SAFETY: CR3 write is privileged; user_pml4 来自 init 阶段的可信来源。
    unsafe {
        core::arch::asm!(
            "mov cr3, {pml4}",
            pml4 = in(reg) user_pml4,
            options(nostack, preserves_flags),
        );
    }
}

// ── 初始化 ────────────────────────────────────────────────────────

/// 初始化 KPTI: 分配 `USER_PML4` 页 (共享模板), 逐页装配"入口依赖面"所需内核页,
/// 启用 PCID, 并广播 per-CPU `kernel_pml4` / `user_pml4`。
///
/// KPTI-08 后**不再**复制 `KERNEL_PML4[256..512]` (高半区整段复制), 用户页表只含
/// `map_kernel_pages_in_user_pml4` 显式映射的最小内核面 (见 `USER_PML4` 文档)。
///
/// 必须在 `vmm::Vmm::init` 之后调用 (依赖 `KERNEL_PML4` 已初始化)。
///
/// **时序约束**: 本函数早于 `gdt_init` / `idt_init` (见 `lib.rs` 的 `vmm_init` →
/// `interrupt_late_init` 顺序), 故装配时**不得**读 `TSS.ist[]` / `SyscallPerCpu` /
/// `sidt` 等尚未初始化的运行时值: 待映射页一律由静态布局推导 (gdt.rs
/// `ist_tops_virt` / `trampoline_top_virt` / `per_cpu_gdt_head_range`, idt.rs
/// `idt_entries_base_lma`)。
///
/// # Safety
///
/// 调用方保证: `KERNEL_PML4` 已初始化; PMM 可分配页面; KPTI 全局状态在 boot 阶段被独占写入。
/// # Panics
/// 分配 `USER_PML4` 页失败时 panic。
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::verbose_bit_mask,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
pub unsafe fn kpti_init(kernel_pml4: u64) {
    if KPTI_READY.load(Ordering::Acquire) {
        return;
    }

    // 1. 分配 USER_PML4 物理页
    let user_pml4_phys = pmm_alloc_page() as u64;
    // 不可恢复: KPTI 初始化需要 USER_PML4 页, 分配失败意味着内存耗尽,
    // 内核无法安全进入用户态, 只能停机
    assert!(
        user_pml4_phys != 0,
        "[KPTI] failed to allocate USER_PML4 page"
    );
    let user_pml4_virt = PhysAddr(user_pml4_phys).to_virt();

    // 2. 清零
    // SAFETY: pmm 分配的页已对齐, 物理页属于内核
    unsafe {
        core::ptr::write_bytes(user_pml4_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
    }

    // 3. [已移除] 原 `USER_PML4[256..512] = KERNEL_PML4[256..512]` 高半区整段复制。
    //
    // 该复制使共享模板持有内核高半区的**全量**别名映射 (收窄前 353 页), 是 Meltdown
    // 侧信道可利用面 —— CPU 在用户态下仍能通过缓存探测命中这些内核地址的 TLB/页表项。
    // KPTI-08 改由 step 4.5 的 `map_kernel_pages_in_user_pml4` 逐页显式映射"入口依赖面"
    // 必需页; 用户页表中其余内核页无任何条目 (不存在的 PTE 无法被缓存探测).

    // 4. [已移除] 原清除 [0..256] USER 位的循环.
    //
    // 低半区 [0..256] 仅包含用户页 (代码 0x400000, 栈 0x7FFFFFFFE000 等),
    // 内核页全部在高半区 [256..512]. 清除低半区 USER 位会导致 Ring 3
    // 无法执行用户代码 → #PF → 内核重启. 该循环无安全收益.
    //
    // KPTI 安全性由高半区加固 (step 4.5) 保证: 仅 trampoline 代码页保留 RX,
    // 其余内核代码页设为 RO+NX, 数据页限制权限.

    // 4.5 装配 KPTI 用户页表所需的内核映射 (KPTI-07 收窄 + KPTI-10 统一)
    //
    // 与 `VirtualMemoryManager::create_user_page_table` 共用同一函数,
    // 映射面 = `.text` 入口区段 (`_kernel_text_start ~ _kpti_trampoline_end`)
    // + 入口路径必需数据页 (USER_CR3_SAVE / SyscallPerCpu, 见该函数文档).
    // 其余内核 `.text`/`.data`/`.bss` 不进本页表.
    //
    // 符号桩化 (host-test): host 无链接脚本符号且无页表上下文, 整段跳过.
    #[cfg(not(feature = "host-test"))]
    // SAFETY: user_pml4_phys 由 pmm 分配并已清零; boot 阶段单线程执行,
    // 无并发修改页表 (KERNEL_PML4 不受影响, 仅改 USER_PML4).
    unsafe {
        map_kernel_pages_in_user_pml4(user_pml4_phys);
    }

    // 5. 启用 PCID (如果 CPU 支持 INVPCID)
    //    CR4.PCIDE (bit 17) 启用后, CR3 低 12 位为 PCID 而非必须为 0.
    //    启用条件: CPU 支持 INVPCID; 当前 CR3 低 12 位为 0 (硬件要求).
    //    启用后, KPTI CR3 切换携带 PCID, TLB 条目按 PCID 隔离,
    //    无需每次切换都全局刷新 TLB, 显著降低 KPTI 性能开销.
    let pcid_enabled = if has_invpcid() {
        // SAFETY: 读取 CR3 判断低 12 位是否为 0 (PCIDE 启用前提).
        let cur_cr3: u64;
        unsafe {
            core::arch::asm!("mov {0}, cr3", out(reg) cur_cr3, options(nostack, nomem));
        }
        if cur_cr3 & 0xFFF == 0 {
            // SAFETY: CR4 写入仅在 boot 阶段, 设置 PCIDE 位.
            unsafe {
                let cr4: u64;
                core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nostack, nomem));
                core::arch::asm!(
                    "mov cr4, {0}",
                    in(reg) cr4 | (1u64 << 17),
                    options(nostack, nomem, preserves_flags),
                );
            }
            // 启用 PCIDE 后, mov cr3 不再隐式刷新 TLB.
            // 做一次全局 TLB 刷新确保一致性, 然后重新加载 CR3 (带 PCID_KERNEL).
            // SAFETY: INVPCID type 2 刷新所有 TLB 条目 (含 global 页).
            unsafe {
                invpcid_flush_all();
            }
            // 重新加载 CR3 带 PCID_KERNEL
            // SAFETY: kernel_pml4 来自 init 阶段的可信来源; PCIDE 已启用, CR3 低 12 位为 PCID.
            let new_cr3 = cr3_with_pcid(kernel_pml4, PCID_KERNEL);
            unsafe {
                core::arch::asm!(
                    "mov cr3, {0}",
                    in(reg) new_cr3,
                    options(nostack, preserves_flags),
                );
            }
            true
        } else {
            false
        }
    } else {
        false
    };

    // 6. 更新所有 per-CPU SyscallPerCpu PML4 字段
    //    汇编 entry/exit 从 [gs:KERNEL_PML4_OFF] / [gs:USER_PML4_OFF] 读取.
    //    PCID 启用时, 值为 PML4_PHYS | PCID; 未启用时为纯 PML4 物理地址.
    // SAFETY: boot 阶段独占写入, cpu_index 0..256 合法.
    let kernel_cr3 = if pcid_enabled {
        cr3_with_pcid(kernel_pml4, PCID_KERNEL)
    } else {
        kernel_pml4
    };
    let user_cr3 = if pcid_enabled {
        cr3_with_pcid(user_pml4_phys, PCID_USER)
    } else {
        user_pml4_phys
    };

    crate::klog_boot_info!(
        "[KPTI] kpti_init: kernel_pml4={:#x}, user_pml4_phys={:#x}, pcid={}, kernel_cr3={:#x}, user_cr3={:#x}",
        kernel_pml4,
        user_pml4_phys,
        pcid_enabled,
        kernel_cr3,
        user_cr3
    );

    // AP 的 GDT 在 SIPI 之前才按需分配 (见 gdt::alloc_ap_gdt), 此刻只存在 BSP
    // 实例, 故仅写入 BSP 槽位; AP 槽位由 gdt_init_ap 从 BSP GDT 复制该组值.
    // SAFETY: boot 阶段单 CPU 执行, kernel_cr3/user_cr3 是合法 PML4 物理地址,
    // gdt_set_kpti_pml4 是安全的 FFI 调用, cpu 索引 0 合法.
    unsafe {
        crate::framework::arch::gdt::gdt_set_kpti_pml4(0, kernel_cr3, user_cr3);
    }

    // 7. 公开状态
    USER_PML4.store(user_pml4_phys, Ordering::Release);
    LAST_KERNEL_PML4.store(kernel_pml4, Ordering::Release);
    KPTI_READY.store(true, Ordering::Release);
}

// ── 用户页表内核映射装配 (KPTI-07 收窄 / KPTI-10 统一) ──────────────

// 符号桩化 (host-test): 调用点 (kpti_init step 4.5 / create_user_page_table) 已整段
// cfg, host 下无调用者, 函数整体不编译 (避免 dead_code).
#[cfg(not(feature = "host-test"))]
/// 把 KPTI 用户页表所需的**全部内核映射**装配到给定页表.
///
/// 统一两条调用路径 (KPTI-10), 保证共享模板与每进程页表的映射面恒等:
/// - `kpti_init` —— 共享 `USER_PML4` 模板;
/// - `VirtualMemoryManager::create_user_page_table` / COW fork (经 `assemble_kernel_half`)
///   —— 每进程用户页表.
///
/// 映射面 (KPTI-08 移除高半区整段复制后) 即"入口依赖面":
/// 1. `.text` 的**入口区段** `_kernel_text_start ~ _kpti_trampoline_end`
///    (低半区恒等 + `KERNEL_BASE` 直映别名 + 链接脚本镜像别名,
///    见 `map_text_region_in_user_pml4`). 链接脚本把 `*(.kpti_trampoline)` 与
///    `build/isr.o(.text)` 排在该区段内 ⇒ 入口 stub / `syscall_entry` /
///    `enter_user_asm` / KPTI 出口 stub 均可取指; 其余内核代码页不进用户页表.
/// 2. 入口路径必需数据页 `map_kpti_data_pages`:
///    `USER_CR3_SAVE` + IDT 条目表 + 每 CPU 的 GDT 头区 / IST0..3 栈顶页 /
///    KPTI trampoline 栈顶页.
/// 3. 每任务的内核栈顶页由 `map_rsp0_page` 在上下文切换时按需追加.
///
/// # Safety
///
/// 调用方保证 `user_pml4_phys` 是已清零的有效 4 级页表根物理地址;
/// 在 boot 阶段单线程执行或持 `VMM_LOCK`, 无并发修改页表.
pub unsafe fn map_kernel_pages_in_user_pml4(user_pml4_phys: u64) {
    let user_pml4 = PhysAddr(user_pml4_phys).to_virt().0 as *mut u64;

    // SAFETY: 三个链接脚本符号仅做地址取值 (不读内容); user_pml4 由 PMM 分配的
    // 页表根物理地址转换而来, 有效; 调用方保证无并发修改.
    unsafe {
        let text_start = core::ptr::addr_of!(_kernel_text_start) as u64;
        let trampoline_end = core::ptr::addr_of!(_kpti_trampoline_end) as u64;
        let text_end = core::ptr::addr_of!(_kernel_text_end) as u64;
        let pages = |from: u64, to: u64| (to - from + PAGE_SIZE as u64 - 1) / PAGE_SIZE as u64;

        // 诊断: 打印收窄前后的映射面 (QEMU 日志据此判定收窄生效).
        crate::klog_boot_info!(
            "[KPTI] user pml4={:#X} kernel mappings: entry {:#X}-{:#X} ({} pages); excluded kernel text {:#X}-{:#X} ({} pages)",
            user_pml4_phys,
            text_start,
            trampoline_end,
            pages(text_start, trampoline_end),
            trampoline_end,
            text_end,
            pages(trampoline_end, text_end)
        );

        map_text_region_in_user_pml4(user_pml4, text_start, trampoline_end);
        map_kpti_data_pages(user_pml4);
    }
}

// ── 其它用户页表的装配入口 + 每任务 RSP0 页 (KPTI-08) ─────────────

/// 把内核映射装配到一份新建 (或 COW 克隆) 的用户页表.
///
/// 统一 `create_user_page_table` 与 `clone_user_page_table_cow_inner` 两条路径的调用
/// 形态, 避免两处各自维护"KPTI 激活/未激活"分支:
/// - KPTI 激活: 逐页装配"入口依赖面"必需内核页 (`map_kernel_pages_in_user_pml4`);
/// - KPTI 未激活: 退化为原 `KERNEL_PML4[256..512]` 整段复制 (无隔离语义不变).
///
/// KPTI 未激活时**必须**保留整段复制: 该模式下不存在"入口依赖面"概念, 内核高半区
/// 全靠继承的别名映射可达; 收窄会直接破坏内核态访问.
///
/// # Safety
///
/// `user_pml4_phys` 必须是已清零的有效 4 级页表根物理地址; `kernel_pml4_phys`
/// 必须是当前内核页表物理地址; 调用方需保证无并发修改 `user_pml4_phys` 指向的页表.
pub unsafe fn assemble_kernel_half(user_pml4_phys: u64, kernel_pml4_phys: u64) {
    // SAFETY: 两个 PML4 物理地址均由调用方保证有效; 本函数只改 user 页表.
    unsafe {
        // 符号桩化 (host-test): host 无链接脚本符号与页表上下文, 跳过真机分支.
        #[cfg(not(feature = "host-test"))]
        if kpti_is_active() {
            map_kernel_pages_in_user_pml4(user_pml4_phys);
            return;
        }

        let src = PhysAddr(kernel_pml4_phys).to_virt().0 as *const u64;
        let dst = PhysAddr(user_pml4_phys).to_virt().0 as *mut u64;
        core::ptr::copy_nonoverlapping(src.add(256), dst.add(256), 256);
    }
}

/// 把给定任务的内核栈**顶页**映射进指定用户页表 (KPTI-08, 上下文切换路径调用).
///
/// 触发场景: 用户态 → 内核态的入口在**切换 CR3 之前**就要用 `TSS.RSP0` /
/// `[gs:kernel_rsp]` 指向的内核栈压入内容 (5 项 iretq 帧 / 保存现场).
/// KPTI-08 移除高半区别名复制后, 该栈顶页不再自动可见 ⇒ 必须显式映射.
///
/// 页选择: `(kernel_stack_top - 1) & !0xFFF` —— 内核栈自顶向下增长, 首次压入必然
/// 落在栈顶页内, 故 1 页足够. aarch64 侧不需此形态: 其异常入口改为**先切 TTBR
/// 再压帧**, 内核栈只经 TTBR1 可达, 从不出现在 EL0 可见页表中.
/// 入参 `kernel_stack_top` 系 `Process::allocate_kernel_stack` 产物, 恒为高半区 VA
/// 且页对齐 (`PhysAddr::to_virt() + KERNEL_STACK_SIZE`).
///
/// 权限: `PRESENT | WRITABLE`, **不设 USER** —— 访问路径 CPL 恒为 0, 设 USER 等于
/// 把内核栈暴露给用户态 (提权风险).
///
/// **不做远程 TLB 失效**: 该 VA 由内核栈分配唯一确定 (`KERNEL_BASE + phys`, 每次
/// 分配得唯一 phys), 故一个用户页表内同一 VA 的 PTE 值恒定; 重复映射写回相同值,
/// 旧 TLB 条目依旧有效. 首次映射时不存在旧条目. (页表遍历 + 幂等写入属可接受代价:
/// 仅在切换目标为不同任务的第一次发生实际分配.)
///
/// # Safety
///
/// 调用方保证 `user_pml4_phys` 是有效用户页表根物理地址 (低 12 位可为 PCID 编码,
/// 本函数自行去掉); `kernel_stack_top` 是该任务内核栈顶的高半区 VA.
pub unsafe fn map_rsp0_page(user_pml4_phys: u64, kernel_stack_top: u64) {
    // 符号桩化 (host-test): host 无页表上下文与 PMM, 真机分支整段跳过.
    #[cfg(feature = "host-test")]
    let _ = (user_pml4_phys, kernel_stack_top);

    #[cfg(not(feature = "host-test"))]
    // SAFETY: 调用方保证 user_pml4_phys 有效; 目标 VA 属该进程私有页表,
    // 每次切换 PA 值恒定, 无并发写冲突 (见本函数文档).
    unsafe {
        let top_page = (kernel_stack_top - 1) & !(PAGE_SIZE as u64 - 1);
        let user_pml4 = PhysAddr(user_pml4_phys & !(PAGE_SIZE as u64 - 1))
            .to_virt()
            .0 as *mut u64;
        map_text_page(
            user_pml4,
            top_page,
            top_page - KERNEL_BASE,
            0x3,
            "RSP0 top page",
        );
    }
}

// ── .text 入口区段映射 ────────────────────────────────────────────

// 符号桩化 (host-test): 唯一调用点 (`map_kernel_pages_in_user_pml4`) 已整段 cfg,
// host 下无调用者, 函数整体不编译 (避免 dead_code).
#[cfg(not(feature = "host-test"))]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 在用户页表中映射 `.text` 入口区段 (PRESENT only, SMEP-safe).
///
/// 对区段内每个物理页映射 **3 个别名**, 三者分别被不同路径使用 (KPTI-08 实证):
/// 1. **低半区恒等** `phys`: 链接脚本 `.text` 的 VMA = LMA (低地址), 故内核取指/
///    函数指针走低地址 (内核线程首切 `jmp [rsi+56]` 取的就是链接低地址);
/// 2. **`KERNEL_BASE` 直映别名** `KERNEL_BASE + phys`: `LSTAR` (syscall 入口) 与
///    IDT 全部门目标都用该别名 —— `gdt_init` 写 `LSTAR`、`idt_init` 的 `addr!`
///    宏均取 `lo + KERNEL_BASE`。**KPTI-08 前该别名靠继承的高半区副本才可达**,
///    移除副本后必须显式映射, 否则 Ring 3 的 syscall/中断入口即刻 #PF;
/// 3. **链接脚本镜像别名** `0xFFFF800001000000 + phys`: 链接脚本声明的
///    `_kernel_text_vma` 偏移, 保留以免误伤未识别的使用点 (其去留需独立验证,
///    不以"试删后能启动"为依据).
///
/// 原因: 异常处理代码 (isr0-isr31, irq0-irq15, `syscall_entry`) 位于该区段,
/// 用户态触发异常/系统调用时 CPU 使用用户页表寻址, 必须能取指执行这些代码页.
///
/// 权限: PRESENT (Ring 0 可执行, SMEP 兼容). 不设 USER 位:
/// SMEP 启用时 Ring 0 不能执行 USER 页, 设 USER 会导致 `syscall_entry` #PF.
/// 不设 WRITABLE → 只读. 不设 NX → 可执行.
///
/// 收窄不变式 (KPTI-07): `text_end_phys` 不得越过 `_kpti_trampoline_end`, 越界即
/// fail-closed 停机 (防止调用点重新放大映射面而静默扩大隔离缺口).
///
/// # Safety
///
/// 调用方保证: `user_pml4` 是有效的用户页表虚拟地址指针;
/// `text_start_phys`/`text_end_phys` 是 `.text` 物理地址范围;
/// 在 boot 阶段单线程执行, 无并发修改页表.
pub(super) unsafe fn map_text_region_in_user_pml4(
    user_pml4: *mut u64,
    text_start_phys: u64,
    text_end_phys: u64,
) {
    // 链接脚本镜像别名偏移 (`_kernel_text_vma = 0xFFFF800001000000 + .`).
    const LINKER_VMA_OFFSET: u64 = 0xFFFF800001000000;

    // 权限位: PRESENT (bit 0) = 0x1
    // 不设置 USER (bit 2): SMEP 启用时 Ring 0 不能执行 USER 页,
    // syscall_entry/isr_common 等入口在 CR3 切换前从用户页表取指,
    // USER 标志会导致 #PF (instruction fetch).
    // 不设置 WRITABLE (bit 1) → 只读
    // 不设置 NX (bit 63) → 可执行
    const FLAGS: u64 = 0x1; // 仅 PRESENT 位 (SMEP 安全)

    // 收窄不变式 (KPTI-07): 映射上界不得越过 `_kpti_trampoline_end`.
    // 越界说明某调用点重新放大了用户页表的代码映射面 ⇒ fail-closed 停机,
    // 而非静默扩大隔离缺口 (Meltdown 面随映射面增长).
    let trampoline_end = core::ptr::addr_of!(_kpti_trampoline_end) as u64;
    assert!(
        text_end_phys <= trampoline_end,
        "[KPTI] .text 映射越界: requested end={text_end_phys:#X} > _kpti_trampoline_end={trampoline_end:#X}"
    );

    let page_start_phys = text_start_phys & !(PAGE_SIZE as u64 - 1);
    let page_end_phys = (text_end_phys + PAGE_SIZE as u64 - 1) & !(PAGE_SIZE as u64 - 1);

    crate::klog_boot_info!(
        "[KPTI] map_text_region: lma={:#X}-{:#X}, aliases=identity/KERNEL_BASE/linker-vma ({} pages)",
        text_start_phys,
        text_end_phys,
        (page_end_phys - page_start_phys) / PAGE_SIZE as u64
    );

    // SAFETY: 调用方保证 user_pml4 有效; text_start/text_end 是合法地址范围;
    // boot 阶段单线程执行, 无并发修改页表. PMM 分配的页已对齐且属于内核.
    unsafe {
        let mut phys = page_start_phys;
        while phys < page_end_phys {
            map_text_page(user_pml4, phys, phys, FLAGS, "low-half identity");
            map_text_page(
                user_pml4,
                KERNEL_BASE + phys,
                phys,
                FLAGS,
                "KERNEL_BASE alias (LSTAR/IDT gates)",
            );
            map_text_page(
                user_pml4,
                LINKER_VMA_OFFSET + phys,
                phys,
                FLAGS,
                "linker _kernel_text_vma alias",
            );
            phys += PAGE_SIZE as u64;
        }
    }
}

/// 映射单个 trampoline 页面到用户页表.
///
/// # Safety
///
/// 调用方保证: `user_pml4` 有效; `vma` 和 `phys` 对齐;
/// boot 阶段单线程执行, 无并发修改页表.
// 有意窄化: 显式收窄, 调用方保证值域
// 符号桩化 (host-test): 调用点 (map_kpti_data_pages / map_text_region_in_user_pml4)
// 均已被 cfg 排除, host 下无调用者, 函数整体不编译 (避免 dead_code).
#[cfg(not(feature = "host-test"))]
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
unsafe fn map_text_page(user_pml4: *mut u64, vma: u64, phys: u64, flags: u64, _desc: &str) {
    // SAFETY: 调用方保证 user_pml4 有效; vma/phys 是合法地址;
    // boot 阶段单线程执行, 无并发修改页表. PMM 分配的页已对齐且属于内核.
    unsafe {
        // 计算 4 级页表索引
        let pml4_idx = (vma >> 39) & 0x1FF;
        let pdpt_idx = (vma >> 30) & 0x1FF;
        let pd_idx = (vma >> 21) & 0x1FF;
        let pt_idx = (vma >> 12) & 0x1FF;

        // 确保 PML4[pml4_idx] 存在 (分配 PDPT 页)
        let pml4e = core::ptr::read_volatile(user_pml4.add(pml4_idx as usize));
        let pdpt_phys = if pml4e & 1 != 0 {
            pml4e & 0x000FFFFFFFFFF000
        } else {
            let new_page = pmm_alloc_page() as u64;
            assert!(new_page != 0, "[KPTI] map_text_page: alloc PDPT failed");
            let new_page_virt = PhysAddr(new_page).to_virt();
            // SAFETY: new_page 由 PMM 分配, 属于内核; 清零 4KB
            core::ptr::write_bytes(new_page_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
            // 设置 PML4 项: PRESENT + WRITABLE + USER
            core::ptr::write_volatile(user_pml4.add(pml4_idx as usize), new_page | 0x7);
            new_page
        };
        let pdpt = (pdpt_phys + KERNEL_BASE) as *mut u64;

        // 确保 PDPT[pdpt_idx] 存在 (分配 PD 页)
        let pdpte = core::ptr::read_volatile(pdpt.add(pdpt_idx as usize));
        let pd_phys = if pdpte & 1 != 0 {
            pdpte & 0x000FFFFFFFFFF000
        } else {
            let new_page = pmm_alloc_page() as u64;
            assert!(new_page != 0, "[KPTI] map_text_page: alloc PD failed");
            let new_page_virt = PhysAddr(new_page).to_virt();
            // SAFETY: new_page 由 PMM 分配, 属于内核; 清零 4KB
            core::ptr::write_bytes(new_page_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
            core::ptr::write_volatile(pdpt.add(pdpt_idx as usize), new_page | 0x7);
            new_page
        };
        let pd = (pd_phys + KERNEL_BASE) as *mut u64;

        // 确保 PD[pd_idx] 存在 (分配 PT 页)
        // 历史 (TRACK-INIT-RING3-SYSCALL): 当时 `USER_PML4[256..511]` 是从
        // `KERNEL_PML4` 复制的, 底层 PDPT/PD 物理页与内核共享, 内核 PD 里存在
        // 2MB 大页条目 (PS=1); 若把大页 PDE 误读为 PT 指针, PTE 会写到错误物理页,
        // CPU 仍按原大页读到全零页 → syscall_entry 解码为 add [rax],al → #PF CR2=0x1.
        //
        // KPTI-08 现状: 用户页表的整棵子树 (PDPT/PD/PT) 全部由本模块私有分配, 从未
        // 写入大页条目 (本模块只用 4KB 叶项) ⇒ PS=1 分支**实际上不可达**. 保留该分支
        // 作为防御: 一旦将来有代码把大页映射进用户页表, 拆分语义仍是唯一正确解,
        // 且拆的是本页表私有的 PD, 不再污染内核页表.
        let pde = core::ptr::read_volatile(pd.add(pd_idx as usize));
        let pt_phys = if pde & 1 != 0 {
            if pde & (1 << 7) != 0 {
                // PS=1: 2MB 大页 → 直接拆分
                const PS_BIT: u64 = 1 << 7;
                let huge_base = pde & 0x000FFFFFFFE00000; // bits 51:21
                let pde_flags = pde & 0xFFF & !PS_BIT; // 保留权限, 清 PS

                let new_pt = pmm_alloc_page() as u64;
                assert!(new_pt != 0, "[KPTI] map_text_page: alloc PT (split) failed");
                let new_pt_virt = PhysAddr(new_pt).to_virt();
                // SAFETY: new_pt 由 PMM 分配; 清零后填充 512 PTE
                core::ptr::write_bytes(new_pt_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
                let new_pt_ptr = new_pt_virt.0 as *mut u64;
                for i in 0..512u64 {
                    core::ptr::write_volatile(
                        new_pt_ptr.add(i as usize),
                        (huge_base + i * PAGE_SIZE as u64) | pde_flags,
                    );
                }
                // 更新 PDE → 指向新 PT (PS=0, 修改共享 PD, 同时影响内核页表)
                core::ptr::write_volatile(pd.add(pd_idx as usize), new_pt | pde_flags);

                crate::klog_boot_info!(
                    "[KPTI] split_2mb: PDE[{}]={:#X} → new PT={:#X}",
                    pd_idx,
                    pde,
                    new_pt
                );
                new_pt
            } else {
                // PS=0: 普通 PDE → 已指向 PT 页
                pde & 0x000FFFFFFFFFF000
            }
        } else {
            let new_page = pmm_alloc_page() as u64;
            assert!(new_page != 0, "[KPTI] map_text_page: alloc PT failed");
            let new_page_virt = PhysAddr(new_page).to_virt();
            // SAFETY: new_page 由 PMM 分配, 属于内核; 清零 4KB
            core::ptr::write_bytes(new_page_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
            core::ptr::write_volatile(pd.add(pd_idx as usize), new_page | 0x7);
            new_page
        };
        let pt = (pt_phys + KERNEL_BASE) as *mut u64;

        // 设置 PT[pt_idx] = phys | flags
        core::ptr::write_volatile(pt.add(pt_idx as usize), phys | flags);
    }
}

// ── KPTI 入口数据页映射 ──────────────────────────────────────────

// 符号桩化 (host-test): 唯一调用点 (`map_kernel_pages_in_user_pml4`) 与
// USER_CR3_SAVE 汇编符号在 host 维均不存在, 函数整体不编译 (避免 dead_code).
#[cfg(not(feature = "host-test"))]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// KPTI 中断/系统调用入口在 CR3 切换前需要访问的数据页面。
///
/// 当 CPU 在用户态触发中断/异常/系统调用时, `isr_common / irq_common /
/// syscall_entry` 在切换到内核页表前需要:
/// 1. `mov [USER_CR3_SAVE], rax` — 保存用户 CR3 到 .bss 变量 (绝对寻址 = 链接低地址);
/// 2. CPU 经 `IDTR.BASE` 取门描述符 (IDT 条目表);
/// 3. CPU 经 `GDTR.BASE` 取门目标 CS 的描述符, 经 TSS 描述符取 `RSP0` / `IST[]`;
/// 4. `mov rax, [gs:KERNEL_PML4_OFF]` — swapgs 后从 `SyscallPerCpu` 读内核 PML4
///    (`GS_BASE` = `&gdt.syscall` 的链接低地址);
/// 5. CPU 用 `TSS.ist[N-1]` 压入异常帧; `process_switch_asm` 出口用
///    `SyscallPerCpu.trampoline_top` 压 iretq 帧。
///
/// 上述访问全部发生在 CR3 切换前 (此时仍为用户页表), 故相关页必须在用户页表中
/// 有 PRESENT | WRITABLE 映射, 否则触发 #PF → Double Fault。
///
/// 映射别名 (KPTI-08 实证, 与被访问方的寻址方式严格对应):
/// - `USER_CR3_SAVE` / IDT / GDT 头区: **低半区恒等** (汇编绝对寻址 / `IDTR.BASE` /
///   `GDTR.BASE` / TSS 描述符基址 / `GS_BASE` 全部是链接低地址) + 两高半区别名;
/// - IST / trampoline / RSP0 栈顶页: **仅 `KERNEL_BASE` 高半区别名**
///   (`TSS.ist[]` 与 `trampoline_top` 由 `init_stack_tops` 写成
///   `KERNEL_BASE + phys`, 而 CPU/stub 正是按该 VA 压栈)。
///
/// # 安全性
///
/// 不设 USER 位. 访问路径 CPL 全部为 0 (syscall 指令强制 CPL=0,
/// 中断入口 CPU 自动加载内核 CS), 因此不需要 USER 位即可访问.
/// 用户态 (CPL=3) 无法读写这些数据页, 不暴露内核 PML4 物理地址
/// 与 per-CPU 内核栈.
///
/// 根本修复方向: 重构 KPTI 入口 trampoline, 将内核 PML4 地址与栈顶地址嵌入
/// 代码本身 (立即数), 使 CR3 切换前不依赖 .data/.bss 中的数据.
///
/// **时序约束**: 本函数在 `kpti_init` (早于 `gdt_init` / `idt_init`) 中即被调用,
/// 故待映射地址一律由**静态布局推导** (`per_cpu_gdt_head_range` /
/// `ist_tops_virt` / `trampoline_top_virt` / `idt_entries_base_lma`),
/// 不得读 TSS / `sidt` 等尚未初始化的运行时值。
///
/// # Safety
///
/// 调用方保证: `user_pml4` 是有效的 `USER_PML4` 虚拟地址指针;
/// 在 boot 阶段单线程执行或持 `VMM_LOCK`, 无并发修改页表.
pub(super) unsafe fn map_kpti_data_pages(user_pml4: *mut u64) {
    // 权限: PRESENT (bit 0) + WRITABLE (bit 1) = 0x3
    //
    // 安全: 不设 USER 位. 访问路径 CPL 全部为 0:
    //   - syscall 指令入口: CPU 强制 CPL=0 (Intel SDM SYSCALL)
    //   - isr_common/irq_common: CPU 自动加载内核 CS from TSS, CPL=0
    // 移除 USER 位防止用户态 (CPL=3) 读 USER_CR3_SAVE / SyscallPerCpu,
    // 避免暴露内核 PML4 物理地址与 per-CPU 内核栈.
    const FLAGS: u64 = 0x3; // PRESENT | WRITABLE
    // 链接脚本镜像别名偏移 (与 `map_text_region_in_user_pml4` 同源).
    const LINKER_VMA_OFFSET: u64 = 0xFFFF800001000000;

    // 1. USER_CR3_SAVE 所在页
    //    USER_CR3_SAVE 位于 .bss 段, isr.asm 用绝对寻址 `mov [USER_CR3_SAVE], rax`,
    //    访问的虚拟地址是链接低地址 (LMA), 需恒等映射; 另映射两高半区别名.
    let user_cr3_page =
        core::ptr::addr_of!(super::super::mm::USER_CR3_SAVE_ASM) as u64 & !(PAGE_SIZE as u64 - 1);

    // SAFETY: user_pml4 有效; 页地址来自链接器符号 / 静态布局, 合法;
    // boot 阶段单线程执行或持 VMM_LOCK, 无并发修改.
    unsafe {
        map_text_page(
            user_pml4,
            user_cr3_page,
            user_cr3_page,
            FLAGS,
            "USER_CR3_SAVE LMA",
        );
        map_text_page(
            user_pml4,
            KERNEL_BASE + user_cr3_page,
            user_cr3_page,
            FLAGS,
            "USER_CR3_SAVE KERNEL_BASE",
        );
        map_text_page(
            user_pml4,
            LINKER_VMA_OFFSET + user_cr3_page,
            user_cr3_page,
            FLAGS,
            "USER_CR3_SAVE linker VMA",
        );
    }

    // 2. IDT 条目表所在页 (仅低半区恒等: `lidt` 装载的 `IDTR.BASE` 就是条目表链接地址,
    //    用户态触发中断时 CPU 在切 CR3 前按该地址取门描述符).
    //    IDT 表 256 项 × 16B = 4KB, 但 `IdtState.entries` 之前还有其它字段, 故可能跨页;
    //    这里以区间方式逐页映射.
    let idt_base = crate::framework::idt::idt_entries_base_lma();
    let idt_page_start = idt_base & !(PAGE_SIZE as u64 - 1);
    let idt_len = crate::framework::idt::IDT_ENTRIES as u64
        * core::mem::size_of::<crate::framework::idt::IdtEntry>() as u64;
    let idt_page_end = (idt_base + idt_len + PAGE_SIZE as u64 - 1) & !(PAGE_SIZE as u64 - 1);

    // SAFETY: user_pml4 有效; IDT 表位于内核静态区, 地址合法; 无并发修改.
    unsafe {
        let mut p = idt_page_start;
        while p < idt_page_end {
            map_text_page(user_pml4, p, p, FLAGS, "IDT entries");
            p += PAGE_SIZE as u64;
        }
    }

    // 3. 逐 CPU: GDT 头区 (GDT entries + TSS + SyscallPerCpu) 与各栈顶页
    //
    //    GDT 头区页: CPU 取段描述符 / TSS 里的 RSP0/IST 需要; `GS_BASE` 指向
    //    `SyscallPerCpu` 的链接低地址 ⇒ 需恒等映射.
    //    栈顶页: IST0..3 与 trampoline 栈均由 CPU/stub 按 `KERNEL_BASE + phys`
    //    的高半区 VA 压栈 ⇒ 只需该别名 (1 页/栈).
    let cpu_count = crate::framework::smp::get_cpu_count().max(1);
    let mut gdt_head_pages = 0u64;

    // SAFETY: user_pml4 有效; 各地址由静态布局推导, 页对齐且属内核; 无并发修改.
    unsafe {
        for cpu in 0..cpu_count {
            let (head_start, head_end) = crate::framework::arch::gdt::per_cpu_gdt_head_range(cpu);
            let mut p = head_start & !(PAGE_SIZE as u64 - 1);
            let end = (head_end + PAGE_SIZE as u64 - 1) & !(PAGE_SIZE as u64 - 1);
            while p < end {
                map_text_page(user_pml4, p, p, FLAGS, "GDT head LMA");
                map_text_page(user_pml4, KERNEL_BASE + p, p, FLAGS, "GDT head KERNEL_BASE");
                map_text_page(
                    user_pml4,
                    LINKER_VMA_OFFSET + p,
                    p,
                    FLAGS,
                    "GDT head linker VMA",
                );
                gdt_head_pages += 1;
                p += PAGE_SIZE as u64;
            }

            // IST0..3 栈顶页: 栈顶页对齐, 故页 = top - PAGE_SIZE.
            for top in crate::framework::arch::gdt::ist_tops_virt(cpu) {
                map_text_page(
                    user_pml4,
                    top - PAGE_SIZE as u64,
                    top - PAGE_SIZE as u64 - KERNEL_BASE,
                    FLAGS,
                    "IST top page",
                );
            }

            // KPTI trampoline 栈顶页 (`process_switch_asm` 出口用).
            let tramp_top = crate::framework::arch::gdt::trampoline_top_virt(cpu);
            map_text_page(
                user_pml4,
                tramp_top - PAGE_SIZE as u64,
                tramp_top - PAGE_SIZE as u64 - KERNEL_BASE,
                FLAGS,
                "trampoline top page",
            );
        }
    }

    crate::klog_boot_info!(
        "[KPTI] data pages mapped: USER_CR3_SAVE={:#X}, IDT={:#X}-{:#X} ({} pages), GDT head {} page(s)/cpu, {} cpu(s)",
        user_cr3_page,
        idt_page_start,
        idt_page_end,
        (idt_page_end - idt_page_start) / PAGE_SIZE as u64,
        gdt_head_pages / u64::from(cpu_count),
        cpu_count
    );
}

// ── 测试辅助 (host-tests) ────────────────────────────────────────

/// KPTI 关闭时 (kpti=false) 的占位 `USER_PML4`。
///
/// KPTI 关闭时, `USER_PML4` 沿用 `KERNEL_PML4` (无隔离), 此函数返回 `KERNEL_PML4` 物理地址
/// 以便 `map_to_user_pml4` 等 API 在两条路径都可用。
#[inline(always)]
pub fn kpti_user_pml4_or_kernel(kernel_pml4: u64) -> u64 {
    let up = USER_PML4.load(Ordering::Acquire);
    if up == 0 { kernel_pml4 } else { up }
}
