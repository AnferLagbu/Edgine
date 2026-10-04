#!/usr/bin/env python3
"""
I-43 块设备抽象统一性 audit

目标: 防止新驱动绕过 `proto_block::register_block_device` (BlockDevice trait 桥接),
     直接调用低层 `egdf_register_block` 导致 UNKFS 看不到该驱动.

设计契约:
  - 驱动实现 `BlockDevice` trait (driver interface, Rust OO)
  - 驱动通过 `proto_block::register_block_device` 注册 (单一桥接入口)
  - UNKFS 通过 `egdf_blk_read/write` I/O (统一 EGDF 路径)
  - 低层 `egdf_register_block` 仅由 `proto_block` 桥接函数调用

规则:
  - `egdf_register_block(` 出现在以下位置允许:
      * src/kernel/framework/egdf/mod.rs (定义 + 单元测试)
      * src/kernel/framework/egdf/proto_block.rs (桥接: register_block_device / register_block_device_with_ops)
  - 其他位置出现 → 违规 (应改用 proto_block::register_block_device)

退出码: 0 = 通过, 1 = 有违规
"""

import os
import re
import sys
from pathlib import Path

BASE = Path('src/kernel')

# 允许直接调用 egdf_register_block 的文件 (桥接 + 定义 + 单元测试)
ALLOWED_FILES = {
    Path('src/kernel/framework/egdf/mod.rs'),
    Path('src/kernel/framework/egdf/proto_block.rs'),
}

# 匹配 `egdf_register_block_dev(` 调用 (排除 egdf_register_with_ops / egdf_register_char 等)
# 严格匹配完整函数名 + 左括号, 避免误报相似前缀
# B01-07 修复: 真实函数名是 egdf_register_block_dev (见 framework/egdf/mod.rs:353),
# 此前正则写的是 egdf_register_block (缺 _dev), 与真实函数名不匹配, 门禁恒 0 空转.
PATTERN = re.compile(r'\begdf_register_block_dev\s*\(')


def main() -> int:
    violations: list[tuple[Path, int, str]] = []
    scanned = 0

    for rs in BASE.rglob('*.rs'):
        scanned += 1
        try:
            text = rs.read_text(encoding='utf-8', errors='replace')
        except OSError as e:
            print(f'  ! {rs}: 读取失败 {e}', file=sys.stderr)
            continue

        # 跳过允许文件
        if rs in ALLOWED_FILES:
            continue

        for lineno, line in enumerate(text.splitlines(), start=1):
            if PATTERN.search(line):
                # 排除注释行 (// 开头) — 但不排除 /* ... */ 块注释, 因为可能误判
                stripped = line.lstrip()
                if stripped.startswith('//') or stripped.startswith('*'):
                    continue
                violations.append((rs, lineno, line.rstrip()))

    print('=' * 78)
    print('I-43 audit: 块设备抽象统一性 — 单一桥接入口')
    print('=' * 78)
    print(f'  扫描文件: {scanned} 个 .rs')
    print(f'  允许文件: {len(ALLOWED_FILES)} 个 (egdf/mod.rs, egdf/proto_block.rs)')
    print(f'  违规调用: {len(violations)} 处')
    print('-' * 78)

    if violations:
        for path, lineno, line in violations:
            rel = path
            print(f'  ✗ {rel}:{lineno}')
            print(f'    {line.strip()}')
        print('-' * 78)
        print('  说明: 块设备驱动应实现 `BlockDevice` trait 并调用')
        print("        `proto_block::register_block_device(dev)` 注册.")
        print("        禁止直接调用低层 `egdf_register_block`.")
        print('=' * 78)
        return 1

    print('  ✓ 单一桥接入口不变式: 全部驱动通过 proto_block::register_block_device')
    print('=' * 78)
    return 0


if __name__ == '__main__':
    sys.exit(main())
