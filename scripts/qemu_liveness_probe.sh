#!/bin/bash
# ============================================================================
# 整机 liveness 量化探针 (lockdep-observer-liveness-metric.md D-1)
#
# 用途: 把 P-30 一次性 soak 里的判冻逻辑固化为**可复用、可入 CI** 的 liveness
#   度量. 在生产 (非 lockdep) 镜像上, 用持续高压 TCP 负载 + 采样窗, 输出机器
#   可判的 ALIVE/FROZEN 与量化 tick 速率, 取代稀有 boot 的人工 gdb `bt` 判读.
#
# 真冻结判据 (三条**同时**成立才判 FROZEN, 规避 KPTI tick 读数伪影):
#   (1) tick (仅在内核态线程上读, 且值 < TICK_SANITY_MAX 才采信; 多次重试)
#       在采样窗内不前进 (delta <= 0);
#   (2) 该 tick 值两次采样完全相同 (排除读数抖动);
#   (3) bt 里没有任一核处于 [halted]/idle (排除"全核在用户态忙"的假冻).
#   任一不满足 → ALIVE. 另交叉记录 tcp echo 成功数作为业务面 liveness 旁证.
#
# KPTI 读数伪影: TICK_ADDR 是内核 .bss 虚拟地址, gdb `x/1xg` 走被附加 CPU 的
#   当前 CR3; 该 CPU 在用户态时读回的是用户指针垃圾 (>= TICK_SANITY_MAX), 而非
#   真 tick. 故在**同一次** gdb 会话内遍历各 vCPU: 只对 "#0 rip ∈ 内核文本窗" 的
#   核读 tick, 且只采信小整数 (旧形态 bt 与 x 分两次会话, 中间 vCPU 可切到用户
#   CR3, 读回垃圾后只剩重试这一条路). 全部核都不可读时判 UNREADABLE, 不判冻.
#
# 用法:
#   scripts/qemu_liveness_probe.sh                    # 单 boot, 自动解析 TICK_ADDR
#   BOOTS=8 SMP=4 HAMMER_S=12 scripts/qemu_liveness_probe.sh
#
# 环境变量 (全部可选):
#   ARCH=x86_64  IMG=<kernel.flat>  MAP=<kernel.map>
#   TICK_ADDR=0x..   不填则从 MAP 解析 timer::tick::TICK_IRQ_COUNT (crate 名随
#                    workspace 拆分有两种形态: kernel_privileged / kernel::privileged)
#   SMP=4  BOOTS=1  HAMMER_S=12  GDB_BASE=1434  HOST_PORT=8080
#   KTEXT_LO=0x100000  KTEXT_HI=0x300000  TICK_SANITY_MAX=0x100000
#
# 输出: 每 boot 一行 `LIVENESS ...`; 末尾一行 `LIVENESS_SUMMARY ...`.
# 退出码: 0=全部 ALIVE; 1=出现 FROZEN; 2=环境/镜像错误, 或存在 UNREADABLE
#   (tick 测不出 ≠ 冻结: 该 boot 不参与判冻, 退出码按「测不出」分档).
# ============================================================================
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

ARCH=${ARCH:-x86_64}
IMG=${IMG:-other/build/${ARCH}/kernel.flat}
MAP=${MAP:-other/build/${ARCH}/kernel.map}
SMP=${SMP:-4}
BOOTS=${BOOTS:-1}
HAMMER_S=${HAMMER_S:-12}
HOST_PORT=${HOST_PORT:-8080}
GDB_BASE=${GDB_BASE:-1434}
KTEXT_LO=${KTEXT_LO:-0x100000}
KTEXT_HI=${KTEXT_HI:-0x300000}
TICK_SANITY_MAX=${TICK_SANITY_MAX:-0x100000}
OUT=${OUT:-other/build/log/liveness_probe}

[ -f "$IMG" ] || { echo "ERROR: 镜像不存在 $IMG (先 ./ci/build.sh $ARCH)"; exit 2; }
command -v qemu-system-"$ARCH" >/dev/null 2>&1 || { echo "ERROR: 缺 qemu-system-$ARCH"; exit 2; }
command -v gdb >/dev/null 2>&1 || { echo "ERROR: 缺 gdb"; exit 2; }
mkdir -p "$OUT"

# 从链接图解析 TICK 计数符地址 (免手工传参, 跨 build 稳健).
resolve_tick_addr() {
    [ -n "${TICK_ADDR:-}" ] && { echo "$TICK_ADDR"; return; }
    [ -f "$MAP" ] || return 1
    # 符号名两种形态: P-71 workspace 拆分后 crate = kernel-privileged
    # (mangled 可读名 kernel_privileged::...); 拆分前是 kernel::privileged::...
    grep -aE '0x[0-9a-f]+[[:space:]]+(kernel_privileged|kernel::privileged)::timer::tick::TICK_IRQ_COUNT' "$MAP" \
        | grep -oE '0x[0-9a-f]+' | head -1
}
TICK_ADDR=$(resolve_tick_addr || true)
[ -n "$TICK_ADDR" ] || { echo "ERROR: 无法解析 TICK_ADDR (MAP=$MAP); 手工传 TICK_ADDR=0x.."; exit 2; }
echo "=== liveness probe: IMG=$IMG TICK_ADDR=$TICK_ADDR SMP=$SMP BOOTS=$BOOTS HAMMER_S=${HAMMER_S}s ==="

GP=""; HP=""   # 供 read_tick 内部引用当前 boot 的 gdb/host 端口
# 读 tick: 单次 gdb 会话内遍历 vCPU, 选 #0 rip ∈ [KTEXT_LO,KTEXT_HI) 且
# 读出值 < TICK_SANITY_MAX 的核; 全部核不可读时返回空串 (调用侧判 UNREADABLE).
read_tick() {
    local script="$OUT/.read_tick.gdb" tries=0 v
    while [ "$tries" -lt 5 ]; do
        cat > "$script" <<GDB
target remote :${GP}
python
import gdb
lo, hi, cap = ${KTEXT_LO}, ${KTEXT_HI}, ${TICK_SANITY_MAX}
for t in gdb.selected_inferior().threads():
    try:
        t.switch()
        if lo <= int(gdb.parse_and_eval('\$pc')) < hi:
            val = int(gdb.parse_and_eval('*(unsigned long*)${TICK_ADDR}'))
            if val < cap:
                print('TICKVAL=0x%x' % val)
                break
    except gdb.error:
        pass
end
detach
quit
GDB
        v=$(gdb -batch -q -x "$script" 2>/dev/null | grep -oE 'TICKVAL=0x[0-9a-f]+' | tail -1)
        [ -n "$v" ] && { rm -f "$script"; echo "${v#TICKVAL=}"; return; }
        tries=$((tries+1)); sleep 0.3
    done
    rm -f "$script"
    echo ""
}

frozen=0; alive=0; unreadable=0
for b in $(seq 1 "$BOOTS"); do
    LOG="$OUT/boot${b}.log"; BT="$OUT/boot${b}.bt"; rm -f "$LOG" "$BT"
    HP=$((HOST_PORT + b)); GP=$((GDB_BASE + b))
    qemu-system-x86_64 -m 512 -smp "$SMP" -kernel "$IMG" -gdb "tcp::${GP}" \
        -device e1000,netdev=n0 -netdev "user,id=n0,hostfwd=tcp::${HP}-:80" \
        -serial "file:${LOG}" -display none -no-reboot >/dev/null 2>&1 &
    QPID=$!
    w=0; ready=0
    while [ "$w" -lt 30 ]; do
        grep -aqF "[tcp] Listening" "$LOG" 2>/dev/null && { ready=1; break; }
        kill -0 "$QPID" 2>/dev/null || break; sleep 1; w=$((w+1))
    done

    # 持续高压 TCP hammer 后台跑 HAMMER_S 秒 (业务面负载)
    HP="$HP" DUR="$HAMMER_S" python3 - <<'PY' >/dev/null 2>&1 &
import os,socket,time
p=int(os.environ["HP"]); dur=float(os.environ["DUR"]); t0=time.time(); n=0
while time.time()-t0 < dur:
    try:
        s=socket.create_connection(("127.0.0.1",p),timeout=1); s.settimeout(1)
        s.sendall(b"LIVENESS"); s.shutdown(socket.SHUT_WR)
        while s.recv(4096): pass
        s.close(); n+=1
    except OSError: pass
    time.sleep(0.005)
PY
    HAM=$!

    a=$(read_tick); sleep "$HAMMER_S"; c=$(read_tick)
    gdb -batch -q -ex "target remote :${GP}" -ex "thread apply all bt 8" -ex "detach" -ex "quit" 2>/dev/null > "$BT" || true
    wait "$HAM" 2>/dev/null
    kill "$QPID" 2>/dev/null; wait "$QPID" 2>/dev/null

    idle=0; grep -aqE "\[halted" "$BT" 2>/dev/null && idle=1
    echo_ok=$(grep -acF "[tcp] Echo ok" "$LOG" 2>/dev/null); echo_ok=${echo_ok:-0}
    if [ -z "$a" ] || [ -z "$c" ]; then
        # 测不出 ≠ 冻结: 缺任一读数即不满足判冻前提, 本 boot 不参与判冻.
        st=UNREADABLE; unreadable=$((unreadable+1))
        printf 'LIVENESS boot=%d state=%s ready=%d tick=NA->NA delta=0 rate_per_s=0 halted_core=%d echo_ok=%d\n' \
            "$b" "$st" "$ready" "$idle" "$echo_ok"
    else
        ad=$((16#${a#0x})); cd_=$((16#${c#0x})); delta=$((cd_ - ad))
        rate=$(( delta / (HAMMER_S>0?HAMMER_S:1) ))
        # FROZEN 需三条件同时: delta<=0 且 两次 tick 相同 且 无 halted 核
        if [ "$delta" -le 0 ] && [ "$a" = "$c" ] && [ "$idle" -eq 0 ]; then
            st=FROZEN; frozen=$((frozen+1))
        else
            st=ALIVE; alive=$((alive+1))
        fi
        printf 'LIVENESS boot=%d state=%s ready=%d tick=0x%x->0x%x delta=%d rate_per_s=%d halted_core=%d echo_ok=%d\n' \
            "$b" "$st" "$ready" "$ad" "$cd_" "$delta" "$rate" "$idle" "$echo_ok"
        [ "$st" = FROZEN ] && { echo "  !!! FROZEN bt 摘录:"; grep -aE "Thread|#[0-9]" "$BT" | head -12 | sed 's/^/    /'; }
    fi
done

total=$((alive + frozen + unreadable)); rc=0
[ "$frozen" -gt 0 ] && rc=1
[ "$rc" -eq 0 ] && [ "$unreadable" -gt 0 ] && rc=2
printf 'LIVENESS_SUMMARY boots=%d alive=%d frozen=%d unreadable=%d exit=%d out=%s\n' \
    "$total" "$alive" "$frozen" "$unreadable" "$rc" "$OUT"
exit "$rc"
