use super::process::{FdTable, PROCESS_TABLE, Process};
use super::types::{ProcessContext, ProcessId, ProcessPriority, ProcessState};
use crate::klog_error;
use crate::privileged::mm::KERNEL_BASE;
use crate::privileged::sync::IrqSpinLock as Mutex;
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub static user_entry_cr3: AtomicU64 = AtomicU64::new(0);

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub static user_entry_target: AtomicU64 = AtomicU64::new(0);

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    fn pmm_alloc_page() -> *mut u8;
    fn pmm_alloc_pages(count: u64) -> *mut u8;
    fn pmm_free_page(page: *mut u8);
    fn vmm_create_user_page_table() -> u64;
    fn vmm_map_page_in_table(table: u64, vaddr: u64, paddr: u64, flags: u64);
    fn vmm_map_page(vaddr: u64, paddr: u64, flags: u64) -> i32;
    fn vmm_ensure_path_user(vaddr: u64);
    fn vmm_destroy_page_table(cr3: u64);
    fn vmm_get_physical_in_table(table: u64, vaddr: u64) -> u64;
    fn memset(s: *mut core::ffi::c_void, c: i32, n: usize) -> *mut core::ffi::c_void;
    fn memcpy(
        dest: *mut core::ffi::c_void,
        src: *const core::ffi::c_void,
        n: usize,
    ) -> *mut core::ffi::c_void;
    fn kmalloc(size: u64) -> *mut u8;
}

/// 进程/调度规模常量 — 统一从 `super::types` 引用
///
/// 全部进程规模/栈/调度参数已在 `types.rs` 集中 re-export, 本文件仅 `use` 它们,
/// 避免分散定义与影子覆盖问题。
pub use super::types::{
    KERNEL_STACK_SIZE, MAX_OPEN_FILES, MAX_PROCESSES, PAGE_SIZE, SCHED_BOOST_INTERVAL,
    SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM, SCHED_LEVEL_3_QUANTUM,
    SCHED_RT_WATCHDOG_TICKS, USER_CODE_BASE, USER_KSTACK_SIZE, USER_STACK_GUARD,
    USER_STACK_MAX_SIZE, USER_STACK_SIZE, USER_STACK_TOP,
};

/// 派生常量: 用户栈自动扩展的下界 (`USER_STACK_TOP` - `USER_STACK_MAX_SIZE`)
pub const USER_STACK_EXPAND_LIMIT: u64 = USER_STACK_TOP - USER_STACK_MAX_SIZE;

/// `PAGE_PRESENT` / WRITABLE / USER — 旧式裸 u64 常量 (保留以兼容 C 端)
/// 业务层推荐使用 `privileged::mm::PageFlags` 类型化抽象, FFI 边界通过 `.bits()` 转换.
#[deprecated(note = "use privileged::mm::PageFlags 替代 (类型安全 + 编译期检查)")]
pub const PAGE_PRESENT: u64 = 1;
#[deprecated(note = "use privileged::mm::PageFlags::WRITABLE 替代")]
pub const PAGE_WRITABLE: u64 = 2;
#[deprecated(note = "use privileged::mm::PageFlags::USER 替代")]
pub const PAGE_USER: u64 = 4;

/// 类型化页面标志 (从 `privileged::mm` 引入, 在 FFI 边界通过 .`bits()` 转 u64)
use crate::privileged::mm::PageFlags;

pub const GDT_USER_DATA: u64 = 0x18;
pub const GDT_USER_CODE: u64 = 0x20;

pub const PT_LOAD: u32 = 1;

/// ELF 头部 / 程序头 — 重导出 elf.rs canonical 定义
///
/// 使用 `privileged::proc::elf::Elf64Header` 作为唯一权威, 避免重复定义
/// 引起字段名不一致 (`machine` vs `e_machine` 等).
pub use super::elf::{Elf64Header as ElfHeader, Elf64Phdr as ElfPhdr};

#[repr(C)]
pub struct UserProcInfo {
    pub entry: u64,
    pub name: [u8; 64],
    pub code_size: u64,
    pub code_data: *const u8,
}

// === 特权层: UserProcess 裸指针/FFI 桥接集中地 ===
//
// 本子模块包含所有与裸指针 (`*mut UserProcess`)、extern "C" FFI 调用
// (kmalloc/memset/memcpy/vmm_*) 相关的 `unsafe` 代码。
//
// 上层 `user_proc.rs` 业务逻辑 (create/enter/setup_user_stack/load_elf_from_memory 等)
// 通过 `raw::UserProcRef` newtype 安全访问 UserProcess 字段, 保持 100% safe Rust。
//
// `unsafe impl Send/Sync` 在本子模块中声明, 因为类型定义本身需要这些 trait
// 才能被 `static USER_PROC_MANAGER` 使用。
pub(crate) mod raw {
    use super::{
        AtomicU32, AtomicU64, FdTable, KERNEL_BASE, Mutex, NonNull, Ordering, PAGE_SIZE, PageFlags,
        Process, ProcessContext, ProcessId, ProcessPriority, ProcessState, String, UserProcess,
        Vec, kmalloc, memcpy, memset, pmm_alloc_page, pmm_alloc_pages, pmm_free_page, raw,
        vmm_create_user_page_table, vmm_destroy_page_table, vmm_ensure_path_user,
        vmm_get_physical_in_table, vmm_map_page, vmm_map_page_in_table,
    };

    // === UserProcess 安全访问封装 (Framekernel privilege wrapper) ===
    //
    // `*mut UserProcess` 在用户进程管理中作为索引句柄使用。将其封装为 `UserProcRef`
    // newtype 后, 所有 `unsafe { (*ptr).field }` 集中在 `UserProcRef` 内部方法中。
    //
    // # SAFETY invariant
    // - 调用方必须保证 `*mut UserProcess` 指向一个有效的 `UserProcess` 分配 (kmalloc)。
    // - 通过 USER_PROC_MANAGER.processes BTreeMap 持有的 NonNull 句柄都是有效的。
    #[derive(Clone, Copy)]
    pub struct UserProcRef(*mut UserProcess);

    impl UserProcRef {
        /// 从裸指针构造, 要求调用方提供 SAFETY 保证。
        ///
        /// # Safety
        /// - `ptr` 必须为非空, 指向有效 `UserProcess` 分配
        /// - 在 `UserProcRef` 存活期间, 不会被释放
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub unsafe fn new_unchecked(ptr: *mut UserProcess) -> Self {
            Self(ptr)
        }

        /// 访问 pid 字段 (委托到 Process)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn pid(&self) -> u32 {
            // SAFETY: `self` 由调用方保证为有效指针; 通过 process() 访问权威 Process
            unsafe { (*self.0).process().pid.0 }
        }

        #[inline(always)]
        #[expect(
            clippy::borrow_as_ptr,
            reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
        )]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_pid(&self, v: u32) {
            // SAFETY: 调用方保证指针/类型有效; 写入权威 Process
            // 注意: Process::pid 是 ProcessId (newtype), 需要通过 ptr::write 更新
            unsafe {
                let proc = (*self.0).process.as_ptr();
                core::ptr::write(&mut (*proc).pid as *mut _, ProcessId(v));
            }
        }

        /// 访问 entry 字段 (读写)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn entry(&self) -> u64 {
            // SAFETY: `self` 由调用方保证为有效指针; 只读访问
            unsafe { (*self.0).entry }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_entry(&self, v: u64) {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                (*self.0).entry = v;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_create_time(&self, v: u64) {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                (*self.0).create_time = v;
            }
        }

        /// 访问 pwm (委托到 Process)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn load_pwm(&self) -> u64 {
            // SAFETY: `self` 由调用方保证为有效指针; 通过 process() 访问权威 Process
            unsafe { (*self.0).process().pwm.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_pwm(&self, v: u64) {
            // SAFETY: 调用方保证指针/类型有效; 写入权威 Process
            unsafe {
                (*self.0).process().pwm.store(v, Ordering::SeqCst);
            }
        }

        /// 访问 cr3 (委托到 Process)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn load_cr3(&self) -> u64 {
            // SAFETY: `self` 由调用方保证为有效指针; 通过 process() 访问权威 Process
            unsafe { (*self.0).process().cr3.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_cr3(&self, v: u64) {
            // SAFETY: 调用方保证指针/类型有效; 写入权威 Process
            unsafe {
                (*self.0).process().cr3.store(v, Ordering::SeqCst);
            }
        }

        /// 访问 `kernel_stack` (委托到 Process)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn load_kernel_stack(&self) -> u64 {
            // SAFETY: `self` 由调用方保证为有效指针; 通过 process() 访问权威 Process
            unsafe { (*self.0).process().kernel_stack.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_kernel_stack(&self, v: u64) {
            // SAFETY: 调用方保证指针/类型有效; 写入权威 Process
            unsafe {
                (*self.0).process().kernel_stack.store(v, Ordering::SeqCst);
            }
        }

        /// 访问 `user_stack` (委托到 Process)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn load_user_stack(&self) -> u64 {
            // SAFETY: `self` 由调用方保证为有效指针; 通过 process() 访问权威 Process
            unsafe { (*self.0).process().user_stack.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_user_stack(&self, v: u64) {
            // SAFETY: 调用方保证指针/类型有效; 写入权威 Process
            unsafe {
                (*self.0).process().user_stack.store(v, Ordering::SeqCst);
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn load_stack_bottom(&self) -> u64 {
            // SAFETY: `self` 由调用方保证为有效指针; 只读访问
            unsafe { (*self.0).stack_bottom.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_stack_bottom(&self, v: u64) {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                (*self.0).stack_bottom.store(v, Ordering::SeqCst);
            }
        }

        /// 访问 state (委托到 Process)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_state(&self, v: u32) {
            // SAFETY: 调用方保证指针/类型有效; 写入权威 Process
            unsafe {
                (*self.0).process().state.store(v, Ordering::SeqCst);
            }
        }

        /// 加载进程状态 (委托到 Process)
        #[inline(always)]
        pub fn load_state(&self) -> u32 {
            // SAFETY: `self` 由调用方保证为有效指针; 通过 process() 访问权威 Process
            unsafe { (*self.0).process().state.load(Ordering::SeqCst) }
        }

        #[expect(
            clippy::trivially_copy_pass_by_ref,
            reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
        )]
        /// 检查进程是否在运行状态 (Running = 2)
        pub fn is_running(&self) -> bool {
            use crate::privileged::proc::types::ProcessState;
            ProcessState::from_u32(self.load_state()).is_alive()
        }

        #[expect(
            clippy::trivially_copy_pass_by_ref,
            reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
        )]
        /// 检查进程是否已退出 (Zombie = 4 或 Terminated = 5)
        pub fn is_exited(&self) -> bool {
            use crate::privileged::proc::types::ProcessState;
            let state = ProcessState::from_u32(self.load_state());
            matches!(state, ProcessState::Zombie | ProcessState::Terminated)
        }
    }

    /// 在 `BTreeMap` 中按 pid 索引得到的 `NonNull` 句柄转成安全引用。
    ///
    /// # Safety (内部)
    /// - `nn` 必须由 `USER_PROC_MANAGER` 持有, 指向有效 `UserProcess` 分配。
    pub fn deref_non_null(nn: NonNull<UserProcess>) -> &'static UserProcess {
        // SAFETY: nn is from USER_PROC_MANAGER BTreeMap, allocation outlives the manager.
        unsafe { &*nn.as_ptr() }
    }

    // === FFI 桥接安全包装 (mm/vmm 设备) ===

    /// 释放一个物理页 (来自内核栈)。
    ///
    /// # Safety (内部)
    /// - `phys` 必须为 `pmm_alloc_page` 返回的合法物理页基址。
    pub fn free_phys_page(phys: *mut u8) {
        // SAFETY: 物理页所有权从分配者转移给本 free。
        unsafe { pmm_free_page(phys) }
    }

    /// 销毁用户页表 (cr3)。
    ///
    /// # Safety (内部)
    /// - `cr3` 必须为 `vmm_create_user_page_table` 返回的合法页表基址。
    pub fn destroy_user_page_table(cr3: u64) {
        // SAFETY: cr3 是 vmm_create_user_page_table 创建的, 调用方负责所有权释放。
        unsafe { vmm_destroy_page_table(cr3) }
    }

    /// 查询用户页表中虚拟地址对应的物理地址。
    ///
    /// # Safety (内部)
    /// - `cr3` 必须是有效的页表基址。
    pub fn virt_to_phys(cr3: u64, vaddr: u64) -> u64 {
        // SAFETY: cr3 来自 user proc 的 cr3 字段, 已由 vmm_create_user_page_table 创建。
        unsafe { vmm_get_physical_in_table(cr3, vaddr) }
    }

    /// 创建用户页表。
    pub fn create_user_page_table() -> u64 {
        // SAFETY: FFI 调用, 内部保证返回非零 cr3 或 0 表示失败。
        unsafe { vmm_create_user_page_table() }
    }

    /// 分配多页连续物理页。
    ///
    /// # Safety (内部)
    /// - 调用方负责通过 `free_phys_pages` 释放。
    pub fn alloc_phys_pages(count: u64) -> *mut u8 {
        // SAFETY: 物理页分配, 调用方负责所有权。
        unsafe { pmm_alloc_pages(count) }
    }

    /// 释放多页连续物理页。
    pub fn free_phys_pages(pages: *mut u8, count: u64) {
        for i in 0..count {
            raw::free_phys_page((pages as u64 + i * PAGE_SIZE) as *mut u8);
        }
    }

    /// 分配一页物理页。
    pub fn alloc_phys_page() -> *mut u8 {
        // SAFETY: 物理页分配, 调用方负责所有权。
        unsafe { pmm_alloc_page() }
    }

    /// 在用户页表中建立映射。
    pub fn vmm_map_user_page(cr3: u64, vaddr: u64, paddr: u64, flags: u64) {
        // SAFETY: cr3 来自 user proc 的 cr3 字段, 已建立。
        unsafe {
            // 整个用户页映射操作保持中断禁用，防止 timer 中断在 VMM 操作间干扰
            let saved_if = crate::arch!(interrupt_disable()) as u64;

            vmm_map_page_in_table(cr3, vaddr, paddr, flags);

            vmm_map_page(vaddr, paddr, flags);

            vmm_ensure_path_user(vaddr);

            crate::arch!(interrupt_restore(saved_if as usize));
        }
    }

    /// 写一个 u8 到用户页表中的某个字节。
    pub fn write_user_byte(cr3: u64, off: usize, v: u8) {
        // SAFETY: vmm_get_physical_in_table 保证返回的物理页对应 vaddr, KERNEL_BASE 偏移后内核可访问。
        unsafe {
            let phys = vmm_get_physical_in_table(cr3, off as u64 & !(PAGE_SIZE - 1));
            if phys != 0 {
                let addr = (phys + KERNEL_BASE + (off as u64 & 0xFFF)) as *mut u8;
                *addr = v;
            }
        }
    }

    /// 写一个 u64 到用户页表中的某个偏移 (unaligned)。
    pub fn write_user_u64(cr3: u64, off: usize, v: u64) {
        // SAFETY: vmm_get_physical_in_table 保证返回的物理页对应 vaddr, KERNEL_BASE 偏移后内核可访问。
        unsafe {
            let phys = vmm_get_physical_in_table(cr3, off as u64 & !(PAGE_SIZE - 1));
            if phys != 0 {
                let ptr = (phys + KERNEL_BASE + (off as u64 & 0xFFF)) as *mut u64;
                core::ptr::write_unaligned(ptr, v);
            }
        }
    }

    /// 读一个字节 (从用户态指针) - 内部 unsafe 封装。
    pub fn read_byte_from_user_ptr(src: *const u8, j: usize) -> u8 {
        // SAFETY: 由调用方保证 src 在 [src, src+j+1) 区间内可读。
        unsafe { *src.add(j) }
    }

    /// 物理页零初始化 + 映射到用户页表。
    pub fn alloc_zeroed_user_page(cr3: u64, vaddr: u64, flags: u64) -> *mut u8 {
        let page = raw::alloc_phys_page();
        if page.is_null() {
            return page;
        }
        // SAFETY: page 来自 alloc_phys_page, 大小为 PAGE_SIZE。
        unsafe {
            memset(page.cast::<core::ffi::c_void>(), 0, PAGE_SIZE as usize);
        }
        raw::vmm_map_user_page(cr3, vaddr, page as u64, flags);
        page
    }

    /// 释放物理页 (用于 ELF 加载失败回滚)。
    pub fn free_phys_page_for_rollback(phys: u64) {
        // SAFETY: 物理页所有权从分配者转移给本 free。
        unsafe { pmm_free_page(phys as *mut u8) }
    }

    /// 从 ELF 文件复制 chunk 到用户物理页 (通过内核映射)。
    pub fn elf_chunk_copy(
        page_phys: u64,
        off_in_page: u64,
        elf_data: *const u8,
        src_off: usize,
        chunk: u64,
    ) {
        // SAFETY: 物理页 + KERNEL_BASE 偏移后内核可写, elf_data 区间内可读。
        unsafe {
            let dest = raw::phys_to_kern_mut(page_phys, off_in_page);
            let src = raw::elf_ptr_at(elf_data, src_off);
            memcpy(
                dest.cast::<core::ffi::c_void>(),
                src.cast::<core::ffi::c_void>(),
                chunk as usize,
            );
        }
    }

    /// 映射单个物理页到用户页表 (用于代码段加载)。
    pub fn map_code_page(cr3: u64, vaddr: u64, page_phys: u64) {
        // SAFETY: 物理页已分配, flags = R|X 简化形式。
        let flags = (PageFlags::PRESENT | PageFlags::USER).bits();
        // SAFETY: cr3 已建立, page_phys 来自 pmm_alloc_page。
        unsafe {
            vmm_map_page_in_table(cr3, vaddr, page_phys, flags);
            vmm_map_page(vaddr, page_phys, flags);
            vmm_ensure_path_user(vaddr);
        }
    }

    /// 用户进程代码页分配 + 清零。
    pub fn alloc_code_page() -> *mut u8 {
        let page = raw::alloc_phys_page();
        if !page.is_null() {
            // SAFETY: page 来自 alloc_phys_page, 大小为 PAGE_SIZE。
            unsafe {
                memset(page.cast::<core::ffi::c_void>(), 0, PAGE_SIZE as usize);
            }
        }
        page
    }

    /// 物理页 → 内核可写指针 (用于代码段 chunk 复制)。
    pub fn phys_to_kern_mut(phys: u64, off: u64) -> *mut u8 {
        (phys + KERNEL_BASE + off) as *mut u8
    }

    /// ELF 文件指针 + 偏移。
    pub fn elf_ptr_at(elf_data: *const u8, off: usize) -> *const u8 {
        // SAFETY: 调用方保证 off 在 elf_size 范围内。
        unsafe { elf_data.add(off) }
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    /// 分配内存并清零 (类似 calloc)。
    pub fn alloc_zeroed(size: u64) -> *mut u8 {
        // SAFETY: kmalloc 由 kernel allocator 提供, 调用方负责释放。
        let ptr = unsafe { kmalloc(size) } as *mut u8;
        if !ptr.is_null() {
            // SAFETY: ptr 来自 kmalloc, 大小为 size, 清零区间 [ptr, ptr+size) 合法。
            unsafe {
                memset(ptr.cast::<core::ffi::c_void>(), 0, size as usize);
            }
        }
        ptr
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 分配并构造一个 `UserProcess` 内存, 清零后返回。
    ///
    /// # Arguments
    /// - `process`: 与本 `UserProcess` 镜像关联的权威 `Process` `NonNull` 句柄.
    ///              构造时写入 `UserProcess::process` 字段, 后续通过
    ///              `UserProcess::process()` 安全访问.
    ///
    /// # 返回
    /// 已清零的 `UserProcess` 裸指针; `process` 字段已正确指向传入的 `Process`.
    pub fn alloc_user_process(process: NonNull<Process>) -> Option<*mut UserProcess> {
        let size = core::mem::size_of::<UserProcess>() as u64;
        let ptr = raw::alloc_zeroed(size) as *mut UserProcess;
        if ptr.is_null() {
            None
        } else {
            // SAFETY: ptr 来自 alloc_zeroed, 大小为 size_of::<UserProcess>(), 区间合法.
            //         process NonNull 句柄由调用方保证有效 (INV-USER-PROC-2).
            unsafe {
                core::ptr::write(&mut (*ptr).process, process);
            }
            Some(ptr)
        }
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 分配并清零一个 `Process` (用于 process table)。
    pub fn alloc_kernel_process() -> Option<*mut Process> {
        let size = core::mem::size_of::<Process>() as u64;
        let ptr = raw::alloc_zeroed(size) as *mut Process;
        if ptr.is_null() { None } else { Some(ptr) }
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    /// 释放 `alloc_kernel_process` 分配的 `Process` 内存 (回滚路径专用).
    ///
    /// # Safety (内部)
    /// - `kproc_ptr` 必须为 `alloc_kernel_process` 返回的合法指针, 且未再被使用.
    ///   调用后该指针失效, 不得再次解引用.
    /// - 仅用于 `UserProcManager::create()` 的失败回滚路径; 成功路径下应
    ///   将其所有权转移给 `PROCESS_TABLE.insert` 或 `destroy` 链路.
    pub fn free_kernel_process(kproc_ptr: *mut Process) {
        if kproc_ptr.is_null() {
            return;
        }

        crate::privileged::mm::kfree(kproc_ptr as *mut u8);
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    /// 释放 `alloc_user_process` 分配的 `UserProcess` 镜像内存 (回滚路径专用).
    ///
    /// # Safety (内部)
    /// - `proc_ptr` 必须为 `alloc_user_process` 返回的合法指针, 且未再被使用.
    /// - 必须先于 `free_kernel_process` 调用 (LIFO 反序), 以避免
    ///   `UserProcess::process` `NonNull` 字段成为悬挂指针.
    pub fn free_user_process(proc_ptr: *mut UserProcess) {
        if proc_ptr.is_null() {
            return;
        }

        crate::privileged::mm::kfree(proc_ptr as *mut u8);
    }

    /// 从 PID/CR3 构造 `UserProcRef` 用于新创建进程。
    ///
    /// # Safety (内部)
    /// - `proc` 必须为 `alloc_user_process` 返回的合法指针, 拥有完整所有权。
    pub fn new_proc_ref(proc: *mut UserProcess) -> UserProcRef {
        // SAFETY: proc 来自 alloc_user_process, 指向有效 UserProcess 分配。
        unsafe { UserProcRef::new_unchecked(proc) }
    }

    /// 在已清零的 `Process` 内存上写入基本字段 (避免业务逻辑中的 `unsafe`)。
    ///
    /// # Safety (内部)
    /// - `kproc_ptr` 必须为 `alloc_kernel_process` 返回的合法指针, 已被清零。
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    pub fn init_kernel_process_fields(
        kproc_ptr: *mut Process,
        pid: u32,
        pwm: u64,
        cr3: u64,
        kstack: u64,
        ustack: u64,
    ) {
        use crate::privileged::proc::SchedPolicy;
        // SAFETY: kproc_ptr 来自 alloc_kernel_process, 已清零, 字段可被 ptr::write 覆盖。
        unsafe {
            core::ptr::write(&mut (*kproc_ptr).pid, ProcessId(pid));
            core::ptr::write(&mut (*kproc_ptr).pwm, AtomicU64::new(pwm));
            core::ptr::write(
                &mut (*kproc_ptr).state,
                AtomicU32::new(ProcessState::Ready as u32),
            );
            core::ptr::write(
                &mut (*kproc_ptr).priority,
                AtomicU32::new(ProcessPriority::Normal as u32),
            );
            core::ptr::write(&mut (*kproc_ptr).flags, AtomicU32::new(0));
            core::ptr::write(&mut (*kproc_ptr).parent, None);
            core::ptr::write(&mut (*kproc_ptr).cr3, AtomicU64::new(cr3));
            core::ptr::write(&mut (*kproc_ptr).kernel_stack, AtomicU64::new(kstack));
            core::ptr::write(&mut (*kproc_ptr).user_stack, AtomicU64::new(ustack));
            core::ptr::write(&mut (*kproc_ptr).exit_code, AtomicU32::new(0));
            // P1 修复: 本路径经 alloc_zeroed 分配 (不走 Process::new), 不写此字段则
            // ref_count 保持 0; 而 `remove_and_free`/`dec_ref_and_maybe_free` 以
            // "减后归零" 为释放判据 ⇒ 0 上做减法回绕到 u32::MAX, Box::from_raw
            // 永不执行, Process::drop 不运行, PID 不回收. 显式置 1 使首个 dec 归零.
            core::ptr::write(&mut (*kproc_ptr).ref_count, AtomicU32::new(1));
            core::ptr::write(&mut (*kproc_ptr).cpu_time, AtomicU64::new(0));
            core::ptr::write(&mut (*kproc_ptr).block_reason, AtomicU32::new(0));
            core::ptr::write(
                &mut (*kproc_ptr).sched_policy,
                AtomicU32::new(SchedPolicy::Normal as u32),
            );
            (*kproc_ptr).rt_priority.store(0, Ordering::SeqCst);
            (*kproc_ptr).session_id.store(0, Ordering::SeqCst);
            (*kproc_ptr).sleep_until.store(0, Ordering::SeqCst);
            // 零初始化包含 alloc 的字段 (Mutex/Vec/String), 保持有效空状态
            core::ptr::write_bytes(
                &mut (*kproc_ptr).name as *mut _ as *mut u8,
                0,
                core::mem::size_of::<Mutex<String>>(),
            );
            core::ptr::write_bytes(
                &mut (*kproc_ptr).children as *mut _ as *mut u8,
                0,
                core::mem::size_of::<Mutex<Vec<ProcessId>>>(),
            );
            core::ptr::write_bytes(
                &mut (*kproc_ptr).context as *mut _ as *mut u8,
                0,
                core::mem::size_of::<Mutex<ProcessContext>>(),
            );
            // fd_table 不能零初始化: entries 全 0 会被 FdTable 误判为
            // "全 64 个 slot 已占用 handle_id=0". 必须显式初始化为空闲态 (u32::MAX).
            // 先例同下方 `namespaces` (同样用 ptr::write 覆盖零初始值).
            core::ptr::write(&mut (*kproc_ptr).fd_table, FdTable::new());
            // TRACK-INIT-RING3-FORK: alloc_kernel_process 清零分配 (不走 Process::new),
            // 因此 `namespaces` (Mutex<NamespaceSet>) 保持全零 (7 个 NULL Arc).
            // fork 时 NamespaceSet::fork_from 对 NULL Arc 执行 Arc::clone → 地址 0 递增
            // 计数 → 溢出 abort (ud2). 必须显式初始化为 init namespace 集合.
            core::ptr::write(
                &mut (*kproc_ptr).namespaces,
                Mutex::new(crate::privileged::proc::NamespaceSet::new_init()),
            );
        }
    }
}

use raw::UserProcRef;

/// 用户进程 FFI 桥接缓存 (Framekernel privilege wrapper)
///
/// # 设计: 单源真相 + FFI 镜像
///
/// `Edgine` 进程子系统维护**两个并行结构**:
/// - `Process` (在 `process.rs` 中, 权威单一源) — 全量进程描述符, 包含调度/
///   信号/文件系统/会话等所有元数据, 由 `PROCESS_TABLE` 管理.
/// - `UserProcess` (本结构, FFI 镜像) — 仅缓存进入 Ring 3 路径上**热访问**的
///   字段 (`pid/pwm/cr3/kernel_stack/user_stack/state`), 以及**独占**的 FFI 字段
///   (`entry`、`stack_bottom`、`create_time`).
///
/// # 字段分类
///
/// ## 共享字段 (与 `Process` 重叠, 同步方向: Process → `UserProcess`)
///
/// - `pid`           对应 `Process::pid`
/// - `pwm`           对应 `Process::pwm` (类型 `AtomicU64`)
/// - `cr3`           对应 `Process::cr3` (类型 `AtomicU64`)
/// - `kernel_stack`  对应 `Process::kernel_stack` (类型 `AtomicU64`)
/// - `user_stack`    对应 `Process::user_stack` (类型 `AtomicU64`)
/// - `state`         对应 `Process::state` (类型 `AtomicU32`)
///
/// 共享字段在 `create()` 完成后即与 `Process` 镜像同步; 运行期状态变更
/// 优先写 `Process`, 然后通过 `sync_from_process()` 推送到本镜像.
///
/// ## FFI 独占字段 (本结构独有, 不存在于 `Process`)
///
/// - `entry`         — asm 跳转入口, 由 `enter()` 读取并跳转
/// - `stack_bottom`  — 用户栈底地址, `setup_user_stack()` 计算依据
/// - `create_time`   — 进程创建时间戳, 调度/审计用
///
/// # 不变量 (INV-USER-PROC)
///
/// 1. **同步不变量**: 本结构共享字段值与 `Process` 对应字段**最终一致**.
///    同步通过 `sync_to_process()` / `sync_from_process()` 显式调用完成.
/// 2. **生命周期不变量**: `process` `NonNull` 指向的 `Process` 存活期 ≥ 本结构.
///    销毁本结构前必须先从 `USER_PROC_MANAGER.processes` 移除条目.
/// 3. **FFI 安全不变量**: `#[repr(C)]` 保持稳定的内存布局, 避免跨 FFI 边界时
///    Rust 端重新布局导致 C 端解析错误.
// SAFETY: UserProcess::process 字段存储 NonNull<Process> 句柄, 不持有所有权.
//         共享字段 (pid/pwm/cr3/kstack/ustack/state) 均为 Atomic* 或 u32, 满足 Send.
//         FFI 独占字段 (entry/stack_bottom/create_time) 均为 u64, 满足 Send.
// SAFETY: UserProcess 含裸指针, 但所有可变访问通过 USER_PROC_MANAGER 锁保护;
//         裸指针仅在锁内解引用, 不会跨线程无锁访问.
//         综合: UserProcess 可在线程间安全转移.
unsafe impl Send for UserProcess {}
// SAFETY: 同上, Sync 安全性由外部锁保证.
unsafe impl Sync for UserProcess {}
/// 用户态进程句柄 — 封装用户态进程特有的 FFI 独占状态, 并持有指向权威 [`Process`] 的引用。
#[repr(C)]
pub struct UserProcess {
    /// ✅ 权威引用: 指向 `PROCESS_TABLE` 中对应的 `Process`.
    ///
    /// 持有 `NonNull` 而非裸指针, 表达"一定有值"的语义;
    /// 构造时强制调用方提供 `Process` 句柄, 杜绝悬垂.
    pub(crate) process: NonNull<Process>,

    // === FFI 独占字段 (用户态特有, 不存在于 Process) ===
    /// asm 跳转入口地址 (由 `enter()` 读取并执行 `jmp entry`).
    pub entry: u64,
    /// 用户栈底虚拟地址, `setup_user_stack()` 据此计算 argv/envp 摆放位置.
    pub stack_bottom: AtomicU64,
    /// 进程创建时间戳 (ticks). 调度/审计用, 不属于 Process 状态.
    pub create_time: u64,
}

impl UserProcess {
    /// 获取权威 `Process` 引用.
    ///
    /// # Returns
    /// 对 `PROCESS_TABLE` 中存储的 `Process` 的 `&'static` 引用 (非空保证由
    /// `NonNull` 字段提供).
    pub fn process(&self) -> &Process {
        // SAFETY: UserProcess::process NonNull 字段的不变量 (INV-USER-PROC-2)
        // 保证其指向的 Process 在 UserProcess 存活期间有效.
        unsafe { self.process.as_ref() }
    }
}

/// 用户进程管理器 — 维护 pid 到 [`UserProcess`] 的映射, 并跟踪当前进程。
pub struct UserProcManager {
    current: AtomicU64,
    // 使用 NonNull<UserProcess> 替代 *mut UserProcess,
    // 使 BTreeMap 自动实现 Send + Sync (NonNull: Send + Sync when T: Send + Sync)。
    processes: Mutex<alloc::collections::BTreeMap<u32, NonNull<UserProcess>>>,
}

// SAFETY: UserProcManager 始终通过静态 USER_PROC_MANAGER 访问.
// 所有变更都走 Mutex, NonNull 指针指向的 UserProcess 对象
// SAFETY: UserProcManager 含裸指针 HashMap, 但所有访问通过自身 Mutex 保护.
//         字段均为 Atomic* 或普通整数.
unsafe impl Send for UserProcManager {}
// SAFETY: 同上, 外部锁保证并发安全.
unsafe impl Sync for UserProcManager {}

impl UserProcManager {
    pub const fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            processes: Mutex::new(alloc::collections::BTreeMap::new()),
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn init(&self) {}

    fn destroy(&self, proc: NonNull<UserProcess>, keep_kstack: bool) {
        // SAFETY: proc 是 NonNull<UserProcess>, 由 kmalloc 分配并插入
        // BTreeMap, 在 destroy 前始终有效.
        let proc_ref = unsafe { raw::UserProcRef::new_unchecked(proc.as_ptr()) };

        // 仅销毁已退出的进程
        if !proc_ref.is_exited() {
            return;
        }

        let cr3 = proc_ref.load_cr3();

        // 释放用户栈物理页 — 必须在销毁页表**之前**完成: 页表销毁后 `cr3` 不再
        // 可翻译用户虚拟地址, 再释放会静默漏释放用户栈 (与 replace_user_space 同序).
        let ustack = proc_ref.load_user_stack();
        if ustack != 0 {
            let stack_virt = USER_STACK_TOP - USER_STACK_SIZE - USER_STACK_GUARD;
            for i in 0..(USER_STACK_SIZE / PAGE_SIZE) {
                let svirt = stack_virt + USER_STACK_GUARD + i * PAGE_SIZE;
                let phys = raw::virt_to_phys(cr3, svirt);
                if phys != 0 {
                    raw::free_phys_page(phys as *mut u8);
                }
            }
        }

        // 计数归零才销毁: cr3 可能被 CLONE_VM 兄弟进程共享, 或已被 execve
        // 转移给其他进程 (转移时源已清空, 见 proc_ops::proc_exec_replace).
        if cr3 != 0
            && crate::privileged::mm::pmm::get_pmm().frame_dec(crate::privileged::mm::PhysAddr(cr3))
        {
            raw::destroy_user_page_table(cr3);
        }
        if !keep_kstack {
            let kstack = proc_ref.load_kernel_stack();
            if kstack != 0 {
                let kstack_base_virt = kstack - USER_KSTACK_SIZE;
                let kstack_base_phys = kstack_base_virt - KERNEL_BASE;
                raw::free_phys_pages(kstack_base_phys as *mut u8, USER_KSTACK_SIZE / PAGE_SIZE);
            }
        }
        let pid = proc_ref.pid();
        self.processes.lock().remove(&pid);
    }

    /// 销毁进程 (接受裸指针的兼容接口)
    fn destroy_raw(&self, proc: *mut UserProcess, keep_kstack: bool) {
        if let Some(nn) = NonNull::new(proc) {
            self.destroy(nn, keep_kstack);
        }
    }

    /// 获取进程裸指针 (向后兼容接口)。
    /// 内部存储为 `NonNull`, 转为 *mut 供外部调用。
    pub fn get(&self, pid: u32) -> Option<*mut UserProcess> {
        self.processes.lock().get(&pid).map(|n| n.as_ptr())
    }

    /// 通过闭包安全访问进程
    pub fn with_process<F, R>(&self, pid: u32, f: F) -> Option<R>
    where
        F: FnOnce(&UserProcess) -> R,
    {
        let processes = self.processes.lock();
        processes.get(&pid).map(|ptr| {
            // SAFETY: ptr 来自 BTreeMap 中的 NonNull<UserProcess>.
            // 进程存活于 manager 的整个生命周期; 持锁时
            // 进程永不被释放.
            f(raw::deref_non_null(*ptr))
        })
    }

    pub fn destroy_by_pid(&self, pid: u32) {
        // 先在独立语句中取句柄 (guard 随该语句结束释放), 再进入 destroy —
        // destroy 内部会重新获取同一把 IrqSpinLock; 若把取句柄写在 `if let`
        // 的 scrutinee 中, guard 会存活到 then 块结束, 造成同锁自锁死.
        let proc = self.processes.lock().get(&pid).copied();
        if let Some(proc) = proc {
            // SAFETY: proc 来自 BTreeMap 中的 NonNull, 进程存活期间有效.
            let proc_ref = unsafe { raw::UserProcRef::new_unchecked(proc.as_ptr()) };
            // 仅销毁非运行状态的进程 (Zombie/Terminated)
            if !proc_ref.is_running() {
                self.destroy(proc, false);
            }
        }
    }

    pub fn destroy_by_pid_no_kstack(&self, pid: u32) {
        // 同 destroy_by_pid: 取句柄与 destroy 的锁必须分离, 避免同锁自锁死.
        let proc = self.processes.lock().get(&pid).copied();
        if let Some(proc) = proc {
            // SAFETY: proc 来自 BTreeMap 中的 NonNull, 进程存活期间有效.
            let proc_ref = unsafe { raw::UserProcRef::new_unchecked(proc.as_ptr()) };
            // 仅销毁已退出的进程
            if proc_ref.is_exited() {
                self.destroy(proc, true);
            }
        }
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 替换进程的用户地址空间 (execve 路径).
    ///
    /// 销毁旧页表和用户栈物理页, 然后将 `CR3/entry/user_stack/stack_bottom`
    /// 更新为新值. 不移除 `BTreeMap` 条目, 不释放内核栈, 不改变 PID —
    /// 保持 POSIX execve 语义 (PID 不变).
    pub fn replace_user_space(
        &self,
        pid: u32,
        new_cr3: u64,
        new_entry: u64,
        new_user_stack: u64,
        new_stack_bottom: u64,
    ) {
        let proc = match self.processes.lock().get(&pid).copied() {
            Some(p) => p,
            None => return,
        };
        // SAFETY: proc 来自 BTreeMap 中的 NonNull, 进程存活期间有效.
        let proc_ref = unsafe { UserProcRef::new_unchecked(proc.as_ptr()) };

        // 1. 销毁旧用户页表 (计数归零才销毁: 旧表可能被 CLONE_VM 兄弟进程共享)
        let old_cr3 = proc_ref.load_cr3();
        if old_cr3 != 0 {
            // 释放旧用户栈物理页 (必须在销毁页表前完成, 否则无法翻译虚拟地址)
            let old_stack_bottom = proc_ref.load_stack_bottom();
            if old_stack_bottom != 0 {
                for i in 0..(USER_STACK_SIZE / PAGE_SIZE) {
                    let svirt = old_stack_bottom + i * PAGE_SIZE;
                    let phys = raw::virt_to_phys(old_cr3, svirt);
                    if phys != 0 {
                        raw::free_phys_page(phys as *mut u8);
                    }
                }
            }
            if crate::privileged::mm::pmm::get_pmm()
                .frame_dec(crate::privileged::mm::PhysAddr(old_cr3))
            {
                raw::destroy_user_page_table(old_cr3);
            }
        }

        // 2. 更新为新的地址空间 (UserProcRef 已委托到 Process, 无需重复写入)
        proc_ref.store_cr3(new_cr3);
        proc_ref.set_entry(new_entry);
        proc_ref.store_user_stack(new_user_stack);
        proc_ref.store_stack_bottom(new_stack_bottom);
    }

    /// 从管理器中移除进程索引但不释放任何资源.
    /// 用于 execve: 新进程资源已转移到旧 PID, 仅需移除索引条目.
    pub fn detach_by_pid(&self, pid: u32) {
        self.processes.lock().remove(&pid);
    }

    pub fn create(&self, info: &UserProcInfo, pwm: u64) -> Option<*mut UserProcess> {
        // ✅ 单源真相: 优先分配权威 Process, 再分配 UserProcess 镜像并关联.
        //    此顺序保证 UserProcess::process NonNull 字段构造时即指向有效 Process.
        let kproc_ptr = raw::alloc_kernel_process()?;
        // SAFETY: alloc_kernel_process 成功时保证返回非空指针
        let kproc_nn = NonNull::new(kproc_ptr)?;

        // 分配并清零 UserProcess 内存, 关联权威 Process 句柄
        let proc_ptr = raw::alloc_user_process(kproc_nn)?;
        let proc = raw::new_proc_ref(proc_ptr);

        // 创建用户页表 (暂存到局部变量, 稍后通过 init_kernel_process_fields 写入 Process)
        let cr3_val = raw::create_user_page_table();
        if cr3_val == 0 {
            // 失败回滚 (DECISION-027): 页表创建失败, 必须释放已分配的
            // UserProcess + Process 内存. 顺序为 LIFO 反序: 先 UserProcess
            // (避免 process NonNull 字段成为悬挂), 再 Process.
            raw::free_user_process(proc_ptr);
            raw::free_kernel_process(kproc_ptr);
            return None;
        }

        // 分配用户栈
        let stack_pages = raw::alloc_phys_pages((USER_STACK_SIZE + USER_STACK_GUARD) / PAGE_SIZE);
        if stack_pages.is_null() {
            // 失败回滚: 用户栈物理页分配失败, 销毁页表并释放结构内存.
            if crate::privileged::mm::pmm::get_pmm()
                .frame_dec(crate::privileged::mm::PhysAddr(cr3_val))
            {
                raw::destroy_user_page_table(cr3_val);
            }
            raw::free_user_process(proc_ptr);
            raw::free_kernel_process(kproc_ptr);
            return None;
        }

        let stack_phys = stack_pages as u64;
        // ASLR: 随机化栈顶地址
        let aslr_stack_top = crate::privileged::config::aslr_stack_top();
        let stack_virt = aslr_stack_top - USER_STACK_SIZE - USER_STACK_GUARD;

        crate::klog_boot_info!(
            "[USER] create: mapping user stack: aslr_top={:#X} stack_virt={:#X} stack_phys={:#X} pages={}",
            aslr_stack_top,
            stack_virt,
            stack_phys,
            USER_STACK_SIZE / PAGE_SIZE
        );

        for i in 0..(USER_STACK_SIZE / PAGE_SIZE) {
            let svirt = stack_virt + USER_STACK_GUARD + i * PAGE_SIZE;
            let sphys = stack_phys + i * PAGE_SIZE;

            raw::vmm_map_user_page(
                cr3_val,
                svirt,
                sphys,
                (PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER).bits(),
            );
        }

        // 验证用户栈映射是否成功
        let check_virt = aslr_stack_top - PAGE_SIZE; // 检查栈顶第一页
        let check_phys = raw::virt_to_phys(cr3_val, check_virt);
        crate::klog_boot_info!(
            "[USER] create: verify stack mapping: virt={:#X} -> phys={:#X} {}",
            check_virt,
            check_phys,
            if check_phys != 0 { "✓" } else { "✗ FAILED" }
        );

        // 关键修复: user_stack 必须指向栈顶的**已映射地址**，而非 guard page 边界。
        // aslr_stack_top 是栈顶边界（未映射），iretq 后第一次 push 会触发 #PF。
        // 已映射栈页范围: [stack_virt + USER_STACK_GUARD, stack_virt + USER_STACK_GUARD + USER_STACK_SIZE)
        // 栈顶已映射地址 = stack_virt + USER_STACK_GUARD + USER_STACK_SIZE - 8（考虑 8 字节对齐）。
        let initial_rsp = stack_virt + USER_STACK_GUARD + USER_STACK_SIZE - 8;
        let initial_stack_bottom = stack_virt + USER_STACK_GUARD;

        // 分配内核栈
        let kstack = raw::alloc_phys_pages(USER_KSTACK_SIZE / PAGE_SIZE);
        if kstack.is_null() {
            // 失败回滚: 内核栈物理页分配失败, 释放栈页+页表+结构内存.
            // 顺序仍为 LIFO 反序: 物理资源 → 镜像 → 权威结构.
            raw::free_phys_page(stack_pages);
            if crate::privileged::mm::pmm::get_pmm()
                .frame_dec(crate::privileged::mm::PhysAddr(cr3_val))
            {
                raw::destroy_user_page_table(cr3_val);
            }
            raw::free_user_process(proc_ptr);
            raw::free_kernel_process(kproc_ptr);
            return None;
        }
        let kstack_top = kstack as u64 + KERNEL_BASE + USER_KSTACK_SIZE;
        crate::klog_boot_info!(
            "[USER] create: kstack_phys={:#X} kstack_top={:#X} (KERNEL_BASE={:#X})",
            kstack as u64,
            kstack_top,
            KERNEL_BASE
        );
        crate::privileged::proc::kernel_stack_write_canary(kstack_top);

        // ✅ PID 分配延后到所有内存/页表/栈资源就绪后:
        //   避免 `alloc_kernel_process` 或 `alloc_user_process` 失败时, 已分配
        //   的 PID 留在 next_pid 计数器中造成 PID 泄漏. 早期失败 (页表/栈分配)
        //   只回滚物理页与页表, 不需要回滚 PID.
        let pid = PROCESS_TABLE.allocate_pid()?;

        // 在权威 Process 上批量初始化基本字段 (通过 init_kernel_process_fields)
        raw::init_kernel_process_fields(kproc_ptr, pid, pwm, cr3_val, kstack_top, initial_rsp);

        // 通过 UserProcRef 设置 UserProcess 独占字段
        proc.set_pid(pid);
        proc.set_entry(info.entry);
        proc.store_pwm(pwm);
        proc.store_state(1);
        proc.store_cr3(cr3_val);
        proc.store_kernel_stack(kstack_top);
        proc.store_user_stack(initial_rsp);
        proc.store_stack_bottom(initial_stack_bottom);
        proc.set_create_time(crate::privileged::timer::get_ticks());

        self.processes.lock().insert(pid, NonNull::new(proc_ptr)?);

        // 插入 PROCESS_TABLE 完成权威注册.
        PROCESS_TABLE.insert(kproc_ptr);

        Some(proc_ptr)
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    pub fn enter(&self, proc: *mut UserProcess) {
        crate::klog_boot_info!("[USER] enter() called with proc={:#X}", proc as u64);
        if proc.is_null() {
            crate::klog_boot_info!("[USER] enter() proc is null, returning");
            return;
        }

        // SAFETY: proc 由调用方保证为非空、生命周期有效 (BTreeMap 中已存在)。
        let proc_ref = unsafe { UserProcRef::new_unchecked(proc) };
        self.current.store(proc as u64, Ordering::SeqCst);
        proc_ref.store_state(2);

        let kstack = proc_ref.load_kernel_stack();
        let rip_val = proc_ref.entry();
        let rsp_val = proc_ref.load_user_stack();
        let cr3 = proc_ref.load_cr3();

        // 调试日志: 追踪 kstack 值
        crate::klog_boot_info!(
            "[USER] enter: kstack={:#X} rip={:#X} rsp={:#X} cr3={:#X}",
            kstack,
            rip_val,
            rsp_val,
            cr3
        );
        let _ss_val = GDT_USER_DATA | 0x03;
        let _cs_val = GDT_USER_CODE | 0x03;
        let _rflags_val: u64 = 0x3202;

        crate::privileged::cpu::arch::set_kernel_stack(kstack);

        // 更新 per-CPU 用户页表 CR3, 使 syscall/中断返回用户态时
        // 从 [gs:USER_PML4_OFF] 读取到正确的进程用户页表.
        // SAFETY: cr3 是当前进程的有效用户页表 PML4 物理地址;
        // 当前在调度器上下文, 独占访问 per-CPU 数据.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            crate::privileged::arch::gdt::gdt_set_user_cr3(cr3);
        }
        // aarch64 无 per-CPU 用户页表 CR3 更新路径 (cr3 仅参与上方日志)
        #[cfg(not(target_arch = "x86_64"))]
        let _ = cr3;

        // 将本任务的内核栈顶页映射到其用户页表 (KPTI-08).
        // 用户态→内核态入口在切换 CR3 **之前**就要用 TSS.RSP0 压入 5 项 iretq 帧,
        // 故内核栈顶页必须在用户页表中可见. 映射面收窄后不再继承高半区别名,
        // 必须显式映射 —— 统一走 `kpti::map_rsp0_page` (与调度器切换路径同源).
        // 权限 PRESENT|WRITABLE 且**不设 USER** (访问路径 CPL 恒为 0, 设 USER 即提权).
        // 仅 x86_64 需要 (TSS RSP0 机制); aarch64 的异常入口先切 TTBR1 再压帧,
        // 内核栈只经 TTBR1 可达, 无需在用户页表内为其保留任何映射.
        #[cfg(target_arch = "x86_64")]
        // SAFETY: cr3 是本进程有效用户页表 PML4 物理地址 (低 12 位可为 PCID);
        // kstack 是本进程内核栈顶的高半区 VA (页对齐). 当前在调度器上下文,
        // 独占访问 per-CPU 数据.
        unsafe {
            crate::privileged::mm::map_rsp0_page(cr3, kstack);
        }

        // 注意: 不再需要低地址恒等映射。
        // enter_user_asm 在切换 CR3 前已清除所有寄存器 (包括 RBP/RSP),
        // 切换后不会残留低地址引用。

        // 自检式调试: 验证用户页表关键映射
        #[cfg(target_arch = "x86_64")]
        {
            let vmm = crate::privileged::mm::get_vmm();

            // 检查用户代码页 (0x400000) — 含 PTE 权限位自检
            let code_page_virt = rip_val & !(PAGE_SIZE - 1);
            if let Some(phys) =
                vmm.get_physical_in_pml4(cr3, crate::privileged::mm::VirtAddr(code_page_virt))
            {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: user_code virt={:#X} -> phys={:#X} ✓",
                    code_page_virt,
                    phys.0
                );
                // 检查 PTE 原始值: 验证 PRESENT/USER/NX 位
                if let Some(pte_raw) =
                    vmm.get_pte_value(cr3, crate::privileged::mm::VirtAddr(code_page_virt))
                {
                    let present = (pte_raw & 0x001) != 0;
                    let writable = (pte_raw & 0x002) != 0;
                    let user = (pte_raw & 0x004) != 0;
                    let nx = (pte_raw & (1u64 << 63)) != 0;
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK: user_code PTE={:#018X} P={} W={} U={} NX={} (NX must be 0 for exec)",
                        pte_raw,
                        u8::from(present),
                        u8::from(writable),
                        u8::from(user),
                        u8::from(nx)
                    );
                    if nx {
                        crate::klog_boot_info!(
                            "[USER] SELF-CHECK: *** BUG: user_code page has NX=1! CPU cannot execute! ***"
                        );
                    }
                    if !user {
                        crate::klog_boot_info!(
                            "[USER] SELF-CHECK: *** BUG: user_code page has U=0! Ring 3 cannot access! ***"
                        );
                    }
                } else {
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK: user_code PTE: could not read (page table level missing)"
                    );
                }
            } else {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: user_code virt={:#X} NOT MAPPED ✗",
                    code_page_virt
                );
            }

            // 检查用户栈页 (第一次压栈将访问 rsp - 8 所在页)
            let first_access_virt = (rsp_val - 8) & !(PAGE_SIZE - 1);
            if let Some(phys) =
                vmm.get_physical_in_pml4(cr3, crate::privileged::mm::VirtAddr(first_access_virt))
            {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: user_stack_first_access virt={:#X} (rsp-8={:#X}) -> phys={:#X} ✓",
                    first_access_virt,
                    rsp_val - 8,
                    phys.0
                );
            } else {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: user_stack_first_access virt={:#X} (rsp-8={:#X}) NOT MAPPED ✗",
                    first_access_virt,
                    rsp_val - 8
                );
            }

            // 检查 guard page (应保持未映射)
            // 已映射栈页范围 = [stack_bottom, stack_bottom + USER_STACK_SIZE),
            // guard 页即其下方一页 (stack_bottom - USER_STACK_GUARD).
            // 注意: `initial_rsp` = 栈顶边界 - 8 (见 `create`), 即 rsp 恒落在栈顶
            // 已映射页内, 故不能以 "含 rsp 的页" 判定 guard —— 该判据恒报 unexpected.
            let guard_page = proc_ref
                .load_stack_bottom()
                .saturating_sub(USER_STACK_GUARD);
            if let Some(phys) =
                vmm.get_physical_in_pml4(cr3, crate::privileged::mm::VirtAddr(guard_page))
            {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: user_stack_guard_page virt={:#X} -> phys={:#X} (unexpected: should be unmapped)",
                    guard_page,
                    phys.0
                );
            } else {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: user_stack_guard_page virt={:#X} NOT MAPPED (expected: guard page) ✓",
                    guard_page
                );
            }

            // 检查内核栈页 (RSP0, iretq 帧所在页)
            let rsp0_check_virt = (kstack - 40) & !(PAGE_SIZE - 1);
            if let Some(phys) =
                vmm.get_physical_in_pml4(cr3, crate::privileged::mm::VirtAddr(rsp0_check_virt))
            {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: rsp0_stack virt={:#X} -> phys={:#X} ✓",
                    rsp0_check_virt,
                    phys.0
                );
            } else {
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: rsp0_stack virt={:#X} NOT MAPPED ✗",
                    rsp0_check_virt
                );
            }

            crate::klog_boot_info!(
                "[USER] SELF-CHECK: entering user mode with rip={:#X} rsp={:#X} cr3={:#X}",
                rip_val,
                rsp_val,
                cr3
            );

            // 扩展自检: 验证 GDT 段描述符
            // 用户代码段 CS = 0x23 (GDT_USER_CODE | 0x03)
            // 用户数据段 SS = 0x1B (GDT_USER_DATA | 0x03)
            crate::klog_boot_info!(
                "[USER] SELF-CHECK: GDT segments: CS={:#X} (expect 0x23) SS={:#X} (expect 0x1B)",
                GDT_USER_CODE | 0x03,
                GDT_USER_DATA | 0x03
            );

            // 扩展自检: 验证 iretq 帧参数
            // iretq 从栈上恢复: RIP, CS, RFLAGS, RSP, SS
            crate::klog_boot_info!(
                "[USER] SELF-CHECK: iretq frame: RIP={:#X} CS={:#X} RFLAGS={:#X} RSP={:#X} SS={:#X}",
                rip_val,
                GDT_USER_CODE | 0x03,
                0x202,
                rsp_val,
                GDT_USER_DATA | 0x03
            );

            // 扩展自检: 验证内核栈指针
            crate::klog_boot_info!(
                "[USER] SELF-CHECK: kstack={:#X} (high-half, will be used as RSP0)",
                kstack
            );

            // 扩展自检: 验证用户代码页内容 (检查是否有有效指令)
            let code_page_virt = rip_val & !(PAGE_SIZE - 1);
            if let Some(phys) =
                vmm.get_physical_in_pml4(cr3, crate::privileged::mm::VirtAddr(code_page_virt))
            {
                // 通过内核映射读取用户代码页内容
                let kernel_virt = phys.0 + crate::privileged::mm::KERNEL_BASE as u64;
                let code_ptr = kernel_virt as *const u8;
                // 读取前 16 字节作为指令样本
                let mut instr_sample = [0u8; 16];
                // SAFETY: code_ptr 指向已映射的物理页 (经 KERNEL_BASE 偏移后内核可访问), 读取 16 字节在页内。
                unsafe {
                    for i in 0..16 {
                        instr_sample[i] = *code_ptr.add(i);
                    }
                }
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK: user_code first 16 bytes: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                    instr_sample[0],
                    instr_sample[1],
                    instr_sample[2],
                    instr_sample[3],
                    instr_sample[4],
                    instr_sample[5],
                    instr_sample[6],
                    instr_sample[7],
                    instr_sample[8],
                    instr_sample[9],
                    instr_sample[10],
                    instr_sample[11],
                    instr_sample[12],
                    instr_sample[13],
                    instr_sample[14],
                    instr_sample[15]
                );
            }
        }

        // ═══ 自检式调试: SYSCALL/SYSRET 配置验证 ═══
        #[cfg(target_arch = "x86_64")]
        {
            const IA32_EFER: u32 = 0xC0000080;
            const IA32_STAR: u32 = 0xC0000081;
            const IA32_LSTAR: u32 = 0xC0000082;
            const IA32_SFMASK: u32 = 0xC0000084;
            const IA32_GS_BASE: u32 = 0xC0000101;
            const IA32_KERNEL_GS_BASE: u32 = 0xC0000102;

            // SAFETY: 仅读取 MSR, 无副作用, boot 阶段单线程
            unsafe {
                let efer = crate::privileged::cpu::msr::read_msr(IA32_EFER);
                let star = crate::privileged::cpu::msr::read_msr(IA32_STAR);
                let lstar = crate::privileged::cpu::msr::read_msr(IA32_LSTAR);
                let sfmask = crate::privileged::cpu::msr::read_msr(IA32_SFMASK);
                let gs_base = crate::privileged::cpu::msr::read_msr(IA32_GS_BASE);
                let kernel_gs_base = crate::privileged::cpu::msr::read_msr(IA32_KERNEL_GS_BASE);

                let sce = (efer & 1) != 0;
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK SYSCALL: EFER={:#x} SCE={} STAR={:#x} LSTAR={:#x} SFMASK={:#x}",
                    efer,
                    sce,
                    star,
                    lstar,
                    sfmask
                );
                if !sce {
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK: *** BUG: EFER.SCE=0! syscall instruction will #UD! ***"
                    );
                }

                crate::klog_boot_info!(
                    "[USER] SELF-CHECK GS (before enter_user_asm): IA32_GS_BASE={:#x} IA32_KERNEL_GS_BASE={:#x}",
                    gs_base,
                    kernel_gs_base
                );
                if gs_base == 0 && kernel_gs_base == 0 {
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK: *** BUG: Both GS MSRs are 0! swapgs in enter_user_asm will produce GS_BASE=0, syscall_entry [gs:0] will access NULL → #PF → Triple Fault! ***"
                    );
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK: *** ROOT CAUSE: gdt_init write_msr may have been overwritten, or &gdt.syscall returns 0 ***"
                    );
                }

                // 验证 LSTAR 页面在用户页表中映射且可执行
                let lstar_page = lstar & !(PAGE_SIZE - 1);
                let vmm = crate::privileged::mm::get_vmm();
                if let Some(phys) =
                    vmm.get_physical_in_pml4(cr3, crate::privileged::mm::VirtAddr(lstar_page))
                {
                    if let Some(pte_raw) =
                        vmm.get_pte_value(cr3, crate::privileged::mm::VirtAddr(lstar_page))
                    {
                        let present = (pte_raw & 0x001) != 0;
                        let user = (pte_raw & 0x004) != 0;
                        let nx = (pte_raw & (1u64 << 63)) != 0;
                        crate::klog_boot_info!(
                            "[USER] SELF-CHECK LSTAR page: virt={:#x} -> phys={:#x} PTE={:#x} P={} U={} NX={}",
                            lstar_page,
                            phys.0,
                            pte_raw,
                            u8::from(present),
                            u8::from(user),
                            u8::from(nx)
                        );
                        // syscall 指令在取指之前已将 CPL 切换到 0,
                        // 因此 LSTAR 页面 U=0 是正确行为 (内核页面不需要 USER 位).
                        // 仅当 P=0 或 NX=1 时才是真正的 bug.
                        if !user {
                            crate::klog_boot_info!(
                                "[USER] SELF-CHECK LSTAR page U=0 (expected: syscall switches CPL→0 before instruction fetch)"
                            );
                        }
                        if nx {
                            crate::klog_boot_info!(
                                "[USER] SELF-CHECK: *** BUG: LSTAR page NX=1! syscall_entry cannot execute! ***"
                            );
                        }
                    }
                } else {
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK: *** BUG: LSTAR page {:#x} NOT MAPPED in user page table! syscall from Ring 3 will #PF! ***",
                        lstar_page
                    );
                }
            }
        }

        // ═══ 自检式调试: 用户代码页完整页表路径遍历 (检查中间层级 USER 位) ═══
        // x86_64 页表遍历要求: PML4E→PDPTE→PDE→PTE 每一级都必须有 USER 位,
        // 否则 Ring 3 访问该页面时触发 #PF, 即使最终 PTE 有 USER 位.
        #[cfg(target_arch = "x86_64")]
        {
            let vaddr = rip_val;
            let pml4_idx = (vaddr >> 39) & 0x1FF;
            let pdpt_idx = (vaddr >> 30) & 0x1FF;
            let pd_idx = (vaddr >> 21) & 0x1FF;
            let pt_idx = (vaddr >> 12) & 0x1FF;

            // SAFETY: 使用物理地址 + KERNEL_BASE 访问页表, 只读操作
            unsafe {
                let pml4_virt = (cr3 + crate::privileged::mm::KERNEL_BASE as u64) as *const u64;
                let pml4e = pml4_virt.add(pml4_idx as usize).read_volatile();
                let pml4e_present = (pml4e & 1) != 0;
                let pml4e_user = (pml4e & 4) != 0;
                let pml4e_frame = pml4e & 0x000FFFFFFFFFF000;
                crate::klog_boot_info!(
                    "[USER] SELF-CHECK PT-WALK user_code {:#x}: PML4E[{}]={:#x} P={} U={} frame={:#x}",
                    vaddr,
                    pml4_idx,
                    pml4e,
                    u8::from(pml4e_present),
                    u8::from(pml4e_user),
                    pml4e_frame
                );
                if pml4e_present && !pml4e_user {
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK: *** BUG: PML4E[{}] U=0! Ring 3 cannot traverse to PDPT! ***",
                        pml4_idx
                    );
                }

                if pml4e_present {
                    let pdpt_virt =
                        (pml4e_frame + crate::privileged::mm::KERNEL_BASE as u64) as *const u64;
                    let pdpte = pdpt_virt.add(pdpt_idx as usize).read_volatile();
                    let pdpte_present = (pdpte & 1) != 0;
                    let pdpte_user = (pdpte & 4) != 0;
                    let pdpte_huge = (pdpte & 0x80) != 0;
                    let pdpte_frame = pdpte & 0x000FFFFFFFFFF000;
                    crate::klog_boot_info!(
                        "[USER] SELF-CHECK PT-WALK: PDPTE[{}]={:#x} P={} U={} HUGE={} frame={:#x}",
                        pdpt_idx,
                        pdpte,
                        u8::from(pdpte_present),
                        u8::from(pdpte_user),
                        u8::from(pdpte_huge),
                        pdpte_frame
                    );
                    if pdpte_present && !pdpte_user {
                        crate::klog_boot_info!(
                            "[USER] SELF-CHECK: *** BUG: PDPTE[{}] U=0! Ring 3 cannot traverse to PD! ***",
                            pdpt_idx
                        );
                    }

                    if pdpte_present && !pdpte_huge {
                        let pd_virt =
                            (pdpte_frame + crate::privileged::mm::KERNEL_BASE as u64) as *const u64;
                        let pde = pd_virt.add(pd_idx as usize).read_volatile();
                        let pde_present = (pde & 1) != 0;
                        let pde_user = (pde & 4) != 0;
                        let pde_huge = (pde & 0x80) != 0;
                        let pde_frame = pde & 0x000FFFFFFFFFF000;
                        crate::klog_boot_info!(
                            "[USER] SELF-CHECK PT-WALK: PDE[{}]={:#x} P={} U={} HUGE={} frame={:#x}",
                            pd_idx,
                            pde,
                            u8::from(pde_present),
                            u8::from(pde_user),
                            u8::from(pde_huge),
                            pde_frame
                        );
                        if pde_present && !pde_user {
                            crate::klog_boot_info!(
                                "[USER] SELF-CHECK: *** BUG: PDE[{}] U=0! Ring 3 cannot traverse to PT! ***",
                                pd_idx
                            );
                        }

                        if pde_present && !pde_huge {
                            let pt_virt = (pde_frame + crate::privileged::mm::KERNEL_BASE as u64)
                                as *const u64;
                            let pte = pt_virt.add(pt_idx as usize).read_volatile();
                            let pte_present = (pte & 1) != 0;
                            let pte_user = (pte & 4) != 0;
                            let pte_nx = (pte & (1u64 << 63)) != 0;
                            let pte_frame = pte & 0x000FFFFFFFFFF000;
                            crate::klog_boot_info!(
                                "[USER] SELF-CHECK PT-WALK: PTE[{}]={:#x} P={} U={} NX={} frame={:#x}",
                                pt_idx,
                                pte,
                                u8::from(pte_present),
                                u8::from(pte_user),
                                u8::from(pte_nx),
                                pte_frame
                            );
                            if pte_present && !pte_user {
                                crate::klog_boot_info!(
                                    "[USER] SELF-CHECK: *** BUG: PTE[{}] U=0! Ring 3 cannot access this page! ***",
                                    pt_idx
                                );
                            }
                            if pte_present && pte_nx {
                                crate::klog_boot_info!(
                                    "[USER] SELF-CHECK: *** BUG: PTE[{}] NX=1! Ring 3 cannot execute this page! ***",
                                    pt_idx
                                );
                            }
                        }
                    }
                }
            }
        }

        crate::klog_boot_info!("[USER] SELF-CHECK: calling enter_user_asm...");

        crate::arch!(enter_user(
            rip_val as usize,
            rsp_val as usize,
            0,
            cr3,
            kstack
        ));
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    /// 将 argc/argv/envp 写入用户进程栈
    /// 返回设置后的新栈指针 (RSP)
    ///
    /// # Safety
    ///
    /// `pid` 对应一个活动进程. 调用方必须持有进程表锁.
    pub unsafe fn setup_user_stack(
        &self,
        proc: *mut UserProcess,
        argv: *const *const u8,
        argc: usize,
        _envp: *const *const u8,
        envc: usize,
    ) -> u64 {
        if proc.is_null() {
            return 0;
        }

        // SAFETY: proc 由调用方保证有效, 内部访问均经 UserProcRef 安全包装。
        let proc_ref = unsafe { UserProcRef::new_unchecked(proc) };
        let stack_top = proc_ref.load_user_stack();
        let cr3 = proc_ref.load_cr3();

        // 所需空间: argc(8) + argv 指针(8*(argc+1)) + envp 指针(8*(envc+1)) + 字符串
        let mut string_bytes: usize = 0;
        let mut arg_lens: alloc::vec::Vec<usize> = alloc::vec::Vec::new();

        if !argv.is_null() {
            for i in 0..argc {
                // SAFETY: argv 由调用方保证至少 argc 个有效指针。
                let s = unsafe { *argv.add(i) };
                if s.is_null() {
                    arg_lens.push(1);
                    string_bytes += 1;
                } else {
                    let mut len: usize = 0;
                    // SAFETY: s 是 C 字符串, 不断读直到 NUL。
                    while unsafe { *s.add(len) } != 0 {
                        len += 1;
                    }
                    arg_lens.push(len + 1);
                    string_bytes += len + 1;
                }
            }
        }

        let ptr_count = 1 + (argc + 1) + (envc + 1); // 指针数: argc(1) + argv(n+1) + envp(m+1)
        let total = ptr_count * 8 + string_bytes;
        // 确保 16 字节对齐
        let total = (total + 15) & !15;

        if total as u64 > stack_top - USER_STACK_TOP + USER_STACK_SIZE {
            return 0; // Stack overflow
        }

        let new_sp = stack_top - total as u64;
        let mut pos = new_sp as usize;

        // 写入 argc
        let argc_off = pos;
        pos += 8;
        // 跳过 argv 指针 (在字符串写完之后回填)
        let argv_start_off = pos;
        pos += (argc + 1) * 8;
        // 跳过 envp 指针
        let envp_start_off = pos;
        pos += (envc + 1) * 8;
        // 字符串区起始
        let strings_off = pos;

        // Write argc
        raw::write_user_u64(cr3, argc_off, argc as u64);

        // 写入 argv 字符串与指针
        let mut str_off = strings_off;
        for i in 0..argc {
            let abs_addr = str_off as u64;
            raw::write_user_u64(cr3, argv_start_off + i * 8, abs_addr);

            if !argv.is_null() && (i < argc) {
                // SAFETY: argv 由调用方保证, i < argc 有效。
                let src = unsafe { *argv.add(i) };
                let l = arg_lens[i];
                for j in 0..l {
                    let b = if src.is_null() {
                        0u8
                    } else {
                        raw::read_byte_from_user_ptr(src, j)
                    };
                    raw::write_user_byte(cr3, str_off + j, b);
                }
                str_off += l;
            }
        }
        // argv 末尾 NULL 哨兵
        raw::write_user_u64(cr3, argv_start_off + argc * 8, 0u64);

        // envp 指针 (暂时全填 NULL)
        for i in 0..=envc {
            raw::write_user_u64(cr3, envp_start_off + i * 8, 0u64);
        }

        // 更新进程栈指针
        proc_ref.store_user_stack(new_sp);
        new_sp
    }

    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn load_elf_from_memory(&self, elf_data: *const u8, elf_size: u64, pwm: u64) -> i32 {
        crate::klog_boot_info!("[ELF] load_elf_from_memory: entry");
        if elf_data.is_null() || elf_size < core::mem::size_of::<ElfHeader>() as u64 {
            crate::klog_boot_info!("[ELF] load_elf_from_memory: null or too small");
            return -1;
        }

        // P1-I-33: 委托给 elf::verify::verify_elf 单一来源, 避免解析方式不一致
        //
        // SAFETY: elf_data 区间已校验 (非空 + size >= header), verify_elf 内部仅读借用。
        crate::klog_boot_info!("[ELF] calling verify_elf...");
        let verified = if let Ok(v) = unsafe { super::elf::verify::verify_elf(elf_data, elf_size) }
        {
            v
        } else {
            crate::klog_boot_info!("[ELF] verify_elf failed");
            return -1;
        };
        crate::klog_boot_info!("[ELF] verify_elf OK, entry={:#x}", verified.entry);

        // SAFETY: verify_elf 已通过校验, header 引用安全
        let header = unsafe { &*(elf_data as *const ElfHeader) };

        // PIE (ET_DYN) 支持: 检测 ELF 类型并计算 load_bias
        let is_pie = verified.is_pie;
        let load_bias: u64 = if is_pie {
            crate::privileged::config::aslr_pie_base()
        } else {
            0
        };
        let entry = verified.entry + load_bias;

        // SAFETY: verify_elf 已通过校验, 后续 raw 指针访问 (header / phdr / 物理页) 集中此块
        unsafe {
            let info = UserProcInfo {
                entry,
                name: [0; 64],
                code_size: 0,
                code_data: core::ptr::null(),
            };

            crate::klog_boot_info!("[ELF] calling self.create...");
            let proc = if let Some(p) = self.create(&info, pwm) {
                crate::klog_boot_info!("[ELF] self.create OK");
                p
            } else {
                crate::klog_boot_info!("[ELF] self.create failed");
                return -1;
            };

            // SAFETY: proc 由 create 返回, 生命周期由 UserProcManager 管理。
            let proc_ref = UserProcRef::new_unchecked(proc);
            let cr3 = proc_ref.load_cr3();

            // P1-I-32 修复: 改用栈上局部数组, 消除 RacyCell 静态分配器在 SMP 下
            // 多核 execve 并发的数据竞争. 8KB 临时缓冲 (1024×u64) 在调用方栈上
            // 分配. launch_first_user_process 路径运行在 boot 栈 (128KB) 上,
            // 剩余空间充足; 退出函数后自动释放, 无锁.
            // 仍按 phnum>256 截断保护, 单 PT_LOAD 段最大 1024 页.
            let mut allocated_pages = [0u64; 1024];
            let allocated_pages: &mut [u64] = &mut allocated_pages;
            let mut page_count: usize = 0;

            let phnum = header.e_phnum as usize;
            if phnum > 256 {
                self.destroy_raw(proc, false);
                return -1;
            }

            for i in 0..phnum {
                let phdr_size = core::mem::size_of::<ElfPhdr>() as u64;
                let phdr_offset = header.e_phoff + (i as u64) * u64::from(header.e_phentsize);
                if phdr_offset + phdr_size > elf_size {
                    self.destroy_raw(proc, false);
                    return -1;
                }
                let phdr = (elf_data.add(phdr_offset as usize)) as *const ElfPhdr;

                if (*phdr).p_type != PT_LOAD {
                    continue;
                }

                let vaddr_start = ((*phdr).p_vaddr + load_bias) & !(PAGE_SIZE - 1);
                let vaddr_end = ((*phdr).p_vaddr + (*phdr).p_memsz + load_bias + (PAGE_SIZE - 1))
                    & !(PAGE_SIZE - 1);
                let num_pages = (vaddr_end - vaddr_start) / PAGE_SIZE;

                for j in 0..num_pages {
                    let vaddr = vaddr_start + j * PAGE_SIZE;

                    let mut flags = PageFlags::PRESENT | PageFlags::USER;
                    if (*phdr).p_flags & 0x02 != 0 {
                        flags |= PageFlags::WRITABLE;
                    }
                    let flags = flags.bits();

                    // 在 aarch64 上, L2_DEVICE 中的 2MB BLOCK 描述符
                    // 会让 vmm_get_physical_in_table 对未映射的用户地址
                    // 返回非零. 跳过复用检查, 一律分配.
                    #[cfg(target_arch = "aarch64")]
                    let existing_phys: u64 = 0;
                    #[cfg(not(target_arch = "aarch64"))]
                    let existing_phys = raw::virt_to_phys(cr3, vaddr);

                    if existing_phys == 0 {
                        let page = raw::alloc_zeroed_user_page(cr3, vaddr, flags);
                        if page.is_null() {
                            for pi in 0..page_count {
                                raw::free_phys_page_for_rollback(allocated_pages[pi]);
                            }
                            self.destroy_raw(proc, false);
                            return -1;
                        }
                        if page_count < 1024 {
                            allocated_pages[page_count] = page as u64;
                            page_count += 1;
                        }
                    } else {
                        // 复用现有页, 记录下来
                        if page_count < 1024 {
                            allocated_pages[page_count] = existing_phys;
                            page_count += 1;
                        }
                    }
                }

                if (*phdr).p_filesz > 0 {
                    let file_offset_bytes = (*phdr).p_offset as usize;
                    let mut copied: u64 = 0;
                    let first_page_offset = (*phdr).p_vaddr & 0xFFF;
                    let start_idx = page_count.saturating_sub(num_pages as usize);

                    for j in 0..num_pages {
                        if copied >= (*phdr).p_filesz {
                            break;
                        }
                        let page_phys = allocated_pages[start_idx + j as usize];
                        if page_phys == 0 {
                            continue;
                        }

                        let off_in_page = if j == 0 { first_page_offset } else { 0 };
                        let max_in_page = PAGE_SIZE - off_in_page;
                        let remaining = (*phdr).p_filesz - copied;
                        let chunk = if max_in_page < remaining {
                            max_in_page
                        } else {
                            remaining
                        };
                        raw::elf_chunk_copy(
                            page_phys,
                            off_in_page,
                            elf_data,
                            file_offset_bytes + (copied as usize),
                            chunk,
                        );
                        copied += chunk;
                    }
                }
            }

            proc_ref.set_entry(entry);

            proc_ref.pid() as i32
        }
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn create_from_binary(&self, code: *const u8, code_size: u64, pwm: u64) -> i32 {
        let info = UserProcInfo {
            entry: USER_CODE_BASE,
            name: [0; 64],
            code_size,
            code_data: code,
        };

        let proc = match self.create(&info, pwm) {
            Some(p) => p,
            None => return -1,
        };

        // SAFETY: proc 由 create 返回, 生命周期由 UserProcManager 管理。
        let proc_ref = unsafe { UserProcRef::new_unchecked(proc) };
        let cr3 = proc_ref.load_cr3();
        let num_code_pages = code_size.div_ceil(PAGE_SIZE);

        for i in 0..num_code_pages {
            let page = raw::alloc_code_page();
            if page.is_null() {
                return -1;
            }

            let copy_size = if code_size - i * PAGE_SIZE > PAGE_SIZE {
                PAGE_SIZE
            } else {
                code_size - i * PAGE_SIZE
            };

            // SAFETY: page 来自 pmm_alloc_page, code 区间内可读。
            unsafe {
                memcpy(
                    page.cast::<core::ffi::c_void>(),
                    code.add((i * PAGE_SIZE) as usize)
                        .cast::<core::ffi::c_void>(),
                    copy_size as usize,
                );
            }

            let vaddr = USER_CODE_BASE + i * PAGE_SIZE;
            raw::map_code_page(cr3, vaddr, page as u64);
        }

        proc_ref.pid() as i32
    }

    pub fn get_current(&self) -> Option<*mut UserProcess> {
        let current = self.current.load(Ordering::SeqCst);
        if current != 0 {
            Some(current as *mut UserProcess)
        } else {
            None
        }
    }

    pub fn set_current(&self, proc: Option<*mut UserProcess>) {
        if let Some(p) = proc {
            self.current.store(p as u64, Ordering::SeqCst);
        } else {
            self.current.store(0, Ordering::SeqCst);
        }
    }
}

pub static USER_PROC_MANAGER: UserProcManager = UserProcManager::new();

/// 用户进程子系统初始化入口 — 初始化全局 [`USER_PROC_MANAGER`] 并注册 memlock 限制查询回调。
pub fn init() {
    USER_PROC_MANAGER.init();

    // 注册 memlock 限制查询回调, 解耦 mm→proc 依赖
    // SAFETY: get_memlock_limit 是 'static 函数指针, 在内核运行期间始终有效.
    unsafe {
        crate::privileged::rlimit_query::register_memlock_limit(
            crate::privileged::proc::get_memlock_limit,
        );
    }
}

/// 分配一个新的 PID（供 `sys_fork` 使用）
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_alloc_pid() -> u32 {
    PROCESS_TABLE.allocate_pid().unwrap_or(0)
}

/// 克隆父进程的 `UserProcess` 给子进程（供 `sys_fork` 使用）
/// 子进程的 CR3 和内核栈已在 `sys_fork` 中分配好，此处仅创建 `UserProcess` 记录
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn user_proc_clone(parent_pid: u32, child_pid: u32) -> i32 {
    let parent_proc = match USER_PROC_MANAGER.get(parent_pid) {
        Some(p) => p,
        None => return -1,
    };

    let child_kernel_proc = match PROCESS_TABLE.get(child_pid) {
        Some(p) => match NonNull::new(p) {
            Some(nn) => nn,
            None => return -1,
        },
        None => return -1,
    };

    // SAFETY: parent_proc / child_kernel_proc 均来自管理器, 有效。
    unsafe {
        let parent_ref = UserProcRef::new_unchecked(parent_proc);
        // ✅ 关联权威 child_kernel_proc 句柄, 建立 UserProcess→Process 反向引用.
        let child_up = raw::alloc_user_process(child_kernel_proc).unwrap_or_default();
        if child_up.is_null() {
            return -1;
        }
        let child_ref = UserProcRef::new_unchecked(child_up);

        // 通过 UserProcRef 设置字段 (委托到 Process)
        child_ref.set_pid(child_pid);
        child_ref.store_pwm(parent_ref.load_pwm());
        child_ref.store_cr3((*child_kernel_proc.as_ptr()).cr3.load(Ordering::SeqCst));
        child_ref.store_kernel_stack(
            (*child_kernel_proc.as_ptr())
                .kernel_stack
                .load(Ordering::SeqCst),
        );
        child_ref.store_user_stack(parent_ref.load_user_stack());
        child_ref.store_stack_bottom(parent_ref.load_stack_bottom());
        child_ref.set_entry(parent_ref.entry());
        child_ref.store_state(1);
        child_ref.set_create_time(crate::privileged::timer::get_ticks());

        USER_PROC_MANAGER.processes.lock().insert(
            child_pid,
            if let Some(nn) = NonNull::new(child_up) {
                nn
            } else {
                klog_error!("user_proc_clone: 子进程指针为空");
                return -1;
            },
        );
    }

    0
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 按缺页地址尝试向下扩展用户栈 — 若 `fault_addr` 落在可扩展区间内, 则映射其所在页。
///
/// 返回是否成功扩展; 地址已越过栈顶、低于扩展下限或当前无进程时返回 `false`。
pub fn try_expand_user_stack(fault_addr: u64) -> bool {
    if fault_addr >= USER_STACK_TOP {
        return false;
    }
    if fault_addr < USER_STACK_EXPAND_LIMIT {
        return false;
    }

    let proc = match USER_PROC_MANAGER.get_current() {
        Some(p) => p,
        None => return false,
    };

    // SAFETY: proc 由管理器返回, 有效。
    let proc_ref = unsafe { UserProcRef::new_unchecked(proc) };
    let stack_bottom = proc_ref.load_stack_bottom();
    if fault_addr >= stack_bottom {
        return false;
    }

    let cr3 = proc_ref.load_cr3();
    if cr3 == 0 {
        return false;
    }

    let page_addr = fault_addr & !(PAGE_SIZE - 1);
    let pages_needed = (stack_bottom - page_addr) / PAGE_SIZE;

    for i in 0..pages_needed {
        let vaddr = page_addr + i * PAGE_SIZE;
        if vaddr >= stack_bottom {
            break;
        }

        if raw::virt_to_phys(cr3, vaddr) != 0 {
            continue;
        }

        let new_page = raw::alloc_zeroed_user_page(
            cr3,
            vaddr,
            (PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER).bits(),
        );
        if new_page.is_null() {
            return false;
        }
    }

    proc_ref.store_stack_bottom(page_addr);
    true
}
