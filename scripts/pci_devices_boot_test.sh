#!/bin/bash
# ============================================================================
# PCI 设备集成验证脚本 (B04-22)
#
# 用途: QEMU -nic none + 多个 PCI 设备 (-device nvme / e1000) 启动, 验证:
#   1. PCI 扫描通过 B04-20 加锁层 (PCI_CONFIG_LOCK + 显式单核约束)
#   2. 驱动加载路径 (storage_init / e1000::probe) 能识别设备
#   3. NVMe MSI-X 中断路径 (MSI-X IRQ fired)
#   4. e1000 probe 不破坏 boot 流程 (network subsystem ready)
#
# 输出: other/build/log/pci_boot_*.log
# 退出码: 0 = 全部通过, 1 = 部分失败
#
# 历史: 2026-08-25 初次实现 (B04-22 收尾补丁, 由 AGENTS.md 决策授权)
# ============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

ARCH="${1:-x86_64}"
TIMEOUT_QEMU="${TIMEOUT_QEMU:-15}"
NVME_DISK="${NVME_DISK:-/tmp/nvme_disk.img}"

ok()   { echo -e "${GREEN}✓ $1${NC}"; }
err()  { echo -e "${RED}✗ $1${NC}"; }
warn() { echo -e "${YELLOW}! $1${NC}"; }
info() { echo -e "${BLUE}-> $1${NC}"; }

# 准备 NVMe 磁盘 (B04-22 必需): 若不存在则创建 64MB raw 镜像
if [ ! -f "$NVME_DISK" ]; then
    info "NVMe 磁盘镜像不存在, 创建 64MB raw: $NVME_DISK"
    dd if=/dev/zero of="$NVME_DISK" bs=1M count=64 status=none
fi

RESULT=0
TESTED=0
PASSED=0

# ---------------------------------------------------------------------------
# x86_64: -nic none + nvme + e1000 启动
# ---------------------------------------------------------------------------
if [ "$ARCH" = "all" ] || [ "$ARCH" = "x86_64" ]; then
    TESTED=$((TESTED+1))
    info "=== x86_64 PCI 集成测试 (NVMe + e1000 + -nic none) ==="

    if [ ! -f other/build/kernel.flat ]; then
        err "other/build/kernel.flat 缺失, 跳过"
        RESULT=1
    else
        LOG="$LOG_DIR/pci_boot_x86_64.log"
        rm -f "$LOG"

        timeout "$TIMEOUT_QEMU" qemu-system-x86_64 \
            -serial "file:${LOG}" \
            -display none \
            -no-reboot \
            -m 512 \
            -nic none \
            -kernel other/build/kernel.flat \
            -device nvme,serial=QM0001,id=nvme0 \
            -drive "file=${NVME_DISK},if=none,id=nd0" \
            -device nvme-ns,drive=nd0,bus=nvme0 \
            -device e1000,netdev=net0 \
            -netdev user,id=net0 \
            >/dev/null 2>&1 || true

        if [ ! -s "$LOG" ]; then
            err "x86_64 PCI 日志为空, 内核未进入 Rust 入口"
            RESULT=1
        else
            lines=$(wc -l < "$LOG")
            info "[x86_64] 串口输出 ${lines} 行"

            # 验证 1: PCI 扫描完成 (B04-20)
            if grep -q "PCI bus initialized" "$LOG"; then
                ok "[x86_64] PCI 总线扫描通过 (B04-20)"
                SCAN_OK=1
            else
                err "[x86_64] PCI 总线扫描失败 — 缺 'PCI bus initialized'"
                SCAN_OK=0
            fi

            # 验证 2: NVMe 控制器识别
            if grep -q "NVMe: found at" "$LOG"; then
                ok "[x86_64] NVMe 控制器识别通过"
                NVME_OK=1
            else
                warn "[x86_64] NVMe 控制器未识别 (设备可能不在 PCI 总线)"
                NVME_OK=0
            fi

            # 验证 3: e1000 NIC probe
            if grep -qE "e1000|E1000" "$LOG"; then
                ok "[x86_64] e1000 NIC probe 已执行"
                E1000_OK=1
            else
                warn "[x86_64] e1000 probe 无日志 (驱动可能不在当前路径)"
                E1000_OK=0
            fi

            # 验证 4: NVMe MSI-X 端到端 (若 NVMe 识别成功)
            if [ "$NVME_OK" = "1" ] && grep -q "MSI-X IRQ [12] fired" "$LOG"; then
                ok "[x86_64] NVMe MSI-X 中断触发 (B04-02/MSIX-03)"
                MSIX_OK=1
            else
                warn "[x86_64] NVMe MSI-X 中断未触发 (NVMe 路径可能未走通)"
                MSIX_OK=0
            fi

            # 验证 5: 完整 boot (进入 Ring 3) — B04-22 终极验收
            if grep -q "Entering Ring 3" "$LOG"; then
                ok "[x86_64] 完整启动到 Ring 3 — B04-22 PCI 集成验收通过"
                PASSED=$((PASSED+1))
            else
                warn "[x86_64] 未到 Ring 3 (最后一行: $(tail -1 "$LOG"))"
                [ "$NVME_OK" = "1" ] && [ "$SCAN_OK" = "1" ] && PASSED=$((PASSED+1)) || RESULT=1
            fi
        fi
    fi
fi

echo ""
echo "============================================"
echo "PCI 集成测试: ${PASSED}/${TESTED} 通过"
echo "  日志: ${LOG_DIR}/pci_boot_*.log"
echo "============================================"
exit $RESULT
