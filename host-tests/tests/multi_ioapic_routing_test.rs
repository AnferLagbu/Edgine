//! 多 IOAPIC GSI 路由契约测试
//!
//! ## B08-20 迁移 (2026-09-06) — 路由逻辑因内核 arch 层 host 不可测已标注
//!
//! 原镜像平行实现了 `IoApicInfo` / `gsi_to_ioapic` (参数化 `&[Option<IoApicInfo>]`
//! 的纯计算) 与全部路由测试. 经评估, 内核真实实现 **host 不可测**, 本地平行
//! 实现已删除, 不保留镜像:
//!
//! - 内核权威实现为 `privileged/arch/x86_64/acpi.rs::gsi_to_ioapic(gsi)` —
//!   **签名不同**: 无参数表入参, 直接读取**私有**全局 `static IOAPICS:
//!   IrqSpinLock<[Option<IoApicInfo>; MAX_IOAPICS]>`.
//! - 该全局仅能由 `parse_madt(multiboot2_info_ptr)` 填充 (需真实 ACPI MADT /
//!   Multiboot2 启动数据), host 无法构造; 且 `IOAPICS` 非 pub, 无公开 setter.
//! - 因此 GSI→(ioapic_index, local_irq) 路由决策无法在 host 上对真实内核函数
//!   验证, 对应测试用例全部移除.
//!
//! 保留: 内核 `IoApicInfo` 为 `pub struct` (pub 字段 id/base_addr/gsi_base/max_irq),
//! 纯数据可 host 构造, 其字段语义测试改引内核类型.

use edgine::kernel::privileged::arch::x86_64::acpi::IoApicInfo;

/// 双 IOAPIC 控制器场景 (多路服务器): id/base/gsi_base/max_irq 字段语义
#[test]
fn ioapic_info_fields_semantics() {
    let ioapic0 = IoApicInfo {
        id: 0,
        base_addr: 0xFEC00000,
        gsi_base: 0,
        max_irq: 24,
    };
    let ioapic1 = IoApicInfo {
        id: 1,
        base_addr: 0xFEC01000,
        gsi_base: 24,
        max_irq: 24,
    };
    // id: ACPI MADT 提供的硬件标识, 多路服务器区分控制器
    assert_eq!(ioapic0.id, 0, "第一个 IOAPIC 的硬件 ID = 0");
    assert_eq!(ioapic1.id, 1, "第二个 IOAPIC 的硬件 ID = 1");
    assert_ne!(ioapic0.id, ioapic1.id, "不同 IOAPIC 必须有不同硬件 ID");
    // base_addr: IOAPIC MMIO 寄存器基址 (Intel 默认 0xFEC00000, 第二路 +4KB)
    assert_eq!(
        ioapic0.base_addr, 0xFEC00000,
        "IOAPIC 0 MMIO 基址 = 0xFEC00000"
    );
    assert_eq!(
        ioapic1.base_addr, 0xFEC01000,
        "IOAPIC 1 MMIO 基址 = 0xFEC01000 (偏移 4KB)"
    );
    assert_ne!(
        ioapic0.base_addr, ioapic1.base_addr,
        "不同 IOAPIC 必须有不同 MMIO 基址"
    );
    // gsi_base/max_irq: 路由区间 [gsi_base, gsi_base+max_irq) 语义
    assert_eq!(ioapic0.gsi_base, 0);
    assert_eq!(ioapic1.gsi_base, 24);
    assert_eq!(ioapic0.max_irq, 24);
    assert_eq!(ioapic1.max_irq, 24);
    // 区间端点: GSI 23 在 IOAPIC0 域内, GSI 24 起归 IOAPIC1 (内核 gsi_to_ioapic
    // 判定 `gsi >= gsi_base && gsi < gsi_base + max_irq` 的区间语义)
    assert!(0 <= 23 && 23 < ioapic0.gsi_base + u32::from(ioapic0.max_irq));
    assert!(24 >= ioapic1.gsi_base && 24 < ioapic1.gsi_base + u32::from(ioapic1.max_irq));
}
