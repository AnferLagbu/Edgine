//! MSR (Model Specific Register) 操作
//!
//! 提供 safe wrapper for RDMSR/WRMSR instructions.

// B03-16 拆分: MSR 常量集中于此 (自 `cpu/mod.rs` 迁出)。
// 内核其他子系统 (arch/x86_64, proc/user_proc) 各自维护本地等价常量,
// 本文件常量供 `cpu` 模块内初始化路径使用。

/// `IA32_EFER` MSR 地址 — SYSCALL/SYSRET 与 NX 位控制
pub(crate) const IA32_EFER: u32 = 0xC0000080;
/// `IA32_EFER.SCE` — 启用 SYSCALL/SYSRET 指令
pub(crate) const EFER_SCE: u64 = 1 << 0;
/// `IA32_STAR` — SYSCALL 目标 CS/SS 和 SYSRET 基址
pub(crate) const IA32_STAR: u32 = 0xC0000081;
/// `IA32_LSTAR` — SYSCALL 入口点 (64-bit 模式)
pub(crate) const IA32_LSTAR: u32 = 0xC0000082;
/// `IA32_SFMASK` — SYSCALL 期间清零的标志位
pub(crate) const IA32_SFMASK: u32 = 0xC0000084;

/// 读取 64 位 MSR 寄存器
///
/// # Arguments
/// * `msr` - MSR 地址 (如 0xC0000080 for `IA32_EFER`)
///
/// # Returns
/// MSR 的 64 位值
///
/// # Safety
/// 必须在 Ring 0 (内核态) 调用, 否则触发 #GP 异常。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn read_msr(msr: u32) -> u64 {
    unsafe {
        let (low, high): (u32, u32);

        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );

        (u64::from(high) << 32) | u64::from(low)
    }
}

/// 写入 64 位 MSR 寄存器
///
/// # Arguments
/// * `msr` - MSR 地址
/// * `value` - 要写入的 64 位值
///
/// # Safety
/// 必须在 Ring 0 调用, 且 MSR 必须存在且可写。
#[inline(always)]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub unsafe fn write_msr(msr: u32, value: u64) {
    unsafe {
        let low = value as u32;
        let high = (value >> 32) as u32;

        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// FFI 兼容: 读取 MSR (返回两个 32 位值)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msr` 是合法的 MSR 索引. 非法索引将触发 #GP 异常.
/// `low` 和 `high` 是合法可写指针.
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn cpu_read_msr(msr: u32, low: *mut u32, high: *mut u32) -> i32 {
    unsafe {
        if low.is_null() || high.is_null() {
            return -1;
        }

        // B03-17 修复: 对齐校验. 之前仅 `is_null()` 检查, 未对齐的 *mut u32
        // 在 aarch64 (4 字节对齐) / RISC-V 上触发 data abort.
        if !(low as usize).is_multiple_of(4) || !(high as usize).is_multiple_of(4) {
            return -1;
        }

        let value = read_msr(msr);
        *low = value as u32;
        *high = (value >> 32) as u32;

        0
    }
}

/// FFI 兼容: 写入 MSR
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msr` 是合法的 MSR 索引. 非法索引将触发 #GP 异常.
pub unsafe extern "C" fn cpu_write_msr(msr: u32, low: u32, high: u32) -> i32 {
    unsafe {
        // B03-17 修复: MSR 合法性预检. 之前非法 MSR 触发 #GP 后仍返回 0,
        // 假设成功 — 静默掩盖了 #GP 异常. 现对已知保留 MSR 范围 (含 IA32/FAM6+
        // 标准 MSR 0..0x1FFF + 扩展 0xC0000000..0xC0001FFF) 允许, 其他返回 -1.
        // 严格白名单需完整 MSR 表, 当前采用"保留范围"宽校验 + 注释说明.
        if !is_msr_likely_valid(msr) {
            return -1;
        }
        write_msr(msr, (u64::from(high) << 32) | u64::from(low));
        0
    }
}

/// B03-17: MSR 范围宽校验. 严格白名单需 CPU 型号特定表, 当前采用
/// 保留区间判定 (IA32 0..0x1FFF, EFER/STAR/LSTAR/SFMASK 等 0xC0000000+,
/// AMD 扩展 0xC0010000+). 不在区间内视为非法, 返回 false.
fn is_msr_likely_valid(msr: u32) -> bool {
    // IA32 架构 MSR: 0..0x1FFF (含 TSC/APIC/SYSCFG 等)
    if msr < 0x2000 {
        return true;
    }
    // AMD K8/K10/MSR: 0xC0000000..0xC0001FFF + 0xC0010000..0xC0011FFF
    if (0xC0000000..0xC0002000).contains(&msr) || (0xC0010000..0xC0012000).contains(&msr) {
        return true;
    }
    // 其他范围视为非法 (vmx/svm 特权 MSR, 型号特定 MSR 等)
    false
}

/// FFI 兼容: 读取 64 位 MSR
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msr` 是合法的 MSR 索引. 非法索引将触发 #GP 异常.
pub unsafe extern "C" fn cpu_read_msr64(msr: u32) -> u64 {
    unsafe { read_msr(msr) }
}

/// FFI 兼容: 写入 64 位 MSR
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msr` 是合法的 MSR 索引. 非法索引将触发 #GP 异常.
pub unsafe extern "C" fn cpu_write_msr64(msr: u32, value: u64) -> i32 {
    unsafe {
        write_msr(msr, value);
        0
    }
}
