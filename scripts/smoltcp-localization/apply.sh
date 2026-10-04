#!/bin/bash
# 应用 smoltcp 本地化 patch 到 vendored 副本
# 用法: scripts/smoltcp-localization/apply.sh
#
# 前提:
#   - 已在 src/kernel/functions/net/smoltcp/ 部署上游源 (无本地化)
#   - 13 个 patch 文件已生成 (相对路径, 在同目录)
# 退出码: 0 = 全部成功, 1 = 有失败

set -uo pipefail
VENDORED="src/kernel/functions/net/smoltcp"
PATCH_DIR="$(cd "$(dirname "$0")" && pwd)"

# 12 个本地化文件 (路径 → patch)
declare -A PATCHES=(
    ["src/iface/interface/ipv4.rs"]="src_iface_interface_ipv4.rs.patch"
    ["src/iface/interface/ipv6.rs"]="src_iface_interface_ipv6.rs.patch"
    ["src/iface/interface/mod.rs"]="src_iface_interface_mod.rs.patch"
    ["src/iface/packet.rs"]="src_iface_packet.rs.patch"
    ["src/phy/sys/bpf.rs"]="src_phy_sys_bpf.rs.patch"
    ["src/phy/sys/mod.rs"]="src_phy_sys_mod.rs.patch"
    ["src/phy/sys/raw_socket.rs"]="src_phy_sys_raw_socket.rs.patch"
    ["src/phy/sys/tuntap_interface.rs"]="src_phy_sys_tuntap_interface.rs.patch"
    ["src/socket/dhcpv4.rs"]="src_socket_dhcpv4.rs.patch"
    ["src/socket/dns.rs"]="src_socket_dns.rs.patch"
    ["src/socket/tcp/congestion/cubic.rs"]="src_socket_tcp_congestion_cubic.rs.patch"
    ["src/wire/ipv6.rs"]="src_wire_ipv6.rs.patch"
    ["src/wire/udp.rs"]="src_wire_udp.rs.patch"
)

cd /home/anfer/Code/Edgine/$VENDORED
total=${#PATCHES[@]}
ok=0
fail=0

for f in "${!PATCHES[@]}"; do
    patch_file="$PATCH_DIR/${PATCHES[$f]}"
    if [ ! -f "$f" ]; then
        echo "[SKIP] 目标文件不存在: $f"
        fail=$((fail + 1))
        continue
    fi
    if patch -p1 --dry-run < "$patch_file" > /dev/null 2>&1; then
        patch -p1 < "$patch_file" > /dev/null
        echo "[OK]   $f"
        ok=$((ok + 1))
    else
        echo "[FAIL] $f (patch 不匹配, 可能是上游源码已变化)"
        fail=$((fail + 1))
    fi
done

echo
echo "=========================================="
echo "应用结果: $ok 成功 / $fail 失败 / $total 总计"
echo "=========================================="
exit $fail
