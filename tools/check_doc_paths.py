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

两种引用形态都校验 (同一存在性判据 + 同一设计册免报):
  - 反引号源码路径: `src/kernel/...rs` / `scripts/...py` / 省略前缀的 `privileged/...rs`.
  - markdown 链接源码引用: [文字](目标) —— 本仓主力引用风格, 数量远多于反引号.
    目标解析约定: 剥掉 #Lnn 行锚点; http(s)/mailto 等外部链接跳过; `../`/`./` 按文档
    所在目录相对解析; 裸 `src/...`/`scripts/...` 按**仓库根**相对解析 (设计册约定, 不是
    文档目录相对); file:// 与 / 开头按绝对路径. 只判源码/脚本扩展名
    (.rs/.py/.sh/.toml/.ld/.S/.asm), doc→doc 的 .md 链接不在「源码路径新鲜度」射程.
    解析后越出仓库根的目标跳过 (无法判定外部路径). 同一文件被两种形态各引一次只报一条.
"""

import os
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

# markdown 链接目标: [文字](目标) —— 取括号内整体, 调用侧再剥可选 title 与行锚点.
MD_LINK_PAT = re.compile(r"\]\(([^)]+)\)")

# markdown 面只判源码/脚本扩展名 (与反引号面的源码侧同集合, 不含 .md: doc→doc 链接
# 属文档间引用, 不在「源码路径新鲜度」射程; 归档移动那类漂移由别的防线管).
SOURCE_EXTS = {".rs", ".py", ".sh", ".toml", ".ld", ".S", ".asm"}


def resolve_link(target: str, doc: Path) -> Path | None:
    """把 markdown 链接目标解析成绝对路径 (外部链接 / 纯锚点返回 None).

    约定见模块 docstring: ../ 与 ./ 按文档目录相对, 裸 src/ 等按仓库根相对.
    用 normpath 折叠 .. (不跟随符号链接), 以便后续 relative_to(ROOT) 成立.
    """
    path = target.split("#", 1)[0].strip()
    if not path or path.startswith(("http://", "https://", "mailto:", "ftp://")):
        return None
    if path.startswith("file://"):
        raw = Path(path[7:])
    elif path.startswith("/"):
        raw = Path(path)
    elif path.startswith("../") or path.startswith("./"):
        raw = doc.parent / path
    else:
        raw = ROOT / path
    return Path(os.path.normpath(raw))


def classify_target(abs_path: Path, is_design: bool) -> tuple[str, str]:
    """判一个已解析路径: 返回 (仓库相对路径, 判定 ok/exempt/drift).

    越出仓库根的目标不在射程, 按 ok 跳过. 存在 ⇒ ok; 不存在且属设计册且 git 全历史
    零命中 ⇒ exempt (计划新建); 其余不存在 ⇒ drift (退役引用或写错路径).
    """
    try:
        rel = str(abs_path.relative_to(ROOT))
    except ValueError:
        return ("", "ok")
    if abs_path.exists():
        return (rel, "ok")
    if is_design and not ever_existed_in_git(rel):
        return (rel, "exempt")
    return (rel, "drift")


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
    seen: set[tuple[Path, str]] = set()  # (doc, 仓库相对路径): 两形态各引一次只报一条
    planned_exempt = 0
    design_root = DOCS / "design"
    for doc in scoped_docs():
        try:
            text = doc.read_text(encoding="utf-8", errors="replace")
        except Exception:
            continue
        is_design = design_root in doc.parents
        # 反引号面 (既有判据不动): src/scripts 全路径 + 省略前缀的 privileged/functions.
        for pat, base, kind in ((PATH_PAT, ROOT, "src-path"), (PATH_PAT2, SRC, "sub-path")):
            for m in pat.finditer(text):
                rel = m.group(1)
                canon, verdict = classify_target(base / rel, is_design)
                if verdict == "exempt":
                    planned_exempt += 1
                elif verdict == "drift" and (doc, canon) not in seen:
                    seen.add((doc, canon))
                    drifted.append((doc, rel, kind))
        # markdown 链接面 (新增): 只判源码/脚本扩展名, 同一存在性判据 + 设计册免报.
        for m in MD_LINK_PAT.finditer(text):
            inner = m.group(1).strip()
            if not inner:
                continue
            target = inner.split(maxsplit=1)[0]  # 剥掉可选的 "title"
            abs_path = resolve_link(target, doc)
            if abs_path is None or abs_path.suffix not in SOURCE_EXTS:
                continue
            canon, verdict = classify_target(abs_path, is_design)
            if verdict == "exempt":
                planned_exempt += 1
            elif verdict == "drift" and (doc, canon) not in seen:
                seen.add((doc, canon))
                drifted.append((doc, target, "md-link"))
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
