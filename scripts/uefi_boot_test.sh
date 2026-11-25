#!/bin/bash
# ============================================================================
# UEFI (OVMF) 引导真跑腿 (D4-i 丙 专项门禁)
#
# 用途: 默认构建 + GRUB2 multiboot2 ISO + **OVMF 固件**真启动, 断言 UEFI 引导路径
#       与 BIOS 路径同样走通. 既有启动腿全部经 SeaBIOS, 而 D4-g/D4-i 修的三处缺陷
#       (MB2 信息块指针硬编码 0 / tag4 覆盖 tag6 / inline RSDP 被当指针读) **只在
#       UEFI 下暴露** —— BIOS 腿全绿也证不了它们, 故单列本腿.
#
#       判据 (缺一即红):
#         1. 真启动到用户态 + 载体链: `Entering Ring 3` / `init from /init` /
#            `[INITRAMFS] Unpacked N` 且 N >= 7;
#         2. ACPI 经 MB2 拿到 (D4-i 甲): `RSDP found via Multiboot2` +
#            `Table scan complete: MADT=true FADT=true HPET=true` + FADT/HPET 读数行;
#            **反向**: 不得出现 `RSDP found via BIOS scan` (UEFI 下走 BIOS 扫描回退
#            即 MB2 通路断链);
#         3. 双核真上线: `MADT … AP count=$SMP` + `[SMP] online CPUs: $SMP` +
#            `[SMP] TLB probe PASS` (修复前 UEFI 恒 `AP count=0`, 单核);
#         4. firmware 保留区真进 PMM (D4-i 乙): `Boot firmware reserved regions: N`
#            的 N >= MIN_FW_REGIONS(10), 且 `[PMM] Early reserve 'firmware':` 行数
#            **恰等于** N (声明与登记两侧对账, 只判一侧时"声明了但没登记"照样绿), 外加
#            `Early reserve 'mb2-info':` (ACPI 发现面在 PMM 之后才读它, 该区必须活到运行期);
#            下界 10 兼作 **UEFI/BIOS 判别器**: SeaBIOS e820 实测 7 区, OVMF 实测
#            13-14 区 ⇒ pflash 未生效而退化成 BIOS 启动时本腿即红, 不产生假绿;
#         5. 内存图权威 (D4-g): `Boot info: mem=M MB` 的 M >= 400 —— OVMF 的 tag4
#            基本内存信息只报 ~8 MB, 修复前它覆盖 tag6 的 511 MB, 故该下界能把回归判红;
#         6. 引导模块早期搬移 (D4-h): `Early relocate 'initrd':` 在，或显式
#            `Early reserve 'initrd' clear of boot window:`（initrd 已不与引导窗相交，
#            搬移按语义是 no-op；两条都缺席才红）;
#         7. 用户态探针可达面: `[TLS] PASS:` / `[SIG] PASS:` / `[UACCESS] PASS:`
#            (clone TID 指向只读 .text 必须 EFAULT, 不进入裸 store) / `[THREAD] PASS:`
#            (U-1 根因修复后线程组探针恢复) / `[EXEC] PASS: cwd+root`
#            (无 `[EXEC] FAIL`) / `[WASM] PASS:` (无 `[WASM] FAIL`) / `[SCHED] SNAP END`
#            + `conserve=ok` (无 `conserve=BAD`) / `[KPTI] EL0 … denied`;
#         8. 驱动与网络: `e1000: 初始化完成` / `DHCP configured (lease applied)`;
#         9. P-62 默认能力行与 BIOS 腿同口径 (本腿不切 feature).
#
#       **U-1 已修复**: `[THREAD]` 腿曾是显式不判项 (UEFI 下确定性挂死), 根因已定位为
#       clone 的 TID 回写命中 4K COW 只读页、x86 核态写保护 #PF 停机. 修复 (uaccess
#       写前预检 + clone TID 失败返回 EFAULT) 后 UEFI 5/5 复跑 PASS, 本腿已把
#       `[THREAD] PASS:` 加回硬断言并删除旧横幅; 登记册 = docs/plan/uefi-thread-exit-hang.md.
#
# 用法:
#   ./scripts/uefi_boot_test.sh [x86_64|all]   # 默认 x86_64
#   ./scripts/uefi_boot_test.sh --probe        # 只探测 OVMF 固件 (不构建不启动)
#   # aarch64 明确拒绝 (非"跳过"): OVMF 是 x86_64 固件, aarch64 的 UEFI 面属 L0 议题.
#   # --probe 是 OVMF 发现的**单源**: ci/audit.sh 用它决定跑腿还是 skip_leg 三态,
#   # 不在门禁侧另抄一份候选路径清单 (两份清单必漂移).
# 环境变量:
#   UEFI_BOOT_TIMEOUT  等待 `[SCHED] SNAP END` 里程碑的上限秒数 (默认 40)
#   UEFI_BOOT_SMP      核数 (默认 2, 与既有启动门禁同口径)
#   UEFI_OVMF_CODE / UEFI_OVMF_VARS  显式指定固件 (优先于发行版候选路径)
#
# 退出码: 0 = 判据全符合, 1 = 任一判据红 (含固件缺失; 三态由调用方 --probe 分辨)
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARG="${1:-x86_64}"
TIMEOUT="${UEFI_BOOT_TIMEOUT:-40}"
SMP="${UEFI_BOOT_SMP:-2}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

# 解包条目数下限: 与 scripts/initramfs_boot_test.sh 同口径 (根目录不计, 留 1 个余量).
MIN_UNPACKED=7
# firmware 保留区数下界 = **UEFI/BIOS 判别器**. 实测同一镜像: SeaBIOS 的 e820 只给出
# 7 区, OVMF 的 MB2 内存图给出 13/14 区 (随 VARS 状态浮动). 取 10 作下界 ⇒ 若 pflash
# 未生效而退化成 SeaBIOS 启动, 本腿即红, 不会把 BIOS 读数当成 UEFI 验证 (假绿).
# 同时覆盖"内存图根本没解析"(0 区) 这一形态.
MIN_FW_REGIONS=10

ok()   { echo -e "\033[0;32m✓ $1\033[0m"; }
err()  { echo -e "\033[0;31m✗ $1\033[0m"; }
warn() { echo -e "\033[1;33m! $1\033[0m"; }
info() { echo -e "\033[0;34m-> $1\033[0m"; }

# ── OVMF 固件发现 (单源) ────────────────────────────────────────────────────
# 只取**非 secboot** 的 raw `.fd`: secboot 变体需预 enroll 密钥, 而本仓 ISO 里的
# GRUB 未签名, 取它只会得到启动失败而非判据读数. qcow2 变体需另配 format, 不在射程.
OVMF_CODE=""
OVMF_VARS=""
OVMF_REASON=""
ovmf_discover() {
    local pairs=(
        "${UEFI_OVMF_CODE:-}|${UEFI_OVMF_VARS:-}"
        "/usr/share/edk2/ovmf/OVMF_CODE.fd|/usr/share/edk2/ovmf/OVMF_VARS.fd"
        "/usr/share/OVMF/OVMF_CODE.fd|/usr/share/OVMF/OVMF_VARS.fd"
        "/usr/share/edk2/ovmf/OVMF_CODE_4M.fd|/usr/share/edk2/ovmf/OVMF_VARS_4M.fd"
        "/usr/share/OVMF/OVMF_CODE_4M.fd|/usr/share/OVMF/OVMF_VARS_4M.fd"
        "/usr/share/qemu/OVMF_CODE.fd|/usr/share/qemu/OVMF_VARS.fd"
        "/usr/share/ovmf/OVMF_CODE.fd|/usr/share/ovmf/OVMF_VARS.fd"
    )
    local pair code vars
    for pair in "${pairs[@]}"; do
        code="${pair%%|*}"
        vars="${pair##*|}"
        [ -n "$code" ] && [ -n "$vars" ] || continue
        if [ -r "$code" ] && [ -r "$vars" ]; then
            OVMF_CODE="$code"
            OVMF_VARS="$vars"
            return 0
        fi
    done
    OVMF_REASON="OVMF 固件缺失: 已查 ${#pairs[@]} 组候选 (edk2/ovmf · OVMF · qemu 三处发行版路径, 非 secboot raw .fd); 可用 UEFI_OVMF_CODE / UEFI_OVMF_VARS 指定"
    return 1
}

case "$ARG" in
    --probe)
        # 机器面模式: 输出纯文本 (无颜色), 供 ci/audit.sh 直接作 skip_leg 的理由串.
        if ovmf_discover; then
            echo "OVMF CODE=$OVMF_CODE VARS=$OVMF_VARS"
            exit 0
        fi
        echo "$OVMF_REASON"
        exit 1
        ;;
    x86_64|all) ;;
    aarch64)
        err "本腿判据对象是 x86_64 UEFI(OVMF) 引导路径; aarch64 的 UEFI 面属 L0 议题, 不在此腿射程"
        exit 1
        ;;
    *) err "用法: $0 [x86_64|all|--probe]"; exit 1 ;;
esac

BOOT_LOG="$LOG_DIR/uefi_boot_x86_64.log"
BUILD_LOG="$LOG_DIR/uefi_build_x86_64.log"
# VARS 必须可写 (固件要写 EFI 变量), 而发行版文件是 root 只读 ⇒ 每轮拷一份私有的.
VARS_RUN="other/build/x86_64/uefi_vars.fd"

# D4: 统一 boot-with-module 载体 (cpio 经 GRUB module2 交付, 不嵌入内核).
source scripts/boot_module.sh

if ! ovmf_discover; then
    err "$OVMF_REASON"
    exit 1
fi
command -v qemu-system-x86_64 >/dev/null 2>&1 || {
    err "未找到 qemu-system-x86_64"
    exit 1
}

# ── 构建 + 组 ISO + 架构自检 ────────────────────────────────────────────────
info "[uefi] boot-module 构建 (出货默认核) + OVMF=$OVMF_CODE"
if ! boot_module_build x86_64 >"$BUILD_LOG" 2>&1; then
    err "[uefi] 构建失败 (日志: $BUILD_LOG)"
    exit 1
fi
if ! boot_module_iso_x86; then
    err "[uefi] GRUB module2 ISO 组包失败 (需 grub2-mkrescue)"
    exit 1
fi
# S-6 防线: 启动前先确认本架构用户态产物的架构身份 (它是 cpio 的来源).
if ! python3 scripts/verify_image_arch.py --arch x86_64 \
    --image other/build/x86_64/kernel.flat \
    --init other/build/x86_64/user/init.bin; then
    err "[uefi] 镜像架构自检未通过 — 拒绝在不可信镜像上跑门禁"
    exit 1
fi
mkdir -p other/build/x86_64
cp "$OVMF_VARS" "$VARS_RUN" || { err "[uefi] VARS 拷贝失败: $OVMF_VARS -> $VARS_RUN"; exit 1; }

# ── 启动 + 轮询里程碑 ───────────────────────────────────────────────────────
: > "$BOOT_LOG"
qemu-system-x86_64 \
    -M q35 -m 512 -smp "$SMP" \
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,file=$VARS_RUN" \
    $(boot_module_carrier_args x86_64) \
    -serial "file:$BOOT_LOG" -display none -no-reboot >/dev/null 2>&1 &
QPID=$!

# 轮询里程碑取 **SNAP END** (init 探针全部跑完后才导出), 不取 `[THREAD]` 行:
# THREAD 在 ~6 s 就出结果, 而 EXEC/WASM/SCHED 在 ~10 s 才打印 —— 以 THREAD 收尾
# 会把后半段判据对象整段切掉, 得到"缺行即红"的假红 (归因轮实测踩过).
DONE_MARKER='[SCHED] SNAP END'
_dead() { ! kill -0 "$QPID" 2>/dev/null; }
for _i in $(seq 1 $((TIMEOUT * 10))); do
    if grep -qaF "$DONE_MARKER" "$BOOT_LOG" || _dead; then
        break
    fi
    sleep 0.1
done
kill "$QPID" 2>/dev/null || true
wait "$QPID" 2>/dev/null || true

# ── 判定 ────────────────────────────────────────────────────────────────────
FAILED=0

# 日志含 NUL (内核日志与探针逐字符并发写串口), 必须 grep -a, 否则按二进制处理时
# 匹配行不输出 / 计数为 0.
need() {
    local marker="$1" desc="$2"
    if grep -qaF "$marker" "$BOOT_LOG"; then
        ok "[uefi] $desc"
    else
        err "[uefi] 缺判据 '$marker' ($desc) (日志: $BOOT_LOG)"
        FAILED=$((FAILED + 1))
    fi
}

forbid() {
    local marker="$1" desc="$2"
    if grep -qaF "$marker" "$BOOT_LOG"; then
        err "[uefi] 出现禁止行 '$marker' ($desc) (日志: $BOOT_LOG)"
        FAILED=$((FAILED + 1))
    fi
}

# 计数取数: 结果必须是非负十进制读数. grep 真错 (日志不可读等) 时输出为空/非数字,
# 此时**不得**读成 "0 条" —— 对账判据会把"测不出"当成"通过".
count_of() {
    local pattern="$1" n
    n=$(grep -caF "$pattern" "$BOOT_LOG" 2>/dev/null || true)
    case "$n" in
        '' | *[!0-9]*) return 1 ;;
        *) printf '%s\n' "$n" ;;
    esac
}

# 1. 真启动到用户态 + 载体链
need "Entering Ring 3" "OVMF 真启动到 Ring 3"
need "init from /init" "PID 1 由 boot-module 载体加载"
if ! UNPACKED=$(grep -ao "Unpacked [0-9]\+" "$BOOT_LOG" | tail -1 | cut -d' ' -f2); then
    UNPACKED=""
fi
case "$UNPACKED" in
    '' | *[!0-9]*)
        err "[uefi] 解包条目数取数失败 (日志: $BOOT_LOG) —— 测不出不等于条目足够"
        FAILED=$((FAILED + 1))
        ;;
    *)
        if [ "$UNPACKED" -lt "$MIN_UNPACKED" ]; then
            err "[uefi] 解包条目数不足: $UNPACKED < $MIN_UNPACKED"
            FAILED=$((FAILED + 1))
        else
            ok "[uefi] 解包里程碑: $UNPACKED 条目 (>= $MIN_UNPACKED)"
        fi
        ;;
esac

# 2. ACPI 经 MB2 (D4-i 甲) + 反向: 不得回落 BIOS 扫描
need "[ACPI] RSDP found via Multiboot2" "RSDP 经 MB2 tag14/15 内联副本取得 (D4-i 甲)"
forbid "[ACPI] RSDP found via BIOS scan" "UEFI 下不得走 BIOS 扫描回退"
need "[ACPI] Table scan complete: MADT=true FADT=true HPET=true" "MADT/FADT/HPET 三表全解析"
need "[ACPI] FADT: PM1a_EVT=" "FADT 电源管理寄存器读数"
need "[ACPI] HPET: base=0xFED00000" "HPET 基址读数"

# 3. 双核真上线
need "[ACPI] MADT: LAPIC base=0xFEE00000, AP count=$SMP" "MADT 报 $SMP 个 LAPIC"
need "[SMP] online CPUs: $SMP" "$SMP 核真上线"
need "[SMP] TLB probe PASS" "跨核 TLB shootdown 探针通过"

# 4. firmware 保留区: 声明侧与登记侧对账 (D4-i 乙)
# 串口行以 CRLF 收尾, 取数前必须连 \r 一起剥掉 —— 否则 "14\r" 过不了十进制校验,
# 判据会把"读到了"误报成"测不出" (本腿首轮实测踩过).
FW_LINE=$(grep -aF "Boot firmware reserved regions: " "$BOOT_LOG" | tail -1 | tr -d '\000\r')
FW_DECLARED="${FW_LINE##*: }"
case "$FW_DECLARED" in
    '' | *[!0-9]*)
        err "[uefi] firmware 保留区声明数取数失败 (日志: $BOOT_LOG)"
        FAILED=$((FAILED + 1))
        ;;
    *)
        if [ "$FW_DECLARED" -lt "$MIN_FW_REGIONS" ]; then
            err "[uefi] firmware 保留区仅 $FW_DECLARED 区 (< $MIN_FW_REGIONS) —— BIOS e820 实测 7 区 / OVMF 实测 13-14 区; \
疑 pflash 未生效退化成 SeaBIOS 启动, 或 UEFI 内存图未被解析 (ACPI 表会被当空闲页覆写)"
            FAILED=$((FAILED + 1))
        elif ! FW_RESERVED=$(count_of "[PMM] Early reserve 'firmware':"); then
            err "[uefi] firmware 预留登记数取数失败 (日志: $BOOT_LOG)"
            FAILED=$((FAILED + 1))
        elif [ "$FW_RESERVED" -ne "$FW_DECLARED" ]; then
            err "[uefi] 声明 $FW_DECLARED 区但只登记 $FW_RESERVED 区 —— 两侧不对账 (D4-i 乙)"
            FAILED=$((FAILED + 1))
        else
            ok "[uefi] firmware 保留区 $FW_DECLARED 区全部进 PMM 早期预留 (D4-i 乙)"
        fi
        ;;
esac
need "[PMM] Early reserve 'mb2-info':" "MB2 信息块本体预留 (ACPI 发现面在 PMM 之后才读它)"

# 5. 内存图权威 (D4-g): tag6 的 max_addr, 不是 tag4 的基本内存
MEM_LINE=$(grep -aF "Boot info: mem=" "$BOOT_LOG" | tail -1 | tr -d '\000\r')
MEM_MB="${MEM_LINE#*Boot info: mem=}"
MEM_MB="${MEM_MB%% MB*}"
case "$MEM_MB" in
    '' | *[!0-9]*)
        err "[uefi] mem_size 取数失败 (日志: $BOOT_LOG)"
        FAILED=$((FAILED + 1))
        ;;
    *)
        # 下界 400: -m 512 下 tag6 报 511 MB, 而 OVMF 的 tag4 只报 ~8 MB
        # (host 回归锁夹具 640/7168 KB). 修复前 tag4 覆盖 tag6 ⇒ 读数 8, 本判据即红.
        if [ "$MEM_MB" -lt 400 ]; then
            err "[uefi] mem_size=${MEM_MB} MB < 400 —— tag4 基本内存覆盖了 tag6 内存图 (D4-g 回归)"
            FAILED=$((FAILED + 1))
        else
            ok "[uefi] 内存图权威: mem=${MEM_MB} MB (tag6, 非 tag4 兜底) (D4-g)"
        fi
        ;;
esac

# 6. 引导模块早期搬移 (D4-h): 真相交则 relocate；已不相交则 clear 行显式记录.
if grep -aqF "[PMM] Early relocate 'initrd':" "$BOOT_LOG" \
    || grep -aqF "[PMM] Early reserve 'initrd' clear of boot window:" "$BOOT_LOG"; then
    ok "[uefi] initrd 引导窗语义成立 (relocate 或显式 clear) (D4-h)"
else
    err "[uefi] initrd 搬移/clear 证据缺失 (D4-h)"
    FAILED=$((FAILED + 1))
fi

# 7. 用户态探针可达面
need "[TLS] PASS:" "TLS fork 继承 + 跨进程切换隔离"
need "[SIG] PASS:" "信号 svc-return + irq-exit 投递"
need "[UACCESS] PASS:" "只读 TID 回写返回 EFAULT (U-1 根因回归锁)"
need "[THREAD] PASS:" "线程组 clone/futex/join/exit_group 全链路 (U-1 修复后回归项)"
need "[EXEC] PASS: cwd+root" "execve 运行时射程聚合行 (cwd + chroot 两腿)"
forbid "[EXEC] FAIL" "execve 单腿失败不得被聚合 PASS 掩盖"
need "[WASM] PASS:" "wasm 进程级载体端到端 (W-7)"
forbid "[WASM] FAIL" "wasm 探针失败腿"
need "$DONE_MARKER" "EEVDF 观测导出"
need "conserve=ok" "EEVDF 账目守恒"
forbid "conserve=BAD" "EEVDF 账目不守恒"
need "[KPTI] EL0 kernel high-half access denied" "KPTI 用户态拒访内核高半区"

# 8. 驱动与网络
need "e1000: 初始化完成" "e1000 网卡初始化"
need "DHCP configured (lease applied)" "DHCP 租约生效"

# 9. P-62 默认能力行 (与 scripts/qemu_boot_test.sh 同口径; 本腿不切 feature)
CAPS_LINE=$(grep -aoE "smp=(on|off) preempt=(on|off) kaslr=(on|off) kpti=(on|off) freg=(on|off)" "$BOOT_LOG" | head -1 || true)
if [ "$CAPS_LINE" = "smp=off preempt=off kaslr=off kpti=on freg=off" ]; then
    ok "[uefi] P-62 默认能力行: $CAPS_LINE"
else
    err "[uefi] P-62 默认能力行不符 — 期望 'smp=off preempt=off kaslr=off kpti=on freg=off', 实测 '${CAPS_LINE:-无}'"
    FAILED=$((FAILED + 1))
fi

echo "================================================================"
echo "================================================================"
if [ "$FAILED" -ne 0 ]; then
    err "UEFI 引导真跑腿失败: $FAILED 项 (D4-i 丙; 日志: $BOOT_LOG)"
    exit 1
fi
ok "UEFI 引导真跑腿判据全符合 (D4-i 丙; 含 U-1 修复后的 [THREAD] 硬断言)"
exit 0