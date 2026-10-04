//! 内存管理子系统 API 层
//!
//! 为内核其它模块提供统一的内存分配/释放/映射接口。
//! 所有公开函数都使用 `#[no_mangle]` 以保证符号名稳定,方便跨模块直接调用。
//!
//! ## 调用方契约
//! - `proc::api` —— 进程创建/销毁时的页表操作 (`vmm_map_page` / `vmm_unmap_page`)
//! - `crate::framework::proc::elf` —— ELF 加载时的 COW 页表克隆 (`vmm_clone_user_page_table_cow`)
//! - `fs::ramfs` / `fs::unkfs` —— 文件系统页缓存分配 (`pmm_alloc_page` / `pmm_free_page`)
//! - `ipc::shm` —— 共享内存段的物理页映射
//! - `driver::*` —— 各驱动的 DMA 缓冲区分配
//!
//! ## 安全约束
//! - 所有指针参数在函数入口处做 `is_null` 检查
//! - `pmm_alloc_*` 返回 null 时调用方必须处理 OOM
//! - `vmm_map_page` 不检查地址冲突,调用方负责确保不重复映射
//! - 物理页分配器 (PM) 由 spinlock 保护,可在中断上下文调用
//! - 页表操作 (VMM) 不可在中断上下文调用 (需要锁)
//!
//! ## 性能特征
//! - PM 分配: 位图扫描 O(N), Buddy 优化后 O(log N)
//! - Slab 分配: O(1) 缓存命中, 无锁 per-CPU 缓存
//! - VMM 映射: 四级页表遍历 O(4), 常数时间
//!
//! 设计目标:
//! - 隐藏内部实现细节(pmm/vmm/slab)
//! - 提供纯 Rust 抽象,无 C ABI 依赖
//! - 异常路径:空指针 / 越界 / 内存不足一律返回 0 / -1 / null,调用方按需检查

use super::{
    PageFlags, PageSize, PhysAddr, VirtAddr, get_kmalloc, get_kmalloc_mut, get_pmm, get_vmm,
};
use alloc::vec::Vec;
use core::sync::atomic::AtomicU64;

/// 内核 malloc 统计结构 (C 兼容)
#[repr(C)]
pub struct KmallocStats {
    pub total_allocs: u64,
    pub total_frees: u64,
    pub current_usage: u64,
    pub peak_usage: u64,
}

// ============================================================
// PMM FFI 函数
// ============================================================

/// 初始化物理内存管理器
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_init(mem_size: u64, kernel_end: u64) {
    super::pmm::pmm_init(mem_size, kernel_end);
}

/// 初始化位图以进行常规操作
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_init_bitmap(reserved_after_kernel: u64) {
    super::pmm::pmm_init_bitmap(reserved_after_kernel);
}

/// 分配单个 4KB 页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_alloc_page() -> *mut u8 {
    get_pmm()
        .alloc_page()
        .map_or(core::ptr::null_mut(), |addr| addr.0 as *mut u8)
}

/// 释放单个页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_free_page(addr: *mut u8) {
    if !addr.is_null() {
        get_pmm().free_page(PhysAddr(addr as u64));
    }
}

/// 获取空闲页数量
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_get_free_pages() -> u64 {
    get_pmm().get_free_pages()
}

/// 获取总页数
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_get_total_pages() -> u64 {
    get_pmm().get_total_pages()
}

/// 获取已用页数
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_get_used_pages() -> u64 {
    get_pmm().get_used_pages()
}

/// 分配多个连续页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_alloc_pages(count: usize) -> *mut u8 {
    get_pmm()
        .alloc_pages(count)
        .map_or(core::ptr::null_mut(), |addr| addr.0 as *mut u8)
}

/// 释放多个连续页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_free_pages(addr: *mut u8, count: usize) {
    if !addr.is_null() && count > 0 {
        get_pmm().free_pages(PhysAddr(addr as u64), count);
    }
}

/// 分配单个 4KB 页, 返回物理地址.
///
/// 供需要 `PhysAddr` 类型安全的调用方使用.
pub fn pmm_alloc_page_phys() -> Option<super::PhysAddr> {
    get_pmm().alloc_page()
}

/// 释放单个页 (物理地址).
pub fn pmm_free_page_phys(addr: super::PhysAddr) {
    get_pmm().free_page(addr);
}

/// 分配多个连续页, 返回物理地址.
///
/// 供需要 `PhysAddr` 类型安全的调用方使用.
pub fn pmm_alloc_pages_phys(count: usize) -> Option<super::PhysAddr> {
    get_pmm().alloc_pages(count)
}

/// 释放多个连续页 (物理地址).
pub fn pmm_free_pages_phys(addr: super::PhysAddr, count: usize) {
    get_pmm().free_pages(addr, count);
}

/// 登记一个帧持有者 (共享语义), 契约见 `PhysicalMemoryManager::frame_inc`.
///
/// 返回 `true` = 计数已 +1; `false` = 帧不处于计数态 (未计数块 / MMIO / pfn 越界),
/// 调用方按 fail-closed 处理 (不得当作登记成功).
pub fn frame_inc(addr: super::PhysAddr) -> bool {
    get_pmm().frame_inc(addr)
}

/// 注销一个帧持有者, 契约见 `PhysicalMemoryManager::frame_dec`.
///
/// 返回 `true` = 计数归零 (调用方据此归还物理帧), `false` = 仍有持有者或未计数帧.
pub fn frame_dec(addr: super::PhysAddr) -> bool {
    get_pmm().frame_dec(addr)
}

/// 观测帧持有者数 (0 = 未计数帧), 契约见 `PhysicalMemoryManager::frame_ref_count`.
pub fn frame_ref_count(addr: super::PhysAddr) -> u32 {
    u32::from(get_pmm().frame_ref_count(addr))
}

/// 分配一个大页 (2MB 或 1GB), 返回物理地址.
pub fn pmm_alloc_huge_page_phys(size_type: super::PageSize) -> Option<super::PhysAddr> {
    get_pmm().alloc_huge_page(size_type)
}

/// 打印 PMM 统计信息
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_dump_stats() {
    get_pmm().dump_stats();
}

/// Slab 分配器统计信息
pub struct SlabStats {
    /// 总分配内存 (字节)
    pub total_memory: u64,
    /// 已使用内存 (字节)
    pub used_memory: u64,
    /// 缓存数量
    pub total_caches: u32,
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
/// 获取 slab 分配器系统级统计
pub fn slab_get_stats() -> SlabStats {
    let mut total_memory = 0u64;
    let mut used_memory = 0u64;
    let mut total_caches = 0u32;

    super::slab::slab_get_system_stats(&mut total_memory, &mut used_memory, &mut total_caches);

    SlabStats {
        total_memory,
        used_memory,
        total_caches,
    }
}

/// 单个 slab 缓存信息
#[derive(Debug, Clone, Copy)]
pub struct SlabCacheInfo {
    /// 对象大小 (字节)
    pub object_size: u32,
    /// 总对象数
    pub total_objects: u32,
    /// 已用对象数
    pub active_objects: u32,
    /// 总 slab 页数
    pub total_slabs: u32,
}

/// 获取所有通用 slab 缓存的逐项信息.
/// `out` 由调用方提供, 最大写入 `out.len()` 项. 返回实际写入数.
pub fn slab_get_cache_infos(out: &mut [SlabCacheInfo]) -> usize {
    // 内部使用 slab 模块的快照函数, 避免直接访问 private static mut
    let mut snapshots = [super::slab::SlabCacheSnapshot {
        object_size: 0,
        total_objects: 0,
        active_objects: 0,
        total_slabs: 0,
    }; 16];
    let count = super::slab::get_all_cache_snapshots(&mut snapshots);
    let n = count.min(out.len());
    for i in 0..n {
        out[i] = SlabCacheInfo {
            object_size: snapshots[i].object_size,
            total_objects: snapshots[i].total_objects,
            active_objects: snapshots[i].active_objects,
            total_slabs: snapshots[i].total_slabs,
        };
    }
    n
}

/// 分配一个大页 (2MB 或 1GB)
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_alloc_huge_page(size_type: PageSize) -> *mut u8 {
    get_pmm()
        .alloc_huge_page(size_type)
        .map_or(core::ptr::null_mut(), |addr| addr.0 as *mut u8)
}

/// 释放一个大页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_free_huge_page(addr: *mut u8, size_type: PageSize) {
    if !addr.is_null() {
        get_pmm().free_huge_page(PhysAddr(addr as u64), size_type);
    }
}

/// 检查大页对齐
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn pmm_is_aligned_for_huge(addr: *const u8, size_type: PageSize) -> i32 {
    if addr.is_null() {
        return 0;
    }

    i32::from(get_pmm().is_aligned_for_huge(PhysAddr(addr as u64), size_type))
}

// ============================================================
// VMM FFI 函数
// ============================================================

/// 初始化虚拟内存管理器
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_init() {
    super::vmm::vmm_init();
}

/// 将虚拟页映射到物理页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_map_page(virt: u64, phys: u64, flags: u64) -> i32 {
    let virt_addr = VirtAddr(virt);
    let phys_addr = PhysAddr(phys);
    let page_flags = PageFlags::from_bits_truncate(flags);

    match get_vmm().map_page(virt_addr, phys_addr, page_flags) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// 映射一个大页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_map_huge_page(virt: u64, phys: u64, flags: u64, size_type: PageSize) -> i32 {
    let virt_addr = VirtAddr(virt);
    let phys_addr = PhysAddr(phys);
    let page_flags = PageFlags::from_bits_truncate(flags);

    match get_vmm().map_huge_page(virt_addr, phys_addr, page_flags, size_type) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// 解除虚拟页映射
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_unmap_page(virt: u64) {
    get_vmm().unmap_page(VirtAddr(virt));
}

/// 将 2MB 大页拆分为 512 个 4KB 页
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_split_2mb_page(virt: u64) -> i32 {
    match get_vmm().split_2mb_page(virt) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// 为虚拟地址对应的 PML4 项设置 USER 标志
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_ensure_pml4_user(virt: u64) {
    get_vmm().ensure_pml4_user(virt);
}

/// 为路径上所有页表项设置 USER 标志, 以允许用户态访问
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_ensure_path_user(virt: u64) {
    get_vmm().ensure_path_user(virt);
}

/// 获取虚拟地址对应的物理地址
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_get_physical(virt: u64) -> u64 {
    get_vmm()
        .get_physical(VirtAddr(virt))
        .map_or(0, |phys| phys.as_u64())
}

/// 在指定页表上下文中获取物理地址
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_get_physical_in_table(pml4: u64, virt: u64) -> u64 {
    get_vmm()
        .get_physical_in_pml4(pml4, VirtAddr(virt))
        .map_or(0, |phys| phys.as_u64())
}

/// 切换到其它页表 (加载 CR3)
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_switch_page_table(cr3: u64) {
    get_vmm().switch_page_table(cr3);
}

/// 创建用户态页表
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_create_user_page_table() -> u64 {
    get_vmm().create_user_page_table().unwrap_or(0)
}

/// 为 `user_root` 页表建立配套的 EL1 视图 (KPTI 方案 S3, aarch64 专属)。
///
/// `create_user_page_table` 已自行建视图; 本入口供 **fork 路径** 使用 —— 子进程
/// 页表由 COW 克隆产生 (只搬用户半区, 保留槽位 `[1:0] = 00` 被过滤), 需补建视图,
/// 否则子进程 EL0→EL1 入口只能回退到全局内核表, 内核态将无法访问其用户页。
///
/// 返回视图根表物理地址; 失败返回 `None`, 调用方须按 fail-closed 处理。
#[cfg(target_arch = "aarch64")]
pub fn vmm_build_el1_view(user_root: u64) -> Option<u64> {
    get_vmm().build_el1_view(user_root)
}

/// 在指定页表中映射 (用于用户态)
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_map_page_in_table(pml4: u64, virt: u64, phys: u64, flags: u64) {
    let virt_addr = VirtAddr(virt);
    let phys_addr = PhysAddr(phys);
    let page_flags = PageFlags::from_bits_truncate(flags);

    get_vmm().map_page_in_table(pml4, virt_addr, phys_addr, page_flags);
}

/// 克隆用户页表 (深拷贝所有用户态映射)
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_clone_user_page_table(parent_pml4: u64) -> u64 {
    get_vmm().clone_user_page_table(parent_pml4).unwrap_or(0)
}

/// 使用 COW (写时复制) 克隆用户页表
/// 共享页在父与子两侧均被标记为只读.
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_clone_user_page_table_cow(parent_pml4: u64) -> u64 {
    super::cow::clone_user_page_table_cow(parent_pml4).unwrap_or(0)
}

/// 销毁页表并释放其关联的全部内存
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vmm_destroy_page_table(pml4: u64) {
    get_vmm().destroy_page_table(pml4);
}

/// 获取内核 PML4 地址 (访问全局变量)
///
/// 注: 这是对全局 `KERNEL_PML4` 变量的访问器
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub static kernel_pml4: AtomicU64 = AtomicU64::new(0);

// ============================================================
// Kmalloc FFI 函数
// ============================================================

/// 从内核堆分配内存
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn k_malloc(size: usize) -> *mut u8 {
    get_kmalloc()
        .allocate(size)
        .map_or(core::ptr::null_mut(), |ptr| ptr as *mut u8)
}

/// 释放 `k_malloc` 分配的内存
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn k_free(ptr: *mut u8) {
    if !ptr.is_null() {
        get_kmalloc().deallocate(ptr as *mut u8);
    }
}

/// 重新分配内存块
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn k_realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    get_kmalloc()
        .reallocate(ptr as *mut u8, size)
        .map_or(core::ptr::null_mut(), |new_ptr| new_ptr as *mut u8)
}

/// 初始化内核堆
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn kmalloc_init(start: u64, initial_size: u64) {
    get_kmalloc_mut().init(VirtAddr(start), initial_size);
}

/// 打印 kmalloc 统计
///
#[unsafe(no_mangle)]
pub extern "C" fn kmalloc_dump_stats() {
    get_kmalloc().dump_stats();
}

/// 获取内核堆统计 (使用已有的 `KmallocStats` 结构)
pub fn kmalloc_get_stats() -> super::kmalloc::HeapStats {
    get_kmalloc().get_stats()
}

/// 校验堆完整性 (调试用)
///
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn kmalloc_validate() -> i32 {
    i32::from(get_kmalloc().validate())
}

// ============================================================
// 兼容性别名 (无下划线)
// 与原始 C API 函数名一致
// ============================================================

/// `k_malloc` 的别名 — 与原始 C API 一致: void* `kmalloc(uint64_t` size)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn kmalloc(size: u64) -> *mut u8 {
    k_malloc(size as usize)
}

/// `k_free` 的别名 — 与原始 C API 一致: void kfree(void* ptr)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn kfree(ptr: *mut u8) {
    k_free(ptr);
}

/// `k_realloc` 的别名 — 与原始 C API 一致: void* krealloc(void* ptr, `uint64_t` size)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn krealloc(ptr: *mut u8, size: u64) -> *mut u8 {
    k_realloc(ptr, size as usize)
}

/// 获取内核堆统计 — 与原始 C API 一致: void `kmalloc_stats(struct` `kmalloc_stats`* stats)
///
/// 注: 此为简化版本, 暂不填充结构
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
pub extern "C" fn kmalloc_stats(stats: *mut u8) {
    if stats.is_null() {
        return;
    }

    let heap_stats = get_kmalloc().get_stats();

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let stats_ptr = stats as *mut KmallocStats;
        (*stats_ptr).total_allocs = heap_stats.alloc_count;
        (*stats_ptr).total_frees = heap_stats.free_count;
        (*stats_ptr).current_usage = heap_stats.current_usage;
        (*stats_ptr).peak_usage = heap_stats.peak_usage;
    }
}

/// 转储内核堆信息 — 与原始 C API 一致: void `kmalloc_dump(void)`
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn kmalloc_dump() {
    get_kmalloc().dump_stats();
}

// ============================================================
// VMA 公共接口
// ============================================================

/// 获取当前进程的内存描述符.
///
/// 通过公共 api 层访问, 避免直接引用 `mm::vma::get_current_mm`.
pub fn vma_get_current_mm() -> Option<&'static super::vma::MmStruct> {
    super::vma::get_current_mm()
}

// VMA 类型 re-export — 避免跨子系统直接引用 mm::vma 内部类型
pub use super::vma::{MmStruct, Vma, VmaType};

/// 设置当前进程的内存描述符.
///
/// 通过公共 api 层访问, 避免直接引用 `mm::vma::set_current_mm`.
pub fn vma_set_current_mm(mm: *const super::vma::MmStruct) {
    super::vma::set_current_mm(mm);
}

/// VMA 的 POD 快照 — 供 services 层安全消费 (纯数据, 不含裸指针 / 锁).
///
/// `flags_bits` 为 `PageFlags` 的原始位掩码, 由 services 自行映射为上层
/// 语义 (如 ELF `PF_R`/`PF_W`/`PF_X`), 避免框架内嵌上层格式知识.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmaInfo {
    /// 起始地址 (含)
    pub start: u64,
    /// 结束地址 (不含)
    pub end: u64,
    /// `PageFlags` 原始位掩码
    pub flags_bits: u64,
}

/// 采集指定 mm 全部 VMA 的 POD 快照 (保持 `vmas` 列表顺序).
fn snapshot_mm(mm: &super::vma::MmStruct) -> Vec<VmaInfo> {
    let vmas = mm.vmas.lock();
    let mut out = Vec::with_capacity(vmas.len());
    for v in vmas.iter() {
        out.push(VmaInfo {
            start: v.start as u64,
            end: v.end as u64,
            flags_bits: v.flags.bits(),
        });
    }
    out
}

/// 采集当前进程全部 VMA 的 POD 快照.
///
/// 无当前 mm 时返回空表. services 可在持锁语义之外安全遍历返回的纯数据,
/// 避免直接触碰 `MmStruct::vmas` 的 `Mutex`.
pub fn vma_snapshot_current() -> Vec<VmaInfo> {
    match vma_get_current_mm() {
        Some(mm) => snapshot_mm(mm),
        None => Vec::new(),
    }
}

// copy_user re-export — 避免跨子系统直接引用 mm::copy_user 内部
pub use super::copy_user::{copy_from_user, copy_to_user, is_user_buf};

// cross_process re-export — 跨进程用户内存代理 (process_vm_readv/writev 底座)
pub use super::cross_process::{copy_from_user_in_mm, copy_to_user_in_mm};

// page_fault re-export — 避免跨子系统直接引用 mm::page_fault 内部
pub use super::page_fault::{PageFaultInfo, PfResult, handle_page_fault, handle_user_page_fault};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::mm::vma::{MmStruct, Vma, VmaType};

    #[test]
    fn test_vma_snapshot_empty_mm() {
        let mm = MmStruct::new();
        assert!(snapshot_mm(&mm).is_empty());
    }

    #[test]
    fn test_vma_snapshot_preserves_order_and_flags() {
        let mm = MmStruct::new();
        {
            let mut vmas = mm.vmas.lock();
            vmas.push(Vma::new(
                0x1000,
                0x2000,
                PageFlags::PRESENT | PageFlags::WRITABLE,
                VmaType::Anonymous,
            ));
            vmas.push(Vma::new(0x3000, 0x4000, PageFlags::PRESENT, VmaType::Stack));
        }

        let snap = snapshot_mm(&mm);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].start, 0x1000);
        assert_eq!(snap[0].end, 0x2000);
        assert_eq!(
            snap[0].flags_bits,
            (PageFlags::PRESENT | PageFlags::WRITABLE).bits()
        );
        assert_eq!(snap[1].start, 0x3000);
        assert_eq!(snap[1].end, 0x4000);
        assert_eq!(snap[1].flags_bits, PageFlags::PRESENT.bits());
    }
}
