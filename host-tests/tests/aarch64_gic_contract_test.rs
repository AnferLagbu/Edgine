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
//! - QEMU 启动脚本须断言该里程碑.

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
