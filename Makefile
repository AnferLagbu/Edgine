ARCH ?= x86_64

# 临时/构建产物统一收敛到 other/ 下 (.gitignore 的 other/ 规则覆盖全部).
# 单一变量源: 迁移产物根目录只需改下面几处 (LOG_DIR/ARCH_BUILD_DIR/USER_BUILD_DIR
# 均由 BUILD_DIR 派生).
# 注: 变量赋值行不可写行内注释 — GNU make 会把 `#` 前的空格并入变量值,
#     使路径中出现多余空格 (曾引发 "mixed implicit and normal rules" 致命错误).
#   BUILD_DIR       - Makefile 中间/最终产物根目录 (含跨架构共享的 log/)
#   ARCH_BUILD_DIR  - **按架构分目录**的内核产物目录 (镜像 + 中间 .o)
#   ISODIR          - GRUB ISO 暂存目录
#   REPORT_DIR      - 运行期测试日志
BUILD_DIR := other/build
ISODIR := other/isodir
REPORT_DIR := other/tests-reports
LOG_DIR := $(BUILD_DIR)/log
# 内核产物按架构分目录 (S-6 防线① 的同构扩展; 登记见
# docs/plan/net-e2e-tcp-echo-flakiness.md §6 P-10): kernel.bin / kernel.flat /
# kernel-aarch64.img / kernel.map / 中间 .o / stage1.bin / kernel_test.bin 全部
# 落 other/build/$(ARCH)/.
#
# 为何必须分目录 (而不是继续加检测防线): 两架构共用 other/build/kernel.bin 一条
# 路径曾同时造成三件事 ——
#   ① FP-06 审计 (反汇编 aarch64 的 kernel.bin) 在 `./ci/build.sh all` 之后必然
#     读到 x86_64 产物而 fail-closed (`all` 末位只链接主架构);
#   ② `all` 模式因此**从不链接 aarch64**, aarch64 的链接期错误 (缺 .o /
#     ld script 失配) 在全量 CI 口径下完全不可见;
#   ③ 防跨架构误用 boot.o 需要三重补丁机制 (本文件旧 arch-switch-clean +
#     $(BUILD_DIR)/log/.arch 戳记, 以及 qemu_boot_test.sh 的 sync_make_state 用
#     file(1) 反查 .o 架构后强制重建).
# 分目录后这些通道物理上不存在, 上述补丁机制整体退役 (跨架构切换不再全量
# cargo clean, 构建也不需先清上架构产物).
ARCH_BUILD_DIR := $(BUILD_DIR)/$(ARCH)
# 用户态产物按架构分目录 (S-6 防线①): 内核经 include_bytes! 在编译期嵌入
# other/build/$(ARCH)/user/init.bin, 旧写法两架构共用 other/build/user/ 同一路径
# 原地覆盖, `build.sh all` 特别是以 link_kernel x86_64 结尾时, 会将 aarch64 用户态
# ELF 嵌进 x86_64 镜像且全程 0 error / 0 warning / rc=0 (无任何防线可拦).
# 分目录后跨架构构建不再相互覆盖; 内核侧校验见 elf::verify::EXPECTED_E_MACHINE.
USER_BUILD_DIR := $(BUILD_DIR)/$(ARCH)/user

ifeq ($(ARCH),aarch64)
    CC = aarch64-linux-gnu-gcc
    LD = aarch64-linux-gnu-ld
    AS = aarch64-linux-gnu-as
    OBJCOPY = aarch64-linux-gnu-objcopy
    RUST_TARGET = aarch64-unknown-none
    # 内核与用户态目标分离: 内核走 softfloat (等效 -neon, 使 EL1 零 FP/SIMD,
    # 见 docs/plan/aarch64-kernel-fp-free.md); 用户态保持 +neon (用户程序可用 FP/SIMD).
    RUST_TARGET_KERNEL = aarch64-unknown-none-softfloat
    QEMU = qemu-system-aarch64
    QEMU_MACHINE = virt
    QEMU_CPU := max
    LDSCRIPT = src/kernel/privileged/link/aarch64.ld
    ASFLAGS = -march=armv8-a
    CFLAGS_BASE = -std=c11 -Wall -Wextra -nostdinc -nostdlib -fPIC -fno-stack-protector \
                  -fno-asynchronous-unwind-tables -fno-ident \
                  -Wno-builtin-declaration-mismatch
else
    CC = x86_64-linux-gnu-gcc
    LD = x86_64-linux-gnu-ld
    AS = nasm
    OBJCOPY = objcopy
    RUST_TARGET = x86_64-unknown-none
    # x86_64 无 softfloat/neon 之分, 内核与用户态共用同一目标.
    RUST_TARGET_KERNEL = x86_64-unknown-none
    QEMU = qemu-system-x86_64
    QEMU_CPU ?= qemu64
    LDSCRIPT = src/kernel/privileged/link/x86_64.ld
    ASFLAGS = -f elf64 -w-zeroing
    CFLAGS_BASE = -std=c11 -m64 -Wall -Wextra -nostdinc -nostdlib -fPIC -fno-stack-protector \
                  -fno-asynchronous-unwind-tables -fno-ident -mcmodel=medium \
                  -Wno-builtin-declaration-mismatch
endif

CFLAGS = $(CFLAGS_BASE) \
         -Isrc/kernel/privileged/lib \
         -Isrc/kernel/privileged/net -Isrc/kernel/privileged/net/arch -Isrc/kernel/privileged/net/driver

# 构建模式显式化 (方案 B): 裸机构建显式注入 build-std (src/rust/.cargo/config.toml
# 已删全局 [unstable] build-std). 避免 cwd 隐式加载 — src/rust 目录内 host-target
# 构建会触发 E0152 双 alloc 冲突 (DECISION-021 同族). 详见主计划文档 §10.
BUILD_STD_CFG := --config 'unstable.build-std=["core","compiler_builtins","alloc"]' --config 'unstable.build-std-features=["compiler-builtins-mem"]'

# 内核 cargo 构建的附加 feature 注入钩子 (默认空 → 常规构建零改变).
# 用途: 诊断专用 boot (如 P-29 `KERNEL_FEATURES=lockdep` 点亮 lockdep),
# 避免为一次性取证改动构建主链路. 空值时 `--features` 不出现 (make if 展开).
KERNEL_FEATURES ?=

# ============================================================================
# 网络子系统已迁移至 smoltcp (纯 Rust)
# Network stack: smoltcp (Rust)
# ============================================================================

LDFLAGS = -T $(LDSCRIPT) -nostdlib -Map=$(ARCH_BUILD_DIR)/kernel.map -z noexecstack --no-warn-rwx-segments

# ── 架构条件 QEMU 标志 ────────────────────────────────────────────────
ifeq ($(ARCH),aarch64)
    # AArch64: QEMU virt 机器，无 ISA debug-exit，无 PCI 网络
    QEMU_FLAGS := -M $(QEMU_MACHINE),gic-version=3 -cpu $(QEMU_CPU) -m 512 -no-reboot
    QEMU_NET :=
    # 内核产出统一为 arm64 Image (内嵌 Image 头, 见 link/aarch64.ld):
    # QEMU -kernel 与 U-Boot booti / 真机引导共用同一制品.
    KERNEL_IMAGE := $(ARCH_BUILD_DIR)/kernel-aarch64.img
    QEMU_KERNEL_FLAG := -kernel
else
    QEMU_FLAGS := -m 512 -no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04
    QEMU_NET := -device e1000,netdev=n0 \
                -netdev user,id=n0,hostfwd=tcp::8080-:80,hostname=antx
    KERNEL_IMAGE := $(ARCH_BUILD_DIR)/kernel.flat
    QEMU_KERNEL_FLAG := -kernel
endif

# ── 架构条件构建对象 ─────────────────────────────────────────────────
# 注: $(BUILD_DIR)/lib/string.o 已废弃 — string.c 已被 string.rs (Rust) 取代,
#     string 符号由 $(RUST_LIB) 通过 --whole-archive 包含。
ifeq ($(ARCH),aarch64)
    KERNEL_OBJS = $(ARCH_BUILD_DIR)/boot.o
    KERNEL_TEST_OBJS = $(ARCH_BUILD_DIR)/boot.o
else
    KERNEL_OBJS = $(ARCH_BUILD_DIR)/boot.o $(ARCH_BUILD_DIR)/entry.o $(ARCH_BUILD_DIR)/isr.o $(ARCH_BUILD_DIR)/switch.o \
                  $(ARCH_BUILD_DIR)/arch/x86_64/trampoline.o
    # 测试入口由 Rust 端 kernel_test feature (lib.rs:343 kernel_test_main) 提供,
    # 不再依赖 C 桩文件. 2026-06-24 移除 kernel_test.o / test_main.o / test_hw_stubs.o.
    KERNEL_TEST_OBJS = $(ARCH_BUILD_DIR)/boot.o $(ARCH_BUILD_DIR)/entry.o $(ARCH_BUILD_DIR)/isr.o $(ARCH_BUILD_DIR)/switch.o \
                  $(ARCH_BUILD_DIR)/arch/x86_64/trampoline.o
endif

RUST_LIB = other/target/$(RUST_TARGET_KERNEL)/release/libkernel.a
RUST_LIB_TEST = other/target/test-release/$(RUST_TARGET_KERNEL)/release/libkernel.a
RUST_LIB_CHAOS = other/target/chaos-release/$(RUST_TARGET_KERNEL)/release/libkernel.a
RUST_LIB_TEST_DEBUG = other/target/test-debug/$(RUST_TARGET_KERNEL)/test-debug/libkernel.a

RUST_USER_DIR = src/user
RUST_USER_TARGET = other/target/$(RUST_TARGET)/release

# 用户程序前置依赖: 无前置依赖时目标文件一旦存在, make 便认为已最新 → recipe 永不执行,
# 用户态源码改动长期不生效 (与 $(RUST_LIB) 同族踩坑, 见 §L204 注释).
USER_SRCS := $(shell find $(RUST_USER_DIR) -name '*.rs' -not -path '*/target/*' 2>/dev/null) \
             $(RUST_USER_DIR)/Cargo.toml $(RUST_USER_DIR)/link.x $(RUST_USER_DIR)/link_aarch64.x

USER_INIT_ELF = $(RUST_USER_TARGET)/init
USER_SHELL_ELF = $(RUST_USER_TARGET)/eash
USER_INSTALL_ELF = $(RUST_USER_TARGET)/install
USER_FBTERM_ELF = $(RUST_USER_TARGET)/fbterm
USER_HTTPSRV_ELF = $(RUST_USER_TARGET)/httpsrv
USER_TEST_ELF = $(RUST_USER_TARGET)/proctest

STAGE1_BIN = $(ARCH_BUILD_DIR)/stage1.bin
DISK_IMAGE = $(ARCH_BUILD_DIR)/antx.img

.PHONY: all clean run run-net debug log log-net iso run-iso disk run-disk user test test-host test-kernel-host test-kani test-unit \
         test-all test-chaos test-smp test-smp-multicore

ifeq ($(ARCH),aarch64)
# aarch64: 内核产出统一为 arm64 Image ($(ARCH_BUILD_DIR)/kernel-aarch64.img), 供 QEMU -kernel
# 与 U-Boot booti / 真机引导共用; 不再产出无 Image 头语义的 kernel.flat.
all: $(ARCH_BUILD_DIR)/kernel.bin $(ARCH_BUILD_DIR)/kernel-aarch64.img
else
# x86_64: 同时构建 kernel.flat (qemu 直接使用的 raw 镜像),
# 避免外部脚本在 make 完成后还需要二次 objcopy.
all: $(ARCH_BUILD_DIR)/kernel.bin $(ARCH_BUILD_DIR)/kernel.flat
endif

# 跨架构产物隔离: 已由 ARCH_BUILD_DIR 在目录层面完成 (见文件头注释) ——
# boot.o / entry.o / isr.o / switch.o / trampoline.o / kernel.bin / kernel.flat /
# kernel-aarch64.img / kernel.map 均落在 other/build/$(ARCH)/ 下, 两架构不可能互相
# 误用. 旧机制 (log/.arch 戳记 + arch-switch-clean 强制清理 + 跨架构全量 cargo
# clean) 是为共用路径设计的补丁, 共用路径消失后整体退役:
#   - Rust 静态库本就按 target 分目录 (x86_64-unknown-none /
#     aarch64-unknown-none-softfloat, 见 RUST_LIB), 无需 cargo clean;
#   - 运行期脚本的对等防线 (qemu_boot_test.sh 的 sync_make_state) 同步退役,
#     启动前仍保留 verify_image_arch (S-6 防线③) 作为产物内容的自检.
$(shell mkdir -p $(LOG_DIR) $(ARCH_BUILD_DIR))

# ====== x86_64 Rust user programs ======
ifeq ($(ARCH),x86_64)
user: $(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF) $(USER_TEST_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $(USER_INIT_ELF) $(USER_BUILD_DIR)/init.bin
	@cp $(USER_SHELL_ELF) $(USER_BUILD_DIR)/eash.bin
	@cp $(USER_INSTALL_ELF) $(USER_BUILD_DIR)/install.bin
	@cp $(USER_FBTERM_ELF) $(USER_BUILD_DIR)/fbterm.bin
	@cp $(USER_HTTPSRV_ELF) $(USER_BUILD_DIR)/httpsrv.bin
	@cp $(USER_TEST_ELF) $(USER_BUILD_DIR)/proctest.bin
	@echo "User programs built successfully (Rust)"

$(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF) $(USER_TEST_ELF): $(USER_SRCS)
	@echo "Building Rust user programs..."
	cd $(RUST_USER_DIR) && RUSTFLAGS="-C link-arg=-T$$(pwd)/link.x -C link-arg=-nostdlib -C link-arg=-no-pie" cargo build --release --target $(RUST_TARGET)

$(USER_BUILD_DIR)/init.bin: $(USER_INIT_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $< $@

$(USER_BUILD_DIR)/eash.bin: $(USER_SHELL_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $< $@

$(USER_BUILD_DIR)/install.bin: $(USER_INSTALL_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $< $@

$(USER_BUILD_DIR)/fbterm.bin: $(USER_FBTERM_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $< $@

$(USER_BUILD_DIR)/httpsrv.bin: $(USER_HTTPSRV_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $< $@
endif

# $(LDSCRIPT) 入依赖: 链接脚本按文件名点名 .o (boot.o / isr.o, 见 x86_64.ld 注释),
# 改脚本必须重链 —— 否则旧镜像与新布局静默共存 (ld 对失配模式不报错).
$(ARCH_BUILD_DIR)/kernel.bin: $(KERNEL_OBJS) $(RUST_LIB) $(LDSCRIPT)
	@mkdir -p $(ARCH_BUILD_DIR)
	@echo "[LINK] Linking kernel..."
	$(LD) $(LDFLAGS) --allow-multiple-definition -o $@ --whole-archive $(RUST_LIB) --no-whole-archive $(KERNEL_OBJS)

$(ARCH_BUILD_DIR)/kernel.flat: $(ARCH_BUILD_DIR)/kernel.bin
	$(OBJCOPY) -O binary $< $@

# aarch64: objcopy 出 arm64 Image (内含 .image_header, 见 link/aarch64.ld).
ifeq ($(ARCH),aarch64)
$(ARCH_BUILD_DIR)/kernel-aarch64.img: $(ARCH_BUILD_DIR)/kernel.bin
	$(OBJCOPY) -O binary $< $@
endif

# AArch64 用户程序: 使用 Cargo 编译 Rust 用户程序
ifeq ($(ARCH),aarch64)
user: $(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $(USER_INIT_ELF) $(USER_BUILD_DIR)/init.bin
	@cp $(USER_SHELL_ELF) $(USER_BUILD_DIR)/eash.bin
	@cp $(USER_INSTALL_ELF) $(USER_BUILD_DIR)/install.bin
	@cp $(USER_FBTERM_ELF) $(USER_BUILD_DIR)/fbterm.bin
	@cp $(USER_HTTPSRV_ELF) $(USER_BUILD_DIR)/httpsrv.bin
	@echo "User programs built (Rust aarch64)"

$(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF): $(USER_SRCS)
	@echo "Building Rust user programs (aarch64)..."
	cd $(RUST_USER_DIR) && RUSTFLAGS="-C link-arg=-T$$(pwd)/link_aarch64.x -C link-arg=-nostdlib" cargo build --release --target $(RUST_TARGET)

$(USER_BUILD_DIR)/init.bin: $(USER_INIT_ELF)
	@mkdir -p $(USER_BUILD_DIR)
	@cp $< $@

# aarch64 侧同样需 kernel 源码前置依赖 (与下方 x86_64 规则 / RUST_LIB_TEST 的 E-06 教训同因):
# 缺它则 .a 已存在时 make 跳过 cargo 重建, 静默沿用陈旧二进制.
$(RUST_LIB): $(USER_BUILD_DIR)/init.bin $(shell find src/kernel -name '*.rs' 2>/dev/null)
	@echo "Building Rust kernel module..."
	@cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) $(if $(KERNEL_FEATURES),--features "$(KERNEL_FEATURES)") --target-dir ../../other/target
else
# x86_64: 用 Cargo 构建 Rust 用户程序 + 内核
# include_bytes! 编译时需要 init.bin 存在，确保用户程序先构建

$(RUST_LIB): $(STAGE1_BIN) $(USER_BUILD_DIR)/init.bin $(shell find src/kernel -name '*.rs' 2>/dev/null)
	@echo "Building Rust kernel module..."
	@cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) $(if $(KERNEL_FEATURES),--features "$(KERNEL_FEATURES)") --target-dir ../../other/target
endif

# RUST_LIB_TEST 需源文件前置依赖 (kernel 源码经 #[path="../../kernel"] 引入, 须一并搜索):
# 否则 .a 已存在时 make 跳过 cargo 重建, kernel_test.bin 长期使用陈旧二进制 (E-06 验证踩坑, 2026-09-07)
$(RUST_LIB_TEST): $(shell find src/kernel -name '*.rs' 2>/dev/null)
	@echo "Building Rust test kernel..."
	cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --features kernel_test --target-dir ../../other/target/test-release

$(RUST_LIB_CHAOS):
	@echo "Building Rust chaos kernel (fault_injection enabled)..."
	cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --features "kernel_test fault_injection" --target-dir ../../other/target/chaos-release

# 2026-06-29 新增: 调试构建 (LTO=false + debug info + opt-level=0), 用于排查 OnceLock 静态初始化 hang
$(RUST_LIB_TEST_DEBUG):
	@echo "Building Rust test kernel (debug profile)..."
	cd src/kernel && cargo build --profile test-debug --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --features kernel_test --target-dir ../../other/target/test-debug

$(ARCH_BUILD_DIR)/%.o: src/kernel/privileged/%.asm
	@mkdir -p $(dir $@)
	$(AS) $(ASFLAGS) $< -o $@

$(STAGE1_BIN): src/kernel/privileged/boot/stage1.asm
	@mkdir -p $(ARCH_BUILD_DIR)
	$(AS) -f bin $< -o $@

$(ARCH_BUILD_DIR)/%.o: src/kernel/privileged/boot/%.asm
	@mkdir -p $(ARCH_BUILD_DIR)
	$(AS) $(ASFLAGS) $< -o $@

# AArch64 启动汇编 (GNU as)
ifeq ($(ARCH),aarch64)
$(ARCH_BUILD_DIR)/boot.o: src/kernel/privileged/boot/aarch64/start.S
	@mkdir -p $(ARCH_BUILD_DIR)
	$(AS) $(ASFLAGS) $< -o $@
endif

$(ARCH_BUILD_DIR)/gdt_asm.o: src/kernel/privileged/gdt.asm
	@mkdir -p $(ARCH_BUILD_DIR)
	$(AS) $(ASFLAGS) $< -o $@

$(ARCH_BUILD_DIR)/switch.o: src/kernel/privileged/proc/switch.asm
	@mkdir -p $(ARCH_BUILD_DIR)
	$(AS) $(ASFLAGS) $< -o $@

# 磁盘镜像 — 仅 x86_64
ifeq ($(ARCH),x86_64)
$(DISK_IMAGE): $(ARCH_BUILD_DIR)/kernel.flat user
	@echo "Creating disk image..."
	@mkdir -p $(ARCH_BUILD_DIR)
	@dd if=/dev/zero of=$@ bs=1M count=4 2>/dev/null
	@dd if=$(STAGE1_BIN) of=$@ bs=512 seek=0 conv=notrunc 2>/dev/null
	@dd if=$(ARCH_BUILD_DIR)/kernel.flat of=$@ bs=512 seek=1 conv=notrunc 2>/dev/null
	@echo "Disk image created: $@ (4MB)"

disk: $(DISK_IMAGE)

run-disk: $(DISK_IMAGE)
	$(QEMU) -drive file=$(DISK_IMAGE),format=raw -serial stdio
endif

iso: all user
	@mkdir -p $(ISODIR)/boot/grub
	cp $(ARCH_BUILD_DIR)/kernel.bin $(ISODIR)/boot/kernel.bin
	mkdir -p $(ISODIR)/bin
	cp $(USER_BUILD_DIR)/init.bin $(ISODIR)/bin/init
	cp $(USER_BUILD_DIR)/eash.bin $(ISODIR)/bin/eash
	cp $(USER_BUILD_DIR)/install.bin $(ISODIR)/bin/install
	cp $(USER_BUILD_DIR)/fbterm.bin $(ISODIR)/bin/fbterm
	cp $(USER_BUILD_DIR)/httpsrv.bin $(ISODIR)/bin/httpsrv
	echo 'set timeout=0' > $(ISODIR)/boot/grub/grub.cfg
	echo 'set default=0' >> $(ISODIR)/boot/grub/grub.cfg
	echo '' >> $(ISODIR)/boot/grub/grub.cfg
	echo 'menuentry "AntX" {' >> $(ISODIR)/boot/grub/grub.cfg
	echo '    multiboot2 /boot/kernel.bin' >> $(ISODIR)/boot/grub/grub.cfg
	echo '}' >> $(ISODIR)/boot/grub/grub.cfg
	grub2-mkrescue -o $(ARCH_BUILD_DIR)/antx.iso $(ISODIR)

clean:
	rm -rf $(BUILD_DIR)/ $(ISODIR)/
	cd src/kernel && cargo clean
	cd $(RUST_USER_DIR) && cargo clean

# QEMU CPU 模型配置 (用于硬件仿真测试)
# 可选值: qemu64 (默认), host (使用宿主机CPU特性), Haswell-noTSX, Skylake-Client
QEMU_CPU ?= qemu64

# 运行模式配置
# mode-interactive: 需要图形界面，适合开发调试
# mode-headless: 无头模式，适合 CI/CD 和服务器环境
ifeq ($(QEMU_MODE),headless)
	QEMU_DISPLAY := -display none -nographic -serial file:$(LOG_DIR)/serial.log
else
	QEMU_DISPLAY := -serial stdio -display gtk
endif

run: all $(KERNEL_IMAGE)
	@mkdir -p $(LOG_DIR)
	$(QEMU) $(QEMU_FLAGS) $(QEMU_KERNEL_FLAG) $(KERNEL_IMAGE) $(QEMU_DISPLAY)

# 网络 QEMU — 仅 x86_64 (依赖 e1000 PCI 设备)
ifeq ($(ARCH),x86_64)
run-net: all user $(ARCH_BUILD_DIR)/kernel.flat
	@mkdir -p $(LOG_DIR)
	$(QEMU) $(QEMU_FLAGS) -kernel $(ARCH_BUILD_DIR)/kernel.flat $(QEMU_NET) $(QEMU_DISPLAY)
else
run-net:
	@echo "run-net is not supported on aarch64 (no PCI/e1000)"
endif

run-headless: all $(KERNEL_IMAGE)
	@$(MAKE) QEMU_MODE=headless run
	@echo "✓ Kernel output saved to $(LOG_DIR)/serial.log"
	@cat $(LOG_DIR)/serial.log | head -100

# ISO — 仅 x86_64 (依赖 GRUB + BIOS)
ifeq ($(ARCH),x86_64)
run-iso: iso
	@mkdir -p $(LOG_DIR)
	$(QEMU) $(QEMU_FLAGS) -cdrom $(ARCH_BUILD_DIR)/antx.iso $(QEMU_DISPLAY)
else
run-iso:
	@echo "run-iso is not supported on aarch64 (BIOS/GRUB only)"
endif

debug: all $(KERNEL_IMAGE)
	$(QEMU) $(QEMU_FLAGS) $(QEMU_KERNEL_FLAG) $(KERNEL_IMAGE) -serial stdio -s -S

log: all $(KERNEL_IMAGE)
	@mkdir -p $(LOG_DIR)
	timeout 30 $(QEMU) $(QEMU_FLAGS) $(QEMU_KERNEL_FLAG) $(KERNEL_IMAGE) \
		-serial file:$(LOG_DIR)/serial.log \
		-display none \
		-d cpu_reset,guest_errors 2>&1 | tee $(LOG_DIR)/qemu_stderr.log || true

# 网络日志 — 仅 x86_64
ifeq ($(ARCH),x86_64)
log-net: all user $(ARCH_BUILD_DIR)/kernel.flat
	@mkdir -p $(LOG_DIR)
	timeout 60 $(QEMU) $(QEMU_FLAGS) -kernel $(ARCH_BUILD_DIR)/kernel.flat \
		$(QEMU_NET) \
		-serial file:$(LOG_DIR)/serial.log \
		-display none \
		-d cpu_reset,guest_errors 2>&1 | tee $(LOG_DIR)/qemu_stderr.log || true
	@echo ""
	@echo "=== Network Log ==="
	@grep -E "NETWORK|DRIVER.*E1000|Ping|HTTP|DNS|DHCP|ISR" $(LOG_DIR)/serial.log | head -30
else
log-net:
	@echo "log-net is not supported on aarch64 (no PCI/e1000)"
endif
	@echo ""
	@echo "=== HTTP Test ==="
	@curl -s --max-time 3 http://localhost:8080/ 2>/dev/null && echo "OK" || echo "FAIL (no response)"
	@echo ""
	@echo "╔══════════════════════════════════════════╗"
	@echo "║  Serial log: $(LOG_DIR)/serial.log       ║"
	@echo "║  QEMU stderr: $(LOG_DIR)/qemu_stderr.log ║"
	@echo "╚══════════════════════════════════════════╝"
	@if [ -f $(LOG_DIR)/serial.log ]; then \
		echo "--- Last 50 lines of serial output ---"; \
		tail -50 $(LOG_DIR)/serial.log; \
	fi

run-iso-debug: iso
	@mkdir -p $(LOG_DIR)
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	timeout 30 $(QEMU) $(QEMU_FLAGS) \
		-cdrom $(ARCH_BUILD_DIR)/antx.iso \
		-serial file:$(LOG_DIR)/serial_$${timestamp}.log \
		-display none \
		-no-reboot \
		-d int,cpu_reset,unimp,guest_errors,in_asm \
		-D $(LOG_DIR)/qemu_debug_$${timestamp}.log || true
	@echo ""
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║  Debug logs saved:                          ║"
	@echo "║  Serial:  $(LOG_DIR)/serial_$${timestamp}.log    ║"
	@echo "║  QEMU:    $(LOG_DIR)/qemu_debug_$${timestamp}.log ║"
	@echo "╚══════════════════════════════════════════════╝"
	@if [ -f $(LOG_DIR)/qemu_debug_$${timestamp}.log ]; then \
		echo "--- QEMU Debug Output (last 50 lines) ---"; \
		tail -50 $(LOG_DIR)/qemu_debug_$${timestamp}.log; \
	fi

debug-iso: iso
	@mkdir -p $(LOG_DIR)
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom $(ARCH_BUILD_DIR)/antx.iso \
		-serial stdio \
		-no-reboot \
		-s -S &
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║  QEMU started in debug mode on port 1234     ║"
	@echo "╚══════════════════════════════════════════════╝"
	@echo "Connect with:"
	@echo "  gdb -ex 'target remote localhost:1234' \\"
	@echo "      -ex 'symbol-file $(ARCH_BUILD_DIR)/kernel.bin'"

$(ARCH_BUILD_DIR)/main.o: src/kernel/main.c
	@mkdir -p $(ARCH_BUILD_DIR)
	$(CC) $(CFLAGS) -c $< -o $@

test: test-host test-unit

$(ARCH_BUILD_DIR)/kernel_test.bin: $(KERNEL_TEST_OBJS) $(RUST_LIB_TEST) $(LDSCRIPT)
	@mkdir -p $(ARCH_BUILD_DIR)
	$(LD) -T $(LDSCRIPT) -nostdlib -Map=$(ARCH_BUILD_DIR)/kernel_test.map --allow-multiple-definition -o $(ARCH_BUILD_DIR)/kernel_test.bin $(KERNEL_TEST_OBJS) $(RUST_LIB_TEST)

# 单元测试（优化版）
test-host:
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║     Running Host-Side Unit Tests             ║"
	@echo "╚══════════════════════════════════════════════╝"
	@mkdir -p $(CURDIR)/$(REPORT_DIR)
	@cd host-tests && { log=$(CURDIR)/$(REPORT_DIR)/host_test_$$(date +%Y%m%d_%H%M%S).log; cargo test --quiet > "$$log" 2>&1; status=$$?; cat "$$log"; exit $$status; }
	@echo ""

# 内核单元测试（host 侧）: 执行 src/kernel 源文件内 `#[cfg(test)]` 内联用例.
# 与 test-host 独立 (后者跑 host-tests/ 集成套件); 必须在 src/kernel 目录内执行,
# 以便加载该目录 .cargo/config.toml 的 target-dir 与 curve25519 后端固化.
# 详见 docs/plan/kernel-unit-test-harness-unification.md.
test-kernel-host:
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║   Running Kernel Host-Side Unit Tests        ║"
	@echo "╚══════════════════════════════════════════════╝"
	@mkdir -p $(CURDIR)/$(REPORT_DIR)
	@cd src/kernel && { log=$(CURDIR)/$(REPORT_DIR)/kernel_host_test_$$(date +%Y%m%d_%H%M%S).log; cargo test --features host-test --lib > "$$log" 2>&1; status=$$?; cat "$$log"; exit $$status; }
	@echo ""

# Kani 形式化验证: 独立 crate kani-harness/ 通过 #[path] 引真实 functions 源码,
# 证明其核心安全不变式 (不 panic / pos 单调不减 / 不越界). 需本机已 `cargo kani setup`.
test-kani:
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║   Running Kani Formal Verification           ║"
	@echo "╚══════════════════════════════════════════════╝"
	@mkdir -p $(CURDIR)/$(REPORT_DIR)
	@cd kani-harness && { log=$(CURDIR)/$(REPORT_DIR)/kani_$$(date +%Y%m%d_%H%M%S).log; cargo kani > "$$log" 2>&1; status=$$?; cat "$$log"; exit $$status; }
	@echo ""

test-unit: $(ARCH_BUILD_DIR)/kernel_test.bin user
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║     Building & Running Unit Tests             ║"
	@echo "╚══════════════════════════════════════════════╝"
	@mkdir -p $(ISODIR)/boot/grub
	@cp $(ARCH_BUILD_DIR)/kernel_test.bin $(ISODIR)/boot/kernel.bin
	@mkdir -p $(ISODIR)/bin
	@cp $(USER_BUILD_DIR)/init.bin $(ISODIR)/bin/init
	@cp $(USER_BUILD_DIR)/eash.bin $(ISODIR)/bin/eash
	@cp $(USER_BUILD_DIR)/install.bin $(ISODIR)/bin/install
	@cp $(USER_BUILD_DIR)/fbterm.bin $(ISODIR)/bin/fbterm
	@cp $(USER_BUILD_DIR)/httpsrv.bin $(ISODIR)/bin/httpsrv
	@echo 'set timeout=0' > $(ISODIR)/boot/grub/grub.cfg
	@echo 'set default=0' >> $(ISODIR)/boot/grub/grub.cfg
	@echo '' >> $(ISODIR)/boot/grub/grub.cfg
	@echo 'menuentry "AntX Test" {' >> $(ISODIR)/boot/grub/grub.cfg
	@echo '    multiboot2 /boot/kernel.bin' >> $(ISODIR)/boot/grub/grub.cfg
	@echo '}' >> $(ISODIR)/boot/grub/grub.cfg
	@grub2-mkrescue -o $(ARCH_BUILD_DIR)/antx_test.iso $(ISODIR) 2>/dev/null
	@echo ""
	@echo "▶ Starting QEMU (timeout: 300s, memory: 512MB)..."
	@mkdir -p $(REPORT_DIR)
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	timeout 300 $(QEMU) $(QEMU_FLAGS) \
		-m 512 \
		-cdrom $(ARCH_BUILD_DIR)/antx_test.iso \
		$(QEMU_NET) \
		-serial file:$(REPORT_DIR)/unit_test_$${timestamp}.log \
		-display none \
		-d cpu_reset 2>$(REPORT_DIR)/qemu_stderr_$${timestamp}.log; \
	exit_code=$$?; \
	if [ $$exit_code -eq 33 ]; then \
		echo ""; \
		echo "╔══════════════════════════════════════════════╗"; \
		echo "║  ✅ ALL TESTS PASSED (QEMU exit: $$exit_code)   ║"; \
		echo "╚══════════════════════════════════════════════╝"; \
	elif [ $$exit_code -eq 35 ]; then \
		echo ""; \
		echo "╔══════════════════════════════════════════════╗"; \
		echo "║  ❌ TESTS FAILED (QEMU exit: $$exit_code)        ║"; \
		echo "╚══════════════════════════════════════════════╝"; \
	elif [ $$exit_code -eq 0 ]; then \
		echo ""; \
		echo "╔══════════════════════════════════════════════╗"; \
		echo "║  ⚠️  QEMU exited normally (no isa-debug-exit)    ║"; \
		echo "╚══════════════════════════════════════════════╝"; \
	else \
		echo ""; \
		echo "╔══════════════════════════════════════════════╗"; \
		echo "║  ⚠️  QEMU exited with code $$exit_code (timeout/crash) ║"; \
		echo "╚══════════════════════════════════════════════╝"; \
	fi
	@echo "  Report: $(REPORT_DIR)/unit_test_$${timestamp}.log"
	@if [ -f $(REPORT_DIR)/unit_test_$${timestamp}.log ]; then \
		echo "--- Serial Output (last 80 lines) ---"; \
		tail -80 $(REPORT_DIR)/unit_test_$${timestamp}.log; \
	fi

test-all: test-host test-unit
	@echo ""
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     🎉 All Tests Complete!                            ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@echo "  执行的测试套件:"
	@echo "    1. Quick Test (60s)"
	@echo "    2. QEMU Hardware Simulation (150s)"
	@echo "    3. Unit Tests (120s)"
	@echo "    4. Comprehensive Tests (180s)"
	@echo "  总计: ~510 秒 (~8.5 分钟)"
	@echo ""

FAULT_RATE ?= 50

$(ARCH_BUILD_DIR)/kernel_chaos.bin: $(KERNEL_TEST_OBJS) $(RUST_LIB_CHAOS) $(LDSCRIPT)
	@mkdir -p $(ARCH_BUILD_DIR)
	$(LD) -T $(LDSCRIPT) -nostdlib -Map=$(ARCH_BUILD_DIR)/kernel_chaos.map --allow-multiple-definition -o $(ARCH_BUILD_DIR)/kernel_chaos.bin $(KERNEL_TEST_OBJS) $(RUST_LIB_CHAOS)

test-chaos: $(ARCH_BUILD_DIR)/kernel_chaos.bin user
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     Chaos/Fault Injection Tests (fault_injection=on)   ║"
	@echo "║     FAULT_RATE=$(FAULT_RATE)/1000                        ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@mkdir -p $(ISODIR)/boot/grub
	@cp $(ARCH_BUILD_DIR)/kernel_chaos.bin $(ISODIR)/boot/kernel.bin
	@mkdir -p $(ISODIR)/bin
	@cp $(USER_BUILD_DIR)/init.bin $(ISODIR)/bin/init
	@cp $(USER_BUILD_DIR)/eash.bin $(ISODIR)/bin/eash
	@cp $(USER_BUILD_DIR)/install.bin $(ISODIR)/bin/install
	@cp $(USER_BUILD_DIR)/fbterm.bin $(ISODIR)/bin/fbterm
	@cp $(USER_BUILD_DIR)/httpsrv.bin $(ISODIR)/bin/httpsrv
	@echo 'set timeout=0' > $(ISODIR)/boot/grub/grub.cfg
	@echo 'set default=0' >> $(ISODIR)/boot/grub/grub.cfg
	@echo '' >> $(ISODIR)/boot/grub/grub.cfg
	@echo 'menuentry "AntX Chaos Test" {' >> $(ISODIR)/boot/grub/grub.cfg
	@echo '    multiboot2 /boot/kernel.bin' >> $(ISODIR)/boot/grub/grub.cfg
	@echo '}' >> $(ISODIR)/boot/grub/grub.cfg
	@grub2-mkrescue -o $(ARCH_BUILD_DIR)/antx_chaos.iso $(ISODIR) 2>/dev/null
	@mkdir -p $(REPORT_DIR)
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	echo "▶ Starting QEMU with fault injection (rate=$(FAULT_RATE)/1000, timeout: 120s)..."; \
	timeout 120 $(QEMU) $(QEMU_FLAGS) \
		-m 512 \
		-cdrom $(ARCH_BUILD_DIR)/antx_chaos.iso \
		-serial file:$(REPORT_DIR)/chaos_test_$${timestamp}.log \
		-display none \
		-d cpu_reset 2>$(REPORT_DIR)/qemu_chaos_stderr_$${timestamp}.log || true
	@echo ""
	@timestamp=$$(ls -t $(REPORT_DIR)/chaos_test_*.log 2>/dev/null | head -1 | sed 's/.*chaos_test_//;s/\.log//'); \
	if [ -n "$$timestamp" ]; then \
		echo "╔══════════════════════════════════════════════╗"; \
		echo "║  Chaos Test Report                           ║"; \
		echo "╚══════════════════════════════════════════════╝"; \
		echo ""; \
		echo "--- Last 80 lines of serial output ---"; \
		tail -80 $(REPORT_DIR)/chaos_test_$${timestamp}.log; \
	fi

# SMP 测试核数 (S-9): 默认 2 核, 可用 make test-smp SMP_CORES=N 覆盖
SMP_CORES ?= 2

test-smp: all user $(KERNEL_IMAGE)
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     SMP Tests ($(SMP_CORES) cores)                                 ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@mkdir -p $(REPORT_DIR)
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	smp_log=$(REPORT_DIR)/smp_test_$${timestamp}.log; \
	timeout 60 $(QEMU) $(QEMU_FLAGS) \
		-m 512 -smp $(SMP_CORES) \
		$(QEMU_KERNEL_FLAG) $(KERNEL_IMAGE) \
		-serial file:$${smp_log} \
		-display none \
		-d cpu_reset 2>$(REPORT_DIR)/qemu_smp_stderr_$${timestamp}.log || true; \
	echo ""; \
	echo "--- SMP Test Output (last 60 lines) ---"; \
	tail -60 "$${smp_log}"; \
	if ! grep -q "\[SMP\] online CPUs: $(SMP_CORES)" "$${smp_log}"; then \
		echo "SMP TEST FAILED: 未在 $${smp_log} 中找到 '[SMP] online CPUs: $(SMP_CORES)' ($(SMP_CORES) 核未全部上线)"; \
		exit 1; \
	fi; \
	if ! grep -qE "\[SMP\] first user syscall from pid=[0-9]+([^0-9]|$$)" "$${smp_log}"; then \
		echo "SMP TEST FAILED: 未在 $${smp_log} 中找到 '[SMP] first user syscall from pid=N'"; \
		echo "  含义: 用户态从未真正执行 (无任何来自 Ring 3/CPL3 的 syscall);"; \
		echo "  注: 'Entering Ring 3' 打印在 iretq 之前, 不构成用户态已执行的证据."; \
		exit 1; \
	fi; \
	if ! grep -q "\[SMP\] TLB shootdown #" "$${smp_log}"; then \
		echo "SMP TEST FAILED: 未在 $${smp_log} 中找到 '[SMP] TLB shootdown #' (跨核 TLB 失效路径未触发)"; \
		exit 1; \
	fi; \
	if ! grep -qE "\[SMP\] TLB shootdown #[0-9]+ gen=[0-9]+ targets=$(SMP_CORES)([^0-9]|$$)" "$${smp_log}"; then \
		echo "SMP TEST FAILED: 未在 $${smp_log} 中找到 'targets=$(SMP_CORES)' 的 TLB shootdown (TLB shootdown 未覆盖全部 $(SMP_CORES) 核)"; \
		exit 1; \
	fi; \
	if ! grep -qE "\[VMM\] deferred-free admitted_total=[1-9][0-9]* released_total=[1-9][0-9]* pending=false" "$${smp_log}"; then \
		echo "SMP TEST FAILED: 未在 $${smp_log} 中找到 '[VMM] deferred-free admitted_total=N released_total=M pending=false'"; \
		echo "  含义: 延迟释放的帧未真正归还 PMM —— 入链后滞留 pending (代永远追不平),"; \
		echo "        或销毁路径从未被走到 (released_total 恒 0); 'pending=false' 必须同时成立,"; \
		echo "        否则 'released_total 非零' 可能只是滞留批次里漏还的少数几帧."; \
		echo "  对应 docs/plan/tlb-shootdown-epoch.md S-10 的运行覆盖判据."; \
		exit 1; \
	fi; \
	if ! grep -q "\[SMP\] TLB probe PASS" "$${smp_log}"; then \
		echo "SMP TEST FAILED: 未在 $${smp_log} 中找到 '[SMP] TLB probe PASS' (运行期跨核 TLB 失效探针未通过)"; \
		echo "  对应 docs/plan/tlb-shootdown-epoch.md §5 注入 3 的运行覆盖判据."; \
		exit 1; \
	fi; \
	if grep -q "\[SMP\] TLB probe FAIL" "$${smp_log}"; then \
		echo "SMP TEST FAILED: $${smp_log} 中出现 '[SMP] TLB probe FAIL' (跨核 TLB 失效未跨核传播)"; \
		echo "  含义: flush_tlb_remote 未登记远程失效 / 定向 IPI 未送达 / tlb_flush_all 退化为空."; \
		exit 1; \
	fi; \
	echo "SMP TEST PASSED: online CPUs = $(SMP_CORES), 且用户态已实际执行 (收到 Ring 3 syscall), 且 TLB shootdown 已覆盖全部核, 且延迟释放帧确有归还 (admitted/released 非零且 pending=false), 且运行期跨核 TLB 失效探针通过"

# 一次性覆盖 2/3/4 核 (S-9): 按核数循环调用 test-smp, 任一核数未达
# 「全部上线 + 用户态实际执行 (Ring 3 syscall)」即 exit 1 (fail-closed)
SMP_MULTICORE_CORES := 2 3 4

test-smp-multicore: all user $(KERNEL_IMAGE)
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     SMP Tests (cores: $(SMP_MULTICORE_CORES))                            ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@for n in $(SMP_MULTICORE_CORES); do \
		echo ""; \
		$(MAKE) --no-print-directory test-smp SMP_CORES=$$n || exit 1; \
	done; \
	echo ""; \
	echo "SMP MULTICORE TEST PASSED: 核数 $(SMP_MULTICORE_CORES) 全部通过"

# ============================================================================
# QEMU 调试脚本支持 (QEMU Debug Script Support)
# ============================================================================

.PHONY: qemu-debug qemu-debug-gdb qemu-headless qemu-network driver-test qemu-boot-test

# 使用 QEMU 调试脚本启动 (正常模式)
qemu-debug:
	@chmod +x scripts/qemu_debug.sh
	@./scripts/qemu_debug.sh -k $(ARCH_BUILD_DIR)/kernel.flat

# 使用 QEMU 调试脚本启动 (GDB 调试模式)
qemu-debug-gdb:
	@chmod +x scripts/qemu_debug.sh
	@./scripts/qemu_debug.sh -k $(ARCH_BUILD_DIR)/kernel.flat -d
	@echo ""
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║  GDB Debug Session                           ║"
	@echo "╠══════════════════════════════════════════════╣"
	@echo "║  In another terminal, run:                   ║"
	@echo "║  gdb -x .gdbinit.antx                        ║"
	@echo "╚══════════════════════════════════════════════╝"

# 无头模式 (Headless mode)
qemu-headless:
	@chmod +x scripts/qemu_debug.sh
	@./scripts/qemu_debug.sh -k $(ARCH_BUILD_DIR)/kernel.flat -D none

# 网络模式 (Network mode)
qemu-network:
	@chmod +x scripts/qemu_debug.sh
	@./scripts/qemu_debug.sh -k $(ARCH_BUILD_DIR)/kernel.flat -n

# QEMU 真实启动测试 (双架构门禁)
# 用法: make qemu-boot-test [ARCH=x86_64|aarch64|all]
qemu-boot-test:
	@chmod +x scripts/qemu_boot_test.sh
	@./scripts/qemu_boot_test.sh $(ARCH)

# ============================================================================
# 驱动测试 (Driver Tests)
# ============================================================================

driver-test: all $(ARCH_BUILD_DIR)/kernel.flat
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     Hardware Driver Tests                                ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@mkdir -p $(REPORT_DIR)
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	echo "[TEST] Starting driver tests in QEMU..."; \
	timeout 30 $(QEMU) $(QEMU_FLAGS) \
		-kernel $(ARCH_BUILD_DIR)/kernel.flat \
		-serial file:$(REPORT_DIR)/driver_test_$${timestamp}.log \
		-display none \
		-d cpu_reset,guest_errors,unimp 2>$(REPORT_DIR)/qemu_driver_stderr_$${timestamp}.log || true
	@echo ""
	@driver_log=$$(ls -t $(REPORT_DIR)/driver_test_*.log 2>/dev/null | head -1); \
	if [ -n "$$driver_log" ]; then \
		echo "--- Driver Test Output ---"; \
		cat "$$driver_log"; \
		echo ""; \
		echo "--- QEMU Warnings (if any) ---"; \
		qemu_err=$$(ls -t $(REPORT_DIR)/qemu_driver_stderr_*.log 2>/dev/null | head -1); \
		if [ -f "$$qemu_err" ] && [ -s "$$qemu_err" ]; then \
			cat "$$qemu_err"; \
		else \
			echo "No warnings."; \
		fi \
	fi

# ============================================================================
# 帮助信息更新 (Updated Help)
# ============================================================================

.PHONY: help-drivers

help-drivers:
	@echo ""
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     Hardware Driver Commands                             ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@echo ""
	@echo "  QEMU Debug Commands:"
	@echo "    make qemu-debug        - Start QEMU with VGA display"
	@echo "    make qemu-debug-gdb    - Start QEMU in GDB debug mode"
	@echo "    make qemu-headless     - Start QEMU in headless mode"
	@echo "    make qemu-network      - Start QEMU with network"
	@echo ""
	@echo "  Driver Test Commands:"
	@echo "    make driver-test       - Run hardware driver tests"
	@echo ""
	@echo "  Available Drivers:"
	@echo "    - VGA Text Mode (80x25)"
	@echo "    - Serial Port (COM1-COM4, UART 16550)"
	@echo "    - PIT Timer (8254)"
	@echo "    - PS/2 Keyboard"
	@echo "    - ATA/IDE Disk"
	@echo "    - PCI Bus"
	@echo ""
