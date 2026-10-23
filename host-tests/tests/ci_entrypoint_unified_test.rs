//! B: CI 入口统一 (Makefile.ci 薄封装 + ci/audit.sh 权威化)
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-11.
//!
//! 旧 Makefile.ci 平行实现了一套弱化判据 (只跑 3 个审计脚本、`grep "^error["`
//! 漏 warning 与 link 阶段、dev profile、`cmd | tee` 把失败掩盖成成功), 与本地
//! 权威入口 ci/build.sh + ci/audit.sh 分叉; 而 AGENTS.md §2.2 宣称「强制」的
//! F1+F2 / F4 / F8 三项审计当时只接入了 GitHub workflow, 本地一键门禁反而不跑.
//! 本测试锁住收口后的形态: Makefile.ci 只委派, ci/audit.sh 是 §2.2 的完整落地.

use std::fs;
use std::path::Path;

const MAKEFILE_CI: &str = "../Makefile.ci";
const AUDIT_SH: &str = "../ci/audit.sh";
const BUILD_SH: &str = "../ci/build.sh";
const AGENTS_MD: &str = "../AGENTS.md";

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
    // AGENTS.md §2.2 把 F1+F2 / F4 / F8 / F3 列为强制门禁: 权威入口必须真跑这四项.
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
/// ① `AUDIT_REQUIRED_LEGS_FULL` 是 `腿名:相对路径` 成对的双引号字面量, ≥8 条
///    (实测 8 = 5 阻断腿 + 2 自测腿 + 1 报告腿; 第 8 条 audit_selftest 由 P-50 ③ 追加);
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
        specs.len() >= 8,
        "AUDIT_REQUIRED_LEGS_FULL 只点名 {} 条腿, 低于实测的 8 条 (5 阻断腿 + 2 自测腿 + 1 报告腿)",
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
        block.contains("elif (cd \"$PROJECT_ROOT\" && python3"),
        "步骤 2e 里阻断腿不再走「判退出码 + 锚定仓库根」的调起形态 —— 所有腿一起变成只播报读数 \
         (审计绿≠门禁绿, P-47/48 同族), 或改成借外层 cwd (P-50 扫描根同族)"
    );
    assert!(
        block.contains("err \"审计腿"),
        "步骤 2e 里阻断腿的非零出口消失 —— 判了退出码却不阻断, 等于没判"
    );

    // ⑤ 报告腿: 非阻断形态必须显式点名 + 写明不判退出码 + 指向另立案号.
    let report_label = block
        .lines()
        .find(|l| l.contains("报告腿"))
        .unwrap_or_else(|| {
            panic!(
                "步骤 2e 里没有「报告腿」标注 —— 报告腿被并入阻断腿时接入即恒红 \
                                   (实装前实测 scan_antx_residue.py --actionable-only 53 处), \
                                   把它悄悄降级又不标注是本锁要拦的形态"
            )
        });
    assert!(
        report_label.contains("不判退出码"),
        "报告腿标注漏了「不判退出码」 —— 日志把报告腿写成门禁, 是 P-47/48 收口的「绿≠拦住」同族"
    );
    assert!(
        report_label.contains("P-58"),
        "报告腿标注未指向另立案号 P-58 —— 非阻断接入产生的残余债必须登记, 不许只留在日志里"
    );
    assert!(
        block.contains("= \"antx_residue\" ]"),
        "报告腿豁免改成无条件形态 (不再按腿名点名) —— 谁都能被塞进非阻断支路"
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
    ] {
        assert!(
            sec_22.contains(needle),
            "AGENTS.md §2.2 缺 {needle} —— 定点锁边界 (P-50 甲) 或新接线腿未自述"
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
        .find("step \"4/6 Lockbud")
        .unwrap_or_else(|| panic!("ci/audit.sh 缺少 Lockbud 腿 (P-47 状态分离失去执法路径)"));
    let rest = &sh[after_step..];
    let leg_end = rest.find("\n# ── 5.").unwrap_or_else(|| {
        panic!("Lockbud 腿与下一条腿 (步骤 5) 的分界标记丢失, 结构锁无法定位块")
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
