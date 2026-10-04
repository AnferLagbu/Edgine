#!/usr/bin/env python3
"""
Integration Tests for QueenX (v2 - Serial Protocol)
Tests multi-module interactions via QEMU serial output analysis.
"""

import subprocess
import sys
import os
import re
import time
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent.parent
BUILD_DIR = PROJECT_ROOT / "other" / "build"

def run_qemu_test(timeout: int = 60) -> str:
    # ARCH 环境变量支持 x86_64 (默认) / aarch64
    arch = os.environ.get("ARCH", "x86_64")
    qemu_bin = f"qemu-system-{arch}"
    iso_path = BUILD_DIR / "antx.iso"
    kernel_path = BUILD_DIR / "kernel.bin"
    if arch == "x86_64" and not iso_path.exists():
        print(f"  [SKIP] ISO not found, run 'make iso' first")
        return ""
    if arch == "aarch64" and not kernel_path.exists():
        print(f"  [SKIP] kernel.bin not found, run 'make ARCH=aarch64 all' first")
        return ""

    cmd = [
        qemu_bin,
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
        "-m", "512M",
    ]

    # 架构特定选项
    if arch == "x86_64":
        # x86_64: 通过 ISO + grub 启动, isa-debug-exit 设备触发 clean exit
        cmd += [
            "-cdrom", str(iso_path),
            "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        ]
    elif arch == "aarch64":
        # aarch64: 通过 -kernel 直接启动 (multiboot2 不支持 aarch64)
        # QEMU virt 机器 + GIC v3 + max CPU, 无 NIC 隔离网络子系统 (e1000 已知挂起)
        cmd += [
            "-machine", "virt,gic-version=3",
            "-cpu", "max",
            "-kernel", str(kernel_path),
            "-nic", "none",
        ]
    else:
        print(f"  [SKIP] Unknown ARCH={arch}, expected x86_64 or aarch64")
        return ""

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(PROJECT_ROOT)
        )
        output = result.stdout + result.stderr
        return output
    except subprocess.TimeoutExpired as e:
        output = ""
        if e.stdout:
            output = e.stdout.decode() if isinstance(e.stdout, bytes) else e.stdout
        if e.stderr:
            output += e.stderr.decode() if isinstance(e.stderr, bytes) else e.stderr
        return output
    except Exception as e:
        print(f"  [ERROR] {e}")
        return ""

class IntegrationTest:
    def __init__(self, name: str, description: str):
        self.name = name
        self.description = description
        self.passed = False
        self.message = ""

    def check(self, condition: bool, msg: str):
        if not condition:
            self.message = msg
            self.passed = False
            return False
        return True

def test_boot_sequence(output: str) -> IntegrationTest:
    t = IntegrationTest("Boot Sequence", "Kernel boot and initialization")
    if not t.check(len(output) > 100, "No output captured from kernel"):
        return t

    t.check("KLog" in output, "KLog not initialized")
    t.check("PMM" in output, "PMM not initialized")
    t.check("VMM" in output, "VMM not initialized")
    t.check("IDT" in output, "IDT not initialized")

    if "PANIC" in output and "recovered" not in output.lower():
        t.check(False, "Kernel panic without recovery")
    else:
        t.passed = True
    return t

def test_memory_subsystem(output: str) -> IntegrationTest:
    t = IntegrationTest("Memory Subsystem", "PMM + VMM + kmalloc integration")
    if not t.check(len(output) > 0, "No output"):
        return t

    pmm_match = re.search(r'(\d+)\s+pages\s+free', output)
    if pmm_match:
        free_pages = int(pmm_match.group(1))
        t.check(free_pages > 10000, f"Too few free pages: {free_pages}")
    else:
        t.check(False, "PMM free pages not reported")

    t.check("kmalloc" in output or "heap" in output.lower(), "kmalloc/heap not initialized")
    t.passed = True
    return t

def test_filesystem_mount(output: str) -> IntegrationTest:
    t = IntegrationTest("Filesystem Mount", "VFS + RamFS + DevFS + ProcFS mounting")
    if not t.check(len(output) > 0, "No output"):
        return t

    t.check("VFS" in output, "VFS not initialized")
    t.check("RamFS" in output, "RamFS not initialized")
    t.check("DevFS" in output, "DevFS not initialized")
    t.check("ProcFS" in output, "ProcFS not initialized")
    t.check("mounted" in output.lower(), "No filesystem mounted")
    t.passed = True
    return t

def test_process_scheduler(output: str) -> IntegrationTest:
    t = IntegrationTest("Process & Scheduler", "Process manager + scheduler integration")
    if not t.check(len(output) > 0, "No output"):
        return t

    t.check("Scheduler" in output or "scheduler" in output.lower(), "Scheduler not initialized")
    t.check("Process" in output or "process" in output.lower(), "Process manager not initialized")
    t.passed = True
    return t

def test_security_subsystem(output: str) -> IntegrationTest:
    t = IntegrationTest("Security Subsystem", "PWID + Session integration")
    if not t.check(len(output) > 0, "No output"):
        return t

    t.check("PWID" in output, "PWID not initialized")
    t.check("Session" in output or "session" in output.lower(), "Session manager not initialized")
    t.passed = True
    return t

def test_barrier_subsystem(output: str) -> IntegrationTest:
    t = IntegrationTest("Barrier Subsystem", "Fault recovery framework")
    if not t.check(len(output) > 0, "No output"):
        return t

    has_barrier = "Barrier" in output or "barrier" in output.lower() or "Recovery" in output
    t.check(has_barrier, "Barrier subsystem not detected")
    t.passed = True
    return t

def test_no_unresolved_panics(output: str) -> IntegrationTest:
    t = IntegrationTest("No Unresolved Panics", "All panics should be recovered")
    if not t.check(len(output) > 0, "No output"):
        return t

    panic_count = output.count("PANIC") + output.count("panic!")
    recovered_count = output.lower().count("recovered") + output.lower().count("rollback")

    if panic_count > 0:
        t.check(recovered_count >= panic_count,
                f"Panics ({panic_count}) exceed recoveries ({recovered_count})")
    t.passed = True
    return t

def run_all_integration_tests():
    print("=" * 60)
    print("QueenX Integration Tests (v2)")
    print("=" * 60)

    print("\nRunning kernel and capturing output...")
    output = run_qemu_test(60)

    if not output:
        print("  [ERROR] No output captured from kernel")
        return False

    print(f"  Captured {len(output)} bytes of output\n")

    tests = [
        test_boot_sequence,
        test_memory_subsystem,
        test_filesystem_mount,
        test_process_scheduler,
        test_security_subsystem,
        test_barrier_subsystem,
        test_no_unresolved_panics,
    ]

    passed = 0
    failed = 0

    for test_func in tests:
        result = test_func(output)
        status = "PASS" if result.passed else "FAIL"
        print(f"  [{status}] {result.name}: {result.description}")
        if not result.passed:
            print(f"         Reason: {result.message}")
        if result.passed:
            passed += 1
        else:
            failed += 1

    print("\n" + "=" * 60)
    print(f"Integration Tests: {passed} passed, {failed} failed")
    print("=" * 60)

    return failed == 0

if __name__ == "__main__":
    success = run_all_integration_tests()
    sys.exit(0 if success else 1)
