#!/usr/bin/env python3
"""
audit_static_mut.py — framework 层 static mut 使用检查 (2026-07-03 新增)

services 层有 ci_check_services_unsafe.py 检查, 但 framework 层无对应脚本.
static mut 在 no_std 内核中可能导致数据竞争, 需定期审查使用点.

用法: python3 scripts/audit_static_mut.py
退出码: 0=通过 (或所有使用已豁免), 1=有违规
"""
import re
import sys
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src/kernel/framework")

# 已知安全的 static mut 使用 (框架基础设施, 有外部锁保护或初始化后独占)
# B01-17 修复: 改为精确匹配 (而非子串). 仅列出真正可能出现的 static mut 名名,
# 移除"范围/唯一所有者/变量在/访问"等中文注释词 (不应作为豁免匹配).
SAFE_PATTERNS = [
    "GLOBAL_PMM",        # OnceLock 保护
    "GLOBAL_KMALLOC",    # 内部锁
    "PER_CPU_GDT",       # 初始化期间独占
    "GRANT_RECORDS",     # GrateLock 保护
    "GLOBAL_TABLE",      # OnceLock 保护
    "GLOBAL_IPC",        # RacyCell 保护
    "GLOBAL_IDENTITY",   # OnceLock 保护
    "NOTIFY_DONATION",   # AtomicPtr, 单写多读
    "NOTIFY_REVOKE",     # AtomicPtr, 单写多读
    "PROCESS_CLEANUP_FN", # AtomicPtr
    "SOFTIRQ",           # IRQ 串行化
    "EARLY_ALLOC",       # 初始化期间
    "L0_TABLE",          # MMU 初始化独占
    "L1_IDMAP",          # MMU 初始化独占
    "L2_DEVICE",         # MMU 初始化独占
    "TTBR1",             # MMU 初始化独占
    "TTBR1_L1",          # MMU 初始化独占 (L1 页表)
    "AP_PER_CPU",        # SMP 初始化独占
    "SERIAL_PORTS",      # 初始化独占
    "VGA_DRIVER",        # 初始化独占
    "GLOBAL_FRAMEBUFFER", # 初始化独占
    "GLOBAL_AUDIT",      # 初始化独占
    "GLOBAL_DMA",        # DMA engine 保护
    "CPU_INFO",          # 初始化独占
    "DmaEngine",         # DMA 保护
    "NET_SNAPSHOT",      # 网络快照
    "LOG_SINKS",         # 日志初始化
    "SLAB_CACHES",       # slab 初始化
    "SLAB_INITIALIZED",  # slab 初始化
    "GENERAL_CACHES",    # slab 初始化
    "SLEEP_FLAG",        # 定时器
    "KB_READ_SLOT",      # 键盘初始化
    "KALLOC_BUF",        # e1000 驱动初始化
    "KALLOC_OFF",        # e1000 驱动初始化
    "CURRENT_MM",        # VMA 初始化
    "VIRTIO_BLK_REGISTRY", # VirtIO 注册
    "NET_DEVICE",        # 网络设备注册
    "NET_STACK",         # 网络栈
    "NET_STACK_TRAIT",   # 网络栈 trait
    "SOCKET_STORAGE",    # 套接字存储
    "SOCKET_SET",        # 套接字集合
    "SOCKET_TABLE",      # 套接字表
    "FD_TYPES",          # 文件描述符类型
    "DHCP_HANDLE",       # DHCP 句柄
    "PREV_DHCP_STATE",   # DHCP 状态
    "ISR_TABLE",         # IRQ 表初始化期间独占
    "__bss_start",       # 链接器符号, 非实际 static mut
    "TCP_RX_BUFS",       # smoltcp 缓冲池
    "TCP_TX_BUFS",       # smoltcp 缓冲池
    "UDP_RX_BUFS",       # smoltcp 缓冲池
    "UDP_TX_BUFS",       # smoltcp 缓冲池
    "UDP_RX_METAS",      # smoltcp 缓冲池
    "UDP_TX_METAS",      # smoltcp 缓冲池
    "MockBlockDevice",   # 测试 mock
    "TEST_TIMER",        # hrtimer 测试专用
    "T1",                # hrtimer 测试专用
    "T2",                # hrtimer 测试专用
]

def main():
    violations = []
    # 扫描 framework 层所有 .rs 文件
    for root, dirs, files in os.walk(SRC):
        for fname in files:
            if not fname.endswith('.rs'):
                continue
            fpath = os.path.join(root, fname)
            rel_path = os.path.relpath(fpath, ROOT)
            with open(fpath, 'r', encoding='utf-8', errors='replace') as f:
                content = f.read()
            # 搜索 static mut 声明 (排除注释中的 static mut 文本)
            # B01-17 修复: 支持 `pub(crate) static mut` / `pub static mut` 形式,
            # 且 SAFE_PATTERNS 改为精确匹配 (避免子串误匹配)
            for m in re.finditer(r'^\s*(pub(?:\([^)]*\))?\s+)?static\s+mut\s+(\w+)', content, re.MULTILINE):
                name = m.group(2)
                line_no = content[:m.start()].count('\n') + 1
                # 检查是否在已知安全列表中 (精确匹配, 而非子串)
                is_safe = name in SAFE_PATTERNS
                if not is_safe:
                    violations.append((rel_path, line_no, name))

    print(f"=== audit_static_mut: 扫描 framework 层 static mut ===")
    if violations:
        print(f"  ✗ {len(violations)} 处未豁免的 static mut:")
        for path, line, name in violations:
            print(f"    ✗ {path}:{line} — {name}")
        print("\n⚠ 存在数据竞争风险")
        sys.exit(1)
    else:
        print("✓ audit_static_mut 通过 (所有 static mut 已豁免)")
        sys.exit(0)

if __name__ == "__main__":
    main()
