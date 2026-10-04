#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Edgine Contributors
#
# check_bench_regression.py — 对比当前 framekernel-bench 结果与 baseline.json
#
# 用法:
#   python3 scripts/check_bench_regression.py [baseline_path] [--threshold 0.15]
#
# 行为:
#   1. 重新运行 `cargo run --release --bin framekernel-bench` 收集当前结果
#   2. 与 baseline.json 逐条目对比 ns_per_op_frac
#   3. 退化超过 threshold (默认 15%) 视为回归, 进程退出码 1
#   4. 改善 (ns/op 减少) 视为 OK
#   5. 新增 / 删除条目: 打印 WARN, 不计入回归
#
# 退出码:
#   0 — 无回归
#   1 — 存在回归
#   2 — 错误 (基线缺失 / bench 失败)

import argparse
import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BASELINE = REPO_ROOT / "host-tests" / "benches" / "baseline.json"
HOST_TESTS_DIR = REPO_ROOT / "host-tests"
DEFAULT_THRESHOLD = 0.15  # 15%


def run_bench() -> dict:
    print("[check_bench_regression] running framekernel-bench (release)...", file=sys.stderr)
    proc = subprocess.run(
        ["cargo", "run", "--release", "--bin", "framekernel-bench"],
        cwd=str(HOST_TESTS_DIR),
        capture_output=True,
        text=True,
        check=True,
    )
    json_line = proc.stdout.strip().splitlines()[-1]
    return json.loads(json_line)


def load_baseline(path: Path) -> dict:
    if not path.is_file():
        print(f"ERROR: 基线文件不存在: {path}", file=sys.stderr)
        print("       请先运行: python3 scripts/record_bench_baseline.py", file=sys.stderr)
        sys.exit(2)
    return json.loads(path.read_text(encoding="utf-8"))


def compare(baseline: dict, current: dict, threshold: float) -> tuple[list, list, list]:
    """
    返回 (regressions, improvements, warnings) 三个列表, 每项为 dict:
      {"name", "category", "baseline", "current", "delta_pct"}
    delta_pct = (current - baseline) / baseline, 正值为退化.

    过滤规则:
      - baseline 或 current 为 0 / 负数: 跳过 (compiler 优化到极限, 无意义)
      - 亚纳秒测量噪声处理:
        * 绝对差 < MIN_ABS_NS (默认 1.0ns) 时, 启用相对噪声门限
          (REL_NOISE_THRESHOLD, 默认 50%). 即: 当 |delta| < 50% 时视为噪声.
          这一规则适用于 sub-nanosecond 量级 (ps 级) 的微基准, 避免
          测量抖动掩盖真正的性能退化 (e.g. +400% 的 5ps→25ps 退化).
        * 绝对差 >= 1ns 时, 仍按传入的 `threshold` 判定
      - 退化超阈值: 进入 regressions
      - 改善超阈值: 进入 improvements
    """
    # 噪声过滤: 区分亚纳秒 (ps 级) 与纳秒级.
    # 亚纳秒抖动来自 CPU 频率切换 / cache 状态变化, 通常在 ±30% 内;
    # 真正的回归 (例如 +100%, +400%) 即使绝对值很小也应被捕获.
    MIN_ABS_NS = 1.0
    REL_NOISE_THRESHOLD = 0.50  # 亚纳秒时, |delta| < 50% 视为噪声

    by_name = {r["name"]: r for r in baseline.get("results", [])}
    regressions, improvements, warnings = [], [], []

    cur_by_name = {r["name"]: r for r in current.get("results", [])}
    # 新增 / 删除
    for name in cur_by_name.keys() - by_name.keys():
        warnings.append({"name": name, "msg": "baseline 中缺失 (新增条目)"})
    for name in by_name.keys() - cur_by_name.keys():
        warnings.append({"name": name, "msg": "当前结果中缺失 (条目被删除)"})

    for name, cur in cur_by_name.items():
        if name not in by_name:
            continue
        base = by_name[name]
        b_ns = base["ns_per_op_frac"]
        c_ns = cur["ns_per_op_frac"]
        if b_ns <= 0.0 or c_ns <= 0.0:
            warnings.append({
                "name": name,
                "msg": f"baseline={b_ns}, current={c_ns} 至少为 0, 跳过 (编译器完全优化掉的工作)"
            })
            continue
        delta_pct = (c_ns - b_ns) / b_ns
        abs_diff = abs(c_ns - b_ns)
        if abs_diff < MIN_ABS_NS:
            # 亚纳秒测量: 启用相对噪声门限, 避免小波动掩盖大相对变化
            if abs(delta_pct) < REL_NOISE_THRESHOLD:
                warnings.append({
                    "name": name,
                    "msg": f"|diff|={abs_diff:.4f}ns < {MIN_ABS_NS}ns 噪声, 跳过 ({delta_pct:+.1%})"
                })
                continue
            # 否则 (e.g. +400% ps 级回归), 不应被视为噪声, 继续到下方判定
        rec = {
            "name": name,
            "category": cur.get("category", base.get("category", "")),
            "baseline": b_ns,
            "current": c_ns,
            "delta_pct": delta_pct,
        }
        if delta_pct > threshold:
            regressions.append(rec)
        elif delta_pct < -threshold:
            improvements.append(rec)
    return regressions, improvements, warnings


def main() -> int:
    parser = argparse.ArgumentParser(description="检查 framekernel-bench 是否回归")
    parser.add_argument(
        "baseline",
        nargs="?",
        default=str(DEFAULT_BASELINE),
        help="基线 JSON 路径 (默认: " + str(DEFAULT_BASELINE) + ")",
    )
    parser.add_argument(
        "--threshold", "-t",
        type=float,
        default=DEFAULT_THRESHOLD,
        help="regression threshold as fraction, default 0.15 means 15 percent",
    )
    args = parser.parse_args()

    baseline = load_baseline(Path(args.baseline))
    current = run_bench()

    regressions, improvements, warnings = compare(baseline, current, args.threshold)

    print()
    print(f"=== 回归检查 (阈值 {args.threshold:.0%}) ===")
    print(f"  baseline: {args.baseline}")
    print(f"  对比条目: {len(baseline.get('results', []))} 项基线 / {len(current.get('results', []))} 项当前")
    print()

    if improvements:
        print(f"[OK] 性能改善 ({len(improvements)} 项):")
        for r in improvements:
            print(f"  - {r['name']:<24} {r['baseline']:.4f}ns -> {r['current']:.4f}ns ({r['delta_pct']:+.1%})")
        print()

    if warnings:
        print(f"[WARN] 元数据变化 ({len(warnings)} 项):")
        for w in warnings:
            print(f"  - {w['name']}: {w['msg']}")
        print()

    if regressions:
        print(f"[FAIL] 性能回归 ({len(regressions)} 项):")
        for r in regressions:
            print(f"  - {r['name']:<24} {r['baseline']:.4f}ns -> {r['current']:.4f}ns ({r['delta_pct']:+.1%})")
        print()
        print(f"回归检查失败: {len(regressions)} 项超出 {args.threshold:.0%} 阈值")
        return 1

    print("[PASS] 所有条目均在阈值范围内, 无回归.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
