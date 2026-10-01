//! 惰性页池 — 按需分配 4KB 物理页的后备存储.
//!
//! 用于替代子系统级 `static [u8; N]` 容量数组: 静态只保留一个槽表
//! (`PAGES` 个 `usize`), 实际页在首次写入时惰性分配, 未分配区间读为 0
//! (与原静态零初始化数组语义一致). 典型收益: 将开机即占的 N MiB 静态区
//! 压缩为 `PAGES * 8` 字节槽表, 页用量随实际数据增长.
//!
//! ## 载体选择
//!
//! 生产 (裸机) 经 PMM 分配物理页, 并经 [`phys_to_virt`] 取其内核高半区域
//! 别名 — 不能直接把物理地址当指针解引用 (aarch64 运行期 TTBR0 的每进程
//! EL1 视图刻意移除 DRAM 块, 物理地址在运行期并非有效可解引用地址).
//! host 测试下 PMM 未经引导链初始化, 改用 `alloc` 堆载体, 使同一份页池
//! 逻辑在两侧 (与 PMM 的 `RawMetaStore` / `VecMetaStore` 同理) 无测试/生产分叉.
//!
//! ## 约束
//!
//! 页池只持有页的直接映射别名, 故经 PMM 分配的页其物理地址必须落在内核
//! 高半区直接映射范围内 (x86_64 为 phys `0..1GB`, aarch64 为 phys `0..2GB`).
//! 该前提与框架既有 [`Frame::zero`](crate::framework::mm::frame::Frame::zero)
//! 一致, 属框架当前直接映射范围的固有约束.

use core::ptr;

#[cfg(any(test, feature = "host-test"))]
use core::alloc::Layout;

use super::PAGE_SIZE;
#[cfg(not(any(test, feature = "host-test")))]
use super::{PhysAddr, phys_to_virt, pmm_alloc_page_phys, pmm_free_page_phys, virt_to_phys};

/// 页字节数 (页池的最小分配与寻址粒度)
const PAGE_BYTES: usize = PAGE_SIZE as usize;

/// 分配一个已清零的 4KB 页, 返回其可直接解引用的虚拟地址; 失败返回 `None`.
#[cfg(not(any(test, feature = "host-test")))]
fn page_alloc() -> Option<usize> {
    let phys = pmm_alloc_page_phys()?;
    let va = phys_to_virt(phys.as_u64());
    // SAFETY: phys 来自 PMM 已分配页 (排他所有权); phys_to_virt 给出该页在内核
    // 高半区的直接映射别名; 地址按页对齐, 写入范围为本页 PAGE_BYTES 字节, 不越界.
    unsafe { ptr::write_bytes(va as *mut u8, 0, PAGE_BYTES) };
    Some(va as usize)
}

/// 归还一个由 [`page_alloc`] 分配的页.
#[cfg(not(any(test, feature = "host-test")))]
fn page_free(page_addr: usize) {
    if page_addr == 0 {
        return;
    }
    let phys = virt_to_phys(page_addr as u64);
    pmm_free_page_phys(PhysAddr(phys));
}

/// 分配一个已清零的 4KB 堆块, 返回其地址; 失败返回 `None`.
#[cfg(any(test, feature = "host-test"))]
fn page_alloc() -> Option<usize> {
    let layout = Layout::from_size_align(PAGE_BYTES, PAGE_BYTES).ok()?;
    // SAFETY: layout 大小非零且对齐为页大小的幂; `alloc_zeroed` 返回已清零块
    // (或空指针, 下方判空处理).
    let p = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if p.is_null() { None } else { Some(p as usize) }
}

/// 归还一个由 [`page_alloc`] 分配的堆块.
#[cfg(any(test, feature = "host-test"))]
fn page_free(page_addr: usize) {
    if page_addr == 0 {
        return;
    }
    let layout = Layout::from_size_align(PAGE_BYTES, PAGE_BYTES)
        .expect("LazyPageArena: 页 layout 编译期常量, 恒合法");
    // SAFETY: page_addr 由 page_alloc 以同一 layout 分配, 且槽位在归还后置 0, 不重复释放.
    unsafe { alloc::alloc::dealloc(page_addr as *mut u8, layout) };
}

/// 分配一个已清零的 4KB 物理页, 并以类型化指针形式返回其可直接解引用的地址.
///
/// 与 [`page_alloc`] 共用同一载体策略 (生产: PMM 物理页经 [`phys_to_virt`] 取
/// 内核高半区别名; host: `alloc` 堆), 供 per-CPU 结构的惰性分配复用. 返回的页
/// 已整体清零, 故 `T` 若为"全零构造"则无需再写入; 若为"非全零构造", 调用方须
/// 在返回指针上 `write` 构造值覆盖首部.
///
/// 约束: `T` 不超出单页 (`size_of::<T>() <= PAGE_SIZE`), 分配失败返回 `None`.
pub(crate) fn alloc_zeroed_page_as<T>() -> Option<*mut T> {
    debug_assert!(
        core::mem::size_of::<T>() <= PAGE_BYTES,
        "alloc_zeroed_page_as: T 超出单页容量"
    );
    let addr = page_alloc()?;
    Some(addr as *mut T)
}

/// 惰性页池 — 以 4KB 页为单位按需分配后备存储的字节区间容器.
///
/// 静态体量为 `PAGES * 8` 字节 (槽表), 不含数据页本身. 未分配页的读取一律
/// 返回 0, 故对上层表现为一个容量 `PAGES * PAGE_SIZE` 的零初始化字节数组.
/// 分配失败时写入类的操作返回 `false` 且不产生部分可见的页状态.
pub struct LazyPageArena<const PAGES: usize> {
    /// 每槽存对应页的可直接解引用虚拟地址, 0 表示未分配
    slots: [usize; PAGES],
}

impl<const PAGES: usize> LazyPageArena<PAGES> {
    /// 构造空页池 (全部槽位未分配), 可用于 `static` 零初始化.
    pub const fn new() -> Self {
        Self { slots: [0; PAGES] }
    }

    /// 容量字节数 (= `PAGES * PAGE_SIZE`).
    pub const fn capacity_bytes(&self) -> usize {
        self.slots.len() * PAGE_BYTES
    }

    /// 取页号对应的槽位地址; 越界页号返回 0 (视作未分配).
    fn page_addr(&self, page: usize) -> usize {
        if page < PAGES { self.slots[page] } else { 0 }
    }

    /// 读取 `offset..offset + dst.len()` 区间到 `dst`.
    ///
    /// 透明跨页; 未分配页 (含超出容量的部分) 读为 0.
    pub fn read(&self, offset: usize, dst: &mut [u8]) {
        let mut done = 0usize;
        while done < dst.len() {
            let pos = offset + done;
            let page = pos / PAGE_BYTES;
            let in_page = pos % PAGE_BYTES;
            let n = (PAGE_BYTES - in_page).min(dst.len() - done);
            let base = self.page_addr(page);
            if base == 0 {
                dst[done..done + n].fill(0);
            } else {
                // SAFETY: base 非 0 ⇒ 为 page_alloc 返回的有效页基址; 读取区间
                // [in_page, in_page + n) 已按页边界截断, 完全落在本页内.
                let src = unsafe { core::slice::from_raw_parts((base as *const u8).add(in_page), n) };
                dst[done..done + n].copy_from_slice(src);
            }
            done += n;
        }
    }

    /// 将 `src` 写入 `offset..` 区间 (透明跨页, 未分配页按需分配并清零).
    ///
    /// 返回 `false` = 目标页超出容量或分配失败, 此时不保证已完整写入.
    pub fn write(&mut self, offset: usize, src: &[u8]) -> bool {
        let mut done = 0usize;
        while done < src.len() {
            let pos = offset + done;
            let page = pos / PAGE_BYTES;
            let in_page = pos % PAGE_BYTES;
            let n = (PAGE_BYTES - in_page).min(src.len() - done);
            if page >= PAGES {
                return false;
            }
            if self.slots[page] == 0 {
                match page_alloc() {
                    Some(p) => self.slots[page] = p,
                    None => return false,
                }
            }
            let base = self.slots[page];
            // SAFETY: base 为有效页基址; 写入区间已按页边界截断, 完全落在本页内;
            // 源 `src` 与目标页互不重叠 (页为页池排他持有).
            unsafe {
                ptr::copy_nonoverlapping(src[done..].as_ptr(), (base as *mut u8).add(in_page), n);
            }
            done += n;
        }
        true
    }

    /// 将 `offset..offset + len` 区间清零 (未分配页按需分配).
    ///
    /// 返回 `false` = 目标页超出容量或分配失败.
    pub fn zero(&mut self, offset: usize, len: usize) -> bool {
        let mut done = 0usize;
        while done < len {
            let pos = offset + done;
            let page = pos / PAGE_BYTES;
            let in_page = pos % PAGE_BYTES;
            let n = (PAGE_BYTES - in_page).min(len - done);
            if page >= PAGES {
                return false;
            }
            if self.slots[page] == 0 {
                match page_alloc() {
                    Some(p) => self.slots[page] = p,
                    None => return false,
                }
            }
            let base = self.slots[page];
            // SAFETY: base 为有效页基址; 清零区间已按页边界截断, 完全落在本页内.
            unsafe { ptr::write_bytes((base as *mut u8).add(in_page), 0, n) };
            done += n;
        }
        true
    }

    /// 读取 4 字节小端 `u32`; 未分配页或越界读为 0.
    pub fn read_u32(&self, offset: usize) -> u32 {
        let mut buf = [0u8; 4];
        self.read(offset, &mut buf);
        u32::from_le_bytes(buf)
    }

    /// 写入 4 字节小端 `u32`; 返回 `false` = 分配失败或越界.
    pub fn write_u32(&mut self, offset: usize, val: u32) -> bool {
        self.write(offset, &val.to_le_bytes())
    }

    /// 释放全部已分配页并复位为全空.
    pub fn clear(&mut self) {
        for addr in &mut self.slots {
            if *addr != 0 {
                page_free(*addr);
                *addr = 0;
            }
        }
    }
}

// LazyPageArena 仅含 `[usize; PAGES]`, 自动实现 Send + Sync.
