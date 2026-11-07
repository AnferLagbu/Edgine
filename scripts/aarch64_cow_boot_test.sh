#!/bin/bash
# ============================================================================
# aarch64 缺页 / COW 恢复路径真跑腿 (#108 丙 专项门禁, 用户 2026-11-07 裁定 丙)
#
# 用途: 默认构建 (KERNEL_FEATURES 空) + 真启动, 断言 aarch64 的"COW 写保护 → 复制/
#       就地恢复 → 页表与帧记账 → 返回原 ELR 重试"整链真在跑, 而不是只在 host 侧
#       被形状锁钉住. host 侧锁 (`host-tests/tests/aarch64_cow_fault_wiring_test.rs`)
#       证不了机器码语义 —— 位极性写反、表根取错、重试链断掉, 四种形态都能编译且
#       host 全绿, 只在真机上表现为"父子写同一物理页"或"内核故障停机".
#
#       判据四条 (缺一即红):
#         1. `[COW] PASS: cow-copy+cow-reuse+stack-grow` —— init 三腿探针全过:
#            `copy` 腿 (计数 2 ⇒ 复制新页, 子侧读到父预写的值)、`reuse` 腿 (子页表
#            拆除后计数回到 1 ⇒ 父侧就地恢复可写)、`stack` 腿 (深栈落到未映射页 ⇒
#            按需分页真增长). 三腿各自靠父进程 wait4 收割子进程退出码判定.
#         2. 无 `[COW] FAIL` 行 —— 任一腿红即红, 不被 PASS 行掩盖.
#         3. `SYNC! ESR=0000000096*` **硬零** —— EC=0x25 (同异常级数据异常) 即
#            "未被恢复的内核故障". ARM 的 AP[2] 同时挡 EL1 写, fork 置只读后核态
#            uaccess 写用户页就是这一形; 本批的窄支路若断, 现象就是 init 在
#            `write_struct_to_user` 的 `str` 上停机 (真机读数 0x9600004F).
#         4. `SYNC! ESR=0000000092*` **>=1** 且 `[KPTI] EL0 kernel high-half access
#            denied` —— 反向判据 (射程不得放宽): EC=0x24 的 EL0 真·非法访问必须仍走
#            "终止进程", 而不是被当成缺页无限重试. 只判 1-3 时, 把恢复射程写成
#            "任何 EL0 故障都 Fixed"同样能全绿, 而那正是缺页风暴.
#
# 与既有门禁的关系: 本腿只新增判定, 不改 `scripts/qemu_boot_test.sh` 的任何既有
# 判据 (用户裁定 丙 的约束面). `SYNC!` 行的语义在 #128 施工 B 后已收窄为"未被恢复
# 的同步异常专属签名", 本腿把该语义钉成硬零判据.
#
# 用法:
#   ./scripts/aarch64_cow_boot_test.sh [aarch64|all]      # 默认 aarch64
#   # x86_64 明确拒绝 (非"跳过"): init 的 cow_probe 是 aarch64 专属, 在 x86_64 上
#   # 跑本腿会得到零判据的空绿.
# 环境变量:
#   AARCH64_COW_BOOT_TIMEOUT  等待 `[COW] PASS` 里程碑的上限秒数 (默认 60)
#   AARCH64_COW_BOOT_SMP      核数 (默认 2, 与既有 aarch64 启动门禁同口径)
#
# 退出码: 0 = 四条判据全符合, 1 = 任一判据红
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-aarch64}"
TIMEOUT="${AARCH64_COW_BOOT_TIMEOUT:-60}"
SMP="${AARCH64_COW_BOOT_SMP:-2}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

ok()   { echo -e "\033[0;32m✓ $1\033[0m"; }
err()  { echo -e "\033[0;31m✗ $1\033[0m"; }
info() { echo -e "\033[0;34m-> $1\033[0m"; }

case "$ARCH" in
    aarch64|all) ;;
    x86_64)
        err "本腿判据对象是 aarch64 专属探针 (init 的 cow_probe 仅 #[cfg(target_arch = \"aarch64\")])"
        exit 1
        ;;
    *) err "用法: $0 [aarch64|all]"; exit 1 ;;
esac

IMAGE="other/build/aarch64/kernel-aarch64.img"
BOOT_LOG="$LOG_DIR/aarch64_cow_boot.log"
BUILD_LOG="$LOG_DIR/aarch64_cow_build.log"

# ── 构建 (默认口径, 不带 feature) ───────────────────────────────────────────
info "[aarch64] 默认构建 (KERNEL_FEATURES 空)..."
if ! make ARCH=aarch64 all >"$BUILD_LOG" 2>&1; then
    err "[aarch64] 构建失败 (日志: $BUILD_LOG)"
    exit 1
fi
# S-6 防线: 启动前先确认镜像内嵌的本架构用户态产物一致 (探针就在 init.bin 里)
if ! python3 scripts/verify_image_arch.py --arch aarch64 \
    --image "$IMAGE" \
    --init "other/build/aarch64/user/init.bin"; then
    err "[aarch64] 镜像架构自检未通过 — 拒绝在不可信镜像上跑门禁"
    exit 1
fi
command -v qemu-system-aarch64 >/dev/null 2>&1 || {
    err "未找到 qemu-system-aarch64"
    exit 1
}

# ── 启动 + 轮询里程碑 ───────────────────────────────────────────────────────
: > "$BOOT_LOG"
qemu-system-aarch64 \
    -M virt,gic-version=3 -cpu max -m 512 -smp "$SMP" -kernel "$IMAGE" \
    -serial "file:$BOOT_LOG" -display none -no-reboot >/dev/null 2>&1 &
QPID=$!

# 日志含 NUL (内核日志与探针逐字符并发写串口), 必须 grep -a, 否则按二进制处理时
# 匹配行不输出.
DONE_MARKER='[COW] PASS: cow-copy+cow-reuse+stack-grow'
FAIL_MARKER='[COW] FAIL'
_dead() { ! kill -0 "$QPID" 2>/dev/null; }
for _i in $(seq 1 $((TIMEOUT * 10))); do
    # 里程碑齐 (PASS 或 FAIL) 即提前收尾; 进程自己退出也收尾 (停机形态).
    if grep -qaF "$DONE_MARKER" "$BOOT_LOG" || grep -qaF "$FAIL_MARKER" "$BOOT_LOG" || _dead; then
        break
    fi
    sleep 0.1
done
kill "$QPID" 2>/dev/null || true
wait "$QPID" 2>/dev/null || true

# ── 判定 ────────────────────────────────────────────────────────────────────
FAILED=0

# 计数取数: 结果必须是非负十进制读数. grep 真错 (日志不可读等) 时输出为空/非数字,
# 此时**不得**读成 "0 条" —— 硬零判据会把"测不出"当成"通过".
count_of() {
    local pattern="$1"
    local n
    n=$(grep -caF "$pattern" "$BOOT_LOG" 2>/dev/null || true)
    case "$n" in
        '' | *[!0-9]*) return 1 ;;
        *) printf '%s\n' "$n" ;;
    esac
}

if grep -qaF "$DONE_MARKER" "$BOOT_LOG"; then
    ok "[aarch64] COW 三腿全过: $(grep -aF "$DONE_MARKER" "$BOOT_LOG" | tail -1)"
else
    err "[aarch64] 未出现 '$DONE_MARKER' —— 缺页/COW 恢复链未真跑 (日志: $BOOT_LOG)"
    FAILED=$((FAILED + 1))
fi

if grep -qaF "$FAIL_MARKER" "$BOOT_LOG"; then
    err "[aarch64] 探针报失败腿: $(grep -aF "$FAIL_MARKER" "$BOOT_LOG" | tail -1)"
    FAILED=$((FAILED + 1))
fi

# EC=0x25 (同 EL 数据异常) 硬零: 核态故障必须全被识别, 一条未恢复即红.
if ! A64_KERNEL_FAULTS=$(count_of "SYNC! ESR=0000000096"); then
    err "[aarch64] EC=0x25 判据取数失败 (日志: $BOOT_LOG) —— 测不出不等于超线为零"
    FAILED=$((FAILED + 1))
elif [ "$A64_KERNEL_FAULTS" -eq 0 ]; then
    ok "[aarch64] 未被恢复的核态同步异常硬零 (EC=0x25)"
else
    err "[aarch64] 出现 $A64_KERNEL_FAULTS 条 EC=0x25 同 EL 数据异常 —— \
核态 uaccess 破 COW 支路断链 (日志: $BOOT_LOG)"
    FAILED=$((FAILED + 1))
fi

# 反向判据: EL0 真·非法访问 (KPTI-09 探针) 必须仍走 terminate, 证明恢复射程没被放宽.
if ! A64_EL0_FAULTS=$(count_of "SYNC! ESR=0000000092"); then
    err "[aarch64] EC=0x24 判据取数失败 (日志: $BOOT_LOG) —— 测不出不等于在射程内"
    FAILED=$((FAILED + 1))
elif [ "$A64_EL0_FAULTS" -ge 1 ] \
    && grep -qaF "[KPTI] EL0 kernel high-half access denied" "$BOOT_LOG"; then
    ok "[aarch64] EL0 真·非法访问仍在终止面 ($A64_EL0_FAULTS 条 EC=0x24 + KPTI 收割行)"
else
    err "[aarch64] 射程反向判据失败: EC=0x24 读数 ${A64_EL0_FAULTS:-?} 条, \
KPTI 收割行缺失 —— 恢复射程被放宽会把真·非法缺页吞成无限重试 (日志: $BOOT_LOG)"
    FAILED=$((FAILED + 1))
fi

echo "================================================================"
if [ "$FAILED" -ne 0 ]; then
    err "aarch64 缺页/COW 真跑腿失败: $FAILED 项 (#108 丙)"
    exit 1
fi
ok "aarch64 缺页/COW 真跑腿四条判据全符合 (#108 丙)"
exit 0
