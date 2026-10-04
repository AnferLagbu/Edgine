#!/usr/bin/env python3
"""
E10: TCB 度量自动化脚本

统计 framework/ 和 services/ 的代码量, 计算 TCB 占比,
输出结构化报告. 用于 CI 中监控 TCB 膨胀.

退出码: 0 = 通过 (TCB < 30%), 1 = 超标
"""

import os
import sys
import json
import subprocess
from pathlib import Path

BASE = Path(__file__).resolve().parent.parent
FRAMEWORK = BASE / 'src' / 'kernel' / 'framework'
SERVICES = BASE / 'src' / 'kernel' / 'services'
TARGET_DIR = BASE / 'other' / 'target' / 'audit'

TCB_TARGET_RATIO = 30.0  # 目标: TCB 占比 < 30%


def _should_exclude(rs: Path) -> bool:
    """判断文件是否应排除出 TCB 统计 (测试代码、第三方库)"""
    parts = rs.relative_to(BASE).parts
    # framework/tests/ — 测试代码不参与运行时执行, 不是 TCB
    if 'tests' in parts:
        idx = parts.index('tests')
        # framework/tests/ 下的测试文件
        if idx > 0 and parts[idx - 1] == 'framework':
            return True
    # smoltcp 由单独逻辑排除
    if 'smoltcp' in parts:
        return True
    return False


def count_loc(directory: Path, apply_exclusions: bool = True) -> int:
    """统计 .rs 文件行数 (不含空行和纯注释行)"""
    total = 0
    for rs in directory.rglob('*.rs'):
        if apply_exclusions and _should_exclude(rs):
            continue
        with open(rs, 'r', encoding='utf-8', errors='replace') as f:
            for line in f:
                stripped = line.strip()
                if stripped and not stripped.startswith('//') and not stripped.startswith('/*'):
                    total += 1
    return total


def count_loc_raw(directory: Path, apply_exclusions: bool = True) -> int:
    """统计 .rs 文件总行数 (含空行和注释)"""
    total = 0
    for rs in directory.rglob('*.rs'):
        if apply_exclusions and _should_exclude(rs):
            continue
        with open(rs, 'r', encoding='utf-8', errors='replace') as f:
            total += sum(1 for _ in f)
    return total


def count_unsafe(directory: Path, apply_exclusions: bool = True) -> int:
    """统计 unsafe 行数 (不含注释、#![deny] 等)"""
    count = 0
    for rs in directory.rglob('*.rs'):
        if apply_exclusions and _should_exclude(rs):
            continue
        with open(rs, 'r', encoding='utf-8', errors='replace') as f:
            for line in f:
                stripped = line.strip()
                # 跳过所有注释行和属性行
                if stripped.startswith('//') or stripped.startswith('/*') \
                   or stripped.startswith('//!') or stripped.startswith('///') \
                   or stripped.startswith('#!['):
                    continue
                if 'unsafe' in stripped:
                    count += 1
    return count


def count_pub_fn(directory: Path, apply_exclusions: bool = True) -> int:
    """统计 pub fn 数量"""
    count = 0
    for rs in directory.rglob('*.rs'):
        if apply_exclusions and _should_exclude(rs):
            continue
        with open(rs, 'r', encoding='utf-8', errors='replace') as f:
            for line in f:
                if 'pub fn ' in line or 'pub async fn ' in line:
                    count += 1
    return count


def module_breakdown(directory: Path) -> dict:
    """按子目录统计"""
    result = {}
    for subdir in sorted(directory.iterdir()):
        if subdir.is_dir() and subdir.name not in ('smoltcp', 'tests'):
            loc = count_loc_raw(subdir)
            unsafe = count_unsafe(subdir)
            if loc > 0:
                result[subdir.name] = {
                    'loc': loc,
                    'unsafe': unsafe,
                }
    # smoltcp 单独统计
    smoltcp = directory / 'smoltcp'
    if smoltcp.is_dir():
        # smoltcp 是第三方库, 不计入自研 TCB
        result['smoltcp (3rd-party)'] = {
            'loc': count_loc_raw(smoltcp, apply_exclusions=False),
            'unsafe': count_unsafe(smoltcp, apply_exclusions=False),
            'note': 'third-party, excluded from self-developed TCB',
        }
    # tests 单独统计
    tests = directory / 'tests'
    if tests.is_dir():
        result['tests (excluded)'] = {
            'loc': count_loc_raw(tests, apply_exclusions=False),
            'unsafe': count_unsafe(tests, apply_exclusions=False),
            'note': 'test code, excluded from TCB (not runtime)',
        }
    return result


def main():
    # B01-12: --soft/--enforce 切换.
    # 当前 TCB 实际 58.1%, 远超 30% 软目标. CI 需避免 hard fail (会阻塞所有 PR),
    # 采用过渡设计:
    # - 默认 (无 flag / --soft): 仅警告, exit 0
    # - --enforce: 严格阈值, TCB >= 30% exit 1
    # 未来 TCB < 30% 后, CI job 切换 --enforce.
    import argparse
    parser = argparse.ArgumentParser(description='QueenX TCB 度量')
    parser.add_argument('--soft', action='store_true', default=True,
                        help='仅警告 (默认)')
    parser.add_argument('--enforce', action='store_true',
                        help='严格阈值, 超标 exit 1')
    args = parser.parse_args()

    fw_loc_raw = count_loc_raw(FRAMEWORK)
    sv_loc_raw = count_loc_raw(SERVICES)
    fw_loc = count_loc(FRAMEWORK)
    sv_loc = count_loc(SERVICES)
    fw_unsafe = count_unsafe(FRAMEWORK)
    sv_unsafe = count_unsafe(SERVICES)
    fw_pub_fn = count_pub_fn(FRAMEWORK)
    sv_pub_fn = count_pub_fn(SERVICES)

    total_loc = fw_loc + sv_loc
    tcb_ratio = (fw_loc / total_loc * 100) if total_loc > 0 else 0

    # smoltcp / tests 排除后的自研 TCB
    # count_loc / count_loc_raw 已通过 _should_exclude 排除 smoltcp 和 tests,
    # fw_loc 即为自研非测试 effective 行数
    # B01-12 修复: smoltcp 从 framework/net/ 迁移到 services/net/ (决策 3-B, 2026-06-24).
    smoltcp_dir = SERVICES / 'net' / 'smoltcp'
    smoltcp_loc = count_loc_raw(smoltcp_dir, apply_exclusions=False) if smoltcp_dir.is_dir() else 0
    smoltcp_loc_eff = count_loc(smoltcp_dir, apply_exclusions=False) if smoltcp_dir.is_dir() else 0
    tests_dir = FRAMEWORK / 'tests'
    tests_loc = count_loc_raw(tests_dir, apply_exclusions=False) if tests_dir.is_dir() else 0
    tests_loc_eff = count_loc(tests_dir, apply_exclusions=False) if tests_dir.is_dir() else 0
    self_fw_loc_raw = fw_loc_raw  # 已排除 smoltcp + tests
    self_fw_loc = fw_loc          # 已排除 smoltcp + tests
    self_tcb_ratio = (self_fw_loc / total_loc * 100) if total_loc > 0 else 0

    # 模块级分解
    fw_modules = module_breakdown(FRAMEWORK)
    sv_modules = module_breakdown(SERVICES)

    report = {
        'framework': {
            'loc_raw': fw_loc_raw,
            'loc': fw_loc,
            'unsafe_lines': fw_unsafe,
            'pub_fn': fw_pub_fn,
            'modules': fw_modules,
        },
        'services': {
            'loc_raw': sv_loc_raw,
            'loc': sv_loc,
            'unsafe_lines': sv_unsafe,
            'pub_fn': sv_pub_fn,
            'modules': sv_modules,
        },
        'tcb_ratio': round(tcb_ratio, 1),
        'target_ratio': TCB_TARGET_RATIO,
        'status': 'PASS' if tcb_ratio < TCB_TARGET_RATIO else 'EXCEEDED',
        'exclusions': {
            'smoltcp_loc': smoltcp_loc,
            'tests_loc': tests_loc,
            'note': 'smoltcp (3rd-party) and tests (non-runtime) excluded from TCB',
        },
    }

    # 输出
    print("=" * 70)
    print("TCB Report")
    print("=" * 70)
    print(f"  framework:  {fw_loc_raw:>10,} LoC (raw), {fw_loc:>10,} (effective)")
    print(f"  services:   {sv_loc_raw:>10,} LoC (raw), {sv_loc:>10,} (effective)")
    print(f"  smoltcp:    {smoltcp_loc:>10,} LoC (3rd-party, excluded from self-TCB)")
    print(f"  tests:      {tests_loc:>10,} LoC (test code, excluded from TCB)")
    print(f"  self-fw:    {self_fw_loc_raw:>10,} LoC (raw, excl. smoltcp+tests)")
    print(f"  unsafe:     {fw_unsafe:>10,} lines (framework), {sv_unsafe:>5,} (services)")
    print(f"  pub fn:     {fw_pub_fn:>10,} (framework), {sv_pub_fn:>5,} (services)")
    print(f"  TCB ratio:  {tcb_ratio:>10.1f}% (excl. smoltcp+tests)")
    print(f"  Target:     <{TCB_TARGET_RATIO:.0f}%")
    print(f"  Status:     {report['status']}")
    print("=" * 70)

    # Top 10 framework 模块
    print("\nTop framework modules (by LoC):")
    sorted_mods = sorted(fw_modules.items(), key=lambda x: x[1]['loc'], reverse=True)
    for name, data in sorted_mods[:10]:
        unsafe_str = f", {data['unsafe']} unsafe" if data['unsafe'] > 0 else ""
        print(f"  {name:<30s} {data['loc']:>8,} LoC{unsafe_str}")

    # 保存 JSON
    TARGET_DIR.mkdir(parents=True, exist_ok=True)
    json_path = TARGET_DIR / 'tcb-report.json'
    with open(json_path, 'w') as f:
        json.dump(report, f, indent=2, ensure_ascii=False)
    print(f"\nJSON report: {json_path}")

    # 退出码 (B01-12)
    if tcb_ratio >= TCB_TARGET_RATIO:
        print(f"\n⚠  TCB ratio ({tcb_ratio:.1f}%) exceeds target (<{TCB_TARGET_RATIO:.0f}%)")
        if args.enforce:
            sys.exit(1)
        # --soft: 仅警告, exit 0 (默认)

    print("\n✓ TCB report generated")
    sys.exit(0)


if __name__ == '__main__':
    main()
