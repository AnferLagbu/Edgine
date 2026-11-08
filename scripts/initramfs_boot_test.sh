#!/bin/bash
# ============================================================================
# initramfs 载体启动测试 (K-07b 专项 feature 腿: 批1 载体 + 批2 execve 射程)
#
# 用途: 以 `KERNEL_FEATURES=initramfs` 真构建 + 真启动, 断言 PID 1 是经**文件系统**
#       (cpio 归档 → ramfs `/init` → VFS 读出 → ELF 装载) 起来的, 而不是编译期
#       `include_bytes!` 的内嵌 `init.bin`.
#
#       默认构建不含该 feature (内嵌 init 仍是出货路径), 所以这条链在默认门禁里
#       完全不可见 —— 与 P-62 甲 的 feature 运行维同形: 只有真构建才暴露断链.
#       本腿断言四条判据 (缺一即红):
#         1. `[INITRAMFS] Unpacked N files` 且 N >= 7 —— 归档被解析且真落盘
#            (数量下限把"只有根目录/只有 /init 落盘"的嵌套目录回归钉住);
#         2. `init from /init` —— PID 1 的镜像来自 VFS 读取 (回退路径打印的是
#            `Entering Ring 3 (init pid=...)`, 不含该串);
#         3. `[THREAD] PASS` —— 载体加载的 init 真跑起来并打完 K-07a 线程组探针,
#            证明从文件系统读出的 ELF 与内嵌 ELF 等价可执行;
#         4. `[EXEC] PASS: cwd+root` 且无 `[EXEC] FAIL` —— K-07b 批2 的 execve
#            运行时射程: cwd 相对名 + chroot 视图绝对路径两条腿都 exec 成功,
#            载荷回读的 argv/envp/cwd 全部一致 (同时钉住归档权限位保留 ——
#            无 x 位即 EACCES). 聚合行只在两腿全过后由 init 父进程打印;
#         5. `[SCHED] SNAP END` 且 `conserve=ok` 无 `conserve=BAD` —— K-07b 批3
#            的 EEVDF 观测面在载体装载路径下同样可达且账目守恒.
#
# 跑完复原默认构建: 本脚本切换过 KERNEL_FEATURES, 若不复原, 后续门禁会跑在
# 带 feature 的镜像上 (与 §2.3 默认构建口径不符). 复原放在判定之后, 失败也已复原.
#
# 构建面取 `--no-default-features --features initramfs` (KERNEL_NO_DEFAULT=1):
# A-1 (2026-11-08 裁定甲) 把 lockdep 放进默认 features 后, x86_64 载体镜像末尾
# 达 0x413000, 越过 K-08 的 USER_CODE_BASE=0x400000 (kview 恒等窗口将覆盖用户
# 映射区) ⇒ 启动期 fail-closed 停机. 载体是诊断 feature、非出货形态, 故本腿
# 显式关闭默认 features (实测末尾 0x3EA000, 五条判据全绿); 出货默认镜像仍
# lockdep 常开, 由 P-62 构建面探针与默认启动腿独立覆盖.
#
# 用法:
#   ./scripts/initramfs_boot_test.sh [x86_64|aarch64|all]   # 默认 all
# 环境变量:
#   INITRAMFS_BOOT_TIMEOUT  单次启动等待里程碑上限秒数 (默认 40)
#   INITRAMFS_BOOT_SMP      核数 (默认 2, 与线程组判据同口径)
#
# 退出码: 0 = 全部通过, 1 = 存在失败
# ============================================================================

set -uo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

ARCH="${1:-all}"
TIMEOUT="${INITRAMFS_BOOT_TIMEOUT:-40}"
SMP="${INITRAMFS_BOOT_SMP:-2}"
LOG_DIR="other/build/log"
mkdir -p "$LOG_DIR"

# 解包条目数下限: 根目录不计, `/init` + `/bin` + 6 个 `/bin/*` 用户态程序 = 8,
# 留 1 个余量 (某架构缺可选程序时仍成立).
MIN_UNPACKED=7

ok()   { echo -e "\033[0;32m✓ $1\033[0m"; }
err()  { echo -e "\033[0;31m✗ $1\033[0m"; }
info() { echo -e "\033[0;34m-> $1\033[0m"; }

case "$ARCH" in
    x86_64|aarch64|all) ;;
    *) err "用法: $0 [x86_64|aarch64|all]"; exit 1 ;;
esac

ARCH_LIST="$ARCH"
[ "$ARCH" = "all" ] && ARCH_LIST="x86_64 aarch64"

image_of() {
    if [ "$1" = "x86_64" ]; then
        echo "other/build/x86_64/kernel.flat"
    else
        echo "other/build/aarch64/kernel-aarch64.img"
    fi
}

qemu_of() { echo "qemu-system-$1"; }

qemu_args_of() {
    local arch="$1"
    if [ "$arch" = "x86_64" ]; then
        echo "-M q35 -m 512 -smp $SMP -kernel $(image_of "$arch")"
    else
        echo "-M virt,gic-version=3 -cpu max -m 512 -smp $SMP -kernel $(image_of "$arch")"
    fi
}

# 一次载体启动: 起 QEMU, 轮询三条判据, 全齐即提前收尾.
boot_and_check() {
    local arch="$1"
    local log="$LOG_DIR/initramfs_boot_${arch}.log"
    local image
    image="$(image_of "$arch")"

    if [ ! -f "$image" ]; then
        err "[$arch] 镜像缺失: $image"
        return 1
    fi
    command -v "$(qemu_of "$arch")" >/dev/null 2>&1 || {
        err "[$arch] 未找到 $(qemu_of "$arch")"
        return 1
    }

    : > "$log"
    # shellcheck disable=SC2046  # qemu_args_of 输出需按空格分词为多个参数
    "$(qemu_of "$arch")" $(qemu_args_of "$arch") \
        -serial "file:$log" -display none -no-reboot >/dev/null 2>&1 &
    local qpid=$!

    local marker_unpack="\[INITRAMFS\] Unpacked"
    local marker_carrier="init from /init"
    local marker_thread="\[THREAD\] PASS"
    # 判据取 init 父进程的聚合行 (`cwd+root` 两腿全过才打印). 不能只查
    # `[EXEC] PASS` —— 载荷每腿都会打印 `[EXEC] PASS: <腿> argv+envp+cwd`,
    # 首腿通过即会让轮询提前收尾, root 腿与聚合行随之被切掉 (假绿).
    local marker_exec="\[EXEC\] PASS: cwd+root"
    # K-07b 批3: 载体 init 在睡醒后触发 EEVDF 观测导出, 命中 END 行且账目
    # 守恒即证明观测面在载体装载路径下同样可达.
    local marker_snap="\[SCHED\] SNAP END"
    local rc=1
    local _i
    # 日志含 NUL (内核日志与探针逐字符并发写串口), 必须 grep -a, 否则按二进制处理
    # 匹配行不输出.
    for _i in $(seq 1 $((TIMEOUT * 10))); do
        if grep -aq "$marker_carrier" "$log" && grep -aq "$marker_thread" "$log" \
            && grep -aq "$marker_exec" "$log" && grep -aq "$marker_snap" "$log"; then
            rc=0
            break
        fi
        kill -0 "$qpid" 2>/dev/null || break
        sleep 0.1
    done

    kill "$qpid" 2>/dev/null || true
    wait "$qpid" 2>/dev/null || true

    if ! grep -aq "$marker_unpack" "$log"; then
        err "[$arch] 未出现解包里程碑 '$marker_unpack' (日志: $log)"
        return 1
    fi
    local unpacked
    unpacked=$(grep -ao "Unpacked [0-9]\+" "$log" | tail -1 | cut -d' ' -f2)
    if [ -z "$unpacked" ] || [ "$unpacked" -lt "$MIN_UNPACKED" ]; then
        err "[$arch] 解包条目数不足: '${unpacked:-?}' < $MIN_UNPACKED (嵌套目录未落盘?)"
        return 1
    fi
    ok "[$arch] 解包里程碑: $unpacked 条目 (>= $MIN_UNPACKED)"

    if ! grep -aq "$marker_carrier" "$log"; then
        err "[$arch] 未出现载体里程碑 '$marker_carrier' —— PID 1 仍走内嵌 init.bin (日志: $log)"
        return 1
    fi
    ok "[$arch] PID 1 由载体加载: $(grep -a "$marker_carrier" "$log" | tail -1)"

    if ! grep -aq "$marker_thread" "$log"; then
        err "[$arch] 载体加载的 init 未打完线程组探针 '$marker_thread' (日志: $log)"
        return 1
    fi
    ok "[$arch] 载体 init 运行判据: $(grep -a "$marker_thread" "$log" | tail -1)"

    if ! grep -aq "$marker_snap" "$log"; then
        err "[$arch] 载体 init 未触发 EEVDF 观测导出 '$marker_snap' (日志: $log)"
        return 1
    fi
    if ! grep -aq "conserve=ok" "$log" || grep -aq "conserve=BAD" "$log"; then
        err "[$arch] EEVDF 观测账目守恒判据失败 (期待 conserve=ok 且无 conserve=BAD)"
        return 1
    fi
    ok "[$arch] EEVDF 观测导出且账目守恒 (K-07b 批3)"

    if ! grep -aq "$marker_exec" "$log"; then
        err "[$arch] execve 运行时探针未通过 '$marker_exec' (日志: $log)"
        return 1
    fi
    # 失败行出现即红: 即便 PASS 行来自另一条腿, 单腿失败也不得被掩盖.
    if grep -aq "\[EXEC\] FAIL" "$log"; then
        err "[$arch] execve 运行时探针存在失败腿: $(grep -a "\[EXEC\] FAIL" "$log" | tail -1)"
        return 1
    fi
    ok "[$arch] execve 运行时判据: $(grep -a "$marker_exec" "$log" | tail -1)"

    return "$rc"
}

FAILED=0
for arch in $ARCH_LIST; do
    echo "================================================================"
    info "[$arch] KERNEL_FEATURES=initramfs KERNEL_NO_DEFAULT=1 构建..."
    # KERNEL_NO_DEFAULT=1: A-1 常开 lockdep 后, x86_64 载体镜像末尾 0x413000 越过
    # K-08 的 USER_CODE_BASE=0x400000 (恒等窗口覆盖用户映射). 载体是诊断 feature、
    # 非出货形态, 故本腿显式关闭默认 features (实测末尾 0x3EA000); 出货默认镜像
    # 仍 lockdep 常开, 由 P-62 构建面探针与默认启动腿独立覆盖.
    if ! make ARCH="$arch" KERNEL_NO_DEFAULT=1 KERNEL_FEATURES=initramfs all >"$LOG_DIR/initramfs_build_${arch}.log" 2>&1; then
        err "[$arch] 带 feature 构建失败 (日志: $LOG_DIR/initramfs_build_${arch}.log)"
        FAILED=$((FAILED + 1))
        continue
    fi
    # S-6 防线③: 启动前先确认镜像内嵌的本架构用户态产物一致
    if ! python3 scripts/verify_image_arch.py --arch "$arch" \
        --image "$(image_of "$arch")" \
        --init "other/build/$arch/user/init.bin"; then
        err "[$arch] 镜像架构自检未通过 — 拒绝在不可信镜像上跑门禁"
        FAILED=$((FAILED + 1))
        continue
    fi
    if ! boot_and_check "$arch"; then
        FAILED=$((FAILED + 1))
    fi
done

# 复原默认构建 (KERNEL_FEATURES 空 ⇒ Makefile 内容戳记失配触发重建)
for arch in $ARCH_LIST; do
    info "[$arch] 复原默认构建 (KERNEL_FEATURES 空)..."
    if ! make ARCH="$arch" all >"$LOG_DIR/initramfs_restore_${arch}.log" 2>&1; then
        err "[$arch] 默认构建复原失败 (日志: $LOG_DIR/initramfs_restore_${arch}.log)"
        FAILED=$((FAILED + 1))
    fi
done

echo "================================================================"
if [ "$FAILED" -ne 0 ]; then
    err "initramfs 载体腿失败: $FAILED 项 (K-07b 批1)"
    exit 1
fi
ok "initramfs 载体腿全部通过 (K-07b 批1)"
exit 0
