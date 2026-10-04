#!/usr/bin/env python3
"""
为 edgine 自己的代码中所有 TODO/FIXME/XXX 加上唯一跟踪 ID (TRACK-XXX),
并在 docs/plan/kernel-roadmap.md 末尾追加 "Backlog" 一节集中登记.

排除: smoltcp vendor 目录 (上游代码, edgine 不负责维护).
执行: 干跑只打印改动; 加 --apply 才落盘; --add-backlog 追加 Backlog 节.
"""
import argparse
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src" / "kernel"
ROADMAP = ROOT / "docs" / "plan" / "kernel-roadmap.md"

# 排除 vendor / 上游 / target
EXCLUDE_DIRS = {"smoltcp", "target", ".git"}

# 匹配 TODO / FIXME / XXX (行尾注释或行内注释)
# 例:  // TODO: 实现 ...
#      /* FIXME: ... */
PAT = re.compile(
    r"(\b)(TODO|FIXME|XXX)(\s*[:：]\s*)",
    flags=re.MULTILINE,
)


def make_id(file: Path, line: int, text: str) -> str:
    h = hashlib.sha1(f"{file}:{line}:{text}".encode()).hexdigest()[:6].upper()
    return f"TRACK-{h}"


def should_skip(path: Path) -> bool:
    parts = set(path.relative_to(ROOT).parts)
    return bool(parts & EXCLUDE_DIRS)


def scan(dry: bool = True) -> tuple[list[tuple[Path, int, str, str]], int]:
    """返回 [(file, line, old, new), ...]"""
    out: list[tuple[Path, int, str, str]] = []
    for f in SRC.rglob("*.rs"):
        if should_skip(f):
            continue
        try:
            text = f.read_text(encoding="utf-8", errors="replace")
        except Exception as e:
            print(f"! {f}: {e}", file=sys.stderr)
            continue
        for m in PAT.finditer(text):
            line_no = text.count("\n", 0, m.start()) + 1
            kind = m.group(2)
            # 跳过已跟踪的
            tail = text[m.end():m.end() + 32]
            if "TRACK-" in tail[:16]:
                continue
            old = m.group(0)
            new = f"{kind}({make_id(f, line_no, text[max(0, m.start()-40):m.end()+80])}){m.group(3)}"
            out.append((f, line_no, old, new))
    return out, 0


def apply(changes: list[tuple[Path, int, str, str]]):
    by_file: dict[Path, list[tuple[int, str, str]]] = {}
    for f, ln, old, new in changes:
        by_file.setdefault(f, []).append((ln, old, new))
    for f, items in by_file.items():
        text = f.read_text(encoding="utf-8")
        for ln, old, new in items:
            if old not in text:
                print(f"  ! {f}:{ln} 找不到原文 (可能并发改动), 跳过", file=sys.stderr)
                continue
            text = text.replace(old, new, 1)
        f.write_text(text, encoding="utf-8")


def build_backlog(changes: list[tuple[Path, int, str, str]]) -> str:
    lines = ["", "## Backlog: 过期 TODO 跟踪", "",
             "> 由 `tools/track_todo.py` 自动维护. 每条 `TRACK-XXX` 唯一对应一处未完成项.",
             "> 修复后删除对应行, 并清掉源码中 `TODO(TRACK-XXX)` 标记.",
             ""]
    for f, ln, old, new in changes:
        rid = new.split("(")[1].split(")")[0]
        kind = new.split("(")[0]
        rel = str(f.relative_to(ROOT))
        lines.append(f"- [{rid}] `{rel}:{ln}` {kind}")
    lines.append("")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true", help="落盘修改")
    ap.add_argument("--add-backlog", action="store_true", help="在路线图追加 Backlog 节")
    args = ap.parse_args()

    changes, _ = scan(dry=not args.apply)
    print(f"待标记 {len(changes)} 条")
    if not args.apply:
        for f, ln, old, new in changes[:20]:
            print(f"  {f.relative_to(ROOT)}:{ln}  {old!r} -> {new!r}")
        if len(changes) > 20:
            print(f"  ... 剩余 {len(changes)-20} 条")
        return 0

    apply(changes)
    if args.add_backlog:
        sec = build_backlog(changes)
        if not ROADMAP.exists():
            print(f"! 路线图不存在: {ROADMAP}", file=sys.stderr)
            return 1
        existing = ROADMAP.read_text(encoding="utf-8")
        if "## Backlog: 过期 TODO 跟踪" in existing:
            # 已存在, 替换
            new = re.sub(
                r"## Backlog: 过期 TODO 跟踪\n.*?(?=\n## |\Z)",
                sec.lstrip("\n") + "\n",
                existing,
                flags=re.DOTALL,
            )
        else:
            new = existing.rstrip() + "\n" + sec
        ROADMAP.write_text(new, encoding="utf-8")
        print(f"已追加 {len(changes)} 条到 {ROADMAP.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
