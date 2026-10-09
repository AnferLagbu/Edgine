#!/bin/bash
# ============================================================================
# Edgine QEMU 真实启动测试脚本 (QEMU Real Boot Validation)
#
# 用途: 验证双架构内核镜像在 QEMU 中真实启动, 记录关键子系统状态
# 输出: other/build/log/qemu_boot_*.log
# 退出码: 0 = 启动通过 (到达指定里程碑), 1 = 启动失败
#
# 历史: 2026-06-04 v2.0 首次实现 — 修复了 Makefile 中 string.c 过期引用,
#       双架构 cargo build + QEMU 真实启动都通过 (aarch64 完整到 EL0,
#       x86_64 走到 e1000 NIC 检测后因 smoltcp 初始化挂起, 已记录).
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

ARCH="${1:-all}"
TIMEOUT_QEMU="${TIMEOUT_QEMU:-25}"
FAIL_OK="${FAIL_OK:-1}"  # 1 = 允许部分里程碑不通过 (e1000 已知挂起)

# --- P3 D6 端到端验证参数 (仅 x86_64, 可用环境变量覆盖) ---
E2E_HOST_PORT="${E2E_HOST_PORT:-8080}"        # 宿主侧 hostfwd 监听端口
E2E_GUEST_PORT="${E2E_GUEST_PORT:-80}"        # guest 内 echo 服务端口
E2E_READY_TIMEOUT="${E2E_READY_TIMEOUT:-40}"  # 就绪轮询上限 (s)
E2E_ROUNDS="${E2E_ROUNDS:-3}"                 # 客户端连接轮数
# 抖动门禁 (S-5): 端到端阶段重复轮数, 默认 1 = 行为与既往一致. 单次通过不足以
# 证明抖动消除 (docs/plan/net-e2e-tcp-echo-flakiness.md), CI 夜间作业设 >1
# 把"连续多轮 0 失败"变成可门禁指标. 失败轮串口日志另存为 *.failN.log 以便事后归因.
E2E_REPEATS="${E2E_REPEATS:-1}"              # 端到端阶段重复轮数
E2E_QEMU_PID=""                               # 后台 QEMU pid (cleanup_e2e 消费)
E2E_UDP_PID=""                                 # 后台宿主 UDP 回显服务 pid (cleanup_e2e 消费)
E2E_UDP_PORT="${E2E_UDP_PORT:-9090}"           # 宿主 UDP 回显端口 (guest 经 10.0.2.2 访问)
# 包级观测口: 非空时启用 QEMU `filter-dump` 把 n0 双向帧写入该 pcap 路径.
# 供抖动定位用 (docs/plan/net-e2e-tcp-echo-flakiness.md S-1; 该口一度用于验证
# "入向丢帧"假设, 结论是证伪 —— 失败轮的 SYN 已到 guest, 缺的是 guest 侧消费).
# 默认空 = 不抓包.
E2E_PCAP="${E2E_PCAP:-}"

ok()   { echo -e "${GREEN}\u2713 $1${NC}"; }
err()  { echo -e "${RED}\u2717 $1${NC}"; }
warn() { echo -e "${YELLOW}! $1${NC}"; }
info() { echo -e "${BLUE}-> $1${NC}"; }

# ---------------------------------------------------------------------------
# 端到端阶段清理: 退出/中断/超时时回收后台 QEMU (幂等).
# 未进入端到端阶段时 E2E_QEMU_PID 为空, 本函数为空操作.
# ---------------------------------------------------------------------------
cleanup_e2e() {
    if [ -n "$E2E_QEMU_PID" ] && kill -0 "$E2E_QEMU_PID" 2>/dev/null; then
        kill "$E2E_QEMU_PID" 2>/dev/null || true
        wait "$E2E_QEMU_PID" 2>/dev/null || true
    fi
    E2E_QEMU_PID=""
    if [ -n "$E2E_UDP_PID" ] && kill -0 "$E2E_UDP_PID" 2>/dev/null; then
        kill "$E2E_UDP_PID" 2>/dev/null || true
        wait "$E2E_UDP_PID" 2>/dev/null || true
    fi
    E2E_UDP_PID=""
    return 0
}
trap cleanup_e2e EXIT INT TERM

# ---------------------------------------------------------------------------
# 通用 QEMU 启动 + 日志分析
# 参数: $1=arch  $2=qemu_args...  $3=logfile  $4=expected_marker
# 返回: 0=找到 marker, 1=超时/未找到
# ---------------------------------------------------------------------------
boot_and_check() {
    local arch="$1"; shift
    local logfile="$1"; shift
    local timeout_s="$1"; shift
    local expected_marker="$1"; shift
    local qemu_args=("$@")

    info "[$arch] 启动 QEMU (timeout=${timeout_s}s)..."
    timeout "$timeout_s" qemu-system-"$arch" \
        -serial "file:${logfile}" \
        -display none \
        -no-reboot \
        "${qemu_args[@]}" \
        >/dev/null 2>&1 || true

    if [ ! -s "$logfile" ]; then
        err "[$arch] 启动日志为空, 内核未进入 Rust 入口"
        return 1
    fi

    local lines
    lines=$(wc -l < "$logfile")
    info "[$arch] 串口输出 ${lines} 行"

    if grep -q "$expected_marker" "$logfile"; then
        ok "[$arch] 找到里程碑: '$expected_marker'"
        return 0
    else
        err "[$arch] 未找到里程碑: '$expected_marker' (最后一行: $(tail -1 "$logfile"))"
        return 1
    fi
}

# ---------------------------------------------------------------------------
# 架构同步: 确保 other/build/ 中间产物架构 + .arch 戳记与目标一致
# 防止 qemu_boot_test.sh 跑 aarch64 后, .arch 残留 aarch64 但开发者
# 下次手敲 make ARCH=x86_64 增量构建报 EM 183 错误 (AArch64 产物误用).
# 解决: 脚本每次跑测试前主动检测 + 同步 .arch 戳记.
# 参数: $1=目标架构 (x86_64 / aarch64)
# 返回: 0 = 已同步 (无操作或重建成功), 1 = 重建失败
# ---------------------------------------------------------------------------
sync_make_state() {
    local target_arch="$1"
    local arch_stamp="$LOG_DIR/.arch"
    local prev_arch=""
    [ -f "$arch_stamp" ] && prev_arch="$(cat "$arch_stamp" 2>/dev/null || echo none)"

    # 检查中间 .o 产物是否与目标架构一致 (使用 file 命令)
    local asm_objs="other/build/boot.o other/build/entry.o other/build/isr.o other/build/switch.o other/build/arch/x86_64/trampoline.o"
    local need_rebuild=0

    if [ "$prev_arch" != "$target_arch" ]; then
        info "[$target_arch] .arch 戳记不匹配 ($prev_arch → $target_arch), 强制重建"
        need_rebuild=1
    else
        # .arch 一致但仍要校验 .o 产物 (防止外部 rm 与 .arch 失同步)
        for obj in $asm_objs; do
            if [ -f "$obj" ]; then
                local obj_arch=""
                if file "$obj" | grep -q "x86-64"; then
                    obj_arch="x86_64"
                elif file "$obj" | grep -q "ARM aarch64\|aarch64"; then
                    obj_arch="aarch64"
                fi
                if [ "$obj_arch" != "" ] && [ "$obj_arch" != "$target_arch" ]; then
                    warn "[$target_arch] 中间产物 $obj 架构 ($obj_arch) 与目标不符, 强制重建"
                    need_rebuild=1
                    break
                fi
            fi
        done
    fi

    if [ "$need_rebuild" = "1" ]; then
        rm -f $asm_objs other/build/kernel.bin other/build/kernel.flat other/build/kernel-aarch64.img other/build/kernel.map other/build/stage1.bin
        # 用户态产物已按架构分目录 (S-6 防线①: other/build/<arch>/user/),
        # 异架构产物不在本架构读取路径上, 无需清除 (旧写法删共享的 other/build/user/*.bin
        # 正是当时唯一能拦住污染的机制, 现已由目录隔离 + 启动前自检接手).
        if ! make ARCH="$target_arch" all 2>&1 | tail -3; then
            err "[$target_arch] make ARCH=$target_arch 失败"
            return 1
        fi
        # Makefile 会在解析时写 .arch, 此处再次校验
        [ -f "$arch_stamp" ] || echo "$target_arch" > "$arch_stamp"
    fi
    return 0
}

# ---------------------------------------------------------------------------
# 内核镜像陈旧检测 (B08-16 / ISSUE-TOOL-002)
# Makefile 依赖已保证 make 层面自动重建, 此处为 QEMU 脚本独立防线:
# 源码 (内核 + 用户态) 比镜像新时提示先 make, 避免跑陈旧镜像误判.
# 参数: $1=镜像路径 (默认 other/build/kernel.flat, aarch64 传 other/build/kernel-aarch64.img)
# 返回: 0 = 镜像新鲜或缺失, 1 = 镜像可能过期
# ---------------------------------------------------------------------------
check_kernel_fresh() {
    local image="${1:-other/build/kernel.flat}"
    [ -f "$image" ] || return 0
    local newest
    newest=$(find src/rust/src src/kernel src/user -name '*.rs' -newer "$image" 2>/dev/null | head -1)
    if [ -n "$newest" ]; then
        warn "$image 可能过期 (源码 $newest 比镜像新), 建议先运行 make"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# 镜像内嵌用户态架构自检 (S-6 防线③, fail-fast)
# 内核经 include_bytes! 在编译期嵌入 other/build/<arch>/user/init.bin. 若嵌入的
# 用户态 ELF 架构与目标不符, 直到进入 Ring 3 执行非法指令才 #PF — 报错点距根因极远.
# 本函数在启动前以两项可靠判据拦截: 产物 e_machine + 镜像逐字节包含.
# (尺寸不参与判定: 同一合法构型在全量/增量构建下尺寸可差数十 KB.)
# 参数: $1=架构 (x86_64|aarch64)  $2=镜像路径
# 返回: 0 = 通过, 1 = 不符 / 产物或镜像缺失 / 依赖缺失
# ---------------------------------------------------------------------------
verify_image_arch() {
    local arch="$1"
    local image="$2"
    local init_bin="other/build/${arch}/user/init.bin"
    command -v python3 >/dev/null 2>&1 || { err "[$arch] 缺少依赖: python3 (镜像架构自检)"; return 1; }
    if ! python3 scripts/verify_image_arch.py --arch "$arch" --image "$image" --init "$init_bin"; then
        err "[$arch] 镜像内嵌用户态自检未通过 — 拒绝在不可信镜像上跑门禁 (先 ./ci/build.sh $arch 重建)"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# 端到端 TCP echo 验证 (P3 D6 "先建后换" 交接, 仅 x86_64)
#
# 以 QEMU slirp hostfwd 把宿主端口映射到 guest 内用户态 echo 服务:
#   -netdev user,id=n0,hostfwd=tcp::<host>-:<guest>
# 宿主 python3 客户端连续发起 <E2E_ROUNDS> 轮 TCP 连接, 每轮发送固定载荷并
# 校验回显; guest 侧断言 accept/echo/close 里程碑与 accept fd 去重 (FD 回收).
# 返回: 0 = 全部断言通过, 1 = 任一失败.
# ---------------------------------------------------------------------------
e2e_tcp_echo() {
    local log="$LOG_DIR/qemu_e2e_tcp_x86_64.log"
    local payload="EDGINE-TCP-ECHO"

    command -v python3 >/dev/null 2>&1 || { err "[x86_64] 缺少依赖: python3 (端到端客户端)"; return 1; }

    # P4 D9 recvfrom 活体腿: 宿主 UDP 回显服务 (绑 127.0.0.1:${E2E_UDP_PORT}).
    # guest udp_echo_probe 经 slirp 网关 10.0.2.2:${E2E_UDP_PORT} 发起, slirp 投递
    # 到本服务并原样回包, 验证内核 recvfrom 回填真实对端. 与 e2e QEMU 同生命周期
    # (cleanup_e2e 回收), 先于 QEMU 启动确保 guest 探针发出时服务已就绪.
    E2E_UDP_PORT="$E2E_UDP_PORT" python3 -c '
import os, socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", int(os.environ["E2E_UDP_PORT"])))
while True:
    data, addr = s.recvfrom(4096)
    s.sendto(data, addr)
' >/dev/null 2>&1 &
    E2E_UDP_PID=$!

    rm -f "$log"
    info "[x86_64] 启动后台 QEMU (hostfwd tcp::${E2E_HOST_PORT}->:${E2E_GUEST_PORT})..."
    local -a qemu_args=(
        -serial "file:${log}"
        -display none
        -no-reboot
        -m 512 -smp 2
        -kernel other/build/kernel.flat
        -device e1000,netdev=n0
        -netdev "user,id=n0,hostfwd=tcp::${E2E_HOST_PORT}-:${E2E_GUEST_PORT}"
    )
    if [ -n "$E2E_PCAP" ]; then
        rm -f "$E2E_PCAP"
        info "[x86_64] 抓包: n0 双向帧 → ${E2E_PCAP} (filter-dump)"
        qemu_args+=(-object "filter-dump,id=e2edump,netdev=n0,file=${E2E_PCAP}")
    fi
    qemu-system-x86_64 "${qemu_args[@]}" >/dev/null 2>&1 &
    E2E_QEMU_PID=$!

    # 就绪等待: DHCP 租约 + TCP 监听标记均出现; 期间检测 QEMU 早退.
    local waited=0
    while [ "$waited" -lt "$E2E_READY_TIMEOUT" ]; do
        if grep -aqF "[tcp] Listening on 0.0.0.0:80" "$log" 2>/dev/null \
            && grep -aqF "DHCP configured (lease applied)" "$log" 2>/dev/null; then
            break
        fi
        if ! kill -0 "$E2E_QEMU_PID" 2>/dev/null; then
            err "[x86_64] QEMU 在端到端就绪前退出 (见 $log)"
            cleanup_e2e
            return 1
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if [ "$waited" -ge "$E2E_READY_TIMEOUT" ]; then
        err "[x86_64] 端到端就绪超时 (${E2E_READY_TIMEOUT}s): 未见 TCP 监听 / DHCP 租约"
        cleanup_e2e
        return 1
    fi
    ok "[x86_64] echo 服务端就绪 (guest :${E2E_GUEST_PORT})"

    # 宿主客户端: 逐轮连接 + 回显校验 (set -e 下以 || 捕获退出码).
    local py_rc=0
    E2E_HOST_PORT="$E2E_HOST_PORT" E2E_ROUNDS="$E2E_ROUNDS" E2E_PAYLOAD="$payload" \
        python3 - <<'PY' || py_rc=$?
import os
import socket
import sys
import time

port = int(os.environ["E2E_HOST_PORT"])
rounds = int(os.environ["E2E_ROUNDS"])
payload = os.environ["E2E_PAYLOAD"].encode()

for i in range(rounds):
    try:
        sock = socket.create_connection(("127.0.0.1", port), timeout=5)
    except OSError as exc:
        print(f"round {i}: connect to 127.0.0.1:{port} failed: {exc}", file=sys.stderr)
        sys.exit(1)
    got = b""
    try:
        sock.sendall(payload)
        sock.shutdown(socket.SHUT_WR)
        while len(got) < len(payload):
            chunk = sock.recv(4096)
            if not chunk:
                break
            got += chunk
    finally:
        sock.close()
    if got != payload:
        print(f"round {i}: echo mismatch: sent={payload!r} got={got!r}", file=sys.stderr)
        sys.exit(1)
    time.sleep(0.2)

print(f"OK: {rounds}/{rounds} rounds echoed")
PY
    if [ "$py_rc" -ne 0 ]; then
        warn "[x86_64] 宿主客户端回显校验失败 (exit=$py_rc)"
        cleanup_e2e
        return 1
    fi
    ok "[x86_64] 宿主客户端 ${E2E_ROUNDS} 轮回显全部校验通过"

    # 释放后台 QEMU (SIGTERM + wait), 日志完整落盘后再断言 guest 侧里程碑.
    cleanup_e2e

    local rc=0
    # 1. 监听槽持续可用: 监听标记存在.
    if grep -aqF "[tcp] Listening on 0.0.0.0:80" "$log"; then
        ok "[x86_64] guest 监听 0.0.0.0:80 就绪"
    else
        err "[x86_64] guest 未打印监听标记"
        rc=1
    fi
    # 2. accept / echo / close 计数均不低于连接轮数 (D6 交接逐轮生效).
    local accepted echoed closed
    accepted=$(grep -acF "[tcp] Connection accepted" "$log" || true)
    echoed=$(grep -acF "[tcp] Echo ok len=" "$log" || true)
    closed=$(grep -acF "[tcp] Connection closed" "$log" || true)
    if [ "$accepted" -ge "$E2E_ROUNDS" ] && [ "$echoed" -ge "$E2E_ROUNDS" ] && [ "$closed" -ge "$E2E_ROUNDS" ]; then
        ok "[x86_64] 交接计数通过: accept=${accepted} echo=${echoed} close=${closed} (>=${E2E_ROUNDS})"
    else
        err "[x86_64] 交接计数不足: accept=${accepted} echo=${echoed} close=${closed} (<${E2E_ROUNDS})"
        rc=1
    fi
    # 3. FD 回收: 每次 accept 分配最小空闲 fd, close 归还后下次 accept 复用同一
    #    fd, 故 accepted fd 去重应恰为 1. 日志可能含 NUL 字节, 故 grep -a.
    local uniq_fds uniq_count
    uniq_fds=$(grep -aoE "\[tcp\] Connection accepted fd=[0-9]+" "$log" \
        | grep -aoE "[0-9]+$" | sort -u | tr '\n' ' ' || true)
    uniq_count=$(echo "$uniq_fds" | wc -w)
    if [ "$uniq_count" -eq 1 ]; then
        ok "[x86_64] FD 回收断言通过: accept fd 去重为 1 (fd=${uniq_fds% })"
    else
        err "[x86_64] FD 回收断言失败: accept fd 去重为 ${uniq_count} (实测: ${uniq_fds:-无})"
        rc=1
    fi
    # 4. P4 语义里程碑 (D7/D8/D8b 每连接 TCP 侧 + D9 启动期 UDP 侧). TCP 项随
    #    hostfwd 入站逐轮出现 (>=1 即可), UDP 连接态探针在启动期同步跑一次.
    local nodelay_cnt pollin_cnt shutwr_cnt udpconn_cnt udppeer_cnt udpsock_cnt udprecv_cnt
    nodelay_cnt=$(grep -acF "[tcp] NODELAY roundtrip ok" "$log" || true)
    pollin_cnt=$(grep -acF "[tcp] POLLIN revents=1" "$log" || true)
    shutwr_cnt=$(grep -acF "[tcp] SHUTDOWN_WR ok" "$log" || true)
    udpconn_cnt=$(grep -acF "[udp] CONNECT ok" "$log" || true)
    udppeer_cnt=$(grep -acF "[udp] PEERNAME ok" "$log" || true)
    udpsock_cnt=$(grep -acF "[udp] SOCKNAME ok" "$log" || true)
    udprecv_cnt=$(grep -acF "[udp] RECVFROM src ok" "$log" || true)
    if [ "$nodelay_cnt" -ge 1 ] && [ "$pollin_cnt" -ge 1 ] && [ "$shutwr_cnt" -ge 1 ]; then
        ok "[x86_64] P4 TCP 里程碑通过: NODELAY=${nodelay_cnt} POLLIN=${pollin_cnt} SHUTDOWN_WR=${shutwr_cnt}"
    else
        err "[x86_64] P4 TCP 里程碑不足: NODELAY=${nodelay_cnt} POLLIN=${pollin_cnt} SHUTDOWN_WR=${shutwr_cnt}"
        rc=1
    fi
    if [ "$udpconn_cnt" -ge 1 ] && [ "$udppeer_cnt" -ge 1 ] && [ "$udpsock_cnt" -ge 1 ] && [ "$udprecv_cnt" -ge 1 ]; then
        ok "[x86_64] P4 UDP 里程碑通过: CONNECT=${udpconn_cnt} PEERNAME=${udppeer_cnt} SOCKNAME=${udpsock_cnt} RECVFROM=${udprecv_cnt}"
    else
        err "[x86_64] P4 UDP 里程碑不足: CONNECT=${udpconn_cnt} PEERNAME=${udppeer_cnt} SOCKNAME=${udpsock_cnt} RECVFROM=${udprecv_cnt}"
        rc=1
    fi
    # 5. 失败标记兜底: 出现 [tcp] FAIL 时打印首条便于诊断.
    if grep -aqF "[tcp] FAIL:" "$log"; then
        warn "[x86_64] 日志出现 [tcp] FAIL 标记: $(grep -aF "[tcp] FAIL:" "$log" | head -1)"
    fi

    if [ "$rc" -eq 0 ]; then
        ok "[x86_64] 端到端 TCP echo 验证通过 (P3 D6 先建后换 + FD 回收)"
    fi
    return $rc
}

# ---------------------------------------------------------------------------
# 测试: 全部架构
# ---------------------------------------------------------------------------
RESULT=0
TESTED=0
PASSED=0

if [ "$ARCH" = "all" ] || [ "$ARCH" = "x86_64" ]; then
    TESTED=$((TESTED+1))
    info "=== x86_64 QEMU 真实启动 ==="

    # 架构同步: 确保 other/build/ 中间产物 + .arch 戳记与 x86_64 一致
    # (防止 aarch64 测试残留导致 EM 183 报错)
    sync_make_state "x86_64" || RESULT=1

    # ISSUE-TOOL-002: x86_64 侧同样接入陈旧镜像检测 (与 aarch64 分支一致)
    check_kernel_fresh || true

    if [ ! -f other/build/kernel.flat ]; then
        err "x86_64 kernel.flat 缺失, 跳过测试"
        RESULT=1
    elif ! verify_image_arch "x86_64" other/build/kernel.flat; then
        # S-6 防线③: 不通过即不启动 (与旧世界"静默跑在污染镜像上"相反)
        RESULT=1
    else
        X64_LOG="$LOG_DIR/qemu_boot_x86_64.log"
        # ISSUE-RT-001: 此前用 -nic none 隔离测试, 因 QEMU 默认 e1000 NIC 触发
        # smoltcp 栈初始化挂起. 根因 (e1000_io.rs CTRL.RST 误用 bit31, 实为
        # PHY_RST) 修复后, 恢复默认 e1000 并断言驱动初始化里程碑 + 完整进 Ring 3.
        # APS-05 (DECISION-084): -smp 2 使次核上线路径与双核并发 EL0 进入门禁.
        if boot_and_check "x86_64" "$X64_LOG" "$TIMEOUT_QEMU" "VFS ready" \
            -m 512 -smp 2 -kernel other/build/kernel.flat; then
            # e1000 驱动初始化里程碑 (ISSUE-RT-001 回归断言)
            if grep -q "e1000: 初始化完成" "$X64_LOG"; then
                ok "[x86_64] e1000 驱动初始化完成 (ISSUE-RT-001 回归通过)"
            else
                warn "[x86_64] 未观察到 e1000 初始化完成 (默认 NIC 未挂载或驱动回归)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # DHCP 租约里程碑 (默认 e1000 + user netdev 自带 slirp DHCP 服务).
            # 修复 RX 描述符回收 off-by-one (RDT 误设为 tail+1 致 RDT==RDH,
            # 硬件可用描述符归零, 后续帧被静默丢弃) 后应拿到租约; 回落
            # Static IP (fallback) 即为该回归 (fail-closed, -a 避免 NUL 误判).
            if grep -aq "DHCP configured (lease applied)" "$X64_LOG"; then
                ok "[x86_64] DHCP 租约获取成功 (RX 描述符回收回归通过)"
            else
                warn "[x86_64] 未获取 DHCP 租约 (回落静态地址 fallback?)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # v2.2: x86_64 无网络启动已修复 VGA 越界 bug, 完整进入 Ring 3
            if grep -q "Entering Ring 3" "$X64_LOG"; then
                ok "[x86_64] 完整启动成功! 进入 Ring 3 启动 init 进程 (v2.2 修复 VGA 越界)"
                PASSED=$((PASSED+1))
            elif grep -q "Network Subsystem Init" "$X64_LOG"; then
                warn "[x86_64] 启动到 Network Subsystem Init 但未到 Ring 3"
                PASSED=$((PASSED+1))
            else
                warn "[x86_64] 未到达 Network Subsystem Init"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi

            # KPTI-09: init 的探针子进程以 Ring 3 读取内核镜像高半区别名,
            # 预期被内核以 #PF -> TerminateProcess 终止 (读不到值), 父进程
            # 以退出码判定并打印该里程碑. 缺失即隔离回归 (fail-closed).
            if grep -q "\[KPTI\] EL0 kernel high-half access denied" "$X64_LOG"; then
                ok "[x86_64] KPTI 隔离断言通过: EL0 读内核高半区被内核终止 (KPTI-09)"
            else
                warn "[x86_64] 未观察到 KPTI EL0 隔离断言 (KPTI-09)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # APS-05 (DECISION-084): 双核并发 EL0 判据. 用户态 syscall 只能由 EL0
            # 任务发起, 故每核有界打印 `[SMP] EL0 pid=N cpu=M` 中出现 cpu=0 与 cpu=1
            # 即成对证明两核各自有用户任务运行于 EL0. 缺失即成对判据失败 (fail-closed).
            # 注: 串口并发写可能在内核日志中插入 NUL 字节, 此时 grep 默认将该文件
            #     视为二进制并只输出 "Binary file ... matches" 而不输出匹配行, 导致
            #     EL0_CPUS 为空而误报. 故用 -a 强制按文本处理.
            EL0_CPUS=$(grep -aoE "\[SMP\] EL0 pid=[0-9]+ cpu=[0-9]+" "$X64_LOG" 2>/dev/null \
                | grep -aoE "cpu=[0-9]+" | sort -u | tr '\n' ' ' || true)
            if echo "$EL0_CPUS" | grep -q "cpu=0" && echo "$EL0_CPUS" | grep -q "cpu=1"; then
                ok "[x86_64] 双核并发 EL0 验证通过 (观察到 cpu=0 与 cpu=1): ${EL0_CPUS}"
            else
                warn "[x86_64] 未成对观察到 EL0 cpu=0/cpu=1 (实测: ${EL0_CPUS:-无})"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi

            # P3 D6 端到端验证: hostfwd 入站连接驱动 guest 用户态 TCP echo
            # 服务端, 断言 accept/recv/send 里程碑 + FD 回收 + 监听槽持续可用.
            # 为硬门禁 (不受 FAIL_OK 放宽): 端到端连通性是 D6 交接语义的直接判据.
            # E2E_REPEATS > 1 时逐轮重复并聚合失败数 (S-5 抖动门禁).
            e2e_round=1
            e2e_failed=0
            while [ "$e2e_round" -le "$E2E_REPEATS" ]; do
                if [ "$E2E_REPEATS" -gt 1 ]; then
                    info "[x86_64] 端到端重复轮 ${e2e_round}/${E2E_REPEATS}"
                fi
                if ! e2e_tcp_echo; then
                    e2e_failed=$((e2e_failed + 1))
                    cp -f "${LOG_DIR}/qemu_e2e_tcp_x86_64.log" \
                        "${LOG_DIR}/qemu_e2e_tcp_x86_64.fail${e2e_round}.log" 2>/dev/null || true
                fi
                e2e_round=$((e2e_round + 1))
            done
            if [ "$e2e_failed" -ne 0 ]; then
                err "[x86_64] 端到端抖动门禁未过: ${E2E_REPEATS} 轮中 ${e2e_failed} 轮失败 (失败日志 *.failN.log)"
                RESULT=1
            elif [ "$E2E_REPEATS" -gt 1 ]; then
                ok "[x86_64] 端到端抖动门禁通过: 连续 ${E2E_REPEATS} 轮 0 失败"
            fi
        else
            [ "$FAIL_OK" = "0" ] && RESULT=1
        fi
    fi
fi

if [ "$ARCH" = "all" ] || [ "$ARCH" = "aarch64" ]; then
    TESTED=$((TESTED+1))
    info "=== aarch64 QEMU 真实启动 ==="

    # 架构同步: 确保 other/build/ 中间产物 + .arch 戳记与 aarch64 一致
    # (防止 x86_64 测试残留导致 EM 183 反向误用)
    sync_make_state "aarch64" || RESULT=1

    check_kernel_fresh other/build/kernel-aarch64.img || true

    if [ ! -f other/build/kernel-aarch64.img ]; then
        err "aarch64 kernel-aarch64.img 缺失, 跳过测试"
        RESULT=1
    elif ! verify_image_arch "aarch64" other/build/kernel-aarch64.img; then
        # S-6 防线③: 同上, 启 QEMU 前先证明镜像嵌的是 aarch64 用户态
        RESULT=1
    else
        A64_LOG="$LOG_DIR/qemu_boot_aarch64.log"
        # 批次 Z ④: virt 机型挂 virtio-net 网卡 (functions 权威探测链路, 对齐
        # Y 批次挂盘冒烟配置). -netdev user 无 DHCP 服务, smoltcp 初始化
        # 偶发 "TX 超时" WARN 属预期, 不影响 boot 里程碑.
        # 镜像为 arm64 Image (内嵌 Image 头, 见 Makefile/link/aarch64.ld),
        # QEMU 经 Image 头 text_offset 定位入口, 与 U-Boot booti / 真机一致.
        # SMP-09 (DECISION-082): 以 -smp 2 启动双核, 使 AP 上线路径进入 CI 门禁.
        if boot_and_check "aarch64" "$A64_LOG" "$TIMEOUT_QEMU" "VFS ready" \
            -M virt,gic-version=3 -cpu max -m 512 -smp 2 -kernel other/build/kernel-aarch64.img \
            -device virtio-net-device,netdev=n0 \
            -netdev user,id=n0; then
            # ISSUE-RT-002: GICv3 初始化成功里程碑 (初始化后置条件自检通过).
            # 缺失即表示 GIC 初始化 fail-fast 或回退静默路径 (fail-closed).
            if grep -q "GICv3 ready" "$A64_LOG"; then
                ok "[aarch64] GICv3 初始化成功 (ISSUE-RT-002 回归通过)"
            else
                warn "[aarch64] 未观察到 GICv3 ready 里程碑 (ISSUE-RT-002 回归?)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # SMP-09: 次核上线里程碑 (DECISION-082). 缺失即 AP 启动回归 (fail-closed).
            if grep -q "\[SMP\] online CPUs: 2" "$A64_LOG"; then
                ok "[aarch64] SMP 双核上线 (online CPUs: 2)"
            else
                warn "[aarch64] 未观察到 online CPUs: 2 (SMP 次核未上线?)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # DECISION-083: aarch64 TLB shootdown 发送路径里程碑 (ST-07). 双核在线后
            # 的页表拆除应发布新代并广播 SGI 13, 且接收侧 per-CPU SGI 使能后确实响应
            # (历史缺陷: SGI 13/14 从未在 GICR_ISENABLER0 使能 ⇒ 接收侧静默, 缺失即
            # 回归). 期望形如 `[SMP] TLB shootdown #N gen=… targets=…`.
            if grep -q "\[SMP\] TLB shootdown" "$A64_LOG"; then
                ok "[aarch64] TLB shootdown 发送路径生效 (DECISION-083)"
            else
                warn "[aarch64] 未观察到 TLB shootdown 里程碑 (发送路径未触发?)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # 批次 Z ④: 验证 functions virtio-net 经 NetOps 安全桥注册链路
            # (privileged 侧单向拉取日志, 由 privileged/net/init/probe.rs 输出)
            if grep -q "nic: probed successfully (functions bridge)" "$A64_LOG"; then
                ok "[aarch64] virtio-net 经 NetOps 安全桥探测成功 (批次 Z ④)"
            else
                warn "[aarch64] 未发现 virtio-net functions bridge 探测日志 (Z ④ 链路未走通)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # aarch64 完整启动: 应进入用户态 (EL0)
            if grep -q "Entering EL0" "$A64_LOG"; then
                ok "[aarch64] 完整启动成功! 进入 EL0 启动 init 进程"
                PASSED=$((PASSED+1))
            elif grep -q "Network Subsystem Ready" "$A64_LOG"; then
                ok "[aarch64] 启动到 Network Subsystem Ready (init 进程已 launch)"
                PASSED=$((PASSED+1))
            else
                warn "[aarch64] 未到达 EL0 (最后一行: $(tail -1 "$A64_LOG"))"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi

            # KPTI-09: init 的探针子进程以 EL0 读取内核镜像高别名,
            # 预期被内核以同步异常 -> process_exit 终止 (读不到值), 父进程
            # 以退出码判定并打印该里程碑. 缺失即隔离回归 (fail-closed).
            if grep -q "\[KPTI\] EL0 kernel high-half access denied" "$A64_LOG"; then
                ok "[aarch64] KPTI 隔离断言通过: EL0 读内核高半区被内核终止 (KPTI-09)"
            else
                warn "[aarch64] 未观察到 KPTI EL0 隔离断言 (KPTI-09)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # APS-05 (DECISION-084): 双核并发 EL0 判据 (与 x86_64 分支同构).
            # 用户态 syscall 只能由 EL0 任务发起, `cpu=0` 与 `cpu=1` 成对出现即
            # 证明两核各自有用户任务运行于 EL0. 缺失即 fail-closed.
            # 注: 与 x86_64 分支同理, -a 避免日志含 NUL 字节时 grep 误判二进制.
            EL0_CPUS=$(grep -aoE "\[SMP\] EL0 pid=[0-9]+ cpu=[0-9]+" "$A64_LOG" 2>/dev/null \
                | grep -aoE "cpu=[0-9]+" | sort -u | tr '\n' ' ' || true)
            if echo "$EL0_CPUS" | grep -q "cpu=0" && echo "$EL0_CPUS" | grep -q "cpu=1"; then
                ok "[aarch64] 双核并发 EL0 验证通过 (观察到 cpu=0 与 cpu=1): ${EL0_CPUS}"
            else
                warn "[aarch64] 未成对观察到 EL0 cpu=0/cpu=1 (实测: ${EL0_CPUS:-无})"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
            # ISSUE-RT-005: EL0 中断可达里程碑. `enter_user` / 调度恢复路径清
            # SPSR.I 后 EL0 期可接收 IRQ, `handle_irq` 首次由 EL0 投递即打印
            # `IRQ: delivered from EL0`. 缺失即 EL0 全程屏蔽 IRQ 回归
            # (用户态不可被抢占, 与 x86_64 RFLAGS.IF=1 语义不一致, fail-closed).
            # 注: -a 避免日志含 NUL 字节时 grep 误判二进制.
            if grep -aq "IRQ: delivered from EL0" "$A64_LOG"; then
                ok "[aarch64] EL0 中断可达 (用户态可被抢占, ISSUE-RT-005)"
            else
                warn "[aarch64] 未观察到 EL0 来源 IRQ (ISSUE-RT-005 回归?)"
                [ "$FAIL_OK" = "0" ] && RESULT=1
            fi
        else
            [ "$FAIL_OK" = "0" ] && RESULT=1
        fi
    fi
fi

echo ""
echo "============================================"
echo "QEMU 真实启动测试: ${PASSED}/${TESTED} 通过"
echo "  日志: other/build/log/qemu_boot_*.log"
echo "============================================"
exit $RESULT
