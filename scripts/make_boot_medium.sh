#!/bin/bash
# ============================================================================
# Edgine 真机引导介质制作脚本 (Boot Medium Builder)
#
# 用途: 将双架构内核制品打包为可写入 USB 的真机引导介质.
#   - x86_64 : GRUB2 multiboot2 ISO (BIOS/UEFI 兼容, 复用 `make iso` 逻辑)
#   - aarch64: 整盘镜像 (MBR + 单个 FAT32 分区), 内含 arm64 Image 与
#              U-Boot 引导配置 (extlinux/extlinux.conf); 由 U-Boot distro boot
#              (`sysboot` -> `booti`) 引导.
#
# 用法:
#   ./scripts/make_boot_medium.sh x86_64
#   ./scripts/make_boot_medium.sh x86_64  --write /dev/sdX
#   ./scripts/make_boot_medium.sh aarch64
#   ./scripts/make_boot_medium.sh aarch64 --load-addr 0x40080000 --write /dev/sdX
#
# 选项:
#   --iso                 仅 x86_64: 复用 GRUB2 ISO 产出 (默认行为, 保留占位)
#   --load-addr <addr>    仅 aarch64: Image 装载地址 (默认 0x40080000, 写入 boot.cmd)
#   --size <N>M           仅 aarch64: 整盘镜像大小 (默认 128M)
#   --write <device>      将产物写入块设备 (破坏性操作, 需二次确认)
#   -h | --help           帮助
#
# 依赖:
#   x86_64 : grub2-mkrescue + xorriso (与 requirements.sh §7 ISO 工具一致)
#   aarch64: sfdisk + mkfs.vfat(dosfstools) + mtools(mcopy)
#            注: 不依赖 mkimage — aarch64 采用 U-Boot distro boot 原生的
#                extlinux.conf, 无需 u-boot-tools; 需要 boot.scr 时见
#                docs/explain/guide-hardware-boot.md 的手工 mkimage 说明.
#
# 产物:
#   x86_64 : other/build/boot/edgine-x86_64.iso
#   aarch64: other/build/boot/edgine-aarch64.img
#
# 与 QEMU 的关系:
#   aarch64 的 Image 制品与 `make ARCH=aarch64 all` / QEMU `-kernel` 共用同一
#   链接契约 (见 src/kernel/framework/link/aarch64.ld 内嵌 arm64 Image 头),
#   故真机介质与 QEMU 验证路径保持单一来源, 避免双契约漂移.
# ============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

OUT_DIR="other/build/boot"
ISO_OUT="${OUT_DIR}/edgine-x86_64.iso"
IMG_OUT="${OUT_DIR}/edgine-aarch64.img"

# aarch64 默认参数: 装载地址须与 Image 头 text_offset 一致
# (DRAM 基址 0x40000000 + text_offset 0x80000 = 0x40080000).
AARCH64_LOAD_ADDR="0x40080000"
AARCH64_SIZE_MIB=128

ok()   { echo -e "${GREEN}\u2713 $1${NC}"; }
err()  { echo -e "${RED}\u2717 $1${NC}"; }
warn() { echo -e "${YELLOW}! $1${NC}"; }
info() { echo -e "${BLUE}-> $1${NC}"; }

usage() {
    sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
}

# 工具存在性检查 (缺失即 fail-closed 并给出安装提示)
require_cmd() {
    local cmd="$1" pkg="$2"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        err "缺少工具: $cmd (包: $pkg)"
        echo "    安装: sudo apt install $pkg"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# 破坏性写入: 将镜像写入块设备 (含多重安全护栏)
# 护栏: ① 必须为块设备; ② 拒绝已挂载设备; ③ 拒绝承载当前根文件系统的设备;
#       ④ 显式输入设备路径 + "YES" 二次确认.
# ---------------------------------------------------------------------------
confirm_and_write() {
    local img="$1" dev="$2"

    if [ ! -b "$dev" ]; then
        err "$dev 不是块设备, 拒绝写入"
        exit 1
    fi

    # 拒绝已挂载 (或任一分区已挂载) 的设备
    if lsblk -no MOUNTPOINT "$dev" 2>/dev/null | grep -q '[^[:space:]]'; then
        err "$dev 或其分区已挂载, 拒绝写入"
        echo "    请先卸载: sudo umount ${dev}*"
        exit 1
    fi

    # 拒绝承载当前根文件系统的设备 (含其分区所在磁盘)
    local root_src root_disk dev_disk
    root_src="$(findmnt -no SOURCE / 2>/dev/null || true)"
    if [ -n "$root_src" ]; then
        root_disk="$(lsblk -no PKNAME "$root_src" 2>/dev/null | head -1 || true)"
        dev_disk="$(lsblk -no PKNAME "$dev" 2>/dev/null | head -1 || true)"
        [ -n "$dev_disk" ] || dev_disk="$(basename "$dev")"
        if [ "$dev_disk" = "$root_disk" ]; then
            err "$dev 承载当前根文件系统 (磁盘 $root_disk), 拒绝写入"
            exit 1
        fi
    fi

    warn "即将擦除并写入 $dev 的全部数据 (不可逆)"
    echo "    镜像: $img ($(du -h "$img" | cut -f1))"
    echo "    目标: $dev"
    echo "    --- 目标设备信息 ---"
    lsblk "$dev" || true
    echo "    --------------------"

    local input_dev confirm
    read -r -p "输入设备路径以确认 (${dev}): " input_dev
    if [ "$input_dev" != "$dev" ]; then
        err "设备路径不匹配, 已取消"
        exit 1
    fi
    read -r -p "输入 YES 以确认不可逆写入: " confirm
    if [ "$confirm" != "YES" ]; then
        err "未确认, 已取消"
        exit 1
    fi

    local sudo_cmd=""
    if [ "$(id -u)" -ne 0 ]; then
        sudo_cmd="sudo"
    fi

    info "写入 $img -> $dev ..."
    $sudo_cmd dd if="$img" of="$dev" bs=4M conv=fsync status=progress
    $sudo_cmd sync
    ok "写入完成: $dev (可安全拔出)"
}

# ---------------------------------------------------------------------------
# x86_64: GRUB2 multiboot2 ISO (复用 Makefile `iso` target, 避免双份 GRUB 逻辑)
# ---------------------------------------------------------------------------
build_x86_64() {
    require_cmd grub2-mkrescue "grub-pc-bin grub-common xorriso"
    require_cmd xorriso "xorriso"

    info "构建 x86_64 内核与用户态并打包 ISO (make ARCH=x86_64 iso)..."
    make ARCH=x86_64 iso

    if [ ! -f other/build/antx.iso ]; then
        err "未生成 other/build/antx.iso, 请检查 make iso 输出"
        exit 1
    fi

    mkdir -p "$OUT_DIR"
    cp other/build/antx.iso "$ISO_OUT"
    ok "ISO 产物: $ISO_OUT"
    file "$ISO_OUT" || true
}

# ---------------------------------------------------------------------------
# aarch64: 整盘镜像 (MBR + 单个 FAT32 分区) + U-Boot 引导配置
# 布局: 分区表 MBR; 分区 1 = FAT32, 起始 1MiB (2048 扇区), 类型 0x0c.
# ---------------------------------------------------------------------------
build_aarch64() {
    require_cmd sfdisk "util-linux"
    require_cmd mkfs.vfat "dosfstools"
    require_cmd mcopy "mtools"

    info "构建 aarch64 内核 Image (make ARCH=aarch64 all)..."
    make ARCH=aarch64 all

    if [ ! -f other/build/kernel-aarch64.img ]; then
        err "未生成 other/build/kernel-aarch64.img, 请检查 make all 输出"
        exit 1
    fi

    mkdir -p "$OUT_DIR"
    local work
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' RETURN

    # 1) U-Boot 引导配置
    #    extlinux.conf: U-Boot distro boot (sysboot) 原生读取, 无外部工具依赖;
    #    装载地址不显式给出 — U-Boot 按 Image 头 text_offset (0x80000) 放置到
    #    DRAM 基址 + text_offset = 0x40080000, 与内核链接地址一致.
    #    boot.cmd: 等价的手工 `booti` 脚本, 供自定义 bootcmd 的板子使用.
    cat > "$work/extlinux.conf" <<EOF
# Edgine aarch64 U-Boot extlinux 配置 (由 U-Boot distro boot / sysboot 读取)
default edgine
timeout 30
label edgine
    menu label Edgine (aarch64)
    linux /Image
EOF
    cat > "$work/boot.cmd" <<EOF
# Edgine aarch64 U-Boot 手工引导脚本 (等价路径, 供自定义 bootcmd 使用)
# 打包为 boot.scr: mkimage -A arm64 -O linux -T script -C none -n "Edgine" -d boot.cmd boot.scr
echo "Booting Edgine (aarch64) ..."
fatload mmc 0:1 ${AARCH64_LOAD_ADDR} Image
booti ${AARCH64_LOAD_ADDR} - \${fdtcontroladdr}
EOF

    # 2) 整盘镜像: 空白 -> MBR 分区表 -> FAT32 (偏移 1MiB) -> 拷入文件
    local disk="$work/disk.img"
    local total_sectors=$((AARCH64_SIZE_MIB * 2048))
    local part_sectors=$((total_sectors - 2048))
    dd if=/dev/zero of="$disk" bs=1M count="$AARCH64_SIZE_MIB" status=none

    sfdisk "$disk" >/dev/null <<EOF
label: dos
unit: sectors
start=2048, size=${part_sectors}, type=c
EOF

    mkfs.vfat --offset=2048 -F 32 -n EDGINE "$disk" >/dev/null

    # mtools `@@<offset>` 语法定位分区内偏移 (1MiB = 1048576 字节)
    mcopy -i "${disk}@@1M" other/build/kernel-aarch64.img ::Image
    mmd   -i "${disk}@@1M" ::extlinux
    mcopy -i "${disk}@@1M" "$work/extlinux.conf" ::extlinux/extlinux.conf
    mcopy -i "${disk}@@1M" "$work/boot.cmd" ::boot.cmd

    cp "$disk" "$IMG_OUT"
    ok "整盘镜像产物: $IMG_OUT"
    file "$IMG_OUT" || true
    echo "    --- 分区内文件列表 ---"
    mdir -i "${IMG_OUT}@@1M" :: || true
    echo "    ----------------------"
    warn "使用前提: 目标板 U-Boot 具备 MMC + FAT + distro boot (sysboot/booti) 能力, 且 DRAM 基址为 0x40000000"
    warn "串口 checklist 与 SoC 契约边界见 docs/explain/guide-hardware-boot.md"
}

# ---------------------------------------------------------------------------
# 参数解析
# ---------------------------------------------------------------------------
ARCH=""
WRITE_DEV=""
while [ $# -gt 0 ]; do
    case "$1" in
        x86_64|aarch64)
            ARCH="$1"; shift ;;
        --iso)
            shift ;;
        --load-addr)
            AARCH64_LOAD_ADDR="${2:?--load-addr 需要参数}"; shift 2 ;;
        --size)
            AARCH64_SIZE_MIB="${2:?--size 需要参数}"; shift 2 ;;
        --write)
            WRITE_DEV="${2:?--write 需要设备路径}"; shift 2 ;;
        -h|--help)
            usage; exit 0 ;;
        *)
            err "未知参数: $1"; echo; usage; exit 1 ;;
    esac
done

if [ -z "$ARCH" ]; then
    err "请指定架构: x86_64 | aarch64"
    echo; usage; exit 1
fi

case "$ARCH" in
    x86_64)
        build_x86_64
        if [ -n "$WRITE_DEV" ]; then
            confirm_and_write "$ISO_OUT" "$WRITE_DEV"
        fi
        ;;
    aarch64)
        build_aarch64
        if [ -n "$WRITE_DEV" ]; then
            confirm_and_write "$IMG_OUT" "$WRITE_DEV"
        fi
        ;;
esac

ok "完成 (ARCH=$ARCH)"
