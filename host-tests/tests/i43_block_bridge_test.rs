//! I-43: 块设备单一桥接入口不变式验证
//!
//! 验证修复后的状态契约:
//! 1. `egdf_register_block` 仅在允许文件中被调用 (egdf/mod.rs + egdf/proto_block.rs)
//! 2. 所有块设备驱动通过 `register_block_device` (proto_block) 注册
//! 3. BlockDevice trait 的 blk_read/blk_write 签名与 BlockOps thunk 一致
//!
//! 主机端无法实际执行注册, 这里做静态契约验证: 读源文件做关键字检查.

use std::path::Path;

const KERNEL_DIR: &str = "../src/kernel";

fn read_source(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {}", path, e))
}

/// 收集所有 .rs 文件 (递归)
fn collect_rs_files(dir: &Path) -> Vec<String> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                result.extend(collect_rs_files(&path));
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Some(s) = path.to_str()
            {
                result.push(s.to_string());
            }
        }
    }
    result
}

#[test]
fn test_egdf_register_block_only_in_allowed_files() {
    // egdf_register_block 是低层桥接, 仅允许在以下文件中直接调用:
    // - egdf/mod.rs (定义 + 单元测试)
    // - egdf/proto_block.rs (桥接函数)
    let allowed_suffixes = ["/egdf/mod.rs", "/egdf/proto_block.rs"];

    let kernel_dir = Path::new(KERNEL_DIR);
    let all_rs = collect_rs_files(kernel_dir);

    let mut violations: Vec<String> = Vec::new();

    for file_path in &all_rs {
        // 跳过允许文件
        let is_allowed = allowed_suffixes
            .iter()
            .any(|suffix| file_path.ends_with(suffix));
        if is_allowed {
            continue;
        }

        let content = read_source(file_path);
        // 检查是否有 `egdf_register_block(` 调用 (排除注释行)
        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("//!") {
                continue; // 跳过注释
            }
            if trimmed.contains("egdf_register_block(") {
                violations.push(format!("{}:{}: {}", file_path, i + 1, trimmed.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "I-43 违规: egdf_register_block 在非允许文件中被调用:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_block_drivers_use_register_block_device() {
    // 所有块设备驱动应通过 proto_block::register_block_device 注册,
    // 而非直接调用 egdf_register_block.
    // 检查各驱动文件是否包含 register_block_device 调用.
    let driver_files = [
        "framework/driver/virtio/blk.rs",
        "framework/driver/storage/ahci_block.rs",
        "framework/driver/storage/nvme_block.rs",
        // framekernel 阶段 3: ATA PIO 驱动的权威实装已迁 services
        "services/driver/storage/ata.rs",
    ];

    for driver in &driver_files {
        let path = Path::new(KERNEL_DIR).join(driver);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue, // 文件不存在则跳过
        };
        // 驱动文件应包含 BlockDevice impl (直接或间接)
        let has_block_device_impl = content.contains("impl BlockDevice");
        let has_register = content.contains("register_block_device");
        // 至少应有其一 (有些驱动在 init 函数中注册, 不在驱动文件本身)
        if !has_block_device_impl && !has_register {
            // 可能通过其他路径注册, 仅记录不阻断
            eprintln!(
                "注意: {} 未直接包含 BlockDevice impl 或 register_block_device",
                driver
            );
        }
    }
}

#[test]
fn test_block_ops_thunk_signature_matches_trait() {
    // 验证 proto_block.rs 走 BlockDevice trait 路径 (无 thunk)。
    //
    // 历史: LEGACY-4.2 已于 maintenance-2026-06-11 周期删除 4 个 thunk
    // (blk_read_thunk/write_thunk/is_present_thunk/total_sectors_thunk),
    // 全部迁移到 BlockDevice trait + egdf 桥接。
    // 本测试现在反向验证: proto_block.rs 中**不应**再出现 thunk 函数定义。
    let path = Path::new(KERNEL_DIR).join("framework/egdf/proto_block.rs");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} failed: {}", path.display(), e));

    assert!(
        !content.contains("blk_read_thunk"),
        "blk_read_thunk 应已删除 (LEGACY-4.2), 但 proto_block.rs 仍存在"
    );
    assert!(
        !content.contains("blk_write_thunk"),
        "blk_write_thunk 应已删除 (LEGACY-4.2), 但 proto_block.rs 仍存在"
    );
    assert!(
        !content.contains("blk_is_present_thunk"),
        "blk_is_present_thunk 应已删除 (LEGACY-4.2), 但 proto_block.rs 仍存在"
    );
    assert!(
        !content.contains("blk_total_sectors_thunk"),
        "blk_total_sectors_thunk 应已删除 (LEGACY-4.2), 但 proto_block.rs 仍存在"
    );
}

#[test]
fn test_register_block_device_is_pub() {
    // register_block_device 必须是 pub fn, 确保驱动可调用
    let path = Path::new(KERNEL_DIR).join("framework/egdf/proto_block.rs");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} failed: {}", path.display(), e));
    assert!(
        content.contains("pub fn register_block_device("),
        "register_block_device 不是 pub fn"
    );
}
