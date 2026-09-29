#!/usr/bin/env python3
"""
M6.2 死锁检测矩阵扫描脚本 — 锁顺序/中断上下文/不可重入函数分析 (v1)

实际检查项 (B01-06 如实降级后):
  (1) 扫描所有 spin::Mutex / spin::RwLock / spin::Once 使用点
  (2) 检测在中断上下文 (IDT handler、ISR、irq handler) 中使用的锁
  (3) 标记非中断安全的锁 (应为 IrqSpinLock 而非 spin::Mutex)
  (4) 扫描裸类型锁名 (如 `SpinMutex` / `MyMutex` 等, 通过 import 解析收集)
  (5) 检测 sleep 锁 (Mutex) 在原子上下文的使用

未实现项 (本期不在范围内, 留作未来增强):
  - AB-BA 死锁环检测: 需先建立锁顺序声明机制 (如 lockdep-style annotation)
  - 不可重入函数检测: 需 Rust 类型系统级分析

退出码: 0 = 无严重风险, 1 = 有高风险 (中断上下文非安全锁)
"""

import os
import re
import sys
import json
from collections import defaultdict
from pathlib import Path

# 扫描范围 (2026-09-13 用户裁决扩展): framework + services 双子树.
# 扩展背景: 第二十六批 nestfs/dedup.rs ABBA 死锁位于 services 子树, 原单根
# 扫描对 services 锁使用不可见 (fail-closed 原则: 不可检查 = 漏检).
BASES = [
    Path('src/kernel/framework'),
    Path('src/kernel/services'),
]

# 中断上下文的函数白名单 (这些函数中使用的 spin::Mutex 视为高风险)
INTERRUPT_CONTEXT_FUNCS = [
    'handle_irq',
    'handle_exception',
    'exception_handler',
    'irq_handler',
    'interrupt_handler',
    'do_softirq',
    'isr_',
    'fault_handler',
    'page_fault_handler',
    'timer_tick',
    'timer_handler',
    'clock_interrupt',
    'schedule_from_isr',
    'eoi',
]

# 严重级别
SEVERITY_CRITICAL = 'CRITICAL'   # 中断上下文使用非安全锁, 必然死锁
SEVERITY_HIGH = 'HIGH'           # 锁顺序可疑
SEVERITY_MEDIUM = 'MEDIUM'       # 跨线程 sleep 锁使用
SEVERITY_LOW = 'LOW'             # 良好实践警告
SEVERITY_INFO = 'INFO'           # 仅记录


def is_in_interrupt_context(filepath, lineno, content):
    """检查给定的行是否在中断上下文中.

    通过查找所在函数名, 与 INTERRUPT_CONTEXT_FUNCS 比对.
    """
    # 读取整个文件
    try:
        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
            lines = f.readlines()
    except Exception:
        return False, None

    # 向上查找最近的 fn 定义
    fn_pattern = re.compile(r'\b(?:pub\s+)?(?:unsafe\s+)?(?:extern\s+"C"\s+)?fn\s+(\w+)')
    for j in range(lineno - 1, -1, -1):
        m = fn_pattern.search(lines[j])
        if m:
            fn_name = m.group(1)
            for ctx in INTERRUPT_CONTEXT_FUNCS:
                if fn_name == ctx or fn_name.startswith(ctx):
                    return True, fn_name
            return False, fn_name
    return False, None


def is_in_impl_block(filepath, lineno, content):
    """检测是否在 impl 块内 (有 self 引用)."""
    try:
        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
            lines = f.readlines()
    except Exception:
        return False

    # 向上查找最近的 impl 或 fn
    impl_pattern = re.compile(r'\bimpl\b.*\bfor\b')
    fn_pattern = re.compile(r'\b(?:pub\s+)?(?:unsafe\s+)?(?:extern\s+"C"\s+)?fn\s+\w+')

    depth = 0
    for j in range(lineno - 1, -1, -1):
        line = lines[j]
        # 简化: 计算 { 和 } 平衡
        opens = line.count('{')
        closes = line.count('}')
        depth += closes - opens
        if depth < 0:
            # 已经退出当前作用域
            if impl_pattern.search(line):
                return True
            if fn_pattern.search(line):
                return False
    return False


def scan_file(filepath):
    """扫描单个文件, 返回问题列表."""
    issues = []

    try:
        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
            content = f.read()
            lines = content.splitlines()
    except Exception as e:
        return issues

    # 模式:
    # - spin::Mutex::new(...)
    # - spin::Mutex<T>
    # - spin::RwLock
    # - spin::Once
    # - .lock() (不区分, 因为 framework/sync 内部也有 .lock())
    # - .read() / .write()

    # 两阶段扫描:
    #   阶段 A: 收集所有 spin::Mutex/RwLock/Once 字段名 (作为已知非安全锁)
    #   阶段 B: 扫描 .lock()/.read()/.write() 调用, 检查字段类型

    # B01-06 返工: 正则覆盖带路径的 import/类型, 如 `spin::mutex::SpinMutex` /
    # `crate::sync::irq_spinlock::IrqSpinLock`. 路径段数不限, 末段为目标类型.
    # B01-06 返工再次改进: 不限制末段为目标类别名 (Mutex/RwLock/...),
    # 而是匹配 `spin` / `sync` 路径下的任何类型, 末段任意.
    # 后续阶段 A.5 收集到 bare_aliases 后, 阶段 B 用别名集合判定
    # 是否 unsafe (SpinMutex 等自定义名都视为 unsafe, IrqSpinLock 视为 safe).
    # 统一正则: 路径段 + 末段类型
    _spin_path_tail = (
        r'(?:::\s*\w+\s*)*::\s*(\w+)\b'
    )
    spin_field_pattern = re.compile(
        r'\b(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:\s*(?:crate::)?(?:spin|sync)'
        + _spin_path_tail,
    )

    spin_static_pattern = re.compile(
        r'\bstatic\s+(\w+)\s*:\s*(?:crate::)?(?:spin|sync)'
        + _spin_path_tail,
    )

    # 安全锁字段模式: 标记这些字段为 IRQ 安全 (即 .lock() 不会产生 CRITICAL 警告)
    # B01-06 返工: 同样支持带路径的形式
    # 2026-09-13 扩展: services::sync::irq_lock::IrqSpinLock 为 framework
    # IrqSpinLock 的类型别名 (services 层 re-export), 全路径声明同样视为安全.
    safe_lock_field_pattern = re.compile(
        r'\b(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:\s*'
        r'(?:crate::framework::sync::irq_spinlock::|framework::sync::irq_spinlock::|'
        r'crate::services::sync::irq_lock::|services::sync::irq_lock::)?'
        r'IrqSpinLock|FrameworkIrqSpinLock|'
        r'(?:crate::)?sync(?:::\s*\w+\s*)*::\s*'
        r'IrqSpinLock\b',
    )
    safe_lock_static_pattern = re.compile(
        r'\bstatic\s+(\w+)\s*:\s*'
        r'(?:crate::framework::sync::irq_spinlock::|framework::sync::irq_spinlock::|'
        r'crate::services::sync::irq_lock::|services::sync::irq_lock::)?'
        r'IrqSpinLock|FrameworkIrqSpinLock|'
        r'(?:crate::)?sync(?:::\s*\w+\s*)*::\s*'
        r'IrqSpinLock\b',
    )

    unsafe_lock_fields = set()  # 已知非安全锁字段名
    unsafe_lock_statics = set()  # 已知非安全锁 static 名
    safe_lock_fields = set()  # 已知安全锁字段名
    safe_lock_statics = set()  # 已知安全锁 static 名

    # 阶段 A.0: B01-06 返工 - 先扫描整个文件收集所有 spin::xxx::Type
    # 类型名 (字段类型, 不限末段), 存入 _all_spin_types 集合.
    # 用于阶段 A 字段收集时判定 unsafe vs safe (查表 bare_aliases).
    _all_spin_types: set[str] = set()
    for lineno_1, line in enumerate(lines, start=1):
        if line.strip().startswith('//'):
            continue
        # spin_field_pattern match 0:type 1:field 2:tail
        for m in spin_field_pattern.finditer(line):
            if m.lastindex and m.lastindex >= 2:
                _all_spin_types.add(m.group(2))
        for m in spin_static_pattern.finditer(line):
            if m.lastindex and m.lastindex >= 2:
                _all_spin_types.add(m.group(2))


    # 阶段 A.5: B01-06 返工 - 收集裸类型别名 (覆盖带路径 import)
    # 通过 `use spin::mutex::SpinMutex` / `use spin::Mutex as MyMutex` /
    # `use spin::{Mutex, RwLock}` / `pub type SpinMutex = spin::Mutex<T>;` 这类
    # 引入的本地名. 这样 `static X: SpinMutex = ...` 这种裸类型名也能被检测到.
    # B01-06 返工关键改进: 正则覆盖带路径的形式, 如 `spin::mutex::SpinMutex`
    # (捕获末段为任意类型名, 包括 SpinMutex/RwLock/Once/IrqSpinLock 等).
    # 类型名 -> unsafe/safe 分类: 末段为 IrqSpinLock → safe, 其余 → unsafe.
    # bare_aliases 在阶段 A 之前定义, 阶段 A 字段收集时按类型名查表.
    bare_aliases: dict[str, str] = {}  # 类型名 -> "unsafe" / "safe"
    pub_type_alias = re.compile(
        r'^\s*(?:pub\s+)?type\s+(\w+)\s*(?:<[^>]*>)?\s*=\s*'
        r'(?:crate::)?(?:spin|sync)'
        r'(?:::\s*\w+\s*)*::\s*(\w+)\b',
    )
    for lineno_1, line in enumerate(lines, start=1):
        s = line.strip()
        if s.startswith('//') or s.startswith('///') or s.startswith('/*'):
            continue
        # use spin::mutex::SpinMutex as X; 或 use spin::{mutex::SpinMutex as X, ...};
        # B01-06 返工: 正则匹配 `use ...::...::* as X` (带路径的 use)
        # 统一正则: 路径段 + 末段类型 + 可选别名
        m_use = re.search(
            r'use\s+(?:crate::)?(?:spin|sync)'
            r'((?:::\s*\w+\s*)*)'
            r'::\s*\{([^}]+)\}',
            line,
        )
        if m_use:
            for item in m_use.group(1).split(','):
                item = item.strip()
                # 形式: `Mutex` / `Mutex as MyMutex` / `mutex::SpinMutex as X`
                if ' as ' in item:
                    orig, alias = item.split(' as ')
                    orig = orig.strip()
                    alias = alias.strip()
                    # 提取末段 (SpinMutex → Mutex 等目标类型)
                    orig_target = orig.split('::')[-1]
                    if orig_target in ('Mutex', 'RwLock', 'Once', 'OnceCell'):
                        bare_aliases[alias] = 'unsafe'
                    elif orig_target == 'IrqSpinLock':
                        bare_aliases[alias] = 'safe'
                else:
                    # 提取末段
                    orig_target = item.split('::')[-1]
                    if orig_target in ('Mutex', 'RwLock', 'Once', 'OnceCell'):
                        bare_aliases[item] = 'unsafe'
                    elif orig_target == 'IrqSpinLock':
                        bare_aliases[item] = 'safe'
            continue
        # use spin::mutex::SpinMutex as X; (带路径形式)
        # B01-06 返工: 正则支持任意末段名 + 任意路径段 + 可选别名
        m_use_simple = re.search(
            r'use\s+(?:crate::)?(?:spin|sync)'
            r'((?:::\s*\w+\s*)*)'
            r'::\s*(\w+)\s*(?:as\s+(\w+))?\s*;',
            line,
        )
        if m_use_simple:
            orig = m_use_simple.group(1)
            alias = m_use_simple.group(2) or orig
            # B01-06 返工: 不限制末段为目标类别名 (原代码只接受
            # Mutex/RwLock/Once/OnceCell, 但 SpinMutex 这种重命名形式漏).
            # 改为: 任何 spin 路径下的类型都视为 unsafe (默认).
            # 后续可在已知 IRQ safe 列表中显式豁免 (例如 IrqSpinLock).
            if orig == 'IrqSpinLock':
                bare_aliases[alias] = 'safe'
            else:
                # 末段以 Mutex/RwLock/Once/OnceCell 结尾, 或自定义名 (SpinMutex 等)
                # 一律视为 unsafe. 详细分类暂不强制.
                bare_aliases[alias] = 'unsafe'
            continue
        # 2026-09-13 扩展: services::sync 是 framework::sync 的 re-export 层
        # (DECISION-K 边界), 形如
        # `use crate::services::sync::irq_lock::IrqSpinLock as Mutex;`
        # 的导入按末段类型分类: IrqSpinLock → safe, 其余 → unsafe.
        m_use_services = re.search(
            r'use\s+(?:crate::)?services::sync'
            r'(?:::\s*\w+\s*)*'
            r'::\s*(\w+)\s*(?:\s+as\s+(\w+))?\s*;',
            line,
        )
        if m_use_services:
            orig = m_use_services.group(1)
            alias = m_use_services.group(2) or orig
            bare_aliases[alias] = 'safe' if orig == 'IrqSpinLock' else 'unsafe'
            continue
        # pub type SpinMutex = spin::mutex::SpinMutex<T>;
        m_type = pub_type_alias.search(line)
        if m_type:
            name = m_type.group(1)
            target = m_type.group(2)
            if target in ('Mutex', 'RwLock', 'Once', 'OnceCell'):
                bare_aliases[name] = 'unsafe'
            elif target == 'IrqSpinLock':
                bare_aliases[name] = 'safe'

    # 把别名也加入已知锁集合 (B01-06 返工: 阶段 B 现在引用)
    unsafe_lock_aliases = {name for name, kind in bare_aliases.items() if kind == 'unsafe'}
    safe_lock_aliases = {name for name, kind in bare_aliases.items() if kind == 'safe'}
    # 阶段 A: 收集字段类型
    # B01-06 返工: 按类型名查表决定 unsafe/safe (用阶段 A.5 的 bare_aliases).
    for lineno_1, line in enumerate(lines, start=1):
        if line.strip().startswith('//'):
            continue
        for m in spin_field_pattern.finditer(line):
            field_name = m.group(1)
            type_tail = m.group(2) if m.lastindex and m.lastindex >= 2 else None
            # 优先查 bare_aliases (use/type 别名), 否则按末段默认 unsafe
            if type_tail in bare_aliases:
                kind = bare_aliases[type_tail]
            else:
                kind = 'safe' if type_tail == 'IrqSpinLock' else 'unsafe'
            if kind == 'safe':
                safe_lock_fields.add(field_name)
            else:
                unsafe_lock_fields.add(field_name)
        for m in spin_static_pattern.finditer(line):
            field_name = m.group(1)
            type_tail = m.group(2) if m.lastindex and m.lastindex >= 2 else None
            if type_tail in bare_aliases:
                kind = bare_aliases[type_tail]
            else:
                kind = 'safe' if type_tail == 'IrqSpinLock' else 'unsafe'
            if kind == 'safe':
                safe_lock_statics.add(field_name)
            else:
                unsafe_lock_statics.add(field_name)
        for m in safe_lock_field_pattern.finditer(line):
            safe_lock_fields.add(m.group(1))
        for m in safe_lock_static_pattern.finditer(line):
            safe_lock_statics.add(m.group(1))
    # 阶段 A.0b: B01-06 返工 - 检测裸类型字段 (如 `static X: SpinMutex`)
    # 当 use 已引入别名后, 字段类型不再带 spin:: 前缀, 但类型名是
    # 已知 spin 派生类 (在 bare_aliases 中). 这种字段也需加入 unsafe 集合.
    bare_field_pattern = re.compile(
        r'\b(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:\s*'
        r'(\w+)\b(?!\s*::)',
    )
    bare_static_pattern = re.compile(
        r'\bstatic\s+(\w+)\s*:\s*(\w+)\b(?!\s*::)',
    )
    for lineno_1, line in enumerate(lines, start=1):
        if line.strip().startswith('//'):
            continue
        # 仅当类型名在 bare_aliases 中时, 视为 spin 引入的别名
        # 排除普通类型 (如 u8, u32, bool 等) 通过 bare_aliases 查表
        for m in bare_field_pattern.finditer(line):
            type_name = m.group(2)
            if type_name in bare_aliases and bare_aliases[type_name] == 'unsafe':
                unsafe_lock_fields.add(m.group(1))
        for m in bare_static_pattern.finditer(line):
            type_name = m.group(2)
            if type_name in bare_aliases and bare_aliases[type_name] == 'unsafe':
                unsafe_lock_statics.add(m.group(1))

    # 阶段 B: 检测 .lock()/.read()/.write() 调用
    lock_call_pattern = re.compile(
        r'(?:(?P<field>\w+)\.)?(?P<method>lock|read|write)\s*\('
    )

    framework_lock_methods = {'with', 'with_mut', 'lock_irqsave', 'try_lock'}

    for lineno_1, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        # 跳过注释行
        stripped = line.strip()
        if stripped.startswith('//') or stripped.startswith('*') or stripped.startswith('///'):
            continue

        # 检测 .lock()/.read()/.write() 调用
        for m in lock_call_pattern.finditer(line):
            field_name = m.group('field')
            method = m.group('method')

            # 跳过非锁方法
            if method in framework_lock_methods:
                continue

            # 跳过 framework::sync 类型的锁 (它们的 method 不会通过 .lock() 暴露)
            if 'framework::sync' in line or 'sync::irq_spinlock' in line:
                continue

            # 跳过非字段调用 (例如: spin::Mutex::new, .call_once, 等)
            if not field_name:
                continue
            if field_name in ('spin', 'core', 'std', 'crate', 'self', 'super'):
                continue
            if method not in ('lock', 'read', 'write'):
                continue

            # 交叉检查字段类型
            if (field_name in safe_lock_fields or field_name in safe_lock_statics
                    or field_name in safe_lock_aliases):
                # 安全锁 (含裸类型别名), 跳过
                continue
            # 如果字段已知为非安全锁 OR 字段未知 (在中断上下文中, 默认 CRITICAL)
            # B01-06 返工: 裸类型别名也加入已知非安全锁集合
            is_known_unsafe = (field_name in unsafe_lock_fields
                               or field_name in unsafe_lock_statics
                               or field_name in unsafe_lock_aliases)

            # 检查是否在中断上下文
            in_irq, fn_name = is_in_interrupt_context(str(filepath), lineno_1, line)
            if in_irq:
                issues.append({
                    'file': str(filepath),
                    'line': lineno_1,
                    'severity': SEVERITY_CRITICAL,
                    'type': 'IRQ_CONTEXT_LOCK_ACQUISITION',
                    'lock': f'{field_name}.{method}()',
                    'function': fn_name,
                    'message': (f'中断上下文函数 `{fn_name}` 在 L{lineno_1} 获取锁 {field_name}.{method}() — '
                                f'{"已知非安全锁 (spin::Mutex), 须替换为 IrqSpinLock" if is_known_unsafe else "字段类型未识别, 须人工确认是否为 IrqSpinLock"}'),
                    'code': line.strip()[:200],
                })
            elif is_known_unsafe:
                # 字段已知为 spin::Mutex 等, 记录为 HIGH 风险
                # (因为该函数可能被中断上下文调用, 须人工审查调用栈)
                issues.append({
                    'file': str(filepath),
                    'line': lineno_1,
                    'severity': SEVERITY_HIGH,
                    'type': 'NON_IRQ_SAFE_LOCK_USE',
                    'lock': f'{field_name}.{method}() (字段类型: spin::Mutex/RwLock/Once)',
                    'function': fn_name,
                    'message': (f'函数 `{fn_name}` 在 L{lineno_1} 获取非 IRQ 安全锁 {field_name}.{method}() — '
                                f'如本函数或其调用者会在中断上下文中执行, 必须改为 IrqSpinLock'),
                    'code': line.strip()[:200],
                })

    # 阶段 C: 直接检测 spin::Mutex/RwLock/Once 声明 (作为非安全锁使用记录)
    spin_mutex_decl = re.compile(r'\bspin::Mutex\b')
    spin_rwlock_decl = re.compile(r'\bspin::RwLock\b')
    spin_once_decl = re.compile(r'\bspin::Once\b')
    spin_once_cell_decl = re.compile(r'\bspin::OnceCell\b')

    # framework 的安全锁 (不应被报告)
    framework_lock_patterns = [
        re.compile(r'\bframework::sync::(irq_spinlock|spinlock|mutex|rwlock|seqlock|once_lock|once_cell)\b'),
        re.compile(r'\bcrate::framework::sync::(irq_spinlock|spinlock|mutex|rwlock|seqlock|once_lock|once_cell)\b'),
        re.compile(r'\bsync::(irq_spinlock|spinlock|mutex|rwlock|seqlock|once_lock|once_cell)\b'),
    ]

    for lineno_1, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        # 跳过注释行
        stripped = line.strip()
        if stripped.startswith('//') or stripped.startswith('*') or stripped.startswith('///'):
            continue

        # 检测第三方 spin 使用
        spin_uses = []
        if spin_mutex_decl.search(line):
            spin_uses.append('spin::Mutex')
        if spin_rwlock_decl.search(line):
            spin_uses.append('spin::RwLock')
        if spin_once_decl.search(line):
            spin_uses.append('spin::Once')
        if spin_once_cell_decl.search(line):
            spin_uses.append('spin::OnceCell')

        for spin_type in spin_uses:
            # 排除 import 语句
            if stripped.startswith('use ') and spin_type in stripped:
                continue
            # 排除注释中的引用
            if '//' in line:
                code_part = line.split('//', 1)[0]
                if spin_type not in code_part:
                    continue

            # 简化: 只要不包含 'framework::sync' 就算第三方使用
            is_framework_use = any(p.search(line) for p in framework_lock_patterns)
            if is_framework_use:
                continue

            # 检查是否在中断上下文
            in_irq, fn_name = is_in_interrupt_context(str(filepath), lineno_1, line)
            if in_irq:
                issues.append({
                    'file': str(filepath),
                    'line': lineno_1,
                    'severity': SEVERITY_CRITICAL,
                    'type': 'IRQ_CONTEXT_UNSAFE_LOCK_DECL',
                    'lock': spin_type,
                    'function': fn_name,
                    'message': f'中断上下文相关函数 `{fn_name}` 引用第三方 {spin_type} 声明, 应替换为 framework::sync::irq_spinlock::IrqSpinLock',
                    'code': line.strip()[:200],
                })
            else:
                # 记录普通使用, 供后续统一迁移
                issues.append({
                    'file': str(filepath),
                    'line': lineno_1,
                    'severity': SEVERITY_INFO,
                    'type': 'THIRD_PARTY_LOCK_USE',
                    'lock': spin_type,
                    'function': fn_name,
                    'message': f'使用第三方 {spin_type} (非中断上下文, 但仍应迁移到 framework::sync)',
                    'code': line.strip()[:200],
                })

    return issues


def scan_directory(base):
    """递归扫描目录中的所有 .rs 文件."""
    all_issues = []
    files = list(base.rglob('*.rs'))
    for f in files:
        # 排除 smoltcp 第三方目录
        if 'smoltcp' in str(f):
            continue
        issues = scan_file(f)
        all_issues.extend(issues)
    return all_issues, len(files)


def generate_report(issues, file_count):
    """生成报告."""
    by_severity = defaultdict(list)
    for issue in issues:
        by_severity[issue['severity']].append(issue)

    report = []
    report.append('=' * 78)
    report.append('M6.2 死锁检测矩阵扫描报告')
    report.append('=' * 78)
    report.append('')
    report.append(f'扫描文件数: {file_count}')
    report.append(f'问题总数: {len(issues)}')
    report.append('')

    for sev in [SEVERITY_CRITICAL, SEVERITY_HIGH, SEVERITY_MEDIUM, SEVERITY_LOW, SEVERITY_INFO]:
        count = len(by_severity[sev])
        if count == 0:
            continue
        report.append(f'[{sev}] {count} 项')
        report.append('-' * 78)

        # 按文件分组
        by_file = defaultdict(list)
        for issue in by_severity[sev]:
            by_file[issue['file']].append(issue)

        for filepath, file_issues in sorted(by_file.items()):
            report.append(f'\n  {filepath}:')
            for issue in sorted(file_issues, key=lambda x: x['line']):
                report.append(f'    L{issue["line"]}: {issue["type"]}')
                report.append(f'      锁: {issue["lock"]}')
                if issue.get('function'):
                    report.append(f'      函数: {issue["function"]}')
                report.append(f'      描述: {issue["message"]}')
                report.append(f'      代码: {issue["code"]}')
        report.append('')

    return '\n'.join(report)


def main():
    for base in BASES:
        if not base.exists():
            print(f'ERROR: {base} not found', file=sys.stderr)
            sys.exit(2)

    issues = []
    file_count = 0
    for base in BASES:
        print(f'扫描 {base} ...')
        root_issues, root_files = scan_directory(base)
        issues.extend(root_issues)
        file_count += root_files
    report = generate_report(issues, file_count)
    print(report)

    # 保存 JSON 报告 (gitignored target/audit/)
    json_path = Path('target/audit/deadlock-matrix.json')
    json_path.parent.mkdir(parents=True, exist_ok=True)
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump({
            'file_count': file_count,
            'issue_count': len(issues),
            'issues': issues,
        }, f, ensure_ascii=False, indent=2)
    print(f'\nJSON 报告保存至: {json_path}')

    # 计算退出码
    critical = sum(1 for i in issues if i['severity'] == SEVERITY_CRITICAL)
    if critical > 0:
        print(f'\n>>> {critical} 个 CRITICAL 问题 (中断上下文非安全锁) <<<')
        sys.exit(1)
    sys.exit(0)


if __name__ == '__main__':
    main()
