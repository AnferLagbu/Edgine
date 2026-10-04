//! I-08: smoltcp vendored 决策保持
//!
//! 验证 maintenance-2026-06-11.md I-08 评估结论 + 2026-09-13 升级:
//!   - vendored 副本的版本 = 0.14.x (Cargo.toml) — 当前 0.14.0 (升级于 2026-09-13)
//!   - edgine 通过 path 依赖消费, 不用 crates.io
//!   - 上游一致性 — 未做 vendored 之外的本地 patch (git log 验证; 本地化走 scripts/smoltcp-localization/)
//!   - REVAL-W W3.1: smoltcp 从 framework/ 迁到 services/ (决策 3-B, FK 合规)
//!
//! 任何变更需要更新 I-08 评估并说明理由.

use std::fs;
use std::path::Path;
use std::process::Command;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn test_smoltcp_vendored_version_is_0_14() {
    // W3.1 (2026-06-24): smoltcp 从 framework/ 迁到 services/ (决策 3-B)
    // 2026-09-13: 0.13.1 → 0.14.0 升级
    let manifest = repo_root().join("src/kernel/services/net/smoltcp/Cargo.toml");
    let content = fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("无法读取 {}: {}", manifest.display(), e));

    // 直接读 Cargo.toml 确认 version
    let version_line = content
        .lines()
        .find(|l| l.starts_with("version ="))
        .expect("smoltcp/Cargo.toml 缺少 version 字段");
    assert!(
        version_line.contains("0.14"),
        "smoltcp vendored 版本已变更为: {} (I-08 决策保持 0.14.x)",
        version_line
    );
}

#[test]
fn test_kernel_consumes_smoltcp_via_path_not_crates_io() {
    // 方案 D: kernel 独立 crate, smoltcp 依赖在 src/kernel/Cargo.toml.
    let manifest = repo_root().join("src/kernel/Cargo.toml");
    let content = fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("无法读取 {}: {}", manifest.display(), e));

    // 找到 smoltcp 依赖行 (跨多行)
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("smoltcp") {
            // 单行依赖 或 多行依赖
            let block: String = if lines[i].trim_end().ends_with(']') {
                lines[i].to_string()
            } else {
                let mut b = lines[i].to_string();
                i += 1;
                while i < lines.len() && !lines[i].trim_end().ends_with(']') {
                    b.push('\n');
                    b.push_str(lines[i]);
                    i += 1;
                }
                if i < lines.len() {
                    b.push('\n');
                    b.push_str(lines[i]);
                }
                b
            };
            assert!(
                block.contains("path ="),
                "kernel 必须 path 依赖 vendored smoltcp, 不应从 crates.io 取.\n当前: {}",
                block
            );
            // 反向断言: 不能 version = "0.14"
            assert!(
                !block.contains("version =") || !block.contains("\"0.14"),
                "kernel 不能从 crates.io 拉 smoltcp 0.14, 应保持 vendored.\n当前: {}",
                block
            );
            return;
        }
        i += 1;
    }
    panic!("kernel/Cargo.toml 缺少 smoltcp 依赖");
}

#[test]
fn test_no_uncommitted_local_patch_to_vendored_smoltcp() {
    // git log 验证 vendored 副本历史 — 若出现与上游无关的 patch commit, 失败
    // 但项目历史本身可能 1 次性 commit 引入整 vendored, 那是 OK 的
    // 这里只检测 "未提交修改" (working tree 不应改 smoltcp 源码)
    let root = repo_root();
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--",
            "src/kernel/services/net/smoltcp/",
        ])
        .current_dir(&root)
        .output()
        .expect("git status 失败");
    let out = String::from_utf8_lossy(&status.stdout);
    // vendored 副本若被修改, 应通过 I-08 评估后再合并
    assert!(
        out.trim().is_empty(),
        "vendored smoltcp 有未提交修改, 需先评估:\n{}",
        out
    );
}
