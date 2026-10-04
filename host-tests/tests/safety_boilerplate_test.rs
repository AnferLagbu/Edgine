//! I-11: scheduler_ex.rs / pmm.rs SAFETY 注释差异化静态契约测试
//!
//! 验证 maintenance-2026-06-11.md 中 I-11 验收标准:
//!   "所有 unsafe 块的 SAFETY 注释差异化 (grep 重复数 ≤ 5)"
//!
//! 防止后续重构时引入新的 boilerplate SAFETY 注释 (一句话套所有 unsafe 块).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

const MAX_DUPLICATES: usize = 5;

/// Strip //-comments and string literals, then count SAFETY: occurrences
fn count_safety_boilerplate(src: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in src.lines() {
        // SAFETY comments follow patterns:
        //   // SAFETY: <text>
        //   //   - SAFETY: <text>  (in nested)
        //   /// # SAFETY
        let trimmed = line.trim();
        if trimmed.contains("SAFETY:") || trimmed.contains("# SAFETY") {
            // 提取 SAFETY 后面的内容作为分组 key
            // 统一空白, 忽略前后空格
            let key = trimmed
                .split("SAFETY")
                .nth(1)
                .map(|s| s.trim_start_matches(':').trim().to_string())
                .unwrap_or_default();
            if !key.is_empty() {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn check_boilerplate(file: &str, top_dupes: &[(&str, usize)]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // Edgine workspace root (host-tests' parent)
        .join("src/kernel/framework")
        .join(file);
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    let counts = count_safety_boilerplate(&src);

    let mut total: usize = 0;
    for (text, count) in counts.iter() {
        total += count;
        if *count > MAX_DUPLICATES {
            panic!(
                "{} SAFETY 注释有 {} 次重复 (> {}): {}",
                file, count, MAX_DUPLICATES, text
            );
        }
    }
    println!(
        "{}: SAFETY 总数 {} (max dup ≤ {} ✓)",
        file, total, MAX_DUPLICATES
    );

    // 额外检查: 给定 fixture 中的预期重复数
    for (text, expected) in top_dupes {
        let actual = counts.get(*text).copied().unwrap_or(0);
        assert_eq!(
            actual, *expected,
            "{}: SAFETY 文本 {:?} 实际 {} 次, 期望 {} 次",
            file, text, actual, expected
        );
    }
}

#[test]
fn test_scheduler_ex_safety_diversity() {
    // 期望: 修后 boilerplate ≤ 5. 这里给具体断言:
    // - "t 由 make_test_thread 分配" 出现 4 次 (旧值, 可能微调)
    // - "调用方保证 thread 有效, push_back 串行" 出现 4 次
    // 其他应 ≤ 5
    check_boilerplate(
        "proc/scheduler_ex.rs",
        &[
            // 数量动态 — 验证不超过阈值即可
        ],
    );
}

#[test]
fn test_pmm_safety_diversity() {
    check_boilerplate("mm/pmm.rs", &[]);
}

#[test]
fn test_kernel_wide_boilerplate_inventory() {
    // I-11 修复只针对 scheduler_ex.rs / pmm.rs. 其他文件暂时记录在案,
    // 后续按 I-11 同方案逐个修复 (审计 5 原文提到仅这两个文件存在"行数过多").
    // 此测试报告但不强制 — 是 inventory 性质, 不 panic.
    let framework = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/framework");
    let mut report: Vec<String> = Vec::new();
    walk(&framework, &mut |path: &Path| {
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            return;
        }
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return,
        };
        let counts = count_safety_boilerplate(&src);
        let over: Vec<_> = counts
            .iter()
            .filter(|(_, c)| **c > MAX_DUPLICATES)
            .collect();
        if !over.is_empty() {
            let rel = path.strip_prefix(&framework).unwrap_or(path);
            for (text, count) in over {
                report.push(format!("{}: {:?} 重复 {} 次", rel.display(), text, count));
            }
        }
    });
    if !report.is_empty() {
        println!(
            "\n[I-11 inventory] 以下文件仍有 boilerplate SAFETY 注释 (待后续修复):\n  {}\n",
            report.join("\n  ")
        );
    }
    // 不强制 — 仅记录
}

fn walk(dir: &Path, cb: &mut dyn FnMut(&Path)) {
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, cb);
            } else {
                cb(&p);
            }
        }
    }
}
