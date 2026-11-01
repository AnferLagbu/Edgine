// SPDX-License-Identifier: MPL-2.0
//! P-77 回归: ACPI 启动路径必须单次遍历 RSDT/XSDT, 解析 MADT + FADT + HPET + DMAR.
//!
//! 缺陷形态 (P-19(c) 调研发现): `parse_all_tables` / `acpi_parse_all` 全仓零调用,
//! 启动路径只调 `parse_madt` ⇒ `has_fadt()` 恒 false、`get_hpet_info()` 恒 `None`、
//! `DMAR_DRHD_LIST` 恒空, 电源管理 / HPET / IOMMU 发现面全是空值.
//!
//! 修复形态: `smp_init::init` 改调 `parse_all_tables(0)` (单次遍历, 内含 MADT),
//! DMAR 表发现状态回填 `has_dmar()`, functions 侧去掉恒 false 占位;
//! 接线同时暴露并修复 HPET 表布局错位 (GAS 偏移 40 被读成 42), FADT 按
//! ACPI 1.0 前缀 (116 字节) 接纳、复位字段单独按 ACPI 2.0+ 长度闸门;
//! QEMU 启动日志断言 FADT 解析与 HPET 基址里程碑. 本文件锁定接线形态本身.

use std::fs;
use std::path::PathBuf;

const ACPI: &str = "src/kernel/privileged/arch/x86_64/acpi.rs";
const SMP_INIT: &str = "src/kernel/privileged/arch/x86_64/smp_init.rs";
const FUN_ACPI: &str = "src/kernel/functions/driver/acpi.rs";
const QEMU: &str = "scripts/qemu_boot_test.sh";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免注释里的历史名词被误判为代码
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 空白不敏感 + 尾随逗号不敏感 (rustfmt 会折行长参数列表)
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .replace(",)", ")")
}

fn has(hay: &str, needle: &str) -> bool {
    norm(hay).contains(&norm(needle))
}

fn occurrences(hay: &str, needle: &str) -> usize {
    let hay = norm(hay);
    let needle = norm(needle);
    assert!(!needle.is_empty(), "探针不得为空串");
    let mut count = 0usize;
    let mut pos = 0usize;
    while let Some(i) = hay[pos..].find(&needle) {
        count += 1;
        pos += i + needle.len();
    }
    count
}

/// `first` 的末次出现早于 `second` 的首次出现.
fn before(hay: &str, first: &str, second: &str) -> bool {
    let hay = norm(hay);
    let (Some(f), Some(s)) = (hay.rfind(&norm(first)), hay.find(&norm(second))) else {
        return false;
    };
    f < s
}

/// 取 `start` 首次出现到其后 `end` 首次出现之间的源码片段.
fn section<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
    let start_pos = src
        .find(start)
        .unwrap_or_else(|| panic!("未找到起点: {start}"));
    let end_rel = src[start_pos..]
        .find(end)
        .unwrap_or_else(|| panic!("未找到终点: {end}"));
    &src[start_pos..start_pos + end_rel]
}

#[test]
fn boot_path_parses_all_acpi_tables_once() {
    let smp = code_only(&read_src(SMP_INIT));
    assert_eq!(
        occurrences(&smp, "super::acpi::parse_all_tables(0)"),
        1,
        "启动路径必须恰调一次 parse_all_tables (单次 SDT 遍历)"
    );
    assert!(
        !has(&smp, "super::acpi::parse_madt(0)"),
        "启动路径不得再直调 parse_madt: 与 parse_all_tables 叠加会重复登记 AP/IOAPIC 列表"
    );
    assert!(
        before(
            &smp,
            "super::acpi::parse_all_tables(0)",
            "super::ioapic::init_from_acpi()"
        ),
        "IOAPIC 初始化必须晚于 ACPI 表解析 (依赖 parse_all_tables 内的 MADT 枚举结果)"
    );
}

#[test]
fn full_table_parser_dispatches_four_signatures() {
    let acpi = code_only(&read_src(ACPI));
    let all = section(
        &acpi,
        "pub fn parse_all_tables",
        "pub extern \"C\" fn acpi_parse_all",
    );
    for (signature, parser) in [
        ("*b\"APIC\"", "parse_madt_entries(table_ptr)"),
        ("*b\"FACP\"", "parse_fadt(table_ptr, table_len)"),
        ("*b\"HPET\"", "parse_hpet(table_ptr)"),
        ("*b\"DMAR\"", "parse_dmar(table_ptr)"),
    ] {
        assert!(has(all, signature), "parse_all_tables 必须识别 {signature}");
        assert!(
            has(all, parser),
            "parse_all_tables 识别 {signature} 后必须调用 {parser}"
        );
    }
    assert!(
        has(all, "MADT_BASE.store(table_ptr, Ordering::Release)"),
        "全表遍历发现 MADT 时也必须填充 get_lapic_base() 依赖的基址"
    );
    assert!(
        has(all, "MADT_FOUND.store(true, Ordering::Release)"),
        "全表遍历发现 MADT 时必须置 found 标志"
    );
    for (signature, min_size) in [
        ("FACP", "FADT_MIN_LEN"),
        ("HPET", "core::mem::size_of::<HpetTable>()"),
        ("DMAR", "core::mem::size_of::<DmarTable>()"),
    ] {
        assert!(
            has(all, &format!("table_len < {min_size}")),
            "{signature} 解析前必须用表头 length 做最小长度闸门, 否则短表越界读"
        );
    }
    assert!(
        has(
            &acpi,
            "const FADT_MIN_LEN: usize = core::mem::offset_of!(Fadt, reset_reg)"
        ),
        "FADT 闸门下界必须是 ACPI 1.0 前缀 (reset_reg 起始), 否则 QEMU 116 字节短表被拒"
    );
    assert!(
        !has(all, "table_len < core::mem::size_of::<Fadt>()"),
        "不得用 ACPI 2.0+ 全结构长度作为 FADT 接纳条件"
    );
}

#[test]
fn short_fadt_is_parsed_but_reset_fields_are_gated() {
    let acpi = code_only(&read_src(ACPI));
    assert!(
        has(
            &acpi,
            "FADT_LEN.store(fadt.header.length, Ordering::Release)"
        ),
        "parse_fadt 必须记录实际表长, 供 acpi_reboot 分级读取"
    );
    assert!(
        has(&acpi, "if fadt_len >= FADT_RESET_MIN_LEN"),
        "acpi_reboot 读 reset_reg/reset_value 前必须校验 ACPI 2.0+ 表长"
    );
    assert!(
        has(
            &acpi,
            "const FADT_RESET_MIN_LEN: usize = core::mem::offset_of!(Fadt, reset_value) + 1"
        ),
        "复位字段闸门须覆盖 reset_reg + reset_value 末尾"
    );
}

#[test]
fn parsers_mark_discovery_state() {
    let acpi = code_only(&read_src(ACPI));
    let fadt = section(&acpi, "fn parse_fadt(", "pub fn acpi_shutdown() -> ! {");
    assert!(
        has(fadt, "FADT_FOUND.store(true, Ordering::Release)"),
        "FADT 解析必须置 has_fadt() 的判定标志"
    );

    let hpet = section(
        &acpi,
        "fn parse_hpet(",
        "pub fn get_hpet_info() -> Option<HpetInfo>",
    );
    assert!(
        has(hpet, "*HPET_INFO.lock() = Some(info)"),
        "HPET 解析必须回填 get_hpet_info() 的唯一数据源"
    );

    let dmar = section(&acpi, "fn parse_dmar(", "pub fn get_dmar_drhd_list()");
    assert!(
        has(dmar, "DMAR_FOUND.store(true, Ordering::Release)"),
        "DMAR 解析必须置 has_dmar() 的判定标志"
    );
    assert!(
        has(&acpi, "pub fn has_dmar() -> bool"),
        "privileged 层必须提供 DMAR 发现状态查询"
    );
    assert!(
        has(&acpi, "DMAR_FOUND.load(Ordering::Acquire)"),
        "privileged has_dmar() 必须读解析期置位的标志"
    );
}

#[test]
fn functions_layer_reports_real_dmar_state() {
    let fun = code_only(&read_src(FUN_ACPI));
    let body = section(&fun, "pub fn has_dmar() -> bool {", "}");
    assert!(
        has(body, "crate::privileged::arch::acpi::has_dmar()"),
        "functions 层 has_dmar() 必须委托 privileged 查询"
    );
    assert!(
        !has(body, "false"),
        "functions 层不得保留恒 false 占位 (DMAR 已由启动期全表遍历解析)"
    );
}

#[test]
fn hpet_layout_and_bitfields_are_locked() {
    let acpi = code_only(&read_src(ACPI));
    assert!(
        has(&acpi, "offset_of!(HpetTable, address) == 40"),
        "HPET GAS 必须固定在 ACPI 规范的偏移 40 (旧布局偏移 42 会读出 0xFED0)"
    );
    assert!(
        has(&acpi, "offset_of!(HpetTable, hpet_number) == 52"),
        "HPET Number 必须固定在偏移 52"
    );
    assert!(
        has(&acpi, "core::mem::size_of::<HpetTable>() == 56"),
        "ACPI HPET 表长度固定 56 字节"
    );
    assert!(
        has(&acpi, "hpet_comparator_count(hpet.caps)"),
        "比较器数量必须从位域字节解码, 不得读独立字节"
    );
    assert!(
        has(&acpi, "hpet_counter_size(hpet.caps)"),
        "计数器位宽必须从位域字节 bit5 解码"
    );
    assert!(
        has(&acpi, "hpet.address[11]"),
        "HPET 基址必须读取 GAS 的完整 64 位地址"
    );
}

#[test]
fn qemu_boot_asserts_fadt_milestone() {
    let qemu = read_src(QEMU);
    assert!(
        has(&qemu, "FADT: PM1a_EVT="),
        "QEMU x86_64 分支必须断言 FADT 解析里程碑, 而非仅断言 MADT"
    );
    assert!(
        before(&qemu, "if boot_and_check \"x86_64\"", "FADT: PM1a_EVT="),
        "FADT 断言必须位于 x86_64 启动检查分支内"
    );
    assert!(
        has(&qemu, "P-77 接线回归"),
        "FADT 断言缺失时必须输出可定位的 P-77 回归文案"
    );
    assert!(
        has(&qemu, "HPET: base=0xFED00000"),
        "QEMU HPET 基址断言必须存在 (旧布局会读出 0xFED0)"
    );
}

#[test]
fn single_table_parser_remains_for_c_abi() {
    let acpi = code_only(&read_src(ACPI));
    assert!(
        has(&acpi, "pub fn parse_madt(multiboot2_info_ptr: u64) -> bool"),
        "parse_madt 保留为独立 MADT 入口"
    );
    assert!(
        has(
            &acpi,
            "pub extern \"C\" fn acpi_parse_madt(mb2_ptr: u64) -> bool"
        ),
        "parse_madt 仍是 C ABI acpi_parse_madt 的转发目标"
    );
    assert!(
        has(&acpi, "parse_madt(mb2_ptr)"),
        "C ABI 入口必须仍委托 parse_madt"
    );
}
