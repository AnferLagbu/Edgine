#!/bin/bash
# ============================================================================
# lockdep 构建面探针 (P-62 甲 feature 运行维第五项, #125)
#
# 用途: 证明 `KERNEL_FEATURES=lockdep` 真把锁依赖检测体系编进了**本架构**镜像.
#       lockdep 是诊断 feature, 默认构建不含, 故「它到底编没编进去」此前只有
#       A-1 相对性能读数的间接证据 —— 转发链 (kernel/Cargo.toml → privileged)
#       任一环断掉, 镜像照样出货, 门禁照样绿.
#
# 判据 (用户 2026-11-05 裁定 ⑤, 双向都判, 单向不判):
#   lockdep 构建: strings -a <本架构镜像> | grep -ac "circular dependency" == 1
#   默认构建:     同命令 == 0
# 两侧各司一职, 缺任一侧都留假绿通道: 正向钉「feature 真编进了本架构镜像」;
# 反向钉「该串确实由 feature 门控」—— 若探针串哪天不再与 lockdep 绑定 (被无条件
# 文本覆盖、或 A-1 选常开使默认构建也带上它), 只判正向时读数仍是 1, 腿照样绿.
#
# 取 lockdep 的 AB-BA 文案作探针: 该串只在 lockdep.rs 的检测分支里出现, 关闭
# feature 时整段不进镜像 (实测双架构 default = 0 / lockdep = 1).
#
# 为什么不是启动维: 启动日志 capabilities 行不含 lockdep 位 (只有
# smp/preempt/kaslr/kpti/freg), 起一趟只能证「还能启动」, 证不了 feature 真在
# 镜像里; 且 aarch64 的 lockdep 镜像实测进用户态后零输出 (#108 乙 采集轮),
# 作运行凭据会稳定红.
#
# 附带后果: 「默认构建 strings == 0」自此是门禁判据 —— 若 A-1 选「lockdep 常开」,
# 反向判据即失效, 本腿须重指 (属判据变更, 需再裁; 已登记 §8.1 A-1/A-2 行).
#
# 用法:
#   ./scripts/lockdep_feature_probe.sh [x86_64|aarch64|all]   # 默认 all
# 环境变量:
#   LOCKDEP_PROBE_MARKER  探针字符串 (默认 "circular dependency")
#   LOCKDEP_PROBE_ON      lockdep 构建期望段数 (默认 1)
#   LOCKDEP_PROBE_OFF     默认构建期望段数 (默认 0)
#
# 退出码: 0 = 双向判据全部符合, 1 = 任一项不符或构建/自检失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-all}"
MARKER="${LOCKDEP_PROBE_MARKER:-circular dependency}"
EXPECT_ON="${LOCKDEP_PROBE_ON:-1}"
EXPECT_OFF="${LOCKDEP_PROBE_OFF:-0}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

ok()   { echo -e "\033[0;32m✓ $1\033[0m"; }
err()  { echo -e "\033[0;31m✗ $1\033[0m"; }
info() { echo -e "\033[0;34m-> $1\033[0m"; }

case "$ARCH" in
    x86_64|aarch64|all) ;;
    *) err "用法: $0 [x86_64|aarch64|all]"; exit 1 ;;
esac

ARCH_LIST="$ARCH"
[ "$ARCH" = "all" ] && ARCH_LIST="x86_64 aarch64"

# strings 缺失时本腿无从判定, 判红而非静默通过 (与「读取失败 fail-closed」口径一致).
command -v strings >/dev/null 2>&1 || {
    err "未找到 strings (binutils), 本腿无法判定"
    exit 1
}

# P-10 后两架构产物分目录, 但**名称不同形**: x86_64 出货 kernel.flat,
# aarch64 出货 kernel-aarch64.img (无 kernel.flat). 判据必须落在各自真被启动的
# 那个产物上, 否则 aarch64 侧会静默读到一个不存在的路径 (计数恒 0 ⇒ 正向永远红,
# 或误指向 kernel.bin 那种非启动产物).
image_of() {
    if [ "$1" = "x86_64" ]; then
        echo "other/build/x86_64/kernel.flat"
    else
        echo "other/build/aarch64/kernel-aarch64.img"
    fi
}

# 探针计数: grep -c 在 0 命中时退出码为 1, 命令替换只取 stdout, 不影响判定.
count_marker() {
    strings -a "$1" | grep -ac "$MARKER"
}

FAILED=0

echo "================================================================"
info "正向腿: KERNEL_FEATURES=lockdep 构建 (探针串 '$MARKER', 期望 $EXPECT_ON 段)"
for arch in $ARCH_LIST; do
    log="$LOG_DIR/lockdep_probe_build_${arch}.log"
    info "[$arch] 构建中 (全量输出: $log)..."
    if ! make ARCH="$arch" KERNEL_FEATURES=lockdep all >"$log" 2>&1; then
        err "[$arch] 带 feature 构建失败 (日志: $log)"
        FAILED=$((FAILED + 1))
        continue
    fi
    image="$(image_of "$arch")"
    if [ ! -f "$image" ]; then
        err "[$arch] 镜像缺失: $image"
        FAILED=$((FAILED + 1))
        continue
    fi
    # strings 判据与架构无关 —— 若本架构路径上躺着异架构产物, 计数照样是 1, 正向
    # 判据会静默绿. 启判前先过 S-6 防线③ (镜像逐字节包含本架构用户态产物).
    if ! python3 scripts/verify_image_arch.py --arch "$arch" --image "$image" \
        --init "other/build/$arch/user/init.bin"; then
        err "[$arch] 镜像架构自检未通过 — 拒绝对不可信产物下判据"
        FAILED=$((FAILED + 1))
        continue
    fi
    on="$(count_marker "$image")"
    if [ "$on" = "$EXPECT_ON" ]; then
        ok "[$arch] lockdep 构建含探针段 $on/$EXPECT_ON ($image)"
    else
        err "[$arch] lockdep 构建探针段数不符 — 期望 $EXPECT_ON, 实测 $on ($image)"
        FAILED=$((FAILED + 1))
    fi
done

echo "================================================================"
info "反向腿: 复原默认构建 (KERNEL_FEATURES 空, 探针串期望 $EXPECT_OFF 段)"
for arch in $ARCH_LIST; do
    log="$LOG_DIR/lockdep_probe_restore_${arch}.log"
    info "[$arch] 复原默认构建 (全量输出: $log)..."
    if ! make ARCH="$arch" all >"$log" 2>&1; then
        err "[$arch] 默认构建复原失败 (日志: $log)"
        FAILED=$((FAILED + 1))
        continue
    fi
    image="$(image_of "$arch")"
    if [ ! -f "$image" ]; then
        err "[$arch] 镜像缺失: $image"
        FAILED=$((FAILED + 1))
        continue
    fi
    off="$(count_marker "$image")"
    if [ "$off" = "$EXPECT_OFF" ]; then
        ok "[$arch] 默认构建不含探针段 ($off/$EXPECT_OFF) — 已复原默认镜像"
    else
        err "[$arch] 默认构建探针段数不符 — 期望 $EXPECT_OFF, 实测 $off ($image)"
        FAILED=$((FAILED + 1))
    fi
done

echo "================================================================"
if [ "$FAILED" -ne 0 ]; then
    err "lockdep 构建面探针失败: $FAILED 项 (P-62 #125)"
    exit 1
fi
ok "lockdep 构建面探针全部通过: 双架构正/负双向符合 (P-62 #125)"
exit 0
