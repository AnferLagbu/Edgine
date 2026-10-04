#!/usr/bin/env python3
"""
扫 docs/ 文档中提到的 src/kernel/privileged/xxx.rs 路径, 检查实际是否存在.
标记漂移项 (文档里写但代码里没有的文件).
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
SRC = ROOT / "src" / "kernel"

# 匹配 docs 中提到的 .rs / .md / 脚本路径
PATH_PAT = re.compile(
    r"`((?:src/(?:kernel|user|rust)/[A-Za-z0-9_./-]+\.(?:rs|md|toml|ld|S|asm))"
    r"|(?:scripts/[A-Za-z0-9_./-]+\.(?:py|sh)))`"
)

# 简化: 也匹配省略 src/ 前缀的 privileged/xxx.rs
PATH_PAT2 = re.compile(
    r"`((?:privileged|functions)/[A-Za-z0-9_./-]+\.(?:rs|md|toml|ld|S|asm))`"
)


def main() -> int:
    drifted: list[tuple[Path, str, str]] = []  # (doc, missing_path, kind)
    for doc in DOCS.rglob("*.md"):
        try:
            text = doc.read_text(encoding="utf-8", errors="replace")
        except Exception:
            continue
        for m in PATH_PAT.finditer(text):
            rel = m.group(1)
            full = ROOT / rel
            if not full.exists():
                drifted.append((doc, rel, "src-path"))
        for m in PATH_PAT2.finditer(text):
            rel = m.group(1)
            full = SRC / rel
            if not full.exists():
                drifted.append((doc, rel, "sub-path"))
    if not drifted:
        print("OK: 无文档路径漂移")
        return 0
    print(f"漂移 {len(drifted)} 条:")
    for doc, rel, kind in drifted[:50]:
        print(f"  [{kind}] {doc.relative_to(ROOT)}: `{rel}` 不存在")
    if len(drifted) > 50:
        print(f"  ... 剩余 {len(drifted)-50} 条")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
