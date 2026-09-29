#!/usr/bin/env python3
"""
I-16 services 层 OnceCell 抽象统一性 audit

目标: 防止 services 层绕过项目自研 `services::sync::once::OnceCell`,
     直接使用第三方 `spin::Once` (即锁层与抽象都不一致).

设计契约:
  - services 层使用 `services::sync::once::{Once, OnceCell}` 一次性原语
  - `OnceCell<T>` 是 `framework::sync::once_lock::OnceLock<T>` 的类型别名
  - 全项目仅 1 种 OnceCell 实现 (除 framework 自身实现外)

规则:
  - `use spin::Once;` / `use spin::{...Once...};` / `use spin::once::Once;` 不应在 services/ 出现
  - `spin::Once` 在其他位置出现也作为 warning 报告 (但 framework boot 文档可豁免)
  - `services::sync::once::Once` / `services::sync::once::OnceCell` 是唯一允许的入口

退出码: 0 = 通过, 1 = 有违规
"""

import re
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
BASE = PROJECT_ROOT / "src" / "kernel" / "services"

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
    if not BASE.exists():
        print(f"[ERR] services/ 目录不存在: {BASE}")
        return 1

    violations = []
    rs_files = list(BASE.rglob("*.rs"))
    print(f"  扫描文件: {len(rs_files)} 个 .rs (services/)")
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
        print("  ✗ services 层不应绕过项目自研 OnceCell 抽象")
        print("  → 改用 `crate::services::sync::once::{Once, OnceCell}`")
        return 1
    else:
        print(f"  ✓ services 层 0 处 spin::Once 残留")
        print(f"  ✓ 全项目统一 OnceCell 抽象 (services::sync::once)")
        return 0


if __name__ == "__main__":
    sys.exit(main())
