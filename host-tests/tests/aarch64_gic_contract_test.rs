//! aarch64 GICv3 初始化 — 静态契约测试
//!
//! 追踪: ISSUE-RT-002
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! ISSUE-RT-002: aarch64 GICv3 初始化或中断处理偶发挂起. 原始现象在
//! QEMU TCG 下不可稳定复现, 故本轮为挂起补充**复验装备与回归防护**:
//! 把"静默挂起"转化为"确定性报错" (redistributor 唤醒超时显式失败 +
//! 初始化后置条件回读自检 + boot 入口 fail-fast + `GICv3 ready` 里程碑).
//!
//! ## 为何用静态契约
//!
//! `framework/arch/aarch64/*` 为架构特有源码, host-tests (host 侧编译)
//! 无法引用其符号; 故采用**源码文本分析**方式固化关键契约, 防止回归:
//!
//! - redistributor 唤醒超时必须**显式失败** (不得静默 `break` 继续);
//! - `init()` 必须返回 `Result` 并在末尾执行后置条件自检;
//! - 自检须覆盖 GICD_CTLR / GICR_WAKER / GICR_ISENABLER0 / ICC_IGRPEN1_EL1;
//! - 不得再写 `GICR_CTLR` (其 bit0 实为 EnableLPIs, 非 redistributor 使能位);
//! - boot 入口须 fail-fast 并打印 `GICv3 ready` 里程碑;
//! - QEMU 启动脚本须断言该里程碑;
//! - EL1h 与 EL0 两条 IRQ 路径须共用同一 SGI 分发 (防平行实现再次分叉).

use std::fs;
use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // QueenX workspace root
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

/// 截取 `src` 中 `begin` 起点到其后首个 `end` 之间的片段.
fn slice_between<'a>(src: &'a str, begin: &str, end: &str) -> &'a str {
    let start = src
        .find(begin)
        .unwrap_or_else(|| panic!("未找到起点标记: {}", begin));
    let rest = &src[start..];
    let end_off = rest
        .find(end)
        .unwrap_or_else(|| panic!("未找到终点标记: {}", end));
    &rest[..end_off]
}

const GIC_RS: &str = "src/kernel/framework/arch/aarch64/gic.rs";
const ENTRY_RS: &str = "src/kernel/framework/boot/aarch64/entry.rs";
const EXCEPTION_RS: &str = "src/kernel/framework/arch/aarch64/exception.rs";
const QEMU_SH: &str = "scripts/qemu_boot_test.sh";

#[test]
fn test_redist_wake_spin_limit_defined() {
    let src = read(GIC_RS);
    assert!(
        src.contains("const REDIST_WAKE_SPIN_LIMIT: u32 = 1_000_000;"),
        "必须定义 redistributor 唤醒自旋上限常量 REDIST_WAKE_SPIN_LIMIT"
    );
}

#[test]
fn test_redist_wake_timeout_fails_explicitly() {
    let src = read(GIC_RS);
    let body = slice_between(
        &src,
        "pub unsafe fn init_redistributor(",
        "/// 使能 CPU Interface",
    );
    assert!(
        body.contains("REDIST_WAKE_SPIN_LIMIT"),
        "redistributor 唤醒自旋须以 REDIST_WAKE_SPIN_LIMIT 为上限"
    );
    assert!(
        body.contains("return Err("),
        "唤醒超时必须显式返回 Err (不得静默继续)"
    );
    assert!(
        !body.contains("break;"),
        "唤醒等待不得使用静默 break 退出 (ISSUE-RT-002 回归点)"
    );
}

#[test]
fn test_init_returns_result_and_verifies_post_conditions() {
    let src = read(GIC_RS);
    let body = slice_between(&src, "pub unsafe fn init()", "/// GIC 初始化后置条件校验.");
    assert!(
        src.contains("pub unsafe fn init() -> Result<(), &'static str>"),
        "init() 必须返回 Result 以支持 boot 入口 fail-fast"
    );
    assert!(
        body.contains("init_per_cpu(0)?"),
        "init() 须用 ? 向上传递 per-CPU (redistributor) 初始化失败"
    );
    assert!(
        body.contains("verify_post_conditions()"),
        "init() 末尾须调用 verify_post_conditions() 做后置条件自检"
    );
}

#[test]
fn test_post_conditions_cover_key_registers() {
    let src = read(GIC_RS);
    let body = slice_between(
        &src,
        "unsafe fn verify_post_conditions()",
        "// 中断管理 API",
    );
    // GICD_CTLR.EnableGrp1NS (bit1)
    assert!(
        body.contains("GICD_CTLR") && body.contains("0b10"),
        "自检须校验 GICD_CTLR.EnableGrp1 (bit1)"
    );
    // GICR_WAKER.ChildrenAsleep (bit2)
    assert!(
        body.contains("GICR_WAKER") && body.contains("(1 << 2)"),
        "自检须校验 GICR_WAKER.ChildrenAsleep (bit2) 已清零"
    );
    // GICR_ISENABLER0 Timer PPI
    assert!(
        body.contains("GICR_ISENABLER0"),
        "自检须校验 GICR_ISENABLER0 已使能 Timer PPI"
    );
    // ICC_IGRPEN1_EL1
    assert!(
        body.contains("icc_igrpen1_el1"),
        "自检须校验 ICC_IGRPEN1_EL1 已使能"
    );
}

#[test]
fn test_no_write_to_gicr_ctlr() {
    let src = read(GIC_RS);
    assert!(
        !src.contains("gicr_write(GICR_CTLR"),
        "不得写 GICR_CTLR (bit0 实为 EnableLPIs, 非 redistributor 使能位)"
    );
    assert!(
        src.contains("EnableLPIs"),
        "须保留说明 GICR_CTLR bit0 语义 (EnableLPIs) 的注释, 防止误加回写"
    );
}

#[test]
fn test_boot_entry_fail_fast_and_marker() {
    let src = read(ENTRY_RS);
    assert!(
        src.contains("crate::framework::arch::gic::init()"),
        "boot 入口须调用 gic::init()"
    );
    assert!(
        src.contains("if let Err(reason) = crate::framework::arch::gic::init()"),
        "boot 入口须对 gic::init() 失败 fail-fast"
    );
    assert!(
        src.contains("FATAL: GICv3 init failed"),
        "boot 入口须打印明确的 GIC 初始化失败信息"
    );
    assert!(
        src.contains("GICv3 ready"),
        "boot 入口须在 GIC 初始化成功后打印 `GICv3 ready` 里程碑"
    );
}

#[test]
fn test_qemu_script_asserts_gic_marker() {
    let src = read(QEMU_SH);
    assert!(
        src.contains("GICv3 ready"),
        "QEMU 启动脚本须断言 `GICv3 ready` 里程碑 (ISSUE-RT-002 回归防护)"
    );
}

/// 内核 SGI 接收路由必须在 EL1h 与 EL0 两条 IRQ 路径上**等价**。
///
/// 追踪: ISSUE-RT-002 (中断处理域)。历史缺陷: EL0 IRQ 路径自带一份裁剪过的分发
/// (仅处理 Timer PPI 与设备 SPI), 丢弃内核 SGI 7/13/14 —— 目标核在 EL0 时跨核
/// TLB 失效 (SGI 13) 被静默 ACK/EOI, 该核 `CPU_TLB_GEN` 永不推进, 以
/// `smp::tlb_gen_min_online` 为判据的延迟释放页帧永久滞留。本契约固化:
/// - 两条 IRQ 入口均委托同一 `handle_irq` (单一实现, 不得再有平行分派);
/// - 共用分发须覆盖全部内核 SGI (barrier 7 / TLB 13 / resched 14)。
#[test]
fn test_el0_and_el1_irq_paths_share_sgi_dispatch() {
    let src = read(EXCEPTION_RS);

    // 两条入口必须委托到同一分发函数。
    let el1 = slice_between(&src, "pub extern \"C\" fn irq_handler(", "\n}\n");
    let el0 = slice_between(&src, "pub extern \"C\" fn irq_handler_el0(", "\n}\n");
    assert!(
        el1.contains("handle_irq(false)"),
        "irq_handler (EL1h) 必须委托 handle_irq (单一分发)"
    );
    assert!(
        el0.contains("handle_irq(true)"),
        "irq_handler_el0 (EL0) 必须委托 handle_irq (单一分发)"
    );
    // EL0 入口不得自带任何独立分派逻辑 (否则平行实现分叉复发)。
    assert!(
        !el0.contains("if intid ==") && !el0.contains("acknowledge()"),
        "irq_handler_el0 不得自带独立分发 (须复用 handle_irq)"
    );

    // 共用分发必须覆盖全部内核 SGI 的路由条件与接收处理。
    let shared = slice_between(&src, "fn handle_irq(", "\n}\n");
    for (cond, call) in [
        ("super::gic::BARRIER_RECOVERY_SGI", "barrier_sgi_handler"),
        ("super::gic::TLB_SHOOTDOWN_SGI", "tlb_catch_up_local"),
        ("super::gic::RESCHEDULE_SGI", "resched_ipi_handler"),
    ] {
        assert!(
            shared.contains(cond),
            "handle_irq 必须路由内核 SGI 条件: {}",
            cond
        );
        assert!(
            shared.contains(call),
            "handle_irq 必须调用接收处理: {}",
            call
        );
    }
}
