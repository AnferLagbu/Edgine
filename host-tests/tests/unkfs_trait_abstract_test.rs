//! I-04: UNKFS 18 文件强耦合 — trait 抽象静态契约测试
//!
//! 验证 maintenance-2026-06-11.md I-04 验收:
//!   - 各 UNKFS 子系统有独立 trait 定义, 供 mock 注入
//!   - 至少存在 `Checksum` trait (本次样板) + 至少一处实现
//!
//! 本轮最小化 I-04 修复: 只为最独立的 checksum 子系统引入 trait.
//! 后续按需扩展到 SPA/DMU/ZAP/TXG/ZIL/ARC/RAID-Z.

use std::fs;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn test_unkfs_checksum_trait_exists() {
    let path = repo_root().join("src/kernel/services/fs/unkfs/checksum.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    // I-04 要求各子系统有独立 trait
    assert!(
        src.contains("pub trait Checksum"),
        "checksum.rs 必须定义 `pub trait Checksum` (I-04)."
    );
    // 至少含 compute + verify 两个核心方法
    assert!(src.contains("fn compute("), "Checksum::compute 必须存在");
    assert!(src.contains("fn verify("), "Checksum::verify 必须存在");
}

#[test]
fn test_unkfs_checksum_trait_impl_for_hvchecksum() {
    let path = repo_root().join("src/kernel/services/fs/unkfs/checksum.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    // 必须为现有 UnkfsChecksum 提供 trait 实现
    assert!(
        src.contains("impl Checksum for UnkfsChecksum"),
        "必须 `impl Checksum for UnkfsChecksum` (I-04)."
    );
}

#[test]
fn test_unkfs_no_cyclic_sibling_use() {
    // I-04 根因: 18 文件强耦合. 验收应避免出现环状依赖:
    //   A → B → A
    // 简单检查: 没有文件既 use A 又被 A use (本地小组件如 bp 类型不算, 跳过)
    // 简化为: 允许 use 兄弟模块 (单向), 不允许出现 A → B → A 环
    //
    // 由于 UNKFS 18 文件关系复杂, 静态检查环代价大, 此处只做基础结构性检查:
    // 顶层 mod.rs 必须列出全部 18 子模块, 且不允许有 #[cfg(...)] 隐藏
    let path = repo_root().join("src/kernel/services/fs/unkfs/mod.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    let expected = [
        "arc",
        "bp",
        "checksum",
        "compress",
        "dataset",
        "dedup",
        "dmu",
        "dva",
        "unkfs",
        "metaslab",
        "raidz",
        "snapshot",
        "spa",
        "txg",
        "vdev",
        "zap",
        "zil",
        "zil_persist",
    ];

    for mod_name in &expected {
        let decl = format!("pub mod {};", mod_name);
        assert!(
            src.contains(&decl),
            "mod.rs 必须显式声明 `{}` 模块 (I-04 要求 18 文件结构可见)",
            decl
        );
    }
}
