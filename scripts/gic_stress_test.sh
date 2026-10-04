#!/bin/bash
# ============================================================================
# aarch64 GICv3 启动压测脚本 (ISSUE-RT-002 复验装备)
#
# 用途: 以多核 (默认 `-smp 2`) 连续启动 aarch64 内核 N 次, 每次都要求串口出现
#       `GICv3 ready` (BSP GIC 就绪) 与 `[SMP] online CPUs: <N>` (各 AP 的
#       per-CPU GIC 就绪并登记上线) 双里程碑, 任一缺失即 fail-closed. 用于把
#       ISSUE-RT-002 (GICv3 偶发挂起) 的"偶发不可复现"转为可重复的**真多核**
#       启动压力测试; 并可用核数/附加参数变换时序以辅助复现.
#
# 说明: 本脚本只断言 GIC/多核启动里程碑. 更完整的启动路径断言 (VFS ready /
#       EL0 / KPTI) 由 scripts/qemu_boot_test.sh 承担.
#
# 用法:
#   ./scripts/gic_stress_test.sh [次数]        # 默认 50 次
# 环境变量:
#   GIC_STRESS_TIMEOUT    单次启动等待里程碑上限秒数 (默认 20)
#   GIC_STRESS_KEEP_LOG   1 = 保留每次串口日志 (默认仅保留失败日志)
#   GIC_STRESS_SMP        核数 (默认 2); 里程碑随之变为 `[SMP] online CPUs: <N>`
#   GIC_STRESS_QEMU_EXTRA 追加 QEMU 参数 (如 `-accel tcg,thread=single` /
#                         `-icount shift=6,align=off,sleep=off`), 用于变换时序
#
# 退出码: 0 = N/N 全部通过, 1 = 存在失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ITER="${1:-50}"
TIMEOUT="${GIC_STRESS_TIMEOUT:-20}"
KEEP_LOG="${GIC_STRESS_KEEP_LOG:-0}"
SMP="${GIC_STRESS_SMP:-2}"
QEMU_EXTRA="${GIC_STRESS_QEMU_EXTRA:-}"
MARKER="GICv3 ready"
# 方括号按字面量匹配需转义 (grep BRE), 与 qemu_boot_test.sh 断言写法一致.
SMP_MARKER="\[SMP\] online CPUs: ${SMP}"

IMAGE="other/build/kernel-aarch64.img"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

if [ ! -f "$IMAGE" ]; then
    echo "[gic-stress] 镜像缺失: $IMAGE (先运行 make ARCH=aarch64 all)" >&2
    exit 1
fi

command -v qemu-system-aarch64 >/dev/null 2>&1 || {
    echo "[gic-stress] 未找到 qemu-system-aarch64" >&2
    exit 1
}

echo "[gic-stress] 目标: $IMAGE  次数: $ITER  单次超时: ${TIMEOUT}s  SMP: ${SMP}"
echo "[gic-stress] 里程碑断言: '$MARKER' && '$SMP_MARKER' (真多核装具)"
[ -n "$QEMU_EXTRA" ] && echo "[gic-stress] 附加 QEMU 参数: $QEMU_EXTRA"

PASS=0
FAIL=0
for i in $(seq 1 "$ITER"); do
    log="$LOG_DIR/gic_stress_iter.log"
    : > "$log"

    # 后台启动 QEMU, 轮询串口日志; 命中里程碑即提前 kill, 避免每次跑满超时.
    # $QEMU_EXTRA 需按空格分词为多个参数, 故不加引号 (本脚本无 -e, 见 set 行).
    qemu-system-aarch64 \
        -M virt,gic-version=3 -cpu max -m 512 -smp "$SMP" \
        $QEMU_EXTRA \
        -kernel "$IMAGE" \
        -serial "file:$log" \
        -display none -no-reboot \
        -device virtio-net-device,netdev=n0 \
        -netdev user,id=n0 \
        >/dev/null 2>&1 &
    qpid=$!

    hit=0
    for _ in $(seq 1 $((TIMEOUT * 10))); do
        # 双里程碑均命中方判通过: 真多核载体要求 AP 的 per-CPU GIC 亦初始化成功
        # (AP GIC init 失败会 halt AP, BSP 超时 ⇒ `online CPUs: 2` 不出现).
        if grep -q "$MARKER" "$log" 2>/dev/null \
            && grep -q "$SMP_MARKER" "$log" 2>/dev/null; then
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
