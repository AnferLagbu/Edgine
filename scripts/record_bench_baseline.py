#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Edgine Contributors
#
# record_bench_baseline.py — 记录 framekernel-bench 当前的性能基线
#
# 用法:
#   python3 scripts/record_bench_baseline.py [out_path] [--runs N]
#
# 行为:
#   1. 调用 `cargo run --release --bin framekernel-bench` 收集 JSON 结果 N 次
#   2. 按条目取 N 次读数的**中位数**作为基线 (N=1 时即该次读数, 与历史行为一致)
#   3. 由中位数派生每项的**绝对预算** budget_ns_per_op = 中位 × 10 向上取整到
#      两位有效数字 (判据形态见下「为什么是绝对预算」)
#   4. 写入 out_path (默认 host-tests/benches/baseline.json)
#   5. 同时打印简洁摘要
#
# 为什么取中位数: 单样本基线会把一次离群的低读数钉成判据, 之后正常读数即被判成
# 回归 (fc45 宿主复录实测: attribution_classify 5 次采样 5.177–7.423ns、1.43 倍
# 摆幅, 据此钉下的基线当场让 ci-bench 自红). 摆幅来自「迭代数 × 单次耗时 < 1ms」
# 的短测量窗, 中位数对单点离群不敏感.
#
# 为什么是绝对预算而不是相对 15%: 相对判据把「宿主当下的状态」也读成回归 —— 同一
# 份代码交替 5 轮的实测假红率 2/5 至 3/5, 红项随 load 轮换 (fc45 台账登记). 且本
# 仓 24 项里 13 项的总测量窗 < 1ms、唯一 ≥10ms 的 bitmap_scan (45ms) 也出现过
# +20% 摆幅, 即「放大窗口」并不能救相对判据. 绝对预算 (×10) 只拦数量级走样 ——
# 热路径多出一次分配 / 循环由 O(n) 变 O(n²) 通常是 ×100 级, 而环境噪声是 ×1.4 级.
# 判据的执行侧见 host-tests/tests/perf_metric_test.rs (release 档).
#
# 基线文件结构 (与 BenchReport 兼容):
#   {
#     "version": 1,
#     "machine": "<hostinfo>",
#     "rustc": "<rustc version>",
#     "runs_per_item": N,
#     "recorded_at": "<ISO8601>",
#     "results": [ { "name": "...", "category": "...", "iterations": ...,
#                    "ns_per_op_frac": ..., "ps_per_op": ...,
#                    "budget_ns_per_op": ... }, ... ]
#   }

import argparse
import json
import math
import os
import platform
import statistics
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "host-tests" / "benches" / "baseline.json"
HOST_TESTS_DIR = REPO_ROOT / "host-tests"

# 绝对预算的钉法 (单源; perf_metric_test.rs 的另一半判据「预算不得被悄悄收紧」
# 按同一倍数断言, 改这里必须同批改那边).
BUDGET_MULTIPLIER = 10.0
# 预算向上取整到的有效数字位数 —— 只为让工件里的数字可读, 不改变「×10」的量级判据.
BUDGET_SIG_DIGITS = 2
# 预算下限 (ns): 读数为 0 的条目 (编译器完全优化掉) 若按 ×10 得到 0 预算, 任何
# 亚纳秒抖动都会把它判红 —— 那正是本文件要消灭的形态. 取下限值让「零成本条目」
# 仍能吸收 10ps 量级的读数地板, 而任何真实回退 (>100ps) 照旧打红.
BUDGET_FLOOR_NS = 0.01


def budget_ns_per_op(median_ns: float) -> float:
    """中位读数 × BUDGET_MULTIPLIER, 向上取整到 BUDGET_SIG_DIGITS 位有效数字."""
    raw = median_ns * BUDGET_MULTIPLIER
    if raw <= 0.0:
        return BUDGET_FLOOR_NS
    exponent = math.floor(math.log10(raw))
    scale = 10.0 ** (exponent - (BUDGET_SIG_DIGITS - 1))
    ceiled = math.ceil(raw / scale) * scale
    # scale < 1 时浮点乘会留下 3.0000000000000004 一类的尾数, 取 12 位抹掉
    return float(round(max(ceiled, BUDGET_FLOOR_NS), 12))


def run_bench() -> dict:
    """调用 cargo run --release 收集 bench JSON, 解析并返回 dict."""
    print("[record_bench_baseline] running framekernel-bench (release)...", file=sys.stderr)
    proc = subprocess.run(
        ["cargo", "run", "--release", "--bin", "framekernel-bench"],
        cwd=str(HOST_TESTS_DIR),
        capture_output=True,
        text=True,
        check=True,
    )
    # 最后一行为 JSON (前面的 cargo 输出走 stderr)
    json_line = proc.stdout.strip().splitlines()[-1]
    try:
        return json.loads(json_line)
    except json.JSONDecodeError as e:
        print(f"ERROR: 无法解析 bench 输出: {e}", file=sys.stderr)
        print(f"raw stdout: {proc.stdout!r}", file=sys.stderr)
        raise


def _items_by_name(report: dict, label: str) -> dict[str, dict]:
    """把一份 BenchReport 的 results 转成 name → 条目; 重名即失败 (重名会让中位数悄悄合并两个条目)."""
    items: dict[str, dict] = {}
    for r in report.get("results", []):
        name = r["name"]
        if name in items:
            raise ValueError(f"{label}读数里条目 {name} 重复, 无法按条目聚合")
        items[name] = r
    if not items:
        raise ValueError(f"{label}读数没有任何条目")
    return items


def aggregate_runs(reports: list[dict]) -> dict:
    """把 N 次 bench 读数按条目取中位数, 合成一份 BenchReport 形态的 dict.

    fail-closed 面: 条目名集合在两次运行间漂移 (会静默丢条目) 或同一条目的
    iterations 漂移 (对不同工作量的读数取中位数没有意义) 都直接抛错, 不写基线.
    """
    if not reports:
        raise ValueError("aggregate_runs: 没有任何 bench 读数")
    base_items = _items_by_name(reports[0], "第 1 次")
    ns_values: dict[str, list[float]] = {n: [r["ns_per_op_frac"]] for n, r in base_items.items()}
    for idx, report in enumerate(reports[1:], start=2):
        items = _items_by_name(report, f"第 {idx} 次")
        if items.keys() != base_items.keys():
            lost = base_items.keys() - items.keys()
            gained = items.keys() - base_items.keys()
            raise ValueError(
                f"第 {idx} 次读数条目集合与第 1 次不一致 (缺失 {sorted(lost)}, 新增 {sorted(gained)}); "
                "跨运行漂移时取中位数会静默改变基线覆盖的条目"
            )
        for name, item in items.items():
            if item["iterations"] != base_items[name]["iterations"]:
                raise ValueError(
                    f"条目 {name} 的 iterations 在两次运行间漂移 "
                    f"({base_items[name]['iterations']} -> {item['iterations']}); "
                    "对不同工作量的读数取中位数没有意义"
                )
            ns_values[name].append(item["ns_per_op_frac"])

    results = []
    for name, item in base_items.items():
        # ps_per_op 由中位 ns 反推, 保持 bench 侧的 ps == ns * 1000 自洽约定
        median_ns = statistics.median(ns_values[name])
        entry = dict(item)
        entry["ns_per_op_frac"] = median_ns
        entry["ps_per_op"] = int(round(median_ns * 1000))
        results.append(entry)
    return {"version": reports[0].get("version", 1), "results": results}


def build_baseline(report: dict, sched_criteria: dict | None = None, runs: int = 1) -> dict:
    """从 BenchReport 提取基线条目, 附加机器 / 编译器信息."""
    entries = []
    for r in report.get("results", []):
        entries.append({
            "name": r["name"],
            "category": r["category"],
            "iterations": r["iterations"],
            "ns_per_op_frac": r["ns_per_op_frac"],
            "ps_per_op": r["ps_per_op"],
            # 预算每次重录都由**本次中位**重算, 不读旧值 —— 否则一次离群读数会
            # 把预算永久钉松 (旧形态) 或钉死 (单样本形态).
            "budget_ns_per_op": budget_ns_per_op(r["ns_per_op_frac"]),
        })
    baseline = {
        "version": report.get("version", 1),
        "machine": f"{platform.machine()}-{platform.system()}-{platform.release()}",
        "python": platform.python_version(),
        # 读数来源: 每个条目由几次采样取中位数. 写进文件是为了让读者能分辨
        # 「单样本钉下」与「中位数钉下」—— 两者对短测量窗条目的稳定性不同.
        "runs_per_item": runs,
        "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "results": entries,
    }
    # sched_criteria 是调度公平性/时延判据 (K-07b 批3), 不是性能读数; 重录
    # 性能基线时必须原样保留, 否则会静默删除 host 回归测试的判据来源.
    if sched_criteria is not None:
        baseline["sched_criteria"] = sched_criteria
    return baseline


def load_sched_criteria(out_path: Path) -> dict | None:
    """读取既有基线文件中的 sched_criteria; 文件不存在/损坏时返回 None."""
    try:
        with open(out_path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError):
        return None
    criteria = data.get("sched_criteria")
    return criteria if isinstance(criteria, dict) else None


def print_summary(baseline: dict) -> None:
    print(f"\n[baseline] 记录 {len(baseline['results'])} 条基线:")
    print(
        f"  {'name':<24} {'category':<10} {'ns/op (frac)':>14} "
        f"{'budget ns/op':>14} {'iterations':>12}"
    )
    print(f"  {'-'*24} {'-'*10} {'-'*14} {'-'*14} {'-'*12}")
    for r in baseline["results"]:
        ns = r["ns_per_op_frac"]
        ns_str = f"{ns:.4f}" if ns < 1.0 else f"{ns:.2f}"
        budget = r["budget_ns_per_op"]
        budget_str = f"{budget:.4f}" if budget < 1.0 else f"{budget:.2f}"
        print(
            f"  {r['name']:<24} {r['category']:<10} {ns_str:>14} "
            f"{budget_str:>14} {r['iterations']:>12}"
        )


def _synthetic_run(samples: list[float], iterations: int = 1000) -> dict:
    """把同一份条目读数包成一条 BenchReport (自检用; ps 与 ns 保持 *1000 自洽)."""
    return {
        "version": 1,
        "results": [
            {
                "name": f"item{i}",
                "category": "selftest",
                "iterations": iterations,
                "ns_per_op_frac": ns,
                "ps_per_op": int(round(ns * 1000)),
            }
            for i, ns in enumerate(samples)
        ],
    }


def _multi_run(values: dict[str, list[float]], iterations: int = 1000) -> list[dict]:
    """把「条目 → N 次读数」转成 N 份 BenchReport."""
    names = list(values)
    return [
        {
            "version": 1,
            "results": [
                {
                    "name": name,
                    "category": "selftest",
                    "iterations": iterations,
                    "ns_per_op_frac": values[name][round_idx],
                    "ps_per_op": int(round(values[name][round_idx] * 1000)),
                }
                for name in names
            ],
        }
        for round_idx in range(len(next(iter(values.values()))))
    ]


def _expect_raises(fn, label: str, failures: list[str]) -> None:
    try:
        fn()
    except ValueError:
        return
    except Exception as exc:  # 形态错的异常也算自检失败
        failures.append(f"{label}: 抛的是 {type(exc).__name__} 而非 ValueError ({exc})")
        return
    failures.append(f"{label}: 期望 fail-closed 抛 ValueError, 实际正常返回")


def self_test() -> list[str]:
    """聚合器自检: 真调 aggregate_runs / build_baseline, 不复制中位数算法."""
    failures: list[str] = []

    def check(label: str, got, want) -> None:
        if got != want:
            failures.append(f"{label}: 期望 {want!r}, 实得 {got!r}")

    # 1. runs=1 与历史行为一致: 单样本原样透传.
    single = aggregate_runs([_synthetic_run([2.5, 0.75])])
    check("单样本透传 ns[0]", single["results"][0]["ns_per_op_frac"], 2.5)
    check("单样本透传 ps[0]", single["results"][0]["ps_per_op"], 2500)
    check("单样本透传条目数", len(single["results"]), 2)

    # 2. 中位数对单点离群不敏感 (fc45 症状: 一次离群读数把基线钉死在低点).
    outlier = _multi_run({"a": [5.0, 5.0, 5.0, 5.0, 100.0], "b": [1.0, 1.0, 1.0, 1.0, 0.01]})
    agg = aggregate_runs(outlier)
    check("离群高读数不抬高基线", agg["results"][0]["ns_per_op_frac"], 5.0)
    check("离群低读数不压低基线", agg["results"][1]["ns_per_op_frac"], 1.0)

    # 3. 偶数次采样取中间两值均值 (不是 min / 不是 mean).
    even = aggregate_runs(_multi_run({"a": [1.0, 2.0, 3.0, 4.0]}))
    check("偶数采样取中位", even["results"][0]["ns_per_op_frac"], 2.5)
    check("中位反推 ps", even["results"][0]["ps_per_op"], 2500)

    # 4. 条目名 / 分类 / 迭代数从首次读数保留, 顺序不变.
    check("条目名保留", [r["name"] for r in agg["results"]], ["a", "b"])
    check("迭代数保留", [r["iterations"] for r in agg["results"]], [1000, 1000])

    # 5. fail-closed 四形: 无读数 / 条目集合漂移 / iterations 漂移 / 重名.
    _expect_raises(lambda: aggregate_runs([]), "空读数", failures)

    def drift_names():
        a = _synthetic_run([1.0, 2.0])
        b = _synthetic_run([1.0])
        return aggregate_runs([a, b])

    _expect_raises(drift_names, "条目集合漂移", failures)

    def drift_iters():
        return aggregate_runs([_synthetic_run([1.0], 1000), _synthetic_run([1.0], 10000)])

    _expect_raises(drift_iters, "iterations 漂移", failures)

    def dup_name():
        report = _synthetic_run([1.0])
        report["results"].append(dict(report["results"][0]))
        return aggregate_runs([report])

    _expect_raises(dup_name, "条目重名", failures)

    # 6. build_baseline 记录采样次数, 并原样保留 sched_criteria (K-07b 批3 判据).
    criteria = {"fairness_max_ratio": 2}
    baseline = build_baseline(agg, criteria, runs=5)
    check("采样次数落档", baseline["runs_per_item"], 5)
    check("sched_criteria 保留", baseline.get("sched_criteria"), criteria)
    check("基线条目数", len(baseline["results"]), 2)
    check(
        "基线条目字段",
        sorted(baseline["results"][0]),
        sorted(
            [
                "name",
                "category",
                "iterations",
                "ns_per_op_frac",
                "ps_per_op",
                "budget_ns_per_op",
            ]
        ),
    )
    # 缺省 runs=1: 老调用点 (不传参数) 行为不变.
    check("runs 缺省为 1", build_baseline(agg)["runs_per_item"], 1)
    check("缺省时不带 sched_criteria", "sched_criteria" in build_baseline(agg), False)

    # 7. 绝对预算口径: ×BUDGET_MULTIPLIER 后向上取整到两位有效数字, 且永不比
    #    中位×倍数更小 (收紧预算 = 把判据调严, 是另一种形态的「放宽后难红」反面,
    #    perf_metric_test.rs 的另一半断言按同一条下界钉).
    check("预算上取整 (亚纳秒)", budget_ns_per_op(0.296), 3.0)
    check("预算上取整 (个位)", budget_ns_per_op(5.858), 59.0)
    check("预算上取整 (百位)", budget_ns_per_op(454.62), 4600.0)
    check("预算上取整 (万位)", budget_ns_per_op(7332.724), 74000.0)
    check("零读数取预算下限", budget_ns_per_op(0.0), BUDGET_FLOOR_NS)
    check(
        "预算不低于中位×倍数",
        all(
            budget_ns_per_op(v) >= v * BUDGET_MULTIPLIER
            for v in (0.0296, 1.423, 5.858, 19.35, 172.018, 454.62, 7332.724)
        ),
        True,
    )
    # 预算由**本次**中位重算: 离群读数抬高的中位会一并抬高预算, 但基线读数与预算
    # 必须同源 (否则「基线放宽 / 预算不变」这类半截改动会让判据自相矛盾).
    check("预算与基线读数同源", baseline["results"][0]["budget_ns_per_op"], budget_ns_per_op(5.0))

    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description="记录 framekernel-bench 性能基线")
    parser.add_argument(
        "out",
        nargs="?",
        default=str(DEFAULT_OUT),
        help="输出 JSON 路径 (默认: " + str(DEFAULT_OUT) + ")",
    )
    parser.add_argument(
        "--runs",
        "-n",
        type=int,
        default=1,
        help="每项取多少次采样的中位数 (默认 1 = 单样本; 短测量窗条目建议 5)",
    )
    parser.add_argument("--self-test", action="store_true", help="只跑聚合器自检")
    args = parser.parse_args()

    if args.runs < 1:
        print(f"ERROR: --runs 必须 >= 1, 实得 {args.runs}", file=sys.stderr)
        return 2

    failures = self_test()
    if failures:
        for msg in failures:
            print(f"自检失败: {msg}", file=sys.stderr)
        return 2
    if args.self_test:
        print(
            "自检通过: 单样本透传 + 中位数抗离群 + fail-closed 四形 "
            "+ 绝对预算口径 + sched_criteria 保留"
        )
        return 0

    if not HOST_TESTS_DIR.is_dir():
        print(f"ERROR: 未找到 host-tests 目录: {HOST_TESTS_DIR}", file=sys.stderr)
        return 1

    reports = [run_bench() for _ in range(args.runs)]
    report = aggregate_runs(reports)
    out_path = Path(args.out)
    baseline = build_baseline(report, load_sched_criteria(out_path), runs=args.runs)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(baseline, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print(f"\n[baseline] 已写入: {out_path} (每项 {args.runs} 次采样取中位)")
    print_summary(baseline)
    return 0


if __name__ == "__main__":
    sys.exit(main())
