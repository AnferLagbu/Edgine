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
pub const KEXEC_DEFAULT_LOAD_ADDR: u64 = 0x01000000;
/// 默认 initrd 加载地址
pub const KEXEC_DEFAULT_INITRD_ADDR: u64 = 0x08000000;

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

impl KexecSubsystem {
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

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 加载内核段
    ///
    /// `seg_type`: 段类型
    /// `dst_addr`: 目标物理地址
    /// `src_data`: 源数据
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
            KexecSegType::Cmdline => KEXEC_MAX_CMDLINE,
            KexecSegType::BootInfo => KEXEC_MAX_CMDLINE,
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

        // 复制数据到目标物理地址
        // SAFETY: dst_addr 是用户指定的物理地址, 我们信任调用者
        // 在实际实现中, 这里应该通过 PMM 确保地址可用
        let dst_ptr = dst_addr as *mut u8;
        let dst_size = src_data.len();
        unsafe {
            // 检查目标地址可写 (简化: 直接写入)
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
        crate::arch!(interrupt_disable());

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
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
pub extern "C" fn sys_kexec(cmd: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    if !kexec_is_initialized() && cmd != 7 {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // load_segment (简化: 仅记录元数据, 不实际复制)
            let seg_type = KexecSegType::from(a1 as u32);
            let dst_addr = a2;
            let size = a3 as usize;
            let max_size = match seg_type {
                KexecSegType::Kernel => KEXEC_MAX_KERNEL_SIZE,
                KexecSegType::Initrd => KEXEC_MAX_INITRD_SIZE,
                KexecSegType::Cmdline => KEXEC_MAX_CMDLINE,
                KexecSegType::BootInfo => KEXEC_MAX_CMDLINE,
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
            kexec_subsystem().get_segment_count() as i64
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
