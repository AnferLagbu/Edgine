//! aarch64 SMP 启动 — 静态契约测试
//!
//! 追踪: DECISION-082 (SMP-08)
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! aarch64 次核启动链路跨三处定义: PSCI 调用 (`arch/aarch64/psci.rs`)、
//! 次核启动槽 (Rust `ApBootInfo` ↔ `boot/aarch64/start.S` 汇编偏移)、GIC
//! redistributor 几何常量 (`arch/aarch64/gic.rs`)。任一端单独修改而另一端
//! 未同步, 将导致 BSP 写入的启动参数被 AP 误读 (静默挂起 / 错误 MMU 配置)
//! 或 PSCI 调用被固件拒绝 (功能静默失效)。
//!
//! ## 为何用静态契约 (而非镜像常量)
//!
//! `framework/arch/aarch64/*` 为架构特有源码, host-tests (host 侧编译) 无法
//! 引用其符号, 亦无法在 x86_64 上编译 aarch64 内联汇编。故本测试以**源码文本
//! 分析**固化契约 —— 与同类测试 `aarch64_gic_contract_test.rs` (批次 A) 同手法。
//!
//! 不采用「镜像常量 + 运行期断言」策略: 在测试内复刻一份常量/结构体属内核
//! 逻辑的平行实现, 与源码漂移时镜像端不会失败 (自证式断言), 违背本项目
//! 「主机测试不得包含内核逻辑平行实现」硬约束。
//!
//! ## 覆盖契约
//!
//! - PSCI 函数 ID 编码 (SYSTEM_OFF/RESET/VERSION/CPU_ON);
//! - `PsciError` 负返回码 ↔ 变体一一映射;
//! - 次核启动槽 `ApBootInfo` 尺寸 (80 字节) 与字段顺序 ↔ 汇编字节偏移;
//! - 启动槽 / 入口 stub 的低半区符号与链接脚本高半区别名。

use std::fs;
use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // Edgine workspace root
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
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

const PSCI_RS: &str = "src/kernel/framework/arch/aarch64/psci.rs";
const SMP_INIT_RS: &str = "src/kernel/framework/arch/aarch64/smp_init.rs";
const GIC_RS: &str = "src/kernel/framework/arch/aarch64/gic.rs";
const START_S: &str = "src/kernel/framework/boot/aarch64/start.S";
const AARCH64_LD: &str = "src/kernel/framework/link/aarch64.ld";

/// 次核启动槽的 10 个字段 (Rust 声明顺序 = 汇编字节偏移递增顺序).
const AP_BOOT_FIELDS: [&str; 10] = [
    "ttbr0",
    "ttbr1",
    "mair",
    "tcr",
    "sctlr",
    "stack_top",
    "entry_va",
    "cpu_index",
    "done",
    "el",
];

/// 次核启动槽每个字段对应的汇编字节偏移 (见 `ap_entry_asm`).
const AP_BOOT_FIELD_OFFSETS: [&str; 10] = [
    "[x10, #0x00]",
    "[x10, #0x08]",
    "[x10, #0x10]",
    "[x10, #0x18]",
    "[x10, #0x20]",
    "[x10, #0x28]",
    "[x10, #0x30]",
    "[x10, #0x38]",
    "[x10, #0x40]",
    "[x10, #0x48]",
];

#[test]
fn psci_function_ids_frozen() {
    let src = read(PSCI_RS);
    assert!(
        src.contains("const PSCI_SYSTEM_OFF: u32 = 0x84000008;"),
        "PSCI_SYSTEM_OFF 必须为 0x84000008 (ARM PSCI v0.2+ 规范值)"
    );
    assert!(
        src.contains("const PSCI_SYSTEM_RESET: u32 = 0x84000009;"),
        "PSCI_SYSTEM_RESET 必须为 0x84000009 (ARM PSCI v0.2+ 规范值)"
    );
    assert!(
        src.contains("const PSCI_VERSION: u32 = 0x84000000;"),
        "PSCI_VERSION 必须为 0x84000000 (ARM PSCI v0.2+ 规范值)"
    );
    assert!(
        src.contains("pub const PSCI_CPU_ON: u32 = 0xC400_0003;"),
        "PSCI_CPU_ON 必须为 SMC64 编码 0xC400_0003 (64 位入参约定)"
    );
}

#[test]
fn psci_error_variants_present() {
    let src = read(PSCI_RS);
    assert!(
        src.contains("pub enum PsciError {"),
        "PsciError 必须为公开枚举 (供 BSP 侧错误分类)"
    );
    for variant in [
        "NotSupported",
        "InvalidParameters",
        "Denied",
        "AlreadyOn",
        "OnPending",
        "InternalFailure",
        "NotPresent",
        "Disabled",
    ] {
        assert!(
            src.contains(variant),
            "PsciError 缺少变体 {} (ARM PSCI 负返回码映射)",
            variant
        );
    }
    assert!(
        src.contains("Other(i64)"),
        "PsciError 必须保留 Other(i64) 兜底变体, 防止未知返回码被静默吞掉"
    );
}

#[test]
fn psci_error_code_mapping_frozen() {
    let src = read(PSCI_RS);
    // -1..=-8 与变体一一对应; 0 (SUCCESS) 映射为 None.
    let mapping = [
        ("-1", "NotSupported"),
        ("-2", "InvalidParameters"),
        ("-3", "Denied"),
        ("-4", "AlreadyOn"),
        ("-5", "OnPending"),
        ("-6", "InternalFailure"),
        ("-7", "NotPresent"),
        ("-8", "Disabled"),
    ];
    for (code, variant) in mapping {
        let arm = format!("{code} => Some(Self::{variant})");
        assert!(
            src.contains(&arm),
            "PsciError::from_i64 缺少映射分支 `{}` (PSCI 负返回码契约)",
            arm
        );
    }
    assert!(
        src.contains("0 => None"),
        "PsciError::from_i64 必须将 0 (SUCCESS) 映射为 None"
    );
    assert!(
        src.contains("other => Some(Self::Other(other))"),
        "PsciError::from_i64 必须保留未知返回码兜底分支"
    );
}

#[test]
fn ap_boot_slot_size_matches_asm() {
    let rust_src = read(SMP_INIT_RS);
    assert!(
        rust_src.contains("size_of::<ApBootInfo>() == 80"),
        "smp_init.rs 必须对 ApBootInfo 施加编译期 size_of == 80 断言"
    );

    let asm = read(START_S);
    let slot = slice_between(&asm, "_ap_boot_info:", "_boot_l0:");
    assert!(
        slot.contains(".space 80"),
        "start.S 的 _ap_boot_info 槽必须预留 80 字节 (10 × u64), 与 ApBootInfo 对齐"
    );
}

#[test]
fn ap_boot_slot_field_order_matches_asm() {
    // Rust 侧: 字段声明顺序必须与 AP_BOOT_FIELDS 一致 (偏移递增).
    let rust_src = read(SMP_INIT_RS);
    let body = slice_between(&rust_src, "struct ApBootInfo {", "\n}");
    let mut last = 0usize;
    for name in AP_BOOT_FIELDS {
        let decl = format!("{name}: u64,");
        let at = body
            .find(&decl)
            .unwrap_or_else(|| panic!("ApBootInfo 缺少字段声明 `{}`", decl));
        assert!(
            at >= last,
            "ApBootInfo 字段 {} 声明顺序错乱 (偏移递增被破坏)",
            name
        );
        last = at;
    }

    // 汇编侧: 每个字段的字节偏移必须与 Rust 声明顺序一一对应.
    let asm = read(START_S);
    let stub = slice_between(&asm, ".global ap_entry_asm", ".section .bootbss");
    for (name, offset) in AP_BOOT_FIELDS.iter().zip(AP_BOOT_FIELD_OFFSETS.iter()) {
        assert!(
            stub.contains(offset),
            "ap_entry_asm 缺少访问槽字段 {} 的偏移 `{}` (Rust↔汇编布局契约)",
            name,
            offset
        );
    }

    // 前 5 个字段须经对应系统寄存器装载 (证明偏移与语义绑定).
    for reg in [
        "msr     ttbr0_el1",
        "msr     ttbr1_el1",
        "msr     mair_el1",
        "msr     tcr_el1",
        "msr     sctlr_el1",
    ] {
        assert!(
            stub.contains(reg),
            "ap_entry_asm 缺少 `{}` (槽字段未绑定到对应系统寄存器)",
            reg
        );
    }
}

#[test]
fn ap_boot_symbols_and_aliases_present() {
    let asm = read(START_S);
    assert!(
        asm.contains(".global ap_entry_asm") && asm.contains("ap_entry_asm:"),
        "start.S 必须导出并定义低半区入口 stub `ap_entry_asm`"
    );
    assert!(
        asm.contains(".global _ap_boot_info") && asm.contains("_ap_boot_info:"),
        "start.S 必须导出并定义低半区启动槽 `_ap_boot_info`"
    );

    let ld = read(AARCH64_LD);
    assert!(
        ld.contains("ap_boot_info_ptr = _kernel_base + _ap_boot_info;"),
        "链接脚本必须暴露启动槽高半区别名 `ap_boot_info_ptr` (供 smp_init.rs 取址)"
    );
    assert!(
        ld.contains("ap_entry_asm_alias = _kernel_base + ap_entry_asm;"),
        "链接脚本必须暴露入口 stub 高半区别名 `ap_entry_asm_alias` (供 PSCI CPU_ON 取物理地址)"
    );
}

#[test]
fn gic_redistributor_geometry_frozen() {
    let src = read(GIC_RS);
    assert!(
        src.contains("const GICR_SGI_OFFSET: u64 = 0x1_0000;"),
        "GICR_SGI_OFFSET 必须为 0x1_0000 (SGI 帧相对 RD 帧偏移, GICv3 规范)"
    );
    assert!(
        src.contains("pub const GICR_STRIDE: u64 = 0x2_0000;"),
        "GICR_STRIDE 必须为 0x2_0000 (相邻 redistributor 帧间距, GICv3 规范)"
    );
}

/// 每核 GIC 初始化必须使能全部内核 SGI (防"分散使能"回归)。
///
/// 追踪: DECISION-083 (SGI 使能收敛)。SGI/PPI 使能位是 per-CPU Redistributor
/// 私有状态 (`GICR_ISENABLER0`); 历史缺陷: SGI 13/14 从未使能 ⇒ 跨核 TLB 失效
/// 接收侧永不响应, 延迟释放帧永久滞留。本契约固化不变式:
/// - SGI 编号集中在 gic.rs 定义 (编号单一 owner, 供 exception/freg 复用);
/// - `init_per_cpu` (BSP/AP 共用入口) 对每核使能 Timer PPI + 全部内核 SGI;
/// - 不再存在 BSP 专属的分散使能入口 `enable_freg_sgi`。
#[test]
fn per_cpu_gic_enables_all_kernel_sgis() {
    let src = read(GIC_RS);
    for decl in [
        "pub const TLB_SHOOTDOWN_SGI: u32 = 0xFD & 0xF;",
        "pub const RESCHEDULE_SGI: u32 = 0xFE & 0xF;",
        "pub const FREG_RECOVERY_SGI: u32 = 7;",
    ] {
        assert!(
            src.contains(decl),
            "gic.rs 必须集中定义内核 SGI 编号: {}",
            decl
        );
    }

    let init = slice_between(&src, "pub unsafe fn init_per_cpu", "\n}\n");
    for call in [
        "enable_timer_ppi(sgi)",
        "enable_sgi(sgi, TLB_SHOOTDOWN_SGI)",
        "enable_sgi(sgi, RESCHEDULE_SGI)",
        "enable_sgi(sgi, FREG_RECOVERY_SGI)",
    ] {
        assert!(
            init.contains(call),
            "init_per_cpu (BSP/AP 共用入口) 必须使能: {}",
            call
        );
    }

    // 分散的 BSP 专属使能入口必须已删除 (F9: 不得残留死代码).
    assert!(
        !src.contains("enable_freg_sgi"),
        "gic.rs 不得残留 enable_freg_sgi (SGI 使能已收敛到 init_per_cpu)"
    );
    let freg = read("src/kernel/framework/arch/aarch64/freg/mod.rs");
    assert!(
        !freg.contains("enable_freg_sgi"),
        "freg/mod.rs 不得残留 enable_freg_sgi (F9)"
    );
    assert!(
        !freg.contains("pub const FREG_RECOVERY_SGI"),
        "freg/mod.rs 不得重复定义 FREG_RECOVERY_SGI (编号 owner 为 gic.rs)"
    );
}
