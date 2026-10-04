#!/bin/bash

# ============================================================================
# QEMU 调试脚本 (QEMU Debug Script)
# 
# 用途：启动 QEMU 并配置调试环境
# 功能：
#   - 支持 GDB 远程调试
#   - 串口输出重定向
#   - VGA 显示配置
#   - 网络设备模拟
#   - 内存和 CPU 配置
# ============================================================================

set -e

# ============================================================================
# 配置变量
# ============================================================================

# QEMU 可执行文件
QEMU_BIN="${QEMU_BIN:-qemu-system-x86_64}"

# 内核镜像路径
KERNEL_IMG="${KERNEL_IMG:-other/build/kernel.flat}"

# 内存大小 (MB)
MEMORY_SIZE="${MEMORY_SIZE:-512}"

# CPU 类型
CPU_TYPE="${CPU_TYPE:-qemu64}"

# 调试模式 (0=关闭, 1=开启)
DEBUG_MODE="${DEBUG_MODE:-0}"

# GDB 端口
GDB_PORT="${GDB_PORT:-1234}"

# 串口输出文件
SERIAL_LOG="${SERIAL_LOG:-logs/serial.log}"

# 日志目录
LOG_DIR="${LOG_DIR:-logs}"

# 显示模式 (gtk, sdl, none)
DISPLAY_MODE="${DISPLAY_MODE:-gtk}"

# 网络模式 (0=关闭, 1=开启)
NETWORK_MODE="${NETWORK_MODE:-0}"

# ============================================================================
# 颜色定义
# ============================================================================

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# ============================================================================
# 辅助函数
# ============================================================================

print_banner() {
    echo -e "${BLUE}"
    echo "╔══════════════════════════════════════════════════════════╗"
    echo "║           Edgine - QEMU Debug Environment           ║"
    echo "╠══════════════════════════════════════════════════════════╣"
    echo "║  Kernel: ${KERNEL_IMG:0:20}...                          ║"
    echo "║  Memory: ${MEMORY_SIZE}MB                                        ║"
    echo "║  CPU:    ${CPU_TYPE}                                        ║"
    echo "║  Debug:  $([ $DEBUG_MODE -eq 1 ] && echo "Enabled (GDB port: $GDB_PORT)" || echo "Disabled")             ║"
    echo "║  Network: $([ $NETWORK_MODE -eq 1 ] && echo "Enabled (e1000)" || echo "Disabled")                   ║"
    echo "╚══════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

check_dependencies() {
    echo -e "${YELLOW}[CHECK] Checking dependencies...${NC}"
    
    # 检查 QEMU
    if ! command -v $QEMU_BIN &> /dev/null; then
        echo -e "${RED}[ERROR] QEMU not found: $QEMU_BIN${NC}"
        echo -e "${YELLOW}[HINT]  Install with: sudo apt install qemu-system-x86${NC}"
        exit 1
    fi
    echo -e "${GREEN}[OK] QEMU found: $(which $QEMU_BIN)${NC}"
    
    # 检查内核镜像
    if [ ! -f "$KERNEL_IMG" ]; then
        echo -e "${RED}[ERROR] Kernel image not found: $KERNEL_IMG${NC}"
        echo -e "${YELLOW}[HINT]  Build with: make build${NC}"
        exit 1
    fi
    echo -e "${GREEN}[OK] Kernel image found: $KERNEL_IMG${NC}"
    
    # 创建日志目录
    mkdir -p "$LOG_DIR"
    echo -e "${GREEN}[OK] Log directory: $LOG_DIR${NC}"
}

# ============================================================================
# QEMU 参数构建
# ============================================================================

build_qemu_args() {
    ARGS=()
    
    # 基础配置
    ARGS+=(-m "$MEMORY_SIZE")
    ARGS+=(-cpu "$CPU_TYPE")
    ARGS+=(-no-reboot)
    
    # 调试退出设备
    ARGS+=(-device isa-debug-exit,iobase=0xf4,iosize=0x04)
    
    # 内核镜像
    ARGS+=(-kernel "$KERNEL_IMG")
    
    # 显示配置
    if [ "$DISPLAY_MODE" = "none" ]; then
        ARGS+=(-display none)
        ARGS+=(-nographic)
        ARGS+=(-serial file:"$SERIAL_LOG")
    else
        ARGS+=(-display "$DISPLAY_MODE")
        ARGS+=(-serial stdio)
    fi
    
    # 调试模式
    if [ $DEBUG_MODE -eq 1 ]; then
        ARGS+=(-s)  # 等待 GDB 连接
        ARGS+=(-S)  # 启动时暂停
        ARGS+=(-gdb tcp::"$GDB_PORT")
    fi
    
    # 网络配置
    if [ $NETWORK_MODE -eq 1 ]; then
        ARGS+=(-device e1000,netdev=n0)
        ARGS+=(-netdev user,id=n0,hostfwd=tcp::8080-:80,hostfwd=tcp::2222-:22)
    fi
    
    # 调试输出
    ARGS+=(-d cpu_reset,guest_errors)
    ARGS+=(-D "$LOG_DIR/qemu_debug.log")
    
    echo "${ARGS[@]}"
}

# ============================================================================
# GDB 配置文件生成
# ============================================================================

generate_gdb_init() {
    local GDB_INIT=".gdbinit.antx"
    
    cat > "$GDB_INIT" << EOF
# Edgine GDB Configuration
set architecture i386:x86-64
target remote localhost:$GDB_PORT

# 常用断点
# break kernel_main
# break panic

# 自动加载符号
# add-symbol-file $KERNEL_IMG

# 显示设置
set print pretty on
set print array on
set print array-indexes on

echo \\n
echo ╔════════════════════════════════════════╗\\n
echo ║   Edgine - GDB Debug Session      ║\\n
echo ╚════════════════════════════════════════╝\\n
echo \\n
echo Ready to debug. Use 'c' to continue.\\n
EOF
    
    echo -e "${GREEN}[OK] GDB init file generated: $GDB_INIT${NC}"
    echo -e "${YELLOW}[HINT] Start GDB with: gdb -x $GDB_INIT${NC}"
}

# ============================================================================
# 主函数
# ============================================================================

main() {
    print_banner
    check_dependencies
    
    # 构建参数
    QEMU_ARGS=$(build_qemu_args)
    
    echo -e "${GREEN}[START] Launching QEMU...${NC}"
    echo -e "${BLUE}[CMD] $QEMU_BIN $QEMU_ARGS${NC}"
    echo ""
    
    # 生成 GDB 配置 (如果调试模式开启)
    if [ $DEBUG_MODE -eq 1 ]; then
        generate_gdb_init
    fi
    
    # 启动 QEMU
    $QEMU_BIN $QEMU_ARGS
    
    EXIT_CODE=$?
    
    echo ""
    echo -e "${BLUE}╔════════════════════════════════════════╗${NC}"
    echo -e "${BLUE}║         QEMU Session Ended             ║${NC}"
    echo -e "${BLUE}╠════════════════════════════════════════╣${NC}"
    echo -e "${BLUE}║  Exit Code: $EXIT_CODE                         ║${NC}"
    echo -e "${BLUE}║  Serial Log: $SERIAL_LOG          ║${NC}"
    echo -e "${BLUE}║  Debug Log: $LOG_DIR/qemu_debug.log  ║${NC}"
    echo -e "${BLUE}╚════════════════════════════════════════╝${NC}"
}

# ============================================================================
# 帮助信息
# ============================================================================

show_help() {
    echo "Edgine QEMU Debug Script"
    echo ""
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  -k, --kernel PATH    Kernel image path (default: other/build/kernel.flat)"
    echo "  -m, --memory SIZE    Memory size in MB (default: 512)"
    echo "  -c, --cpu TYPE       CPU type (default: qemu64)"
    echo "  -d, --debug          Enable GDB debug mode"
    echo "  -p, --port PORT      GDB port (default: 1234)"
    echo "  -n, --network        Enable network emulation"
    echo "  -D, --display MODE   Display mode: gtk, sdl, none (default: gtk)"
    echo "  -h, --help           Show this help message"
    echo ""
    echo "Examples:"
    echo "  # Normal run with VGA display"
    echo "  $0"
    echo ""
    echo "  # Debug mode with GDB"
    echo "  $0 -d"
    echo "  # Then in another terminal:"
    echo "  gdb -x .gdbinit.antx"
    echo ""
    echo "  # Headless mode with network"
    echo "  $0 -D none -n"
    echo ""
    echo "  # Custom memory and CPU"
    echo "  $0 -m 1024 -c host"
}

# ============================================================================
# 参数解析
# ============================================================================

while [[ $# -gt 0 ]]; do
    case $1 in
        -k|--kernel)
            KERNEL_IMG="$2"
            shift 2
            ;;
        -m|--memory)
            MEMORY_SIZE="$2"
            shift 2
            ;;
        -c|--cpu)
            CPU_TYPE="$2"
            shift 2
            ;;
        -d|--debug)
            DEBUG_MODE=1
            shift
            ;;
        -p|--port)
            GDB_PORT="$2"
            shift 2
            ;;
        -n|--network)
            NETWORK_MODE=1
            shift
            ;;
        -D|--display)
            DISPLAY_MODE="$2"
            shift 2
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            show_help
            exit 1
            ;;
    esac
done

# 运行主函数
main
