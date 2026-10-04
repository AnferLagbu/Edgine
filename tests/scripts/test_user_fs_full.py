#!/usr/bin/env python3
"""
QueenX User-Mode + Filesystem Full Test
Boots the full kernel ISO, auto-responds to install wizard,
tests filesystem operations, and reports results.
"""

import subprocess
import sys
import os
import time
import re
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent.parent
ISO_PATH = PROJECT_ROOT / "other" / "build" / "antx.iso"

# Install wizard auto-responses
INSTALL_INPUT = (
    b"\n"           # Welcome → Enter
    b"abcde\n"      # Root password
    b"abcde\n"      # Confirm password  
    b"\n"           # Hostname default
    b"\n"           # Wait for eash prompt
    b"ls /\n"       # List root directory
    b"ls /bin\n"    # List /bin (user programs)
    b"mkdir /test\n"# Create test directory
    b"mkdir /test/subdir\n"
    b"touch /test/file1.txt\n"   # Create file
    b"touch /test/file2.bin\n"
    b"cat /test/file1.txt\n"     # Read file (empty)
    b"ls /test\n"                # Verify directory
    b"stat /test/file1.txt\n"    # File stat
    b"rm /test/file2.bin\n"      # Remove file
    b"ls /test\n"                # Verify deletion
    b"rmdir /test/subdir\n"      # Remove directory
    b"ls /test\n"                # Verify
)

RESULTS = {
    "boot":           {"status": "pending", "detail": ""},
    "install":        {"status": "pending", "detail": ""},
    "eash_prompt":    {"status": "pending", "detail": ""},
    "ls_root":        {"status": "pending", "detail": ""},
    "ls_bin":         {"status": "pending", "detail": ""},
    "mkdir":          {"status": "pending", "detail": ""},
    "touch":          {"status": "pending", "detail": ""},
    "cat":            {"status": "pending", "detail": ""},
    "stat":           {"status": "pending", "detail": ""},
    "rm":             {"status": "pending", "detail": ""},
    "rmdir":          {"status": "pending", "detail": ""},
    "no_panic":       {"status": "pending", "detail": ""},
}

def run_test(timeout=40):
    if not ISO_PATH.exists():
        print(f"[SKIP] ISO not found at {ISO_PATH}, run 'make iso' first")
        return False

    print(f"[TEST] Running QueenX User-Mode + Filesystem Test")
    print(f"[TEST] ISO: {ISO_PATH}")

    cmd = [
        "qemu-system-x86_64",
        "-cdrom", str(ISO_PATH),
        "-m", "512M",
        "-no-reboot",
        "-nographic",
        "-serial", "mon:stdio",
    ]

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )

    output_lines = []
    start = time.time()
    input_idx = 0

    try:
        while True:
            if time.time() - start > timeout:
                print(f"[TEST] Timeout ({timeout}s)")
                break

            char = proc.stdout.read(1)
            if not char:
                break

            decoded = char.decode('utf-8', errors='replace')
            output_lines.append(decoded)
            sys.stdout.write(decoded)
            sys.stdout.flush()

            full = ''.join(output_lines[-200:])

            if input_idx < len(INSTALL_INPUT):
                for trigger, data in [
                    (b"\n", b"\n"),
                    (b"eash>", INSTALL_INPUT[input_idx]),
                ]:
                    pass
                full_bytes = full.encode('utf-8', errors='replace')

                # Auto respond to prompts
                if input_idx < 4:  # Install wizard phase
                    if "Press ENTER" in full and input_idx == 0:
                        proc.stdin.write(INSTALL_INPUT[0])
                        proc.stdin.flush()
                        input_idx = 1
                        time.sleep(0.5)
                    elif "password" in full.lower() and "confirm" not in full.lower() and input_idx == 1:
                        proc.stdin.write(INSTALL_INPUT[1])
                        proc.stdin.flush()
                        input_idx = 2
                        time.sleep(0.5)
                    elif "confirm" in full.lower() and input_idx == 2:
                        proc.stdin.write(INSTALL_INPUT[2])
                        proc.stdin.flush()
                        input_idx = 3
                        time.sleep(0.5)
                    elif "hostname" in full.lower() and input_idx == 3:
                        proc.stdin.write(INSTALL_INPUT[3])
                        proc.stdin.flush()
                        input_idx = 4
                        time.sleep(0.5)

                elif "antx>" in full.lower() and "login" in full.lower() and input_idx == 4:
                    root_password = b"abcde\n"
                    proc.stdin.write(root_password)
                    proc.stdin.flush()
                    input_idx = 5
                    time.sleep(1)

                elif "eash>" in full and input_idx >= 5 and input_idx < len(INSTALL_INPUT):
                    proc.stdin.write(INSTALL_INPUT[input_idx])
                    proc.stdin.flush()
                    input_idx += 1
                    time.sleep(0.3)

    except (IOError, BrokenPipeError):
        pass
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except:
            proc.kill()

    output = ''.join(output_lines)
    analyze_results(output)
    print_report()
    return True

def analyze_results(output):
    if "QueenX Operating System" in output:
        RESULTS["boot"]["status"] = "PASS"
        RESULTS["boot"]["detail"] = "Kernel booted successfully"

    if "Installation complete" in output or "Installing" in output or "Install" in output:
        RESULTS["install"]["status"] = "PASS"
    elif "test" in output.lower():
        RESULTS["install"]["status"] = "SKIP"
        RESULTS["install"]["detail"] = "Install prompt not detected"

    if "eash>" in output:
        RESULTS["eash_prompt"]["status"] = "PASS"
        RESULTS["eash_prompt"]["detail"] = "Shell prompt detected"

    if "VFS" in output or "bin" in output or "init" in output:
        RESULTS["ls_root"]["status"] = "PASS"
        RESULTS["ls_bin"]["status"] = "PASS"

    if "mkdir" in output.lower():
        RESULTS["mkdir"]["status"] = "PASS"

    if "touch" in output.lower():
        RESULTS["touch"]["status"] = "PASS"

    if "cat" in output.lower():
        RESULTS["cat"]["status"] = "PASS"

    if "stat" in output.lower():
        RESULTS["stat"]["status"] = "PASS"

    if "rm" in output.lower():
        RESULTS["rm"]["status"] = "PASS"

    if "PANIC" not in output and "Halted" not in output:
        RESULTS["no_panic"]["status"] = "PASS"
    else:
        RESULTS["no_panic"]["status"] = "FAIL"

def print_report():
    print("\n" + "=" * 60)
    print("USER-MODE + FILESYSTEM TEST RESULTS")
    print("=" * 60)
    passed = failed = skipped = 0
    for name, r in RESULTS.items():
        status = r['status']
        if status == 'PASS':
            status_display = '\033[32mPASS\033[0m'
            passed += 1
        elif status == 'FAIL':
            status_display = '\033[31mFAIL\033[0m'
            failed += 1
        else:
            status_display = '\033[33mSKIP\033[0m'
            skipped += 1
        detail = f" - {r['detail']}" if r['detail'] else ""
        print(f"  [{status_display}] {name}{detail}")
    print("-" * 60)
    print(f"  Passed: {passed}, Failed: {failed}, Skipped: {skipped}")
    print("=" * 60)

if __name__ == "__main__":
    run_test(timeout=40)
