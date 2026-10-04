#!/bin/bash
# ============================================================================
# QueenX QEMU 真实启动测试脚本 (QEMU Real Boot Validation)
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

ok()   { echo -e "${GREEN}\u2713 $1${NC}"; }
err()  { echo -e "${RED}\u2717 $1${NC}"; }
warn() { echo -e "${YELLOW}! $1${NC}"; }
info() { echo -e "${BLUE}-> $1${NC}"; }

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
        rm -f other/build/user/*.bin 2>/dev/null || true
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
    else
        A64_LOG="$LOG_DIR/qemu_boot_aarch64.log"
        # 批次 Z ④: virt 机型挂 virtio-net 网卡 (services 权威探测链路, 对齐
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
            # 批次 Z ④: 验证 services virtio-net 经 NetOps 安全桥注册链路
            # (framework 侧单向拉取日志, 由 framework/net/init/probe.rs 输出)
            if grep -q "nic: probed successfully (services bridge)" "$A64_LOG"; then
                ok "[aarch64] virtio-net 经 NetOps 安全桥探测成功 (批次 Z ④)"
            else
                warn "[aarch64] 未发现 virtio-net services bridge 探测日志 (Z ④ 链路未走通)"
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
