#!/usr/bin/env python3
"""framework→services 生产反向依赖审计 (DECISION-J 验收口径).

口径依据 (docs/plan/framekernel-paradigm-enforcement.md):
- §7.3: framework/tests 测试载体访问 services 属合理, 不纳入整治;
- §7 ipc 表: cfg(test) 测试代码访问 services 真实代码属合理 (§7.3 精神).

生产反向依赖 = 非测试、非注释上下文中的 `crate::services` 引用.

口径说明:
- 路径形式随方案 D (kernel 独立 crate 化, `crate::kernel::X` → `crate::X`) 更新为
  `crate::services`; 旧形式 `crate::kernel::services` 在 crate 内不可编译, 无需再匹配.
- 测试上下文判定 (fail-closed: 判定不了的按生产违规计):
  1. framework/tests/ 目录整体 — feature (kernel_test|host-test) 门控测试载体;
  2. `#[cfg(test)] mod X;` 引入的外部文件 — 该文件整体归测试;
  3. `#[cfg(test)] mod X { ... }` 内联模块 — 花括号深度跟踪到闭合;
  4. 上述规则无法解析的结构 (如 cfg(test) 属性后无 mod 声明) — 按生产违规计.
- 注释行 (`//` / `///` / `//!` 行注释与 `/* ... */` 块注释) 内的引用仅为文档交叉引用,
  不构成代码依赖, 单列统计不计违规.

用法: python3 scripts/audit_reverse_deps.py [--verbose]
退出码: 0 = 生产反向依赖为 0; 1 = 存在生产反向依赖 (逐项列出).
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

BASE = Path(__file__).resolve().parent.parent / "src" / "kernel" / "framework"
TESTS_DIR = BASE / "tests"

# 引用模式 (方案 D 后统一为 crate::services; \b 防 services_v2 之类误匹配)
RE_REF = re.compile(r"\bcrate::services\b")
# cfg(test) 属性行 (含 all(test,...) 变体)
RE_CFG_TEST_ATTR = re.compile(r'#\s*\[\s*cfg\s*\(\s*(all\s*\()?\s*test\b')
# 外部模块声明 `mod X;`
RE_EXTERN_MOD = re.compile(r'^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;')
# 内联模块声明起始 `mod X {`
RE_INLINE_MOD = re.compile(r'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?mod\s+(\w+)\s*\{')


def classify_file(path: Path) -> str:
    """判定整个文件是否测试载体. 返回 'test' | 'prod'."""
    if TESTS_DIR in path.parents or path == TESTS_DIR:
        return "test"
    return "prod"


def scan_inline_cfg_test_blocks(lines: list[str]) -> set[int]:
    """逐行扫描内联 cfg(test) 块, 返回覆盖的行号集合.

    花括号深度跟踪: 遇 `#[cfg(test)]` + `mod X {` 进入测试上下文, 至配对 `}` 退出.
    解析不出的结构不进入上下文 (fail-closed: 这些行的引用按生产计).
    """
    test_lines: set[int] = set()
    cfg_pending = False  # 上一有效属性行是 cfg(test)
    depth_stack: list[int] = []  # 退出深度
    depth = 0

    for idx, raw in enumerate(lines):
        lineno = idx + 1
        stripped = raw.strip()

        # 跟踪已有测试块的深度闭合
        if depth_stack and depth <= depth_stack[-1]:
            depth_stack.pop()  # 已回到块外深度, 块结束
        in_test = bool(depth_stack)

        depth_before = depth
        depth += raw.count("{") - raw.count("}")

        if in_test:
            test_lines.add(lineno)
            continue

        if RE_CFG_TEST_ATTR.search(stripped):
            cfg_pending = True
            continue

        if cfg_pending:
            if RE_INLINE_MOD.match(stripped):
                # 内联块: 退出深度 = 块外层深度 (本行 `{` 之前的深度)
                depth_stack.append(depth_before)
                test_lines.add(lineno)
            # 属性后无 mod 声明 (如直接跟 #[test] fn) — 仅标记属性行, 不误扩
            cfg_pending = False

    return test_lines


def collect_extern_test_mods() -> set[Path]:
    """收集 `#[cfg(test)] mod X;` 声明引入的外部文件路径 (同目录 X.rs)."""
    extern_files: set[Path] = set()
    for path in BASE.rglob("*.rs"):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeDecodeError):
            continue
        cfg_pending = False
        for raw in lines:
            stripped = raw.strip()
            if RE_CFG_TEST_ATTR.search(stripped):
                cfg_pending = True
                continue
            if cfg_pending:
                m = RE_EXTERN_MOD.match(stripped)
                if m:
                    extern_files.add(path.parent / f"{m.group(1)}.rs")
                cfg_pending = False
    return extern_files


def scan_comment_lines(lines: list[str]) -> set[int]:
    """返回"注释行"行号集合.

    覆盖 `//` / `///` / `//!` 行注释与 `/* ... */` 块注释 (含嵌套):
    - 行首非空白起为 `//` → 注释行;
    - 行首非空白起为 `/*` 或处于块注释内 → 注释行, 至配对 `*/` 退出.
    仅排除"整行注释"; 行内代码后跟随的注释不整行排除 (fail-closed 计生产).
    """
    comment_lines: set[int] = set()
    depth = 0  # 块注释嵌套深度
    for idx, raw in enumerate(lines):
        lineno = idx + 1
        stripped = raw.lstrip()
        if depth > 0:
            comment_lines.add(lineno)
            depth += stripped.count("/*") - stripped.count("*/")
            if depth < 0:
                depth = 0
            continue
        if stripped.startswith("//"):
            comment_lines.add(lineno)
        elif stripped.startswith("/*"):
            comment_lines.add(lineno)
            depth += stripped.count("/*") - stripped.count("*/")
            if depth < 0:
                depth = 0
    return comment_lines


def scan_file(
    path: Path, extern_test_files: set[Path]
) -> tuple[list[dict], list[dict], list[dict]]:
    """返回 (生产引用, 测试引用, 注释引用) 列表. 每项 {line, code}."""
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        # fail-closed: 读不了 = 生产违规
        return [{"line": 0, "code": f"<unreadable: {e}>"}], [], []

    lines = text.splitlines()
    prod, test, comment = [], [], []

    # 0. 注释行 — 文档交叉引用, 不计违规
    comment_lines = scan_comment_lines(lines)

    # 1. 整文件测试载体 (tests 目录 / 被 cfg(test) mod X; 引入)
    is_test_carrier = classify_file(path) == "test" or path in extern_test_files
    test_lines = set() if is_test_carrier else scan_inline_cfg_test_blocks(lines)

    for idx, raw in enumerate(lines):
        if not RE_REF.search(raw):
            continue
        entry = {"line": idx + 1, "code": raw.strip()}
        if (idx + 1) in comment_lines:
            comment.append(entry)
        elif is_test_carrier or (idx + 1) in test_lines:
            test.append(entry)
        else:
            prod.append(entry)

    return prod, test, comment


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--verbose", action="store_true", help="同时输出测试引用明细")
    args = parser.parse_args()

    if not BASE.is_dir():
        print(f"[FAIL-CLOSED] framework 目录不存在: {BASE}", file=sys.stderr)
        return 1

    all_prod: dict[Path, list[dict]] = {}
    all_test: dict[Path, list[dict]] = {}
    all_comment: dict[Path, list[dict]] = {}
    file_count = 0

    extern_test_files = collect_extern_test_mods()

    for path in sorted(BASE.rglob("*.rs")):
        file_count += 1
        prod, test, comment = scan_file(path, extern_test_files)
        if prod:
            all_prod[path] = prod
        if test:
            all_test[path] = test
        if comment:
            all_comment[path] = comment

    prod_lines = sum(len(v) for v in all_prod.values())
    test_lines = sum(len(v) for v in all_test.values())
    comment_lines = sum(len(v) for v in all_comment.values())
    prod_files = len(all_prod)

    print("=" * 70)
    print("framework→services 生产反向依赖审计 (audit_reverse_deps)")
    print("=" * 70)
    print(f"扫描文件数: {file_count}")
    print(f"生产反向依赖: {prod_files} 文件 / {prod_lines} 行")
    print(f"测试上下文引用 (§7.3 合理, 不纳入): {len(all_test)} 文件 / {test_lines} 行")
    print(f"注释引用 (文档交叉引用, 不计违规): {len(all_comment)} 文件 / {comment_lines} 行")
    print()

    if all_prod:
        print("[FAIL] 生产反向依赖明细:")
        for path, entries in sorted(all_prod.items()):
            rel = path.relative_to(BASE.parent.parent)
            print(f"\n  {rel}:")
            for e in entries:
                print(f"    L{e['line']}: {e['code']}")
        print()
        print(f">>> 存在 {prod_files} 文件 / {prod_lines} 行生产反向依赖 <<<")
        rc = 1
    else:
        print(">>> 生产反向依赖为 0, 检查通过 <<<")
        rc = 0

    if args.verbose and all_test:
        print("\n[INFO] 测试上下文引用明细 (§7.3 合理, 不计违规):")
        for path, entries in sorted(all_test.items()):
            rel = path.relative_to(BASE.parent.parent)
            print(f"\n  {rel}:")
            for e in entries:
                print(f"    L{e['line']}: {e['code']}")

    if args.verbose and all_comment:
        print("\n[INFO] 注释引用明细 (文档交叉引用, 不计违规):")
        for path, entries in sorted(all_comment.items()):
            rel = path.relative_to(BASE.parent.parent)
            print(f"\n  {rel}:")
            for e in entries:
                print(f"    L{e['line']}: {e['code']}")

    return rc


if __name__ == "__main__":
    sys.exit(main())
