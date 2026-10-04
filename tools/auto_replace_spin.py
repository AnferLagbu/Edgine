#!/usr/bin/env python3
"""
批量把 privileged 内所有 `spin::Mutex<T>` / `spin::Mutex::new(...)` 替换为
`IrqSpinLock<T>` / `IrqSpinLock::new(...)`, 并自动添加 use 语句.

策略:
  1. 扫描 `spin::Mutex` 和 `spin::Mutex::new` 所有出现
  2. 替换文本 (类型/init)
  3. 若文件没有 `use crate::kernel::privileged::sync::irq_spinlock::IrqSpinLock`, 在 use 区添加
  4. 不动 .lock() 调用 (Guard API 兼容)
  5. 跳过 `spin::Once` / `spin::OnceCell` (已识别的 init-once 模式, 单独处理)
  6. 跳过 host-tests / 第三方目录 (miri-tests 已删除 2026-06-26)

输出: 直接 patch 文件
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SCAN_DIR = "src/kernel/privileged"

USE_LINE = "use crate::kernel::privileged::sync::irq_spinlock::IrqSpinLock;\n"


def list_files_using_spin() -> list[Path]:
    out = subprocess.check_output(
        ["grep", "-rln", "--include=*.rs", r"spin::Mutex", SCAN_DIR],
        cwd=PROJECT_ROOT, text=True,
    )
    return [Path(line) for line in out.splitlines() if line]


def patch_file(path: Path) -> tuple[int, str]:
    text = path.read_text(encoding="utf-8", errors="replace")
    orig = text
    n_replaced = 0

    # 1. spin::Mutex::new(...) → IrqSpinLock::new(...)
    new_text, n = re.subn(r"\bspin::Mutex::new\b", "IrqSpinLock::new", text)
    n_replaced += n
    text = new_text

    # 2. spin::Mutex<T> → IrqSpinLock<T> (含泛型)
    def replace_mutex_type(s: str) -> tuple[str, int]:
        out = []
        i = 0
        n = 0
        while i < len(s):
            m = re.match(r"\bspin::Mutex<", s[i:])
            if not m:
                out.append(s[i])
                i += 1
                continue
            i += m.end()
            depth = 1
            while i < len(s) and depth > 0:
                c = s[i]
                if c == "<":
                    depth += 1
                elif c == ">":
                    depth -= 1
                    if depth == 0:
                        i += 1
                        break
                i += 1
            out.append("IrqSpinLock<" + s[i - (1 + 1):i][1:])
            # 上方处理不严谨, 重新写
            n += 1
        return "".join(out), n

    # 上方 replace_mutex_type 的实际替换有 bug, 改用更稳健的方法
    def robust_replace_mutex_type(s: str) -> tuple[str, int]:
        out = []
        i = 0
        n = 0
        while i < len(s):
            if s[i:].startswith("spin::Mutex<"):
                out.append("IrqSpinLock<")
                i += len("spin::Mutex<")
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
                n += 1
            else:
                out.append(s[i])
                i += 1
        return "".join(out), n

    new_text, n = robust_replace_mutex_type(text)
    n_replaced += n
    text = new_text

    # 3. use spin::Mutex; → use crate::kernel::privileged::sync::irq_spinlock::IrqSpinLock as Mutex;
    #    这样既兼容 `Mutex<T>` 类型使用, 又不引入第三方依赖
    new_text, n = re.subn(
        r"^use\s+spin::Mutex\s*;\s*$",
        "use crate::kernel::privileged::sync::irq_spinlock::IrqSpinLock as Mutex;",
        text,
        flags=re.MULTILINE,
    )
    n_replaced += n
    text = new_text

    # 4. `use spin::Mutex;` 在 use-group 中
    new_text, n = re.subn(
        r"(\buse\s+spin::\{[^}]*\b)Mutex\b([^}]*\}\s*;)",
        r"\1IrqSpinLock\2",
        text,
    )
    n_replaced += n
    text = new_text

    # 5. 自动添加 use IrqSpinLock 语句 (若无)
    if "IrqSpinLock" in text and "use crate::kernel::privileged::sync::irq_spinlock::IrqSpinLock" not in text:
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
    return n_replaced, ("patched" if text != orig else "no-op")


def main():
    files = list_files_using_spin()
    print(f"[auto-replace] {len(files)} files use spin::Mutex", file=sys.stderr)
    total = 0
    for path in files:
        n, status = patch_file(path)
        total += n
        print(f"  {path}: {n} repl ({status})", file=sys.stderr)
    print(f"[auto-replace] total: {total} replacements", file=sys.stderr)


if __name__ == "__main__":
    main()
