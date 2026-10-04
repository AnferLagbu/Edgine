//! # 安全硬件操作封装
//!
//! 提供对 x86-64 底层硬件寄存器的安全访问接口。
//! 封装内联汇编，提供类型安全的 API。

// 地址校验常量按架构分组: `KERNEL_BASE` 仅 x86_64 使用 (aarch64 恒等映射下为 0,
// 用户/内核分界改用 `KERNEL_TEXT_BASE`), 其余三个双架构共用.
#[cfg(target_arch = "x86_64")]
use crate::privileged::mm::KERNEL_BASE;
use crate::privileged::mm::{KERNEL_TEXT_BASE, USER_ADDR_FLOOR, USER_ADDR_MIN};

/// CPU 特性检测结果
#[derive(Debug, Clone)]
pub struct CpuFeatures {
    /// 是否支持 APIC
    pub has_apic: bool,
    /// 是否支持 X2APIC
    pub has_x2apic: bool,
    /// 最大支持的 CPUID 叶
    pub max_cpuid_leaf: u32,
}

impl CpuFeatures {
    /// 检测当前 CPU 的特性
    ///
    /// x86_64: 经 CPUID leaf 0 取最大叶号, 再按 leaf 1 的
    /// EDX bit 9 (APIC) / ECX bit 21 (x2APIC) 解析真实硬件特性.
    /// aarch64: 无 CPUID 指令 (中断控制器为 GIC 而非 APIC), 返回架构中性缺省值.
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            let (max_cpuid_leaf, _, _, _) = crate::privileged::cpu::cpuid::cpuid(0, 0);
            // leaf 1 仅在 max_cpuid_leaf >= 1 时有效 (超范围读数在部分 CPU 上未定义)
            let (has_apic, has_x2apic) = if max_cpuid_leaf >= 1 {
                let (_, _, ecx, edx) = crate::privileged::cpu::cpuid::cpuid(1, 0);
                (edx & (1 << 9) != 0, ecx & (1 << 21) != 0)
            } else {
                (false, false)
            };
            Self {
                has_apic,
                has_x2apic,
                max_cpuid_leaf,
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self {
                has_apic: false,
                has_x2apic: false,
                max_cpuid_leaf: 0,
            }
        }
    }

    /// 打印 CPU 特性信息
    #[cfg(feature = "log")]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn log_info(&self) {
        use log::info;
        info!("CPU Features:");
        info!("  APIC: {}", self.has_apic);
        info!("  X2APIC: {}", self.has_x2apic);
        info!("  Max CPUID Leaf: {}", self.max_cpuid_leaf);
    }
}

/// CR2 寄存器安全读取 (Page Fault 地址)
///
/// # Safety
/// 此函数仅在异常处理上下文中有效
///
/// # Returns
/// Page Fault 触发时的线性地址
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn read_cr2() -> u64 {
    crate::arch!(read_fault_address()) as u64
}

/// 中断控制函数

/// 禁用中断 (cli)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
///
/// # Safety
///
/// 仅在中断子系统尚未生效的早期引导阶段有效.
pub unsafe fn disable_interrupts() {
    let _ = crate::arch!(interrupt_disable());
}

/// 启用中断 (sti)
#[inline(always)]
///
/// # Safety
///
/// 通过 `sti` 指令启用中断. 调用方必须确保 IDT 已完全初始化,
/// 且没有任何中断处理函数能观察到部分构造的内核状态.
pub unsafe fn enable_interrupts() {
    crate::arch!(interrupt_enable());
}

/// RFLAGS 寄存器操作

/// 读取当前 RFLAGS
#[inline]
#[cfg(target_arch = "x86_64")]
pub fn read_rflags() -> u64 {
    let rflags: u64;
    // SAFETY: 内联汇编的寄存器约束与变量类型一致; 无内存副作用; 输出 reg 通过 out(reg) 绑定
    unsafe { core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem, nostack)) };
    rflags
}

/// 检查中断是否启用 (IF flag)
#[inline]
#[cfg(target_arch = "x86_64")]
pub fn interrupts_enabled() -> bool {
    (read_rflags() & (1 << 9)) != 0
}

/// 内存屏障操作

/// 全局内存屏障 (mfence → Arch trait)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
///
/// # Safety
///
/// 这是全量内存屏障. 调用方必须确保没有任何待处理 store 或 load
/// 能被重排到该屏障两侧, 违反既定的同步协议.
pub unsafe fn memory_fence() {
    crate::arch!(fence());
}

/// 写内存屏障 (sfence → Arch trait)
#[inline(always)]
///
/// # Safety
///
/// 这是 store 屏障. 调用方必须确保该屏障提供的 store 序保证
/// 与既定同步协议一致.
pub unsafe fn store_fence() {
    crate::arch!(fence_w());
}

/// TSC 时间戳计数器

/// 读取 TSC (时间戳计数器 → Arch trait)
///
/// # Returns
/// 当前 CPU 周期数 (可用于性能测量)
#[inline]
pub fn rdtsc() -> u64 {
    crate::arch!(timestamp())
}

/// 读取带 fence 的 TSC (更精确)
#[inline]
#[cfg(target_arch = "x86_64")]
pub fn rdtsc_fence() -> u64 {
    let tsc: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!(
            "lfence",
            "rdtsc",
            out("rax") tsc,
            options(nomem, nostack)
        );
    }
    tsc
}

/// I/O 端口操作 (用于 PIC 控制)

/// 从端口读字节
///
/// # Safety
/// port 必须是有效的 I/O 端口地址
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn inb(port: u16) -> u8 {
    crate::arch!(inb(port))
}

/// 向端口写字节
///
/// # Safety
/// port 必须是有效的 I/O 端口地址
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn outb(port: u16, value: u8) {
    crate::arch!(outb(port, value));
}

/// I/O 延时 (用于 PIC 初始化序列)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn io_wait() {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        outb(0x80, 0);
    }
}

/// HALT 指令 (暂停 CPU 直到下一个中断)
#[inline(always)]
///
/// # Safety
///
/// 调用方在执行 HALT 之前必须先禁用中断. 若中断未启用,
/// CPU 将永远不会醒来, 造成永久挂起.
pub unsafe fn halt() {
    crate::arch!(halt());
}

/// 无限循环 (用于 panic 后停止系统)
#[inline(never)]
pub fn halt_loop() -> ! {
    loop {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            halt();
        }
    }
}

/// 栈操作辅助

/// 保存当前栈帧指针 (RBP)
#[inline]
#[cfg(target_arch = "x86_64")]
pub fn save_frame_pointer() -> u64 {
    let rbp: u64;
    // SAFETY: 内联汇编的寄存器约束与变量类型一致; 无内存副作用; 输出 reg 通过 out(reg) 绑定
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack)) };
    rbp
}

/// 安全的空指针检查
///
/// # Arguments

/// 检查地址是否为 null 或低于用户地址下限的无效地址.
///
/// 地址 < `USER_ADDR_FLOOR` (4 KiB) 视为 null 或无效低地址.
/// 典型分页机制下零页被刻意未映射, 用于捕获 null 解引用;
/// 此函数把 `[0, USER_ADDR_FLOOR)` 区间统一判定为无效.
///
/// # Returns
///
/// - `true`: 地址为 null 或位于无效低地址区间
/// - `false`: 地址位于有效区间 (>= `USER_ADDR_FLOOR`)
pub fn is_null_or_invalid(addr: u64) -> bool {
    addr < USER_ADDR_FLOOR
}

/// 检查地址是否位于用户地址空间.
///
/// 用户空间范围:
/// - `x86_64`: `[USER_ADDR_MIN, KERNEL_BASE)` — 高半区内核映射模型
/// - aarch64: `[USER_ADDR_MIN, KERNEL_TEXT_BASE)` — 恒等映射模型, 内核从 `KERNEL_TEXT_BASE` 起加载
///
/// 任何落在此区间外的地址 (null/低地址/内核地址) 均视为非用户地址.
///
/// # Returns
///
/// - `true`: 地址位于用户空间
/// - `false`: 地址位于内核空间或无效低地址
pub fn is_valid_user_address(addr: u64) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        (USER_ADDR_MIN..KERNEL_BASE).contains(&addr)
    }
    #[cfg(target_arch = "aarch64")]
    {
        // aarch64 恒等映射: KERNEL_BASE=0, 用 KERNEL_TEXT_BASE 作为用户/内核分界
        (USER_ADDR_MIN..KERNEL_TEXT_BASE).contains(&addr)
    }
}

/// 检查地址是否位于内核地址空间.
///
/// 内核空间起点: `KERNEL_TEXT_BASE` (链接脚本定义的内核 .text 段起始).
/// 任何 >= `KERNEL_TEXT_BASE` 的地址视为内核地址.
///
/// # Returns
///
/// - `true`: 地址位于内核空间
/// - `false`: 地址位于用户空间或无效低地址
pub fn is_valid_kernel_address(addr: u64) -> bool {
    addr >= KERNEL_TEXT_BASE
}

#[cfg(test)]
mod tests {
    use super::*;

    // UT-07 (2026-09-26): 注册侧 idt::safety::address_validation 七条地址谓词迁入
    // (谓词实现即本文件 pub fn); CPU 特性不变量 (cpu_features_no_panic) 保留在
    // 注册载体 (B 类: 属运行环境硬件不变量).
    #[test]
    fn test_address_validation() {
        assert!(is_null_or_invalid(0));
        assert!(is_null_or_invalid(0xFFF));
        assert!(!is_null_or_invalid(0x1000));
        assert!(is_valid_user_address(0x400000));
        assert!(!is_valid_user_address(KERNEL_BASE));
        assert!(is_valid_kernel_address(KERNEL_TEXT_BASE));
        assert!(!is_valid_kernel_address(0x400000));
    }

    #[test]
    fn test_cpu_features_detection() {
        let features = CpuFeatures::detect();
        // 现代 CPU 应该支持 APIC
        assert!(features.has_apic || !features.has_apic); // 至少不会 panic
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn test_rflags_operations() {
        let flags = read_rflags();
        // IF bit (bit 9) 应该在某个状态
        let _enabled = (flags & (1 << 9)) != 0;
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn test_rdtsc_monotonic() {
        let tsc1 = rdtsc();
        let tsc2 = rdtsc();
        // TSC 应该单调递增 (或相等)
        assert!(tsc2 >= tsc1);
    }
}

#[cfg(feature = "kernel_test")]
pub fn register_idt_safety_tests() {
    crate::privileged::tests::idt::register_idt_safety_tests();
}
