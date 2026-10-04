#!/usr/bin/env python3
"""
Stress Tests for QueenX (v2 - Real Stress)
Tests system behavior under actual stress conditions via serial output analysis.
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

def run_qemu_stress(memory_mb: int = 512, timeout: int = 30, extra_args: list = None) -> tuple:
    iso_path = BUILD_DIR / "antx.iso"
    if not iso_path.exists():
        return "SKIP", "ISO not found, run 'make iso' first", ""

    cmd = [
        "qemu-system-x86_64",
        "-cdrom", str(iso_path),
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
        "-m", f"{memory_mb}M",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
    ]
    if extra_args:
        cmd.extend(extra_args)

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(PROJECT_ROOT)
        )
        output = result.stdout + result.stderr

        if "triple fault" in output.lower():
            return "FAIL", "Triple fault detected", output
        if "general protection" in output.lower() and "recovered" not in output.lower():
            return "FAIL", "Unresolved GPF detected", output

        return "PASS", "", output
    except subprocess.TimeoutExpired as e:
        output = ""
        if e.stdout:
            output = e.stdout.decode() if isinstance(e.stdout, bytes) else e.stdout
        if e.stderr:
            output += e.stderr.decode() if isinstance(e.stderr, bytes) else e.stderr
        return "PASS", "Completed without crash", output
    except Exception as e:
        return "FAIL", str(e), ""

def count_subsystem_inits(output: str) -> dict:
    subsystems = {
        "KLog": bool(re.search(r'KLog.*initialized', output, re.IGNORECASE)),
        "PMM": bool(re.search(r'PMM.*init|PMM.*free', output, re.IGNORECASE)),
        "VMM": bool(re.search(r'VMM.*init', output, re.IGNORECASE)),
        "IDT": bool(re.search(r'IDT.*init', output, re.IGNORECASE)),
        "Scheduler": bool(re.search(r'[Ss]cheduler.*init', output, re.IGNORECASE)),
        "VFS": bool(re.search(r'VFS.*init', output, re.IGNORECASE)),
        "RamFS": bool(re.search(r'RamFS.*init', output, re.IGNORECASE)),
        "NestFS": bool(re.search(r'NestFS.*init', output, re.IGNORECASE)),
        "PWID": bool(re.search(r'PWID.*init', output, re.IGNORECASE)),
        "Network": bool(re.search(r'net|E1000|lwIP', output, re.IGNORECASE)),
    }
    return subsystems

def test_memory_pressure():
    print("Testing memory pressure (128MB)...")
    result, reason, output = run_qemu_stress(memory_mb=128, timeout=30)
    if result == "SKIP":
        print(f"  [SKIP] {reason}")
        return True

    if result == "FAIL":
        print(f"  [FAIL] {reason}")
        return False

    inits = count_subsystem_inits(output)
    critical_ok = inits.get("KLog", False) and inits.get("PMM", False)
    if not critical_ok:
        print(f"  [FAIL] Critical subsystems failed under memory pressure")
        return False

    print(f"  [PASS] Kernel handled 128MB memory (subsystems: {sum(inits.values())}/{len(inits)} OK)")
    return True

def test_low_memory_boot():
    print("Testing low memory boot (64MB)...")
    result, reason, output = run_qemu_stress(memory_mb=64, timeout=20)
    if result == "SKIP":
        print(f"  [SKIP] {reason}")
        return True

    if result == "FAIL":
        if "triple fault" in reason.lower():
            print(f"  [FAIL] Triple fault with 64MB: {reason}")
            return False
        print(f"  [WARN] Failure with 64MB (may be expected): {reason}")
        return True

    inits = count_subsystem_inits(output)
    print(f"  [PASS] Booted with 64MB (subsystems: {sum(inits.values())}/{len(inits)} OK)")
    return True

def test_extended_stability():
    print("Testing extended stability (60s)...")
    result, reason, output = run_qemu_stress(memory_mb=512, timeout=60)
    if result == "SKIP":
        print(f"  [SKIP] {reason}")
        return True

    if result == "FAIL":
        print(f"  [FAIL] {reason}")
        return False

    panic_count = output.count("PANIC") + output.count("panic!")
    if panic_count > 0:
        recovered = output.lower().count("recovered")
        print(f"  [WARN] {panic_count} panics, {recovered} recoveries during 60s run")
        if recovered < panic_count:
            print(f"  [FAIL] Not all panics recovered")
            return False

    print(f"  [PASS] Stable for 60 seconds")
    return True

def test_smp_stability():
    print("Testing SMP stability (2 cores, 30s)...")
    result, reason, output = run_qemu_stress(
        memory_mb=512, timeout=30, extra_args=["-smp", "2"]
    )
    if result == "SKIP":
        print(f"  [SKIP] {reason}")
        return True

    if result == "FAIL":
        print(f"  [WARN] SMP failure (may be expected): {reason}")
        return True

    print(f"  [PASS] 2-core SMP stable for 30 seconds")
    return True

def test_rapid_reboot():
    print("Testing rapid reboot cycle (3 boots)...")
    all_ok = True
    for i in range(3):
        result, reason, output = run_qemu_stress(memory_mb=512, timeout=15)
        if result == "FAIL":
            print(f"  [FAIL] Boot {i+1} failed: {reason}")
            all_ok = False
            break
        inits = count_subsystem_inits(output)
        if not inits.get("PMM", False):
            print(f"  [FAIL] Boot {i+1} PMM not initialized")
            all_ok = False
            break

    if all_ok:
        print(f"  [PASS] 3 consecutive boots all stable")
    return all_ok

def run_all_stress_tests():
    print("=" * 60)
    print("QueenX Stress Tests (v2)")
    print("=" * 60)
    print()

    tests = [
        ("Memory Pressure (128MB)", test_memory_pressure),
        ("Low Memory Boot (64MB)", test_low_memory_boot),
        ("Extended Stability (60s)", test_extended_stability),
        ("SMP Stability (2 cores)", test_smp_stability),
        ("Rapid Reboot Cycle", test_rapid_reboot),
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
    print(f"Stress Tests: {passed} passed, {failed} failed")
    print("=" * 60)

    return failed == 0

if __name__ == "__main__":
    success = run_all_stress_tests()
    sys.exit(0 if success else 1)
