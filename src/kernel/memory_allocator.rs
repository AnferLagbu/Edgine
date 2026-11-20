use crate::privileged::mm::alloc_meta::{META_LEN, PAGE_BYTES, align_offset, pages_for};
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

/// 走 kmalloc 堆的最大对齐。再大的对齐改走 PMM 整页, 避免堆内对齐间隙膨胀。
const KMALLOC_ALIGN: usize = 16;

const TAG_SIZE: usize = core::mem::size_of::<u64>();

const TAG_KMALLOC: u64 = 0xA115_4B4D_414C_4C01;
const TAG_PMM: u64 = 0xA115_504D_4D50_4702;

/// 写尾随元数据并返回按 `align` 上抬后的数据指针。
///
/// 元数据落点是**数据指针之后**: `[ptr + size, ptr + size + META_LEN)`
/// (来源标签 `u64` + 对齐偏移 `u64`)。dealloc 因此只需 `Layout` 即可定位元数据。
///
/// # Safety
/// `raw` 必须指向一块本次分配独占、至少 `align + size + META_LEN` 字节可写的内存
/// (PMM 路径由 `pages_for` 保证), 且写入后调用方不再以 `raw` 为基址另作他用。
unsafe fn write_meta(raw: *mut u8, size: usize, align: usize, tag: u64) -> *mut u8 {
    // SAFETY: 本函数的安全契约保证 [raw + offset + size, raw + offset + size + 16)
    // 在本次分配内; `write_unaligned` 不要求地址对齐 (载荷起点与元数据都可能是
    // 任意字节偏移)。返回的指针由 `align_offset` 保证满足 `align`。
    unsafe {
        let offset = align_offset(raw as usize, align);
        let ptr = raw.add(offset);
        let meta = ptr.add(size);
        core::ptr::write_unaligned(meta.cast::<u64>(), tag);
        core::ptr::write_unaligned(meta.add(TAG_SIZE).cast::<u64>(), offset as u64);
        ptr
    }
}

// SAFETY: `GlobalAlloc` 契约 = alloc/dealloc 按同一 `Layout` 配对、返回指针满足
// `layout.align()`、失败返回 null。按「小对象 / 大对象且对齐 ≤ 页 / 其余」三路路由:
// - `size <= PAGE_THRESHOLD` 且 `align <= KMALLOC_ALIGN`: 走 kmalloc, 请求
//   `size + 16 + align` (容纳尾随元数据与最坏对齐偏移), 数据指针按 `align_offset`
//   上抬, 元数据写在 `[数据指针 + size, +16)` = [来源标签 u64][对齐偏移 u64];
// - `size > PAGE_THRESHOLD` 且 `align <= PAGE_BYTES`: 沿用无元数据路径, 页基址本身
//   已是 >= 页对齐, 数据指针 = 基础块指针, 页数 `size.div_ceil(4096)` (不因 16 字节
//   元数据而给整页大小的分配多要一页);
// - 其余 (小对象改走 PMM, 或大对象要求 > 页的对齐): 走 PMM, 页数由
//   `alloc_meta::pages_for` 按「载荷 + 元数据 + 最坏偏移」算, 同样写尾随元数据。
// 两条写元数据的路径都使返回指针对任意合法 `align` 成立; dealloc 以同一 Layout
// 复原路由与基础块指针, 页数与 alloc 同式。
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 本块是 `GlobalAlloc::alloc` 的实现体, 总契约见上方 impl 的 SAFETY。
        // `kmalloc` 返回至少 8 字节对齐的堆块 (早期从 `early_buffer` 切分, 未必 16
        // 对齐), 故偏移在拿到 raw 之后再算; 请求长度已含 `META_LEN + align` 上界,
        // 元数据与最坏偏移都落在本次分配内。PMM 路径 `phys + KERNEL_BASE` 是内核
        // 线性映射 (x86_64 0xFFFF_8000_0000_0000 / aarch64 0xFFFF_0000_0000_0000),
        // 页对齐且整页可写; 分配失败即返回 null 不构造指针。
        // 需要尾随元数据的 PMM 路径由 `pages_for` 按最坏偏移计页; 无元数据路径的
        // 页基址本身已满足 <= 页的对齐。
        unsafe {
            let size = layout.size();
            let align = layout.align();

            if size <= PAGE_THRESHOLD && align <= KMALLOC_ALIGN {
                let Some(total) = size
                    .checked_add(META_LEN)
                    .and_then(|n| n.checked_add(align))
                else {
                    return core::ptr::null_mut();
                };
                let raw = kmalloc(total as u64);
                if !raw.is_null() {
                    return write_meta(raw, size, align, TAG_KMALLOC);
                }
            }

            // 大对象且对齐 <= 页: 页基址已满足对齐, 沿无元数据路径, 不因 16 字节元数据
            // 给整页大小的分配多要一页; 其余路径按「载荷 + 元数据 + 最坏偏移」计页。
            let tagged = size <= PAGE_THRESHOLD || align > PAGE_BYTES;
            let pages = if tagged {
                let Some(pages) = pages_for(size, align) else {
                    return core::ptr::null_mut();
                };
                pages
            } else {
                size.div_ceil(PAGE_BYTES)
            };

            // OOM 按 `GlobalAlloc` 契约返回 null (旧实现把 0 物理地址加上
            // KERNEL_BASE 当有效指针返回, 等于把一个非法地址交给调用方).
            let alloc = if pages == 1 {
                pmm_alloc_page_phys()
            } else {
                pmm_alloc_pages_phys(pages)
            };
            let Some(phys) = alloc else {
                return core::ptr::null_mut();
            };
            let raw = (phys.as_u64() + KERNEL_BASE) as *mut u8;
            if tagged {
                write_meta(raw, size, align, TAG_PMM)
            } else {
                raw
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 本块是 `GlobalAlloc::dealloc` 的实现体, 契约要求 `ptr` 来自本分配器
        // 同一 `Layout` 的 alloc 且尚未释放。路由判据与 alloc 同式: 大对象且对齐 <= 页
        // 走无元数据路径 (页数 `size.div_ceil(4096)`, 数据指针即基础块指针); 其余路径的
        // 元数据固定在 `ptr + size` 处 (不受对齐偏移影响), 读回来源标签与偏移即可复原
        // 基础块指针 `raw = ptr - offset`。kmalloc 路径按 `kfree(raw)` 归还, PMM 路径按
        // `pages_for(size, align)` 的同一页数归还 `raw - KERNEL_BASE`。未知标签 (堆损坏)
        // 回退 `kfree(raw)`, 与旧行为同口径。
        unsafe {
            if ptr.is_null() {
                return;
            }

            let size = layout.size();

            if size > PAGE_THRESHOLD && layout.align() <= PAGE_BYTES {
                let pages = size.div_ceil(PAGE_BYTES);
                let phys_addr = (ptr as u64) - KERNEL_BASE;
                if pages <= 1 {
                    pmm_free_page_phys(PhysAddr(phys_addr));
                } else {
                    pmm_free_pages_phys(PhysAddr(phys_addr), pages);
                }
                return;
            }

            let meta = ptr.add(size);
            let tag = core::ptr::read_unaligned(meta.cast::<u64>());
            let offset = crate::privileged::glue::usize_from_u64(core::ptr::read_unaligned(
                meta.add(TAG_SIZE).cast::<u64>(),
            ));
            let raw = ptr.sub(offset);

            match tag {
                TAG_PMM => {
                    // alloc 侧成功即说明页数可算; 溢出分支在 dealloc 不可达, 直接返回.
                    let Some(pages) = pages_for(size, layout.align()) else {
                        return;
                    };
                    let phys_addr = (raw as u64) - KERNEL_BASE;
                    if pages <= 1 {
                        pmm_free_page_phys(PhysAddr(phys_addr));
                    } else {
                        pmm_free_pages_phys(PhysAddr(phys_addr), pages);
                    }
                }
                // 未知标签 (含 TAG_KMALLOC) 回退按 kmalloc 块释放
                _ => {
                    kfree(raw);
                }
            }
        }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;
