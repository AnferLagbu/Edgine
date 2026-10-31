// SPDX-License-Identifier: MPL-2.0
//! P-19(c) 回归: x86_64 PCI `INTx` → IOAPIC → e1000 中断投递的接线契约.
//!
//! 缺陷形态 (P-19(b) 记录前): `interrupt_line` 全内核零消费者, IOAPIC 从未
//! 初始化 (`IoApicInfo::max_irq` 恒 0 使 `gsi_to_ioapic` 恒 `None`), MSI-X
//! 缺失机型 (QEMU 默认 82540EM) 只剩轮询; 若贸然写 IMS 又会形成"配掩码无人
//! 接收"的死接线.
//!
//! 修复形态: MADT ISO → GSI 解析 + IOAPIC 按 ACPI 初始化并回填条目数 +
//! `INTx` 路由写重定向表 + IDT 注册; e1000 按 MSI-X → `INTx` → 轮询 的优先级接线,
//! IMS 只在中断源接线成功后才使能. 中断实际到达由 QEMU e2e 断言
//! (`first device interrupt delivered`), 本文件锁定接线形态本身.

use std::fs;
use std::path::PathBuf;

const ACPI: &str = "src/kernel/privileged/arch/x86_64/acpi.rs";
const IOAPIC: &str = "src/kernel/privileged/arch/x86_64/ioapic.rs";
const SMP_INIT: &str = "src/kernel/privileged/arch/x86_64/smp_init.rs";
const NET_IRQ: &str = "src/kernel/privileged/net/init/irq.rs";
const E1000: &str = "src/kernel/functions/driver/net/e1000.rs";
const IDT: &str = "src/kernel/privileged/idt/table.rs";

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

#[test]
fn madt_iso_drives_irq_to_gsi_and_backfills_ioapic_lines() {
    let acpi = code_only(&read_src(ACPI));
    assert!(
        has(&acpi, "MADT_TYPE_ISO =>"),
        "MADT 解析必须消费 ISO 条目 (0x02), 否则 IRQ→GSI 覆盖丢失"
    );
    assert!(
        has(&acpi, "IRQ_OVERRIDES.lock()[idx] = Some(IrqOverride"),
        "ISO 条目必须落表供 irq_override/irq_to_gsi 查询"
    );
    assert!(
        has(&acpi, "pub fn irq_to_gsi(irq: u8) -> u32"),
        "IRQ→GSI 查询面必须存在 (INTx 路由的唯一来源)"
    );

    let ioapic = code_only(&read_src(IOAPIC));
    assert!(
        has(&ioapic, "for irq in 0..max_irq"),
        "IOAPIC 初始化掩蔽范围必须来自 IOAPICVER 条目数, 不得硬编码"
    );
    assert!(
        !has(&ioapic, "for irq in 0..24u8"),
        "不得回退到硬编码 24 条重定向表"
    );
    assert!(
        has(&ioapic, "let max_irq = init_one(info.base_addr)"),
        "init_from_acpi 必须取得硬件条目数"
    );
    assert!(
        has(&ioapic, "set_ioapic_max_irq(idx, max_irq)"),
        "硬件条目数必须回填 ACPI 的 IoApicInfo, 否则 gsi_to_ioapic 恒 None"
    );
}

#[test]
fn ioapic_initialized_before_device_wiring() {
    let smp = code_only(&read_src(SMP_INIT));
    assert!(
        before(
            &smp,
            "super::acpi::parse_madt(0)",
            "super::ioapic::init_from_acpi()"
        ),
        "IOAPIC 初始化必须晚于 MADT 解析 (依赖枚举结果)"
    );
    assert!(
        before(&smp, "super::ioapic::init_from_acpi()", "if ap_count <= 1"),
        "IOAPIC 初始化必须早于单核提前返回 (单核同样需要中断路由)"
    );

    let e1000 = code_only(&read_src(E1000));
    assert!(
        has(&e1000, "net_register_intx_isr(dev.interrupt_line)"),
        "INTx 接线必须使用 PCI 配置空间 interrupt_line"
    );
    assert!(
        has(&e1000, "} else if dev.interrupt_pin != 0 {"),
        "MSI-X 不可用且存在 INTx pin 时必须尝试 IOAPIC 路由"
    );
}

#[test]
fn net_intx_registration_programs_route() {
    let net_irq = code_only(&read_src(NET_IRQ));
    assert!(
        has(&net_irq, "if !(1..16).contains(&irq)"),
        "x86_64 INTx 仅接受 1..=15 (0 属 PIT, >=16 属 MSI/LAPIC 向量域)"
    );
    assert!(
        has(&net_irq, "register_irq(irq, net_intx_irq_handler"),
        "INTx 必须先登记 IDT handler"
    );
    assert!(
        before(
            &net_irq,
            "net_intx_irq_handler",
            "route_intx_irq(irq, IRQ_BASE + irq"
        ),
        "登记 handler 之后才写 IOAPIC 路由 (避免无接收者的投递窗口)"
    );
    assert!(
        has(&net_irq, "raise_softirq(SoftirqVec::NetRx)"),
        "ISR 分发仍须唤醒 NetRx 底半部"
    );
}

#[test]
fn e1000_enables_ims_only_after_successful_wiring() {
    let e1000 = code_only(&read_src(E1000));
    assert_eq!(
        occurrences(&e1000, "pci::msi::msix_enable(dev, 1)"),
        1,
        "MSI-X 尝试必须保持单点"
    );
    assert_eq!(
        occurrences(&e1000, "net_register_msix_isr(msix.base_vector)"),
        1,
        "MSI-X 注册必须保持单点"
    );
    assert_eq!(
        occurrences(&e1000, "net_register_intx_isr(dev.interrupt_line)"),
        1,
        "INTx 注册必须保持单点"
    );
    assert_eq!(
        occurrences(
            &e1000,
            "irq_enable(E1000_ICR_RXT0 | E1000_ICR_RXDMT0 | E1000_ICR_LSC)"
        ),
        2,
        "IMS 只允许出现在 MSI-X 与 INTx 两个成功支 (新增无条件使能即破坏死接线防线)"
    );
    assert_eq!(
        occurrences(&e1000, "IMS 保持掩蔽走轮询"),
        2,
        "两条失败支都必须保持 IMS 掩蔽并回落轮询"
    );
}

#[test]
fn ioapic_delivery_skips_8259_spurious_check() {
    let idt = code_only(&read_src(IDT));
    assert!(
        has(
            &idt,
            "!legacy_irq_via_ioapic()\n&& let Some(spurious) = detect_spurious_8259_irq(irq)"
        ),
        "IOAPIC 投递时 PIC ISR 恒 0, 不得再走 8259 假性中断判定"
    );
}
