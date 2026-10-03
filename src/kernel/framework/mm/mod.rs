//! 内存管理子系统
//!
//! kernel/mm 的 Rust 重写 (PMM, VMM, Kmalloc)
//! 提供物理内存管理、虚拟内存映射以及内核堆分配, 附带内存安全保证.
//!
//! ## 依赖声明
//!
//! framework 内部依赖: sync, syscall, proc, tests
//! services 依赖: `services::mm` (安全代理)

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU64, Ordering};

// 汇编层定义的用户态 CR3 临时保存 (isr.asm .bss)
// KPTI 开启时, 中断入口在切换到内核页表前将硬件 CR3 (用户页表) 写入此变量.
// host-test 下 isr.asm 无产物且 read_user_cr3_asm 桩化, 声明一并排除 (符号契约归零).
#[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    #[link_name = "USER_CR3_SAVE"]
    static USER_CR3_SAVE_ASM: AtomicU64;
}

// 读取汇编层保存的用户 CR3 (page fault handler 使用)
// x86_64: 从 isr.asm .bss 中的 USER_CR3_SAVE 读取, 汇编在 KPTI 切换前写入.
// aarch64: 回退到硬件 CR3 (aarch64 KPTI 实现不同).
pub fn read_user_cr3_asm() -> u64 {
    // 符号桩化 (host-test): host 无 isr.asm 的 USER_CR3_SAVE 符号且不执行中断
    // 上下文读取, 常量中性返回 0 (与 E-04 cpu_id 桩同模式).
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        // SAFETY: USER_CR3_SAVE 在 isr.asm .bss 中定义, 汇编入口在切换 CR3 前写入;
        // 读取发生在中断上下文, 无竞争.
        unsafe { USER_CR3_SAVE_ASM.load(Ordering::Acquire) }
    }
    #[cfg(all(target_arch = "x86_64", feature = "host-test"))]
    {
        0
    }
    #[cfg(target_arch = "aarch64")]
    {
        vmm::get_current_pml4()
    }
}

/// 将 `Copy` POD 值视为只读字节视图.
///
/// 供 services 层零 unsafe 序列化 POD 结构 (如写入 fd) 使用. `T: Copy`
/// 保证无 `Drop` / 无内部可变别名, 按位读取安全.
pub fn pod_as_bytes<T: Copy>(val: &T) -> &[u8] {
    // SAFETY: val 是有效 `&T`; `size_of::<T>()` 字节完全落在 val 内存范围内;
    // `from_raw_parts` 仅构造只读 u8 视图 (对齐要求 u8=1 恒满足), 不越界.
    unsafe {
        core::slice::from_raw_parts(
            core::ptr::from_ref(val).cast::<u8>(),
            core::mem::size_of::<T>(),
        )
    }
}

pub mod pmm;

#[cfg(target_arch = "x86_64")]
#[path = "vmm_x86_64.rs"]
pub mod vmm;

#[cfg(target_arch = "aarch64")]
#[path = "vmm_aarch64.rs"]
pub mod vmm;

/// T-02: 物理页帧分配决策 trait
pub mod alloc_trait;
pub mod api;
pub mod arch;
pub mod copy_user;
pub mod cow;
pub mod cross_process;
/// 跨架构共用的 TLB shootdown 延迟释放机制 (x86_64 与 aarch64 共用, 禁平行实现)
pub(crate) mod deferred_free;
pub mod frame;
pub mod kmalloc;
/// 惰性页池 — 按需分配 4KB 物理页的后备存储 (替代子系统级静态容量数组)
pub mod lazy_arena;
/// L-03: 机制 API 集中导出 — 供 services 层策略实现调用
pub mod mechanism;
/// D3: NUMA 拓扑感知与内存策略
pub mod numa;
pub mod page_fault;
pub mod page_fault_policy;
pub mod pcache;
/// T2-2: PMM 策略决策 trait (阶数选择/碎片化/水位线)
pub mod pmm_trait;
/// DECISION-O ②: 内存压力机制 (类型/状态/update_pressure 包装 + 分级策略注册口)
pub mod pressure;
pub mod slab;
/// T2-3: Slab 策略决策 trait (缓存大小选择/对象数计算/分配优先级)
pub mod slab_trait;
pub mod swap;
/// T2-4: Swap 策略决策 trait (LRU 管理/回收决策/kswapd 触发)
pub mod swap_trait;
/// T1 G4: userfaultfd 机制 (#PF 拦截 + 事件队列 + 用户态提供页)
pub mod uffd;
pub mod vma;

#[cfg(target_arch = "x86_64")]
pub mod kpti;
#[cfg(target_arch = "aarch64")]
#[path = "kpti_aarch64.rs"]
pub mod kpti;

// 重新导出常用类型
pub use kmalloc::*;
pub use lazy_arena::LazyPageArena;
pub(crate) use lazy_arena::alloc_zeroed_page_as;
pub use pmm::*;
pub use vmm::*;

// api 公共接口 re-export — 避免跨子系统直接访问 mm::api 内部
// 注意: 不使用 glob re-export 因与 vmm::* 有名称冲突 (vmm_init 等)
pub use api::{
    KmallocStats, PageFaultInfo, PfResult, VmaInfo, copy_from_user, copy_from_user_in_mm,
    copy_to_user, copy_to_user_in_mm, handle_page_fault, handle_user_page_fault, is_user_buf,
    k_free, k_malloc, kfree, kmalloc_stats, pmm_alloc_huge_page, pmm_alloc_huge_page_phys,
    pmm_alloc_page, pmm_alloc_page_phys, pmm_alloc_pages, pmm_alloc_pages_phys, pmm_dump_stats,
    pmm_free_huge_page, pmm_free_page, pmm_free_page_phys, pmm_free_pages, pmm_free_pages_phys,
    pmm_get_free_pages, pmm_get_total_pages, pmm_get_used_pages, pmm_init, pmm_init_bitmap,
    pmm_is_aligned_for_huge, vma_get_current_mm, vma_set_current_mm, vma_snapshot_current,
    vmm_clone_user_page_table_cow, vmm_destroy_page_table, vmm_switch_page_table,
};

// vma 公共类型 re-export — 避免跨子系统直接访问 mm::vma 内部
pub use vma::{MmStruct, Vma, VmaType};

// swap 公共接口 re-export — 避免跨子系统直接访问 mm::swap 内部
pub use swap::{kswapd_wakeup, set_page_locked};

// kpti 公共接口 re-export — 避免跨子系统直接访问 mm::kpti 内部
#[cfg(target_arch = "aarch64")]
pub use kpti::kpti_trampoline_ttbr1_or_kernel;
// x86_64 侧对应接口: 把任务内核栈顶页映射进该进程用户页表 (KPTI-08)
#[cfg(target_arch = "x86_64")]
pub use kpti::map_rsp0_page;

// KPTI 方案 S3 公共接口 re-export (aarch64 专属) — fork 路径补建子进程 EL1 视图
#[cfg(target_arch = "aarch64")]
pub use api::vmm_build_el1_view;

// alloc_trait 公共接口 re-export — T-02 策略-机制分离
pub use alloc_trait::{
    AllocContext, AllocDecision, FallbackAllocPolicy, FrameAllocDecision, current_alloc_decision,
    register_alloc_decision,
};

// pmm_trait 公共接口 re-export — T2-2 PMM 策略-机制分离
pub use pmm_trait::{
    FallbackPmmPolicy, PmmPolicy, PmmPolicyContext, Watermarks, current_pmm_policy,
    register_pmm_policy,
};

// slab_trait 公共接口 re-export — T2-3 Slab 策略-机制分离
pub use slab_trait::{
    FallbackSlabPolicy, SlabAllocSource, SlabPolicy, SlabPolicyContext, current_slab_policy,
    register_slab_policy,
};

// swap_trait 公共接口 re-export — T2-4 Swap 策略-机制分离
pub use swap_trait::{
    FallbackSwapPolicy, LruPageInfo, SwapPolicy, SwapPolicyContext, current_swap_policy,
    register_swap_policy,
};

// page_fault_policy 公共接口 re-export — 缺页策略-机制分离
pub use page_fault_policy::{
    FallbackPageFaultPolicy, PageFaultPolicy, current_page_fault_policy, register_page_fault_policy,
};

// cow 模块的计数入口已收敛到 PMM 帧持有计数面 (`pmm::frame_inc` / `frame_dec` /
// `frame_ref_count`), 原 `cow_inc_ref` / `cow_dec_ref` / `cow_init` / `cow_ref_count`
// re-export 随 `COW_REFS` 一并删除 (见 docs/plan/cr3-lifetime-ownership.md §8.1)

// uffd 公共接口 re-export — T1 G4 userfaultfd 机制 (services 侧 ABI 层消费)
pub use uffd::{
    UffdFaultOutcome, UffdIoApi, UffdIoCopy, UffdIoRange, UffdIoRegister, UffdIoZeropage,
    UffdMsgPagefault, api_negotiate, clear_reader, create as uffd_create, fault_notify,
    fill_provided_page, is_active as uffd_is_active, is_open as uffd_is_open, is_uffd_fd,
    pop_event, provide_page, register as uffd_register, release as uffd_release, set_reader,
    unregister as uffd_unregister, wake as uffd_wake,
};

// DECOUPL-4: 顶层 re-export NUMA 初始化入口, 避免 framework 内部 3+ 层深度访问
pub use numa::numa_init;

// mechanism 模块供 services 层通过 framework::mm::mechanism::* 访问机制 API
// 不使用 glob re-export 因与现有 api re-export 产生歧义

/// Page size and huge-page constants (统一从 config.rs 引用)
pub use crate::framework::config::{
    HUGE_PAGE_1G_SHIFT, HUGE_PAGE_1G_SIZE, HUGE_PAGE_2M_SHIFT, HUGE_PAGE_2M_SIZE, PAGE_SHIFT,
    PAGE_SIZE,
};

/// 内存布局常量
/// `x86_64`: 高半核映射 (`0xFFFF_8000_0000_0000`)
/// aarch64: 高半区内核区 (`0xFFFF_0000_0000_0000`, 内核区 VMA = LMA + 本常量)
#[cfg(target_arch = "x86_64")]
pub const KERNEL_BASE: u64 = 0xFFFF800000000000u64;
#[cfg(target_arch = "aarch64")]
pub const KERNEL_BASE: u64 = 0xFFFF000000000000u64;
pub const PHYSICAL_BASE: u64 = 0x0000000000000000u64;

/// 用户空间低地址保护阈值.
/// 低于此地址的指针视为空指针/不可解引用区域, 用于 canary 校验、
/// 地址合法性检查等场景.
/// `x86_64`: Linux 习惯 0x1000 (4 KiB), 覆盖 NULL 页 + 少量保留区.
/// aarch64: 同 0x1000.
pub const USER_ADDR_FLOOR: u64 = 0x1000;

/// 用户空间低地址上限 (`x86_64` 兼容阈值).
/// 低于此地址的 RIP/故障地址视为空指针区域, 用于异常处理中的
/// 地址分类 (空指针解引用 vs 用户有效地址).
/// `x86_64`: 0xFFFF (覆盖 null descriptor + 低 64 KiB 保留区).
/// aarch64: 0xFFFF (与 `x86_64` 一致).
#[cfg(target_arch = "x86_64")]
pub const USER_ADDR_MIN: u64 = 0xFFFF;
#[cfg(target_arch = "aarch64")]
pub const USER_ADDR_MIN: u64 = 0xFFFF;

/// 缓存行大小 (字节). `x86_64` 与 aarch64 通用值为 64.
/// 用于 DMA 缓存刷写对齐、false sharing 避免等场景.
pub const CACHE_LINE_SIZE: u64 = 64;

/// 运行期跨核 TLB 失效探针的专用虚拟地址 (用户半区 `PML4[255]` 空闲槽).
///
/// 选址依据: 用户半区 (`PML4[0..255]`) 不参与 KPTI 共享 (KPTI 仅复制高半区
/// `PML4[256..512]`), 且引导期只有 `PML4[0]` 与 `PML4[256]` 被 boot 建立
/// (低 4 GiB 恒等映射), 故 `PML4[255]` 可整槽新建 / 整槽拆除而不扰动既有映射.
/// 该页在探针未装备时无映射, 装备前不得访问 (见 `framework::smp::tlb_probe_report`).
pub const TLB_PROBE_VA: u64 = 0x0000_7F80_0000_0000;

/// 内核文本段基址 (符号地址).
/// `x86_64`: 高半核最高 2GB 区域 (-2GB 符号地址), 用于内核代码绝对寻址
///   和 RIP/地址分类 (内核文本段 vs 直接映射区).
/// aarch64: 内核镜像装载基址 (LMA 0x40080000) 的高半区地址.
#[cfg(target_arch = "x86_64")]
pub const KERNEL_TEXT_BASE: u64 = 0xFFFFFFFF80000000;
#[cfg(target_arch = "aarch64")]
pub const KERNEL_TEXT_BASE: u64 = KERNEL_BASE + 0x4008_0000;

/// 页表项标志 (与 C 定义一致)
pub const PAGE_PRESENT: u64 = 1 << 0;
pub const PAGE_WRITABLE: u64 = 1 << 1;
pub const PAGE_USER: u64 = 1 << 2;
pub const PAGE_HUGE: u64 = 1 << 7; // 大页标志 (huge page)
pub const PAGE_NX: u64 = 1u64 << 63;

/// 裸 `u64` 页表项是否为**用户 leaf** —— 帧持有计数面的唯一判据.
///
/// fork 的 +1 侧 (`cow::clone_user_page_table_cow_inner`) 与拆除的 −1 侧
/// (`vmm::unmap_page_in_table` / `vmm::destroy_page_table`) 必须**同集**, 否则会把
/// 仍在映射的帧计入零而误释放 (UAF). 置于父模块是为了让两处共用同一实现 ——
/// `vmm` 反向依赖 `cow` 会构成模块环依赖 (违反 F3).
///
/// 类型化封装 `PageTableEntry::is_user` 只适用于 `x86_64` 的 PTE 结构, 裸 `u64`
/// 判据必须按架构分派位布局:
/// - `x86_64`: bit2 为用户位 (USER).
/// - aarch64: bits[1:0] = 0b11 页描述符, bit6 = AP[1] (EL0 可访问).
///   aarch64 的 bit2 是 MAIR 属性索引位, 裸用 `PAGE_USER` 会误判.
#[inline]
pub(crate) fn is_user_leaf(entry: u64) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        entry & PAGE_USER != 0
    }
    #[cfg(target_arch = "aarch64")]
    {
        entry & 0b11 == 0b11 && entry & (1 << 6) != 0
    }
}

/// 单级页表翻译结果: 目标虚拟地址对应的物理地址 + 叶子项是否可写.
///
/// 供跨进程用户内存代理 (`mm::cross_process`) 判定目标页的写权限:
/// 读方向只需 `phys`, 写方向必须额外校验 `writable` (只读页拒绝写入).
/// `writable` 取自叶子表项 (4KB PTE / 2MB 或 1GB 大页) 的写位, 各架构布局不同,
/// 由 `vmm::translate_in_pml4` 按架构填值.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTranslation {
    /// 翻译得到的物理地址 (含页内偏移)
    pub phys: PhysAddr,
    /// 叶子表项是否允许写 (x86_64: PTE.W; aarch64: `AP[2] == 0`)
    pub writable: bool,
}

/// 归还一个物理帧 —— **要求调用方已持 `VMM_LOCK`**
///
/// 「帧计数归零后何时真正归还物理页」的**唯一语义点** (mm 子系统共用此入口,
/// 不再在调用点重复架构分派): 统一走延迟释放 [`deferred_free::defer_free`] ——
/// 把帧挂入批次链, 等全部在线核的 TLB 代追平后才真正归还 PMM.
///
/// 原因: 归零不等于「远端核 TLB 已失效」—— 他核若曾运行该地址空间, 其 TLB 仍
/// 缓存旧映射, 立即归还的帧被重分配后会经陈旧映射访问他人物理页 (UAF).
/// x86_64 需靠定向 IPI 让对端重载 CR3; aarch64 的广播失效 (`tlbi vaae1is`) 虽已
/// 覆盖跨核, 但仍需对端执行上下文同步 (`dsb`) 并声明追平代, 页表页方可安全回收.
/// 故两架构共用同一延迟释放实现 (禁平行实现); `defer_free` 内部以本帧前 16 字节
/// 为链节点, 故**同一帧只能入链一次**.
///
/// 锁序约束: 调用方另持的锁必须在 `VMM_LOCK` **之前**取得 (如 `VMA_LOCK`);
/// **不得在持有 pcache 桶锁时调用** —— 既定锁序为 `VMA_LOCK → VMM_LOCK → 桶锁`,
/// 桶锁内调用构成反向嵌套.
pub(crate) fn release_frame_locked(phys: PhysAddr) {
    deferred_free::defer_free(phys.0);
}

/// 归还一个物理帧 —— **自持 `VMM_LOCK` 变体**
///
/// 语义与锁序约束见 [`release_frame_locked`]; 本入口只负责加/解锁, 供不持
/// `VMM_LOCK` 的路径 (缺页换叶 / COW / 页缓存条目释放) 使用. 已持锁的路径必须
/// 直接用 `release_frame_locked`, 否则构成递归加锁.
pub(crate) fn release_frame(phys: PhysAddr) {
    let vmm_inst = vmm::get_vmm();
    let lock_flags = vmm_inst.acquire_lock();
    release_frame_locked(phys);
    vmm_inst.release_lock(&lock_flags);
}

/// 页表索引辅助宏
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub const fn pml4_index(addr: u64) -> usize {
    ((addr >> 39) & 0x1FF) as usize
}

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub const fn pdpt_index(addr: u64) -> usize {
    ((addr >> 30) & 0x1FF) as usize
}

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub const fn pd_index(addr: u64) -> usize {
    ((addr >> 21) & 0x1FF) as usize
}

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub const fn pt_index(addr: u64) -> usize {
    ((addr >> 12) & 0x1FF) as usize
}

/// 物理地址转虚拟地址 (内核空间)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub const fn phys_to_virt(phys: u64) -> u64 {
    phys + KERNEL_BASE
}

/// 虚拟地址转物理地址 (内核空间)
#[inline(always)]
pub const fn virt_to_phys(virt: u64) -> u64 {
    virt - KERNEL_BASE
}

/// Page size type enum
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PageSize {
    Size4K = 0, // Standard 4KB page
    Size2M = 1, // 2MB huge page
    Size1G = 2, // 1GB giant page
}

impl PageSize {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn size(&self) -> u64 {
        match self {
            Self::Size4K => PAGE_SIZE,
            Self::Size2M => HUGE_PAGE_2M_SIZE,
            Self::Size1G => HUGE_PAGE_1G_SIZE,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn shift(&self) -> u64 {
        match self {
            Self::Size4K => PAGE_SHIFT,
            Self::Size2M => HUGE_PAGE_2M_SHIFT,
            Self::Size1G => HUGE_PAGE_1G_SHIFT,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    /// 检查地址是否按当前页大小正确对齐
    pub fn is_aligned(&self, addr: u64) -> bool {
        let mask = self.size() - 1;
        (addr & mask) == 0
    }
}

/// 内存信息结构 (与 C 结构体一致)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryInfo {
    pub total_pages: u64,
    pub free_pages: u64,
    pub used_pages: u64,
    pub kernel_end: u64,
}

impl Default for MemoryInfo {
    fn default() -> Self {
        Self {
            total_pages: 0,
            free_pages: 0,
            used_pages: 0,
            kernel_end: 0,
        }
    }
}

impl MemoryInfo {
    pub const fn const_default() -> Self {
        Self {
            total_pages: 0,
            free_pages: 0,
            used_pages: 0,
            kernel_end: 0,
        }
    }
}

/// Physical address wrapper for type safety
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub u64);

impl PhysAddr {
    pub fn new(addr: u64) -> Self {
        Self(addr)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    /// 向上对齐到页边界
    #[inline(always)]
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn align_up(&self, align: u64) -> Self {
        Self((self.0 + align - 1) & !(align - 1))
    }

    /// 向下对齐到页边界
    #[inline(always)]
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn align_down(&self, align: u64) -> Self {
        Self(self.0 & !(align - 1))
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 转为内核空间虚拟地址
    pub fn to_virt(&self) -> VirtAddr {
        VirtAddr(phys_to_virt(self.0))
    }
}

/// 虚拟地址 (类型安全包装)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtAddr(pub u64);

impl VirtAddr {
    pub fn new(addr: u64) -> Self {
        Self(addr)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    /// 向上对齐到页边界
    #[inline(always)]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn align_up(&self, align: u64) -> Self {
        Self((self.0 + align - 1) & !(align - 1))
    }

    /// 向下对齐到页边界
    #[inline(always)]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub fn align_down(&self, align: u64) -> Self {
        Self(self.0 & !(align - 1))
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 转为物理地址 (假定为内核空间)
    pub fn to_phys(&self) -> PhysAddr {
        PhysAddr(virt_to_phys(self.0))
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取该地址的 PML4 索引
    pub fn pml4_idx(&self) -> usize {
        pml4_index(self.0)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取该地址的 PDPT 索引
    pub fn pdpt_idx(&self) -> usize {
        pdpt_index(self.0)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取该地址的 PD 索引
    pub fn pd_idx(&self) -> usize {
        pd_index(self.0)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    /// 获取该地址的 PT 索引
    pub fn pt_idx(&self) -> usize {
        pt_index(self.0)
    }
}

// 页表项标志 (bitflags 风格)
bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PageFlags: u64 {
        const PRESENT     = 1 << 0;
        const WRITABLE    = 1 << 1;
        const USER        = 1 << 2;
        const WRITE_THROUGH = 1 << 3;
        const CACHE_DISABLE = 1 << 4;
        const ACCESSED    = 1 << 5;
        const DIRTY       = 1 << 6;
        const HUGE_PAGE   = 1 << 7;
        const GLOBAL      = 1 << 8;
        const NX          = 1u64 << 63;
    }
}

impl Default for PageFlags {
    fn default() -> Self {
        Self::PRESENT | Self::WRITABLE
    }
}

/// 页表项结构 (与 C 位域布局一致)
#[repr(C)]
pub struct PageTableEntry {
    bits: AtomicU64,
}

impl PageTableEntry {
    pub fn new() -> Self {
        Self {
            bits: AtomicU64::new(0),
        }
    }

    /// 从裸值创建
    pub fn from_value(value: u64) -> Self {
        Self {
            bits: AtomicU64::new(value),
        }
    }

    /// 返回页表项原始值.
    pub fn value(&self) -> u64 {
        self.bits.load(Ordering::Acquire)
    }

    /// 设置页表项原始值.
    pub fn set_value(&self, value: u64) {
        self.bits.store(value, Ordering::Release);
    }

    /// Is present?
    pub fn is_present(&self) -> bool {
        self.bits.load(Ordering::Acquire) & PAGE_PRESENT != 0
    }

    /// 设置 Present 标志位.
    pub fn set_present(&self, present: bool) {
        let mut val = self.bits.load(Ordering::Acquire);
        if present {
            val |= PAGE_PRESENT;
        } else {
            val &= !PAGE_PRESENT;
        }
        self.bits.store(val, Ordering::Release);
    }

    /// Is writable?
    pub fn is_writable(&self) -> bool {
        self.bits.load(Ordering::Acquire) & PAGE_WRITABLE != 0
    }

    /// 设置 Writable 标志位.
    pub fn set_writable(&self, writable: bool) {
        let mut val = self.bits.load(Ordering::Acquire);
        if writable {
            val |= PAGE_WRITABLE;
        } else {
            val &= !PAGE_WRITABLE;
        }
        self.bits.store(val, Ordering::Release);
    }

    /// Is user accessible?
    pub fn is_user(&self) -> bool {
        self.bits.load(Ordering::Acquire) & PAGE_USER != 0
    }

    /// 设置 User 标志位.
    pub fn set_user(&self, user: bool) {
        let mut val = self.bits.load(Ordering::Acquire);
        if user {
            val |= PAGE_USER;
        } else {
            val &= !PAGE_USER;
        }
        self.bits.store(val, Ordering::Release);
    }

    /// Is huge page?
    pub fn is_huge(&self) -> bool {
        self.bits.load(Ordering::Acquire) & PAGE_HUGE != 0
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 返回帧地址 (页的物理地址).
    pub fn frame(&self) -> PhysAddr {
        PhysAddr(self.bits.load(Ordering::Acquire) & 0x000FFFFFFFFFF000)
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 设置帧地址.
    pub fn set_frame(&self, frame: PhysAddr) {
        let mut val = self.bits.load(Ordering::Acquire);
        val = (val & !0x000FFFFFFFFFF000) | (frame.0 & 0x000FFFFFFFFFF000);
        self.bits.store(val, Ordering::Release);
    }

    /// 返回所有标志位的 `PageFlags` 位掩码.
    pub fn flags(&self) -> PageFlags {
        PageFlags::from_bits_truncate(self.bits.load(Ordering::Acquire))
    }

    /// 从 `PageFlags` 位掩码设置所有标志位.
    pub fn set_flags(&self, flags: PageFlags) {
        let mut val = self.bits.load(Ordering::Acquire);
        val = (val & !(PageFlags::all().bits())) | flags.bits();
        self.bits.store(val, Ordering::Release);
    }
}

impl Default for PageTableEntry {
    fn default() -> Self {
        Self::new()
    }
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 将帧缓冲物理地址映射到内核虚拟地址空间
///
/// 帧缓冲位于 PCI MMIO 区域（高位物理地址），启动页表的恒等映射
/// 仅覆盖低 1GB 物理内存。此函数通过 VMM 动态建立 2MB 大页映射，
/// 确保帧缓冲可被内核访问。
///
/// G1 阶段暂不配置 Write-Combining 缓存模式 (通过 PAT)，
/// 这不会阻止像素正常显示；WC 优化将在 G3 阶段添加。
pub fn map_framebuffer(phys_addr: u64, size: u64) -> *mut u8 {
    use crate::framework::mm::get_vmm;

    let vmm = get_vmm();

    let page_2m: u64 = 0x200000;
    let start_page = phys_addr & !(page_2m - 1);
    let end = phys_addr + size;
    let end_page = (end + page_2m - 1) & !(page_2m - 1);

    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::GLOBAL;

    let mut pa = start_page;
    while pa < end_page {
        let va = phys_to_virt(pa);
        match vmm.map_huge_page(VirtAddr(va), PhysAddr(pa), flags, PageSize::Size2M) {
            Ok(()) => {}
            Err(e) => {
                crate::klog_boot_info!(
                    "[MM] map_framebuffer: FAILED pa={:#X} va={:#X} err={}",
                    pa,
                    va,
                    e
                );
            }
        }
        pa += page_2m;
    }

    phys_to_virt(phys_addr) as *mut u8
}
