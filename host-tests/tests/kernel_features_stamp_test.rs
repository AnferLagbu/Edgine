// SPDX-License-Identifier: MPL-2.0
// KERNEL_FEATURES 构建戳记契约测试 (构建系统面).
//
// 缺陷 (实测): 原实现的「值是否变化」判定写在 Makefile 解析期的 `$(shell ...)` 里,
// 于是**任何**一次 make 调用 —— 哪怕目标与构建无关 (`make test-host`) 或只是 `-n`
// 干跑 —— 都在读取 Makefile 时把戳记改写, 消费掉「值已变化」这一信号; 随后的构建腿
// 看到未变化 ⇒ 不把戳记挂成 `$(RUST_LIB)` 的前置 ⇒ cargo 不重跑.
// 后果: `make KERNEL_FEATURES=lockdep all` 之后 `./ci/build.sh all` 打印
// "Passed: 6 / Failed: 0", 而 `kernel.flat` 仍是 lockdep 版 (strings 探针 circular=1,
// 字节数与 feature 构建一致) —— 门禁的「跑完复原默认构建」整条失效, 后续 QEMU 腿
// 跑的其实是特性镜像.
//
// 修复后的判据 (本文件逐条锁住):
//   - 解析期零副作用: 干跑不改写戳记内容与 mtime
//   - 值变化必须由**规则级**内容比较触发重写, 且重写后的戳记新于下游目标
//   - 值不变则不重写 (增量构建语义保持)
//   - 默认目标仍是构建镜像 (戳记规则提前到文件首条规则时, `make` 无参会退化成只写戳记)
//   - Makefile 文本面: 解析期判定符号已消失, 三处前置恒挂戳记

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const MAKEFILE: &str = "../Makefile";
const STAMP_NAME: &str = ".kernel_features";

/// 每用例独立临时构建目录 (含进程号 + 用例标签, 并行不互踩).
fn work_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edgine_stamp_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    dir
}

/// `make -C .. -s ARCH_BUILD_DIR=<dir> [KEY=VAL...] targets...`
///
/// 返回 (退出码, stdout+stderr). 命令行的 `ARCH_BUILD_DIR=` 覆盖 Makefile 的 `:=`,
/// 故戳记与全部下游路径都落进临时目录 —— 不触碰真构建产物, 也不动仓内戳记.
fn make(dir: &Path, extra: &[&str], targets: &[&str]) -> (i32, String) {
    let stamp_arg = format!("ARCH_BUILD_DIR={}", dir.display());
    let mut cmd = Command::new("make");
    cmd.args(["-C", "..", "-s", &stamp_arg]);
    cmd.args(extra);
    cmd.args(targets);
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("spawn make {targets:?}: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

/// 跑真正的戳记规则 (内容比较 + 必要时重写), 返回戳记路径.
fn run_stamp_rule(dir: &Path, features: &str) -> PathBuf {
    let key = format!("KERNEL_FEATURES={features}");
    let stamp = dir.join(STAMP_NAME);
    let (code, text) = make(dir, &[&key], &[stamp.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "戳记规则执行失败 (KERNEL_FEATURES={features}): {text}"
    );
    assert!(stamp.exists(), "戳记规则没有产出 {}", stamp.display());
    stamp
}

fn stamp_content(dir: &Path) -> String {
    fs::read_to_string(dir.join(STAMP_NAME)).unwrap_or_else(|e| panic!("read stamp: {e}"))
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// 解析期零副作用: 干跑 (与真构建同一次解析) 不得改写戳记.
///
/// 这一条即原缺陷的正中面 —— 原实现下 `-n` 也执行解析期 `$(shell)`, 戳记当场被改写.
#[test]
fn dry_run_does_not_rewrite_stamp() {
    let dir = work_dir("dryrun");
    let stamp = run_stamp_rule(&dir, "lockdep");
    assert_eq!(stamp_content(&dir), "lockdep");
    let before = mtime(&stamp);

    // 换成默认 (空) 值做干跑: 若判定仍在解析期, 这一步就把戳记改写成空
    let (code, text) = make(&dir, &["-n", "KERNEL_FEATURES="], &["all"]);
    assert_eq!(code, 0, "干跑 make -n all 失败: {text}");
    assert_eq!(
        stamp_content(&dir),
        "lockdep",
        "干跑改写戳记内容 —— 解析期 side effect 又回来了, 随后的构建腿会复用错误 feature 的制品"
    );
    assert_eq!(
        mtime(&stamp),
        before,
        "干跑触碰戳记 mtime —— 下游目标会被无谓判定为过期 (增量语义丢失)"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 值变化 ⇒ 规则重写戳记 ⇒ 戳记新于下游目标 (这条边正是 cargo 重跑的凭据).
#[test]
fn feature_change_rewrites_stamp_newer_than_target() {
    let dir = work_dir("change");
    run_stamp_rule(&dir, "lockdep");
    // 假想的上一次构建产物: 比戳记新, 单独看是「无需重编」
    let target = dir.join("libkernel.a");
    fs::write(&target, b"stale-lockdep-artifact").unwrap();
    std::thread::sleep(Duration::from_millis(1_100));

    let stamp = run_stamp_rule(&dir, "");
    assert_eq!(
        stamp_content(&dir),
        "",
        "值变化后戳记内容未跟上新 KERNEL_FEATURES"
    );
    assert!(
        mtime(&stamp) > mtime(&target),
        "重写后的戳记不比下游目标新 —— 特性切换仍会静默沿用旧制品"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 值不变 ⇒ 不重写 ⇒ mtime 保持 (增量构建行为与改造前一致).
#[test]
fn same_feature_keeps_stamp_mtime() {
    let dir = work_dir("same");
    let stamp = run_stamp_rule(&dir, "lockdep");
    let before = mtime(&stamp);
    std::thread::sleep(Duration::from_millis(1_100));

    let again = run_stamp_rule(&dir, "lockdep");
    assert_eq!(
        mtime(&again),
        before,
        "同值重复执行改写了戳记 —— 每次 make 都会连带重编内核库 (增量语义丢失)"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 默认目标必须仍是镜像链, 而不是文件里最先出现的那条规则 (戳记).
#[test]
fn default_goal_still_builds_image() {
    let dir = work_dir("goal");
    let (code, text) = make(&dir, &["-n", "KERNEL_FEATURES="], &[]);
    assert_eq!(code, 0, "裸 `make -n` 失败: {text}");
    assert!(
        text.contains("boot.asm") || text.contains("stage1"),
        "裸 `make -n` 的默认目标不再走镜像链 (输出: {}) —— \
         外部脚本的无参 make 会只写戳记不产镜像",
        text.lines().next().unwrap_or("<空>")
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Makefile 文本面: 解析期判定已退役, 戳记恒为三处下游前置.
#[test]
fn makefile_gates_rebuild_by_rule_not_parse() {
    let src = fs::read_to_string(MAKEFILE).unwrap_or_else(|e| panic!("read {MAKEFILE}: {e}"));
    assert!(
        !src.contains("KERNEL_FEATURES_CHANGED"),
        "{MAKEFILE} 又出现 KERNEL_FEATURES_CHANGED —— 解析期判定回归, \
         第一次 make 消费掉变化信号后构建腿会复用旧 feature 制品"
    );
    let prereq_sites = src
        .lines()
        .filter(|l| {
            l.contains("$(KERNEL_FEATURES_STAMP)") && !l.starts_with("KERNEL_FEATURES_STAMP")
        })
        .count();
    assert!(
        prereq_sites >= 4,
        "戳记作为前置/规则的挂点只有 {prereq_sites} 处 (期望 >= 4: 戳记规则自身 + \
         kernel.bin + 两架构 $(RUST_LIB)), 说明某条构建腿没挂上戳记边"
    );
    assert!(
        !src.contains("$(if $(KERNEL_FEATURES_CHANGED)"),
        "{MAKEFILE} 仍有条件挂前置的写法 —— 戳记边必须是恒挂, 由规则自己做内容比较"
    );
    assert!(
        src.contains(".DEFAULT_GOAL := all"),
        "{MAKEFILE} 缺显式默认目标 —— 戳记规则位于文件首条规则时 `make` 无参会退化成只写戳记"
    );
}
