#!/usr/bin/env python3
"""
废弃命名扫描器 (Obsolete Naming Scanner)

检出仓内对已废弃项目名的任何引用. 受检词 = AntX —— 本仓早期内核与项目的代号,
2026-10-04 全项目更名后已彻底废弃: 产物名 / GRUB 标题 / QEMU hostname / 安装标记 /
调试脚本一律并入 Edgine (P-58 裁定「乙・彻底废弃」+「甲・全部并入 Edgine」).

## 口径 (P-58 校正后的四条)

1. **范围 = `git ls-files` 列出的全部受控文本文件**, 不按扩展名白名单选文件.
   旧实现只扫 `.rs/.toml/.sh/.py/.md/...`, 于是最集中的产物命名 —— 无扩展名的
   `Makefile` —— 整体漏检; 旧实现的 `len(line) < 50` 启发式还会静默丢弃短行小写
   命中. 两处漏检在收口时实测: `Makefile` 10 行 + `.asm` 1 行真实陈旧头注释.
2. **历史面 KEEP**: `docs/plan/` 与 `docs/report/` 记录执行当时形态 (AGENTS §6),
   其中出现旧名是历史事实, 不计违规. 否则「描述这个废弃名」本身就会让门禁常红 ——
   旧口径实测 53 → 65 → 69, 每写一段说明就涨几个点, 读数永不收敛.
3. **自命中豁免**: 扫描器自身文件不扫 (本文件必须写出受检词才能扫描它).
4. **二值判定**: 无违规退 0; 有违规退 1 并逐条列出; 扫描失败退 2 (fail-closed,
   读取失败不静默 continue). 正常扫描前先跑检测器自检, 自检失败同样退 2 ——
   检测器自己坏了不许给出「零违规」.

## 用法

    python3 scripts/scan_obsolete_naming.py              # 全仓扫描 (CI 用法)
    python3 scripts/scan_obsolete_naming.py --json       # JSON 报告
    python3 scripts/scan_obsolete_naming.py --self-test  # 只跑检测器自检
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
SELF_PATH = Path(__file__).resolve()

NEEDLE = "antx"
# 边界口径: 前后不是 ASCII 字母/数字, 但 `_` / `.` / `-` 都算边界 ⇒
# `xxx.iso` / `xxx_test.iso` / `.gdbinit.xxx` / `xxx-stage1.asm` 全部命中,
# 而 `ant` / `antxyz` / `xantx` 不误报.
PATTERN = re.compile(r"(?i)(?<![a-z0-9])" + NEEDLE + r"(?![a-z0-9])")

HISTORY_PREFIXES = ("docs/plan/", "docs/report/")
EXCLUDED_PREFIXES = (
    "other/",
    "target/",
    "src/kernel/functions/net/smoltcp/",
)
BINARY_PROBE_BYTES = 8192
CONTEXT_CHARS = 120

SELF_TEST_CASES = [
    ("产物路径形态", "scripts/a.sh", f"other/build/x86_64/{NEEDLE}.iso", 1),
    ("下划线标识符形态", "Makefile", f"{NEEDLE}_test.iso", 1),
    ("大写形态", "src/a.rs", NEEDLE.upper(), 1),
    ("连字符形态", "src/a.asm", f"{NEEDLE}-stage1.asm", 1),
    ("点前缀形态", "scripts/a.sh", f".gdbinit.{NEEDLE}", 1),
    ("普通词不误报", "src/a.rs", "ant", 0),
    ("后缀连写不误报", "src/a.rs", f"{NEEDLE}yz", 0),
    ("前缀连写不误报", "src/a.rs", f"x{NEEDLE}", 0),
]


class ScanError(RuntimeError):
    """扫描基础设施失败 (git ls-files / 文件读取), 必须 fail-closed."""


def is_history(rel: str) -> bool:
    """历史面: 记录执行当时形态的 plan/report 文本 (AGENTS §6)."""
    return rel.startswith(HISTORY_PREFIXES)


def scan_text(rel: str, text: str) -> list[dict]:
    """纯函数检测器: 输入 (相对路径, 文本), 输出命中列表 (自检直接调用它)."""
    hits = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        for match in PATTERN.finditer(line):
            hits.append(
                {
                    "file": rel,
                    "line": line_no,
                    "col": match.start() + 1,
                    "context": line.strip()[:CONTEXT_CHARS],
                }
            )
    return hits


def tracked_files() -> list[str]:
    """全部受控文件 (含无扩展名的 Makefile 与 .asm), 不按扩展名过滤."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(PROJECT_ROOT), "ls-files", "-z"],
            capture_output=True,
            check=False,
        )
    except OSError as exc:
        raise ScanError(f"git 不可用: {exc}") from exc
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", "replace").strip()
        raise ScanError(f"git ls-files 失败 (exit={proc.returncode}): {stderr}")
    return [p for p in proc.stdout.decode("utf-8", "surrogateescape").split("\0") if p]


def decode_text(rel: str) -> str | None:
    """读文本; 二进制 (前 8 KiB 含 NUL) 返回 None; 读取失败 fail-closed."""
    try:
        raw = (PROJECT_ROOT / rel).read_bytes()
    except OSError as exc:
        raise ScanError(f"{rel}: {exc}") from exc
    if b"\0" in raw[:BINARY_PROBE_BYTES]:
        return None
    return raw.decode("utf-8", errors="replace")


def collect() -> tuple[list[dict], list[dict], int]:
    self_rel = str(SELF_PATH.relative_to(PROJECT_ROOT))
    violations: list[dict] = []
    kept: list[dict] = []
    scanned = 0
    for rel in tracked_files():
        if rel == self_rel or rel.startswith(EXCLUDED_PREFIXES):
            continue
        text = decode_text(rel)
        if text is None:
            continue
        scanned += 1
        hits = scan_text(rel, text)
        if not hits:
            continue
        (kept if is_history(rel) else violations).extend(hits)
    return violations, kept, scanned


def self_test() -> list[str]:
    """检测器自检: 真调 scan_text, 不模拟算法 (P-50 ③ 同型要求)."""
    failures = []
    for label, rel, text, want in SELF_TEST_CASES:
        got = len(scan_text(rel, text))
        if got != want:
            failures.append(f"{label}: 期望 {want} 处, 实得 {got} 处")
    for rel in ("docs/plan/x.md", "docs/report/x.md"):
        if not is_history(rel):
            failures.append(f"历史面判定: {rel} 必须 KEEP")
    if is_history("docs/explain/x.md"):
        failures.append("历史面判定: docs/explain 不应被 KEEP")
    if "Makefile" not in tracked_files():
        failures.append("受控文件枚举: Makefile 不在 git ls-files 结果中")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(
        description="扫描已废弃项目名 (AntX) 的残留引用",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--json", action="store_true", help="输出 JSON 报告")
    parser.add_argument("--self-test", action="store_true", help="只跑检测器自检")
    args = parser.parse_args()

    try:
        failures = self_test()
        if failures:
            for msg in failures:
                print(f"自检失败: {msg}", file=sys.stderr)
            return 2
        if args.self_test:
            print(f"自检通过: {len(SELF_TEST_CASES)} 个判定用例 + 历史面 + 受控文件枚举")
            return 0
        violations, kept, scanned = collect()
    except ScanError as exc:
        print(f"扫描失败 (fail-closed): {exc}", file=sys.stderr)
        return 2

    if args.json:
        print(
            json.dumps(
                {
                    "scan_summary": {
                        "files_scanned": scanned,
                        "violations": len(violations),
                        "history_kept": len(kept),
                    },
                    "violations": violations,
                    "history_kept_hits": kept,
                },
                ensure_ascii=False,
                indent=2,
            )
        )
    else:
        print("=" * 70)
        print("废弃命名扫描报告 (Obsolete Naming)")
        print("=" * 70)
        print(f"受检文件: {scanned}")
        print(f"历史面 KEEP: {len(kept)} 处 (docs/plan + docs/report, 不计违规)")
        print(f"违规: {len(violations)} 处")
        for hit in violations:
            print(f"  {hit['file']}:{hit['line']}:{hit['col']}  {hit['context'][:80]}")

    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
