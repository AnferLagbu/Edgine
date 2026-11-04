// SPDX-License-Identifier: MPL-2.0
//! framekernel-bench 基线记录器回归锁 (fc45 宿主复录批).
//!
//! 口径来源: `docs/plan/multithreading-project.md` §7 台账「2026-11-04 宿主环境
//! 跨版本升级」条的 bench 基线部分.
//!
//! 本锁钉的是**记录器**的行为, 不是 ×10 预算倍数本身 (那由 `perf_metric_test.rs` 钉).
//! 触发原因是实测到的失效形态:
//! fc45 上按单样本重录后, `attribution_classify` 被一次离群低读数钉成 5.268ns,
//! 而该项 5 次真实采样落在 5.177–7.423ns (1.43 倍), 于是当时的相对判据腿对刚提交的
//! 基线当场自红 (+41.7%). 短测量窗条目 (迭代数 × 单次耗时 < 1ms) 的抖动本来就大,
//! 单样本基线把抖动写成了判据.
//!
//! 三条静默通道逐处收口:
//!
//! 1. **聚合口径漂移**: 记录器若改用 min / mean, 基线会被系统性压低或拉高, 而
//!    性能指标腿仍绿 —— 由 `--self-test` 真调 `aggregate_runs` 的判定用例锁死
//!    (自检在 `main()` 里先于任何写文件执行, 自检红 ⇒ 不产基线).
//! 2. **跨运行漂移**: N 次采样里条目名集合或 `iterations` 变了还取中位数, 等于
//!    对不同工作量的读数求中位并静默改变基线覆盖面 —— `aggregate_runs` fail-closed.
//! 3. **基线覆盖面缩水**: 判据腿只对「基线里有的条目」打分 ⇒ 条目越少越"干净",
//!    空基线会让该腿恒绿 —— 用「基线条目数 == bench 源码 `measure(` 调用点数」
//!    跨源钉死 (`perf_metric_test.rs` 的运行期例另做双向对账).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const RECORDER_PY: &str = "scripts/record_bench_baseline.py";
const PERF_TEST_RS: &str = "host-tests/tests/perf_metric_test.rs";
const BENCH_RS: &str = "host-tests/src/framekernel_bench.rs";
const BASELINE_JSON: &str = "host-tests/benches/baseline.json";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 真跑记录器自检 (不复制算法: 自检按路径加载 `aggregate_runs` / `build_baseline`).
fn run_recorder_self_test() -> (Option<i32>, String) {
    let out = Command::new("python3")
        .arg(repo_root().join(RECORDER_PY))
        .arg("--self-test")
        .output()
        .expect("启动 python3 失败");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), text)
}

#[test]
fn bench_recorder_self_test_passes() {
    let (code, text) = run_recorder_self_test();
    assert_eq!(
        code,
        Some(0),
        "记录器自检必须通过 (失败即不产基线):\n{text}"
    );
    assert!(
        text.contains("自检通过"),
        "自检没有打印「自检通过」—— --self-test 可能被改成空跑:\n{text}"
    );
}

#[test]
fn bench_recorder_aggregation_is_median_of_runs() {
    let py = read_src(RECORDER_PY);
    // 聚合口径: 中位数, 不是 min / mean / 首个样本.
    assert!(
        py.contains("statistics.median("),
        "record_bench_baseline.py 不再用 statistics.median 聚合 —— 换成 min 会压低基线、\
         换成 mean 会让单次离群读数重新进入基线, 两种退化 ci-bench 都看不出来"
    );
    // --runs 存在且缺省为 1 (老调用点行为不变, 按裁定).
    assert!(
        py.contains("\"--runs\"") && py.contains("default=1,"),
        "record_bench_baseline.py 缺 `--runs` 或其缺省值不再是 1 —— 裁定要求默认保持\
         单样本的历史行为"
    );
    assert!(
        py.contains("if args.runs < 1:"),
        "record_bench_baseline.py 放行 --runs 0/负数 —— 零采样会写出空基线"
    );
    // 采样次数落档, 让读者能分辨单样本钉下与中位数钉下.
    assert!(
        py.contains("\"runs_per_item\": runs,"),
        "基线不再记录 runs_per_item —— 覆盖面条目看不出是几次采样取的中位"
    );
    // 自检必须真调被测函数, 而不是复制一份中位数算法.
    for entry in ["aggregate_runs(", "build_baseline("] {
        let body = py
            .split("def self_test()")
            .nth(1)
            .unwrap_or_else(|| panic!("record_bench_baseline.py 缺 self_test()"))
            .split("def main(")
            .next()
            .unwrap_or("");
        assert!(
            body.contains(entry),
            "self_test() 不再真调 {entry} —— 复制算法后聚合器退化时自检仍绿"
        );
    }
    // 离群不敏感 + 偶数采样取中间两值均值, 两条判定用例都必须在.
    for needle in [
        "离群高读数不抬高基线",
        "离群低读数不压低基线",
        "偶数采样取中位",
        "单样本透传 ns[0]",
    ] {
        assert!(
            py.contains(needle),
            "自检缺判定用例 `{needle}` —— 对应形态的聚合退化不再被发现"
        );
    }
}

#[test]
fn bench_recorder_fails_closed_on_run_drift() {
    let py = read_src(RECORDER_PY);
    for needle in [
        "if items.keys() != base_items.keys():",
        "item[\"iterations\"] != base_items[name][\"iterations\"]",
    ] {
        assert!(
            py.contains(needle),
            "record_bench_baseline.py 的跨运行漂移判据 `{needle}` 丢失 —— \
             N 次采样漂移时取中位会静默改变基线覆盖面 / 混算不同工作量"
        );
    }
    assert!(
        py.contains("if not reports:") && py.contains("if not items:"),
        "record_bench_baseline.py 不再拒绝空读数 —— 空基线会让回归腿恒绿"
    );
    // 负控制由 self_test 的 _expect_raises 承担, 且必须期望 ValueError (fail-closed).
    assert!(
        py.contains("期望 fail-closed 抛 ValueError"),
        "自检的 fail-closed 负控制丢失 —— 漂移判定被改成静默容忍后无人知晓"
    );
}

#[test]
fn committed_baseline_records_median_and_full_coverage() {
    let text = read_src(BASELINE_JSON);
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("host-tests/benches/baseline.json 必须是合法 JSON");

    // ① provenance: fc45 复录后的环境戳 + 每项采样次数.
    assert!(
        value["machine"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "baseline.json 的 machine 戳为空 —— 跨宿主对比时无法归因"
    );
    let runs = value["runs_per_item"]
        .as_u64()
        .unwrap_or_else(|| panic!("baseline.json 缺 runs_per_item —— 记录器不再声明采样次数"));
    assert!(
        runs >= 1,
        "baseline.json 的 runs_per_item={runs} < 1 —— 零采样的基线没有读数来源"
    );

    // ② 覆盖面: 基线条目数 == bench 源码的 measure( 调用点数.
    let results = value["results"]
        .as_array()
        .unwrap_or_else(|| panic!("baseline.json 的 results 不是数组"));
    assert!(
        !results.is_empty(),
        "baseline.json 的 results 为空 —— 空基线会让 ci-bench 对任何回归都判绿"
    );
    let bench_src = read_src(BENCH_RS);
    let call_sites = bench_src.matches("measure(").count() as u64;
    assert_eq!(
        results.len() as u64,
        call_sites,
        "baseline.json 条目数 ({}) != framekernel_bench.rs 的 measure( 调用点数 ({}) —— \
         有 bench 掉出基线; 判据腿只对基线里有的条目打分, 条目丢失不会让它变红",
        results.len(),
        call_sites
    );
    let mut names: Vec<&str> = results
        .iter()
        .map(|r| r["name"].as_str().expect("基线条目必须有 name"))
        .collect();
    names.sort_unstable();
    let unique = {
        let mut v = names.clone();
        v.dedup();
        v.len()
    };
    assert_eq!(
        unique,
        names.len(),
        "baseline.json 有重名条目 —— 聚合会把两个条目悄悄合并"
    );

    // ③ 字段自洽: ps_per_op 与 ns_per_op_frac 同源 (bench 侧 ps == ns * 1000).
    for r in results {
        let name = r["name"].as_str().expect("基线条目必须有 name");
        let ns = r["ns_per_op_frac"]
            .as_f64()
            .unwrap_or_else(|| panic!("条目 {name} 缺 ns_per_op_frac (f64)"));
        // as_f64 对 JSON 整数同样有效, 免去 i64→f64 的显式转换.
        let ps = r["ps_per_op"]
            .as_f64()
            .unwrap_or_else(|| panic!("条目 {name} 缺 ps_per_op (整数皮秒)"));
        assert!(
            ps.fract() == 0.0 && ps >= 0.0,
            "条目 {name} 的 ps_per_op={ps} 不是非负整数皮秒"
        );
        assert!(
            (ps / 1000.0 - ns).abs() <= 0.0005,
            "条目 {name} 的 ps_per_op 与 ns_per_op_frac 不自洽 (ps={ps}, ns={ns}) —— \
             两者由同一次读数派生, 分叉说明聚合改了一个没改另一个"
        );
        assert!(
            r["iterations"].as_u64().unwrap_or(0) > 0,
            "条目 {name} 的 iterations 为 0 —— 该条目没有实际工作量"
        );
    }

    // ④ 判定侧仍按同一字段取数 (记录器与判据腿不得各写一套字段名).
    let perf_test = read_src(PERF_TEST_RS);
    assert!(
        perf_test.contains("item[\"ns_per_op_frac\"]") && perf_test.contains("budget_ns_per_op"),
        "perf_metric_test.rs 不再读 ns_per_op_frac / budget_ns_per_op —— 与记录器写入的\
         字段不同源, 预算判定会对着不存在的字段放行"
    );
}

#[test]
fn bench_recorder_preserves_sched_criteria_on_re_record() {
    // sched_criteria 是 K-07b 批3 的 host 判据来源; 重录性能读数时丢掉它, 丢的
    // 不是基线而是另一条测试的判据 —— sched_observability_k07b_test 会当场红,
    // 但那已经是"别人家的腿", 故这里在记录器侧就钉住.
    let py = read_src(RECORDER_PY);
    assert!(
        py.contains("baseline[\"sched_criteria\"] = sched_criteria"),
        "record_bench_baseline.py 不再写回 sched_criteria —— 重录性能基线会静默删掉调度判据"
    );
    let text = read_src(BASELINE_JSON);
    let value: serde_json::Value = serde_json::from_str(&text).expect("baseline.json 必须合法");
    for key in [
        "lag_bound_permille",
        "wake_credit_bound_permille",
        "wake_turnaround_schedules",
        "fairness_window_ticks",
        "fairness_spread_permille",
    ] {
        assert!(
            value["sched_criteria"].get(key).is_some(),
            "baseline.json 的 sched_criteria 缺 {key} —— fc45 复录把 K-07b 批3 判据洗掉了"
        );
    }
}
