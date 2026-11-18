// L2 源码契约锁: fs 层对 SGEG 的直接引用只允许两个具名出口
// (`authorize.rs` / `subject.rs`)。其余文件出现 `sgeg::` / `privileged::sgeg`
// 即视为解耦退化, 门禁转红。
//
// 口径:
//   - 只判代码行; 行首为 `//` / `///` / `//!` 的注释行跳过 (文档里提到 `sgeg::`
//     不构成依赖);
//   - 白名单按文件名, 不按目录 —— 两个出口本身必须真引用 sgeg (非空转断言)。

use std::fs;
use std::path::{Path, PathBuf};

const FS_ROOT: &str = "src/kernel/functions/fs";
const EXITS: [&str; 2] = ["authorize.rs", "subject.rs"];

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("读 fs 子树") {
        let p = entry.expect("目录项").path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn code_line(ln: &str) -> bool {
    let s = ln.trim_start();
    !(s.starts_with("//") || s.is_empty())
}

#[test]
fn fs_layer_only_reaches_sgeg_through_two_exits() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(FS_ROOT);
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(!files.is_empty(), "fs 子树必须扫到源码");

    let mut violations = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_str().unwrap();
        if EXITS.contains(&name) {
            continue;
        }
        let src = fs::read_to_string(f).expect("读源码");
        for (i, ln) in src.lines().enumerate() {
            if !code_line(ln) {
                continue;
            }
            let compact: String = ln.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.contains("sgeg::") || compact.contains("privileged::sgeg") {
                violations.push(format!(
                    "{}:{}",
                    f.strip_prefix(&root).unwrap().display(),
                    i + 1
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "fs 层只允许经 authorize/subject 两个出口访问 SGEG, 违规点: {violations:?}"
    );

    // 非空转: 两个出口必须真引用 SGEG
    for name in EXITS {
        let p = root.join(name);
        let src = fs::read_to_string(&p).expect("出口文件必须存在");
        assert!(
            src.contains("privileged::sgeg"),
            "{name} 必须实引用 privileged::sgeg (否则本锁空转)"
        );
    }
}
