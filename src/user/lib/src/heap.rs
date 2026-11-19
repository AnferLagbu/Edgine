//! 用户态全局分配器 — mmap 后端的 first-fit free-list (`no_std`).
//!
//! WASM 退用户态迁移 P0: 用户态运行时 (wasmi 等) 依赖 `alloc` crate, 需要一个
//! `#[global_allocator]`. 本模块提供该分配器, 内存后端走 `mmap` 匿名映射
//! (`SYS_MMAP=9`), 算法为地址升序 first-fit free-list + 块分裂 + 相邻合并
//! (K&R 风格, 与内核 `privileged::mm::kmalloc` 同源思路, 但去掉内核专属的
//! 早期分配/统计/校验面).
//!
//! 分层: [`FreeList`] 是纯指针逻辑核心, 不依赖任何 syscall, 可在 host 侧用静态
//! arena 直接单测; [`UserAlloc`] 在其外包自旋锁 + mmap 增长 + 任意对齐处理, 构成
//! `GlobalAlloc`. host ring-3 无法跑 Edgine 用户内存 syscall (CR4 限制), 故 mmap
//! 后端在 host 测试中以桩替代, 真映射只在 QEMU 用户态运行时生效.
//!
//! 块布局: `[Header (16B)][payload...]`, 块起始与数据指针均 16 字节对齐.
//! `Header.size` 低位借作空闲标志 (块尺寸恒为 16 倍数, 低 4 位恒 0).

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(test))]
use crate::sys;

/// 块对齐 (字节): 所有块起始地址与常规数据指针的对齐保证.
const ALIGN: usize = 16;
/// 页尺寸 (字节): mmap 增长粒度按页向上取整.
const PAGE: usize = 4096;
/// mmap 单次增长下限 (字节): 减少 syscall 次数, 小分配也映射整块.
const CHUNK: usize = 64 * 1024;
/// `Header.size` 的空闲标志位 (块尺寸为 `ALIGN` 倍数, 低位空闲).
const FREE_BIT: usize = 1;
/// 最小块尺寸 (含 header): 分裂剩余须能容纳一个 header + 最小对齐载荷.
const MIN_BLOCK: usize = 32;

/// 向上取整到 `a` 的倍数 (`a` 须为 2 的幂).
const fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

/// 块头 (16 字节, `repr(C)`): 整块尺寸 + 空闲链指针.
///
/// `size` 含 header 自身, 恒为 `ALIGN` 倍数; 最低位为 [`FREE_BIT`] 空闲标志.
/// `next` 仅在块空闲时有意义, 指向地址更高的下一空闲块 (地址升序单链).
#[repr(C)]
struct Header {
    size: usize,
    next: *mut Header,
}

/// 块头尺寸 (字节).
const HEADER_SIZE: usize = core::mem::size_of::<Header>();

/// 块实际尺寸 (剥掉空闲标志位).
///
/// # Safety
/// `block` 须指向合法块头且调用方独占访问.
unsafe fn block_size(block: *mut Header) -> usize {
    // SAFETY: 契约由调用方保证.
    unsafe { (*block).size & !FREE_BIT }
}

/// 块是否空闲.
///
/// # Safety
/// `block` 须指向合法块头且调用方独占访问.
unsafe fn block_is_free(block: *mut Header) -> bool {
    // SAFETY: 契约由调用方保证.
    unsafe { (*block).size & FREE_BIT != 0 }
}

/// 地址升序 first-fit free-list 核心 (纯指针逻辑, 无 syscall, host 可测).
///
/// 不变式: 链表按块地址严格升序; 相邻空闲块已合并; 每个空闲块尺寸 >= `HEADER_SIZE`.
struct FreeList {
    head: *mut Header,
}

impl FreeList {
    /// 构造空 free-list.
    const fn new() -> Self {
        Self {
            head: ptr::null_mut(),
        }
    }

    /// 把一段连续内存 `[ptr, ptr + size)` 作为单个空闲块并入链表.
    ///
    /// # Safety
    /// `ptr` 须 `ALIGN` 对齐, `size` 须为 `ALIGN` 倍数且 >= `MIN_BLOCK`,
    /// 该内存区独占可写且不与既有块重叠.
    unsafe fn add_region(&mut self, ptr: *mut u8, size: usize) {
        let block = (ptr as usize) as *mut Header;
        // SAFETY: ptr 对齐且区段足够, 写入块头合法.
        unsafe {
            (*block).size = size | FREE_BIT;
            (*block).next = ptr::null_mut();
            self.insert(block);
        }
    }

    /// 地址升序插入空闲块, 并与物理相邻的前后空闲块合并.
    ///
    /// # Safety
    /// `block` 须为合法空闲块 (已置 `FREE_BIT`), 且调用方独占整个堆.
    unsafe fn insert(&mut self, block: *mut Header) {
        // SAFETY: 以下指针解引用均在独占堆前提下, 块头由调用方保证合法.
        unsafe {
            let mut prev: *mut Header = ptr::null_mut();
            let mut cur = self.head;
            while !cur.is_null() && (cur as usize) < (block as usize) {
                prev = cur;
                cur = (*cur).next;
            }
            // 把 block 链接到 prev 与 cur 之间.
            (*block).next = cur;
            if prev.is_null() {
                self.head = block;
            } else {
                (*prev).next = block;
            }
            // 与后继 cur 合并 (物理相邻: block 尾 == cur 头).
            if !cur.is_null() && (block as usize + block_size(block)) == (cur as usize) {
                (*block).size += block_size(cur);
                (*block).next = (*cur).next;
            }
            // 与前驱 prev 合并 (物理相邻: prev 尾 == block 头).
            if !prev.is_null() && (prev as usize + block_size(prev)) == (block as usize) {
                (*prev).size += block_size(block);
                (*prev).next = (*block).next;
            }
        }
    }

    /// first-fit 分配至少 `need` 字节的块, 返回已标记占用的块头; 无可用块返 null.
    ///
    /// `need` 须为 `ALIGN` 倍数且 >= `MIN_BLOCK`. 命中块若剩余 >= `MIN_BLOCK`
    /// 则从尾部切出 `need` (剩余留在原地址, 保持链表地址序), 否则整块分配.
    ///
    /// # Safety
    /// 调用方独占整个堆; 返回块头指向的内存 >= `need` 字节且可写.
    unsafe fn alloc_block(&mut self, need: usize) -> *mut Header {
        // SAFETY: 独占堆前提下的块头解引用.
        unsafe {
            let mut prev: *mut Header = ptr::null_mut();
            let mut cur = self.head;
            while !cur.is_null() {
                let total = block_size(cur);
                if total >= need {
                    if total >= need + MIN_BLOCK {
                        // 从尾部切出 need, 剩余空闲块留在 cur 原地址.
                        let remainder = total - need;
                        let new_block = (cur as usize + remainder) as *mut Header;
                        (*new_block).size = need;
                        (*new_block).next = ptr::null_mut();
                        (*cur).size = remainder | FREE_BIT;
                        return new_block;
                    }
                    // 整块分配, 从链表摘除.
                    if prev.is_null() {
                        self.head = (*cur).next;
                    } else {
                        (*prev).next = (*cur).next;
                    }
                    (*cur).size = total;
                    (*cur).next = ptr::null_mut();
                    return cur;
                }
                prev = cur;
                cur = (*cur).next;
            }
            ptr::null_mut()
        }
    }

    /// 释放块头对应的块, 标记空闲并并入链表 (含相邻合并).
    ///
    /// # Safety
    /// `block` 须为本 free-list 先前分配出的合法块头; 重复释放被忽略.
    unsafe fn free_block(&mut self, block: *mut Header) {
        // SAFETY: block 由调用方保证合法; 空闲标志读写独占.
        unsafe {
            if block_is_free(block) {
                return;
            }
            (*block).size |= FREE_BIT;
            (*block).next = ptr::null_mut();
            self.insert(block);
        }
    }
}

/// mmap 后端: 映射 `len` 字节匿名可读写内存, 成功返页对齐指针, 失败返 null.
///
/// `len` 须已页对齐; 返回内存归调用方独占管理.
#[cfg(not(test))]
fn backend_map(len: usize) -> *mut u8 {
    let ret = sys::mmap(
        0,
        len as u64,
        sys::PROT_READ | sys::PROT_WRITE,
        sys::MAP_PRIVATE | sys::MAP_ANONYMOUS,
        -1,
        0,
    );
    if ret < 0 {
        ptr::null_mut()
    } else {
        ret.cast_unsigned() as *mut u8
    }
}

/// host 测试桩: 恒模拟 mmap 失败, 用于验证 OOM 向 `alloc` 返回 null 的传播.
#[cfg(test)]
fn backend_map(_len: usize) -> *mut u8 {
    ptr::null_mut()
}

/// 用户态全局分配器: 自旋锁保护的 free-list + mmap 增长 + 任意对齐.
struct UserAlloc {
    lock: AtomicBool,
    list: UnsafeCell<FreeList>,
}

// SAFETY: `list` 由 `lock` 自旋锁串行化访问, 同一时刻仅一个线程进入临界区,
// 故跨线程共享安全. `FreeList` 内的裸指针只在持锁临界区内解引用.
unsafe impl Sync for UserAlloc {}

impl UserAlloc {
    /// 构造空分配器 (首次 alloc 时经 mmap 增长).
    const fn new() -> Self {
        Self {
            lock: AtomicBool::new(false),
            list: UnsafeCell::new(FreeList::new()),
        }
    }

    /// 取自旋锁 (忙等).
    fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    /// 放自旋锁.
    fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }

    /// 经 mmap 增长堆, 把新区段并入 free-list; 映射失败返 false.
    ///
    /// # Safety
    /// 调用方须持锁; 返回 true 时新区段已并入链表.
    unsafe fn grow(&self, need: usize) -> bool {
        let map_size = align_up(need.max(CHUNK), PAGE);
        let ptr = backend_map(map_size);
        if ptr.is_null() {
            return false;
        }
        // SAFETY: ptr 页对齐 (>= ALIGN), map_size 为 ALIGN 倍数且 >= MIN_BLOCK,
        // 持锁独占, 满足 add_region 契约.
        unsafe { (*self.list.get()).add_region(ptr, map_size) };
        true
    }
}

// SAFETY: `alloc`/`dealloc` 遵守 GlobalAlloc 契约: 返回指针满足 layout 对齐与
// 尺寸, 临界区由自旋锁串行化; 对齐 > ALIGN 时在数据指针前回填块头指针供 dealloc 复原.
unsafe impl GlobalAlloc for UserAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(ALIGN);
        let size = layout.size();
        if size == 0 {
            // 零尺寸分配返回对齐的悬垂指针 (GlobalAlloc 允许), 不参与释放.
            return align as *mut u8;
        }
        // 对齐 > ALIGN 时预留对齐 slack 与回填指针空间.
        let need = if align > ALIGN {
            align_up(HEADER_SIZE + size + align, ALIGN)
        } else {
            align_up(HEADER_SIZE + size, ALIGN)
        }
        .max(MIN_BLOCK);

        self.acquire();
        // SAFETY: 持锁独占 free-list.
        let list = unsafe { &mut *self.list.get() };
        // SAFETY: need 为 ALIGN 倍数且 >= MIN_BLOCK, 满足 alloc_block 契约.
        let mut block = unsafe { list.alloc_block(need) };
        if block.is_null() {
            // SAFETY: 持锁调用 grow.
            if unsafe { self.grow(need) } {
                // SAFETY: 同上.
                block = unsafe { list.alloc_block(need) };
            }
        }
        self.release();

        if block.is_null() {
            return ptr::null_mut();
        }
        if align > ALIGN {
            let base = block as usize + HEADER_SIZE;
            let aligned = align_up(base, align);
            // SAFETY: aligned >= base, aligned - 8 落在块内 (header 的 next 字段
            // 或 payload 前隙); 占用块的 next 字段不被读取, 回填块头指针安全.
            // need 已含 align slack, aligned + size 不超出块尾.
            unsafe {
                *((aligned - core::mem::size_of::<*mut Header>()) as *mut *mut Header) = block;
            }
            aligned as *mut u8
        } else {
            (block as usize + HEADER_SIZE) as *mut u8
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() || layout.size() == 0 {
            return;
        }
        let align = layout.align().max(ALIGN);
        // SAFETY: ptr 由本分配器 alloc 返回; 对齐 > ALIGN 时数据指针前存有块头
        // 指针, 否则块头就在 ptr - HEADER_SIZE. 复原块头后持锁释放.
        let block = if align > ALIGN {
            // SAFETY: ptr 由本分配器 alloc 返回且 align > ALIGN, 故 alloc 已在
            // aligned - size_of::<*mut Header>() 处回填块头指针 (见上方 alloc 分支);
            // aligned 是 align (>= ALIGN=16, 2 的幂) 的倍数, 减一个指针宽度后仍按
            // *mut Header 对齐且落在本块内, 读一个指针宽度即复原块头.
            unsafe { *((ptr as usize - core::mem::size_of::<*mut Header>()) as *const *mut Header) }
        } else {
            (ptr as usize - HEADER_SIZE) as *mut Header
        };
        self.acquire();
        // SAFETY: 持锁独占; block 为先前分配的合法块头.
        unsafe { (*self.list.get()).free_block(block) };
        self.release();
    }
}

#[cfg(not(test))]
#[global_allocator]
static ALLOC: UserAlloc = UserAlloc::new();

#[cfg(test)]
impl UserAlloc {
    /// 测试专用: 用静态 arena 预置堆, 绕过 mmap 后端.
    ///
    /// # Safety
    /// `ptr`/`size` 须满足 `FreeList::add_region` 契约.
    unsafe fn seed(&self, ptr: *mut u8, size: usize) {
        // SAFETY: 契约由调用方保证.
        unsafe { (*self.list.get()).add_region(ptr, size) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16 字节对齐的测试 arena.
    #[repr(C, align(16))]
    struct Arena([u8; 8192]);

    /// 在 arena 上构造预置 free-list.
    fn seeded_list(arena: &mut Arena) -> FreeList {
        let mut list = FreeList::new();
        let ptr = arena.0.as_mut_ptr();
        let size = arena.0.len();
        // SAFETY: arena 16 对齐, size 为 16 倍数且 >= MIN_BLOCK, 独占.
        unsafe { list.add_region(ptr, size) };
        list
    }

    #[test]
    fn alloc_basic_alignment_and_size() {
        let mut arena = Arena([0; 8192]);
        let mut list = seeded_list(&mut arena);
        // SAFETY: 独占 arena, need 合法.
        let block = unsafe { list.alloc_block(64) };
        assert!(!block.is_null());
        // SAFETY: block 合法.
        unsafe {
            assert!(block_size(block) >= 64);
            assert!(!block_is_free(block));
            assert_eq!((block as usize) % ALIGN, 0);
            let data = (block as usize + HEADER_SIZE) as *mut u8;
            assert_eq!((data as usize) % ALIGN, 0);
            core::ptr::write_bytes(data, 0xAB, 64);
            assert_eq!(*data, 0xAB);
        }
    }

    #[test]
    fn free_then_reuse_same_block() {
        let mut arena = Arena([0; 8192]);
        let mut list = seeded_list(&mut arena);
        // SAFETY: 独占 arena.
        unsafe {
            let a = list.alloc_block(64);
            list.free_block(a);
            assert!(block_is_free(a));
            let b = list.alloc_block(64);
            // first-fit 应复用刚释放的同尺寸块.
            assert_eq!(a, b);
        }
    }

    #[test]
    fn split_remainder_reusable() {
        let mut arena = Arena([0; 8192]);
        let mut list = seeded_list(&mut arena);
        // SAFETY: 独占 arena.
        unsafe {
            let small = list.alloc_block(64);
            // 剩余应仍可分配一个大块.
            let big = list.alloc_block(4096);
            assert!(!small.is_null());
            assert!(!big.is_null());
            assert_ne!(small, big);
        }
    }

    #[test]
    fn coalesce_adjacent_free_blocks() {
        let mut arena = Arena([0; 8192]);
        let mut list = seeded_list(&mut arena);
        // SAFETY: 独占 arena.
        unsafe {
            let a = list.alloc_block(64);
            let b = list.alloc_block(64);
            let c = list.alloc_block(64);
            // 释放顺序 a, c, b: 最终三块合并, 应能再分配远超单块的大块.
            list.free_block(a);
            list.free_block(c);
            list.free_block(b);
            let big = list.alloc_block(8000);
            assert!(!big.is_null(), "合并后应能分配 8000 字节");
        }
    }

    #[test]
    fn double_free_is_idempotent() {
        let mut arena = Arena([0; 8192]);
        let mut list = seeded_list(&mut arena);
        // SAFETY: 独占 arena.
        unsafe {
            let a = list.alloc_block(64);
            list.free_block(a);
            list.free_block(a); // 重复释放应被忽略, 不破坏链表.
            let b = list.alloc_block(64);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn oom_returns_null_when_exhausted() {
        let mut arena = Arena([0; 8192]);
        let mut list = seeded_list(&mut arena);
        // SAFETY: 独占 arena.
        unsafe {
            // 耗尽 arena: 整块分配 (剩余 < MIN_BLOCK 不分裂), 链表清空后再分配必失败.
            let a = list.alloc_block(8192);
            assert!(!a.is_null());
            let b = list.alloc_block(64);
            assert!(b.is_null(), "耗尽后应返回 null");
        }
    }

    #[test]
    fn global_alloc_seed_alloc_dealloc_realloc() {
        let mut arena = Arena([0; 8192]);
        let alloc = UserAlloc::new();
        let ptr = arena.0.as_mut_ptr();
        let size = arena.0.len();
        // SAFETY: arena 满足 add_region 契约.
        unsafe { alloc.seed(ptr, size) };

        let layout = Layout::from_size_align(100, 8).unwrap();
        // SAFETY: 预置堆, layout 合法.
        unsafe {
            let p = alloc.alloc(layout);
            assert!(!p.is_null());
            assert_eq!((p as usize) % 8, 0);
            core::ptr::write_bytes(p, 0x11, 100);
            // realloc 增长 (默认实现: 新分配 + 复制 + 释放).
            let p2 = alloc.realloc(p, layout, 200);
            assert!(!p2.is_null());
            assert_eq!(*p2, 0x11, "realloc 应保留旧内容");
            assert_eq!((p2 as usize) % 8, 0);
            alloc.dealloc(p2, Layout::from_size_align(200, 8).unwrap());
        }
    }

    #[test]
    fn global_alloc_high_alignment() {
        let mut arena = Arena([0; 8192]);
        let alloc = UserAlloc::new();
        let ptr = arena.0.as_mut_ptr();
        let size = arena.0.len();
        // SAFETY: arena 满足契约.
        unsafe { alloc.seed(ptr, size) };

        // 对齐 > ALIGN 的路径: 验证数据指针对齐且 dealloc 能正确复原块头.
        for align in [32usize, 64, 256, 4096] {
            let layout = Layout::from_size_align(48, align).unwrap();
            // SAFETY: 预置堆足够, layout 合法.
            unsafe {
                let p = alloc.alloc(layout);
                assert!(!p.is_null(), "align={align} 分配失败");
                assert_eq!((p as usize) % align, 0, "align={align} 未对齐");
                core::ptr::write_bytes(p, 0x5A, 48);
                alloc.dealloc(p, layout);
            }
        }
        // 高对齐块释放后, 常规分配仍可用 (链表未被破坏).
        let layout = Layout::from_size_align(64, 16).unwrap();
        // SAFETY: 预置堆.
        unsafe {
            let p = alloc.alloc(layout);
            assert!(!p.is_null());
            alloc.dealloc(p, layout);
        }
    }

    #[test]
    fn global_alloc_oom_propagates_null() {
        // 不预置堆: alloc_block 失败 → grow 经测试桩返回 null → alloc 返回 null.
        let alloc = UserAlloc::new();
        let layout = Layout::from_size_align(64, 8).unwrap();
        // SAFETY: 空堆, 后端桩恒失败.
        let p = unsafe { alloc.alloc(layout) };
        assert!(p.is_null(), "mmap 失败应传播为 null");
    }

    #[test]
    fn global_alloc_zero_size_returns_aligned_dangling() {
        let alloc = UserAlloc::new();
        let layout = Layout::from_size_align(0, 16).unwrap();
        // SAFETY: 零尺寸不触碰堆.
        let p = unsafe { alloc.alloc(layout) };
        assert!(!p.is_null());
        assert_eq!((p as usize) % 16, 0);
        // SAFETY: 零尺寸 dealloc 直接返回.
        unsafe { alloc.dealloc(p, layout) };
    }
}
