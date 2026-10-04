//! I-53: 网卡驱动编译时架构互斥静态契约测试
//!
//! 验证 maintenance-2026-06-11.md 中 I-53 验收:
//!   "双架构二进制包含全部网卡驱动"
//!
//! 防止后续重构时在网卡驱动路径上引入 `#[cfg(target_arch = "...")]` 互斥,
//! 阻断单二进制双架构运行.
//!
//! 批次 Z ④: 旧 privileged virtio-net 驱动已删除 (权威迁 functions, 经
//! NetOps 安全桥接入), 本文件仅保留 e1000 架构无关契约.

use std::fs;
use std::path::Path;

const DRIVER_NET_DIR: &str = "src/kernel/privileged/driver/net";

/// 收集 `#[cfg(target_arch = "...")]` 紧邻 `let mut xxx = ...` 的赋值 (排除模块/类型声明).
fn find_arch_mutex_let_assigns(src: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        // 形如: #[cfg(target_arch = "x86_64")] 紧跟 let dma_phys = ...
        if (t.starts_with("#[cfg(target_arch = \"x86_64\")]")
            || t.starts_with("#[cfg(target_arch = \"aarch64\")]"))
            && i + 1 < lines.len()
        {
            let next = lines[i + 1].trim();
            // 仅当紧邻行是 `let <name> = ...` 才算"互斥赋值"; 排除 cfg 模块声明
            if next.starts_with("let ") && next.contains('=') {
                // 提取变量名
                let ident: String = next
                    .trim_start_matches("let ")
                    .trim_start_matches("mut ")
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !ident.is_empty() {
                    out.push((i + 1, ident));
                }
            }
        }
    }
    out
}

#[test]
fn test_e1000_driver_arch_agnostic() {
    // e1000 驱动应当不包含任何 cfg(target_arch) — 全部走 IoMem 抽象
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(DRIVER_NET_DIR)
        .join("e1000.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    // e1000_probe 中的 #[cfg(target_arch = "aarch64")] return -1 是合法例外:
    // aarch64 QEMU virt 无 PCI ECAM, e1000 probe 访问 0x3F000000 导致 Data Abort,
    // 必须在 probe 函数内部安全返回. 其余代码应保持架构无关.
    let x86 = src.matches("cfg(target_arch = \"x86_64\")").count();
    let arm = src.matches("cfg(target_arch = \"aarch64\")").count();
    // 允许 e1000_probe 中的 1 处 aarch64 guard (返回 -1)
    assert_eq!(x86, 0, "e1000.rs 不应硬编码 x86_64 cfg (I-53)");
    assert!(
        arm <= 1,
        "e1000.rs 不应有多处 aarch64 cfg (I-53), 允许 e1000_probe 早期返回"
    );
}

#[test]
fn test_functions_virtio_net_no_arch_mutex_let_assigns() {
    // functions VirtioNetDriver (virtio-net 权威, 批次 Z ④) 中不应再有
    // `#[cfg(target_arch)] + let x = ...` 的架构互斥赋值.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/functions/driver/virtio/net.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    let mutexes = find_arch_mutex_let_assigns(&src);
    assert!(
        mutexes.is_empty(),
        "functions virtio/net.rs 仍存在编译时架构互斥的 let 赋值 (I-53): {:?}",
        mutexes
    );
}
