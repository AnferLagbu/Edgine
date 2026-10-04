#!/usr/bin/env python3
"""
QueenX OS User Process Debug Test Script

This script automates running the QueenX OS kernel in QEMU and captures
detailed logs to diagnose user process execution issues.

Usage:
    python3 test_user_process.py [--debug] [--timeout SECONDS]
"""

import subprocess
import sys
import os
import time
import argparse
from datetime import datetime

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LOG_DIR = os.path.join(PROJECT_ROOT, "logs")
ISO_PATH = os.path.join(PROJECT_ROOT, "..", "other", "build", "antx.iso")

INSTALL_INPUTS = [
    b"\n",           # Press ENTER to continue welcome screen
    b"abcde\n",      # Root password
    b"abcde\n",      # Confirm password
    b"\n",           # Hostname (default: localhost)
]

def ensure_log_dir():
    os.makedirs(LOG_DIR, exist_ok=True)

def get_log_filename(suffix=""):
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    return os.path.join(LOG_DIR, f"user_process_test{suffix}_{timestamp}.log")

def run_qemu_basic_test(timeout=30):
    """Run QEMU with serial output to file, auto-input installation wizard"""
    ensure_log_dir()
    log_file = get_log_filename("_basic")
    
    print(f"[TEST] Starting basic user process test...")
    print(f"[TEST] Log file: {log_file}")
    
    cmd = [
        "qemu-system-x86_64",
        "-cdrom", ISO_PATH,
        "-m", "512M",
        "-no-reboot",
        "-nographic",
        "-serial", "mon:stdio",
    ]
    
    print(f"[TEST] Command: {' '.join(cmd)}")
    
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=False
        )
        
        full_output = bytearray()
        start_time = time.time()
        input_index = 0
        
        while True:
            elapsed = time.time() - start_time
            if elapsed > timeout:
                print(f"[TEST] Timeout after {timeout}s")
                break
            
            try:
                char = proc.stdout.read(1)
                if not char:
                    print(f"[TEST] QEMU process ended (exit code: {proc.poll()})")
                    break
                
                full_output.extend(char)
                sys.stdout.buffer.write(char)
                sys.stdout.buffer.flush()
                
                output_str = full_output.decode('latin-1', errors='replace')
                
                if "Press ENTER to continue" in output_str and input_index == 0:
                    time.sleep(0.3)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                    print(f"\n[TEST] Sent input {input_index}: ENTER")
                
                elif "Enter root password" in output_str and input_index == 1:
                    time.sleep(0.3)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                    print(f"\n[TEST] Sent input {input_index}: password")
                
                elif "Confirm root password" in output_str and input_index == 2:
                    time.sleep(0.3)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                    print(f"\n[TEST] Sent input {input_index}: confirm password")
                
                elif "Enter hostname" in output_str and input_index == 3:
                    time.sleep(0.3)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                    print(f"\n[TEST] Sent input {input_index}: hostname (default)")
                
                elif "Schedule: switch to PID=1" in output_str:
                    print(f"\n[TEST] *** User process switch detected! Waiting for result... ***")
                    
                    for _ in range(50):
                        try:
                            char = proc.stdout.read(1)
                            if char:
                                full_output.extend(char)
                                sys.stdout.buffer.write(char)
                                sys.stdout.buffer.flush()
                        except:
                            break
                        time.sleep(0.1)
                    
                    print(f"\n[TEST] *** Post-switch capture complete ***")
                    break
                
            except Exception as e:
                print(f"[TEST] Read error: {e}")
                break
        
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except:
            proc.kill()
        
        with open(log_file, 'wb') as f:
            f.write(full_output)
        
        output_text = full_output.decode('latin-1', errors='replace')
        
        print(f"\n{'='*60}")
        print(f"[TEST] Test complete!")
        print(f"[TEST] Log saved to: {log_file}")
        print(f"[TEST] Total output length: {len(full_output)} bytes")
        print(f"{'='*60}")
        
        analyze_output(output_text, log_file)
        
        return 0
        
    except FileNotFoundError:
        print(f"[ERROR] qemu-system-x86_64 not found or ISO not found")
        print(f"[ERROR] ISO path: {ISO_PATH}")
        return 1
    except Exception as e:
        print(f"[ERROR] Unexpected error: {e}")
        import traceback
        traceback.print_exc()
        return 1

def run_qemu_debug_test(timeout=30):
    """Run QEMU with CPU debug tracing enabled"""
    ensure_log_dir()
    log_file = get_log_filename("_debug")
    
    print(f"[DEBUG] Starting debug trace test...")
    print(f"[DEBUG] Log file: {log_file}")
    
    cmd = [
        "qemu-system-x86_64",
        "-cdrom", ISO_PATH,
        "-m", "512M",
        "-no-reboot",
        "-nographic",
        "-d", "int,cpu_reset",
        "-D", log_file.replace(".log", "_qemu.log"),
    ]
    
    print(f"[DEBUG] Command: {' '.join(cmd)}")
    
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=False
        )
        
        full_output = bytearray()
        start_time = time.time()
        input_index = 0
        
        while True:
            elapsed = time.time() - start_time
            if elapsed > timeout:
                print(f"[DEBUG] Timeout after {timeout}s")
                break
            
            try:
                char = proc.stdout.read(1)
                if not char:
                    break
                
                full_output.extend(char)
                sys.stdout.buffer.write(char)
                sys.stdout.buffer.flush()
                
                output_str = full_output.decode('latin-1', errors='replace')
                
                if "Press ENTER to continue" in output_str and input_index < len(INSTALL_INPUTS):
                    time.sleep(0.2)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                    print(f"\n[DEBUG] Sent input {input_index}")
                elif "Enter root password" in output_str and input_index < len(INSTALL_INPUTS):
                    time.sleep(0.2)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                elif "Confirm root password" in output_str and input_index < len(INSTALL_INPUTS):
                    time.sleep(0.2)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                elif "Enter hostname" in output_str and input_index < len(INSTALL_INPUTS):
                    time.sleep(0.2)
                    proc.stdin.write(INSTALL_INPUTS[input_index])
                    proc.stdin.flush()
                    input_index += 1
                elif "Schedule: switch to PID=1" in output_str:
                    print(f"\n[DEBUG] *** Switch detected! Capturing post-switch output... ***")
                    for _ in range(30):
                        try:
                            char = proc.stdout.read(1)
                            if char:
                                full_output.extend(char)
                                sys.stdout.buffer.write(char)
                                sys.stdout.buffer.flush()
                        except:
                            break
                        time.sleep(0.1)
                    break
                    
            except Exception as e:
                break
        
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except:
            proc.kill()
        
        with open(log_file, 'wb') as f:
            f.write(full_output)
        
        output_text = full_output.decode('latin-1', errors='replace')
        analyze_output(output_text, log_file)
        
        return 0
        
    except Exception as e:
        print(f"[ERROR] {e}")
        return 1

def run_gdb_test():
    """Run QEMU with GDB server for interactive debugging"""
    ensure_log_dir()
    log_file = get_log_filename("_gdb")
    
    print(f"[GDB] Starting GDB debug server...")
    print(f"[GDB] Log file: {log_file}")
    print(f"[GDB] Connect with: gdb -ex 'target remote :1234' other/build/kernel.bin")
    print(f"[GDB] Set breakpoint at: process_start_user_asm")
    print(f"[GDB] Or at: scheduler_schedule (when prev==NULL)")
    
    cmd = [
        "qemu-system-x86_64",
        "-cdrom", ISO_PATH,
        "-m", "512M",
        "-no-reboot",
        "-nographic",
        "-serial", "mon:stdio",
        "-s", "-S",
    ]
    
    print(f"[GDB] Command: {' '.join(cmd)}")
    print(f"[GDB] Waiting for GDB connection on port 1234...")
    print(f"[GDB] Press Ctrl+C in GDB, then 'c' to continue")
    
    try:
        proc = subprocess.Popen(cmd)
        proc.wait()
        return 0
    except KeyboardInterrupt:
        proc.terminate()
        return 0

def analyze_output(output_text, log_file):
    """Analyze the captured output for common issues"""
    print(f"\n{'='*60}")
    print("[ANALYSIS] Output Analysis")
    print(f"{'='*60}")
    
    issues_found = []
    
    if "Schedule: switch to PID=1" in output_text:
        idx = output_text.index("Schedule: switch to PID=1")
        post_switch = output_text[idx:]
        
        if "[INIT]" in post_switch or "user_print" in post_switch or "Starting shell" in post_switch:
            print("[PASS] User process appears to have executed successfully!")
            print(f"[INFO] Post-switch output:\n{post_switch[:500]}")
        else:
            issues_found.append("User process crashed immediately after iretq")
            print("[FAIL] No user process output detected after switch")
            print(f"[INFO] Last 200 chars before end:\n{post_switch[:200]}")
    
    if "Page Fault" in output_text:
        issues_found.append("Page Fault occurred")
        print("[WARN] Page Fault detected in output")
        for line in output_text.split('\n'):
            if 'Page Fault' in line:
                print(f"       {line.strip()}")
    
    if "General Protection" in output_text:
        issues_found.append("General Protection Fault")
        print("[WARN] General Protection Fault detected")
    
    if "Triple fault" in output_text or "reboot" in output_text.lower():
        issues_found.append("System reboot/triple fault")
        print("[WARN] Possible triple fault (system restart)")
    
    if "ELF:" in output_text:
        for line in output_text.split('\n'):
            if 'ELF:' in line:
                print(f"[INFO] {line.strip()}")
    
    print(f"\n[SUMMARY] Issues found: {len(issues_found)}")
    for i, issue in enumerate(issues_found, 1):
        print(f"         {i}. {issue}")
    
    analysis_file = log_file.replace(".log", "_analysis.txt")
    with open(analysis_file, 'w') as f:
        f.write(f"Test Log Analysis\n")
        f.write(f"{'='*60}\n\n")
        f.write(f"Issues found: {len(issues_found)}\n\n")
        for i, issue in enumerate(issues_found, 1):
            f.write(f"{i}. {issue}\n")
        f.write(f"\nFull output length: {len(output_text)} chars\n")
        f.write(f"\nLast 1000 chars of output:\n")
        f.write(output_text[-1000:] if len(output_text) > 1000 else output_text)
    
    print(f"[ANALYSIS] Detailed analysis saved to: {analysis_file}")

def main():
    parser = argparse.ArgumentParser(description="QueenX OS User Process Debug Test")
    parser.add_argument("--mode", choices=["basic", "debug", "gdb"], default="basic",
                       help="Test mode: basic (serial), debug (QEMU trace), gdb (GDB server)")
    parser.add_argument("--timeout", type=int, default=30,
                       help="Timeout in seconds (default: 30)")
    parser.add_argument("--analyze-only", type=str, default=None,
                       help="Analyze existing log file")
    
    args = parser.parse_args()
    
    print(f"QueenX OS User Process Debug Tool")
    print(f"{'='*40}")
    print(f"Mode: {args.mode}")
    print(f"Timeout: {args.timeout}s")
    print(f"Project Root: {PROJECT_ROOT}")
    print()
    
    if args.analyze_only:
        with open(args.analyze_only, 'r') as f:
            output_text = f.read()
        analyze_output(output_text, args.analyze_only)
        return 0
    
    if args.mode == "basic":
        return run_qemu_basic_test(args.timeout)
    elif args.mode == "debug":
        return run_qemu_debug_test(args.timeout)
    elif args.mode == "gdb":
        return run_gdb_test()

if __name__ == "__main__":
    sys.exit(main())
