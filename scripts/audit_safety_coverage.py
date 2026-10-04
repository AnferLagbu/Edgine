#!/usr/bin/env python3
"""
M6.1 SAFETY 完备性审计脚本 — privileged 全量 SAFETY 覆盖

修复 B01-13: 原脚本仅扫描硬编码 8 文件, 报告 53/53=100% 覆盖掩盖了剩余 2547 处
unsafe 块. 现改为动态发现 privileged/mod.rs 中所有 pub mod, 全量扫描.

检测函数委托给 tools/audit_unsafe.py (B01-15 已修复), 保持一致.

退出码: 0 = 100% 覆盖, 1 = 有缺失
"""

import argparse
import os
import re
import sys
from pathlib import Path

BASE = Path('src/kernel/privileged')
PROJECT_ROOT = Path(__file__).resolve().parent.parent
# 复用 B01-15 修复后的 audit_unsafe.py
sys.path.insert(0, str(PROJECT_ROOT / 'tools'))
import audit_unsafe  # noqa: E402


def discover_modules(base: Path) -> list[str]:
    """从 privileged/mod.rs 的 pub mod 声明动态发现所有 .rs 模块.

    解析 `pub mod <name>;` 和 `pub mod <name> { ... };` 两种形式.
    返回模块相对路径列表 (相对于 base), 例如 ['mm', 'mm/pmm', ...].
    """
    mod_rs = base / 'mod.rs'
    if not mod_rs.exists():
        return []
    content = mod_rs.read_text(encoding='utf-8', errors='replace')
    # 匹配 pub mod xxx; 或 pub mod xxx { ... }
    mods: set[str] = set()
    for m in re.finditer(r'pub\s+mod\s+(\w+)\s*[;{]', content):
        mods.add(m.group(1))
    return sorted(mods)


def collect_rs_files(base: Path, modules: list[str]) -> list[Path]:
    """从模块列表收集所有 .rs 文件路径.

    对于简单模块 (e.g. 'mm'), 包含 mod.rs 和子目录所有 .rs.
    对于嵌套模块 (e.g. 'mm'), 子目录递归包含.
    """
    files: set[Path] = set()
    files.add(base / 'mod.rs')  # 顶层 mod.rs 始终包含
    for mod in modules:
        mod_dir = base / mod
        mod_rs = mod_dir / 'mod.rs'
        if mod_rs.exists():
            files.add(mod_rs)
        # 子目录下所有 .rs
        if mod_dir.is_dir():
            for rs in mod_dir.rglob('*.rs'):
                files.add(rs)
    return sorted(files)


def main():
    parser = argparse.ArgumentParser(description='Edgine SAFETY 注释覆盖审计')
    parser.add_argument('--missing-only', action='store_true',
                        help='只输出缺 SAFETY 的位置 (每行: file:line:kind:code)')
    args = parser.parse_args()

    if not BASE.exists():
        print(f'ERROR: {BASE} 不存在', file=sys.stderr)
        sys.exit(2)

    modules = discover_modules(BASE)
    files = collect_rs_files(BASE, modules)
    # 过滤掉 arch/ 子目录 (架构特定, 与 SAFETY 主题无关)
    # SAFETY 主题针对 privileged TCB 主线, 不针对每架构 asm
    # 但保留 arch/*/mod.rs 以监控
    if not files:
        print('ERROR: 未发现任何 .rs 模块', file=sys.stderr)
        sys.exit(2)

    # 使用 audit_unsafe.py 的扫描函数 (B01-15 修复后的核心)
    total_unsafe = 0
    total_covered = 0
    all_gaps: list[tuple[str, int, str, str]] = []  # (file, line, kind, code)

    for path in files:
        # scan_file 需要绝对路径以满足 relative_to PROJECT_ROOT
        abs_path = path if path.is_absolute() else (PROJECT_ROOT / path).resolve()
        hits = audit_unsafe.scan_file(abs_path)
        for hit in hits:
            total_unsafe += 1
            if hit.has_safety:
                total_covered += 1
            else:
                all_gaps.append((hit.file, hit.line, hit.kind, hit.code))
                if args.missing_only:
                    print(f'{hit.file}\t{hit.line}\t{hit.kind}\t{hit.code[:80]}')

    if args.missing_only:
        # CI 模式下输出 TSV 后直接 exit
        if all_gaps:
            sys.exit(1)
        sys.exit(0)

    # 人类可读报告
    print("=" * 78)
    print("M6.1 SAFETY 完备性审计 — privileged 全量 (B01-13 修复)")
    print("=" * 78)
    print(f"  扫描模块: {len(modules)} 个 (从 privileged/mod.rs 动态发现)")
    print(f"  扫描文件: {len(files)} 个 .rs")
    print(f"  unsafe 引用: {total_unsafe}")
    print(f"  SAFETY 覆盖: {total_covered} ({total_covered * 100 // max(total_unsafe, 1)}%)")
    print(f"  缺 SAFETY: {len(all_gaps)}")
    print("=" * 78)

    if all_gaps:
        # 按文件分组
        by_file: dict[str, list[tuple[int, str, str]]] = {}
        for f, ln, kind, code in all_gaps:
            by_file.setdefault(f, []).append((ln, kind, code))
        # Top 5 缺漏最多文件
        top5 = sorted(by_file.items(), key=lambda x: -len(x[1]))[:5]
        for f, items in top5:
            print(f"  ✗ {f}: {len(items)} 处缺漏")
            for ln, kind, code in items[:3]:
                print(f"    L{ln} ({kind}): {code[:60]}")
            if len(items) > 3:
                print(f"    ... 共 {len(items)} 处")
        print()
        print(f"❌ M6.1 失败: {len(all_gaps)} 处 SAFETY 缺失 (详见 --missing-only)")
        return 1
    else:
        print("\n✓ M6.1 通过: 100% SAFETY 覆盖")
        return 0


if __name__ == '__main__':
    sys.exit(main())
