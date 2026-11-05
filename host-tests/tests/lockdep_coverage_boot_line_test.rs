// SPDX-License-Identifier: MPL-2.0
//! A-127d 回归: lockdep 覆盖面读数必须有真实消费者.
//!
//! 缺陷形态 (A-1 前置 ②): `deadlock_detected()` / `num_violations()` 与 A-127b 新增的
//! 两个粘滞失效读数 (`class_table_dropped` / `held_stack_overflowed`) 在内核里长期零
//! 消费者 —— 启动路径真触发锁序违规、或真丢了检测覆盖, 门禁面毫无感知, 关于它们的文案
//! 也无从对账. 用户裁定取「接一个真实消费者」而非实现 panic.
//!
//! 修复形态: 内核侧在进用户态前的最后一点 (`boot_syscall_and_launch`, 晚于 canary 复核、
//! 早于 `launch_first_user_process`) 打一行 `[LOCKDEP] coverage …` 读数, 由
//! `scripts/qemu_boot_test.sh` 双架构分支以四项硬零判据消费. 本文件锁定该接线形状.
//!
//! 判据面 (全部为源码契约, 与 `lockdep_low_overhead_test` 同口径):
//! ① 读数行取到覆盖面判据的全部读数 (四项查询经 `functions::sync::lockdep` 安全代理 +
//!    两个粘滞失效读数), 且不走 `lockdep_log!`
//!    (那条宏的「恰 6 处」报告串契约属 A-127b, 不能被启动读数行污染);
//! ② 打印点确在进 Ring 3 前的最后一段, 不是随便挑的早期点位;
//! ③ 消费腿双架构各一次且非零即 `RESULT=1` (不受 `FAIL_OK` 放宽);
//! ④ 参与硬零判定的字段只有四项 —— `classes=` 与两个记账计数不参与 (默认 release 构建
//!    门控关闭时 classes 恒为 0, lockdep/debug 构建 > 0, 两种形态共用同一判据).

use std::fs;
use std::path::PathBuf;

const LIB: &str = "src/kernel/lib.rs";
const QEMU: &str = "scripts/qemu_boot_test.sh";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

/// 空白不敏感 (rustfmt 会折行长参数列表与长字符串)
fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn has(hay: &str, needle: &str) -> bool {
    norm(hay).contains(&norm(needle))
}

/// `first` 末次出现早于 `second` 首次出现.
fn before(hay: &str, first: &str, second: &str) -> bool {
    let hay = norm(hay);
    let (Some(f), Some(s)) = (hay.rfind(&norm(first)), hay.find(&norm(second))) else {
        return false;
    };
    f < s
}

/// 取 `start` 首次出现到其后 `end` 首次出现之间的源码片段.
fn section<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
    let start_pos = src
        .find(start)
        .unwrap_or_else(|| panic!("未找到起点: {start}"));
    let end_rel = src[start_pos..]
        .find(end)
        .unwrap_or_else(|| panic!("未找到终点: {end}"));
    &src[start_pos..start_pos + end_rel]
}

/// ①: 读数行必须把覆盖面判据的七个读数全部取出来.
#[test]
fn coverage_line_reports_every_lockdep_reading() {
    let lib = read_src(LIB);
    let body = section(&lib, "fn report_lockdep_coverage()", "\n}\n");
    for needle in [
        "crate::functions::sync::lockdep",
        "lockdep::num_classes()",
        "lockdep::num_violations()",
        "lockdep::deadlock_detected()",
        "lockdep::stats()",
        "stats.acquire_ops",
        "stats.new_edges",
        "stats.class_table_dropped",
        "stats.held_stack_overflowed",
    ] {
        assert!(has(body, needle), "读数行缺 {needle} —— 覆盖面判据少一项");
    }
    assert!(
        !body.contains("privileged::sync::"),
        "读数行走 functions 安全代理 (ref-lock-order.md 记的即该路径), 不再直插 privileged 内部"
    );
    for field in [
        "classes=",
        "violations=",
        "detected=",
        "acquire_ops=",
        "new_edges=",
        "class_table_dropped=",
        "held_stack_overflowed=",
    ] {
        assert!(
            body.contains(field),
            "读数行格式串缺字段 `{field}` —— 门禁按字段名取值, 少写即读成缺失"
        );
    }
    assert!(
        body.contains("[LOCKDEP] coverage"),
        "读数行标签必须是 `[LOCKDEP] coverage` —— 与 qemu_boot_test.sh 的取值正则同源"
    );
    assert!(
        has(body, "crate::klog_boot_info!"),
        "读数行走 klog_boot_info (启动段), 不得换成别的出口"
    );
    assert!(
        !body.contains("lockdep_log!"),
        "读数行不得复用 lockdep_log! —— 那条宏的「报告串恰 6 处」是 A-127b 的契约"
    );
}

/// ② + 门控: 打印点在进 Ring 3 前的最后一段, 且与启动路径同门控.
#[test]
fn coverage_line_is_emitted_at_the_last_point_before_el0() {
    let lib = read_src(LIB);
    let boot = section(&lib, "fn boot_syscall_and_launch()", "\n}\n");
    assert!(
        has(boot, "report_lockdep_coverage();"),
        "boot_syscall_and_launch 未调用 report_lockdep_coverage —— 读数行没有发射点"
    );
    assert!(
        before(
            boot,
            "check_boot_stack_canary()",
            "report_lockdep_coverage();"
        ),
        "读数行必须晚于 canary 复核 (启动末段), 否则前面还有子系统未初始化"
    );
    assert!(
        before(
            boot,
            "report_lockdep_coverage();",
            "launch_first_user_process();"
        ),
        "读数行必须早于 launch_first_user_process —— 进 EL0 后内核不再回到该函数"
    );
    assert!(
        has(
            &lib,
            "#[cfg(not(any(feature = \"kernel_test\", feature = \"host-test\")))] fn report_lockdep_coverage()"
        ),
        "读数函数必须与启动路径同门控: kernel_test / host-test 构建里没有真启动段"
    );
}

/// ③: 消费腿双架构各一次, 非零即硬失败.
#[test]
fn qemu_gate_consumes_the_line_on_both_arches() {
    let qemu = read_src(QEMU);
    assert!(
        has(
            &qemu,
            "lockdep_coverage_check \"x86_64\" \"$X64_LOG\" || RESULT=1"
        ),
        "x86_64 分支未接 lockdep_coverage_check 或未硬失败"
    );
    assert!(
        has(
            &qemu,
            "lockdep_coverage_check \"aarch64\" \"$A64_LOG\" || RESULT=1"
        ),
        "aarch64 分支未接 lockdep_coverage_check 或未硬失败"
    );
    assert!(
        before(
            &qemu,
            "if boot_and_check \"x86_64\"",
            "lockdep_coverage_check \"x86_64\""
        ),
        "x86_64 判据必须落在启动成功分支内 (日志未生成即无从取值)"
    );
    assert!(
        before(
            &qemu,
            "if boot_and_check \"aarch64\"",
            "lockdep_coverage_check \"aarch64\""
        ),
        "aarch64 判据必须落在启动成功分支内"
    );
}

/// ④: 硬零判定只取四项, 且取值口径与内核侧字段名一致.
#[test]
fn hard_zero_criteria_covers_exactly_four_fields() {
    let qemu = read_src(QEMU);
    let check = section(&qemu, "lockdep_coverage_check() {", "\n}\n");
    assert!(
        has(
            check,
            "\\[LOCKDEP\\] coverage classes=[0-9]+ violations=[0-9]+"
        ),
        "取值正则与内核侧格式串脱钩 —— 读数行改格式时本腿必须一起改"
    );
    assert!(
        has(
            check,
            "for field in detected violations class_table_dropped held_stack_overflowed; do"
        ),
        "硬零判定的字段清单必须恰为四项 (classes 与两个记账计数不参与判定)"
    );
    assert!(
        !has(check, "for field in detected violations classes"),
        "classes 被拉进硬零判定 —— 默认 release 构建 classes=0、lockdep 构建 > 0, \
         同判据无法两形态共用"
    );
    assert!(
        has(check, "want=\"false\""),
        "detected 的合格值必须是 false"
    );
    assert!(
        has(check, "未观察到 lockdep 覆盖面读数行"),
        "读数行缺失时必须给出可定位的 A-127d 文案"
    );
}
