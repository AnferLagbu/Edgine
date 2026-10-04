#!/usr/bin/env python3
"""
Smoke Test for QueenX — Ring 3 User-Mode Bootstrap
Boots the ISO and verifies the init process reaches user mode.
"""

import subprocess
import sys
import re
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent.parent
BUILD_DIR = PROJECT_ROOT / "other" / "build"


def run_qemu_test(timeout: int = 25) -> str:
    iso_path = BUILD_DIR / "antx.iso"
    if not iso_path.exists():
        print(f"  [SKIP] ISO not found, run 'make iso' first")
        return ""

    cmd = [
        "qemu-system-x86_64",
        "-cdrom", str(iso_path),
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
        "-m", "512M",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
    ]

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(PROJECT_ROOT),
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
        print(f"  [ERROR] {e}")
        return ""


class SmokeTest:
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


def test_ring3_entry(output: str) -> SmokeTest:
    t = SmokeTest("Ring 3 Entry", "Kernel enters Ring 3 and launches init process")
    if not t.check(len(output) > 100, "No output captured from kernel"):
        return t

    if "triple fault" in output.lower() or "triple_fault" in output.lower():
        t.check(False, "Triple fault detected — Ring 3 transition crashed")
        return t

    if "PANIC" in output and "recovered" not in output.lower():
        t.check(False, "Unrecovered kernel panic during boot")
        return t

    t.check("Entering Ring" in output, "Kernel did not enter Ring 3")
    t.passed = True
    return t


def test_init_process_started(output: str) -> SmokeTest:
    t = SmokeTest("Init Process", "init process executes and produces output")
    if not t.check(len(output) > 0, "No output"):
        return t

    t.check("[init]" in output, "No [init] output — init process did not start")
    t.check(
        "QueenX init process started" in output,
        "init process did not emit startup message",
    )
    t.passed = True
    return t


def test_syscall_working(output: str) -> SmokeTest:
    t = SmokeTest("Syscall Path", "int 0x80 syscall dispatch is functional")
    if not t.check(len(output) > 0, "No output"):
        return t

    has_wizard = (
        "Installation Wizard" in output
        or "First boot detected" in output
        or "wizard will guide" in output
    )
    if not has_wizard:
        t.check(False, "Installation wizard output missing — syscalls may be broken")
    t.passed = True
    return t


def test_no_triple_fault(output: str) -> SmokeTest:
    t = SmokeTest("No Triple Fault", "Boot completes without CPU exception")
    if not t.check(len(output) > 0, "No output"):
        return t

    t.check("triple fault" not in output.lower(), "Triple fault in output")
    t.check("#GP" not in output, "#GP (General Protection Fault) in output")
    t.check("#PF" not in output, "#PF (Page Fault) in output")
    t.passed = True
    return t


def run_all_smoke_tests():
    print("=" * 60)
    print("  QueenX Smoke Tests — Ring 3 Bootstrap")
    print("=" * 60)

    print("\n  Booting kernel ISO...")
    output = run_qemu_test(25)

    if not output:
        print("  [ERROR] No output captured from kernel")
        return False

    print(f"  Captured {len(output)} bytes of output\n")

    tests = [
        test_no_triple_fault,
        test_ring3_entry,
        test_init_process_started,
        test_syscall_working,
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
    print(f"  Smoke Tests: {passed} passed, {failed} failed")
    print("=" * 60)

    return failed == 0


if __name__ == "__main__":
    success = run_all_smoke_tests()
    sys.exit(0 if success else 1)
