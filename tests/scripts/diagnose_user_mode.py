#!/usr/bin/env python3
"""
QueenX 用户态进程启动诊断脚本
用于诊断 iretq 后的 Page Fault 问题
"""

import subprocess
import re
import sys
from pathlib import Path

def run_qemu_with_debug():
    """运行 QEMU 并捕获调试输出"""
    print("=" * 60)
    print("运行 QEMU 并捕获调试信息...")
    print("=" * 60)
    
    cmd = [
        "timeout", "5",
        "qemu-system-x86_64",
        "-cdrom", "other/build/antx.iso",
        "-serial", "stdio",
        "-no-reboot",
        "-d", "int",
        "-D", "/tmp/qemu_debug.log"
    ]
    
    try:
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10
        )
        
        log_file = Path("/tmp/qemu_debug.log")
        debug_log = ""
        if log_file.exists():
            with open(log_file, 'r') as f:
                debug_log = f.read()
        
        return result.stdout + result.stderr + "\n" + debug_log
    except subprocess.TimeoutExpired:
        print("QEMU 运行超时")
        log_file = Path("/tmp/qemu_debug.log")
        if log_file.exists():
            with open(log_file, 'r') as f:
                return f.read()
        return ""
    except Exception as e:
        print(f"运行 QEMU 时出错: {e}")
        return ""

def analyze_page_fault(output):
    """分析 Page Fault 信息"""
    print("\n" + "=" * 60)
    print("分析 Page Fault 信息...")
    print("=" * 60)
    
    fault_pattern = r"v=0e e=([0-9a-f]+) i=([0-9]) cpl=([0-9]) IP=([0-9a-f]+):([0-9a-f]+) CR2=([0-9a-f]+)"
    matches = re.findall(fault_pattern, output)
    
    if not matches:
        print("未找到 Page Fault 信息")
        return
    
    for i, match in enumerate(matches, 1):
        error_code, interrupt, cpl, cs, rip, cr2 = match
        
        print(f"\n第 {i} 次 Page Fault:")
        print(f"  错误代码: 0x{error_code}")
        print(f"  CPL (当前特权级): {cpl}")
        print(f"  CS: 0x{cs}")
        print(f"  RIP: 0x{rip}")
        print(f"  CR2 (故障地址): 0x{cr2}")
        
        error_code_int = int(error_code, 16)
        present = (error_code_int >> 0) & 1
        write = (error_code_int >> 1) & 1
        user = (error_code_int >> 2) & 1
        
        print(f"\n  错误代码解析:")
        print(f"    P (页面存在): {present}")
        print(f"    W/R (写/读): {'写' if write else '读'}")
        print(f"    U/S (用户/内核): {'用户' if user else '内核'}")
        
        if present and user:
            print(f"\n  诊断: 保护违规 - 用户态尝试访问存在但无权限的页面")
        elif not present:
            print(f"\n  诊断: 页面不存在 - 地址 0x{cr2} 未映射")
        else:
            print(f"\n  诊断: 其他错误")

def analyze_kernel_output(output):
    """分析内核输出"""
    print("\n" + "=" * 60)
    print("分析内核输出...")
    print("=" * 60)
    
    if "User bit: SET" in output:
        print("✓ 用户位已设置")
    else:
        print("✗ 用户位未设置")
    
    if "User stack is 16-byte aligned" in output:
        print("✓ 用户栈已 16 字节对齐")
    else:
        print("✗ 用户栈未 16 字节对齐")
    
    entry_pattern = r"entry=0x([0-9a-f]+)"
    entry_match = re.search(entry_pattern, output)
    if entry_match:
        entry = entry_match.group(1)
        print(f"\n入口地址: 0x{entry}")
    
    cr3_pattern = r"cr3=0x([0-9a-f]+)"
    cr3_match = re.search(cr3_pattern, output)
    if cr3_match:
        cr3 = cr3_match.group(1)
        print(f"用户页表 CR3: 0x{cr3}")
    
    pml4_pattern = r"PML4\[([0-9]+)\] = 0x([0-9a-f]+)"
    pml4_matches = re.findall(pml4_pattern, output)
    if pml4_matches:
        print("\n页表结构:")
        for idx, value in pml4_matches:
            value_int = int(value, 16)
            present = (value_int >> 0) & 1
            rw = (value_int >> 1) & 1
            user = (value_int >> 2) & 1
            ps = (value_int >> 7) & 1
            frame = (value_int >> 12) & 0xFFFFFFFFFF
            
            print(f"  PML4[{idx}] = 0x{value}")
            print(f"    P={present} RW={rw} U={user} PS={ps} Frame=0x{frame:X}")

def check_page_table_permissions(output):
    """检查页表权限"""
    print("\n" + "=" * 60)
    print("检查页表权限...")
    print("=" * 60)
    
    pt_pattern = r"PT\[([0-9]+)\] = 0x([0-9a-f]+)"
    pt_matches = re.findall(pt_pattern, output)
    
    if not pt_matches:
        print("未找到 PT 信息")
        return
    
    for idx, value in pt_matches:
        value_int = int(value, 16)
        present = (value_int >> 0) & 1
        rw = (value_int >> 1) & 1
        user = (value_int >> 2) & 1
        frame = (value_int >> 12) & 0xFFFFFFFFFF
        
        print(f"\nPT[{idx}] = 0x{value}")
        print(f"  Present: {present}")
        print(f"  Read/Write: {rw}")
        print(f"  User: {user}")
        print(f"  Frame: 0x{frame:X}")
        
        if not user:
            print(f"  ⚠️  警告: 用户位未设置！")
        else:
            print(f"  ✓ 用户位已设置")

def analyze_qemu_debug_log():
    """分析 QEMU 调试日志"""
    print("\n" + "=" * 60)
    print("分析 QEMU 调试日志...")
    print("=" * 60)
    
    log_file = Path("/tmp/qemu_debug.log")
    if not log_file.exists():
        print("调试日志文件不存在")
        return
    
    with open(log_file, 'r') as f:
        lines = f.readlines()
    
    print(f"日志文件共 {len(lines)} 行")
    
    int_pattern = r"check_exception old: (0x[0-9a-f]+) new (0x[0-9a-f]+)"
    int_matches = [line for line in lines if re.search(int_pattern, line)]
    
    if int_matches:
        print(f"\n找到 {len(int_matches)} 次异常:")
        for i, line in enumerate(int_matches[:5], 1):
            print(f"  {i}. {line.strip()}")

def main():
    """主函数"""
    print("QueenX 用户态进程启动诊断工具")
    print("=" * 60)
    
    output = run_qemu_with_debug()
    
    if not output:
        print("无法获取 QEMU 输出")
        return
    
    analyze_kernel_output(output)
    analyze_page_fault(output)
    check_page_table_permissions(output)
    analyze_qemu_debug_log()
    
    print("\n" + "=" * 60)
    print("诊断建议:")
    print("=" * 60)
    print("1. 检查用户页表是否正确映射了用户态代码")
    print("2. 确认页表项的用户位是否设置")
    print("3. 验证 GRUB 大页面是否正确拆分")
    print("4. 检查内核代码在用户页表中的映射")
    print("5. 确认段寄存器设置正确")

if __name__ == "__main__":
    main()
