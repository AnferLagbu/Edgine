#!/usr/bin/env python3
"""
I-07 C 风格命名残留 audit

目标: 防止 C 风格类型名/函数名 (u8_t/kfree/kmalloc) 在 Rust 代码中残留.

设计契约:
  - 类型名禁用 C 后缀: u8_t / u16_t / u32_t / u64_t / i8_t / i16_t / i32_t / i64_t
  - 函数名遵循 snake_case (clippy non_snake_case)
  - kmalloc/kfree 限 C-ABI extern "C" 函数使用, 不作为常规 Rust 标识符
  - 保留 Linux 兼容名 (如 sys_call_table) 由本脚本白名单

规则:
  - `u8_t` / `u32_t` / `i64_t` 等 C 类型后缀计数 = 0
  - 函数名驼峰或大写: clippy::non_snake_case (本脚本以模式近似)
  - kmalloc/kfree 出现位置必须为 extern "C" 块或 #[no_mangle] 函数

退出码: 0 = 通过, 1 = 有违规
"""

import re
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
BASE = PROJECT_ROOT / "src" / "kernel"

# C 风格类型后缀 (用于类型别名/字段类型/参数)
C_TYPE_SUFFIX = re.compile(r"\b(?:u8|u16|u32|u64|i8|i16|i32|i64|usize|isize)_t\b")

# C 风格函数/方法名 (kfree/kmalloc 出现在 Rust 函数名中)
# 排除: extern "C" / #[no_mangle] / C-ABI 兼容导出
# 模式: 在 fn 关键字后出现 kmalloc/kfree 单词
FN_C_NAMING = re.compile(r"\bfn\s+[a-zA-Z_]*?(kfree|kmalloc)\b")

# 项目内部保留的"kmalloc/kfree"命名 (历史命名约定, 已文档化为非 C-ABI)
# 仅在 mm 命名空间下视为允许
LEGACY_KMALLOC_NAMES = frozenset({
    "get_kmalloc",
})

# 驼峰命名 (用于函数名): [a-z]+[A-Z]
CAMEL_CASE_FN = re.compile(r"\bfn\s+([a-z]+[A-Z][a-zA-Z0-9_]*)\b")

# 允许位置: 注释、字符串字面量、extern "C" 块、宏定义
ALLOW_LINE_MARKERS = (
    "//",  # 注释行
    "*",   # 多行注释
    'extern "C"',  # C-ABI 块
    'extern "system"',
    "#[no_mangle]",
)


def is_allowed_line(line: str, prev_lines: list[str] = None) -> bool:
    stripped = line.lstrip()
    if stripped.startswith("//"):
        return True
    if stripped.startswith("*"):
        return True
    if "extern \"C\"" in line or "extern \"system\"" in line:
        return True
    if "no_mangle" in line:
        return True
    # 检查前 3 行是否含 no_mangle 属性 (跨行属性场景)
    if prev_lines:
        for prev in prev_lines[-3:]:
            if "no_mangle" in prev:
                return True
            if "extern \"C\"" in prev or "extern \"system\"" in prev:
                return True
    return False


def main() -> int:
    if not BASE.exists():
        print(f"error: base dir not found: {BASE}", file=sys.stderr)
        return 1

    issues: list[str] = []

    for rs_file in BASE.rglob("*.rs"):
        try:
            content = rs_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue

        rel = rs_file.relative_to(PROJECT_ROOT)

        lines = content.splitlines()
        # 预计算每行的 extern "C" 块嵌套深度 (O(n) 扫描)
        extern_depth = [0] * (len(lines) + 1)
        depth = 0
        for i, line in enumerate(lines):
            if 'extern "C"' in line and '{' in line:
                depth += 1
            elif depth > 0:
                depth += line.count('{') - line.count('}')
                depth = max(depth, 0)
            extern_depth[i + 1] = depth

        for lineno, line in enumerate(lines, start=1):
            prev_lines = lines[max(0, lineno-4):lineno-1]
            if is_allowed_line(line, prev_lines=prev_lines):
                continue

            in_extern_block = extern_depth[lineno]

            # C 类型后缀
            if C_TYPE_SUFFIX.search(line):
                # 排除字符串中的 C 类型字面量
                m = C_TYPE_SUFFIX.search(line)
                if m:
                    # 排除 "C:\..." 之类路径
                    issues.append(
                        f"{rel}:{lineno}: C 风格类型后缀 `{m.group(0)}`: {line.strip()[:100]}"
                    )

            # C 风格函数名 (kmalloc/kfree 作为 fn 名)
            if FN_C_NAMING.search(line):
                # 排除调用点 (如 kmalloc_align(...)
                if re.search(r"\bfn\s+", line):
                    m = FN_C_NAMING.search(line)
                    if m:
                        # 项目内部保留名 (mm 命名空间下)
                        if "mm/kmalloc" in str(rel) or "mm/api.rs" in str(rel):
                            # 提取完整 fn 名
                            full_match = re.search(r"\bfn\s+([a-zA-Z_][a-zA-Z0-9_]*)\b", line)
                            if full_match and full_match.group(1) in LEGACY_KMALLOC_NAMES:
                                continue
                        # extern "C" 块内的 fn 声明 (FFI import)
                        if in_extern_block > 0:
                            continue
                        issues.append(
                            f"{rel}:{lineno}: C 风格 fn 命名 `{m.group(0)}`: {line.strip()[:100]}"
                        )

            # 驼峰 fn 名 (近似 non_snake_case)
            m = CAMEL_CASE_FN.search(line)
            if m:
                # 排除 #[allow(non_snake_case)]
                if "#[allow(non_snake_case)]" in line or "#[allow(non_snake_case," in line:
                    continue
                issues.append(
                    f"{rel}:{lineno}: 驼峰 fn 名 `{m.group(1)}`: {line.strip()[:100]}"
                )

    if issues:
        print("I-07 audit FAILED:")
        for issue in issues[:50]:
            print(f"  {issue}")
        if len(issues) > 50:
            print(f"  ... and {len(issues) - 50} more")
        return 1

    print("I-07 audit PASSED: 0 C 风格命名残留")
    return 0


if __name__ == "__main__":
    sys.exit(main())
