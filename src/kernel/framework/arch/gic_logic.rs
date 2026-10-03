//! GICv3 初始化判据 — 架构中立纯逻辑
//!
//! 追踪: ISSUE-RT-002
//!
//! 本模块把 GICv3 初始化/配置中**纯判定逻辑** (寄存器实测值 → 判定结果) 从
//! [`super::aarch64::gic`] 的 MMIO / 系统寄存器访问中剥离出来, 以求在 host 侧
//! 可编译并逐支路单元测试 (`host-test` feature):
//!
//! - 后置条件回读判定 (GICD_CTLR / GICR_WAKER / GICR_ISENABLER0 / ICC_IGRPEN1_EL1);
//! - 亲和路由 (ARE) 模式选择 (决定 SPI 走 `GICD_IROUTER` 还是 `GICD_ITARGETSR`);
//! - redistributor 唤醒自旋超限判定。
//!
//! `aarch64/gic.rs` 仅负责**读寄存器**并把值交给本模块判定, 不再重复判定逻辑;
//! 故生产路径与 host 单元测试所验证的判据**同源**, 无平行实现。
//!
//! 编译门控: 仅 aarch64 目标, 或被 `host-test` 测试宿主引用时编译
//! (见 [`super::gic_logic` 的模块声明](super))。

/// `GICD_CTLR` 亲和路由使能掩码 (ARE_S bit4 / ARE_NS bit5)。
pub const GICD_CTLR_ARE_MASK: u32 = (1 << 4) | (1 << 5);

/// `GICD_CTLR.EnableGrp1NS` (bit1) 掩码 — 后置条件校验用。
pub const GICD_CTLR_ENABLE_GRP1_MASK: u32 = 0b10;

/// `GICR_WAKER.ChildrenAsleep` (bit2) 掩码。
pub const GICR_WAKER_CHILDREN_ASLEEP_MASK: u32 = 1 << 2;

/// ARM 架构定时器 PPI (Non-secure Physical Timer, CNTPNSIRQ) 编号。
///
/// 单一定义点: `aarch64/gic.rs` 的 per-CPU 使能与优先级计算、以及本模块的后置
/// 条件判定共用本常量, 避免同一硬件编号在多处各写一份而失同步。
pub const TIMER_PPI: u32 = 30;

/// Redistributor 唤醒自旋上限。
///
/// 唤醒应在数十次读内完成; 超限即判定未唤醒, 由调用方显式报错而非静默继续。
pub const REDIST_WAKE_SPIN_LIMIT: u32 = 1_000_000;

/// 判定 `GICD_CTLR` 是否处于亲和路由模式 (ARE=1)。
///
/// `true`  ⇒ SPI 路由走 64 位 `GICD_IROUTER` (GICv3 原生);
/// `false` ⇒ 走 GICv2 兼容的 8 位 `GICD_ITARGETSR`。
#[must_use]
pub fn uses_affinity_routing(gicd_ctlr: u32) -> bool {
    gicd_ctlr & GICD_CTLR_ARE_MASK != 0
}

/// 判定 redistributor 唤醒等待是否已超限。
///
/// 边界: `wait_count == REDIST_WAKE_SPIN_LIMIT` 仍判**未超限** (允许恰好用满上限),
/// 仅在严格大于上限时判超时。
#[must_use]
pub fn wake_timed_out(wait_count: u32) -> bool {
    wait_count > REDIST_WAKE_SPIN_LIMIT
}

/// GICv3 初始化后置条件回读判定。
///
/// 入参依次为 `GICD_CTLR` / `GICR_WAKER` / `GICR_ISENABLER0` / `ICC_IGRPEN1_EL1`
/// 的实测值。返回 `Err(原因)` 表示**首个**不满足的后置条件, `Ok(())` 表示全部通过。
///
/// 判定顺序与错误文案与 `aarch64/gic.rs` 原实现逐字一致, 以保证 boot 入口
/// fail-fast 打印的诊断锚点不变。
///
/// # Errors
///
/// 返回 `Err(原因)` 表示回读发现某项后置条件未满足, `原因` 为固定的中文诊断串
/// (如 `"GICD_CTLR.EnableGrp1 未置位"`), 由 boot 入口打印并 halt。
pub fn verify_post_conditions(
    gicd_ctlr: u32,
    gicr_waker: u32,
    gicr_isenabler0: u32,
    icc_igrpen1: u64,
) -> Result<(), &'static str> {
    if gicd_ctlr & GICD_CTLR_ENABLE_GRP1_MASK == 0 {
        return Err("GICD_CTLR.EnableGrp1 未置位");
    }
    if gicr_waker & GICR_WAKER_CHILDREN_ASLEEP_MASK != 0 {
        return Err("GICR_WAKER.ChildrenAsleep 未清零");
    }
    if gicr_isenabler0 & (1u32 << (TIMER_PPI % 32)) == 0 {
        return Err("GICR_ISENABLER0 未使能 Timer PPI");
    }
    if icc_igrpen1 & 0x1 == 0 {
        return Err("ICC_IGRPEN1_EL1 未使能");
    }
    Ok(())
}
