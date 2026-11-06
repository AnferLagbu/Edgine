#!/bin/bash
# ============================================================================
# lockdep 开销 A/B 复跑工具 (M7 / A-1 取证 harness, #136)
#
# 用途: 在**同一宿主窗口**内交替测量「默认构建 (D)」与「lockdep 构建 (L)」从 QEMU
#       启动到 `[SCHED] SNAP END` 的墙钟, 逐轮给出相对倍率 r_i = L_i / D_i 与其
#       中位数. 它是 A-1「lockdep 常开」与 M7 低开销化改动的**读数来源**,
#       不是门禁 —— 本脚本不接入 `ci/audit.sh`, 判据阈值尚未裁定, 故脚本本身
#       只对「测量是否可信」下结论, 不对 r 的下界下结论.
#
# 为什么是相对倍率而不是绝对毫秒差 (用户 2026-11-06 裁定 ③, 口径丙):
#   宿主墙钟抖动足以伪造几百毫秒 (单份基线的 15% 相对判据已被 §8 废弃), 而 D/L
#   在同一轮内相邻采样, 共担同一段宿主负载 —— 同窗口相对倍率才是可复现的量.
#
# 为什么逐轮先过 strings 探针:
#   「feature 到底在不在被启动的那个镜像里」决定读数是 D 还是 L. 若拷错文件、
#   或镜像被上一轮构建覆盖, 会把 D 当 L 除出一个接近 1 的 r —— 这种读数比没有
#   读数更坏. 每轮启动前对**实际启动的那个文件**重取探针段数, 不符即整轮作废.
#
# 为什么交错 (D→L→D→L…) 而非「先跑完 5 次 D 再跑 5 次 L」:
#   交错把宿主侧单调漂移 (散热/后台任务/频率调节) 摊进每一对, r_i 的离散度直接
#   暴露漂移大小; 分段跑则会把漂移整个算进倍率.
#
# 计时起点与终点:
#   起点 = fork QEMU 前的一刻 (含 QEMU 自身初始化, 与既往 M7 读数同口径);
#   终点 = 串口日志首次出现 `[SCHED] SNAP END`. 该里程碑由
#   `privileged::proc::scheduler::sched_snapshot_dump()` 打印, 是启动编排末尾
#   跨核账目快照的收束行, 双构建都有. 轮询步长 1ms; SIMPLIFIED: 用宿主挂钟差
#   作时长, 影响面 = 宿主时钟源与 NTP 校正, 何时需扩展 = 若需 sub-ms 可信度再换
#   `clock_gettime(CLOCK_MONOTONIC)` 的专用计时器.
#
# 用法:
#   ./scripts/lockdep_overhead_ab.sh                    # 5 轮 x86_64
#   LOCKDEP_AB_ROUNDS=9 ./scripts/lockdep_overhead_ab.sh
#   LOCKDEP_AB_KEEP=1 ./scripts/lockdep_overhead_ab.sh  # 保留工作目录与逐轮日志
#
# 环境变量:
#   LOCKDEP_AB_ROUNDS   交错轮数 (默认 5)
#   LOCKDEP_AB_TIMEOUT  单次启动的墙钟上限秒数 (默认 60)
#   LOCKDEP_AB_KEEP     非空 = 保留工作目录 (默认清理)
#   LOCKDEP_AB_MARKER   探针字符串 (默认 "circular dependency", 与 P-62 #125 同源)
#   LOCKDEP_AB_SMP      -smp 值 (默认 2, 与 qemu_boot_test.sh x86_64 腿一致)
#
# 退出码: 0 = 至少配成一对可比读数且镜像身份检查全过,
#         1 = 前置条件不成立 / 镜像身份不符 / 一轮都没有配成可比读数.
#       本脚本**不因 r 的大小**返回非零 —— 合格线待裁定.
#       单侧未到里程碑记 MISS 并继续 (启动挂死是 §6③/#130 已登记的预存缺口, 默认
#       构建同样复现, 与本 harness 的测量对象无关); MISS 计数随汇总打印, 不静默吞掉.
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ROUNDS="${LOCKDEP_AB_ROUNDS:-5}"
RUN_TIMEOUT="${LOCKDEP_AB_TIMEOUT:-60}"
KEEP="${LOCKDEP_AB_KEEP:-}"
MARKER="${LOCKDEP_AB_MARKER:-circular dependency}"
SMP="${LOCKDEP_AB_SMP:-2}"
ARCH="x86_64"
IMAGE_SRC="other/build/x86_64/kernel.flat"
MILESTONE='[SCHED] SNAP END'
# lockdep 构建含 1 段探针串, 默认构建 0 段 —— 与 scripts/lockdep_feature_probe.sh
# 的正/负双向判据同值, 两处必须一致否则本 harness 会把自己骗过去.
EXPECT_ON=1
EXPECT_OFF=0

ok()   { echo -e "\033[0;32m✓ $1\033[0m"; }
err()  { echo -e "\033[0;31m✗ $1\033[0m"; }
warn() { echo -e "\033[0;33m! $1\033[0m"; }
info() { echo -e "\033[0;34m-> $1\033[0m"; }

# ---------------------------------------------------------------------------
# 前置依赖: 缺任一项则测量无从成立, 直接判红而非静默出数 (fail-closed).
# ---------------------------------------------------------------------------
for dep in qemu-system-"$ARCH" strings python3 timeout; do
    command -v "$dep" >/dev/null 2>&1 || {
        err "缺少依赖: $dep — 本 harness 无法测量"
        exit 1
    }
done

if ! [[ "$ROUNDS" =~ ^[1-9][0-9]*$ ]]; then
    err "LOCKDEP_AB_ROUNDS 需为正整数, 实测 '$ROUNDS'"
    exit 1
fi

WORK="$(mktemp -d -t lockdep-ab-XXXXXX)"
cleanup() {
    if [ -n "$KEEP" ]; then
        info "工作目录保留: $WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# count_marker <file>: 该镜像里的探针段数. grep -c 在 0 命中时退出码为 1,
# 命令替换只取 stdout, 不影响判定 (与 lockdep_feature_probe.sh 同处理).
# ---------------------------------------------------------------------------
count_marker() {
    strings -a "$1" | grep -ac "$MARKER"
}

# ---------------------------------------------------------------------------
# build_image <features> <dst> <label> <expect>: 构建本架构镜像 → 先过 S-6 防线③
# (产物逐字节包含本架构用户态产物) → 再验探针段数 == <expect> → 拷入 harness 工作
# 目录 <dst>. 判据落在真被启动的那个文件上, 拷贝后才做逐轮复验.
# ---------------------------------------------------------------------------
build_image() {
    local features="$1" dst="$2" label="$3" expect="$4" log build_rc
    log="$WORK/build_${label}.log"
    info "[$label] 构建中 (KERNEL_FEATURES='${features}', 全量输出: $log)"
    if [ -n "$features" ]; then
        make ARCH="$ARCH" KERNEL_FEATURES="$features" all >"$log" 2>&1
        build_rc=$?
    else
        make ARCH="$ARCH" all >"$log" 2>&1
        build_rc=$?
    fi
    if [ "$build_rc" -ne 0 ]; then
        err "[$label] 构建失败 (日志: $log)"
        return 1
    fi
    if [ ! -f "$IMAGE_SRC" ]; then
        err "[$label] 镜像缺失: $IMAGE_SRC"
        return 1
    fi
    if ! python3 scripts/verify_image_arch.py --arch "$ARCH" --image "$IMAGE_SRC" \
        --init "other/build/$ARCH/user/init.bin" >"$WORK/verify_${label}.log" 2>&1; then
        err "[$label] 镜像架构自检未通过 — 拒绝对不可信产物计时 ($WORK/verify_${label}.log)"
        return 1
    fi
    local got
    got="$(count_marker "$IMAGE_SRC")"
    if [ "$got" != "$expect" ]; then
        err "[$label] 探针段数不符 — 期望 $expect, 实测 $got ($IMAGE_SRC)"
        return 1
    fi
    cp "$IMAGE_SRC" "$dst" || { err "[$label] 镜像拷贝失败"; return 1; }
    ok "[$label] 就绪: $dst (探针段 $got/$expect)"
}

# ---------------------------------------------------------------------------
# run_once <image> <log>: 启动一趟, 返回墙钟毫秒 (stdout); 未见里程碑则返回非零.
# 启动前对该文件重取探针段数 (由调用方核对), 本函数只管计时与里程碑.
# 读数终点取在「串口日志首见里程碑」那一瞬, 之后立刻终止 QEMU —— 过完里程碑 guest
# 继续跑用户态、不会自行停机, 若不终止则每轮都要耗满预算, 读数会被计时器外的停机
# 成本污染. 预算按**墙钟截止**算而非按轮询次数算: `sleep 0.001` 实睡时长由宿主
# 调度决定 (实测 2–4ms), 用次数换算会把 60s 预算睡成 180s.
# ---------------------------------------------------------------------------
run_once() {
    local image="$1" log="$2" pid start now deadline
    rm -f "$log"
    start=$(date +%s%N)
    deadline=$((start + RUN_TIMEOUT * 1000000000))
    timeout "$RUN_TIMEOUT" qemu-system-"$ARCH" \
        -serial "file:$log" -display none -no-reboot \
        -m 512 -smp "$SMP" -kernel "$image" >/dev/null 2>&1 &
    pid=$!
    now=0
    while [ "$now" -eq 0 ] && [ "$(date +%s%N)" -lt "$deadline" ]; do
        if [ -s "$log" ] && grep -aqF "$MILESTONE" "$log"; then
            now=$(date +%s%N)
        else
            sleep 0.001
        fi
    done
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if [ "$now" -eq 0 ]; then
        return 1
    fi
    echo $(( (now - start) / 1000000 ))
}

echo "================================================================"
info "lockdep 开销 A/B harness: ${ROUNDS} 轮交错 (x86_64, -smp ${SMP}, 里程碑 '${MILESTONE}')"
info "口径 = 同窗口相对倍率 r_i = L_i / D_i; 合格线未裁, 本脚本只保证测量可信"
echo "================================================================"

DEFAULT_IMG="$WORK/kernel-default.flat"
LOCKDEP_IMG="$WORK/kernel-lockdep.flat"

build_image "" "$DEFAULT_IMG" default "$EXPECT_OFF" || exit 1
build_image lockdep "$LOCKDEP_IMG" lockdep "$EXPECT_ON" || exit 1

# 复跑期用到的是工作目录里的两份拷贝, 出货路径随之回到默认构建 —— 显式复原,
# 免得下一位读者 (或下一趟门禁) 在 kernel.flat 上看到 lockdep 镜像.
info "复原出货路径为默认构建 ($IMAGE_SRC)"
if ! make ARCH="$ARCH" all >"$WORK/restore_default.log" 2>&1; then
    err "默认构建复原失败 (日志: $WORK/restore_default.log)"
    exit 1
fi
if [ "$(count_marker "$IMAGE_SRC")" != "$EXPECT_OFF" ]; then
    err "复原后默认镜像仍含探针串 — 出货路径不可信"
    exit 1
fi
ok "出货路径已复原为默认构建"

D_VALUES=()
L_VALUES=()
R_VALUES=()
PAIRS=0
MISS_DEFAULT=0
MISS_LOCKDEP=0

for ((round = 1; round <= ROUNDS; round++)); do
    info "----- 第 $round/$ROUNDS 轮 -----"
    # 逐轮复验: 两份拷贝各自必须是期望段数, 否则本轮 (及后续) 读数无意义.
    d_on="$(count_marker "$DEFAULT_IMG")"
    l_on="$(count_marker "$LOCKDEP_IMG")"
    if [ "$d_on" != "$EXPECT_OFF" ] || [ "$l_on" != "$EXPECT_ON" ]; then
        err "第 $round 轮镜像身份不符 — default=$d_on(期望 $EXPECT_OFF) lockdep=$l_on(期望 $EXPECT_ON)"
        exit 1
    fi

    # 单侧未到里程碑**不作废整趟**: 启动挂死是已登记的预存缺口 (§6③/#130, 默认构建
    # 也复现), 与本 harness 的测量对象无关. 处理方式是把该侧记为 MISS 并继续, 只在
    # 两侧同轮都 HIT 时才配成一条 r —— MISS 计数随汇总一起打印, 不静默吞掉.
    d_ms=""
    if d_ms="$(run_once "$DEFAULT_IMG" "$WORK/run_default_${round}.log")"; then
        :
    else
        d_ms=""
        MISS_DEFAULT=$((MISS_DEFAULT + 1))
        warn "第 $round 轮 default 未在 ${RUN_TIMEOUT}s 内出现里程碑 (MISS)"
    fi
    l_ms=""
    if l_ms="$(run_once "$LOCKDEP_IMG" "$WORK/run_lockdep_${round}.log")"; then
        :
    else
        l_ms=""
        MISS_LOCKDEP=$((MISS_LOCKDEP + 1))
        warn "第 $round 轮 lockdep 未在 ${RUN_TIMEOUT}s 内出现里程碑 (MISS)"
    fi

    if [ -z "$d_ms" ] || [ -z "$l_ms" ]; then
        printf '  轮 %-2d  default=%s  lockdep=%s  r=不可配对\n' \
            "$round" "${d_ms:-MISS}" "${l_ms:-MISS}"
        continue
    fi

    # r_i 保留 4 位小数: 倍率的判决位在个位与十分位, 更多位只是宿主噪声.
    r_i=$(awk -v l="$l_ms" -v d="$d_ms" 'BEGIN { printf "%.4f", l / d }')
    D_VALUES+=("$d_ms")
    L_VALUES+=("$l_ms")
    R_VALUES+=("$r_i")
    PAIRS=$((PAIRS + 1))
    printf '  轮 %-2d  default=%sms  lockdep=%sms  r=%s\n' "$round" "$d_ms" "$l_ms" "$r_i"
done

# ---------------------------------------------------------------------------
# median <values...>: 升序取中位 (偶数个取中间两者均值, 奇数个取正中).
# ---------------------------------------------------------------------------
median() {
    local -a sorted
    mapfile -t sorted < <(printf '%s\n' "$@" | sort -n)
    local n=${#sorted[@]} mid
    if [ $((n % 2)) -eq 1 ]; then
        mid=$((n / 2))
        echo "${sorted[$mid]}"
    else
        mid=$((n / 2))
        awk -v a="${sorted[$((mid - 1))]}" -v b="$sorted[$mid]" 'BEGIN { printf "%.4f", (a + b) / 2 }'
    fi
}

echo "================================================================"
info "汇总: ${ROUNDS} 轮计划 / ${PAIRS} 对可比读数 / MISS default=${MISS_DEFAULT} lockdep=${MISS_LOCKDEP}"
if [ "$PAIRS" -eq 0 ]; then
    err "没有任何一对可比读数 — 本趟不产出 r, 请先看逐轮日志定位挂死侧"
    exit 1
fi
printf '  %-10s %-14s %-14s\n' "配置" "中位(ms)" "逐轮(ms, 仅配对轮)"
printf '  %-10s %-14s %-14s\n' "default" "$(median "${D_VALUES[@]}")" "${D_VALUES[*]}"
printf '  %-10s %-14s %-14s\n' "lockdep" "$(median "${L_VALUES[@]}")" "${L_VALUES[*]}"
printf '  %-10s %-14s %-14s\n' "r=L/D" "$(median "${R_VALUES[@]}")" "${R_VALUES[*]}"
if [ "$MISS_DEFAULT" -ne 0 ] || [ "$MISS_LOCKDEP" -ne 0 ]; then
    warn "存在 MISS 轮: 启动挂死读数已计入台账输入 (§6③ / #130), 不影响配对读数的口径"
fi
echo "================================================================"
ok "测量可信: ${PAIRS}/${ROUNDS} 轮配对完成, 镜像身份与里程碑检查全过 (合格线待裁定)"
exit 0
