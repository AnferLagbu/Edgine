#!/usr/bin/env python3
"""
DRIVER-2 QEMU virtio-vga 生产 kernel 集成测试 (Display 真机增强验证)

## 目标

与基础版 (`run_driver2_display_vga_test.py`) 互补, 用**生产 kernel** 跑
完整 `display_init → probe_vga_fb_via_pci → GfxConsole` 路径,
验证 framebuffer self-test (真图形渲染) 在 QEMU virtio-vga 下通过.

## 与基础版区别

| 维度 | 基础版 (vga) | 增强版 (production) |
|------|--------------|---------------------|
| ISO | antx_test.iso | **antx.iso** (生产 kernel) |
| Boot 路径 | kernel_test → test_runner_init | 完整 driver::init_all() |
| display_init | 不调用 | **调用** (PCI probe 路径) |
| framebuffer self_test | 不执行 | **执行 (ALL PASSED)** |
| 验证层级 | boot 回归 + 静态 | **真机初始化 + 图形渲染** |

## 测试步骤

1. 启动 QEMU + virtio-vga + 加载 antx.iso
2. 捕获 serial 输出
3. 验证关键标记 (全部应出现):
   - "[DISPLAY] display_init: probing framebuffer"
   - "[DISPLAY] no Multiboot2 framebuffer tag, falling back to PCI probe"
   - "VGA via PCI **:.*.* BAR0=0x*"  (PCI 探测成功)
   - "[DISPLAY] OK: 1024x768x32 @ 0x*"  (framebuffer 初始化)
   - "[DISPLAY] self-test: ALL PASSED"  (真图形渲染)
   - "[DISPLAY] GfxConsole initialized"  (图形控制台)
4. 验收: 所有 6 个标记出现 → 真机集成验证 PASS

## 前置条件

- antx.iso (生产 ISO, 非 test ISO) 在 other/build/ 目录
  → `make iso` (含完整 boot → driver::init_all)
- qemu-system-x86_64 ≥ 9.0
- virtio-vga 是 QEMU 内置设备, 不需要物理显卡

## 关联

- DRIVER-2 基础版: tests/integration/run_driver2_display_vga_test.py
- DRIVER-2 增强版 (本脚本): 生产 kernel 真机验证
- 本脚本 2026-06-25
"""

import re
import subprocess
import sys
import time
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent.parent
BUILD_DIR = PROJECT_ROOT / "other" / "build"
LOG_DIR = PROJECT_ROOT / "other" / "tests-reports" / "driver2_display_production"
ANTX_ISO = BUILD_DIR / "antx.iso"

QEMU_TIMEOUT_SEC = 15

# 关键 display_init 标记 (按出现顺序)
REQUIRED_DISPLAY_MARKERS = [
    ("display_init_called",      r"\[DISPLAY\]\s*display_init:\s*probing\s*framebuffer"),
    ("multiboot_fallback",       r"\[DISPLAY\]\s*no\s*Multiboot2\s*framebuffer"),
    ("pci_vga_probed",           r"VGA\s*via\s*PCI\s+[\d:.]+\s*BAR0=0x[\dA-Fa-f]+"),
    ("framebuffer_ok",           r"\[DISPLAY\]\s*OK:\s*\d+x\d+.*@.*0x[\dA-Fa-f]+"),
    ("self_test_passed",         r"\[DISPLAY\]\s*self-test:\s*ALL\s*PASSED"),
    ("gfx_console_init",         r"\[DISPLAY\]\s*GfxConsole\s*initialized"),
]


def run_qemu_with_virtio_vga(iso_path: Path, log_path: Path,
                              timeout: int = QEMU_TIMEOUT_SEC) -> int:
    """启动 QEMU + virtio-vga + 生产 antx.iso, 捕获 serial 输出."""
    cmd = [
        "qemu-system-x86_64",
        "-m", "512M",
        "-no-reboot",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-cdrom", str(iso_path),
        "-device", "virtio-vga",  # QEMU 内置 PCI VGA 设备
        "-serial", f"file:{log_path}",
        "-display", "none",
    ]
    print(f"[QEMU] 启动: {' '.join(cmd)}")
    try:
        result = subprocess.run(cmd, capture_output=True, timeout=timeout,
                                cwd=str(PROJECT_ROOT))
        return result.returncode
    except subprocess.TimeoutExpired:
        print(f"[QEMU] 达到 {timeout}s 超时, 强制终止")
        return -1


def analyze_display_init(log_path: Path) -> dict:
    """分析 display_init 路径日志."""
    if not log_path.exists():
        return {"valid": False, "reason": "no log file"}

    content = log_path.read_text(encoding="utf-8", errors="replace")

    # 检查每个必需标记
    found = {}
    for name, pattern in REQUIRED_DISPLAY_MARKERS:
        match = re.search(pattern, content)
        found[name] = match.group(0) if match else None

    # 提取关键参数
    fb_match = re.search(r"\[DISPLAY\]\s*OK:\s*(\d+)x(\d+)x(\d+)\s*@\s*(0x[\dA-Fa-f]+)", content)
    fb_params = {
        "width":  int(fb_match.group(1)) if fb_match else 0,
        "height": int(fb_match.group(2)) if fb_match else 0,
        "bpp":    int(fb_match.group(3)) if fb_match else 0,
        "addr":   fb_match.group(4) if fb_match else None,
    } if fb_match else {}

    bar_match = re.search(r"VGA\s*via\s*PCI\s+([\d:.]+)\s*BAR0=(0x[\dA-Fa-f]+)\s*size=(0x[\dA-Fa-f]+)", content)
    pci_params = {
        "bdf":      bar_match.group(1) if bar_match else None,
        "bar0":     bar_match.group(2) if bar_match else None,
        "bar_size": bar_match.group(3) if bar_match else None,
    } if bar_match else {}

    # 检查 boot panic
    boot_panics = re.findall(r"KERNEL PANIC[^\n]*", content)

    return {
        "valid": True,
        "found": found,
        "fb_params": fb_params,
        "pci_params": pci_params,
        "boot_panics": boot_panics,
        "log_size": len(content),
        "all_markers_found": all(v is not None for v in found.values()),
    }


def main() -> int:
    print("=" * 64)
    print("  DRIVER-2 QEMU virtio-vga 生产 kernel 集成测试")
    print("  Display 真机增强验证 (display_init + framebuffer self_test)")
    print("=" * 64)
    print()

    # 0. 前置检查
    if not ANTX_ISO.exists():
        print(f"[FAIL] antx.iso 不存在: {ANTX_ISO}")
        print(f"       请先执行 'make iso' 构建生产 ISO (含 driver::init_all)")
        return 1

    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / "serial.log"

    # 1. 启动 QEMU
    print("-" * 64)
    print("  QEMU + virtio-vga + antx.iso 启动")
    print("-" * 64)
    start_ts = time.time()
    rc = run_qemu_with_virtio_vga(ANTX_ISO, log_path)
    elapsed = time.time() - start_ts
    print(f"[QEMU] 退出码: {rc}, 耗时: {elapsed:.1f}s")
    print()

    # 2. 分析 display_init 路径
    analysis = analyze_display_init(log_path)
    if not analysis["valid"]:
        print(f"[FAIL] 日志分析失败: {analysis.get('reason')}")
        return 1

    print(f"[LOG] 日志大小: {analysis['log_size']} bytes")
    print()

    # 3. 验收 display_init 路径标记
    print("-" * 64)
    print("  display_init 路径标记验收")
    print("-" * 64)
    passed_count = 0
    for name, pattern in REQUIRED_DISPLAY_MARKERS:
        match = analysis["found"].get(name)
        status = "✓" if match else "✗"
        print(f"  [{status}] {name}: {match if match else 'NOT FOUND'}")
        if match:
            passed_count += 1
    print()
    print(f"  通过: {passed_count}/{len(REQUIRED_DISPLAY_MARKERS)}")
    print()

    # 4. 显示关键参数
    if analysis["fb_params"]:
        fb = analysis["fb_params"]
        print(f"  framebuffer 参数:")
        print(f"    分辨率: {fb['width']}x{fb['height']}x{fb['bpp']}")
        print(f"    物理地址: {fb['addr']}")
    if analysis["pci_params"]:
        pci = analysis["pci_params"]
        print(f"  PCI 设备参数:")
        print(f"    BDF: {pci['bdf']}")
        print(f"    BAR0: {pci['bar0']} (size {pci['bar_size']})")
    print()

    # 5. Boot panic 检查
    if analysis["boot_panics"]:
        print(f"  [FAIL] boot 阶段出现 {len(analysis['boot_panics'])} 个 KERNEL PANIC:")
        for p in analysis["boot_panics"][:3]:
            print(f"         {p}")
    else:
        print(f"  [PASS] boot 阶段无 KERNEL PANIC")
    print()

    # 6. 显示 display_init 完整日志片段
    if log_path.exists():
        content = log_path.read_text(encoding="utf-8", errors="replace")
        display_lines = [
            line for line in content.splitlines()
            if "DISPLAY" in line or "VGA via" in line
        ]
        if display_lines:
            print("-" * 64)
            print("  display_init 完整日志")
            print("-" * 64)
            for line in display_lines:
                print(f"  {line}")
            print()

    # 7. 结论
    print("=" * 64)
    all_passed = (
        analysis["all_markers_found"]
        and passed_count == len(REQUIRED_DISPLAY_MARKERS)
        and not analysis["boot_panics"]
    )

    if all_passed:
        print("  ✅ DRIVER-2 真机增强验证 PASS")
        print()
        print("  验证完成:")
        print("  ✓ display_init 调用 (boot 阶段 PCI probe 路径)")
        print("  ✓ virtio-vga PCI 设备发现")
        print("  ✓ framebuffer 初始化 (1024x768x32)")
        print("  ✓ framebuffer self-test ALL PASSED (真图形渲染)")
        print("  ✓ GfxConsole 启动 (图形控制台可用)")
        print()
        print("  DRIVER-2 (Display HDMI/DP) 真机集成验证完整收口.")
        print("=" * 64)
        return 0
    else:
        print("  ❌ DRIVER-2 真机增强验证 FAIL")
        print()
        print(f"  display_init 路径标记: {passed_count}/{len(REQUIRED_DISPLAY_MARKERS)}")
        if analysis["boot_panics"]:
            print(f"  KERNEL PANIC: {len(analysis['boot_panics'])}")
        print()
        print("  失败可能原因:")
        print("  - antx.iso 未包含 driver::init_all (用 make iso 重新构建)")
        print("  - QEMU virtio-vga 设备未正确连接")
        print("  - framebuffer self_test 渲染失败")
        print("=" * 64)
        return 1


if __name__ == "__main__":
    sys.exit(main())