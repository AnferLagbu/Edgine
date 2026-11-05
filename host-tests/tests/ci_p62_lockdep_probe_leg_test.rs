// SPDX-License-Identifier: MPL-2.0
// P-62 #125 回归锁: lockdep 构建面探针腿的**接线**契约.
//
// 背景:
//   lockdep 是默认关闭的诊断 feature, 且不在启动日志 capabilities 行里, 所以它的
//   运行维判据取构建面字符串双向探针 (`KERNEL_FEATURES=lockdep` 构建含
//   "circular dependency" 恰 1 段 / 默认构建 0 段), 由
//   `scripts/lockdep_feature_probe.sh` 在 full 档真构建真计数.
//
//   本文件**不**重复真构建 (那太贵且已由门禁承担), 只钉五处接线: 探针串与源码同源、
//   双向判据都在、两架构产物都在射程、audit 真调用且失败即 err、feature 转发链在位.
//   任一处被改坏后, 那条腿会静默空转或缩面 —— full 档读数照样绿.
//
// 验收:
//   1. 探针串取自 lockdep.rs 的检测文案, 脚本默认值与它逐字一致 (改名即 fail,
//      防止判据与被测对象漂移).
//   2. 正向期望 1 / 反向期望 0 各自参与比较, 且反向比较发生在"复原默认构建"之后
//      (否则读到的是 feature 镜像, 反向恒绿是假的).
//   3. 两架构各自的**真启动产物**都在射程内 (x86_64 `kernel.flat` /
//      aarch64 `kernel-aarch64.img`, 二者名称不同形).
//   4. `ci/audit.sh` 的 full 档步骤 4 真调用本脚本, 失败走 `err` (该脚本里 err 即
//      exit 1, 判红不会被吞).
//   5. `lockdep` feature 在 kernel → privileged 两级 Cargo 清单里逐项转发在位.

use std::fs;

const PROBE_SCRIPT: &str = "../scripts/lockdep_feature_probe.sh";
const AUDIT_SCRIPT: &str = "../ci/audit.sh";
const KERNEL_MANIFEST: &str = "../src/kernel/Cargo.toml";
const PRIVILEGED_MANIFEST: &str = "../src/kernel/privileged/Cargo.toml";
const LOCKDEP_SRC: &str = "../src/kernel/privileged/sync/lockdep.rs";

/// 探针串: lockdep AB-BA 检测文案的固定片段, 判据与被测对象共用同一字面量.
const MARKER: &str = "circular dependency";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 断言 `a` 在 `b` 之前出现 (两处都必须存在, 缺任一处即 fail).
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay.find(a).unwrap_or_else(|| panic!("{a:?} 未找到"));
    let ib = hay.find(b).unwrap_or_else(|| panic!("{b:?} 未找到"));
    assert!(ia < ib, "{a:?} 必须出现在 {b:?} 之前");
}

/// 1. 探针串与 lockdep 源码同源, 且脚本默认值逐字一致.
#[test]
fn test_probe_marker_is_derived_from_lockdep_source() {
    let lockdep = read(LOCKDEP_SRC);
    assert!(
        lockdep.contains(MARKER),
        "lockdep.rs 的检测文案不再含探针串 {MARKER:?} —— 判据已与被测对象脱钩, \
         须同步改 scripts/lockdep_feature_probe.sh 的探针串"
    );
    let script = read(PROBE_SCRIPT);
    assert!(
        script.contains(&format!("LOCKDEP_PROBE_MARKER:-{MARKER}")),
        "脚本默认探针串必须与 lockdep.rs 的字面量逐字一致"
    );
}

/// 2. 双向都判: 正向期望 1、反向期望 0, 且反向比较在复原默认构建之后.
#[test]
fn test_probe_judges_both_directions() {
    let script = read(PROBE_SCRIPT);
    assert!(
        script.contains("EXPECT_ON=\"${LOCKDEP_PROBE_ON:-1}\""),
        "正向期望段数必须是 1 (恰一段, 不是「>=1」)"
    );
    assert!(
        script.contains("EXPECT_OFF=\"${LOCKDEP_PROBE_OFF:-0}\""),
        "反向期望段数必须是 0 (默认构建不含探针串)"
    );
    assert!(
        script.contains("\"$on\" = \"$EXPECT_ON\""),
        "正向读数必须与期望比较并判红"
    );
    assert!(
        script.contains("\"$off\" = \"$EXPECT_OFF\""),
        "反向读数必须与期望比较并判红 —— 只判正向时探针串与 feature 脱钩不会红"
    );
    // 反向必须在"复原默认构建"之后取数, 否则读的还是 feature 镜像.
    assert_before(
        &script,
        "make ARCH=\"$arch\" all >",
        "\"$off\" = \"$EXPECT_OFF\"",
    );
}

/// 3. 两架构真启动产物都在射程内 (名称不同形, 不可共用一条路径).
#[test]
fn test_probe_targets_both_arch_boot_images() {
    let script = read(PROBE_SCRIPT);
    assert!(
        script.contains("other/build/x86_64/kernel.flat"),
        "x86_64 判据必须落在真被 -kernel 加载的 kernel.flat"
    );
    assert!(
        script.contains("other/build/aarch64/kernel-aarch64.img"),
        "aarch64 判据必须落在真被 -kernel 加载的 kernel-aarch64.img (该架构无 kernel.flat)"
    );
    assert!(
        script.contains("ARCH_LIST=\"x86_64 aarch64\""),
        "缺省参数必须是双架构, 单架构形态会让另一侧静默失守"
    );
    // 架构自检 (S-6 防线③) 必须在下判据之前: strings 判据与架构无关, 异架构顶替
    // 会让正向照样计到 1.
    assert_before(
        &script,
        "verify_image_arch.py --arch",
        "\"$on\" = \"$EXPECT_ON\"",
    );
}

/// 4. full 档步骤 4 真调用本脚本, 且失败即 err.
#[test]
fn test_audit_full_invokes_probe_leg_with_err_on_fail() {
    let audit = read(AUDIT_SCRIPT);
    assert!(
        audit.contains("lockdep_feature_probe.sh\" all"),
        "ci/audit.sh 必须以 all (双架构) 调用探针脚本 —— 缺调用即本腿空转"
    );
    assert_eq!(
        audit.matches("lockdep_feature_probe.sh").count(),
        2,
        "本腿在 audit 里应只有「注释点名脚本 + 真调用」两处, 实得 {} 处",
        audit.matches("lockdep_feature_probe.sh").count()
    );
    assert_before(
        &audit,
        "step \"4/6 QEMU 双架构真实启动测试\"",
        "lockdep_feature_probe",
    );
    assert!(
        audit.contains("err \"lockdep 构建面探针失败"),
        "本腿失败必须走 err (audit.sh 的 err 内建 exit 1, 判红不被吞)"
    );
    assert!(
        audit.contains("audit_lockdep_probe.log"),
        "本腿必须有独立取证日志 —— 偶发红要靠真构建读数归因"
    );
}

/// 5. lockdep feature 两级转发在位 (本腿判定的对象本身).
#[test]
fn test_lockdep_feature_forwarding_chain_present() {
    let kernel = read(KERNEL_MANIFEST);
    assert!(
        kernel.contains("lockdep = [\"kernel-privileged/lockdep\"]"),
        "kernel 清单必须把 lockdep 转发给 privileged (P-71: 未转发的 feature 在子 \
         crate 恒关闭, cfg 分支静默消失 —— 正是本腿要抓的断链形态)"
    );
    let privileged = read(PRIVILEGED_MANIFEST);
    assert!(
        privileged.contains("lockdep = []"),
        "privileged 清单必须声明 lockdep 本体"
    );
}
