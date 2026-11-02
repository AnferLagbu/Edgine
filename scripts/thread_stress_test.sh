#!/bin/bash
# ============================================================================
# 线程组启动压测脚本 (K-07a 复验装备)
#
# 用途: 以多核 (默认 `-smp 2`) 连续启动内核 N 次, 每次都要求串口出现 init 的
#       `[THREAD] PASS` 里程碑 —— 该里程碑由 src/user/init 的线程组探针打印,
#       一次启动即覆盖 clone 建线程 + TID 回填、每线程独立 TLS、gettid/getpid
#       身份、tgkill/kill 的 sig==0 存在性探测、futex 双向往返 +
#       CLONE_CHILD_CLEARTID join、exit_group 终止整组六条判据.
#
#       多线程缺陷 (丢唤醒、join 竞态、跨核时序) 天然是偶发的, 单次启动不足以
#       判稳; 本脚本把「偶发不可复现」转为可重复的真多核压力. 两架构都跑:
#       历史上 aarch64 侧的线程栈权限与致命信号收口都是只有真跑才现形的缺陷.
#
# 说明: 本脚本只断言线程组里程碑. 其余启动路径断言 (VFS ready / GICv3 / KPTI /
#       EL0) 由 scripts/qemu_boot_test.sh 承担.
#
# 用法:
#   ./scripts/thread_stress_test.sh [次数]        # 默认 50 次 (每架构各 50 次)
# 环境变量:
#   THREAD_STRESS_TIMEOUT    单次启动等待里程碑上限秒数 (默认 25)
#   THREAD_STRESS_KEEP_LOG   1 = 保留每次串口日志 (默认仅保留失败日志)
#   THREAD_STRESS_SMP        核数 (默认 2); 单核时线程腿退化为串行, 覆盖不到
#                            跨核交替, 故默认取 2
#   THREAD_STRESS_ARCH       x86_64 | aarch64 | both (默认 both)
#   THREAD_STRESS_QEMU_EXTRA 追加 QEMU 参数 (如 `-accel tcg,thread=single` /
#                            `-icount shift=6,align=off,sleep=off`), 用于变换时序
#
# 退出码: 0 = 全部通过, 1 = 存在失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ITER="${1:-50}"
TIMEOUT="${THREAD_STRESS_TIMEOUT:-25}"
KEEP_LOG="${THREAD_STRESS_KEEP_LOG:-0}"
SMP="${THREAD_STRESS_SMP:-2}"
ARCHS="${THREAD_STRESS_ARCH:-both}"
QEMU_EXTRA="${THREAD_STRESS_QEMU_EXTRA:-}"
# 方括号按字面量匹配需转义 (grep BRE), 与 qemu_boot_test.sh 断言写法一致.
MARKER="\[THREAD\] PASS:"

case "$ARCHS" in
    x86_64) ARCH_LIST="x86_64" ;;
    aarch64) ARCH_LIST="aarch64" ;;
    both) ARCH_LIST="x86_64 aarch64" ;;
    *)
        echo "[thread-stress] THREAD_STRESS_ARCH 取值非法: $ARCHS (x86_64 | aarch64 | both)" >&2
        exit 1
        ;;
esac

LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

# 单架构的一次启动: 后台起 QEMU, 轮询里程碑, 命中即提前 kill.
# $QEMU_EXTRA 需按空格分词为多个参数, 故不加引号 (本脚本无 -e, 见 set 行).
boot_once() {
    local arch="$1" log="$2"
    local image qemu_bin
    local -a args
    if [ "$arch" = "x86_64" ]; then
        image="other/build/x86_64/kernel.flat"
        qemu_bin="qemu-system-x86_64"
        args=(-M q35 -m 512 -smp "$SMP" -kernel "$image")
    else
        image="other/build/aarch64/kernel-aarch64.img"
        qemu_bin="qemu-system-aarch64"
        args=(-M virt,gic-version=3 -cpu max -m 512 -smp "$SMP" -kernel "$image")
    fi

    if [ ! -f "$image" ]; then
        echo "[thread-stress] 镜像缺失: $image (先运行 ./ci/build.sh all)" >&2
        return 2
    fi
    command -v "$qemu_bin" >/dev/null 2>&1 || {
        echo "[thread-stress] 未找到 $qemu_bin" >&2
        return 2
    }

    : > "$log"
    "$qemu_bin" "${args[@]}" $QEMU_EXTRA \
        -serial "file:$log" -display none -no-reboot \
        >/dev/null 2>&1 &
    local qpid=$!

    local hit=1
    for _ in $(seq 1 $((TIMEOUT * 10))); do
        # 用 -a: 内核日志与探针逐字符并发写串口会插入 NUL 字节, 否则 grep 按
        # 二进制处理, 匹配行不输出.
        if grep -aq "$MARKER" "$log" 2>/dev/null; then
            hit=0
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
    return "$hit"
}

TOTAL=0
FAILED=0
echo "[thread-stress] 次数: $ITER/架构  架构: $ARCH_LIST  单次超时: ${TIMEOUT}s  SMP: ${SMP}"
echo "[thread-stress] 里程碑断言: '$MARKER' (init 线程组探针六条判据)"
[ -n "$QEMU_EXTRA" ] && echo "[thread-stress] 附加 QEMU 参数: $QEMU_EXTRA"

for arch in $ARCH_LIST; do
    echo "[$arch]"
    for i in $(seq 1 "$ITER"); do
        TOTAL=$((TOTAL + 1))
        log="$LOG_DIR/thread_stress_${arch}_iter.log"
        if boot_once "$arch" "$log"; then
            printf '  [%3d/%3d] \033[0;32mPASS\033[0m\n' "$i" "$ITER"
            [ "$KEEP_LOG" = "1" ] || rm -f "$log"
        else
            FAILED=$((FAILED + 1))
            fail_log="$LOG_DIR/thread_stress_${arch}_fail_${i}.log"
            cp "$log" "$fail_log" 2>/dev/null || true
            printf '  [%3d/%3d] \033[0;31mFAIL\033[0m  (日志: %s)\n' "$i" "$ITER" "$fail_log"
        fi
    done
done

echo "--------------------------------------------"
PASS_COUNT=$((TOTAL - FAILED))
echo "[thread-stress] 结果: ${PASS_COUNT}/${TOTAL} 通过, ${FAILED} 失败"
if [ "$FAILED" -ne 0 ]; then
    echo "[thread-stress] ❌ 存在失败 (K-07a 线程组判据复验未通过)"
    exit 1
fi
echo "[thread-stress] ✅ 全部通过"
exit 0
