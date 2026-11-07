// SPDX-License-Identifier: MPL-2.0
// M7 合格线回归锁: lockdep 开销 A/B harness 的判定契约 (2026-11-07 裁定).
//
// 背景:
//   `scripts/lockdep_overhead_ab.sh` 是 A-1「lockdep 常开」与 M7 低开销化的读数来源,
//   此前它只保证「测量可信」而不判 r 的下界 (合格线待裁定). 2026-11-07 裁定把合格线
//   钉在 `median(r_i) ≤ 1.10`、统计量下限 5 对配对读数, 并要求报数同时给绝对差与
//   guest 内核时标核对. 本文件钉住这个判定的**形状**, 不重复真构建真启动 (那是脚本
//   自己的事, 也从不接入 `ci/audit.sh`).
//
//   为什么值得加锁: 合格线一旦只写在文档里, 后来人把判定改成均值、把退出码并档、或
//   在配对不足时仍然判绿, 文档一个字都不会变红 —— 而这三种改法都能让"读数看起来更好".
//
// 验收:
//   1. 合格线与统计量下限都是具名环境变量的默认值 (1.10 / 5), 不是散落在比较式里的
//      字面量.
//   2. 判定只取 r 的中位, 单轮 r_i 不参与比较.
//   3. 配对数不足即"判据不可用", 且该分支既不判合格也不判超线.
//   4. 超线用退出码 2, 与"测不出"的 1 分档 —— 全脚本只有这一处 2.
//   5. guest 内核时标与绝对差都在报数面上, 但时标不进判定 (判定只有 r 一个入口).
//   6. harness 仍未接入 `ci/audit.sh` (接入属门禁改动, 未授权).

use std::fs;

const HARNESS: &str = "../scripts/lockdep_overhead_ab.sh";
const AUDIT_SCRIPT: &str = "../ci/audit.sh";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// 断言 `a` 在 `b` 之前出现 (两处都必须存在, 缺任一处即 fail).
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay.find(a).unwrap_or_else(|| panic!("{a:?} 未找到"));
    let ib = hay.find(b).unwrap_or_else(|| panic!("{b:?} 未找到"));
    assert!(ia < ib, "{a:?} 必须出现在 {b:?} 之前");
}

/// 1. 合格线与下限都是具名默认值.
#[test]
fn test_line_and_pair_floor_are_named_defaults() {
    let script = read(HARNESS);
    assert!(
        script.contains(r#"R_MAX="${LOCKDEP_AB_R_MAX:-1.10}""#),
        "合格线默认值不再是 1.10 —— 改数值须先经用户裁定, 并同步本锁与台账"
    );
    assert!(
        script.contains(r#"MIN_PAIRS="${LOCKDEP_AB_MIN_PAIRS:-5}""#),
        "统计量下限不再是 5 对 —— 裁定原文是 ≥5 对配对的 median(r_i)"
    );
}

/// 2. 判定只用 r 的中位, 单轮 `r_i` 不得成为比较对象.
#[test]
fn test_verdict_uses_the_median_not_a_single_round() {
    let script = read(HARNESS);
    assert!(
        script.contains(r#"R_MED="$(median "${R_VALUES[@]}")""#),
        "median(r_i) 不再是 r 序列的中位 —— 换成均值/最小值会把离群轮读成结论"
    );
    assert!(
        script.contains(r#"if awk -v m="$R_MED" -v lim="$R_MAX""#),
        "合格判定不再拿中位去比合格线"
    );
    assert_eq!(
        script.matches(r#"-v lim="$R_MAX""#).count(),
        1,
        "合格线出现了第二个比较点 —— 单轮 r_i 或别的量被拖进了判定"
    );
    assert_eq!(
        script.matches(r#""${R_VALUES[@]}""#).count(),
        1,
        "r 序列被第二处展开成判定量 —— 判定入口应当只有 median(r_i) 一个"
    );
}

/// 3. 配对不足即判据不可用: 该分支先于合格/超线两档, 且自身走红.
#[test]
fn test_insufficient_pairs_produce_no_verdict() {
    let script = read(HARNESS);
    assert!(
        script.contains(r#"if [ "$PAIRS" -lt "$MIN_PAIRS" ]"#),
        "配对数下限检查缺失 —— 一对读数也能判合格"
    );
    assert_before(
        &script,
        r#""$PAIRS" -lt "$MIN_PAIRS""#,
        "median(r_i)=${R_MED} ≤",
    );
    assert!(
        script.contains("判据不可用, 不产出合格/超线结论"),
        "配对不足时的措辞改了 —— 必须是'不产出结论'而不是默认放行"
    );
    // 配置层的同源约束: MIN_PAIRS 大于 ROUNDS 时判据永不成立, 也要 fail-closed.
    assert!(
        script.contains("该组合下判据永不成立"),
        "MIN_PAIRS 大于 ROUNDS 的静默失效未封住 —— 那会让脚本永远判'配对不足'"
    );
}

/// 4. 超线是退出码 2, 与"测不出"的 1 分档; 全脚本只有这一处 2.
#[test]
fn test_over_line_uses_a_distinct_exit_code() {
    let script = read(HARNESS);
    assert_eq!(
        script.matches("exit 2").count(),
        1,
        "退出码 2 被用在了第二个地方 —— 1/2 分档 (测不出 / 真超线) 是本判据的前提"
    );
    assert_before(&script, "超线: median(r_i)", "exit 2");
    assert_before(&script, "合格: ", "exit 0");
    assert!(
        script.contains("与'测不出'的 1 分档"),
        "超线文案不再声明与退出码 1 的分档关系"
    );
}

/// 5. 绝对差与 guest 内核时标都在报数面上, 但时标不进判定.
#[test]
fn test_absolute_delta_and_guest_clock_are_reported_but_not_judged() {
    let script = read(HARNESS);
    assert!(
        script.contains("\"绝对差\""),
        "绝对差一行没了 (裁定②要求与倍率同报)"
    );
    assert!(
        script.contains("guest 中位") && script.contains("guest_stamp()"),
        "guest 内核时标核对没了 —— 墙钟倍率单独一个口径不足以区分宿主噪声与 guest 开销"
    );
    assert!(
        !script.contains(r#"m="$G_"#),
        "guest 时标被拖进了合格判定 —— 判据只有一个入口, 就是 median(r_i)"
    );
}

/// 6. harness 仍未接入 audit full.
#[test]
fn test_harness_is_not_wired_into_the_ci_gate() {
    let audit = read(AUDIT_SCRIPT);
    assert!(
        !audit.contains("lockdep_overhead_ab"),
        "harness 被接进了 `ci/audit.sh` —— 它每趟要真构建两份内核并交错启动 ≥5 对, \
         接成门禁腿属门禁改动, 未经用户裁定不得接入 (接的那天须连同本锁一起改口)"
    );
}
