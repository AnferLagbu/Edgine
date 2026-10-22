#!/bin/bash
# 内核 feature 构建维 release compile-only 检查 (P-51)
#
# 背景: src/kernel 声明 30 个 feature, 六门槛只点亮 kernel_test / host-test.
#   - lockdep 的 76 处 cfg 全部形如 any(debug_assertions, feature = "lockdep"),
#     纯 feature = "lockdep" 零处; 常规 release 构建不带任何 feature ⇒ Cargo.toml 特意
#     保留的「release + lockdep」组合从不构建.
#   - e1000-real-hw 有两套真实 cfg 分支 (read_mac_address), 默认构建只编译
#     not(e1000-real-hw) 侧.
# 本脚本把这两条组合装成回归锁: 谁让这些分支腐化, 门禁转红.
#
# 两个维, 判据与 scripts/check_kernel_test_release_dim.sh 同形 (缺一即失败):
#   1. cargo check --release --target x86_64-unknown-none --features <set> 退出码为 0
#   2. 本 crate 诊断路径下的告警数为 0
# 只编译不跑 ⇒ 不产出覆盖读数; e1000-real-hw 无真硬件时只能证明「可编译」.
# 只覆盖 x86_64 (QEMU 主镜像; 两个 feature 的作用面都在 x86_64 侧), 不扩 aarch64.
#
# 判据 2 以「--> 之后是相对路径」识别本 crate: 本脚本 cwd 固定为 src/kernel, 故本 crate
# 的诊断恒为相对路径 (privileged/... functions/... lib.rs), 依赖链的诊断恒为绝对路径.
# 若将来改成从仓根用 --manifest-path 调用, 本 crate 的诊断会变成绝对路径 ⇒ 该判据静默失效.
#
# cwd 必须为 src/kernel: 该目录 .cargo/config.toml 固化 target-dir (other/target) 与
# CARGO_CFG_CURVE25519_DALEK_BACKEND=serial (LLVM SIGILL 防线).
#
# 退出码: 0 通过 / 1 任一维失败 / 其余透传 cargo.
set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL_DIR="$PROJECT_ROOT/src/kernel"
KERNEL_TARGET=x86_64-unknown-none

FAIL=0
for FEATURE_SET in lockdep e1000-real-hw; do
    OUT="$(cd "$KERNEL_DIR" && cargo check --release --target "$KERNEL_TARGET" --features "$FEATURE_SET" 2>&1)"
    RC=$?

    SELF_WARN="$(printf '%s\n' "$OUT" | grep -cE '^[[:space:]]*--> [^/[:space:]]')"

    printf '%s\n' "$OUT" | tail -n 8

    if [ "$RC" -ne 0 ]; then
        echo "[FAIL] feature 维 '$FEATURE_SET' 编译失败 (exit=$RC)"
        FAIL=1
        continue
    fi

    if [ "$SELF_WARN" -ne 0 ]; then
        echo "[FAIL] feature 维 '$FEATURE_SET' 本 crate 告警 $SELF_WARN 处"
        printf '%s\n' "$OUT" | grep -B2 -E '^[[:space:]]*--> [^/[:space:]]' | head -n 24
        FAIL=1
        continue
    fi

    echo "[OK] feature 维 '$FEATURE_SET': 编译通过 + 本 crate 零告警 (compile-only)"
done

exit "$FAIL"
