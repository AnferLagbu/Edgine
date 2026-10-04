#!/usr/bin/env python3
"""
QueenX QEMU Diagnostic Tool
===========================

Quick diagnosis of common QEMU and kernel boot issues.

Usage:
    python3 diagnose_qemu.py              # Run all checks
    python3 diagnose_qemu.py --check-env   # Check environment only
    python3 diagnose_qemu.py --test-boot   # Test kernel boot
"""

import subprocess
import sys
import os
import shutil
from pathlib import Path
from typing import Dict, Tuple

PROJECT_ROOT = Path(__file__).parent.parent
KERNEL_BIN = PROJECT_ROOT / "other" / "build" / "kernel.bin"
KERNEL_TEST_BIN = PROJECT_ROOT / "other" / "build" / "kernel_test.bin"


def print_header(title: str):
    """Print formatted header"""
    print(f"\n{'='*70}")
    print(f" {title}")
    print(f"{'='*70}\n")


def check_command(cmd: str) -> Tuple[bool, str]:
    """Check if a command exists and return version info"""
    path = shutil.which(cmd)
    if not path:
        return False, f"❌ {cmd} not found"

    try:
        result = subprocess.run(
            [cmd, "--version"],
            capture_output=True,
            text=True,
            timeout=5
        )
        version = result.stdout.split('\n')[0] if result.stdout else "unknown"
        return True, f"✓ {cmd} found at {path} ({version})"
    except Exception as e:
        return False, f"⚠️  {cmd} found but error: {e}"


def check_environment() -> Dict[str, Tuple[bool, str]]:
    """Check development environment"""
    results = {}

    # Check QEMU
    ok, msg = check_command("qemu-system-x86_64")
    results["QEMU"] = (ok, msg)

    # Check GCC cross-compiler
    ok, msg = check_command("x86_64-linux-gnu-gcc")
    results["Cross-compiler"] = (ok, msg)

    # Check GRUB
    ok, msg = check_command("grub2-mkrescue")
    results["GRUB"] = (ok, msg)

    # Check Make
    ok, msg = check_command("make")
    results["Make"] = (ok, msg)

    # Check Rust (optional)
    ok, msg = check_command("rustc")
    results["Rust"] = (ok, msg)

    # Check display environment
    display = os.environ.get("DISPLAY", "")
    wayland_display = os.environ.get("WAYLAND_DISPLAY", "")

    if display or wayland_display:
        results["Display"] = (
            True,
            f"✓ Display available (DISPLAY={display or 'N/A'}, WAYLAND={wayland_display or 'N/A'})"
        )
    else:
        results["Display"] = (
            False,
            "⚠️  No display server detected (headless mode)"
        )

    # Check kernel binary
    if KERNEL_BIN.exists():
        size = KERNEL_BIN.stat().st_size
        results["Kernel binary"] = (
            True,
            f"✓ Found ({size / 1024:.1f} KB)"
        )
    elif KERNEL_TEST_BIN.exists():
        size = KERNEL_TEST_BIN.stat().st_size
        results["Kernel binary"] = (
            True,
            f"✓ Test kernel found ({size / 1024:.1f} KB)"
        )
    else:
        results["Kernel binary"] = (
            False,
            "❌ Not found. Run 'make' first."
        )

    return results


def test_kernel_boot(timeout: int = 10) -> Dict[str, any]:
    """
    Attempt to boot the kernel and capture initial output.

    Returns diagnostic information.
    """
    result = {
        "success": False,
        "exit_code": None,
        "output": "",
        "stderr": "",
        "timeout": False,
        "issues": []
    }

    kernel = KERNEL_TEST_BIN if KERNEL_TEST_BIN.exists() else KERNEL_BIN

    if not kernel.exists():
        result["issues"].append("Kernel binary not found")
        return result

    cmd = [
        "qemu-system-x86_64",
        "-m", "512",
        "-no-reboot",
        "-kernel", str(kernel),
        "-serial", "file:/tmp/queenx_diagnostic_serial.log",
        "-display", "none",
        "-d", "cpu_reset,guest_errors"
    ]

    print(f"▶ Testing kernel boot (timeout: {timeout}s)...")
    print(f"   Command: {' '.join(cmd[:8])}...")

    try:
        process = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(PROJECT_ROOT)
        )

        try:
            stdout, stderr = process.communicate(timeout=timeout)
            result["exit_code"] = process.returncode
            result["stderr"] = stderr.decode('utf-8', errors='replace')
        except subprocess.TimeoutExpired:
            process.kill()
            stdout, stderr = process.communicate()
            result["timeout"] = True
            result["exit_code"] = -1
            result["stderr"] = stderr.decode('utf-8', errors='replace')

        # Read serial output
        serial_log = Path("/tmp/queenx_diagnostic_serial.log")
        if serial_log.exists():
            with open(serial_log, 'r') as f:
                result["output"] = f.read()

        # Analyze output for issues
        output = result["output"]
        stderr = result["stderr"]

        # Check for successful boot indicators
        if "QueenX" in output or "[TEST]" in output or "QueenX" in output:
            result["success"] = True
            result["issues"].append("✓ Kernel appears to have started")

        # Check for problems
        if "Booting from ROM.." in output:
            count = output.count("Booting from ROM..")
            if count > 2:
                result["issues"].append(f"🔄 Boot loop detected ({count} restarts)")

        if "triple fault" in output.lower():
            result["issues"].append("💥 Triple fault occurred")

        if "Gdk" in stderr and "assertion" in stderr:
            result["issues"].append("🖥️  Graphics assertion (can be ignored in headless mode)")

        if "cannot use stdio" in stderr:
            result["issues"].append("🔌 Serial port conflict (should use file backend)")

        if not output.strip():
            result["issues"].append("📭 No serial output - kernel may not have started")

        if result["timeout"]:
            result["issues"].append(f"⏰ Timed out after {timeout}s (may be normal for long tests)")

    except FileNotFoundError:
        result["issues"].append("❌ QEMU not found")

    except Exception as e:
        result["issues"].append(f"❌ Error: {e}")

    return result


def print_environment_report(env_checks: Dict):
    """Print environment check results"""
    print_header("ENVIRONMENT CHECK")

    all_ok = True
    for name, (ok, msg) in env_checks.items():
        print(f"  {msg}")
        if not ok:
            all_ok = False

    if all_ok:
        print("\n✅ Environment looks good!")
    else:
        print("\n⚠️  Some issues detected - see above")


def print_boot_report(boot_result: Dict):
    """Print boot test results"""
    print_header("KERNEL BOOT TEST")

    if boot_result["success"]:
        print("  ✅ Kernel booted successfully!\n")
    else:
        print("  ❌ Kernel failed to boot properly\n")

    print("  Issues detected:")
    for issue in boot_result["issues"]:
        print(f"    {issue}")

    if boot_result["output"].strip():
        print(f"\n  Serial output ({len(boot_result['output'])} bytes):")
        print("  " + "-"*66)
        lines = boot_result['output'].split('\n')[-30:]
        for line in lines:
            print(f"  | {line}")
        print("  " + "-"*66)


def main():
    import argparse

    parser = argparse.ArgumentParser(
        description="QueenX QEMU Diagnostic Tool",
        formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--env-only", action="store_true",
        help="Only check environment, don't test boot"
    )
    parser.add_argument(
        "--boot-only", action="store_true",
        help="Only test boot, skip environment check"
    )
    parser.add_argument(
        "--timeout", type=int, default=10,
        help="Boot test timeout in seconds (default: 10)"
    )

    args = parser.parse_args()

    print("\n" + "╔" + "="*68 + "╗")
    print("║" + "  QueenX QEMU Diagnostic Tool".center(68) + "║")
    print("╚" + "="*68 + "╝")

    if not args.boot_only:
        env_checks = check_environment()
        print_environment_report(env_checks)

    if not args.env_only:
        boot_result = test_kernel_boot(timeout=args.timeout)
        print_boot_report(boot_result)

    print("\n" + "╔" + "="*68 + "╗")
    print("║" + "  Recommendations".center(68) + "║")
    print("╠" + "="*68 + "╣")

    recommendations = []

    if not args.boot_only:
        env_ok = all(ok for ok, _ in env_checks.values())
        if not env_ok:
            recommendations.append("• Fix environment issues listed above before running tests")

        display_ok, _ = env_checks.get("Display", (False, ""))
        if not display_ok:
            recommendations.append("• Use headless mode: make run-headless or make test-unit")
            recommendations.append("• Or set DISPLAY=:0 if you have X11 available")

    if not args.env_only:
        if boot_result["timeout"] and not boot_result["success"]:
            recommendations.append("• Increase timeout: make test-unit (120s default)")
            recommendations.append("• Or run quick test: make test-quick (30s)")

        if not boot_result["output"].strip():
            recommendations.append("• Check if kernel has serial initialization code")
            recommendations.append("• Try: make log (captures to file)")

        if "Boot loop" in str(boot_result["issues"]):
            recommendations.append("• Check multiboot header in boot.asm")
            recommendations.append("• Verify linker script entry point")

    if recommendations:
        for rec in recommendations:
            print("║" + f"  {rec}".ljust(68) + "║")
    else:
        print("║" + "  ✓ Everything looks good! Try running: make test-unit".center(68) + "║")

    print("╚" + "="*68 + "╝\n")


if __name__ == "__main__":
    main()
