//! 内存管理 — 机制 API 集中导出
//!
//! L-03: 将 privileged/mm 的纯机制函数集中导出, 供 functions 层策略实现调用.
//!
//! **机制 (Mechanism)**: 直接操作硬件或全局数据结构的底层操作:
//! - 物理页分配/释放 (PMM)
//! - 虚拟内存映射/解除映射 (VMM)
//! - 页表切换 (CR3/TTBR0)
//! - 内核堆分配/释放
//! - COW 页表克隆/销毁
//! - 用户空间数据拷贝
//!
//! **策略 (Policy)**: 决定"何时/如何"使用机制:
//! - 内存压力感知的分配策略 (T-02: FrameAllocDecision)
//! - mmap/munmap/mprotect 的参数验证与 VMA 管理
//! - brk 堆扩展策略
//! - 页面换出策略

// ==================== PMM 机制 ====================

pub use super::api::pmm_alloc_huge_page;
pub use super::api::pmm_alloc_huge_page_phys;
pub use super::api::pmm_alloc_page;
pub use super::api::pmm_alloc_page_phys;
pub use super::api::pmm_alloc_pages;
pub use super::api::pmm_alloc_pages_phys;
pub use super::api::pmm_dump_stats;
pub use super::api::pmm_free_huge_page;
pub use super::api::pmm_free_page;
pub use super::api::pmm_free_page_phys;
pub use super::api::pmm_free_pages;
pub use super::api::pmm_free_pages_phys;
pub use super::api::pmm_get_free_pages;
pub use super::api::pmm_get_total_pages;
pub use super::api::pmm_get_used_pages;
pub use super::api::pmm_init;
pub use super::api::pmm_init_bitmap;
pub use super::api::pmm_is_aligned_for_huge;

// ==================== VMM 机制 ====================

pub use super::api::vmm_clone_user_page_table;
pub use super::api::vmm_clone_user_page_table_cow;
pub use super::api::vmm_create_user_page_table;
pub use super::api::vmm_destroy_page_table;
pub use super::api::vmm_ensure_path_user;
pub use super::api::vmm_ensure_pml4_user;
pub use super::api::vmm_get_physical;
pub use super::api::vmm_get_physical_in_table;
pub use super::api::vmm_init;
pub use super::api::vmm_map_huge_page;
pub use super::api::vmm_map_page;
pub use super::api::vmm_map_page_in_table;
pub use super::api::vmm_split_2mb_page;
pub use super::api::vmm_switch_page_table;
pub use super::api::vmm_unmap_page;

// ==================== 内核堆机制 ====================

pub use super::api::KmallocStats;
pub use super::api::k_free;
pub use super::api::k_malloc;
pub use super::api::k_realloc;
pub use super::api::kfree;
pub use super::api::kmalloc;
pub use super::api::kmalloc_dump;
pub use super::api::kmalloc_dump_stats;
pub use super::api::kmalloc_init;
pub use super::api::kmalloc_stats;
pub use super::api::kmalloc_validate;
pub use super::api::krealloc;

// ==================== VMA 机制 ====================

pub use super::api::vma_get_current_mm;
pub use super::api::vma_set_current_mm;
pub use super::api::{MmStruct, Vma, VmaType};

// ==================== 用户空间拷贝 ====================

pub use super::api::copy_from_user;
pub use super::api::copy_to_user;
pub use super::api::is_user_buf;

// ==================== 页错误处理 ====================

pub use super::api::PageFaultInfo;
pub use super::api::PfResult;
pub use super::api::handle_page_fault;
pub use super::api::handle_user_page_fault;
