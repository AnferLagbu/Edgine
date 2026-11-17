#!/bin/bash
# ============================================================================
# aarch64 EGFS 磁盘挂载真跑腿 (D5 专项门禁, 解耦批「EGFS 接入 aarch64」)
#
# 用途: 默认构建 + 真启动 + 真 virtio-blk 盘, 断言 aarch64 的「virtio-blk legacy
#       初始化 → EGFS 发现盘 → 空盘自格式化 → 挂到 /disk → 引导根下进 EL0」整链真在跑.
#       D5 的三处修复 (blk legacy 页大小/队列序、挂载调用处 NUL 终止、去 x86-only
#       门控) 都只在**带盘真启动**下暴露: 无盘路径恒走 memory mode, 既有启动腿
#       (qemu_boot_test / initramfs / cow) 一律不带盘, 故它们全绿证不了磁盘挂载成立.
#
#       拓扑 (D5-e 裁定, Linux 式 switch_root): 引导根恒为 ramfs/initramfs, EGFS
#       只挂非根挂载点 /disk, 由用户态 init 切根进磁盘根. 故本腿判的是「磁盘被发现
#       并挂上 /disk」, 不是「EGFS 抢占 /」; EL0 里程碑判的是引导根下用户态仍在跑.
#
#       三相判据 (缺一即红):
#         相 1 空盘首启: 设备注册 1 块 + `formatting first drive` + `FORMAT: Complete`
#              + `EGFS mounted at /disk (disk)` + `disk root installed:` +
#              `/disk/init read back` + `Entering EL0` + `[SWROOT] PASS: pivot to /disk`;
#              `virtio-blk: 请求超时` / `EGFS mount failed` / `disk root install
#              incomplete` / `[SWROOT] FAIL` / `[SWROOT] SKIP` **硬零** —— 依次是 D5-PR
#              (legacy 初始化序)、挂载 NUL 终止 bug (曾使 x86 磁盘挂载亦从未成功, 只因
#              无盘未触发)、D5-g 写路径静默丢数据 (解压计数非零但字节读不回)、D5-h
#              用户态切根失败、以及"带盘却走了无盘分支"的回归锁.
#         相 2 同盘二启: /disk 挂载仍成立 + `/disk/init present (skip disk-root
#              install)` 在 + `[SWROOT] PASS` 在, 且 `formatting first drive` /
#              `disk root installed:` / `[SWROOT] FAIL` / `[SWROOT] SKIP` **硬零** ——
#              证明 uberblock 与已安装的磁盘根都真落盘并被读到, 而不是每次启动都重新
#              格式化/重装 (只判相 1 时, 「写盘丢失」与「写盘成功」读数完全一样; 重装
#              还会覆盖用户改动), 且二启的 put_old 复用不使切根退化.
#         相 3 无盘负控制: `EGFS: running in memory mode (no disk)` 在 +
#              `[SWROOT] SKIP: no disk root` 在, 且 `EGFS mounted at /disk` /
#              `disk root installed:` / `disk root install incomplete` /
#              `[SWROOT] PASS` / `[SWROOT] FAIL` **硬零** —— 证明磁盘分支、磁盘根安装
#              与用户态切根都不是被无条件接管, 去门控没有把无盘路径也拖进磁盘分支.
#
# 与既有门禁的关系: 本腿只新增判定, 不改 scripts/qemu_boot_test.sh 的任何既有判据.
# 盘镜像每次由本腿新建空白 raw 盘 (内核自格式化, 无需 host mkfs 工具 —— D0 定的方案).
#
# 用法:
#   ./scripts/aarch64_egfs_disk_boot_test.sh [aarch64|all]   # 默认 aarch64
#   # x86_64 明确拒绝 (非"跳过"): x86 走 PCI AHCI/NVMe, 本腿判据对象是 virtio-mmio
#   # 块设备 + aarch64 /disk 挂载, 在 x86_64 上跑会得到零判据的空绿.
# 环境变量:
#   AARCH64_EGFS_DISK_TIMEOUT  等待 `Entering EL0` 里程碑的上限秒数 (默认 120, 含格式化)
#   AARCH64_EGFS_DISK_SMP      核数 (默认 2, 与既有 aarch64 启动门禁同口径)
#   AARCH64_EGFS_DISK_MB       空白盘容量 MiB (默认 128, 与 D5 实测同口径)
#
# 退出码: 0 = 三相判据全符合, 1 = 任一判据红
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-aarch64}"
TIMEOUT="${AARCH64_EGFS_DISK_TIMEOUT:-120}"
SMP="${AARCH64_EGFS_DISK_SMP:-2}"
DISK_MB="${AARCH64_EGFS_DISK_MB:-128}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

ok()   { echo -e "\033[0;32m✓ $1\033[0m"; }
err()  { echo -e "\033[0;31m✗ $1\033[0m"; }
info() { echo -e "\033[0;34m-> $1\033[0m"; }

case "$ARCH" in
    aarch64|all) ;;
    x86_64)
        err "本腿判据对象是 aarch64 /disk 挂载 (virtio-mmio 块设备); x86_64 走 PCI AHCI/NVMe, 不属本腿射程"
        exit 1
        ;;
    *) err "用法: $0 [aarch64|all]"; exit 1 ;;
esac

IMAGE="other/build/aarch64/kernel-aarch64.img"
DISK="other/build/aarch64/egfs_disk.img"
DISK_ARGS=(-drive "file=$DISK,if=none,id=vd0,format=raw" -device virtio-blk-device,drive=vd0)

# D4: 统一 boot-with-module 载体 (cpio 经引导器交付, 不嵌入内核).
source scripts/boot_module.sh

# ── 构建 (默认核; cpio 经 boot-module 交付, 不嵌入内核) ──────────────────────
info "[aarch64] boot-module 构建 (出货默认核)..."
BUILD_LOG="$LOG_DIR/aarch64_egfs_disk_build.log"
if ! boot_module_build aarch64 >"$BUILD_LOG" 2>&1; then
    err "[aarch64] 构建失败 (日志: $BUILD_LOG)"
    exit 1
fi
# S-6 防线: 启动前先确认产物 ELF 头是本架构 (零嵌入后镜像不含用户态字节, 判据在产物侧)
if ! python3 scripts/verify_image_arch.py --arch aarch64 \
    --image "$IMAGE" \
    --init "other/build/aarch64/user/init.bin"; then
    err "[aarch64] 镜像架构自检未通过 — 拒绝在不可信镜像上跑门禁"
    exit 1
fi
command -v qemu-system-aarch64 >/dev/null 2>&1 || {
    err "未找到 qemu-system-aarch64"
    exit 1
}

# ── 启动 helper ─────────────────────────────────────────────────────────────
# 日志含 NUL (内核日志与探针逐字符并发写串口), 所有 grep 必须带 -a.
MILESTONE='Entering EL0'
# D5-h: 切根终态在 EL0 **之后**才由 init 打印, 故每相各自等到自己的终态标记;
# 只等 EL0 会在切根发生前就收机, 判据恒绿 (测不到等于没测).
SWROOT_PASS='[SWROOT] PASS: pivot to /disk'
SWROOT_SKIP='[SWROOT] SKIP: no disk root'
SWROOT_FAIL='[SWROOT] FAIL'
_dead() { ! kill -0 "$1" 2>/dev/null; }

# boot_once <log> <终态标记> [qemu 额外参数...]
boot_once() {
    local log="$1" until="$2"; shift 2
    : > "$log"
    qemu-system-aarch64 \
        -M virt,gic-version=3 -cpu max -m 512 -smp "$SMP" \
        $(boot_module_carrier_args aarch64) "$@" \
        -serial "file:$log" -display none -no-reboot >/dev/null 2>&1 &
    local qpid=$!
    local _i
    for _i in $(seq 1 $((TIMEOUT * 10))); do
        # 终态到 / 明确失败签名出现 / 进程自己退出 (停机形态) 即提前收尾.
        if grep -qaF "$until" "$log" \
            || grep -qaF "$SWROOT_FAIL" "$log" \
            || grep -qaF "EGFS mount failed" "$log" \
            || grep -qaF "virtio-blk: 请求超时" "$log" \
            || _dead "$qpid"; then
            break
        fi
        sleep 0.1
    done
    kill "$qpid" 2>/dev/null || true
    wait "$qpid" 2>/dev/null || true
}

FAILED=0

# 计数取数: 结果必须是非负十进制读数. grep 真错 (日志不可读等) 时输出为空/非数字,
# 此时**不得**读成 "0 条" —— 硬零判据会把"测不出"当成"通过".
count_of() {
    local log="$1" pattern="$2" n
    n=$(grep -caF "$pattern" "$log" 2>/dev/null || true)
    case "$n" in
        '' | *[!0-9]*) return 1 ;;
        *) printf '%s\n' "$n" ;;
    esac
}

need() {
    local log="$1" needle="$2" desc="$3"
    if grep -qaF "$needle" "$log"; then
        ok "[aarch64] $desc: $(grep -aF "$needle" "$log" | tail -1)"
    else
        err "[aarch64] $desc —— 未出现 '$needle' (日志: $log)"
        FAILED=$((FAILED + 1))
    fi
}

forbid() {
    local log="$1" needle="$2" desc="$3" n
    if ! n=$(count_of "$log" "$needle"); then
        err "[aarch64] $desc 取数失败 (日志: $log) —— 测不出不等于硬零"
        FAILED=$((FAILED + 1))
    elif [ "$n" -eq 0 ]; then
        ok "[aarch64] $desc 硬零 ('$needle')"
    else
        err "[aarch64] $desc —— 出现 $n 条 '$needle' (日志: $log)"
        FAILED=$((FAILED + 1))
    fi
}

# ── 相 1: 空白盘首启 (自格式化 → 挂 /disk → 引导根下 EL0) ────────────────────
info "[aarch64] 相 1: 新建 ${DISK_MB}MiB 空白盘, 首启走自格式化..."
mkdir -p "$(dirname "$DISK")"
if ! dd if=/dev/zero of="$DISK" bs=1M count="$DISK_MB" status=none; then
    err "[aarch64] 空白盘创建失败: $DISK"
    exit 1
fi
P1_LOG="$LOG_DIR/aarch64_egfs_disk_first_boot.log"
boot_once "$P1_LOG" "$SWROOT_PASS" "${DISK_ARGS[@]}"

need "$P1_LOG" "virtio-blk: 1 device(s) registered" "virtio-blk 发现并注册 1 块盘"
need "$P1_LOG" "formatting first drive" "空盘被判无有效 uberblock (真走格式化分支)"
need "$P1_LOG" "[EGFS] FORMAT: Complete" "EGFS 自格式化完成"
need "$P1_LOG" "EGFS mounted at /disk (disk)" "EGFS 磁盘挂载到 /disk 成功"
need "$P1_LOG" "disk root installed:" "载体全量安装到磁盘根 (D5-g)"
need "$P1_LOG" "/disk/init read back" "安装后回读 /disk/init 成功 (写路径真落盘)"
need "$P1_LOG" "$MILESTONE" "引导根 (ramfs) 下进 EL0 跑用户态"
need "$P1_LOG" "$SWROOT_PASS" "用户态切根到磁盘根并从新根读回 /init (D5-h)"
forbid "$P1_LOG" "virtio-blk: 请求超时" "virtio-blk I/O 超时 (D5-PR legacy 初始化序回归锁)"
forbid "$P1_LOG" "EGFS mount failed" "挂载失败 (NUL 终止 bug 回归锁)"
forbid "$P1_LOG" "disk root install incomplete" "解压计数非零但 /disk/init 读不回 (静默丢数据)"
forbid "$P1_LOG" "$SWROOT_FAIL" "用户态切根失败 (mkdir/pivot/读回任一环节)"
forbid "$P1_LOG" "$SWROOT_SKIP" "带盘却判无磁盘根 (载体自检退化成恒 SKIP)"

# ── 相 2: 同盘二启 (读既有 uberblock, 不得重新格式化) ───────────────────────
info "[aarch64] 相 2: 同盘二启, 判 uberblock 真落盘..."
P2_LOG="$LOG_DIR/aarch64_egfs_disk_second_boot.log"
boot_once "$P2_LOG" "$SWROOT_PASS" "${DISK_ARGS[@]}"

need "$P2_LOG" "EGFS mounted at /disk (disk)" "二启 /disk 挂载仍成立"
need "$P2_LOG" "/disk/init present (skip disk-root install)" "二启盘上 /init 仍在 (跳过重装)"
need "$P2_LOG" "$MILESTONE" "二启引导根下仍进 EL0"
need "$P2_LOG" "$SWROOT_PASS" "二启用户态仍切根成功 (put_old 已存在不退化)"
forbid "$P2_LOG" "formatting first drive" "二启重新格式化 (uberblock 未持久化)"
forbid "$P2_LOG" "disk root installed:" "二启重装磁盘根 (会覆盖用户改动, 幂等失效)"
forbid "$P2_LOG" "virtio-blk: 请求超时" "二启 virtio-blk I/O 超时"
forbid "$P2_LOG" "$SWROOT_FAIL" "二启用户态切根失败"
forbid "$P2_LOG" "$SWROOT_SKIP" "二启带盘却判无磁盘根"

# ── 相 3: 无盘负控制 (memory mode, 盘根分支不得无条件接管) ──────────────────
info "[aarch64] 相 3: 摘盘负控制, 判无盘路径无回归..."
P3_LOG="$LOG_DIR/aarch64_egfs_disk_nodisk.log"
boot_once "$P3_LOG" "$SWROOT_SKIP"

need "$P3_LOG" "EGFS: running in memory mode (no disk)" "无盘走 memory mode"
need "$P3_LOG" "$MILESTONE" "无盘路径仍进 EL0 (去门控无回归)"
need "$P3_LOG" "$SWROOT_SKIP" "无盘时用户态切根自检判 SKIP (不冒充切根成功)"
forbid "$P3_LOG" "EGFS mounted at /disk" "无盘却宣称 /disk 挂载 (分支被无条件接管)"
forbid "$P3_LOG" "disk root installed:" "无盘却宣称装了磁盘根 (安装被无条件调用)"
forbid "$P3_LOG" "disk root install incomplete" "无盘却报磁盘根安装不完整"
forbid "$P3_LOG" "$SWROOT_PASS" "无盘却宣称切根成功 (SKIP 判据恒真化)"
forbid "$P3_LOG" "$SWROOT_FAIL" "无盘路径切根报错 (应走 SKIP 而非 FAIL)"

echo "================================================================"
if [ "$FAILED" -ne 0 ]; then
    err "aarch64 EGFS 磁盘挂载真跑腿失败: $FAILED 项 (D5)"
    exit 1
fi
ok "aarch64 EGFS 磁盘挂载真跑腿三相判据全符合 (D5)"
exit 0