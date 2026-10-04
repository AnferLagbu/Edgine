//! DECISION-H storage 专项: NVMe/AHCI 架构契约验证 (3 号子步 storage_init 退位后)
//!
//! 验证退位后的状态契约 (functions 权威, privileged 机制保留):
//! 1. privileged `nvme.rs` 仅剩 wire 类型 (NvmeCommand/NvmeCompletion), 控制器业务已删
//! 2. privileged `ahci.rs` 仅剩 wire 命令结构 (H2dFis/命令头/命令表), HBA 寄存器布局已删
//! 3. privileged `storage_init` 已整体退位 (无 ATA 回退路径); PCI AHCI/NVMe + ATA
//!    探测/注册全部由 functions 接管
//! 4. functions `storage_init` 调用 `_block` 适配器注册 EGDF + MSI-X 接线
//! 5. crate root lib.rs 编排 functions storage_init (合法双向编排者)
//! 6. 双侧均无文件级 dead_code 豁免 (I-49 契约延续)
//!
//! 主机端无法实际跑 PCI 探测, 这里做静态契约验证: 读源文件做关键字检查.

use std::fs;
use std::path::Path;

const PRIVILEGED_DIR: &str = "../src/kernel/privileged/driver/storage";
const FUNCTIONS_DIR: &str = "../src/kernel/functions/driver/storage";

fn read_source(dir: &str, name: &str) -> String {
    let path = Path::new(dir).join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {} failed: {}", path.display(), e))
}

/// 剥离 `//` 与 `//!` 注释行 — 静态契约只匹配真实代码, 不匹配文档图示
fn strip_comment_lines(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// privileged 侧: 机制保留面 (wire 类型 + ATA 回退)
// ============================================================================

#[test]
fn test_privileged_nvme_wire_types_only() {
    let src = read_source(PRIVILEGED_DIR, "nvme.rs");
    // wire 类型必须保留 (privileged safe wrapper 与 functions 驱动共用)
    for sym in ["pub struct NvmeCommand", "pub struct NvmeCompletion"] {
        assert!(src.contains(sym), "privileged nvme.rs 缺失 {}", sym);
    }
    // 控制器业务必须已退位 (DECISION-H 3 号子步)
    assert!(
        !src.contains("pub struct NvmeController"),
        "privileged nvme.rs 仍含 NvmeController (应已迁 functions)"
    );
    assert!(
        !src.contains("#![allow(dead_code)]"),
        "privileged nvme.rs 不应有文件级 dead_code 豁免"
    );
}

#[test]
fn test_privileged_ahci_wire_types_only() {
    let src = read_source(PRIVILEGED_DIR, "ahci.rs");
    // wire 命令结构必须保留 (privileged mod.rs 填充原语使用)
    for sym in [
        "pub struct AhciCommandHeader",
        "pub struct AhciCommandTable",
        "pub struct H2dFis",
    ] {
        assert!(src.contains(sym), "privileged ahci.rs 缺失 {}", sym);
    }
    // HBA 寄存器布局与控制器业务必须已退位
    for sym in [
        "pub struct AhciHbaGhc",
        "pub struct AhciPort",
        "pub struct AhciController",
    ] {
        assert!(
            !src.contains(sym),
            "privileged ahci.rs 仍含 {} (应已迁 functions)",
            sym
        );
    }
    assert!(
        !src.contains("#![allow(dead_code)]"),
        "privileged ahci.rs 不应有文件级 dead_code 豁免"
    );
}

#[test]
fn test_privileged_storage_init_removed() {
    let code = strip_comment_lines(&read_source(PRIVILEGED_DIR, "mod.rs"));
    // framekernel 阶段 3: privileged storage_init 整体退位 (ATA 回退路径已迁 functions)
    assert!(
        !code.contains("pub fn storage_init"),
        "privileged storage_init 应已整体退位 (ATA 迁 functions)"
    );
    assert!(
        !code.contains("ata_init"),
        "privileged mod.rs 不应再含 ATA 检测 (应已迁 functions)"
    );
    // 对应模块文件亦应删除
    assert!(
        !Path::new(PRIVILEGED_DIR).join("ata.rs").exists(),
        "privileged ata.rs 应已删除 (迁 functions)"
    );
    assert!(
        !Path::new(PRIVILEGED_DIR).join("ata_block.rs").exists(),
        "privileged ata_block.rs 应已删除 (迁 functions)"
    );
    // PCI AHCI/NVMe 探测业务必须已退位
    assert!(
        !code.contains("scan_all_buses"),
        "privileged mod.rs 仍做 PCI 扫描 (应已迁 functions)"
    );
    assert!(
        !code.contains("AhciController::new") && !code.contains("NvmeController::new"),
        "privileged mod.rs 仍初始化控制器 (应已迁 functions)"
    );
    // MSI-X ISR 编排机制保留 (注册契约槽 + ISR 注册入口)
    for sym in [
        "nvme_register_msix_isr",
        "nvme_register_functions_msix_dispatch",
    ] {
        assert!(code.contains(sym), "privileged mod.rs 缺失机制入口 {}", sym);
    }
}

// ============================================================================
// functions 侧: 权威实现 (控制器业务 + 注册路径 + MSI-X 接线)
// ============================================================================

#[test]
fn test_functions_controllers_present() {
    let ahci = read_source(FUNCTIONS_DIR, "ahci.rs");
    assert!(
        ahci.contains("pub struct AhciController"),
        "functions AhciController 缺失"
    );
    assert!(
        ahci.contains("pub struct AhciPort"),
        "functions AhciPort 缺失"
    );
    assert!(
        ahci.contains("#![deny(unsafe_code)]"),
        "functions ahci.rs 必须 0 unsafe"
    );
    let nvme = read_source(FUNCTIONS_DIR, "nvme.rs");
    assert!(
        nvme.contains("pub struct NvmeController"),
        "functions NvmeController 缺失"
    );
    assert!(
        nvme.contains("#![deny(unsafe_code)]"),
        "functions nvme.rs 必须 0 unsafe"
    );
}

#[test]
fn test_functions_storage_init_uses_block_devices() {
    // 验证 functions 启动路径实际调用了 block 设备注册 (非死代码)
    let src = read_source(FUNCTIONS_DIR, "mod.rs");
    assert!(
        src.contains("AhciBlockDevice::new"),
        "functions storage_init 未调用 AhciBlockDevice::new"
    );
    assert!(
        src.contains("NvmeBlockDevice::new"),
        "functions storage_init 未调用 NvmeBlockDevice::new"
    );
    assert!(
        src.contains("AtaBlockDevice::new"),
        "functions storage_init 未调用 AtaBlockDevice::new (ATA 回迁后应注册 ata0-3)"
    );
    assert!(
        src.contains("register_block_device"),
        "functions storage_init 未注册 block 设备到 EGDF"
    );
    // MSI-X 接线 (DECISION-H 2 号子步): 启用 + ISR 注册 + functions 分发契约注册
    assert!(
        src.contains("enable_msix") && src.contains("nvme_register_msix_isr"),
        "functions storage_init 未接线 NVMe MSI-X"
    );
    assert!(
        src.contains("nvme_register_functions_msix_dispatch"),
        "functions storage_init 未注册 functions MSI-X 分发契约"
    );
}

#[test]
fn test_lib_rs_orchestrates_functions_storage_init() {
    // crate root (合法双向编排者) 必须调用 functions storage_init (x86_64 门控)
    let src = fs::read_to_string("../src/kernel/lib.rs").expect("read lib.rs failed");
    assert!(
        src.contains("functions::driver::storage::storage_init"),
        "crate root lib.rs 未编排 functions storage_init"
    );
}

// ============================================================================
// 双侧公共契约: 无 dead_code 豁免 (I-49 延续)
// ============================================================================

#[test]
fn test_no_dead_code_allow_in_storage() {
    for (dir, name) in [
        (PRIVILEGED_DIR, "mod.rs"),
        (PRIVILEGED_DIR, "nvme.rs"),
        (PRIVILEGED_DIR, "ahci.rs"),
        (FUNCTIONS_DIR, "mod.rs"),
        (FUNCTIONS_DIR, "nvme.rs"),
        (FUNCTIONS_DIR, "ahci.rs"),
        (FUNCTIONS_DIR, "ata.rs"),
    ] {
        let src = read_source(dir, name);
        assert!(
            !src.contains("#![allow(dead_code)]") && !src.contains("#![allow(unused)]"),
            "{}/{} 含文件级 dead_code 豁免",
            dir,
            name
        );
    }
}
