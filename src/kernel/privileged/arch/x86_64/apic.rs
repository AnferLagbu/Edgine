//! Local APIC (高级可编程中断控制器) 驱动
//!
//! `x86_64` Local APIC: 每 CPU 中断控制、定时器与 IPI。

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
static APIC_TIMER_CALIBRATED: AtomicBool = AtomicBool::new(false);
static APIC_TIMER_HZ: AtomicU64 = AtomicU64::new(0);

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

pub fn apic_read(reg: u32) -> u32 {
    let base = APIC_BASE.load(Ordering::Acquire);
    // SAFETY: `const` 由调用方保证为有效指针; 只读访问
    unsafe { core::ptr::read_volatile((base + u64::from(reg)) as *const u32) }
}

pub fn apic_write(reg: u32, value: u32) {
    let base = APIC_BASE.load(Ordering::Acquire);
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::ptr::write_volatile((base + u64::from(reg)) as *mut u32, value);
    }
}

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

pub fn is_initialized() -> bool {
    APIC_INITIALIZED.load(Ordering::Acquire)
}

pub fn get_id() -> u32 {
    if !is_initialized() {
        return 0;
    }
    apic_read(APIC_ID) >> 24
}

pub fn get_version() -> u32 {
    if !is_initialized() {
        return 0;
    }
    apic_read(APIC_VERSION) & 0xFF
}

pub fn eoi() {
    if is_initialized() {
        apic_write(APIC_EOI, 0);
    }
}

// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn send_ipi(apic_id: u8, vector: u8) {
    if !is_initialized() {
        return;
    }
    apic_write(APIC_ICR_HIGH, u32::from(apic_id) << 24);
    apic_write(APIC_ICR_LOW, u32::from(vector) | ICR_ASSERT as u32);
    while apic_read(APIC_ICR_LOW) & (1 << 12) != 0 {}
}

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
    while apic_read(APIC_ICR_LOW) & (1 << 12) != 0 {}
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
pub fn init_timer(vector: u8, periodic: bool, divisor: u32) {
    if !is_initialized() {
        return;
    }

    let div_val = match divisor {
        1 => TIMER_DIV_1,
        2 => TIMER_DIV_2,
        4 => TIMER_DIV_4,
        8 => TIMER_DIV_8,
        16 => TIMER_DIV_16,
        32 => TIMER_DIV_32,
        64 => TIMER_DIV_64,
        128 => TIMER_DIV_128,
        _ => TIMER_DIV_16,
    };

    apic_write(APIC_TIMER_DCR, div_val);

    let mode = if periodic { LVT_TIMER_PERIODIC } else { 0 };
    apic_write(APIC_LVT_TIMER, mode | u32::from(vector));
}

pub fn set_timer_count(count: u32) {
    if is_initialized() {
        apic_write(APIC_TIMER_ICR, count);
    }
}

pub fn get_timer_count() -> u32 {
    if !is_initialized() {
        return 0;
    }
    apic_read(APIC_TIMER_CCR)
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub fn calibrate_timer(_pit_hz: u64, target_ms: u64) -> u64 {
    if !is_initialized() {
        return 0;
    }

    apic_write(APIC_TIMER_DCR, TIMER_DIV_16);
    apic_write(APIC_LVT_TIMER, LVT_MASK);
    apic_write(APIC_TIMER_ICR, 0xFFFFFFFF);

    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    unsafe extern "C" {
        fn timer_sleep_busy(ms: u64);
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        timer_sleep_busy(target_ms);
    }

    let remaining = apic_read(APIC_TIMER_CCR);
    let elapsed = 0xFFFFFFFFu32 - remaining;

    let ticks_per_ms = u64::from(elapsed) / target_ms;
    let apic_hz = ticks_per_ms * 16 * 1000;

    APIC_TIMER_HZ.store(apic_hz, Ordering::Release);
    APIC_TIMER_CALIBRATED.store(true, Ordering::Release);

    apic_hz
}

pub fn get_timer_hz() -> u64 {
    APIC_TIMER_HZ.load(Ordering::Acquire)
}

pub fn is_timer_calibrated() -> bool {
    APIC_TIMER_CALIBRATED.load(Ordering::Acquire)
}

pub fn mask_lint0() {
    if is_initialized() {
        apic_write(APIC_LVT_LINT0, apic_read(APIC_LVT_LINT0) | LVT_MASK);
    }
}

pub fn mask_lint1() {
    if is_initialized() {
        apic_write(APIC_LVT_LINT1, apic_read(APIC_LVT_LINT1) | LVT_MASK);
    }
}

pub fn unmask_lint0(mode: u32) {
    if is_initialized() {
        apic_write(APIC_LVT_LINT0, mode);
    }
}

pub fn unmask_lint1(mode: u32) {
    if is_initialized() {
        apic_write(APIC_LVT_LINT1, mode);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_init() {
    init();
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_eoi() {
    eoi();
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_is_ready() -> bool {
    is_initialized()
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_get_id() -> u32 {
    get_id()
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_send_ipi(apic_id: u8, vector: u8) {
    send_ipi(apic_id, vector);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_broadcast_ipi(vector: u8) {
    broadcast_ipi(vector);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_init_timer(vector: u8, periodic: bool, divisor: u32) {
    init_timer(vector, periodic, divisor);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_set_timer_count(count: u32) {
    set_timer_count(count);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn apic_calibrate_timer(pit_hz: u64, target_ms: u64) -> u64 {
    calibrate_timer(pit_hz, target_ms)
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
