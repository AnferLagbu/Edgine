# 第三方库选型清单可行性评估报告

> 总体判断：这份清单本质是**一套完整的嵌入式 MCU 固件栈**——从 no_std 网络、掉电安全 Flash 文件系统、NOR Flash KV、USB device、HCI BLE Host，一路到传感器抽象（embedded-hal）与 Embassy 生态。它与 Edgine 当前"x86_64 + aarch64 通用内核 + framekernel"的定位只在**上半部分**天然重合：smoltcp 早已 vendored 采用、RustCrypto（ed25519-dalek）已在用、heapless 已作为 smoltcp 的非可选依赖进入依赖图。而其余六项（littlefs2-rust / SLATE / usb-device / TrouBLE / embedded-hal / defmt / linked_list_allocator）要么指向 MCU 单板固件场景、要么与项目已建成的等价设施正面冲突，**不宜按清单原样引入**。

本报告是对第三方库选型清单的一次性可行性快照。评估依据为 Edgine 仓库当前事实（`src/kernel/Cargo.toml`、`src/kernel/Cargo.lock`、`src/kernel/services/` 与 `src/kernel/framework/` 目录树），不依赖外部推介材料。作为后续制定 plan 与修复工程的输入依据。

## 一、评估范围与方法

清单共 10 项，逐项对照三类事实来源：

- **已入依赖图事实**：`src/kernel/Cargo.toml` 的 `[dependencies]` 与 `Cargo.lock`
- **已有等价设施**：`services/`（含 `fs/`、`driver/usb/`、`net/smoltcp/`）与 `framework/`（含 `klog`、`alloc/`）目录树
- **架构约束**：framekernel 分野（`framework/` 允许 unsafe / `services/` 100% safe）、TCB 占比软约束（AGENTS.md §2.2）、services 零 unsafe 硬规则（§5 F1/F2）

按此将 10 项归为三档：**已采用/方向一致**、**与既有架构冲突**、**属嵌入式 MCU 栈、暂不在当前射程**。

## 二、已采用或方向一致（3 项）

**smoltcp —— TCP/IP 网络。** 已 vendored 于 `src/kernel/services/net/smoltcp/`，版本 0.14.0（edition 2024，rust-version 1.91，0BSD），经 `src/kernel/Cargo.toml` 的 path 依赖接入，`default-features = false` 并按需勾选 13 个功能 feature。`services/net/smoltcp_impl.rs` 是 services 层唯一允许直接触达 smoltcp 类型的翻译层。这意味着清单的 TCP/IP 条目**早已落地为既定事实**，无需任何动作。此处推介的"poll 驱动、无 runtime、no_std"三项特征，与项目现状完全一致。

**RustCrypto —— 密码原语。** 项目当前已在用 `ed25519-dalek 3`（DECISION-078，`default-features = false, features = ["fast"]`）做真实 Ed25519 签名验证。RustCrypto 是 no_std 友好的模块化 crate 生态，其"按需引入子 crate"的形态与 services 层 0 unsafe 的定位契合。可行路径是**未来按需追加具体子 crate**（如哈希/HKDF/AES），而不是引入某个伞包；现有 ed25519-dalek 已覆盖签名验证需求，本项**方向一致、无新增动作**。

**heapless —— 静态容器。** 无堆容器（固定容量 `Vec`/`String`/`spsc` 等）。关键事实：smoltcp 的 `Cargo.toml` 第 30 行以**非可选依赖** `heapless = "0.9"` 引入，`src/kernel/Cargo.lock` 已锁定 `heapless 0.9.3`，即 heapless **已经在 kernel 的依赖图内**（经 smoltcp 传递）。因此对内核固定容量路径（中断上下文、early boot、per-CPU 结构、无堆缓冲区）而言，**这是清单中最值得直接采纳的一项**，且引入成本近乎为零——只需把"经 smoltcp 间接可用"升级为"kernel 显式直接依赖"。落点建议在 `framework/`（需要时才允许），或优先封装成 safe API 供 `services/` 使用。

## 三、与既有架构冲突（2 项）

**defmt —— 日志。** 与项目自研 `framework::klog`（`src/kernel/framework/mod.rs` 声明 `pub mod klog;`）功能重叠。日志是 framework 的基础设施，替换会牵动 TCB 且无对应收益。`defmt` 在 smoltcp vendored 副本中是**可选依赖**（第 28 行 `defmt = { version = "1", optional = true }`），Edgine 的 feature 列表**未启用**它，说明项目有意保持自研日志路径。结论：**不建议替换**；若仅需结构化调试输出，可在 klog 内部借鉴思路，不引入 crate。

**linked_list_allocator —— 堆分配器。** 与项目已建成的全局分配器正面冲突：`src/kernel/memory_allocator.rs` 中 `unsafe impl GlobalAlloc for KernelAllocator`（第 27 行）并以 `#[global_allocator]`（第 118 行）注册，配套 `framework/alloc/slab_alloc.rs` 的 slab 层。全局分配器属内核关键路径，替换是重大架构决策，**不能因一份外部清单就动**。若确要评估替代方案，应单独立项并给出论证与迁移路径。

## 四、属嵌入式 MCU 栈、暂不在当前射程（5 项）

以下五项共享同一前提：**嵌入式单板固件场景**。Edgine 当前定位是通用内核，且这些方向要么已有等价设施、要么方向相反：

- **littlefs2-rust（文件系统）**：项目 `services/fs/` 已建成一套完整的块/内存文件系统家族——`ramfs`/`tmpfs`/`ext2`/`exfat`/`unkfs`（含 SPA/DMU/ZIL/RAIDZ 等 ZFS 风格分层）/`overlayfs`/`initramfs`/`devfs`/`sysfs`/`procfs`/`configfs`/`cgroupfs`/`virtiofs`，另有 `vfs_manager`/`dcache`/`inode`/`file_ops` 等 VFS 骨架。littlefs 的"掉电安全 + 块设备抽象"在通用内核场景并非缺位项（ext2/exfat 已覆盖块设备路径）。**仅在出现"受限 Flash 镜像、掉电安全优先"的新需求时才有讨论价值**。
- **SLATE（KV 存储）**：no-alloc、NOR Flash 原子写入。这是比 littlefs 更窄的 MCU 场景（无文件系统、直接 KV-on-NOR）。当前项目无该类需求，**暂不在射程**。
- **usb-device（USB 设备）**：项目 USB 栈在 `services/driver/usb/`（`xhci`/`enumerate`/`hid`/`mass_storage`/`ring`/`usb_core`），全部是**host 侧**；而 usb-device 是 **device/gadget 侧**库，两者方向相反。是否引入**完全取决于项目是否要做 USB gadget 角色**（如模拟 U 盘/键盘给外部主机），这是一个独立的功能决策，而非"换库"。
- **TrouBLE（BLE Host）**：HCI 抽象、可脱离 Embassy。属于 BLE 无线栈，Edgine 现无蓝牙子系统。**暂不在射程**。
- **embedded-hal（外设抽象）**：用于复用海量传感器驱动，是 MCU 生态的驱动接口层。它与 MCU 的 GPIO/I2C/SPI 抽象强绑定，对通用内核的 x86_64/aarch64 平台意义有限。**暂不在射程**。

一句话：这五项是"若有嵌入式支线才评估"，不是"主线可选库"。

## 五、横向观察

**1. 清单整体更适配 MCU 固件而非通用内核。** 10 项里有 5 项（littlefs2-rust / SLATE / usb-device / TrouBLE / embedded-hal）在功能上互相咬合，共同构成一个 Cortex-M 级固件方案（并带 Embassy 生态色彩，如 TrouBLE）。这与 Edgine 的多核 SMP 通用内核定位是两条不同的技术路线。**应整体审视"是否有嵌入式支线"，而非逐库往主线里塞。**

**2. TCB 膨胀风险。** framekernel 的核心是 TCB 最小化。若把这些第三方 crate 引入 `framework/`，会直接推高 TCB 占比（§2.2 软约束 < 30%）。若要采纳其中的库，应尽量将其**置于 `services/`（0 unsafe）**，或由 `framework/` 先封装成 safe API 再供 services 使用——这既是硬规则要求，也是 TCB 纪律要求。

**3. no_std 前提已被项目满足。** 清单里"no_std / 无 heap / poll 模型"等卖点，对 Edgine 而言不是加分项而是**既有约束**：内核本体即 no_std，且已有 `KernelAllocator` + slab 的自研堆方案。这进一步说明清单多数条目的价值点与项目现状重叠或错位。

## 六、结论与后续建议

- **可直接受益**：`heapless`——已在依赖图内，建议显式纳入 `framework/`（或封装 safe API 下探到 services），服务中断上下文与无堆路径。
- **维持现状**：`smoltcp`（已 vendored 采用）、`RustCrypto`（ed25519-dalek 已用，未来按需加子 crate）。
- **不建议引入**：`defmt`（与 `framework::klog` 冲突）、`linked_list_allocator`（与自研 `KernelAllocator` 冲突）。若有替换意向，须单独立项论证，不得夹带。
- **暂缓、条件触发**：`littlefs2-rust`（触发条件：掉电安全 Flash 镜像需求）、`SLATE`（触发条件：NOR Flash KV 需求）、`usb-device`（触发条件：USB gadget 角色需求）、`TrouBLE`（触发条件：蓝牙需求）、`embedded-hal`（触发条件：嵌入式支线确立）。

后续若据此制定 plan，建议以第五节两条风险线（TCB 边界、路线定位）为约束前提；在"嵌入式支线"未确立前，只推进 `heapless` 一项即可，其余留作条件触发的候选。
