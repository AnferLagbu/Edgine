//! B: CI 入口统一 (Makefile.ci 薄封装 + ci/audit.sh 权威化)
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-11.
//!
//! 旧 Makefile.ci 平行实现了一套弱化判据 (只跑 3 个审计脚本、`grep "^error["`
//! 漏 warning 与 link 阶段、dev profile、`cmd | tee` 把失败掩盖成成功), 与本地
//! 权威入口 ci/build.sh + ci/audit.sh 分叉; 而 AGENTS.md §2.2 宣称「强制」的
//! F1+F2 / F4 两项硬规则审计与锁顺序专项 (附加门禁, P-68) 当时只接入了 GitHub
//! workflow, 本地一键门禁反而不跑.
//! 本测试锁住收口后的形态: Makefile.ci 只委派, ci/audit.sh 是 §2.2 的完整落地.

use std::fs;
use std::path::Path;

const MAKEFILE_CI: &str = "../Makefile.ci";
const AUDIT_SH: &str = "../ci/audit.sh";
const BUILD_SH: &str = "../ci/build.sh";
const AGENTS_MD: &str = "../AGENTS.md";
const REQUIREMENTS_SH: &str = "../scripts/requirements.sh";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {}", path, e))
}

/// 取出 Makefile.ci 中定义的目标名 (行首 `name:` 形式, 忽略变量赋值与注释).
fn defined_targets(mk: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in mk.lines() {
        let line = raw.trim_start();
        if line.starts_with('#') {
            continue;
        }
        // 只认顶格目标 (recipe 行以 Tab 开头, 已被 trim_start 之外的判定排除).
        if raw.starts_with('\t') || raw.is_empty() {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let name = line[..colon].trim();
        if name.is_empty()
            || name.contains(' ')
            || name.contains('$')
            || name.starts_with('.')
            || line[colon + 1..].contains('=')
        {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

fn phony_list(mk: &str) -> String {
    // .PHONY 是续行写法, 把整个声明 (直到空行) 拼起来再查.
    let mut acc = String::new();
    let mut in_phony = false;
    for line in mk.lines() {
        if let Some(rest) = line.trim_start().strip_prefix(".PHONY:") {
            in_phony = true;
            acc.push_str(rest);
        } else if in_phony {
            if line.trim().is_empty() {
                break;
            }
            acc.push(' ');
            acc.push_str(line.trim());
        }
    }
    acc
}

#[test]
fn test_makefile_ci_delegates_instead_of_reimplementing() {
    let mk = read(MAKEFILE_CI);
    // 委派到两个权威入口. 档位字面量不在此断言 —— 由
    // test_gate_criterion_mode_is_single_sourced 单点持有 (P-48), 两处锁同一字符串会
    // 在做档位调整时互相绊住, 使「谁定的判据」重新变成两个源.
    assert!(
        mk.contains("bash ci/audit.sh"),
        "ci-audit 必须委派 ci/audit.sh (权威审计栈), 不再自己挑脚本"
    );
    assert!(
        mk.contains("./ci/build.sh all"),
        "ci-build 必须委派 ci/build.sh (双架构 check + link)"
    );
    // 旧平行实现的标志物必须消失.
    for gone in [
        "audit_safety_coverage.py",
        "audit_functions_boundary.py",
        "audit_deadlock_matrix.py",
        "grep -q \"^error[\"",
    ] {
        assert!(
            !mk.contains(gone),
            "Makefile.ci 仍含平行实现标志 {gone} —— 判据必须留在被委派脚本里"
        );
    }
    // 检查逻辑不落在 Makefile.ci: 除单点快查外不得直接调 python 审计脚本.
    let py_calls = mk
        .lines()
        .filter(|l| l.trim_start().starts_with('@') && l.contains("$(PY)"))
        .filter(|l| !l.contains("ci_check_functions_unsafe.py"))
        .filter(|l| !l.contains("bench_baseline"))
        .filter(|l| !l.contains("bench_regression"))
        .collect::<Vec<_>>();
    assert!(
        py_calls.is_empty(),
        "Makefile.ci 直接调用审计脚本 (平行实现风险):\n{}",
        py_calls.join("\n")
    );
}

#[test]
fn test_makefile_ci_pipefail_and_phony_complete() {
    let mk = read(MAKEFILE_CI);
    assert!(
        mk.contains("SHELL := /bin/bash -o pipefail"),
        "Makefile.ci 必须开 pipefail: `cmd | tee` 形态会把检查失败掩盖成 PASS"
    );

    let phony = phony_list(&mk);
    let missing: Vec<String> = defined_targets(&mk)
        .into_iter()
        .filter(|t| !phony.split_whitespace().any(|p| p == t))
        .collect();
    assert!(
        missing.is_empty(),
        "目标未列入 .PHONY (存在同名文件时会被 make 判为最新而跳过): {missing:?}"
    );
}

#[test]
fn test_ci_aggregate_covers_six_gates() {
    let mk = read(MAKEFILE_CI);
    let line = mk
        .lines()
        .find(|l| l.trim_start().starts_with("ci: "))
        .unwrap_or_else(|| panic!("Makefile.ci 缺少 ci 聚合目标"));
    for gate in [
        "ci-build",            // §2.3-1 双架构 0 error / 0 warning
        "ci-audit",            // §2.3-2/3 clippy + fmt + 核心审计
        "ci-test-host",        // §2.3-4 host-tests
        "ci-test-kernel-host", // §2.3-6 内核 host 单测
        "ci-boot",             // §2.3-5 QEMU 集成
        "ci-rustdoc",          // CI 对齐 (ci-lint.yml rustdoc-check)
    ] {
        assert!(
            line.split_whitespace().any(|p| p == gate),
            "ci 聚合缺 {gate} —— 「本地一键绿」不等于 §2.3 六门槛绿"
        );
    }
    // ci-boot 必须同时覆盖两架构 (旧聚合只跑 aarch64).
    let boot = mk
        .lines()
        .find(|l| l.trim_start().starts_with("ci-boot: "))
        .unwrap_or_else(|| panic!("Makefile.ci 缺少 ci-boot 目标"));
    for arch in ["ci-test-x86", "ci-test-aarch64"] {
        assert!(
            boot.split_whitespace().any(|p| p == arch),
            "ci-boot 未含 {arch}"
        );
    }
}

#[test]
fn test_audit_sh_carries_hard_rule_audits() {
    // AGENTS.md §2.2 把 F1+F2 / F4 / F3 与锁顺序专项 (附加门禁, P-68) 列为强制门禁:
    // 权威入口必须真跑这四项.
    // coupling 一列由 P-45 收口时补入 —— 此前 F3 的指定检查方式在本地权威入口
    // 没有调用点, 只在从不触发的 workflow 里存在 (文档标「强制」而无执法路径).
    let sh = read(AUDIT_SH);
    for hard in [
        "functions_boundary",
        "safety_coverage",
        "deadlock_matrix",
        "coupling",
    ] {
        assert!(
            sh.contains(hard),
            "ci/audit.sh 未接入 audit_{hard}.py —— §2.2 宣称的强制门禁与本地入口分叉"
        );
    }
}

/// P-46 结构锁: `ci/audit.sh` 的审计腿存在性包装必须逐处 fail-closed, 且到位清单
/// 不得点名不存在的脚本.
///
/// 病灶形态 (P-46 负控制实测): 13 处 `if command -v python3 && [ -f 脚本 ]` 包装
/// 全部无 `else`, 而 `step` 头写在包装内部 ⇒ 脚本被改名 / 缺失 / 环境无 python3 时
/// 该腿连一行日志都不留, 退出码仍是 0 ——「audit 全绿」与「少跑了几条腿」不可区分.
///
/// 锁的三条判据:
/// ① 包装位点数 == `audit_leg_missing` 调用行数 (删任一 `else` 或新增第 14 处裸包装
///    都立即不等 ⇒ 红);
/// ② `AUDIT_REQUIRED_LEGS` 清单里的每个腿名都能在 `mark_leg "名"` 或 `for … in 列表`
///    里找到登记点 (整条腿被摘掉 ⇒ 红);
/// ③ 清单里每个名字对应的 `audit_<名>.py` 必须真实存在 (清单漂移 ⇒ 红).
#[test]
fn test_audit_sh_audit_legs_are_fail_closed() {
    let sh = read(AUDIT_SH);
    let wrapper_pat = "command -v python3 >/dev/null 2>&1 && [ -f";
    let wrappers = sh.lines().filter(|l| l.contains(wrapper_pat)).count();
    let guards = sh
        .lines()
        .filter(|l| l.trim_start().starts_with("audit_leg_missing "))
        .count();
    assert!(
        wrappers > 0,
        "ci/audit.sh 里找不到存在性包装腿, 测绘口径已变"
    );
    assert_eq!(
        wrappers, guards,
        "ci/audit.sh 有 {wrappers} 处 `[ -f 脚本 ]` 包装但只有 {guards} 处 audit_leg_missing \
         硬失败出口 —— P-46 回退, 缺失的腿会静默消失且退出码仍为 0"
    );

    let required_line = sh
        .lines()
        .find(|l| l.trim_start().starts_with("AUDIT_REQUIRED_LEGS="))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 AUDIT_REQUIRED_LEGS 到位清单 (P-46 乙)"));
    let required: Vec<&str> = required_line
        .split('"')
        .nth(1)
        .unwrap_or_else(|| panic!("AUDIT_REQUIRED_LEGS 不是双引号字面量"))
        .split_whitespace()
        .collect();
    assert!(
        required.len() >= 19,
        "AUDIT_REQUIRED_LEGS 只点名 {} 条腿, 低于实测的 19 条 (18 被包装 + coupling)",
        required.len()
    );

    // 两个 for 循环里的腿名以列表形态出现, 不在 mark_leg 的字面量里.
    let for_lines: Vec<&str> = sh
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("for ") && t.contains(" in ") && t.contains("; do")
        })
        .collect();
    for leg in &required {
        let marked = sh.contains(&format!("mark_leg \"{leg}\""));
        let looped = for_lines.iter().any(|l| {
            l.split_whitespace()
                .map(|w| w.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_')))
                .any(|w| w == *leg)
        });
        assert!(
            marked || looped,
            "审计腿 {leg} 在 AUDIT_REQUIRED_LEGS 里点名却找不到登记点 (mark_leg / for 列表) \
             —— 该腿已被摘除而清单未同步"
        );
        let script_exists = [
            format!("../scripts/audit_{leg}.py"),
            format!("../tools/audit_{leg}.py"),
        ]
        .iter()
        .any(|p| Path::new(p).exists());
        assert!(
            script_exists,
            "AUDIT_REQUIRED_LEGS 点名的 {leg} 无对应 audit_{leg}.py —— 清单与脚本集漂移"
        );
    }
}

/// P-53 结构锁: F9 全仓执法路径 = 脚本 + 冻结侧车 + 两档 audit 腿, 三要素任一失效即红.
///
/// 病灶形态 (P-53 测绘): §5 F9 的「检查方式」只有 rustc dead_code lint, 而 rustc 只
/// 覆盖被编译到的路径; 仓内兜底只有 4 份 host-tests 里按手抄文件清单跑的 grep. 同族的
/// 三种退化必须被本锁挡住:
/// ① 脚本写出来却不接线 (P-49 的「零调用者脚本」形态);
/// ② 接线了但自检没有执行点 (P-50 ③ 的「自测只模拟算法」形态);
/// ③ 有阻塞口径却没有冻结基线 —— 存量 292 处 clippy 抑制会让两档恒红, 下一个人只会
///    把整条腿摘掉 (P-49 已实测的「为绿而摘」动力).
///
/// 五臂:
/// ① 五树扫描根 / vendored 按路径分量排除 / 扫描根缺失 fail-closed 出口齐备;
/// ② 属性位置锚定检测 + 自测执行点 + 死代码抑制先于基线写入 (dead_code 永不进基线);
/// ③ 基线唯一写入口是 --update-baseline, 读取侧必须双向对账 (over + under);
/// ④ 腿在 AUDIT_REQUIRED_LEGS 里, mark_leg 与 audit_leg_missing 成对, 且位于 0.5n
///    到位断言之前 (放后面则「本腿未跑」不进单点比对);
/// ⑤ 侧车快照真实存在, 且 _meta.disposition 点名承接工程 P-64.
#[test]
fn test_p53_f9_dead_code_comment_gate_is_wired() {
    let sh = read(AUDIT_SH);
    let py = read("../scripts/audit_dead_code_comment.py");
    assert!(
        Path::new("../scripts/audit_dead_code_comment.baseline.json").exists(),
        "F9 冻结侧车 audit_dead_code_comment.baseline.json 缺失 —— \
         无基线则 292 处存量抑制让腿恒红, 腿会被摘掉而不是被修"
    );
    let baseline = read("../scripts/audit_dead_code_comment.baseline.json");

    for needle in [
        r#"SCAN_ROOTS = ("src/kernel", "host-tests", "src/user", "src/rust", "kani-harness")"#,
        r#"VENDORED_DIRS = {"smoltcp"}"#,
        "for root in SCAN_ROOTS",
        "扫描根不存在",
        "if args.self_test:",
        "if args.update_baseline:",
    ] {
        assert!(
            py.contains(needle),
            "audit_dead_code_comment.py 缺关键结构 `{needle}` —— P-53 甲 的检测面退化"
        );
    }
    let dead_zero = py
        .find("if dead_hits:")
        .unwrap_or_else(|| panic!("脚本缺死代码抑制的阻断分支 (F9 硬零)"));
    let baseline_write = py
        .find("if args.update_baseline:")
        .unwrap_or_else(|| panic!("脚本缺 --update-baseline 写入口"));
    assert!(
        dead_zero < baseline_write,
        "死代码抑制的阻断分支位于基线写入之后 —— dead_code 族可被 --update-baseline 洗进基线"
    );
    assert!(
        py.contains("if over or under:"),
        "基线对账必须双向 (超出 + 减少) —— 单向只挡新增, 快照会永久过期"
    );
    assert!(
        baseline.contains("P-64"),
        "侧车 _meta.disposition 未点名 P-64 —— 存量 1661 处抑制的承接工程丢失"
    );

    let required_line = sh
        .lines()
        .find(|l| l.trim_start().starts_with("AUDIT_REQUIRED_LEGS="))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 AUDIT_REQUIRED_LEGS 到位清单"));
    assert!(
        required_line
            .split('"')
            .nth(1)
            .is_some_and(|s| s.split_whitespace().any(|w| w == "dead_code_comment")),
        "dead_code_comment 未进 AUDIT_REQUIRED_LEGS —— 0.5n 到位断言不再要求本腿执行"
    );

    let leg = sh
        .find("mark_leg \"dead_code_comment\"")
        .unwrap_or_else(|| panic!("ci/audit.sh 未登记 dead_code_comment 腿"));
    let assertion_anchor = sh
        .find("0.5n/6")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺 0.5n 到位性断言"));
    let quick_exit = sh
        .find("if [ \"$MODE\" = \"quick\" ]")
        .unwrap_or_else(|| panic!("ci/audit.sh 找不到 quick 早退分支"));
    assert!(
        leg < assertion_anchor,
        "dead_code_comment 腿位于 0.5n 到位断言之后 —— 未执行时不会进缺失清单"
    );
    assert!(
        leg < quick_exit,
        "dead_code_comment 腿被挪到 quick 早退之后 —— F9 是 §5 硬规则, 两档都必须执法"
    );
    assert!(
        sh.contains(r#"audit_leg_missing "dead_code_comment""#),
        "dead_code_comment 腿缺 audit_leg_missing 出口 —— 脚本缺失时静默消失 (P-46 回退)"
    );
    assert!(
        sh.contains(r#""$PROJECT_ROOT/scripts/audit_dead_code_comment.py" --self-test"#),
        "audit.sh 未执行脚本自检 —— 检测器退化时门禁会静默全绿 (P-50 ③ 同族)"
    );
    assert!(
        sh.contains("DC_RC=$?"),
        "audit.sh 未捕获脚本退出码 (B01-25 惯用法) —— pipefail 下 `if cmd | tail` 的 \
         失败分支不可达, 真因会被「执行异常」吞掉"
    );
}

/// P-49 结构锁: full 档存量审计腿清单必须逐条真实接线, 且只在 quick 早退之后执行.
///
/// 病灶形态 (P-49 测绘): 六条审计脚本在仓内**零调用者** —— §2.2 与 plan 点名它们,
/// 却没有任何本地执法路径 (`ci/audit.sh` / `Makefile*` / `.github/workflows/*` 全仓 grep
/// 无命中). 同族病灶是「脚本自己测自己」的自测也只有人手工跑.
///
/// 接线形态必须是「full 档 + 冻结基线」而非「quick 档 + 立刻红」: 存量债 (48 处隐式依赖)
/// 不因接入而减少, 放进两档清单会让 quick 恒红, 于是下一次有人为了「先绿起来」把它
/// 整条摘掉 —— 那正是 P-46 测绘的静默消失形态. 本锁把这三条约束钉死.
///
/// 六条判据:
/// ① `AUDIT_REQUIRED_LEGS_FULL` 是 `腿名:相对路径` 成对的双引号字面量, ≥9 条
///    (实测 9 = 6 存量阻断腿 + 2 自测腿 + 1 编译器口径债册腿; 第 8 条 audit_selftest 由
///    P-50 ③ 追加, 第 9 条 missing_docs_debt 由 P-59 裁定 ① 追加);
/// ② 每条相对路径在 `scripts/` 下真实存在 (含 `tests/` 子目录里的自测腿) —— 清单漂移即红;
/// ③ 腿名与 quick 档清单不相交, 且 step 2e 位于 quick 早退分支**之后** —— 挪进 quick
///    或把整块前移都会让弱档恒红, 由此臂转红;
/// ④ 2e 块内「遍历清单 + mark_leg_full 登记 + P-46 存在性包装 + audit_leg_missing 出口 +
///    到位断言」五要素齐备 —— 只留清单删掉执行体 ⇒ 红;
/// ⑤ 报告腿的非阻断形态必须显式点名腿名并写明「不判退出码」与另立案号 —— 把报告腿
///    伪装成门禁 (或把阻断腿悄悄降级) 都由此臂转红;
/// ⑥ `# - full:` 头部自述行点名步骤 2e —— P-48 甲 收口的「档位自述与实际结构一致」
///    是同族约束, 新增 full-only 腿却漏自述即回到该病灶.
#[test]
fn test_audit_sh_full_only_legs_are_wired() {
    let sh = read(AUDIT_SH);

    let required_line = sh
        .lines()
        .find(|l| l.trim_start().starts_with("AUDIT_REQUIRED_LEGS_FULL="))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 AUDIT_REQUIRED_LEGS_FULL 清单 (P-49)"));
    let specs: Vec<&str> = required_line
        .split('"')
        .nth(1)
        .unwrap_or_else(|| panic!("AUDIT_REQUIRED_LEGS_FULL 不是双引号字面量"))
        .split_whitespace()
        .collect();
    assert!(
        specs.len() >= 9,
        "AUDIT_REQUIRED_LEGS_FULL 只点名 {} 条腿, 低于实测的 9 条 (6 存量阻断腿 + 2 自测腿 + \
         1 编译器口径债册腿)",
        specs.len()
    );
    assert!(
        specs
            .iter()
            .any(|s| s.starts_with("implicit_deps_selftest:")),
        "清单里没有 implicit_deps_selftest 腿 —— 只有人手工跑的自测与「零调用者审计脚本」\
         是同一个病灶 (P-49 甲), 接线时不给它调用点则本轮修法当场失效"
    );
    assert!(
        specs.iter().any(|s| s.starts_with("audit_selftest:")),
        "清单里没有 audit_selftest 腿 —— P-50 ③ 真调化后的四项自检若不进 CI, \
         等于把 P-49 的「零调用者」病灶换个位置复现"
    );
    assert!(
        specs.iter().any(|s| s.starts_with("missing_docs_debt:")),
        "清单里没有 missing_docs_debt 腿 —— P-59 裁定 ① 把 F8 的存在性交 rustc \
         `missing_docs` 作准, 本腿是该口径唯一的执法点; 摘掉它, 存量债册与「新增未文档\
         公共项」一起退回无人执法"
    );

    let quick_legs: Vec<&str> = sh
        .lines()
        .find(|l| l.trim_start().starts_with("AUDIT_REQUIRED_LEGS="))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 AUDIT_REQUIRED_LEGS 清单 (P-46 乙)"))
        .split('"')
        .nth(1)
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    for spec in &specs {
        let (leg, rel) = spec.split_once(':').unwrap_or_else(|| {
            panic!("AUDIT_REQUIRED_LEGS_FULL 的 {spec} 不是「腿名:scripts 相对路径」形态")
        });
        assert!(
            Path::new("../scripts").join(rel).exists(),
            "full 档腿 {leg} 点名 scripts/{rel} 但文件不存在 —— 接线清单与脚本集漂移"
        );
        assert!(
            !quick_legs.contains(&leg),
            "full 档腿 {leg} 同时进了 quick 档清单 —— 存量债腿会让 quick 恒红; \
             要挪档需先把债项归零并同步本锁与 §2.3 档位自述"
        );
    }

    let quick_exit = sh
        .find("if [ \"$MODE\" = \"quick\" ]")
        .unwrap_or_else(|| panic!("ci/audit.sh 找不到 quick 早退分支 —— 档位语义已变"));
    let block_start = sh
        .find("step \"2e/6")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少步骤 2e (P-49 存量审计腿接入)"));
    assert!(
        quick_exit < block_start,
        "步骤 2e 被挪到 quick 早退分支之前 (2e={block_start} exit={quick_exit}) —— full 档腿变\
         成两档都跑, 与 §2.3/头部自述的覆盖边界分叉"
    );
    let block = &sh[block_start..];
    let block = match block.find("\nstep \"") {
        Some(next) => &block[..next],
        None => block,
    };

    // ④ 五要素: 遍历清单 / 登记 / 存在性包装 / 硬失败出口 / 到位断言.
    for element in [
        "for P49_SPEC in $AUDIT_REQUIRED_LEGS_FULL",
        "mark_leg_full",
        "command -v python3 >/dev/null 2>&1 && [ -f",
        "audit_leg_missing",
        "AUDIT_MISSING_LEGS_FULL",
    ] {
        assert!(
            block.contains(element),
            "步骤 2e 块里找不到 {element} —— 接线体被摘除而清单仍在 (P-46 静默消失同族)"
        );
    }
    assert!(
        block.contains("if (cd \"$PROJECT_ROOT\" && python3"),
        "步骤 2e 里腿不再走「判退出码 + 锚定仓库根」的调起形态 —— 所有腿一起变成只播报读数 \
         (审计绿≠门禁绿, P-47/48 同族), 或改成借外层 cwd (P-50 扫描根同族)"
    );
    assert!(
        block.contains("err \"审计腿"),
        "步骤 2e 里阻断腿的非零出口消失 —— 判了退出码却不阻断, 等于没判"
    );

    // ⑤ P-58: 报告腿已升为阻断腿 —— 步骤 2e 里不允许再出现任何非阻断豁免分支.
    assert!(
        !block.contains("报告腿") && !block.contains("不判退出码"),
        "步骤 2e 里又出现了报告腿 / 不判退出码的豁免标注 —— P-58 已裁定升为阻断腿, \
         重新引入非阻断支路会让「审计绿」不再等于「旧名已清零」"
    );
    assert!(
        block.matches("(cd \"$PROJECT_ROOT\" && python3").count() == 1,
        "步骤 2e 里判退出码的调起形态不是唯一形态 —— 存在绕过阻断的支路"
    );
    assert!(
        specs.iter().any(|s| s.starts_with("obsolete_naming:")),
        "full 档清单里没有 obsolete_naming 腿 —— P-58 升阻断后的执法点丢失, \
         废弃命名扫描退回无人执行的脚本"
    );

    // ⑥ 档位自述与实际结构一致 (P-48 甲 同族).
    let header = &sh[..sh
        .find("set -euo pipefail")
        .unwrap_or_else(|| panic!("audit.sh 头块定位失败"))];
    let full_claim = header
        .lines()
        .find(|l| l.trim_start().starts_with("# - full:"))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 full 档自述行"));
    assert!(
        full_claim.contains("2e"),
        "full 档自述没点名步骤 2e —— 新增 full-only 腿却漏自述, 回到 P-48 甲 收口的失真病灶"
    );
}

/// P-69 结构锁: 两档到位断言的期望值不得与实测值同源 (self-referential).
///
/// 病灶形态 (P-59 负控制 C5a 实测): 两处到位断言都用「清单变量 − 已登记腿」求差,
/// 把整条腿从清单里摘掉时差集仍为空 ⇒ runtime 打「到位 9/9 → 8/8」全绿, 只有
/// host-tests 会红. 即在 CI 日志层面「少跑一条腿」不可见.
///
/// 五条判据:
/// ① 对账块存在, 且文案点名 §2.2 与 P-69;
/// ② 块内同时把 `AGENTS.md` 与两档清单传入 —— 只读清单即回到自参照;
/// ③ 文档切片字面量 `### 2.2` / `### 2.3` 与双前缀正则 `(?:audit|scan)_` 齐备, 且两个
///    前缀都会被剥离后再与清单求交 (`scan_obsolete_naming` → `obsolete_naming`);
/// ④ fail-closed: 出口是 `err "P-69:` 而非只打印黄字; 且前置无 `command -v python3`
///    守卫 —— 缺 python3 时走 else 而非静默跳过 (P-46 同族);
/// ⑤ 位置在两档清单定义之后、首个 `step "` 之前 —— 挪到任何门禁之后都让「先跑门禁
///    再报假绿」重新出现.
#[test]
fn test_p69_leg_manifest_is_reconciled_against_agents() {
    let sh = read(AUDIT_SH);
    let start = sh
        .find("# ── P-69 乙: 腿清单与 AGENTS §2.2 反向对账")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 P-69 反向对账块 —— 到位断言退回自参照"));
    let rest = &sh[start..];
    let end = rest
        .find("# ── P-47 甲")
        .unwrap_or_else(|| panic!("P-69 对账块尾部定位失败 (P-47 台账标记消失?)"));
    let block = &rest[..end];

    assert!(
        block.contains("err \"P-69:"),
        "P-69 对账块缺硬失败出口 —— 文档点名腿被摘除时只打印不阻断, 假绿照样收尾"
    );
    assert!(
        block.contains(
            "if python3 - \"$PROJECT_ROOT/AGENTS.md\" \"$AUDIT_REQUIRED_LEGS\" \
             \"$AUDIT_REQUIRED_LEGS_FULL\""
        ),
        "对账块未同时把 AGENTS.md 与两档清单传入 —— 少传清单就只剩自参照, 摘腿仍绿"
    );
    assert!(
        !block.contains("command -v python3"),
        "对账块前置 command -v python3 守卫 —— 环境缺 python3 时该腿静默跳过 (P-46 同族)"
    );
    for literal in [
        "### 2.2",
        "### 2.3",
        "(?:audit|scan)_",
        "\"audit_\"",
        "\"scan_\"",
    ] {
        assert!(
            block.contains(literal),
            "对账块缺切片/前缀字面量 {literal} —— 文档结构或腿名命名一改就静默放过"
        );
    }

    let quick_line = sh
        .find("AUDIT_REQUIRED_LEGS=\"")
        .unwrap_or_else(|| panic!("缺少 AUDIT_REQUIRED_LEGS 清单"));
    let full_line = sh
        .find("AUDIT_REQUIRED_LEGS_FULL=\"")
        .unwrap_or_else(|| panic!("缺少 AUDIT_REQUIRED_LEGS_FULL 清单"));
    let first_step = sh
        .find("\nstep \"0/6")
        .unwrap_or_else(|| panic!("找不到步骤 0 头"));
    assert!(
        start > quick_line && start > full_line,
        "P-69 对账块在清单定义之前使用它们 —— 读到空/未定义值, 对账恒绿"
    );
    assert!(
        start < first_step,
        "P-69 对账块被挪到门禁之后 (start={start} step={first_step}) —— 摘腿的假绿已先播报"
    );
}

/// P-50 结构锁: 扫描根全扩与自测真调化两处口径必须落地, 且 §2.2 不得把定点锁读成树级防线.
///
/// 病灶 (P-50 测绘):
/// - `audit_comment_language.py` (F7) 与 `audit_c_naming.py` (I-07) 此前只扫 `src/kernel`,
///   于是「中文注释强制」只在内核层执法; host-tests / src/user / kani-harness 三树零覆盖
///   (实测 66 + 3 + 2 处违规). 扩根后还必须逐根 fail-closed —— 根拼错要被读成配置错误,
///   不能读成「扫描零违规」.
/// - `scripts/tests/audit_selftest.py` 的四个用例在自测文件里**复制**了检测算法,
///   检测器退化时自测照样全绿. 真调化后若不进 CI, 等于把 P-49 的「零调用者」换个位置复现.
/// - `audit_repr_c.py` / `audit_volatile_access.py` / `audit_tlb_receive_order.py` 是
///   冻结清单上的定点回归锁, §2.2 必须写明这一边界, 否则读者会把「清单内 0 违规」
///   当成「全树无同类风险」.
///
/// 判据:
/// ① 两个审计脚本的 BASES 各含四根, 且扫描循环由 BASES 驱动 (声明但不使用 ⇒ 红);
/// ② 两脚本都有「覆盖根缺失」fail-closed 出口并点名 P-50 丙;
/// ③ 自测文件按路径真调检测器 (`spec_from_file_location` + 四个真实入口), 且不再含
///    「手动模拟」/「核心检测算法」这类复制实现的痕迹;
/// ④ §2.2 的定点锁脚注点名三个脚本、冻结清单规模、「非树级防线」与 P-61 案号.
#[test]
fn test_p50_audit_scope_and_selftest_are_wired() {
    let agents = read(AGENTS_MD);
    let selftest = read("../scripts/tests/audit_selftest.py");

    // ① + ②: 两个全树审计脚本的覆盖根与 fail-closed 出口.
    for (path, tree) in [
        ("../scripts/audit_comment_language.py", "F7"),
        ("../scripts/audit_c_naming.py", "I-07"),
    ] {
        let py = read(path);
        for root in [
            r#"PROJECT_ROOT / "src" / "kernel""#,
            r#"PROJECT_ROOT / "host-tests""#,
            r#"PROJECT_ROOT / "src" / "user""#,
            r#"PROJECT_ROOT / "kani-harness""#,
        ] {
            assert!(
                py.contains(root),
                "{path} 的 BASES 缺覆盖根 {root} —— {tree} 对三树重新失明 (P-50 丙)"
            );
        }
        assert!(
            py.contains("for b in BASES"),
            "{path} 声明了 BASES 但扫描循环不遍历它 —— 加根不生效的静默形态"
        );
        assert!(
            py.contains("覆盖根缺失 (fail-closed, P-50 丙)"),
            "{path} 缺「覆盖根缺失」fail-closed 出口 —— 根拼错会被读成扫描零违规"
        );
    }

    // ③: 自测与检测器同源.
    assert!(
        selftest.contains("importlib.util.spec_from_file_location"),
        "audit_selftest.py 不再按路径加载检测器 —— 回到 sys.path 猜测或复制实现的形态"
    );
    for entry in ["check_forbidden_imports(", "detector.main()"] {
        assert!(
            selftest.contains(entry),
            "audit_selftest.py 缺真实检测入口 {entry} —— 自测不再证明检测器本身有效"
        );
    }
    assert!(
        selftest.matches(".scan_file(").count() >= 2,
        "audit_selftest.py 直接调用的 scan_file 少于 2 处 (deadlock_matrix + audit_unsafe 期望) \
         —— 至少一个用例退回复制算法"
    );
    for marker in ["手动模拟", "核心检测算法"] {
        assert!(
            !selftest.contains(marker),
            "audit_selftest.py 重新出现「{marker}」—— 自测复制检测算法后, 检测器退化时自测仍绿"
        );
    }

    // ④: §2.2 的定点锁口径.
    let sec_22_start = agents
        .find("### 2.2 核心审计脚本（硬规则门槛）")
        .unwrap_or_else(|| panic!("AGENTS.md 找不到 §2.2 标题"));
    let sec_22_end = agents[sec_22_start..]
        .find("### 2.3 验证门槛")
        .map(|i| sec_22_start + i)
        .unwrap_or_else(|| panic!("AGENTS.md 找不到 §2.3 标题 (无法界定 §2.2)"));
    let sec_22 = &agents[sec_22_start..sec_22_end];
    for needle in [
        "定点锁口径",
        "`audit_repr_c.py`",
        "`audit_volatile_access.py`",
        "`audit_tlb_receive_order.py`",
        "2 struct / 4 字段 / 1 函数",
        "非树级防线",
        "P-61",
        "`audit_selftest`",
        // P-68: deadlock_matrix 不得再被标成 F8 (F8 = 公共 API 文档), 改列附加门禁.
        "附加门禁（锁顺序/中断上下文专项，P-68）",
    ] {
        assert!(
            sec_22.contains(needle),
            "AGENTS.md §2.2 缺 {needle} —— 定点锁边界 (P-50 甲) / 编号口径 (P-68) 或新接线腿未自述"
        );
    }
}

/// P-47 结构锁: Lockbud 腿的「工具缺失」与「发现问题」两态必须分离, 且未执行台账
/// 必须在两个档位出口都播报.
///
/// 病灶形态 (P-47 描述实测): `cargo lockbud … || LOCKBUD_RESULT=$?` 的非零结果只打成
/// 一句 `⚠ lockbud 返回非零 (可能发现潜在问题或工具未安装)` 的黄字并继续 —— 本机
/// `cargo-lockbud` 未安装 ⇒ `full` 每次都走这一支 ⇒ 该腿从未产生读数, 而退出码仍 0,
/// 日志与「扫描通过」在语义上不可区分.
///
/// 锁的四条判据:
/// ① 腿内含 `command -v cargo-lockbud` 存在性探测 (缺工具时不得让 cargo 去 registry 拉包);
/// ② 缺工具分支调用 `skip_leg "lockbud"` (回退成合并黄字 ⇒ 红);
/// ③ 真跑非零走 `err` 阻断, 且腿内不存在任何以黄字打印 lockbud 结果的语句 (合并态复活 ⇒ 红);
/// ④ `report_unexecuted_legs` 定义存在且在 quick / full 两个出口各调用一次 (只报一档 ⇒ 红).
#[test]
fn test_audit_sh_lockbud_leg_separates_missing_tool_from_findings() {
    let sh = read(AUDIT_SH);
    let after_step = sh
        .find("step \"3/6 Lockbud")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 Lockbud 腿 (P-47 状态分离失去执法路径)"));
    let rest = &sh[after_step..];
    let leg_end = rest.find("\n# ── 已退役: miri").unwrap_or_else(|| {
        panic!("Lockbud 腿与下一条腿 (已退役 miri 注记) 的分界标记丢失, 结构锁无法定位块")
    });
    let leg = &rest[..leg_end];

    assert!(
        leg.contains("command -v cargo-lockbud >/dev/null 2>&1"),
        "Lockbud 腿缺少工具存在性探测 —— 回退到 P-47 病灶: 未安装时让 cargo 去 registry 拉包并静默继续"
    );
    assert!(
        leg.contains("skip_leg \"lockbud\""),
        "Lockbud 腿的「工具缺失」态未登记进未执行台账 (skip_leg) —— \
         缺失与发现问题的合并黄字复活, audit 全绿将重新包含从未执行的腿"
    );
    assert!(
        leg.contains("err \"lockbud"),
        "Lockbud 真跑返回非零时未阻断 —— §2.2 把该项列为「强制」, 只打黄字等于无门禁"
    );
    assert!(
        !leg.lines().any(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && t.contains("${YELLOW}") && t.contains("lockbud")
        }),
        "Lockbud 腿内仍存在非阻断的黄字出口 —— P-47 合并态 (未安装/有发现混为一谈) 回退"
    );

    let reports = sh
        .lines()
        .filter(|l| l.trim_start().starts_with("report_unexecuted_legs"))
        .count();
    assert!(
        sh.contains("report_unexecuted_legs() {"),
        "ci/audit.sh 缺少未执行腿台账函数 (P-47 甲)"
    );
    assert!(
        reports >= 3,
        "report_unexecuted_legs 只有 {reports} 处出现 (定义 1 + 出口 2 为期望) —— \
         少一个档位出口意味着 quick 或 full 的绿里仍可混入未执行腿"
    );
}

/// P-48 结构锁 (甲＋丙): 门槛 3 的**档位判据**必须单源, 且脚本自述不得宣称已退役的工具.
///
/// 病灶: `ci/audit.sh` 缺省档 = quick 而 quick 到步骤 2c 即退出; `Makefile.ci` 的 `ci-audit`
/// 调 quick 却自称「§2.2 表格的落地入口」, AGENTS §2.3 第 3 条又不写档位 —— 于是「按文档
/// 字面执行」拿到的是弱档绿. 同族: 工具栈自述仍列一个 2026-06-26 就退役的工具.
///
/// 四条判据:
/// ① `Makefile.ci` 委派 `bash ci/audit.sh full`, 且全文不存在 `ci/audit.sh quick` 调用;
/// ② AGENTS.md §2.3 第 3 条把判据写实为 `bash ci/audit.sh full` (档位必写);
/// ③ `ci/audit.sh` 头部自述块 (`set -euo pipefail` 之前) 不出现退役工具名 Miri,
///    且 `# 工具栈:` / `# - quick:` / `# - full:` 三类自述行齐备 (块被删空 ⇒ 红);
/// ④ 自述的覆盖边界与实际结构一致: quick 早退分支必须位于 step 2c 之后、step 2d 之前,
///    且 `# - full:` 行点名 QEMU (加 full-only 腿却漏自述 ⇒ 由此臂转红).
#[test]
fn test_gate_criterion_mode_is_single_sourced() {
    let mk = read(MAKEFILE_CI);
    let sh = read(AUDIT_SH);
    let agents = read(AGENTS_MD);

    assert!(
        mk.contains("bash ci/audit.sh full"),
        "Makefile.ci 的 ci-audit 不再委派 full —— 一键入口与 §2.3 第 3 条的判据重新分叉 (P-48 丙)"
    );
    assert!(
        !mk.contains("bash ci/audit.sh quick"),
        "Makefile.ci 重新出现 quick 委派 —— quick 不含 2d / Lockbud / QEMU, 一键绿不等于验收绿 (P-48 丙)"
    );

    let gate3 = agents
        .lines()
        .find(|l| l.trim_start().starts_with("3. 核心审计全部通过"))
        .unwrap_or_else(|| panic!("AGENTS.md §2.3 第 3 条形态已变, 结构锁无法定位判据行"));
    assert!(
        gate3.contains("bash ci/audit.sh full"),
        "AGENTS §2.3 第 3 条未写档位 —— 脚本缺省档是 quick, 按文档字面执行会落到弱档 (P-48 甲)"
    );

    let header_end = sh
        .find("set -euo pipefail")
        .unwrap_or_else(|| panic!("ci/audit.sh 头部自述块定位失败: 找不到 set -euo pipefail"));
    let header = &sh[..header_end];
    assert!(
        !header.to_lowercase().contains("miri"),
        "ci/audit.sh 头部自述仍宣称退役工具 (单源在步骤 5 的退役记录, 自述块不得复抄)"
    );
    let claim_lines: Vec<&str> = header
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("# 工具栈:") || t.starts_with("# - quick:") || t.starts_with("# - full:")
        })
        .collect();
    assert!(
        claim_lines.len() >= 3,
        "ci/audit.sh 头部自述行只剩 {} 行 (工具栈 / quick / full 三行为期望) —— 自述面形态已变, \
         本锁失去定位依据, 需同步本测试",
        claim_lines.len()
    );

    let quick_exit = sh.find("if [ \"$MODE\" = \"quick\" ]").unwrap_or_else(|| {
        panic!("ci/audit.sh 找不到 quick 早退分支 —— 档位语义已变, 需同步 §2.3 与本锁")
    });
    let step_2c = sh
        .find("step \"2c/6")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少步骤 2c (rustfmt 门禁)"));
    let step_2d = sh
        .find("step \"2d/6")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少步骤 2d (P-44 release 维 compile-only)"));
    assert!(
        step_2c < quick_exit && quick_exit < step_2d,
        "quick 早退位置变了 (2c={step_2c} exit={quick_exit} 2d={step_2d}) —— 头部自述与 §2.3 \
         里「quick 到 2c 即退出」的覆盖边界陈述需同步"
    );
    let full_claim = claim_lines
        .iter()
        .find(|l| l.trim_start().starts_with("# - full:"))
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 full 档自述行"));
    assert!(
        full_claim.contains("QEMU"),
        "full 档自述未点名 QEMU 腿 —— full-only 腿与自述重新分叉 (P-48 甲的同族病灶)"
    );
}

/// P-52 甲: clippy pedantic 必须双架构 —— 单架构时 `src/kernel` 里 147 处
/// `target_arch = "aarch64"` 门控代码完全不经 clippy (远端 ci-x86.yml 的 aarch64
/// 步骤因 Gitee-only 从不执行, 不能代替本地门禁).
///
/// 三臂:
/// ① 步骤 2 块内含双三元组循环头, 两枚三元组都在循环头里 (只写进注释 ⇒ 红);
/// ② 循环变量必须喂给 `--target` —— 否则三元组只是摆设;
/// ③ 该块必须整体位于 quick 早退分支之前 (裁定为双档加维; 挪到 full-only ⇒ 红).
#[test]
fn test_audit_sh_clippy_covers_both_arches() {
    let sh = read(AUDIT_SH);
    let start = sh
        .find("step \"2/6")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少步骤 2 (clippy pedantic)"));
    let block = &sh[start..];
    let block = match block.find("\nstep \"") {
        Some(next) => &block[..next],
        None => block,
    };
    let loop_head = block
        .lines()
        .find(|l| l.trim_start().starts_with("for target in "))
        .unwrap_or_else(|| panic!("步骤 2 没有双架构循环头 —— P-52 甲 裁定双档加维"));
    for target in ["x86_64-unknown-none", "aarch64-unknown-none-softfloat"] {
        assert!(
            loop_head
                .split_whitespace()
                .any(|t| t.trim_end_matches(';') == target),
            "步骤 2 的循环头未点名 {target} —— clippy 单架构会让目标门控代码成为 lint 盲区"
        );
    }
    assert!(
        block.contains("--target \"${target}\""),
        "步骤 2 的循环变量没有喂给 --target —— 双三元组只写在注释里不算覆盖"
    );
    let quick_exit = sh
        .find("if [ \"$MODE\" = \"quick\" ]")
        .unwrap_or_else(|| panic!("ci/audit.sh 找不到 quick 早退分支"));
    assert!(
        start < quick_exit,
        "步骤 2 被挪到 quick 早退之后 (2={start} exit={quick_exit}) —— 裁定是双档加维, \
         quick 也必须有 aarch64 clippy"
    );
}

#[test]
fn test_ci_rustdoc_matches_workflow_command() {
    // 本地入口与 workflow 的 rustdoc 判据必须同源, 否则本地绿仍可能在 CI 红.
    // 阻断面 = broken_intra_doc_links + private_intra_doc_links (R-1 乙裁定加压).
    let mk = read(MAKEFILE_CI);
    let wf = read("../.github/workflows/ci-lint.yml");
    assert!(
        mk.contains(
            "RUSTDOCFLAGS=\"-D rustdoc::broken-intra-doc-links -D rustdoc::private_intra_doc_links\""
        ),
        "ci-rustdoc 缺少与 workflow 一致的 RUSTDOCFLAGS 阻断"
    );
    assert!(
        wf.contains(
            "RUSTDOCFLAGS=\"-D rustdoc::broken-intra-doc-links -D rustdoc::private_intra_doc_links\""
        ),
        "workflow 侧 rustdoc 判据已变, 需同步 ci-rustdoc"
    );
    for target in ["x86_64-unknown-none", "aarch64-unknown-none-softfloat"] {
        assert!(
            mk.contains(target) && wf.contains(target),
            "rustdoc 双架构覆盖缺失 {target}"
        );
    }
}

#[test]
fn test_p54_coupling_judges_both_layers_and_reconciles_baseline() {
    // P-54 病灶: 旧判据 = `合计引用 > 20` 才阻断, 于是 4 对 privileged 存量环整体放行,
    // functions 层的环压根不进入判定. 现判据 = 两层都检测 + 机器派生基线双向对账.
    let py = read("../scripts/audit_coupling.py");
    assert!(
        py.contains("detect_circular_deps(fw_matrix, 'privileged')"),
        "privileged 层循环依赖未判定 —— F3 的原有覆盖面丢失"
    );
    assert!(
        py.contains("detect_circular_deps(svc_matrix, 'functions')"),
        "functions 层循环依赖未判定 —— P-54 甲 要求两层都进基线"
    );
    assert!(
        !py.contains("severe_circular") && !py.contains("'total'] > 20"),
        "旧「合计引用 > 20」阈值仍在 —— 存量环会被它整体放行 (P-54 立项病灶)"
    );
    for needle in [
        "--update-baseline",
        "BASELINE_FILE",
        "cycles_to_entries",
        "diff_baseline",
        "totals_of",
    ] {
        assert!(
            py.contains(needle),
            "audit_coupling.py 缺基线结构 `{needle}` —— 判据回退成手写清单或阈值"
        );
    }
    assert!(
        py.contains("or over or under:"),
        "基线对账必须双向 (超出 + 落后) —— 单向只挡新增, 环消解后快照会永久过期"
    );

    let baseline_path = Path::new("../scripts/audit_coupling.baseline.json");
    assert!(
        baseline_path.exists(),
        "机器派生基线侧车缺失 —— 5 对存量环会让 F3 腿恒红 (而非被修)"
    );
    let baseline = read("../scripts/audit_coupling.baseline.json");
    assert!(
        baseline.contains("\"privileged\"") && baseline.contains("\"functions\""),
        "基线侧车没有同时覆盖两层 —— functions 层的环会重新落回「不判定」"
    );
    assert!(
        baseline.contains("P-54"),
        "基线 _meta.disposition 未点名 P-54 —— 存量环的承接工程丢失"
    );
    assert!(
        baseline.contains("\"pairs\"") && baseline.contains("\"refs\""),
        "基线缺 totals 自洽校验位 —— 手改 entries 不会被脚本发现"
    );
}

/// P-55 结构锁 (乙 + ②四处全修): QEMU 腿缺工具必须进未执行台账, 且四条腿的失败成因
/// 必须由捕获式退出码给出 (不能把真失败一律打印成「执行异常」).
///
/// 病灶: ① QEMU 腿缺 `qemu-system-*` 时只打黄字、退出码不变 ⇒ full 全绿里混着
/// 一条从未执行的 gate 5; ② `if cmd 2>&1 | tail; then RC=${PIPESTATUS[0]}` 在
/// `set -o pipefail` 下进入 then 已蕴含 RC=0 ⇒ 失败详情分支不可达.
#[test]
fn test_p55_qemu_skip_and_pipe_failure_causes_are_visible() {
    let sh = read(AUDIT_SH);

    let qemu_start = sh
        .find("step \"4/6 QEMU")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 QEMU 腿 (P-55 的执法路径丢失)"));
    let qemu = &sh[qemu_start..];
    assert!(
        qemu.contains("skip_leg \"qemu\""),
        "QEMU 腿的「工具缺失」态未登记进未执行台账 —— 缺 qemu 的机器上 gate 5 从未执行, \
         日志却仍只打黄字且 EXIT=0 (P-55 ① 病灶)"
    );
    assert!(
        !qemu.contains("⚠ QEMU 二进制不可用, 跳过启动测试"),
        "QEMU 腿仍存在不记账的黄色跳过出口 —— P-55 乙 要求该态必须进入 skip 台账"
    );

    // ② 四处同族: 失败成因必须来自捕获到的真实退出码.
    for (probe, marker) in [
        ("FEATURE_CLIPPY_OUT=$(cargo clippy", "2b clippy feature 维"),
        ("RELEASE_DIM_OUT=$(", "2d 测试维"),
        ("FEATURE_DIM_OUT=$(", "2d feature 维"),
        ("QEMU_OUT=$(FAIL_OK=0", "QEMU 腿"),
    ] {
        assert!(
            sh.contains(probe),
            "{marker} 未使用 B01-25 捕获式退出码 —— 真失败会被打成「执行异常」(P-55 ②)"
        );
    }
    for (old, marker) in [
        (
            "cast_precision_loss 2>&1 | tail -10; then",
            "2b clippy feature 维",
        ),
        (
            "check_kernel_test_release_dim.sh\" 2>&1 | tail -40; then",
            "2d 测试维",
        ),
        (
            "check_kernel_feature_release_dims.sh\" 2>&1 | tail -40; then",
            "2d feature 维",
        ),
        ("qemu_boot_test.sh\" all 2>&1 | tail -6; then", "QEMU 腿"),
    ] {
        assert!(
            !sh.contains(old),
            "{marker} 回退到 `if cmd | tail; then` 形态 —— pipefail 下失败详情分支不可达"
        );
    }
    for (var, rc) in [
        ("FEATURE_CLIPPY_OUT", "FEATURE_CLIPPY_RC"),
        ("RELEASE_DIM_OUT", "RELEASE_DIM_RC"),
        ("FEATURE_DIM_OUT", "FEATURE_DIM_RC"),
        ("QEMU_OUT", "QEMU_RC"),
    ] {
        assert!(
            sh.contains(&format!("{rc}=$?")),
            "{var} 未捕获 $? 到 {rc} —— 退出码未与输出分离, 失败成因仍可能丢失"
        );
    }
}

/// P-56 结构锁 (甲＋甲＋丙): 两条不承重的统计腿必须不存在, 剩余 full-only 步骤必须连续编号,
/// 头部自述不得再把它们写回覆盖清单; Lockbud 腿须保留「缺失 / 已装但工具链不匹配 / 可跑」
/// 三态, 且调用形态必须带 `--` 分界、`--exit-code-on-bug` 与全量本仓 crate 白名单.
///
/// H-1 乙 (2026-10-30) 追加: 腿绑 `nightly-2026-02-07` (wrapper 内建 rustc 同版本) 只取读数;
/// 该绑定 + `-k deadlock` + `--release` + 专用冷 target-dir 均为实测必需 (见 ci/audit.sh 注),
/// 白名单必须列全 P-71 拆出的四个 crate (只写 `kernel` 会退化成只扫 glue 的假绿).
///
/// 病灶: ① 「3 SAFETY 注释覆盖率统计」用 `grep -c "unsafe {"` 对 `grep -c "// SAFETY:"`
/// 求比值, 实测 131% —— 与 0.5 组里恒阻断的 audit_safety_coverage.py (1722/1722) 同名不同尺,
/// 且百分比本身不成立; ② 「6 模块级 SAFETY 不变式」数的是注释文案出现频次, 实测全仓仅
/// user_proc.rs 1 个文件命中、阈值 >=5 ⇒ 每次 full 都稳定打黄字. 删除后 Lockbud 4→3,
/// QEMU 7→4. ③ Lockbud 派发器把第一个 `--` 之前的参数当自身 flags (未知 flag 被
/// unwrap_or_default 静默丢弃), 原调用无 `--` ⇒ `--manifest-path` 丢失、扫不到 kernel;
/// 且默认 `exit_code_on_bug=false`, 有发现也退 0. 上游还要求项目与 lockbud 同版本 nightly,
/// 已装但版本不匹配时必须记 skip 而不是让 full 永久转红.
#[test]
fn test_p56_statistics_legs_removed_steps_renumbered_and_lockbud_states() {
    let sh = read(AUDIT_SH);

    assert!(
        !sh.contains("step \"3/6 SAFETY"),
        "「SAFETY 注释覆盖率统计」统计腿复活 —— 它与 audit_safety_coverage.py 同名不同尺, \
         实测打印过 131% 的伪覆盖率 (P-56 甲)"
    );
    assert!(
        !sh.contains("step \"6/6"),
        "「模块级 SAFETY 不变式」统计腿复活 —— 它数注释文案频次, 删除前每次都稳定打黄字 (P-56 甲)"
    );
    assert!(
        !sh.contains("Framekernel privilege wrapper"),
        "步骤 6 的魔数串探测残留 —— 该判据统计的不是不变式, 而是五个词的出现次数"
    );

    assert!(
        sh.contains("step \"3/6 Lockbud"),
        "Lockbud 腿未按 P-56 重编号为 3/6 —— 删除两条统计腿后步骤序号必须连续"
    );
    assert!(
        sh.contains("step \"4/6 QEMU"),
        "QEMU 腿未按 P-56 重编号为 4/6 (原 7/7 的编号本身也不自洽)"
    );
    assert!(
        !sh.contains("step \"4/6 Lockbud"),
        "旧的 Lockbud 步骤号 (4/6) 残留 —— 与 QEMU 的新编号冲突"
    );
    assert!(
        !sh.contains("step \"7/7 QEMU"),
        "旧的 QEMU 步骤号 (7/7) 残留 —— 重编号回退"
    );

    let header_line = sh
        .lines()
        .find(|l| l.starts_with("# - full:"))
        .unwrap_or_else(|| panic!("ci/audit.sh 头部缺少 `# - full:` 覆盖自述行 (P-48 ③)"));
    assert!(
        header_line.contains("3 Lockbud") && header_line.contains("4 QEMU"),
        "头部 `# - full:` 行未同步重编号后的步骤 (实际: {header_line})"
    );
    assert!(
        !header_line.contains("SAFETY 统计腿"),
        "头部仍把已删除的 SAFETY 统计腿写进 full 覆盖清单 (自述与结构脱节)"
    );

    // P-56 丙 + H-1 乙: Lockbud 三态与调用形态 (2026-10-30 实测口径).
    assert!(
        sh.contains("rustup run \"$LOCKBUD_TOOLCHAIN\" lockbud --version"),
        "Lockbud 腿缺少「能否在取证工具链下加载」的判据 —— 已装但版本不匹配时会直接真跑, \
         把 full 永久染红 (上游要求项目与 lockbud 同版本 nightly, 见 P-56 丙 / H-1 乙)"
    );
    assert!(
        sh.contains("工具与取证工具链不匹配"),
        "Lockbud 的「已装但不可执行」态消失 —— 它必须 skip 并点名实测原因, \
         不能混进「工具缺失」或伪装成「真跑失败」"
    );
    assert!(
        sh.contains("LOCKBUD_TOOLCHAIN=\"nightly-2026-02-07\""),
        "Lockbud 取证工具链名丢失 —— wrapper 内建 rustc 1.95.0-nightly (2026-02-06), \
         只有 nightly-2026-02-07 能加载 (H-1 乙); 换工具链前先重跑 wrapper 加载探针"
    );
    assert!(
        sh.contains("LOCKBUD_CRATES=\"kernel,kernel-privileged,kernel-functions,edgine-abi\""),
        "Lockbud 白名单未列全 P-71 拆出的本仓 crate —— `-l` 按 crate 名精确匹配, \
         只写 kernel 只扫 glue, 是零发现的假绿"
    );
    assert!(
        sh.contains("cargo +\"$LOCKBUD_TOOLCHAIN\" lockbud -k deadlock --exit-code-on-bug -l \"$LOCKBUD_CRATES\" --"),
        "Lockbud 调用形态回退 (缺 +工具链 / -k deadlock / --exit-code-on-bug / 白名单 / \
         `--` 分界) —— 派发器会把 `--manifest-path` 等误当 lockbud flags 并静默丢弃 (P-56 ③)"
    );
    assert!(
        sh.contains("--release --manifest-path ../kernel/Cargo.toml"),
        "Lockbud 腿未显式走 release 档 —— 取证工具链下 sha2 0.11 的 debug codegen 会 \
         `rustc-LLVM ERROR` 崩溃 (H-1 实测②)"
    );
    assert!(
        !sh.contains("cargo lockbud --manifest-path"),
        "旧的 Lockbud 调用形态残留 (无 `--` 分界) —— cargo 退化为在 src/rust 裸跑 cargo build"
    );
    assert!(
        sh.contains("rm -rf \"$LOCKBUD_TARGET_DIR\"") && sh.contains("Detecting deadlock"),
        "Lockbud 腿缺少「冷缓存 + 分析到位」判据 —— lockbud 只在编译期分析, 复用旧产物时 \
         cargo 不重编却会回放上一轮 wrapper 日志 (实测 0.08s / 零 Compiling 仍有 4 条 \
         Detecting deadlock); 必须清空专用 target-dir, 并把分析条数与 crate 数对账 (H-1 ④)"
    );
    assert!(
        sh.contains("LOCKBUD_BUILT=")
            && sh
                .contains("^   Compiling (kernel|kernel-privileged|kernel-functions|edgine-abi) v")
            && sh.contains("本轮实编 ${LOCKBUD_BUILT}/${LOCKBUD_EXPECTED}"),
        "Lockbud 腿的假绿守卫回退成只数回放日志 —— 必须同时要求「本轮实编 crate 数」达标, \
         否则复用旧产物时 cargo 回放的 Detecting deadlock 会让守卫失效 (H-1 ④)"
    );

    let req = read(REQUIREMENTS_SH);
    assert!(
        req.contains("has_cmd cargo-lockbud && has_cmd lockbud"),
        "requirements.sh 的 Lockbud 判据回退 —— 实测一次 cargo install 产出两个二进制, \
         缺一即不可用 (P-56 丙①)"
    );
    assert!(
        !req.contains("check_testing \"lockbud"),
        "requirements.sh 重新用 check_testing 登记 lockbud —— 它会把非 crates.io 工具塞进 \
         TESTING_PACKAGES, 安装器随后按系统包安装 (P-56 丙②)"
    );
    assert!(
        !req.contains("cargo +nightly lockbud --help"),
        "requirements.sh 仍用 `cargo +nightly lockbud --help` 作判据 —— 它只验证派发器能打印 \
         帮助, 既不看 wrapper 能否加载, 又重复计数 (P-56 丙①③)"
    );
    assert!(
        req.contains("rsproxy.cn") && req.contains("ghproxy.net"),
        "requirements.sh 缺少国内镜像安装记录 (P-56 丙 的实测安装路径)"
    );
}

#[test]
fn test_delegated_entrypoints_exist() {
    for p in [MAKEFILE_CI, AUDIT_SH, BUILD_SH] {
        assert!(
            fs::metadata(p).is_ok(),
            "{p} 缺失 —— Makefile.ci 的委派目标全部失效"
        );
    }
    // Makefile.ci 里 `@bash ci/audit.sh` / `@./ci/build.sh` 的调用形态要求
    // 被委派脚本自身以退出码表态 (不能只打印 FAIL 文本).
    let sh = read(AUDIT_SH);
    assert!(
        sh.contains("set -e") || sh.contains("set -eu"),
        "ci/audit.sh 未开 set -e: 单步失败不会反映到整体退出码"
    );
}

/// P-57 甲: 日志收尾必须自报档位覆盖边界, 且与「环境缺失」台账分开播报.
///
/// 五臂:
/// ① `report_mode_boundary` 恰有 2 个调用点 (quick 早退 + full 收尾);
/// ② quick 出口先报环境缺失台账、再报档位边界;
/// ③ quick 行点名 full 与不含的步骤 (2d/2e), 否则读日志仍会误判;
/// ④ 旧的正向措辞「本轮未执行腿数」不得残留, 新措辞两个分支都在;
/// ⑤ Makefile.ci 的聚合依赖仍含 ci-boot (P-57 丙 裁定保留重复执行).
#[test]
fn test_p57_log_self_reports_mode_and_environment_ledger() {
    let sh = read(AUDIT_SH);
    assert!(
        sh.contains("report_mode_boundary() {"),
        "ci/audit.sh 缺少日志侧档位自述函数 (P-57 甲)"
    );
    let call_sites = sh
        .lines()
        .filter(|l| l.trim() == "report_mode_boundary")
        .count();
    assert_eq!(
        call_sites, 2,
        "档位自述调用点不是 2 个 (quick 早退 + full 收尾) —— 少一处即有一个出口的日志仍不可自解释"
    );

    let quick_start = sh
        .find("if [ \"$MODE\" = \"quick\" ]")
        .unwrap_or_else(|| panic!("quick 早退分支定位失败"));
    let quick_exit = sh[quick_start..]
        .find("exit 0")
        .unwrap_or_else(|| panic!("quick 早退分支缺少 exit 0"));
    let quick = &sh[quick_start..quick_start + quick_exit];
    let env = quick
        .find("report_unexecuted_legs")
        .unwrap_or_else(|| panic!("quick 出口缺少环境缺失台账播报"));
    let mode = quick
        .find("report_mode_boundary")
        .unwrap_or_else(|| panic!("quick 出口缺少档位自述"));
    assert!(
        env < mode,
        "quick 出口必须先播报环境缺失台账, 再播报档位边界"
    );

    let boundary = sh
        .split("report_mode_boundary() {")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .unwrap_or_else(|| panic!("档位自述函数体定位失败"));
    let quick_line = boundary
        .lines()
        .find(|l| l.contains("档位: quick"))
        .unwrap_or_else(|| panic!("档位自述函数缺少 quick 行"));
    assert!(
        quick_line.contains("2d") && quick_line.contains("2e"),
        "quick 档位行未点名 full-only 步骤 —— 自述不足以纠正「零未执行 = 全跑过」的误读"
    );
    assert!(
        quick_line.contains("验收请用") && quick_line.contains("full"),
        "quick 档位行未指向 full 档 —— 读日志的人不知道验收该用哪档"
    );
    assert!(
        boundary.lines().any(|l| l.contains("档位: full")),
        "full 档位行缺失 —— full 收尾同样需要自报覆盖边界"
    );

    assert!(
        !sh.contains("本轮未执行腿数"),
        "旧的正向措辞「本轮未执行腿数」残留 —— 它会被读成「所有腿都跑过」(P-57 甲)"
    );
    assert_eq!(
        sh.matches("本轮因环境缺失未执行腿数").count(),
        2,
        "「因环境缺失未执行腿数」应同时出现在 0 与非 0 两个播报分支"
    );

    let mk = read(MAKEFILE_CI);
    assert!(
        mk.lines()
            .any(|l| l.trim_start().starts_with("ci:") && l.contains("ci-boot")),
        "聚合目标 ci 不再依赖 ci-boot —— P-57 丙 裁定保留重复 (聚合入口的 QEMU 硬失败面), \
         改动须重新裁定而非顺手去掉"
    );
}

/// P-58 结构锁: 废弃命名扫描必须是「全仓受控文本 + 历史面豁免 + 自测前置 + 阻断」形态.
///
/// 病灶 (P-58 测绘, 旧扫描器实测):
/// - **范围**: 按扩展名白名单 `rglob` ⇒ 无扩展名的 `Makefile` (10 行) 与 `.asm`
///   整体零覆盖; 另有「短行 < 50 字符跳过」启发式吞掉脚本里的路径字面值.
/// - **读数**: 不过滤工具自身 ⇒ 53 处 actionable 中 46 处是扫描器源文本 (86%);
///   且「写一段说明就涨几个点」(53 → 65 → 69), 读数不可解释、永不收敛.
/// - **判定**: 默认形态无条件退 0, 只有 `--actionable-only` 才判退出码 ⇒
///   接入 CI 就是「绿 ≠ 拦住」; `--include-archive` 实为 no-op.
/// 裁定 (用户): 废弃名彻底废弃 + 扫描器校正并升为阻断腿, 产物命名一律并入 Edgine.
///
/// 判据:
/// ① 范围由 `git ls-files` 驱动, 无扩展名白名单 / 短行跳过 / 无效开关残留;
/// ② 历史面 (docs/plan + docs/report) 命中只进 KEEP, 不进违规;
/// ③ 自路径排除, 且自测是每次扫描的前置 (检测器坏了不许给「零违规」);
/// ④ 判定二值 (0/1) + 读取失败 fail-closed (2);
/// ⑤ `ci/audit.sh` 的 full 档清单接线本脚本, 且 2e 不存在非阻断支路.
#[test]
fn test_p58_obsolete_naming_gate_is_repo_wide() {
    let py = read("../scripts/scan_obsolete_naming.py");
    let sh = read(AUDIT_SH);
    let agents = read(AGENTS_MD);

    // ① 全仓受控文本范围.
    assert!(
        py.contains("\"ls-files\", \"-z\"") || py.contains("ls-files"),
        "扫描范围不再由 git ls-files 驱动 —— 回到扩展名遍历会重新漏掉 Makefile / .asm"
    );
    for banned in [
        "rglob",
        ".suffix",
        "strip()) < 50",
        "--include-archive",
        "--actionable-only",
    ] {
        assert!(
            !py.contains(banned),
            "扫描器残留 {banned} —— 扩展名白名单 / 短行启发式 / 无效开关都是 P-58 测绘出的漏检源或假旋钮"
        );
    }
    assert!(
        py.contains("\"Makefile\"") && py.contains("tracked_files()"),
        "自测未验证 Makefile 在受控文件枚举内 —— 无扩展名文件正是旧口径的盲区"
    );

    // ② 历史面 KEEP 与违规分账.
    assert!(
        py.contains("HISTORY_PREFIXES = (\"docs/plan/\", \"docs/report/\")"),
        "历史面前缀不再是 docs/plan + docs/report —— 少一个即让执行当时的历史记录推红门禁"
    );
    assert!(
        py.contains("(kept if is_history(rel) else violations)"),
        "历史面命中未与违规分账 —— 历史归档会推动退出码, 口径重新变成不可收敛"
    );

    // ③ 自排除 + 自测前置.
    assert!(
        py.contains("SELF_PATH") && py.contains("rel == self_rel"),
        "扫描器不再排除自身 —— 工具源文本 (受检词 + 模式串) 会重新占据读数大头"
    );
    let selftest = py
        .find("failures = self_test()")
        .unwrap_or_else(|| panic!("扫描主流程缺少自测前置"));
    let collect = py
        .find("violations, kept, scanned = collect()")
        .unwrap_or_else(|| panic!("扫描主流程缺少 collect 调用"));
    assert!(
        selftest < collect,
        "自测在扫描之后才跑 —— 检测器退化时仍会先给出「零违规」读数"
    );
    assert!(
        py.contains("scan_text(rel, text)") && py.contains("SELF_TEST_CASES"),
        "自测没有真调检测器入口 —— 复制算法的自测在检测器退化时照样全绿 (P-50 ③ 同型)"
    );

    // ④ 二值判定 + fail-closed.
    assert!(
        py.contains("return 1 if violations else 0"),
        "退出码不再只由违规数决定 —— 阻断腿必须「有违规即红」"
    );
    assert!(
        py.contains("return 2") && py.contains("raise ScanError"),
        "扫描失败出口消失 —— 读取失败/枚举失败被静默 continue 就是 fail-open"
    );
    assert!(
        py.contains("BINARY_PROBE_BYTES") && py.contains("b\"\\0\""),
        "缺少二进制判定 —— 二进制文件按 utf-8 替换解码会产生假命中"
    );

    // ⑤ CI 接线与 §2.2 自述一致.
    let full_start = sh
        .find("AUDIT_REQUIRED_LEGS_FULL=\"")
        .map(|i| i + "AUDIT_REQUIRED_LEGS_FULL=\"".len())
        .unwrap_or_else(|| panic!("full 档腿清单定位失败"));
    let full_end = sh[full_start..]
        .find('"')
        .map(|i| full_start + i)
        .unwrap_or_else(|| panic!("full 档腿清单缺少收尾"));
    assert!(
        sh[full_start..full_end].contains("obsolete_naming:scan_obsolete_naming.py"),
        "full 档清单没有接线废弃命名腿 —— 扫描器退回零调用者 (P-49 同族)"
    );
    assert!(
        agents.contains("scan_obsolete_naming"),
        "AGENTS §2.2 未把该腿记为阻断 —— 文档仍描述成报告腿会让读者以为有残留也可过"
    );
}

/// P-59 结构锁: F8 的存在性必须由 rustc `missing_docs` 执法, 且两面都要有真实执法点.
///
/// 病灶 (P-59 测绘, 三条都是实测而非推断):
/// - AGENTS §5 F8 与 `docs/explain/spec-engineering.md` 4.4/10.2 都写「clippy
///   `missing_docs_in_crate_items` 强制」, 而该 lint 名**不存在** (实测 `cargo clippy`
///   报 `error[E0602]: unknown lint`); 真名 `clippy::missing_docs_in_private_items` 属
///   restriction 组, 门槛从不启用 ⇒ kernel crate 的编译器 F8 执法长期为**零**.
/// - `audit_public_api_docs.py` 的两面靠 `--strict` 分流, 而 CI 步骤 2e 按
///   `python3 scripts/<腿>.py` **无参**调起 ⇒ 533 处 impl 面存量在门禁里根本没有执法点.
/// - 脚本报告行号偏到声明前的空行 (`^\s*` 吞 `\n`), 实测 247 条错锚 —— 按行号施工会改错处.
/// 裁定 (用户): ① rustc `missing_docs` 作准; ② 533 处基线对账升阻断; ③ 行号本轮顺带修;
/// 追加「存量不应冻结 —— 既然会报就属于代码仍有问题」⇒ 债册是待办清单不是豁免面,
/// 每批补写完就把该模块转 `#![deny(missing_docs)]` 硬执法 (清零工程 P-66).
///
/// 判据:
/// ① 三处文档口径不再把不存在的 lint 当执法点, 且写明新的两层执法链;
/// ② 默认面与 impl 面在同一次裸调里都判得到 (`--strict` 分流必须消失);
/// ③ 债册腿以编译器为唯一尺、排除 vendored、用隔离 target 目录、失败 fail-closed;
/// ④ 已清零模块的 `#![deny(missing_docs)]` 在位, 且该模块不重复占债册;
/// ⑤ 到位断言的腿数由清单派生 (P-49 裁定: 禁止手写可由脚本派生的数字).
#[test]
fn test_p59_f8_is_enforced_by_rustc_missing_docs() {
    let agents = read(AGENTS_MD);
    let spec = read("../docs/explain/spec-engineering.md");
    let api = read("../scripts/audit_public_api_docs.py");
    let debt = read("../scripts/audit_missing_docs_debt.py");
    let sh = read(AUDIT_SH);

    // ① 文档口径: §5 F8 行与 4.4 都必须点名 rustc `missing_docs` 作准者.
    let f8_line = agents
        .lines()
        .find(|l| l.trim_start().starts_with("| F8 |"))
        .unwrap_or_else(|| panic!("AGENTS §5 找不到 F8 行"));
    assert!(
        f8_line.contains("rustc `missing_docs`") && f8_line.contains("audit_missing_docs_debt.py"),
        "§5 F8 的检查方式没落到编译器口径 + 债册腿 —— 回到「clippy 强制」就是宣称一条\
         不存在的门禁 (P-59 病灶本体)"
    );
    assert!(
        !f8_line.contains("missing_docs_in_crate_items"),
        "§5 F8 仍引用不存在的 lint 名 —— 该行是唯一执法链的入口, 失真会让整条 F8 无人执法"
    );
    let rule44 = spec
        .lines()
        .find(|l| l.contains("4.4 公共 API 必须有文档注释"))
        .unwrap_or_else(|| panic!("spec-engineering 找不到 4.4 行"));
    assert!(
        rule44.contains("rustc `missing_docs`") && rule44.contains("不存在的 lint 名"),
        "4.4 未把 F8 口径改为编译器作准 —— 编码规范与 §5 分叉即回到两处口径互不暴露"
    );
    let clippy_toml = read("../src/rust/clippy.toml");
    assert!(
        clippy_toml.contains("惰性配置"),
        "clippy.toml 的 missing-docs-in-crate-items 未注明它只被从不启用 restriction 组的\
         lint 读取 —— 留着「看起来像执法」的配置是本轮病灶的同型形态"
    );

    // ② 裸调即两面同判.
    assert!(
        !api.contains("\"--strict\"") && !api.contains("'--strict'"),
        "脚本又用 --strict 分流两面, 而 2e 无参调起 ⇒ impl 面 533 处存量重新失去执法点"
    );
    assert!(
        api.contains("default_violations") && api.contains("strict_violations"),
        "一次扫描不再同时派生默认面与 impl 面 —— 两面若各自单独扫描, 门禁只判到其中一面"
    );
    assert!(
        api.contains("return 2") && api.contains("基线快照缺失"),
        "基线不可用时不再 fail-closed —— 侧车被删就变成「零违规」假绿"
    );
    // 裁定 ③ (行号顺带修): 声明正则的行首空白只吞 [ \\t] —— 吞 `\\s` 会把声明前的空行吃进
    // match, 报告行号落到空行上 (实测 247 条错锚), 按行号施工即改错处. 只锁声明正则本身:
    // `_in_nonpub_mod` 的模块正则是另一种用法 (位置不用于报告), 不在此锁范围.
    assert!(
        api.contains(r"r'^[ \t]*(?P<vis>pub"),
        "声明正则不再是 `^[ \\\\t]` 起头 —— 报告行号会重新偏到声明前的空行 (P-59 裁定 ③ 的\
         修法失效)"
    );

    // ③ 债册腿的取数与射程.
    assert!(
        debt.contains("LINT = \"missing_docs\"") && debt.contains("f\"-D{LINT}\""),
        "债册腿不再以 rustc missing_docs 为唯一尺 —— 换回自定义口径就是 P-59 裁定的倒退"
    );
    assert!(
        debt.contains("--message-format=json"),
        "取数不是编译器结构化输出 —— 靠解析人读文本会随 rustc 文案变化而静默失明"
    );
    assert!(
        debt.contains("smoltcp"),
        "vendored smoltcp 不再排除 —— 锁定不可改的树会把债册变成不可能完成的清单"
    );
    assert!(
        debt.contains("other/target/audit-missing-docs")
            && !debt.contains("\"../../other/target\""),
        "债册腿借用了主 target 目录 —— 尾旗标参与 cargo 的 unit fingerprint, 会把门槛 1/2\
         的全部产物判脏 (实测主目录被搅脏后重编 kernel lib)"
    );
    assert!(
        debt.contains("raise ScanError") && debt.contains("return 2"),
        "取数失败不再 fail-closed —— cargo 缺失/编译中断会被读成「读数 0, 与债册不符」之外的\
         任何形态"
    );

    // ④ 已清零模块转硬执法, 且不双计.
    let errno = read("../src/kernel/privileged/errno.rs");
    assert!(
        errno.contains("#![deny(missing_docs)]"),
        "errno 模块的 F8 存量已清零却没有转硬执法 —— 该模块的新增未文档项会退回债册记账"
    );
    let debt_baseline = read("../scripts/audit_missing_docs_debt.baseline.json");
    assert!(
        !debt_baseline.contains("privileged/errno.rs"),
        "已加 #![deny(missing_docs)] 的模块仍占债册 —— 同一面被两处记账, 补写进度不可解释"
    );
    assert!(
        Path::new("../scripts/audit_public_api_docs.baseline.json").exists(),
        "impl 面侧车快照不在位 —— 2e 的 public_api_docs 腿会当场 RC=2"
    );

    // ⑤ 腿数由清单派生, 不许手写.
    assert!(
        sh.contains("AUDIT_LEG_FULL_TOTAL=$(set -- $AUDIT_REQUIRED_LEGS_FULL; echo $#)"),
        "full 档到位腿数被改成手写字面量 —— P-49 裁定禁止手写可由脚本派生的数字"
    );
}

/// P-60 结构锁: OnceCell 扩四树、F8 射程写明, 两处「自述与实装不符」缺陷不许回退.
///
/// 病灶 (P-60 立案测绘, HEAD 实测):
/// - `audit_once_cell.py` 自述「全项目统一 OnceCell 抽象」却只扫 `src/kernel/functions`;
///   另三树 (host-tests / src/user / kani-harness) 零覆盖, 扩根实测 0 违规 / 871 文件.
/// - `audit_safety_coverage.py` 注释称「过滤掉 arch/ 子目录」, 而 `discover_modules`
///   从 privileged/mod.rs 发现 `pub mod arch`、`collect_rs_files` 递归归集 ⇒ 注释与实装相反.
/// - `audit_c_naming.py` 读失败 (OSError / 非 UTF-8) 静默 continue ⇒ 扫描面静默缩小,
///   与 P-46「静默消失」同族, 只是粒度到了单文件.
/// - F8 射程从未写明: 编译器面只覆盖 `src/kernel` 三成员、语言面只覆盖 `privileged/`,
///   而 host-tests / src/user 实测 154 处缺中文 doc (默认面) 不在任何门禁射程 ⇒ 须显式裁定.
///
/// 判据:
/// ① once_cell 覆盖四树 + 循环由 BASES 驱动 + 根缺失 fail-closed;
/// ② safety_coverage 不再声称过滤 arch, 且 arch 确在实装射程 (`pub mod arch` 在位);
/// ③ c_naming 读失败进 read_errors 并影响退出码, 不再静默 continue;
/// ④ AGENTS §2.2 写明 F8 射程与「不在射程」的三树.
#[test]
fn test_p60_scope_closure_and_silent_skips_fixed() {
    let once_cell = read("../scripts/audit_once_cell.py");
    let safety = read("../scripts/audit_safety_coverage.py");
    let c_naming = read("../scripts/audit_c_naming.py");
    let agents = read(AGENTS_MD);

    // ① OnceCell 四树 + fail-closed.
    for root in [
        r#"PROJECT_ROOT / "src" / "kernel""#,
        r#"PROJECT_ROOT / "host-tests""#,
        r#"PROJECT_ROOT / "src" / "user""#,
        r#"PROJECT_ROOT / "kani-harness""#,
    ] {
        assert!(
            once_cell.contains(root),
            "audit_once_cell.py 的 BASES 缺覆盖根 {root} —— 「全项目统一」对三树重新失明 (P-60 甲)"
        );
    }
    assert!(
        once_cell.contains("for b in BASES"),
        "audit_once_cell.py 声明了 BASES 但扫描循环不遍历它 —— 加根不生效的静默形态"
    );
    assert!(
        once_cell.contains("覆盖根缺失 (fail-closed, P-60 甲)"),
        "audit_once_cell.py 缺「覆盖根缺失」fail-closed 出口 —— 根拼错会被读成扫描零违规"
    );

    // ② safety_coverage 注释与实装一致: 不再声称过滤 arch, 且 arch 确在扫描面.
    assert!(
        !safety.contains("过滤掉 arch/ 子目录"),
        "audit_safety_coverage.py 又写「过滤掉 arch/」而代码从未实现该过滤 —— 自述与实装不符 (P-60 丙)"
    );
    assert!(
        safety.contains("SAFETY 覆盖没有架构豁免"),
        "audit_safety_coverage.py 未写明 arch 在射程 —— 读者会按旧注释把 arch 当成被豁免面"
    );
    let privileged_mod = read("../src/kernel/privileged/mod.rs");
    assert!(
        privileged_mod.contains("pub mod arch;"),
        "privileged/mod.rs 不再声明 pub mod arch —— 「arch 在射程」的注释失去依据, 需重核"
    );

    // ③ c_naming 读失败不再静默.
    assert!(
        c_naming.contains("read_errors") && c_naming.contains("read_errors.append"),
        "audit_c_naming.py 读失败又退回静默跳过 —— 扫描面缩小在日志与退出码上都不可见 (P-60 丙)"
    );
    assert!(
        c_naming.contains("读取失败 (fail-closed, P-60 丙)")
            && c_naming.contains("if issues or read_errors"),
        "audit_c_naming.py 的读失败不进入退出码 —— 只打日志的 fail-closed 等于没接门禁"
    );

    // ④ AGENTS §2.2 的 F8 射程口径.
    let sec_22_start = agents
        .find("### 2.2 核心审计脚本（硬规则门槛）")
        .unwrap_or_else(|| panic!("AGENTS.md 找不到 §2.2 标题"));
    let sec_22_end = agents[sec_22_start..]
        .find("### 2.3 验证门槛")
        .map(|i| sec_22_start + i)
        .unwrap_or_else(|| panic!("AGENTS.md 找不到 §2.3 标题 (无法界定 §2.2)"));
    let sec_22 = &agents[sec_22_start..sec_22_end];
    for needle in [
        "F8 射程口径",
        "P-60 甲",
        "rustc `missing_docs`",
        "`audit_public_api_docs.py` 只判 `privileged/`",
        "不在 F8 射程",
        "`host-tests` / `src/user` / `kani-harness`",
    ] {
        assert!(
            sec_22.contains(needle),
            "AGENTS.md §2.2 缺 {needle} —— F8 射程仍只存在于脚本实装里, 文档宣称会再次大于实装"
        );
    }
}

/// P-67 结构锁: 债册腿必须自带 `missing_docs` 抑制的定点检查, 且在取数之前.
///
/// 病灶 (P-59 债册腿设计时读源, 本轮首次补做负控制): 债册只看编译器读数, 而
/// `#![allow(missing_docs)]` 会让读数下降 —— 「真补写」与「关灯」在门禁里同形,
/// 重导快照即可复绿. F9 的 `audit_dead_code_comment.py` 族清单只含 dead_code/unused*
/// 与 `clippy::`, 不含一般 rustc lint ⇒ 这一盏灯原本无人看.
///
/// 判据:
/// ① 扫描根 = kernel 树, vendored smoltcp 按路径组件排除;
/// ② 抑制族只含 allow/expect (`deny`/`forbid` 是 P-59 裁定 ④ 要求的合法硬执法);
/// ③ 属性实参里的字符串先剥掉再匹配 —— `reason = "missing_docs"` 不是抑制;
/// ④ 抑制检查在取数之前调用 —— 命中即零耗时转红, 不给「更绿的读数」出场机会;
/// ⑤ fail-closed: 扫描根缺失/读取失败走 return 2, 不是「扫不动就当没有」.
#[test]
fn test_p67_missing_docs_suppression_is_policed_by_debt_leg() {
    let debt = read("../scripts/audit_missing_docs_debt.py");

    // ① 射程.
    assert!(
        debt.contains("SCAN_ROOT = os.path.join(ROOT, \"src\", \"kernel\")"),
        "抑制扫描根不再是 kernel 树 —— 换了根就漏判一半子树"
    );
    assert!(
        debt.contains("VENDORED_DIRS = (\"smoltcp\",)"),
        "vendored smoltcp 不再按路径组件排除 —— 锁定不可改的树一旦出现抑制即恒红"
    );

    // ② 只判 allow/expect.
    let sup_line = debt
        .lines()
        .find(|l| l.trim_start().starts_with("SUPPRESSION_RE ="))
        .unwrap_or_else(|| panic!("缺少 SUPPRESSION_RE 定义"));
    assert!(
        sup_line.contains("(allow|expect)"),
        "抑制族不再是 allow/expect —— 判族一改, P-67 的射程随之漂移"
    );
    assert!(
        !sup_line.contains("deny") && !sup_line.contains("forbid"),
        "抑制正则收进了 deny/forbid —— 模块级 `#![deny(missing_docs)]` 会被误判成绕过"
    );

    // ③ 字符串先剥.
    assert!(
        debt.contains("STRING_RE = re.compile") && debt.contains("STRING_RE.sub"),
        "属性实参里的字符串未剥除 —— `reason = \"missing_docs\"` 会被读成抑制"
    );

    // ④ 在取数之前调用.
    let scan = debt
        .find("suppressions = scan_suppressions()")
        .unwrap_or_else(|| panic!("主流程没有调起抑制扫描 —— P-67 的检查退化成死函数 (F9)"));
    let compile = debt
        .find("entries = run_compiler()")
        .unwrap_or_else(|| panic!("主流程缺少取数调用"));
    assert!(
        scan < compile,
        "抑制检查在取数之后 (scan={scan} compile={compile}) —— 先拿到「更绿的读数」再报抑制, 顺序不承重"
    );

    // ⑤ fail-closed.
    assert!(
        debt.contains("抑制扫描失败") && debt.contains("return 2"),
        "抑制扫描失败没有 fail-closed 出口 —— 扫不动会被读成「无抑制」假绿"
    );
}
