#!/bin/bash
# ============================================================================
# Edgine IPv6 SLAAC / DHCPv6 端到端联调测试 (IPv6 Autoconfig End-to-End Validation)
#
# 用途: 以 QEMU tap0 直连宿主 dnsmasq, 按 MODE 验证两种 IPv6 自动配置形态:
#   MODE=slaac (默认, P6a): dnsmasq 广播 RA (前缀 fd00::/64, ra-only), 验证:
#     1. 内核接收 RA 并派生 SLAAC 全局地址 fd00::5054:ff:fe12:3456/64;
#     2. 用户态经 AF_INET6/sockaddr_in6 向宿主 [fd00::1]:9999 UDP 收发往返成功.
#   MODE=dhcpv6 (P6b): dnsmasq 改有状态地址池 + RA 置 M/O 位, 验证:
#     1. 内核 raw ICMPv6 捕获 RA → M=1 触发有状态 SARR, 命中 "DHCPv6 bound:" 里程碑;
#     2. SLAAC 共存 (RA 前缀仍派生地址, 证 raw 捕获不破坏内部 SLAAC);
#     3. 绑定后用户态 IPv6 UDP 收发往返仍 ok.
# 对应模式的里程碑全部命中即判定 IPv6 链路连通.
#
# 依赖 (宿主, 需 root 或 sudo 免密):
#   - ip (iproute2)      : 配置 tap0 设备与宿主侧 IPv6 地址
#   - dnsmasq            : RA 路由通告 + (dhcpv6 模式) 有状态 DHCPv6 服务
#   - python3            : UDP6 回显服务
#   - qemu-system-x86_64
#
# 输出: other/build/log/qemu_slaac_x86_64.log
# 退出码: 0 = 对应模式里程碑全部命中; 1 = 任一未命中或环境不满足
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

# --- 模式: slaac (默认, P6a 原行为) | dhcpv6 (P6b 有状态 SARR e2e) ---
# dhcpv6 模式: dnsmasq 改有状态地址池 + RA 置 M/O 位, 内核经 RA M=1 触发
# DHCPv6 SARR, 断言 "DHCPv6 bound:" 里程碑 (functions driver 一次性上报).
MODE="${MODE:-slaac}"
DHCPV6_POOL_START="${DHCPV6_POOL_START:-fd00::1000}"
DHCPV6_POOL_END="${DHCPV6_POOL_END:-fd00::2000}"
DHCPV6_LEASE="${DHCPV6_LEASE:-12h}"

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
QEMU_PID=""
cleanup() {
    if [ -n "$QEMU_PID" ] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
    fi
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
        # nodad: 测试 tap 无旁路主机, 跳过 DAD 使地址立即可用 (避免 NO-CARRIER
        # 期 DAD 停摆致地址 tentative, 回显服务 bind 失败 Errno 99).
        $SUDO ip -6 addr add "$HOST_V6/$PREFIX_LEN" dev "$TAP_IF" nodad
    fi
    $SUDO ip link set "$TAP_IF" up
}

# --- 启动 dnsmasq RA 服务 (自管 conf/pid, 不污染系统配置) ---
start_dnsmasq() {
    TMP_DIR="$(mktemp -d)"
    DNSMASQ_CONF="$TMP_DIR/dnsmasq-ra.conf"
    DNSMASQ_PID="$TMP_DIR/dnsmasq.pid"
    if [ "$MODE" = "dhcpv6" ]; then
        # 有状态 DHCPv6 地址池: dnsmasq 配置 dhcp-range 后 RA 自动置 M=1/O=1,
        # 内核 raw ICMPv6 捕获 RA → ra::parse 提取 M 位 → 触发有状态 SARR.
        # information-refresh-time 选项供无状态 (O=1) 路径取刷新间隔.
        cat > "$DNSMASQ_CONF" <<EOF
interface=$TAP_IF
bind-interfaces
enable-ra
dhcp-range=$DHCPV6_POOL_START,$DHCPV6_POOL_END,$PREFIX_LEN,$DHCPV6_LEASE
dhcp-option=option6:information-refresh-time,3600
no-resolv
no-hosts
port=0
pid-file=$DNSMASQ_PID
log-dhcp
EOF
        info "启动 dnsmasq 有状态 DHCPv6 服务 (池 $DHCPV6_POOL_START-$DHCPV6_POOL_END, RA M/O)"
    else
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
    fi
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
        -m 512 -smp 2 \
        -kernel other/build/x86_64/kernel.flat \
        -netdev tap,id=n0,ifname="$TAP_IF",script=no,downscript=no \
        -device e1000,netdev=n0,mac="$KERNEL_MAC" \
        >/dev/null 2>&1 || true
}

# --- 等待 tap0 载波就位 (QEMU 打开设备后 LOWER_UP → state UP) ---
# TAP 仅在有进程 (QEMU) 持有打开时才有载波; 无载波期 IPv6 DAD 无法完成,
# 宿主地址停留 tentative → 回显 bind 失败 (Errno 99) 且 dnsmasq 无法发 RA.
# 故必须先起 QEMU 建立载波, 再启 dnsmasq/回显. 有界轮询 ~10s.
wait_for_carrier() {
    local i
    for i in $(seq 1 50); do
        if ip link show "$TAP_IF" 2>/dev/null | grep -q "state UP"; then
            return 0
        fi
        sleep 0.2
    done
    return 1
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

if [ "$MODE" != "slaac" ] && [ "$MODE" != "dhcpv6" ]; then
    err "MODE 须为 slaac 或 dhcpv6 (当前: '$MODE')"
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

if [ ! -f other/build/x86_64/kernel.flat ]; then
    err "other/build/x86_64/kernel.flat 缺失, 请先执行 make ARCH=x86_64 all"
    exit 1
fi

ensure_tap
# 先起 QEMU 建立 tap0 载波 (后台), 再启宿主服务: 无载波期 IPv6 DAD 停摆,
# 宿主地址 tentative 会致回显 bind 失败 (Errno 99) 且 dnsmasq 无法发 RA.
run_qemu &
QEMU_PID=$!
if ! wait_for_carrier; then
    err "tap0 载波未就位 (QEMU 未能打开设备?), 终止"
    exit 1
fi
start_dnsmasq
start_echo
# 给 dnsmasq/回显服务留出就绪窗口 (内核启动后经 RS/RA 交互派生地址)
sleep 1

wait "$QEMU_PID" || true

echo
info "=== 里程碑断言 (日志: $LOG, 模式: $MODE) ==="
RESULT=0
if [ "$MODE" = "dhcpv6" ]; then
    # 主判据: 有状态 SARR 绑定里程碑 (functions driver 首次落地址一次性上报).
    check_marker "内核 DHCPv6 有状态绑定 (RA M=1 触发 SARR)" \
        "DHCPv6 bound:" || RESULT=1
    # 共存判据: raw ICMPv6 捕获不破坏 smoltcp 内部 SLAAC (RA 前缀仍派生地址).
    check_marker "SLAAC 共存全局地址派生 ($KERNEL_V6/$PREFIX_LEN)" \
        "IPv6 SLAAC: global address $KERNEL_V6/$PREFIX_LEN acquired" || RESULT=1
    # 链路判据: 绑定后用户态 IPv6 UDP 收发往返仍 ok.
    check_marker "用户态 IPv6 UDP 发送 (TX ok)" "[net6] TX ok" || RESULT=1
    check_marker "用户态 IPv6 UDP 回显接收 (RX ok)" "[net6] RX ok" || RESULT=1
else
    check_marker "内核 SLAAC 全局地址派生 ($KERNEL_V6/$PREFIX_LEN)" \
        "IPv6 SLAAC: global address $KERNEL_V6/$PREFIX_LEN acquired" || RESULT=1
    check_marker "用户态 IPv6 UDP 发送 (TX ok)" "[net6] TX ok" || RESULT=1
    check_marker "用户态 IPv6 UDP 回显接收 (RX ok)" "[net6] RX ok" || RESULT=1
fi

echo
if [ "$RESULT" -eq 0 ]; then
    if [ "$MODE" = "dhcpv6" ]; then
        ok "IPv6 DHCPv6 联调通过: RA M=1 触发有状态 SARR 绑定 + SLAAC 共存 + UDP 收发 ok"
    else
        ok "IPv6 SLAAC 联调通过: 链路连通 (RA 派生 + 用户态 UDP 收发往返 ok)"
    fi
else
    err "IPv6 $MODE 联调未通过 (日志: $LOG)"
    warn "关键相关行 (末 20 条):"
    grep -aE "\[net6\]|SLAAC|DHCPv6|\[NET\]" "$LOG" | tail -20 || true
fi
exit "$RESULT"
