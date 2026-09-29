#!/usr/bin/env python3
"""
M6.5 services 层隐式依赖审计脚本

检查 services 层模块对 framework 全局状态的引用,
检测通过全局静态变量产生的隐式依赖。

退出码: 0 = 通过, 1 = 有严重违规
"""

import os
import re
import sys
from pathlib import Path

SERVICES_BASE = Path('src/kernel/services')
FRAMEWORK_BASE = Path('src/kernel/framework')

# framework 全局状态变量 (services 不应直接访问)
# 仅包含实际定义在 framework 中的变量
FRAMEWORK_GLOBALS = [
    'SCHEDULER',
    'PROCESS_TABLE',
    'SOCKET_SET',
    'NET_DEVICE',
    'NET_STACK',
    'GLOBAL_FRAMEBUFFER',
    'VGA_DRIVER',
    'ISR_TABLE',
    'LOG_SINKS',
    'SLAB_CACHES',
    'GENERAL_CACHES',
    'GLOBAL_DMA',
    'GLOBAL_KMALLOC',
]

# 允许的直接引用 (framework 层安全 API)
ALLOWED_PATTERNS = [
    r'crate::services::fs::VFS_MANAGER',
    r'crate::framework::ipc::IPC_NAMESPACE',
]

def find_framework_globals():
    """查找 framework 中实际定义的全局变量.

    B01-23 修复: FRAMEWORK_GLOBALS 改为动态发现 (扫描 framework/ 全部
    `static [mut] NAME:` 与 `static NAME: TYPE` 声明). 原硬编码 14
    项名字静态列表, 改名/新增后静默不再检测.
    """
    # 关键字过滤 (避免 `Self` / `usize` / `u32` 等被误作为全局变量名)
    RUST_KEYWORDS = {
        'Self', 'self', 'static', 'const', 'let', 'mut', 'ref', 'pub',
        'use', 'fn', 'struct', 'enum', 'trait', 'impl', 'mod', 'crate',
        'super', 'as', 'in', 'if', 'else', 'for', 'while', 'loop', 'match',
        'return', 'break', 'continue', 'true', 'false', 'usize', 'u8', 'u16',
        'u32', 'u64', 'u128', 'isize', 'i8', 'i16', 'i32', 'i64', 'i128',
        'f32', 'f64', 'bool', 'char', 'str', 'String', 'Vec', 'Option',
        'Result', 'Box', 'Rc', 'Arc', 'Cell', 'RefCell',
    }

    globals_found = set()
    # 静态模式: `static [mut] NAME: TYPE` 或 `static NAME: TYPE`
    # NAME 必须是合法 Rust 标识符 (且非关键字)
    static_pattern = re.compile(
        r'\bstatic\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*'
        r'(?::\s*[A-Za-z][\w:<> ,]*)?\s*[=;{]'
    )
    for rust_file in FRAMEWORK_BASE.rglob('*.rs'):
        if 'smoltcp' in str(rust_file):
            continue
        try:
            with open(rust_file, 'r', encoding='utf-8', errors='ignore') as f:
                for line in f:
                    m = static_pattern.search(line)
                    if m:
                        name = m.group(1)
                        if name in RUST_KEYWORDS:
                            continue
                        globals_found.add(name)
        except (OSError, UnicodeDecodeError):
            continue
    # 与原静态列表合并 (兜底, 防止动态发现漏检)
    for name in FRAMEWORK_GLOBALS:
        globals_found.add(name)
    return globals_found


def scan_services(framework_globals):
    """扫描 services 层对 framework 全局状态的引用.

    B01-23 修复: `global_name in line` 子串匹配误判.
    - `SCHEDULER` 误匹配 `SCHEDULER_READY` 等包含子串
    - 改用 `\\b{name}\\b` 词边界, 排除子串误命中
    """
    violations = []

    for rust_file in SERVICES_BASE.rglob('*.rs'):
        if 'smoltcp' in str(rust_file):
            continue

        try:
            with open(rust_file, 'r', encoding='utf-8', errors='ignore') as f:
                lines = f.read().split('\n')
        except (OSError, UnicodeDecodeError):
            continue

        for line_num, line in enumerate(lines, 1):
            stripped = line.strip()
            if stripped.startswith('//') or stripped.startswith('/*') or stripped.startswith('*'):
                continue

            for global_name in framework_globals:
                # B01-23: 词边界匹配, 避免 SCHEDULER 误匹配 SCHEDULER_READY
                if re.search(r'\b' + re.escape(global_name) + r'\b', line):
                    is_allowed = any(re.search(pattern, line) for pattern in ALLOWED_PATTERNS)
                    if not is_allowed:
                        violations.append({
                            'file': str(rust_file),
                            'line': line_num,
                            'global': global_name,
                            'code': line.strip(),
                        })

    return violations

def main():
    print("M6.5 services 层隐式依赖审计")
    print("=" * 60)
    
    # 先查找 framework 中实际定义的全局变量
    framework_globals = find_framework_globals()
    print(f"\n检测到 framework 全局变量: {len(framework_globals)} 个")
    for g in sorted(framework_globals):
        print(f"  - {g}")
    
    violations = scan_services(framework_globals)
    
    if violations:
        print(f"\n发现 {len(violations)} 处隐式依赖:")
        print("-" * 60)
        
        for v in violations:
            rel_path = os.path.relpath(v['file'], Path.cwd())
            print(f"  {rel_path}:{v['line']}: {v['global']}")
            print(f"    {v['code'][:80]}")
            print()
        
        print("-" * 60)
        print(f"FAIL: {len(violations)} 处 services 层直接访问 framework 全局状态")
        sys.exit(1)
    else:
        print("\nPASS: services 层无隐式依赖")
        sys.exit(0)

if __name__ == '__main__':
    main()
