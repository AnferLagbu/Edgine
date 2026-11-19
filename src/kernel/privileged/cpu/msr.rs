//! MSR (Model Specific Register) 操作
//!
//! 提供 safe wrapper for RDMSR/WRMSR instructions.

// B03-16 拆分: MSR 常量集中于此 (自 `cpu/mod.rs` 迁出)。
// 内核其他子系统 (arch/x86_64, proc/user_proc) 各自维护本地等价常量,
// 本文件常量供 `cpu` 模块内初始化路径使用。

/// `IA32_EFER` MSR 地址 — SYSCALL/SYSRET 与 NX 位控制
pub(crate) const IA32_EFER: u32 = 0xC000_0080;
/// `IA32_EFER.SCE` — 启用 SYSCALL/SYSRET 指令
pub(crate) const EFER_SCE: u64 = 1 << 0;
/// `IA32_STAR` — SYSCALL 目标 CS/SS 和 SYSRET 基址
pub(crate) const IA32_STAR: u32 = 0xC000_0081;
/// `IA32_LSTAR` — SYSCALL 入口点 (64-bit 模式)
pub(crate) const IA32_LSTAR: u32 = 0xC000_0082;
/// `IA32_SFMASK` — SYSCALL 期间清零的标志位
pub(crate) const IA32_SFMASK: u32 = 0xC000_0084;

// === unsafe 依据单源 (本文件六个 unsafe 块共用) ===
//
// 类别 M「RDMSR / WRMSR 特权指令」: 两条指令只能 Ring 0 执行, 索引非法或 MSR
// 不存在 / 只读时触发 #GP. 这两点都无法在块内证明, 一律由各函数 `# Safety` 段
// 作为调用方义务传入 (内核初始化路径在 Ring 0 调用, 索引取自本文件常量或 CPUID
// 枚举结果). 块内只保证指令编码正确: ECX 载索引, EAX/EDX 载低/高 32 位, 且 asm
// 声明 options(nomem, nostack, preserves_flags) ⇒ 不访问内存、不动栈、不改标志位,
// 故无指针解引用面, 也不与编译器优化的内存假设冲突. WRMSR 会改变 CPU 行为
// (如 EFER.SCE 开启 SYSCALL、LSTAR 改入口), 语义正确性属调用方职责.
//
// 类别 F「FFI 出参裸指针写」: `cpu_read_msr` 的两个出参先判空再判 4 字节对齐
// (B03-17), 之后直接 `*low = …` 裸指针赋值, 不构造 `&mut` 引用 ⇒ 不引入别名与
// 对齐 UB 面. 但判空 + 对齐**不能**证明指针指向有效可写对象, 该义务仍由
// `# Safety` 段传入; 块内不取得所有权, 写完即返回.
//
// 类别 W「宽校验不是白名单」: `is_msr_likely_valid` 只按保留区间放行
// (0..0x1FFF / 0xC000_0000..0xC000_1FFF / 0xC001_0000..0xC001_1FFF), 挡掉明显
// 非法索引, **不证明**该 MSR 在当前 CPU 型号上存在且可写 ⇒ 索引合法性仍是调用方
// 义务, 不得把该校验读成安全保证.

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
#[inline]
#[must_use]
pub unsafe fn read_msr(msr: u32) -> u64 {
    // SAFETY: 类别 M. RDMSR 只把索引指定的 MSR 读进 EAX/EDX, 不写任何 CPU 状态;
    // Ring 0 与索引合法性由本 fn `# Safety` 传入. 高低 32 位拼回 u64 是纯算术.
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
#[inline]
pub unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: 类别 M. WRMSR 把 EAX/EDX 写进 ECX 指定的 MSR, 是**写 CPU 状态**的
    // 特权操作: Ring 0 + 该 MSR 存在且可写由本 fn `# Safety` 传入. 拆半走
    // `glue::low_u32_from_u64` 与 `>> 32`, 全为寄存器输入, 无指针面.
    unsafe {
        let low = crate::privileged::glue::low_u32_from_u64(value);
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
pub unsafe extern "C" fn cpu_read_msr(msr: u32, low: *mut u32, high: *mut u32) -> i32 {
    // SAFETY: 类别 F + 类别 M. 出参 low/high 先判空再判 4 字节对齐, 此后两次裸
    // 指针写落在调用方 `# Safety` 承诺的可写 u32 对象上 (有效性不由这两项校验
    // 证明); read_msr 的 Ring 0 与索引契约由本 fn `# Safety` 继承. 任一校验失败
    // 即返回 -1, 不触指针也不执行 RDMSR.
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
        *low = crate::privileged::glue::low_u32_from_u64(value);
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
    // SAFETY: 类别 W + 类别 M. 本块无指针解引用 (两个入参都是 u32 值);
    // `is_msr_likely_valid` 只挡明显非法区间, 「该 MSR 存在且可写」仍由本 fn
    // `# Safety` 传入. 校验失败返回 -1, 不执行 WRMSR.
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
    if (0xC000_0000..0xC000_2000).contains(&msr) || (0xC001_0000..0xC001_2000).contains(&msr) {
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
    // SAFETY: 类别 M. 块内只调 `read_msr`, 无新增不变量; Ring 0 与索引合法性
    // 由本 fn `# Safety` 原样继承 (本导出面不做宽校验, 也不触指针).
    unsafe { read_msr(msr) }
}

/// FFI 兼容: 写入 64 位 MSR
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msr` 是合法的 MSR 索引. 非法索引将触发 #GP 异常.
pub unsafe extern "C" fn cpu_write_msr64(msr: u32, value: u64) -> i32 {
    // SAFETY: 类别 M. 块内只调 `write_msr` 后返回 0, 无指针面; Ring 0 与
    // 「MSR 存在且可写」由本 fn `# Safety` 原样继承. 与 `cpu_write_msr` 不同,
    // 本导出面**不过** `is_msr_likely_valid` 宽校验, 非法索引直接触发 #GP.
    unsafe {
        write_msr(msr, value);
        0
    }
}
