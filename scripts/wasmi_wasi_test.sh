#!/bin/bash
# ============================================================================
# P3 WASI 覆盖探针专项 feature 腿 (WASM 退用户态迁移 · 测试覆盖里程碑)
#
# 用途: 以 `KERNEL_FEATURES="wasm_userspace"` 真构建 + 真启动, 断言
#       `scripts/fixtures/wasi_p3_probe.wasm` (wasi-sdk 34 真产物) 在用户态
#       wasmi 运行时 (wasmrun) 下把 P2 未在真机载荷覆盖的 WASI 入口全部跑通:
#
#         - `P3 start argc=N`   探针进入 _start (载体 + exec 分派正常)
#         - `OK <name>`         逐项通过 (每个失败项打印 `FAIL <name> errno=<n>`)
#         - `P3 done fails=0`   载荷总判定 (任何一项失败即 fails>0)
#         - `[WASMI-P3] exit=0` 父进程收割退出码 0
#
#       覆盖增量 (相对 P2 真机载荷):
#         sched_yield / fd_allocate / fd_advise / fd_renumber / fd_dup /
#         path_filestat_set_times / fd_prestat_get+dir_name / fd_seek(SEEK_END) /
#         多条目 fd_readdir / ENOENT·EEXIST·ENOTEMPTY·ENOTDIR·EBADF 错误分支.
#
#       未覆盖项与原因写进 docs/plan/wasm-userspace-migration.md P3 详情:
#       sock_* 需要「运行时预置 socket fd」通道, 而 preview1 无 sock_open,
#       普通 wasi-sdk 载荷结构上拿不到 socket fd, P4 前不做假绿.
#
# 载体: 与 P1 腿同形 (boot-module 交付 cpio; 内核镜像零嵌入).
#
# 跑完复原默认构建 (失败也复原).
#
# 用法:
#   ./scripts/wasmi_wasi_test.sh [x86_64|aarch64|all]   # 默认 all
# 环境变量:
#   WASMI_WASI_TIMEOUT  单次启动等待里程碑上限秒数 (默认 40)
#   WASMI_WASI_SMP      核数 (默认 2)
#
# 退出码: 0 = 全部通过, 1 = 存在失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-all}"
TIMEOUT="${WASMI_WASI_TIMEOUT:-90}"
SMP="${WASMI_WASI_SMP:-2}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

source scripts/boot_module.sh

ok()   { echo -e "\033[0;32m✓ $1\033[0m"; }
err()  { echo -e "\033[0;31m✗ $1\033[0m"; }
info() { echo -e "\033[0;34m-> $1\033[0m"; }

case "$ARCH" in
    x86_64|aarch64|all) ;;
    *) err "用法: $0 [x86_64|aarch64|all]"; exit 1 ;;
esac

ARCH_LIST="$ARCH"
[ "$ARCH" = "all" ] && ARCH_LIST="x86_64 aarch64"

kernel_artifact_of() {
    if [ "$1" = "x86_64" ]; then
        echo "other/build/x86_64/kernel.flat"
    else
        echo "other/build/aarch64/kernel-aarch64.img"
    fi
}

qemu_args_of() {
    local arch="$1"
    if [ "$arch" = "x86_64" ]; then
        echo "-M q35 -m 512 -smp $SMP $(boot_module_carrier_args x86_64)"
    else
        echo "-M virt,gic-version=3 -cpu max -m 512 -smp $SMP $(boot_module_carrier_args aarch64)"
    fi
}

boot_and_check() {
    local arch="$1"
    local log="$LOG_DIR/wasmi_wasi_${arch}.log"
    local image
    image="$(boot_module_image "$arch")"

    if [ ! -f "$image" ]; then
        err "[$arch] 引导产物缺失: $image"
        return 1
    fi
    command -v "qemu-system-$arch" >/dev/null 2>&1 || {
        err "[$arch] 未找到 qemu-system-$arch"
        return 1
    }

    : > "$log"
    # shellcheck disable=SC2046
    "qemu-system-$arch" $(qemu_args_of "$arch") \
        -serial "file:$log" -display none -no-reboot >/dev/null 2>&1 &
    local qpid=$!

    local marker_start="P3 start argc="
    local marker_carrier="init from /init"
    local marker_done="P3 done fails=0"
    local marker_exit="\[WASMI-P3\] exit=0"
    local rc=1
    local _i
    for _i in $(seq 1 $((TIMEOUT * 10))); do
        if grep -aq "$marker_start" "$log" && grep -aq "$marker_done" "$log" \
            && grep -aq "$marker_exit" "$log"; then
            rc=0
            break
        fi
        kill -0 "$qpid" 2>/dev/null || break
        sleep 0.1
    done

    kill "$qpid" 2>/dev/null || true
    wait "$qpid" 2>/dev/null || true

    if ! grep -aq "$marker_carrier" "$log"; then
        err "[$arch] 未出现载体里程碑 '$marker_carrier' (日志: $log)"
        return 1
    fi
    ok "[$arch] PID 1 由载体加载"

    if ! grep -aq "$marker_start" "$log"; then
        err "[$arch] 探针未进入 _start (日志: $log)"
        return 1
    fi
    ok "[$arch] P3 探针进入 _start"

    # 失败明细先打出来, 便于定位而不只是「总判定红」.
    local failed_lines
    failed_lines=$(grep -a '^FAIL ' "$log" || true)
    if [ -n "$failed_lines" ]; then
        echo "$failed_lines" | sed 's/^/    /'
    fi

    if ! grep -aq "$marker_done" "$log"; then
        err "[$arch] 探针总判定未通过 (期望 '$marker_done'; 日志: $log)"
        local done_line
        done_line=$(grep -a 'P3 done fails=' "$log" | tail -1 || true)
        [ -n "$done_line" ] && err "[$arch] 实际: $done_line"
        return 1
    fi
    local ok_count
    ok_count=$(grep -ac '^OK ' "$log" || true)
    ok "[$arch] P3 done fails=0 (OK 项 $ok_count 个)"

    if ! grep -aq "$marker_exit" "$log"; then
        local exit_line
        exit_line=$(grep -a '\[WASMI-P3\] exit=' "$log" | tail -1 || true)
        err "[$arch] 探针未正常退出 (期望 '$marker_exit'; 实际: ${exit_line:-<无>}; 日志: $log)"
        return 1
    fi
    ok "[$arch] [WASMI-P3] exit=0"

    return "$rc"
}

FAILED=0
for arch in $ARCH_LIST; do
    echo "================================================================"
    info "[$arch] KERNEL_FEATURES='wasm_userspace' 构建..."
    if ! make ARCH="$arch" KERNEL_FEATURES="wasm_userspace" all >"$LOG_DIR/wasmi_wasi_build_${arch}.log" 2>&1; then
        err "[$arch] 带 feature 构建失败 (日志: $LOG_DIR/wasmi_wasi_build_${arch}.log)"
        FAILED=$((FAILED + 1))
        continue
    fi
    if [ "$arch" = "x86_64" ] && ! boot_module_iso_x86; then
        err "[x86_64] GRUB module2 ISO 组包失败 (需 grub2-mkrescue)"
        FAILED=$((FAILED + 1))
        continue
    fi
    if ! python3 scripts/verify_image_arch.py --arch "$arch" \
        --image "$(kernel_artifact_of "$arch")" \
        --init "other/build/$arch/user/init.bin"; then
        err "[$arch] 镜像架构自检未通过 — 拒绝在不可信镜像上跑门禁"
        FAILED=$((FAILED + 1))
        continue
    fi
    if ! boot_and_check "$arch"; then
        FAILED=$((FAILED + 1))
    fi
done

for arch in $ARCH_LIST; do
    info "[$arch] 复原默认构建 (KERNEL_FEATURES 空)..."
    if ! make ARCH="$arch" all >"$LOG_DIR/wasmi_wasi_restore_${arch}.log" 2>&1; then
        err "[$arch] 默认构建复原失败 (日志: $LOG_DIR/wasmi_wasi_restore_${arch}.log)"
        FAILED=$((FAILED + 1))
    fi
done

echo "================================================================"
if [ "$FAILED" -eq 0 ]; then
    ok "WASM P3 WASI 覆盖探针全部通过"
    exit 0
fi
err "WASM P3 存在 $FAILED 项失败"
exit 1
