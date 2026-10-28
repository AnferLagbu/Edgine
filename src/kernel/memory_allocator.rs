use crate::privileged::mm::{
    PhysAddr, pmm_alloc_page_phys, pmm_alloc_pages_phys, pmm_free_page_phys, pmm_free_pages_phys,
};
use core::alloc::{GlobalAlloc, Layout};

// 注: 物理帧的分配 / 归还不走 `extern "C"` 声明 (改走 `mm::pmm_*_phys` 的 Rust
// 入口). `extern "C"` fn 因 ABI 限制不能标 `track_caller`, 经它调用 PMM 会把帧
// 所有权归因吞到 `mm/api.rs` 包装层的固定行号, 双归还告警就无法点名争抢双方
// (本堆归还 vs 地址空间销毁). 本文件的 `GlobalAlloc` trait 实现方法同样不能标
// `track_caller` (trait 定义无该属性), 但归因至少精确落到下面每个调用点的行号.
unsafe extern "C" {
    fn kmalloc(size: u64) -> *mut u8;
    fn kfree(ptr: *mut u8);
}

#[cfg(target_arch = "aarch64")]
const KERNEL_BASE: u64 = 0xFFFF_0000_0000_0000_u64;

#[cfg(not(target_arch = "aarch64"))]
const KERNEL_BASE: u64 = 0xFFFF_8000_0000_0000_u64;

pub struct KernelAllocator;

const PAGE_THRESHOLD: usize = 2048;

const TAG_KMALLOC: u64 = 0xA115_4B4D_414C_4C01;
const TAG_PMM_PAGE: u64 = 0xA115_504D_4D50_4702;
const TAG_PMM_PAGES: u64 = 0xA115_504D_4D50_4703;
const TAG_SIZE: usize = core::mem::size_of::<u64>();

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let size = layout.size();
            let tag_offset = if size <= PAGE_THRESHOLD { TAG_SIZE } else { 0 };

            if size <= PAGE_THRESHOLD {
                let kmalloc_ptr = kmalloc((size + tag_offset) as u64);
                if !kmalloc_ptr.is_null() {
                    let raw = kmalloc_ptr.cast::<u8>();
                    let tag_ptr = raw.cast::<u64>();
                    *tag_ptr = TAG_KMALLOC;
                    return raw.add(TAG_SIZE);
                }
            }

            let pages_needed = (size + tag_offset).div_ceil(4096);
            let tag: u64 = if pages_needed == 1 {
                TAG_PMM_PAGE
            } else {
                TAG_PMM_PAGES
            };

            // OOM 按 `GlobalAlloc` 契约返回 null (旧实现把 0 物理地址加上
            // KERNEL_BASE 当有效指针返回, 等于把一个非法地址交给调用方).
            if pages_needed == 1 {
                let Some(phys) = pmm_alloc_page_phys() else {
                    return core::ptr::null_mut();
                };
                let virt = (phys.as_u64() + KERNEL_BASE) as *mut u8;
                if tag_offset > 0 {
                    let tag_ptr = virt.cast::<u64>();
                    *tag_ptr = tag;
                    virt.add(TAG_SIZE)
                } else {
                    virt
                }
            } else {
                let Some(phys) = pmm_alloc_pages_phys(pages_needed) else {
                    return core::ptr::null_mut();
                };
                let virt = (phys.as_u64() + KERNEL_BASE) as *mut u8;
                if tag_offset > 0 {
                    let tag_ptr = virt.cast::<u64>();
                    *tag_ptr = tag;
                    virt.add(TAG_SIZE)
                } else {
                    virt
                }
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            if ptr.is_null() {
                return;
            }

            let size = layout.size();
            if size <= PAGE_THRESHOLD {
                let raw = ptr.sub(TAG_SIZE);
                let tag = *(raw as *const u64);
                match tag {
                    TAG_PMM_PAGE => {
                        let phys_addr = (raw as u64) - KERNEL_BASE;
                        pmm_free_page_phys(PhysAddr(phys_addr));
                    }
                    TAG_PMM_PAGES => {
                        let phys_addr = (raw as u64) - KERNEL_BASE;
                        let pages_needed = (size + TAG_SIZE).div_ceil(4096);
                        pmm_free_pages_phys(PhysAddr(phys_addr), pages_needed);
                    }
                    // 未知标签 (含 TAG_KMALLOC) 回退按 kmalloc 块释放
                    _ => {
                        kfree(raw.cast::<u8>());
                    }
                }
            } else {
                let pages = size.div_ceil(4096);
                let phys_addr = (ptr as u64) - KERNEL_BASE;
                if pages <= 1 {
                    pmm_free_page_phys(PhysAddr(phys_addr));
                } else {
                    pmm_free_pages_phys(PhysAddr(phys_addr), pages);
                }
            }
        }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;
