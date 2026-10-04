//! I-09: Rust nightly 不稳定 API 依赖最小化 — 静态契约测试
//!
//! 验证 maintenance-2026-06-11.md I-09 验收:
//!   - edgine 不再依赖 `#![feature(asm)]` (已稳定为 `core::arch::asm!`)
//!   - edgine 内 `feature(` 总数 ≤ 1 (仅 alloc_error_handler)
//!   - 所有内联汇编均走 `core::arch::asm!` 路径, 不再裸用 `asm!`
//!
//! 任何新增 `#![feature(...)]` 顶层声明需要先在 I-09 评估中说明.

use std::fs;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn test_edgine_lib_rs_no_feature_asm() {
    let lib = repo_root().join("src/kernel/lib.rs");
    let content =
        fs::read_to_string(&lib).unwrap_or_else(|e| panic!("无法读取 {}: {}", lib.display(), e));

    // 提取非注释行 (以 // 起始的行) — 注释里出现的文本是文档, 不算数
    let non_comment: String = content
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !non_comment.contains("#![feature(asm)]"),
        "edgine 不应再声明 #![feature(asm)], asm! 已稳定为 core::arch::asm!"
    );
    assert!(
        !non_comment.contains("#![feature(asm "),
        "edgine 不应再以 feature(asm ...) 形式声明"
    );
}

#[test]
fn test_edgine_lib_rs_feature_count_minimal() {
    // edgine 内的 #![feature(...)] 数量应 ≤ 1 (仅 alloc_error_handler)
    let lib = repo_root().join("src/kernel/lib.rs");
    let content =
        fs::read_to_string(&lib).unwrap_or_else(|e| panic!("无法读取 {}: {}", lib.display(), e));

    // 取非注释的前 30 行, 统计 feature( 出现次数
    let head: String = content
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .take(30)
        .collect::<Vec<_>>()
        .join("\n");
    let count = head.matches("#![feature(").count();
    assert!(
        count <= 1,
        "edgine 顶层 #![feature(...)] 数量 = {} (> 1, I-09 要求最小化).\n当前:\n{}",
        count,
        head
    );
}

#[test]
fn test_kernel_uses_core_arch_asm_not_bare_asm() {
    // 扫描 privileged/, 内联汇编必须走 `core::arch::asm!` 路径.
    // 允许两种风格:
    //   1) `core::arch::asm!(...)` 直接限定
    //   2) `use core::arch::asm;` 后裸用 `asm!(...)` (仍来自稳定 core::arch)
    // 不允许: 完全没有 core::arch 来源的 `asm!(...)` 调用
    //         `llvm_asm!` 旧式调用
    // 例外: smoltcp/benches (vendored 第三方, 不在审查范围).
    let privileged = repo_root().join("src/kernel/privileged");
    let mut bad: Vec<String> = Vec::new();

    fn walk(dir: &Path, out: &mut Vec<String>, privileged_root: &Path) {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    if p.ends_with("smoltcp") || p.ends_with("target") {
                        continue;
                    }
                    walk(&p, out, privileged_root);
                } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                    let src = match fs::read_to_string(&p) {
                        Ok(s) => s,
                        Err(_) => return,
                    };

                    // 文件级导入: `use core::arch::asm;` 允许裸用
                    let has_qualified_use = src.lines().any(|l| {
                        l.trim_start().starts_with("use ") && l.contains("core::arch::asm")
                    });

                    for (n, line) in src.lines().enumerate() {
                        let trimmed = line.trim_start();
                        // 跳过注释与字符串
                        if trimmed.starts_with("//") {
                            continue;
                        }
                        // 检查裸 `asm!(...)` 调用
                        if trimmed.starts_with("asm!")
                            && !line.contains("core::arch::")
                            && !has_qualified_use
                        {
                            let rel = p.strip_prefix(privileged_root).unwrap_or(&p);
                            out.push(format!("{}:{}: {}", rel.display(), n + 1, line.trim()));
                        }
                        // 旧式 llvm_asm!
                        if trimmed.contains("llvm_asm!") {
                            let rel = p.strip_prefix(privileged_root).unwrap_or(&p);
                            out.push(format!(
                                "{}:{}: llvm_asm! (旧式 API, 已废弃): {}",
                                rel.display(),
                                n + 1,
                                line.trim()
                            ));
                        }
                    }
                }
            }
        }
    }

    walk(&privileged, &mut bad, &privileged);

    assert!(
        bad.is_empty(),
        "I-09: 内联汇编必须走 `core::arch::asm!` (允许 `use core::arch::asm;` 后裸用). 违规:\n{}",
        bad.join("\n")
    );
}
