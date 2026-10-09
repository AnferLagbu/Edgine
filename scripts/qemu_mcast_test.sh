#!/bin/bash
# ============================================================================
# Edgine IPv4/IPv6 组播成员管理端到端验证 (P6 / D11 Multicast End-to-End Validation)
#
# 用途: 用 QEMU 的 socket (UDP 隧道) netdev 把宿主构造的**组播以太网帧**灌入 guest
#   的 e1000, 端到端验证:
#     setsockopt(IP_ADD_MEMBERSHIP) → per-socket 引用计数 → iface 组表
#     → IGMPv2 Report 出向 → L3 RX 过滤放行组播 dst → UDP 端口匹配投递
#     → IP_DROP_MEMBERSHIP → IGMP Leave 出向
#   以及 IPv6 成员管理的 ABI 腿 (IPPROTO_IPV6 level 命中 + level/optname 成对).
#   十条里程碑全命中即判定组播链路连通.
#
# 拓扑原理 (关键: QEMU socket netdev 传的是完整 L2 帧, 不是宿主 IP 报文):
#   - `-netdev socket,udp=A:pa,localaddr=B:pb` 是一条 UDP 隧道: QEMU 把从 pb 收到
#     的数据报 **payload 原样当作以太网帧** 投递给 guest; 反向同理 —— guest 的 TX
#     帧被 QEMU 当作 payload 发到 pa. 两个地址均取 127.0.0.1, 故不依赖宿主组播路由.
#     (实测: 同机两进程各自 bind 同一多播 group:port 时互不可达, 故传输段走 loopback
#      UDP; 组播语义完全由**内层帧**承载, 与传输段无关.)
#   - 因此注入器必须自行构造 Ethernet + IPv4 + UDP 三层头, 且 IP 头校验和与 UDP
#     校验和都必须正确 (内核 `EGDFNetDevice::capabilities()` 用
#     `DeviceCapabilities::default()`, 未声明 checksum offload → smoltcp 逐包校验).
#   - 帧内 Ethernet dst MAC 必须由**内层组地址**低 23 位派生
#     (01:00:5e + (b1 & 0x7f, b2, b3)); 与内层 IPv4 dst 不一致会被 smoltcp 丢弃.
#   - 帧补齐到以太网最小 60 字节 (不带 FCS); 真实 NIC 会 padding, 不给模型留歧义.
#
# 与常规启动测试的差异: 该 netdev 无 DHCP/网关语义, 内核走 `FALLBACK_IPV4`
#   (10.0.2.15/24) 静态回退, 故 boot 里程碑与 qemu_boot_test.sh 不同, 单开本脚本.
#
# 依赖 (宿主, 无需 root): qemu-system-x86_64 + python3; 仅需 loopback UDP 可用.
#
# 输出: other/build/log/qemu_mcast_x86_64.log   (guest 串口)
#       other/build/log/qemu_mcast_inject.log   (宿主注入器 / IGMP 观测)
# 退出码: 0 = 十条里程碑全部命中; 1 = 任一未命中或环境不满足
#
# 环境约定 (与 src/user/init/src/main.rs::mcast_probe 逐字一致):
#   QEMU ↔ 注入器 UDP 隧道: QEMU localaddr=127.0.0.1:12346, 对端 127.0.0.1:12345
#   内层目的组 239.255.42.42 / guest 监听 UDP 7890 / 载荷 "EDGEMCAST"
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
LOG="$LOG_DIR/qemu_mcast_x86_64.log"
INJECT_LOG="$LOG_DIR/qemu_mcast_inject.log"

# --- 联调参数 (可用环境变量覆盖) ---
# QEMU UDP 隧道两端: 注入器 listen 于 HOST_PORT 收 guest TX, 向 PEER_PORT 注帧.
HOST_ADDR="${HOST_ADDR:-127.0.0.1}"
HOST_PORT="${HOST_PORT:-12345}"
PEER_PORT="${PEER_PORT:-12346}"
GUEST_GROUP="${GUEST_GROUP:-239.255.42.42}"
GUEST_PORT="${GUEST_PORT:-7890}"
KERNEL_MAC="${KERNEL_MAC:-52:54:00:12:34:56}"
# 注入周期与轮数: 覆盖 "内核 DHCP 超时 → fallback 静态 IP → init 跑探针" 的整个窗口.
INJECT_INTERVAL_MS="${INJECT_INTERVAL_MS:-250}"
INJECT_ROUNDS="${INJECT_ROUNDS:-400}"
TIMEOUT_QEMU="${TIMEOUT_QEMU:-90}"
SKIP_BUILD="${SKIP_BUILD:-0}"

ok()   { echo -e "${GREEN}\u2713 $1${NC}"; }
err()  { echo -e "${RED}\u2717 $1${NC}"; }
warn() { echo -e "${YELLOW}! $1${NC}"; }
info() { echo -e "${BLUE}-> $1${NC}"; }

INJECT_PID=""
TMP_DIR=""
cleanup() {
    if [ -n "$INJECT_PID" ] && kill -0 "$INJECT_PID" 2>/dev/null; then
        kill "$INJECT_PID" 2>/dev/null || true
    fi
    [ -n "$TMP_DIR" ] && rm -rf "$TMP_DIR"
    return 0
}
trap cleanup EXIT INT TERM

# ============================================================================
# 宿主注入器: join 中转组 → 周期发送自造组播 L2 帧 → 反向解析收到的 IGMP 报文
# ============================================================================
write_injector() {
    cat > "$TMP_DIR/mcast_inject.py" <<'PY'
"""组播帧注入 + guest IGMP 出向观测 (QEMU socket netdev 的宿主对端)."""
import select
import socket
import struct
import sys

host_addr, host_port = sys.argv[1], int(sys.argv[2])
peer_port = int(sys.argv[3])
guest_group, guest_port = sys.argv[4], int(sys.argv[5])
interval_ms, rounds = int(sys.argv[6]), int(sys.argv[7])

g = [int(x) for x in guest_group.split(".")]
src_ip = "10.0.2.99"
payload = b"EDGEMCAST"


def csum(data: bytes) -> int:
    """RFC 1071 一补数和 (16-bit)."""
    if len(data) % 2:
        data += b"\x00"
    total = sum(struct.unpack("!%dH" % (len(data) // 2), data))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def multicast_mac(addr: list[int]) -> bytes:
    """IPv4 组地址 → Ethernet 组播 MAC: 01:00:5e + 低 23 位."""
    return bytes([0x01, 0x00, 0x5E, addr[1] & 0x7F, addr[2], addr[3]])


def build_frame() -> bytes:
    """Ethernet + IPv4(DF, ttl=64, proto=UDP) + UDP, 两级校验和均自算."""
    udplen = 8 + len(payload)
    # 先用占位校验和 0 求出 IP 头校验和 (校验和字段计算时置零).
    ihl_ver = b"\x45\x00"
    totlen = struct.pack("!H", 20 + udplen)
    ident = struct.pack("!H", 0x0001)
    flags = struct.pack("!H", 0x4000)
    ttl_proto = b"\x40\x11"
    addrs = socket.inet_aton(src_ip) + socket.inet_aton(guest_group)
    ip_csum = struct.pack("!H", csum(ihl_ver + totlen + ident + flags + ttl_proto + addrs))
    ipv4 = ihl_ver + totlen + ident + flags + ttl_proto + ip_csum + addrs

    udp_hdr_wo_csum = struct.pack("!HHH", 5000, guest_port, udplen) + b"\x00\x00"
    # UDP 校验和含 IPv4 伪头 (src, dst, zero, proto=17, udp 长度).
    pseudo = addrs + b"\x00\x11" + struct.pack("!H", udplen)
    u = csum(pseudo + udp_hdr_wo_csum + payload) or 0xFFFF
    udp = udp_hdr_wo_csum[:6] + struct.pack("!H", u) + payload

    eth = multicast_mac(g) + bytes.fromhex("020000000001") + struct.pack("!H", 0x0800)
    frame = eth + ipv4 + udp
    # 补齐到以太网最小帧 (60B, 不含 FCS): 真实 NIC 会 padding, UDP 长度字段不变
    # 故 smoltcp 只看 IP 总长, 尾部 pad 自然忽略.
    if len(frame) < 60:
        frame += b"\x00" * (60 - len(frame))
    return frame


# 单 socket 双向: bind 于 QEMU 的 TX 目的端 (host_port), 注帧发到 QEMU 的 localaddr.
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind((host_addr, host_port))
dest = (host_addr, peer_port)
print(
    f"[inject] bound {host_addr}:{host_port} <-> {host_addr}:{peer_port}, "
    f"inner group {guest_group}",
    flush=True,
)

frame = build_frame()
for i in range(rounds):
    ready = select.select([s], [], [], interval_ms / 1000.0)[0]
    if ready:
        # 同一组里也会收到自己发的帧 (loop) 与其它 guest TX; 只关心 IGMP(proto=2).
        data, _ = s.recvfrom(2048)
        if len(data) > 14 + 20 and data[12:14] == b"\x08\x00" and data[14 + 9] == 2:
            ihl = (data[14] & 0x0F) * 4
            ip = data[14 : 14 + ihl]
            src = socket.inet_ntoa(ip[12:16])
            dst = socket.inet_ntoa(ip[16:20])
            igmp = data[14 + ihl :]
            mtype = igmp[0]
            grp = socket.inet_ntoa(igmp[4:8]) if len(igmp) >= 8 else "?"
            name = {0x11: "QUERY", 0x16: "REPORT", 0x17: "LEAVE", 0x22: "V3_REPORT"}.get(
                mtype, f"0x{mtype:02x}"
            )
            print(f"[igmp] {name} src={src} dst={dst} group={grp}", flush=True)
            if name == "REPORT" and grp == guest_group:
                print(f"[igmp] IGMP-REPORT {guest_group}", flush=True)
            if name == "LEAVE" and grp == guest_group:
                print(f"[igmp] IGMP-LEAVE {guest_group}", flush=True)
    s.sendto(frame, dest)
    if i % 20 == 0:
        print(f"[inject] round {i}/{rounds} sent {len(frame)}B frame", flush=True)
print("[inject] done", flush=True)
PY
}

start_injector() {
    info "启动宿主注入器 ($HOST_ADDR:$HOST_PORT ↔ :$PEER_PORT → 内层组 $GUEST_GROUP:$GUEST_PORT)"
    python3 "$TMP_DIR/mcast_inject.py" \
        "$HOST_ADDR" "$HOST_PORT" "$PEER_PORT" "$GUEST_GROUP" "$GUEST_PORT" \
        "$INJECT_INTERVAL_MS" "$INJECT_ROUNDS" >"$INJECT_LOG" 2>&1 &
    INJECT_PID=$!
}

# --- 在 UDP 隧道 netdev 上运行内核镜像 (固定 MAC, 便于日志定位) ---
run_qemu() {
    info "启动 QEMU x86_64 (netdev=socket,udp=$HOST_ADDR:$HOST_PORT localaddr=$HOST_ADDR:$PEER_PORT mac=$KERNEL_MAC timeout=${TIMEOUT_QEMU}s)"
    rm -f "$LOG"
    timeout "$TIMEOUT_QEMU" qemu-system-x86_64 \
        -serial "file:${LOG}" \
        -display none \
        -no-reboot \
        -m 512 -smp 2 \
        -kernel other/build/kernel.flat \
        -netdev socket,id=n0,udp="$HOST_ADDR:$HOST_PORT",localaddr="$HOST_ADDR:$PEER_PORT" \
        -device e1000,netdev=n0,mac="$KERNEL_MAC" \
        >/dev/null 2>&1 || true
}

# --- 里程碑断言 (字面匹配: 标记含 [ ] . 等正则元字符, 用 -F) ---
check_marker() {
    local label="$1"
    local file="$2"
    local needle="$3"
    if grep -aqF "$needle" "$file"; then
        ok "$label"
        return 0
    fi
    err "$label — 未命中: '$needle' ($file)"
    return 1
}

# ============================================================================
# 主流程
# ============================================================================
ARCH="${1:-x86_64}"
if [ "$ARCH" != "x86_64" ]; then
    err "本脚本仅支持 x86_64 (aarch64 无该 netdev 联调环境)"
    exit 1
fi

for tool in qemu-system-x86_64 python3; do
    command -v "$tool" >/dev/null 2>&1 || { err "缺少依赖: $tool"; exit 1; }
done

# init 内嵌于内核镜像 (include_bytes!), 用户态源码变更须先重建.
# 走 ci/build.sh 而非直接 make: 它自带跨架构中间产物陈旧处理 (上一次构建为
# aarch64 时, 裸 `make ARCH=x86_64 user all` 会因子命令串扰而失败).
if [ "$SKIP_BUILD" != "1" ]; then
    info "构建 x86_64 用户态 + 内核 (./ci/build.sh x86_64)"
    build_log="$(mktemp)"
    if ! ./ci/build.sh x86_64 >"$build_log" 2>&1; then
        err "构建失败 (./ci/build.sh x86_64), 末 10 行:"
        tail -10 "$build_log" >&2
        rm -f "$build_log"
        exit 1
    fi
    rm -f "$build_log"
fi

if [ ! -f other/build/kernel.flat ]; then
    err "other/build/kernel.flat 缺失, 请先执行 make ARCH=x86_64 all"
    exit 1
fi

TMP_DIR="$(mktemp -d)"
write_injector
start_injector
# 注入器先就位: 内核 boot 期 DHCP 超时长, 探针 join 时组播帧必须已在链路上.
sleep 1

run_qemu

echo
info "=== 里程碑断言 (guest: $LOG / host: $INJECT_LOG) ==="
RESULT=0
check_marker "组播加入成功 (iface 组表登记)"        "$LOG" "[mcast] JOIN ok"                    || RESULT=1
check_marker "重复加入返回 EADDRINUSE (引用计数)"   "$LOG" "[mcast] DUP ok"                     || RESULT=1
check_marker "组播报文投递到 ANY:port socket"       "$LOG" "[mcast] RX ok"                      || RESULT=1
check_marker "退组成功 (引用归零 → iface 拆组)"     "$LOG" "[mcast] DROP ok"                    || RESULT=1
check_marker "非成员退组返回 EADDRNOTAVAIL"         "$LOG" "[mcast] NOENT ok"                   || RESULT=1
check_marker "IPv6 level IPPROTO_IPV6=41 命中内核分" "$LOG" "[mcast] JOIN6 ok"                  || RESULT=1
check_marker "level/optname 不成对返回 ENOPROTOOPT"  "$LOG" "[mcast] LEVEL ok"                 || RESULT=1
check_marker "IPv6 退组成功 (MLD 路径)"              "$LOG" "[mcast] DROP6 ok"                 || RESULT=1
check_marker "guest 发出 IGMPv2 Membership Report"  "$INJECT_LOG" "[igmp] IGMP-REPORT $GUEST_GROUP" || RESULT=1
check_marker "guest 发出 IGMP Leave Group"          "$INJECT_LOG" "[igmp] IGMP-LEAVE $GUEST_GROUP"  || RESULT=1

echo
if [ "$RESULT" -eq 0 ]; then
    ok "组播链路联调通过 (入组 + IGMP 出向 + 组播 RX + 退组 + errno ABI 边界 + IPv6 level ABI 全命中)"
else
    err "组播链路联调未通过"
    warn "guest 关键行 (末 25 条):"
    grep -aE "\[mcast\]|DHCP|Static IP|\[NET\]" "$LOG" | tail -25 || true
    warn "host 注入器关键行 (末 15 条):"
    tail -15 "$INJECT_LOG" || true
fi
exit "$RESULT"
