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
        required.len() >= 18,
        "AUDIT_REQUIRED_LEGS 只点名 {} 条腿, 低于实测的 18 条 (17 被包装 + coupling)",
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
