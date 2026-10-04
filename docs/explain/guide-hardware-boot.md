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

脚本执行 `make ARCH=x86_64 iso`，该目标把内核与用户态程序装入 `other/isodir/`，生成 GRUB 配置并以 `grub2-mkrescue` 打包为 `other/build/antx.iso`，脚本再拷贝为 `edgine-x86_64.iso`。GRUB 配置（由 [Makefile](../../Makefile) `iso` 目标生成）以 multiboot2 协议加载内核：

```
menuentry "AntX" {
    multiboot2 /boot/kernel.bin
}
```

写入 USB 后，在目标机 BIOS/UEFI 启动菜单选择该 USB 设备即可。若要直接写入块设备：

```bash
sudo ./scripts/make_boot_medium.sh x86_64 --write /dev/sdX
```

`--write` 带四重破坏性写入护栏：必须是块设备、拒绝已挂载设备、拒绝承载当前根文件系统的设备、并要求依次输入设备路径与 `YES` 二次确认。

### aarch64：整盘 FAT32 + U-Boot

脚本执行 `make ARCH=aarch64 all`，产出 `other/build/kernel-aarch64.img`，再把它打成一整块磁盘镜像。分区布局：

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

- 装载地址 = DRAM 基址 `0x40000000` + `text_offset` `0x80000` = **`0x40080000`**；
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

由此得到的移植约束：**内核装载与运行必须落在 DRAM 窗口内**，**所有外设（UART / GIC）必须落在 Device 窗口内**。超出这两个窗口的设备或内存，在当前引导页表下不可访问。

## aarch64 移植边界

以下为当前实现**明确不支持**的场景，移植到不满足条件的 SoC 需要改动源码，不是配置项：

| 限制 | 说明 | 相关源码 |
|---|---|---|
| 仅 GICv3 | 中断控制器必须是 `arm,gic-v3`；GICv2 无支持路径 | [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs) |
| 仅 PL011 UART | 串口必须是 `arm,pl011`（ARM PrimeCell）；其他 IP（如 8250 / SBSA UART）需另写驱动 | [uart.rs](../../src/kernel/privileged/arch/aarch64/uart.rs) |
| DRAM 基址固定 `0x40000000` | 装载地址 `0x40080000` 是 `text_offset` 与 DRAM 基址的组合产物；基址不同需改链接脚本、引导页表与装载地址 | [aarch64.ld](../../src/kernel/privileged/link/aarch64.ld) |
| DRAM 窗口上限 512 MiB | 引导页表只映射 `[0x4000_0000, 0x8000_0000)`，更大内存需扩表 | [mmu.rs](../../src/kernel/privileged/arch/aarch64/mmu.rs) |
| 单核启动 | 引导路径为单核；未见 SMP 引导（secondary core dispatch）流程 | [start.S](../../src/kernel/privileged/boot/aarch64/start.S) |
| 需具备 distro boot 能力的 U-Boot | 介质依赖 `sysboot` / `booti`；无 U-Boot 的裸板需自备装载器并保证经 `x0` 传入合规 DTB | [make_boot_medium.sh](../../scripts/make_boot_medium.sh) |

x86_64 侧无 DTB 契约，由 GRUB2 经 multiboot2 装载，真机仅需目标机支持 BIOS/UEFI 从 USB 或光驱引导。

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
