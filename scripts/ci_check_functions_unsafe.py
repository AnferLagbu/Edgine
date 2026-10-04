#!/usr/bin/env python3
"""
检查 functions/ 层是否有任何 unsafe 代码 (排除注释行与 vendored 第三方代码).

用法: python3 scripts/ci_check_functions_unsafe.py
退出码: 0 = 通过 (无 unsafe), 1 = 有违规

修复 B01-14: 原脚本扫描 src/kernel/functions 全树, 把 vendored smoltcp 的
18 处 unsafe 块当作 functions 业务代码违规, 实测返 1. 本修复复制
audit_functions_boundary.py 的 VENDORED_EXCLUDE 列表, 与边界审计保持统一.
"""

import re
import sys
from pathlib import Path

# Vendored 3rd-party 库目录: 不属于项目自有代码, 审计豁免.
# 与 scripts/audit_functions_boundary.py VENDORED_EXCLUDE 同步.
VENDORED_EXCLUDE = [
    Path('src/kernel/functions/net/smoltcp'),  # 上游 smoltcp 0.13.1 (2026-06)
]


def is_vendored(filepath: Path) -> bool:
    """检查文件是否在 vendored 第三方目录中 (审计豁免)."""
    try:
        fpath = filepath.resolve()
        for excl in VENDORED_EXCLUDE:
            if str(fpath).startswith(str(excl.resolve())):
                return True
    except Exception:
        pass
    return False


def main():
    functions = Path('src/kernel/functions')
    if not functions.exists():
        print(f'ERROR: {functions} not found', file=sys.stderr)
        return 2

    issues = []
    rs_files = sorted(functions.rglob('*.rs'))
    skipped = 0

    for f in rs_files:
        # 跳过 vendored 第三方代码
        if is_vendored(f):
            skipped += 1
            continue

        try:
            content = f.read_text(encoding='utf-8', errors='replace')
        except Exception as e:
            print(f'WARN: {f}: {e}', file=sys.stderr)
            continue

        for ln, line in enumerate(content.splitlines(), 1):
            stripped = line.strip()
            # 跳过注释行
            if (stripped.startswith('//') or
                stripped.startswith('*') or
                stripped.startswith('/*')):
                continue
            # 跳过 #![deny(...)] 等 attribute
            if stripped.startswith('#![') or stripped.startswith('#['):
                continue

            if re.search(r'\bunsafe\s*\{', line):
                issues.append(f'{f}:{ln}: unsafe {{ block: {line.strip()[:80]}')
            elif re.search(r'\bunsafe\s+fn\b', line):
                issues.append(f'{f}:{ln}: unsafe fn: {line.strip()[:80]}')
            elif re.search(r'\bunsafe\s+impl\b', line):
                issues.append(f'{f}:{ln}: unsafe impl: {line.strip()[:80]}')
            elif re.search(r'\bunsafe\s+trait\b', line):
                issues.append(f'{f}:{ln}: unsafe trait: {line.strip()[:80]}')

    print(f'扫描文件数: {len(rs_files)} (排除 vendored: {skipped})')
    if issues:
        print(f'FAIL: functions/ 中发现 {len(issues)} 处 unsafe')
        for i in issues:
            print(f'  {i}')
        return 1

    print('PASS: functions/ 0 unsafe (排除 vendored smoltcp)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
