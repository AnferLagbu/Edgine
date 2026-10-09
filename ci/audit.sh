#!/bin/bash
# Edgine 内核代码审计脚本
# 工具栈: cargo check + clippy (pedantic) + Lockbud + Miri 配置验证
# 用法: ./ci/audit.sh [quick|full]
#
# - quick: clippy + 双架构 check (CI 默认)
# - full:  quick + Lockbud + Miri 严格模式 + SAFETY 覆盖率统计

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

MODE="${1:-quick}"

# 构建模式显式化 (方案 B): 裸机构建显式注入 build-std (src/rust/.cargo/config.toml
# 已删全局 [unstable] build-std). 避免 cwd 隐式加载 — src/rust 目录内 host-target
# 构建会触发 E0152 双 alloc 冲突 (DECISION-021 同族). 详见主计划文档 §10.
BUILD_STD_CFG=(--config 'unstable.build-std=["core","compiler_builtins","alloc"]' --config 'unstable.build-std-features=["compiler-builtins-mem"]')

step() { echo -e "\n${YELLOW}━━━ $1 ━━━${NC}"; }
ok()   { echo -e "${GREEN}✓ $1${NC}"; }
err()  { echo -e "${RED}✗ $1${NC}"; exit 1; }

# ── 0. TCB 安全边界门禁 (M2 里程碑硬约束) ────────────────────
# 服务层 (functions/) 不允许任何 unsafe 代码。这是框内核架构的核心契约。
# check_tcb.sh 是 fail-fast 门禁: 一旦发现立即 exit 1, 整个 audit 终止。
# 历史教训: 2026-06-03 审计发现 check_tcb.sh 正则有 bug (变长 lookbehind),
# 导致 functions/ 出现 8 处 unsafe 仍报 PASS。已修复, 见 tools/check_tcb.sh 顶部注释。
step "0/6 TCB 安全边界门禁 (functions/ 零 unsafe)"
if [ -x "$PROJECT_ROOT/tools/check_tcb.sh" ]; then
    if "$PROJECT_ROOT/tools/check_tcb.sh"; then
        ok "TCB 边界: functions/ 零 unsafe, privileged/ 收敛"
    else
        err "TCB 边界被破坏! 见上方 FAIL 输出"
    fi
else
    err "tools/check_tcb.sh 不存在或不可执行"
fi

# ── 0.5a 产物目录卫生门禁 ────────────────────────────────────────
# 临时/构建产物统一收敛到 other/ 下 (整个 other/ 被 .gitignore 覆盖).
# fail-closed: 若已跟踪文件中混入被忽略产物 (git ls-files -i -c --exclude-standard
# 非空), 说明产物被误提交 (历史上 .arch/target 等曾漏出), 阻断 CI 并提示移除.
step "0.5a/6 产物目录卫生 (tracked ∩ ignored = ∅)"
STALE_TRACKED=$(git ls-files -i -c --exclude-standard)
if [ -z "$STALE_TRACKED" ]; then
    ok "产物目录卫生: 无被跟踪的忽略产物 (全部收敛到 other/)"
else
    echo "$STALE_TRACKED" | head -20
    err "有被跟踪的忽略产物混入仓库 (见上), 需从 git 移除 (git rm --cached <path>)"
fi

# ── 0.5b TCB 度量报告 (E10) ──────────────────────────────────────
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_tcb_ratio.py" ]; then
    step "0.5b/6 TCB 度量报告 (E10)"
    "$PROJECT_ROOT/scripts/audit_tcb_ratio.py" 2>&1 | tail -20
fi

# ── 0.5c 6 安全不变式审计 (E9) ────────────────────────────────────
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_invariants.py" ]; then
    step "0.5c/6 6 安全不变式审计 (E9)"
    if "$PROJECT_ROOT/scripts/audit_invariants.py" 2>&1 | tail -12; then
        ok "6 安全不变式: 全部满足"
    else
        err "6 安全不变式: 有违反! 见上方输出"
    fi
fi

# privileged 全量 SAFETY 注释覆盖审计
# quick 模式也会跑 (核心 fail-fast 门禁, 不需要 Lockbud/Miri 等重工具)
# B01-27 修复: audit_unsafe.py 窗口逻辑缺陷 (属性闭合行/多行属性误报) 已修复,
# 原 127 处"缺失"中 108 处实为工具误报 (已有 SAFETY 注释但未识别),
# 真实缺失 20 处已全部补齐, 基线归零 (0 容忍, 任何缺失即 CI 失败).
EXPECTED_MAX_SAFETY_MISSING=0
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/tools/audit_unsafe.py" ]; then
    step "0.5/6 Privileged SAFETY 注释全量审计 (B01-27 工具修复, 基线 ≤${EXPECTED_MAX_SAFETY_MISSING})"
    AUDIT_RESULT=$("$PROJECT_ROOT/tools/audit_unsafe.py" --summary 2>&1 || true)
    echo "$AUDIT_RESULT" | tail -25
    MISSING=$(echo "$AUDIT_RESULT" | grep -E "缺 SAFETY:" | head -1 | awk '{print $NF}')
    # B01-16 修复: 当脚本输出为空或缺 "缺 SAFETY:" 行时, 视为异常 (fail-closed).
    # 原脚本静默通过 (无 ok 也无 err) 隐藏门禁失效.
    if [ -z "$AUDIT_RESULT" ]; then
        err "audit_unsafe.py 输出为空 (脚本异常或 privileged/ 为空)"
    elif [ -n "$MISSING" ] && [ "$MISSING" -le "$EXPECTED_MAX_SAFETY_MISSING" ]; then
        ok "privileged SAFETY 覆盖 (缺漏 $MISSING ≤ ${EXPECTED_MAX_SAFETY_MISSING} 基线)"
    elif [ -n "$MISSING" ] && [ "$MISSING" -gt "$EXPECTED_MAX_SAFETY_MISSING" ]; then
        err "privileged SAFETY 缺漏 $MISSING > 基线 ${EXPECTED_MAX_SAFETY_MISSING}, 需审查"
    else
        # 输出非空但未匹配 "缺 SAFETY:" 行 — 视为脚本异常
        err "audit_unsafe.py 输出格式异常 (未找到 缺 SAFETY: 行)"
    fi
fi

# I-43: 块设备抽象统一性 audit — 防驱动绕过 proto_block 桥接
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_block_registration.py" ]; then
    step "0.5d/6 块设备单一桥接入口 (I-43)"
    if "$PROJECT_ROOT/scripts/audit_block_registration.py" 2>&1 | tail -10; then
        ok "I-43: 块设备驱动统一通过 proto_block::register_block_device"
    else
        err "I-43: 有块设备驱动绕过 proto_block 桥接! 见上方输出"
    fi
fi

# I-16: functions 层 OnceCell 抽象统一性 audit — 防 functions 绕过 OnceCell 用 spin::Once
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_once_cell.py" ]; then
    step "0.5e/6 functions OnceCell 单一抽象 (I-16)"
    if "$PROJECT_ROOT/scripts/audit_once_cell.py" 2>&1 | tail -8; then
        ok "I-16: functions 统一通过 sync::once::OnceCell"
    else
        err "I-16: 有 functions 模块绕过 OnceCell 抽象用 spin::Once! 见上方输出"
    fi
fi

# I-07: C 风格命名残留 audit — 防 C 类型后缀/C 函数名混入 Rust 代码
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_c_naming.py" ]; then
    step "0.5f/6 C 风格命名残留 (I-07)"
    if "$PROJECT_ROOT/scripts/audit_c_naming.py" 2>&1 | tail -8; then
        ok "I-07: 0 C 风格类型后缀, kmalloc/kfree 仅限 extern \"C\""
    else
        err "I-07: 有 C 风格命名残留! 见上方输出"
    fi
fi

# TD-22: 注释语言一致性 audit — 硬阈值门禁 (违规 > 0 即 CI 失败)
# 已完成全部清理: 1983→0 (2026-06-15). 新代码引入英文段落注释将被阻断.
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_comment_language.py" ]; then
    step "0.5g/6 注释语言一致性 (TD-22, 硬阈值)"
    AUDIT_OUT=$("$PROJECT_ROOT/scripts/audit_comment_language.py" 2>&1 || true)
    if echo "$AUDIT_OUT" | grep -q "PASSED"; then
        ok "TD-22: 注释中文化 100% 完成 (0 违规)"
    else
        VIOLATION_COUNT=$(echo "$AUDIT_OUT" | grep -E "FAILED:" | head -1 | awk '{print $5}')
        FILE_COUNT=$(echo "$AUDIT_OUT" | grep -E "FAILED:" | head -1 | awk '{print $NF}' | tr -d '()')
        err "TD-22: 英文段落注释残留 ${VIOLATION_COUNT} 处, 涉及 ${FILE_COUNT} 文件 (硬阈值, 阻断 CI)"
    fi
fi

# S-14: TLB 失效接收侧三段次序 audit — 防"先 flush 后读代"的假追平
# (结构性次序问题, 运行期不可观测; 以静态 fail-closed 审计确定性覆盖,
#  见 docs/plan/tlb-shootdown-epoch.md §5 注入 2)
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_tlb_receive_order.py" ]; then
    step "0.5h/6 TLB 失效接收侧三段次序 (S-14)"
    if "$PROJECT_ROOT/scripts/audit_tlb_receive_order.py" 2>&1 | tail -8; then
        ok "S-14: 接收侧次序正确 (读代 → 全量失效 → 声明追平)"
    else
        err "S-14: 接收侧三段次序被破坏 (假追平风险)! 见上方输出"
    fi
fi

# FP-06: aarch64 内核「零 FP/SIMD」反汇编白名单审计 — 防 EL1 出现浮点/NEON
# (软浮点 target 只保证编译期不生成 FP 指令, 无法拦截源码/汇编层的回退;
#  以反汇编白名单确定性覆盖, 见 docs/plan/aarch64-kernel-fp-free.md FP-06)
# 注意: 本步骤读取 other/build/kernel.bin, 要求其为最近一次 aarch64 链接的产物
# (双架构共用该输出路径, ./ci/build.sh all 最后链接 x86_64 ⇒ 需先跑 build.sh aarch64).
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_aarch64_kernel_fp_free.py" ]; then
    step "0.5i/6 aarch64 内核零 FP/SIMD (FP-06)"
    FP_AUDIT_OUT=$("$PROJECT_ROOT/scripts/audit_aarch64_kernel_fp_free.py" 2>&1) && FP_AUDIT_RC=0 || FP_AUDIT_RC=$?
    echo "$FP_AUDIT_OUT" | tail -20
    if [ "$FP_AUDIT_RC" -eq 0 ]; then
        ok "FP-06: aarch64 内核白名单外 FP/SIMD = 0"
    else
        err "FP-06: aarch64 内核存在白名单外 FP/SIMD 或不可检查 (fail-closed)! 见上方输出"
    fi
fi

# ── 0.5j 内存安全静态防线 audit (repr(C) / volatile / static mut) ──
# 对应 I2/I5/I6 不变式与 LTO 字段错位防线; 源码级 fail-closed 确定性覆盖.
# 接线说明: 此前三脚本仅本地可跑、未接入 CI; 现纳入 quick 门禁 (见 AGENTS.md §2.2).
# 依赖 set -o pipefail 使 `cmd | tail` 管道正确反映脚本退出码 (B01-16 同源).
for MEMSAFETY_AUDIT in repr_c volatile_access static_mut; do
    if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_${MEMSAFETY_AUDIT}.py" ]; then
        step "0.5j/6 内存安全防线 audit (${MEMSAFETY_AUDIT})"
        if "$PROJECT_ROOT/scripts/audit_${MEMSAFETY_AUDIT}.py" 2>&1 | tail -8; then
            ok "${MEMSAFETY_AUDIT}: 内存安全防线通过"
        else
            err "${MEMSAFETY_AUDIT}: 内存安全防线被破坏! 见上方输出"
        fi
    fi
done

# ── 1. 双架构 check ─────────────────────────────────────────────
step "1/6 双架构 cargo check (x86_64 + aarch64)"
pushd src/rust > /dev/null
unset RUSTC_WRAPPER
for target in x86_64-unknown-none aarch64-unknown-none-softfloat; do
    echo -e "${BLUE}[audit] target=${target}${NC}"
    # 方案 D: kernel 独立 crate, 裸机 check 指向 kernel manifest (edgine 壳仅 host).
    if cargo check --manifest-path ../kernel/Cargo.toml --target "${target}" --target-dir ../../other/target "${BUILD_STD_CFG[@]}" 2>&1 | tail -3; then
        ok "${target}: check passed"
    else
        err "${target}: check FAILED"
    fi
done
popd > /dev/null

# ── 2. Clippy pedantic (x86_64) ────────────────────────────────
step "2/6 Clippy pedantic (x86_64, lib only)"
pushd src/rust > /dev/null
unset RUSTC_WRAPPER
# B01-28 修复: clippy 命令与 CI (ci-x86.yml clippy-pedantic job) 对齐:
#   + --release: dev 模式会编译诊断代码 (nvme similar_names 假阳性等), 与 CI 一致用 release
#   + -A cast_* 四个豁免: DECISION-041 (内核刻意窄化, 显式收窄), CI 亦豁免
#   + 去掉 -W clippy::cargo: 依赖多版本 bitflags 会误报, CI 未启用
#   + -D warnings 保留更强门禁 (任何 warning 阻断, 见 B01-16)
# B01-16 修复: 加 -D warnings 让任何 warning 阻断 CI, 失败走 err 而非仅警告.
# 原代码 `if cmd | tail; then ok; else warn; fi` 中 `tail` 退出 0 总是成功,
# 即使 cargo clippy 失败也被掩盖 (P0-05 类问题).
if cargo clippy --manifest-path ../kernel/Cargo.toml --release --lib --bins --examples --target x86_64-unknown-none --target-dir ../../other/target \
    "${BUILD_STD_CFG[@]}" \
    -- -D warnings -D clippy::pedantic \
    -A clippy::cast_possible_truncation \
    -A clippy::cast_sign_loss \
    -A clippy::cast_possible_wrap \
    -A clippy::cast_precision_loss 2>&1 | tail -10; then
    CLIPPY_RC=${PIPESTATUS[0]}
    if [ "$CLIPPY_RC" -eq 0 ]; then
        ok "clippy pedantic (lib): passed"
    else
        err "clippy pedantic (lib) 失败 (exit=$CLIPPY_RC, 见上方输出)"
    fi
else
    err "clippy pedantic (lib) 执行异常"
fi
popd > /dev/null

# ── 2b. Clippy feature 维 (kernel_test + host-test, J-01/G-02/G-18) ──
# J-01 (2026-09-08): E-03 同源双编译后 feature 门控代码 lint 纳入门槛.
# host target 运行 (避开裸机 build.rs 产物检查), 与 ci-x86.yml clippy-pedantic
# job 对齐. 两维清理后零 unfulfilled 作为门槛 (G-02 合并).
step "2b/6 Clippy feature 维 (kernel_test + host-test, host target)"
for FEATURE in kernel_test host-test; do
    # 方案 D: feature 维 clippy 指向 kernel manifest (host target, 门控代码 lint)
    if cargo clippy --manifest-path "$PROJECT_ROOT/src/kernel/Cargo.toml" --features "$FEATURE" --lib \
        --target-dir "$PROJECT_ROOT/other/target" \
        -- -D warnings -D clippy::pedantic \
        -A clippy::cast_possible_truncation \
        -A clippy::cast_sign_loss \
        -A clippy::cast_possible_wrap \
        -A clippy::cast_precision_loss 2>&1 | tail -10; then
        CLIPPY_RC=${PIPESTATUS[0]}
        if [ "$CLIPPY_RC" -eq 0 ]; then
            ok "clippy ${FEATURE} 维: passed"
        else
            err "clippy ${FEATURE} 维失败 (exit=$CLIPPY_RC, 见上方输出)"
        fi
    else
        err "clippy ${FEATURE} 维执行异常"
    fi
done

# ── 2c. rustfmt 风格门禁 (全仓四 crate) ────────────────────────
# 长期修复 (2026-10-03): 此前本地 audit 不跑 fmt, 导致 kernel 39 hunks 漂移长期
# 不可见 (仅 CI 暴露且已被忽略). 现与 ci-x86.yml clippy-pedantic job 对齐, 覆盖
# kernel / host-tests / edgine 壳; rustfmt.toml 与 rust-toolchain.toml 均在仓库根.
# 补入 src/user (2026-10-09): 用户态七 crate 此前不受任何 fmt 门禁, 漂移静默积累.
step "2c/6 rustfmt 风格门禁 (kernel + host-tests + edgine 壳 + user 态)"
for FMT_MANIFEST in src/kernel/Cargo.toml host-tests/Cargo.toml src/rust/Cargo.toml src/user/Cargo.toml; do
    if cargo fmt --manifest-path "$FMT_MANIFEST" -- --check; then
        ok "fmt: ${FMT_MANIFEST} 通过"
    else
        err "fmt: ${FMT_MANIFEST} 存在格式漂移 — 运行 cargo fmt --manifest-path ${FMT_MANIFEST} 修复"
    fi
done

if [ "$MODE" = "quick" ]; then
    echo -e "\n${GREEN}━━━ audit (quick) 完成 ━━━${NC}"
    exit 0
fi

# ── 3. SAFETY 注释覆盖率统计 ────────────────────────────────────
step "3/6 SAFETY 注释覆盖率统计"
KERNEL_DIR="$PROJECT_ROOT/src/kernel"
TOTAL_UNSAFE=$(grep -rcE "unsafe \{" "$KERNEL_DIR" --include="*.rs" 2>/dev/null | awk -F: '{s+=$2} END{print s}')
TOTAL_SAFETY=$(grep -rcE "// SAFETY:" "$KERNEL_DIR" --include="*.rs" 2>/dev/null | awk -F: '{s+=$2} END{print s}')
if [ "$TOTAL_UNSAFE" -gt 0 ]; then
    COVERAGE=$(( TOTAL_SAFETY * 100 / TOTAL_UNSAFE ))
    echo "  unsafe blocks : $TOTAL_UNSAFE"
    echo "  SAFETY 注释   : $TOTAL_SAFETY"
    echo "  覆盖率        : ${COVERAGE}%"
    if [ "$COVERAGE" -ge 50 ]; then
        ok "SAFETY 覆盖率 >= 50%"
    else
        echo -e "${YELLOW}⚠ SAFETY 覆盖率 < 50%${NC}"
    fi
fi

# ── 4. Lockbud 死锁/数据竞争扫描 ────────────────────────────────
step "4/6 Lockbud 死锁/数据竞争扫描"
pushd src/rust > /dev/null
unset RUSTC_WRAPPER
LOCKBUD_RESULT=0
cargo lockbud --manifest-path ../kernel/Cargo.toml --target x86_64-unknown-none --target-dir ../../other/target "${BUILD_STD_CFG[@]}" 2>&1 | tail -25 || LOCKBUD_RESULT=$?
if [ $LOCKBUD_RESULT -eq 0 ]; then
    ok "lockbud: passed"
else
    echo -e "${YELLOW}⚠ lockbud 返回非零 (可能发现潜在问题或工具未安装, 见上)${NC}"
fi
popd > /dev/null

# ── 5. miri 已于 2026-06-26 弃用 ──
# 原 Miri 严格 provenance 配置验证已删除, UB 检测由 Rust 编译期 + 7 个审计脚本覆盖

# ── 6. 模块级 SAFETY 不变式审计 ────────────────────────────────
step "6/6 模块级 SAFETY 不变式 (框架特权层)"
PRIVILEGE_MODULES=$(grep -rln "Framekernel privilege wrapper\|SAFETY invariant" "$KERNEL_DIR" --include="*.rs" 2>/dev/null | wc -l)
echo "  含模块级 SAFETY 不变式的文件: $PRIVILEGE_MODULES"
if [ "$PRIVILEGE_MODULES" -ge 5 ]; then
    ok "框架特权层封装充分"
else
    echo -e "${YELLOW}⚠ 框架特权层模块数 < 5${NC}"
fi

# ── 7. QEMU 真实启动测试 (full 模式) ──────────────────────────
# 双架构内核必须在 QEMU 中真实启动通过
# 跳过条件: QEMU 二进制不可用 (e.g. CI 镜像未装 qemu-system-*)
step "7/7 QEMU 双架构真实启动测试"
if [ "$MODE" = "full" ]; then
    if command -v qemu-system-x86_64 >/dev/null 2>&1 && command -v qemu-system-aarch64 >/dev/null 2>&1; then
        # B01-16 修复: 设置 FAIL_OK=0 让 QEMU 测试失败立即阻断.
        # 原脚本默认 FAIL_OK=1, 启动失败时仍返 0 (虚假通过).
        if FAIL_OK=0 "$PROJECT_ROOT/scripts/qemu_boot_test.sh" all 2>&1 | tail -6; then
            QEMU_RC=${PIPESTATUS[0]}
            if [ "$QEMU_RC" -eq 0 ]; then
                ok "QEMU 双架构启动测试: 2/2 通过"
            else
                err "QEMU 双架构启动测试失败 (exit=$QEMU_RC, 见 scripts/qemu_boot_test.sh 输出)"
            fi
        else
            err "QEMU 双架构启动测试执行异常"
        fi
    else
        echo -e "${YELLOW}⚠ QEMU 二进制不可用, 跳过启动测试 (安装 qemu-system-x86 + qemu-system-aarch64 启用)${NC}"
    fi
else
    echo -e "${BLUE}-> 跳过 (仅 full 模式执行; quick 模式跑 SA bootstrap 即可)${NC}"
fi

echo -e "\n${GREEN}━━━ audit (full) 完成 ━━━${NC}"
