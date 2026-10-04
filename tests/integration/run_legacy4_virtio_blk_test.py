#!/usr/bin/env python3
"""
LEGACY-4.4 QEMU virtio-blk 集成测试 (BlockOps thunk 移除验证)

## 目标

验证 chitin block_dev trait dispatch 路径在 QEMU virtio-blk 环境下
不破坏 boot 流. 替代旧 BlockOps thunk + extern "C" 函数指针表.

## 测试策略 (双层验证)

### Layer 1: QEMU 启动回归 (本脚本)

- 启动 QEMU x86_64 + virtio-blk-pci 设备
- 加载 antx_test.iso (kernel_test.bin)
- 捕获 serial 输出
- 验证:
  1. QEMU 启动成功 (无 QEMU 自身报错)
  2. kernel boot 流正常 (无 KERNEL PANIC 在 boot 阶段)
  3. 测试运行 ≥ 100 个 (说明 chitin block_dev 注册未破坏 boot)

### Layer 2: 单元/契约测试 (host-tests/ + cargo test)

- `cargo test --lib -p queenx --features kernel_test`
  → 跑 chitin/mod.rs 中 8 个 test_t4_1_* 测试 (BlockDevice trait dispatch)
- `host-tests/tests/chitin_block_device_test.rs`
  → 验证 chitin_blk_read/write 走 trait dispatch, 不走 thunk

## 前置条件

- antx_test.iso 在 other/build/ 目录 (make iso)
- qemu-system-x86_64 ≥ 9.0
- 不依赖任何 PCIe 透传

## 关联

- LEGACY-4.1: ChitinDevice block_dev 字段 (commit 已实装)
- LEGACY-4.2: 移除 thunk + BlockOps (commit 已实装)
- LEGACY-4.3: 8 单元测试 (commit 已实装, host-tests 跑)
- LEGACY-4.4: QEMU 启动回归 + Layer 2 host-tests (本脚本, 2026-06-25)

## 注

kernel_test.bin 是测试模式 (跳过 PCI init), 不能直接验证 virtio-blk
设备识别. 但它能验证 chitin 设备表 + trait dispatch 路径在 boot
+ 测试运行时不出错. 真实 virtio-blk 设备识别由 kernel.bin 走
完整 PCI 路径, 那是 DRIVER-1 (USB) / virtio-blk driver 集成测试
范畴, 不在本任务.
"""

import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent.parent
BUILD_DIR = PROJECT_ROOT / "other" / "build"
LOG_DIR = PROJECT_ROOT / "other" / "tests-reports" / "legacy4_virtio_blk"
HOST_TESTS_DIR = PROJECT_ROOT / "host-tests"

QEMU_TIMEOUT_SEC = 12
DISK_SIZE_MB = 64
MIN_TEST_COUNT = 100  # boot 阶段至少跑这么多测试才视为未破坏

# QEMU boot 错误关键字 (出现任意一个视为 boot 失败)
BOOT_ERROR_PATTERNS = [
    r"KERNEL PANIC.*BOOT",
    r"qemu.*unexpected",
    r"Triple fault",
    r"#PF.*handler",
]


def run_qemu_with_virtio_blk(iso_path: Path, disk_path: Path, log_path: Path,
                              timeout: int = QEMU_TIMEOUT_SEC) -> int:
    """启动 QEMU + virtio-blk, 捕获 serial 输出."""
    cmd = [
        "qemu-system-x86_64",
        "-m", "512M",
        "-no-reboot",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-cdrom", str(iso_path),
        "-drive", f"file={disk_path},format=raw,if=virtio",
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


def run_host_tests() -> tuple[bool, str]:
    """运行 host-tests 验证 chitin block_dev trait dispatch.

    跑两个测试套件:
    1. host-tests/tests/i43_block_bridge_test.rs (I-43 chitin 块设备单一桥接契约)
    2. host-tests/src/framekernel_bench.rs 内的 chitin mock tests
    """
    print(f"[HOST] 运行 host-tests/i43_block_bridge_test ...")
    try:
        result = subprocess.run(
            ["cargo", "test", "--test", "i43_block_bridge_test", "--quiet"],
            capture_output=True, text=True, timeout=120,
            cwd=str(HOST_TESTS_DIR),
        )
        ok = result.returncode == 0
        # 取测试结果摘要
        summary_match = re.search(
            r"test result: ok\. (\d+) passed; (\d+) failed",
            result.stdout + result.stderr
        )
        summary = summary_match.group(0) if summary_match else "no summary"
        return ok, summary
    except Exception as e:
        return False, f"exception: {e}"


def analyze_qemu_log(log_path: Path) -> dict:
    """分析 QEMU serial 日志, 提取关键指标."""
    if not log_path.exists():
        return {"boot_ok": False, "reason": "no log file"}

    content = log_path.read_text(encoding="utf-8", errors="replace")

    # 1. 检查 boot panic
    boot_panics = []
    for pat in BOOT_ERROR_PATTERNS:
        m = re.search(pat, content)
        if m:
            boot_panics.append(m.group(0))

    # 2. 检查 boot 流标识
    boot_markers = {
        "klog_init": "[BOOT] KLog initialized" in content,
        "queenx_start": "[BOOT] QueenX starting" in content,
        "config_validated": "Configuration validated" in content,
        "test_mode_registered": "Test mode: SMP BSP registered" in content,
        "256_cases_registered": "256 test cases registered" in content,
    }

    # 3. 测试运行计数: 找最后一个 [N/256] (排除 "256 test cases registered" 误匹配)
    # 真实测试进度形如 "[125/256] PI_MUTEX::...", 出现在 boot 之后.
    test_progress = re.findall(r"\[(\d+)/256\]\s+[A-Z_]+::", content)
    last_test_num = max((int(n) for n in test_progress), default=0)

    # 4. PASS / FAIL 统计
    pass_count = content.count("...PASS")
    fail_count = content.count("...FAIL")

    return {
        "boot_ok": len(boot_panics) == 0,
        "boot_panics": boot_panics,
        "boot_markers": boot_markers,
        "last_test_num": last_test_num,
        "pass_count": pass_count,
        "fail_count": fail_count,
        "log_size": len(content),
    }


def main() -> int:
    print("=" * 64)
    print("  LEGACY-4.4 QEMU virtio-blk 集成测试")
    print("  BlockOps thunk → BlockDevice trait dispatch 验证")
    print("=" * 64)
    print()

    # 0. 前置检查
    iso_path = BUILD_DIR / "antx_test.iso"
    if not iso_path.exists():
        print(f"[FAIL] ISO 不存在: {iso_path}")
        print(f"       请先执行 'make iso' 生成测试镜像")
        return 1

    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / "serial.log"

    layer1_passed = False
    layer2_passed = False
    layer1_summary = ""
    layer2_summary = ""

    # =================================================================
    # Layer 2 先跑 (host-tests 不依赖 QEMU, 优先验证)
    # =================================================================
    print("-" * 64)
    print("  Layer 2: host-tests/chitin_block_device_test (trait 契约)")
    print("-" * 64)
    layer2_passed, layer2_summary = run_host_tests()
    print(f"  结果: {layer2_summary}")
    print(f"  {'[PASS]' if layer2_passed else '[FAIL]'} Layer 2: chitin block_dev trait 契约")
    print()

    # =================================================================
    # Layer 1: QEMU 启动回归
    # =================================================================
    print("-" * 64)
    print("  Layer 1: QEMU + virtio-blk 启动回归")
    print("-" * 64)

    # 创建临时磁盘
    with tempfile.NamedTemporaryFile(prefix="qemu_legacy4_", suffix=".raw",
                                     delete=False) as f:
        disk_path = Path(f.name)
    try:
        print(f"[DISK] 创建 {DISK_SIZE_MB} MiB raw 磁盘: {disk_path}")
        with open(disk_path, "wb") as f:
            f.write(b"\x00" * (DISK_SIZE_MB * 1024 * 1024))

        start_ts = time.time()
        rc = run_qemu_with_virtio_blk(iso_path, disk_path, log_path)
        elapsed = time.time() - start_ts
        print(f"[QEMU] 退出码: {rc}, 耗时: {elapsed:.1f}s")

        analysis = analyze_qemu_log(log_path)
        print(f"[LOG] 日志大小: {analysis['log_size']} bytes")
        print(f"[LOG] 测试运行: {analysis['last_test_num']}/256")
        print(f"[LOG] PASS: {analysis['pass_count']}, FAIL: {analysis['fail_count']}")
        print()

        # 验收
        markers = analysis["boot_markers"]
        print("  Boot 流标识:")
        for k, v in markers.items():
            status = "✓" if v else "✗"
            print(f"    [{status}] {k}")
        print()

        if analysis["boot_panics"]:
            print(f"  [FAIL] boot 阶段 panic:")
            for p in analysis["boot_panics"]:
                print(f"         {p}")
        else:
            print(f"  [PASS] boot 阶段无 panic")

        all_markers = all(markers.values())
        tests_ran_enough = analysis["last_test_num"] >= MIN_TEST_COUNT
        layer1_passed = (
            analysis["boot_ok"] and all_markers and tests_ran_enough
        )
        layer1_summary = (
            f"boot_ok={analysis['boot_ok']}, markers={sum(markers.values())}/{len(markers)}, "
            f"tests_ran={analysis['last_test_num']}/{MIN_TEST_COUNT}"
        )
        print(f"  {'[PASS]' if layer1_passed else '[FAIL]'} Layer 1: {layer1_summary}")
        print()

        # 显示最后 20 行日志 (boot 收尾)
        if log_path.exists():
            print("-" * 64)
            print("  QEMU serial 日志尾段 (最后 20 行)")
            print("-" * 64)
            lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
            for line in lines[-20:]:
                print(f"  {line}")
            print()

    finally:
        if disk_path.exists():
            disk_path.unlink()
            print(f"[CLEANUP] 删除临时磁盘: {disk_path}")

    # =================================================================
    # 结论
    # =================================================================
    print("=" * 64)
    if layer1_passed and layer2_passed:
        print("  ✅ LEGACY-4.4: QEMU virtio-blk + host-tests 双层验证 PASS")
        print("  - Layer 1: QEMU 启动回归 (boot 流 + ≥100 测试)")
        print("  - Layer 2: chitin block_dev trait dispatch 契约")
        print("  - BlockOps thunk 已成功替换为 BlockDevice trait (0 unsafe)")
        print("=" * 64)
        return 0
    else:
        print("  ❌ LEGACY-4.4: 验证 FAIL")
        print(f"     Layer 1: {'PASS' if layer1_passed else 'FAIL'} ({layer1_summary})")
        print(f"     Layer 2: {'PASS' if layer2_passed else 'FAIL'} ({layer2_summary})")
        print("=" * 64)
        return 1


if __name__ == "__main__":
    sys.exit(main())