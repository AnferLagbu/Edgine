// SPDX-License-Identifier: MPL-2.0
// TD-15 (fd 对象模型阶段 4 收口): 全局数值 FD 编号空间退役反转锁
//
// 阶段 3 已把 8 个非 VFS 子系统全部迁到 per-process `FdTable` +
// `OpenFileTable`; 阶段 4 删除全局 FD 编号规划层 (`FdPlan`/`FdSubsystem`/
// `subsystem_of`/`idx_of`/`fd_at`/`max_slots`/`FdRange`) 与 fd_alloc 模块.
// 本锁为反转锁: 符号重新进入源码或模块文件重新出现即红.
//
// 容量单源: socket 槽位上限收敛到 `privileged::net::slots::MAX_SLOTS`,
// `init` / `wait_queue` / `save` 三处引用面不得再各自派生.

use std::fs;
use std::path::{Path, PathBuf};

const NET_INIT: &str = "src/kernel/privileged/net/init.rs";
const NET_WAIT_QUEUE: &str = "src/kernel/privileged/net/wait_queue.rs";
const NET_SAVE: &str = "src/kernel/privileged/net/save.rs";
const NET_SLOTS: &str = "src/kernel/privileged/net/slots.rs";
const NET_SOCKETS: &str = "src/kernel/privileged/net/init/sockets.rs";

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(relative)
}

fn read(relative: &str) -> String {
    fs::read_to_string(repo_path(relative)).unwrap_or_else(|e| panic!("无法读取 {relative}: {e}"))
}

/// 递归收集 `src/kernel` 下的 `.rs` 源文件 (排除 vendored smoltcp).
fn kernel_rs_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "smoltcp") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&repo_path("src/kernel"), &mut out);
    out
}

/// 标识符边界匹配: `needle` 前一个字符不得是字母/数字/下划线
/// (排除 `set_fd_at(` 这类合法方法名对 `fd_at(` 的子串误报).
fn has_symbol(src: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = src[from..].find(needle) {
        let abs = from + pos;
        let prev_ok = src[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if prev_ok {
            return true;
        }
        from = abs + 1;
    }
    false
}

#[test]
fn test_numeric_fd_plan_symbols_are_gone() {
    // 反转锁: 任一名词重新出现在内核源码即阶段 4 回退.
    let mut offenders = Vec::new();
    for path in kernel_rs_files() {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        for needle in [
            "FdPlan",
            "FdSubsystem",
            "subsystem_of",
            "idx_of",
            "fd_at(",
            "max_slots(",
            "FdRange",
        ] {
            if has_symbol(&src, needle) {
                offenders.push(format!("{}: {needle}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "全局数值 FD 编号规划符号必须整体退役, 残留: {offenders:?}"
    );
}

#[test]
fn test_fd_alloc_modules_and_declarations_removed() {
    for module in [
        "src/kernel/privileged/proc/fd_alloc.rs",
        "src/kernel/functions/proc/fd_alloc.rs",
    ] {
        assert!(
            !repo_path(module).exists(),
            "fd_alloc 模块必须随阶段 4 退役: {module}"
        );
    }
    for mod_rs in [
        "src/kernel/privileged/proc/mod.rs",
        "src/kernel/functions/proc/mod.rs",
    ] {
        let src = read(mod_rs);
        assert!(
            !src.contains("mod fd_alloc"),
            "{mod_rs} 不得残留 fd_alloc 模块声明"
        );
    }
}

#[test]
fn test_net_slot_capacity_single_source() {
    let slots = read(NET_SLOTS);
    assert!(
        slots.contains("pub const MAX_SLOTS: usize = 256;"),
        "槽位容量唯一真源必须落 net/slots.rs (MAX_SLOTS = 256)"
    );
    for (path, needle) in [
        (
            NET_INIT,
            "const MAX_SM_FD: usize = crate::privileged::net::slots::MAX_SLOTS;",
        ),
        (
            NET_WAIT_QUEUE,
            "const MAX_SM_FD: usize = crate::privileged::net::slots::MAX_SLOTS;",
        ),
        (
            NET_SAVE,
            "pub const SNAPSHOT_FD_COUNT: usize = crate::privileged::net::slots::MAX_SLOTS;",
        ),
    ] {
        assert!(
            read(path).contains(needle),
            "{path} 必须从 net::slots::MAX_SLOTS 派生容量"
        );
    }
    // 用户可调容量与槽位单源保持同值 (改容量须两处同步)
    assert!(
        read(NET_SOCKETS).contains("const MAX_SOCKETS: usize = 256;"),
        "MAX_SOCKETS 必须与 slots::MAX_SLOTS 同值 (256)"
    );
}
