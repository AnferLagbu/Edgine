//! framework spin::Mutex → IrqSpinLock 迁移契约测试 (P1-I-17)
//!
//! 验证:
//! 1. 全 framework 模块 `use spin::Mutex` 计数 = 0
//! 2. 全 framework 模块 `spin::Mutex<` (内联) 计数 = 0
//! 3. 全 framework 模块 `spin::Once` 计数 = 0 (改用 OnceLock)
//! 4. 自研 IrqSpinLock 替换完毕
//! 5. 源码静态文本扫描: 各模块必须从 framework::sync::irq_spinlock 导入 IrqSpinLock
//!
//! 主机端测试: 验证源码静态契约. 真实锁替换由编译期保证 (类型签名兼容).

use std::fs;
use std::path::Path;

fn read_src(rel: &str) -> String {
    // host-tests/CARGO_MANIFEST_DIR = <workspace>/host-tests
    // 目标在 <workspace>/src/kernel/framework/...
    // 用 CARGO_MANIFEST_DIR 动态计算, 避免硬编码仓库绝对路径
    let path = format!(
        "{}/../src/kernel/framework/{}",
        env!("CARGO_MANIFEST_DIR"),
        rel
    );
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path, e))
}

fn framework_root() -> String {
    format!("{}/../src/kernel/framework", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn no_use_spin_mutex_in_framework() {
    // P1-I-17 验收: 框架内不再有 use spin::Mutex;
    let root = framework_root();
    let mut offenders = Vec::new();
    walk_rs(&root, &mut |path| {
        if let Ok(content) = fs::read_to_string(path) {
            for (lineno, line) in content.lines().enumerate() {
                if line.contains("use spin::Mutex;") {
                    offenders.push(format!("{}:{}", path, lineno + 1));
                }
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "P1-I-17: 以下位置仍使用第三方 spin::Mutex;\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn no_inline_spin_mutex_in_framework() {
    // P1-I-17 验收: 框架内不再有内联 spin::Mutex<
    let root = framework_root();
    let mut offenders = Vec::new();
    walk_rs(&root, &mut |path| {
        if let Ok(content) = fs::read_to_string(path) {
            for (lineno, line) in content.lines().enumerate() {
                if line.contains("spin::Mutex<") {
                    offenders.push(format!("{}:{}", path, lineno + 1));
                }
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "P1-I-17: 以下位置仍内联使用 spin::Mutex<...\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn no_spin_once_in_framework() {
    // P1-I-17 验收: 框架内不再用 spin::Once (改用 OnceLock)
    let root = framework_root();
    let mut offenders = Vec::new();
    walk_rs(&root, &mut |path| {
        if let Ok(content) = fs::read_to_string(path) {
            for (lineno, line) in content.lines().enumerate() {
                // 注释中允许出现, 排除注释行
                let trimmed = line.trim();
                if trimmed.starts_with("//") || trimmed.starts_with("///") {
                    continue;
                }
                if line.contains("spin::Once") {
                    offenders.push(format!("{}:{}", path, lineno + 1));
                }
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "P1-I-17: 以下位置仍使用 spin::Once\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn irq_spinlock_adopted_in_migrated_files() {
    // P1-I-17 验收: 迁移模块必须从 framework 路径导入 IrqSpinLock
    let files = [
        "driver/kexec.rs",
        "driver/uefi.rs",
        "timer/time_sync.rs",
        "timer/tickless.rs",
        "arch/shadow_stack.rs",
        "sgeg/secure_boot.rs",
        "debug/ebpf.rs",
        "proc/process.rs",
        // cgroup/namespace/seccomp/io_uring 已迁移到 services 层, framework 仅 re-export
        // driver/power.rs, mm/numa.rs 不使用 IrqSpinLock
    ];
    for f in files {
        let content = read_src(f);
        assert!(
            content.contains("use crate::framework::sync::IrqSpinLock")
                || content.contains("use crate::framework::sync::IrqSpinLock as Mutex",)
                || content.contains("use crate::framework::sync::irq_spinlock::IrqSpinLock")
                || content
                    .contains("use crate::framework::sync::irq_spinlock::IrqSpinLock as Mutex",),
            "P1-I-17: {} 必须从 framework 路径导入 IrqSpinLock",
            f
        );
    }
}

#[test]
fn cgroup_uses_framework_once_lock() {
    // P1-I-17 验收: cgroup 改用 OnceLock (项目自研) 替代 spin::Once
    // (DECISION-J 2026-09-13: cgroup 反转迁回 framework/proc/cgroup.rs)
    let path = format!(
        "{}/../src/kernel/framework/proc/cgroup.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    let content = fs::read_to_string(&path).expect("read framework/proc/cgroup.rs");
    assert!(
        content.contains("OnceLock"),
        "P1-I-17: cgroup.rs 必须用 OnceLock"
    );
    // 不再使用 call_once
    assert!(
        !content.contains("call_once"),
        "P1-I-17: cgroup.rs 必改用 get_or_init"
    );
}

#[test]
fn irq_spinlock_exposes_lockdep_named() {
    // P1-I-17 验收: IrqSpinLock 必暴露 named(name, data) (项目自研锁的核心契约)
    let path = format!(
        "{}/../src/kernel/framework/sync/irq_spinlock.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    let content = fs::read_to_string(&path).expect("read irq_spinlock.rs");
    assert!(
        content.contains("pub fn named"),
        "P1-I-17: IrqSpinLock 必须有 named() 方法 (lockdep 集成入口)"
    );
    assert!(
        content.contains("register_class"),
        "P1-I-17: IrqSpinLock 必须在 lockdep 注册"
    );
}

fn walk_rs<F: FnMut(&str)>(dir: &str, f: &mut F) {
    let path = Path::new(dir);
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk_rs(p.to_str().unwrap(), f);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                f(p.to_str().unwrap());
            }
        }
    }
}
