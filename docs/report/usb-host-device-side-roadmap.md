# QueenX USB 主/从侧（Host / Device）实现路线报告

> 总体判断：QueenX 现有 USB 子系统为**纯 Host 侧**，其**自持实现**是 framekernel 边界下唯一可行的路径，应当延续；**Device（gadget）侧**受"UDC 硬件存在性"与"QEMU 无虚拟 UDC"两项前置条件制约，**暂不自持，条件触发**。host 侧第三方库（`usb-host` 等）不可用，device 侧第三方库（`usb-device`）只覆盖上半层——两者均不建议引入。

本报告是 QueenX 对 USB 主/从侧实现路线的一次性技术快照。评估依据为 QueenX 仓库当前事实（`src/kernel/services/driver/usb/`、`src/rust/deny.toml`、`docs/report/third-party-library-selection-assessment.md`）与 crates.io 官方元数据（`usb-device` 0.3.2、`usb-host` 0.1.3）。作为后续制定 plan 与修复工程的输入依据。

## 一、问题背景：USB 的硬性主从不对称

USB 总线在设计上规定了唯一的根主机（Host），其余节点全为外设（Device）。这一不对称是故意写入协议规范的：Host 独占发起权——枚举新设备、分配地址、调度每一次传输、经 VBUS 供电；Device 只能被动响应，无法主动发起事务。

由此，同一根 USB 总线上的控制器在物理上分为两类，软件栈也天然分叉：

| 维度 | Host 侧（主） | Device 侧（从） |
|---|---|---|
| 控制器硬件 | xHCI / EHCI / OHCI / UHCI | UDC（USB Device Controller）：DWC2 / MUSB / Synopsys 等 |
| 软件职责 | 发现并驱动外部设备的**类** | **实现**一个类功能，把自己表现为键盘 / U 盘 / 串口 |
| 通信方向 | 树根 → 扫叶子（发起） | 叶子 → 等主机来扫（响应） |
| Linux 落点 | `drivers/usb/host/` + `drivers/usb/core/` | `drivers/usb/gadget/` + `drivers/usb/gadget/udc/` |

关键点在于**类驱动方向相反**：host 侧是"HID 类驱动去驱动一把外部键盘"，device 侧是"HID 类实现让本机对外表现为一把键盘"。名字相同，语义相反——因此两者不是可替换的候选，而是同一总线上的两条互补腿。

## 二、QueenX 现状：100% Host 侧、且已自持

`src/kernel/services/driver/usb/` 全部为 Host 侧实现，落在 services 子树（0 unsafe），模块结构见 `services/driver/usb/mod.rs` L11-19：

| 文件 | 职责 |
|---|---|
| `xhci.rs` | xHCI 主机控制器（USB 3.0）安全代理 |
| `usb_core.rs` | 核心类型（Descriptors / URB / `HostController` Trait） |
| `enumerate.rs` | 设备枚举（描述符解析 + 枚举流程） |
| `ring.rs` | xHCI 环形缓冲区（Command Ring + Event Ring） |
| `hid.rs` | HID 类驱动（键盘 / 鼠标 Boot Protocol） |
| `mass_storage.rs` | 大容量存储类驱动（BBB + SCSI） |

其设计原则（`mod.rs` L23-25）已明确"零 unsafe / MMIO 经 `framework::IoMem` 代理 / DMA 经 framework 安全包装"；控制器列表由 services 自持（`mod.rs` L55），PCI 发现常量（`mod.rs` L68-72）与初始化入口 `usb_init()`（`mod.rs` L141）亦均在 services。后续计划文件为 `ehci.rs` / `uhci.rs` / `ohci.rs`（`mod.rs` L29-31），方向仍是 host 侧各代控制器。

结论：QueenX 的 USB 能力当前**只有一个方向**——作为主机去驱动外部设备；device 侧尚无任何代码。

## 三、Host 侧：自持是唯一合架构路径

Host 侧不建议、也无必要改用第三方库，理由有三：

**其一，framekernel 边界决定的。** 主机控制器驱动必须直接访问 MMIO（xHCI 寄存器组）并处理 DMA（Command/Event Ring、URB 缓冲）。按 `AGENTS.md` §4.1 归属决策树，这类机制必须由 framework 封装为 safe API（`IoMem` / DMA 包装），功能实现在 services。第三方库无法提供这种切分——它们通常整包携带自身的 `unsafe` 与硬件抽象，会把 TCB 边界搅乱。

**其二，host 侧无可用第三方库。** 详见第五节。

**其三，该路径已在本仓库建成。** 现有实现即为自持路线的产物：0 unsafe、TCB 零膨胀，正符合 framekernel 的 Minimalism 目标。

## 四、Device 侧：暂不自持，受两项前置条件制约

不建议在现阶段投入 device 侧，原因不在"能否实现"，而在两项前置条件尚未解决：

| 前置条件 | Host 侧 | Device 侧 |
|---|---|---|
| 硬件控制器 | xHCI / EHCI，PC / 服务器 / AArch64 板普遍具备 | **UDC**；x86_64 平台典型不具备，仅部分 aarch64 板（DWC2 / MUSB）具备 |
| 验证手段（§2.3 QEMU 门槛） | 可挂 `usb-kbd` / `usb-storage` 等仿真设备到 xHCI 控制器 | **QEMU 无标准虚拟 UDC**，集成门槛对 gadget 基本空转 |

此外，需求本身尚未确立：gadget 角色（把本机装成 U 盘 / 串口暴露给外部主机）对通用内核属增强能力，而非核心能力。故 device 侧定位为"条件触发"，触发条件为"USB gadget 角色成为真实需求 + 存在带 UDC 的目标平台"。

## 五、第三方库评估：为何两者都不引入

### 5.1 `usb-device`（device 侧，方向相反）

| 项 | 值 |
|---|---|
| 描述 | USB stack for embedded devices |
| 归属 | `rust-embedded-community/usb-device` |
| 最新版 | 0.3.2（2024-03） |
| 代码规模 | 2563 行 / 12 文件 |
| 许可证 | MIT |
| 分层 | `UsbDevice` / `UsbClass` / `UsbBus` 三层 trait，仅覆盖**上半层** |

其方向与 QueenX 现有 host 栈相反（既有结论见 `docs/report/third-party-library-selection-assessment.md` L36）。更关键的是：即便将来做 device 侧，`usb-device` 也只提供 upper-half，底下的 `UsbBus` 控制器驱动**仍须自行编写**；且其 trait 形状为 MCU peripheral 风格（poll 驱动、embedded-hal 取向），与通用内核的上下文 / 中断模型并不契合。引入它省不掉最硬的部分，只增加一层接口耦合。

### 5.2 `usb-host`（host 侧，不可用）

| 项 | 值 |
|---|---|
| 描述 | Traits for USB host driver development |
| 归属 | 个人项目（作者 `bjc`），非 rust-embedded 组织 |
| 最新版 | 0.1.3（2022-10，**已停更**） |
| 代码规模 | 505 行 / 3 文件（仅 trait 定义，无实现） |
| 许可证 | **LGPL-3.0-or-later** |
| 下载量 | 累计约 8000（近 90 天约 100） |

三重不合适：许可证 LGPL-3.0-or-later 不在 `src/rust/deny.toml` L6-18 的 allow 清单（MIT / Apache-2.0 / 0BSD / Unicode-3.0 / BSD-3-Clause / MPL-2.0）内，`cargo-deny` 会直接拒绝；内容上仅 505 行 trait，对现成 xHCI 栈零增益；维护上停更近四年、近乎无人使用。

### 5.3 其余 host 侧第三方（均不适用）

`cotton-usb-host`、`crab-usb`、`rp-pio-usb-host` 等均为 MCU / Embassy async 向实现，绑定特定微控制器（RP2040 等）或 async 执行器，不面向 QueenX 的 x86_64 / aarch64 通用平台。

## 六、若将来推进 Device 侧

一旦触发条件满足，正确路径应与 host 侧**同构**：由 framework 封装 UDC 原语（寄存器 / FIFO / 端点安全的 safe API），gadget 类功能实现在 services（0 unsafe）。这样可保持 F1 / F2 干净、TCB 不膨胀，并与现有 USB 栈风格一致——而非整包引入 `usb-device`。

## 七、结论

- **Host 侧**：维持自持。这是 framekernel 边界下的唯一合架构路径，且已建成，继续按既有方向补充 EHCI / UHCI / OHCI。
- **Device 侧**：暂不自持，条件触发（触发条件：USB gadget 角色需求 + 带 UDC 的目标平台）。
- **第三方库**：`usb-device`（方向相反、仅上半层）与 `usb-host`（LGPL、停更、仅 trait）均不引入；其余 MCU 向 host 栈不适用。
