// SPDX-License-Identifier: MPL-2.0
// A-2: lockdep 门控掩码统一契约测试.
//
// 验收:
//   - 五把同步原语的 lockdep 链路一律用 `any(debug_assertions, feature = "lockdep")`
//     门控 —— 只写 `debug_assertions` 的锁在 release + `KERNEL_FEATURES=lockdep`
//     构建里静默不跟踪, 锁序图缺边 (实测 RwLock / PiMutex 曾整链如此)
//   - `named()` 的两种构建分支都由统一掩码门控 (缺一半即某些构建拿不到该类)
//   - `lockdep` feature 从 kernel 包单点转发到 privileged 包
//
// 全部为源码契约测试: 被测面是 cfg 掩码形态, 只在真裸机构建里才有语义,
// host 侧无法用运行期断言覆盖 (与 privileged_spinlock_migration_test 同口径).

use std::fs;

/// 参与 lockdep 跟踪的同步原语实现文件.
const SYNC_LOCKS: [&str; 5] = [
    "../src/kernel/privileged/sync/spinlock.rs",
    "../src/kernel/privileged/sync/irq_spinlock.rs",
    "../src/kernel/privileged/sync/mutex.rs",
    "../src/kernel/privileged/sync/rwlock.rs",
    "../src/kernel/privileged/sync/pi_mutex.rs",
];
const KERNEL_MANIFEST: &str = "../src/kernel/Cargo.toml";
const PRIVILEGED_MANIFEST: &str = "../src/kernel/privileged/Cargo.toml";

const UNIFIED_ON: &str = r#"#[cfg(any(debug_assertions, feature = "lockdep"))]"#;
const UNIFIED_OFF: &str = r#"#[cfg(not(any(debug_assertions, feature = "lockdep")))]"#;
const BARE_ON: &str = "#[cfg(debug_assertions)]";
const BARE_OFF: &str = "#[cfg(not(debug_assertions))]";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 取 cfg 门所管辖的构造: 跳过空行与注释, 返回紧随其后的第一行代码.
fn gated_item(lines: &[&str], idx: usize) -> String {
    lines[idx + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|t| !t.is_empty() && !t.starts_with("//"))
        .unwrap_or_default()
        .to_string()
}

/// lockdep 链路的标志: `named()` 的存在理由就是注册锁类, 故一并计入.
fn is_lockdep_item(item: &str) -> bool {
    ["lockdep", "LockClass", "LockKind", "fn named("]
        .iter()
        .any(|token| item.contains(token))
}

#[test]
fn sync_locks_gate_lockdep_with_unified_mask() {
    for path in SYNC_LOCKS {
        let src = read(path);
        let lines: Vec<&str> = src.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t != BARE_ON && t != BARE_OFF {
                continue;
            }
            let item = gated_item(&lines, idx);
            assert!(
                !is_lockdep_item(&item),
                "{path}:{} 用裸 `{t}` 门控 lockdep 链路 (`{item}`); \
                 必须改成 any(debug_assertions, feature = \"lockdep\"), \
                 否则 release + KERNEL_FEATURES=lockdep 构建静默不跟踪这把锁",
                idx + 1,
            );
        }
        assert!(
            src.contains(UNIFIED_ON),
            "{path}: 缺统一掩码 —— lockdep 门控面已被整体拆除"
        );
    }
}

#[test]
fn named_ctor_branches_use_unified_mask() {
    for path in SYNC_LOCKS {
        let src = read(path);
        let lines: Vec<&str> = src.lines().collect();
        let mut defined = 0usize;
        for (idx, line) in lines.iter().enumerate() {
            let t = line.trim();
            if !(t.starts_with("pub fn named(") || t.starts_with("pub const fn named(")) {
                continue;
            }
            defined += 1;
            let gate = lines[..idx]
                .iter()
                .rev()
                .map(|l| l.trim())
                .find(|l| l.starts_with("#[cfg("))
                .unwrap_or_else(|| panic!("{path}:{} 的 named() 没有 cfg 门", idx + 1));
            assert!(
                gate == UNIFIED_ON || gate == UNIFIED_OFF,
                "{path}:{} 的 named() 由 `{gate}` 门控, 未统一到 \
                 any(debug_assertions, feature = \"lockdep\")",
                idx + 1,
            );
        }
        if defined > 1 {
            assert!(
                src.contains(UNIFIED_OFF),
                "{path}: named() 有多个分支但缺掩码关闭侧, 默认构建会少一个同名构造"
            );
        }
    }
}

#[test]
fn lockdep_feature_forwards_from_kernel_to_privileged() {
    let kernel = read(KERNEL_MANIFEST);
    assert!(
        kernel.contains(r#"lockdep = ["kernel-privileged/lockdep"]"#),
        "kernel 包的 lockdep 必须转发到 privileged 包, 否则 KERNEL_FEATURES=lockdep \
         只影响外层 crate, 同步原语层拿不到该 cfg"
    );
    let privileged = read(PRIVILEGED_MANIFEST);
    assert!(
        privileged.contains("\nlockdep = []\n"),
        "privileged 包必须声明 lockdep feature (空依赖即纯 cfg 开关)"
    );
}
