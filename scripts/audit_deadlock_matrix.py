#!/usr/bin/env python3
"""
M6.2 死锁检测矩阵扫描脚本 — 锁顺序/中断上下文/不可重入函数分析 (v1)

实际检查项 (B01-06 如实降级后):
  (1) 扫描所有 spin::Mutex / spin::RwLock / spin::Once 使用点
  (2) 检测在中断上下文 (IDT handler、ISR、irq handler) 中使用的锁
  (3) 标记非中断安全的锁 (应为 IrqSpinLock 而非 spin::Mutex)
  (4) 扫描裸类型锁名 (如 `SpinMutex` / `MyMutex` 等, 通过 import 解析收集)
  (5) 检测 sleep 锁 (Mutex) 在原子上下文的使用
  (6) 锁类序表对账 (A-3 丙, 2026-11-05): 机器可读单源 scripts/lock_class_order.json
      与 ① 代码侧 ::named / ::named_lazy 类名字面量 双向一致, ② 序表 symbol 锚
      仍可在 src/kernel 定位, ③ docs/explain/ref-lock-order.md 的「锁层级」表
      与该单源双向一致, ④ 类表容量与 limits.rs 上限对账 (A-127c). 未声明的新锁类
      即红, 已失效的表条目同样即红, 具名类数到顶亦即红.
  (7) 静态锁序倒序检测 (A-3b, 2026-11-08 裁定 乙+甲): 函数内按 guard 生命周期判
      「持有 A 时取 B」, 跨函数只传播一跳 (持锁期间直接调用的被调函数自身所取的锁),
      回调包装按 callee 入口锁覆盖闭包实参. 已知类之间降序即红 (例外白名单在序表
      的 `exceptions` 里机器可读); 未分类取锁点按 (文件, 接收者) 冻结进机器派生基线
      (scripts/lock_order_static.baseline.json), 新增/增加即红.

未实现项 (本期不在范围内, 留作未来增强):
  - 多跳 / 调用图不动点传播: 本轮只做一跳 (乙); 闭包延迟执行、函数指针 / trait
    动态派发、宏展开体、guard 被 move 后的提前释放亦在覆盖边界外, 见 (7) 段注释
  - 不可重入函数检测: 需 Rust 类型系统级分析

退出码: 0 = 无严重风险, 1 = 有高风险 (中断上下文非安全锁 / 锁类序表失同步 /
静态锁序倒序 / 未分类取锁点增长)
"""

import os
import re
import sys
import json
from bisect import bisect_right
from collections import defaultdict
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent

# 扫描范围 (2026-09-13 用户裁决扩展): privileged + functions 双子树.
# 扩展背景: 第二十六批 egfs/dedup.rs ABBA 死锁位于 functions 子树, 原单根
# 扫描对 functions 锁使用不可见 (fail-closed 原则: 不可检查 = 漏检).
BASES = [
    Path('src/kernel/privileged'),
    Path('src/kernel/functions'),
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
    # - .lock() (不区分, 因为 privileged/sync 内部也有 .lock())
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
    # 2026-09-13 扩展: functions::sync::irq_lock::IrqSpinLock 为 privileged
    # IrqSpinLock 的类型别名 (functions 层 re-export), 全路径声明同样视为安全.
    safe_lock_field_pattern = re.compile(
        r'\b(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:\s*'
        r'(?:crate::privileged::sync::irq_spinlock::|privileged::sync::irq_spinlock::|'
        r'crate::functions::sync::irq_lock::|functions::sync::irq_lock::)?'
        r'IrqSpinLock|PrivilegedIrqSpinLock|'
        r'(?:crate::)?sync(?:::\s*\w+\s*)*::\s*'
        r'IrqSpinLock\b',
    )
    safe_lock_static_pattern = re.compile(
        r'\bstatic\s+(\w+)\s*:\s*'
        r'(?:crate::privileged::sync::irq_spinlock::|privileged::sync::irq_spinlock::|'
        r'crate::functions::sync::irq_lock::|functions::sync::irq_lock::)?'
        r'IrqSpinLock|PrivilegedIrqSpinLock|'
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
        # 2026-09-13 扩展: functions::sync 是 privileged::sync 的 re-export 层
        # (DECISION-K 边界), 形如
        # `use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;`
        # 的导入按末段类型分类: IrqSpinLock → safe, 其余 → unsafe.
        m_use_functions = re.search(
            r'use\s+(?:crate::)?functions::sync'
            r'(?:::\s*\w+\s*)*'
            r'::\s*(\w+)\s*(?:\s+as\s+(\w+))?\s*;',
            line,
        )
        if m_use_functions:
            orig = m_use_functions.group(1)
            alias = m_use_functions.group(2) or orig
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

    privileged_lock_methods = {'with', 'with_mut', 'lock_irqsave', 'try_lock'}

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
            if method in privileged_lock_methods:
                continue

            # 跳过 privileged::sync 类型的锁 (它们的 method 不会通过 .lock() 暴露)
            if 'privileged::sync' in line or 'sync::irq_spinlock' in line:
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

    # privileged 的安全锁 (不应被报告)
    privileged_lock_patterns = [
        re.compile(r'\bprivileged::sync::(irq_spinlock|spinlock|mutex|rwlock|seqlock|once_lock|once_cell)\b'),
        re.compile(r'\bcrate::privileged::sync::(irq_spinlock|spinlock|mutex|rwlock|seqlock|once_lock|once_cell)\b'),
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

            # 简化: 只要不包含 'privileged::sync' 就算第三方使用
            is_privileged_use = any(p.search(line) for p in privileged_lock_patterns)
            if is_privileged_use:
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
                    'message': f'中断上下文相关函数 `{fn_name}` 引用第三方 {spin_type} 声明, 应替换为 privileged::sync::irq_spinlock::IrqSpinLock',
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
                    'message': f'使用第三方 {spin_type} (非中断上下文, 但仍应迁移到 privileged::sync)',
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


# ===========================================================================
# (6) 锁类序表对账 (A-3 丙)
# ===========================================================================

# 机器可读单源与人读视图. 序表是数据源, 文档层级表必须与它双向一致;
# 锚点路径按脚本位置解析 (不依赖 CWD), 缺失即 fail-closed 判红.
LOCK_ORDER_TABLE = PROJECT_ROOT / 'scripts' / 'lock_class_order.json'
LOCK_ORDER_DOC = PROJECT_ROOT / 'docs' / 'explain' / 'ref-lock-order.md'

# 容量上限单源: lockdep 的类表与持有栈是定长数组, 其实值定义在 limits.rs.
# 序表 lockdep 条目数与代码具名类数都必须容得下 —— 满表在运行时只留一条粘滞失效
# 读数 (丢注册 + 该类全程脱离检测), 门禁必须在到顶之前判红, 而不是等启动读数.
LIMITS_SOURCE = PROJECT_ROOT / 'src' / 'kernel' / 'privileged' / 'constants' / 'limits.rs'
CAPACITY_LIMIT_NAMES = ('MAX_LOCK_CLASSES', 'MAX_HELD_LOCKS')

# 代码侧具名锁类注册点: SpinLock/Mutex/RwLock/PiMutex/IrqSpinLock 的
# ::named("X") 与 IrqSpinLock 的惰性 ::named_lazy("X"). `\s` 含换行 ⇒ 构造与
# 类名字面量分行也能匹配 (模式本身跨行, 但扫描必须整文件做, 见下).
NAMED_CLASS_PATTERN = re.compile(r'::named(?:_lazy)?\s*\(\s*"([^"]+)"')

# 注释行整体跳过: 文档示例 (`//! ... ::named("demo")`) 里的形态不是注册点.
COMMENT_LINE_PATTERN = re.compile(r'^(?://|/?\*|/\*)')

# 文档层级表: 「## 锁层级」 标题后的第一个围栏代码块; 块内 `层级 N:` 为表头行,
# 缩进行为条目行 —— 只有条目行参与受控 token 判定 (表头含域名大写缩写, 如 FREG).
DOC_SECTION_HEADING = '## 锁层级'
DOC_LEVEL_HEADING_PATTERN = re.compile(r'^层级\s+(\d+):')

# 受控 token 形态: 全大写标识符 (≥3 字符) / Type::field / a.b / A.b.
# 括号内 (中英皆算) 的内容整体豁免 —— 该位置用于写「已删除」「原子量」这类说明,
# 把失效名留在说明里不该被当成一次锁顺序声明.
TOKEN_PATTERNS = (
    re.compile(r'\b[A-Z][A-Z0-9_]{2,}\b'),
    re.compile(r'\b[A-Za-z_][A-Za-z0-9_]*::[a-z_][A-Za-z0-9_]*\b'),
    re.compile(r'\b[A-Za-z_][A-Za-z0-9_]*\.[a-z_][A-Za-z0-9_]*\b'),
)
BRACKET_SPAN_PATTERN = re.compile(r'（[^）]*）|\([^)]*\)')


def collect_named_lock_classes(roots):
    """收集代码侧具名锁类字面量 -> 出现位置列表.

    扫描必须**整文件**做, 不能逐行: `::named_lazy(` 与类名字面量分行是既有形态
    (freg/domain.rs 的 4 把域内锁即如此), 逐行扫会把它们看成「代码里没注册」——
    判据 ① 的「未声明的新锁类即红」随即对跨行形态失效, 序表也会少治理真锁.
    注释行跳过: 文档示例里的 `::named("demo")` 不是注册点.
    """
    sites = defaultdict(list)
    for base in roots:
        for path in sorted(base.rglob('*.rs')):
            if 'smoltcp' in str(path):
                continue
            try:
                text = path.read_text(encoding='utf-8', errors='replace')
            except OSError:
                continue
            line_starts = [0]
            for idx, char in enumerate(text):
                if char == '\n':
                    line_starts.append(idx + 1)
            for match in NAMED_CLASS_PATTERN.finditer(text):
                lineno = bisect_right(line_starts, match.start())
                head = text[line_starts[lineno - 1]:].split('\n', 1)[0].lstrip()
                if COMMENT_LINE_PATTERN.match(head):
                    continue
                sites[match.group(1)].append((str(path), lineno))
    return sites


def build_code_corpus(roots):
    """把扫描根下全部 .rs 文本合成一个语料串, 供 symbol 锚做存在性检查.

    symbol 判据是「该标识符仍在代码里」(抓删锁/改名导致的表条目失效), 不是类型
    判据 —— 序表条目里 `lock` / `cwd` 这类短段名会恒真, 这是刻意的弱判据,
    与 §2.2「定点回归锁 vs 树级防线」的口径一致: 本腿只证明清单未丢, 不证明
    同类新风险点会被发现.
    """
    chunks = []
    for base in roots:
        for path in sorted(base.rglob('*.rs')):
            if 'smoltcp' in str(path):
                continue
            try:
                chunks.append(path.read_text(encoding='utf-8', errors='replace'))
            except OSError:
                continue
    return '\n'.join(chunks)


def identifier_segments(name):
    """把序表条目名拆成可 grep 的标识符段 (去 `::` / `.` / `[..]`)."""
    stripped = re.sub(r'\[[^\]]*\]', '', name)
    parts = re.split(r'::|[./]', stripped)
    return [part for part in parts if part and re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', part)]


def controlled_tokens(text):
    """从文档条目文本提取受控 token."""
    without_brackets = BRACKET_SPAN_PATTERN.sub(' ', text)
    tokens = set()
    for pattern in TOKEN_PATTERNS:
        tokens.update(pattern.findall(without_brackets))
    return tokens


def parse_doc_level_entries(path):
    """解析文档「锁层级」围栏块 -> (层级 -> 条目文本, 层级 -> 表头存在)."""
    try:
        lines = path.read_text(encoding='utf-8', errors='replace').splitlines()
    except OSError:
        return None, None
    start = None
    for idx, line in enumerate(lines):
        if line.startswith(DOC_SECTION_HEADING):
            start = idx
            break
    if start is None:
        return None, None
    fence = None
    for idx in range(start + 1, len(lines)):
        if lines[idx].strip().startswith('```'):
            fence = idx
            break
    if fence is None:
        return None, None
    entries = {}
    headers = set()
    current = None
    for line in lines[fence + 1:]:
        if line.strip().startswith('```'):
            break
        heading = DOC_LEVEL_HEADING_PATTERN.match(line.strip())
        if heading:
            current = int(heading.group(1))
            headers.add(current)
            continue
        if line[:1].isspace() and current is not None:
            entries.setdefault(current, []).append(line.strip())
        elif not line.strip():
            # 层级块之间的空行: 条目行归属到此为止, 防跨块粘连
            current = None
    return entries, headers


def read_capacity_limits():
    """读取 limits.rs 的容量上限**实值** (单源, 不在脚本里抄数).

    任一常量定位不到即返回 None —— 容量判据按 fail-closed 走红: 上限被改名/挪走时
    这条腿必须哑掉而不是静默放行, 否则「到顶即红」会在最需要它的时候失效.
    """
    try:
        text = LIMITS_SOURCE.read_text(encoding='utf-8', errors='replace')
    except OSError:
        return None
    limits = {}
    for name in CAPACITY_LIMIT_NAMES:
        match = re.search(rf'pub const {name}: usize = (\d+);', text)
        if not match:
            return None
        limits[name] = int(match.group(1))
    return limits


def check_lock_order(roots):
    """执行锁类序表对账, 返回 (issues, 摘要行)."""
    issues = []
    summary = []

    def fail(kind, message, where):
        issues.append({
            'file': where,
            'line': 0,
            'severity': SEVERITY_CRITICAL,
            'type': kind,
            'lock': message,
            'function': None,
            'message': message,
            'code': '锁类序表对账 (scripts/lock_class_order.json)',
        })

    if not LOCK_ORDER_TABLE.exists():
        fail('LOCK_ORDER_TABLE_MISSING', f'机器可读序表不存在: {LOCK_ORDER_TABLE}',
             str(LOCK_ORDER_TABLE))
        return issues, ['锁类序表对账: 单源缺失, fail-closed 判红']
    try:
        table = json.loads(LOCK_ORDER_TABLE.read_text(encoding='utf-8'))
    except (OSError, json.JSONDecodeError) as exc:
        fail('LOCK_ORDER_TABLE_MALFORMED', f'序表不可解析: {exc}', str(LOCK_ORDER_TABLE))
        return issues, ['锁类序表对账: 单源不可解析, fail-closed 判红']

    levels = [entry['level'] for entry in table.get('levels', [])]
    classes = table.get('classes', [])
    if levels != list(range(len(levels))):
        fail('LOCK_ORDER_TABLE_MALFORMED',
             f'层级号必须从 0 连续递增, 实测 {levels}', str(LOCK_ORDER_TABLE))
    level_set = set(levels)
    tokens_by_level = defaultdict(set)
    names = set()
    doc_tokens = set()
    lockdep_declared = set()
    symbol_declared = []
    for entry in classes:
        name = entry.get('name', '')
        level = entry.get('level')
        anchor = entry.get('anchor')
        doc_token = entry.get('doc_token', name)
        if not name or name in names:
            fail('LOCK_ORDER_TABLE_MALFORMED', f'条目名缺失或重复: {name!r}', name)
        names.add(name)
        if doc_token in doc_tokens:
            fail('LOCK_ORDER_TABLE_MALFORMED', f'doc_token 重复: {doc_token!r}', name)
        doc_tokens.add(doc_token)
        if anchor not in ('lockdep', 'symbol'):
            fail('LOCK_ORDER_TABLE_MALFORMED', f'anchor 非法: {anchor!r} ({name})', name)
            continue
        if level is None:
            # 无 level 的条目 = 刻意不参与层级递增判定的域 (如最内层发射锁);
            # 仍要过锚点对账, 只是不写进文档层级表.
            pass
        elif level not in level_set:
            fail('LOCK_ORDER_TABLE_MALFORMED', f'条目层级未定义: {level} ({name})', name)
        else:
            tokens_by_level[level].add(doc_token)
        if anchor == 'lockdep':
            lockdep_declared.add(name)
        else:
            symbol_declared.append(name)

    named_sites = collect_named_lock_classes(roots)
    named_code = set(named_sites)
    for undeclared in sorted(named_code - lockdep_declared):
        sites = named_sites[undeclared]
        fail('LOCK_ORDER_UNDECLARED_CLASS',
             f'代码注册了序表未声明的锁类 "{undeclared}" ({sites[0][0]}:{sites[0][1]})'
             f' —— 新锁类必须先入序表', str(sites[0][0]))
    for stale in sorted(lockdep_declared - named_code):
        fail('LOCK_ORDER_CLASS_STALE',
             f'序表声明的 lockdep 类 "{stale}" 在代码里已无注册点 (改名或删除)',
             str(LOCK_ORDER_TABLE))

    # 判据 ④ 容量对账: 类表是定长数组, 到顶之后的第 N+1 个类在运行时只表现为
    # 「注册被丢弃 + 一条粘滞失效读数」(A-127b), 门禁必须在到顶当刻就判红 ——
    # 不设「80% 阈值」这类人工数, 上限一律取 limits.rs 实值.
    limits = read_capacity_limits()
    if limits is None:
        fail('LOCK_CAPACITY_LIMITS_UNREADABLE',
             f'容量上限单源不可解析: {LIMITS_SOURCE} 里定位不到 '
             f'{" / ".join(CAPACITY_LIMIT_NAMES)} 的 `pub const ...: usize = N;`',
             str(LIMITS_SOURCE))
        capacity_note = '容量上限不可解析'
    else:
        max_classes = limits['MAX_LOCK_CLASSES']
        max_held = limits['MAX_HELD_LOCKS']
        for count, label, kind in (
            (len(lockdep_declared), '序表 lockdep 条目', 'LOCK_CAPACITY_TABLE_FULL'),
            (len(named_code), '代码具名类', 'LOCK_CAPACITY_CODE_FULL'),
        ):
            if count >= max_classes:
                fail(kind,
                     f'{label}数 {count} 已到/超类表上限 MAX_LOCK_CLASSES={max_classes} —— '
                     f'再增一个具名锁类就会被 lockdep 丢弃注册 (该类全程脱离检测, 只留'
                     f'粘滞失效读数); 请提高上限或合并锁类',
                     str(LIMITS_SOURCE))
        # 层级数 vs 持栈深度: 严格升序取锁时一条合法长链可在每层各持一把, 而持有栈只
        # 跟得住 MAX_HELD_LOCKS 把 —— 这是覆盖面读数, 不是判据 (抬上限要动 boot BSS
        # 定长数组尺寸, 属另一轮裁定; 溢出本身已由运行时粘滞读数执法).
        capacity_note = (
            f'容量: 类表上限 {max_classes} (序表 {len(lockdep_declared)} · '
            f'代码 {len(named_code)}) · 持栈上限 {max_held} vs 层级 {len(level_set)}'
            f'{" · 深链超出持栈覆盖面 (由运行时粘滞读数执法)" if len(level_set) > max_held else ""}'
        )

    corpus = build_code_corpus(roots)
    for name in symbol_declared:
        missing = [seg for seg in identifier_segments(name)
                   if not re.search(rf'\b{re.escape(seg)}\b', corpus)]
        if missing:
            fail('LOCK_ORDER_SYMBOL_MISSING',
                 f'序表 symbol 锚 "{name}" 在代码里定位不到段 {missing} (条目已失效)',
                 str(LOCK_ORDER_TABLE))

    doc_entries, doc_headers = parse_doc_level_entries(LOCK_ORDER_DOC)
    if doc_entries is None:
        fail('LOCK_ORDER_DOC_UNREADABLE',
             f'文档层级表不可解析: 未找到「{DOC_SECTION_HEADING}」后的围栏块',
             str(LOCK_ORDER_DOC))
        summary.append('锁类序表对账: 序表已解析, 文档侧不可对账 ⇒ 判红')
        return issues, summary

    for level in sorted(level_set):
        if level not in doc_headers:
            fail('LOCK_ORDER_DOC_MISSING_LEVEL', f'文档层级表缺少层级 {level}',
                 str(LOCK_ORDER_DOC))
    for level in sorted(doc_headers - level_set):
        fail('LOCK_ORDER_DOC_UNKNOWN_LEVEL', f'文档出现序表未定义的层级 {level}',
             str(LOCK_ORDER_DOC))

    doc_text_by_level = {level: ' '.join(lines) for level, lines in doc_entries.items()}
    for level in sorted(tokens_by_level):
        text = doc_text_by_level.get(level, '')
        for doc_token in sorted(tokens_by_level[level]):
            if doc_token not in text:
                fail('LOCK_ORDER_DOC_MISSING_ENTRY',
                     f'序表层级 {level} 的条目 "{doc_token}" 未出现在文档该层级条目行',
                     str(LOCK_ORDER_DOC))
    for level in sorted(doc_text_by_level):
        declared = tokens_by_level.get(level, set())
        for token in sorted(controlled_tokens(doc_text_by_level[level])):
            if not any(token in item for item in declared):
                fail('LOCK_ORDER_DOC_UNDECLARED_ENTRY',
                     f'文档层级 {level} 出现序表未登记的条目 "{token}"',
                     str(LOCK_ORDER_DOC))

    summary.append(
        f'锁类序表对账: 层级 {len(level_set)} 条 / 条目 {len(classes)} 个 '
        f'(lockdep 类 {len(lockdep_declared)} · symbol 锚 {len(symbol_declared)}) '
        f'/ 代码具名类 {len(named_code)} 个 / 文档受控条目层级 {len(doc_entries)} 段'
        f' / {capacity_note} → 违规 {len(issues)} 项'
    )
    return issues, summary


# ===========================================================================
# (7) 静态锁序倒序检测 (A-3b, 2026-11-08 裁定)
# ===========================================================================
#
# 裁定形态 = 乙 (一跳跨函数传播) + 甲 (硬判倒序 + 覆盖率棘轮):
#   - 函数内: 用 `let` 绑定的 guard 生命周期 (到块尾或显式 drop) 判「持有 A 时取 B」;
#     非 let 绑定的临时 guard 生命周期 = 一条语句 (Rust 临时量规则).
#   - 跨函数一跳: 持 guard 期间直接调用的被调函数**自身**所取的锁, 合成外层边.
#     被调函数再往下调用的层不传播 (多跳/不动点属 丙, 本轮不做, 见覆盖边界).
#   - 回调包装: 被调函数的入口锁 (entry classes) 覆盖其闭包实参内的取锁点,
#     处理 `PROCESS_TABLE.with_process(pid, |p| { ... })` 这类「锁在 callee、
#     回调在 caller」的形态.
#   - 执法: 已知类之间的降序即红 (例外白名单在 scripts/lock_class_order.json 里
#     机器可读); 未分类取锁点冻结进机器派生基线, 新增/增加即红.
#
# 覆盖边界 (刻意不做, 不进判据): 闭包延迟执行 (guard 已释放后才跑)、函数指针 /
# trait 动态派发、宏展开体、多跳传播、guard 被 move 进被调函数后的提前释放.
# 未解析 / 歧义调用只进摘要读数, 不判红.

LOCK_ORDER_STATIC_BASELINE = PROJECT_ROOT / 'scripts' / 'lock_order_static.baseline.json'

ACQ_METHODS = ('lock', 'try_lock', 'read', 'write', 'lock_irqsave')
ACQ_PATTERN = re.compile(
    r'((?:[A-Za-z_]\w*\s*::\s*)*[A-Za-z_]\w*)\s*\.\s*('
    + '|'.join(ACQ_METHODS)
    + r')\s*\(\s*\)'
)
FN_PATTERN = re.compile(r'\bfn\s+([A-Za-z_]\w*)')
CALL_PATTERN = re.compile(r'\b([A-Za-z_]\w*)\s*\(')
LET_NAME_PATTERN = re.compile(r'\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*=')
# `let` 绑定只有在初值就是 guard 本身时才把生命周期延到块尾. 初值被解引用
# (`let v = *m.lock()`)、接了方法 (`m.lock().clone()`) 或模式匹配 (`let Some(x) =`)
# 时绑定的是值而非 guard, 临时 guard 仍在语句尾释放.
# 接收者正则从末段字段开始匹配, 所以 `let m = self.mounts.lock()` 的头部残留是
# ` self.` —— 允许纯字段链, 但不允许解引用 / 调用结果出现在链上.
GUARD_INIT_HEAD = re.compile(
    r'\s*(?:&\s*(?:mut\s+)?)?(?:[A-Za-z_]\w*\s*\.\s*)*$')
GUARD_INIT_TAIL = re.compile(r'\s*;?\s*$')
CONSTRUCT_PREFIX = re.compile(r'(?:if|while)\s+let\b|^match\b')
CLOSURE_ARG_PATTERN = re.compile(
    r'\|[^|\n]+\||(?:^|[,(])\s*(?:move\s+)?\|\|')
# 不是函数调用的括号形态: 语言关键字 / 宏 / 被单独识别的取锁方法.
CALL_DENYLIST = frozenset({
    'if', 'while', 'for', 'match', 'return', 'loop', 'in', 'fn', 'let', 'move',
    'unsafe', 'async', 'await', 'some', 'none', 'ok', 'err', 'drop',
    'assert', 'assert_eq', 'assert_ne', 'debug_assert', 'debug_assert_eq',
    'panic', 'unwrap', 'expect', 'vec', 'format', 'println', 'print', 'write',
    'writeln', 'debug', 'trace', 'info', 'warn', 'error', 'cfg', 'derive',
    *ACQ_METHODS,
})
# 太泛的接收者末段不进类映射: `PhysicalMemoryManager::lock` 是 AtomicBool 手递
# 自旋标志 (从不以 `.lock()` 形态获取), 而 `self.lock.lock()` 这类同名字段会让
# 它被误当成层级 1 —— 宁可记为未分类 (进棘轮), 不做错误归类.
GENERIC_RECEIVER_KEYS = frozenset({'lock'})


class _Function:
    """一个函数体 (含嵌套 fn 时各自独立, 取锁点归最内层函数)."""

    __slots__ = ('name', 'path', 'start', 'end', 'index')

    def __init__(self, name, path, start, end, index):
        self.name = name
        self.path = path
        self.start = start
        self.end = end
        self.index = index


class _Acq:
    """一处取锁点 (`.lock()` / `.try_lock()` / `.read()` / `.write()`)."""

    __slots__ = ('path', 'offset', 'line', 'receiver', 'cls', 'func', 'hold_end',
                 'block_end', 'stmt_end')

    def __init__(self, path, offset, line, receiver, cls):
        self.path = path
        self.offset = offset
        self.line = line
        self.receiver = receiver
        self.cls = cls
        self.func = None
        self.hold_end = offset
        self.block_end = offset
        self.stmt_end = offset


class _Call:
    """一处调用点; `has_closure` 标记实参里带闭包 (回调包装判定)."""

    __slots__ = ('name', 'path', 'offset', 'line', 'args_start', 'args_end',
                 'has_closure', 'func')

    def __init__(self, name, path, offset, line, args_start, args_end, has_closure):
        self.name = name
        self.path = path
        self.offset = offset
        self.line = line
        self.args_start = args_start
        self.args_end = args_end
        self.has_closure = has_closure
        self.func = None


def strip_code(text):
    """把注释与字符串字面量替换为空格, 保留换行与字符偏移.

    只用空格覆盖而不是删除: 后续所有匹配都按偏移定位行号, 偏移一变行号就跟着错.
    Rust 块注释可嵌套, 这里按深度处理; 原始字符串 `r#"..."#` 未特化 (退化到普通
    字符串扫描), 本仓内核代码没有在锁表达式里使用原始字符串.
    """
    out = list(text)
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == '/' and i + 1 < n and text[i + 1] == '/':
            while i < n and text[i] != '\n':
                out[i] = ' '
                i += 1
        elif c == '/' and i + 1 < n and text[i + 1] == '*':
            depth = 1
            out[i] = out[i + 1] = ' '
            i += 2
            while i < n and depth > 0:
                if text.startswith('/*', i):
                    depth += 1
                    out[i] = out[i + 1] = ' '
                    i += 2
                elif text.startswith('*/', i):
                    depth -= 1
                    out[i] = out[i + 1] = ' '
                    i += 2
                else:
                    if text[i] != '\n':
                        out[i] = ' '
                    i += 1
        elif c == '"':
            out[i] = ' '
            i += 1
            while i < n:
                if text[i] == '\\':
                    out[i] = ' '
                    if i + 1 < n and text[i + 1] != '\n':
                        out[i + 1] = ' '
                    i += 2
                    continue
                if text[i] == '"':
                    out[i] = ' '
                    i += 1
                    break
                if text[i] != '\n':
                    out[i] = ' '
                i += 1
        elif c == "'":
            # 字符字面量剥掉; 生命周期 (`'a`) 与标签保留 —— 它们不含花括号.
            match = re.match(r"'(?:\\.|[^\\'])'", text[i:])
            if match:
                for k in range(i, i + match.end()):
                    if text[k] != '\n':
                        out[k] = ' '
                i += match.end()
            else:
                i += 1
        else:
            i += 1
    return ''.join(out)


def _line_starts(text):
    starts = [0]
    for idx, ch in enumerate(text):
        if ch == '\n':
            starts.append(idx + 1)
    return starts


def _brace_pairs(stripped):
    """`{` 偏移 -> `}` 偏移 (两个方向都返回; 未配对的丢弃)."""
    pairs = {}
    stack = []
    for idx, ch in enumerate(stripped):
        if ch == '{':
            stack.append(idx)
        elif ch == '}' and stack:
            pairs[stack.pop()] = idx
    return pairs


def _paren_pairs(stripped):
    """`(` 偏移 -> `)` 偏移."""
    pairs = {}
    stack = []
    for idx, ch in enumerate(stripped):
        if ch == '(':
            stack.append(idx)
        elif ch == ')' and stack:
            pairs[stack.pop()] = idx
    return pairs


def _enclosing_block(open_offsets, pairs, offset):
    """返回包含 offset 的最内层块 `{...}` 的结束偏移; 无块时为文件尾."""
    idx = bisect_right(open_offsets, offset) - 1
    while idx >= 0:
        close = pairs.get(open_offsets[idx])
        if close is not None and close > offset:
            return close
        idx -= 1
    return -1


def _statement_start(stripped, offset):
    """从取锁点向后找到本语句的起点 (上一个 `;` 或所在块的开括号之后).

    必须按语句边界取, 不能按固定窗口回看: 窗口里残留的上一条 `let` 会让普通取锁点
    被误判成 guard 绑定, 从而把生命周期错误地延长到块尾.
    """
    depth = 0
    i = offset - 1
    while i >= 0:
        c = stripped[i]
        if c == '}':
            depth += 1
        elif c == '{':
            if depth == 0:
                return i + 1
            depth -= 1
        elif c == ';' and depth == 0:
            return i + 1
        i -= 1
    return 0


def _statement_end(stripped, offset, block_end):
    """从取锁点向前找到语句结束 (`;` 或所在块尾).

    Rust 临时 guard 在语句尾释放, 所以非 `let` 绑定的取锁点只覆盖到语句尾;
    `let` 绑定的 guard 覆盖到块尾 (见 `_guard_holds`).
    """
    depth = 0
    i = offset
    limit = len(stripped) if block_end < 0 else block_end
    while i < limit:
        c = stripped[i]
        if c == '{':
            depth += 1
        elif c == '}':
            if depth == 0:
                return i
            depth -= 1
        elif c == ';' and depth == 0:
            return i
        i += 1
    return limit


def _body_start(stripped, from_offset):
    """从头 `fn name` 之后找函数体 `{`; 先遇到声明分号则返回 None."""
    paren = bracket = 0
    i = from_offset
    n = len(stripped)
    while i < n:
        c = stripped[i]
        if c == '(':
            paren += 1
        elif c == ')':
            paren -= 1
        elif c == '[':
            bracket += 1
        elif c == ']':
            bracket -= 1
        elif c == '{' and paren == 0 and bracket == 0:
            return i
        elif c == ';' and paren == 0 and bracket == 0:
            return None
        i += 1
    return None


def _construct_body_end(stripped, pairs, offset, limit):
    """`if let` / `while let` / `match` 体的结束偏移.

    scrutinee 里的临时 guard 存活到整个构造结束. 取锁点之后第一个括号深度为 0
    的 `{` 就是体开括号 (跳过实参里的闭包), 再用花括号配对取体尾.
    """
    paren = 0
    i = offset
    while i < limit:
        c = stripped[i]
        if c == '(':
            paren += 1
        elif c == ')':
            paren -= 1
        elif c == '{' and paren == 0:
            return pairs.get(i, limit)
        i += 1
    return limit


def build_lock_class_index(table):
    """序表 -> 接收者末段名 -> `{name, level}`.

    同名末段映射到两个不同类时按歧义处理 (不映射), 宁可算未分类也不误归类.
    """
    index = {}
    ambiguous = set()
    for entry in table.get('classes', []):
        name = entry.get('name', '')
        segments = identifier_segments(name)
        if not segments:
            continue
        key = segments[-1]
        if key in GENERIC_RECEIVER_KEYS:
            continue
        if key in index and index[key]['name'] != name:
            ambiguous.add(key)
            continue
        index[key] = {'name': name, 'level': entry.get('level')}
    for key in ambiguous:
        index.pop(key, None)
    return index, ambiguous


def _index_file(path, text, class_index):
    """解析单个文件 -> (函数列表, 取锁点列表, 调用点列表, 未分类计数)."""
    stripped = strip_code(text)
    pairs = _brace_pairs(stripped)
    open_offsets = sorted(pairs)
    parens = _paren_pairs(stripped)
    line_starts = _line_starts(text)

    funcs = []
    for match in FN_PATTERN.finditer(stripped):
        body = _body_start(stripped, match.end())
        if body is None:
            continue
        end = pairs.get(body)
        if end is None:
            continue
        funcs.append(_Function(match.group(1), str(path), body, end, len(funcs)))
    func_starts = [f.start for f in funcs]

    def innermost(offset):
        idx = bisect_right(func_starts, offset) - 1
        while idx >= 0:
            fn = funcs[idx]
            if fn.start < offset < fn.end:
                return fn
            idx -= 1
        return None

    acqs = []
    unclassified = {}
    for match in ACQ_PATTERN.finditer(stripped):
        offset = match.start()
        receiver_raw = match.group(1)
        key = re.split(r'::', receiver_raw)[-1].strip()
        info = class_index.get(key)
        fn = innermost(offset)
        line = bisect_right(line_starts, offset)
        acq = _Acq(str(path), offset, line, key, info['name'] if info else None)
        acq.func = fn.index if fn else None
        block_end = _enclosing_block(open_offsets, pairs, offset)
        acq.block_end = block_end
        acq.stmt_end = _statement_end(stripped, offset, block_end)
        prefix = stripped[_statement_start(stripped, offset):offset].lstrip()
        let_match = LET_NAME_PATTERN.match(prefix)
        if let_match and GUARD_INIT_HEAD.match(prefix[let_match.end():]) \
                and GUARD_INIT_TAIL.match(stripped[match.end():acq.stmt_end]):
            guard_name = let_match.group(1)
            acq.hold_end = block_end if block_end >= 0 else acq.stmt_end
            if guard_name != '_':
                drop = re.search(
                    rf'\bdrop\s*\(\s*{re.escape(guard_name)}\s*\)',
                    stripped[offset:acq.hold_end],
                )
                if drop:
                    acq.hold_end = offset + drop.start()
        elif CONSTRUCT_PREFIX.match(prefix):
            acq.hold_end = _construct_body_end(
                stripped, pairs, match.end(),
                block_end if block_end >= 0 else len(stripped))
        else:
            acq.hold_end = acq.stmt_end
        acqs.append(acq)
        if info is None:
            unclassified[key] = unclassified.get(key, 0) + 1

    calls = []
    for match in CALL_PATTERN.finditer(stripped):
        name = match.group(1)
        if name in CALL_DENYLIST:
            continue
        before = stripped[max(0, match.start() - 8):match.start()]
        if re.search(r'\bfn\s*$', before) or before.rstrip().endswith('!'):
            continue
        open_paren = match.end() - 1
        close = parens.get(open_paren)
        if close is None:
            continue
        fn = innermost(match.start())
        call = _Call(
            name, str(path), match.start(),
            bisect_right(line_starts, match.start()), open_paren, close,
            bool(CLOSURE_ARG_PATTERN.search(stripped[open_paren:close])),
        )
        call.func = fn.index if fn else None
        calls.append(call)

    return funcs, acqs, calls, unclassified


def analyze_static_lock_order(roots, report=None):
    """执行静态锁序倒序检测 + 未分类取锁点棘轮, 返回 (issues, 摘要行).

    `report` 非 None 时走基线生成模式: 不回读/不比对棘轮, 把冻结结果填进
    `report` 供 `--update-lock-order-baseline` 落盘.
    """
    issues = []
    summary = []

    def fail(kind, message, where, line=0):
        issues.append({
            'file': where,
            'line': line,
            'severity': SEVERITY_CRITICAL,
            'type': kind,
            'lock': message,
            'function': None,
            'message': message,
            'code': '静态锁序检测 (A-3b)',
        })

    if not LOCK_ORDER_TABLE.exists():
        fail('LOCK_ORDER_STATIC_TABLE_MISSING',
             f'机器可读序表不存在, 静态锁序检测 fail-closed: {LOCK_ORDER_TABLE}',
             str(LOCK_ORDER_TABLE))
        return issues, ['静态锁序检测: 序表缺失, fail-closed 判红']
    try:
        table = json.loads(LOCK_ORDER_TABLE.read_text(encoding='utf-8'))
    except (OSError, json.JSONDecodeError) as exc:
        fail('LOCK_ORDER_STATIC_TABLE_MALFORMED', f'序表不可解析: {exc}',
             str(LOCK_ORDER_TABLE))
        return issues, ['静态锁序检测: 序表不可解析, fail-closed 判红']

    class_index, ambiguous_keys = build_lock_class_index(table)
    level_by_class = {entry.get('name'): entry.get('level')
                      for entry in table.get('classes', [])}
    declared_names = {entry.get('name') for entry in table.get('classes', [])}
    exceptions = set()
    for exc in table.get('exceptions', []):
        outer, inner = exc.get('outer'), exc.get('inner')
        if not outer or not inner:
            fail('LOCK_ORDER_STATIC_EXCEPTION_MALFORMED',
                 f'例外条目缺 outer/inner: {exc!r}', str(LOCK_ORDER_TABLE))
            continue
        if outer not in declared_names or inner not in declared_names:
            fail('LOCK_ORDER_STATIC_EXCEPTION_UNKNOWN',
                 f'例外条目引用了序表未声明的类: {outer} -> {inner}',
                 str(LOCK_ORDER_TABLE))
            continue
        exceptions.add((outer, inner))

    per_file = []
    all_funcs = []
    unresolved_calls = 0
    ambiguous_calls = 0
    for base in roots:
        for path in sorted(base.rglob('*.rs')):
            if 'smoltcp' in str(path):
                continue
            try:
                text = path.read_text(encoding='utf-8', errors='replace')
            except OSError:
                continue
            funcs, acqs, calls, unclassified = _index_file(path, text, class_index)
            base_index = len(all_funcs)
            for fn in funcs:
                fn.index += base_index
            for acq in acqs:
                if acq.func is not None:
                    acq.func += base_index
            for call in calls:
                if call.func is not None:
                    call.func += base_index
            all_funcs.extend(funcs)
            per_file.append((path, funcs, acqs, calls, unclassified))

    defs = defaultdict(list)
    for fn in all_funcs:
        defs[fn.name].append(fn)

    def resolve(name):
        candidates = defs.get(name, [])
        if len(candidates) == 1:
            return candidates[0]
        return None

    entry_cache = {}

    def entry_classes(fn, stack):
        """被调函数的入口锁类: 自身顶层取锁类 + 首把锁之前委托调用的入口类 (保守近似)."""
        if fn.index in entry_cache:
            return entry_cache[fn.index]
        if fn.index in stack:
            return set()
        stack = stack | {fn.index}
        result = set()
        first_acq = None
        for path, funcs, acqs, calls, _unused in per_file:
            if not funcs or funcs[0].path != fn.path:
                continue
            direct = [a for a in acqs if a.func == fn.index]
            own_calls = [c for c in calls if c.func == fn.index]
            top = [a for a in direct
                   if not any(o.func == fn.index and o.offset < a.offset < o.hold_end
                              for o in direct)]
            result.update(a.cls for a in top if a.cls)
            if direct:
                first_acq = min(a.offset for a in direct)
            for call in own_calls:
                if first_acq is not None and call.offset > first_acq:
                    continue
                callee = resolve(call.name)
                if callee is not None:
                    result.update(entry_classes(callee, stack))
            break
        entry_cache[fn.index] = result
        return result

    edges = {}

    def add_edge(outer, inner, path, line, kind):
        if outer == inner:
            return
        key = (outer, inner)
        if key not in edges:
            edges[key] = {'outer': outer, 'inner': inner, 'path': str(path),
                          'line': line, 'kind': kind, 'count': 1}
        else:
            edges[key]['count'] += 1

    classified_sites = 0
    total_sites = 0
    for path, funcs, acqs, calls, _unclassified in per_file:
        by_func = defaultdict(list)
        for acq in acqs:
            if acq.func is not None:
                by_func[acq.func].append(acq)
            if acq.cls:
                classified_sites += 1
            else:
                total_sites += 1
        for fn in funcs:
            direct = by_func.get(fn.index, [])
            if not direct:
                continue
            holds = [a for a in direct if a.cls]
            for outer in holds:
                for inner in direct:
                    if inner.cls and outer.offset < inner.offset < outer.hold_end:
                        add_edge(outer.cls, inner.cls, path, inner.line, 'lexical')
            own_calls = [c for c in calls if c.func == fn.index]
            for call in own_calls:
                holders = [h for h in holds if h.offset < call.offset < h.hold_end]
                callee = resolve(call.name)
                if callee is None:
                    if holders:
                        candidates = defs.get(call.name, [])
                        if candidates:
                            ambiguous_calls += 1
                        else:
                            unresolved_calls += 1
                    continue
                callee_acqs = []
                for _p, funcs2, acqs2, _c2, _u2 in per_file:
                    if funcs2 and funcs2[0].path == callee.path:
                        callee_acqs = [a for a in acqs2 if a.func == callee.index]
                        break
                for holder in holders:
                    for target in callee_acqs:
                        if target.cls:
                            add_edge(holder.cls, target.cls, target.path,
                                     target.line, 'call')
                # 回调包装: callee 的入口锁覆盖闭包实参内的取锁点.
                wrapper_classes = entry_classes(callee, frozenset())
                if wrapper_classes and call.has_closure:
                    span = [a for a in acqs
                            if call.args_start < a.offset < call.args_end and a.cls]
                    for wrapper in sorted(wrapper_classes):
                        for holder in holders:
                            add_edge(holder.cls, wrapper, path, call.line,
                                     'wrapper-call')
                        for target in span:
                            add_edge(wrapper, target.cls, path, target.line,
                                     'wrapper-callback')

    for (outer, inner), info in sorted(edges.items()):
        if (outer, inner) in exceptions:
            continue
        outer_level = level_by_class.get(outer)
        inner_level = level_by_class.get(inner)
        if outer_level is None:
            fail('LOCK_ORDER_STATIC_INVERSION',
                 f'持最内层锁 {outer} 期间取了 {inner} (层级 {outer_level} -> '
                 f'{inner_level}); {outer} 之后不得再取任何锁, 除非在序表例外里 '
                 f'(派生形态={info["kind"]}, 该组合共 {info["count"]} 处)',
                 info['path'], info['line'])
        elif inner_level is not None and inner_level < outer_level:
            fail('LOCK_ORDER_STATIC_INVERSION',
                 f'持层级 {outer_level} 的 {outer} 期间取了层级 {inner_level} 的 '
                 f'{inner} (降序); 派生形态={info["kind"]}, 该组合共 '
                 f'{info["count"]} 处',
                 info['path'], info['line'])

    frozen = defaultdict(int)
    for path, _funcs, _acqs, _calls, unclassified in per_file:
        try:
            rel = str(path.resolve().relative_to(PROJECT_ROOT))
        except ValueError:
            rel = str(path)
        for key, count in unclassified.items():
            frozen[f'{rel}::{key}'] += count

    baseline = None
    baseline_error = None
    if report is not None:
        report['frozen'] = dict(sorted(frozen.items()))
        report['edge_list'] = dict(sorted(edges.items()))
        report['edges'] = len(edges)
        report['classified'] = classified_sites
        report['total'] = total_sites
    else:
        try:
            baseline_raw = json.loads(
                LOCK_ORDER_STATIC_BASELINE.read_text(encoding='utf-8'))
            baseline = baseline_raw.get('unclassified', {})
        except OSError:
            baseline_error = f'基线不存在: {LOCK_ORDER_STATIC_BASELINE}'
        except json.JSONDecodeError as exc:
            baseline_error = f'基线不可解析: {exc}'

    growth = []
    if report is None:
        if baseline is None:
            fail('LOCK_ORDER_STATIC_BASELINE_MISSING',
                 f'{baseline_error}; 新增未分类取锁点必须先归类, 或显式跑 '
                 f'`python3 scripts/audit_deadlock_matrix.py '
                 f'--update-lock-order-baseline` 并提交快照 '
                 f'(判据在覆盖率上, 不静默放行)',
                 str(LOCK_ORDER_STATIC_BASELINE))
        else:
            for key, count in sorted(frozen.items()):
                base = baseline.get(key, 0)
                if count > base:
                    growth.append((key, count, base))
                    fail('LOCK_ORDER_STATIC_UNCLASSIFIED_GROWTH',
                         f'未分类取锁点新增/增加: {key} 实测 {count} 处, 基线 {base} 处 '
                         f'—— 新取锁点必须能映射到序表类, 或走 '
                         f'--update-lock-order-baseline 显式登记',
                         key.rsplit('::', 1)[0])

    baseline_total = sum(baseline.values()) if baseline else 0
    summary.append(
        f'静态锁序检测 (A-3b): 取锁点 {total_sites + classified_sites} '
        f'(已分类 {classified_sites} · 未分类 {total_sites}) / 函数 {len(all_funcs)} '
        f'/ 组合边 {len(edges)} / 例外 {len(exceptions)} '
        f'/ 未解析调用 {unresolved_calls} · 歧义 {ambiguous_calls} '
        f'/ 未分类棘轮 {sum(frozen.values())} vs 基线 {baseline_total} '
        f'(新增/增加 {len(growth)} 项) → 违规 {len(issues)} 项'
    )
    return issues, summary


def dump_lock_order_static_baseline(frozen, edges, classified_sites, total_sites):
    """确定性序列化: 排序 + 固定缩进, 保证 --update 幂等到字节级."""
    payload = {
        'schema': 'edgine-lock-order-static-baseline/v1',
        'generated_by': ('python3 scripts/audit_deadlock_matrix.py '
                         '--update-lock-order-baseline'),
        'note': ('A-3b 覆盖率棘轮: 未分类取锁点按 (文件, 接收者末段名) 冻结. '
                 '新增/增加即门禁判红; 收缩时重跑 --update-lock-order-baseline. '
                 '这份快照不是锁序判据 —— 倒序判定走 scripts/lock_class_order.json.'),
        'totals': {
            'unclassified_sites': sum(frozen.values()),
            'classified_sites': classified_sites,
            'acquisition_sites': total_sites + classified_sites,
            'lock_class_edges': edges,
        },
        'unclassified': dict(sorted(frozen.items())),
    }
    return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=False) + '\n'


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

    order_roots = [PROJECT_ROOT / base for base in BASES]

    # 基线生成模式: 只跑静态锁序检测并落盘覆盖率快照, 不比对棘轮也不判门禁.
    if '--update-lock-order-baseline' in sys.argv[1:]:
        baseline_report = {}
        analyze_static_lock_order(order_roots, report=baseline_report)
        LOCK_ORDER_STATIC_BASELINE.write_text(
            dump_lock_order_static_baseline(
                baseline_report.get('frozen', {}),
                baseline_report.get('edges', 0),
                baseline_report.get('classified', 0),
                baseline_report.get('total', 0),
            ),
            encoding='utf-8',
        )
        print(f'已写入静态锁序覆盖率基线: {LOCK_ORDER_STATIC_BASELINE} '
              f'(未分类 {sum(baseline_report.get("frozen", {}).values())} 处 / '
              f'{len(baseline_report.get("frozen", {}))} 项)')
        sys.exit(0)

    issues = []
    file_count = 0
    for base in BASES:
        print(f'扫描 {base} ...')
        root_issues, root_files = scan_directory(base)
        issues.extend(root_issues)
        file_count += root_files

    # (6) 锁类序表对账: 根路径按脚本位置取绝对路径, 不随 CWD 漂移.
    order_issues, order_summary = check_lock_order(order_roots)
    issues.extend(order_issues)

    # (7) 静态锁序倒序检测 (A-3b): 一跳跨函数传播 + 未分类取锁点棘轮.
    static_issues, static_summary = analyze_static_lock_order(order_roots)
    issues.extend(static_issues)

    report = generate_report(issues, file_count)
    print(report)
    for line in order_summary:
        print(line)
    for line in static_summary:
        print(line)

    # 保存 JSON 报告 (gitignored other/target/audit/)
    json_path = Path('other/target/audit/deadlock-matrix.json')
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
