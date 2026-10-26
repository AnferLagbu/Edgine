#!/bin/bash
# 内核测试构建 release 维检查 (P-44)
#
# 目的: 给 P-43 那类「测试侧 cfg 与被测代码掩码不同形」的病灶装一台回归锁.
# 门槛 6 的判据是 debug 配方 (Makefile 的 test-kernel-host, 不带 --release), 而 release
# 测试维此前不由任何门禁编译 ⇒ 把掩码改回去时六门槛照样全绿.
#
# 两条判据, 缺一即失败:
#   1. cargo test --workspace --release --features host-test --lib --no-run 退出码为 0
#      —— 拦 error 类错配 (lockdep 形态: 测试引用被掩码摘除的符号)
#      P-71: privileged 已拆为独立 crate, --workspace 使两个测试目标都受检.
#   2. 本 crate 诊断路径下的告警数为 0
#      —— 拦 warning 类错配 (spinlock 的历史形态: 整条用例不掩 + 内层语句掩 ⇒ unused variable)
#
# 判据 2 以「--> 之后是相对路径」识别本 crate: 本脚本 cwd 固定为 src/kernel, 故本 crate
# 的诊断恒为相对路径 (privileged/... functions/... lib.rs), 依赖链的诊断恒为绝对路径
# (/home/.../.cargo/registry/...). 若将来改成从仓根用 --manifest-path 调用, 本 crate 的
# 诊断会变成绝对路径 ⇒ 该判据静默失效, 届时要同步改这里的模式.
#
# 只编译不执行 ⇒ 不产出 release 覆盖读数. release 维比 debug 维少 11 例 (10 条 lockdep
# + 1 条 spinlock), lockdep 在该维用例数为 0, 不得读成覆盖已达成 (口径见
# docs/plan/net-e2e-tcp-echo-flakiness.md §6 P-43 / P-44).
#
# cwd 必须为 src/kernel: 该目录 .cargo/config.toml 固化 target-dir (other/target) 与
# CARGO_CFG_CURVE25519_DALEK_BACKEND=serial (LLVM SIGILL 防线).
#
# 退出码: 0 通过 / 1 本 crate 告警非零 / 其余透传 cargo.
set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL_DIR="$PROJECT_ROOT/src/kernel"

OUT="$(cd "$KERNEL_DIR" && cargo test --workspace --release --features host-test --lib --no-run 2>&1)"
RC=$?

SELF_WARN="$(printf '%s\n' "$OUT" | grep -cE '^[[:space:]]*--> [^/[:space:]]')"

printf '%s\n' "$OUT" | tail -n 12

if [ "$RC" -ne 0 ]; then
    echo "[FAIL] release 测试维编译失败 (exit=$RC) — 测试侧 cfg 与被测代码掩码不同形 (P-43 病灶)"
    exit "$RC"
fi

if [ "$SELF_WARN" -ne 0 ]; then
    echo "[FAIL] release 测试维本 crate 告警 $SELF_WARN 处 (P-44 判据 2) — 详见上方诊断"
    printf '%s\n' "$OUT" | grep -B2 -E '^[[:space:]]*--> [^/[:space:]]' | head -n 24
    exit 1
fi

echo "[OK] release 测试维: 编译通过 + 本 crate 零告警 (compile-only, 不作为覆盖读数)"
