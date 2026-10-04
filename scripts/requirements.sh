#!/bin/bash
# QueenX 内核构建环境依赖检查与安装工具
#
# 适用于 QueenX 框内核项目, 基于 2026-06-05 v3.2 工具链实测:
#
#   ┌──────────────────────────────────────────────────────────┐
#   │  工具链组成 (全 Rust 化 + 最小 C 链接层 + Python CI 胶水)│
#   ├──────────────────────────────────────────────────────────┤
#   │  1. Rust 工具链     : rustc / cargo / rustup (核心)      │
#   │  2. Rust 编译目标   : x86_64-unknown-none                │
#   │                       aarch64-unknown-none               │
#   │                       aarch64-unknown-none-softfloat     │
#   │  3. Rust 组件       : rust-src, llvm-tools-preview       │
#   │                       clippy, rustfmt (CI 推荐)          │
#   │  4. Rust 测试工具   : lockbud / cargo-deny / cargo-audit │
#   │                       / cargo-llvm-cov / cargo-mutants   │
#   │                       / cargo-bloat / cargo-geiger / ... │
#   │  5. C 链接层 (可选) : nasm / aarch64-linux-gnu-as        │
#   │                       {x86_64,aarch64}-linux-gnu-ld      │
#   │                       {x86_64,aarch64}-linux-gnu-objcopy │
#   │                       (仅裸机链接 / 启动汇编)            │
#   │  6. C 测试桩 (可选) : {x86_64,aarch64}-linux-gnu-gcc     │
#   │                       (tests/*.c 已弃用, 仅历史兼容)     │
#   │  7. ISO 制作        : grub2-mkrescue + xorriso + mtools  │
#   │  8. QEMU 仿真       : qemu-system-x86_64                 │
#   │                       qemu-system-aarch64                │
#   │  9. Python 3 CI 胶水: audit_*.py / check_bench_*.py      │
#   │                       record_bench_*.py / ci_check_*.py  │
#   │ 10. 调试            : gdb-multiarch / strace             │
#   │ 11. 项目本地工具    : tools/check_tcb.sh                 │
#   │                       tools/audit_unsafe.{sh,py}         │
#   └──────────────────────────────────────────────────────────┘
#
# 用法:
#   ./scripts/requirements.sh                  # 交互式
#   ./scripts/requirements.sh -y               # 全部自动安装
#   ./scripts/requirements.sh --check-only     # 仅检查, 不安装
#   ./scripts/requirements.sh --skip-c         # 跳过 C 工具链 (含链接层+测试桩)
#   ./scripts/requirements.sh --skip-c-linker   # 仅跳过 C 链接层 (保留测试桩)
#   ./scripts/requirements.sh --skip-iso       # 跳过 ISO 工具 (不构建 ISO)
#   ./scripts/requirements.sh --skip-tests     # 跳过 Rust 测试工具链
#   ./scripts/requirements.sh --skip-optional  # 仅检查必需
#   ./scripts/requirements.sh --skip-project   # 跳过项目本地工具检查
#   ./scripts/requirements.sh -v | --verbose   # 显示详细版本
#   ./scripts/requirements.sh -h | --help      # 帮助
#
# 关于 C 工具链 (v3.2 决策):
#   QueenX 源码已 100% Rust 化, C 工具链默认归类为"可选" (--skip-c 即可跳过).
#   但 Makefile 仍引用 ld/nasm/objcopy 进行裸机链接 (rustc 默认链接器无法生成
#   裸机 ELF), 因此:
#     - C 链接层 (ld/nasm/objcopy) — 实际必需, 但本脚本不强制 (--skip-c 时
#       用户须自备), 单独保留为 --skip-c-linker 跳过.
#     - C 测试桩 (gcc 编译 tests/*.c) — 真正可选, src/kernel/tests/ 下的
#       kernel_test.c/test_main.c 等 C 测试桩已弃用, 但保留编译入口.

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
BOLD='\033[1m'
NC='\033[0m'

# 全局开关
AUTO_INSTALL=false
VERBOSE=false
SKIP_OPTIONAL=false
SKIP_C=false
SKIP_C_LINKER=false
SKIP_ISO=false
SKIP_TESTS=false
SKIP_PROJECT=false
CHECK_ONLY=false

# 缺失清单 (按类别)
REQUIRED_PACKAGES=()       # 缺失的必需包
RECOMMENDED_PACKAGES=()    # 缺失的强烈推荐包
TESTING_PACKAGES=()        # 缺失的 Rust 测试工具
OPTIONAL_PACKAGES=()       # 缺失的可选包
C_LEGACY_PACKAGES=()       # 缺失的 C 链接层包
C_TEST_STUB_PACKAGES=()    # 缺失的 C 测试桩包 (新分类)
ISO_PACKAGES=()            # 缺失的 ISO 制作包
PROJECT_TOOLS_MISSING=()   # 缺失/不可用的项目本地工具

MISSING_RUSTUP_COMPONENTS=()
MISSING_RUSTUP_TARGETS=()
MISSING_CARGO_SUBCMDS=()   # cargo 子命令 (格式: cmd=crate)

# 统计
REQUIRED_OK=0
REQUIRED_TOTAL=0
RECOMMENDED_OK=0
RECOMMENDED_TOTAL=0
TESTING_OK=0
TESTING_TOTAL=0
OPTIONAL_OK=0
OPTIONAL_TOTAL=0
C_LEGACY_OK=0
C_LEGACY_TOTAL=0
C_TEST_STUB_OK=0
C_TEST_STUB_TOTAL=0
ISO_OK=0
ISO_TOTAL=0
PROJECT_TOOLS_OK=0
PROJECT_TOOLS_TOTAL=0

# ───────────────────────── 输出辅助 ─────────────────────────

print_header() {
    echo ""
    echo -e "${CYAN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║   QueenX 内核构建环境依赖检查工具 v3.2 (2026-06-05)     ║${NC}"
    echo -e "${CYAN}║   Project: QueenX Framekernel | Toolchain: Rust 2021      ║${NC}"
    echo -e "${CYAN}║   C 工具链已分类: 链接层(5) + 测试桩(6) — 均可跳过       ║${NC}"
    echo -e "${CYAN}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
}

print_section() {
    echo ""
    echo -e "${YELLOW}━━━ $1 ━━━${NC}"
}

print_subsection() {
    echo ""
    echo -e "  ${MAGENTA}▸ $1${NC}"
}

print_check() {
    printf "  ${CYAN}%-44s${NC}" "$1"
}

print_ok() {
    echo -e "${GREEN}✓ 已安装${NC}"
}

print_missing() {
    echo -e "${RED}✗ 未找到${NC}"
}

print_recommended_missing() {
    echo -e "${YELLOW}△ 推荐安装${NC}"
}

print_optional() {
    echo -e "${YELLOW}○ 未安装 (可选)${NC}"
}

print_info() {
    echo -e "      ${BLUE}└─ $1${NC}"
}

print_warning() {
    echo -e "  ${YELLOW}⚠ 警告: $1${NC}"
}

print_success() {
    echo -e "${GREEN}✓ 成功: $1${NC}"
}

print_error() {
    echo -e "${RED}✗ 错误: $1${NC}"
}

print_hr() {
    echo -e "${CYAN}────────────────────────────────────────────────────────────${NC}"
}

# 询问用户 yes/no
ask_yes_no() {
    local prompt="$1"
    local default="${2:-n}"

    if [ "$AUTO_INSTALL" = true ]; then
        return 0
    fi

    local prompt_str
    if [ "$default" = "y" ]; then
        prompt_str="${prompt} [Y/n] "
    else
        prompt_str="${prompt} [y/N] "
    fi

    while true; do
        printf "  ${YELLOW}?${NC} %b" "$prompt_str"
        read -r response

        case "$response" in
            [Yy]|[Yy][Ee][Ss]) return 0 ;;
            [Nn]|[Nn][Oo]) return 1 ;;
            "")
                if [ "$default" = "y" ]; then
                    return 0
                else
                    return 1
                fi
                ;;
            *)
                echo -e "  ${RED}请输入 y 或 n${NC}"
                ;;
        esac
    done
}

# ───────────────────────── 工具函数 ─────────────────────────

# 检测命令是否存在 (静默)
has_cmd() {
    command -v "$1" &> /dev/null
}

# 检测包管理器
detect_package_manager() {
    if has_cmd apt-get; then
        echo "apt"
    elif has_cmd dnf; then
        echo "dnf"
    elif has_cmd yum; then
        echo "yum"
    elif has_cmd pacman; then
        echo "pacman"
    elif has_cmd apk; then
        echo "apk"
    else
        echo "unknown"
    fi
}

# 通用检查函数
check_command() {
    if has_cmd "$1"; then
        print_ok
        return 0
    else
        print_missing
        return 1
    fi
}

# 显示命令版本 (VERBOSE 模式下)
show_version() {
    local cmd="$1"
    if [ "$VERBOSE" = true ] && has_cmd "$cmd"; then
        local VERSION
        VERSION=$($cmd --version 2>&1 | head -1)
        [ -n "$VERSION" ] && print_info "$VERSION"
    fi
}

# 检查必需项
check_required() {
    local desc="$1"
    local cmd="$2"
    local pkg="$3"
    REQUIRED_TOTAL=$((REQUIRED_TOTAL + 1))

    print_check "$desc"
    if check_command "$cmd"; then
        REQUIRED_OK=$((REQUIRED_OK + 1))
        show_version "$cmd"
        return 0
    else
        [ -n "$pkg" ] && REQUIRED_PACKAGES+=("$pkg")
        return 1
    fi
}

# 检查强烈推荐
check_recommended() {
    local desc="$1"
    local cmd="$2"
    local pkg="$3"
    RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 1))

    print_check "$desc"
    if check_command "$cmd"; then
        RECOMMENDED_OK=$((RECOMMENDED_OK + 1))
        show_version "$cmd"
        return 0
    else
        print_recommended_missing
        [ -n "$pkg" ] && RECOMMENDED_PACKAGES+=("$pkg")
        return 1
    fi
}

# 检查 Rust 测试工具
check_testing() {
    local desc="$1"
    local cmd="$2"
    local pkg="$3"
    TESTING_TOTAL=$((TESTING_TOTAL + 1))

    print_check "$desc"
    if check_command "$cmd"; then
        TESTING_OK=$((TESTING_OK + 1))
        show_version "$cmd"
        return 0
    else
        print_recommended_missing
        [ -n "$pkg" ] && TESTING_PACKAGES+=("$pkg")
        return 1
    fi
}

# 检查 cargo 子命令 (通过 cargo --list 检测)
check_cargo_subcmd() {
    local subcmd="$1"
    local crate="$2"
    local desc="$3"

    TESTING_TOTAL=$((TESTING_TOTAL + 1))

    print_check "$desc (cargo $subcmd)"
    if cargo --list 2>/dev/null | grep -q "^ *${subcmd} "; then
        TESTING_OK=$((TESTING_OK + 1))
        return 0
    else
        print_recommended_missing
        MISSING_CARGO_SUBCMDS+=("${subcmd}=${crate}")
        return 1
    fi
}

# 检查可选
check_optional() {
    local desc="$1"
    local cmd="$2"
    local pkg="$3"
    OPTIONAL_TOTAL=$((OPTIONAL_TOTAL + 1))

    print_check "$desc"
    if check_command "$cmd"; then
        OPTIONAL_OK=$((OPTIONAL_OK + 1))
        show_version "$cmd"
        return 0
    else
        print_optional
        [ -n "$pkg" ] && OPTIONAL_PACKAGES+=("$pkg")
        return 1
    fi
}

# 检查 C 遗留
check_c_legacy() {
    local desc="$1"
    local cmd="$2"
    local pkg="$3"
    C_LEGACY_TOTAL=$((C_LEGACY_TOTAL + 1))

    print_check "$desc"
    if check_command "$cmd"; then
        C_LEGACY_OK=$((C_LEGACY_OK + 1))
        show_version "$cmd"
        return 0
    else
        print_optional
        [ -n "$pkg" ] && C_LEGACY_PACKAGES+=("$pkg")
        return 1
    fi
}

# 检查 C 测试桩 (v3.2 新分类)
check_c_test_stub() {
    local desc="$1"
    local cmd="$2"
    local pkg="$3"
    C_TEST_STUB_TOTAL=$((C_TEST_STUB_TOTAL + 1))

    print_check "$desc"
    if check_command "$cmd"; then
        C_TEST_STUB_OK=$((C_TEST_STUB_OK + 1))
        show_version "$cmd"
        return 0
    else
        print_optional
        [ -n "$pkg" ] && C_TEST_STUB_PACKAGES+=("$pkg")
        return 1
    fi
}

# 检查项目本地工具 (v3.2 新增)
check_project_tool() {
    local desc="$1"
    local path="$2"   # 相对项目根的路径
    PROJECT_TOOLS_TOTAL=$((PROJECT_TOOLS_TOTAL + 1))

    print_check "$desc ($path)"
    if [ -x "$path" ] || ( [ -f "$path" ] && [ -r "$path" ] ); then
        # 进一步检查可执行性 (Python 脚本)
        if [[ "$path" == *.py ]]; then
            if python3 "$path" --help &>/dev/null || python3 "$path" --version &>/dev/null || \
               python3 -c "import ast; ast.parse(open('$path').read())" 2>/dev/null; then
                print_ok
                PROJECT_TOOLS_OK=$((PROJECT_TOOLS_OK + 1))
                return 0
            else
                print_warning "项目工具存在但解析失败: $path"
                PROJECT_TOOLS_MISSING+=("$path")
                return 1
            fi
        fi
        print_ok
        PROJECT_TOOLS_OK=$((PROJECT_TOOLS_OK + 1))
        return 0
    else
        print_missing
        PROJECT_TOOLS_MISSING+=("$path")
        return 1
    fi
}

# 检查 ISO 工具
check_iso() {
    local desc="$1"
    local cmd="$2"
    local pkg="$3"
    ISO_TOTAL=$((ISO_TOTAL + 1))

    print_check "$desc"
    if check_command "$cmd"; then
        ISO_OK=$((ISO_OK + 1))
        show_version "$cmd"
        return 0
    else
        print_optional
        [ -n "$pkg" ] && ISO_PACKAGES+=("$pkg")
        return 1
    fi
}

# ───────────────────────── 安装函数 ─────────────────────────

# 安装包的通用函数
install_packages() {
    local pkg_manager=$(detect_package_manager)
    local packages=("$@")

    if [ ${#packages[@]} -eq 0 ]; then
        return 0
    fi

    echo ""
    echo -e "${YELLOW}正在安装以下软件包 (${pkg_manager}):${NC}"

    for pkg in "${packages[@]}"; do
        echo -e "  • ${CYAN}$pkg${NC}"
    done

    if ! ask_yes_no "确认安装以上软件包？"; then
        print_warning "用户取消安装"
        return 1
    fi

    echo ""
    echo -e "${BLUE}执行安装命令...${NC}"

    case "$pkg_manager" in
        apt)
            sudo apt update && sudo apt install -y "${packages[@]}"
            ;;
        dnf)
            sudo dnf install -y "${packages[@]}"
            ;;
        yum)
            sudo yum install -y "${packages[@]}"
            ;;
        pacman)
            sudo pacman -S --noconfirm "${packages[@]}"
            ;;
        apk)
            sudo apk add "${packages[@]}"
            ;;
        *)
            print_error "无法识别包管理器, 请手动安装:"
            echo "  sudo apt install ${packages[*]}  # Debian/Ubuntu"
            echo "  sudo dnf install ${packages[*]}  # Fedora"
            return 1
            ;;
    esac

    if [ $? -eq 0 ]; then
        print_success "所有软件包安装完成!"
        return 0
    else
        print_error "安装过程中出现错误"
        return 1
    fi
}

# 安装 rustup 组件
install_rust_components() {
    local components=("$@")

    echo ""
    echo -e "${YELLOW}正在安装 rustup 组件: ${components[*]}${NC}"

    for comp in "${components[@]}"; do
        echo -e "  • ${CYAN}$comp${NC}"
    done

    if ! ask_yes_no "确认安装？"; then
        print_warning "用户取消"
        return 1
    fi

    for comp in "${components[@]}"; do
        rustup component add "$comp" 2>&1 | tail -3
    done
}

# 安装 rustup 目标
install_rust_targets() {
    local targets=("$@")

    echo ""
    echo -e "${YELLOW}正在安装 rustup targets: ${targets[*]}${NC}"

    for tgt in "${targets[@]}"; do
        echo -e "  • ${CYAN}$tgt${NC}"
    done

    if ! ask_yes_no "确认安装？"; then
        print_warning "用户取消"
        return 1
    fi

    for tgt in "${targets[@]}"; do
        rustup target add "$tgt" 2>&1 | tail -3
    done
}

# 安装 cargo 子命令
install_cargo_subcmds() {
    local entries=("$@")

    echo ""
    echo -e "${YELLOW}正在安装 cargo 子命令:${NC}"
    for entry in "${entries[@]}"; do
        local subcmd="${entry%%=*}"
        local crate="${entry#*=}"
        echo -e "  • ${CYAN}cargo ${subcmd}${NC} (crate: ${crate})"
    done

    if ! ask_yes_no "确认安装？"; then
        print_warning "用户取消"
        return 1
    fi

    for entry in "${entries[@]}"; do
        local subcmd="${entry%%=*}"
        local crate="${entry#*=}"
        echo -e "${BLUE}安装 cargo-${subcmd}...${NC}"
        cargo install "$crate" --locked 2>&1 | tail -5
    done
}

# 安装 Rust 工具链 (主入口)
install_rust_toolchain() {
    echo ""
    echo -e "${YELLOW}正在安装 Rust 工具链...${NC}"

    if ! ask_yes_no "确认安装 Rust (rustc, cargo, rustup)？"; then
        print_warning "用户取消安装"
        return 1
    fi

    echo ""
    echo -e "${BLUE}下载并运行 Rust 安装脚本...${NC}"

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

    if [ $? -eq 0 ]; then
        print_success "Rust 基础工具链安装完成!"

        # 加载环境变量
        # shellcheck disable=SC1091
        source "$HOME/.cargo/env"

        # 询问是否安装 nightly
        if ask_yes_no "是否安装 Rust Nightly 工具链 (内核编译需要)？" "y"; then
            echo ""
            echo -e "${BLUE}安装 Rust Nightly...${NC}"
            rustup toolchain install nightly

            if [ $? -eq 0 ]; then
                print_success "Rust Nightly 安装完成!"

                # 询问是否安装核心组件
                if ask_yes_no "是否安装核心组件 (rust-src + clippy + rustfmt)？" "y"; then
                    rustup component add rust-src clippy rustfmt
                fi

                # miri 已于 2026-06-26 弃用 (工具未实际安装, 由 Rust 编译期 + 审计脚本覆盖)
                # 询问是否安装 llvm-tools-preview
                if ask_yes_no "是否安装 llvm-tools-preview (cargo cov / 覆盖率)？" "y"; then
                    rustup +nightly component add llvm-tools-preview
                fi

                # 询问是否安装 targets
                if ask_yes_no "是否安装裸机目标 (x86_64-unknown-none + aarch64-unknown-none + aarch64-unknown-none-softfloat)？" "y"; then
                    rustup target add x86_64-unknown-none aarch64-unknown-none aarch64-unknown-none-softfloat
                fi
            else
                print_warning "Rust Nightly 安装失败"
            fi
        fi

        return 0
    else
        print_error "Rust 安装失败"
        return 1
    fi
}

# ───────────────────────── Python 模块检查 ─────────────────────────

check_python_module() {
    local module="$1"
    python3 -c "import $module" 2>/dev/null
}

check_python_modules() {
    print_subsection "Python 3 标准库 (CI 静态分析 / bench 报告)"

    # 实际被 scripts/*.py 引用的标准库模块
    # (基于 grep 实证: audit_*.py / ci_check_*.py / check_bench_*.py / record_bench_*.py)
    local python_modules=(
        "argparse"     # CLI 参数解析
        "json"         # baseline.json / 报告
        "os"           # 文件遍历 / 环境变量
        "pathlib"      # 路径处理
        "platform"     # 主机信息 (baseline)
        "re"           # 正则 (审计扫描)
        "subprocess"   # cargo 调用
        "sys"          # sys.exit / sys.stderr
        "time"         # baseline 时间戳
        "collections"  # defaultdict (边界/死锁)
    )
    local all_ok=true
    for mod in "${python_modules[@]}"; do
        if ! check_python_module "$mod"; then
            all_ok=false
            print_warning "缺少 Python 标准库: $mod (极少见, 请检查 Python 安装)"
        fi
    done
    if [ "$all_ok" = true ]; then
        print_check "Python 3 标准库 (10 个核心模块)"
        print_ok
    fi
}

# ───────────────────────── Rust 工具链检查 ─────────────────────────

check_rustup_component() {
    local comp="$1"
    if has_cmd rustup && rustup component list 2>/dev/null | grep -q "^${comp}.*installed"; then
        return 0
    else
        return 1
    fi
}

check_rustup_target() {
    local tgt="$1"
    if has_cmd rustup && rustup target list --installed 2>/dev/null | grep -q "^${tgt}$"; then
        return 0
    else
        return 1
    fi
}

# ───────────────────────── 显示帮助 ─────────────────────────

show_help() {
    cat <<EOF
${CYAN}用法:${NC} $0 [选项]

${YELLOW}选项:${NC}
  -y, --yes             自动确认所有提示 (非交互模式)
  -s, --skip-optional   跳过可选依赖 (仅检查必需)
      --skip-c          跳过 C 工具链 (含链接层+测试桩) — Rust-only 环境
      --skip-c-linker   仅跳过 C 链接层 (保留 C 测试桩检查)
      --skip-iso        跳过 ISO 制作工具 (grub2-mkrescue + xorriso)
      --skip-tests      跳过 Rust 测试工具链 (clippy/llvm-cov 等仍属于推荐)
      --skip-project    跳过项目本地工具 (tools/check_tcb.sh 等)
      --check-only      仅检查, 不安装
  -v, --verbose         显示详细版本信息
  -h, --help            显示此帮助

${YELLOW}示例:${NC}
  $0                       # 交互式, 询问每个操作
  $0 -y                    # 自动安装所有缺失依赖
  $0 --check-only          # 仅显示状态, 不安装
  $0 --skip-c -y           # 跳过 C 工具链 (Rust-only)
  $0 --skip-c-linker -y    # 仅跳过 C 链接层 (保留 C 测试桩)
  $0 --skip-iso -y         # 跳过 ISO 制作
  $0 --skip-tests -y       # 跳过测试工具链
  $0 --skip-project -y     # 跳过项目本地工具检查
  $0 --skip-optional       # 仅检查必需依赖

${YELLOW}依赖分类 (8 类, v3.2):${NC}
  ${RED}必需 (1/8)${NC}    : Rust 工具链 + QEMU + Python 3 + Make
  ${YELLOW}推荐 (2/8)${NC}    : rust-src/clippy/rustfmt/llvm-tools/targets (miri 已弃用 2026-06-26)
  ${MAGENTA}测试 (3/8)${NC}    : lockbud/cargo-deny/cargo-audit/llvm-cov/mutants/bloat/geiger
  ${YELLOW}可选 (4/8)${NC}    : rust-analyzer/bindgen/htop/tmux/gdb/strace
  ${BLUE}C 链接 (5/8)${NC}   : nasm / {x86_64,aarch64}-linux-gnu-{ld,objcopy,as}
  ${BLUE}C 测试 (6/8)${NC}   : {x86_64,aarch64}-linux-gnu-gcc (C 测试桩编译)
  ${BLUE}ISO (7/8)${NC}      : grub2-mkrescue / xorriso / mtools (--skip-iso)
  ${CYAN}项目工具 (8/8)${NC} : tools/check_tcb.sh + tools/audit_unsafe.{sh,py}

${YELLOW}C 工具链 v3.2 决策:${NC}
  QueenX 源码已 100% Rust 化, C 工具链默认归类为"可选" (--skip-c 即可跳过).
  但 Makefile 仍引用 ld/nasm/objcopy 进行裸机链接, 因此:
    - C 链接层 (ld/nasm/objcopy) — 实际裸机构建必需, 但本脚本不强制
    - C 测试桩 (gcc 编译 tests/*.c) — 真正可选, 已弃用, 仅历史兼容
  全 Rust 内核镜像需配合 ld + 启动汇编才能生成可执行 ELF/ISO.

${YELLOW}配置文件引用:${NC}
  - rust-toolchain.toml  : nightly-2026-10-02 (仓库根, 全仓唯一来源)
                           + rust-src / llvm-tools-preview / rustfmt / clippy
                           + x86_64-unknown-none / aarch64-unknown-none(-softfloat)
  - .cargo/config.toml   : x86_64/aarch64 rustflags + build-std
  - clippy.toml          : deny all, 认知复杂度阈值 25
  - deny.toml            : 许可证 (MIT/Apache-2.0) + 多版本禁止

EOF
}

# ───────────────────────── 解析参数 ─────────────────────────

parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            -y|--yes)
                AUTO_INSTALL=true
                shift
                ;;
            -s|--skip-optional)
                SKIP_OPTIONAL=true
                shift
                ;;
            --skip-c)
                SKIP_C=true
                SKIP_C_LINKER=true   # --skip-c 隐含跳过 C 链接层
                shift
                ;;
            --skip-c-linker)
                SKIP_C_LINKER=true
                shift
                ;;
            --skip-iso)
                SKIP_ISO=true
                shift
                ;;
            --skip-tests)
                SKIP_TESTS=true
                shift
                ;;
            --skip-project)
                SKIP_PROJECT=true
                shift
                ;;
            --check-only)
                CHECK_ONLY=true
                shift
                ;;
            -v|--verbose)
                VERBOSE=true
                shift
                ;;
            -h|--help)
                show_help
                exit 0
                ;;
            *)
                print_error "未知参数: $1"
                show_help
                exit 1
                ;;
        esac
    done
}

# ───────────────────────── 主流程 ─────────────────────────

parse_args "$@"

# 关闭 set -e — 检查函数 (check_required / check_recommended / check_testing /
# check_optional / check_c_legacy / check_c_test_stub / check_iso /
# check_project_tool) 通过返回 1 表示"未找到", 用于累积到 *_MISSING 数组.
# 若保留 set -e, 第一次未找到 (如 cargo-llvm-cov) 会直接终止脚本, 无法
# 完成剩余分类检查. 检查结果通过 *_OK / *_TOTAL 计数器追踪, 退出码由
# 最终汇总决定.
set +e

print_header

echo -e "${BLUE}检测系统信息...${NC}"
OS_INFO=$(cat /etc/os-release 2>/dev/null | grep PRETTY_NAME | cut -d'"' -f2 || echo "Unknown")
PKG_MANAGER=$(detect_package_manager)
ARCH_HOST=$(uname -m)
RUSTC_VER=""
[ -x "$(command -v rustc 2>/dev/null)" ] && RUSTC_VER=$(rustc --version 2>/dev/null || echo "未安装")
PYTHON_VER=""
[ -x "$(command -v python3 2>/dev/null)" ] && PYTHON_VER=$(python3 --version 2>/dev/null || echo "未安装")
echo -e "  操作系统:       ${CYAN}$OS_INFO${NC}"
echo -e "  宿主机架构:     ${CYAN}$ARCH_HOST${NC}"
echo -e "  包管理器:       ${CYAN}$PKG_MANAGER${NC}"
echo -e "  Rust 工具链:    ${CYAN}${RUSTC_VER}${NC}"
echo -e "  Python 3:       ${CYAN}${PYTHON_VER}${NC}"
echo -e "  C 工具链模式:   ${CYAN}$([ "$SKIP_C" = true ] && echo "Rust-only (全跳过)" || ([ "$SKIP_C_LINKER" = true ] && echo "仅跳过链接层" || echo "全检查"))${NC}"
echo -e "  跳过 ISO:       ${CYAN}$([ "$SKIP_ISO" = true ] && echo "是" || echo "否")${NC}"
echo -e "  跳过测试工具:   ${CYAN}$([ "$SKIP_TESTS" = true ] && echo "是" || echo "否")${NC}"
echo -e "  跳过项目工具:   ${CYAN}$([ "$SKIP_PROJECT" = true ] && echo "是" || echo "否")${NC}"

# ==================== 第 1 部分: 必需依赖 ====================
print_section "[1/8] 必需依赖 (Required) — 内核编译/运行核心"

print_subsection "Rust 工具链 (核心 — rust-toolchain.toml)"
check_required "rustc 编译器" "rustc" ""
check_required "cargo 包管理器" "cargo" ""
check_required "rustup 工具链管理" "rustup" ""

print_subsection "QEMU 双架构仿真 (Makefile QEMU 变量)"
check_required "QEMU (x86_64)" "qemu-system-x86_64" "qemu-system-x86"
check_required "QEMU (aarch64)" "qemu-system-aarch64" "qemu-system-arm"

print_subsection "基础构建工具 (Makefile / qemu_boot_test.sh)"
check_required "Python 3 (>=3.8, CI 脚本依赖)" "python3" "python3"
check_required "GNU Make" "make" "make"
check_required "Git (版本管理)" "git" "git"
check_required "Curl (rustup 安装/网络测试)" "curl" "curl"
check_required "dd (磁盘镜像 dd 写入)" "dd" "coreutils"
check_required "find (.rs 依赖追踪)" "find" "findutils"

# ==================== 第 2 部分: 强烈推荐 (Rust 组件) ====================
if [ "$SKIP_OPTIONAL" = false ]; then
    print_section "[2/8] 强烈推荐 (Recommended) — rust-toolchain.toml 声明的组件"

    print_subsection "Rust 工具链 (nightly + 核心组件)"
    if has_cmd rustup; then
        print_check "Rust Nightly 工具链"
        if rustup show 2>/dev/null | grep -q "nightly"; then
            print_ok
            TOOLCHAIN=$(rustup show active-toolchain 2>/dev/null || echo "unknown")
            [ "$VERBOSE" = true ] && print_info "$TOOLCHAIN"
            RECOMMENDED_OK=$((RECOMMENDED_OK + 1))
        else
            print_recommended_missing
            MISSING_RUSTUP_COMPONENTS+=("nightly-toolchain")
        fi
        RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 1))

        # rust-toolchain.toml 声明的组件
        for comp in "rust-src" "clippy" "rustfmt"; do
            print_check "rustup component: $comp"
            if check_rustup_component "$comp"; then
                print_ok
                RECOMMENDED_OK=$((RECOMMENDED_OK + 1))
            else
                print_recommended_missing
                MISSING_RUSTUP_COMPONENTS+=("$comp")
            fi
            RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 1))
        done
    else
        print_warning "rustup 未安装, 跳过 rustup 组件检查"
        RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 4))
    fi

    print_subsection "Rust 编译目标 (.cargo/config.toml 引用)"
    if has_cmd rustup; then
        local_targets=("x86_64-unknown-none" "aarch64-unknown-none" "aarch64-unknown-none-softfloat")
        for tgt in "${local_targets[@]}"; do
            print_check "rustup target: $tgt"
            if check_rustup_target "$tgt"; then
                print_ok
                RECOMMENDED_OK=$((RECOMMENDED_OK + 1))
            else
                print_recommended_missing
                MISSING_RUSTUP_TARGETS+=("$tgt")
            fi
            RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 1))
        done
    else
        print_warning "rustup 未安装, 跳过 target 检查"
        RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 3))
    fi

    # miri 已于 2026-06-26 弃用, 跳过 miri 组件检查

    print_subsection "LLVM Tools (cargo cov / 性能分析)"
    print_check "llvm-tools-preview 组件"
    if has_cmd rustup && rustup +nightly component list 2>/dev/null | grep -q "^llvm-tools-preview.*installed"; then
        print_ok
        RECOMMENDED_OK=$((RECOMMENDED_OK + 1))
    else
        print_recommended_missing
        MISSING_RUSTUP_COMPONENTS+=("llvm-tools-preview")
    fi
    RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 1))
fi

# ==================== 第 3 部分: Rust 测试工具链 ====================
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_TESTS" = false ]; then
    print_section "[3/8] Rust 测试工具链 (Testing) — 实际被 CI/Makefile.ci 使用"

    print_subsection "代码质量 (Clippy 严苛模式 + 格式)"
    print_check "cargo clippy (clippy.toml deny all)"
    if has_cmd cargo && cargo clippy --version &>/dev/null; then
        print_ok
        RECOMMENDED_OK=$((RECOMMENDED_OK + 1))
    else
        print_recommended_missing
        MISSING_RUSTUP_COMPONENTS+=("clippy")
    fi
    RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 1))

    print_check "cargo fmt --check (CI 格式门禁)"
    if has_cmd cargo && cargo fmt --version &>/dev/null; then
        print_ok
        RECOMMENDED_OK=$((RECOMMENDED_OK + 1))
    else
        print_recommended_missing
        MISSING_RUSTUP_COMPONENTS+=("rustfmt")
    fi
    RECOMMENDED_TOTAL=$((RECOMMENDED_TOTAL + 1))

    print_subsection "并发安全与死锁 (ci/audit.sh full 模式)"
    check_testing "lockbud (数据竞争/死锁静态分析)" "lockbud" "lockbud"
    # 注: lockbud 是 cargo 子命令, 通过 `cargo +nightly lockbud` 调用
    # 实际安装的二进制名为 `lockbud` (位于 ~/.cargo/bin/lockbud),
    # 而非 `cargo-lockbud` (cargo install 创建 cargo-X 形式只是约定, 部分工具不遵循)
    # 当 lockbud 二进制不存在时, `cargo +nightly lockbud` 会触发 cargo 尝试下载
    # 其 registry index, 耗时数十秒. 因此用 `timeout 2` 快速失败, 避免挂起.
    if has_cmd lockbud; then
        if timeout 2 cargo +nightly lockbud --help &>/dev/null; then
            TESTING_OK=$((TESTING_OK + 1))
        fi
    fi

    print_subsection "代码覆盖率 (M6.9 bench + llvm-cov)"
    check_testing "cargo-llvm-cov (覆盖率高精度)" "cargo-llvm-cov" "cargo-llvm-cov"
    check_testing "grcov (聚合多模式覆盖率)" "grcov" "grcov"

    print_subsection "依赖治理 (deny.toml + 安全审计)"
    check_testing "cargo-deny (deny.toml 配套, 许可证/漏洞/版本)" "cargo-deny" "cargo-deny"
    check_testing "cargo-audit (RustSec 漏洞数据库)" "cargo-audit" "cargo-audit"
    check_testing "cargo-outdated (过期依赖检查)" "cargo-outdated" "cargo-outdated"

    print_subsection "测试质量与运行 (nextest + 变异测试)"
    check_testing "cargo-nextest (更快的测试运行器)" "cargo-nextest" "cargo-nextest"
    check_testing "cargo-mutants (变异测试, 评估用例有效性)" "cargo-mutants" "cargo-mutants"
    check_testing "cargo-test (随 cargo 携带, 应已可用)" "cargo" ""

    print_subsection "二进制与 unsafe 度量"
    check_testing "cargo-bloat (二进制大小分析)" "cargo-bloat" "cargo-bloat"
    check_testing "cargo-geiger (unsafe 代码量统计)" "cargo-geiger" "cargo-geiger"

    print_subsection "性能剖析 (M6.9 bench 升级)"
    check_testing "cargo-flamegraph (perf 火焰图)" "cargo-flamegraph" "cargo-flamegraph"

    print_subsection "宏/构建 辅助 (内核宏展开调试)"
    check_testing "cargo-expand (宏展开调试)" "cargo-expand" "cargo-expand"

    print_subsection "内存/UB 验证 (严苛模式)"
    check_testing "miri (cargo 子命令, UB 检测)" "cargo-miri" "miri"

    print_subsection "Fuzzing 模糊测试 (smoltcp/fuzz 可选)"
    check_testing "cargo-fuzz (libFuzzer 绑定, smoltcp 模糊用)" "cargo-fuzz" "cargo-fuzz"
fi

# ==================== 第 4 部分: 可选依赖 ====================
if [ "$SKIP_OPTIONAL" = false ]; then
    print_section "[4/8] 可选依赖 (Optional) — 提升开发体验"

    print_subsection "Rust 开发增强"
    check_optional "rust-analyzer (LSP/IDE 智能补全)" "rust-analyzer" "rust-analyzer"
    check_optional "cargo-edit (cargo add/rm)" "cargo-edit" "cargo-edit"
    check_optional "cargo-update (cargo install -u)" "cargo-update" "cargo-update"

    print_subsection "调试与追踪"
    check_optional "GDB 调试器 (含多架构支持)" "gdb-multiarch" "gdb-multiarch"
    check_optional "GDB 通用 (非多架构)" "gdb" "gdb"
    check_optional "strace (系统调用追踪)" "strace" "strace"

    print_subsection "辅助工具"
    check_optional "htop 进程监控" "htop" "htop"
    check_optional "tmux 终端复用" "tmux" "tmux"
    check_optional "tree 目录树查看" "tree" "tree"
    check_optional "jq JSON 处理 (CI 报告)" "jq" "jq"
    check_optional "ripgrep (文档/代码搜索)" "rg" "ripgrep"

    print_subsection "QEMU 增强 (磁盘/快照)"
    check_optional "qemu-img (磁盘镜像创建/转换)" "qemu-img" "qemu-utils"
fi

# ==================== 第 5 部分: C 链接层 ====================
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_C" = false ] && [ "$SKIP_C_LINKER" = false ]; then
    print_section "[5/8] C 链接层 (Linker) — 裸机链接 / 启动汇编"
    echo -e "  ${BLUE}说明: QueenX 源码已 100% Rust 化, C 链接层仅用于${NC}"
    echo -e "  ${BLUE}       1. 裸机链接 (ld + 链接脚本 src/kernel/framework/link/*.ld)${NC}"
    echo -e "  ${BLUE}       2. 启动汇编 (nasm x86_64 / aarch64-linux-gnu-as)${NC}"
    echo -e "  ${BLUE}       3. ELF→bin 转换 (objcopy, Makefile other/build/kernel.flat)${NC}"
    echo -e "  ${BLUE}  v3.2 决策: 全 Rust 化后已归类为可选, --skip-c-linker 可单独跳过${NC}"

    print_subsection "x86_64 工具链 (Makefile x86_64 条件分支)"
    check_c_legacy "x86_64-linux-gnu-ld (裸机链接)" "x86_64-linux-gnu-ld" "binutils-x86-64-linux-gnu"
    check_c_legacy "x86_64-linux-gnu-objcopy (ELF→bin)" "x86_64-linux-gnu-objcopy" "binutils-x86-64-linux-gnu"
    check_c_legacy "NASM (x86_64 启动汇编)" "nasm" "nasm"

    print_subsection "aarch64 工具链 (Makefile aarch64 条件分支)"
    check_c_legacy "aarch64-linux-gnu-ld (裸机链接)" "aarch64-linux-gnu-ld" "binutils-aarch64-linux-gnu"
    check_c_legacy "aarch64-linux-gnu-as (启动汇编)" "aarch64-linux-gnu-as" "binutils-aarch64-linux-gnu"
    check_c_legacy "aarch64-linux-gnu-objcopy (qemu_boot_test.sh)" "aarch64-linux-gnu-objcopy" "binutils-aarch64-linux-gnu"

    print_subsection "Binutils 辅助 (链接分析/段检查)"
    check_c_legacy "objdump (反汇编/段检查)" "objdump" "binutils"
    check_c_legacy "readelf (ELF 头/PHDR/SH 检查)" "readelf" "binutils"
fi

# ==================== 第 6 部分: C 测试桩 (v3.2 新分类) ====================
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_C" = false ]; then
    print_section "[6/8] C 测试桩 (C Test Stubs) — 已弃用, 仅历史兼容"
    echo -e "  ${BLUE}说明: src/kernel/tests/ 下的 kernel_test.c / test_main.c${NC}"
    echo -e "  ${BLUE}       / test_hw_stubs.c 已弃用, 但 Makefile 保留编译入口.${NC}"
    echo -e "  ${BLUE}  v3.2 决策: 全部 C 测试桩已迁移至 Rust 集成测试 (host-tests/tests/),${NC}"
    echo -e "  ${BLUE}             因此 C 测试桩归类为可选, --skip-c 即可跳过.${NC}"
    echo -e "  ${BLUE}  若用户需要历史 C 测试桩, 须安装以下 gcc 工具链:${NC}"

    print_subsection "交叉编译 gcc (Makefile CC 变量)"
    check_c_test_stub "x86_64-linux-gnu-gcc (x86_64 C 测试桩)" "x86_64-linux-gnu-gcc" "gcc-x86-64-linux-gnu"
    check_c_test_stub "aarch64-linux-gnu-gcc (aarch64 C 测试桩)" "aarch64-linux-gnu-gcc" "gcc-aarch64-linux-gnu"
fi

# ==================== 第 6 部分: ISO 制作 ====================
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_ISO" = false ]; then
    print_section "[7/8] ISO 制作 (ISO Build) — grub2-mkrescue + xorriso + mtools"
    echo -e "  ${BLUE}说明: 仅 make iso / make run-iso 时需要${NC}"
    echo -e "  ${BLUE}  可通过 --skip-iso 跳过此节${NC}"

    print_subsection "GRUB (BIOS Multiboot2 引导)"
    if check_command "grub2-mkrescue"; then
        ISO_OK=$((ISO_OK + 1))
        ISO_TOTAL=$((ISO_TOTAL + 1))
    elif check_command "grub-mkrescue"; then
        ISO_OK=$((ISO_OK + 1))
        ISO_TOTAL=$((ISO_TOTAL + 1))
        print_info "使用 grub-mkrescue (替代方案)"
    else
        ISO_TOTAL=$((ISO_TOTAL + 1))
        ISO_PACKAGES+=("grub2-common" "grub-pc-bin")
    fi

    print_subsection "ISO + 镜像工具"
    check_iso "xorriso (ISO 镜像生成)" "xorriso" "xorriso"
    check_iso "mformat (GRUB 启动镜像)" "mformat" "mtools"

    print_subsection "终端录制 (QEMU 启动日志)"
    check_iso "script (终端录制)" "script" "util-linux"
fi

# ==================== 第 8 部分: 项目本地工具 (v3.2 新增) ====================
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_PROJECT" = false ]; then
    print_section "[8/8] 项目本地工具 (Project Tools) — 框内核 TCB 审计"
    echo -e "  ${BLUE}说明: 项目根 tools/ 下的 TCB 审计脚本, 由 ci/audit.sh 引用${NC}"
    echo -e "  ${BLUE}       1. tools/check_tcb.sh      - services/ 0 unsafe 强制门禁${NC}"
    echo -e "  ${BLUE}       2. tools/audit_unsafe.sh   - framework/ SAFETY 注释覆盖率${NC}"
    echo -e "  ${BLUE}       3. tools/audit_unsafe.py   - 上述 sh 的 Python 解析版${NC}"
    echo -e "  ${BLUE}  缺失这些工具时 ci/audit.sh 会失败, 建议保留${NC}"
    echo -e "  ${BLUE}  可通过 --skip-project 跳过此节${NC}"

    print_subsection "TCB 边界门禁"
    check_project_tool "TCB 边界检查" "tools/check_tcb.sh"

    print_subsection "SAFETY 注释审计"
    check_project_tool "framework/ SAFETY 审计 (Bash)" "tools/audit_unsafe.sh"
    check_project_tool "framework/ SAFETY 审计 (Python)" "tools/audit_unsafe.py"
fi

# ==================== 第 7 部分: Python 模块 ====================
if [ "$SKIP_OPTIONAL" = false ]; then
    print_section "[项目胶水] Python 3 标准库 (CI 静态分析依赖)"
    check_python_modules

    print_subsection "Python 包 (可选, 逆向分析)"
    optional_py=("elftools" "capstone" "pyelftools")
    for mod in "${optional_py[@]}"; do
        printf "  ${CYAN}%-44s${NC}" "Python 包: $mod (可选, 逆向分析)"
        if check_python_module "$mod" 2>/dev/null; then
            echo -e "${GREEN}✓ 已安装${NC}"
        else
            echo -e "${YELLOW}○ 未安装 (可选)${NC}"
        fi
    done
fi

# ==================== 结果汇总 ====================
print_section "检查结果汇总"
print_hr

# 必需
if [ $REQUIRED_OK -eq $REQUIRED_TOTAL ] && [ $REQUIRED_TOTAL -gt 0 ]; then
    echo -e "  ${GREEN}✓ 必需依赖:   ${REQUIRED_OK}/${REQUIRED_TOTAL} 已满足${NC}"
else
    echo -e "  ${RED}✗ 必需依赖:   ${REQUIRED_OK}/${REQUIRED_TOTAL} (缺失 $((REQUIRED_TOTAL - REQUIRED_OK)) 项)${NC}"
fi

# 推荐
if [ "$SKIP_OPTIONAL" = false ]; then
    if [ $RECOMMENDED_OK -eq $RECOMMENDED_TOTAL ] && [ $RECOMMENDED_TOTAL -gt 0 ]; then
        echo -e "  ${GREEN}✓ 强烈推荐:   ${RECOMMENDED_OK}/${RECOMMENDED_TOTAL} 已满足${NC}"
    else
        echo -e "  ${YELLOW}△ 强烈推荐:   ${RECOMMENDED_OK}/${RECOMMENDED_TOTAL} (缺失 $((RECOMMENDED_TOTAL - RECOMMENDED_OK)) 项)${NC}"
    fi
fi

# 测试
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_TESTS" = false ]; then
    if [ $TESTING_OK -eq $TESTING_TOTAL ] && [ $TESTING_TOTAL -gt 0 ]; then
        echo -e "  ${MAGENTA}✓ Rust 测试:  ${TESTING_OK}/${TESTING_TOTAL} 已满足${NC}"
    else
        echo -e "  ${MAGENTA}△ Rust 测试:  ${TESTING_OK}/${TESTING_TOTAL} (缺失 $((TESTING_TOTAL - TESTING_OK)) 项)${NC}"
    fi
fi

# 可选
if [ "$SKIP_OPTIONAL" = false ]; then
    echo -e "  ${YELLOW}○ 可选依赖:   ${OPTIONAL_OK}/${OPTIONAL_TOTAL} 已满足${NC}"
fi

# C 链接层
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_C" = false ] && [ "$SKIP_C_LINKER" = false ]; then
    if [ $C_LEGACY_OK -eq $C_LEGACY_TOTAL ] && [ $C_LEGACY_TOTAL -gt 0 ]; then
        echo -e "  ${BLUE}※ C 链接层:   ${C_LEGACY_OK}/${C_LEGACY_TOTAL} 已满足${NC}"
    else
        echo -e "  ${BLUE}※ C 链接层:   ${C_LEGACY_OK}/${C_LEGACY_TOTAL} (缺失 $((C_LEGACY_TOTAL - C_LEGACY_OK)) 项)${NC}"
    fi
fi

# C 测试桩
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_C" = false ]; then
    if [ $C_TEST_STUB_TOTAL -gt 0 ]; then
        if [ $C_TEST_STUB_OK -eq $C_TEST_STUB_TOTAL ]; then
            echo -e "  ${BLUE}※ C 测试桩:   ${C_TEST_STUB_OK}/${C_TEST_STUB_TOTAL} 已满足${NC}"
        else
            echo -e "  ${BLUE}※ C 测试桩:   ${C_TEST_STUB_OK}/${C_TEST_STUB_TOTAL} (缺失 $((C_TEST_STUB_TOTAL - C_TEST_STUB_OK)) 项)${NC}"
        fi
    fi
fi

# 项目工具
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_PROJECT" = false ]; then
    if [ $PROJECT_TOOLS_TOTAL -gt 0 ]; then
        if [ $PROJECT_TOOLS_OK -eq $PROJECT_TOOLS_TOTAL ]; then
            echo -e "  ${CYAN}◇ 项目工具:   ${PROJECT_TOOLS_OK}/${PROJECT_TOOLS_TOTAL} 可用${NC}"
        else
            echo -e "  ${CYAN}◇ 项目工具:   ${PROJECT_TOOLS_OK}/${PROJECT_TOOLS_TOTAL} (缺失 $((PROJECT_TOOLS_TOTAL - PROJECT_TOOLS_OK)) 项)${NC}"
        fi
    fi
fi

# ISO
if [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_ISO" = false ]; then
    echo -e "  ${BLUE}※ ISO 制作:   ${ISO_OK}/${ISO_TOTAL} 已满足${NC}"
fi
print_hr

# ==================== 详细缺失清单 ====================
if [ ${#REQUIRED_PACKAGES[@]} -gt 0 ]; then
    echo ""
    echo -e "${RED}✗ 缺失必需包 (${#REQUIRED_PACKAGES[@]} 项):${NC}"
    for pkg in "${REQUIRED_PACKAGES[@]}"; do
        echo -e "  ${RED}•${NC} $pkg"
    done
fi

if [ ${#RECOMMENDED_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    echo -e "${YELLOW}△ 缺失推荐包 (${#RECOMMENDED_PACKAGES[@]} 项):${NC}"
    for pkg in "${RECOMMENDED_PACKAGES[@]}"; do
        echo -e "  ${YELLOW}•${NC} $pkg"
    done
fi

if [ ${#TESTING_PACKAGES[@]} -gt 0 ] && [ "$SKIP_TESTS" = false ]; then
    echo ""
    echo -e "${MAGENTA}△ 缺失测试工具 (${#TESTING_PACKAGES[@]} 项):${NC}"
    for pkg in "${TESTING_PACKAGES[@]}"; do
        echo -e "  ${MAGENTA}•${NC} $pkg (cargo install --locked $pkg)"
    done
fi

if [ ${#MISSING_CARGO_SUBCMDS[@]} -gt 0 ] && [ "$SKIP_TESTS" = false ]; then
    echo ""
    echo -e "${MAGENTA}△ 缺失 cargo 子命令 (${#MISSING_CARGO_SUBCMDS[@]} 项):${NC}"
    for entry in "${MISSING_CARGO_SUBCMDS[@]}"; do
        echo -e "  ${MAGENTA}•${NC} $entry (cargo install --locked \${entry#*=})"
    done
fi

if [ ${#MISSING_RUSTUP_COMPONENTS[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    echo -e "${YELLOW}△ 缺失 rustup 组件 (${#MISSING_RUSTUP_COMPONENTS[@]} 项):${NC}"
    for comp in "${MISSING_RUSTUP_COMPONENTS[@]}"; do
        echo -e "  ${YELLOW}•${NC} $comp (rustup component add $comp)"
    done
fi

if [ ${#MISSING_RUSTUP_TARGETS[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    echo -e "${YELLOW}△ 缺失 rustup targets (${#MISSING_RUSTUP_TARGETS[@]} 项):${NC}"
    for tgt in "${MISSING_RUSTUP_TARGETS[@]}"; do
        echo -e "  ${YELLOW}•${NC} $tgt (rustup target add $tgt)"
    done
fi

if [ ${#OPTIONAL_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    echo -e "${YELLOW}○ 缺失可选包 (${#OPTIONAL_PACKAGES[@]} 项):${NC}"
    for pkg in "${OPTIONAL_PACKAGES[@]}"; do
        echo -e "  ${YELLOW}•${NC} $pkg"
    done
fi

if [ ${#C_LEGACY_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_C" = false ] && [ "$SKIP_C_LINKER" = false ]; then
    echo ""
    echo -e "${BLUE}※ 缺失 C 链接层包 (${#C_LEGACY_PACKAGES[@]} 项):${NC}"
    for pkg in "${C_LEGACY_PACKAGES[@]}"; do
        echo -e "  ${BLUE}•${NC} $pkg"
    done
    echo -e "  ${BLUE}说明: v3.2 全 Rust 化后已归类为可选, --skip-c-linker 可单独跳过${NC}"
fi

if [ ${#C_TEST_STUB_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_C" = false ]; then
    echo ""
    echo -e "${BLUE}※ 缺失 C 测试桩包 (${#C_TEST_STUB_PACKAGES[@]} 项):${NC}"
    for pkg in "${C_TEST_STUB_PACKAGES[@]}"; do
        echo -e "  ${BLUE}•${NC} $pkg"
    done
    echo -e "  ${BLUE}说明: v3.2 C 测试桩已弃用, 仅 src/kernel/tests/*.c 历史兼容需要${NC}"
fi

if [ ${#PROJECT_TOOLS_MISSING[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_PROJECT" = false ]; then
    echo ""
    echo -e "${CYAN}◇ 缺失项目工具 (${#PROJECT_TOOLS_MISSING[@]} 项):${NC}"
    for tool in "${PROJECT_TOOLS_MISSING[@]}"; do
        echo -e "  ${CYAN}•${NC} $tool"
    done
    echo -e "  ${CYAN}说明: 这些工具由项目自带, 若缺失可能影响 ci/audit.sh 流程${NC}"
    echo -e "  ${CYAN}  请确认该文件存在且可执行:${NC}"
    for tool in "${PROJECT_TOOLS_MISSING[@]}"; do
        echo "    ls -la $tool"
    done
fi

if [ ${#ISO_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_ISO" = false ]; then
    echo ""
    echo -e "${BLUE}※ 缺失 ISO 制作包 (${#ISO_PACKAGES[@]} 项):${NC}"
    for pkg in "${ISO_PACKAGES[@]}"; do
        echo -e "  ${BLUE}•${NC} $pkg"
    done
    echo -e "  ${BLUE}说明: 仅 make iso / make run-iso 时需要${NC}"
fi

# ==================== 检查模式退出 ====================
if [ "$CHECK_ONLY" = true ]; then
    echo ""
    echo -e "${BLUE}检查模式: 未执行任何安装操作${NC}"
    echo ""
    echo -e "${CYAN}要安装缺失的依赖, 请重新运行:${NC}"
    echo "  $0             # 交互式"
    echo "  $0 -y          # 自动安装"
    if [ $REQUIRED_OK -lt $REQUIRED_TOTAL ]; then
        exit 1
    else
        exit 0
    fi
fi

# ==================== 交互式安装 ====================

# 安装必需包
if [ ${#REQUIRED_PACKAGES[@]} -gt 0 ]; then
    echo ""
    if ask_yes_no "是否自动安装缺失的 ${RED}必需${NC}依赖？" "y"; then
        install_packages "${REQUIRED_PACKAGES[@]}"
    else
        print_warning "跳过必需依赖安装"
    fi
fi

# 安装 rustup 组件
if [ ${#MISSING_RUSTUP_COMPONENTS[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${YELLOW}rustup 组件${NC}？" "y"; then
        install_rust_components "${MISSING_RUSTUP_COMPONENTS[@]}"
    else
        print_warning "跳过 rustup 组件安装"
        echo -e "  ${BLUE}稍后可手动运行:${NC}"
        for comp in "${MISSING_RUSTUP_COMPONENTS[@]}"; do
            echo "    rustup component add $comp"
        done
    fi
fi

# 安装 rustup targets
if [ ${#MISSING_RUSTUP_TARGETS[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${YELLOW}rustup targets${NC}？" "y"; then
        install_rust_targets "${MISSING_RUSTUP_TARGETS[@]}"
    else
        print_warning "跳过 rustup targets 安装"
    fi
fi

# 安装 cargo 子命令 (测试工具)
if [ ${#MISSING_CARGO_SUBCMDS[@]} -gt 0 ] && [ "$SKIP_TESTS" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${MAGENTA}cargo 测试子命令${NC} (cargo install --locked)？"; then
        install_cargo_subcmds "${MISSING_CARGO_SUBCMDS[@]}"
    else
        print_warning "跳过 cargo 子命令安装"
        echo ""
        echo -e "  ${BLUE}稍后可手动运行:${NC}"
        for entry in "${MISSING_CARGO_SUBCMDS[@]}"; do
            local subcmd="${entry%%=*}"
            local crate="${entry#*=}"
            echo "    cargo install --locked $crate   # 提供 cargo-${subcmd}"
        done
    fi
fi

# 安装系统包形式的测试工具
if [ ${#TESTING_PACKAGES[@]} -gt 0 ] && [ "$SKIP_TESTS" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${MAGENTA}测试工具系统包${NC}？"; then
        install_packages "${TESTING_PACKAGES[@]}"
    else
        print_warning "跳过测试工具系统包安装"
    fi
fi

# 安装推荐包
if [ ${#RECOMMENDED_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${YELLOW}推荐${NC}依赖？" "y"; then
        install_packages "${RECOMMENDED_PACKAGES[@]}"
    else
        print_warning "跳过推荐依赖安装"
    fi
fi

# 安装 C 链接层
if [ ${#C_LEGACY_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_C" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${BLUE}C 链接层${NC}工具链 (裸机链接/汇编/C 测试桩)？"; then
        install_packages "${C_LEGACY_PACKAGES[@]}"
    else
        print_warning "跳过 C 链接层安装"
        echo ""
        echo -e "  ${BLUE}如需构建可执行镜像, 可稍后运行:${NC}"
        echo "    sudo apt install ${C_LEGACY_PACKAGES[*]}"
    fi
fi

# 安装 ISO 制作
if [ ${#ISO_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ] && [ "$SKIP_ISO" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${BLUE}ISO 制作${NC}工具？"; then
        install_packages "${ISO_PACKAGES[@]}"
    else
        print_warning "跳过 ISO 制作工具安装"
        echo ""
        echo -e "  ${BLUE}如需 ISO 启动, 可稍后运行:${NC}"
        echo "    sudo apt install ${ISO_PACKAGES[*]}"
    fi
fi

# 安装可选包
if [ ${#OPTIONAL_PACKAGES[@]} -gt 0 ] && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    if ask_yes_no "是否安装缺失的 ${YELLOW}可选${NC}依赖？"; then
        install_packages "${OPTIONAL_PACKAGES[@]}"
    else
        print_warning "跳过可选依赖安装"
    fi
fi

# 检查 Rust 工具链 (若 rustc 未找到)
if ! has_cmd rustc && [ "$SKIP_OPTIONAL" = false ]; then
    echo ""
    if ask_yes_no "是否安装 ${RED}Rust 工具链${NC} (rustc/cargo/rustup)？"; then
        install_rust_toolchain
    else
        print_warning "跳过 Rust 工具链安装"
        echo ""
        echo -e "  ${BLUE}稍后可手动运行:${NC}"
        echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
    fi
fi

# ==================== 最终总结 ====================
echo ""
echo -e "${CYAN}════════════════════════════════════════════════════════════${NC}"
echo -e "${CYAN}                      检查完成                            ${NC}"
echo -e "${CYAN}════════════════════════════════════════════════════════════${NC}"
echo ""

if [ $REQUIRED_OK -eq $REQUIRED_TOTAL ] && [ $REQUIRED_TOTAL -gt 0 ]; then
    echo -e "  ${GREEN}✓ 必需依赖已满足, 可以开始构建 QueenX 内核${NC}"
else
    echo -e "  ${RED}✗ 必需依赖缺失, 请先安装后重试${NC}"
fi

if [ "$SKIP_OPTIONAL" = false ]; then
    if [ $RECOMMENDED_OK -eq $RECOMMENDED_TOTAL ] && [ $RECOMMENDED_TOTAL -gt 0 ]; then
        echo -e "  ${GREEN}✓ 强烈推荐依赖已满足, 验证工具齐备${NC}"
    else
        echo -e "  ${YELLOW}△ 推荐依赖部分缺失, Miri / SAFETY 审计可能受限${NC}"
    fi
fi

if [ "$SKIP_TESTS" = false ] && [ $TESTING_TOTAL -gt 0 ]; then
    if [ $TESTING_OK -eq $TESTING_TOTAL ]; then
        echo -e "  ${MAGENTA}✓ Rust 测试工具链已就位, CI/本地测试完备${NC}"
    else
        echo -e "  ${MAGENTA}△ Rust 测试工具链 $((TESTING_TOTAL - TESTING_OK)) 项缺失, 部分 CI 任务将跳过${NC}"
    fi
fi

echo ""
echo -e "${CYAN}下一步操作:${NC}"
echo ""
echo -e "  ${BOLD}编译构建 (Makefile):${NC}"
echo "    make all                      # 完整编译 (x86_64)"
echo "    make ARCH=aarch64 all         # aarch64 编译"
echo "    make run                      # QEMU 启动"
echo "    make run-iso                  # ISO 启动 (需 grub/xorriso)"
echo "    make qemu-boot-test ARCH=all  # 双架构启动验证"
echo ""
echo -e "  ${BOLD}测试验证 (Makefile.ci + cargo):${NC}"
echo "    make -f Makefile.ci ci                  # Full CI flow"
echo "    make -f Makefile.ci ci-audit            # SAFETY + boundary + deadlock"
echo "    make -f Makefile.ci ci-unsafe-scan      # services 0 unsafe"
echo "    make -f Makefile.ci ci-cargo            # cargo check (x86_64 + aarch64)"
echo "    make -f Makefile.ci ci-bench            # framekernel-bench + 回归检查"
echo "    make -f Makefile.ci ci-test-host        # host-tests 全量 (Cargo 自动发现)"
echo "    make test                                # test-host + test-unit"
echo ""
echo -e "  ${BOLD}代码质量 (cargo 子命令):${NC}"
echo "    cargo clippy -- -D warnings             # clippy.toml deny all"
echo "    cargo fmt --check                       # 格式门禁"
echo "    # miri 已于 2026-06-26 弃用, UB 检测由 Rust 编译期 + 7 个审计脚本覆盖"
echo "    cargo deny check                        # deny.toml 许可证/漏洞/版本"
echo "    cargo audit                             # RustSec 漏洞库"
echo "    cargo llvm-cov test --html              # 覆盖率 HTML 报告"
echo "    cargo mutants --test=unit               # 变异测试"
echo "    cargo bloat --release                   # 二进制大小"
echo "    cargo geiger                            # unsafe 代码量统计"
echo ""
echo -e "  ${BOLD}依赖验证 (本脚本):${NC}"
echo "    ./scripts/requirements.sh --check-only    # 重新检查环境"
echo "    ./scripts/requirements.sh --skip-c -y     # 跳过 C 链接层"
echo "    ./scripts/requirements.sh --skip-iso -y   # 跳过 ISO 制作"
echo ""
