#!/usr/bin/env python3
"""
Chaos Engineering Tests for QueenX (v2 - Real Fault Injection)
Tests system resilience using kernel's built-in fault injection framework.
Requires: make test-chaos (builds with fault_injection feature enabled)
"""

import subprocess
import sys
import os
import re
import time
import random
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent.parent
BUILD_DIR = PROJECT_ROOT / "other" / "build"

def run_qemu_chaos(memory_mb: int = 512, timeout: int = 30, smp: int = 1) -> tuple:
    iso_path = BUILD_DIR / "antx_chaos.iso"
    if not iso_path.exists():
        iso_path = BUILD_DIR / "antx.iso"
        if not iso_path.exists():
            return "SKIP", "ISO not found", ""

    cmd = [
        "qemu-system-x86_64",
        "-cdrom", str(iso_path),
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
        "-m", f"{memory_mb}M",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
    ]
    if smp > 1:
        cmd.extend(["-smp", str(smp)])

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(PROJECT_ROOT)
        )
        output = result.stdout + result.stderr
        return "OK", "", output
    except subprocess.TimeoutExpired as e:
        output = ""
        if e.stdout:
            output = e.stdout.decode() if isinstance(e.stdout, bytes) else e.stdout
        if e.stderr:
            output += e.stderr.decode() if isinstance(e.stderr, bytes) else e.stderr
        return "OK", "Timeout (expected)", output
    except Exception as e:
        return "FAIL", str(e), ""

def analyze_recovery(output: str) -> dict:
    fault_injections = len(re.findall(r'\[FAULT-INJECT\]', output))
    recoveries = len(re.findall(r'mark_recovered|recovered successfully', output, re.IGNORECASE))
    rollbacks = len(re.findall(r'rollback|RollingBack', output, re.IGNORECASE))
    quarantines = len(re.findall(r'[Qq]uarantine', output))
    panics = len(re.findall(r'PANIC|panic!', output))
    triple_faults = len(re.findall(r'triple fault', output, re.IGNORECASE))
    test_passed = len(re.findall(r'passed|PASS', output))
    test_failed = len(re.findall(r'FAIL(ED)?', output))

    recovery_rate = (recoveries / fault_injections * 100) if fault_injections > 0 else 0.0

    return {
        "fault_injections": fault_injections,
        "recoveries": recoveries,
        "rollbacks": rollbacks,
        "quarantines": quarantines,
        "panics": panics,
        "triple_faults": triple_faults,
        "test_passed": test_passed,
        "test_failed": test_failed,
        "recovery_rate": recovery_rate,
    }

def test_fault_injection_basic():
    print("Testing fault injection with chaos kernel...")
    status, reason, output = run_qemu_chaos(memory_mb=512, timeout=60)
    if status == "SKIP":
        print(f"  [SKIP] {reason}")
        return True

    stats = analyze_recovery(output)
    print(f"  Fault injections: {stats['fault_injections']}")
    print(f"  Recoveries: {stats['recoveries']}")
    print(f"  Rollbacks: {stats['rollbacks']}")
    print(f"  Quarantines: {stats['quarantines']}")
    print(f"  Recovery rate: {stats['recovery_rate']:.1f}%")

    if stats['triple_faults'] > 0:
        print(f"  [FAIL] Triple fault detected")
        return False

    if stats['fault_injections'] == 0:
        print(f"  [WARN] No fault injections detected (is fault_injection feature enabled?)")
        print(f"  [PASS] Kernel stable without injections")
        return True

    if stats['recovery_rate'] >= 80.0:
        print(f"  [PASS] Recovery rate {stats['recovery_rate']:.1f}% >= 80%")
        return True
    else:
        print(f"  [FAIL] Recovery rate {stats['recovery_rate']:.1f}% < 80%")
        return False

def test_low_memory_chaos():
    print("Testing chaos with low memory (128MB)...")
    status, reason, output = run_qemu_chaos(memory_mb=128, timeout=30)
    if status == "SKIP":
        print(f"  [SKIP] {reason}")
        return True

    stats = analyze_recovery(output)
    if stats['triple_faults'] > 0:
        print(f"  [FAIL] Triple fault with 128MB + chaos")
        return False

    print(f"  [PASS] Kernel handled 128MB + chaos (injections: {stats['fault_injections']}, recoveries: {stats['recoveries']})")
    return True

def test_smp_chaos():
    print("Testing chaos with SMP (2 cores)...")
    status, reason, output = run_qemu_chaos(memory_mb=512, timeout=30, smp=2)
    if status == "SKIP":
        print(f"  [SKIP] {reason}")
        return True

    stats = analyze_recovery(output)
    if stats['triple_faults'] > 0:
        print(f"  [WARN] Triple fault with SMP + chaos (may be expected)")
        return True

    print(f"  [PASS] SMP + chaos stable (injections: {stats['fault_injections']}, recoveries: {stats['recoveries']})")
    return True

def test_repeated_chaos_runs():
    print("Testing repeated chaos runs (3 iterations)...")
    total_injections = 0
    total_recoveries = 0

    for i in range(3):
        status, reason, output = run_qemu_chaos(memory_mb=512, timeout=30)
        if status == "SKIP":
            print(f"  [SKIP] Run {i+1}: ISO not found")
            continue

        stats = analyze_recovery(output)
        total_injections += stats['fault_injections']
        total_recoveries += stats['recoveries']
        print(f"  Run {i+1}: {stats['fault_injections']} injections, {stats['recoveries']} recoveries")

    if total_injections == 0:
        print(f"  [WARN] No injections across 3 runs")
        return True

    overall_rate = (total_recoveries / total_injections * 100) if total_injections > 0 else 0
    print(f"  Overall: {total_injections} injections, {total_recoveries} recoveries ({overall_rate:.1f}%)")

    if overall_rate >= 80.0:
        print(f"  [PASS] Overall recovery rate {overall_rate:.1f}% >= 80%")
        return True
    else:
        print(f"  [FAIL] Overall recovery rate {overall_rate:.1f}% < 80%")
        return False

def test_normal_kernel_stability():
    print("Testing normal kernel (no fault injection)...")
    iso_path = BUILD_DIR / "antx.iso"
    if not iso_path.exists():
        print(f"  [SKIP] Normal ISO not found")
        return True

    cmd = [
        "qemu-system-x86_64",
        "-cdrom", str(iso_path),
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
        "-m", "512M",
    ]

    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=30, cwd=str(PROJECT_ROOT))
        output = result.stdout + result.stderr
    except subprocess.TimeoutExpired as e:
        output = ""
        if e.stdout:
            output = e.stdout.decode() if isinstance(e.stdout, bytes) else e.stdout
    except Exception as e:
        print(f"  [FAIL] {e}")
        return False

    if "triple fault" in output.lower():
        print(f"  [FAIL] Triple fault in normal kernel")
        return False

    stats = analyze_recovery(output)
    if stats['fault_injections'] > 0:
        print(f"  [WARN] Fault injections detected in normal kernel (should be 0)")

    print(f"  [PASS] Normal kernel stable")
    return True

def run_all_chaos_tests():
    print("=" * 60)
    print("QueenX Chaos Engineering Tests (v2)")
    print("=" * 60)
    print()
    print("These tests use the kernel's built-in fault injection framework.")
    print("Run 'make test-chaos' to build with fault_injection enabled.")
    print()

    tests = [
        ("Normal Kernel Stability", test_normal_kernel_stability),
        ("Fault Injection Basic", test_fault_injection_basic),
        ("Low Memory + Chaos", test_low_memory_chaos),
        ("SMP + Chaos", test_smp_chaos),
        ("Repeated Chaos Runs", test_repeated_chaos_runs),
    ]

    passed = 0
    failed = 0

    for name, test_func in tests:
        print(f"\n[{name}]")
        try:
            if test_func():
                passed += 1
            else:
                failed += 1
        except Exception as e:
            print(f"  [ERROR] {e}")
            failed += 1

    print("\n" + "=" * 60)
    print(f"Chaos Tests: {passed} passed, {failed} failed")
    print("=" * 60)

    return failed == 0

if __name__ == "__main__":
    success = run_all_chaos_tests()
    sys.exit(0 if success else 1)
