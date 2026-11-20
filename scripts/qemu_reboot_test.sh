#!/bin/bash
# ============================================================================
# reboot 双路径真重启腿 (R3: ㊿ x86_64 三重故障复位 / 51 aarch64 PSCI SYSTEM_RESET)
#
# 用途: 用专用 PID 1 载荷 (src/user/reboottest) 真启动两颗架构的内核, 由载荷调
#       `reboot(REBOOT_CMD_RESTART)` 走完整 syscall 链, 并**不带 `-no-reboot`**
#       让 QEMU 在复位后重新引导; 日志里同一份 `/init` 第二次打印请求行, 即证明
#       整机真的复位而不是"停机自旋 / 空转返回".
#
# 为什么必须真机腿:
#   - x86_64 路径是 `lidt` 载 10 字节 IDTR + `int 0` 制造三重故障, host 侧只能锁
#     描述符尺寸, 锁不了"CPU 真的复位";
#   - aarch64 路径是 PSCI `SYSTEM_RESET` 经 HVC/SMC 进固件, 功能号 / conduit /
#     clobber 任一写错都表现为"不复位"或"停机", 只有真启动能区分.
#
# 判据 (缺一即红):
#   1. `[REBOOT] PASS: reboottest requesting restart (cmd=0)` 出现 >= 2 次
#      —— 同一份 /init 在复位后的第二次引导里再次执行;
#   2. 架构对应的进用户态里程碑 (`Entering Ring 3` / `Entering EL0`) 出现 >= 2 次
#      —— 第二次引导真的走到了用户态 (不是复位前重复打印);
#   3. 无 `[REBOOT] FAIL` 行 —— reboot 机制未复位而返回时载荷会显式打该行.
#
# 负控制: 把 `reboot_mechanism` 的命令语义改回 0=停机自旋, 第一轮启动后不再复位,
#   判据 1/2 只到 1 次 ⇒ 超时红. 实测读数见 docs/plan/safety-gap-remediation.md R3.
#
# 用法:
#   ./scripts/qemu_reboot_test.sh [x86_64|aarch64|all]      # 默认 all
# 环境变量:
#   REBOOT_TEST_TIMEOUT   等待第二轮启动的上限秒数 (默认 120)
#   REBOOT_TEST_SMP       核数 (默认 2, 与既有启动腿同口径)
#
# 退出码: 0 = 两架构判据全过, 1 = 任一失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-all}"
TIMEOUT="${REBOOT_TEST_TIMEOUT:-120}"
SMP="${REBOOT_TEST_SMP:-2}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

ok()   { echo -e "${GREEN}\u2713 $1${NC}"; }
err()  { echo -e "${RED}\u2717 $1${NC}"; }
info() { echo -e "${BLUE}-> $1${NC}"; }
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

case "$ARCH" in
    x86_64|aarch64|all) ;;
    *) err "用法: $0 [x86_64|aarch64|all]"; exit 1 ;;
esac

ARCH_LIST="$ARCH"
[ "$ARCH" = "all" ] && ARCH_LIST="x86_64 aarch64"

# 架构对应的进用户态里程碑 (内核侧每轮启动打印一次).
user_mode_marker_of() {
    if [ "$1" = "x86_64" ]; then echo "Entering Ring 3"; else echo "Entering EL0"; fi
}

# 组 x86_64 GRUB ISO: multiboot2 载 kernel.bin + module2 载专用 cpio.
prepare_x86_grub_iso() {
    local idir="other/build/reboottest_isodir"
    rm -rf "$idir"; mkdir -p "$idir/boot/grub"
    cp other/build/x86_64/kernel.bin "$idir/boot/kernel.bin" || return 1
    cp other/build/x86_64/user/reboottest.cpio "$idir/boot/reboottest.cpio" || return 1
    cat > "$idir/boot/grub/grub.cfg" <<'GEOF'
set timeout=0
set default=0
menuentry "Edgine" {
    multiboot2 /boot/kernel.bin
    module2 /boot/reboottest.cpio initramfs
}
GEOF
    command -v grub2-mkrescue >/dev/null 2>&1 || return 1
    grub2-mkrescue -o other/build/x86_64/reboottest.iso "$idir" >/dev/null 2>&1
}

# 一次真重启启动: 起 QEMU (无 -no-reboot), 轮询两条计数判据.
boot_reboot_cycle() {
    local arch="$1"
    local log="$LOG_DIR/qemu_reboot_${arch}.log"
    local marker="[REBOOT] PASS:"
    local umarker
    umarker="$(user_mode_marker_of "$arch")"
    local qemu_bin="qemu-system-${arch}"

    command -v "$qemu_bin" >/dev/null 2>&1 || { err "[$arch] 未找到 $qemu_bin"; return 1; }

    local -a qargs
    if [ "$arch" = "x86_64" ]; then
        qargs=(-M q35 -m 512 -smp "$SMP" -cdrom other/build/x86_64/reboottest.iso)
    else
        qargs=(-M virt,gic-version=3 -cpu max -m 512 -smp "$SMP"
               -kernel other/build/aarch64/kernel-aarch64.img
               -initrd other/build/aarch64/user/reboottest.cpio)
    fi

    rm -f "$log"
    info "[$arch] 启动 QEMU (无 -no-reboot, timeout=${TIMEOUT}s)..."
    "$qemu_bin" -serial "file:$log" -display none "${qargs[@]}" >/dev/null 2>&1 &
    local qpid=$!

    local i cycles boots
    for i in $(seq 1 $((TIMEOUT * 5))); do
        cycles=$(grep -acF "$marker" "$log" 2>/dev/null || true)
        boots=$(grep -acF "$umarker" "$log" 2>/dev/null || true)
        if [ "${cycles:-0}" -ge 2 ] && [ "${boots:-0}" -ge 2 ]; then
            break
        fi
        kill -0 "$qpid" 2>/dev/null || break
        sleep 0.2
    done

    kill "$qpid" 2>/dev/null || true
    wait "$qpid" 2>/dev/null || true

    cycles=$(grep -acF "$marker" "$log" 2>/dev/null || true)
    boots=$(grep -acF "$umarker" "$log" 2>/dev/null || true)

    if grep -aqF "[REBOOT] FAIL" "$log"; then
        err "[$arch] reboot 机制返回而未复位: $(grep -aF "[REBOOT] FAIL" "$log" | head -1)"
        return 1
    fi
    if [ "${cycles:-0}" -lt 2 ]; then
        err "[$arch] 载荷请求行只出现 ${cycles:-0} 次 (<2) —— 整机未在复位后重新引导 (日志: $log)"
        return 1
    fi
    if [ "${boots:-0}" -lt 2 ]; then
        err "[$arch] '$umarker' 只出现 ${boots:-0} 次 (<2) —— 第二轮未进入用户态 (日志: $log)"
        return 1
    fi
    ok "[$arch] 真重启判据通过: 请求行 ${cycles} 次, '$umarker' ${boots} 次"
    return 0
}

FAILED=0
for arch in $ARCH_LIST; do
    echo "================================================================"
    info "[$arch] 构建出货默认内核与 reboottest 载荷..."
    if ! make ARCH="$arch" all >"$LOG_DIR/reboot_build_${arch}.log" 2>&1; then
        err "[$arch] 构建失败 (日志: $LOG_DIR/reboot_build_${arch}.log)"
        FAILED=$((FAILED + 1))
        continue
    fi

    # 专用 cpio: 只含 reboottest 作 /init (其余可选产物缺失即跳过).
    local_src="other/build/${arch}/reboottest_src"
    rm -rf "$local_src"; mkdir -p "$local_src"
    if ! cp "other/build/${arch}/user/reboottest.bin" "$local_src/init.bin"; then
        err "[$arch] reboottest.bin 缺失"
        FAILED=$((FAILED + 1))
        continue
    fi
    if ! python3 scripts/make_initramfs_cpio.py --arch "$arch" --src "$local_src" \
        --out "other/build/${arch}/user/reboottest.cpio"; then
        err "[$arch] 专用 cpio 打包失败"
        FAILED=$((FAILED + 1))
        continue
    fi

    if [ "$arch" = "x86_64" ]; then
        if ! prepare_x86_grub_iso; then
            err "[x86_64] GRUB module2 ISO 组包失败 (需 grub2-mkrescue)"
            FAILED=$((FAILED + 1))
            continue
        fi
    fi

    if ! boot_reboot_cycle "$arch"; then
        FAILED=$((FAILED + 1))
    fi
done

echo "================================================================"
if [ "$FAILED" -ne 0 ]; then
    err "reboot 真重启腿失败: ${FAILED} 项 (R3)"
    exit 1
fi
ok "reboot 真重启腿全部通过 (R3: x86_64 IDT / aarch64 PSCI)"
exit 0
