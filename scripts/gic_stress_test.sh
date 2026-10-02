#!/bin/bash
# ============================================================================
# aarch64 GICv3 启动压测脚本 (ISSUE-RT-002 复验装备)
#
# 用途: 连续启动 aarch64 内核 N 次, 每次都要求串口出现 `GICv3 ready`
#       里程碑, 任一缺失即 fail-closed. 用于把 ISSUE-RT-002 (GICv3 偶发
#       挂起) 的"偶发不可复现"转为可重复的启动压力测试.
#
# 说明: 本脚本只断言 GIC 初始化里程碑 (`GICv3 ready`, 由 boot 入口在初始化
#       后置条件自检通过后打印). 更完整的启动路径断言 (VFS ready / EL0 /
#       KPTI) 由 scripts/qemu_boot_test.sh 承担.
#
# 用法:
#   ./scripts/gic_stress_test.sh [次数]        # 默认 50 次
# 环境变量:
#   GIC_STRESS_TIMEOUT   单次启动等待里程碑上限秒数 (默认 20)
#   GIC_STRESS_KEEP_LOG  1 = 保留每次串口日志 (默认仅保留失败日志)
#
# 退出码: 0 = N/N 全部通过, 1 = 存在失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ITER="${1:-50}"
TIMEOUT="${GIC_STRESS_TIMEOUT:-20}"
KEEP_LOG="${GIC_STRESS_KEEP_LOG:-0}"
MARKER="GICv3 ready"

IMAGE="build/kernel-aarch64.img"
LOG_DIR="build/log"
mkdir -p "$LOG_DIR"

if [ ! -f "$IMAGE" ]; then
    echo "[gic-stress] 镜像缺失: $IMAGE (先运行 make ARCH=aarch64 all)" >&2
    exit 1
fi

command -v qemu-system-aarch64 >/dev/null 2>&1 || {
    echo "[gic-stress] 未找到 qemu-system-aarch64" >&2
    exit 1
}

echo "[gic-stress] 目标: $IMAGE  次数: $ITER  单次超时: ${TIMEOUT}s"
echo "[gic-stress] 里程碑断言: '$MARKER'"

PASS=0
FAIL=0
for i in $(seq 1 "$ITER"); do
    log="$LOG_DIR/gic_stress_iter.log"
    : > "$log"

    # 后台启动 QEMU, 轮询串口日志; 命中里程碑即提前 kill, 避免每次跑满超时.
    qemu-system-aarch64 \
        -M virt,gic-version=3 -cpu max -m 512 \
        -kernel "$IMAGE" \
        -serial "file:$log" \
        -display none -no-reboot \
        -device virtio-net-device,netdev=n0 \
        -netdev user,id=n0 \
        >/dev/null 2>&1 &
    qpid=$!

    hit=0
    for _ in $(seq 1 $((TIMEOUT * 10))); do
        if grep -q "$MARKER" "$log" 2>/dev/null; then
            hit=1
            break
        fi
        # QEMU 已退出且未命中 -> 直接判失败, 不空等超时.
        if ! kill -0 "$qpid" 2>/dev/null; then
            break
        fi
        sleep 0.1
    done

    kill "$qpid" 2>/dev/null || true
    wait "$qpid" 2>/dev/null || true

    if [ "$hit" = "1" ]; then
        PASS=$((PASS + 1))
        printf '  [%3d/%3d] \033[0;32mPASS\033[0m\n' "$i" "$ITER"
        [ "$KEEP_LOG" = "1" ] || rm -f "$log"
    else
        FAIL=$((FAIL + 1))
        fail_log="$LOG_DIR/gic_stress_fail_${i}.log"
        cp "$log" "$fail_log" 2>/dev/null || true
        printf '  [%3d/%3d] \033[0;31mFAIL\033[0m  (日志: %s)\n' "$i" "$ITER" "$fail_log"
    fi
done

echo "--------------------------------------------"
echo "[gic-stress] 结果: ${PASS}/${ITER} 通过, ${FAIL} 失败"
if [ "$FAIL" -ne 0 ]; then
    echo "[gic-stress] ❌ 存在失败 (ISSUE-RT-002 复验未通过)"
    exit 1
fi
echo "[gic-stress] ✅ 全部通过"
exit 0
