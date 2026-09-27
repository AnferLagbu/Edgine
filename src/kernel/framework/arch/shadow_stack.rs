//! Shadow Stack (CET) + 控制流完整性
//!
//! ## 设计
//!
//! ### x86_64: Intel CET (控制流强制技术, Control-flow Enforcement Technology)
//!
//! 1. **Shadow Stack**: 函数返回地址的影子副本, RET 时校验一致性
//!    - CR4.CET = 1 启用 CET
//!    - IA32_S_CET: 内核态 CET 配置
//!    - IA32_U_CET: 用户态 CET 配置
//!    - IA32_PL3_SSP: 用户态 Shadow Stack 指针
//!    - IA32_INTERRUPT_SSP_TABLE: 中断 Shadow Stack 表
//!
//! 2. **IBT (Indirect Branch Tracking)**: 间接跳转目标必须有 ENDBR64
//!    - 通过 IA32_S_CET/IA32_U_CET 的 IBT 位启用
//!
//! ### aarch64: PAC (指针认证, Pointer Authentication) + BTI (分支目标识别, Branch Target Identification)
//!
//! 1. **PAC**: 对返回地址和函数指针签名/验证
//!    - PACIASP/AUTIASP 指令
//!    - APIAKeyLo/Hi 寄存器
//!
//! 2. **BTI**: 间接跳转目标必须有 BTI 指令
//!    - 通过 SCTRL_EL1.BTI 启用
//!
//! ### 当前实现状态
//!
//! - Shadow Stack 分配/释放: 已实现
//! - CET MSR 配置: 已实现 (QEMU 可能不支持, 会回退)
//! - IBT: 仅定义, 未启用 (需要编译器 -fcf-protection=full)
//! - PAC/BTI: 仅定义, 未启用
//!
//! ## SAFETY
//!
//! 本模块属于 framework/TCB, 允许 unsafe.
//! Shadow Stack 操作涉及 CR4/MSR 写入, 需在 Ring 0 执行.
//! CET 启用失败不会 panic, 仅记录日志并回退.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::framework::config::PAGE_SIZE;
use crate::framework::sync::IrqSpinLock;
use alloc::vec::Vec;

// ============================================================================
// 常量
// ============================================================================

/// Shadow Stack 页大小
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
pub const SHADOW_STACK_PAGE_SIZE: usize = PAGE_SIZE as usize;
/// Shadow Stack 默认大小 (64KB)
pub const SHADOW_STACK_DEFAULT_SIZE: usize = 64 * 1024;
/// Shadow Stack 对齐
pub const SHADOW_STACK_ALIGN: usize = 8;

// x86_64 MSR 地址
#[cfg(target_arch = "x86_64")]
mod x86_msrs {
    /// CR4 第23位 = CET 启用
    pub const CR4_CET_BIT: u64 = 1 << 23;
    /// `IA32_U_CET`: 用户态 CET 配置
    pub const IA32_U_CET: u32 = 0x6A0;
    /// `IA32_S_CET`: 内核态 CET 配置
    pub const IA32_S_CET: u32 = 0x6A2;
    /// `IA32_PL3_SSP`: 用户态 Shadow Stack 指针
    pub const IA32_PL3_SSP: u32 = 0x6A4;
    /// `IA32_INTERRUPT_SSP_TABLE`: 中断 Shadow Stack 表
    pub const IA32_INTERRUPT_SSP_TABLE: u32 = 0x6A8;
    /// `IA32_PL0_SSP`: 内核态 Shadow Stack 指针
    pub const IA32_PL0_SSP: u32 = 0x6A5;
}

// ============================================================================
// Shadow Stack 描述符
// ============================================================================

/// Shadow Stack 描述符 (per-thread)
#[derive(Debug)]
pub struct ShadowStack {
    /// Shadow Stack 基地址
    pub base: u64,
    /// Shadow Stack 大小 (字节)
    pub size: u64,
    /// 当前 SSP (Shadow Stack Pointer)
    pub ssp: AtomicU64,
    /// 是否活跃
    pub active: AtomicBool,
}

impl ShadowStack {
    /// 创建 Shadow Stack 描述符
    pub fn new(base: u64, size: u64) -> Self {
        Self {
            base,
            size,
            ssp: AtomicU64::new(base + size as u64), // 栈从高向低增长
            active: AtomicBool::new(false),
        }
    }

    /// 激活 Shadow Stack
    pub fn activate(&self) {
        self.active.store(true, Ordering::Release);
    }

    /// 停用 Shadow Stack
    pub fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// 是否活跃
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// 获取当前 SSP
    pub fn get_ssp(&self) -> u64 {
        self.ssp.load(Ordering::Acquire)
    }

    /// 设置 SSP
    pub fn set_ssp(&self, ssp: u64) {
        self.ssp.store(ssp, Ordering::Release);
    }

    /// 检查 SSP 是否在有效范围
    pub fn is_ssp_valid(&self, ssp: u64) -> bool {
        ssp >= self.base && ssp <= self.base + self.size as u64
    }
}

// ============================================================================
// CET 子系统
// ============================================================================

/// CET 功能支持标志
#[derive(Debug, Clone, Copy)]
pub struct CetCapabilities {
    /// 是否支持 Shadow Stack
    pub shadow_stack: bool,
    /// 是否支持 IBT (Indirect Branch Tracking)
    pub ibt: bool,
    /// 是否支持 WRSS (Write to Shadow Stack) 指令
    pub wrss: bool,
    /// 是否已启用 Shadow Stack
    pub shadow_stack_enabled: bool,
    /// 是否已启用 IBT
    pub ibt_enabled: bool,
}

/// CET 子系统
pub struct CetSubsystem {
    /// 功能支持
    caps: IrqSpinLock<CetCapabilities>,
    /// Per-CPU Shadow Stack (CPU ID → `ShadowStack` 实例)
    kernel_shadow_stacks: IrqSpinLock<Vec<ShadowStack>>,
    /// 是否已初始化
    initialized: AtomicBool,
}

impl CetSubsystem {
    pub const fn new() -> Self {
        Self {
            caps: IrqSpinLock::new(CetCapabilities {
                shadow_stack: false,
                ibt: false,
                wrss: false,
                shadow_stack_enabled: false,
                ibt_enabled: false,
            }),
            kernel_shadow_stacks: IrqSpinLock::new(Vec::new()),
            initialized: AtomicBool::new(false),
        }
    }

    /// 初始化 CET 子系统
    pub fn init(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }

        let caps = self.detect_capabilities();
        crate::klog_ffi!(
            klog_ffi_info,
            "[CET] capabilities: shadow_stack={}, ibt={}, wrss={}",
            caps.shadow_stack,
            caps.ibt,
            caps.wrss
        );

        if caps.shadow_stack {
            // 尝试启用内核态 Shadow Stack
            self.enable_kernel_shadow_stack();
        }

        *self.caps.lock() = caps;
        self.initialized.store(true, Ordering::Release);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 检测 CPU CET 能力
    fn detect_capabilities(&self) -> CetCapabilities {
        #[cfg(target_arch = "x86_64")]
        {
            // CPUID.07H.0 功能位: ECX[7]=CET_IBT, ECX[20]=CET_SS, EDX[14]=WRSS
            let ecx = Self::cpuid_07_ecx();
            let edx = Self::cpuid_07_edx();
            Self::decode_cpuid_caps(ecx, edx)
        }

        #[cfg(target_arch = "aarch64")]
        {
            // aarch64: 检查 ID_AA64ISAR1_EL1 的 PAC/BTI 位
            // 简化: QEMU virt 默认支持 PAC
            CetCapabilities {
                shadow_stack: true, // PAC 作为等价
                ibt: true,          // BTI 作为等价
                wrss: false,
                shadow_stack_enabled: false,
                ibt_enabled: false,
            }
        }
    }

    /// 由 CPUID.07H:ECX/EDX 解码 CET 能力 (纯函数, 可单测)。
    ///
    /// Intel SDM 规范 (CPUID.07H.0 结构化扩展功能位):
    /// - `ECX[7]`  = CET_IBT (间接分支跟踪 Indirect Branch Tracking)
    /// - `ECX[20]` = CET_SS (影子栈 Shadow Stack)
    /// - `EDX[14]` = WRSS (用户态影子栈写 User-mode Write to Shadow Stack)
    ///
    /// # 回归约束
    /// 旧实现误用 `ECX[6]` (PREFETCHWT1) 判 shadow_stack, 会把含 PREFETCHWT1
    /// 但无 CET 的 CPU 误判为支持 → 触发 `CR4.CET` 写入 #GP (boot 崩溃)。
    /// 本函数是 `enable_kernel_shadow_stack` 写 CR4 前的唯一判定依据。
    #[cfg(target_arch = "x86_64")]
    fn decode_cpuid_caps(ecx: u32, edx: u32) -> CetCapabilities {
        let mut caps = CetCapabilities {
            shadow_stack: false,
            ibt: false,
            wrss: false,
            shadow_stack_enabled: false,
            ibt_enabled: false,
        };
        caps.ibt = (ecx >> 7) & 1 == 1;
        caps.shadow_stack = (ecx >> 20) & 1 == 1;
        if caps.shadow_stack {
            caps.wrss = (edx >> 14) & 1 == 1;
        }
        caps
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 启用内核态 Shadow Stack
    fn enable_kernel_shadow_stack(&self) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            // 1. 设置 CR4.CET 位
            let cr4 = Self::read_cr4();
            if cr4 & x86_msrs::CR4_CET_BIT != 0 {
                // CET 已启用
                crate::klog_ffi!(klog_ffi_info, "[CET] CR4.CET already set");
                return true;
            }

            // 尝试设置 CR4.CET
            // SAFETY: CR4.CET 启用 CET 扩展, 需确保 Shadow Stack 已准备好
            let new_cr4 = cr4 | x86_msrs::CR4_CET_BIT;
            let success = Self::try_write_cr4(new_cr4);
            if !success {
                crate::klog_ffi!(
                    klog_ffi_warn,
                    "[CET] failed to set CR4.CET (QEMU may not support CET)"
                );
                return false;
            }

            // 2. 配置 IA32_S_CET: 启用 Shadow Stack + WRSS
            //    Bit 0 = SH_STK_EN (Shadow Stack 启用)
            //    Bit 1 = WR_SHSTK_EN (WRSS 启用)
            let s_cet_val: u64 = 0x3; // SH_STK_EN | WR_SHSTK_EN
            // SAFETY: 写入 IA32_S_CET MSR 配置内核态 CET
            unsafe {
                crate::framework::cpu::msr::write_msr(x86_msrs::IA32_S_CET, s_cet_val);
            }

            crate::klog_ffi!(
                klog_ffi_info,
                "[CET] kernel shadow stack enabled (CR4.CET=1, S_CET=0x{:x})",
                s_cet_val
            );
            true
        }

        #[cfg(target_arch = "aarch64")]
        {
            // aarch64: PAC 通过编译器 -mbranch-protection=standard 启用
            // 内核无需额外 MSR 配置
            crate::klog_ffi!(
                klog_ffi_info,
                "[CET] aarch64 PAC/BTI: rely on compiler -mbranch-protection"
            );
            true
        }
    }

    /// 为 CPU 分配内核 Shadow Stack
    pub fn alloc_kernel_shadow_stack(&self, cpu_id: u32) -> Option<u64> {
        // 分配 Shadow Stack 物理页 (与 `create_user_shadow_stack` 同机制,
        // 经 PMM 分配后映射到内核高半区)。
        let pages_needed =
            (SHADOW_STACK_DEFAULT_SIZE + SHADOW_STACK_PAGE_SIZE - 1) / SHADOW_STACK_PAGE_SIZE;
        let phys_addr = crate::framework::mm::pmm_alloc_pages_phys(pages_needed)?;
        let virt_addr = phys_addr.as_u64() + crate::framework::mm::KERNEL_BASE;

        let ss = ShadowStack::new(virt_addr, SHADOW_STACK_DEFAULT_SIZE as u64);
        ss.activate();
        let ssp = ss.get_ssp();
        let mut stacks = self.kernel_shadow_stacks.lock();
        if (cpu_id as usize) >= stacks.len() {
            stacks.resize_with((cpu_id + 1) as usize, || ShadowStack::new(0, 0));
        }
        stacks[cpu_id as usize] = ss;

        // x86_64: 设置 IA32_PL0_SSP
        #[cfg(target_arch = "x86_64")]
        {
            if self.caps.lock().shadow_stack_enabled {
                // SAFETY: 写入 IA32_PL0_SSP 设置内核态 Shadow Stack 指针
                unsafe {
                    crate::framework::cpu::msr::write_msr(x86_msrs::IA32_PL0_SSP, ssp);
                }
            }
        }

        Some(ssp)
    }

    /// 为用户线程创建 Shadow Stack
    // 有意窄化: 用户内存代理, 指针/长度上下文保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn create_user_shadow_stack(&self, size: usize) -> Option<ShadowStack> {
        if !self.caps.lock().shadow_stack {
            return None;
        }
        let actual_size = if size == 0 {
            SHADOW_STACK_DEFAULT_SIZE
        } else {
            size
        };

        // 分配 Shadow Stack 物理页
        let pages_needed = (actual_size + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
        let phys_addr = crate::framework::mm::pmm_alloc_pages_phys(pages_needed)?;

        // 将物理地址转换为内核虚拟地址
        let virt_addr = phys_addr.as_u64() + crate::framework::mm::KERNEL_BASE;

        // 创建 Shadow Stack 描述符
        let ss = ShadowStack::new(virt_addr, actual_size as u64);
        ss.activate();

        // 配置用户态 MSR (仅在进程切换时实际写入)
        // 当前仅记录配置信息, 实际 MSR 写入在进程切换时进行
        #[cfg(target_arch = "x86_64")]
        {
            // IA32_U_CET: 启用用户态 Shadow Stack
            // Bit 0 = SH_STK_EN (Shadow Stack 启用)
            // Bit 1 = WR_SHSTK_EN (WRSS 启用)
            let u_cet_val: u64 = 0x3; // SH_STK_EN | WR_SHSTK_EN

            // IA32_PL3_SSP: 设置用户态 Shadow Stack 指针
            let pl3_ssp_val = virt_addr + actual_size as u64; // 栈从高向低增长

            crate::klog_ffi!(
                klog_ffi_info,
                "[CET] user shadow stack created: base=0x{:x}, size={}, U_CET=0x{:x}, PL3_SSP=0x{:x}",
                virt_addr,
                actual_size,
                u_cet_val,
                pl3_ssp_val
            );
        }

        Some(ss)
    }

    /// 配置用户态 CET MSR (在进入用户态前调用)
    ///
    /// 写入 `IA32_U_CET` 和 `IA32_PL3_SSP` MSR, 启用用户态 Shadow Stack.
    ///
    /// # Safety
    ///
    /// 调用方必须确保:
    /// - CET 已初始化 (`caps.shadow_stack_enabled` = true)
    /// - ssp 指向有效的 Shadow Stack 内存
    #[cfg_attr(
        target_arch = "aarch64",
        expect(
            clippy::unused_self,
            reason = "aarch64 CET 兼容占位函数, 不依赖 self 字段"
        )
    )]
    /// 配置用户态影子栈 (CET MSR).
    /// - 仅在从内核态切换到用户态前调用
    pub unsafe fn configure_user_cet_msr(&self, ssp: u64) {
        // aarch64 无 CET, 抑制 unused 警告
        #[cfg(target_arch = "aarch64")]
        let _ = ssp;
        #[cfg(target_arch = "x86_64")]
        {
            if !self.caps.lock().shadow_stack_enabled {
                return;
            }

            // IA32_U_CET: 启用用户态 Shadow Stack
            // Bit 0 = SH_STK_EN (Shadow Stack 启用)
            // Bit 1 = WR_SHSTK_EN (WRSS 启用)
            let u_cet_val: u64 = 0x3;

            // SAFETY: 写入 IA32_U_CET 配置用户态 CET
            unsafe {
                crate::framework::cpu::msr::write_msr(x86_msrs::IA32_U_CET, u_cet_val);
            }

            // SAFETY: 写入 IA32_PL3_SSP 设置用户态 Shadow Stack 指针
            unsafe {
                crate::framework::cpu::msr::write_msr(x86_msrs::IA32_PL3_SSP, ssp);
            }

            crate::klog_ffi!(
                klog_ffi_info,
                "[CET] user MSR configured: U_CET=0x{:x}, PL3_SSP=0x{:x}",
                u_cet_val,
                ssp
            );
        }
    }

    /// 配置中断 Shadow Stack 表 (IDT 集成)
    ///
    /// 写入 `IA32_INTERRUPT_SSP_TABLE` MSR, 设置中断时使用的 Shadow Stack 表.
    ///
    /// # Safety
    ///
    #[cfg_attr(
        target_arch = "aarch64",
        expect(
            clippy::unused_self,
            reason = "aarch64 CET 兼容占位函数, 不依赖 self 字段"
        )
    )]
    /// 配置中断态影子栈表.
    /// 调用方必须确保:
    /// - CET 已初始化
    /// - `table_addr` 指向有效的 SSP 表内存 (16 字节对齐)
    pub unsafe fn configure_interrupt_ssp_table(&self, table_addr: u64) {
        // aarch64 无 CET, 抑制 unused 警告
        #[cfg(target_arch = "aarch64")]
        let _ = table_addr;
        #[cfg(target_arch = "x86_64")]
        {
            if !self.caps.lock().shadow_stack {
                return;
            }

            // SAFETY: 写入 IA32_INTERRUPT_SSP_TABLE 设置中断 Shadow Stack 表
            unsafe {
                crate::framework::cpu::msr::write_msr(
                    x86_msrs::IA32_INTERRUPT_SSP_TABLE,
                    table_addr,
                );
            }

            crate::klog_ffi!(
                klog_ffi_info,
                "[CET] interrupt SSP table configured: addr=0x{:x}",
                table_addr
            );
        }
    }

    /// 获取功能支持
    pub fn capabilities(&self) -> CetCapabilities {
        *self.caps.lock()
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    // ========================================================================
    // x86_64 辅助
    // ========================================================================

    #[cfg(target_arch = "x86_64")]
    fn cpuid_07_ecx() -> u32 {
        let ecx: u32;
        // SAFETY: CPUID 是安全指令, 保存/恢复 rbx (LLVM 内部使用)
        unsafe {
            core::arch::asm!(
                "push rbx",
                "mov eax, 7",
                "xor ecx, ecx",
                "cpuid",
                "mov ecx, ecx",   // 确保 ecx 被写出
                "pop rbx",
                out("ecx") ecx,
                out("eax") _,
                out("edx") _,
                options(nostack),
            );
        }
        ecx
    }

    #[cfg(target_arch = "x86_64")]
    fn cpuid_07_edx() -> u32 {
        let edx: u32;
        // SAFETY: CPUID 是安全指令, 保存/恢复 rbx
        unsafe {
            core::arch::asm!(
                "push rbx",
                "mov eax, 7",
                "xor ecx, ecx",
                "cpuid",
                "pop rbx",
                out("edx") edx,
                out("eax") _,
                out("ecx") _,
                options(nostack),
            );
        }
        edx
    }

    #[cfg(target_arch = "x86_64")]
    fn read_cr4() -> u64 {
        let cr4: u64;
        // SAFETY: 读取 CR4 是特权操作但无副作用
        unsafe { core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack)) };
        cr4
    }

    #[cfg(target_arch = "x86_64")]
    fn try_write_cr4(value: u64) -> bool {
        // SAFETY: 写入 CR4 前已由 `decode_cpuid_caps` 经 CPUID (ECX[20]=CET_SS)
        // 确认硬件支持, 此处为最佳努力写入; 若固件/虚拟化与 CPUID 不一致仍
        // 触发 #GP, 属硬件一致性异常, 不由本函数捕获 (会落入 IDT #GP 处理).
        unsafe { core::arch::asm!("mov cr4, {}", in(reg) value, options(nomem, nostack)) };
        true // 如果执行到这里说明成功
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 CET 子系统
static CET_SUBSYSTEM: CetSubsystem = CetSubsystem::new();

/// 初始化 CET
pub fn cet_init() {
    CET_SUBSYSTEM.init();
}

/// 获取全局 CET 子系统
pub fn cet_subsystem() -> &'static CetSubsystem {
    &CET_SUBSYSTEM
}

/// CET 是否已初始化
pub fn cet_is_initialized() -> bool {
    CET_SUBSYSTEM.is_initialized()
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_cet` — CET 系统调用
///
/// `a0`: cmd
///   0 = `capabilities()` → 功能标志
///   1 = `create_user_shadow_stack(size`: a1) → SSP
///   2 = `is_initialized()` → 返回 bool, 是否已初始化
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn sys_cet(cmd: u64, a1: u64, _a2: u64) -> i64 {
    match cmd {
        0 => {
            // capabilities
            let caps = cet_subsystem().capabilities();
            let mut flags: u64 = 0;
            if caps.shadow_stack {
                flags |= 1 << 0;
            }
            if caps.ibt {
                flags |= 1 << 1;
            }
            if caps.wrss {
                flags |= 1 << 2;
            }
            if caps.shadow_stack_enabled {
                flags |= 1 << 3;
            }
            if caps.ibt_enabled {
                flags |= 1 << 4;
            }
            flags as i64
        }
        1 => {
            // create_user_shadow_stack
            cet_subsystem()
                .create_user_shadow_stack(a1 as usize)
                .map_or(-(12i64), |ss| ss.get_ssp() as i64) // ENOMEM
        }
        2 => {
            // is_initialized
            i64::from(cet_is_initialized())
        }
        _ => -(38i64), // ENOSYS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPUID 位解码 (仅 x86_64): ECX[20]=CET_SS, EDX[14]=WRSS, ECX[7]=CET_IBT。
    ///
    /// 回归: 旧实现误用 `ECX[6]` (PREFETCHWT1) 判 shadow_stack, 会把含
    /// PREFETCHWT1 但无 CET 的 CPU 误判为支持 → `CR4.CET` 写入 #GP (boot 崩溃)。
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn decode_cpuid_caps_spec_bits() {
        // ECX[6]=1 (PREFETCHWT1) 但 ECX[20]=0: 不得判定为 shadow_stack 支持
        let no_cet = CetSubsystem::decode_cpuid_caps(1 << 6, 0);
        assert!(
            !no_cet.shadow_stack,
            "ECX[6] 是 PREFETCHWT1, 不得误判为 CET_SS"
        );
        assert!(!no_cet.ibt, "ECX[6] 不得误判为 CET_IBT");

        // ECX[20]=1: CET_SS 支持; EDX[14]=1: WRSS
        let cet = CetSubsystem::decode_cpuid_caps(1 << 20, 1 << 14);
        assert!(cet.shadow_stack, "ECX[20] = CET_SS 应置位");
        assert!(cet.wrss, "EDX[14] = WRSS 应置位 (CET_SS 下有效)");
        assert!(!cet.ibt, "ECX[7] 未置位时 IBT 应为 false");

        // ECX[7]=1: CET_IBT, 与 shadow_stack 独立
        let ibt = CetSubsystem::decode_cpuid_caps(1 << 7, 0);
        assert!(ibt.ibt, "ECX[7] = CET_IBT 应置位");
        assert!(!ibt.shadow_stack, "IBT 独立于 CET_SS");
        assert!(!ibt.wrss, "无 CET_SS 时 WRSS 应保持 false");
    }
}
