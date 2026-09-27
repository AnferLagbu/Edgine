#!/bin/bash
# QueenX 双架构构建验证脚本
# 用法: ./ci/build.sh [x86_64|aarch64|all]

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

# 构建模式显式化 (方案 B): 裸机构建显式注入 build-std (src/rust/.cargo/config.toml
# 已删全局 [unstable] build-std). 避免 cwd 隐式加载 — src/rust 目录内 host-target
# 构建会触发 E0152 双 alloc 冲突 (DECISION-021 同族). 详见主计划文档 §10.
BUILD_STD_CFG=(--config 'unstable.build-std=["core","compiler_builtins","alloc"]' --config 'unstable.build-std-features=["compiler-builtins-mem"]')

# 生成内核 build.rs 在编译期校验的 Makefile 产物 (src/kernel/build.rs:9-36).
#
# 为何必须前置: build.rs 只校验 launcher 产物**存在**, 而这些产物由 Makefile 生成;
# 且两架构的 build/user/*.bin 是不同 ELF, 不能混用 (build.rs 无法辨别架构, 跨架构
# 产物会被静默嵌入). 之前 make 仅出现在 link_kernel 中, 排在 build_arch 之后 ——
# 于是当 build/ 处于另一架构状态 (arch-switch-clean 已删除这些产物) 时,
# 首个 build_arch 必然失败 (曾表现为 `./ci/build.sh all` → `Passed: 4 Failed: 1`).
#
# 本函数只做播种, 不占用 PASSED/FAILED 计数: 若播种失败, 紧随其后的 build_arch
# 会以 build.rs 的「构建产物缺失」报错暴露问题, 保持步骤计数与既有口径一致.
seed_artifacts() {
    local arch=$1
    echo -e "${YELLOW}[CI] Seeding Makefile artifacts (ARCH=${arch})...${NC}"
    # x86_64 需 build/stage1.bin (专属引导码 boot/stage1.asm); 两架构均需
    # build/user/*.bin (user 目标一次生成全部用户态产物)
    if [ "$arch" = "x86_64" ]; then
        make ARCH="${arch}" build/stage1.bin user 2>&1 | tail -3 || true
    else
        make ARCH="${arch}" user 2>&1 | tail -3 || true
    fi
}

build_arch() {
    local arch=$1
    local target=$2
    echo -e "${YELLOW}[CI] Building ARCH=${arch} (target: ${target})...${NC}"

    pushd src/rust > /dev/null
    # 方案 D: 内核独立 crate, 裸机 build 指向 kernel manifest (queenx 壳仅 host).
    if cargo build --manifest-path ../kernel/Cargo.toml --release --target "${target}" --target-dir target "${BUILD_STD_CFG[@]}" 2>&1 | tail -5; then
        echo -e "${GREEN}[CI] ARCH=${arch}: build passed${NC}"
        popd > /dev/null
        return 0
    else
        echo -e "${RED}[CI] ARCH=${arch}: build FAILED${NC}"
        popd > /dev/null
        return 1
    fi
}

# 链接最终内核镜像 (kernel.flat / kernel.bin).
# cargo build 仅生成 Rust 静态库 (.a), 必须通过 make 链接汇编对象
# 才能生成可启动的内核二进制. 跳过此步骤会导致 QEMU 使用过期镜像.
# 注意: 双架构不能同时链接 (共用 build/kernel.bin 输出路径),
#       因此 all 模式下仅链接主架构 (x86_64).
link_kernel() {
    local arch=$1
    echo -e "${YELLOW}[CI] Linking ARCH=${arch} kernel image...${NC}"
    if make ARCH="${arch}" 2>&1 | tail -5; then
        echo -e "${GREEN}[CI] ARCH=${arch}: link passed${NC}"
        return 0
    else
        echo -e "${RED}[CI] ARCH=${arch}: link FAILED${NC}"
        return 1
    fi
}

run_host_tests() {
    echo -e "${YELLOW}[CI] Running host-side unit tests...${NC}"
    pushd host-tests > /dev/null
    if cargo test --quiet 2>&1 | tail -10; then
        echo -e "${GREEN}[CI] Host tests: passed${NC}"
        popd > /dev/null
        return 0
    else
        echo -e "${RED}[CI] Host tests: FAILED${NC}"
        popd > /dev/null
        return 1
    fi
}

check_forbidden_patterns() {
    echo -e "${YELLOW}[CI] Checking forbidden asm patterns...${NC}"

    # 查找所有 asm! 调用（排除架构特定目录和文件）
    # 架构特定目录: arch/x86_64/, arch/aarch64/, boot/aarch64/
    # 这些目录内的代码天然受模块系统 cfg 约束
    # 排除 cargo 构建产物 target/（vendored 依赖生成的 probe.rs 非本仓库源码）
    local matches
    matches=$(grep -rFn 'asm!("' src/kernel/ --include='*.rs' 2>/dev/null \
        | grep -v '/target/' \
        | grep -v 'arch/x86_64/' \
        | grep -v 'arch/aarch64/' \
        | grep -v 'boot/aarch64/' \
        | grep -v 'arch/mod.rs' \
        | grep -v '#\[cfg' \
        | grep -v '#!\[cfg' \
        || true)

    if [ -z "$matches" ]; then
        echo -e "${GREEN}[CI] Forbidden patterns check: clean${NC}"
        return 0
    fi

    # 过滤掉已有 cfg(target_arch) 门控的 asm! 调用
    local filtered=""
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        local file
        file=$(echo "$line" | cut -d: -f1)
        
        # 检查文件是否有文件级 cfg(target_arch) 门控 (前 50 行)
        if head -50 "$file" | grep -qE '^#!\[cfg\(target_arch'; then
            continue
        fi
        
        # 检查文件是否是架构特定文件 (文件名含 _x86_64 或 _aarch64)
        local basename
        basename=$(basename "$file")
        if echo "$basename" | grep -qE '_x86_64\.rs$|_aarch64\.rs$'; then
            continue
        fi
        
        # 检查 asm! 前 50 行是否有 cfg(target_arch) 门控
        # 这包括:
        # - 块级 #[cfg(target_arch)] 直接包裹 asm!
        # - 函数/模块级 #[cfg(target_arch)] 包裹包含 asm! 的函数
        # - 复合条件 #[cfg(all(..., target_arch = "..."))]
        local lineno
        lineno=$(echo "$line" | cut -d: -f2)
        local start=$((lineno > 50 ? lineno - 50 : 1))
        if sed -n "${start},${lineno}p" "$file" | grep -qE '#\[cfg\(.*target_arch'; then
            continue
        fi
        
        filtered="${filtered}${line}"$'\n'
    done <<< "$matches"

    if [ -z "$(echo "$filtered" | tr -d '[:space:]')" ]; then
        echo -e "${GREEN}[CI] Forbidden patterns check: clean (all asm! calls have cfg gating)${NC}"
        return 0
    fi

    # 显示真正缺少 cfg gating 的 asm! 调用
    echo "$filtered" | while IFS=: read -r file line rest; do
        [ -z "$file" ] && continue
        echo -e "  ${YELLOW}→${NC} $file:$line$rest"
    done
    local count
    count=$(echo "$filtered" | grep -c '.' || true)
    echo -e "${RED}[CI] Forbidden patterns: found ${count} asm! calls without cfg gating (above). Verify cfg gating.${NC}"
    return 1
}

# ============================================================================
# Main
# ============================================================================

ARCH="${1:-all}"
PASSED=0
FAILED=0

case "$ARCH" in
    x86_64)
        seed_artifacts "x86_64"
        build_arch "x86_64" "x86_64-unknown-none" && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        link_kernel "x86_64" && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        ;;
    aarch64)
        seed_artifacts "aarch64"
        build_arch "aarch64" "aarch64-unknown-none-softfloat" && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        link_kernel "aarch64" && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        ;;
    all)
        seed_artifacts "x86_64"
        build_arch "x86_64" "x86_64-unknown-none" && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        seed_artifacts "aarch64"
        build_arch "aarch64" "aarch64-unknown-none-softfloat" && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        run_host_tests && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        check_forbidden_patterns && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        # 双架构共享 build/kernel.bin 输出路径, 仅链接主架构.
        # aarch64 链接验证在单独 `./ci/build.sh aarch64` 时完成.
        link_kernel "x86_64" && PASSED=$((PASSED+1)) || FAILED=$((FAILED+1))
        ;;
    *)
        echo "Usage: $0 [x86_64|aarch64|all]"
        exit 1
        ;;
esac

echo ""
echo -e "${GREEN}Passed: ${PASSED}${NC}  ${RED}Failed: ${FAILED}${NC}"
exit $FAILED