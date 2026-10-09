#!/usr/bin/env python3
"""
校验「必须与代码同步」的文档中提到的源码路径是否真实存在, 报告漂移项.

校验范围 (AGENTS.md §6 文档定位决定, 不是所有文档都要求路径新鲜):
  - docs/explain/  : 项目引导与解释 —— 描述当前代码形态, 路径失效即文档说谎
  - docs/design/   : 目标架构设计 —— 同上, 引用的机制落点须可定位
  - 根文档         : AGENTS.md / README.md / README.en.md / docs/README.md —— 构建与
                     工具链指引直接指向脚本与产物路径, 失效会误导任何读者
排除范围:
  - docs/plan/ 与 docs/plan/archive/ : 任务规划与历史快照. plan 条目记录的是**执行当时**
      的代码形态 (批次目标文件 / 实测行数 / 已下沉或已删除的模块), 其路径按定义会随后续
      批次失效; §9.2 要求 plan 同步的是状态标记 []/[X], 不是历史路径文本. 把它们计入
      漂移等于要求文档篡改历史记录.
  - docs/report/ : 发布即冻结的结果快照 (同样理由).
"""

import re
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
SRC = ROOT / "src" / "kernel"

# 必须与代码同步的文档 (相对仓库根); 目录项递归, 文件项单点.
SCOPED_PATHS = (
    DOCS / "explain",
    DOCS / "design",
    ROOT / "AGENTS.md",
    ROOT / "README.md",
    ROOT / "README.en.md",
    DOCS / "README.md",
)

# 匹配 docs 中提到的 .rs / .md / 脚本路径
PATH_PAT = re.compile(
    r"`((?:src/(?:kernel|user|rust)/[A-Za-z0-9_./-]+\.(?:rs|md|toml|ld|S|asm))"
    r"|(?:scripts/[A-Za-z0-9_./-]+\.(?:py|sh)))`"
)

# 简化: 也匹配省略 src/ 前缀的 privileged/xxx.rs
PATH_PAT2 = re.compile(
    r"`((?:privileged|functions)/[A-Za-z0-9_./-]+\.(?:rs|md|toml|ld|S|asm))`"
)


def scoped_docs() -> list[Path]:
    """展开校验范围: 目录取全部 *.md, 文件取自身 (不存在的项静默跳过)."""
    out: list[Path] = []
    for entry in SCOPED_PATHS:
        if entry.is_dir():
            out.extend(sorted(entry.rglob("*.md")))
        elif entry.is_file():
            out.append(entry)
    return out


def main() -> int:
    drifted: list[tuple[Path, str, str]] = []  # (doc, missing_path, kind)
    for doc in scoped_docs():
        try:
            text = doc.read_text(encoding="utf-8", errors="replace")
        except Exception:
            continue
        for m in PATH_PAT.finditer(text):
            rel = m.group(1)
            if not (ROOT / rel).exists():
                drifted.append((doc, rel, "src-path"))
        for m in PATH_PAT2.finditer(text):
            rel = m.group(1)
            if not (SRC / rel).exists():
                drifted.append((doc, rel, "sub-path"))
    scanned = len(scoped_docs())
    if not drifted:
        print(f"OK: 无文档路径漂移 (校验 {scanned} 份必须与代码同步的文档)")
        return 0
    print(f"漂移 {len(drifted)} 条 (校验 {scanned} 份文档):")
    for doc, rel, kind in drifted[:50]:
        print(f"  [{kind}] {doc.relative_to(ROOT)}: `{rel}` 不存在")
    if len(drifted) > 50:
        print(f"  ... 剩余 {len(drifted) - 50} 条")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
