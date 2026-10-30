#!/usr/bin/env python3
"""
I-16 OnceCell 抽象统一性 audit (四树: src/kernel / host-tests / src/user / kani-harness)

目标: 防止 functions 层绕过项目自研 `functions::sync::once::OnceCell`,
     直接使用第三方 `spin::Once` (即锁层与抽象都不一致).

设计契约:
  - functions 层使用 `functions::sync::once::{Once, OnceCell}` 一次性原语
  - `OnceCell<T>` 是 `privileged::sync::once_lock::OnceLock<T>` 的类型别名
  - 全项目仅 1 种 OnceCell 实现 (除 privileged 自身实现外)

规则:
  - `use spin::Once;` / `use spin::{...Once...};` / `use spin::once::Once;` 不应在任一覆盖树出现
  - `spin::Once` 在注释中出现不计 (注释行跳过)
  - `functions::sync::once::Once` / `functions::sync::once::OnceCell` 是唯一允许的入口

覆盖根 (P-60 甲): 「全项目统一」的宣称要求四树全扫 —— `src/kernel` / `host-tests` /
`src/user` / `kani-harness`. 原实现只扫 `src/kernel/functions`, 另三树不在射程.
逐根 fail-closed: 任一覆盖根缺失即报配置错误并返 1, 不接受「少扫一棵树读成零违规」.

退出码: 0 = 通过, 1 = 有违规或覆盖根缺失
"""

import re
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
# 覆盖根 (P-60 甲・四树): 本审计的宣称是「全项目统一 OnceCell 抽象」, 原实现只扫
# functions/ ⇒ host-tests / src/user / kani-harness 三树不在射程, 「全项目」名不副实.
BASES = [
    PROJECT_ROOT / "src" / "kernel",
    PROJECT_ROOT / "host-tests",
    PROJECT_ROOT / "src" / "user",
    PROJECT_ROOT / "kani-harness",
]

# B01-08 修复: 正则支持 `pub use spin::Once` / `use spin::OnceCell` / `use spin::once::Once`
# 等形式. 原正则锚定 `^\s*use` 不匹配 `pub use`, `Once\b` 词边界不命中 `OnceCell`.
USE_SPIN_ONCE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+(?:crate::)?spin\s*::\s*(?:once\s*::\s*)?(?:Once|OnceCell)\b"
    r"|^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+spin\s*::\s*\{[^}]*\b(?:Once|OnceCell)\b[^}]*\}"
)

# 匹配代码行内的 `spin::Once` / `spin::OnceCell` (排除注释)
SPIN_ONCE_IN_CODE = re.compile(
    r"[^/]\bspin\s*::\s*OnceCell?\b|\bspin\s*::\s*OnceCell?\b[^/]"
)


def main() -> int:
    missing = [str(b.relative_to(PROJECT_ROOT)) for b in BASES if not b.exists()]
    if missing:
        print(f"[ERR] 覆盖根缺失 (fail-closed, P-60 甲): {', '.join(missing)}")
        return 1

    violations = []
    rs_files = sorted(p for b in BASES for p in b.rglob("*.rs"))
    print(f"  扫描文件: {len(rs_files)} 个 .rs (四树: src/kernel / host-tests / src/user / kani-harness)")
    print(f"  检查模式: use spin::Once / spin::Once 残留")
    print("  " + "-" * 60)

    for rs in rs_files:
        try:
            text = rs.read_text(encoding="utf-8", errors="replace")
        except Exception as e:
            print(f"  [WARN] 无法读取 {rs}: {e}")
            continue

        for lineno, line in enumerate(text.splitlines(), start=1):
            stripped = line.lstrip()
            # 跳过纯注释行 (// 开头) 与块注释行 (* 开头)
            if stripped.startswith("//") or stripped.startswith("*") or stripped.startswith("/*"):
                continue
            if USE_SPIN_ONCE.search(line) or SPIN_ONCE_IN_CODE.search(line):
                # 进一步排除: 行内注释部分含 `spin::Once` (但前面是 // 注释)
                # 通过查找 // 出现位置, 取代码段
                code_part = line.split("//", 1)[0] if "//" in line else line
                if USE_SPIN_ONCE.search(code_part) or SPIN_ONCE_IN_CODE.search(code_part):
                    violations.append((rs, lineno, line.rstrip()))

    if violations:
        print(f"  [FAIL] 发现 {len(violations)} 处 spin::Once 残留:")
        for rs, lineno, line in violations[:20]:
            rel = rs.relative_to(PROJECT_ROOT)
            print(f"    {rel}:{lineno}: {line}")
        if len(violations) > 20:
            print(f"    ... 另有 {len(violations) - 20} 处省略")
        print("  " + "-" * 60)
        print("  ✗ 覆盖树内不应有绕过 OnceCell 抽象的 spin::Once 残留")
        print("  → 改用 `crate::functions::sync::once::{Once, OnceCell}`")
        return 1
    else:
        print(f"  ✓ 四树 0 处 spin::Once 残留")
        print(f"  ✓ 全项目统一 OnceCell 抽象 (functions::sync::once)")
        return 0


if __name__ == "__main__":
    sys.exit(main())
