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
  - docs/report/ : 正文只读的结果快照 (修订规则见 AGENTS.md §6, 同样理由).

设计册计划路径免报 (docs/design/ 专属, 发现式判据, 不设人工注册表):
  design 文档指向长期演化目标, 合法地引用**尚未新建**的模块名 (如 freg v2 迁移表的
  state.rs / slot.rs). 判据取 git 全历史: 一个不存在的路径若**从未在任何提交中出现过**
  (`git log --all --full-history` 零命中) ⇒ 计划新建, 免报; 若**曾存在而后被删除** ⇒
  退役文件引用 (如 fd_alloc.rs), 仍是真漂移, 照报. 该判据客观可复现, 不依赖人工白名单,
  也不改动设计意图. explain/ 与根文档描述当前代码形态, 不适用此免报 (任何失效路径都报).
"""

import re
import subprocess
from functools import lru_cache
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


@lru_cache(maxsize=None)
def ever_existed_in_git(repo_rel: str) -> bool:
    """路径是否在 git 全历史中出现过 (含已删除). 区分计划新建 vs 退役引用.

    git 不可用或返回非零时保守按「曾存在」处理 (仍报漂移), 不因工具故障而漏报.
    """
    try:
        proc = subprocess.run(
            ["git", "log", "--all", "--full-history", "--format=%H", "--", repo_rel],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError):
        return True
    if proc.returncode != 0:
        return True
    return bool(proc.stdout.strip())


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
    planned_exempt = 0
    design_root = DOCS / "design"
    for doc in scoped_docs():
        try:
            text = doc.read_text(encoding="utf-8", errors="replace")
        except Exception:
            continue
        is_design = design_root in doc.parents
        for pat, base, kind in ((PATH_PAT, ROOT, "src-path"), (PATH_PAT2, SRC, "sub-path")):
            for m in pat.finditer(text):
                rel = m.group(1)
                if (base / rel).exists():
                    continue
                # 设计册中「从未在 git 全历史出现」的路径 = 计划新建模块, 免报;
                # 曾存在而后删除的 = 退役引用, 仍报 (如 fd_alloc.rs).
                if is_design:
                    repo_rel = str((base / rel).relative_to(ROOT))
                    if not ever_existed_in_git(repo_rel):
                        planned_exempt += 1
                        continue
                drifted.append((doc, rel, kind))
    scanned = len(scoped_docs())
    if not drifted:
        msg = f"OK: 无文档路径漂移 (校验 {scanned} 份必须与代码同步的文档"
        if planned_exempt:
            msg += f"; 设计册计划新建路径免报 {planned_exempt} 条 (git 全历史零命中)"
        msg += ")"
        print(msg)
        return 0
    print(f"漂移 {len(drifted)} 条 (校验 {scanned} 份文档):")
    for doc, rel, kind in drifted[:50]:
        print(f"  [{kind}] {doc.relative_to(ROOT)}: `{rel}` 不存在")
    if len(drifted) > 50:
        print(f"  ... 剩余 {len(drifted) - 50} 条")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
