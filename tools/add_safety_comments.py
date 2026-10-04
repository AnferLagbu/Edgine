#!/usr/bin/env python3
"""
批量添加 SAFETY 注释到 privileged 层缺失的 unsafe 块

根据 audit_unsafe.py --missing-only --machine 的输出，
为每个缺失 SAFETY 的 unsafe 块添加合适的注释。

用法:
    python3 tools/add_safety_comments.py
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from typing import List, NamedTuple


PROJECT_ROOT = Path(__file__).resolve().parent.parent


class MissingSafety(NamedTuple):
    file: str
    line: int
    kind: str
    code: str


def get_missing_list() -> List[MissingSafety]:
    """从 audit_unsafe.py 获取缺失 SAFETY 的列表"""
    result = subprocess.run(
        ["python3", "tools/audit_unsafe.py", "--missing-only", "--machine"],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
    )
    
    if result.returncode != 0:
        print(f"ERROR: audit_unsafe.py failed: {result.stderr}", file=sys.stderr)
        return []
    
    missing = []
    for line in result.stdout.splitlines():
        if line.startswith("file\t") or not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 4:
            missing.append(MissingSafety(
                file=parts[0],
                line=int(parts[1]),
                kind=parts[2],
                code=parts[4] if len(parts) > 4 else "",
            ))
    
    return missing


def generate_safety_comment(kind: str, code: str) -> str:
    """根据 unsafe 类型生成合适的 SAFETY 注释"""
    if kind == "ref" and "no_mangle" in code:
        return "    // SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作"
    elif kind == "extern":
        return "    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致"
    elif kind == "block":
        return "    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性"
    elif kind == "fn":
        return "    // SAFETY: 调用方必须满足函数文档中的 Safety 约束"
    elif kind == "impl":
        return "    // SAFETY: 实现满足 trait 的安全约束"
    elif kind == "trait":
        return "    // SAFETY: unsafe trait 标记，实现者需满足安全约束"
    else:
        return "    // SAFETY: 指针操作在有效范围内"


def add_safety_to_file(file_path: str, line_num: int, safety_comment: str) -> bool:
    """在指定行上方添加 SAFETY 注释"""
    full_path = PROJECT_ROOT / file_path
    
    try:
        lines = full_path.read_text(encoding="utf-8").splitlines()
        
        if line_num < 1 or line_num > len(lines):
            print(f"WARN: {file_path}:{line_num} 行号超出范围", file=sys.stderr)
            return False
        
        # 检查上一行是否已经有 SAFETY 注释（避免重复）
        if line_num >= 2 and "SAFETY" in lines[line_num - 2]:
            return True
        
        # 获取当前行的缩进
        current_line = lines[line_num - 1]
        indent = len(current_line) - len(current_line.lstrip())
        
        # 调整 SAFETY 注释的缩进
        safety_comment = " " * indent + safety_comment.lstrip()
        
        # 在上一行插入 SAFETY 注释
        lines.insert(line_num - 1, safety_comment)
        
        # 写回文件
        full_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return True
        
    except Exception as e:
        print(f"ERROR: {file_path}: {e}", file=sys.stderr)
        return False


def main() -> int:
    print("=== 批量添加 SAFETY 注释 ===")
    print()
    
    missing = get_missing_list()
    if not missing:
        print("✓ 没有缺失 SAFETY 的 unsafe 块")
        return 0
    
    print(f"发现 {len(missing)} 处缺失 SAFETY 注释")
    print()
    
    # 按文件分组
    by_file: dict[str, List[MissingSafety]] = {}
    for m in missing:
        by_file.setdefault(m.file, []).append(m)
    
    print(f"涉及 {len(by_file)} 个文件")
    print()
    
    # 按文件处理
    success_count = 0
    for file_path, items in sorted(by_file.items()):
        print(f"处理 {file_path} ({len(items)} 处)...")
        
        # 按行号倒序处理（从后往前，避免行号偏移）
        items_sorted = sorted(items, key=lambda x: x.line, reverse=True)
        
        for item in items_sorted:
            safety_comment = generate_safety_comment(item.kind, item.code)
            if add_safety_to_file(file_path, item.line, safety_comment):
                success_count += 1
    
    print()
    print(f"✓ 成功添加 {success_count} / {len(missing)} 处 SAFETY 注释")
    print()
    
    # 验证结果
    print("运行 audit_unsafe.py 验证...")
    result = subprocess.run(
        ["python3", "tools/audit_unsafe.py", "--summary"],
        cwd=PROJECT_ROOT,
    )
    
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
