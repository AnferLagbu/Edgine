// SPDX-License-Identifier: MPL-2.0
// P-79 接线形状锁: 恒真反向针形态腿 (audit_vacuous_needle) 的接线与判据面契约.
//
// 背景:
//   `str::contains` / `matches` / `find` 收的是逐字文本, 不收正则. 把正则写进**反向**
//   断言 (`assert!(!hay.contains(r#"$r_i.*$R_MAX"#))`) 时, 匹配不到正是它想要的"通过"
//   ⇒ 恒真, 永远咬不住任何东西, 而在 CI 日志里与一条真判据完全同形. M7 合格线批的
//   那条针就是靠 clippy 的 `needless_raw_string_hashes` 恰好路过才暴露的.
//   2026-11-07 裁定把它从"靠 lint 巧合"改成"靠门禁判据": 新增 full 档第 10 条存量审计腿
//   `scripts/audit_vacuous_needle.py` + `audit_selftest` 第 10 项负控制.
//
//   本文件**不**重复扫描全仓 (那是门禁的事), 只钉接线与判据面: 腿在清单里、判据九项
//   记号齐备、判白只有内在引号规则一条、fail-closed 三档退出码在位、自测真调检测器.
//
// 写针的纪律 (本腿会扫自己, 故本文件也受它管):
//   **反向** `.contains(` / `.matches(` / `.find(` 的针内**不得**出现纯正则记号
//   (`.*` `.+` `\d` `\s` `\w` `\b` `[0-9]` `{n,m}` `(?:`); 需要拿这类文本作比对时走
//   正向断言 (匹配不到即当场红) 或把字面量经变量传入 (静态不可判即不计).
//
// 验收:
//   1. 腿只进 `AUDIT_REQUIRED_LEGS_FULL` (quick 档不放), 脚本真实存在, 到位断言的总数
//      仍由清单派生而非手写.
//   2. 判据面九项记号逐项在位且总数不扩不缩; 三个逐字 API 齐备; 违规谓词两面齐;
//      判白只有"针内含引号"一条内在规则, 没有人工豁免表也没有 `--strict` 式分流.
//   3. 射程面: 只 `.rs`、含未提交在制品、排除 vendored smoltcp; 注释与字符串被屏蔽;
//      首参非字面量即不计.
//   4. 退出码三档在位 (违规 1 / 扫描失败 2 / 无违规 0), 且 `err` 消费由 2e 循环承担.
//   5. 自测执行点真调 `scan_text` + `violations_of` (P-50 ③ 同型要求), 且口径第 4 条
//      实测的那条判白存量针仍在位.

use std::fs;
use std::path::Path;

const AUDIT_SH: &str = "../ci/audit.sh";
const SCANNER: &str = "../scripts/audit_vacuous_needle.py";
const SELFTEST: &str = "../scripts/tests/audit_selftest.py";
const UNIFIED_TEST: &str = "../host-tests/tests/ci_entrypoint_unified_test.rs";

/// 裁定口径第 3 条的九项记号, 逐项列出 (与脚本 `REGEX_TOKENS` 的名字面一致).
const TOKEN_NAMES: [&str; 9] = [
    ".*", ".+", r"\d", r"\s", r"\w", r"\b", "[0-9]", "{n,m}", "(?:",
];

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 取 `start` 与 `end` 两个锚点之间的片段 (两处缺一即 fail, 不静默给空串).
fn section<'a>(hay: &'a str, start: &str, end: &str) -> &'a str {
    let ia = hay
        .find(start)
        .unwrap_or_else(|| panic!("{start:?} 未找到"));
    let ib = hay.find(end).unwrap_or_else(|| panic!("{end:?} 未找到"));
    assert!(ia < ib, "{start:?} 必须出现在 {end:?} 之前");
    &hay[ia..ib]
}

/// 接线面 (判据 1 与 4): 腿在 full 清单里且只在那里, 脚本存在, 到位断言由清单派生.
#[test]
fn test_vacuous_needle_leg_is_wired_into_full_only() {
    let audit = read(AUDIT_SH);
    assert!(
        Path::new("../scripts/audit_vacuous_needle.py").exists(),
        "脚本本体不在了 —— 清单腿会当场被存在性包装判成 audit_leg_missing"
    );
    let full_line = audit
        .lines()
        .find(|l| l.trim_start().starts_with("AUDIT_REQUIRED_LEGS_FULL="))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 AUDIT_REQUIRED_LEGS_FULL 清单"));
    assert!(
        full_line.contains("vacuous_needle:audit_vacuous_needle.py"),
        "full 档清单里没有本腿 —— 摘腿后恒真反向针退回无人执法 (P-49 的零调用者同族)"
    );
    let quick_line = audit
        .lines()
        .find(|l| l.trim_start().starts_with("AUDIT_REQUIRED_LEGS="))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 AUDIT_REQUIRED_LEGS 清单"));
    assert!(
        !quick_line.contains("vacuous_needle"),
        "本腿被挪进 quick 档 —— 存量审计腿只按裁定在 full 执行, 挪档需先裁口径并同步\
         到位断言的两档互校"
    );
    // 到位总数必须由清单派生 (P-49 裁定: 禁止手写可由脚本派生的数字).
    assert!(
        audit.contains("AUDIT_LEG_FULL_TOTAL=$(set -- $AUDIT_REQUIRED_LEGS_FULL; echo $#)"),
        "full 档到位总数不再是清单派生 —— 加第 10 条腿时手写数字会把它读成 9/10 假红\
         或 10/10 假绿"
    );
    assert!(
        audit.contains("2026-11-07 裁定「树级形态腿」"),
        "清单注释没写明本腿的裁定案号 —— 后来人只看到多一条腿, 看不到它拦的是哪类恒真判据"
    );
}

/// 2. 判据面: 三个逐字 API + 九项记号 + 违规谓词两面 + 唯一一条内在判白.
#[test]
fn test_vacuous_needle_judgment_surface_is_pinned() {
    let sc = read(SCANNER);

    // 三个逐字文本 API 都在射程内: 少一个即该类恒真针漏检.
    assert!(
        sc.contains(r"(?P<method>contains|matches|find)"),
        "方法清单不再是 contains / matches / find 三个 —— 收窄射程等于放过剩下那类恒真针"
    );

    // 九项记号: 逐项在位, 且条目总数恰 9 (扩与缩都红 —— 裁定原文逐项, 不擅自增删).
    let tokens = section(&sc, "REGEX_TOKENS", "SOURCE_TEXT_QUOTES");
    assert_eq!(
        tokens.matches("re.compile(").count(),
        TOKEN_NAMES.len(),
        "记号条目数不再是九项 (实得 {}), 与裁定口径不一致",
        tokens.matches("re.compile(").count()
    );
    for name in TOKEN_NAMES {
        let plain = format!("\"{name}\"");
        let raw = format!("r\"{name}\"");
        assert!(
            tokens.contains(&plain) || tokens.contains(&raw),
            "记号 {name:?} 不在集合里 —— 该项恒真针从此无人判"
        );
    }

    // 违规 = 反向 AND 无判白理由, 两个条件都必须在谓词里 (少一个即整类判据变形).
    assert!(
        sc.contains(r#"r["negated"] and r["exempt"] is None"#),
        "违规谓词不再同时要求「反向」与「无判白理由」 —— 把 exempt 读成真值会把判白\
         条目也算违规, 反之会把恒真针放行"
    );
    // 判白只有内在的引号规则一条 (口径第 4 条), 且正向面另有独立理由串.
    assert!(
        sc.contains("if any(q in needle for q in SOURCE_TEXT_QUOTES)"),
        "「针内含引号字符 => 逐字源码文本」这条唯一判白规则消失 —— 存量那条真判据会被\
         判成恒真针, 只能靠人工豁免绕过 (正是裁定禁止的形态)"
    );
    assert!(
        sc.contains("if not negated:") && sc.contains("EXEMPT_POSITIVE"),
        "正向针不再单独给判白理由 —— 「不判」与「判不到」在报告里重新同形"
    );
    // 无人工豁免表 / 无 --strict 式分流: 两者都会让门禁在默认调用下零执法 (P-59 同族病灶).
    for stray in [
        "\"--strict\"",
        "\"--allow\"",
        "EXEMPT_FILES",
        "WHITE_LIST",
        "SKIP_LIST",
    ] {
        assert!(
            !sc.contains(stray),
            "脚本里出现 {stray} —— 本腿的裁定是「不设人工豁免表、裸调即判」, 任一形态\
             都等于把执法点挪回需要人记得加参数"
        );
    }
}

/// 3. 射程面与 fail-closed: 只 .rs 全量 + 未提交在制品 + 排除 vendored, 三档退出码齐备.
#[test]
fn test_vacuous_needle_scope_and_fail_closed_are_pinned() {
    let sc = read(SCANNER);

    // 枚举口径: git ls-files 且带 --others (本批在制品同样受检).
    for flag in [
        "\"--cached\"",
        "\"--others\"",
        "\"--exclude-standard\"",
        "\"*.rs\"",
    ] {
        assert!(
            sc.contains(flag),
            "受检文件枚举少了 {flag} —— 缺 --others 时未提交的恒真针要等提交才红, \
             缺 *.rs 时射程整体丢失"
        );
    }
    assert!(
        sc.contains("VENDOR_PREFIX = \"src/kernel/functions/net/smoltcp/\""),
        "vendored smoltcp 不再排除 —— 锁定不可改的树会把本腿变成不可能完成的清单"
    );

    // 词法面: 注释与字符串必须先屏蔽, 否则文档里"举一个坏例子"会让门禁常红.
    assert!(
        sc.contains("if not mask[m.start()]"),
        "调用点不再检查可执行文本掩码 —— 注释里的示例形态会被判成恒真针"
    );
    assert!(
        sc.contains("def first_arg_literal"),
        "首参字面量判定消失 —— 断言文案 (距离针很远的字面量) 会被当成针, 而真正的\
         常量 / format! 针会被静态误判"
    );

    // 退出码三档: 扫描失败 2 (fail-closed) / 违规 1 / 无违规 0, 且两处判定都判退出码.
    assert!(
        sc.contains("class ScanError") && sc.contains("return 2"),
        "读取/枚举失败不再 fail-closed —— 扫描器自己坏了却给出「零违规」是假绿"
    );
    assert_eq!(
        sc.matches("return 1 if bad else 0").count(),
        2,
        "「有违规即红」的出口不再是 JSON 与文本两条路径各一处 (实得 {} 处) —— 少一处\
         就等于 CI 换用另一种输出格式时静默退 0",
        sc.matches("return 1 if bad else 0").count()
    );
    assert!(
        sc.contains("sys.exit(main())"),
        "退出码没有真正交给进程 —— audit.sh 的 2e 循环读不到判红"
    );
    assert!(
        sc.contains("rows.extend(scan_text(rel, text))"),
        "扫描循环不再经唯一的检测器入口 —— 报告面与判据面分叉即回到两处口径互不暴露"
    );
}

/// 5. 自测执行点与判白存量: 负控制必须真调检测器, 且那条逐字源码文本针仍在语料里.
#[test]
fn test_vacuous_needle_selftest_and_white_stock_are_locked() {
    let selftest = read(SELFTEST);
    // P-50 ③ 同型要求: 自测必须按路径加载检测器并真调它, 不在自测文件里复制算法.
    assert!(
        selftest.contains("SCRIPTS_DIR / \"audit_vacuous_needle.py\""),
        "自测没有真调本检测器 —— 检测器退化时自测照样全绿, 负控制失去判别力"
    );
    assert!(
        selftest.contains("detector.scan_text(") && selftest.contains("detector.violations_of("),
        "自测不再经检测器唯一的两个入口判针与判违规 —— 另算一套就是把 P-50 的病灶换个位置复现"
    );
    assert!(
        selftest.contains("test_vacuous_needle(),"),
        "本腿的负控制没进 main() 的结果清单 —— 只有人手工跑的自测与零调用者脚本同病灶 (P-49 甲)"
    );
    assert!(
        selftest.contains("[Test 10/10]"),
        "自测项数标注仍停在旧档位 —— 接线自测腿的到位读数与清单不一致"
    );
    assert!(
        !selftest.contains("[Test 1/9]"),
        "标注没随第 10 项一起改口 —— 报告读数与实际用例数分叉"
    );

    // 语料里唯一那条自动判白的针仍在位 (口径第 4 条的实测存量恰 1 条).
    let unified = read(UNIFIED_TEST);
    assert!(
        unified.contains("旧单行 use 正则仍有残留") && unified.contains("re.match"),
        "P-65 那条搜旧正则行的逐字源码针已消失 —— 判白存量读数不再可解释, 需同步本锁与台账"
    );
}
