//! Local APIC (高级可编程中断控制器) 驱动
//!
//! `x86_64` Local APIC: 每 CPU 中断控制、定时器与 IPI。

#![deny(missing_docs)]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const APIC_BASE_MSR: u32 = 0x1B;
const APIC_BASE_ADDR_MASK: u64 = 0xFFFF_F000;
const APIC_BASE_ENABLE: u64 = 1 << 11;

const APIC_ID: u32 = 0x020;
const APIC_VERSION: u32 = 0x030;
const APIC_TPR: u32 = 0x080;
const APIC_EOI: u32 = 0x0B0;
const APIC_SVR: u32 = 0x0F0;
const APIC_ESR: u32 = 0x280;
const APIC_ICR_LOW: u32 = 0x300;
const APIC_ICR_HIGH: u32 = 0x310;
const APIC_LVT_TIMER: u32 = 0x320;
const APIC_LVT_THERMAL: u32 = 0x330;
const APIC_LVT_PERF: u32 = 0x340;
const APIC_LVT_LINT0: u32 = 0x350;
const APIC_LVT_LINT1: u32 = 0x360;
const APIC_LVT_ERROR: u32 = 0x370;
const APIC_TIMER_ICR: u32 = 0x380;
const APIC_TIMER_CCR: u32 = 0x390;
const APIC_TIMER_DCR: u32 = 0x3E0;

// ISR/TMR/IRR 寄存器组基址 (每个 8 x 32-bit 寄存器, 覆盖 256 个中断向量)
/// In-Service Register 基址 (0x100-0x17F)
const APIC_ISR_BASE: u32 = 0x100;
/// Trigger Mode Register 基址 (0x180-0x1FF)
const APIC_TMR_BASE: u32 = 0x180;
/// Interrupt Request Register 基址 (0x200-0x27F)
const APIC_IRR_BASE: u32 = 0x200;

const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASK: u32 = 1 << 16;
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
/// LAPIC 定时器 TSC-Deadline 模式位 (Intel SDM 3-1079): 置 1 后计数器由
/// `IA32_TSC_DEADLINE` 驱动, LVT Timer_Count 写入被忽略.
const LVT_TIMER_TSC_DEADLINE: u32 = 1 << 18;
/// `IA32_TSC_DEADLINE` MSR —— deadline 模式下的绝对 TSC 截止期.
const MSR_TSC_DEADLINE: u32 = 0x6E0;
// LVT 投递模式
const LVT_DELIVERY_FIXED: u32 = 0x000;
const LVT_DELIVERY_SMI: u32 = 0x200;
const LVT_DELIVERY_NMI: u32 = 0x400;
const LVT_DELIVERY_EXTINT: u32 = 0x700;

const ICR_ASSERT: u64 = 1 << 14;
// ICR 模式位
const ICR_LEVEL: u64 = 1 << 15;
const ICR_BROADCAST: u64 = 1 << 19;
const ICR_ALL_EXCLUDE_SELF: u64 = 1 << 18 | 1 << 19;

const TIMER_DIV_1: u32 = 0x0B;
const TIMER_DIV_2: u32 = 0x00;
const TIMER_DIV_4: u32 = 0x01;
const TIMER_DIV_8: u32 = 0x02;
const TIMER_DIV_16: u32 = 0x03;
const TIMER_DIV_32: u32 = 0x04;
const TIMER_DIV_64: u32 = 0x05;
const TIMER_DIV_128: u32 = 0x06;

static APIC_BASE: AtomicU64 = AtomicU64::new(0);
static APIC_INITIALIZED: AtomicBool = AtomicBool::new(false);

fn rdmsr(msr: u32) -> u64 {
    let (high, low): (u32, u32);
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("rdmsr", out("eax") low, out("edx") high, in("ecx") msr, options(nomem, nostack));
    }
    (u64::from(high) << 32) | u64::from(low)
}

// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
fn wrmsr(msr: u32, value: u64) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack));
    }
}

/// 读取 APIC 寄存器 (`reg` 为相对 MMIO 基址的偏移)。
pub fn apic_read(reg: u32) -> u32 {
    let base = APIC_BASE.load(Ordering::Acquire);
    // SAFETY: `const` 由调用方保证为有效指针; 只读访问
    unsafe { core::ptr::read_volatile((base + u64::from(reg)) as *const u32) }
}

/// 写入 APIC 寄存器 (`reg` 为相对 MMIO 基址的偏移)。
pub fn apic_write(reg: u32, value: u32) {
    let base = APIC_BASE.load(Ordering::Acquire);
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::ptr::write_volatile((base + u64::from(reg)) as *mut u32, value);
    }
}

/// 初始化本地 APIC: 使能 SVR, 屏蔽未用 LVT 项并清零 TPR/ESR。
pub fn init() {
    let msr = rdmsr(APIC_BASE_MSR);
    let base = msr & APIC_BASE_ADDR_MASK;

    if base == 0 {
        return;
    }

    wrmsr(APIC_BASE_MSR, base | APIC_BASE_ENABLE);

    APIC_BASE.store(base, Ordering::Release);

    apic_write(APIC_SVR, SVR_ENABLE | 0xFF);

    apic_write(APIC_LVT_TIMER, LVT_MASK);
    apic_write(APIC_LVT_THERMAL, LVT_MASK);
    apic_write(APIC_LVT_PERF, LVT_MASK);
    apic_write(APIC_LVT_LINT0, LVT_DELIVERY_EXTINT);
    apic_write(APIC_LVT_LINT1, LVT_MASK);
    apic_write(APIC_LVT_ERROR, LVT_MASK);

    apic_write(APIC_TPR, 0);

    apic_write(APIC_ESR, 0);
    apic_read(APIC_ESR);

    APIC_INITIALIZED.store(true, Ordering::Release);
}

/// 查询本地 APIC 是否已完成初始化。
pub fn is_initialized() -> bool {
    APIC_INITIALIZED.load(Ordering::Acquire)
}

/// 获取本地 APIC ID (未初始化时返回 0)。
pub fn get_id() -> u32 {
    if !is_initialized() {
        return 0;
    }
    apic_read(APIC_ID) >> 24
}

/// 获取本地 APIC 版本号低字节 (未初始化时返回 0)。
pub fn get_version() -> u32 {
    if !is_initialized() {
        return 0;
    }
    apic_read(APIC_VERSION) & 0xFF
}

/// 向本地 APIC 发送 EOI, 结束当前中断处理。
pub fn eoi() {
    if is_initialized() {
        apic_write(APIC_EOI, 0);
    }
}

/// ICR 投递完成等待的自旋上界。
///
/// 硬件正常情形下投递在数个周期内完成 (bit 12 清零); 本上界仅为防止在 IF=0 下
/// 因目标 APIC 异常而无限自旋 —— 那会挂死整核 (APS 早期挂死曾观测到该点自旋).
const ICR_DELIVERY_SPIN_LIMIT: u32 = 1_000_000;

/// 等待 ICR 投递完成 (bit 12 = Delivery Status 清零), 带上界自旋。
#[inline]
fn wait_icr_delivery() {
    let mut spins: u32 = 0;
    while apic_read(APIC_ICR_LOW) & (1 << 12) != 0 {
        spins = spins.saturating_add(1);
        if spins >= ICR_DELIVERY_SPIN_LIMIT {
            break;
        }
        core::hint::spin_loop();
    }
}

/// 向目标 APIC 发送 IPI, 等待 ICR 投递完成后返回。
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn send_ipi(apic_id: u8, vector: u8) {
    if !is_initialized() {
        return;
    }
    apic_write(APIC_ICR_HIGH, u32::from(apic_id) << 24);
    apic_write(APIC_ICR_LOW, u32::from(vector) | ICR_ASSERT as u32);
    wait_icr_delivery();
}

/// 向所有其他 CPU 广播 IPI, 等待 ICR 投递完成后返回。
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn broadcast_ipi(vector: u8) {
    if !is_initialized() {
        return;
    }
    apic_write(APIC_ICR_HIGH, 0);
    apic_write(
        APIC_ICR_LOW,
        u32::from(vector) | ICR_ALL_EXCLUDE_SELF as u32 | ICR_ASSERT as u32,
    );
    wait_icr_delivery();
}

/// LAPIC 定时器工作模式 (机制层枚举, 不含频率换算与策略).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerMode {
    /// 递减计数, 到 0 产生一次中断后停止 (one-shot), 需上层重新装填。
    OneShot,
    /// 递减计数, 到 0 自动重装初值 (周期)。
    Periodic,
    /// 由 `IA32_TSC_DEADLINE` 指定的绝对 TSC 值到期 (one-shot 语义)。
    TscDeadline,
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
/// 分频值 → DCR 编码 (硬件编码不连续, 保留完整分支以对照硬件手册阅读).
const fn timer_div_bits(divisor: u32) -> u32 {
    match divisor {
        1 => TIMER_DIV_1,
        2 => TIMER_DIV_2,
        4 => TIMER_DIV_4,
        8 => TIMER_DIV_8,
        16 => TIMER_DIV_16,
        32 => TIMER_DIV_32,
        64 => TIMER_DIV_64,
        128 => TIMER_DIV_128,
        _ => TIMER_DIV_16,
    }
}

/// 设置本核定时器分频器 (仅 count 模式参与计数; deadline 模式硬件不使用该值)。
pub fn set_timer_divisor(divisor: u32) {
    if is_initialized() {
        apic_write(APIC_TIMER_DCR, timer_div_bits(divisor));
    }
}

/// 编程本核定时器: 模式 + 投递向量。
///
/// 写 LVT 即解除屏蔽 (mask 位为 0), 屏蔽由 [`mask_timer`] 负责 —— 使"模式
/// 与向量装配"和"启停"成为两个正交原语, 装配次序由上层 (timer::clockevent) 决定。
pub fn program_timer(mode: TimerMode, vector: u8) {
    if !is_initialized() {
        return;
    }
    let mode_bits = match mode {
        TimerMode::TscDeadline => LVT_TIMER_TSC_DEADLINE,
        TimerMode::OneShot => 0,
        TimerMode::Periodic => LVT_TIMER_PERIODIC,
    };
    apic_write(APIC_LVT_TIMER, mode_bits | u32::from(vector));
}

/// 屏蔽本核定时器 (置 LVT mask 位), 不改变已装配的模式与向量。
pub fn mask_timer() {
    if is_initialized() {
        apic_write(APIC_LVT_TIMER, apic_read(APIC_LVT_TIMER) | LVT_MASK);
    }
}

/// 回读本核定时器 LVT (mask 位 + 模式位 + 向量; 未初始化时返回 0)。
///
/// 唯一消费者是 tick 源装配失败的诊断路径: 到 `program_timer` 之后 LVT 的实际
/// 内容决定了"模式没被接受 / 仍被屏蔽 / 向量写错"三种静默失效中到底是哪一种。
pub fn read_timer_lvt() -> u32 {
    if !is_initialized() {
        return 0;
    }
    apic_read(APIC_LVT_TIMER)
}

/// 装载定时器计数初值 (count 模式): 从该值递减到 0 触发。
pub fn set_timer_count(count: u32) {
    if is_initialized() {
        apic_write(APIC_TIMER_ICR, count);
    }
}

/// 读取定时器当前剩余计数 (未初始化时返回 0)。
pub fn get_timer_count() -> u32 {
    if !is_initialized() {
        return 0;
    }
    apic_read(APIC_TIMER_CCR)
}

/// 写 `IA32_TSC_DEADLINE`: 本核下一个定时器到期的绝对 TSC 值。
///
/// 仅在 [`timer_deadline_supported`] 为真时调用; 在不支持该 MSR 的 CPU 上写
/// 会触发 #GP。
pub fn set_deadline(deadline: u64) {
    if is_initialized() {
        wrmsr(MSR_TSC_DEADLINE, deadline);
    }
}

/// 探测本核 LAPIC 是否真正接受 TSC-Deadline 模式 (写 bit18 后回读)。
///
/// `CPUID.01H:ECX[24]` 是 CPU 侧的架构性声明, 但 LAPIC 集成可能未实现该位而
/// CPUID 仍报支持; 若仅据 CPUID 选定 deadline 而硬件忽略 bit18, 定时器永不
/// 到期 —— 表现为启动后该核再无 tick (挂死类故障, 事后极难归因). 回读探测
/// 把该风险收敛成一个可判定的分支。
///
/// 探测全程保持定时器屏蔽且不带向量号, 不影响已装配的 tick。
pub fn timer_deadline_supported() -> bool {
    if !is_initialized() {
        return false;
    }
    apic_write(APIC_LVT_TIMER, LVT_MASK);
    apic_write(APIC_LVT_TIMER, LVT_MASK | LVT_TIMER_TSC_DEADLINE);
    let accepted = apic_read(APIC_LVT_TIMER) & LVT_TIMER_TSC_DEADLINE != 0;
    apic_write(APIC_LVT_TIMER, LVT_MASK);
    accepted
}

/// 屏蔽 LINT0 本地中断 (置 LVT 掩码位)。
pub fn mask_lint0() {
    if is_initialized() {
        apic_write(APIC_LVT_LINT0, apic_read(APIC_LVT_LINT0) | LVT_MASK);
    }
}

/// 屏蔽 LINT1 本地中断 (置 LVT 掩码位)。
pub fn mask_lint1() {
    if is_initialized() {
        apic_write(APIC_LVT_LINT1, apic_read(APIC_LVT_LINT1) | LVT_MASK);
    }
}

/// 解除 LINT0 屏蔽并按 `mode` 设置投递模式。
pub fn unmask_lint0(mode: u32) {
    if is_initialized() {
        apic_write(APIC_LVT_LINT0, mode);
    }
}

/// 解除 LINT1 屏蔽并按 `mode` 设置投递模式。
pub fn unmask_lint1(mode: u32) {
    if is_initialized() {
        apic_write(APIC_LVT_LINT1, mode);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 初始化 Local APIC 硬件
#[unsafe(no_mangle)]
pub extern "C" fn apic_init() {
    init();
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 发送 EOI 结束当前中断
#[unsafe(no_mangle)]
pub extern "C" fn apic_eoi() {
    eoi();
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 返回 Local APIC 是否已初始化
#[unsafe(no_mangle)]
pub extern "C" fn apic_is_ready() -> bool {
    is_initialized()
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 返回当前 CPU 的 Local APIC ID
#[unsafe(no_mangle)]
pub extern "C" fn apic_get_id() -> u32 {
    get_id()
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 向指定 APIC ID 的 CPU 发送 IPI
#[unsafe(no_mangle)]
pub extern "C" fn apic_send_ipi(apic_id: u8, vector: u8) {
    send_ipi(apic_id, vector);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 向所有 CPU 广播 IPI
#[unsafe(no_mangle)]
pub extern "C" fn apic_broadcast_ipi(vector: u8) {
    broadcast_ipi(vector);
}

// ============================================================================
// LVT 配置 API
// ============================================================================

/// 配置 LVT LINT0 为指定投递模式
pub fn configure_lint0(mode: u32) {
    if is_initialized() {
        apic_write(APIC_LVT_LINT0, mode);
    }
}

/// 配置 LVT LINT1 为指定投递模式
pub fn configure_lint1(mode: u32) {
    if is_initialized() {
        apic_write(APIC_LVT_LINT1, mode);
    }
}

/// 获取 Fixed 投递模式常量
pub fn delivery_fixed() -> u32 {
    LVT_DELIVERY_FIXED
}

/// 获取 SMI 投递模式常量
pub fn delivery_smi() -> u32 {
    LVT_DELIVERY_SMI
}

/// 获取 NMI 投递模式常量
pub fn delivery_nmi() -> u32 {
    LVT_DELIVERY_NMI
}

/// 获取 `ExtINT` 投递模式常量
pub fn delivery_extint() -> u32 {
    LVT_DELIVERY_EXTINT
}

// ============================================================================
// ISR/TMR/IRR 内省 API
// ============================================================================

/// 读取 In-Service Register 的 256 位 (8 个 32-bit 寄存器)
///
/// ISR 记录当前正在处理的中断向量. 每个 bit 对应一个中断向量,
/// 置 1 表示该向量的中断正在被 CPU 处理 (尚未 EOI).
///
/// 返回 [u32; 8], 索引 i 对应 bit[i*32..(i+1)*32-1].
pub fn apic_read_isr() -> [u32; 8] {
    let mut isr = [0u32; 8];
    for i in 0..8u32 {
        isr[i as usize] = apic_read(APIC_ISR_BASE + i * 0x10);
    }
    isr
}

/// 读取 Trigger Mode Register 的 256 位 (8 个 32-bit 寄存器)
///
/// TMR 记录中断的触发模式: 每个 bit 对应一个中断向量,
/// 置 1 = 电平触发, 清 0 = 边沿触发.
///
/// 返回 [u32; 8], 索引 i 对应 bit[i*32..(i+1)*32-1].
pub fn apic_read_tmr() -> [u32; 8] {
    let mut tmr = [0u32; 8];
    for i in 0..8u32 {
        tmr[i as usize] = apic_read(APIC_TMR_BASE + i * 0x10);
    }
    tmr
}

/// 读取 Interrupt Request Register 的 256 位 (8 个 32-bit 寄存器)
///
/// IRR 记录待处理的中断请求. 每个 bit 对应一个中断向量,
/// 置 1 表示该向量有中断请求等待 CPU 响应.
///
/// 返回 [u32; 8], 索引 i 对应 bit[i*32..(i+1)*32-1].
pub fn apic_read_irr() -> [u32; 8] {
    let mut irr = [0u32; 8];
    for i in 0..8u32 {
        irr[i as usize] = apic_read(APIC_IRR_BASE + i * 0x10);
    }
    irr
}

/// 查询指定向量是否在 ISR 中 (正在处理)
pub fn apic_is_in_isr(vector: u8) -> bool {
    let reg = vector / 32;
    let bit = vector % 32;
    apic_read(APIC_ISR_BASE + u32::from(reg) * 0x10) & (1 << bit) != 0
}

/// 查询指定向量是否在 IRR 中 (待处理)
pub fn apic_is_in_irr(vector: u8) -> bool {
    let reg = vector / 32;
    let bit = vector % 32;
    apic_read(APIC_IRR_BASE + u32::from(reg) * 0x10) & (1 << bit) != 0
}

/// 查询指定向量的触发模式 (true = 电平触发, false = 边沿触发)
pub fn apic_is_level_triggered(vector: u8) -> bool {
    let reg = vector / 32;
    let bit = vector % 32;
    apic_read(APIC_TMR_BASE + u32::from(reg) * 0x10) & (1 << bit) != 0
}

// ============================================================================
// ICR 配置 API
// ============================================================================

/// 发送带 level 触发模式的 IPI
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn send_ipi_level(apic_id: u8, vector: u8) {
    if !is_initialized() {
        return;
    }
    apic_write(APIC_ICR_HIGH, u32::from(apic_id) << 24);
    apic_write(
        APIC_ICR_LOW,
        u32::from(vector) | ICR_ASSERT as u32 | ICR_LEVEL as u32,
    );
    while apic_read(APIC_ICR_LOW) & (1 << 12) != 0 {}
}

/// 发送带 broadcast 模式的 IPI
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn broadcast_ipi_level(vector: u8) {
    if !is_initialized() {
        return;
    }
    apic_write(APIC_ICR_HIGH, 0);
    apic_write(
        APIC_ICR_LOW,
        u32::from(vector) | ICR_ALL_EXCLUDE_SELF as u32 | ICR_ASSERT as u32 | ICR_LEVEL as u32,
    );
    while apic_read(APIC_ICR_LOW) & (1 << 12) != 0 {}
}

/// 获取 `ICR_LEVEL` 常量
pub fn icr_level() -> u64 {
    ICR_LEVEL
}

/// 获取 `ICR_BROADCAST` 常量
pub fn icr_broadcast() -> u64 {
    ICR_BROADCAST
}
