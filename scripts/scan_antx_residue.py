#!/usr/bin/env python3
"""
AntX 命名残留扫描器 (AntX Naming Residue Scanner)

按用户要求检查项目代码中遗漏的 AntX 命名引用, 输出分类报告.

## 用法

```bash
# 全部扫描 (默认排除历史归档/构建产物/第三方)
python3 scripts/scan_antx_residue.py

# 包含历史归档 (CHANGELOG/docs/plan/archive)
python3 scripts/scan_antx_residue.py --include-archive

# 只看需修改项 (排除保留项: 发行版概念 + 历史决策)
python3 scripts/scan_antx_residue.py --actionable-only

# 严格模式: 把所有变体 (AntX/antx/ANTX) 都视为需修改
python3 scripts/scan_antx_residue.py --strict

# 输出 JSON 报告
python3 scripts/scan_antx_residue.py --json
```

## 分类 (5 类)

1. **SOURCE** 源码 (.rs): 注释/字符串字面值, 改动安全
2. **CONFIG** 配置 (.toml/.clippy/.lock): 需评估是否影响构建
3. **SCRIPT** 脚本 (.sh/.py): 注释/路径/版权
4. **DOC** 文档 (.md): 描述/历史/链接
5. **BUILD** 构建产物 (Cargo.lock 已忽略, target/* 已忽略)

## 保留判断 (5 种)

1. 发行版概念: README 中 "AntX 是未来发行版代号" (合理保留)
2. 历史决策: CHANGELOG/AGENTS/docs/plan/archive 中历史记录
3. 第三方: smoltcp vendored
4. 构建产物: target/*, *.lock (committed)
5. .git/

## 输出格式

每个引用含:
- file: 相对路径
- line: 行号
- col: 列号
- context: 该行内容
- category: SOURCE/CONFIG/SCRIPT/DOC
- action: KEEP (合理保留) / MODIFY (需改) / REVIEW (需评估)
- reason: 分类原因

## 退出码

- 0: 无需修改项 (所有 AntX 引用都合理保留)
- 1: 有需修改项 (--actionable-only 模式下)
- 2: 错误
"""

import os
import re
import sys
import json
import argparse
from pathlib import Path
from typing import NamedTuple
from enum import Enum

PROJECT_ROOT = Path(__file__).resolve().parents[1]


class Category(str, Enum):
    SOURCE = "SOURCE"   # .rs 源码
    CONFIG = "CONFIG"   # .toml / .clippy / .lock
    SCRIPT = "SCRIPT"   # .sh / .py
    DOC = "DOC"         # .md
    BUILD = "BUILD"     # Cargo.lock committed
    OTHER = "OTHER"     # 其他


class Action(str, Enum):
    KEEP = "KEEP"       # 合理保留 (历史/概念)
    MODIFY = "MODIFY"   # 需改
    REVIEW = "REVIEW"   # 需评估


class Hit(NamedTuple):
    file: str
    line: int
    col: int
    context: str
    category: Category
    action: Action
    reason: str
    pattern: str  # AntX / antx / ANTX / antx-host-tests


# 排除规则
EXCLUDE_DIRS = {
    ".git",
    "target",
    "isodir",
    "build",
    "other",
    "node_modules",
    "__pycache__",
    ".mypy_cache",
}

# smoltcp 是第三方 vendored, 排除
THIRD_PARTY_DIRS = {
    "src/kernel/functions/net/smoltcp",  # smoltcp 0.13 vendored
}

# 历史归档: 保留 (不可改)
HISTORY_PATHS = {
    "CHANGELOG.md",
    "AGENTS.md",  # 变更历史段
    "docs/CHANGELOG.md",
    "docs/plan/maintenance-cycle-2026-06-19.md",  # 维护历史
}

# 历史归档目录
HISTORY_DIRS = {
    "docs/plan/archive",
}

# 文件扩展名分类
EXT_CATEGORY = {
    ".rs": Category.SOURCE,
    ".toml": Category.CONFIG,
    ".lock": Category.BUILD,
    ".sh": Category.SCRIPT,
    ".py": Category.SCRIPT,
    ".md": Category.DOC,
    ".txt": Category.DOC,
    ".yml": Category.SCRIPT,
    ".yaml": Category.SCRIPT,
}


def should_exclude(path: Path) -> bool:
    """检查路径是否应排除 (target/.git/第三方)"""
    parts = set(path.parts)
    if parts & EXCLUDE_DIRS:
        return True
    rel = str(path.relative_to(PROJECT_ROOT)) if path.is_absolute() else str(path)
    for tp in THIRD_PARTY_DIRS:
        if rel.startswith(tp):
            return True
    return False


def get_category(path: Path) -> Category:
    """根据扩展名分类"""
    return EXT_CATEGORY.get(path.suffix.lower(), Category.OTHER)


def is_history(path: Path) -> bool:
    """检查是否为历史归档/不可改"""
    rel = str(path.relative_to(PROJECT_ROOT)) if path.is_absolute() else str(path)
    if rel in HISTORY_PATHS:
        return True
    for hd in HISTORY_DIRS:
        if rel.startswith(hd):
            return True
    return False


def classify_action(path: Path, context: str, pattern: str) -> tuple[Action, str]:
    """判断是 KEEP/MODIFY/REVIEW"""
    rel = str(path.relative_to(PROJECT_ROOT)) if path.is_absolute() else str(path)
    cat = get_category(path)

    # 1. 发行版概念保留 (README.md 第 3 段)
    if rel == "README.md" and "AntX" in context and "发行版" in context:
        return Action.KEEP, "README 发行版概念 (AntX 留作未来 OS 代号)"

    # 2. 历史决策保留
    if is_history(path):
        # 但历史中也可能有"实装中/进行中"条目
        if "尚未" in context or "待实装" in context or "TODO" in context:
            return Action.REVIEW, "历史归档中可能需更新"
        return Action.KEEP, "历史归档 (不可改)"

    # 3. AGENTS.md 变更历史段保留
    if rel == "AGENTS.md" and ("变更历史" in context or "| 日期" in context or "| 2026-" in context):
        return Action.KEEP, "AGENTS 变更历史记录"

    # 4. 硬编码路径 (仓库绝对路径) - 需改
    if pattern in ("AntX", "antx") and "/home/anfer/Code/AntX" in context:
        return Action.MODIFY, "硬编码仓库路径 (仓库改名后失效)"

    # 5. Cargo crate name (antx-host-tests 等) - 影响构建
    if "antx-host-tests" in context or "antx_host_tests" in pattern:
        return Action.REVIEW, "Cargo crate name (改会破坏 host-tests 构建)"

    # 6. Cargo package name (axsh → eash 同类) - 源码 Cargo.toml
    if cat == Category.CONFIG and re.search(r'name\s*=\s*["\']antx', context, re.IGNORECASE):
        return Action.REVIEW, "Cargo package name (需评估是否改)"

    # 7. 注释/版权/描述 - 安全改
    if cat in (Category.SOURCE, Category.SCRIPT) and (
        "Copyright" in context or "Copyright (c)" in context
        or "# " in context or "// " in context
    ):
        return Action.MODIFY, "源码/脚本注释 (安全改)"

    # 8. 字符串字面值 - 影响运行时, 需评估
    if cat == Category.SOURCE and ('"' in context or "'" in context):
        return Action.REVIEW, "源码字符串字面值 (需评估运行时影响)"

    # 9. 文档描述 - 需改
    if cat == Category.DOC:
        return Action.MODIFY, "文档描述 (需改)"

    # 10. 其他 - 需改
    return Action.MODIFY, "通用 AntX 引用 (需改)"


# 4 种匹配模式
PATTERNS = [
    (r"\bAntX\b", "AntX"),                # 标准
    (r"\bantx\b", "antx"),                # 小写
    (r"\bANTX\b", "ANTX"),                # 大写
    (r"\bantx[-_]host[-_]tests\b", "antx-host-tests"),  # crate 名
]


def scan_file(path: Path, include_archive: bool, strict: bool) -> list[Hit]:
    """扫描单个文件"""
    if should_exclude(path) and not include_archive:
        return []

    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except Exception:
        return []

    cat = get_category(path)
    hits = []

    for line_num, line in enumerate(text.splitlines(), start=1):
        for pattern, name in PATTERNS:
            if not strict and name == "antx" and "antx-host-tests" not in line:
                # 非 strict 模式下, 跳过 "antx" (小写) 一般是误报 (english word "ant")
                # 除非整词是 "antx" 独立
                if not re.search(r"\bantx\b", line):
                    continue
            for match in re.finditer(pattern, line):
                action, reason = classify_action(path, line.strip(), name)
                if not strict and action == Action.MODIFY and "antx" == name and len(line.strip()) < 50:
                    # 短行小写 antx 误报率高, 跳过
                    continue
                hits.append(Hit(
                    file=str(path.relative_to(PROJECT_ROOT)),
                    line=line_num,
                    col=match.start() + 1,
                    context=line.strip()[:120],
                    category=cat,
                    action=action,
                    reason=reason,
                    pattern=name,
                ))

    return hits


def main():
    parser = argparse.ArgumentParser(
        description="扫描 AntX 命名残留 (AntX / antx / ANTX / antx-host-tests)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--include-archive", action="store_true",
                        help="包含历史归档 (CHANGELOG/docs/plan/archive)")
    parser.add_argument("--actionable-only", action="store_true",
                        help="只看需修改项 (排除 KEEP)")
    parser.add_argument("--strict", action="store_true",
                        help="严格模式: 把 antx/ANTX 也视为需修改")
    parser.add_argument("--json", action="store_true",
                        help="输出 JSON 报告")
    args = parser.parse_args()

    all_hits: list[Hit] = []
    files_scanned = 0

    # 扫描所有相关扩展
    extensions = {".rs", ".toml", ".sh", ".py", ".md", ".txt", ".yml", ".yaml"}
    # Cargo.lock 仅在 host-tests 提交时扫描
    for lock in PROJECT_ROOT.rglob("Cargo.lock"):
        if not should_exclude(lock):
            files_scanned += 1
            all_hits.extend(scan_file(lock, args.include_archive, args.strict))

    for ext in extensions:
        for path in PROJECT_ROOT.rglob(f"*{ext}"):
            if should_exclude(path):
                continue
            files_scanned += 1
            all_hits.extend(scan_file(path, args.include_archive, args.strict))

    # 过滤
    if args.actionable_only:
        all_hits = [h for h in all_hits if h.action != Action.KEEP]

    # 按 action 排序: MODIFY > REVIEW > KEEP
    action_order = {Action.MODIFY: 0, Action.REVIEW: 1, Action.KEEP: 2}
    all_hits.sort(key=lambda h: (action_order[h.action], h.file, h.line))

    # JSON 输出
    if args.json:
        output = {
            "scan_summary": {
                "files_scanned": files_scanned,
                "total_hits": len(all_hits),
                "by_action": {
                    a.value: sum(1 for h in all_hits if h.action == a)
                    for a in Action
                },
                "by_category": {
                    c.value: sum(1 for h in all_hits if h.category == c)
                    for c in Category
                },
                "by_pattern": {
                    p: sum(1 for h in all_hits if h.pattern == p)
                    for _, p in PATTERNS
                },
            },
            "hits": [h._asdict() for h in all_hits],
        }
        print(json.dumps(output, ensure_ascii=False, indent=2))
        return 0 if (not args.actionable_only or not all_hits) else 1

    # 文本输出
    print("=" * 70)
    print("AntX 命名残留扫描报告 (AntX / antx / ANTX / antx-host-tests)")
    print("=" * 70)
    print(f"扫描文件: {files_scanned}")
    print(f"命中总数: {len(all_hits)}")
    print()
    print("按 action 统计:")
    for a in Action:
        n = sum(1 for h in all_hits if h.action == a)
        print(f"  {a.value:8s}: {n}")
    print()
    print("按 category 统计:")
    for c in Category:
        n = sum(1 for h in all_hits if h.category == c)
        if n > 0:
            print(f"  {c.value:8s}: {n}")
    print()
    print("按 pattern 统计:")
    for _, p in PATTERNS:
        n = sum(1 for h in all_hits if h.pattern == p)
        if n > 0:
            print(f"  {p:20s}: {n}")
    print()

    # 按 action 分组显示
    for action in Action:
        action_hits = [h for h in all_hits if h.action == action]
        if not action_hits:
            continue
        print("=" * 70)
        print(f"[{action.value}] 共 {len(action_hits)} 项 - {action_legend(action)}")
        print("=" * 70)
        # 按文件分组
        by_file: dict[str, list[Hit]] = {}
        for h in action_hits:
            by_file.setdefault(h.file, []).append(h)
        for file, file_hits in by_file.items():
            print(f"\n  {file} ({len(file_hits)} 处):")
            for h in file_hits[:5]:  # 每文件最多显示 5 处
                print(f"    L{h.line:5d}:{h.col:3d}  [{h.pattern:18s}] {h.context[:80]}")
                print(f"             原因: {h.reason}")
            if len(file_hits) > 5:
                print(f"    ... (还有 {len(file_hits) - 5} 处)")

    # 退出码
    has_actionable = any(h.action != Action.KEEP for h in all_hits)
    if args.actionable_only and has_actionable:
        return 1
    return 0


def action_legend(a: Action) -> str:
    return {
        Action.MODIFY: "需修改 (注释/描述/路径, 安全改)",
        Action.REVIEW: "需评估 (字符串字面值/Cargo name, 改前需评估)",
        Action.KEEP: "合理保留 (历史/发行版概念, 不可改)",
    }[a]


if __name__ == "__main__":
    sys.exit(main())
