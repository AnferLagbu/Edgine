# EGDF 驱动框架评估

> 结论：EGDF（Edgine Generic Driver Framework）是一个**设计成立、纪律良好、但生态只接入了三分之一**的自研驱动框架。约 4,090 行代码中，真正跑在启动与 I/O 路径上的是「驱动注册 + 块设备 I/O + 字符设备占名」；`devtree`、`composite`、`user_driver`、`proto_net` 四个分支属于「实现完整、无生产者 / 无入口 / 无注册者」。最实的一条缺陷不是缺功能，而是**行为错误**：`driver.init()` 的失败返回值被丢弃，设备仍被标成 `Ready`。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读：源码阅读与静态检索，未构建、未运行。

## 一、定位与结构

`privileged/egdf/mod.rs` 的文件头把 EGDF 定义为“`Edgine` 的统一设备驱动模型 — 唯一的注册/发现/初始化/I/O 入口”。实际分层：

| 层 | 位置 | 行数 | 内容 |
|---|---|---|---|
| API 层 | `privileged/egdf/mod.rs` | 1,322 | `EGDF_DEVICES` 注册表、`EGDFDevice`、`Driver` / `BlockDevice` trait、`EGDFOps` 静态操作表 |
| 协议族 | `privileged/egdf/proto_{block,char,input,net}.rs` | 209 | 类 Linux `struct ops` 的函数指针表；注释称“静态分发，无 vtable 开销” |
| 设备树 | `privileged/egdf/devtree.rs` + `functions/egdf/devtree.rs` | 787 | `EGDFNode` / `Property` / `NodeId`、compatible 匹配、绑定与查询 |
| 复合设备 | `privileged/egdf/composite.rs` + functions 封装 | 522 | RAID0 条带 / RAID1 镜像探测 |
| 用户态驱动 | `privileged/egdf/user_driver.rs` + functions 封装 | 603 | bind / unbind / map / unmap / forward_irq / cleanup |
| 固件 | `privileged/egdf/firmware.rs` | 152 | 固件 blob 描述 |
| 安全代理 | `functions/egdf/mod.rs` 等 | 1,050 | 强类型 `DeviceId` / `Result`，`#![deny(unsafe_code)]` |

纪律面：privileged 侧 `#![deny(missing_docs)]`，functions 侧 0 unsafe，错误类型收敛到 `KernelError`（TD-20）。这部分与项目整体风格一致，没有放水。

## 二、真实接线的部分

驱动注册是真调用链，不是空壳：

- PCI 总线：`privileged/driver/bus/pci.rs:53` 调 `egdf_register_driver`；
- 显示：`privileged/driver/display/mod.rs:394`；
- 字符设备：`functions/driver/char/mod.rs:36-61`（vga / serial）、`char/pl011.rs:12`（aarch64）；
- 块设备：`proto_block::register_block_device`，EGFS 经 `egdf_blk_read` / `egdf_blk_write` 落到 `BlockDevice::blk_read/write`；
- 热插拔重枚举：`privileged/driver/mod.rs:179` 调 `devtree_probe_composites()`。

注册表形态：`Vec<EGDFDevice>` + `IrqSpinLock`，线性查找；`DeviceId` 由 `NEXT_DEVICE_ID.fetch_add` 单调分配；注销走 `devices.remove(pos)`（`mod.rs:514`）。

## 三、缺口

### 3.1 `init()` 失败被吞，设备仍标 Ready

`privileged/egdf/mod.rs:881-890`：

```rust
let _ = driver.init();
...
state: DeviceState::Ready,
```

初始化返回值被 `let _ =` 丢弃，随后无条件写入 `DeviceState::Ready`。三条后果：

1. 初始化失败的设备对外仍表现为可用，故障推迟到第一次 I/O；
2. 注册函数返回的 `id` 是成功语义，调用方无法据此判断设备是否真的就绪；
3. host 测试只断言“注册后状态应为 Ready”（`host-tests/tests/egdf_registry_io_host_test.rs:132`），没有覆盖 init 失败路径，因此这条行为长期不会被测试发现。

这是本报告中唯一属于“已有功能行为错误”而非“功能未接”的缺陷，也是与 `fake-success-syscall-assessment.md` 同一模式（返回成功、语义未兑现）的驱动层实例。

### 3.2 devtree 零生产者，composite 恒判 0

- `devtree_create_node_impl`（`privileged/egdf/devtree.rs:201`）在 `src/kernel` 内**零调用**；
- `devtree_bind_device`（`:322`）零外部调用；
- `devtree_find_compatible` / `devtree_children` / `devtree_walk` 因此没有可查询的数据。

`composite.rs:271-286` 的 `devtree_probe_composites()` 依赖 `devtree_walk` 查找 `eg,raid0` / `eg,raid1` 节点；节点集为空时打印 `no composite device tree nodes found` 并返回 0。

因此 **devtree（787 行）+ composite（522 行）共 1,309 行处于“实现完整、无数据源”状态**，且这是因果链而非两个独立缺口：devtree 不填充，composite 永远不产生设备。

### 3.3 aarch64 FDT 与 EGDF devtree 无桥接

aarch64 的设备发现走 `privileged/dtb.rs`：`boot/aarch64/entry.rs:174` 调 `dtb::parse`，`psci.rs:26` 取 PSCI 方法，`smp_init.rs:129` 取 CPU 拓扑。全仓检索 `egdf` 与 `dtb` 的交叉为**零**。

即：EGDF 声称是“唯一的注册/发现入口”，但 aarch64 的发现路径完全绕开它；两套设备描述结构（FDT 解析结果 vs `EGDFNode` 树）并存，且都不指向对方。

### 3.4 user_driver 无 syscall 入口

`privileged/egdf/user_driver.rs` 提供六个公开入口：

- `devtree_bind_user_device`（:63）
- `devtree_unbind_user_device`（:113）
- `devtree_map_user_device`（:184）
- `devtree_unmap_user_device`（:291）
- `egdf_forward_irq`（:359）
- `egdf_process_cleanup`（:428）

functions 层（`functions/egdf/user_driver.rs`）已封装成强类型 safe API，但 `functions/syscall` 与 `privileged/syscall` 目录内**无任何调用**。唯一被接入的是 `egdf_process_cleanup`，以函数指针形式登记进进程清理路径（`privileged/egdf/mod.rs:950-953`）。

结论：用户态驱动这一能力在代码上完整、在 ABI 上不可达。

### 3.5 proto_net 无注册者

`privileged/egdf/mod.rs` 文件头写“`net::e1000/virtio` —— 通过 `proto_net` 注册网卡”。实际检索：

- `NETOps` 类型定义与 `EGDFOps::Net` 构造存在（`mod.rs:80`、`:181`、`:269`、`:469`、`:1126`）；
- 全仓**没有** `egdf_register_net` 一类的注册调用；
- 网卡实际走 `privileged/net` + smoltcp，不经过 EGDF 注册表。

因此 `proto_net`（78 行）目前是“有协议表、无设备”的声明面。

### 3.6 字符设备无 I/O 绑定

`functions/driver/char/mod.rs:30` 自述：

> `SIMPLIFIED: 注册走 egdf_register_driver` (无 `CharOps` 读写绑定)——EGDF char …

即字符设备只在注册表里占一个 `Char` 类型的名字与状态，没有绑定读写操作。调用方无法通过 EGDF 对字符设备发起 I/O，只能走各自的专用路径。

### 3.7 注册表无上限，文档承诺不执法

文件头注释称“注册表: `Vec<EGDFDevice>` + spinlock, O(N) 查找 (N ≤ 64)”，但 `egdf_register_driver` 的 `devices.push(dev)`（`mod.rs:899-901`）**没有容量检查**，`Vec` 无界增长。

“N ≤ 64”是描述性承诺，不是约束；设备数超限不会报错，只会让 O(N) 查找继续变长。这与 `hardcoded-capacity-limits-assessment.md` 中的 C 类（文档与行为不一致）同构，方向相反：那里是常数存在但不一致，这里是常数不存在但被文档引用。

### 3.8 热插拔的用户态通知未实装

`privileged/driver/hotplug.rs:17` 注释：“未来: 用户态通知 (/dev/hotplug)”；同文件 `:139` 对非 x86 架构返回“PCIe hotplug not supported on this architecture”。前者是承诺，后者是显式能力边界——两者都不是缺陷，但都没有进入任何可追踪清单。

## 四、测试覆盖现状

引用 EGDF 的 host 测试共 5 个：

- `egdf_registry_io_host_test.rs`
- `egdf_proto_proxy_host_test.rs`
- `i43_block_bridge_test.rs`
- `hotplug_reenum_contract_test.rs`
- `td20_functions_kernel_error_test.rs`

`egdf_registry_io_host_test.rs` 的断言集中在契约面：两次注册分配不同 id（:100）、注册后总数（:101）、`state == Ready`（:132）、`set_state` 生效（:141）、首次注销成功（:144）。

未覆盖的路径：

- `init()` 失败 → 设备状态与注册返回值；
- devtree 节点创建 → compatible 匹配 → 绑定 → composite 探测的完整链路；
- `user_driver` 的 bind/map/unmap/forward_irq（因无 syscall 入口，无法从外部驱动）；
- `proto_net` 注册与 I/O（因无注册者）。

测试口径与项目整体一致：**契约锁充分，行为链缺失**。

## 五、与其它缺口的关系

- `3.1`（init 被吞）与 `fake-success-syscall-assessment.md` 的五条假成功同源：返回成功，语义未兑现；
- `3.2`（devtree 零生产者）与 `subsystem-wiring-coverage-assessment.md` 的“已实现面 > 已接线面”同源：结构完整，无入口；
- `3.7`（N ≤ 64 承诺）与 `hardcoded-capacity-limits-assessment.md` 的 C 类同源：文档与代码不互为约束；
- `3.3`（FDT 无桥接）属于架构分叉：两套发现机制并存，且 EGDF 的“唯一入口”定位与实际不符。

## 六、对计划的意义

EGDF 的处置不应从“补齐 devtree / composite / user_driver”开始，而应按“是否掩盖故障”排序：

1. **先修 3.1**：`init()` 失败必须落到 `DeviceState::Failed`，并让注册结果可判别（返回错误或提供状态查询）。这是唯一一条会让现有可用路径产生错误结论的缺陷。
2. **再决定 devtree 的去留**：`3.2` 与 `3.3` 是同一个决策的两面——要么把 aarch64 FDT 解析结果桥接进 `EGDFNode` 树并让 composite 真正工作，要么明确 EGDF devtree 为“未启用分支”并从能力宣称中移除。继续维持“实现完整、无数据源”的中间态，会让 1,309 行代码与两处文档承诺同时失真。
3. **user_driver 与 proto_net 需要单独裁决**：两者都是“框架先行、ABI 后置”。在 0.1 阶段它们与外部程序试点无关，可登记为后续批次；但不应继续出现在“EGDF 已支持”的表述里。
4. **注册表上限二选一**：把 `N ≤ 64` 变成真实约束（满时返回错误），或删除该承诺。文档与代码必须互为约束，否则下一个维护者会按错误前提扩容。

最后一点值得单独记录：`docs/design/` 目前没有 EGDF 的设计立场文档。一个有 4,090 行、六个分支、且在文件头自称“唯一入口”的框架，其边界（哪些是已启用、哪些是预留、为何 user_driver 未接 ABI）只存在于源码注释里——这正是 `comment-promise-inventory.md` 所指的“承诺只活在注释中”的结构性问题。
