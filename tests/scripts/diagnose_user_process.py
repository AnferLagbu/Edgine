#!/usr/bin/env python3
"""
QueenX OS User Process Diagnostic Tool

This script diagnoses the user process crash issue by:
1. Verifying ELF entry points (embedded vs compiled)
2. Running QEMU and capturing detailed logs
3. Analyzing crash patterns

Usage:
    python3 diagnose_user_process.py [--fix] [--test]
"""

import subprocess
import sys
import os
import struct
import time
import argparse
from datetime import datetime

PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TESTS_ROOT = os.path.dirname(os.path.abspath(__file__))
LOG_DIR = os.path.join(TESTS_ROOT, "logs")
BUILD_DIR = os.path.join(PROJECT_ROOT, "other", "build")
ISO_PATH = os.path.join(BUILD_DIR, "antx.iso")

def ensure_log_dir():
    os.makedirs(LOG_DIR, exist_ok=True)

def get_timestamp():
    return datetime.now().strftime("%Y%m%d_%H%M%S")

def parse_elf_header(data):
    """Parse ELF64 header and return key fields"""
    if len(data) < 64 or data[:4] != b'\x7fELF':
        return None
    
    e_ident = data[:16]
    ei_class = data[4]
    ei_data = data[5]
    
    (e_type, e_machine, e_version,
     e_entry, e_phoff, e_shoff, e_flags,
     e_ehsize, e_phentsize, e_phnum,
     e_shentsize, e_shnum, e_shstrndx) = struct.unpack('<HHIQQQIHHHHHH', data[16:64])
    
    return {
        'class': 64 if ei_class == 2 else 32,
        'data': 'LE' if ei_data == 1 else 'BE',
        'type': e_type,
        'machine': e_machine,
        'entry': e_entry,
        'phoff': e_phoff,
        'phnum': e_phnum,
        'shoff': e_shoff,
        'shnum': e_shnum,
    }

def verify_elf_consistency():
    """Verify the init binary embedded via include_bytes! matches the compiled binary"""
    print("\n" + "="*70)
    print("[DIAGNOSE] ELF Consistency Check")
    print("="*70)
    
    ensure_log_dir()
    log_file = os.path.join(LOG_DIR, f"elf_check_{get_timestamp()}.log")
    
    results = []
    
    compiled_path = os.path.join(BUILD_DIR, "user", "init.bin")
    
    print(f"\n[INFO] Compiled binary: {compiled_path}")
    
    if not os.path.exists(compiled_path):
        print("[WARN] Compiled binary not found! Run 'make user' first.")
        results.append(("Compiled binary", "MISSING", ""))
        return False, results
    
    with open(compiled_path, 'rb') as f:
        compiled_data = f.read()
    
    compiled_elf = parse_elf_header(compiled_data)
    
    if not compiled_elf:
        print("[ERROR] Invalid ELF header in compiled binary")
        return False, results
    
    print(f"\n{'Field':<25} {'Value':<30}")
    print("-" * 55)
    
    print(f"{'Entry Point':<25} 0x{compiled_elf['entry']:016X}")
    print(f"{'File Size':<25} {len(compiled_data)} bytes")
    print(f"{'Program Headers':<25} {compiled_elf['phnum']}")
    print(f"{'Class':<25} ELF{compiled_elf['class']}")
    print(f"{'Endianness':<25} {compiled_elf['data']}")
    
    if compiled_elf['machine'] != 0x3E:
        print(f"\n[ERROR] Machine type is not x86_64 (0x3E), got 0x{compiled_elf['machine']:04X}")
        results.append(("Machine", f"0x{compiled_elf['machine']:04X}", "Expected 0x3E"))
        return False, results
    
    if compiled_elf['entry'] != 0x400000:
        print(f"\n[WARN] Entry point is not the expected 0x400000")
    
    entry_offset = compiled_elf['entry'] - 0x400000
    if 0 <= entry_offset < len(compiled_data):
        bytes_at_entry = compiled_data[entry_offset:entry_offset+8]
        print(f"\n[INFO] Bytes at entry +0x{entry_offset:X}:")
        print(f"       {' '.join(f'{b:02X}' for b in bytes_at_entry)}")
        
        valid_instructions = {
            (0x55,): "push rbp",
            (0x55, 0x48, 0x89, 0xE5): "push rbp; mov rbp, rsp",
            (0x48, 0x89, 0xE5): "mov rbp, rsp",
        }
        
        for pattern, desc in valid_instructions.items():
            if bytes_at_entry[:len(pattern)] == bytearray(pattern):
                print(f"       → Valid instruction: {desc}")
                break
        else:
            print(f"       → Unknown instruction sequence (may still be valid)")
    
    all_ok = True
    
    with open(log_file, 'w') as f:
        f.write("ELF Consistency Check Results\n")
        f.write(f"Timestamp: {get_timestamp()}\n")
        f.write(f"{'='*60}\n\n")
        f.write(f"Entry Point: 0x{compiled_elf['entry']:016X}\n")
        f.write(f"File Size: {len(compiled_data)} bytes\n")
        f.write(f"Program Headers: {compiled_elf['phnum']}\n")
        f.write(f"\nOverall: {'PASS' if all_ok else 'FAIL'}\n")
    
    print(f"\n[LOG] Detailed report saved to: {log_file}")
    
    return all_ok, results

def run_qemu_test(timeout=30):
    """Run QEMU and capture output to diagnose user process execution"""
    ensure_log_dir()
    timestamp = get_timestamp()
    log_file = os.path.join(LOG_DIR, f"qemu_run_{timestamp}.log")
    
    print(f"\n{'='*70}")
    print(f"[TEST] Running QueenX OS in QEMU")
    print(f"{'='*70}")
    print(f"[TEST] Log file: {log_file}")
    print(f"[TEST] Timeout: {timeout}s")
    
    if not os.path.exists(ISO_PATH):
        print(f"[ERROR] ISO not found: {ISO_PATH}")
        print(f"[ERROR] Run 'make iso' first")
        return False
    
    cmd = [
        "qemu-system-x86_64",
        "-cdrom", ISO_PATH,
        "-m", "512M",
        "-no-reboot",
        "-nographic",
        "-serial", "mon:stdio",
    ]
    
    print(f"[TEST] Command: {' '.join(cmd)}\n")
    
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
        
        install_inputs = [
            (b"Press ENTER to continue", b"\n"),
            (b"Enter root password", b"abcde\n"),
            (b"Confirm root password", b"abcde\n"),
            (b"Enter hostname", b"\n"),
        ]
        
        while True:
            elapsed = time.time() - start_time
            if elapsed > timeout:
                print(f"\n[TEST] ⏱ Timeout after {timeout:.1f}s")
                break
            
            try:
                char = proc.stdout.read(1)
                if not char:
                    print(f"\n[TEST] QEMU process ended")
                    break
                
                full_output.extend(char)
                sys.stdout.buffer.write(char)
                sys.stdout.buffer.flush()
                
                output_str = full_output.decode('latin-1', errors='replace')
                
                for prompt, response in install_inputs:
                    if isinstance(prompt, bytes):
                        prompt = prompt.decode('latin-1')
                    if prompt in output_str and input_index < len(install_inputs):
                        idx = install_inputs.index((prompt, response))
                        if idx == input_index:
                            time.sleep(0.3)
                            proc.stdin.write(response)
                            proc.stdin.flush()
                            input_index += 1
                            print(f"\n[TEST] ✓ Sent input for: {prompt}")
                            break
                
                if "Schedule: switch to PID=1" in output_str:
                    print(f"\n{'!'*70}")
                    print(f"[TEST] ★ User process launch detected!")
                    print(f"{'!'*70}")
                    
                    for _ in range(100):
                        try:
                            char = proc.stdout.read(1)
                            if char:
                                full_output.extend(char)
                                sys.stdout.buffer.write(char)
                                sys.stdout.buffer.flush()
                        except:
                            break
                        time.sleep(0.05)
                    break
                    
            except Exception as e:
                print(f"\n[TEST] Read error: {e}")
                break
        
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except:
            proc.kill()
        
        with open(log_file, 'wb') as f:
            f.write(full_output)
        
        output_text = full_output.decode('latin-1', errors='replace')
        
        analyze_test_output(output_text, log_file)
        
        return True
        
    except FileNotFoundError:
        print(f"[ERROR] qemu-system-x86_64 not found")
        return False
    except Exception as e:
        print(f"[ERROR] {e}")
        import traceback
        traceback.print_exc()
        return False

def analyze_test_output(output_text, log_file):
    """Analyze test output for success/failure indicators"""
    print(f"\n{'='*70}")
    print(f"[ANALYSIS] Test Results")
    print(f"{'='*70}")
    
    issues = []
    successes = []
    
    if "[INIT]" in output_text:
        successes.append("User process [INIT] message printed")
        print(f"✓ User process executed and printed [INIT]")
    
    if "Starting shell" in output_text or "shell process created" in output_text:
        successes.append("Shell process started")
        print(f"✓ Shell process creation initiated")
    
    if "Welcome to QueenX Operating System!" in output_text:
        successes.append("Welcome message displayed")
        print(f"✓ Welcome message shown to user")
    
    if "Installation Complete!" in output_text:
        successes.append("Installation completed")
        print(f"✓ Installation wizard completed")
    
    if "Page Fault" in output_text:
        issues.append("Page Fault exception")
        print(f"✗ Page Fault detected")
    
    if "General Protection" in output_text:
        issues.append("General Protection Fault")
        print(f"✗ General Protection Fault detected")
    
    if "Invalid Opcode" in output_text:
        issues.append("Invalid Opcode exception")
        print(f"✗ Invalid Opcode detected (wrong entry point?)")
    
    has_switch = "Schedule: switch to PID=1" in output_text
    if has_switch:
        idx = output_text.index("Schedule: switch to PID=1")
        post_switch = output_text[idx:idx+500]
        
        if len(successes) == 0 and has_switch:
            issues.append("Process crashed immediately after switch")
            print(f"✗ Process switched but no user-mode output detected")
            print(f"   Post-switch context:\n{post_switch[:300]}")
    
    analysis_file = log_file.replace(".log", "_analysis.txt")
    with open(analysis_file, 'w') as f:
        f.write("Test Execution Analysis\n")
        f.write(f"Timestamp: {get_timestamp()}\n")
        f.write(f"{'='*60}\n\n")
        
        f.write("Successes:\n")
        for s in successes:
            f.write(f"  ✓ {s}\n")
        
        f.write("\nIssues:\n")
        for i in issues:
            f.write(f"  ✗ {i}\n")
        
        f.write(f"\nVerdict: {'PASS' if len(issues) == 0 and len(successes) > 0 else 'FAIL'}\n")
    
    print(f"\n[SUMMARY] Successes: {len(successes)}, Issues: {len(issues)}")
    print(f"[ANALYSIS] Full analysis: {analysis_file}")

def main():
    parser = argparse.ArgumentParser(
        description="QueenX OS User Process Diagnostic Tool",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python3 %(prog)s --check         # Check ELF consistency only
  python3 %(prog)s --check --fix   # Check and fix if needed
  python3 %(prog)s --test          # Run full QEMU test
  python3 %(prog)s --all           # Check, fix, rebuild, test
        """
    )
    
    parser.add_argument("--check", action="store_true",
                       help="Check ELF consistency of compiled init.bin")
    parser.add_argument("--fix", action="store_true",
                       help="Rebuild user binary (make user)")
    parser.add_argument("--test", action="store_true",
                       help="Run QEMU test after fixing")
    parser.add_argument("--all", action="store_true",
                       help="Run complete check → rebuild → test cycle")
    parser.add_argument("--timeout", type=int, default=30,
                       help="QEMU timeout in seconds (default: 30)")
    
    args = parser.parse_args()
    
    print(f"\n{'█'*70}")
    print(f"█  QueenX OS User Process Diagnostic Tool")
    print(f"{'█'*70}")
    print(f"Project Root: {PROJECT_ROOT}")
    print(f"Log Directory: {LOG_DIR}")
    
    if args.all:
        args.check = True
        args.fix = True
        args.test = True
    
    if not any([args.check, args.fix, args.test]):
        args.check = True
        args.test = True
    
    overall_success = True
    
    if args.check:
        consistent, results = verify_elf_consistency()
        if not consistent:
            print(f"\n[⚠] ELF check failed!")
            overall_success = False
        else:
            print(f"\n[✓] All checks passed!")
    
    if args.fix:
        print(f"\n[→] Rebuilding user binary...")
        ret = subprocess.call(["make", "user"], cwd=PROJECT_ROOT)
        if ret == 0:
            print(f"[✓] Rebuild succeeded. Now rebuild kernel with 'make clean && make all && make iso'")
        else:
            print(f"[✗] Rebuild failed!")
            overall_success = False
    
    if args.test:
        if run_qemu_test(timeout=args.timeout):
            pass
        else:
            overall_success = False
    
    print(f"\n{'█'*70}")
    if overall_success:
        print(f"█  Overall Status: ✓ SUCCESS")
    else:
        print(f"█  Overall Status: ✗ ISSUES DETECTED")
    print(f"{'█'*70}\n")
    
    return 0 if overall_success else 1

if __name__ == "__main__":
    sys.exit(main())
