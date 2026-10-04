//! AArch64 FREG (Fault Recovery Stack) 实现
//!
//! ## 架构等价
//!
//! | x86_64              | aarch64               |
//! |---------------------|------------------------|
//! | `int 0x82`          | SGI 7 (via ICC_SGI1R) |
//! | IDT entry 0x82      | IRQ handler intid==7  |
//! | `iretq` 恢复        | `eret` 恢复           |
//! | IST2 专用栈         | EL1h SP (独立栈)      |
//!
//! ## 两种触发场景
//!
//! 1. **运行时恢复** (`freg_trigger_recovery()`):
//!    代码调用 → SGI 7 触发 → IRQ handler 执行恢复 → eret 回到调用点
//!
//! 2. **Panic 恢复** (panic_handler):
//!    直接调用 recovery_try_recover_from_panic() 进行域回滚,
//!    不回现场执行 (panic 不可恢复执行流).
//!
//! ## 在 IRQ handler 中的集成
//!
//! `irq_handler()` 检测 `intid == FREG_RECOVERY_SGI (7)`,
//! 调用 `freg_sgi_handler()` 执行恢复.
//!
//! ## 依赖
//!
//! - GICv3 已初始化 (SGI 7 已使能)
//! - FREG域已注册 (PMM / PROC)
//! - `src/kernel/freg/` 模块可用 (跨架构通用)

/// 触发FREG恢复 SGI (运行时使用)
///
/// 向自身发送 SGI 7, 将由 IRQ handler 中的 `freg_sgi_handler()` 处理.
/// 调用后, 中断返回 (eret) 将回到本函数调用点.
///
/// SGI 编号取自 [`super::gic::FREG_RECOVERY_SGI`], 使能由每核入口
/// [`super::gic::init_per_cpu`] 统一完成 (不再由本模块单独使能).
///
/// # Safety
///
/// 调用者必须在中断使能的环境下调用 (IRQ 已开启).
/// SGI 会立即触发 IRQ 异常, 在 ISR 中执行恢复逻辑.
///
/// # PANIC_FLAG
///
/// 本函数不设置 PANIC_FLAG. 调用者应通过设置 PANIC_FLAG 来指示恢复需求,
/// 或由 bytes_mut 接口触发 domain 故障.
#[inline(always)]
pub fn freg_trigger_recovery() {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        // SGI 7 定向发往**当前 CPU** (IRM=0, 不广播)。ICC_SGI1R_EL1 的字段布局为
        // INTID[27:24] / Aff1[23:16] / TargetList[15:0]: 目标的 Aff0 必须编码为
        // TargetList 的对应位 (`1 << aff0`); 历史缺陷曾误置入 [23:16] (Aff1), 会
        // 寻址到不存在的簇, SGI 永不投递 (与 `send_ipi` 同族, 已一并修正)。
        let cpu = crate::framework::cpu::arch::cpu_id();
        let sgi: u64 = (u64::from(super::gic::FREG_RECOVERY_SGI) << 24) // INTID = 7
                      | (1u64 << (cpu & 0xF)); // TargetList: 当前核 Aff0
        core::arch::asm!(
            "msr icc_sgi1r_el1, {sgi}",
            "isb",
            sgi = in(reg) sgi,
        );
    }
}

/// SGI FREG恢复处理函数 (由 irq_handler 调用)
///
/// 当 IRQ handler 检测到 `intid == FREG_RECOVERY_SGI` 时调用此函数.
/// 调用 `recovery_try_recover_from_idt()` 执行域级回滚.
///
/// # 返回值
///
/// - `0`: 恢复成功
/// - `-1`: 恢复失败 (域不可恢复)
/// - `-2`: 已尝试恢复 (防止递归)
///
/// # 注意
///
/// 此函数在 IRQ 上下文 (EL1h) 执行, 不需要额外压栈/出栈.
/// 如果恢复成功, IRQ handler 正常返回 (eret), 调用点继续执行.
pub fn freg_sgi_handler() -> i32 {
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    unsafe extern "C" {
        fn recovery_try_recover_from_idt() -> i32;
    }
    // SAFETY: `recovery_try_recover_from_idt` 是有效的 C ABI 函数指针; 参数列表与声明一致
    unsafe { recovery_try_recover_from_idt() }
}
