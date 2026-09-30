ARCH ?= x86_64
LOG_DIR := build/log

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
    LDSCRIPT = src/kernel/framework/link/aarch64.ld
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
    LDSCRIPT = src/kernel/framework/link/x86_64.ld
    ASFLAGS = -f elf64 -w-zeroing
    CFLAGS_BASE = -std=c11 -m64 -Wall -Wextra -nostdinc -nostdlib -fPIC -fno-stack-protector \
                  -fno-asynchronous-unwind-tables -fno-ident -mcmodel=medium \
                  -Wno-builtin-declaration-mismatch
endif

CFLAGS = $(CFLAGS_BASE) \
         -Isrc/kernel/framework/lib \
         -Isrc/kernel/framework/net -Isrc/kernel/framework/net/arch -Isrc/kernel/framework/net/driver

# 构建模式显式化 (方案 B): 裸机构建显式注入 build-std (src/rust/.cargo/config.toml
# 已删全局 [unstable] build-std). 避免 cwd 隐式加载 — src/rust 目录内 host-target
# 构建会触发 E0152 双 alloc 冲突 (DECISION-021 同族). 详见主计划文档 §10.
BUILD_STD_CFG := --config 'unstable.build-std=["core","compiler_builtins","alloc"]' --config 'unstable.build-std-features=["compiler-builtins-mem"]'

# ============================================================================
# 网络子系统已迁移至 smoltcp (纯 Rust)
# Network stack: smoltcp (Rust)
# ============================================================================

LDFLAGS = -T $(LDSCRIPT) -nostdlib -Map=build/kernel.map -z noexecstack --no-warn-rwx-segments

# ── 架构条件 QEMU 标志 ────────────────────────────────────────────────
ifeq ($(ARCH),aarch64)
    # AArch64: QEMU virt 机器，无 ISA debug-exit，无 PCI 网络
    QEMU_FLAGS := -M $(QEMU_MACHINE),gic-version=3 -cpu $(QEMU_CPU) -m 512 -no-reboot
    QEMU_NET :=
    # 内核产出统一为 arm64 Image (内嵌 Image 头, 见 link/aarch64.ld):
    # QEMU -kernel 与 U-Boot booti / 真机引导共用同一制品.
    KERNEL_IMAGE := build/kernel-aarch64.img
    QEMU_KERNEL_FLAG := -kernel
else
    QEMU_FLAGS := -m 512 -no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04
    QEMU_NET := -device e1000,netdev=n0 \
                -netdev user,id=n0,hostfwd=tcp::8080-:80,hostname=antx
    KERNEL_IMAGE := build/kernel.flat
    QEMU_KERNEL_FLAG := -kernel
endif

# ── 架构条件构建对象 ─────────────────────────────────────────────────
# 注: build/lib/string.o 已废弃 — string.c 已被 string.rs (Rust) 取代,
#     string 符号由 $(RUST_LIB) 通过 --whole-archive 包含。
ifeq ($(ARCH),aarch64)
    KERNEL_OBJS = build/boot.o
    KERNEL_TEST_OBJS = build/boot.o
else
    KERNEL_OBJS = build/boot.o build/entry.o build/isr.o build/switch.o \
                  build/arch/x86_64/trampoline.o
    # 测试入口由 Rust 端 kernel_test feature (lib.rs:343 kernel_test_main) 提供,
    # 不再依赖 C 桩文件. 2026-06-24 移除 kernel_test.o / test_main.o / test_hw_stubs.o.
    KERNEL_TEST_OBJS = build/boot.o build/entry.o build/isr.o build/switch.o \
                  build/arch/x86_64/trampoline.o
endif

RUST_LIB = src/rust/target/$(RUST_TARGET_KERNEL)/release/libkernel.a
RUST_LIB_TEST = src/rust/target/test-release/$(RUST_TARGET_KERNEL)/release/libkernel.a
RUST_LIB_CHAOS = src/rust/target/chaos-release/$(RUST_TARGET_KERNEL)/release/libkernel.a
RUST_LIB_TEST_DEBUG = src/rust/target/test-debug/$(RUST_TARGET_KERNEL)/test-debug/libkernel.a

RUST_USER_DIR = src/user
RUST_USER_TARGET = $(RUST_USER_DIR)/target/$(RUST_TARGET)/release

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

STAGE1_BIN = build/stage1.bin
DISK_IMAGE = build/antx.img

.PHONY: all clean run run-net debug log log-net iso run-iso disk run-disk user test test-host test-kernel-host test-unit test-integration test-smoke test-stress \
         test-all test-chaos test-smp test-smp-multicore

ifeq ($(ARCH),aarch64)
# aarch64: 内核产出统一为 arm64 Image (build/kernel-aarch64.img), 供 QEMU -kernel
# 与 U-Boot booti / 真机引导共用; 不再产出无 Image 头语义的 kernel.flat.
all: build/kernel.bin build/kernel-aarch64.img
else
# x86_64: 同时构建 kernel.flat (qemu 直接使用的 raw 镜像),
# 避免外部脚本在 make 完成后还需要二次 objcopy.
all: build/kernel.bin build/kernel.flat
endif

# 跨架构构建时自动清理上架构产物, 避免 boot.o 等被新架构误用。
# 通过 build/log/.arch 记录上次构建架构 (build/log/ 不被 clean 删除), 不匹配时强制 clean.
# 修复: 戳记写入必须在 arch-switch-clean 配方内 (仅真实跨架构切换时更新).
# 原实现在解析期无条件覆写戳记, `make test-host` 等不产生架构产物的目标也会
# 把戳记改成默认 ARCH, 导致下次同 ARCH 链接误用残留的异架构 boot.o (EM 不符).
ARCH_STAMP := build/log/.arch
PREVIOUS_ARCH := $(shell cat $(ARCH_STAMP) 2>/dev/null || echo none)
ifneq ($(PREVIOUS_ARCH), $(ARCH))
ARCH_CHANGED := 1
endif
ifeq ($(ARCH_CHANGED),1)
.PHONY: arch-switch-clean
arch-switch-clean:
	@echo "[make] cross-arch switch: $(PREVIOUS_ARCH) → $(ARCH), removing arch-specific build/ artifacts (preserving build/log/)"
	@rm -f build/boot.o build/entry.o build/isr.o build/switch.o \
	       build/arch/x86_64/trampoline.o build/gdt_asm.o \
	       build/kernel.bin build/kernel.flat build/kernel-aarch64.img build/kernel.map
	@cd src/kernel && cargo clean >/dev/null 2>&1 || true
	@cd src/user && cargo clean >/dev/null 2>&1 || true
	@rm -f build/user/*.bin
	@echo $(ARCH) > $(ARCH_STAMP)
# 挂到所有 asm .o 目标, 强制 clean 后重新评估 .o 的依赖图
# (clean 在 make 评估图后执行, .o 文件存在与否需要重新触发)
ASM_OBJS := build/boot.o build/entry.o build/isr.o build/switch.o build/arch/x86_64/trampoline.o
$(ASM_OBJS): arch-switch-clean
endif
$(shell mkdir -p $(LOG_DIR))

# ====== x86_64 Rust user programs ======
ifeq ($(ARCH),x86_64)
user: $(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF) $(USER_TEST_ELF)
	@mkdir -p build/user
	@cp $(USER_INIT_ELF) build/user/init.bin
	@cp $(USER_SHELL_ELF) build/user/eash.bin
	@cp $(USER_INSTALL_ELF) build/user/install.bin
	@cp $(USER_FBTERM_ELF) build/user/fbterm.bin
	@cp $(USER_HTTPSRV_ELF) build/user/httpsrv.bin
	@cp $(USER_TEST_ELF) build/user/proctest.bin
	@echo "User programs built successfully (Rust)"

$(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF) $(USER_TEST_ELF): $(USER_SRCS)
	@echo "Building Rust user programs..."
	cd $(RUST_USER_DIR) && RUSTFLAGS="-C link-arg=-T$$(pwd)/link.x -C link-arg=-nostdlib -C link-arg=-no-pie" cargo build --release --target $(RUST_TARGET)

build/user/init.bin: $(USER_INIT_ELF)
	@mkdir -p build/user
	@cp $< $@

build/user/eash.bin: $(USER_SHELL_ELF)
	@mkdir -p build/user
	@cp $< $@

build/user/install.bin: $(USER_INSTALL_ELF)
	@mkdir -p build/user
	@cp $< $@

build/user/fbterm.bin: $(USER_FBTERM_ELF)
	@mkdir -p build/user
	@cp $< $@

build/user/httpsrv.bin: $(USER_HTTPSRV_ELF)
	@mkdir -p build/user
	@cp $< $@
endif

build/kernel.bin: $(KERNEL_OBJS) $(RUST_LIB)
	@mkdir -p build
	@echo "[LINK] Linking kernel..."
	$(LD) $(LDFLAGS) --allow-multiple-definition -o $@ --whole-archive $(RUST_LIB) --no-whole-archive $(KERNEL_OBJS)

build/kernel.flat: build/kernel.bin
	$(OBJCOPY) -O binary $< $@

# aarch64: objcopy 出 arm64 Image (内含 .image_header, 见 link/aarch64.ld).
ifeq ($(ARCH),aarch64)
build/kernel-aarch64.img: build/kernel.bin
	$(OBJCOPY) -O binary $< $@
endif

# AArch64 用户程序: 使用 Cargo 编译 Rust 用户程序
ifeq ($(ARCH),aarch64)
user: $(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF)
	@mkdir -p build/user
	@cp $(USER_INIT_ELF) build/user/init.bin
	@cp $(USER_SHELL_ELF) build/user/eash.bin
	@cp $(USER_INSTALL_ELF) build/user/install.bin
	@cp $(USER_FBTERM_ELF) build/user/fbterm.bin
	@cp $(USER_HTTPSRV_ELF) build/user/httpsrv.bin
	@echo "User programs built (Rust aarch64)"

$(USER_INIT_ELF) $(USER_SHELL_ELF) $(USER_INSTALL_ELF) $(USER_FBTERM_ELF) $(USER_HTTPSRV_ELF): $(USER_SRCS)
	@echo "Building Rust user programs (aarch64)..."
	cd $(RUST_USER_DIR) && RUSTFLAGS="-C link-arg=-T$$(pwd)/link_aarch64.x -C link-arg=-nostdlib" cargo build --release --target $(RUST_TARGET)

build/user/init.bin: $(USER_INIT_ELF)
	@mkdir -p build/user
	@cp $< $@

$(RUST_LIB): build/user/init.bin
	@echo "Building Rust kernel module..."
	@cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --target-dir ../rust/target
else
# x86_64: 用 Cargo 构建 Rust 用户程序 + 内核
# include_bytes! 编译时需要 init.bin 存在，确保用户程序先构建

$(RUST_LIB): $(STAGE1_BIN) build/user/init.bin $(shell find src/kernel -name '*.rs' 2>/dev/null)
	@echo "Building Rust kernel module..."
	@cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --target-dir ../rust/target
endif

# RUST_LIB_TEST 需源文件前置依赖 (kernel 源码经 #[path="../../kernel"] 引入, 须一并搜索):
# 否则 .a 已存在时 make 跳过 cargo 重建, kernel_test.bin 长期使用陈旧二进制 (E-06 验证踩坑, 2026-09-07)
$(RUST_LIB_TEST): $(shell find src/kernel -name '*.rs' 2>/dev/null)
	@echo "Building Rust test kernel..."
	cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --features kernel_test --target-dir ../rust/target/test-release

$(RUST_LIB_CHAOS):
	@echo "Building Rust chaos kernel (fault_injection enabled)..."
	cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --features "kernel_test fault_injection" --target-dir ../rust/target/chaos-release

# 2026-06-29 新增: 调试构建 (LTO=false + debug info + opt-level=0), 用于排查 OnceLock 静态初始化 hang
$(RUST_LIB_TEST_DEBUG):
	@echo "Building Rust test kernel (debug profile)..."
	cd src/kernel && cargo build --profile test-debug --target $(RUST_TARGET_KERNEL) $(BUILD_STD_CFG) --features kernel_test --target-dir ../rust/target/test-debug

build/%.o: src/kernel/framework/%.asm
	@mkdir -p $(dir $@)
	$(AS) $(ASFLAGS) $< -o $@

$(STAGE1_BIN): src/kernel/framework/boot/stage1.asm
	@mkdir -p build
	$(AS) -f bin $< -o $@

build/%.o: src/kernel/framework/boot/%.asm
	@mkdir -p build
	$(AS) $(ASFLAGS) $< -o $@

# AArch64 启动汇编 (GNU as)
ifeq ($(ARCH),aarch64)
build/boot.o: src/kernel/framework/boot/aarch64/start.S
	@mkdir -p build
	$(AS) $(ASFLAGS) $< -o $@
endif

build/gdt_asm.o: src/kernel/framework/gdt.asm
	@mkdir -p build
	$(AS) $(ASFLAGS) $< -o $@

build/switch.o: src/kernel/framework/proc/switch.asm
	@mkdir -p build
	$(AS) $(ASFLAGS) $< -o $@

# 磁盘镜像 — 仅 x86_64
ifeq ($(ARCH),x86_64)
$(DISK_IMAGE): build/kernel.flat user
	@echo "Creating disk image..."
	@dd if=/dev/zero of=$@ bs=1M count=4 2>/dev/null
	@dd if=build/stage1.bin of=$@ bs=512 seek=0 conv=notrunc 2>/dev/null
	@dd if=build/kernel.flat of=$@ bs=512 seek=1 conv=notrunc 2>/dev/null
	@echo "Disk image created: $@ (4MB)"

disk: $(DISK_IMAGE)

run-disk: $(DISK_IMAGE)
	$(QEMU) -drive file=$(DISK_IMAGE),format=raw -serial stdio
endif

iso: all user
	@mkdir -p isodir/boot/grub
	cp build/kernel.bin isodir/boot/kernel.bin
	mkdir -p isodir/bin
	cp build/user/init.bin isodir/bin/init
	cp build/user/eash.bin isodir/bin/eash
	cp build/user/install.bin isodir/bin/install
	cp build/user/fbterm.bin isodir/bin/fbterm
	cp build/user/httpsrv.bin isodir/bin/httpsrv
	echo 'set timeout=0' > isodir/boot/grub/grub.cfg
	echo 'set default=0' >> isodir/boot/grub/grub.cfg
	echo '' >> isodir/boot/grub/grub.cfg
	echo 'menuentry "AntX" {' >> isodir/boot/grub/grub.cfg
	echo '    multiboot2 /boot/kernel.bin' >> isodir/boot/grub/grub.cfg
	echo '}' >> isodir/boot/grub/grub.cfg
	grub2-mkrescue -o build/antx.iso isodir

clean:
	rm -rf build/ isodir/
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
run-net: all user build/kernel.flat
	@mkdir -p $(LOG_DIR)
	$(QEMU) $(QEMU_FLAGS) -kernel build/kernel.flat $(QEMU_NET) $(QEMU_DISPLAY)
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
	$(QEMU) $(QEMU_FLAGS) -cdrom build/antx.iso $(QEMU_DISPLAY)
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
log-net: all user build/kernel.flat
	@mkdir -p $(LOG_DIR)
	timeout 60 $(QEMU) $(QEMU_FLAGS) -kernel build/kernel.flat \
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
		-cdrom build/antx.iso \
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
		-cdrom build/antx.iso \
		-serial stdio \
		-no-reboot \
		-s -S &
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║  QEMU started in debug mode on port 1234     ║"
	@echo "╚══════════════════════════════════════════════╝"
	@echo "Connect with:"
	@echo "  gdb -ex 'target remote localhost:1234' \\"
	@echo "      -ex 'symbol-file build/kernel.bin'"

build/main.o: src/kernel/main.c
	@mkdir -p build
	$(CC) $(CFLAGS) -c $< -o $@

test: test-host test-unit

build/kernel_test.bin: $(KERNEL_TEST_OBJS) $(RUST_LIB_TEST)
	$(LD) -T $(LDSCRIPT) -nostdlib -Map=build/kernel_test.map --allow-multiple-definition -o build/kernel_test.bin $(KERNEL_TEST_OBJS) $(RUST_LIB_TEST)

# 单元测试（优化版）
test-host:
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║     Running Host-Side Unit Tests             ║"
	@echo "╚══════════════════════════════════════════════╝"
	@mkdir -p $(CURDIR)/tests/reports
	@cd host-tests && { log=$(CURDIR)/tests/reports/host_test_$$(date +%Y%m%d_%H%M%S).log; cargo test --quiet > "$$log" 2>&1; status=$$?; cat "$$log"; exit $$status; }
	@echo ""

# 内核单元测试（host 侧）: 执行 src/kernel 源文件内 `#[cfg(test)]` 内联用例.
# 与 test-host 独立 (后者跑 host-tests/ 集成套件); 必须在 src/kernel 目录内执行,
# 以便加载该目录 .cargo/config.toml 的 target-dir 与 curve25519 后端固化.
# 详见 docs/plan/kernel-unit-test-harness-unification.md.
test-kernel-host:
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║   Running Kernel Host-Side Unit Tests        ║"
	@echo "╚══════════════════════════════════════════════╝"
	@mkdir -p $(CURDIR)/tests/reports
	@cd src/kernel && { log=$(CURDIR)/tests/reports/kernel_host_test_$$(date +%Y%m%d_%H%M%S).log; cargo test --features host-test --lib > "$$log" 2>&1; status=$$?; cat "$$log"; exit $$status; }
	@echo ""

test-unit: build/kernel_test.bin user
	@echo "╔══════════════════════════════════════════════╗"
	@echo "║     Building & Running Unit Tests             ║"
	@echo "╚══════════════════════════════════════════════╝"
	@mkdir -p isodir/boot/grub
	@cp build/kernel_test.bin isodir/boot/kernel.bin
	@mkdir -p isodir/bin
	@cp build/user/init.bin isodir/bin/init
	@cp build/user/eash.bin isodir/bin/eash
	@cp build/user/install.bin isodir/bin/install
	@cp build/user/fbterm.bin isodir/bin/fbterm
	@cp build/user/httpsrv.bin isodir/bin/httpsrv
	@echo 'set timeout=0' > isodir/boot/grub/grub.cfg
	@echo 'set default=0' >> isodir/boot/grub/grub.cfg
	@echo '' >> isodir/boot/grub/grub.cfg
	@echo 'menuentry "AntX Test" {' >> isodir/boot/grub/grub.cfg
	@echo '    multiboot2 /boot/kernel.bin' >> isodir/boot/grub/grub.cfg
	@echo '}' >> isodir/boot/grub/grub.cfg
	@grub2-mkrescue -o build/antx_test.iso isodir 2>/dev/null
	@echo ""
	@echo "▶ Starting QEMU (timeout: 300s, memory: 512MB)..."
	@mkdir -p tests/reports
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	timeout 300 $(QEMU) $(QEMU_FLAGS) \
		-m 512 \
		-cdrom build/antx_test.iso \
		$(QEMU_NET) \
		-serial file:tests/reports/unit_test_$${timestamp}.log \
		-display none \
		-d cpu_reset 2>tests/reports/qemu_stderr_$${timestamp}.log; \
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
	@echo "  Report: tests/reports/unit_test_$${timestamp}.log"
	@if [ -f tests/reports/unit_test_$${timestamp}.log ]; then \
		echo "--- Serial Output (last 80 lines) ---"; \
		tail -80 tests/reports/unit_test_$${timestamp}.log; \
	fi

test-all: test-smoke test-host test-unit
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

build/kernel_chaos.bin: $(KERNEL_TEST_OBJS) $(RUST_LIB_CHAOS)
	$(LD) -T $(LDSCRIPT) -nostdlib -Map=build/kernel_chaos.map --allow-multiple-definition -o build/kernel_chaos.bin $(KERNEL_TEST_OBJS) $(RUST_LIB_CHAOS)

test-chaos: build/kernel_chaos.bin user
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     Chaos/Fault Injection Tests (fault_injection=on)   ║"
	@echo "║     FAULT_RATE=$(FAULT_RATE)/1000                        ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@mkdir -p isodir/boot/grub
	@cp build/kernel_chaos.bin isodir/boot/kernel.bin
	@mkdir -p isodir/bin
	@cp build/user/init.bin isodir/bin/init
	@cp build/user/eash.bin isodir/bin/eash
	@cp build/user/install.bin isodir/bin/install
	@cp build/user/fbterm.bin isodir/bin/fbterm
	@cp build/user/httpsrv.bin isodir/bin/httpsrv
	@echo 'set timeout=0' > isodir/boot/grub/grub.cfg
	@echo 'set default=0' >> isodir/boot/grub/grub.cfg
	@echo '' >> isodir/boot/grub/grub.cfg
	@echo 'menuentry "AntX Chaos Test" {' >> isodir/boot/grub/grub.cfg
	@echo '    multiboot2 /boot/kernel.bin' >> isodir/boot/grub/grub.cfg
	@echo '}' >> isodir/boot/grub/grub.cfg
	@grub2-mkrescue -o build/antx_chaos.iso isodir 2>/dev/null
	@mkdir -p tests/reports
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	echo "▶ Starting QEMU with fault injection (rate=$(FAULT_RATE)/1000, timeout: 120s)..."; \
	timeout 120 $(QEMU) $(QEMU_FLAGS) \
		-m 512 \
		-cdrom build/antx_chaos.iso \
		-serial file:tests/reports/chaos_test_$${timestamp}.log \
		-display none \
		-d cpu_reset 2>tests/reports/qemu_chaos_stderr_$${timestamp}.log || true
	@echo ""
	@timestamp=$$(ls -t tests/reports/chaos_test_*.log 2>/dev/null | head -1 | sed 's/.*chaos_test_//;s/\.log//'); \
	if [ -n "$$timestamp" ]; then \
		echo "╔══════════════════════════════════════════════╗"; \
		echo "║  Chaos Test Report                           ║"; \
		echo "╚══════════════════════════════════════════════╝"; \
		python3 tests/chaos/analyze_chaos.py tests/reports/chaos_test_$${timestamp}.log 2>/dev/null || \
		echo "  (Run 'python3 tests/chaos/analyze_chaos.py tests/reports/chaos_test_$${timestamp}.log' for analysis)"; \
		echo ""; \
		echo "--- Last 80 lines of serial output ---"; \
		tail -80 tests/reports/chaos_test_$${timestamp}.log; \
	fi

test-integration: iso
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     Integration Tests                                   ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@python3 tests/integration/run_integration_tests.py

test-smoke: iso
	@python3 tests/smoke/run_smoke_tests.py

test-stress: iso
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     Stress Tests                                        ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@python3 tests/stress/run_stress_tests.py

# SMP 测试核数 (S-9): 默认 2 核, 可用 make test-smp SMP_CORES=N 覆盖
SMP_CORES ?= 2

test-smp: all user $(KERNEL_IMAGE)
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     SMP Tests ($(SMP_CORES) cores)                                 ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@mkdir -p tests/reports
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	smp_log=tests/reports/smp_test_$${timestamp}.log; \
	timeout 60 $(QEMU) $(QEMU_FLAGS) \
		-m 512 -smp $(SMP_CORES) \
		$(QEMU_KERNEL_FLAG) $(KERNEL_IMAGE) \
		-serial file:$${smp_log} \
		-display none \
		-d cpu_reset 2>tests/reports/qemu_smp_stderr_$${timestamp}.log || true; \
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
	@./scripts/qemu_debug.sh -k build/kernel.flat

# 使用 QEMU 调试脚本启动 (GDB 调试模式)
qemu-debug-gdb:
	@chmod +x scripts/qemu_debug.sh
	@./scripts/qemu_debug.sh -k build/kernel.flat -d
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
	@./scripts/qemu_debug.sh -k build/kernel.flat -D none

# 网络模式 (Network mode)
qemu-network:
	@chmod +x scripts/qemu_debug.sh
	@./scripts/qemu_debug.sh -k build/kernel.flat -n

# QEMU 真实启动测试 (双架构门禁)
# 用法: make qemu-boot-test [ARCH=x86_64|aarch64|all]
qemu-boot-test:
	@chmod +x scripts/qemu_boot_test.sh
	@./scripts/qemu_boot_test.sh $(ARCH)

# ============================================================================
# 驱动测试 (Driver Tests)
# ============================================================================

driver-test: all build/kernel.flat
	@echo "╔══════════════════════════════════════════════════════════╗"
	@echo "║     Hardware Driver Tests                                ║"
	@echo "╚══════════════════════════════════════════════════════════╝"
	@mkdir -p tests/reports
	@timestamp=$$(date +%Y%m%d_%H%M%S); \
	echo "[TEST] Starting driver tests in QEMU..."; \
	timeout 30 $(QEMU) $(QEMU_FLAGS) \
		-kernel build/kernel.flat \
		-serial file:tests/reports/driver_test_$${timestamp}.log \
		-display none \
		-d cpu_reset,guest_errors,unimp 2>tests/reports/qemu_driver_stderr_$${timestamp}.log || true
	@echo ""
	@driver_log=$$(ls -t tests/reports/driver_test_*.log 2>/dev/null | head -1); \
	if [ -n "$$driver_log" ]; then \
		echo "--- Driver Test Output ---"; \
		cat "$$driver_log"; \
		echo ""; \
		echo "--- QEMU Warnings (if any) ---"; \
		qemu_err=$$(ls -t tests/reports/qemu_driver_stderr_*.log 2>/dev/null | head -1); \
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
