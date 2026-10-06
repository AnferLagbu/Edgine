#!/bin/bash
# ============================================================================
# Edgine IPv6 SLAAC 端到端联调测试 (IPv6 SLAAC End-to-End Validation)
#
# 用途: 以 QEMU tap0 直连宿主 dnsmasq (广播 RA, 前缀 fd00::/64), 验证:
#   1. 内核接收 RA 并派生 SLAAC 全局地址 fd00::5054:ff:fe12:3456/64;
#   2. 用户态经 AF_INET6/sockaddr_in6 向宿主 [fd00::1]:9999 UDP 收发往返成功.
# 三条里程碑全部命中即判定 IPv6 链路连通 (P6a 联调通过).
#
# 依赖 (宿主, 需 root 或 sudo 免密):
#   - ip (iproute2)      : 配置 tap0 设备与宿主侧 IPv6 地址
#   - dnsmasq            : RA 路由通告服务 (--enable-ra)
#   - python3            : UDP6 回显服务
#   - qemu-system-x86_64
#
# 输出: other/build/log/qemu_slaac_x86_64.log
# 退出码: 0 = 三条里程碑全部命中; 1 = 任一未命中或环境不满足
#
# 环境约定 (与 src/user/init/src/main.rs::ipv6_udp_probe 一致):
#   内核 MAC 52:54:00:12:34:56 → EUI-64 SLAAC 地址 fd00::5054:ff:fe12:3456
#   宿主 tap0 fd00::1/64; 用户态本地端口 7777; 宿主回显端口 9999
#
# 说明: 仅支持 x86_64 (aarch64 无 tap+RA 联调环境); tap0 为 persistent,
#       测试后保留, 故重复运行无需重建设备. 脚本仅清理自启的 dnsmasq 与回显.
# ============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/qemu_slaac_x86_64.log"

# --- 联调参数 (可用环境变量覆盖) ---
TAP_IF="${TAP_IF:-tap0}"
KERNEL_MAC="${KERNEL_MAC:-52:54:00:12:34:56}"
KERNEL_V6="${KERNEL_V6:-fd00::5054:ff:fe12:3456}"
HOST_V6="${HOST_V6:-fd00::1}"
PREFIX_LEN="${PREFIX_LEN:-64}"
RA_RANGE="${RA_RANGE:-fd00::}"
ECHO_PORT="${ECHO_PORT:-9999}"
TIMEOUT_QEMU="${TIMEOUT_QEMU:-30}"
SKIP_BUILD="${SKIP_BUILD:-0}"

ok()   { echo -e "${GREEN}\u2713 $1${NC}"; }
err()  { echo -e "${RED}\u2717 $1${NC}"; }
warn() { echo -e "${YELLOW}! $1${NC}"; }
info() { echo -e "${BLUE}-> $1${NC}"; }

# --- sudo 选择: root 直跑, 否则要求免密 sudo (CI/自动化不可交互) ---
if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
elif sudo -n true 2>/dev/null; then
    SUDO="sudo -n"
else
    err "需 root 或 sudo 免密以配置 $TAP_IF 与 dnsmasq (当前 sudo 需交互)"
    exit 1
fi

# --- 环境清理 (仅清理本脚本自启的进程与临时文件) ---
ECHO_PID=""
DNSMASQ_PID=""
DNSMASQ_CONF=""
TMP_DIR=""
cleanup() {
    if [ -n "$ECHO_PID" ] && kill -0 "$ECHO_PID" 2>/dev/null; then
        kill "$ECHO_PID" 2>/dev/null || true
    fi
    if [ -n "$DNSMASQ_PID" ] && [ -f "$DNSMASQ_PID" ]; then
        $SUDO kill "$(cat "$DNSMASQ_PID" 2>/dev/null)" 2>/dev/null || true
    fi
    [ -n "$TMP_DIR" ] && rm -rf "$TMP_DIR"
    return 0
}
trap cleanup EXIT INT TERM

# --- 确保 tap0 就位 (persistent, 归属当前用户, QEMU 方可非 root 打开) ---
ensure_tap() {
    if ! ip link show "$TAP_IF" >/dev/null 2>&1; then
        info "创建 persistent TAP 设备 $TAP_IF (owner=$(id -un))"
        $SUDO ip tuntap add dev "$TAP_IF" mode tap user "$(id -un)"
    fi
    if ! ip -6 addr show dev "$TAP_IF" | grep -q "$HOST_V6/$PREFIX_LEN"; then
        info "为 $TAP_IF 配置宿主地址 $HOST_V6/$PREFIX_LEN"
        $SUDO ip -6 addr add "$HOST_V6/$PREFIX_LEN" dev "$TAP_IF"
    fi
    $SUDO ip link set "$TAP_IF" up
}

# --- 启动 dnsmasq RA 服务 (自管 conf/pid, 不污染系统配置) ---
start_dnsmasq() {
    TMP_DIR="$(mktemp -d)"
    DNSMASQ_CONF="$TMP_DIR/dnsmasq-ra.conf"
    DNSMASQ_PID="$TMP_DIR/dnsmasq.pid"
    cat > "$DNSMASQ_CONF" <<EOF
interface=$TAP_IF
bind-interfaces
enable-ra
dhcp-range=$RA_RANGE,ra-only
no-resolv
no-hosts
port=0
pid-file=$DNSMASQ_PID
log-dhcp
EOF
    info "启动 dnsmasq RA 服务 (前缀 $RA_RANGE/$PREFIX_LEN)"
    if ! $SUDO dnsmasq --conf-file="$DNSMASQ_CONF"; then
        err "dnsmasq 启动失败"
        return 1
    fi
    return 0
}

# --- 启动宿主 UDP6 回显服务 (把收到的报文原样回给来源) ---
start_echo() {
    cat > "$TMP_DIR/echo6.py" <<'PY'
import socket
import sys

host = sys.argv[1]
port = int(sys.argv[2])
s = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind((host, port))
while True:
    data, addr = s.recvfrom(2048)
    s.sendto(data, addr)
PY
    info "启动宿主 UDP6 回显服务 [$HOST_V6]:$ECHO_PORT"
    python3 "$TMP_DIR/echo6.py" "$HOST_V6" "$ECHO_PORT" &
    ECHO_PID=$!
}

# --- 在 tap0 上运行内核镜像 (e1000 直连, 使用固定 MAC 以匹配预期 SLAAC 地址) ---
run_qemu() {
    info "启动 QEMU x86_64 (tap=$TAP_IF mac=$KERNEL_MAC timeout=${TIMEOUT_QEMU}s)"
    rm -f "$LOG"
    timeout "$TIMEOUT_QEMU" qemu-system-x86_64 \
        -serial "file:${LOG}" \
        -display none \
        -no-reboot \
        -m 512 -smp 1 \
        -kernel other/build/kernel.flat \
        -netdev tap,id=n0,ifname="$TAP_IF",script=no,downscript=no \
        -device e1000,netdev=n0,mac="$KERNEL_MAC" \
        >/dev/null 2>&1 || true
}

# --- 里程碑断言 (字面匹配: 标记含 [ ] 与 . 等正则元字符, 用 -F) ---
check_marker() {
    local label="$1"
    local needle="$2"
    if grep -aqF "$needle" "$LOG"; then
        ok "$label"
        return 0
    fi
    err "$label — 未命中: '$needle'"
    return 1
}

# ============================================================================
# 主流程
# ============================================================================
ARCH="${1:-x86_64}"
if [ "$ARCH" != "x86_64" ]; then
    err "本脚本仅支持 x86_64 (aarch64 无 tap+RA 联调环境)"
    exit 1
fi

for tool in qemu-system-x86_64 dnsmasq python3 ip; do
    command -v "$tool" >/dev/null 2>&1 || { err "缺少依赖: $tool"; exit 1; }
done

# init 内嵌于内核镜像 (include_bytes!), 用户态源码变更须先重建
if [ "$SKIP_BUILD" != "1" ]; then
    info "构建 x86_64 用户态 + 内核 (make ARCH=x86_64 user all)"
    if ! make ARCH=x86_64 user all >/dev/null 2>&1; then
        err "构建失败, 请手动执行 make ARCH=x86_64 user all 查看详情"
        exit 1
    fi
fi

if [ ! -f other/build/kernel.flat ]; then
    err "other/build/kernel.flat 缺失, 请先执行 make ARCH=x86_64 all"
    exit 1
fi

ensure_tap
start_dnsmasq
start_echo
# 给 dnsmasq/回显服务留出就绪窗口 (内核启动后经 RS/RA 交互派生地址)
sleep 1

run_qemu

echo
info "=== 里程碑断言 (日志: $LOG) ==="
RESULT=0
check_marker "内核 SLAAC 全局地址派生 ($KERNEL_V6/$PREFIX_LEN)" \
    "IPv6 SLAAC: global address $KERNEL_V6/$PREFIX_LEN acquired" || RESULT=1
check_marker "用户态 IPv6 UDP 发送 (TX ok)" "[net6] TX ok" || RESULT=1
check_marker "用户态 IPv6 UDP 回显接收 (RX ok)" "[net6] RX ok" || RESULT=1

echo
if [ "$RESULT" -eq 0 ]; then
    ok "IPv6 SLAAC 联调通过: 链路连通 (RA 派生 + 用户态 UDP 收发往返 ok)"
else
    err "IPv6 SLAAC 联调未通过 (日志: $LOG)"
    warn "关键相关行 (末 20 条):"
    grep -aE "\[net6\]|SLAAC|\[NET\]" "$LOG" | tail -20 || true
fi
exit "$RESULT"
