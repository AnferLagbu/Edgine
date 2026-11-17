#!/bin/bash
# ============================================================================
# P1 用户态 wasmi stdio demo 专项 feature 腿 (WASM 退用户态迁移首里程碑)
#
# 用途: 以 `KERNEL_FEATURES="wasm_userspace"` 真构建 + 真启动, 断言一个
#       wasi-sdk 编译的真实工具链 wasm 模块 (hello_wasi.wasm) 在**用户态 wasmi
#       运行时** (wasmrun) 下 stdio 输出正确且正常退出 —— 迁移可行性 de-risk.
#
#       `wasm_userspace` 是默认关闭的 feature: 默认 exec 读 `/bin/wasmstub`
#       (内核解释器桩), 点亮后改读 `/bin/wasmrun` (见 functions/wasm/stub.rs
#       `stub_path()` 选桩). 本腿与 K-07b 载体腿同形 (真构建 + 真启动 + 断言
#       里程碑), 但消费的是 P1 新增的 `[WASMI]` 探针而非既有 `[WASM]` 探针:
#         1. `init from /init` —— PID 1 由载体加载 (探针载荷来自 cpio);
#         2. `hello from wasmi` —— 模块 fd_write 经 wasmrun host 层翻译到 write
#            syscall, stdio 直达串口 (wasmi 真在用户态解释执行);
#         3. `[WASMI] exit=0` —— 模块正常结束 (proc_exit(0)), 父进程收割退出码 0.
#       三条缺一即红. 证明力来自选桩: 本腿以 wasm_userspace 构建, exec 的 \0asm
#       派发只读 `/bin/wasmrun`, 故 stdio 输出必出自 wasmi. 载荷取最小 WASI 子集
#       (fd_write + proc_exit), 与内核解释器桩兼容, 但默认载体腿不消费 [WASMI].
#
# 载体: D4 严格零嵌入后内核镜像不含任何用户态字节 (init / cpio / 两 wasm 桩),
#       故本腿必须经 boot-module 交付 cpio —— 复用 `scripts/boot_module.sh`
#       (x86_64 GRUB multiboot2 + module2 ISO; aarch64 `-kernel Image -initrd`).
#       裸 `-kernel flat` 不交付模块 (D1 实测), 起不来 PID 1.
#
# 跑完复原默认构建: 本脚本切换过 KERNEL_FEATURES, 若不复原后续门禁会跑在带
# feature 的镜像上. 复原放在判定之后, 失败也已复原.
#
# 用法:
#   ./scripts/wasmi_boot_test.sh [x86_64|aarch64|all]   # 默认 all
# 环境变量:
#   WASMI_BOOT_TIMEOUT  单次启动等待里程碑上限秒数 (默认 40)
#   WASMI_BOOT_SMP      核数 (默认 2)
#
# 退出码: 0 = 全部通过, 1 = 存在失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-all}"
TIMEOUT="${WASMI_BOOT_TIMEOUT:-40}"
SMP="${WASMI_BOOT_SMP:-2}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

# D4: boot-with-module helper (ISO 组包 + 载体启动片段单源)
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

# 内核本体产物 (供 verify_image_arch 校验用户态产物的架构一致).
kernel_artifact_of() {
    if [ "$1" = "x86_64" ]; then
        echo "other/build/x86_64/kernel.flat"
    else
        echo "other/build/aarch64/kernel-aarch64.img"
    fi
}

qemu_of() { echo "qemu-system-$1"; }

qemu_args_of() {
    local arch="$1"
    if [ "$arch" = "x86_64" ]; then
        echo "-M q35 -m 512 -smp $SMP $(boot_module_carrier_args x86_64)"
    else
        echo "-M virt,gic-version=3 -cpu max -m 512 -smp $SMP $(boot_module_carrier_args aarch64)"
    fi
}

# 一次 feature 启动: 起 QEMU, 轮询三条判据, 全齐即提前收尾.
boot_and_check() {
    local arch="$1"
    local log="$LOG_DIR/wasmi_boot_${arch}.log"
    local image
    image="$(boot_module_image "$arch")"

    if [ ! -f "$image" ]; then
        err "[$arch] 引导产物缺失: $image"
        return 1
    fi
    command -v "$(qemu_of "$arch")" >/dev/null 2>&1 || {
        err "[$arch] 未找到 $(qemu_of "$arch")"
        return 1
    }

    : > "$log"
    # shellcheck disable=SC2046  # qemu_args_of 输出需按空格分词为多个参数
    "$(qemu_of "$arch")" $(qemu_args_of "$arch") \
        -serial "file:$log" -display none -no-reboot >/dev/null 2>&1 &
    local qpid=$!

    local marker_carrier="init from /init"
    local marker_stdio="hello from wasmi"
    local marker_exit="\[WASMI\] exit=0"
    local rc=1
    local _i
    # 日志含 NUL (内核日志与探针逐字符并发写串口), 必须 grep -a.
    for _i in $(seq 1 $((TIMEOUT * 10))); do
        if grep -aq "$marker_carrier" "$log" && grep -aq "$marker_stdio" "$log" \
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

    if ! grep -aq "$marker_stdio" "$log"; then
        err "[$arch] 未出现 wasmi stdio 输出 '$marker_stdio' (日志: $log)"
        return 1
    fi
    ok "[$arch] wasmi stdio 输出: $(grep -a "$marker_stdio" "$log" | tail -1)"

    if ! grep -aq "$marker_exit" "$log"; then
        err "[$arch] wasmi 探针未正常退出 '$marker_exit' (日志: $log)"
        return 1
    fi
    ok "[$arch] wasmi 进程级载体端到端通过 (P1)"

    return "$rc"
}

FAILED=0
for arch in $ARCH_LIST; do
    echo "================================================================"
    info "[$arch] KERNEL_FEATURES='wasm_userspace' 构建..."
    if ! make ARCH="$arch" KERNEL_FEATURES="wasm_userspace" all >"$LOG_DIR/wasmi_build_${arch}.log" 2>&1; then
        err "[$arch] 带 feature 构建失败 (日志: $LOG_DIR/wasmi_build_${arch}.log)"
        FAILED=$((FAILED + 1))
        continue
    fi
    # x86_64: cpio 不嵌入内核, 须由 GRUB 作 module2 交付.
    if [ "$arch" = "x86_64" ] && ! boot_module_iso_x86; then
        err "[x86_64] GRUB module2 ISO 组包失败 (需 grub2-mkrescue)"
        FAILED=$((FAILED + 1))
        continue
    fi
    # S-6 防线③: 启动前先确认用户态产物的架构与本腿一致
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

# 复原默认构建 (KERNEL_FEATURES 空 ⇒ Makefile 内容戳记失配触发重建)
for arch in $ARCH_LIST; do
    info "[$arch] 复原默认构建 (KERNEL_FEATURES 空)..."
    if ! make ARCH="$arch" all >"$LOG_DIR/wasmi_restore_${arch}.log" 2>&1; then
        err "[$arch] 默认构建复原失败 (日志: $LOG_DIR/wasmi_restore_${arch}.log)"
        FAILED=$((FAILED + 1))
    fi
done

echo "================================================================"
if [ "$FAILED" -ne 0 ]; then
    err "wasmi stdio demo 腿失败: $FAILED 项 (P1)"
    exit 1
fi
ok "wasmi stdio demo 腿全部通过 (P1)"
exit 0