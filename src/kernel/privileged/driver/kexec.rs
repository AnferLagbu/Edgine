//! kexec — 从当前内核直接引导新内核
//!
//! ## 设计
//!
//! kexec 允许从当前运行的内核直接引导新内核, 无需经过 BIOS/UEFI 固件.
//! 这大大减少了重启时间, 适用于:
//!
//! 1. 内核热升级
//! 2. 崩溃转储后快速恢复
//! 3. 嵌入式/云环境快速部署
//!
//! ### 流程
//!
//! 1. 用户通过 syscall 加载新内核镜像 + initrd + 命令行
//! 2. kexec 将镜像复制到目标物理内存
//! 3. 准备引导参数 (Multiboot2 信息 / Device Tree)
//! 4. 关闭当前内核 (中断/设备/内存)
//! 5. 跳转到新内核入口点
//!
//! ### 与 Linux 的差异
//!
//! 1. **无 kexec_file_load**: 仅实现 kexec_load (用户态提供镜像)
//! 2. **无 kexec 压缩**: 不支持内核镜像解压
//! 3. **无 crashkernel**: 不支持 kdump 保留区域
//! 4. **仅 Multiboot2**: x86_64 仅支持 Multiboot2 协议
//!
//! ## SAFETY
//!
//! 本模块属于 privileged/TCB, 允许 unsafe.
//! kexec 涉及物理内存操作、设备关闭、直接跳转, 极度危险.

#![deny(missing_docs)]

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::privileged::sync::IrqSpinLock;
use alloc::vec::Vec;

// ============================================================================
// 常量
// ============================================================================

/// kexec 加载的内核最大大小 (64MB)
pub const KEXEC_MAX_KERNEL_SIZE: usize = 64 * 1024 * 1024;
/// kexec 加载的 initrd 最大大小 (128MB)
pub const KEXEC_MAX_INITRD_SIZE: usize = 128 * 1024 * 1024;
/// 命令行最大长度
pub const KEXEC_MAX_CMDLINE: usize = 4096;
/// 默认内核加载地址 (16MB, 避开低地址)
pub const KEXEC_DEFAULT_LOAD_ADDR: u64 = 0x0100_0000;
/// 默认 initrd 加载地址
pub const KEXEC_DEFAULT_INITRD_ADDR: u64 = 0x0800_0000;

// ============================================================================
// kexec 段描述
// ============================================================================

/// kexec 段 (内存区域描述)
#[derive(Debug, Clone, Copy)]
pub struct KexecSegment {
    /// 目标物理地址
    pub dst_addr: u64,
    /// 源数据大小 (字节)
    pub size: usize,
    /// 段类型
    pub seg_type: KexecSegType,
}

/// kexec 段类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum KexecSegType {
    /// 内核镜像
    Kernel = 0,
    /// initrd
    Initrd = 1,
    /// 命令行
    Cmdline = 2,
    /// 引导信息 (Multiboot2 / DTB)
    BootInfo = 3,
}

// ============================================================================
// kexec 状态
// ============================================================================

/// kexec 加载状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum KexecState {
    /// 空闲, 未加载
    Idle = 0,
    /// 已加载, 待执行
    Loaded = 1,
    /// 正在执行 (不可逆)
    Executing = 2,
}

// ============================================================================
// 段目标校验
// ============================================================================

/// 把内核切片指针换算为物理区间.
///
/// 仅在能证明指针落在**内核高半区直射区**或**低半区恒等面**时返回 `Some`;
/// 高半区的内核文本/rodata 别名 (非直射) 无法由地址反推物理页, 返回 `None`.
fn kernel_slice_phys(ptr: u64, len: usize) -> Option<(u64, usize)> {
    let len_u64 = u64::try_from(len).ok()?;
    let end = ptr.checked_add(len_u64)?;
    let kernel_base = crate::privileged::mm::KERNEL_BASE;
    let direct_end = kernel_base.checked_add(crate::privileged::mm::DIRECT_MAP_LIMIT)?;
    if ptr >= kernel_base && end <= direct_end {
        return Some((ptr - kernel_base, len));
    }
    if ptr < crate::privileged::mm::DIRECT_MAP_LIMIT
        && end <= crate::privileged::mm::DIRECT_MAP_LIMIT
    {
        return Some((ptr, len));
    }
    None
}

/// 校验 kexec 目标物理区间.
///
/// 判据: 非零长度 / 页对齐 / 完整落在直射区 / 完整落在 PMM 管理范围 /
/// 不与运行中内核映像 `[0, kernel_end)` 重叠 / 与可判定的源物理区间不重叠.
/// 任一不符即拒绝 (调用方不写任何字节).
fn validate_segment_destination(
    dst_addr: u64,
    size: usize,
    kernel_end: u64,
    managed_bytes: u64,
    src_phys: Option<(u64, usize)>,
) -> Result<(), &'static str> {
    if size == 0 {
        return Err("kexec: 零长度段");
    }
    if !dst_addr.is_multiple_of(crate::privileged::mm::PAGE_SIZE) {
        return Err("kexec: 目标地址未页对齐");
    }
    let size_u64 = u64::try_from(size).map_err(|_| "kexec: 段长度超出 u64")?;
    let dst_end = dst_addr
        .checked_add(size_u64)
        .ok_or("kexec: 目标区间加法回绕")?;
    if dst_end > crate::privileged::mm::DIRECT_MAP_LIMIT {
        return Err("kexec: 目标区间超出直射区");
    }
    if dst_end > managed_bytes {
        return Err("kexec: 目标区间超出 PMM 管理范围");
    }
    if crate::privileged::mm::early_reserve::ranges_overlap(dst_addr, size_u64, 0, kernel_end) {
        return Err("kexec: 目标区间与运行中内核映像重叠");
    }
    if let Some((src, src_len)) = src_phys {
        let src_len = u64::try_from(src_len).map_err(|_| "kexec: 源长度超出 u64")?;
        if crate::privileged::mm::early_reserve::ranges_overlap(dst_addr, size_u64, src, src_len) {
            return Err("kexec: 源/目标区间重叠");
        }
    }
    Ok(())
}

// ============================================================================
// kexec 子系统
// ============================================================================

/// kexec 子系统
pub struct KexecSubsystem {
    /// 当前状态
    state: AtomicU32,
    /// 已加载的段
    segments: IrqSpinLock<Vec<KexecSegment>>,
    /// 内核入口点
    entry_point: AtomicU64,
    /// 命令行
    cmdline: IrqSpinLock<Vec<u8>>,
    /// 是否已初始化
    initialized: AtomicBool,
}

impl Default for KexecSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

impl KexecSubsystem {
    /// 构造一个空闲 (未加载) 的 kexec 子系统实例。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(KexecState::Idle as u32),
            segments: IrqSpinLock::new(Vec::new()),
            entry_point: AtomicU64::new(0),
            cmdline: IrqSpinLock::new(Vec::new()),
            initialized: AtomicBool::new(false),
        }
    }

    /// 初始化
    pub fn init(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        self.initialized.store(true, Ordering::Release);
        crate::klog_ffi!(
            klog_ffi_info,
            "[kexec] initialized: direct kernel boot ready"
        );
    }

    /// 加载内核段
    ///
    /// `seg_type`: 段类型
    /// `dst_addr`: 目标**物理地址** (页对齐); 内核经 `phys_to_virt` 直射别名写入
    /// `src_data`: 源数据
    ///
    /// 拒绝条件 (任一即返回 `false` 且不写任何字节): 零长度 / 目标未页对齐 /
    /// 目标区间超出直射区或 PMM 管理范围 / 与运行中内核映像重叠 / 与可判定的
    /// 源物理区间重叠.
    pub fn load_segment(&self, seg_type: KexecSegType, dst_addr: u64, src_data: &[u8]) -> bool {
        // 检查状态
        let current = KexecState::from_u32(self.state.load(Ordering::Acquire));
        if current == KexecState::Executing {
            return false;
        }

        // 大小检查
        let max_size = match seg_type {
            KexecSegType::Kernel => KEXEC_MAX_KERNEL_SIZE,
            KexecSegType::Initrd => KEXEC_MAX_INITRD_SIZE,
            KexecSegType::Cmdline | KexecSegType::BootInfo => KEXEC_MAX_CMDLINE,
        };
        if src_data.len() > max_size {
            crate::klog_ffi!(
                klog_ffi_warn,
                "[kexec] segment too large: type={} size={} max={}",
                seg_type as u32,
                src_data.len(),
                max_size
            );
            return false;
        }

        // 目标区间校验: 直射区 / PMM 管理范围 / 内核映像 / 源目标重叠.
        let kernel_end = crate::privileged::boot::boot_get_kernel_end();
        let managed_bytes = crate::privileged::mm::pmm::get_pmm()
            .get_total_pages()
            .saturating_mul(crate::privileged::mm::PAGE_SIZE);
        let src_phys = kernel_slice_phys(src_data.as_ptr() as u64, src_data.len());
        if let Err(reason) = validate_segment_destination(
            dst_addr,
            src_data.len(),
            kernel_end,
            managed_bytes,
            src_phys,
        ) {
            crate::klog_ffi!(klog_ffi_warn, "[kexec] segment rejected: {}", reason);
            return false;
        }

        // 复制数据到目标物理地址
        let dst_ptr = crate::privileged::mm::phys_to_virt(dst_addr) as *mut u8;
        let dst_size = src_data.len();
        // SAFETY: 本块把 src_data 复制到直射区目标. 已成立的前提:
        // ① Executing 状态已拒绝, 且 src_data.len() 不超过该段类型上限, 故 count
        //    不越源切片; 源指针来自有效已初始化切片.
        // ② `validate_segment_destination` 已证明 dst 页对齐、[dst, dst+size) 在
        //    直射区 [0, DIRECT_MAP_LIMIT) 与 PMM 管理范围内、不与运行中内核映像
        //    [0, kernel_end) 重叠, 且与可判定的源物理区间不重叠.
        // ③ src_phys 不可判定时 (高半区非直射别名), 源必属内核映像映射面, 已由
        //    ② 的内核映像排除保证物理不重叠.
        // ④ phys_to_virt 对 [0, DIRECT_MAP_LIMIT) 内的物理地址给出唯一内核别名,
        //    故 dst_ptr 可写且不触用户映射.
        unsafe {
            core::ptr::copy_nonoverlapping(src_data.as_ptr(), dst_ptr, dst_size);
        }

        // 记录段
        let seg = KexecSegment {
            dst_addr,
            size: src_data.len(),
            seg_type,
        };
        self.segments.lock().push(seg);

        // 如果是内核段, 记录入口点
        if seg_type == KexecSegType::Kernel {
            // Multiboot2 入口点在镜像头部之后
            // 简化: 使用 dst_addr 作为入口点
            self.entry_point.store(dst_addr, Ordering::Release);
        }

        // 如果是命令行段, 保存
        if seg_type == KexecSegType::Cmdline {
            let mut cmdline = self.cmdline.lock();
            cmdline.clear();
            cmdline.extend_from_slice(src_data);
        }

        crate::klog_ffi!(
            klog_ffi_info,
            "[kexec] loaded segment: type={} addr={:#x} size={}",
            seg_type as u32,
            dst_addr,
            src_data.len()
        );
        true
    }

    /// 设置入口点
    pub fn set_entry_point(&self, entry: u64) {
        self.entry_point.store(entry, Ordering::Release);
    }

    /// 执行 kexec: 跳转到新内核
    ///
    /// 此函数不会返回
    pub fn execute(&self) -> ! {
        let current = KexecState::from_u32(self.state.load(Ordering::Acquire));
        if current != KexecState::Loaded {
            // 未加载, 无法执行
            crate::klog_ffi!(
                klog_ffi_error,
                "[kexec] cannot execute: state={:?}, expected Loaded",
                current
            );
            loop {
                core::hint::spin_loop();
            }
        }

        self.state
            .store(KexecState::Executing as u32, Ordering::Release);
        let entry = self.entry_point.load(Ordering::Acquire);

        crate::klog_ffi!(klog_ffi_info, "[kexec] executing: entry={:#x}", entry);

        // 1. 关闭所有设备
        crate::privileged::driver::shutdown_all();

        // 2. 关闭中断
        let _ = crate::arch!(interrupt_disable());

        // 3. 刷新缓存
        Self::flush_caches();

        // 4. 准备引导参数并跳转
        Self::jump_to_kernel(entry);
    }

    /// 标记为已加载 (所有段加载完成后调用)
    pub fn mark_loaded(&self) -> bool {
        let current = KexecState::from_u32(self.state.load(Ordering::Acquire));
        if current != KexecState::Idle {
            return false;
        }

        let segments = self.segments.lock();
        let has_kernel = segments.iter().any(|s| s.seg_type == KexecSegType::Kernel);
        if !has_kernel {
            crate::klog_ffi!(klog_ffi_warn, "[kexec] no kernel segment loaded");
            return false;
        }

        self.state
            .store(KexecState::Loaded as u32, Ordering::Release);
        crate::klog_ffi!(
            klog_ffi_info,
            "[kexec] marked as loaded: {} segments, entry={:#x}",
            segments.len(),
            self.entry_point.load(Ordering::Acquire)
        );
        true
    }

    /// 取消加载
    pub fn cancel(&self) -> bool {
        let current = KexecState::from_u32(self.state.load(Ordering::Acquire));
        if current == KexecState::Executing {
            return false;
        }
        self.segments.lock().clear();
        self.cmdline.lock().clear();
        self.entry_point.store(0, Ordering::Release);
        self.state.store(KexecState::Idle as u32, Ordering::Release);
        true
    }

    /// 获取状态
    pub fn get_state(&self) -> KexecState {
        KexecState::from_u32(self.state.load(Ordering::Acquire))
    }

    /// 获取段数量
    pub fn get_segment_count(&self) -> usize {
        self.segments.lock().len()
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    // ========================================================================
    // 架构相关
    // ========================================================================

    /// 刷新缓存
    fn flush_caches() {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: WBINVD 是安全的缓存刷新指令
            unsafe {
                core::arch::asm!("wbinvd", options(nostack, nomem));
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: 缓存维护指令
            unsafe {
                core::arch::asm!(
                    "dc cisw, x0",  // 清除并无效 D-cache
                    "ic ialluis",    // 无效 I-cache
                    out("x0") _,
                    options(nostack),
                );
            }
        }
    }

    /// 跳转到新内核入口点
    fn jump_to_kernel(entry: u64) -> ! {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: 这是 kexec 的核心操作, 跳转到新内核
            // 此时所有设备已关闭, 中断已禁用, 缓存已刷新
            unsafe {
                core::arch::asm!(
                    // 设置段寄存器 (64位模式清零即可)
                    "xor eax, eax",
                    "mov ds, ax",
                    "mov es, ax",
                    "mov fs, ax",
                    "mov gs, ax",
                    "mov ss, ax",
                    // 设置栈 (使用临时栈)
                    "mov rsp, 0x90000",
                    // 关闭分页: CR0.PG = bit 31, 用 btr 清除
                    "mov rax, cr0",
                    "btr rax, 31",
                    "mov cr0, rax",
                    // 跳转到入口点
                    "jmp rcx",
                    in("rcx") entry,
                    options(noreturn, nostack),
                );
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: 跳转到新内核入口
            unsafe {
                core::arch::asm!(
                    // 禁用 MMU (SCTLR_EL1.M=0, bit 1)
                    "mrs x1, sctlr_el1",
                    "bic x1, x1, #2",
                    "msr sctlr_el1, x1",
                    "isb",
                    // 跳转
                    "br {0}",
                    in(reg) entry,
                    options(noreturn, nostack),
                );
            }
        }
    }
}

impl KexecState {
    /// 由 u32 状态码还原 kexec 状态 (未知值按空闲处理)。
    #[must_use]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Loaded,
            2 => Self::Executing,
            _ => Self::Idle,
        }
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 kexec 子系统
static KEXEC: KexecSubsystem = KexecSubsystem::new();

/// 初始化 kexec
pub fn kexec_init() {
    KEXEC.init();
}

/// 获取全局 kexec 子系统
#[must_use]
pub fn kexec_subsystem() -> &'static KexecSubsystem {
    &KEXEC
}

/// kexec 是否已初始化
pub fn kexec_is_initialized() -> bool {
    KEXEC.is_initialized()
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_kexec` — kexec 系统调用
///
/// `a0`: cmd
///   0 = `load_segment(type`: a1, `dst_addr`: a2, size: a3) — 简化: 仅记录段
///   1 = `set_entry_point(entry`: a1)
///   2 = `mark_loaded()`
///   3 = `execute()` — 不会返回
///   4 = `cancel()`
///   5 = `get_state()` → state
///   6 = `get_segment_count()` → count
///   7 = `is_initialized()` → 是否已初始化
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sys_kexec(cmd: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    if !kexec_is_initialized() && cmd != 7 {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // load_segment (简化: 仅记录元数据, 不实际复制)
            let seg_type = KexecSegType::from(crate::privileged::glue::low_u32_from_u64(a1));
            let dst_addr = a2;
            let size = crate::privileged::glue::usize_from_u64(a3);
            let max_size = match seg_type {
                KexecSegType::Kernel => KEXEC_MAX_KERNEL_SIZE,
                KexecSegType::Initrd => KEXEC_MAX_INITRD_SIZE,
                KexecSegType::Cmdline | KexecSegType::BootInfo => KEXEC_MAX_CMDLINE,
            };
            if size > max_size {
                return -(22i64); // EINVAL
            }
            let seg = KexecSegment {
                dst_addr,
                size,
                seg_type,
            };
            kexec_subsystem().segments.lock().push(seg);
            if seg_type == KexecSegType::Kernel {
                kexec_subsystem()
                    .entry_point
                    .store(dst_addr, Ordering::Release);
            }
            0
        }
        1 => {
            // set_entry_point
            kexec_subsystem().set_entry_point(a1);
            0
        }
        2 => {
            // mark_loaded
            if kexec_subsystem().mark_loaded() {
                0
            } else {
                -(22i64)
            }
        }
        3 => {
            // execute — 不会返回
            kexec_subsystem().execute();
        }
        4 => {
            // cancel
            if kexec_subsystem().cancel() {
                0
            } else {
                -(22i64)
            }
        }
        5 => {
            // get_state
            kexec_subsystem().get_state() as i64
        }
        6 => {
            // get_segment_count
            crate::privileged::glue::i64_from_usize(kexec_subsystem().get_segment_count())
        }
        7 => {
            // is_initialized
            i64::from(kexec_is_initialized())
        }
        _ => -(38i64), // ENOSYS
    }
}

impl From<u32> for KexecSegType {
    fn from(v: u32) -> Self {
        match v {
            1 => Self::Initrd,
            2 => Self::Cmdline,
            3 => Self::BootInfo,
            _ => Self::Kernel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u64 = crate::privileged::mm::PAGE_SIZE;
    const MANAGED: u64 = 0x0800_0000;
    const KERNEL_END: u64 = 0x0040_0000;

    #[test]
    fn kernel_slice_phys_maps_direct_map_alias() {
        let ptr = crate::privileged::mm::KERNEL_BASE + 0x1000;
        assert_eq!(kernel_slice_phys(ptr, 0x100), Some((0x1000, 0x100)));
    }

    #[test]
    fn kernel_slice_phys_maps_low_identity() {
        assert_eq!(kernel_slice_phys(0x1000, 0x100), Some((0x1000, 0x100)));
    }

    #[test]
    fn kernel_slice_phys_rejects_non_direct_alias() {
        let ptr = crate::privileged::mm::KERNEL_BASE + crate::privileged::mm::DIRECT_MAP_LIMIT;
        assert_eq!(kernel_slice_phys(ptr, 0x100), None);
    }

    #[test]
    fn destination_accepts_valid_range() {
        assert_eq!(
            validate_segment_destination(0x0080_0000, 0x1000, KERNEL_END, MANAGED, None),
            Ok(())
        );
    }

    #[test]
    fn destination_rejects_zero_length() {
        assert_eq!(
            validate_segment_destination(0x0080_0000, 0, KERNEL_END, MANAGED, None),
            Err("kexec: 零长度段")
        );
    }

    #[test]
    fn destination_rejects_unaligned() {
        assert_eq!(
            validate_segment_destination(0x0080_0001, 0x1000, KERNEL_END, MANAGED, None),
            Err("kexec: 目标地址未页对齐")
        );
    }

    #[test]
    fn destination_rejects_outside_direct_map() {
        assert_eq!(
            validate_segment_destination(
                crate::privileged::mm::DIRECT_MAP_LIMIT,
                0x1000,
                KERNEL_END,
                u64::MAX,
                None
            ),
            Err("kexec: 目标区间超出直射区")
        );
    }

    #[test]
    fn destination_rejects_outside_managed_range() {
        assert_eq!(
            validate_segment_destination(0x003F_F000, 0x2000, 0x10_0000, 0x0040_0000, None),
            Err("kexec: 目标区间超出 PMM 管理范围")
        );
    }

    #[test]
    fn destination_rejects_kernel_image_overlap() {
        assert_eq!(
            validate_segment_destination(0x003F_F000, 0x2000, 0x0040_0000, MANAGED, None),
            Err("kexec: 目标区间与运行中内核映像重叠")
        );
    }

    #[test]
    fn destination_rejects_source_overlap_and_accepts_adjacent() {
        assert_eq!(
            validate_segment_destination(
                0x0080_0000,
                0x1000,
                KERNEL_END,
                MANAGED,
                Some((0x0080_0000, 0x1000))
            ),
            Err("kexec: 源/目标区间重叠")
        );
        assert_eq!(
            validate_segment_destination(
                0x0080_0000,
                0x1000,
                KERNEL_END,
                MANAGED,
                Some((0x0081_0000, 0x1000))
            ),
            Ok(())
        );
    }

    #[test]
    fn destination_rejects_range_addition_wrap() {
        let dst = crate::privileged::mm::DIRECT_MAP_LIMIT - PAGE;
        assert_eq!(
            validate_segment_destination(dst, usize::MAX, KERNEL_END, u64::MAX, None),
            Err("kexec: 目标区间加法回绕")
        );
    }
}
