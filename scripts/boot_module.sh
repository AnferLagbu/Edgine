#!/bin/bash
# ============================================================================
# boot_module.sh — D4 共享「boot-with-module」载体 helper (被各启动腿 `source`)。
#
# 背景: D3 严格零嵌入后, 内核镜像不再编译期嵌入 initramfs cpio; 用户态内容由引导器
# 作 **boot-module** 交付 (x86_64 GRUB `multiboot2` + `module2`; aarch64 QEMU virt
# `-kernel Image -initrd` 经 DTB /chosen)。各默认启动腿须统一改走本 helper 交付 cpio,
# 才能在不嵌 cpio 的镜像上把 init 从 boot-module 载起来 (D4-b 逐腿接线)。
#
# 说明: D4-e 起 `initramfs` feature 已退役 (零嵌入后解包无条件执行, 该 feature 不改任何
# 代码路径), 载体腿一律构建**出货默认**镜像 (lockdep 常开), cpio 只经 boot-module 交付。
#
# 用法: 在启动腿里 `source scripts/boot_module.sh`, 调用下列函数。
# ============================================================================

# 构建出货默认内核 (cpio 不嵌入; 消费者 load_init_from_initramfs 读 boot-module)。
boot_module_build() {
    local arch="$1"
    make ARCH="$arch" all
}

# 非-lockdep 变体 (liveness / perf 测量专用: 关 lockdep 降观测开销失真).
boot_module_build_nolockdep() {
    local arch="$1"
    make ARCH="$arch" KERNEL_NO_DEFAULT=1 all
}

# x86_64: 组 GRUB module2 ISO (multiboot2 kernel.bin + module2 cpio),
# 产出 other/build/x86_64/boot_module.iso。
boot_module_iso_x86() {
    local idir="other/build/boot_module_isodir"
    rm -rf "$idir"; mkdir -p "$idir/boot/grub"
    cp other/build/x86_64/kernel.bin "$idir/boot/kernel.bin" || return 1
    cp other/build/x86_64/user/initramfs.cpio "$idir/boot/initramfs.cpio" || return 1
    cat > "$idir/boot/grub/grub.cfg" <<'GEOF'
set timeout=0
set default=0
menuentry "Edgine" {
    multiboot2 /boot/kernel.bin
    module2 /boot/initramfs.cpio initramfs
}
GEOF
    command -v grub2-mkrescue >/dev/null 2>&1 || return 1
    grub2-mkrescue -o other/build/x86_64/boot_module.iso "$idir" >/dev/null 2>&1
}

# 引导产物路径 (x86_64 = GRUB ISO; aarch64 = arm64 Image)。
boot_module_image() {
    if [ "$1" = "x86_64" ]; then
        echo "other/build/x86_64/boot_module.iso"
    else
        echo "other/build/aarch64/kernel-aarch64.img"
    fi
}

# QEMU 内核启动片段 (仅引导载体; -m / -smp / 设备由调用腿自行追加)。
boot_module_carrier_args() {
    local arch="$1"
    if [ "$arch" = "x86_64" ]; then
        echo "-cdrom $(boot_module_image x86_64)"
    else
        echo "-kernel $(boot_module_image aarch64) -initrd other/build/aarch64/user/initramfs.cpio"
    fi
}
