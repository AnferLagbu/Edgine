# Edgine 真机引导验证指南

> 本文档给需要把 Edgine 内核放到**真实硬件**上跑起来的维护者：如何制作双架构引导介质、如何接串口、在内核日志中看到哪些里程碑才算启动成功，以及 aarch64 侧对 SoC 的硬性契约与移植边界。配套 [guide-dev.md](./guide-dev.md)（代码归属与变更流程）与 [explain-framekernel.md](./explain-framekernel.md)（架构与安全不变式）阅读。适用读者：在实体 PC / aarch64 开发板上验证内核构建产物的开发者。

## 适用范围

本指南覆盖从"构建产物"到"真机串口看到里程碑"的完整链路：

- 用 [make_boot_medium.sh](../../scripts/make_boot_medium.sh) 把 `make` 产出的内核制品打包为可写 USB 的引导介质（x86_64 GRUB2 ISO / aarch64 整盘 FAT32 镜像）；
- 双架构的串口连接参数与启动日志里程碑；
- aarch64 对 SoC 的引导契约（设备树、异常级、内存映射）与**不支持项**清单。

不涵盖：日常开发构建流程（见 [guide-dev.md](./guide-dev.md)）；QEMU 自动化验证（见 [qemu_boot_test.sh](../../scripts/qemu_boot_test.sh)，本指南只描述真机侧）。**真机验证不能替代 QEMU 回归**——QEMU 覆盖的是确定性的里程碑断言，真机覆盖的是真实固件 / 内存映射 / 时钟差异暴露的问题，两者互补。

## 介质制作

### 统一入口

两个架构共用同一脚本，产物落在 `other/build/boot/`：

```bash
./scripts/make_boot_medium.sh x86_64    # -> other/build/boot/edgine-x86_64.iso
./scripts/make_boot_medium.sh aarch64   # -> other/build/boot/edgine-aarch64.img
```

脚本对工具缺失**失败即停**（`require_cmd`），并打印对应 apt 包名。依赖工具随架构不同：

| 架构 | 必需工具 | 缺失时的安装包 |
|---|---|---|
| x86_64 | `grub2-mkrescue`、`xorriso` | `grub-pc-bin grub-common xorriso` |
| aarch64 | `sfdisk`、`mkfs.vfat`、`mcopy` | `util-linux` / `dosfstools` / `mtools` |

> aarch64 路径**不依赖 `mkimage`**（`u-boot-tools`）。它走 U-Boot distro boot 原生的 `extlinux/extlinux.conf`，因此只需一个能写 MBR + FAT32 的工具链即可产出可引导介质。需要 `boot.scr` 的板子见下文「手工 boot.scr」。

### x86_64：GRUB2 multiboot2 ISO

脚本执行 `make ARCH=x86_64 iso`，该目标把内核与用户态程序装入 `other/isodir/`，生成 GRUB 配置并以 `grub2-mkrescue` 打包为 `other/build/x86_64/edgine.iso`，脚本再拷贝为 `edgine-x86_64.iso`。GRUB 配置（由 [Makefile](../../Makefile) `iso` 目标生成）以 multiboot2 协议加载内核：

```
menuentry "Edgine" {
    multiboot2 /boot/kernel.bin
}
```

写入 USB 后，在目标机 BIOS/UEFI 启动菜单选择该 USB 设备即可。若要直接写入块设备：

```bash
sudo ./scripts/make_boot_medium.sh x86_64 --write /dev/sdX
```

`--write` 带四重破坏性写入护栏：必须是块设备、拒绝已挂载设备、拒绝承载当前根文件系统的设备、并要求依次输入设备路径与 `YES` 二次确认。

### aarch64：整盘 FAT32 + U-Boot

脚本执行 `make ARCH=aarch64 all`，产出 `other/build/aarch64/kernel-aarch64.img`，再把它打成一整块磁盘镜像。分区布局：

| 项 | 值 |
|---|---|
| 分区表 | MBR（`label: dos`） |
| 分区 1 | FAT32，起始扇区 2048（偏移 1 MiB），类型 `0x0c` |
| 盘大小 | 默认 128 MiB（`--size <N>M` 可调） |
| 卷标 | `EDGINE` |

分区内文件：

| 路径 | 内容 |
|---|---|
| `/Image` | arm64 Linux Image（含内嵌 Image 头，见下文） |
| `/extlinux/extlinux.conf` | U-Boot distro boot 引导配置（主路径） |
| `/boot.cmd` | 等价的手工 `booti` 脚本（供自定义 `bootcmd` 的板子参考） |

`extlinux.conf` 内容：

```
default edgine
timeout 30
label edgine
    menu label Edgine (aarch64)
    linux /Image
```

U-Boot 的 distro boot（`sysboot`）会自动在分区上扫描 `/extlinux/extlinux.conf`，按 `linux /Image` 找到 arm64 Image，据 Image 头 `text_offset` 把它放置到 DRAM 基址 + `text_offset`，随后经 `booti` 跳转。由于未在 `extlinux.conf` 中显式给出 `fdt` 行，`booti` 会使用 U-Boot 自身的控制设备树（`${fdtcontroladdr}`）——内核由此获得 DTB。

`aarch64.ld` 与 QEMU `-kernel`、U-Boot `booti` 共用**同一个 Image 契约**（见 [aarch64.ld](../../src/kernel/privileged/link/aarch64.ld)），因此真机介质与 QEMU 验证是单一来源，不存在两套启动约定漂移的问题：

```bash
sudo ./scripts/make_boot_medium.sh aarch64 --load-addr 0x40080000 --write /dev/sdX
```

> **使用前提**：目标板 U-Boot 需具备 MMC + FAT + distro boot（`sysboot` / `booti`）能力，且 DRAM 基址为 `0x40000000`。不满足时的处理见「aarch64 移植边界」。

## 串口 checklist

真机验证的第一步是接好串口。aarch64 的日志主要经 UART 输出，接错参数会看到乱码。

| 架构 | 串口 | 参数 |
|---|---|---|
| x86_64 | 主板 / 虚拟 COM 或 UEFI 串口 | 由固件 / GRUB 控制台决定；GRUB 控制台默认 `console` |
| aarch64 | PL011（`arm,pl011`） | **115200-8N1**（固定） |

aarch64 侧 PL011 波特率除数在 [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs) 中硬编码为 `UARTIBRD=13, UARTFBRD=0`，其前提是 **UARTCLK ≈ 24 MHz**（24000000 / (16 × 115200) ≈ 13.02）。若目标板 UART 时钟不是 24 MHz，串口会输出乱码或不输出——此时需按板的实际时钟重算除数。

接线要点：USB-TTL 适配器（3.3V 电平），适配器 TX → 板 RX、适配器 RX → 板 TX、GND 共地，按目标板手册确认串口头（aarch64 常见 3-pin 或 4-pin）。

### 观察点

aarch64 启动日志按以下顺序出现（前几行来自 [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs) 的 `uart::puts`）：

```
[BOOT] Edgine starting...
[BOOT] Setting up exception vectors...
[BOOT] Initializing GICv3...
[BOOT] Initializing timer...
[BOOT] Booting kernel...
```

随后进入统一内核初始化，直到出现子系统里程碑：`VFS ready` → `Entering EL0`（用户态 init 启动）。完整成功判据与 QEMU 一致：[qemu_boot_test.sh](../../scripts/qemu_boot_test.sh) 对 aarch64 断言 `VFS ready`、`nic: probed successfully (functions bridge)`、`Entering EL0` 与 KPTI 隔离断言。

x86_64 侧关键里程碑为 `VFS ready` → `e1000: 初始化完成` → `Entering Ring 3`（以及 KPTI 断言）。

**看不到 `[BOOT] Edgine starting...`** = 内核尚未执行到 UART 输出，问题在更早（固件装载 / Image 头 / 异常级 / MMU），不在内核逻辑。

## aarch64 SoC 契约与边界

### 引导入口与异常级

镜像文件偏移 0 处是 **arm64 Linux Image 头**（64 字节），由 [start.S](../../src/kernel/privileged/boot/aarch64/start.S) 的 `.image_header` 段定义、[aarch64.ld](../../src/kernel/privileged/link/aarch64.ld) 固定其位置。头的关键字段：文件偏移 56 处为幻数 `ARM\x64`（`0x644d5241`），`code0` 为 `b _start`，`text_offset = 0x80000`，`image_size` 由链接期符号 `_image_size` 填充。

因此：

- 装载地址 = `0x40000000` + `text_offset` `0x80000` = **`0x40080000`**。这里的 `0x40000000` 是内核假定的 DRAM 窗口起点，不要求目标板 DRAM 物理基址与之一致；但装载器必须把镜像放到 `0x40080000`，否则与链接地址不符（见下文"装载地址"）；
- 真实入口 `_start` 位于 **`0x40080040`**（Image 头之后 64 字节）。

`_start` 的**第一条指令**就把 **`x0`**（引导程序传入的设备树物理地址）存入低半区符号 `_fdt_addr`——这是 arm64 引导约定，U-Boot `booti` 与 QEMU `-kernel` 均经 `x0` 传递 DTB。

[start.S](../../src/kernel/privileged/boot/aarch64/start.S) 支持从 **EL3 / EL2 / EL1 任一异常级**进入：读 `CurrentEL` 后逐级下降（EL3 配 `SCR_EL3` → EL2 配 `HCR_EL2` → EL1），最终在 **EL1h** 建立引导页表（L0→L1→L2，覆盖低 2 GB：Device 低 1 GB + DRAM 1 GB），开启 MMU 后跳到高半区内核 VMA 的 `entry()`。这使内核既能跑在 QEMU（复位于 EL1）上，也能跑在常见从 EL2 或 EL3 入场的 SoC 上。

### 设备树探测

[dtb.rs](../../src/kernel/privileged/dtb.rs) 是**最小** FDT 解析器（遵循 Devicetree Spec v0.4，FDT 版本 17），启动期只提取三类硬件资源：

| 资源 | 设备树匹配 | 用途 |
|---|---|---|
| 物理内存 | `/memory` 的 `reg` | 记录 `memory_base` / `memory_size`（**仅记录**，不改动页表） |
| 串口 | `arm,pl011` | 覆盖 PL011 基址（`uart::set_base`） |
| 中断控制器 | `arm,gic-v3` | 覆盖 GICD / GICR 基址（`gic::set_bases`） |

应用条件（在 [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs) 的 `apply_fdt_overrides()` 中）：

- **DTB 物理地址**必须落在 DRAM 窗口 `[0x4000_0000, 0x8000_0000)`，且整棵树不越出该窗口，否则探测直接放弃；
- **UART / GIC 基址**必须 `< 0x4000_0000`（即落在 Device 窗口）才会被采纳，越界基址被忽略、沿用默认值。

任一条不满足（无 DTB、地址越界、解析失败、头部非法）都返回 `None`，内核**沿用内置的 QEMU virt 默认基址**（PL011 `0x0900_0000`、GICD `0x0800_0000`、GICR `0x080A_0000`），保证 QEMU 零回归。

> **探测日志的可见性差异**：`apply_fdt_overrides()` 返回 `Some` 时，启动日志经 `klog_ffi!` 输出；但此时 klog 尚未初始化，该行被**丢弃、不显示**。返回 `None` 时，走 `uart::puts`，会**显示** `[BOOT] DTB unavailable, using built-in QEMU virt defaults`。因此判断"DTB 是否被采纳"要看**是否缺失** `DTB unavailable` 那一行——不出现 `unavailable`，即说明 DTB 探测成功且已应用。

### 内存映射硬编码窗口

引导页表（[mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs) 与 [start.S](../../src/kernel/privileged/boot/aarch64/start.S) 4.3 节一致）在启动期只映射两个窗口：

| 窗口 | 虚拟 / 物理地址范围 | 属性 |
|---|---|---|
| Device | `[0x0000_0000, 0x4000_0000)` | Device-nGnRnE，2 MiB 块粒度 |
| DRAM | `[0x4000_0000, 0x8000_0000)` | Normal-WBWA，1 GB 块 |

高半区别名满足 `VA = PA + 0xFFFF_0000_0000_0000`，其 `VA[47:39]` 恒为 0，故 `TTBR0_EL1`（低半区恒等映射）与 `TTBR1_EL1`（高半区内核）**共用同一张 L0 表**，运行时同一物理页两种视角可达。

由此得到的移植约束：**内核装载与运行必须落在 DRAM 窗口内**，**所有外设（UART / GIC）必须落在 Device 窗口内**。超出这两个窗口的设备或内存，在当前引导页表下不可访问。这两个窗口按 QEMU virt 布局选定；真实 SoC 的 UART / GIC 通常位于 `0xF000_0000` 以上，直接落在窗口之外，需扩表（见下节瑞芯微可行性核查）。

## aarch64 移植边界

以下为当前实现**明确不支持**的场景，移植到不满足条件的 SoC 需要改动源码，不是配置项：

| 限制 | 说明 | 相关源码 |
|---|---|---|
| 仅 GICv3 | 中断控制器必须是 `arm,gic-v3`；GICv2 无支持路径 | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs) |
| 仅 PL011 UART | 串口必须是 `arm,pl011`（ARM PrimeCell）；其他 IP（如 8250 / `snps,dw-apb-uart`）需另写驱动 | [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs) |
| DRAM 基址固定 `0x40000000` | 装载地址 `0x40080000` 是 `text_offset` 与 DRAM 基址的组合产物；基址不同需改链接脚本、引导页表与装载地址 | [aarch64.ld](../../src/kernel/privileged/link/aarch64.ld) |
| DRAM 窗口上限 512 MiB | 引导页表只映射 `[0x4000_0000, 0x8000_0000)`，更大内存需扩表 | [mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs) |
| 多核依赖 PSCI 与 EL1 入场 | SMP 已实现（PSCI `CPU_ON` 逐核上线）；但 AP 入口只接受 EL1，若固件把 AP 投递到 EL2 则会被判为非法异常级而放弃该核 | [smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs) / [start.S](../../src/kernel/privileged/boot/aarch64/start.S) |
| 需具备 distro boot 能力的 U-Boot | 介质依赖 `sysboot` / `booti`；无 U-Boot 的裸板需自备装载器并保证经 `x0` 传入合规 DTB | [make_boot_medium.sh](../../scripts/make_boot_medium.sh) |

x86_64 侧无 DTB 契约，由 GRUB2 经 multiboot2 装载，真机仅需目标机支持 BIOS/UEFI 从 USB 或光驱引导。

## 瑞芯微卡片电脑可行性核查（RK3566 / RK3568 / RK3588）

本节回答一个具体问题：能否把当前 aarch64 镜像直接烧到瑞芯微卡片电脑（鲁班猫一类，RK3566 / RK3568 / RK3588）上真机启动。核查只读源码与设备树事实，不改代码；所有判定都给出可追溯的源码位置或外部设备树证据。

### 结论

**需移植（中等规模）**，既不是"能直接跑"，也不是"不可行"。三条必须先解的硬阻塞依次是：

1. **控制台串口**：内核只有 PL011 驱动，而瑞芯微三款 SoC 的调试串口都是 `snps,dw-apb-uart`（16550 兼容、32 位寄存器步长）；内置基址 `0x0900_0000` 在真机是未映射地址，第一批 `[BOOT]` 输出就会失败。
2. **地址空间窗口**：引导页表只映射低 1 GiB Device + `[0x4000_0000, 0x8000_0000)` DRAM，而瑞芯微 UART / GIC 位于 `0xF...`（4 GiB 附近）；同时 [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L186-L194) 会主动丢弃 ≥ `0x4000_0000` 的 UART / GIC 基址。
3. **装载与 DTB 落位**：链接地址固定 `0x40080000`，而瑞芯微 DRAM 自 `0x0` 起、低 2 MiB 被 loader/ATF 占用；镜像必须被显式放到 `0x40080000`，且 DTB 必须落在内核认可的 DRAM 窗口 `[0x4000_0000, 0x8000_0000)` 内，否则 [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L158-L170) 直接放弃探测。

中断控制器**不是**阻塞点：RK3566 / RK3568 / RK3588 的主线 Linux 设备树均声明 `arm,gic-v3`（GICv3，带 Redistributor 与 `ICC_*` 系统寄存器），与内核现有实现同代。任务前提里"瑞芯微 RK356x/RK3588 使用 GIC-400（GICv2）"的说法不成立——这是本节核查中唯一需要纠正的前提性事实。

### 逐项判定

| # | 项目 | 现状 | 真机要求 | 差距等级 | 证据 |
|---|---|---|---|---|---|
| 1 | 串口驱动 | 仅 PL011（`arm,pl011`）；基址默认 `0x0900_0000`；波特率除数写死 `IBRD=13, FBRD=0`（按 24 MHz 算） | 瑞芯微为 `snps,dw-apb-uart`（16550 兼容，寄存器 4 字节步长，`reg-shift=2`），实例基址如 RK3568 UART0 `0xfdd5_0000`、RK3588 调试口 UART2 `0xfeb5_0000`；UARTCLK 由板级时钟树决定 | **硬阻塞** | [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs#L1-L5) / [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs#L15-L16) / [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs#L97-L99)；DTB 仅识别 `arm,pl011`：[dtb.rs](../../src/kernel/privileged/dtb.rs#L412) |
| 2 | GIC 版本 | 完整 GICv3：GICD + GICR（RD/SGI frame，128 KiB 步长）、`GICR_TYPER` 亲和匹配、`ICC_*` 系统寄存器 | 三款 SoC 均为 GICv3：RK356x GICD `0xfd40_0000` / GICR `0xfd46_0000`（`0x80000`，4 帧）；RK3588 GICD `0xfe60_0000` / GICR `0xfe68_0000`（`0x100000`，8 帧） | **小**（架构匹配，仅基址与安全态配置） | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L25-L29) / [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L174-L184) / [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L373-L389)；DTB 仅识别 `arm,gic-v3`：[dtb.rs](../../src/kernel/privileged/dtb.rs#L419)；外部证据为主线 Linux DTS |
| 3 | 中断 / 定时器 | ARM Generic Timer，用 `CNTPCT_EL0` / `CNTFRQ_EL0` / `CNTP_TVAL_EL0` / `CNTP_CTL_EL0`；运行时读频率，仅在 `CNTFRQ=0` 时回退 62.5 MHz；定时器 PPI 固定 30 | 真机同样用 NS-EL1 物理定时器，INTID 30 是 GICv3 架构定义；Linux 设备树写作 `ppi 14`，经 GICv3 irq domain 的 +16 偏移即 30。`CNTFRQ` 由固件设置（常见 24 MHz） | **无** | [timer.rs](../../src/kernel/privileged/arch/aarch64/timer.rs#L14-L21) / [timer.rs](../../src/kernel/privileged/arch/aarch64/timer.rs#L100-L118) / [gic_logic.rs](../../src/kernel/privileged/arch/gic_logic.rs#L61) / [exception.rs](../../src/kernel/privileged/arch/aarch64/exception.rs#L756) |
| 4 | 引导链 | 文件偏移 0 为 arm64 Linux Image 头（`ARM\x64`、`text_offset=0x80000`、`flags=0`、`code0=b _start`）；`x0` 传 DTB；U-Boot `booti` 兼容；aarch64 无 UEFI / multiboot 路径 | 瑞芯微 U-Boot 走 `booti`；但 DRAM 自 `0x0` 起，`kernel_addr_r` 默认值不保证等于 `0x40080000`；DTB 经 `x0` 传入且必须落在内核窗口内 | **中** | [start.S](../../src/kernel/privileged/boot/aarch64/start.S#L22-L35) / [start.S](../../src/kernel/privileged/boot/aarch64/start.S#L40-L45) / [make_boot_medium.sh](../../scripts/make_boot_medium.sh#L57-L58) / [make_boot_medium.sh](../../scripts/make_boot_medium.sh#L203-L204) |
| 5 | 平台假设 | 低 1 GiB 全 Device、`0x4000_0000-0x8000_0000` 为 DRAM；PMM `RAM_BASE=0x4000_0000`；默认 512 MiB；DTB 与 UART/GIC 基址均受窗口过滤 | 外设位于 `0xF...`；DRAM 自 `0x0` 起；DTB 由 U-Boot 放在任意 DRAM 地址 | **硬阻塞** | [mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs#L111-L120) / [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L17-L21) / [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L158-L170) / [pmm.rs](../../src/kernel/privileged/mm/pmm.rs#L53-L59) / [boot/mod.rs](../../src/kernel/privileged/boot/mod.rs#L390-L392) |
| 6 | PSCI | conduit 从设备树 `/psci` 的 `method` 读取，缺失时回退 SMC；`CPU_ON` 用 SMC64 函数 ID | 瑞芯微三款均为 `method="smc"`（ATF 驻 EL3） | **无**（前提是 DTB 能被解析，见第 5 项） | [psci.rs](../../src/kernel/privileged/arch/aarch64/psci.rs#L20-L27) / [psci.rs](../../src/kernel/privileged/arch/aarch64/psci.rs#L37-L66) / [dtb.rs](../../src/kernel/privileged/dtb.rs#L362-L365) |
| 7 | 多核上限与亲和 | `MAX_CPUS=1024`；DTB 侧 `CPU_MPIDR_MAX=8`；拓扑取自 `/cpus/cpu@N/reg` 首个条目；`affinity_pack` 打包 Aff3..Aff0 四级亲和；Redistributor 按 `GICR_TYPER` 亲和匹配而非线性索引；PSCI `CPU_ON` 用原始 MPIDR；AP 入口只接受 EL1 | RK3588 为 8 核、跨 3 个 cluster（4×A55 + 2+2 A76），MPIDR 亲和值跨簇；RK356x 为 4 核 | **小**（8 核恰好覆盖，无余量；跨簇理论可行，需实测） | [capacity.rs](../../src/kernel/privileged/config/capacity.rs#L20) / [dtb.rs](../../src/kernel/privileged/dtb.rs#L65) / [dtb.rs](../../src/kernel/privileged/dtb.rs#L384) / [mod.rs](../../src/kernel/privileged/arch/aarch64/mod.rs#L48-L54) / [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L174-L184) / [smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L199) / [start.S](../../src/kernel/privileged/boot/aarch64/start.S#L250-L255) |
| 8 | 既有真机痕迹 | 仓库内无任何 `.dts` / `.dtsi` / `.dtb`，无板级配置、无"真机"计划条目；与真机相关的仅有 Image 头、`booti` 介质脚本与本篇文档 | — | —（检索结论） | `Glob **/*.{dts,dtsi,dtb}` 无命中；`docs/` 检索 `RK3566|RK3568|RK3588|rockchip|瑞芯微|鲁班猫|real hardware` 无命中 |

### 绑定 QEMU virt 的硬编码清单

下表把"只在 QEMU virt 成立"的地址与常量按文件行号列出。移植时每一项都要重新审视；标"默认值"的项可被设备树覆盖，但覆盖目前受窗口过滤限制。

| 常量 / 地址 | 值 | 位置 | 说明 |
|---|---|---|---|
| PL011 基址默认值 | `0x0900_0000` | [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs#L16) | 可被 DTB 覆盖；只认 `arm,pl011` |
| PL011 波特率除数 | `IBRD=13` / `FBRD=0` | [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs#L97-L99) | 隐含 UARTCLK=24 MHz |
| GICD 基址默认值 | `0x0800_0000` | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L25) | 可被 DTB 覆盖；只认 `arm,gic-v3` |
| GICR 基址默认值 | `0x080A_0000` | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L27) | 高半区别名为 `0xFFFF_0000_080A_0000` |
| GICR SGI frame | RD + `0x1_0000` | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L32-L49) | 与 GICv3 规范一致，真机同样成立 |
| Redistributor 步长 | `0x2_0000`（128 KiB） | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L38) | 与 RK356x（4 帧）/ RK3588（8 帧）吻合 |
| SPI 路由目标 | `GICD_IROUTER=0`（固定 CPU0） | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L691-L695) | QEMU 双核下可行的简化；真机需确认亲和值 0 对应 BSP |
| Device 窗口 | `[0x0, 0x4000_0000)`，2 MiB 块，Device-nGnRnE | [mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs#L111-L120) / [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L17) | 瑞芯微外设在 `0xF...`，当前不可达 |
| DRAM 窗口 | `[0x4000_0000, 0x8000_0000)`，1 GiB 块，Normal-WBWA | [mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs#L120) / [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L19-L21) | 仅映射 1 GiB；多出的 DRAM 不可用 |
| DTB 落位约束 | 必须在 `[0x4000_0000, 0x8000_0000)` 内 | [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L158-L170) | 瑞芯微 U-Boot 默认 `fdtcontroladdr` 未必落在该区间 |
| UART / GIC 基址过滤 | 必须 `< 0x4000_0000` | [entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L186-L194) | 瑞芯微基址全部被丢弃，回退 QEMU 默认值 |
| 链接 / 装载地址 | `0x40080000` | [aarch64.ld](../../src/kernel/privileged/link/aarch64.ld#L21) / [make_boot_medium.sh](../../scripts/make_boot_medium.sh#L57-L58) | 与 `text_offset=0x80000` + 假定 DRAM 基址 `0x40000000` 绑定 |
| 物理内存基址 | `RAM_BASE=0x4000_0000` | [pmm.rs](../../src/kernel/privileged/mm/pmm.rs#L53-L59) | 瑞芯微 DRAM 自 `0x0` 起；该常量决定 PMM 覆盖哪一段 |
| 默认 RAM 大小 | 512 MiB（可用 `AARCH64_MEM_MB` 覆盖） | [boot/mod.rs](../../src/kernel/privileged/boot/mod.rs#L390-L392) | 与窗口上限一致 |
| 定时器频率回退 | `62_500_000` | [timer.rs](../../src/kernel/privileged/arch/aarch64/timer.rs#L9) | 仅 `CNTFRQ=0` 时生效 |
| 定时器 PPI | 30 | [gic_logic.rs](../../src/kernel/privileged/arch/gic_logic.rs#L61) / [exception.rs](../../src/kernel/privileged/arch/aarch64/exception.rs#L756) | GICv3 架构值，真机成立 |
| DTB CPU 上限 | `CPU_MPIDR_MAX=8` | [dtb.rs](../../src/kernel/privileged/dtb.rs#L65) | RK3588 恰好 8 核，无余量 |
| QEMU 启动参数 | `-M virt,gic-version=3 -cpu max -m 512 -smp 2` | [qemu_boot_test.sh](../../scripts/qemu_boot_test.sh#L520) | 当前验证面的全部平台假设来源 |

### 移植工作量估算

按"能真机启动并看到完整里程碑日志"这一目标估算，量级为人日，不含购板、烧录和往返调试的等待时间。

| 组别 | 工作内容 | 量级 | 依据 | 是否必须先做 |
|---|---|---|---|---|
| 驱动 | 新增 `snps,dw-apb-uart`（16550 兼容）控制台驱动：THR/IER/FCR/LCR/MCR/LSR 偏移（4 字节步长）、波特率按实际 UARTCLK 计算、与现有 `uart::puts` 接口对齐 | 2–4 人日 | 现有驱动寄存器布局是 PL011（[uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs#L41-L48)），与 dw-apb 完全不兼容；无串口即无任何可观测性 | **是**（第一优先） |
| 平台适配 | 扩引导页表覆盖 `0xF...` MMIO（如 L1[3] 1 GiB Device 块或二级表）；放宽 `apply_fdt_overrides` 的 UART/GIC 基址过滤；放宽或迁移 DTB 落位约束；核对 `RAM_BASE` / 默认内存大小与板载 DRAM 实际区间 | 3–5 人日 | [mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs#L94-L120) 只建 L1[0]/L1[1]；[entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs#L186-L194) 主动过滤；[pmm.rs](../../src/kernel/privileged/mm/pmm.rs#L59) 假定 RAM 基址 | **是**（第二优先） |
| 引导 | 固定装载地址到 `0x40080000`（覆盖 U-Boot `kernel_addr_r` 或显式 `booti 0x40080000`）；控制 `fdtcontroladdr` / DTB 放置；准备板级启动介质 | 1–2 人日 | Image 头与链接地址已兼容 `booti`（[start.S](../../src/kernel/privileged/boot/aarch64/start.S#L22-L35)），剩余是装载地址与 DTB 地址的工程约束 | 是（可与平台适配并行） |
| 中断 / GIC 验证 | 补写 `ICC_SRE_EL1.SRE`（当前未写，见 [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs#L373-L389)）；确认 GICD_CTLR 的 `DS` / 安全态配置允许 NS 写 `ARE_NS` 与 Group1；验证多簇 GICR 扫描与 PPI 30 | 1–2 人日 | 架构已匹配，但真机安全态与 QEMU 不同，属"能跑起来之后"的调试项 | 否（但失败会阻断中断与定时器） |
| 多核 / PSCI 验证 | 8 核逐核上线、AP 实际入场 EL 检查、跨簇 SGI/重调度 | 1–2 人日 | [smp_init.rs](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L164-L218) 与 [start.S](../../src/kernel/privileged/boot/aarch64/start.S#L250-L255) 的 EL1-only 约束需要实测确认 | 否（单核可先跑通） |

合计约 **8–15 人日**。必须先做的是"串口驱动"与"MMU/DTB 窗口"两组：前者恢复可观测性，后者决定任何 `0xF...` 外设能否被访问；其余可在单核最小启动成功后再推进。

### 替代方案对比

目标若是"低成本拿到 aarch64 真机时序证据"，四个选项的前置改造量差异很大：

| 选项 | 串口 | GIC | 其他前置改造 | 总成本排序 |
|---|---|---|---|---|
| (a) 瑞芯微卡片电脑（RK3566/68/88） | 需新增 dw-apb-uart 驱动（2–4 人日） | GICv3，**匹配**，仅基址与安全态验证 | 扩 MMU 窗口、放宽 DTB 过滤、装载地址覆盖（5–8 人日） | 第 2（真机方案中最低） |
| (b) 树莓派 5（BCM2712） | PL011，**匹配**（需扩窗映射 `0x7d...`） | **GIC-400（GICv2）**，与现有仅 GICv3 的实现硬不匹配，需新写 GICv2 分发/CPU 接口路径 | VideoCore 固件启动链、扩窗、装载地址（合计更高） | 第 4（最高） |
| (c) 云 arm64 裸金属 | 通常经 BMC / serial-over-IP，无需改驱动 | 可能为 GICv3（需按实例确认） | 需新增 UEFI 引导链（EFI stub / 装载器）；无本项目 Image 头直载路径 | 第 3 |
| (d) 放弃真机，只做 TCG | 无 | 无 | 无 | 第 1（零改造，但拿不到真机时序证据） |

排序（改造量由低到高）：**(d) < (a) < (c) < (b)**。若必须拿真机时序且只买一块板，(a) 是唯一"中断控制器代际匹配"的选择；树莓派 5 的 GICv2 会引入一个全新中断控制器驱动，成本高于瑞芯微整条移植链。

### 不确定项（需实物或厂商文档确认）

以下结论无法仅凭仓库源码断定，必须在拿到板卡、原理图或 TRM 后确认：

- **调试串口实例与基址**：瑞芯微每款 SoC 有多个 UART，鲁班猫具体把哪一路引到 USB-C 桥、基址是 `0xfdd5_0000`（RK3568 UART0）还是 `0xfeb5_0000`（RK3588 UART2）一类，需板级 DTS / 原理图确认。
- **UARTCLK**：`24 MHz` 还是 `100 MHz` 决定波特率除数；瑞芯微 BSP U-Boot 常用 `1500000` 波特率，与内核默认 `115200-8N1` 不一致，需同时改驱动初始化参数。
- **GIC 安全态**：`ICC_SRE_EL1.SRE` 初值、`GICD_CTLR.DS`、ATF 是否把中断组交给 NS-EL1，决定 `ICC_*` 访问是否 UNDEF、能否写 `ARE_NS` 与 Group1。架构代际已确定是 GICv3，但这一层必须实测。
- **AP 实际入场 EL**：PSCI `CPU_ON` 由 EL1 发起时固件通常回到 EL1，但若固件把 AP 投递到 EL2，[start.S](../../src/kernel/privileged/boot/aarch64/start.S#L250-L255) 会放弃该核，表现为多核起不来。
- **DRAM 可用区间**：瑞芯微 DRAM 自 `0x0` 起且低 2 MiB 被 loader/ATF 占用；`[0x4000_0000, 0x8000_0000)` 是否全部为可用 DRAM、以及板载容量是否 ≥ 1 GiB，需实物确认（板载小于 1 GiB 时该窗口会越界）。
- **U-Boot 环境**：`kernel_addr_r` / `fdt_addr_r` / `fdtcontroladdr` 的默认值是否可覆盖、是否启用 distro boot、SD 与 SPI 启动优先级，需板级 U-Boot 环境确认。
- **RK3588 GIC IP 细节**：主线设备树给出 GICR 区域 `0x100000`（8 帧 × 128 KiB），与 8 核拓扑吻合；具体 IP 型号与 GICR 帧数仍建议用 TRM 复核。
- **USB-C 串口桥**：板载 USB-UART 桥的芯片型号与枚举方式（CH340 / CP210x / FTDI 等）需实物确认；这决定宿主机侧是否需要额外驱动。

## 手工 boot.scr（可选）

`extlinux/extlinux.conf` 是无外部工具依赖的主路径。若目标板 U-Boot 的 `bootcmd` 不走 distro boot 而期望 `boot.scr`，可用分区内已附带的 `/boot.cmd` 打包：

```bash
mkimage -A arm64 -O linux -T script -C none -n "Edgine" -d boot.cmd boot.scr
```

`boot.cmd` 内容（脚本生成）：

```
echo "Booting Edgine (aarch64) ..."
fatload mmc 0:1 ${AARCH64_LOAD_ADDR} Image
booti ${AARCH64_LOAD_ADDR} - ${fdtcontroladdr}
```

`AARCH64_LOAD_ADDR` 默认 `0x40080000`（与 Image 头 `text_offset` 一致，可用 `--load-addr` 覆盖）。`booti` 第三参数为 DTB 地址，此处用 U-Boot 控制设备树 `${fdtcontroladdr}`；若板子有专用 DTB，替换为对应地址或文件即可。该路径需要 `u-boot-tools`（提供 `mkimage`），属于可选增强，不影响主路径。

## 故障排查

| 症状 | 可能原因 | 排查方向 |
|---|---|---|
| 串口无任何输出 | 内核未执行到 UART；固件装载 / Image 头 / 异常级 / MMU 阶段失败 | 确认介质识别为 arm64 Image（`file` 报 `Linux kernel ARM64`）；确认 U-Boot `booti` 使用的装载地址与 `text_offset` 一致 |
| 串口乱码 | UARTCLK 与硬编码除数（假设 24 MHz）不符；或波特率 / 数据位不匹配 | 确认终端为 **115200-8N1**；核对板级 UARTCLK |
| 出现 `[BOOT] DTB unavailable ...` | DTB 未被采纳：`x0` 未传 DTB、DTB 地址越界、或解析失败 | 确认 U-Boot 传入控制设备树；确认 DTB 物理地址落在 `[0x4000_0000, 0x8000_0000)` |
| 卡在 `Initializing GICv3...` | 板非 GICv3，或 GIC 基址未被探测 / 越界 | 核对设备树 `interrupt-controller` 兼容串为 `arm,gic-v3`，基址在 Device 窗口内 |
| 启动到某里程碑后挂起 | 缺少该里程碑所需的设备 / 驱动路径 | 对照上文观察点定位到具体子系统 |
| x86_64 无引导 | USB 未按 UEFI/BIOS 模式引导，或 GRUB 未找到内核 | 在固件启动菜单选择正确模式；用 `file` 确认 ISO 为 bootable |

## 关联文档与源码

- [guide-dev.md](./guide-dev.md)：privileged / functions 代码归属与变更流程。
- [explain-framekernel.md](./explain-framekernel.md)：框内核架构与 6 条安全不变式（I1-I6）——真机路径同样受其约束。
- [make_boot_medium.sh](../../scripts/make_boot_medium.sh)：双架构引导介质制作脚本。
- [qemu_boot_test.sh](../../scripts/qemu_boot_test.sh)：QEMU 启动回归（真机验证的自动化对照）。
- `src/kernel/privileged/link/aarch64.ld`：arm64 Image 头与低半区引导区 / 高半区内核区的链接契约。
- `src/kernel/privileged/boot/aarch64/start.S`：Image 头、DTB 捕获、异常级降级、引导页表。
- `src/kernel/privileged/boot/aarch64/entry.rs`：DTB 应用与硬件基址覆盖、启动里程碑输出。
- `src/kernel/privileged/dtb.rs`：最小 FDT 解析器（memory / uart / gicv3）。
- `src/kernel/privileged/arch/aarch64/`：PL011 UART、GICv3、MMU、定时器的框架层实现。
