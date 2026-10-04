#!/usr/bin/env python3
"""
E9: 6 安全不变式审计脚本

检查 QueenX/QueenX 是否违反星绽框内核定义的 6 条安全不变式:

  I1: 内核态 CPU 状态不可被 services 篡改
  I2: 内核内存不可被 services 非法访问
  I3: 用户态 CPU 状态只能通过 framework 安全入口修改
  I4: 用户内存只能通过 framework 安全代理访问
  I5: 外设 MMIO/PIO 只能通过 framework 安全代理访问
  I6: 外设 DMA 不可写入内核内存

退出码: 0 = 通过, 1 = 有违反
"""

import re
import sys
import json
from pathlib import Path

BASE = Path(__file__).resolve().parent.parent
SERVICES = BASE / 'src' / 'kernel' / 'services'
TARGET_DIR = BASE / 'other' / 'target' / 'audit'

violations = []


def check_i1():
    """I1: services 不可直接操作内核态 CPU 状态寄存器"""
    # 检查 services 中是否包含 CR0/CR3/CR4/GDT/IDT/LDT/TR/MSR 的汇编操作
    # 注意: 仅检查汇编/内联汇编中的寄存器操作, 不检查字符串/注释/类型名引用
    patterns = [
        r'mov\s+cr[0-4]', r'mov\s+.*,\s*cr[0-4]',  # x86 CR 寄存器操作
        r'wrmsr', r'rdmsr',                           # x86 MSR 操作
        r'lgdt', r'lidt', r'lldt', r'ltr',            # x86 描述符表加载
        r'mrs\s+sctlr', r'msr\s+sctlr',               # ARM SCTLR 操作
        r'mrs\s+ttbr[01]', r'msr\s+ttbr[01]',         # ARM 页表基址
        r'mrs\s+vbar', r'msr\s+vbar',                  # ARM 异常向量基址
    ]
    found = _scan_services(patterns, 'I1')
    return found


def check_i2():
    """I2: services 不可直接访问内核内存 (裸指针解引用).

    B01-25 修复: I2 检测范围回归 services 层.
    设计依据: I2 不变式 (AGENTS.md §4.2) 语义为 "内核内存不可被 services
    非法访问", 守护对象是 services, 而非 framework. framework 是合法 TCB,
    裸指针解引用是其职责 (127 处全部位于 unsafe 块内), 其安全由 F4
    (audit_safety_coverage.py SAFETY 100% 覆盖) 守护, 不属 I2 范畴.

    历史: B01-21 (commit c00e9e55) 将 I2 范围误改为 framework 层, 产生
    127 处误报 (framework 合法 unsafe 解引用). B01-25 恢复扫描 services,
    消除误报. services 层 `#![deny(unsafe_code)]` (B01-09) 编译期保证
    无裸指针, 本检查作为纵深防御 (防未来某天移除 deny).

    注: 原 B01-21 前误报 (services 层对 `&T`/`Box` 的 safe 解引用被误判
    为裸指针解引用) 已由当前 pattern 负向前瞻排除方法实参位置解决,
    实测 services 0 违规.
    """
    # 真正的违反: services 中对裸指针的解引用 (*ptr).field
    patterns = [
        r'(?<![\w.,(])\(\*\w+\)\.',        # (*ptr).field — 表达式起首, 非方法实参
        r'\(\*mut\s+\w+\)\.',              # (*mut T).field
        r'\(\*const\s+\w+\)\.',            # (*const T).field
    ]
    return _scan_services(patterns, 'I2', exclude_patterns=[r'^\s*use\s+', r'^\s*//', r'^\s*//!'])


def check_i3():
    """I3: 用户态 CPU 状态只能通过 framework 安全入口修改"""
    # 检查 services 中是否有直接 iretq/eret/sysret 汇编指令
    # 注意: "syscall" 作为标识符/字符串不应匹配, 仅匹配汇编指令
    patterns = [
        r'\biretq?\b',               # x86 中断返回
        r'\beret\b',                  # ARM 异常返回
        r'\bsysretq?\b',              # x86 系统调用返回
    ]
    found = _scan_services(patterns, 'I3')
    return found


def check_i4():
    """I4: 用户内存只能通过 framework 安全代理访问"""
    # 检查 services 中是否有直接的用户内存访问 (非 copy_from/to_user)
    patterns = [
        r'read_volatile\b(?!.*copy_from_user)',
        r'write_volatile\b(?!.*copy_to_user)',
        r'core::ptr::read\b(?!.*copy_from_user)',
        r'core::ptr::write\b(?!.*copy_to_user)',
    ]
    found = _scan_services(patterns, 'I4', exclude_patterns=[r'copy_from_user', r'copy_to_user', r'userptr'])
    return found


def check_i5():
    """I5: 外设 MMIO/PIO 只能通过 framework 安全代理访问"""
    # 检查 services 中是否有直接的 I/O 端口或 MMIO 读写指令
    # 合法: 通过 framework::iomem / framework::ioport 代理
    patterns = [
        r'\binb\s*\(', r'\boutb\s*\(',    # x86 I/O 端口
        r'\binl\s*\(', r'\boutl\s*\(',
        r'\binw\s*\(', r'\boutw\s*\(',
    ]
    found = _scan_services(patterns, 'I5', exclude_patterns=[r'iomem', r'ioport', r'framework'])
    return found


def check_i6():
    """I6: 外设 DMA 不可写入内核内存"""
    # 检查 services 中是否有直接的 DMA 映射操作 (不通过 dma_buf)
    patterns = [
        r'dma_map\b(?!.*dma_buf)',
        r'dma_unmap\b(?!.*dma_buf)',
        r'dma_sync\b(?!.*dma_buf)',
    ]
    found = _scan_services(patterns, 'I6', exclude_patterns=[r'dma_buf', r'framework'])
    return found


def _scan_services(patterns, invariant_id, exclude_patterns=None):
    """扫描 services 目录, 返回匹配数"""
    found = 0
    for rs in SERVICES.rglob('*.rs'):
        # B01-18/B01-21 修复: 排除 vendored smoltcp (3rd-party,
        # 上游未修改, 无需按 I1-I6 守护)
        if 'smoltcp' in str(rs):
            continue
        with open(rs, 'r', encoding='utf-8', errors='replace') as f:
            for lineno, line in enumerate(f, 1):
                stripped = line.strip()
                # 跳过注释
                if stripped.startswith('//') or stripped.startswith('/*') or stripped.startswith('//!'):
                    continue
                # 跳过排除模式
                if exclude_patterns:
                    skip = False
                    for ep in exclude_patterns:
                        if re.search(ep, stripped):
                            skip = True
                            break
                    if skip:
                        continue
                for pat in patterns:
                    if re.search(pat, stripped, re.IGNORECASE):
                        rel = rs.relative_to(BASE)
                        violations.append({
                            'invariant': invariant_id,
                            'file': str(rel),
                            'line': lineno,
                            'content': stripped[:120],
                        })
                        found += 1
                        break  # 每行最多报一次
    return found


def main():
    print("=" * 70)
    print("6 Safety Invariants Audit")
    print("=" * 70)

    checks = [
        ('I1', 'CPU state not tampered by services', check_i1),
        ('I2', 'Kernel memory not accessed by services', check_i2),
        ('I3', 'User CPU state via framework only', check_i3),
        ('I4', 'User memory via framework only', check_i4),
        ('I5', 'MMIO/PIO via framework only', check_i5),
        ('I6', 'DMA cannot write kernel memory', check_i6),
    ]

    total_violations = 0
    for inv_id, desc, check_fn in checks:
        count = check_fn()
        status = "PASS" if count == 0 else f"FAIL ({count} violations)"
        symbol = "✓" if count == 0 else "✗"
        print(f"  {symbol} {inv_id}: {desc} — {status}")
        total_violations += count

    print("=" * 70)

    if total_violations > 0:
        print(f"\n⚠  {total_violations} invariant violations found:")
        for v in violations[:20]:  # 最多显示 20 条
            print(f"  {v['invariant']}: {v['file']}:{v['line']}: {v['content']}")
        if len(violations) > 20:
            print(f"  ... and {len(violations) - 20} more")
    else:
        print("\n✓ All 6 safety invariants satisfied")

    # 保存 JSON
    TARGET_DIR.mkdir(parents=True, exist_ok=True)
    json_path = TARGET_DIR / 'invariants-report.json'
    with open(json_path, 'w') as f:
        json.dump({
            'total_violations': total_violations,
            'violations': violations[:100],
        }, f, indent=2, ensure_ascii=False)
    print(f"\nJSON report: {json_path}")

    sys.exit(1 if total_violations > 0 else 0)


if __name__ == '__main__':
    main()
