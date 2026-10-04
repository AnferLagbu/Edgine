#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Edgine Contributors
#
# record_bench_baseline.py — 记录 framekernel-bench 当前的性能基线
#
# 用法:
#   python3 scripts/record_bench_baseline.py [out_path]
#
# 行为:
#   1. 调用 `cargo run --release --bin framekernel-bench` 收集 JSON 结果
#   2. 提取每条目的 ns_per_op_frac 作为基线
#   3. 写入 out_path (默认 host-tests/benches/baseline.json)
#   4. 同时打印简洁摘要
#
# 基线文件结构 (与 BenchReport 兼容):
#   {
#     "version": 1,
#     "machine": "<hostinfo>",
#     "rustc": "<rustc version>",
#     "recorded_at": "<ISO8601>",
#     "results": [ { "name": "...", "category": "...", "ns_per_op_frac": ..., "iterations": ... }, ... ]
#   }

import argparse
import json
import os
import platform
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "host-tests" / "benches" / "baseline.json"
HOST_TESTS_DIR = REPO_ROOT / "host-tests"


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


def build_baseline(report: dict) -> dict:
    """从 BenchReport 提取基线条目, 附加机器 / 编译器信息."""
    entries = []
    for r in report.get("results", []):
        entries.append({
            "name": r["name"],
            "category": r["category"],
            "iterations": r["iterations"],
            "ns_per_op_frac": r["ns_per_op_frac"],
            "ps_per_op": r["ps_per_op"],
        })
    return {
        "version": report.get("version", 1),
        "machine": f"{platform.machine()}-{platform.system()}-{platform.release()}",
        "python": platform.python_version(),
        "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "results": entries,
    }


def print_summary(baseline: dict) -> None:
    print(f"\n[baseline] 记录 {len(baseline['results'])} 条基线:")
    print(f"  {'name':<24} {'category':<10} {'ns/op (frac)':>14} {'iterations':>12}")
    print(f"  {'-'*24} {'-'*10} {'-'*14} {'-'*12}")
    for r in baseline["results"]:
        ns = r["ns_per_op_frac"]
        ns_str = f"{ns:.4f}" if ns < 1.0 else f"{ns:.2f}"
        print(f"  {r['name']:<24} {r['category']:<10} {ns_str:>14} {r['iterations']:>12}")


def main() -> int:
    parser = argparse.ArgumentParser(description="记录 framekernel-bench 性能基线")
    parser.add_argument(
        "out",
        nargs="?",
        default=str(DEFAULT_OUT),
        help="输出 JSON 路径 (默认: " + str(DEFAULT_OUT) + ")",
    )
    args = parser.parse_args()

    if not HOST_TESTS_DIR.is_dir():
        print(f"ERROR: 未找到 host-tests 目录: {HOST_TESTS_DIR}", file=sys.stderr)
        return 1

    report = run_bench()
    baseline = build_baseline(report)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(baseline, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print(f"\n[baseline] 已写入: {out_path}")
    print_summary(baseline)
    return 0


if __name__ == "__main__":
    sys.exit(main())
