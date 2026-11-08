#!/bin/bash
# ============================================================================
# lockdep 构建面探针 (P-62 甲 feature 运行维第五项, #125)
#
# 用途: 证明本架构**出货镜像**里真有锁依赖检测体系, 且 `KERNEL_FEATURES=lockdep`
#       显式请求不改变这一事实. A-1 (2026-11-08 裁定甲「全量常开」) 起 `lockdep`
#       在默认 features 里 —— 默认构建即检测构建, 判据的默认侧期望随之由 0 重指为 1
#       (该重指是同批裁定的附带后果, 见 docs/plan/multithreading-project.md §8.1 A-1).
#
# 判据 (用户 2026-11-05 裁定 ⑤ 的双侧形态, 默认侧期望经 A-1 重指):
#   lockdep 构建: strings -a <本架构镜像> | grep -ac "circular dependency" == 1
#   默认构建:     同命令 == 1
# 两侧各司一职: 正向钉「显式 feature 请求不会把检测体系编丢或编重」; 默认侧钉
# 「常开真落地」—— 转发链 (kernel/Cargo.toml → privileged) 任一环断掉, 默认镜像
# 就不再含检测体系, 本腿红.
#
# A-1 之前的反向判据 (「该串确实由 feature 门控」: 默认构建 == 0) 随常开退役 ——
# 出货面已没有「默认关闭」的构建, 该性质只能在 `--no-default-features` 构建上观测,
# 不在本腿射程内 (边界见 §8.1 A-1 行). 探针串仍只在 lockdep.rs 的检测分支里出现.
#
# 为什么不是启动维: 启动日志 capabilities 行不含 lockdep 位 (只有
# smp/preempt/kaslr/kpti/freg), 起一趟只能证「还能启动」, 证不了检测体系真在
# 镜像里.
#
# 用法:
#   ./scripts/lockdep_feature_probe.sh [x86_64|aarch64|all]   # 默认 all
# 环境变量:
#   LOCKDEP_PROBE_MARKER  探针字符串 (默认 "circular dependency")
#   LOCKDEP_PROBE_ON      lockdep 构建期望段数 (默认 1)
#   LOCKDEP_PROBE_OFF     默认构建期望段数 (默认 1, A-1 常开后)
#
# 退出码: 0 = 双侧判据全部符合, 1 = 任一项不符或构建/自检失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-all}"
MARKER="${LOCKDEP_PROBE_MARKER:-circular dependency}"
EXPECT_ON="${LOCKDEP_PROBE_ON:-1}"
EXPECT_OFF="${LOCKDEP_PROBE_OFF:-1}"
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
info "默认侧腿: 复原默认构建 (KERNEL_FEATURES 空, 探针串期望 $EXPECT_OFF 段)"
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
        ok "[$arch] 默认构建含探针段 ($off/$EXPECT_OFF) — 已复原默认镜像"
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
ok "lockdep 构建面探针全部通过: 双架构正向/默认两侧符合 (P-62 #125)"
exit 0
