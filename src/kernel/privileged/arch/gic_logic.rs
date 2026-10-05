//! GICv3 初始化判据 — 架构中立纯逻辑
//!
//! 追踪: ISSUE-RT-002
//!
//! 本模块把 GICv3 初始化/配置中**纯判定逻辑** (寄存器实测值 → 判定结果) 从
//! [`super::aarch64::gic`] 的 MMIO / 系统寄存器访问中剥离出来, 以求在 host 侧
//! 可编译并逐支路单元测试 (`host-test` feature):
//!
//! - 后置条件回读判定 (GICD_CTLR / GICR_WAKER / GICR_ISENABLER0 / ICC_IGRPEN1_EL1);
//! - 亲和路由 (ARE) 后置条件判定 (内核 SGI 经 `ICC_SGI1R_EL1`、SPI 经 `GICD_IROUTER`
//!   分发, 均以 ARE=1 为前提, 故须 fail-fast 校验而非运行期分支);
//! - redistributor 唤醒自旋超限判定;
//! - `GICD_CTLR.RWP` 写入生效等待超限判定;
//! - redistributor 帧亲和扫描 (`GICR_TYPER` 的亲和值 / `Last` 判定)。
//!
//! `aarch64/gic.rs` 仅负责**读寄存器**并把值交给本模块判定, 不再重复判定逻辑;
//! 故生产路径与 host 单元测试所验证的判据**同源**, 无平行实现。
//!
//! 编译门控: 仅 aarch64 目标, 或被 `host-test` 测试宿主引用时编译
//! (见 [`super::gic_logic` 的模块声明](super))。

/// `GICD_CTLR` 亲和路由使能掩码 (ARE_S bit4 / ARE_NS bit5)。
pub const GICD_CTLR_ARE_MASK: u32 = (1 << 4) | (1 << 5);

/// `GICD_CTLR.EnableGrp0` (bit0) 掩码 — Distributor 初始化用。
pub const GICD_CTLR_ENABLE_GRP0_MASK: u32 = 0b01;

/// `GICD_CTLR.EnableGrp1NS` (bit1) 掩码 — 后置条件校验用。
pub const GICD_CTLR_ENABLE_GRP1_MASK: u32 = 0b10;

/// `GICD_CTLR.RWP` (Register Write Pending, bit31) 掩码。
///
/// `GICD_CTLR` 写后该位置位, 表示写入尚未对后续访问生效; 须自旋等待其归零。
/// 尤其在 ARE 与 Group 使能位**分两步写**时 (ARE 仅在所有 Group 使能位为 0 时可
/// 写, ARM IHI 0069), 两组写入之间必须等 RWP 清零。
pub const GICD_CTLR_RWP_MASK: u32 = 1 << 31;

/// `GICR_WAKER.ChildrenAsleep` (bit2) 掩码。
pub const GICR_WAKER_CHILDREN_ASLEEP_MASK: u32 = 1 << 2;

/// `GICR_TYPER.Affinity_Value` 位域起点 (bits[63:32])。
///
/// 该 32 位字段为该 redistributor 帧所属核的**紧凑亲和值** (`Aff3:Aff2:Aff1:Aff0`,
/// 每级 8 bit), 布局与 `aarch64/mod.rs` 的 `affinity_pack` 一致; 故可作为
/// "当前核硬件 id → 本核 redistributor 帧" 的匹配键。
pub const GICR_TYPER_AFFINITY_SHIFT: u32 = 32;

/// `GICR_TYPER.Last` (bit4) 掩码 — 置位表示该帧为 GICR 区域内最后一个 redistributor 帧。
pub const GICR_TYPER_LAST_MASK: u64 = 1 << 4;

/// redistributor 亲和扫描的帧数防御上限。
///
/// 正常情形由 `GICR_TYPER.Last` 提前终止; 本上限仅防御 `Last` 位异常缺失导致的越界
/// 扫描。取值覆盖架构最大 CPU 数, 且 `上限 × GICR_STRIDE` 仍落在设备映射窗口内。
pub const REDIST_SCAN_MAX_FRAMES: u32 = 1024;

/// ARM 架构定时器 PPI (Non-secure Physical Timer, CNTPNSIRQ) 编号。
///
/// 单一定义点: `aarch64/gic.rs` 的 per-CPU 使能与优先级计算、以及本模块的后置
/// 条件判定共用本常量, 避免同一硬件编号在多处各写一份而失同步。
pub const TIMER_PPI: u32 = 30;

/// Redistributor 唤醒自旋上限。
///
/// 唤醒应在数十次读内完成; 超限即判定未唤醒, 由调用方显式报错而非静默继续。
pub const REDIST_WAKE_SPIN_LIMIT: u32 = 1_000_000;

/// `GICD_CTLR.RWP` 清零自旋上限 (语义与边界同 [`REDIST_WAKE_SPIN_LIMIT`])。
pub const CTLR_RWP_SPIN_LIMIT: u32 = 1_000_000;

/// 判定 `GICD_CTLR` 是否处于亲和路由模式 (ARE=1)。
///
/// `true`  ⇒ SPI 路由走 64 位 `GICD_IROUTER`、SGI 经 `ICC_SGI1R_EL1` 投递 (GICv3 原生);
/// `false` ⇒ 处于 GICv2 兼容的 8 位 `GICD_ITARGETSR` 模式。
///
/// 内核跨核 IPI 与 SPI 分发**均以 ARE=1 为前提**, 故本函数在
/// [`verify_post_conditions`] 中作为 fail-fast 判据使用, 而非运行期分支条件
/// (历史缺陷: 依赖回读自校正分派, 掩盖了 ARE 未被显式置位的模型错误)。
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

/// 判定 `GICD_CTLR.RWP` 清零等待是否已超限。
///
/// 边界: `wait_count == CTLR_RWP_SPIN_LIMIT` 仍判**未超限**, 仅严格大于时判超时。
#[must_use]
pub fn ctlr_rwp_timed_out(wait_count: u32) -> bool {
    wait_count > CTLR_RWP_SPIN_LIMIT
}

/// 从 `GICR_TYPER` 提取 redistributor 的紧凑亲和值 (`Aff3:Aff2:Aff1:Aff0`)。
#[must_use]
pub fn redist_affinity(typer: u64) -> u32 {
    ((typer >> GICR_TYPER_AFFINITY_SHIFT) & 0xFFFF_FFFF) as u32
}

/// 判定 `GICR_TYPER.Last`: 该帧是否为 GICR 区域内最后一个 redistributor 帧。
#[must_use]
pub fn redist_is_last(typer: u64) -> bool {
    typer & GICR_TYPER_LAST_MASK != 0
}

/// 线性扫描 GICR 区域, 返回亲和值等于 `target_affinity` 的 redistributor 帧索引。
///
/// `typer_at(frame_index)` 由调用方读回该帧的 `GICR_TYPER` (MMIO 访问留在 `aarch64/gic.rs`),
/// 本函数仅做纯判定, 故 host 侧可与 aarch64 生产路径**同源**逐支路实证:
/// 命中即返回 `Some(帧索引)`; 遇 `Last` 位或达 `max_frames` 仍未命中 ⇒ 返回 `None`
/// (fail-closed, 由调用方报错, 而非按逻辑序号线性推算到他人帧 —— ISSUE-RT-002 关联缺陷)。
#[must_use]
pub fn find_redist_frame(
    target_affinity: u32,
    max_frames: u32,
    mut typer_at: impl FnMut(u32) -> u64,
) -> Option<u32> {
    let mut frame = 0u32;
    while frame < max_frames {
        let typer = typer_at(frame);
        if redist_affinity(typer) == target_affinity {
            return Some(frame);
        }
        if redist_is_last(typer) {
            return None;
        }
        frame += 1;
    }
    None
}

/// GICv3 初始化后置条件回读判定。
///
/// 入参依次为 `GICD_CTLR` / `GICR_WAKER` / `GICR_ISENABLER0` / `ICC_IGRPEN1_EL1`
/// 的实测值。返回 `Err(原因)` 表示**首个**不满足的后置条件, `Ok(())` 表示全部通过。
///
/// 判定顺序: `GICD_CTLR.EnableGrp1` → `GICD_CTLR.ARE` → `GICR_WAKER`
/// → `GICR_ISENABLER0` → `ICC_IGRPEN1_EL1`。错误文案与 `aarch64/gic.rs` 原实现
/// 逐字一致, 以保证 boot 入口 fail-fast 打印的诊断锚点不变。
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
    // ARE 未置位 ⇒ 内核 SGI (ICC_SGI1R_EL1) 与 SPI (GICD_IROUTER) 分发均失效,
    // 表现为静默挂起; 故在此 fail-fast (ISSUE-RT-002 根因判据)。
    if !uses_affinity_routing(gicd_ctlr) {
        return Err("GICD_CTLR.ARE 未置位 (SGI/SPI 依赖亲和路由)");
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
