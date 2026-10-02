//! KPTI (Kernel Page Table Isolation) — AArch64
//!
//! ARMv8-A Meltdown 缓解: 用户态 (EL0) 运行期间令 TTBR1_EL1 指向**最小化内核页表**
//! (trampoline 表), 仅映射异常入口实际用到的页, 缩小内核地址空间在用户态的可见面.
//!
//! # 全切换模型 (对齐 x86_64 CR3 语义)
//!
//! 内核代码/数据/栈住在 **TTBR1 领地** (高半区); 异常向量表与 KPTI 全局量页经
//! 高半区别名 (`VA = PA + 高半区基址`) 由 TTBR1_EL1 翻译. 故"用户态隔离"必须
//! TTBR0/TTBR1 **同时**切换:
//!
//! - **EL0 运行期**: TTBR0 = 用户页表; TTBR1 = trampoline 表 (页级最小化).
//! - **异常入口** (`exception.rs` 的 `handle_el0_*`): **先**切
//!   TTBR0 → 内核恒等表 (`kernel_ttbr0`)、TTBR1 → 完整内核根表
//!   (`kernel_ttbr1`), **再**向内核栈压入 280 字节异常帧 —— 内核栈只经
//!   TTBR1 可达 (其 VA 在高半区), 切表前压帧必然 Data Abort. 切换序列本身需
//!   2 个 scratch 寄存器, 而入口时刻全部 GPR 都是用户态活跃值, 故先把它们存入
//!   [`KPTI_GLOBALS`] 的暂存槽, 切表后取回.
//! - **异常出口** (eret 前): 帧先读回 (切表后内核栈即不可达), 再切回
//!   TTBR0 → 用户页表 (`user_ttbr0`), TTBR1 → trampoline 表.
//!   切换代码必须位于高半区 (`.vectors`) —— TTBR0 切换后低半区代码即不可取指
//!   (实测: 低半区切换必然 Prefetch Abort).
//! - **进入 EL0**: `arch/aarch64/mod.rs::enter_user` 跳转到 `.vectors` 内的
//!   `kpti_enter_user_trampoline` (高半区) 完成切换后 eret.
//!
//! 内核栈页**不出现在任何 EL0 可见页表**中 (trampoline 表只映射 `.vectors` 与
//! [`KPTI_GLOBALS`] 两页), 这是"先切表再压帧"相对"把栈顶页映射进用户页表"的
//! 隔离收益. 与 x86_64 的差异: x86 的用户 PML4 能映射高 VA, 故其 RSP0 栈页
//! 仍走"映射进用户页表"形态 (见 `kpti::map_rsp0_page`).
//!
//! # SMP 前提 (DECISION-082 订正)
//!
//! 本模块的全局量 [`KPTI_GLOBALS`] 为**单实例** (汇编按固定字节偏移访问, 无按核
//! 索引). aarch64 自 DECISION-082 起已具备次核上线路径 (`arch/aarch64/smp_init.rs`),
//! 故须区分两类字段:
//!
//! - **boot 期一次性发布、此后只读**: `ready` / `kernel_ttbr0` / `kernel_ttbr1` /
//!   `tramp_ttbr1` —— 核间共享安全 (发布由 `ready` 的 Release/Acquire 配对
//!   保证, 见 [`kpti_init`]).
//! - **每核活跃值**: `user_ttbr0` 与 `tramp_save0`/`tramp_save1` —— 由 EL0 入口/出口
//!   汇编与任务切换路径按核读写, 单实例下**多核并发 EL0 会跨核改写** (入口保存的
//!   `x3`/`x4` 与出口待恢复的用户页表可能被其他核覆盖). 当前 AP 未调度用户任务故
//!   未触发; 拆分方案登记于 `docs/plan/aarch64-smp-bringup.md` 后续项.

#![cfg(target_arch = "aarch64")]

use core::sync::atomic::{AtomicU64, Ordering};

use crate::framework::mm::PAGE_SIZE;
use crate::framework::mm::phys_to_virt;
use crate::framework::mm::pmm_alloc_page;
use crate::framework::mm::virt_to_phys;

// ── 公共状态 ──────────────────────────────────────────────────────

/// KPTI 全局量集合.
///
/// 单独成页 (`repr(align(4096))`) 有两个作用:
/// 1. trampoline 表以**页级最小化**映射本页, 使入口/出口汇编的
///    `adrp KPTI_GLOBALS` 在 tramp 表下可取数;
/// 2. 汇编按固定字节偏移访问各字段, 布局由下方 `offset_of!` 静态断言锁定.
#[repr(C, align(4096))]
pub struct KptiGlobals {
    /// Trampoline TTBR1_EL1 物理地址 (EL0 运行期 TTBR1).
    pub tramp_ttbr1: AtomicU64,
    /// 完整内核 TTBR1_EL1 物理地址 (异常入口切回).
    pub kernel_ttbr1: AtomicU64,
    /// 内核恒等 TTBR0_EL1 物理地址 (异常入口切回).
    pub kernel_ttbr0: AtomicU64,
    /// 当前用户进程 TTBR0_EL1 物理地址 (异常出口 / 进入 EL0 时切回).
    pub user_ttbr0: AtomicU64,
    /// KPTI 是否已初始化 (0 = 未就绪, 1 = 就绪).
    pub ready: AtomicU64,
    /// 入口/出口暂存槽 0: 保住切换序列 clobber 掉的用户 `x4`.
    ///
    /// 入口时刻 `x0-x30` 全是用户态活跃值, 而切表需要 scratch 寄存器; 若无暂存槽
    /// 则被 clobber 的用户值永久丢失 (帧尚未压入内核栈). 出口侧对称使用.
    pub tramp_save0: AtomicU64,
    /// 入口/出口暂存槽 1: 保住切换序列 clobber 掉的用户 `x3`.
    pub tramp_save1: AtomicU64,
}

/// KPTI 全局状态实例. `#[unsafe(no_mangle)]` 供汇编按符号名/`sym` 操作数访问.
// SAFETY: FFI 导出静态量，通过 C ABI 与汇编代码互操作
#[unsafe(no_mangle)]
pub static KPTI_GLOBALS: KptiGlobals = KptiGlobals {
    tramp_ttbr1: AtomicU64::new(0),
    kernel_ttbr1: AtomicU64::new(0),
    kernel_ttbr0: AtomicU64::new(0),
    user_ttbr0: AtomicU64::new(0),
    ready: AtomicU64::new(0),
    tramp_save0: AtomicU64::new(0),
    tramp_save1: AtomicU64::new(0),
};

// 汇编按字面偏移 (0/8/16/24/32/40/48) 访问上述字段; 调整字段顺序/宽度会在此编译失败.
const _: () = assert!(core::mem::offset_of!(KptiGlobals, tramp_ttbr1) == 0);
const _: () = assert!(core::mem::offset_of!(KptiGlobals, kernel_ttbr1) == 8);
const _: () = assert!(core::mem::offset_of!(KptiGlobals, kernel_ttbr0) == 16);
const _: () = assert!(core::mem::offset_of!(KptiGlobals, user_ttbr0) == 24);
const _: () = assert!(core::mem::offset_of!(KptiGlobals, ready) == 32);
const _: () = assert!(core::mem::offset_of!(KptiGlobals, tramp_save0) == 40);
const _: () = assert!(core::mem::offset_of!(KptiGlobals, tramp_save1) == 48);

// 高半区别名基数由 `mm::KERNEL_BASE` 单一提供 (L1-04 收敛: 迁移后二者同值,
// 不再另设 `HIGH_ALIAS_BASE`), 换算统一走 `mm::phys_to_virt` / `mm::virt_to_phys`.

// ── 公开 API ──────────────────────────────────────────────────────

/// 返回 KPTI 是否已就绪.
#[inline(always)]
pub fn kpti_is_active() -> bool {
    KPTI_GLOBALS.ready.load(Ordering::Acquire) != 0
}

/// 记录当前用户进程 TTBR0 物理地址 (由 `enter_user` 在进入 EL0 前调用).
#[inline(always)]
pub fn kpti_set_user_ttbr0(ttbr0: u64) {
    KPTI_GLOBALS.user_ttbr0.store(ttbr0, Ordering::Release);
}

// ── 初始化 ────────────────────────────────────────────────────────

// SAFETY: C ABI 互操作，符号由 link/aarch64.ld 定义
unsafe extern "C" {
    /// 异常向量表段起点 (link/aarch64.ld 的 `.vectors`)
    static _vectors_start: u8;
    /// 异常向量表段终点 (4KB 对齐)
    static _vectors_end: u8;
}

#[expect(
    clippy::missing_panics_doc,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
#[expect(
    clippy::manual_assert,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 初始化 KPTI: 创建**页级最小化**的 trampoline TTBR1 页表.
///
/// trampoline 表 (4 级根) 仅映射 EL0↔EL1 边界真正用到的页 (高半区别名):
/// - `.vectors` 全部页 (异常入口/出口 + 进入 EL0 trampoline 的取指面)
/// - `KPTI_GLOBALS` 所在页 (入口/出口汇编读写的全局量)
///
/// 其余高半区条目为 0 ⇒ 用户态不可见内核代码/数据. TTBR1 由 T1SZ=16 决定硬件
/// **从 level 0 起**遍历, 故 tramp 根必须是 4 级表 (与 `mmu.rs::init_kernel_ttbr1`
/// 同一归因, 勿重犯历史上"指向 L1 形态表"的错误).
///
/// # Safety
///
/// 调用方保证: `kernel_ttbr1` 是当前 TTBR1_EL1 的有效物理地址; PMM 已初始化;
/// KPTI 全局状态在 boot 阶段被独占写入.
///
/// `vmm` 由调用方以 `&self` 传入 (而非经 `get_vmm()`): 本函数在
/// `vmm_init` 的 `OnceLock` 初始化**过程中**被调用, 此刻全局实例尚不可获取.
pub unsafe fn kpti_init(vmm: &super::vmm::Aarch64Vmm, kernel_ttbr1: u64) {
    if kpti_is_active() {
        return;
    }

    // 1. 读取当前 TTBR0_EL1 (mmu::init 建立的内核恒等表)
    let kernel_ttbr0: u64;
    // SAFETY: mrs 只读系统寄存器, 无副作用.
    unsafe {
        core::arch::asm!("mrs {}, ttbr0_el1", out(reg) kernel_ttbr0);
    }

    // 2. 分配并清零 trampoline 4 级根表
    let tramp_l0_phys = pmm_alloc_page() as u64;
    if tramp_l0_phys == 0 {
        // 不可恢复: KPTI 初始化需要 trampoline 根表, 分配失败意味着内存耗尽,
        // 内核无法安全进入用户态, 只能停机
        panic!("[KPTI] failed to allocate trampoline L0 page");
    }
    // SAFETY: pmm 分配的页 4KB 对齐且属内核; phys_to_virt 给出可写内核 VA.
    unsafe {
        core::ptr::write_bytes(
            phys_to_virt(tramp_l0_phys) as *mut u8,
            0,
            PAGE_SIZE as usize,
        );
    }

    // 3. 页级最小化映射 (高半区别名 VA → 同物理页, EL1 RW 可执行, 无 USER 位).
    //    `.vectors` 段与 KPTI_GLOBALS 均链接于高半区 (VMA = PA + KERNEL_BASE),
    //    故映射项的物理地址须经 virt_to_phys 还原; 映射 VA 由同一物理地址经
    //    phys_to_virt 换算, 结果恰为符号自身的高半区地址.
    let mut pa = virt_to_phys(&raw const _vectors_start as u64);
    let vectors_end = virt_to_phys(&raw const _vectors_end as u64);
    while pa < vectors_end {
        vmm.map_page_in_table(
            tramp_l0_phys,
            super::VirtAddr(phys_to_virt(pa)),
            super::PhysAddr(pa),
            super::PageFlags::PRESENT | super::PageFlags::WRITABLE,
        );
        pa += PAGE_SIZE;
    }
    let globals_pa = virt_to_phys(core::ptr::addr_of!(KPTI_GLOBALS) as u64) & !(PAGE_SIZE - 1);
    vmm.map_page_in_table(
        tramp_l0_phys,
        super::VirtAddr(phys_to_virt(globals_pa)),
        super::PhysAddr(globals_pa),
        super::PageFlags::PRESENT | super::PageFlags::WRITABLE,
    );

    // 4. 公开状态 (写入顺序: 先数据后 ready, ready 兼作 Release 屏障)
    KPTI_GLOBALS
        .kernel_ttbr0
        .store(kernel_ttbr0, Ordering::Release);
    KPTI_GLOBALS
        .kernel_ttbr1
        .store(kernel_ttbr1, Ordering::Release);
    KPTI_GLOBALS
        .tramp_ttbr1
        .store(tramp_l0_phys, Ordering::Release);
    KPTI_GLOBALS.ready.store(1, Ordering::Release);
}

/// KPTI 关闭时的占位: 返回 trampoline TTBR1, 未就绪时退回完整内核 TTBR1.
#[inline(always)]
pub fn kpti_trampoline_ttbr1_or_kernel(kernel_ttbr1: u64) -> u64 {
    let t = KPTI_GLOBALS.tramp_ttbr1.load(Ordering::Acquire);
    if t == 0 { kernel_ttbr1 } else { t }
}

// L1-04 收敛: 原本地 `phys_to_virt` / `virt_to_phys` 副本已删除 ——
// 换算唯一入口为 `mm::phys_to_virt` / `mm::virt_to_phys` (文件顶部导入).
