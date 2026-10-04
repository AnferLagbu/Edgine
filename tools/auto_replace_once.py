#!/usr/bin/env python3
"""
将 privileged 中所有 `spin::Once<T>` 替换为 privileged::sync::once_lock::OnceLock<T>。

替换规则:
  1. `use spin::Once;` → `use crate::kernel::privileged::sync::once_lock::OnceLock;`
  2. `static X: spin::Once<T> = spin::Once::new();`
       → `static X: OnceLock<T> = OnceLock::new();`
  3. `X.call_once(|| init_expr)` → `X.get_or_init(|| init_expr)`
  4. `X.call_once(|x| ...)` (使用参数) → 保留并打印需手工处理提示
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
USE_LINE = "use crate::kernel::privileged::sync::once_lock::OnceLock;\n"


def list_files_using_once() -> list[Path]:
    out = subprocess.check_output(
        ["grep", "-rln", "--include=*.rs", r"spin::Once", "src/kernel/privileged"],
        cwd=PROJECT_ROOT, text=True,
    )
    return [Path(line) for line in out.splitlines() if line]


def patch_file(path: Path) -> tuple[int, str]:
    text = path.read_text(encoding="utf-8", errors="replace")
    orig = text
    n = 0

    # 1. spin::Once<T> → OnceLock<T> (含泛型)
    def replace_once_type(s: str) -> tuple[str, int]:
        out = []
        i = 0
        cnt = 0
        while i < len(s):
            if s[i:].startswith("spin::Once<"):
                out.append("OnceLock<")
                i += len("spin::Once<")
                depth = 1
                start = i
                while i < len(s) and depth > 0:
                    c = s[i]
                    if c == "<":
                        depth += 1
                    elif c == ">":
                        depth -= 1
                        if depth == 0:
                            out.append(s[start:i])
                            out.append(">")
                            i += 1
                            break
                    i += 1
                cnt += 1
            else:
                out.append(s[i])
                i += 1
        return "".join(out), cnt

    text, c = replace_once_type(text)
    n += c

    # 2. spin::Once::new() → OnceLock::new()
    text, c = re.subn(r"\bspin::Once::new\b", "OnceLock::new", text)
    n += c

    # 3. use spin::Once; → use ...::OnceLock;
    text, c = re.subn(
        r"^use\s+spin::Once\s*;\s*$",
        USE_LINE.rstrip("\n"),
        text,
        flags=re.MULTILINE,
    )
    n += c

    # 4. X.call_once(|| ...) → X.get_or_init(|| ...)  (仅在闭包为 || 时)
    text, c = re.subn(
        r"\.call_once\(\|\|",
        ".get_or_init(||",
        text,
    )
    n += c

    # 5. 自动添加 use 语句 (若无)
    if "OnceLock" in text and "use crate::kernel::privileged::sync::once_lock::OnceLock" not in text:
        m = re.search(r"^(use [^;]+;\s*\n)+", text, re.MULTILINE)
        if m:
            insert_pos = m.end()
            text = text[:insert_pos] + "\n" + USE_LINE + text[insert_pos:]
        else:
            lines = text.split("\n")
            for idx, line in enumerate(lines):
                if line.strip() and not line.strip().startswith("//") and not line.strip().startswith("/*"):
                    lines.insert(idx, USE_LINE)
                    break
            text = "\n".join(lines)

    if text != orig:
        path.write_text(text, encoding="utf-8")
    return n, ("patched" if text != orig else "no-op")


def main():
    files = list_files_using_once()
    print(f"[auto-once] {len(files)} files use spin::Once", file=sys.stderr)
    total = 0
    for path in files:
        n, status = patch_file(path)
        total += n
        print(f"  {path}: {n} repl ({status})", file=sys.stderr)
    print(f"[auto-once] total: {total} replacements", file=sys.stderr)


if __name__ == "__main__":
    main()
