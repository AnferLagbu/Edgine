//! aarch64 GICv3 初始化判据 — host 侧确定性分支测试
//!
//! 追踪: ISSUE-RT-002
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! ISSUE-RT-002 的处置策略是"把静默挂起转为确定性报错": redistributor 唤醒超限
//! fail-fast + `GICD_CTLR.RWP` 写入生效等待 fail-fast + 初始化后置条件回读自检
//! (含 ARE 必须置位)。本轮对这些**失败分支**做确定性实证 —— 不做盲目压测复现,
//! 而是以构造的寄存器值逐支路断言判据确实生效。
//!
//! ## 为何可 host 测试
//!
//! 判据已从 `privileged/arch/aarch64/gic.rs` 的 MMIO / 系统寄存器访问中剥离到架构
//! 中立的 `privileged/arch/gic_logic.rs` (纯函数, 无 asm)。生产路径仅"读寄存器 →
//! 交给同一判据", 故 host 侧所测判据与 aarch64 生产路径**同源**, 无平行实现。
//!
//! 与之互补的**源码结构**回归防护见 `aarch64_gic_contract_test.rs`。

use edgine::kernel::privileged::arch::gic_logic::{
    CTLR_RWP_SPIN_LIMIT, GICD_CTLR_ARE_MASK, GICD_CTLR_ENABLE_GRP1_MASK, GICR_TYPER_AFFINITY_SHIFT,
    GICR_TYPER_LAST_MASK, REDIST_WAKE_SPIN_LIMIT, ctlr_rwp_timed_out, find_redist_frame,
    redist_affinity, redist_is_last, uses_affinity_routing, verify_post_conditions, wake_timed_out,
};

// ── ARE 亲和路由判据 ────────────────────────────────────────────────

#[test]
fn are_mode_selects_affinity_routing() {
    // ARE_S(bit4) / ARE_NS(bit5) 任一置位 ⇒ 亲和路由使能
    assert!(uses_affinity_routing(1 << 4));
    assert!(uses_affinity_routing(1 << 5));
    assert!(uses_affinity_routing(0b11_0000));
}

#[test]
fn are_clear_is_rejected_as_fail_closed() {
    // ARE 未置位 ⇒ uses_affinity_routing 返回 false; 内核据此在后置条件中 fail-fast,
    // 不再运行期自校正走 ITARGETSR 分支 (自校正会掩盖模型错误, ISSUE-RT-002 根因)。
    assert!(!uses_affinity_routing(0));
    // 历史缺陷值: 仅置 EnableGrp0|EnableGrp1 而未置 ARE 的 0x3 —— 现由后置条件拒绝。
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

// ── GICD_CTLR.RWP 写入生效等待超限 fail-fast ─────────────────────────

#[test]
fn ctlr_rwp_boundary_does_not_time_out_at_limit() {
    assert!(!ctlr_rwp_timed_out(0));
    assert!(!ctlr_rwp_timed_out(1));
    assert!(!ctlr_rwp_timed_out(CTLR_RWP_SPIN_LIMIT));
}

#[test]
fn ctlr_rwp_times_out_just_past_limit() {
    assert!(ctlr_rwp_timed_out(CTLR_RWP_SPIN_LIMIT + 1));
    assert!(ctlr_rwp_timed_out(u32::MAX));
}

// ── 后置条件回读自检 (逐项 + 优先级) ────────────────────────────────

/// 全部满足的基准值: EnableGrp1 置位 / ARE 置位 / ChildrenAsleep 清零 /
/// Timer PPI(30) 使能 / ICC_IGRPEN1_EL1 使能.
const GOOD_CTLR: u32 = GICD_CTLR_ENABLE_GRP1_MASK | GICD_CTLR_ARE_MASK;
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
fn post_condition_fails_when_are_clear() {
    // EnableGrp1 置位但 ARE 未置位 (即历史 0x3 值) ⇒ 返回 ARE 诊断 (根因判据)
    assert_eq!(
        verify_post_conditions(
            GICD_CTLR_ENABLE_GRP1_MASK,
            GOOD_WAKER,
            GOOD_ISENABLER0,
            GOOD_IGRPEN1
        ),
        Err("GICD_CTLR.ARE 未置位 (SGI/SPI 依赖亲和路由)")
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

// ── GICR_TYPER 亲和值 / Last 判据 (redistributor 帧定位) ─────────────

/// 构造 `GICR_TYPER`: 紧凑亲和值放 bits[63:32], 可选置 `Last`(bit4)。
fn typer_of(affinity: u32, last: bool) -> u64 {
    (u64::from(affinity) << GICR_TYPER_AFFINITY_SHIFT) | u64::from(last) * GICR_TYPER_LAST_MASK
}

#[test]
fn redist_affinity_extracts_high_word() {
    // 紧凑亲和 (Aff3:Aff2:Aff1:Aff0) 位于 bits[63:32]; 低 32 位 (ProcessID 等) 不参与匹配
    assert_eq!(redist_affinity(typer_of(0x0000_0001, false)), 1);
    assert_eq!(redist_affinity(typer_of(0x00A0_0B01, false)), 0x00A0_0B01);
    // 低 32 位噪声不影响亲和值提取
    assert_eq!(redist_affinity(0xDEAD_BEEF_0000_0005), 0xDEAD_BEEF);
}

#[test]
fn redist_is_last_reads_bit4() {
    assert!(redist_is_last(typer_of(1, true)));
    assert!(!redist_is_last(typer_of(1, false)));
    // Last 与亲和值字段互不影响
    assert!(redist_is_last(GICR_TYPER_LAST_MASK));
    assert!(!redist_is_last(typer_of(0xFFFF_FFFF, false)));
}

#[test]
fn find_redist_frame_hits_matching_affinity() {
    // 帧 0..2 亲和依次 0/1/2, 末帧 (idx=2) 置 Last
    let frames = [(0u32, false), (1, false), (2, true)];
    let lookup = |idx: u32| {
        let (aff, last) = frames[idx as usize];
        typer_of(aff, last)
    };
    assert_eq!(find_redist_frame(0x0000_0001, 1024, lookup), Some(1));
    assert_eq!(find_redist_frame(0x0000_0002, 1024, lookup), Some(2));
}

#[test]
fn find_redist_frame_stops_at_last() {
    // 目标亲和不在区域中, 遇 Last 提前终止 ⇒ None (fail-closed, 不越界扫描)
    let mut probes = 0u32;
    let result = find_redist_frame(0xDEAD_BEEF, 1024, |idx| {
        probes += 1;
        typer_of(idx, idx == 3)
    });
    assert_eq!(result, None);
    // 第 4 帧 (idx=3) 检出 Last 即停, 共探测 4 次 (idx 0..=3)
    assert_eq!(probes, 4);
}

#[test]
fn find_redist_frame_fails_closed_without_last() {
    // Last 位异常缺失 ⇒ 达上限后返回 None, 不无限扫描
    let mut probes = 0u32;
    let result = find_redist_frame(0xDEAD_BEEF, 8, |_| {
        probes += 1;
        typer_of(0, false)
    });
    assert_eq!(result, None);
    assert_eq!(probes, 8);
}

#[test]
fn find_redist_frame_distinguishes_high_affinity_levels() {
    // 稀疏/多簇: 仅低位相同但更高亲和级不同的核不得误命中 (ISSUE-RT-002 关联缺陷)
    let frames = [0x0000_0100u32, 0x0000_0200, 0x0001_0100]; // 0.0.1.0 / 0.0.2.0 / 0.1.1.0
    let lookup = |idx: u32| {
        let aff = frames[idx as usize];
        typer_of(aff, idx == 2)
    };
    assert_eq!(find_redist_frame(0x0001_0100, 1024, lookup), Some(2));
    assert_eq!(find_redist_frame(0x0000_0300, 1024, lookup), None);
}
