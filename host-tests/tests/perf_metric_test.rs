// SPDX-License-Identifier: MPL-2.0
//! 性能指标测试腿 (perf metric gate) — 绝对预算断言 + 指标随时可查.
//!
//! 本文件把「性能门禁」从**相对判据**换成**测试项**: 原 `check_bench_regression.py`
//! 拿单份读数与基线比 15%, 于是宿主当下状态也被读成回归. 实测形态 (fc45 台账登记):
//! 同一份代码交替 5 轮 ⇒ 旧基线 2/5 红、中位数基线 3/5 红, 红项随宿主 load
//! 0.91–1.83 轮换; 24 项里 13 项的总测量窗 < 1ms, 而唯一 ≥10ms 的 `bitmap_scan`
//! (45ms 窗) 也出现过 +20% 摆幅 ⇒ 放大窗口救不了相对判据.
//!
//! 现在的判据: 每项一个**绝对预算** (`baseline.json` 的 `budget_ns_per_op`,
//! 由记录器按「中位读数 × 10 向上取整到两位有效数字」钉). ×10 的取舍是「只拦
//! 数量级走样」—— 热路径多出一次分配、循环由 O(n) 变 O(n²) 这类真劣化通常是
//! ×100 级, 而环境噪声是 ×1.4 级.
//!
//! 三条失效形态各有对应:
//!
//! 1. **预算被手改** (收紧 = 把判据调严到误报, 放宽 = 把腿调成恒绿): 由
//!    `perf_baseline_budget_follows_the_recorded_median` 真调记录器自身的
//!    `budget_ns_per_op()` 逐条对账, 本文件不复制取整算法.
//! 2. **接线退化** (腿被改成 debug 档跑 ⇒ 拿 release 预算判 debug 读数):
//!    `perf_metric_gate_is_wired_to_the_release_profile` 钉 `Makefile.ci` 与
//!    `ci/audit.sh` 两侧都显式 `--release` 且点名本测试目标.
//! 3. **覆盖面分叉** (bench 加了条目而基线没有, 或基线留着我们不再测的条目):
//!    本文件唯一的运行期例 `perf_metrics_within_absolute_budget` 做双向对账,
//!    缺预算与无读数都判红 (原脚本对缺失条目只打 WARN, 属静默绿通道).
//!
//! 运行期例只在 release 档存在 (`cfg(not(debug_assertions))`): `make test-host`
//! 是 debug 档 (Makefile 的 `cargo test --quiet`), debug 代码比 release 慢一个
//! 数量级, 用 release 读数钉下的预算去判它只会造出假红. debug 档仍跑上面两条
//! 静态例, 即本目标的工件面与接线面在两种 profile 下都有读数.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
// 只被 release 档的运行期例消费, 与它同 cfg: 顶层无条件导入会在 `make test-host`
// (debug 档) 下报 `unused_imports`, 而 F9 禁止用标注抑制, 只能让导入跟着唯一消费者走.
#[cfg(not(debug_assertions))]
use std::collections::{BTreeMap, BTreeSet};

const BASELINE_JSON: &str = "host-tests/benches/baseline.json";
const RECORDER_PY_DIR: &str = "scripts";
const MAKEFILE_CI: &str = "Makefile.ci";
const AUDIT_SH: &str = "ci/audit.sh";
const RECORDER_PY: &str = "scripts/record_bench_baseline.py";

/// 预算规则的对账脚本: 逐项要求工件里的 `budget_ns_per_op` **等于**记录器按
/// 「中位 × 倍数并向上取整」算出的值. 规则只在记录器一处定义, 这里按路径 import
/// 真函数, 不在 Rust 侧复制取整 (复制一份就会有两处会漂移的判据).
const BUDGET_RULE_CHECK_PY: &str = r#"
import json, sys
sys.path.insert(0, sys.argv[1])
from record_bench_baseline import budget_ns_per_op

with open(sys.argv[2], encoding="utf-8") as handle:
    data = json.load(handle)
bad = []
for item in data["results"]:
    want = budget_ns_per_op(item["ns_per_op_frac"])
    got = item.get("budget_ns_per_op")
    if got != want:
        bad.append(f"{item['name']}: 工件预算 {got!r} != 规则值 {want!r}")
if bad:
    print("\n".join(bad))
    sys.exit(1)
print(f"同源: {len(data['results'])} 条预算均由记录器规则推出")
"#;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

fn run_python(script: &str, args: &[String]) -> (Option<i32>, String) {
    let out = Command::new("python3")
        .arg("-c")
        .arg(script)
        .args(args)
        .output()
        .expect("启动 python3 失败");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), text)
}

/// 工件里的 name -> `budget_ns_per_op` 表 (`serde_json` 读数, 缺字段即 panic 并指名条目).
#[cfg(not(debug_assertions))]
fn budget_table(value: &serde_json::Value) -> BTreeMap<String, f64> {
    let mut table = BTreeMap::new();
    for item in value["results"]
        .as_array()
        .unwrap_or_else(|| panic!("{BASELINE_JSON} 的 results 不是数组"))
    {
        let name = item["name"]
            .as_str()
            .unwrap_or_else(|| panic!("{BASELINE_JSON} 有条目缺 name"))
            .to_string();
        let budget = item["budget_ns_per_op"]
            .as_f64()
            .unwrap_or_else(|| panic!("条目 {name} 缺 budget_ns_per_op —— 判据来源消失"));
        assert!(
            budget.is_finite() && budget > 0.0,
            "条目 {name} 的预算 {budget} 不是正有限值 —— 0/NaN/负值都让该项恒判绿"
        );
        let duplicated = table.insert(name.clone(), budget).is_some();
        assert!(
            !duplicated,
            "条目 {name} 在基线里出现两次 —— 两份预算会被悄悄合并"
        );
    }
    table
}

#[test]
fn perf_baseline_budget_follows_the_recorded_median() {
    let baseline_path = repo_root().join(BASELINE_JSON);
    let (code, text) = run_python(
        BUDGET_RULE_CHECK_PY,
        &[
            repo_root().join(RECORDER_PY_DIR).display().to_string(),
            baseline_path.display().to_string(),
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "基线预算与记录器规则不同源 (判据可被手改松/紧):\n{text}"
    );
    assert!(
        text.contains("同源"),
        "对账脚本没有报出同源读数, 实际输出:\n{text}"
    );

    // 工件自身的字段面: 记录器改字段名而测试不跟着改, 会让上面的对账读到空表.
    let source = read_src(RECORDER_PY);
    assert!(
        source.contains("\"budget_ns_per_op\": budget_ns_per_op(r[\"ns_per_op_frac\"])"),
        "{RECORDER_PY} 不再把预算写进基线条目 —— 判据来源会随重录消失"
    );
    assert!(
        source.contains("BUDGET_MULTIPLIER = 10.0"),
        "{RECORDER_PY} 的预算倍数不再是 ×10 —— 改倍数属判据口径变更, 需重新裁定"
    );
}

#[test]
fn perf_metric_gate_is_wired_to_the_release_profile() {
    let makefile = read_src(MAKEFILE_CI);
    assert!(
        makefile.contains("--release --test perf_metric_test"),
        "{MAKEFILE_CI} 的 ci-bench 不再以 release 档跑本目标 —— debug 读数会被拿去判 \
         release 预算, 造出与代码无关的假红"
    );

    let audit = read_src(AUDIT_SH);
    assert!(
        audit.contains("2f/6"),
        "{AUDIT_SH} 缺 full 档的性能指标腿 (步骤 2f) —— 本腿若不进验收门槛, \
         就退化成「没人跑就不说话」的按需工具"
    );
    // 同条命令里必须同时出现 `--release` 与本目标名: 只断言「文件里有 perf_metric_test」
    // 会放过「audit 侧摘掉 --release」的退化 —— debug 档下本目标只剩两条静态例,
    // 预算判定例连编译都不参与, 该腿就变成「绿着不判」的静默通道.
    assert!(
        audit
            .lines()
            .any(|line| line.contains("perf_metric_test") && line.contains("--release")),
        "{AUDIT_SH} 的性能指标腿不再在同一条命令里带 `--release` —— 该腿会在 debug 档下 \
         只剩静态例并照样打绿"
    );
}

/// 运行期例: 只在 release 档存在 (理由见文件头).
/// `run_all` 的导入放在函数内: 顶层导入在 debug 档下无人使用, 会造出 `unused_imports`
/// 警告 (F9 禁止用标注抑制, 只能让导入与它唯一的消费者同 cfg).
#[cfg(not(debug_assertions))]
#[test]
fn perf_metrics_within_absolute_budget() {
    use edgine_host_tests::framekernel_bench::run_all;
    let raw = fs::read_to_string(repo_root().join(BASELINE_JSON))
        .unwrap_or_else(|e| panic!("无法读取 {BASELINE_JSON}: {e}"));
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("host-tests/benches/baseline.json 必须是合法 JSON");
    let budgets = budget_table(&value);

    let report = run_all();
    let mut lines = vec![format!(
        "\n[perf] {} 项实测 vs 绝对预算 (budget = 中位 × 10):",
        report.results.len()
    )];
    lines.push(format!(
        "  {:<24} {:>12} {:>12} {:>8} {}",
        "name", "ns/op", "budget", "占比", "判定"
    ));
    let mut judged = 0usize;
    let mut violations = Vec::new();

    for entry in &report.results {
        let Some(budget) = budgets.get(&entry.name) else {
            violations.push(format!(
                "{} 无预算条目 —— 基线覆盖面与 bench 条目集分叉",
                entry.name
            ));
            continue;
        };
        let ratio = entry.ns_per_op_frac / budget;
        let measured = entry.ns_per_op_frac;
        let verdict = if ratio > 1.0 {
            "超预算"
        } else {
            "在预算内"
        };
        lines.push(format!(
            "  {:<24} {:>12.3} {:>12.2} {:>7.1}% {}",
            entry.name,
            measured,
            budget,
            ratio * 100.0,
            verdict
        ));
        judged += 1;
        if ratio > 1.0 {
            violations.push(format!(
                "{} 实测 {measured:.3}ns 超预算 {budget:.2}ns ({:.1}%)",
                entry.name,
                ratio * 100.0
            ));
        }
    }

    // 反向覆盖面: 基线里有而本轮没测到的条目 (bench 侧删了一项而基线没跟着清).
    let measured_names: BTreeSet<String> = report.results.iter().map(|e| e.name.clone()).collect();
    for name in budgets.keys() {
        if !measured_names.contains(name) {
            violations.push(format!(
                "基线条目 {name} 在 bench 里已不存在 —— 覆盖面静默缩水"
            ));
        }
    }

    println!("{}", lines.join("\n"));
    assert!(
        judged > 0,
        "[perf] 一项都没判 ⇒ 基线与 bench 条目集完全不相交, 这不是绿而是判据消失"
    );
    assert!(
        violations.is_empty(),
        "[perf] 性能指标超预算 / 覆盖面分叉 ({} 项):\n{}",
        violations.len(),
        violations.join("\n")
    );
}
