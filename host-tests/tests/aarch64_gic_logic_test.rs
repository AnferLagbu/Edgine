//! aarch64 GICv3 初始化判据 — host 侧确定性分支测试
//!
//! 追踪: ISSUE-RT-002
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! ISSUE-RT-002 的处置策略是"把静默挂起转为确定性报错": redistributor 唤醒超限
//! fail-fast + 初始化后置条件回读自检 + ARE 路由自校正。本轮对这些**失败/自校正
//! 分支**做确定性实证 —— 不做盲目压测复现, 而是以构造的寄存器值逐支路断言判据
//! 确实生效。
//!
//! ## 为何可 host 测试
//!
//! 判据已从 `privileged/arch/aarch64/gic.rs` 的 MMIO / 系统寄存器访问中剥离到架构
//! 中立的 `privileged/arch/gic_logic.rs` (纯函数, 无 asm)。生产路径仅"读寄存器 →
//! 交给同一判据", 故 host 侧所测判据与 aarch64 生产路径**同源**, 无平行实现。
//!
//! 与之互补的**源码结构**回归防护见 `aarch64_gic_contract_test.rs`。

use edgine::kernel::privileged::arch::gic_logic::{
    REDIST_WAKE_SPIN_LIMIT, uses_affinity_routing, verify_post_conditions, wake_timed_out,
};

// ── ARE 路由自校正分派 ──────────────────────────────────────────────

#[test]
fn are_mode_selects_affinity_routing() {
    // ARE_S(bit4) / ARE_NS(bit5) 任一置位 ⇒ 走 64 位 GICD_IROUTER
    assert!(uses_affinity_routing(1 << 4));
    assert!(uses_affinity_routing(1 << 5));
    assert!(uses_affinity_routing(0b11_0000));
}

#[test]
fn legacy_mode_selects_itargetsr() {
    // ARE 未置位 ⇒ 走 GICv2 兼容 GICD_ITARGETSR
    assert!(!uses_affinity_routing(0));
    // 当前内核 init_distributor 写 GICD_CTLR=0x3 (EnableGrp0+EnableGrp1, 未置 ARE),
    // 故运行期走 ITARGETSR 臂 —— 本断言固化了"当前确为 legacy 模式"这一事实.
    assert!(!uses_affinity_routing(0x3));
}

// ── redistributor 唤醒超限 fail-fast ────────────────────────────────

#[test]
fn wake_boundary_does_not_time_out_at_limit() {
    assert!(!wake_timed_out(0));
    assert!(!wake_timed_out(1));
    // 边界: 恰好用满上限仍判未超限
    assert!(!wake_timed_out(REDIST_WAKE_SPIN_LIMIT));
}

#[test]
fn wake_times_out_just_past_limit() {
    assert!(wake_timed_out(REDIST_WAKE_SPIN_LIMIT + 1));
    assert!(wake_timed_out(u32::MAX));
}

// ── 后置条件回读自检 (逐项 + 优先级) ────────────────────────────────

/// 全部满足的基准值: EnableGrp1 置位 / ChildrenAsleep 清零 /
/// Timer PPI(30) 使能 / ICC_IGRPEN1_EL1 使能.
const GOOD_CTLR: u32 = 0b10;
const GOOD_WAKER: u32 = 0;
const GOOD_ISENABLER0: u32 = 1 << 30;
const GOOD_IGRPEN1: u64 = 0x1;

#[test]
fn post_conditions_pass_when_all_good() {
    assert_eq!(
        verify_post_conditions(GOOD_CTLR, GOOD_WAKER, GOOD_ISENABLER0, GOOD_IGRPEN1),
        Ok(())
    );
}

#[test]
fn post_condition_fails_on_gicd_ctlr() {
    assert_eq!(
        verify_post_conditions(0, GOOD_WAKER, GOOD_ISENABLER0, GOOD_IGRPEN1),
        Err("GICD_CTLR.EnableGrp1 未置位")
    );
}

#[test]
fn post_condition_fails_on_gicr_waker() {
    assert_eq!(
        verify_post_conditions(GOOD_CTLR, 1 << 2, GOOD_ISENABLER0, GOOD_IGRPEN1),
        Err("GICR_WAKER.ChildrenAsleep 未清零")
    );
}

#[test]
fn post_condition_fails_on_isenabler0() {
    assert_eq!(
        verify_post_conditions(GOOD_CTLR, GOOD_WAKER, 0, GOOD_IGRPEN1),
        Err("GICR_ISENABLER0 未使能 Timer PPI")
    );
}

#[test]
fn post_condition_fails_on_igrpen1() {
    assert_eq!(
        verify_post_conditions(GOOD_CTLR, GOOD_WAKER, GOOD_ISENABLER0, 0),
        Err("ICC_IGRPEN1_EL1 未使能")
    );
}

#[test]
fn post_condition_reports_first_failure_in_check_order() {
    // 多项同时不满足 ⇒ 返回首个 (GICD_CTLR), 保证 fail-fast 诊断锚点稳定
    assert_eq!(
        verify_post_conditions(0, 1 << 2, 0, 0),
        Err("GICD_CTLR.EnableGrp1 未置位")
    );
}
