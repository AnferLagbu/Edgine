# TrouBLE（BLE Host）引入路线与未来规划报告

> 总体判断：TrouBLE 是一个 **BLE Host 上半层**（HCI 之上的 GAP/L2CAP/SMP/ATT/GATT），它**本身不等于"有蓝牙"**——没有外部 Controller（链路层 + 无线电固件）经 HCI 传输接入，它一条包也发不出去。因此引入 TrouBLE 的真实成本不在"这个 crate 好不好用"（它质量不错，Apache-2.0 OR MIT、`no_std`、async-first、22766 行 Rust / 43 文件），而在于它牵出的三件内核侧前置件：**HCI 传输的 safe 代理、内核 async 运行时、以及可执行的虚拟 Controller 验证路径**。据此，本报告的结论与第三方库选型评估一致：**暂缓、条件触发（触发条件：蓝牙需求）**；一旦蓝牙从"想做"变成"要做"，建议按本报告分阶段推进，且第一阶段先补验证基座、再谈协议栈落地。

本报告是 Edgine 对 TrouBLE 引入路线与未来规划的一次性可行性快照。评估依据为 TrouBLE crates.io 官方元数据（`trouble-host` 0.8.0，2026-08-25 发布）与 Edgine 仓库当前事实（`src/kernel/Cargo.toml`、`scripts/audit_services_boundary.py`、`src/kernel/framework/` 与 `src/kernel/services/` 目录树）。作为后续制定 plan 与修复工程的输入依据。

## 一、TrouBLE 本体事实

以下元数据取自 crates.io 官方 API（`trouble-host`），非推介材料转述：

| 项 | 值 |
|---|---|
| crate 名 | `trouble-host` |
| 最新版本 | 0.8.0（2026-08-25 发布；首个版本 2022-09-07，累计 12 个版本） |
| 许可 | Apache-2.0 OR MIT |
| edition | 2021 |
| rust-version | 未在 crate 元数据声明（MSRV 无官方承诺） |
| 代码规模（0.8.0） | Rust 22766 行 / 43 文件（0.6.0 为 16234 行，0.7.0 为 21753 行） |
| 定位关键词 | `no-std`、`hardware-support`、`embedded` |
| 仓库 / 属主 | `https://github.com/embassy-rs/trouble`（owner `lulf`） |

默认 feature 集为 `peripheral`、`central`、`gatt`、`derive`、`default-packet-pool`、`extended-advertising`。需要关注的是其可选 feature 的**依赖传染性**：`security` 拉入 `p256`/`aes`/`cmac`/`rand_core`/`rand_chacha`/`rand` 六项加密依赖，`legacy-pairing` 又依赖 `security`，`derive` 依赖同仓库的 `trouble-host-macros` 过程宏 crate。这意味着"只要 central+peripheral 的最小可用形态"与"完整含配对加密的形态"，依赖面差异很大——引入时必须以 `default-features = false` + 显式列表的方式收敛，与项目对 smoltcp 的既有做法一致（`src/kernel/Cargo.toml` L47-61）。

**关键事实：TrouBLE 强绑 Embassy async 生态。** 它的 API 形态是 `async fn`，运行时语义依赖 `embassy-futures`/`embassy-sync`/`embassy-time`。Edgine 内核当前**没有任何 async 运行时**（见第五节），这是引入的第一道真实门槛，而非 F1/F2 边界规则。

## 二、定位与边界：Host 上半层 ≠ 有蓝牙

BLE 协议栈纵向分为三层实体：

- **Controller（链路层 + 无线电）**：负责广播/扫描/连接的时序、跳频、加密的链路层部分。**必须由硬件 + 独立厂家固件承担**，TrouBLE 不提供。
- **HCI（Host Controller Interface）**：Host 与 Controller 之间的传输协议，物理载体是 UART / USB / IPC。
- **Host**：GAP（发现与连接管理）、L2CAP（逻辑链路复用）、SMP（配对与密钥分发）、ATT/GATT（属性协议与数据库）。**TrouBLE 覆盖的是这一层。**

因此 TrouBLE 的完整运行链路是：`TrouBLE（Host）` → `HCI 传输` → `Controller 固件` → `无线电`。Edgine 现无蓝牙子系统（`src/kernel` 全树检索 `trouble`/`embassy`/`Executor`/`block_on` 零命中；`bluetooth` 亦零真实命中，`hci` 的命中均来自存储驱动 `ahci` 的误匹配）。这意味着引入 TrouBLE 是**从零起一条新子系统**，且只能覆盖其中一段——Controller 与 HCI 传输两端都要自己补。

**这个边界决定了成本判断**：TrouBLE 的质量与 Edgine 引入它的工程量之间几乎没有关系。工程量集中在 Controller 接入与 HCI 传输这一侧，而非 Host 协议逻辑。

## 三、与 Linux 蓝牙切分的对照

Linux 的蓝牙实现**横跨内核态与用户态**，且分界线与 TrouBLE 的边界并不重合：

- **内核态**：`net/bluetooth/`（L2CAP、RFCOMM、BNEP、HIDP、SCO、SSP/SMP）+ `drivers/bluetooth/`（`btusb`、`hci_uart` 及 `btintel`/`btrtl`/`btmtk` 等厂家固件加载）。
- **用户态**：BlueZ（`bluetoothd`/`obexd`）负责 GAP 策略与适配器管理、**ATT/GATT 数据库**、A2DP/HFP/HID 等 profile，以及 D-Bus 接口。

对照可见：TrouBLE 的覆盖范围（GAP/L2CAP/SMP/ATT/GATT）**横跨了 Linux 的内核/用户态分界线**——L2CAP/SMP 在 Linux 内核侧，GAP/GATT 在用户态侧。这直接对应 Edgine 的两条候选路线：

| 路线 | 落点 | 参照系 | 对 TCB 的影响 |
|---|---|---|---|
| A．整体落 `services/` | vendored `services/ble/trouble/`，0 unsafe | 接近 Linux（协议栈在核内），但整体更靠上 | TCB 不增（vendored 编译单元不算自有 TCB 代码），但需 `#![deny(unsafe_code)]` 边界确认 |
| B．整体落用户态 | 用户态进程内跑 TrouBLE，内核仅提供 HCI 传输通道 | 更接近 microkernel 式切分，TCB 更小 | 内核侧仅增 HCI 传输 safe 代理，TCB 增量最小 |

两条路线在本项目均无既有否决项；选择取决于"Edgine 是否要走核内蓝牙"这一方向决策（属用户决策范畴，见 AGENTS.md §9.1），本报告不代替此决策。

## 四、framekernel 合规性

**F1（services 0 unsafe）与 F2（services 禁访 framework 内部）均不构成阻断。** 依据：

- 第三方 crate 是**独立编译单元**，`#![deny(unsafe_code)]` 只约束项目自有源码，不追溯依赖 crate。项目已有同型先例：vendored smoltcp 位于 `src/kernel/services/net/smoltcp/`。
- 边界审计脚本 `scripts/audit_services_boundary.py` 已具备 vendored 豁免机制：L306-308 的 `VENDORED_EXCLUDE` 以绝对路径前缀匹配跳过整个 vendored 目录，注释明确"添加新 vendored 库时，追加 `Path` 即可"。引入 TrouBLE 若走路线 A，只需在此追加一条 `Path('src/kernel/services/ble/trouble')`。
- services 侧已有可用的 framework 安全面：`SAFE_FRAMEWORK_APIS`（L119-150）白名单已含 `framework::timer`（时钟）、`framework::iomem`/`ioport`（MMIO/PIO 代理）、`framework::irqline`、`framework::dma_buf`——这些正是 HCI 传输（UART/USB 寄存器访问）所需的安全代理面。

**真正的约束来自 TCB 纪律（AGENTS.md §2.2、§4.1）与 F9（死代码零容忍）**，而非边界硬规则：若为 TrouBLE 在 `framework/` 新增 unsafe 层，需说明其对 TCB 占比的影响并配套 `// SAFETY:` 注释（F4）；若暂不启用某 feature 而引入其类型，须通过实现使用路径消除，不能留 `#[allow(dead_code)]`。

## 五、落地缺口清单

按"引入 TrouBLE 必须先补什么"排序，共四项，其中前两项是真正的阻塞项：

| # | 缺口 | 现状证据 | 阻塞级 |
|---|---|---|---|
| 1 | **内核 async 运行时缺失** | `src/kernel` 全树 `Executor`/`block_on` 零命中；唯一 `wake_by_ref()` 在 `services/ipc/async_ipc.rs:93`，属手写 `Future`（`Poll::Ready`/`Poll::Pending` 均自带），无可复用调度器 | 阻塞 |
| 2 | **中断 → waker 唤醒入口缺失** | 现有范式是同步轮询 + trait 注入（`services/net/smoltcp_impl.rs` 的 `poll(ts_ms)`/`poll_at()` + `framework/net/init.rs::poll_network()`），无"中断唤醒 executor"通路 | 阻塞 |
| 3 | HCI 传输 safe 代理 | `framework::iomem`/`ioport`/`irqline` 已在 `SAFE_FRAMEWORK_APIS` 白名单；字符设备侧 `framework/driver/char/mod.rs` 已有物理基址查询面。基本就绪，缺的是把它组织成 HCI 传输抽象的薄层 | 非阻塞 |
| 4 | HCI 所需时钟源 | `framework/timer` 已有 safe 时钟面：`get_time_ms()`/`get_uptime_ms()`（`calibration.rs` L311、`tick.rs` L316）、`get_adjusted_time_ns()`（`time_sync.rs` L364）。**缺口小于早前判断** | 非阻塞 |

一句话：**缺的不是协议栈，是跑得动协议栈的运行时与中断唤醒通路。** 缺口 1、2 是 Edgine 目前完全不具备的能力，且与蓝牙本身无关——它们对任何 async 驱动（USB gadget、异步块设备）都是共用的。这提示一个正确的推进顺序：**若要为 TrouBLE 建 async 运行时，应作为一个独立的通用内核能力立项，而非夹带在蓝牙工程里。**

## 六、验证路径

Linux 蓝牙测试体系给出的最有价值的一条经验是：**协议栈内部不写单元测试，注入点在 HCI/mgmt 边界。** 它靠三根支柱支撑：

1. **虚拟控制器**：内核 `hci_vhci`（`/dev/vhci`）+ 用户态 `btvirt` + BlueZ `emulator/` 的 `btdev.c`/`bthost.c`/`hciemu.c`。
2. **tester 套件**：`mgmt-tester`/`l2cap-tester`/`smp-tester` 等，配 `tools/test-runner`，在 QEMU 内运行。
3. **抓包与模糊测试**：`btmon`（btsnoop）、syzkaller/syzbot、VirtFuzz、FuzzBT。

映射到 Edgine 的门槛体系（AGENTS.md §2.3），可落地为：

- `framework/` 侧做**虚拟 HCI 传输**（对应 `hci_vhci`），让 HCI 传输这层可在无真实无线电时被驱动。
- `host-tests/` 侧做**模拟 Controller**（对应 `btdev`/`bthost`），在标准测试进程内喂 HCI 事件与 ACL 数据，从而对 TrouBLE 的 GAP/L2CAP/ATT/GATT 逻辑做端到端测试。
- QEMU 集成门槛（对应 `test-runner`）作为最终验收。

**核心结论：没有虚拟控制器，§2.3 的 QEMU 集成门槛对蓝牙就是空的。** 因此任何"推进蓝牙"的计划，第一步都应该是验证基座，而不是引入协议栈。这也与第五节的判断互相印证——先补运行时与验证通路，再谈协议栈落地。

## 七、触发条件与阶段划分

本报告维持第三方库选型评估报告的结论：TrouBLE **暂缓、条件触发，触发条件为"蓝牙需求"**。在触发条件满足前不做任何引入动作。

触发后建议的分阶段路径（每阶段均以可验证目标收口，成功标准强度按 AGENTS.md §12.3 在开工前与用户确认）：

- **阶段零（前置，与蓝牙解耦）**：以独立工程设计内核 async 运行时 + 中断唤醒通路（缺口 1、2）。这是通用能力，即使蓝牙最终不做也有价值。此阶段不引入 TrouBLE。
- **阶段一（验证基座）**：实现虚拟 HCI 传输（framework 侧）+ 模拟 Controller（host-tests 侧），把 §2.3 的蓝牙验证门槛从"空"变成"可执行"。此阶段仍不引入 TrouBLE。
- **阶段二（协议栈落地）**：在阶段零、一的成果上引入 `trouble-host`，按 `default-features = false` + 显式 feature 列表接入；落点按第三节路线 A/B 由用户决策。以 `gatt`+`central`+`peripheral` 的最小可用形态起步，`security`/`legacy-pairing` 按需后置。
- **阶段三（Controller 接入）**：真实 Controller 与 HCI 传输（UART/USB/IPC）接入。这一阶段的硬件依赖与固件依赖超出软件仓库范围，需单独立项。

后续据此制定 plan 时，建议以第五节编号 # 为缺口索引、第六节三条映射为验证索引、第七节四个阶段为推进索引。需强调：**阶段零与阶段一先于任何 TrouBLE 引入动作**——若跳过它们直接引入协议栈，得到的是一堆无法被 §2.3 门槛验证的代码，这违反 AGENTS.md §12.4"目标驱动执行"。
