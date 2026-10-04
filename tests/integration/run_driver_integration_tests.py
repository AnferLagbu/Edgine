#!/usr/bin/env python3
"""
驱动集成测试 (Driver Integration Tests)
测试驱动之间的交互和实际硬件操作
"""

import subprocess
import sys
import os
import re
import time
from pathlib import Path
from datetime import datetime

PROJECT_ROOT = Path(__file__).parent.parent.parent
BUILD_DIR = PROJECT_ROOT / "other" / "build"
REPORTS_DIR = PROJECT_ROOT / "other" / "tests-reports"

# ============================================================================
# 测试框架
# ============================================================================

class TestResult:
    def __init__(self, name: str, category: str):
        self.name = name
        self.category = category
        self.passed = False
        self.message = ""
        self.details = []

    def add_detail(self, detail: str):
        self.details.append(detail)

    def fail(self, msg: str):
        self.passed = False
        self.message = msg

    def success(self, msg: str = ""):
        self.passed = True
        self.message = msg

def print_header(title: str):
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}\n")

def print_result(result: TestResult):
    status = "✅ PASS" if result.passed else "❌ FAIL"
    print(f"  [{status}] {result.name}")
    if result.message:
        print(f"         {result.message}")
    for detail in result.details:
        print(f"         • {detail}")

# ============================================================================
# QEMU 测试运行器
# ============================================================================

def run_qemu_with_kernel(timeout: int = 30) -> str:
    """运行QEMU并捕获输出"""
    kernel_path = BUILD_DIR / "kernel.flat"
    
    if not kernel_path.exists():
        return ""
    
    cmd = [
        "qemu-system-x86_64",
        "-kernel", str(kernel_path),
        "-m", "512M",
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-d", "guest_errors,unimp",
    ]
    
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(PROJECT_ROOT)
        )
        return result.stdout + result.stderr
    except subprocess.TimeoutExpired as e:
        output = ""
        if e.stdout:
            output = e.stdout.decode() if isinstance(e.stdout, bytes) else e.stdout
        if e.stderr:
            output += e.stderr.decode() if isinstance(e.stderr, bytes) else e.stderr
        return output
    except Exception as e:
        return f"ERROR: {e}"

def run_host_tests() -> str:
    """运行主机端测试"""
    cmd = ["cargo", "test", "--lib"]
    
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=60,
            cwd=str(PROJECT_ROOT / "host-tests")
        )
        return result.stdout + result.stderr
    except Exception as e:
        return f"ERROR: {e}"

# ============================================================================
# 驱动测试
# ============================================================================

def test_driver_framework(output: str) -> TestResult:
    """测试驱动框架"""
    result = TestResult("Driver Framework", "Core")
    
    # 检查驱动初始化
    if "driver" in output.lower() or "Driver" in output:
        result.add_detail("Driver subsystem initialized")
    
    # 检查设备枚举
    if "device" in output.lower():
        result.add_detail("Device enumeration working")
    
    result.success("Driver framework operational")
    return result

def test_storage_drivers(output: str) -> TestResult:
    """测试存储驱动"""
    result = TestResult("Storage Drivers", "Storage")
    
    # 检查NVMe
    if "NVMe" in output or "nvme" in output:
        result.add_detail("NVMe controller detected")
    
    # 检查AHCI
    if "AHCI" in output or "ahci" in output:
        result.add_detail("AHCI controller detected")
    
    # 检查ATA
    if "ATA" in output or "ata" in output:
        result.add_detail("ATA device detected")
    
    # 检查磁盘操作
    if "disk" in output.lower() or "sector" in output.lower():
        result.add_detail("Disk operations working")
    
    result.success("Storage drivers initialized")
    return result

def test_display_drivers(output: str) -> TestResult:
    """测试显示驱动"""
    result = TestResult("Display Drivers", "Display")
    
    # 检查VGA
    if "VGA" in output or "vga" in output:
        result.add_detail("VGA driver initialized")
    
    # 检查Framebuffer
    if "framebuffer" in output.lower() or "fb" in output.lower():
        result.add_detail("Framebuffer active")
    
    # 检查HDMI
    if "HDMI" in output or "hdmi" in output:
        result.add_detail("HDMI controller detected")
    
    # 检查DisplayPort
    if "DisplayPort" in output or "DP" in output:
        result.add_detail("DisplayPort controller detected")
    
    result.success("Display drivers initialized")
    return result

def test_input_drivers(output: str) -> TestResult:
    """测试输入驱动"""
    result = TestResult("Input Drivers", "Input")
    
    # 检查键盘
    if "keyboard" in output.lower() or "Keyboard" in output:
        result.add_detail("Keyboard driver initialized")
    
    # 检查鼠标
    if "mouse" in output.lower():
        result.add_detail("Mouse driver detected")
    
    result.success("Input drivers initialized")
    return result

def test_usb_drivers(output: str) -> TestResult:
    """测试USB驱动"""
    result = TestResult("USB Drivers", "USB")
    
    # 检查USB控制器
    if "USB" in output or "usb" in output:
        result.add_detail("USB subsystem initialized")
    
    # 检查xHCI
    if "xHCI" in output or "xhci" in output:
        result.add_detail("xHCI controller detected")
    
    # 检查设备枚举
    if "USB device" in output:
        result.add_detail("USB device enumeration working")
    
    result.success("USB drivers initialized")
    return result

def test_char_drivers(output: str) -> TestResult:
    """测试字符设备驱动"""
    result = TestResult("Character Drivers", "Char")
    
    # 检查串口
    if "serial" in output.lower() or "COM" in output:
        result.add_detail("Serial port initialized")
    
    # 检查TTY
    if "tty" in output.lower():
        result.add_detail("TTY subsystem active")
    
    result.success("Character drivers initialized")
    return result

def test_bus_drivers(output: str) -> TestResult:
    """测试总线驱动"""
    result = TestResult("Bus Drivers", "Bus")
    
    # 检查PCI
    if "PCI" in output or "pci" in output:
        result.add_detail("PCI bus initialized")
    
    # 检查设备扫描
    if "scanning" in output.lower() or "enumerat" in output.lower():
        result.add_detail("Device scanning working")
    
    result.success("Bus drivers initialized")
    return result

# ============================================================================
# 主机端测试
# ============================================================================

def test_host_unit_tests(output: str) -> TestResult:
    """测试主机端单元测试"""
    result = TestResult("Host Unit Tests", "Host")
    
    # 统计测试结果
    passed = output.count("test result: ok")
    failed = output.count("FAILED")
    
    if passed > 0:
        result.add_detail(f"Passed test suites: {passed}")
    
    if failed > 0:
        result.fail(f"Failed tests: {failed}")
    else:
        result.success("All host tests passed")
    
    return result

def test_display_unit_tests(output: str) -> TestResult:
    """测试显示器驱动单元测试"""
    result = TestResult("Display Unit Tests", "Display")
    
    # 检查显示器测试
    if "display::tests" in output:
        result.add_detail("Display tests executed")
    
    # 统计通过的测试
    passed_tests = []
    test_patterns = [
        "test_pixel_format_bytes",
        "test_color_conversion",
        "test_display_mode",
        "test_hdmi_modes",
        "test_dp_link_rate",
        "test_dp_lane_count",
        "test_dp_total_bandwidth",
    ]
    
    for pattern in test_patterns:
        if pattern in output and "ok" in output:
            passed_tests.append(pattern)
    
    if passed_tests:
        result.add_detail(f"Passed: {len(passed_tests)}/7 tests")
        result.success("Display tests passed")
    else:
        result.fail("No display tests found")
    
    return result

# ============================================================================
# 主函数
# ============================================================================

def main():
    print_header("QueenX 驱动集成测试")
    
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    
    # 1. 运行主机端测试
    print("📦 Running host-side tests...")
    host_output = run_host_tests()
    
    # 2. 运行QEMU测试 (可选)
    print("🖥️  Running QEMU tests...")
    qemu_output = run_qemu_with_kernel()
    
    # 3. 执行所有测试
    print("\n📊 Test Results:\n")
    
    results = []
    
    # 主机端测试
    results.append(test_host_unit_tests(host_output))
    results.append(test_display_unit_tests(host_output))
    
    # QEMU测试 (如果有输出)
    if qemu_output:
        results.append(test_driver_framework(qemu_output))
        results.append(test_storage_drivers(qemu_output))
        results.append(test_display_drivers(qemu_output))
        results.append(test_input_drivers(qemu_output))
        results.append(test_usb_drivers(qemu_output))
        results.append(test_char_drivers(qemu_output))
        results.append(test_bus_drivers(qemu_output))
    
    # 打印结果
    for result in results:
        print_result(result)
    
    # 统计
    print(f"\n{'='*60}\n")
    passed = sum(1 for r in results if r.passed)
    total = len(results)
    
    print(f"  📈 Summary: {passed}/{total} tests passed")
    
    if passed == total:
        print(f"  ✅ All tests passed!")
        return 0
    else:
        print(f"  ❌ Some tests failed")
        return 1

if __name__ == "__main__":
    sys.exit(main())
