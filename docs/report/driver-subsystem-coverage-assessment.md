# 驱动子系统覆盖深度评估报告

> 总体判断：Edgine 当前驱动栈的真实射程是**「QEMU 上的 x86_64 演示级可用」**，不是可交付的设备支持面。字符控制台、e1000、AHCI/NVMe、virtio-blk/net、PS/2 键盘与 Multiboot2 帧缓冲构成唯一一条端到端成立的链路；USB、HDMI/DP、UEFI、用户态驱动、IOMMU 与热插拔则分别停在骨架、占位、未接线或仅解析的状态。驱动侧最尖锐的问题不是「缺哪些设备」，而是**统一驱动框架缺位**：EGDF 目前只是「全局注册表 + 协议函数表」，没有匹配、探测、移除、资源分配与错误状态机，且模块文档对它的能力宣称明显高于实现（`src/kernel/privileged/egdf/mod.rs:1-40` 自称「唯一的注册/发现/初始化/I/O 入口」）。第二尖锐的问题是**中断与 DMA 两条安全地基未闭合**：外设中断固定投递到 BSP，MSI-X 只写 entry 0、aarch64 地址写 0，IOMMU/SMMU 完全不存在，I6 门禁腿则因模式名不匹配而恒空转。

本报告是一次源码取证快照，评估基线为 HEAD `1fc4c204` 加当时工作树（工作树另有 10 项与本报告无关的在制改动）。全文行号只对交付时刻的工作树有效；后续读者应以「函数名 + 行号」双重定位，判据以函数体与控制流为准。作为制定驱动补齐计划与修复工程的输入依据，发布后事实部分冻结，规划与决策部分明确标注为建议。

## 一、结论摘要

**结论一：覆盖面的「已实现」远大于「已验证」，且真实可用面仅覆盖 QEMU x86_64 的一条最小链路。** `src/kernel/functions/driver/` 有 17,203 行、`src/kernel/privileged/driver/` 有 9,308 行（含 104 处 unsafe），但能产生用户可见 I/O 的只有控制台（`functions/driver/char/mod.rs:33-69`）、e1000（x86_64，`functions/driver/net/e1000.rs:23,446-456`）、AHCI/NVMe（x86_64，`functions/driver/storage/mod.rs:763`）、virtio-blk/net（MMIO）、PS/2 键盘（`privileged/driver/input/mod.rs:26-34`）与帧缓冲（`privileged/driver/display/mod.rs:301`）。aarch64 上只有 PL011 与 virtio 两族，且因 EGFS 磁盘挂载被 `#[cfg(target_arch = "x86_64")]` 门控（`src/kernel/lib.rs:938-969`），virtio-blk 仍无法兑现为根文件系统。

**结论二：EGDF 是「注册表 + 协议表」，不是驱动框架。** `Driver` trait 只有 `name/device_type/init/shutdown/is_ready/reset/status`（`privileged/driver/infra.rs:274-315`），没有 `probe/remove/match/id_table`；注册即无条件置 `Ready`，`init()` 的返回值被丢弃（`privileged/egdf/mod.rs:874-904`，`:881` 与 `:890`）；`egdf_init_all` 只处理 `Uninit`，而注册路径已把状态写成 `Ready`，该分支无生产者（`:948-965`）；`Probing`/`Failed` 两个状态在注册路径上同样无生产者（`:159-172`）；`shutdown_all` 把设备标成 `Failed` 而非 `Removed`（`:968-985`）。设备树 `devtree::init()` 在 functions 有包装（`functions/egdf/devtree.rs:252-254`），但全仓无生产调用者，根节点为空，`devtree_probe_composites` 随之退化（`privileged/egdf/composite.rs:271`）。用户态驱动的 bind/map/forward_irq 有实现（`privileged/egdf/user_driver.rs:63-359`），但 `src/rust/` 中无任何 `DRIVER`/`EGDF` syscall 常量，整条路没有 syscall 入口。

**结论三：中断亲和与 DMA 隔离是结构性瓶颈，且现有门禁不能证明它们成立。** x86_64 网卡 INTx 初始化时用调用者本核 LAPIC ID（`privileged/net/init/irq.rs:125-134`，`:131`），即固定 BSP；`route_irq_to_cpu_gsi()` 的目的域写作 `(apic_id & 0x0F) << 24`（`privileged/arch/x86_64/ioapic.rs:284-294`，`:290`），只保留 4 bit，且该函数连同 `route_irq_to_cpu` 与 FFI 入口全仓无生产调用者；MSI-X 只配置第一个 entry、无 PBA、无多向量，aarch64 分支把消息地址写 0（`privileged/pci/msi.rs:359-388`，`:379-383`）；aarch64 SPI 固定路由 CPU0（`privileged/arch/aarch64/gic.rs:683-697`）。DMA 侧无 IOMMU/SMMU：DMAR 表被解析（`privileged/arch/x86_64/acpi.rs:1005-1054`），但结果只被 `has_dmar()` 转成一个能力标记（`functions/driver/acpi.rs:57-58,325`），`get_dmar_drhd_list()` 无调用者（`:1058`），没有任何 VT-d 编程或 DMA 域分配；I6 门禁只扫 `dma_map/dma_unmap/dma_sync` 三个名字（`scripts/audit_invariants.py:111-121`），而这三个名字在全仓零命中——外设可 DMA 写任意物理页，门禁恒绿。

## 二、取证口径与方法

**成熟度分档**（全文统一使用）。*可用*：存在真实硬件路径，且至少一次端到端执行（QEMU 计入），能被上层或用户态消费。*仅初始化*：控制器被初始化并注册，但数据面、类驱动或中断闭环未成立。*仅探测/骨架*：设备发现、寄存器序列或协议解析存在，但不能产生有效 I/O。*未实装*：无对应模块，仅有常量、注释或占位函数。

**测试证据分档**。*行为测试*：真正执行被检逻辑（含 mock 对象驱动）。*文本契约*：`read_to_string`/`include_str!` 读源码后做字符串匹配，只证明源文本形状，不证明行为。现有驱动 host-tests 中相当一部分属后者，报告会在引用时注明。

**量化读数**（`python3 scripts/audit_tcb_ratio.py` 实测）。privileged 124,675 行 / functions 88,318 行，TCB 占比 **56.5%**（目标 <30%）；`privileged/driver` 9,308 行 / 104 unsafe，是 TCB 内最大的业务模块之一。驱动相关 privileged 合计约 **15,223 行**（driver 9,308 + pci 1,606 + egdf 3,110 + dma 1,199），另加 `privileged/dma_buf.rs` 283 行；驱动相关 functions 约 **18,183 行**（driver 17,203 + egdf 980）。`functions/driver/usb` 4,696 行、69 个内联 `fn test_`；`functions/driver` 合计 126 个内联测试；host-tests 中驱动相关文件 14 个。

**「不存在」结论的检索范围**。`find src/kernel` 对 `rtc/watchdog/wdt/audio/hda/i2s/gpu/modeset/mouse/ehci/uhci/ohci` 全部零命中；全树检索 `iommu|IOMMU|SMMU` 仅命中 ACPI 解析与查询注释；`src/rust` 检索 `DRIVER|EGDF` 零命中；`docs/explain/` 8 篇文档无驱动专篇，`docs/design/` 2 篇中的 `freg-design.md` 有驱动域重置的设计叙述（`:293`、`:657-669`），但没有驱动代码注册 FREG 域（`src/kernel/*/driver/**` 检索 `freg` 零命中）。

## 三、对既有预设的三处修正

**其一，USB 不是「无栈」，而是「有骨架、无数据面」。** `functions/driver/usb/` 4,696 行，覆盖 xHCI 控制器 reset/start、Command Ring/Transfer Ring、PortSC 轮询（`functions/driver/usb/mod.rs:141-172,179-227`）、描述符解析与 69 个单测；但枚举是 mock：`enumerate_connected_devices()` 固定端口 1 并传入 `|| Ok(1)` 的假描述符（`:231-242`），`enumerate.rs:304-331` 直接使用 `mock_get_configuration_descriptor_response()`；`submit_urb` 只校验参数后 ring doorbell，不写 Transfer Ring TRB、不跟踪完成（`functions/driver/usb/xhci.rs:1204-1232`，SIMPLIFIED 注释在 `:1228-1230`）；`HidDriver`/`MassStorageDriver` 只有定义与测试，全目录检索 `HidDriver::new|MassStorageDriver::new` 零命中。因此准确的表述是「Host 侧 xHCI 骨架 + 未闭环的枚举与传输 + 未实例化的类驱动」，而非「没有 USB 栈」。

**其二，EGDF 不是「仅设计」，但也不是完整框架。** 它有 4,090 行实现（privileged 3,110 + functions 980），包括注册表、`EGDFProto`/`EGDFOps`、`BlockDevice` trait、`devtree`、`composite`、`firmware` 与 `user_driver`；但缺少驱动框架的四个核心件：设备-驱动匹配、probe/remove 生命周期、资源仲裁、失败隔离。它与文档的关系是「设计意图被写成了实现的一部分」：`docs/plan/framekernel-paradigm-enforcement.md:176-177` 把 `egdf/devtree.rs` 列为「privileged 机制保留 + functions 安全代理转发」，而实际生产路径从未初始化设备树。

**其三，aarch64 不是「纯内核镜像 + initramfs，无驱动」，而是「有 virtio MMIO、无磁盘根、无 PCI」。** virtio-blk 与 virtio-net 的 MMIO 路径在 aarch64 可用（`functions/driver/virtio/mod.rs:96-98`；`functions/driver/virtio/mod.rs:41` 的 virtio-net 探测回调）；但 `storage_init` 整体 `#[cfg(target_arch = "x86_64")]`（`functions/driver/storage/mod.rs:763-764`），`privileged/driver/bus/pci.rs` 仅在 x86 编译（`privileged/driver/bus/mod.rs:15-16`），aarch64 的 `bus_init` 是空 stub（`:37-43`）；`pci::init()` 的调用点只有 `bus_init`、`storage_init:769` 与 `e1000.rs:348`，均属 x86 路径。故 aarch64 上 AHCI/NVMe/xHCI/e1000 全部不可用，virtio-blk 虽可被探测，却因 EGFS 挂载门控而不成为根文件系统。另外，`privileged/pci/mod.rs:87-92` 有 aarch64 ECAM 默认基址 `0x3F00_0000`，`set_ecam_base` 存在（`:230-234`）但无调用者，ACPI MCFG 未解析——这是一条「代码在、路径不在」的典型。

## 四、现状盘点

### 4.1 初始化编排

驱动初始化由 crate root `boot_driver_init()` 单向编排（`src/kernel/lib.rs:838-888`），顺序为：帧缓冲绘图策略（`:845`）→ `privileged::driver::init_all()`（`:846`）→ 字符设备（`:849`）→ virtio-blk（`:852`）→ x86 存储（`:856-857`）→ USB（`:861`）→ 显示控制器（`:866-867`）。`init_all()` 内部再分：x86 PCI 总线与 PS/2 输入（`privileged/driver/mod.rs:160-166`）、显示（`:170`）、热插拔（`:174`）、复合设备探测（`:179`）、Block softirq 注册（`:182-185`）。延迟初始化另有 `pm_init`（`lib.rs:916-918`）与 UEFI `init(0)`（`:926-929`）。

编排是单点且顺序清楚的，这一点优于多数同类自研内核；问题在于被编排的多数 `init` 内部对失败不传播（见 §5.4），以及 `functions::driver::init()`（注册共享中断决策策略，`functions/driver/mod.rs:97-101`）在全仓没有生产调用者——`privileged/idt/irq_trait.rs:103` 的注释说它「由 functions::driver::init 调用」，但该函数从未被调用，`DriverIrqDecision` 实际上没有注册。

### 4.2 字符设备

x86_64 有 VGA 文本模式与 16550 串口（COM1 注册带 `io_base` 与 IRQ4），aarch64 有 PL011（`functions/driver/char/mod.rs:33-69`，x86 分支 `:41-52`，aarch64 分支 `:60-68`）。成熟度：控制台可用。但 EGDF 的 char 读写路径没有生产消费者：`char_init` 只做 `egdf_register_driver`，不绑定 `CharOps`，文件头 SIMPLIFIED 明说 `egdf_char_write/read` 休眠（`:29-32`）。这意味着字符设备目前只服务内核内部控制台与 devfs 节点暴露，不构成通用 tty。

### 4.3 存储设备

x86_64 有 ATA PIO 回退、AHCI 与 NVMe（`functions/driver/storage/mod.rs`，`storage_init` 在 `:763-764`）。队列缓冲存在两条分配路径：较新的 `alloc_dma_buffer` 走 `DmaStream` RAII（`privileged/driver/storage/mod.rs:200-223`），遗留路径仍走 `alloc_coherent` 裸指针（`:172-175,543-546`）；NVMe 的 MSI-X 接线与受控自检在 `functions/driver/storage/mod.rs:260-289,683-752`。virtio-blk 在两架构都通过 MMIO 探测（`functions/driver/virtio/mod.rs:96-98`），x86 通常因 PCI AHCI/NVMe 优先而不成为主盘。USB 存储未接线：`drives_for_location` 对 `BusType::Usb | BusType::Virtio` 返回空（`functions/driver/storage/mod.rs:541`）。EGFS 磁盘挂载仅 x86（`lib.rs:938-969`）。成熟度：x86 AHCI/NVMe 可用（QEMU），aarch64 virtio-blk 可探测但不成根。

### 4.4 网络设备

e1000 的真实实现被 `#[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]` 门控（`functions/driver/net/e1000.rs:23,446-456`），非该组合下是恒返 `None` 的桩（`:453-456`）。中断优先级为 MSI-X → INTx → 轮询，失败支保持 IMS 掩蔽（`qemu_boot_test.sh` 的 e2e 与 `host-tests/tests/intx_ioapic_wiring_test.rs:164-194` 的文本契约锁）。virtio-net 走 MMIO + `NetOps` 桥（`functions/driver/net/mod.rs:17-21`）。成熟度：x86 e1000 可用（INTx 或轮询），aarch64 virtio-net 可用（QEMU 软观测）。

网络中断亲和是固定 BSP：`net_register_intx_isr` 取调用者本核 ID（`privileged/net/init/irq.rs:125-134`），aarch64 `register_device_spi` → `route_spi_to_cpu0`（`privileged/arch/aarch64/gic.rs:725-728`）。这在小核数 QEMU 上无感，在 SMP 真机上会把全部网卡中断压到 CPU0。

### 4.5 显示设备

真实可用的只有帧缓冲：`privileged/driver/display/mod.rs:301` 的 `display_init()` 从 Multiboot2 framebuffer 或 PCI VGA 取基址，`display/mod.rs:44` 实现 `Driver`。HDMI/DP 是 fallback 占位：`functions/driver/display/mod.rs:57-82` 无条件注册 `hdmi0`/`dp0`/`display-manager`，`HdmiController::new()` 的 `iomem: None`（`functions/driver/display/hdmi.rs:820-823`），只有 `new_with_iomem` 才接硬件（`:839-847`），而注册路径没有调用后者。SIMPLIFIED 注释承认「QEMU 无真实 HDMI/DP 硬件，以 fallback 构造注册」（`functions/driver/display/mod.rs:45-48`）。aarch64 无 Multiboot2 framebuffer，故显示为 headless。成熟度：x86 帧缓冲可用；HDMI/DP 仅初始化（占位），两架构均无模式设置。

### 4.6 输入设备

仅 x86_64 PS/2 键盘：`input_init` 调 `keyboard_init()`，经 `egdf_register_with_ops` 注册带 `InputOps` 与 IRQ1 的设备（`privileged/driver/input/mod.rs:26-34`；`keyboard.rs` 实现 `Driver`）。文件头把鼠标、手柄标为「未来」（`input/mod.rs:5-15`）；aarch64 `input_init` 为空（`:36-38`）。无 USB HID 生产实例，无触摸板/鼠标。

### 4.7 总线、固件与电源

PCI 枚举在 x86 走 CF8/CFC，aarch64 走 ECAM（`privileged/pci/mod.rs:78-92`）；`scan_all_buses`/`init` 在 `:556-605`，只做总线扫描与按 class 查找，无桥递归、无 BAR sizing/分配、无总线资源仲裁。注释本身承认 e1000「自行执行 PCI 扫描（经 0xCF8/0xCFC），不复用本模块的设备列表」（`privileged/pci/mod.rs:33`），即存在重复枚举。UEFI 只有转发与占位：`functions/driver/uefi.rs:16-18`，`lib.rs:926-929` 固定传 0。ACPI 在 x86 有安全查询代理（`functions/driver/acpi.rs:1-59`），aarch64 无 ACPI。设备树解析只覆盖 memory/uart/gicv3 三类基址（`privileged/dtb.rs:14-17` 的 SIMPLIFIED 自述），没有 `interrupt-map`/`phandle`/按 `device_type` 枚举。电源管理有 C-state/idle 与 S3/S5 状态枚举（`privileged/driver/power.rs:498-516`），但 S3 是 halt 占位（`:757-772`，`:759` 明确写 placeholder），不写 `PM1_CNT`、不做设备挂起/恢复序列。

### 4.8 架构能力矩阵

| 设备类别 | x86_64 | aarch64 | 成熟度 | 关键证据 |
|---|---|---|---|---|
| VGA/16550 控制台 | 可用 | — | 可用 | `functions/driver/char/mod.rs:41-52` |
| PL011 控制台 | — | 可用 | 可用 | `functions/driver/char/mod.rs:60-68` |
| PS/2 键盘 | 可用 | 未实装 | 可用（x86） | `privileged/driver/input/mod.rs:26-34` |
| AHCI / NVMe | 可用（QEMU） | 不可用 | 可用（x86） | `functions/driver/storage/mod.rs:763`；`bus/mod.rs:15-16` |
| ATA PIO | 回退 | 不可用 | 仅初始化 | `privileged/driver/storage/mod.rs` |
| virtio-blk | 可探测 | 可探测 | 仅初始化（aarch64 不成根） | `functions/driver/virtio/mod.rs:96-98`；`lib.rs:938-969` |
| USB 存储 | 未接线 | 未接线 | 未实装 | `storage/mod.rs:541` |
| e1000 | 可用（QEMU） | 不可用 | 可用（x86） | `net/e1000.rs:23,446-456` |
| virtio-net | 可探测 | 可用（QEMU） | 可用（aarch64） | `net/mod.rs:17-21` |
| 帧缓冲 | 可用 | 无 | 可用（x86） | `privileged/driver/display/mod.rs:301` |
| HDMI / DP | 占位 | 占位 | 仅初始化（fallback） | `functions/driver/display/mod.rs:45-82`；`hdmi.rs:820-823` |
| xHCI | 骨架 | 骨架 | 仅初始化/骨架 | `functions/driver/usb/mod.rs:141-172`；`xhci.rs:1204-1232` |
| HID / MSC 类驱动 | 未实例化 | 未实例化 | 未实装 | 全目录检索 `HidDriver::new` 零命中 |
| UEFI | 占位 | — | 未实装 | `lib.rs:926-929` |
| PCI 枚举 | 可用（扫描） | 代码在、未接线 | 仅探测（x86） | `pci/mod.rs:87-92,230-234,556-605` |
| ACPI | 查询代理 | 无 | 仅探测 | `functions/driver/acpi.rs:1-59` |
| FDT | — | 最小解析 | 仅探测 | `privileged/dtb.rs:14-17` |
| RTC / 看门狗 / 音频 / GPU modeset | 无 | 无 | 未实装 | `find` 零命中 |

各类别与测试的映射（详见 §4.9）：控制台对应 QEMU 启动日志断言；PS/2 键盘对应内联单测；AHCI/NVMe 对应 `storage_identify_parse_host_test.rs`、`nvme_ahci_activation_test.rs`、`ahci_port_hotplug_contract_test.rs`（后两者为文本契约）与手工脚本 `pci_devices_boot_test.sh`；e1000 对应 `driver_e1000_eeprom_test.rs`、`driver_e1000_ctrl_bits_test.rs`、`intx_ioapic_wiring_test.rs` 与 QEMU e1000 e2e；virtio 对应 `virtio_vring_layout_contract_test.rs`、`virtio_net_arch_unify_test.rs`、`net_device_ops_bridge_test.rs`；显示对应 `driver_display_test.rs`；USB 对应 69 个内联单测（无集成）；ACPI 对应 `p77_acpi_full_table_wiring_test.rs`；FDT、RTC/WDT、音频、GPU modeset 无测试。

### 4.9 测试覆盖

驱动相关 host-tests 14 个，其中较有价值的行为测试是 `egdf_registry_io_host_test.rs`（`MockBlk` 驱动注册表与 `blk_read/blk_write` 往返）、`driver_display_test.rs`（EDID/时序/链路训练算法）、`storage_identify_parse_host_test.rs`（IDENTIFY 解析）、`net_device_ops_bridge_test.rs`（`NetOps` mock）与 `driver_e1000_eeprom_test.rs`（EEPROM 解析）。其余多为文本契约：`intx_ioapic_wiring_test.rs`、`nvme_ahci_activation_test.rs`、`virtio_vring_layout_contract_test.rs`、`hotplug_reenum_contract_test.rs`、`ahci_port_hotplug_contract_test.rs`、`driver_e1000_ctrl_bits_test.rs`。`driver_e1000_eeprom_test.rs` 自己说明 MockIoMem 无法挂接内核私有 MMIO 路径，QEMU 承担覆盖；`nvme_ahci_activation_test.rs:1-12` 明确「主机端无法实际跑 PCI 探测」。

QEMU 侧只有网络维度是真设备：x86 挂 `-device e1000`（`scripts/qemu_boot_test.sh:329`）并断言驱动初始化与 INTx 投递（`:564-584`），aarch64 挂 `virtio-net-device` 做软观测（`:478,895`）。`qemu_boot_test.sh` 不挂 NVMe/AHCI/xHCI/USB/磁盘设备。存在一份 `scripts/pci_devices_boot_test.sh`，覆盖 NVMe + e1000 与 5 项断言（文件头 `:1-15`），但它没有被 `ci/audit.sh` 或 `Makefile*` 引用，只在一份归档 plan 中被提及（`docs/plan/archive/audit-fix-04-framework-net-drivers.md:133`），属手工脚本，不构成门禁。

## 五、抽象与边界

### 5.1 驱动模型的现有形态

现有模型由三块拼成：`Driver` trait（行为契约）、`EGDFDevice` 注册表（设备描述）、各驱动的 `init()` 函数（硬件序列）。`Driver` trait 的形态是一个「运行期对象接口」，不是「总线匹配接口」（`privileged/driver/infra.rs:274-315`）：`init`/`shutdown`/`is_ready`/`reset`/`status` 都是设备已确定后的动作，缺 `probe(device) -> Result`、`remove`、以及 `id_table`/`compatible` 之类的匹配元数据。`DeviceInfo` 只是元数据容器，注释明确「不含注册逻辑」（`infra.rs:209-224`）。

`EGDFDevice` 携带 `id/name/proto/state/io_base/irq/driver_data/driver_owned/ops/block_dev`（`privileged/egdf/mod.rs:192-225`），其中 `driver_data: *mut u8` 类型擦除，配套 `unsafe impl Send/Sync`（`:231-233`）与 `unsafe fn driver_as_mut/as_ref`。`irq: Option<u8>` 把中断号固化在 8 bit，无法表达 MSI-X 多向量或 SPI 编号域，也没有「向量句柄」概念。

### 5.2 EGDF 与文档的落差

模块头自述「唯一的注册/发现/初始化/I/O 入口」（`privileged/egdf/mod.rs:1-40`），但逐条对照实现：

- **注册**：`egdf_register_driver` 先调 `driver.init()`，丢弃结果（`:881`），再无条件写 `state: DeviceState::Ready`（`:890`）；`egdf_register_driver_with_ops` 同构（`:915,924`）。
- **初始化**：`egdf_init_all` 只处理 `Uninit && driver_owned`（`:948-965`，判定在 `:959`），但注册时已置 `Ready`，故该分支永远不命中；`egdf_set_state`（`:543-549`）存在，但 functions 侧包装无生产调用者，故 `Probing`/`Failed` 在生产路径上无生产者。
- **失败**：`Failed` 由 `shutdown_all` 写入（`:968-985`，赋值在 `:976`），语义是「关闭后」，不是「初始化失败」，与 `DeviceState` 文档（`:159-172`）不符。
- **移除**：块设备有墓碑注销（`egdf_unregister_block` 在 `:528-541`，生产调用见 `functions/driver/storage/mod.rs:438,622`）；通用 `egdf_unregister` 也在（`:511-516`），functions 侧有包装（`functions/egdf/mod.rs:281-283`），但除单测（`:1082`）外无生产调用者。该函数只 `remove` 条目并返回裸指针，不调用 `shutdown`、不 `drop` EGDF 持有的 `Box`（`:511-516`），未来接线会泄漏 `driver_owned` 对象（代码推导，未运行时验证）。因此 Char/Net/Input/Bus 没有 remove 生命周期，热插拔仍是「重扫描 + 重新注册」。
- **设备树**：`functions/egdf/devtree.rs:252-254` 的包装存在，生产无调用者；`privileged/egdf/devtree.rs:408-409` 的 FFI 只在测试（`:457,472,485`）被调。
- **用户态驱动**：`user_driver` 的 bind/unbind/map/unmap/forward_irq 齐全（`privileged/egdf/user_driver.rs:63-359`），functions 侧有强类型包装（`functions/egdf/user_driver.rs:4` 起），但 `src/rust` 无 `DRIVER`/`EGDF` syscall 编号，`functions/egdf/user_driver.rs` 的包装无生产调用者。
- **固件**：`firmware` 的 attach/get 依赖 devtree 节点（`privileged/egdf/firmware.rs:96-127`），devtree 未初始化即整条不可用；GPU VBIOS 类路径因此被阻塞。

结论：EGDF 的「注册表 + 协议函数表」部分是可用的（`BlockDevice` 与 `blk_read/blk_write` 有 host 行为测试），但「框架」部分（设备-驱动匹配、通用 probe/remove 生命周期、资源仲裁、失败隔离）尚未实现——块设备墓碑注销是唯一成型的移除路径。文档把它称为框架，属于**设计意图前置到实现文档**。

### 5.3 资源分配与所有权

- **MMIO/PIO**：`IoMem`/`IoPort` 是 functions 访问硬件的唯一合法代理（`functions/driver/mod.rs:21-27` 的迁移约定），e1000、serial、pl011、framebuffer 均经此；但 privileged 内部仍有裸路径（见 5.5）。
- **DMA**：存在两套 API。`DmaStream` 是 RAII 强约束（`privileged/dma_buf.rs:73-137`：页对齐、非零、≤256 MiB、`checked_add`、`Frame` 持有防重用）；`dma::engine::alloc_coherent` 返回裸 `(VirtAddr, PhysAddr)`（`privileged/dma/engine.rs:105-155`），`free_coherent` 存在但依赖 shutdown 统一释放（`:157`）。NVMe/AHCI 的旧路径仍用后者（`privileged/driver/storage/mod.rs:172-175,543-546`），而 `alloc_dma_buffer` 已改用前者（`:200-223`）。两套并存使「DMA 生命周期」没有单一契约。
- **PCI BAR**：`PciDevice` 有 `bars` 与 `bar_count`，但无 BAR sizing/写入、无资源冲突仲裁；MSI-X 直接假定 `bar.base_addr + table_offset` 是 identity 映射后的虚拟地址（`privileged/pci/msi.rs:364`），没有 `IoMem` 代理。
- **IRQ**：`irq: Option<u8>` + 全局 `IdtManager::register_irq`，`InterruptHandler` 是裸函数指针，无法捕获驱动实例状态；键盘驱动用全局槽位绕开（`privileged/driver/input/keyboard.rs:766,957`）。注销 API 存在（`IdtManager::unregister_irq`，`privileged/idt/table.rs:544`），但驱动层无调用点，热移除无法释放中断线。

### 5.4 错误处理约定

`DriverError` 的错误码集合完整（`privileged/driver/infra.rs` 的 `DriverError` 段：`InvalidParameter/DeviceNotFound/Timeout/HardwareError/BufferTooSmall/UnsupportedOperation/Busy/NotInitialized`），但**没有传播纪律**：注册路径丢弃 `init` 结果（`egdf/mod.rs:881,915`），`storage_init` 的 SIMPLIFIED 注释写明「错误降级为日志不传播」（`functions/driver/storage/mod.rs:762`），`char_init` 同模式。其后果是「设备注册成功但不可用」与「设备不存在」在注册表里不可区分——两者都是 `Ready`。`reset()` 默认返回 `UnsupportedOperation`（`infra.rs:305-307`），但没有任何调用者；`status()` 默认恒 `"Ready"`（`:312-314`）。失败隔离、重试、隔离舱（quarantine）均不存在。

### 5.5 privileged↔functions 边界与安全封装漏口

边界主轴是正确的：驱动业务在 functions（0 unsafe），MMIO/PIO/DMA/中断机制在 privileged。但以下漏口使「安全代理」不完整：

- **PCI MSI-X 表裸指针**：`privileged/pci/msi.rs:364-386` 直接 `*mut MsixTableEntry` 写 MMIO，未经 `IoMem`，也无边界校验（只检查 `table_bar < bar_count` 与 BAR 类型）。
- **PCI ECAM/CF8 裸访问**：`privileged/pci/mod.rs:275,300,324,354,385,411` 的 `read_volatile/write_volatile`，以及 0xCF8/0xCFC 直写；这些在 privileged 内不算违规，但意味着 PCI 没有像 e1000 那样的 `IoMem` 安全代理层。
- **显示 PIO 裸汇编**：`privileged/driver/display/mod.rs:446,456` 的 `asm!("out dx, ax")` / `asm!("in ax, dx")` 与 `:203` 的 `read_volatile`，绕过 `IoPort`。
- **USB 的 `Box::leak`**：`functions/driver/usb/mod.rs:155` 把控制器泄漏成 `&'static mut`，EGDF 只登记裸指针（`:156-162`），所有权既不在 EGDF（`driver_owned: false`）也不在可追踪的句柄中；这是 `audit_static_mut` 扫描范围外的静态可变面。
- **EGDF 类型擦除指针**：`driver_data: *mut u8` + `unsafe impl Send/Sync`（`privileged/egdf/mod.rs:225-233`），安全性依赖「驱动自身锁」与调用方类型正确性，没有类型级约束。
- **I6 门禁空转**：`scripts/audit_invariants.py:111-121` 只匹配 `dma_map/dma_unmap/dma_sync`，三个名字全仓零命中；真实 DMA 出口是 `alloc_coherent`/`DmaStream` 且都在 privileged，故无论怎么用该腿都返回零违规。
- **I5 门禁射程**：`scripts/audit_invariants.py:98-108` 只扫 functions 层的 `inb/outb/inl/outl/inw/outw` 文本，privileged 内裸访问全部逃逸。

## 六、不足清单

下表按「缺陷」（现有宣称/路径不成立，需修）与「能力」（尚不存在，需建）两类标注。严重度 S1 = 阻断可用性或安全不变式；S2 = 显著限制覆盖面；S3 = 工程完备性问题。

| 编号 | 类别 | 事实与证据 | 严重度 | 性质 |
|---|---|---|---|---|
| G1 | USB HID/存储 | 枚举 mock（`usb/mod.rs:231-242`；`enumerate.rs:304-331`）；`submit_urb` 只 doorbell（`xhci.rs:1204-1232`）；`HidDriver`/`MassStorageDriver` 无生产实例；`drives_for_location` 对 USB 返回空（`storage/mod.rs:541`） | S1 | 能力 |
| G2 | GPU 模式设置/3D | 只有帧缓冲与 HDMI/DP 占位；`HdmiController::new()` 无 IoMem（`hdmi.rs:820-823`）；无 GPU 驱动文件（`find` 零命中）；VBIOS 路径被未初始化 devtree 阻塞（`egdf/firmware.rs:96-127`） | S2 | 能力 |
| G3 | 音频 | `find` 对 `audio/hda/i2s` 零命中；无 PCM/混音/设备节点 | S2 | 能力 |
| G4 | RTC / 看门狗 | `find` 对 `rtc/watchdog/wdt` 零命中；`FREG` 设计提到 watchdog 策略窗口（`docs/design/freg-design.md:657`），但无代码 | S2 | 能力 |
| G5 | PCIe 枚举与资源 | 无桥递归、无 BAR sizing/分配、无总线资源仲裁（`pci/mod.rs:556-605`）；aarch64 ECAM 硬编码且 `set_ecam_base` 无调用者（`:87-92,230-234`）；ACPI MCFG 未解析；e1000 自行扫描（`:33`） | S1 | 缺陷 |
| G6 | 电源管理 | S3 仅 halt 占位（`privileged/driver/power.rs:757-772`）；无设备 suspend/resume、无 runtime PM、无 `PM1_CNT` 序列 | S2 | 能力 |
| G7 | DMA 安全 | 无 IOMMU/SMMU；DMAR 解析结果只成能力标记、无 VT-d 域分配（`arch/x86_64/acpi.rs:1005-1058`；`functions/driver/acpi.rs:57-58,325`）；I6 门禁空转（`audit_invariants.py:111-121`）；双 DMA API（`dma_buf.rs:79-137` vs `dma/engine.rs:105-157`） | S1 | 缺陷 |
| G8 | 中断 | 固定 BSP（`net/init/irq.rs:125-134`）；GSI 路由函数 4-bit dest 且无调用者（`ioapic.rs:284-294`）；MSI-X 单 entry/无 PBA/多向量（`pci/msi.rs:359-397`）；aarch64 MSI-X 地址 0（`:379-383`）；`route_intx_irq` 不解析 `_PRT`、无共享线 handler 链（`ioapic.rs:365-398`）；aarch64 SPI 固定 CPU0（`gic.rs:683-697`） | S1 | 缺陷 |
| G9 | 热插拔/热移除 | 只有轮询：`hotplug_init` + `register_aux_poll`（`privileged/driver/hotplug.rs:350-385`）；PCIe slot 事件靠轮询（`pci/hotplug.rs:229`）；无 ACPI GED/PME 中断；无 surprise removal 的 I/O 静默与在途请求处理 | S2 | 混合 |
| G10 | 错误恢复/FREG | 注册忽略 init 失败（`egdf/mod.rs:881,915`）；`reset` 无调用者（`infra.rs:305-307`）；驱动目录零 `freg` 命中；FREG 设计有驱动域重置叙述（`docs/design/freg-design.md:293,657-669`）但无接线 | S1 | 缺陷+能力 |
| G11 | 真机支持 | 仅有 `docs/explain/guide-hardware-boot.md`；CI/QEMU 全为仿真；无真机矩阵、无硬件兼容性清单 | S2 | 能力 |
| G12 | aarch64 DT/ACPI 双栈 | DTB 仅 memory/uart/gic（`dtb.rs:14-17`）；无 aarch64 ACPI；无 aarch64 PCI 初始化（`bus/mod.rs:15-16`；`pci::init` 调用点全 x86）；EGFS 磁盘挂载仅 x86（`lib.rs:938-969`） | S1 | 缺陷 |
| G13 | 测试/mock | 多数驱动 host-tests 为文本契约；MockIoMem 无法注入内核私有 MMIO 路径（`driver_e1000_eeprom_test.rs` 自述）；`pci_devices_boot_test.sh` 未入门禁；无故障注入设备 | S2 | 能力 |

补充两条工程性欠缺（S3）：其一，驱动相关 `docs/explain/` 无专篇，驱动边界只能从代码与 plan 归档反推；其二，EGDF 模块文档与实际能力不一致（§5.2），对后续维护者是误导源。

## 七、对照参照

以下为架构对照，属外部参照（未在本仓验证），只用于判断取向，不作为 Edgine 现状证据。

**Linux 驱动模型。** Linux 以 `bus_type` → `device_driver` 匹配（`id_table`/`of_match`/`acpi_match`）、`probe/remove` 生命周期、`devres` 资源托管、`genirq` 中断域、`IOMMU` 子系统、`driver core` 的电源管理与热插拔事件为核心。对照本仓：`Driver` trait 只覆盖「已匹配之后」的动作，`EGDFDevice` 的 `io_base/irq` 是扁平字段而非资源图，缺 `probe/remove`、缺 `devres` 式自动释放、缺 IRQ 域抽象、缺 `device_link`。代价是本仓模型极薄、易读；收益是短期内代码量小。但一旦设备类别超过个位数，缺少匹配与资源仲裁会让每个新驱动重复「找设备、抢资源、手工释放」的样板，且失败路径不可组合。

**Asterinas 的 framekernel 方案。** 据公开资料，Asterinas 以 OSTD（framework 层）封装 unsafe 与硬件机制，内核业务在安全 Rust 中实现，驱动也沿「机制在 framework、策略在 kernel」切分。Edgine 的 `IoMem`/`IoPort`/`DmaStream` 与 AGENTS §4.1 的归属决策树与这一取向同源，方向正确。差别在于 Edgine 的「framework 层」只提供了机制原语，没有提供设备模型；Asterinas 的设备抽象与驱动注册比本仓更接近「框架」。因此本仓当前不是「Asterinas 的简化版」，而是「Asterinas 的机制层 + 一个尚未成为框架的注册表」。

**seL4 / Fuchsia 的用户态驱动。** seL4 把驱动放到用户态，靠 capability 授权 MMIO/IRQ/DMA，内核只保留 IPC 与调度；Fuchsia 的 DFv2 也是用户态驱动框架，配合 IOMMU 与进程隔离。收益是 TCB 显著缩小、故障隔离天然成立；代价是 IPC 往返、驱动运行时、设备发现与电源管理的复杂度，以及**必须有 IOMMU 才能安全地把 DMA 交给用户态**。对照本仓：当前驱动在核内、DMA 无 IOMMU、用户态驱动只有未接线的 `user_driver` 骨架。若直接转向用户态驱动而不先做 IOMMU，等于把「外设可写任意物理页」从内核内移到用户进程，安全收益为零、复杂度大增。

**偏离「最小 TCB」的地方。** 按 AGENTS §4.1，驱动的 unsafe 机制应封装为 safe API 后把功能放 functions；本仓大体照做，但仍有四处使 TCB 偏大或边界变薄：PCI MSI-X/ECAM 未走 `IoMem`（`pci/msi.rs:364`；`pci/mod.rs:275-411`）；显示 PIO 裸汇编（`display/mod.rs:446,456`）；USB `Box::leak` 的静态可变面（`usb/mod.rs:155`）；EGDF 的类型擦除裸指针（`egdf/mod.rs:225-233`）。它们不是「功能必须在 privileged」的必然结果，而是封装未完成。驱动相关 privileged 15.2k 行占 TCB 12.2%，其中相当一部分是 wire 类型与队列包装，不是不可省的机制。

## 八、长期规划（建议）

以下为建议，不是既有事实。人月数为估算，依据两条：一是现有驱动相关代码量（functions 18.2k + privileged 15.5k 行）与「硬件 bring-up 含调试/测试约 600–900 行/人月」的工程经验；二是按条目拆分的下限/上限求和。真机兼容性维护不计入一次性估算。

### 阶段一：近期补齐阻塞项（缺陷修复为主）

目标是让现有宣称与实现对齐，并把两条安全地基补到「可证」而非「可宣称」。

关键工作：修 `route_irq_to_cpu_gsi` 的 4-bit 目的域或删除该死代码并统一到 `encode_intx_rte`；给 IRQ 路由增加「目标 CPU」入口并接到网卡/存储初始化；MSI-X 支持多 entry、PBA、per-vector mask，aarch64 明确返回 `Unsupported` 而不是写地址 0；EGDF 注册传播 `init` 失败并真正使用 `Probing/Failed`，删除或接通 `egdf_init_all` 的死分支；`devtree::init` 要么在启动编排中调用，要么删除包装与相关固件路径；统一 DMA 到 `DmaStream`，把 `alloc_coherent` 收敛为内部实现或删除；把 I6 审计改为检查「DMA 映射是否经 IOMMU/`DmaStream` 且缓冲区不落在内核保留区」，而不是三个不存在的符号名；建立 host 侧 mock 设备 harness（mock bus + mock MMIO + 可注入失败）。

验证方式：门槛 6 增加纯函数单测（RTE 编码、MSI-X entry 布局、EGDF 状态迁移）；门槛 4 增加 mock 设备行为测试（注册失败→`Failed`、`remove`→`Removed`、DMA 越界拒绝）；门槛 3 增加 I6 真实判据腿；门槛 5 在 `qemu_boot_test.sh` 增加 NVMe/AHCI/xHCI 挂载与断言；把 `pci_devices_boot_test.sh` 纳入 full 档或合并进 QEMU 腿。门槛 1/2 不新增内容，只保证上述改动双架构零告警。

估算：IRQ 0.3–0.5、MSI-X 0.8–1.2、EGDF 状态机 0.5–0.8、DMA/I6 0.5–0.8、mock harness 0.8–1.2，合计 **3–4.5 人月**。其中约 2.1–3.3 人月属缺陷修复，0.8–1.2 属测试基建（能力）。

### 阶段二：中期形成统一驱动框架（能力建设为主）

目标是把「注册表」升级为「设备模型」：总线发现 → 设备对象 → 驱动匹配 → probe → 资源授予 → remove。

关键设计决策：是否继续沿用 EGDF 名字与 `EGDFDevice`（建议沿用注册表与协议表，新增 `Bus`/`Device`/`DriverMatch` 三层，避免推倒重来）；设备树与 ACPI 是否统一为一棵「资源树」（建议统一为内部 `DeviceNode`，DTB/ACPI 只做前端解析）；MSI-X 与中断亲和是否进入设备模型（建议是，IRQ 句柄进 `Device`，向量数由驱动声明）；IOMMU 是否在此阶段接入（建议先做「每设备 DMA 域」抽象与 VT-d/SMMU 探测，完整编程可留到阶段三）；用户态驱动是否在此阶段接线（建议只保留接口，不承诺可用）。

验证方式：门槛 4 增加「mock 总线 + 两个驱动竞争同一设备」的匹配测试、「probe 失败回退到下一驱动」测试、「remove 后在途 I/O 拒绝」测试；门槛 5 增加 USB 键盘/存储与 NVMe 的真实 QEMU 设备断言；门槛 3 增加「设备资源不得重复授予」的审计腿；门槛 6 增加设备模型状态机单测。

估算：匹配/probe/remove/资源 1.5–2.0、DT/ACPI 设备模型 1.0–1.5、MSI-X 完整化与亲和 0.8–1.2、IOMMU 前端 1.5–2.5、热插拔事件模型统一 0.8–1.2、文档与测试 1.0–1.5，合计 **5.6–9.4 人月**；与阶段一重叠项合并后按 **5–8 人月** 计。

### 阶段三：长期接近可用（能力建设）

目标是让「常见设备在常见硬件上可用」：USB HID/存储完整、PCIe 资源与热插拔、ACPI 睡眠、GPU 至少一种模式设置路径、音频、RTC/WDT、aarch64 对等。

关键设计决策：用户态驱动 vs 内核内驱动（建议在 IOMMU 可用后，把「类驱动」（HID/MSC/音频）迁用户态，控制器与 DMA 机制留核内）；GPU 是否做完整 modeset（建议先做「固件+帧缓冲+EDID」的可用路径，完整 modeset 作为独立长项，或用户态）；USB 优先级（建议先 Host HID boot keyboard，再 MSC，device/gadget 侧维持「条件触发」——与既有 `docs/report/usb-host-device-side-roadmap.md:49-66` 的结论一致）；aarch64 是否补 ACPI（建议先补 PCI ECAM 发现与 virtio-blk 根文件系统，再评估 ACPI）。

验证方式：门槛 5 扩展为「设备矩阵」QEMU 腿（usb-kbd、usb-storage、nvme、ahci、virtio-blk/net、e1000、S3 挂起恢复）；门槛 4 引入故障注入（超时、DMA 错位、设备消失）；门槛 3 增加 IOMMU 配置与 DMA 域隔离审计；门槛 1/2 保持双架构与 clippy；门槛 6 增加电源状态机单测。真机验证需另建硬件矩阵，不属六道门槛能覆盖的范围。

估算：USB 完整 4–6、GPU 可用路径 3–6、音频 2–3、RTC/WDT 0.5–1、PCIe 资源+AER 2–3、ACPI S3+运行时 PM 1.5–3、aarch64 对等 2–4，合计 **14–26 人月**；按「不同时全做、按优先级串行」的常见节奏，一次性规划取 **15–24 人月**。

### 缺陷修复与能力建设的区分

阶段一以缺陷修复为主：4-bit 目的域、MSI-X aarch64 死路径、EGDF 错误状态、DMA 双 API、I6 空转门禁，都是「现有代码/文档不成立」的修正。阶段二、三以能力建设为主：匹配/probe/remove、资源仲裁、IOMMU、USB 数据面、GPU modeset、音频与电源管理，都是「尚不存在」的补齐。把两者混在一个批次里会导致「修完仍不可用、建完仍不安全」，建议按阶段拆 PR。

## 九、决策点

**D1 用户态驱动 vs 内核内驱动。** 内核内（现状）的利：无 IPC 往返、与 VFS/网络栈同地址空间、实现简单、QEMU 验证容易；弊：TCB 随驱动增长、故障不隔离、DMA 无 IOMMU 时无法真隔离。用户态的利：TCB 小、故障隔离、权限可按设备授予；弊：IPC 成本、需要驱动运行时与设备发现、**必须先有 IOMMU**、调试链路长。建议：阶段一、二维持内核内；在 IOMMU 可用后，把类驱动（HID/MSC/音频）作为首批用户态候选，控制器与 DMA 机制留核内。

**D2 EGDF 是否值得先落地。** 方案 A：在现有 EGDF 上补匹配/probe/remove/资源，保留 `BlockDevice`/`Ops` 兼容；方案 B：另建 Linux 式 device model，EGDF 退化为协议表；方案 C：放弃 EGDF，按设备类别直接接线。A 的利：已有 4,090 行与 host 测试可复用，迁移成本最低；弊：`EGDFDevice` 的扁平字段与 8-bit IRQ 会限制扩展，需要一次结构升级。B 的利：模型干净；弊：重写与双轨期长。C 的利：最快；弊：回到「每类驱动各自为政」，与统一框架目标相反。建议 A，但先把文档从「框架」降级为「注册表 + 协议表」，避免继续以框架名义扩张。

**D3 USB 优先级。** Host HID boot keyboard 的利：用户可见价值高、QEMU `usb-kbd` 可验证、依赖 Event Ring + 真实枚举 + 控制传输，正好补齐 xHCI 数据面；弊：需要先做 Event Ring 与完成跟踪。MSC 的利：可与块设备/EGFS 串起来；弊：依赖 Bulk 传输与 SCSI 命令层，工作量更大。Device/gadget 侧的利：能力完整；弊：QEMU 无标准 UDC、x86 平台通常无 UDC，验证手段缺失（既有报告已判「条件触发」）。建议顺序：HID → MSC →（条件满足时）gadget。

**D4 IOMMU 接入时机。** 早接的利：I6 从「宣称」变「成立」，为用户态驱动铺路，DMA 域隔离可与设备模型一起设计；弊：VT-d/SMMU 编程复杂，QEMU 验证需 `intel-iommu` 设备，短期看不到用户可见收益。晚接的利：先把设备面做宽；弊：所有 DMA 路径后续要改一次，用户态驱动无法安全启动。建议：阶段二做「DMA 域抽象 + 探测」，阶段三做「编程 + 强制」，与用户态驱动同步推进。

**D5 GPU 路径。** 仅帧缓冲的利：已有实现、风险低；弊：无法改分辨率/多显示器/硬件加速。完整 modeset 的利：真实可用；弊：厂商差异大（Intel/AMD/Synopsys）、EDID/DP 训练/时钟/PLL 工作量大、QEMU 覆盖有限。建议：短期维持帧缓冲 + EDID/DP 算法资产（已有 `functions/driver/display`），完整 modeset 作为独立长项，优先用户态或单一厂商。

**D6 aarch64 对等优先级。** 先补 PCI 的利：xHCI/AHCI/NVMe 在 aarch64 服务器与 QEMU `virt` 上可用，能复用 x86 驱动；弊：ECAM 基址需从 DT/ACPI 获取，`set_ecam_base` 目前无调用者。先补 EGFS 挂载的利：virtio-blk 已可用，改门控即可让 aarch64 从磁盘根启动；弊：收益集中在启动路径，不扩展设备面。建议：先做「virtio-blk 根文件系统 + ECAM 基址来源」，再做 PCI 设备复用。

## 十、推测 / 未验证 / 估算清单

| 条目 | 类型 | 说明 |
|---|---|---|
| 阶段一 3–4.5 人月、阶段二 5–8 人月、阶段三 15–24 人月 | 估算 | 依据为现有代码量 + 条目拆分；无历史工时数据校准，区间宽 |
| 「600–900 行/人月」bring-up 生产率 | 估算 | 工程经验值，非本仓实测 |
| Linux / Asterinas / seL4 / Fuchsia 的对照描述 | 未验证 | 基于公开资料的概括，未在本仓或上游源码逐条核对 |
| 「Asterinas 的设备抽象比本仓更接近框架」 | 推测 | 由公开描述推断，未读其当前源码 |
| 「用户态驱动必须以 IOMMU 为前提」 | 推断 | 基于 DMA 安全逻辑；若采用其它隔离机制（如固定 DMA 窗口）可部分替代 |
| 「aarch64 无 Multiboot2 framebuffer，故无显示」 | 推断 | 由引导链与 `display_init` 取源推断；未在 aarch64 QEMU 实测显示输出 |
| 「virtio-blk 在 aarch64 可探测但不成根」 | 已核实代码路径，未实测挂载 | `blk_init` 在 aarch64 调用（`lib.rs:852`）；EGFS 挂载仅 x86（`:938-969`） |
| 「USB 69 个单测只覆盖算法/解析」 | 推测 | 由测试命名与模块结构判断，未逐条阅读全部 69 个用例 |
| 「QEMU 默认 e1000 无 MSI-X，走 INTx」 | 引用既有结论 | 见 `scripts/qemu_boot_test.sh:438` 与 `intx_ioapic_wiring_test.rs:4-12` |
| 「驱动相关 privileged 15,223 行」 | 实测派生 | 四个目录 `wc -l` 之和；是否应计入 `dma_buf.rs` 取决于口径，已单列 |
| 真机兼容性、硬件矩阵工作量 | 未验证/估算 | 无真机测试数据，仅按设备类别粗估 |

## 十一、附录：关键证据索引

| 主题 | 文件与位置 |
|---|---|
| 驱动初始化编排 | `src/kernel/lib.rs:838-888`；`privileged/driver/mod.rs:159-186` |
| EGDF 注册/状态/关闭 | `privileged/egdf/mod.rs:159-172,874-904,947-985` |
| Driver trait | `privileged/driver/infra.rs:274-315` |
| 设备树未初始化 | `functions/egdf/devtree.rs:252-254`；`privileged/egdf/devtree.rs:408-409,457-485` |
| 用户态驱动无 syscall | `privileged/egdf/user_driver.rs:63-359`；`src/rust` 检索零命中 |
| USB 骨架与 mock | `functions/driver/usb/mod.rs:141-242`；`usb/xhci.rs:1204-1232`；`usb/enumerate.rs:242,304-331` |
| 存储门控与 USB 存储缺口 | `functions/driver/storage/mod.rs:541,763-769`；`privileged/driver/storage/mod.rs:172-175,204-218` |
| e1000 架构门控 | `functions/driver/net/e1000.rs:23,348,446-456` |
| 中断 BSP 固定与 4-bit | `privileged/net/init/irq.rs:125-134`；`privileged/arch/x86_64/ioapic.rs:284-294` |
| INTx 路由与 SIMPLIFIED | `privileged/arch/x86_64/ioapic.rs:313-320,365-398` |
| MSI-X 单 entry / aarch64 0 | `privileged/pci/msi.rs:338-397` |
| aarch64 SPI CPU0 | `privileged/arch/aarch64/gic.rs:683-697,725-728` |
| DMA 双 API | `privileged/dma_buf.rs:73-137`；`privileged/dma/engine.rs:105-157` |
| IOMMU 缺失 | `privileged/arch/x86_64/acpi.rs:962-1077`；全树 `iommu` 检索 |
| I6 门禁空转 | `scripts/audit_invariants.py:111-121` |
| 显示占位 | `functions/driver/display/mod.rs:45-82`；`functions/driver/display/hdmi.rs:820-823,839-847` |
| 电源 S3 占位 | `privileged/driver/power.rs:498-516,757-772` |
| PCI 资源/ECAM | `privileged/pci/mod.rs:87-92,230-234,556-605` |
| DTB 最小解析 | `privileged/dtb.rs:14-17` |
| QEMU 设备覆盖 | `scripts/qemu_boot_test.sh:329,478,564-584,895` |
| 手工 PCI 测试未入门禁 | `scripts/pci_devices_boot_test.sh:1-15`；`docs/plan/archive/audit-fix-04-framework-net-drivers.md:133` |
| TCB 读数 | `python3 scripts/audit_tcb_ratio.py`：56.5%，driver 9,308 行 / 104 unsafe |
