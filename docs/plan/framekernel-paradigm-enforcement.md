# F/S 分层范式落实与反向依赖全面整治

> **优先级（2026-09-11 用户裁决）：本工程优先于分册 9**。理由：范式落实（归属判据 + 依赖方向）直接影响后续一切开发的代码归属与接口设计，是分册 9（死代码/TODO 治理）及其后工程的前置。分册 9 的 B09-13 反向依赖治理被本工程 §7 吸收。

> 工程定位：独立架构工程。落实 Asterinas framekernel 范式（机制/策略分离 + Minimalism + 依赖单向），全面整治 framework→services 反向依赖。**实施由 AI 全权接手（用户委派），用户审查。**

## 1. 背景与依据

描述：QueenX 当前 framework 承担功能层（driver/net/fs-vfs/proc/syscall/ipc/wasm），TCB 占比 60.1%（目标 <30%），framework→services 反向依赖 136 处/78 文件，与 Asterinas framekernel 范式偏离（功能应在 services、framework 仅机制契约与安全代理、依赖单向）。
方案：按 Asterinas 范式整治——功能下沉 services、framework 只留机制契约与安全代理、依赖经 trait 注入解决。
详情：
- 依据 1：Asterinas APSys'24 论文 OSTD 四准则（Soundness / Expressiveness / **Minimalism** / Efficiency）。
- 依据 2：`other/asterinas-0.18.1/` 源码实证——kernel/core 含全部功能（device/fs/net/process），ostd 仅机制（io_mem/io_port/dma/mm/task/sync/arch），kernel `#![deny(unsafe_code)]` 0 unsafe，驱动经 `ostd::mm::IoMem/VmIo` 安全访问硬件。
- 依据 3：AGENTS.md §4.1 + explain-framekernel.md（2026-09-11 补全归属决策树 Q1/Q2/Q3）。

## 2. 核心判据（归属决策树）

描述：模块归属判据（与 AGENTS.md §4.1 一致）。2026 年审核员复核后修正：**"0 unsafe" 只说明"不必须 framework"，不决定归属；归属看服务对象**（见 Q1' 服务对象准则）。
方案：
```
Q1: 该功能必须 unsafe 吗（直接碰硬件/页表/裸内存）？
 ├─ 否 → Q1': 服务对象是谁？（服务对象准则，审核员 DECISION-F 定案）
 │       ├─ framework 机制对 services 的安全导出面 → "保留"
 │       │     （框架封装 unsafe 为 safe API 供 services 消费 = 合法且核心的交互模式，
 │       │       例: IoMem::from_pci_bar / userptr safe 构造 / proto_block::register_block_device）
 │       ├─ 仅被 services 内部消费 / 经既有分发机制（dispatch_trait）消费 → "下沉"
 │       └─ 被 framework 机制直接调用 → 下沉需接口化（trait/回调/Chitin 注册），否则 "保留"
 └─ 是 → Q2: 它是"机制"还是"功能"？
       ├─ 机制（页表/切换/寄存器原语/同步/安全代理/FFI 边界）→ "保留"
       └─ 功能（驱动/FS/网络/进程/signal/syscall）→ Q3
             Q3: 能否封装为 safe API 供 services 用？
              ├─ 能 → "封装+下沉"（framework 留机制原语 + IoMem/IoPort/DmaStream/UserPtr safe API，功能迁 services）
              └─ 不能（self-referential / FFI ABI / 中断上下文）→ "保留薄层"
```
另附两个判定：**壳**（纯 re-export，0 unsafe 几行）→ 删壳；**双份**（services 已有 0 unsafe 权威实现）→ services 权威、framework 删业务。
要点：**"要 unsafe" ≠ "放 framework"**——功能要 unsafe 也应由 framework 封装 safe API 后实现在 services（Minimalism 落地关键）；**"0 unsafe" ≠ "放 services"**——framework 的安全 API 出口（机制的"嘴"）留在 framework，下沉的永远是使用它的功能，而不是提供它的接口。

**服务对象准则（2026-09-11 补）**：`0 unsafe` 只说明"不必须 framework"，**不决定归属**——归属看**服务对象**：
- 文件是 **framework 机制对 services 的安全导出面**（如 chitin 注册包装 `register_block_device`、proc/mechanism.rs、IoMem::from_pci_bar）→ **保留 framework**（框架提供安全 API，services 消费 = 合法方向 services→framework）
- 文件仅被 **services 内部使用 / 经既有分发机制**（dispatch_trait）消费 → **下沉**
- 文件被 **framework 机制直接调用** → 下沉需接口化（trait/回调/Chitin 注册），否则保留

## 3. 工程目标（验收标准）

描述：达成 Asterinas 范式对齐。
方案：
1. [ ] TCB 占比 < 30%（阶段 6 实测 56.7%，未达标；framework 110,637 vs services 79,821 LoC raw，smoltcp 排除，见 `docs/report/framekernel-paradigm-validation.md`）
2. [X] framework→services 反向依赖 = 0（生产口径 = framework 生产代码引用 `crate::services`；经 `audit_reverse_deps.py` 核验 0 文件/0 行，原 136 处/78 文件已全部下沉/反转/收敛/按 §7.3 豁免）
3. [X] 功能层 services 权威（driver / fs 核心 / net 策略 / proc 策略 / syscall 业务；阶段 6 核验通过，见 `docs/report/framekernel-paradigm-validation.md`）
4. [ ] framework 仅机制/契约/安全代理（阶段 6 实测 framework `.rs` 300 个，未收敛至 §6.6 规划的 200；见 §6.6）
5. [X] services 保持 0 unsafe（F1 不回归；阶段 6 核验 0，真实 unsafe 仅 vendored smoltcp 3rd-party 锁定子树）
6. [X] §2.3 五条验证门槛（双架构 0w0e / clippy 0 / 核心审计 / host-tests / QEMU；阶段 6 全绿）

## 4. 治理原则

描述：治本双动作，禁止治标。
方案：
- 动作 1：**功能下沉 services**（清单 §6：下沉 20 + 封装+下沉 23 + 部分下沉 11 + 双份合并 20）。
- 动作 2：**framework 机制与 services 交互全 trait 注入**（清单 §7：壳删 82 + 直接 use trait 化 ~20）。
- 禁止：复用 B09-12 / B04-AUDIT-005 的"把 services 功能拉进 framework"模式——该模式让依赖方向"看似正确"但 **TCB 膨胀、机制/策略混淆**，违反 Minimalism。
- 依赖方向靠 trait 注入（framework 定义契约 + services 注册）解决，不靠移动实现代码。

## 5. 决策登记

描述：本工程推翻既往两处治理方向（用户 2026-09-11 裁决）。
方案：
- **DECISION-A（VFS 4 文件下沉）**：`fs/vfs/vfs.rs`（VfsManager）、`dcache.rs`、`types.rs`、`open_file_table.rs` 按 Asterinas 严格判据下沉 services——**推翻 B09-12/DECISION-H13 的 VFS 迁回**（B09-12 中仅 Errno/error 基础库迁回 framework 正确，保留）。依据：Asterinas 的 VFS 在 kernel/services（`kernel/core/src/fs/vfs`），OSTD 不含文件系统抽象。
- **DECISION-B（E1000 业务回迁）**：`driver/net/e1000.rs` 业务回迁 services，framework 留 `dma_ring.rs`（DMA 描述符机制）+ E1000Io（IoMem 封装）——**推翻 B04-AUDIT-005 的 E1000 上移**。依据：驱动是功能，经 safe API 在 services 实现。**（阶段 3 落地：业务收敛为 services 单一 `E1000NetDriver`，见 §8「阶段 3 执行记录」）**
- 统一原则：**依赖方向靠 trait 注入，不靠移动实现代码**。

## 6. 逐文件下沉清单（framework 363 + services 260+ 文件逐文件判定，2026-09-11 调研）

### 6.1 下沉（0 unsafe，服务对象准则复核终局）——20 文件 → 3 确认下沉 + 17 保留

> 终局（DECISION-F，审核员裁决）：原"0 unsafe → 直接下沉"判定作废。按服务对象准则复核：
> - ✅ **3 确认下沉**：syscall brk/canary/posix_timer（经 dispatch_trait，无 framework 内部调用残留）——已提交 `c6455358`
> - 🔒 **17 保留**：其余全部被 framework 机制直接调用或为机制安全导出面（逐文件依据见下表现"复核结论"列）
> - 审计白名单（如 audit_block_registration）仅在归属变更后同步路径，禁止先改白名单再迁移

| 文件 | 下沉目标 | 依据 |
|---|---|---|
| framework/syscall/brk.rs | services/syscall/brk | ✅ 确认下沉：经 dispatch_trait 分发 |
| framework/syscall/canary.rs | services/syscall/canary | ✅ 确认下沉：经 dispatch_trait |
| framework/syscall/posix_timer.rs | services/syscall/posix_timer | ✅ 确认下沉：经 dispatch_trait |
| framework/net/init/dns.rs | services/net/dns | ⚠ 0 unsafe 静态 hosts；**framework net 调用方确认后定** |
| framework/driver/hotplug.rs | services/driver | ⚠ 0 unsafe 事件分发；**framework 事件源确认后定** |
| framework/driver/bus/mod.rs | services/driver/bus | ⚠ 0 unsafe 编排；**init_all 调用需回调注册或保留** |
| framework/driver/bus/pci.rs | services/driver/bus | ⚠ 同上（bus 一组）|
| framework/driver/display/controller.rs | services/driver/display | ⚠ 0 unsafe 纯数据结构；display/mod 调用确认 |
| framework/driver/display/font.rs | **保留（framework 基础层）** | 被 framework console/gfx_console（保留）消费渲染字符——服务对象=framework 内部 |
| framework/driver/display/framebuffer.rs | **保留（framework 基础层）** | 同上，console 机制消费 |
| framework/driver/display/self_test.rs | services/driver/display | ⚠ 0 unsafe 自检图案；调用方确认 |
| framework/chitin/firmware.rs | services/driver/firmware | ⚠ 0 unsafe blob 管理；framework chitin 调用确认 |
| framework/chitin/proto_block.rs | **保留（安全注册 API 出口）** | 0 unsafe 但为 chitin 机制对 services 的安全导出面（同 IoMem::from_pci_bar 模式）；下沉反而制造 framework 调用方（storage/composite 5 处）→services 反向依赖 |
| framework/credo/engine.rs | services/credo | ⚠ 0 unsafe 能力检查；framework credo/api C ABI 调用确认 |
| framework/proc/canary.rs | **保留（framework 安全 API）** | 被 framework 内部（proc/process.rs、syscall/api.rs）直接依赖 + services 消费——安全导出面；下沉需回调注册（§7.4 已标）|
| framework/barrier/fault_inject.rs | services/barrier | ⚠ 0 unsafe；**经 BarrierDegradePolicy 类 trait 注入**（同 §6.3）|
| framework/barrier/reset/audit.rs | services/barrier | ⚠ 同上 trait 注入 |
| framework/barrier/reset/bbr.rs | services/barrier | ⚠ 依赖 RECOVERY_MANAGER 框架全局，经 trait 注入 |
| framework/barrier/reset/layered.rs | services/barrier | ⚠ 同上 |
| framework/barrier/reset/parallel.rs | services/barrier | ⚠ 同上 |

> **复核终局（DECISION-F）**：上述 ⚠ 待定项经逐文件核查"framework 侧保留代码是否直接调用"，全部判定保留（见 DECISION-F §6.1 复核终局）——§6.1 收口为 **3 下沉 + 17 保留**。

### 6.2 封装+下沉（safe API 后迁）——23 文件 → 3 完成（2-A）+ 4 复核保留（2-B）+ 2 复核保留（2-C）+ 2 完成（2-D usb）+ 1 完成（2-E display）+ 2 复核保留（2-F credo/storage+net query）+ 2 完成（2-G firmware·ftrace）+ 2 完成（2-H info·wait4）+ 1 完成（2-J coredump）+ 4 复核保留（rlimit/e1000/e1000_io/virtio）

> 复核纪律（DECISION-F）：本表 0 unsafe 项**先按服务对象准则（§2）查服务对象再动工**（安全导出面 → 保留；仅 services 消费 → 下沉；被 framework 机制直接调用 → 接口化后下沉或保留）；含 unsafe 的按原"封装+下沉"路径。
>
> 进度（批次 2-A syscall fd/pipe 组）：clone / io / sendfile 三文件完成，标 ✅。批次 2-B（fd 事件族）：epoll / eventfd / signalfd / timerfd 四文件经复核**保留 framework**（耦合判据见 §11 DECISION-P），标 🔒；批次 2-C（char/input）：pl011 / keyboard 二文件经复核**保留 framework**（FFI ops 桥判据见 §11 DECISION-Q），标 🔒，并订正 `input_init` 重复注册（同批）；批次 2-D（usb）：framework/driver/usb 二文件经用户裁定改走 **USB 整体下沉**（覆盖原「20 unsafe 集中，机制留框架」处方），三子步收口——4 个 0-unsafe 文件下沉 + usb_core/xhci safe 权威实装 + framework 侧整目录删除（判据见 §11 DECISION-R），标 ✅；批次 2-E（display）：`framework/driver/display/mod.rs` 经复核**部分下沉**——`controller.rs` 管理策略（0 unsafe、无框架机制消费者）迁 `services/driver/display/controller`，VBE 原语 / framebuffer / font 因被 `gfx_console`（klog/panic 机制）与 `syscall/dispatch`（fb_open/fb_mmap）直接绑定而保留框架（判据见 §11 DECISION-S），标 ✅（后续 Framebuffer 机制/策略拆分登记 §6.3）；批次 2-F（credo/storage + net query）：`framework/credo/storage.rs` 与 `framework/net/init/query.rs` 二文件经复核**均保留 framework**——credo/storage 因 framework 无 VFS safe API 面（属阶段 4）+ `credo/api.rs` 三处 FFI 直接绑定 storage + 序列化直读 credo TCB `PwmEntry` 原子字段，**登记后续条目**（前置＝阶段 4 VFS safe API 就绪后「编排+序列化」整体下沉 `services/credo/persist`）；net query 为 net TCB 状态（DHCP 状态机写入的全局 Atomic）**只读访问面**，按「状态只读访问器与状态定义同层」判据应留 framework（判据见 §11 DECISION-T），标 🔒；批次 2-G（firmware·ftrace）：`framework/syscall/firmware.rs` 与 `framework/syscall/ftrace_kgdb.rs` 二文件经复核**下沉 services**——11+ 处用户指针拷贝改造为 framework safe API（`copy_from_user`/`copy_to_user`），处理策略（参数校验 + 编排 + 逐字段序列化）可 0-unsafe 化且不直接调用 framework 内部机制（判据见 §11 DECISION-U），标 ✅（framework 侧源文件已删）；批次 2-H（info·wait4）：`framework/syscall/info.rs`（uname/sysinfo 处理策略）与 `framework/syscall/wait4.rs`（wait 机制编排 + `wait_reap`/`WaitOutcome` helper）二文件经复核**下沉 services**（`services/proc/info.rs` + `services/proc/wait4.rs`，用户指针写改 `api::write_struct_to_user` 等 safe 代理，framework 侧源文件已删），标 ✅；批次 2-J（coredump）：`framework/proc/coredump.rs` 经复核**整体下沉 services**（`services/proc/coredump.rs`，0 unsafe 完整实装——ELF Core 写出经 `framework::fs::vfs::api::{vfs_open_safe, vfs_write_pod, vfs_write_safe, vfs_close_safe}` 安全代理，signal→coredump 经 `coredump_trait` trait 注入 + OnceCell 注册，中断帧经 `read_interrupt_regs` POD 快照，VMA 枚举经 `vma_snapshot_current`；一并修正 P0-17/P0-20 + note name OOB 读），framework 侧源文件已删，标 ✅（判据见 §11 DECISION-V）；批次 2-K（末 4 文件复核保留）：`framework/proc/rlimit.rs`（`RlimitTable` 为 `Process` 机制字段，状态定义与访问面同层）、`framework/driver/net/e1000.rs` + `e1000_io.rs`（驱动整体在 framework，services 侧仅常量 + re-export 壳）、`framework/driver/virtio/*`（经用户裁定保留）经复核**保留 framework**（判据见 §11 DECISION-V），标 🔒。**§6.2 全表收口**：23 文件 = 11 完成下沉（2-A×3 clone/io/sendfile + 2-D×2 usb + 2-E×1 display + 2-G×2 firmware/ftrace + 2-H×2 info/wait4 + 2-J×1 coredump）+ 11 复核保留（2-B×4 epoll/eventfd/signalfd/timerfd + 2-C×2 pl011/keyboard + 2-F×1 net query + 2-K×4 rlimit/e1000/e1000_io/virtio）+ 1 复核保留并登记后续（2-F credo/storage）。（**后续更新**：2-F credo/storage 已由阶段 4b 全量下沉收口——`framework/credo/storage.rs` 删除，整体迁 `services/credo/persist`，见 §7 阶段 4b 实施记录）

| 文件 | 下沉目标 | 依据 |
|---|---|---|
| framework/proc/coredump.rs | services/proc/coredump | ✅ 已完成（2-J）：`framework/proc/coredump.rs` 整体下沉 `services/proc/coredump.rs`（0 unsafe 完整实装：ELF Core 写出经 `framework::fs::vfs::api::{vfs_open_safe, vfs_write_pod, vfs_write_safe, vfs_close_safe}` 安全代理；signal→coredump 经 `framework::proc::coredump_trait::{CoredumpSink, register_coredump_sink}` trait 注入 + OnceCell 注册，框架 signal 回调改走 `current_coredump_sink().coredump(...)`；中断帧寄存器经 `read_interrupt_regs` POD 快照；VMA 枚举经 `vma_snapshot_current`；一并修正 P0-17（用当前 mm 而非目标 pid mm）+ P0-20（core_limit 未实质截断）+ note name OOB 读）；framework 源文件已删、`pub mod coredump` 移除、审计悬空条目清理（判据见 §11 DECISION-V）|
| framework/proc/rlimit.rs | services/proc/rlimit | 🔒 保留 framework（复核）：`RlimitTable` 为 `Process` 机制字段（`process.rs::rlimit_table`），`RLIMIT_*` 常量与访问器由 framework proc TCB + syscall 契约面直接持有/读写，属「状态定义与其只读/写入访问面同层」；services 侧 `services/proc/rlimit.rs` 为纯 re-export 壳。维持 framework。|
| framework/syscall/clone.rs | services/proc/clone（已存在）| ✅ 已完成：`sys_clone` 迁移至 services/proc/clone.rs `clone_impl`（0 unsafe，用户指针写改 `api::write_struct_to_user`）；framework 源文件已删 |
| framework/syscall/epoll.rs | services/syscall/epoll | 🔒 保留 framework（2-B）：epoll 本身即机制（wait_queue + 阻塞调度 + 中断 pwake），`epoll_pwake` 被 framework `fd_notify` / `timerfd` / `inotify` 直接调用，`check_fd_ready` 又耦合 eventfd/signalfd/timerfd。services 侧 `services/sync/epoll.rs` 为薄代理壳。【复核】原表列 `services/syscall/epoll`，实际壳落点 `services/sync/epoll` |
| framework/syscall/eventfd.rs | services/syscall/eventfd | 🔒 保留 framework（2-B）：被 epoll 机制 `check_fd_ready` 直接调用（同类耦合），close 路径调 `epoll_pwake`；1 unsafe 为 read 用户指针写。【复核】实际壳落点 `services/sync/eventfd` |
| framework/syscall/firmware.rs | services/syscall/firmware | ✅ 已完成（2-G）：`sys_fw_load/get/get_info/detach` 下沉 `services/syscall/firmware.rs`（0 unsafe，用户指针拷贝改 `copy_from_user`/`copy_to_user`，`FirmwareInfo` 逐字段 `to_ne_bytes` 序列化，无对齐假设）；framework 源文件已删（判据见 §11 DECISION-U）|
| framework/syscall/ftrace_kgdb.rs | services/syscall/ftrace | ✅ 已完成（2-G）：`sys_ftrace_enable/disable/read/stat` + `sys_kgdb_enter` 下沉 `services/syscall/ftrace.rs`（0 unsafe，`TraceEvent`/统计逐字段 `to_ne_bytes` 序列化；KGDB 经 `framework::debug` 机制原语）；framework 源文件已删（判据见 §11 DECISION-U）|
| framework/syscall/info.rs | services/proc/info（已存在）| ✅ 已完成（2-H）：`sys_getpid/gettid/getppid/getpgid/uname` 下沉 `services/proc/info.rs`（0 unsafe，用户指针写改 `framework::syscall::api::write_struct_to_user`，主机名/域名源为 `framework::proc::namespace::uts_current`）；framework 源文件已删 |
| framework/syscall/io.rs | services/fs/io | ✅ 已完成：用户指针/fcntl 拷贝改 safe API。【复核】实际落点 `services/fs/io`（原表列 `services/syscall/io`）；framework 源文件已删 |
| framework/syscall/sendfile.rs | services/fs/sendfile | ✅ 已完成：2 unsafe 改 VFS/pipe safe API。【复核】实际落点 `services/fs/sendfile`（原表列 `services/syscall/sendfile`）；framework 源文件已删 |
| framework/syscall/signalfd.rs | services/syscall/signalfd | 🔒 保留 framework（2-B）：被 epoll 机制 `check_fd_ready` 直接调用（同类耦合），close 路径调 `epoll_pwake`；2 unsafe 为用户指针读写。【复核】实际壳落点 `services/sync/signalfd` |
| framework/syscall/timerfd.rs | services/syscall/timerfd | 🔒 保留 framework（2-B）：`timerfd_callback(&HrTimer)` 依赖 container_of 反推 `TimerFdSlot`（同 `framework/proc/posix_timer.rs` 先例），回调整体不可 safe 化；经用户裁定否决"扩展 `HrTimer` 增 safe cookie"的 TCB 改动。【复核】实际壳落点 `services/timer/timerfd` |
| framework/syscall/wait4.rs | services/proc/wait4（已存在）| ✅ 已完成（2-H）：`sys_wait4/waitid` + `wait_reap`/`WaitOutcome` helper 下沉 `services/proc/wait4.rs`（0 unsafe，`2 unsafe` 用户指针写改 framework safe API）；framework 源文件已删 |
| framework/net/init/query.rs | services/net/query | 🔒 保留 framework（2-F）：查询函数为 framework net TCB 状态（`G_INIT_STATE`/`G_MAC`/`G_IPV4`/`G_GATEWAY`/`G_DNS`，由 DHCP 状态机写入的全局 Atomic）的**只读访问面**，按「状态只读访问器应与状态定义同层」判据应留 framework；且被 `net/api.rs`（契约面）与 `net/init/sm_fi.rs`（启动编排）直接调用（先例 `net_socket.rs::reset_network_state`）。【复核】原表列「查询纯 Atomic → 可迁」，实为 TCB 状态访问面，终局保留非权宜（判据见 §11 DECISION-T）|
| framework/driver/char/pl011.rs | services/driver/char/pl011 | 🔒 保留 framework（2-C）：`arch::uart` 为 boot 早期控制台机制（klog/panic 依赖）必须留框架；`pl011_read/write` CharOps FFI 桥（裸指针 `driver_data`）不可 safe 化；PL011 为固定平台基址（非 PCI），`IoMem` 无固定基址 safe 构造器。【复核】原表列「MMIO 改 IoMem 封装」，实不可行 |
| framework/driver/display/mod.rs | services/driver/display | ✅ 已完成（2-E）：`controller.rs` 管理策略（`DisplayController`/`DisplayManager`/`DisplayMode`/`MonitorInfo`/`DisplayOutput`，0 unsafe 且无框架机制消费者）整体迁 `services/driver/display/controller`；framework 侧删 `pub mod controller` + re-export + `display_init` 内 `let _manager` 死语句，`driver/mod.rs` 补 re-export `PixelFormat`。VBE 原语 / framebuffer / font / 自检 / `FB_PHYS_ADDR` / `get_framebuffer` 因被 `gfx_console` 与 `syscall/dispatch` 直接绑定而保留框架（判据见 §11 DECISION-S）|
| framework/driver/input/keyboard.rs | services/driver/input | 🔒 保留 framework（2-C）：`kb_input_read/has/irq` InputOps FFI 桥（裸指针 `driver_data as *mut KeyboardDriver`）不可 safe 化；`read_line` 依赖 `unsafe extern "C" scheduler_yield_ex`；纯 scancode/shift 表可 safe 化但无独立价值。【复核】原表列「scancode 迁出」，实为 FFI 桥绑定整体 |
| framework/driver/net/e1000.rs | services/driver/net/e1000（回迁）| ✅ 阶段 3 收尾（**DECISION-B 落地，覆盖下文 2-K「复核保留」**）：E1000 业务（复位/链路探测/描述符环配置/收发）整体迁 `services/driver/net/e1000` 单一 `E1000NetDriver`（0 unsafe，impl `NetDeviceOps` 经泛型桥生成 extern "C" 回调 + `register_net_device`，PCI 探测经框架 safe API + 复合探测 `net_services_probe` e1000→virtio 回落）；framework 侧仅留 `dma_ring.rs` DMA 环机制 + `e1000_io.rs` MMIO 访问器。|
| framework/driver/net/e1000_io.rs | services/driver/net/e1000 | 🔒 保留 framework（阶段 3 复核确认）：`E1000Io` 为 `IoMem` MMIO 封装的安全导出面，阶段 3 后消费方为 services `E1000NetDriver`（经 safe API 调用），维持 framework；模块经 `#[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]` 门控避免非目标构建下寄存器常量死代码（F9）。|
| framework/driver/usb/mod.rs | services/driver/usb | ✅ 已完成（2-D）：PCI 发现 + `usb_init` 迁 services（chitin proto=Bus），framework 源文件已删 |
| framework/driver/usb/xhci.rs | services/driver/usb/xhci | ✅ 已完成（2-D）：xhci/枚举/类驱动**整体下沉**（原「20 unsafe 集中，机制留框架」处方经裁定覆盖，见 DECISION-R），framework 源文件已删 |
| framework/driver/virtio/mod.rs | services/virtio/transport | 🔒 保留 framework（复核）：经用户裁定维持 framework——virtio transport（`mod.rs` + `queue.rs`）为设备发现/队列机制，与 framework driver/chitin 编排直接耦合，下沉将制造 framework→services 反向依赖。|
| framework/credo/storage.rs | services/credo/persist | 🔄 复核保留（2-F）+ 登记后续：`w8/w16/w32/…` 序列化算法（0 unsafe 纯函数）与 `save/load/remove_database` 编排（功能）终局应整体迁 `services/credo/persist`，仅 `vfs_*_internal` C FFI 留 framework 薄层。**前置缺失**：framework 当前无 VFS safe API 面（属阶段 4），强行只迁序列化会强加仅为过渡存在的 trait 注入（违 §12.3）；且 `credo/api.rs` 三处 FFI 直接调 `storage::{save,load,remove}_database`（安全导出面绑定）+ 序列化直读 credo TCB `PwmEntry` 原子字段。**登记后续条目（前置＝阶段 4 VFS safe API 就绪 → 编排+序列化整体下沉）**。【复核】原目标路径 `services/credo/storage` 已被块设备代理占用（`services/credo/storage/`），订正为 `services/credo/persist`（判据见 §11 DECISION-T）|

### 6.3 部分下沉（机制文件内策略拆分）——11 文件 + 2 后续登记（display 2-E / credo-storage 2-F）

> **拆分接口原则**：迁出的策略函数与 framework 机制的交互必须显式化——中断/panic 上下文经 **trait 注入**（framework 定义契约 + services 注册，OnceLock 全局，只读原子访问）；boot 早期经**回调注册**；普通路径经 **framework 机制 API**。禁止 framework 直接调用 services 函数（反向依赖）。

| 文件 | 拆分说明 | 拆分接口（交互方式） |
|---|---|---|
| framework/mm/vma.rs | mremap/madvise_range/mlock/mincore/mprotect 策略迁出；MmStruct/Vma/find/insert/remove 保留 | ⚠ services 经 framework 提供的 VMA 查询/修改 API + vmm map/unmap 机制编排 mremap 流程（MmStruct 无私有访问权）|
| framework/mm/page_fault.rs | 栈扩展参数/阈值策略迁出；demand paging/COW/swap-in 机制保留 | ⚠ **PageFaultPolicy trait 注入**（#PF 中断上下文；参数由 boot 注册或框架承接）|
| framework/mm/pcache.rs | 容量参数策略化；哈希桶/引用计数/脏页机制保留 | 参数移 services config，机制经 framework 常量 API 读取 |
| framework/mm/swap.rs | LruList/kswapd 决策已 trait 化；slot I/O/swap-in-out 机制保留 | ✅ 已有 SwapPolicy trait（services/swap_policy 权威）|
| framework/timer/time_sync.rs | NTP/PLL 算法迁出；ClockAdjState 状态承接 | 状态留 framework API 承接，算法纯函数经 framework 时钟查询 |
| framework/timer/tickless.rs | NO_HZ 协议迁出；LAPIC 编程机制保留 | 决策经 framework 调度查询 safe API（sched_ops），编程经 framework apic API |
| framework/timer/sleep.rs | adaptive_sleep 阈值/轮询策略迁出；busy_wait 机制保留 | 阈值参数经 framework 常量 API；busy_wait 机制留框架 |
| framework/timer/calibration.rs | 采样算法迁出；static 频率缓存留框架 API 承接 | ⚠ boot 早期调用，经**回调注册**（framework 注册采样回调，services 实现）|
| framework/barrier/domain.rs | apply_degradation 降级策略迁出；RecoveryDomain 状态机保留 | ⚠ **BarrierDegradePolicy trait 注入**（panic/中断上下文；framework 提供原子状态访问 API）|
| framework/barrier/reset/bsr.rs | freeze/unfreeze/rollback 编排迁出；mmio_write32 机制保留 | 编排经 framework 恢复机制 API（RECOVERY_MANAGER）；mmio 写留框架 |
| framework/debug/ebpf.rs | 验证器策略已 trait 化（services）；解释执行引擎保留 | ✅ 已有 BpfVerifier trait（services/ebpf_verifier 权威）|
| framework/driver/display/framebuffer.rs（+ font/self_test）| Framebuffer 绘图策略（set_pixel/fill/fill_rect/draw_line/blend/aa 等）迁 services；IoMem 映射、`FB_PHYS_ADDR`/`FB_PHYS_SIZE`、`get_framebuffer` 机制原语保留 | ⚠ 2-E 后续（登记项，见 §11 DECISION-S）：**前置**需先解除 `gfx_console` 对 `*mut Framebuffer` 的裸指针绑定（klog/panic 机制经 trait 注入或回调注册消费绘图能力），再将绘图策略拆至 services + framework 留机制原语 |
| framework/credo/storage.rs（2-F 后续登记）| ✅ **已由阶段 4b 全量下沉收口**：序列化算法（0 unsafe 纯函数）+ `save/load/remove_database` 编排整体迁 `services/credo/persist`；framework 侧源文件删除，**无 `vfs_*_internal` C FFI 薄层残留**（63 处 `no_mangle` 壳随 4b 一并删除）；`PwmEntrySnapshot` POD 解绑序列化对 credo TCB `PwmEntry` 原子字段直读，`credo/api.rs` 三处 FFI 改调 services 编排 |（判据见 §11 DECISION-T）|

### 6.4 双份合并（services 权威，framework 删业务）——20 文件

> 状态（按 DECISION-G「直接方案 B」方向裁决 + 批次 X/Y/Z 执行 + 阶段 3 收尾）：**✅ 已收口 16 项** —— usb×5（随 2-D 整体下沉）、char `serial`+`vga`（批次 X，`9ba997e3`）、virtio `blk`+`net`（批次 Z③ transport 去重 + Z④ NetOps 桥）、storage `mod`+`ahci`+`ahci_block`+`nvme`+`nvme_block`（批次 Y，`storage_init` 退位：`_block` 适配层删除，`ahci`/`nvme`/`mod` 保留机制 wire 类型薄层）、storage `ata`+`ata_block`（阶段 3 收尾：framework ATA PIO 回退路径整体迁 `services/driver/storage/ata`，仅注册块设备 `ata0-3`）；**🔒 复核保留 framework 4 项** —— chitin `composite`/`devtree` + credo `grant`/`session`（framework 机制 + services 策略/安全代理正确形态，见 DECISION-G 项 2/3，非「删业务」对象）。display/hdmi×7 孤儿另计已删。**§6.4 全表收口**。DECISION-B E1000 业务回迁于阶段 3 一并落地（详下 E1000 净段）。

| framework 文件 | services 权威 / 处置 |
|---|---|
| driver/char/serial.rs | services/driver/char/serial（0 unsafe 完整实现）—— ✅ 批次 X 收口（framework 侧已删，仅留 aarch64 pl011）|
| driver/char/vga.rs | services/driver/char/vga —— ✅ 批次 X 收口（framework 侧已删）|
| driver/storage/mod.rs | services/driver/storage —— ⚠ 机制薄层保留（业务已退位 services `storage_init`）|
| driver/storage/ahci.rs + ahci_block.rs | services/driver/storage/ahci —— ⚠ `ahci_block.rs` 已删（批次 Y）；`ahci.rs` 保留机制 wire 类型薄层 |
| driver/storage/ata.rs + ata_block.rs | services/driver/storage/ata —— ✅ 阶段 3 收尾（framework 两侧源文件已删；services 真实 PIO 驱动经 `IoPort::new_safe` 0 unsafe 实现，仅注册块设备 `ata0-3`，跟随 ahci/nvme 形态）|
| driver/storage/nvme.rs + nvme_block.rs | services/driver/storage/nvme —— ⚠ `nvme_block.rs` 已删（批次 Y）；`nvme.rs` 保留机制 wire 类型薄层 |
| driver/usb/enumerate.rs | services/driver/usb/enumerate（✅ 已随 2-D 整体下沉删除 framework 侧）|
| driver/usb/hid.rs | services/driver/usb/hid（✅ 已随 2-D 整体下沉删除 framework 侧）|
| driver/usb/mass_storage.rs | services/driver/usb/mass_storage（✅ 已随 2-D 整体下沉删除 framework 侧）|
| driver/usb/ring.rs | services/driver/usb/ring（✅ 已随 2-D 整体下沉删除 framework 侧）|
| driver/usb/usb_core.rs | services/driver/usb/usb_core（✅ 已随 2-D 整体下沉删除 framework 侧）|
| driver/virtio/blk.rs | services/driver/virtio/blk —— ✅ 批次 Z③ 收口（transport 去重：framework 侧 blk 业务已删，留机制薄层；IRQ 路径见 DECISION-I 专项登记后续）|
| driver/virtio/net.rs | services/driver/virtio/net —— ✅ 批次 Z④ 收口（NetOps 桥，framework 侧删除业务）|
| chitin/composite.rs | services/chitin/composite —— 🔒 framework 机制保留（RAID0/RAID1 组装 `CompositeType`）+ services 安全代理 `probe` |
| chitin/devtree.rs | services/chitin/devtree —— 🔒 framework 机制保留（设备树拓扑 `ChitinNode`/`CHITIN_ROOT`）+ services 安全代理转发 |
| credo/grant.rs | services/credo/grants —— 🔒 framework 机制保留（`GRANT_RECORDS` 原语）+ services 策略层（链式委托）|
| credo/session.rs | services/credo/sessions —— 🔒 framework 机制保留（P2-I-30 后绑定 `Process` 字段）+ services 策略层（会话生命周期）|
| （备注）driver/display/hdmi/ 7 文件 | **整目录未挂载孤儿**，services/driver/display/hdmi 权威 —— ✅ 已删除 |

### 6.5 壳删除（re-export 兼容层）——82 文件

| 子模块 | 壳文件 |
|---|---|
| proc（9）| cfs / cgroup / fd_alloc / madvise_mlock / namespace / oomd / seccomp / session / types |
| syscall（4）| madvise_mlock / mmap / mprotect / types |
| fs（10）| mod / vfs/mod / devfs/mod / devfs / procfs/mod / procfs / ramfs/mod / ramfs / nestfs/mod / flock |
| net（5）| mod / types / wait_queue / netfilter / driver/mod（+route/inotify 为"壳+薄层"）|
| ipc（5）| async_ipc / scheduler_integration / sem / signal / types |
| io（2）| mod / iouring |
| wasm（6）| mod / interpreter / leb128 / module / runtime / types |
| credo（3）| capability / sha256 / types |
| driver（8 + hdmi 7）| block / char/mod / input/mod / net/mod + display/hdmi/ 7 孤儿 |
| config（11）| 全部（boot_image/capacity/caps/error/kaslr/memory/procfs/sched/slab/validate + mod）|
| debug（2）| mod / api |
| barrier（4）| mod / types / reset/mod / reset/config |
| mm（5）| mod / api / mechanism / numa / pressure |
| timer（1）| mod |

### 6.6 保留 framework（机制/TCB）——200 文件

- **arch（19）**：x86_64/aarch64 全部（页表/中断/上下文切换/APIC/GIC/PSCI/UART/启动）
- **sync（14）**：全部同步原语（含 pi_mutex 优先级继承协议、lockdep）
- **idt/irq/smp/cpu/pci/dma（17）**：IDT 编程/softirq/SMP 基础设施/CPU 探测/PCI 配置/MSI/DMA 引擎
- **mm 机制（15）**：vmm 页表/copy_user/cow/frame/kmalloc/slab/pmm/kpti/arch + 4 trait 契约（alloc/pmm/slab/swap）
- **proc 机制（16 + canary）**：process/thread/user_proc/scheduler/scheduler_ex/信号投递/elf 加载/cpu_queue/proc_ops/mechanism + **canary（安全 API，§6.1 重判）** + sched/signal/dispatch trait 契约
- **syscall 入口（5）**：dispatch/api/mod（FFI + raw）/futex（用户原子）/dispatch_trait
- **fs 契约（终局 2 + POD）**：`VfsOps`（framework 保留机制的消费契约，阶段 4a 新建于 `fs/vfs/ops_trait.rs`）+ `vfs_poll_trait`（`syscall/epoll.rs` 消费）+ POD `VfsFileType`（`fs/vfs/types_pod.rs`）。**DECISION-W + 4b 收窄**：终局仅留上述 2 契约（原 §6.6「3 契约」含 `backend_trait`/`inode` 的判定随阶段 4b 一并推翻——`FsBackend`/`Inode` 与具象类型随 VFS 完整下沉 services）；`handle`/`mount`/`path` 的 syscall 处理器与逻辑下沉 services（userptr 收敛为 framework 通用 safe API）；VFS 契约方法仅含 POD/framework 类型。
- **net 集成（12）**：init 状态机/raw（static mut）/smoltcp_impl/sockets（self-referential）/sm_fi/syscall/save/iface_trait/api
- **ipc 机制（6）**：mod（命名空间）/dynamic/msgq（侵入式链表）/pipe/shm FFI 薄层/api
- **driver 机制（11 + 安全注册包装）**：mod/framework（端口 I/O 原语）/kexec/uefi/power 硬件原语/net/dma_ring/virtio/queue/chitin 注册表 + **安全注册包装（proto_block，§6.1 重判）** + proto 指针表（char/input/net）+ user_driver
- **credo 机制（8）**：mod/api（C ABI）/audit 环形缓冲/bootstrap/csprng/identity/secure_boot
- **debug 机制（3）**：ftrace/kgdb/ringbuf
- **barrier 机制（10）**：api/manager/recoverable/recovery/snapshot/undo_log/bhr + reset/mod
- **console（2 + 基础层）**：mod/gfx_console + **display/font + framebuffer（基础算法层，§6.1 重判，console 机制消费）**
- **顶层杂项（25）**：cpu_local/iomem/ioport/dma_buf/frame/iobuf/irqline/userptr/usermode/userctx/vmspace/errno/error/credo_pwm/net_socket/proc_elf/process_cleanup/racy_cell/rlimit_query/syscall_init/tick_query/fd_notify/page_table/prelude/mod
- **lib/klog/constants/alloc/boot（18）**：基础库/日志/常量/分配 trait/引导

### 6.7 services 侧现状（260+ 文件确认）

- **权威实现 ~89 文件**：T1-T9/E6 系列已完成（config 全、credo 类型层、ipc 策略/类型、mm 策略、net 策略、proc 策略、wasm、fs 伪文件系统 devfs/procfs_core/nestfs/flock/inotify/ramfs_core/iouring）。
- **策略实现 ~90 文件**：exfat/ext2（独立 FS）、cgroupfs/configfs/devpts/sysfs/systree/virtiofs/overlayfs/tmpfs、wasi（9）、sync barrier/once/scoped、proc memfd/pidfd、driver display dp/ddc/controller。
- **壳/代理 ~63 文件**：framework 权威，services 薄层（chitin/credo 运行时/debug/ipc 命名空间/mm 物理层/net syscall/proc 进程表/sync/syscall/timer/klog 等）。
- **影子双份**：driver char/storage/virtio + display/hdmi——即 §6.4 合并对象（usb 已随 2-D 整体下沉收口）。
- 注意：B09-12 已把 fs 的 dcache/vfs_types/vfs_manager/open_file_table 迁回 framework——按 DECISION-A 重新下沉（§5）。

## 7. trait 化改造清单（反向依赖全面整治）

描述：framework 机制与 services 交互全部经 trait/回调/注册表，清除直接引用。
方案：
- **7.1 壳删除（§6.5 82 文件）**：framework→services 引用 -70 处（`pub use` 壳）。
- **7.2 直接 use trait 化（~20 处）**：framework 生产代码直接 `use services` 的逐处改造——
  - syscall 分发 → dispatch_trait 注入（已有雏形）
  - 调度/信号决策 → sched_trait/signal_trait（保留契约位，确认无直接 use）
  - 其余逐处：sm_fi/sendfile/user_proc 等 → 封装 API 或回调注册。
- **7.3 framework/tests 访问 services（7 处）**：测试载体访问 services 真实代码属合理，不纳入整治（另议）。
- **7.4 下沉后 framework 残留调用点 + 接口方案**（2026-09-11 实测）：

| 下沉类 | framework 残留调用点 | 接口方案 |
|---|---|---|
| syscall 17 个（brk/epoll/eventfd/timerfd/signalfd/info/io/sendfile/wait4/firmware/ftrace_kgdb/clone/posix_timer/canary）| dispatch.rs:207 已走 dispatch_trait；**但 dispatch.rs:96/208 直接引用 `services::syscall::types::{SYS_rt_sigreturn, ENOSYS_RET}`** | ✅ dispatch_trait（已有雏形）；收敛 2 处类型引用为 framework 类型 |
| driver 12 个（bus/hotplug/display/char/input/storage/usb/virtio/e1000）| driver/mod.rs:197 `init_all` 直接调 `char::char_init()`/display_init/bus 等 | ⚠ **Chitin 注册**（驱动注册表分发）或 init 回调注册 |
| canary 2 个（proc/syscall）| syscall/api.rs:260/265 直接调 `super::canary::sys_getrandom`（api.rs 保留）| ⚠ 回调注册（framework 提供注册点）|
| coredump/rlimit/wait4 | framework signal/process 机制调用点 | ⚠ 实施前调用方确认 |
| net 类（dns/query）| framework net init 调用点 | ⚠ 实施前调用方确认 |

- 复用既有策略注入模式：pmm_trait/slab_trait/swap_trait/alloc_trait/sched_trait/dispatch_trait。
- **7.5 保留 framework 文件内的 services 引用（43 处需收敛，2026-09-11 全量统计）**——136 处反向依赖中，除壳（70）/下沉文件（12）/tests（9 另议）外，保留机制文件内仍有 43 处 `use services` 需逐处收敛：

| 保留文件 | 引用数 | 处置 |
|---|---|---|
| framework/ipc/mod.rs | 11 | 收敛：机制保留，services 策略类型改经 framework 顶层 re-export / 接口抽象 |
| framework/ipc/pipe.rs | 5 | 收敛：FFI 薄层保留，策略类型经 services 顶层暴露 |
| framework/ipc/shm.rs | 4 | 同上 |
| framework/ipc/msgq.rs | 4 | 收敛：机制（侵入式链表）与策略 re-export 拆分 |
| framework/net/syscall.rs | 4 | 收敛：services 类型 → framework 类型（copy-in/out 桥接）|
| framework/net/init/sm_fi.rs | 3 | 收敛：FFI 边界，wire 类型翻译集中框架 |
| framework/syscall/dispatch.rs + dispatch_trait.rs | 5 | 收敛 2 处类型引用（SYS_rt_sigreturn/ENOSYS_RET）为 framework 常量 |
| framework/proc/{user_proc.rs,process.rs,sched_ops.rs} | 4 | 收敛：services 类型 → framework 类型 |
| framework/credo/{identity.rs,mod.rs} | 3 | 收敛：services 类型 → framework 类型 |
| framework/driver/power.rs | 2 | ✅ 正确"薄层+re-export"形态（services 策略权威 + framework 硬件原语），保留 |
| framework/tests + ipc/stress_tests.rs | 9 | 另议（测试载体访问 services 合理）|

**合计**：43 处收敛（ipc 24 为第一优先）+ 2 处正确形态 + 9 处另议 = 54 处；加壳 70 + 下沉 12 = **136 处全覆盖**。

### §7 ipc 现状分析 + 收敛方案（2026-09-12 复核）

> 实测 framework/ipc 的 services 引用重新归类（原表"24 处"含壳与测试，生产引用为 13 处 FFI 边界调用）：

| 类别 | 文件 | 引用 | 处置 |
|---|---|---|---|
| 纯 re-export 壳 | types / sem / signal / scheduler_integration / async_ipc.rs | `pub use services::ipc::*` | §6.5 壳删除 |
| **FFI 边界调 services（核心 13 处）** | pipe.rs（5：is_pipe_fd + 4 FFI）、shm.rs（4）、msgq.rs（4）| extern "C" 薄层持 namespace/UserPtr（framework 机制）→ 调 services `*_safe` | **trait 注入**：framework 定义 `IpcStrategy` 契约，services 实现 + OnceLock 注册（复用 pmm_trait/swap_trait 模式）；FFI 边界调 trait（framework→services 归零） |
| cfg(test) | mod.rs tests（11）、stress_tests.rs | 测试访问 services 真实代码 | §7.3 另议（合理） |

- **收敛方案（pipe 起）**：① framework/ipc 定义 `IpcStrategy` trait（方法签名含 `&mut IpcNamespace`/`&mut IpcId`/`pid` 等 framework 机制类型，与 `*_safe` 一一对应）；② services/ipc 实现 trait（内部调既有 `*_safe`）；③ framework 提供 `OnceLock<&'static dyn IpcStrategy>` 注册点 + `ipc_init` 期回调（boot 早期注册）；④ FFI 边界改调 trait（`IPC_STRATEGY.get().pipe_create(...)`），framework→services 引用归零；⑤ 验证链全绿。
- **风险**：注册时序（boot 早期须先于首个 syscall）；trait 对象动态分派微开销（可接受，与 pmm_trait 同级）。
- **进度**：本分析为 §7 ipc 施工前置。**§6.5 壳删除（types/sem/signal/scheduler_integration/async_ipc 5 壳 + 全仓 70 壳）是更低风险的首批收敛动作**，可与 trait 注入并行推进。

### §7 壳删除真实障碍：所有权纠缠（2026-09-12 复核，需裁决）

> 实测发现：**所有 §6.5 壳并非纯机械删除**——壳存在的原因正是 framework 生产代码依赖 services 项。壳删除 = 逐项判定归属 + 所有权反转，而非删文件。

| 壳组 | framework 生产依赖 | 归属判定 | 处置 |
|---|---|---|---|
| ipc 类型壳（types.rs）| `IPC_NAMESPACE: RacyCell<IpcNamespace>`（framework 机制）持有 services 类型（IpcNamespace/Pipe/MsgQueue/ShmSegment/Semaphore/Message/WaitQueue/WaitQueueItem + IPC_MAX_* 常量，均定义于 services/ipc/types.rs）| 命名空间是 framework 机制（§6.6），其数据类型 = 机制类型 | **类型所有权反转**：迁回 framework，services re-export（合法 services→framework）——DECISION-A 式反转 |
| config 常量壳（11 文件）| `framework::config::{PAGE_SIZE, MAX_CPUS}` 等被 iobuf/cpu_local/smp/rcu/irq/mm 等 framework 机制大量使用；`config::procfs/caps/validate` 被 framework/tests 使用 | 页大小/最大 CPU 等 = 机制常量 | **常量迁回 framework**，services config re-export |
| 其余壳（proc/debug/barrier/wasm 等）| 同构——壳为 framework 消费 services 项的兼容层 | 逐项判定 | 分批所有权反转 |

**裁决请求**：壳删除的正确执行路径 = **DECISION-A 式所有权反转**（机制项迁回 framework，services 侧改 re-export 保持 API 兼容），而非机械删文件。是否确认此方向？若是，§7 壳删除批次将按"先迁移机制项到 framework → services re-export → 删除 framework 壳 → 清理引用"执行（每批验证链全绿）。

### DECISION-J 第一批执行记录：ipc 类型反转（commit 3519410e）

> 审核员裁决（DECISION-J）：采纳所有权反转路径，ipc 类型反转第一批通过。边界：仅迁机制类型/常量、策略逻辑禁止随迁、依赖闭包检查、services re-export 保兼容。

- **迁回 framework**：`framework/ipc/types.rs` 由 re-export 壳改为真实类型定义（约 600 行）——`IpcId`/`IPC_MAX_*`(8 常量, 含 E-03 any 门控)/`PIPE_BUFFER_SIZE`/`SHM_MAX_SIZE`/`MSG_MAX_SIZE`/`MSG_QUEUE_MAX_MSGS`/`IpcType`/`SignalNum`(+From)/`SignalAction`/`WaitQueueItem`/`WaitQueue`(+B07-15 中断安全实现)/`Pipe`/`SignalHandlerFn`/`SignalHandler`/`SignalPending`/`ShmSegment`/`Message`/`MsgQueue`/`Semaphore`/`IpcNamespace` + cfg(test) WaitQueue 测试。0 unsafe（依赖闭包：WaitQueue→IrqSpinLock 为 framework 自身类型）。
- **services 改 re-export**：`services/ipc/types.rs` 改为 `pub use crate::kernel::framework::ipc::types::*`（含 #![deny(unsafe_code)]），API 兼容。
- **策略逻辑未随迁**：services/ipc/{pipe,shm,msgq,sem,signal}.rs 的 `*_safe` 策略实现保持 services（T6 权威），且它们本就经 `crate::kernel::framework::ipc::types::*` 引用类型（services→framework 合法方向）。
- **引用计数**：framework 文件级反向依赖 79→78（types.rs 壳引用消除）；framework/ipc/mod.rs 的 `use types::*` 现解析到 framework 自身类型（0 services 引用）。
- **验证**：双架构 0w0e ✅ / clippy -D pedantic 双架构 0 ✅ / 核心审计全 0 ✅ / host-tests 待确认 / QEMU 未跑（纯类型归位不触 boot，下一批壳删除时合并冒烟）。

## 8. 批次实施计划

描述：分 6 阶段，每阶段独立可验证（实施交委托人）。
方案：
- 阶段 0：**safe API 缺口补齐 + 策略注入 trait 定义**。[X]（本工程首批交付）
  - ✅ **PageFaultPolicy trait**（[framework/mm/page_fault_policy.rs](../../src/kernel/framework/mm/page_fault_policy.rs)）：栈扩展三参数策略化，fallback=历史值；page_fault.rs 已接入（`current_page_fault_policy`）
  - ✅ **BarrierDegradePolicy trait**（[framework/barrier/degrade_policy.rs](../../src/kernel/framework/barrier/degrade_policy.rs)）：降级矩阵策略化，fallback=历史矩阵；domain.rs `apply_degradation` 已接入
  - ✅ **IoMem::from_pci_bar 化**：usb/mod.rs xhci-pci 改安全包装（-1 unsafe）；xhci.rs 测试用 fake MMIO 保留 `IoMem::new`
  - ✅ **UserPtr safe 构造**：`UserReadPtr/UserWritePtr::checked_new`（先 `validate_user_buf` 再构造）+ 单测；FFI 调用点（unsafe extern "C" 契约）随对应下沉阶段迁移
  - ✅ **VMA 查询/修改 API**：已满足——services/mm/{mremap,mprotect}.rs 已委托 framework `MmStruct::{mremap,mprotect}`，无需新增
  - 📝 **DmaStream 收敛裸指针**：已无 pub 裸指针（`cpu_addr` 返回 `NonNull<u8>`），随阶段 3 下沉驱动验证
  - 📝 **FFI 薄层分离**：随阶段 6.2/6.3 下沉实施（syscall 用户指针拷贝集中框架）
  - 📝 **calibration 采样回调**：boot 早期路径，随阶段 6.3 timer 部分下沉实施
- 阶段 1：**纯策略下沉**（§6.1 20 文件）。[X] 收口——3 确认下沉（syscall×3，已提交）+ 17 保留（服务对象准则复核终局，DECISION-F）
- 阶段 2：**封装+下沉**（§6.2 23 文件）。[X] 收口——2-A syscall fd/pipe 组完成 3 文件（clone/io/sendfile，0 unsafe 落地 services）；2-B fd 事件族 4 文件（epoll/eventfd/signalfd/timerfd）经复核**保留 framework**（耦合判据见 §11 DECISION-P）；2-C char/input 2 文件（pl011/keyboard）经复核**保留 framework**（FFI ops 桥判据见 §11 DECISION-Q），并订正 `input_init` 重复注册；2-D usb 2 文件经裁定改走 **USB 整体下沉**（三子步收口，framework 侧整目录删除，判据见 §11 DECISION-R）；2-E display 经复核**部分下沉**（`controller.rs` 管理策略迁 services，VBE 原语/framebuffer/font 保留框架，判据见 §11 DECISION-S）；2-F credo/storage+net query 二文件经复核**均保留 framework**（credo/storage 登记后续条目（前置＝阶段 4 VFS safe API），**已由阶段 4b 全量下沉收口**（`framework/credo/storage.rs` 删除，整体迁 `services/credo/persist`）；net query 为 net TCB 状态只读访问面，判据见 §11 DECISION-T）；2-G firmware·ftrace 二文件（`framework/syscall/firmware.rs` + `ftrace_kgdb.rs`）经复核**下沉 services**（`services/syscall/firmware.rs` + `ftrace.rs`，11+ 处 unsafe 用户指针拷贝改 framework safe API，framework 侧源文件删除，判据见 §11 DECISION-U）；2-H info·wait4 二文件（`framework/syscall/info.rs` + `wait4.rs`）经复核**下沉 services**（`services/proc/info.rs` + `wait4.rs`，用户指针写改 framework safe API，framework 侧源文件删除）；2-J coredump 一文件经复核**整体下沉 services**（`services/proc/coredump.rs` 0 unsafe 完整实装，经 `coredump_trait` 注入 + VFS POD safe API + `read_interrupt_regs`/`vma_snapshot_current` 快照，一并修正 P0-17/P0-20；判据见 §11 DECISION-V）；2-K 末 4 文件（rlimit/e1000/e1000_io/virtio）经复核**保留 framework**（判据见 §11 DECISION-V）。**§6.2 收口：23 = 12 完成下沉 + 11 复核保留（其中 credo/storage 登记后续，已由阶段 4b 全量下沉收口）**
- 阶段 3：**驱动双份合并 + E1000 回迁**（§6.4 20 文件）。[X] 收口——批次 X/Y/Z 逐项接线后（见 §8 各批记录），本阶段收尾 2 项：① storage `ata`+`ata_block`（framework ATA PIO 回退路径整体迁 `services/driver/storage/ata`，IoPort::new_safe 安全 PIO，仅注册块设备 `ata0-3`，跟随 ahci/nvme 形态）；② **DECISION-B E1000 业务回迁**（`framework/driver/net/e1000.rs` 业务迁 `services/driver/net/e1000` 单一 `E1000NetDriver`，framework 仅留 `dma_ring.rs` 环机制 + `e1000_io.rs` MMIO 访问器，复合探测 e1000→virtio 回落）。§6.4 全表收口（16 收口 + 4 机制保留）。见 §8「阶段 3 执行记录」。
- 阶段 4：**VFS 整体下沉**（终局 = Asterinas 对齐·完整下沉，DECISION-W 覆盖 DECISION-A 的「4 文件」口径）。[X] 拆分三子批：
  - **4a 契约先行**：framework 新建 `VfsOps` 契约（`framework/fs/vfs/ops_trait.rs`）+ reroute 3 文件 6 处消费面（proc_ops/epoll/page_fault）；不改 TCB 归属、不改语义，仅新增契约面 + 间接层。[X]（完成：VfsOps + Fallback + register/current，6 处 reroute；§2.3 六门槛全绿，见 §11 DECISION-W）
  - **4b 实现下沉**：`framework/fs/vfs` 实现整体迁 `services/fs`（types/dcache/vfs/open_file_table + handle/mount/path/flock/inotify + ramfs/devfs/initramfs/nestfs）；services 于 `fs::init` 注册 `VfsOps` 实现替换 Fallback；framework 侧仅留 2 契约（`VfsOps` + `vfs_poll_trait`）+ POD `VfsFileType` + nestfs unsafe 机制适配层。同时 `framework/credo/storage.rs` 连锁（登记项 2-F）整体下沉 `services/credo/persist`。[X]（完成：无壳单批做尽，63 处 `#[unsafe(no_mangle)] vfs_*` 壳删除；§2.3 六门槛全绿，见 §11 DECISION-W 4b 实施记录）
  - **4c 边界收敛**：ramfs/devfs/initramfs 等 framework 内 fs 消费者路径重定向 + §6.6 fs 项收窄（6→2 契约）收口 + 全量验证。[X]（完成：陈旧 doc 注释路径改写 + fs 域内核测试载体归属收敛，见 §11 阶段 4c 实施记录）
- 阶段 5：**壳删除 82 + 直接 use trait 化 20 + 保留文件 43 处收敛**（§7.5，ipc 24 第一优先）。[X]（完成：framework 域生产反向依赖归零，经 DECISION-J→K 全序列第 1~27 批实施；见 §11「阶段 5 实施记录」）
- 阶段 6：**全量验证**（§3 验收 + §9 门槛）。[X] 已执行——§9 七门槛 6 项达标（项 7 TCB 未达）+ §3 六项 4 项达标（项 1 TCB / 项 4 framework 文件数未达，见 §3）；本轮修复阶段 3 引入的 `scripts/qemu_boot_test.sh` 陈旧 grep 串回归。验证报告见 `docs/report/framekernel-paradigm-validation.md`。

### §7 DECISION-J 批次进度（2026-09-12 截止第八批）

描述：DECISION-J 所有权反转按批推进，每批独立验证（双架构 0w0e / clippy 0 / audit 全过 / host-tests / QEMU）。
方案：
- **已完成批次**（反向依赖 79 文件/137 行 → **50 文件/约 110 行**）：
  - J-1 ipc 类型反转（3519410e）→ J-2 config 常量（dfaa4918）→ J-3 config 8 壳（eed99468）→ J-4 ipc 4 纯壳删（98f9c2d6）→ **I-首战 IpcStrategy trait 注入**（f30febcb，**DECISION-K 修订已落地**：注册点前置 interrupt_late_init 前 + Option 降级 + 门禁测试，见 DECISION-K 修订执行记录）→ J-5 sync/types（dcdcb893）→ J-6 机制常量 4 壳（c7231ca5）→ J-7 wasm 5 壳删（e0e66351+3815390f）→ J-8 net 3 壳（75cafcae，route 合并解双向引用）
- **剩余批次计划**（按序，部分需专项/裁决）：
  - **fs 系列**：ramfs/devfs/nestfs/flock/inotify 被 framework VFS 机制消费 → 反转归位。**依赖闭包发现（第八批后调研）**：devfs/flock 依赖 `services::sync::irq_lock::IrqSpinLock`（= `framework::sync::IrqSpinLock` 类型别名，可替换）+ devfs 依赖 `services::fs::inode::Inode`（services 实现，**闭包不闭合** → 按 DECISION-K 项 5：inode 走 backend_trait 注入，不连带迁回）；ramfs_core 为目录模块（ramfs_data/ramfs_node，深度耦合 dcache/inode）——**建议专项评估依赖闭包后施工**；nestfs 为大型 ZFS 风格实现（反转工作量大的单批）；**procfs 壳已删（第九批 bc013bcb）**
  - **net 剩余 2**：syscall.rs（framework TCB 引用 `services::net::socket::{Domain,SockType,SockAddrIn}` + `services::net::unix::SockAddrUn`——协议 wire 类型，机制属性强，**建议迁回 framework（DECISION-J 模式），但涉及从大文件 socket.rs/unix.rs 抽取类型 + 大量引用改造，专项施工**）、init/sm_fi.rs（启动编排调 services uds/fd_alloc——功能编排，逐项判定）
  - **credо 专项**：安全敏感 + 框架/services 双份实现（audit/identity/capability/sha256/secure_boot/types 双份），3 壳（capability/types/sha256）被 framework re-export 消费 + identity.rs 引用 PwmEntry —— **需专项调研后裁决**，不贸然施工
  - **driver 2 壳（power/hdmi）**：`driver/power.rs` 为"framework 机制持有 `PM_SUBSYSTEM: PmSubsystem` 全局实例 + syscall 入口调 `services sys_pm_dispatch`"（同 IpcStrategy 模式：类型迁回 + 分发 trait 注入）；`driver/display/hdmi` 为 framework 机制文件 + 壳 re-export services hdmi——专项
  - **杂项**：barrier/types（framework 机制壳）、net/init/sm_fi.rs（启动编排调 services uds/fd_alloc——功能编排判定。**第十批后调研**：sm_fi 反依赖 3 处 = `uds_setsockopt` 委托（机制调策略 → SocketStrategy/UDS 委托 trait 注入）+ `fd_alloc::alloc_fd`（fd 表为内核机制但实现在 services，eventfd/signalfd/timerfd 亦调用 → **fd_alloc 迁回 framework 候选**））
  - **proc 系列**（约 13 文件）：核心子系统，types 壳 + 多个直接调用——需专项
  - **syscall 系列**（约 12 文件）：FFI 边界，部分需 trait 注入/接口化——需专项
  - **validate 壳**：ConfigValidateHook trait 注入（DECISION-I 顺序，ipc 之后）
- **待评审项**（2026-09-12 已评审，见 DECISION-K）：IpcStrategy 注册时序 / validate ConfigValidateHook / virtio-blk IRQ 专项 / storage 专项后续 / credо 与 proc 处理方式

### 委托批次清单 X/Y/Z（2026-09-13 用户确认登记，可直接开工）

> 承接 §6.4 未完成项（委托人所列 4 项）——方向全部已定（DECISION-G/H/F + 服务对象准则），无新决策点，剩余为排期执行。实施交委托人，审核员审查。

| 批次 | 内容 | 前置 | 验收 |
|---|---|---|---|
| **X** | ① char serial/vga 接线（char 桥模式，`9ba997e3` 已验证）+ ② 前置核实表更新（L941-L943：virtio 两行标"已核实 L962/L1008"、storage 两行标"归 storage 专项"）| 无（桥模式已跑通）| 双架构 0w0e + clippy 0 + 核心审计 + host-tests + **QEMU 冒烟**（接线触启动路径）|
| **Y** | ① storage 专项 5 子步（DECISION-H：0 号 identify helper → _block 适配器 → MSI-X/IRQ → storage_init 退位 → QEMU 存储冒烟）| 无（独立排期）| 每子步双架构 0w0e + host-tests + 专项 QEMU 存储冒烟 + audit_services_boundary |
| **Z** | ③ transport 去重（services `VirtioDevice` 删/薄代理，framework `VirtioMmioDevice` 保留，**已完成 `77407b8e`**）→ ④ NetOps 安全桥（同 CharOps 桥模式，net 中断驱动路径，**设计文档 [netops-bridge-design.md](archive/netops-bridge-design.md) 已审核通过（P1×2 + P2×3 修正后实施），实施完成**：framework `net_device_ops.rs` trait + 泛型桥 + DECISION-K 注册契约槽位；services `VirtioNetDriver` impl + `net_init` 填充；旧 framework `VirtioNet` 驱动 + `virtio_net_*` FFI + `VIRTIO_NET_OPS_STATIC` + `probe_all` 已删除）| ③ 是 ④ 前置（transport 单一化后桥接线更干净）| ③ 双架构 0w0e + 核心审计；④ 双架构 0w0e + clippy + 核心审计 + host-tests（含桥契约套件）+ QEMU aarch64 virt 挂网卡冒烟 |

**执行顺序**：X / Y 可并行（独立）；Z 内部 ③→④ 串行。④ 为接线最后一步（依赖 nic_probe_all 接入点确认）。

### 委托批次 X 执行记录（实施：AI）

- **X-① char 接线**：核对确认已由 `9ba997e3` 落地——services 权威（serial/vga impl Driver + char_init 经 chitin_register_driver 注册）、framework 删双份仅留 aarch64 pl011、lib.rs:794 合法编排接线；CharOps 读写桥休眠（SIMPLIFIED 注明，devfs 接入时补桥）。本批无代码改动。
- **X-② 核实表更新**：前置核实表 virtio 行标"**已核实**"（指向下文 virtio 净段两记录）、storage 两行标"**归 storage 专项**（DECISION-H，批次 Y）"。
- **验证**：双架构 0w0e ✅ / clippy 三维 0 ✅ / 核心审计 ✅ / host-tests ✅ / QEMU x86_64 冒烟 ✅（本链同时为 `058cb518` smoltcp 0.14.0 升级后首次全量验证）。

### 委托批次 Y 执行记录（实施：AI）

- **Y-① storage 专项 5 子步**（DECISION-H 全序列）：
  - **0 号 identify helper**：`nvme_read_identify_*` 解析迁 services（`05c9a648`，见上文记录）。
  - **_block 适配器**：services 新增 `AhciBlockDevice`（端口 → `BlockDevice` 适配 + IDENTIFY 容量探测）与 `NvmeBlockDevice`（namespace → `BlockDevice`），统一经 Chitin `register_block_device` 注册（`ahci{ci}-p{pi}` / `nvme{ci}-ns{nsid}`）；framework 旧 `_block` 适配层删除（ahci_block.rs / nvme_block.rs 孤儿文件移除）。
  - **MSI-X/IRQ**：NVMe MSI-X 端到端实装——**I/O CQ 必须在 MSI-X 使能后创建**（QEMU `nvme_init_cq` 仅在 msix_enabled 时 `msix_vector_use`，时序颠倒则完成中断永不到达，冒烟实证）；framework MSI-X ISR 纯编排，经 services 分发契约转发（DECISION-K 注册契约模式）+ MSIX-03 受控自测；AHCI 保持轮询提交。
  - **storage_init 退位**：framework 删 PCI AHCI/NVMe 探测/初始化/注册（约 470 行 storage_init + 约 1400 行控制器/寄存器代码），仅留 ATA 回退路径与机制原语（wire 类型 / DMA fill / MSI-X ISR 编排）；services `storage_init` 由 crate root lib.rs 编排调用（x86_64 门控）。（该 ATA 回退路径已于**阶段 3 收尾**整体迁 `services/driver/storage/ata`，见「阶段 3 执行记录」。）
  - **QEMU 存储冒烟**：NVMe MSI-X 端到端 Ok + PCI BAR5 解析修复（BAR 槽位保持）生效；AHCI 首次带盘冒烟暴露 4 项预存缺陷（见下），修复后双机型复验通过。
- **AHCI 首次带盘发现与本批内修复**（用户裁决 B）：
  1. **COMRESET 缺失**：HBA 复位后未发 `PxSCTL.DET` COMRESET 序列，PHY 链路不重建，带盘端口扫描恒报"0 端口活动"（pc ich9-ahci 与 q35 内建 SATA 均复现；framework 被删版同构缺失，历史冒烟从未挂 AHCI 盘，非本批回归）。补 `comreset`（DET=1 保持 → 释放 → 轮询 DET=3）+ `enable_fis_receive`（FRE=1 → 等 FR → 锁存 PxSIG）。
  2. **`AhciCommandHeader` 布局错位**：多余独立 `prdtl` 字段致结构体 36 字节（规范 4 DW + 4 保留 = 32 字节），CTBA 落 DW3 槽位而硬件按 DW2 读 → 读到恒 0。按 AHCI 1.3.1 §4.2.2 修正（PRDTL 是 DW0 高 16 位，非独立 DW；休眠单测 size==32 断言基线正确但从未运行）。
  3. **`ahci_fill_cmd_header` PRDTL 未移位**：`flags | prdt_len` 把 PRDTL 挤进 CFL 位域（CFL 变 6、PRDTL=0）。修正为 `flags | (prdt_len << 16)`。
  4. **`ahci_alloc_port_dma` 丢虚拟地址**：`cmd_list_virt` 硬编码置 0，services 填命令头写到虚拟地址 0（页 0 野写，恰被内核映射未崩溃），设备侧自 PxCLB 读到全零命令头（CFL=0/CTBA=0）静默丢弃命令、PxCI 恒挂。QEMU monitor `xp` 实证：命令表 phys 处 IDENTIFY FIS 字节正确、命令列表 phys 处全零。修正为保留全部 `alloc_coherent` 虚拟地址。
  - **伴随加固**：命令完成判定纳入 `PxIS.TFES`（错误完成也是完成，避免越界/中止类失败白烧 5M 自旋超时）；错误路径清 `PxSERR`（AHCI 1.3.1 §6.4.2 W1C）防端口楔死；二分探测（越界读 × 24 次，TCG 下 ≈50s 且依赖错误恢复收敛）改为 IDENTIFY 单命令容量探测（word 100-103，de facto 语义 = 总扇区数，与 Linux `ata_id_u64(id,100)` 一致，避免 +1 末扇区越界）。
- **复验结果**（pc ich9-ahci + q35 双机型，64MB SATA 盘）：带盘端口检测 `sig=00000101`（SATA）→ `ahci0-p0` 注册 **131072 扇区**（精确 64MB）→ Chitin `blk=3` → Ring 3，全程秒级；q35 同时验证双 AHCI 控制器共存（显式 ich9-ahci + 内建 00:1F.2）。
- **验证**：见 §9 验证链 4.6 复跑记录（修复后全量）。

### 委托批次 Z ③ 执行记录（transport 去重，实施：AI）

- **目标**：消除 services/framework 平行 virtio transport。services `VirtioDevice`（transport.rs，~500 行复刻实现）删除，framework `VirtioMmioDevice` 保留并补齐能力成为唯一 transport 实现。
- **framework 补齐**（`driver/virtio/mod.rs`）：`VirtioMmioDevice` 增 `vendor_id`/`version` 字段（probe 填充）+ 查询（`device_id/version/is_legacy/mmio_base/status/set_status`）+ 状态机（`reset/ack/set_driver/features_ok`）+ 特性协商（`device_features/set_driver_features`）+ virtqueue 细粒度配置（`select_queue/queue_num_max/set_queue_ready/notify_queue/setup_queue_addrs/setup_queue_legacy`）+ 中断（`interrupt_status/ack_interrupt_mask`）。全部 safe 方法，复用既有私有 `read32/write32/read64/write64`。
- **services 删除**：`transport.rs` 整体删除——`VirtioDevice`（平行实现）+ `VirtioDeviceKind`（blk/net 全未使用，死代码，F9 违规）+ 重复寄存器常量（`DEVICE_ID_*`/`MAGIC_VALUE` 等，framework 已有 `VIRTIO_ID_*`）。
- **services 改造**：blk.rs/net.rs/mod.rs 直接持有并使用 framework `VirtioMmioDevice`（0 unsafe 合法依赖）；`DEVICE_ID_BLOCK/NET` → framework `VIRTIO_ID_BLOCK/NET`；`device.ack_interrupt(mask)` → `ack_interrupt_mask(mask)`（framework 无参 `ack_interrupt()` 保留供旧调用）。blk/net 业务逻辑零变化（仅类型与 import）。
- **去重收益**：平行 transport 实现单源化（机制归 framework，符合 Asterinas 判据 + DECISION-H/HDMI 方向）；消灭 `VirtioDeviceKind` 死代码；重复常量收敛。
- **验证**：双架构 0w0e ✅ / clippy 双架构 0 ✅ / 核心审计全绿 ✅（services_boundary/safety/deadlock/coupling/comment/once_cell/invariants/reverse_deps 等）/ host-tests ✅ / **aarch64 QEMU virt 挂盘冒烟**：`virtio-blk: registered device #1` + Chitin blk=1 + Entering EL0 ✅ / x86_64 boot 回归 1/1 ✅。TCB 66.9→67.1%（transport 机制入 framework 的预期推高，软门槛既有超标非本批引入）。

### 委托批次 Z ④ 执行记录（NetOps 安全桥，实施：AI）

- **目标**：services `VirtioNetDriver`（0 unsafe）接入 smoltcp。`ChitinNetDevice` 要求 `&'static NetOps`（extern "C" 指针表，需裸指针转换），services 无法直接构造 → framework 提供安全桥 trait + 泛型桥（monomorphization 生成 extern "C" 回调，unsafe 转换全部留在 framework）。设计见 [netops-bridge-design.md](archive/netops-bridge-design.md)（审核通过，P1×2 + P2×3 修正后实施）。
- **framework 新增**（`net/net_device_ops.rs`）：`NetDeviceOps` trait（send/try_receive/get_mac/handle_irq 默认空）+ `net_ops_for::<T>` 泛型桥（Box::leak 生成 `&'static NetOps`）+ `register_net_device::<T>`（Box::into_raw 所有权转移，注册前安全读 MAC）+ DECISION-K 注册契约槽（`NET_SERVICES_DRIVER: OnceLock<fn() -> Option<NetDeviceRegistration>>` + `net_register_services_driver` set-once + `net_services_driver` 单向拉取）。`net_services_driver` 按 kernel_test cfg-out 同步门控（probe 模块 kernel_test 下不编译，F9）。
- **framework 删除**：`driver/virtio/net.rs` 旧 `VirtioNet` 驱动（~620 行）+ `virtio_net_*` FFI + `VIRTIO_NET_OPS_STATIC`；`nic_probe_all` virtio 分支改经槽位拉取（e1000 失败后）。
- **services 接线**：`VirtioNetDriver` impl `NetDeviceOps`（`try_receive` 显式全路径 `VirtioNetDriver::try_receive(self, buf)` 防同名递归，P1-1）+ `finalize`（vq0/vq1 MMIO 配置 + DRIVER_OK + RX 预填）+ `net_init` 填充探测回调（crate root 在 `qx_net_init` 前编排）。`virtio_net_registration` 扫描 virtio-mmio 发现 `VIRTIO_ID_NET` 即 `VirtioNetDriver::new` + `finalize` + `register_net_device`。
- **验证**：双架构 `build all` 0w0e ✅ / clippy `--release -D warnings` 双架构 0 ✅ / clippy pedantic (lib x86_64) 0 ✅ / clippy kernel_test 维 0 ✅ / 核心审计 + 8 项补充全绿 ✅ / host-tests 755/0（99 套件，含新增 `net_device_ops_bridge_test` 3 测试）✅ / **QEMU aarch64 virt 挂网卡冒烟**：`virtio-net: probed successfully (services bridge)` + Network Subsystem Ready ✅（`qemu_boot_test.sh` aarch64 段已加 `-device virtio-net-device` + 桥探测断言，P2-5）/ x86_64 boot 回归 1/1（Ring 3）✅。
- **遗留修复**（实施期发现，批次 Y `4994cbba` 引入）：AHCI `identify` clippy pedantic 违规（similar-names + manual-let-else）→ 按审核裁决修复（let-else 根治 manual-let-else + `#[expect(clippy::similar_names, reason=...)]` 兑底，与同文件 read/write_dma 双 expect 模式一致；read/write_dma 预存不动，§12.2）。
- **预存登记（实测修正）**：`--features host-test --lib` clippy 报 E0152 duplicate lang item `owned_box`——**环境条件触发**（cwd=src/rust 目录内时 rustup 加载 rust-toolchain.toml 的 rust-src 组件 → build-std 生效，与 host std 的 alloc 冲突；DECISION-021 同族工具链限制），与代码改动无关。**实测**：CI 等价命令（repo 根 + `--manifest-path`）与 audit.sh 2b 等价命令（repo 根 + `+nightly`）强制重编译均 0 error 通过；host-tests 从根构建不受影响——**验证链无碍，无代码/脚本缺陷**；根治走 §10「构建模式显式化工程」（可立即开工）。

### 阶段 3 执行记录（驱动双份合并收尾 + E1000 回迁，实施：AI）

- **目标**：§6.4 全表收口。批次 X/Y/Z 已逐项接线（char/virtio/storage 主体），本阶段收尾 2 项：① storage `ata`+`ata_block` 双份（framework ATA PIO 回退路径 → services）；② DECISION-B E1000 业务回迁（framework `driver/net/e1000` → services 单一驱动）。
- **ATA 迁移（①）**：
  - **framework 侧删除**：`framework/driver/storage/ata.rs` + `ata_block.rs` 整体删除；`framework/driver/storage/mod.rs` 移除 ATA 段与 `ata_block` 模块，仅留机制薄层；`framework/driver/mod.rs` 移除 ATA re-export。
  - **services 侧实装**：`services/driver/storage/ata.rs` 为完整真实 PIO 驱动（端口 I/O 全经 framework safe 代理 `IoPort::new_safe`，0 unsafe），`AtaController` + `AtaBlockDevice`（块设备适配器经注册表查找控制器）；容量经 IDENTIFY word 100-103（LBA48）回落 word 60-61（LBA28），不做二分探测。
  - **注册形态**：跟随 ahci/nvme 模式，`services/driver/storage/mod.rs` 新增 `ATA_CONTROLLERS` 注册表 + `storage_init` Step 5 仅注册块设备 `ata0-3`（不注册控制器设备、不 `impl Driver`）。
- **E1000 回迁（②，DECISION-B 落地）**：
  - **framework 收敛为机制层**：`framework/driver/net/e1000.rs` 裁剪至仅 `TxRing`/`RxRing` 环机制（DMA 环改用 `DmaEngine::alloc_coherent` + 补 `unsafe impl Send/Sync`）；`framework/driver/net/e1000_io.rs` 保留 `E1000Io` MMIO 安全访问器（原 `E1000Driver` 业务 / FFI 回调 / `no_mangle` 壳已删），模块经 `#[cfg(all(target_arch="x86_64", not(feature="kernel_test")))]` 门控（唯一消费者为 services x86_64 驱动，避免 aarch64/kernel_test 构建下寄存器常量死代码，F9）。
  - **services 权威**：新增 `services/driver/net/e1000.rs` 单一 `E1000NetDriver`（0 unsafe，impl `NetDeviceOps` 经泛型桥生成 extern "C" 回调 + `register_net_device`，PCI 探测经 framework safe API），业务含复位 / 链路探测 / 描述符环配置 / 收发。
  - **复合探测**：`services/driver/net/mod.rs` 新增 `net_services_probe`（`e1000_net_registration` 失败回落 `virtio_net_registration`），`net_init` 注册 DECISION-K 槽；非 x86_64 提供返回 `None` 的 e1000 桩。
- **验证（§2.3 六门槛全绿）**：`./ci/build.sh all` Passed 5 / Failed 0（双架构 0 error / 0 kernel warning）✅ / `./ci/audit.sh quick` rc=0（0/6 TCB 边界、0.5b TCB 59.0%、0.5c 6 不变式、0.5 SAFETY 1822/1822 100%、0.5d~0.5j 补充防线、1/6 双架构 check、2/6 clippy pedantic、**2b/6 clippy kernel_test + host-test 双维**）✅ / `make test-host` 各套件 ok ✅ / `make test-kernel-host` 941 passed / 0 failed ✅ / `./scripts/qemu_boot_test.sh x86_64` 1/1（VFS ready + Ring 3 + KPTI-09）✅。
- **实施期修复（本轮改动直接导致）**：① `services/driver/virtio/mod.rs::virtio_net_registration` 补 `#[cfg(not(feature="kernel_test"))]` 门控（唯一消费者 `net_services_probe` 同门控，避免 kernel_test 维死代码 → 门槛 2b 失败根因）；② `services/driver/storage/ata.rs::test_identify_capacity` 白盒期望字面量订正（原将 words[101] 误作位 48-63；实现符合 ATA 规范 word100 为最低 16 位，改为全 4 word 覆盖 → 门槛⑤ 失败根因）。

## 9. 验证门槛

描述：每阶段提交必须满足（§2.3 + 本工程专项）。
方案：
1. 双架构 cargo check --release 0 error / 0 warning
2. clippy -D warnings 0
3. 核心审计全过（audit_services_boundary / audit_coupling / audit_tcb_ratio 关注 TCB 下降）
4. host-tests 全过
5. QEMU 集成测试（boot/驱动改动必跑）
6. **反向依赖计数单调下降至 0**（`crate::services` 引用，生产口径；起点 136 处/78 文件 → 0，核验脚本 `audit_reverse_deps.py`）
7. **TCB 占比逐阶段下降**（60.1% → <30%）

## 10. 关联工程

- 分册 9 B09-13（反向依赖治理）→ 本工程 §7 吸收。
- 分册 9 B09-12（vfs/api）→ VFS 部分被 DECISION-A 推翻；Errno/error 基础库迁回保留。
- B04-AUDIT-005（E1000 上移）→ 被 DECISION-B 回迁。
- 分册 5/6 系列迁移（T1-T9/E6）→ 已下沉成果为本工程基础。
- AGENTS.md §4.1 / explain-framekernel.md（2026-09-11 决策树补全）→ 判据来源。
- **方案 D（登记，2026-09-13）：kernel 独立 crate 化（RA `#[path]` 误报根治）**
  - 背景：`src/rust/src/lib.rs:188` `#[path = "../../kernel/mod.rs"] pub mod kernel;` 使 rust-analyzer 报 `unresolved module`（跨 crate 目录 `#[path]` 是 RA 已知解析缺陷），但 cargo 双架构 0 error（路径实际有效）。
  - 方案：将 kernel 改为 workspace 独立 crate（成员 crate），lib.rs 以正常 `use` 依赖而非 `#[path]` 内嵌模块——RA 原生支持 crate 依赖，根治误报。
  - 代价：Cargo.toml workspace 成员 + 全部 `crate::kernel::` 路径引用改写（大量），独立工程。
  - 状态：**实施完成（2026-09-14）**。kernel 独立 crate（src/kernel）+ queenx 壳（pub use kernel）；`crate::kernel::`→`crate::` 3370 处；Makefile/CI/audit 检查点指向 kernel manifest + clippy.toml/rustfmt.toml/.cargo config 随迁；host-tests 静态契约测试同步；RA 误报根因（跨 crate #[path]）移除。验证：双架构 0w0e + clippy + audit quick + host-tests 755/0 + QEMU 双机型 + kernel_test 链接。实施记录见 `docs/plan/archive/kernel-crate-separation.md`。
  - 关联：与分册 9 B09-19 孤儿测试治理无冲突；与 F/S 分层无冲突（纯工程结构改造）；为后续拆 services 独立 crate（编译器级 F1 services 0 unsafe / F3 无环依赖）铺路。
- **预存审计积压登记（登记，2026-09-13）：六项审计积压处置分类**
  - 来源：委托人报告（§12.5 报告），按"登记待处置"处理，与方案 D 同类，**不阻塞批次 Z 开工**。
  - 项目与处置：
    1. **audit_unwired_pub_fn（CRITICAL=157）**：非违规，属治理进度跟踪（公共 API 未被接线引用）。已有分册 9 B09-05/B09-17 计划覆盖，作为进度工具不豁免、不本工程处理。
    2. **audit_public_api_docs（缺中文文档 2204 处）**：量大 + 低风险（F8 文档性软规范）。**待用户裁决**：A 豁免软规范（推荐）/ B 单独立项补全 / C 保持硬门槛。
    3. **audit_implicit_deps（services 直访 framework 全局 122 处）**：与反向依赖同源（framework 全局被 services 直接引用），并入本工程 §7 trait 化改造覆盖，作为 §7 基线指标（批次 Z 开工前置 3 分钟对齐基线）。
    4. **audit_smoltcp_purity**：随 smoltcp 0.14 升级刚验证通过，无需处理。
    5. **audit_edition2024 / audit_feature_semantics**：数据待确认，归零或单列待定。
  - 状态：登记待处置；仅 implicit_deps 需批次 Z 开工前基线对齐（3 分钟），其余不阻塞。
  - 关联：157 → 分册 9 B09-05/B09-17；122 → 本工程 §7；2204 → 待用户裁决（A/B/C）。
- **构建模式显式化工程（登记，2026-09-13）：E0152 整族根治（对齐 Asterinas osdk 命令层注入）**
  - 背景：`src/rust/.cargo/config.toml [unstable] build-std` 全局生效，src/rust 目录内 host-target 构建触发 E0152（build-std 与 host std 双 alloc）；项目以"cwd 隐式约定"规避（裸机在 src/rust 内、host 从根），4+ 处登记（DECISION-021 同族）。
  - 参照：Asterinas osdk 命令层注入（`-Zbuild-std=core,alloc,compiler_builtins` + 显式裸机 target，无全局 config）——成熟实践。per-target build-std 实测语法不存在（cargo 拒绝：`expected a table`）。
  - 方案：config.toml 删全局 build-std；所有裸机构建入口显式注入 `--config 'unstable.build-std=["core","compiler_builtins","alloc"]' --config 'unstable.build-std-features=["compiler-builtins-mem"]'`（已实测可行，25s 裸机 release check 通过）。
  - 前置① RA（已完成）：RA 默认 host target 检查，不依赖全局 build-std；现状（有 config）RA 无 E0152 异常 → 移除后不可能退化，无静默退化点。
  - 前置② 入口盘点（已完成，~17 处直接 + 1 间接，全部 cwd=src/rust 依赖 config）：build.sh `build_arch` 1 处 + Makefile 7 处（L142/186/194/201/208/212/217）+ audit.sh 2 处（clippy L153/lockbud L220）+ ci-x86.yml 4 处（L41/91/176/193）+ ci-aarch64.yml 1 处（L41）+ ci-bench.yml 1 处（L106）+ ci-lint.yml 2 处（L243/248）+ qemu_boot_test.sh 间接。host 维入口（wd=repo 根，ci-x86 L207/226）不注入、不动。**实施期修正**：Makefile L142/186（user 程序，cwd=src/user 用 src/user/.cargo + rustup 预装 core）实测不需要 build-std，不注入；audit.sh 第 1 步双架构 check（L133）补注入。
  - 实施步骤：① config.toml 删 `[unstable] build-std` ② build.sh / Makefile 顶部定义统一注入变量、CI yml 各裸机步内联同参（示例：`BUILD_STD_CFG="--config 'unstable.build-std=[\"core\",\"compiler_builtins\",\"alloc\"]' --config 'unstable.build-std-features=[\"compiler-builtins-mem\"]'"`，`cargo build $BUILD_STD_CFG --target ...`）③ 全量验证（§2.3 全门槛 + src/rust 内 host clippy 确认 E0152 消失 + repo 根裸机构建确认注入生效）。
  - 风险（实测修正）：**漏注入非显式失败**——实测无 `--config` 裸机构建时 cargo 回退 rustup 预装 core **静默成功**（非 fail-loud），产物可能与 build-std 不一致；故全入口注入为必要保障，**未来新增裸机构建入口必须注入（静默退化非报错）**，最终正确性靠 QEMU boot 实证兜底。`--config` 双引号嵌套在 yml/Makefile 转义易错（已用 bash 数组 / make 变量封装）。
  - 状态：**实施完成（2026-09-14）**。验证：双架构 `build all` 0w0e ✅ / clippy `--release -D warnings` 双架构 0 ✅ / `audit.sh quick` 全绿（REAL_EXIT=0，含 check/clippy pedantic/lockbud 注入 + kernel_test/host-test 维）✅ / QEMU x86_64 Ring 3 + aarch64 virt 挂网卡冒烟 1/1（实证注入产物正确，含 Makefile 注入链）✅ / src/rust 内 `--features host-test` clippy E0152 消失 ✅（根治目标达成，cwd 不再影响构建模式）。
  - 关联：DECISION-021/022、eliminate-parallel-implementations.md 工程计划 A/B、audit-fix-08 G-01、本工程 L386/L1072 登记。

## 11. 中途问题与决策记录

> 本工程实施过程中遇到的问题与用户决策登记（变更历史由 git 提交承载，此处仅登记决策内容与理由）。

### DECISION-C: aarch64 clippy 回归处置（用户裁决方案 B — 显式导入）

- **发现时机**: 阶段 0 验证 aarch64 `clippy -D pedantic` 时，`mm/vmm_aarch64.rs:14` 报 `wildcard_imports` 错误（`use super::*;`）。
- **根因**: 既有 commit `a7851509`（分册 9 死代码治理，2026-09-11）将本文件的 `#![allow(clippy::wildcard_imports, clippy::cast_possible_truncation)]` 当作"过时 allow"删除，但 `use super::*;` 仍在——该 allow 保护的是 aarch64 移植约定（glob 导入 mm 父模块全部公开 API，与 x86_64 侧同步），删除即 CI 回归。
- **候选方案**:
  - A: 恢复 `#![allow(clippy::wildcard_imports)]`——保留移植约定，改动 1 行，风险最低；
  - B: 按 clippy 建议改显式导入清单——消除 glob，风格与 x86_64 侧（已无 glob）一致，更干净。
- **用户裁决**: B。已将 `use super::*;` 改为 `use super::{PAGE_NX, PAGE_SIZE, PAGE_USER, PAGE_WRITABLE, PageFlags, PageSize, PhysAddr, VirtAddr, get_pmm};`；`super::KERNEL_BASE` / `super::kpti::kpti_init` 保留显式路径。
- **状态**: [X]

### DECISION-D: 阶段 0 范围登记（三项顺延至对应阶段）

- **描述**: 阶段 0 原列的 DmaStream 收敛裸指针 / FFI 薄层分离 / calibration 采样回调，经调研确认与后续阶段绑定，登记顺延：
  - **DmaStream 收敛裸指针**: 当前已无 pub 裸指针返回（`cpu_addr` 返回 `NonNull<u8>`，构造走 safe `from_frame`），待阶段 3 驱动下沉时随业务验证；
  - **FFI 薄层分离**: 属阶段 6.2/6.3 下沉动作的一部分（syscall 用户指针拷贝集中框架、中断/panic 上下文经 trait 注入）；
  - **calibration 采样回调**: boot 早期路径（PIT 参考时钟），随阶段 6.3 timer 部分下沉一并实施。
- **状态**: [X]（登记，不在阶段 0 交付）

### 阶段 0 验证结果（§9 门槛）

| 门槛 | 结果 |
|---|---|
| 双架构 cargo check --release 0w0e | ✅ x86_64 + aarch64（RUSTFLAGS=-D warnings） |
| clippy -D pedantic 0（除 cast_*） | ✅ x86_64 + aarch64 |
| 核心审计 | ✅ boundary 0 / safety 100% / coupling 0 / comment 0 / deadlock 0（1 项 HIGH 为既有 smp_init.rs）/ invariants 全 PASS |
| host-tests | ✅ 全过（exit 0） |
| QEMU | ⚠️ 未跑——x86_64 进 Ring 3 卡点（display→usb 区间）为既有 ISSUE-RT-001，与本阶段改动正交；本阶段为纯机制重构 + fallback 保持行为 |

### DECISION-E: proto_block 归属（初裁 → 被审核员推翻，见 DECISION-F）

- **矛盾**: §6.1 列 `framework/chitin/proto_block.rs` 下沉 services（0 unsafe 纯注册逻辑）；§6.6 保留清单含"chitin 注册表 + proto 指针表 + user_driver"。
- **初裁（本工程，已作废）**: 依据 Asterinas 范式实证（`kernel/core/src/device/registry/block.rs` 在 safe 层）+ Q1/Q2/Q3 判据，裁决"§6.1 正确、下沉 services"。
- **审核员推翻（最终裁决，DECISION-F 吸收）**: proto_block **保留 framework**——它是 chitin 注册表机制的**安全导出面**（机制之"嘴"），下沉会制造 5 处 framework 调用方（driver/storage ×4、chitin/composite ×1）→ services 反向依赖，违背 Minimalism + 依赖单向。
- **状态**: [X]（初裁作废，最终裁决见 DECISION-F）

### DECISION-F: 服务对象准则定案 + §6.1 复核终局（审核员裁决）

- **判据修正（写入 §2）**: `0 unsafe` 只说明"不必须 framework"，**不决定归属**——归属看**服务对象**：
  1. framework 机制对 services 的**安全导出面** → 保留（例: IoMem::from_pci_bar / userptr safe 构造 / proto_block::register_block_device）；
  2. 仅被 services 内部 / 经既有分发机制（dispatch_trait）消费 → 下沉；
  3. 被 framework 机制直接调用 → 下沉需接口化（trait/回调/Chitin 注册），否则保留。
- **§6.1 复核终局**（逐文件查"framework 侧保留代码是否直接调用它"）:
  - ✅ **3 确认下沉**: syscall brk/canary/posix_timer（经 dispatch_trait、无 framework 残留调用）——已提交 `c6455358`；
  - 🔒 **17 保留**: 其余全部被 framework 机制直接调用或是机制安全导出面——proto_block（chitin 注册表安全导出）、proc/canary（process.rs:332 机制直接依赖 + services 顶层 re-export 消费）、net/init/dns（net init cmd.rs 调用）、driver/hotplug（syscall/dispatch.rs:960 + init_all 调用）、driver/bus×2（init_all 调用）、driver/display×4（显示机制内部 + console 消费）、chitin/firmware（devtree.rs:94 直接引用）、credo/engine（session/api/user_driver 直接调用）、barrier/fault_inject（recoverable.rs:71 调用）、barrier/reset×4（恢复机制本体）。
- **审计脚本纪律**: 凡确认下沉的文件，其审计白名单（如 audit_block_registration）仅在归属变更后同步路径，禁止先改白名单再迁移。
- **状态**: [X]

### DECISION-G: §6.4 双份合并复核（接线实证，2026-09-12 调研）

> 按"核查后再动工"纪律，对 §6.4 全部 20 项做接线实证（谁被 init_all/services 实际调用），发现原表方向与真实状态有较大出入，**不能按原表直接执行**。

**实证结论**：
1. **framework/driver/mod.rs `init_all` 全部接线 framework 侧驱动**（char/bus/storage/input/display/usb/hotplug，L197-226）——framework 驱动是 **active 权威实现**；services 侧驱动（storage/nvme+ahci、char/serial+vga、virtio/blk+net）是**真实实现但未接入启动路径**的影子（即既有 MIG-005 未理清的双份）。
2. **services/usb×5（enumerate/hid/mass_storage/ring/usb_core）、chitin/devtree、driver/net/e1000、uefi、kexec、firmware** 均为 `pub use crate::kernel::framework::...::*` 的 **re-export 壳** → 属 §6.5 壳删除，**不是** §6.4 合并对象。
3. **services/credo/grants+sessions、services/chitin/composite** 是 framework 机制（grant/session/composite 机制）之上的**策略层/安全代理**——按服务对象准则（安全导出面保留）是**正确形态**，framework 版本应保留，无"删业务"。
4. **display/hdmi**：**已实证（第二十七批）**——framework/driver/display/hdmi/ 整目录未挂载（`display/mod.rs` 无 `pub mod hdmi;` 声明），mod.rs 壳 + 7 孤儿文件零外部消费者（目录外 `hdmi::` 引用均指向 services 权威实现），已按 §6.4 备注行预授权整目录删除。

**处置**：§6.4 原表**暂缓执行**，分类改为：
- 🔒 壳（→§6.5 删壳，非本阶段）：usb×5、chitin/devtree、e1000、uefi、kexec、firmware
- 🔒 机制/策略正确形态（保留 framework，无重复）：credo/grant+session、chitin/composite
- ⚠ 真双份（framework wired active + services 影子）：storage×7、char×2、virtio×2 —— **方向裁决：直接方案 B**（见下）
- ✅ display/hdmi（7 文件孤儿）已实证为整目录未挂载死文件，第二十七批删除（方向 A services 收敛）

**方向裁决（审核员，2026-09-12）——真双份 11 项执行"直接方案 B"，取消"先 A 后 B"**：
- **理由**：services 影子是"真实实现但未接线"（内容完整）——直接 B = 接线切到 services + framework 留机制删业务，一步到位；"先 A"会误删可复用 services 实现，B 时仍需从 framework 迁回（重复搬移）。接线改造风险是 A 后 B 也必经的，A 只推迟不消除。
- **执行要求**：
  1. **前置核实**：11 项 services 影子内容自足性（0 unsafe ✓ / 硬件访问经 IoMem/IoPort/DmaStream/Chitin 机制 API / 业务自含不依赖 framework 内部）。**完整 → 直接接线**；**不完整（半成品/依赖 framework 内部）→ 该项改从 framework 迁业务**（仍是 B 形态）。核实不通过不构成选 A 的理由。
  2. **接线改造**：init_all 从"调 framework 驱动"改为 **Chitin 注册分发指向 services 驱动**（§7.4 接口化模式）。
  3. **framework 退位**：留 IoMem/IoPort/DmaStream/Chitin 注册机制，删驱动业务。
  4. **分子类推进**：先 1 子类（char 或 storage）→ QEMU 驱动冒烟 → 再扩展；每子类跑 audit_services_boundary。
  5. **MIG-005 收尾**：真双份 11 项即 MIG-005 遗留，本阶段接管（§10 关联登记）。

**状态**: [X]（复核登记 + 方向裁决完成；§6.4 施工按直接方案 B 执行）

### DECISION-H: storage 转独立专项（内容自足性核实发现半成品，2026-09-12 审核员裁决）

> **前置核实结论**（委托人）：services storage 是 Phase 2.1.3/2.1.4 的"并行实现但内容不等价"半成品——缺 MSI-X/IRQ 路径（NVMe B07）、I-42 IRQ、ATA PIO 真实驱动、MSIX-03 验收钩子、`_block` 适配器与注册路径；services/driver/mod.rs 头注自认"迁移中"。**不符合"直接接线"前置**（DECISION-G 的"不完整"分支）。

**裁决**：
1. **storage 从 §6.4 剥离为独立专项工程**（framework 迁业务路径，非接线路径）：
   - 0 号子步（立即）：`nvme_read_identify_*` 解析 helper 迁 services（纯逻辑 + host 测试）
   - services 补 `_block` 适配器 + `impl BlockDevice` + Chitin 注册路径
   - services 补 MSI-X/IRQ（NVMe B07）+ I-42 中断路径
   - framework `storage_init`（x86_64 大函数：PCI 扫描 + MSI-X + 验收钩子）整体退位
   - 接线 + QEMU 存储冒烟 + audit_services_boundary
2. **方案 C（强行接线）排除**：丢 MSI-X/I-42/ATA PIO/验收钩子，违反"不损失功能"原则。
3. **主线程行（不阻塞）**：§7 反向依赖治理（核心目标，独立于 driver 双份）；§6.2 复核。
4. **char/virtio-blk 同步前置核实**（同 storage 标准：MSI-X/IRQ/_block 适配器/注册路径等值存在？）——半成品 → 转独立专项；完整 → 按 DECISION-G 直接接线。

**状态**: [X]（裁决完成；storage 专项另立，主线转 §7 + §6.2 复核）

### DECISION-I: virtio-blk IRQ + §7 施工顺序（长期最优，2026-09-12 审核员裁决）

1. **virtio-blk IRQ**：**转专项补 I-42 路径**（services 补中断驱动 + framework 留 virtqueue 机制），**不接受轮询为功能等值**——轮询 CPU 占用/延迟不等值；丢 IRQ = 降级迁移，违反"不损失功能"铁律；与 storage 缺 MSI-X 转专项同一标准；Asterinas virtio-blk 中断驱动在 kernel。轮询仅作专项完成前过渡兜底，不作终态。
2. **§7 施工顺序**：采纳**先 §6.5 壳删除（分批，每批双架构 0w0e + audit_services_boundary + host-tests）→ 再 trait 注入**。理由：壳删 -70 反向依赖清障、验证 services 顶层 API 完备、为 IpcStrategy 注册时序设计提供干净依赖面。**IpcStrategy trait 注入为 trait 化首战**（ipc 24 处最大头，13 处 FFI 集中）；注册时序（framework 机制先启 → services 注册 → 使用）单独评审。

**状态**: [X]（裁决完成）

### DECISION-J: 壳删除 = 所有权反转路径（机制项迁回 framework，2026-09-12 审核员裁决）

> **障碍实证**（委托人）：§6.5 壳非纯机械删除——壳存在 = framework 生产代码持有 services 定义的类型/常量（`framework/ipc/mod.rs:89` 的 `IPC_NAMESPACE: RacyCell<IpcNamespace>` 持有 services 类型；`framework::config::{PAGE_SIZE, MAX_CPUS}` 被 iobuf/cpu_local/smp/rcu/irq/mm 机制大量使用）。

**裁决**：
1. **壳删除路径 = DECISION-A 式所有权反转**：机制项（类型/常量）迁回 framework，services 侧改 `pub use framework::...::*` 保持 API 兼容，再删 framework 壳、清理引用。
2. **统一判据**：机制持有的数据结构/常量归 framework，功能实现归 services——与 DECISION-A（VFS 下沉）不矛盾，是同一判据的两面。
3. **边界（防 B09-12 治标重演）**：
   - 反转对象仅限机制项（framework 机制持有/使用的类型/常量）；
   - 策略逻辑禁止随迁（services 的 sem/msgq/pipe 策略权威保持 services）；
   - 依赖闭包检查（迁回类型依赖的 services 项一并处理，防迁一半）；
   - services re-export 保持 API 兼容，内部引用路径同步。
4. **第一批（ipc 类型反转）通过**：`IpcNamespace`/`Pipe`/`MsgQueue`/`ShmSegment`/`Semaphore`/`Message`/`WaitQueue`/`IPC_MAX_*` 迁回 `framework/ipc/types.rs`（~500 行）——衔接 DECISION-I（ipc 24 处第一优先 + IpcStrategy 首战清依赖面）。
5. **每批验证**：双架构 0w0e + audit_services_boundary 0 + host-tests + `kernel::services` 引用计数下降。

**状态**: [X]（裁决完成）

### DECISION-K: 五项待评审结论（2026-09-12 审核员评审）

1. **IpcStrategy 注册时序**：**长期最优（2026-09-12 修订）——注册点前置 + 时序契约化**：
   - **注册点前置**：`register_default_ipc_strategy()` 紧随 framework `ipc_init()` 后立即（kernel_init 早期），删去"VFS 后"约束——依据：`DefaultIpcStrategy` 零字段构造 + `static` 零初始化 + OnceLock 存指针，**注册零依赖**；策略方法**惰性调用**（用户态 syscall 才执行 `*_safe`，彼时 VFS 早已就绪）——注册点与调用点分离，注册无需等 VFS。
   - **启动契约化**：注册点位置固定 + `// IPC 策略注册契约点` 注释；添加**时序门禁测试**（host-test 断言注册在 IPC FFI 可达路径之前必然执行）。
   - **panic 语义（2026-09-12 二次修订）**：**运行时 release 不 panic**——`current_ipc_strategy()` 返回 `Option`，未注册 → 调用点返回 `Err(ENOSYS)` + 显式错误日志（"IpcStrategy 未注册：检查 kernel_init 编排"）；**开发期**由时序门禁测试 + `debug_assert` 捕获。**依据**：QX panic 触发 barrier 系统级恢复（`panic!()→PANIC_FLAG→int 0x82`）——"策略未注册"是确定性逻辑错误（启动顺序 bug），非可恢复故障，panic 会使逻辑错误错配恢复机制（syscall 错误升级为系统恢复事件）。**统一原则：逻辑错误一律降级 + 日志，不进恢复流程**（与 validate Option 可空一致）。13 处 FFI 调用点 `let Some(s) = current_ipc_strategy() else { log + return ENOSYS }`。
   - **演进预留**（SIMPLIFIED）：未来策略增多时演进为阶段化注册表（PHASE_INIT→PHASE_SERVICES→PHASE_RUN 阶段机校验），当前不引入。
2. **ConfigValidateHook trait 注入**：模式同 IpcStrategy ✓；**并入统一"机制 init 后立即注册策略"启动契约**——注册在 framework `config::init()` 之前（机制初始化后立即，services validate 依赖闭包轻可极早注册）；未注册语义 **Option 可空**（未注册跳过校验 + 打日志——validate 是启动增强）。**与 IpcStrategy 统一"逻辑错误降级原则"**（均不 panic；差异仅在降级语义：validate 跳过校验 vs IPC 返回 ENOSYS）。
3. **virtio-blk IRQ 专项**：确认 DECISION-I——services 补中断驱动（IrqLine + 完成通知）→ framework 留 virtqueue 机制 + ISR 注册原语 → 接线 + QEMU 冒烟；轮询仅过渡兜底。
4. **storage 专项后续子步**：按 DECISION-H 推进（补 _block/MSI-X/I-42 → storage_init 退位 → 接线 + QEMU 冒烟），无新裁决。
5. **fs 依赖闭包 / credo / proc**：统一"持有者判据 + trait 注入"——
   - **inode.rs 选 trait 注入（不连带迁回）**：Inode 是契约接口留 framework，具体 FS inode 实现经 backend_trait 注入（services 注册）；**连带迁回会把 ext2/exfat 具体实现拉进 framework = B09-12 治标重演，禁止**；
   - fs 机制持有的具体类型（framework 全局表/句柄）→ 迁回（DECISION-J 模式）；策略留 services；
   - credo：framework 留 C ABI + 安全原语（csprng/audit/identity/secure_boot），策略（auth/policy/grants/sessions）留 services，持有类型迁回；
   - proc：framework 留进程表/调度器机制，策略（sched_policy/seccomp/session）留 services，跨层策略经 ProcStrategy trait 注入。
6. **nestfs 注入归零（2026-09-13 委托人选定方案 B）**：nestfs 29 文件 ZFS 风格实现整体留 services（不进 framework，TCB 最小化）；framework 挂载/格式化消费点改经 `register_nestfs_fs` 注册表 + `FileSystem::fs_format` trait 分发（未注册 fail-closed）；`services::fs::init()` 注册契约统一承载 FsBackend/VFS poll/NestFS/热插拔四项注册。

**状态**: [X]（评审完成）

### DECISION-K 修订执行记录：IpcStrategy 注册点前置 + Option 降级（2026-09-12 委托人实施）

> 按 DECISION-K 两项修订实施并验证（此前 I-首战 f30febcb 为"VFS 后注册 + 未注册 panic"，现已修正）。

- **注册点前置**：lib.rs 注册块从"VFS 后"（原 9-0.5 节）前移至 `interrupt_late_init` 之前（5.75 节，kernel_init 早期），带 `// IPC 策略注册契约点 (DECISION-K)` 注释。依据：`DefaultIpcStrategy` 零字段构造 + static 零初始化 + OnceLock 存指针，注册零依赖；策略方法惰性调用（用户态 syscall 才执行），注册点与调用点分离。
- **Option 降级语义**：`framework/ipc/strategy.rs` 的 `current_ipc_strategy()` 由 `&dyn`（未注册 panic）改为 `Option<&dyn IpcStrategy>`（`.copied()`，未注册 None）。依据：QX panic 触发 barrier 系统级恢复（int 0x82），"策略未注册"是确定性逻辑错误（启动顺序 bug），非可恢复故障——**逻辑错误一律降级 + 日志，不进恢复流程**。
- **13 处 FFI 降级**：pipe(5)/shm(4)/msgq(4) 每处 `let Some(s) = current_ipc_strategy() else { klog_warn!("IpcStrategy 未注册: <fn>"); return <哨兵> }`；哨兵值：i32/i64 类返回 `-(Errno::ENOSYS as i32/i64)`（=-38），create 类（返回 IpcId）返回 0（无效 id），`is_pipe_fd` 返回 false。
- **时序门禁测试**：新增 `host-tests/tests/ipc_strategy_registration_test.rs`（4 用例）：注册行号 < `Arch>::interrupt_late_init()` 调用行号（排除注释干扰）/ 契约注释存在 / `current_ipc_strategy` 返回 Option 且不 panic / 13 处 FFI 含 ENOSYS 降级标记。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 全过 ✅ / host-tests 98 套件（含门禁 4 用例）全过 ✅ / QEMU x86_64 启动通过 ✅。

### DECISION-L: barrier 开发阻塞（2026-09-12 审核员裁决）

> **背景**：barrier（可恢复栏栈）后期将整体重构设计升级。裁决：**阻塞（挂起）barrier 相关开发任务，重构后处理**。

**裁决**：
1. **挂起清单**（标"待重构后处理"）：§6.1 barrier 5（fault_inject/reset·audit/bbr/layered/parallel）、§6.3 barrier 2（domain·apply_degradation / reset·bsr 编排）、阶段 0 BarrierDegradePolicy trait 定义。
2. **边界**：阻塞的是**开发/下沉/改造**——barrier 运行时功能（panic→int 0x82 恢复、undo_log、域降级）**保持可用，绝不禁用**（安全基线）；framework 保留机制（api/manager/recoverable/recovery/snapshot/undo_log/bhr）不动。
3. **验收影响**：实测 framework/barrier `kernel::services` 引用 = 0——阻塞对"反向依赖 0"验收零影响；TCB 目标不受 barrier 项影响（机制本就留 framework）。
4. **理由**：barrier 后期整体重构，现做下沉/改造大概率作废（避免重复劳动）；barrier 涉中断/panic 上下文 + 内存回滚（安全敏感），重构前动它风险高收益低。
5. **主线不受影响**：§7 反向依赖治理等继续推进。

**状态**: [X]（裁决完成；barrier 项挂起待重构）

### DECISION-M: sys_pm_dispatch 归属（framework→services→framework 循环消除，2026-09-12 审核员裁决）

> **背景**：`framework/driver/power.rs` 机制壳 + `services/driver/power.rs` 策略主体（T4-4），`sys_pm → services::sys_pm_dispatch → framework::pm_suspend` 构成循环。类型+方法迁回 framework 无分歧（DECISION-F/J）。

**裁决**：**采纳方案 B——`sys_pm_dispatch` 直接迁回 framework**，`sys_pm → sys_pm_dispatch → pm_suspend` 自闭环，0 反向依赖；services 侧改 re-export 壳或删。

**依据**：
1. **服务对象准则**：sys_pm_dispatch 仅被 framework sys_pm 调用（grep 全量确认）——机制直接调用 → 归机制。
2. **无独立策略业务（抽查证实）**：`get_stats`/`ondemand_check`/`register_notifier` 全部操作 `self.per_cpu_stats`/`governor`/`thresholds`/`per_cpu_freq_idx`/`notifiers`（PmSubsystem 内部字段）——governor/C-state 算法无法脱离机制状态独立存在。
3. **与 IpcStrategy 本质区别**：IpcStrategy 的 services 实现有真实业务算法（pipe/shm/msgq），故 trait 注入；sys_pm_dispatch 无独立业务，trait 注入=空转转发+新增复杂度（PmDispatch trait/OnceLock/注册点/时序门禁）。
4. **先例一致**：fd_alloc/madvise_mlock 迁回 + services re-export 壳。
5. **简约原则**（§12.3）。
6. **循环消除**：framework→services→framework 自闭环。

**演进预留**：未来 cpuidle governor 独立化（独立策略+可单独测试）时再下沉 services，trait 注入接口随时可加——现在归机制不堵死。

**执行**：15 类型 + `sys_pm_dispatch` 迁回 `framework/driver/power.rs`（PmSubsystem 及组成、CpuIdleState/Stats、FreqGovernor/Level/CpuFreqDriver、SuspendNotifier、MAX_* 常量），services 改 `pub use framework::driver::power::*` 壳；验证：双架构 0w0e + audit_services_boundary 0 + power 反向依赖清零。

**状态**: [X]（裁决完成；实施交委托人）

### DECISION-N: audit_services_boundary 代理豁免 + 验证链修正（2026-09-12 审核员裁决）

> **背景**：委托人单独运行 `audit_services_boundary.py` 暴露 2 个 HIGH 预存违规——`services/barrier/reset_config.rs:12`（re-export `framework::barrier::reset::config`）、`services/sync/types.rs:13`（re-export `framework::sync::types`），均为 DECISION-J 第五/六批反转引入，当时未跑边界审计（audit quick 不含 boundary 维）。

**裁决**：
1. **豁免合法代理（选项 A）**：两处加入 `PROXY_ALLOWANCE` 白名单（is_proxy_allowed 机制已存在，与现有 5 条同质）——`('src/kernel/services/barrier/reset_config.rs', 'framework::barrier::reset::config')`、`('src/kernel/services/sync/types.rs', 'framework::sync::types')`。**选项 B（framework 改 pub）不成立**：审计拦的是黑名单路径引用，非可见性。豁免条件与现有 5 条同质：**仅 re-export 代理**，不包含 services 直接 use framework 内部做业务。
2. **性质**：合法代理壳（DECISION-J 反转的预期形态），非违规；非"为迁移开绿灯"（DECISION-G 禁令针对未确认归属先改审计）。
3. **流程修正（验证盲区）**：每批反转/迁移验证链**强制补 `audit_services_boundary.py` 单独运行**（不依赖 audit quick，后者不含 boundary 维）。
4. **全量核查**：委托人对 DECISION-J 全部反转批次跑完整 boundary 审计，确认无其他漏豁免项，一并报回。

**状态**: [X]（裁决完成；豁免+核查交委托人）

### DECISION-O: 两预存问题处置（§12.5 登记，2026-09-12 审核员裁决）

> **背景**：委托人 §12.5 登记 2 预存问题：① `audit_volatile_access` 报 `pmm.rs:1212 bitmap_size.get()` 非 volatile（本批未触碰 mm）；② proc 残余 `cfs.rs` 常量 re-export 与 `oomd.rs MemoryPressure` 仍在反向依赖清单（proc 批残留）。

**裁决**：
1. **问题① pmm.rs:1212 = 审计误报（预存）**：`bitmap_size` 是 buddy 元数据字段（初始化后只读普通内存值，非 MMIO/共享 volatile 场景），`.get()` 普通读正确。处置：登记待 mm 专项核实 audit_volatile_access 规则后定性（误报→修脚本/豁免；真有共享语义→mm 批修）。不阻塞本批。
2. **问题② proc 残留 = 真实反向依赖欠账（本工程 DECISION-J proc 批未清干净）**：实证 `framework/proc/oomd.rs:29` 直接 `use services::mm::memory_pressure::{MemoryPressure, update_pressure}`（framework→services 反向）+ cfs.rs 双向 re-export 混乱。处置（**登记回 proc 批收尾，不顺手在 fs 批改**）：按 DECISION-J 统一判据——`MemoryPressure` 类型**归 framework**（被机制 oomd 使用 = 机制持有的类型），`update_pressure` 算法留 services（策略实现）；cfs 双向 re-export 收敛为单向（framework 壳删或理顺方向）。
3. **原则**：① 属无关预存（§12.5 记录待专项）；② 属本工程欠账（§9.3 范畴，proc 批补清，不跨批顺手改——外科手术原则）。

**状态**: [X]（裁决完成；proc 批收尾 + mm 专项登记交委托人）

### DECISION-N 执行记录：PROXY_ALLOWANCE 豁免 + 全量核查（2026-09-13）

- **豁免实施**：`scripts/audit_services_boundary.py` PROXY_ALLOWANCE 新增 2 条（与现有 5 条同质，仅 re-export 代理壳）：`('src/kernel/services/sync/types.rs', 'framework::sync::types')`、`('src/kernel/services/barrier/reset_config.rs', 'framework::barrier::reset')`。
  - **实现细节**：barrier 条目禁条字面量用 `'framework::barrier::reset'`（而非裁决文本中的 `framework::barrier::reset::config`）——`is_proxy_allowed` 按 `forbidden == allow_forbidden` 精确匹配 FORBIDDEN_FRAMEWORK_MODULES 条目，黑名单登记的是 `framework::barrier::reset`（L79），故豁免键须与之一致。已核实两文件均为纯 `pub use ... ::*` 壳（豁免前提成立）。
- **全量核查（裁决第 4 条）**：对 DECISION-J 全部反转批次跑完整 boundary 审计——**HIGH 0 / CRITICAL 0**，无其他漏豁免项。剩余 2 MEDIUM 为 services 内部 inter-module 依赖白名单（`proc→mm`、`timer→syscall`），非 framework 边界穿透、非本裁决范畴（预存）。
- **流程修正（裁决第 3 条）**：验证链补 `audit_services_boundary.py` 单独运行为每批强制项（audit quick 不含 boundary 维=盲区）。本批起执行。
- **验证**：boundary 审计 `>>> services 边界检查通过 <<<` ✅。
- **审查结论（DECISION-N 闭环，无遗留）**：豁免 2 条与黑名单精确匹配、两文件纯 re-export 壳前提成立、全量核查 HIGH 0、提交范围无越界、流程修正已登记。
- **2 MEDIUM 预存项登记（审查建议 1）**（services 内部 inter-module 依赖，非 F2 framework 边界穿透，后续架构演进时留意，不即时处理）：
  1. `services/proc/oomd.rs:23` — `proc→mm`：`use services::mm::memory_pressure::{MemoryPressure, update_pressure}`（oomd 压力监控消费 mm 压力信号）
  2. `services/timer/posix_timer.rs:50` — `timer→syscall`：`use services::syscall::posix_timer as syscall_ptimer`（posix_timer 实现由 timer 与 syscall 两层共享）

### DECISION-J 第十四批执行记录：driver/power 迁回（DECISION-M 方案 B 实施）

> 调研确认：`framework/driver/power.rs` 机制壳（PM_SUBSYSTEM static + pm_init/pm_idle/pm_suspend/sys_pm + 硬件操作），`services/driver/power.rs` 策略主体（15 类型 + sys_pm_dispatch，0 unsafe），`sys_pm → services::sys_pm_dispatch → framework::pm_suspend` 构成循环。按 DECISION-M 方案 B 迁回。

- **迁回 framework**：`framework/driver/power.rs` 由 re-export 壳改为完整实现（合并 services 全部内容：CpuIdleState/CpuIdleStats/CpuIdleDriver/FreqGovernor/FreqLevel/CpuFreqDriver/SuspendNotifier/PmSubsystem + MAX_* 常量 + sys_pm_dispatch），保留原机制部分（PM_SUBSYSTEM/pm_init/pm_idle/pm_suspend/read_timestamp/arch_halt/arch_suspend_to_ram/arch_shutdown/sys_pm FFI），`sys_pm → sys_pm_dispatch → pm_suspend` 自闭环。
- **services 改壳**：`services/driver/power.rs` 改 glob re-export `framework::driver::power::*`（framework/driver/power 为 `pub mod` + 顶层 `pub use power::*`，同 fd_alloc/madvise_mlock 模式）。
- **消费点**：framework/proc/sched_ops.rs:125 `pm_init`、framework/syscall/dispatch.rs:384 `sys_pm`（路径不变）；services 内无其他消费方（grep 确认）。
- **引用计数**：framework 文件级反向依赖 41→40、行数 87→85；driver/power.rs 反向依赖清零。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit quick 全 0（pedantic 三维）✅ / host-tests 全通过 ✅ / QEMU x86_64 完整启动到 Ring 3（VFS ready，串口 242 行）✅。
- **边界审计遗留（预存）**：`audit_services_boundary.py` 单独运行发现 2 个 HIGH 预存违规（`services/barrier/reset_config.rs` re-export `framework::barrier::reset::config`、`services/sync/types.rs` re-export `framework::sync::types`，均为 DECISION-J 第五/六批反转引入），与本批无关；audit quick 不含 boundary 维故未暴露。处置待用户裁决（§12.5）。

### DECISION-J 第十五批执行记录：net 协议 wire 类型迁回（socket_types）

> 调研确认：`framework/net/syscall.rs`（TCB raw 桥接）引用 `services::net::socket::{Domain, SockType, SockAddrIn}` + `services::net::unix::SockAddrUn` 共 4 处。这些是**用户态 ABI 协议 wire 类型**（AF_INET=2 / SOCK_STREAM=1 / sockaddr_in / sockaddr_un 字节布局），由 framework `raw_read_sockaddr_in`/`raw_read_sockaddr_un`/`raw_write_sockaddr_un` 从用户内存 copy-in/copy-out 直接构造并返回——属"机制的安全导出面"（DECISION-F），文档 L325 已裁决"协议 wire 类型，机制属性强，建议迁回 framework（DECISION-J 模式）"。

- **迁回 framework**：新建 `framework/net/socket_types.rs`（0 unsafe）承载 `Domain`/`SockType`/`SockAddrIn`/`SockAddrUn`/`UNIX_PATH_MAX`（自 services/net/socket.rs + unix.rs 迁出，含 `SockAddrUn::new/path_slice` 方法）；framework/net/mod.rs 加 `pub mod socket_types`。
- **services 改 re-export**：`services/net/socket.rs` 类型定义段改 `pub use framework::net::socket_types::{Domain, SockAddrIn, SockType}`；`services/net/unix.rs` 的 `UNIX_PATH_MAX` 与 `SockAddrUn` 改 re-export（`PATH_MAX = UNIX_PATH_MAX` 别名不变，services/net/mod.rs 顶层 re-export 解析路径不变，API 兼容）。
- **消费点**：`framework/net/syscall.rs` 4 处改本地 `framework::net::socket_types::` 路径；`services/wasm/wasi/sock.rs` 经 services re-export 保持可用（无需改）。
- **host-test 同步**：`net_dual_stack_socket_test.rs` 的 `SOCKET_RS` 源路径 `services/net/socket.rs` → `framework/net/socket_types.rs`（断言 `Inet6 = 10` + from_i32 映射随定义迁移）。
- **引用计数**：framework 文件级反向依赖 40→39、行数 85→81；net/syscall.rs 清零。
- **sm_fi 剩余**：`uds_svc`（`services::net::unix`）委托留待 UDS/SocketStrategy 委托 trait 注入专项。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit quick 全 0（pedantic 三维）✅ / host-tests 全通过（双栈套件 6/6）✅ / QEMU x86_64 完整启动到 Ring 3 ✅ / 边界审计无新增违规（2 HIGH/2 MEDIUM 均为预存，与本批前一致）✅。

### DECISION-J 第十六批执行记录：ipc 专项收口 + 生产口径计数工具（audit_reverse_deps.py）

> ipc 实测现状：IpcStrategy trait 注入（DECISION-I）完成后，framework/ipc 生产代码反向依赖已归零——剩余 14 处全部为 cfg(test) 测试代码（mod.rs 内联 tests 12 处 + stress_tests.rs 2 处）。文档 §7.3/L256/L268 已裁决"测试载体访问 services 真实代码属合理，不纳入整治"，本批落地该口径。

- **口径修正**：裸 grep 计数（39 文件/81 行）混淆生产与测试上下文。新增 [audit_reverse_deps.py](../../scripts/audit_reverse_deps.py)（fail-closed）区分两类：
  - **生产反向依赖（验收对象）**：30 文件 / 53 行
  - **测试上下文引用（§7.3 合理，不纳入）**：9 文件 / 28 行（ipc/mod.rs tests 12 + ipc/stress_tests.rs 2 + framework/tests/ 7 文件 14）
  - 对账：30+9=39 文件、53+28=81 行，与裸 grep 精确一致 ✅
- **测试上下文判定规则**（fail-closed：判定不了的按生产违规计）：① `framework/tests/` 目录整体（feature 门控测试载体）；② `#[cfg(test)] mod X;` 引入的外部文件；③ `#[cfg(test)] mod X { ... }` 内联块（花括号深度跟踪）。抽查确认 ipc/mod.rs tests 正确归类 + user_proc.rs L329/L339、clone.rs L131 生产引用不误判。
- **ipc 专项结论**：生产反向依赖 0（DECISION-I trait 注入 + DECISION-J 类型反转完成），剩余测试项按 §7.3 合理保留。ipc 目录后续不再单列批次。
- **工具定位**：audit_reverse_deps.py 为本工程验收计数工具（非 CI 门槛——生产未归零前纳入 audit.sh 会直接打破 CI，待生产归零后再议纳入）。
- **剩余生产 30 文件分布**（下一批对象）：壳 re-export 类（credo 4/nestfs 18 行/devfs/ramfs/flock/inotify/cgroup/namespace/oomd/fd_table/rlimit/seccomp/session/types/mmap/mprotect/syscall types 等约 24 文件）+ 真实调用点（dispatch.rs 4、dispatch_trait.rs 1、clone.rs 1、sched_ops.rs 1、identity.rs 2、inotify.rs 1、sendfile.rs 1、user_proc.rs 2）。
- **验证**：脚本自身行为验证（归类单测 + 全量对账）✅；不触内核编译，无重跑验证链必要（audit quick 最近一轮全绿后无内核代码变更）。

### DECISION-J 第十七批执行记录：syscall 编号表迁回（types）

> 调研确认：`services/syscall/types.rs`（878 行，T5-4 于 2026-06-16 自 framework 迁入）是**用户态 ABI 机制**——syscall 编号表（编号空间分配 DECISION-037 承载物，用户态程序直接依赖）+ `ENOSYS_RET` 分发回退哨兵 + `SyscallResult` 别名。被 framework dispatch/dispatch_trait 消费（3 处生产引用）+ services dispatch 消费，依赖闭包为空（纯 const + type alias，仅 re-export `framework::errno::Errno`）——按 DECISION-J 统一判据迁回（同 fd_alloc 编号机制模式），文档 L252 "收敛为 framework 常量" 的彻底执行（整体迁回避免双份事实源）。

- **迁回 framework**：`framework/syscall/types.rs` 由 9 行壳改为完整定义（878 行，0 unsafe）；services 侧改 glob re-export 壳（framework/syscall 的 types 为 `pub mod`，同 fd_alloc 模式）。`Errno` 在 framework 版内 re-export 自 `framework::errno`（原行保留，API 兼容）。
- **消费点**：framework/dispatch.rs L96（`SYS_rt_sigreturn`）、L208（`ENOSYS_RET`）+ dispatch_trait.rs L50（`ENOSYS_RET`）改本地 framework 路径；services/dispatch.rs L82 与 host-tests（td23/errno_from_ret）经 services 壳引用保持可用（合法方向，无需改）。
- **host-test 注释同步**：errno_from_ret_test.rs 权威实现说明改指 `framework::errno::Errno`（原"services/types.rs::Errno"注释本已过时）；td23 的 `framework/syscall/types.rs::QX_SIGALTSTACK` 注释迁回后恰好变为正确，无需改。
- **边界审计**：新壳触发 1 HIGH（`framework::syscall::types` 在黑名单），按 DECISION-N 既定模式（纯 re-export 代理壳）加入 PROXY_ALLOWANCE；boundary 复核通过。
- **引用计数**：生产反向依赖 30→28 文件、53→49 行（audit_reverse_deps.py 口径）；syscall 目录生产引用仅剩 dispatch.rs（mremap_syscall + ExecveResult）+ sendfile.rs（svc_pipe）+ clone.rs（NamespaceSet::clone_from）+ mmap.rs/mprotect.rs 壳。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit quick 全 0 ✅ / boundary 通过 ✅ / host-tests 全通过 ✅ / QEMU x86_64 完整启动 ✅。

### DECISION-J 第十八批执行记录：syscall 目录收尾（mremap 迁 services + sendfile 走 IpcStrategy + 死壳删除）

> 调研确认 syscall 目录剩余 4 类生产引用：① mmap.rs/mprotect.rs 壳（T6-17 迁移遗留，全 kernel grep 确认 **0 消费者**——消费方早已直走 `services::mm::mmap`）；② dispatch.rs SYS_mremap 分支调 `services::mm::mremap::mremap_syscall`（策略主体在 services，framework 仅做参数校验转发——与 dispatch_mm 的 mmap/munmap/mprotect 系列完全平行）；③ sendfile.rs 3 处 `svc_pipe::pipe_{read,write}_safe`（IpcStrategy trait 已有 `pipe_read/pipe_write` 同语义方法）；④ QX_EXECVE 的 `ExecveResult::from_ret`（errno 白名单过滤策略，留待 execve 分发迁移专项）。

- **死壳删除**：`framework/syscall/mmap.rs` + `mprotect.rs` 删除（0 消费者），mod.rs 移除声明。host-tests 无源路径引用。
- **SYS_mremap 迁 services dispatch_mm**：framework dispatch 删分支（连带 try_flags helper——唯一消费者）；services/dispatch.rs dispatch_mm 加 SYS_mremap 分支（vma_get_current_mm + i32 严格校验语义不变，services→framework 合法方向）。分发顺序不变：framework 先问 services 分发器，回退才走 framework match。
- **sendfile 走 IpcStrategy**：sys_sendfile/sys_splice 3 处 pipe 读写改走 `current_ipc_strategy().pipe_{read,write}`（trait 方法签名 `Result<u32, i32>`，原 `.map_or(-1, ...)` 丢弃错误码语义不变）；策略未注册降级 ENOSYS（DECISION-K 降级契约）。framework→services 引用清零。
- **边界审计**：boundary 通过，无新增违规。
- **引用计数**：生产反向依赖 28→25 文件、49→45 行。syscall 目录生产引用仅剩 dispatch.rs（QX_EXECVE ExecveResult）1 处。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅（含 pedantic manual_let_else 修正）/ audit quick 全 0 ✅ / boundary 通过 ✅ / host-tests 全通过 ✅ / QEMU x86_64 完整启动 ✅。

### DECISION-J 第十九批执行记录：proc 系壳批 A（types/fd_table/oomd/rlimit/seccomp 反转）

> 调研确认 9 个 proc 壳全部有 framework 生产消费（scheduler tick → OOMD、dispatch → session、user_proc → ProcessState、process.rs → FdTable、clone.rs → NamespaceSet、顶层 re-export 链 → rlimit/seccomp/cgroup），按 DECISION-J 统一判据全部反转。分 2 批执行（9 文件共 3965 行）。

**本批（5 文件 1348 行）**：
- **纯壳反转 4 个**：`types.rs`（388 行，PID/TID/ProcessState/Priority/Context——进程机制核心类型）、`oomd.rs`（122 行，scheduler.rs:987 `OOMD.tick()` 直接驱动的机制组件，同 DECISION-M 判据）、`seccomp.rs`（407 行，SeccompState 挂 Process 机制状态 + seccomp_check 被 syscall 分发消费）— services 改 glob re-export 壳。
- **半壳合并 1 个**：`rlimit.rs`——framework 壳原本就含 sys_getrlimit/sys_setrlimit（unsafe 用户指针入口），策略主体（RlimitTable/Rlimit/常量/check_*）自 services 迁回合并；顺带删除 services 侧无消费者的 getrlimit_syscall/setrlimit_syscall Result 包装（实际消费走 `services::proc::sysinfo::getrlimit_syscall` 独立实现）。
- **fd_table.rs 新增反转**：`framework/proc/fd_table.rs`（137 行，FdTable 是 Process 机制字段）+ mod.rs 加 `pub mod fd_table` + process.rs L22 re-export 改引 framework 本地路径；services 改 glob 壳。
- **消费点消引**：user_proc.rs L329/L339 `ProcessState` 改 framework 本地路径。
- **host-test 同步**：`fd_table_extraction_test.rs` 整体改写为反转后口径（定义位置断言 services→framework、services 壳纯 re-export 断言、first-fit/close 行为断言读 framework 源）。
- **引用计数**：生产反向依赖 25→20 文件、45→39 行。proc 壳剩 cfs/cgroup/namespace/session（第二十批）。
- **验证**：双架构 0w0e ✅ / clippy 双架构 0 ✅ / audit quick 全 0 ✅ / boundary 通过 ✅ / host-tests 全通过（fd_table 套件 7/7 反转口径）✅ / QEMU x86_64 完整启动 ✅。

### DECISION-J 第二十批执行记录：proc 系壳批 B（cfs/cgroup/namespace/session 反转）— proc 壳清零

**本批（4 文件 2617 行）**：
- **cfs.rs**（585 行，自 services/proc/sched_policy.rs）：CfsRunQueue/DlRunQueue 是 scheduler 机制运行队列状态，权重/vruntime/抢占判定全部操作队列内部字段（同 DECISION-M sys_pm_dispatch 判据：算法本质是机制状态操作）— 反转。services/proc/sched_policy.rs 改 glob 壳。
- **cgroup.rs**（632 行）：cgroup 层级管理器是进程资源控制机制状态，被 framework proc 顶层导出消费 — 反转。
- **namespace.rs**（827 行）：NamespaceSet 是 Process 结构体字段，被 framework syscall/clone.rs `clone_from` 消费 + proc 顶层导出（sys_setns/sys_unshare）— 反转。clone.rs L131 改 framework 本地路径。
- **session.rs**（573 行）：会话/进程组是进程关系机制状态，sys_tcgetpgrp/sys_tcsetpgrp 被 framework syscall dispatch 直接消费 — 反转。
- **host-test 同步**：cfs_btreemap_bench_test.rs（I-34 BTreeMap 静态契约读 framework/proc/cfs.rs）+ framework_spinlock_migration_test.rs（P1-I-17 cgroup OnceLock 断言读 framework/proc/cgroup.rs）。
- **引用计数**：生产反向依赖 20→16 文件、39→35 行。**proc 目录壳清零**（仅剩 dispatch.rs QX_EXECVE 1 处真实调用，留 execve 分发迁移专项）。
- **验证**：双架构 0w0e ✅ / audit quick 全 0 ✅ / boundary 通过 ✅ / host-tests 全通过 ✅ / QEMU x86_64 完整启动 ✅。

### DECISION-J 第二十一批执行记录：fs 系壳批 A（flock/devfs/inotify 反转 + ramfs/nestfs 专项判定）

> fs 系 5 壳逐项闭包调研后分治：flock/devfs/inotify 闭包闭合 → 反转归位；ramfs 闭包不闭合、nestfs 体量大 → 各归专项批（与 §"fs 系列" 依赖闭包调研结论一致）。

**本批（3 文件 2187 行）**：
- **flock.rs**（728 行，自 services/fs/flock.rs）：flock/POSIX 锁表被 framework VFS 机制内联消费（vfs/path.rs inode 释放路径调用 `posix_lock_release_inode`），锁表属 VFS 机制状态 — 反转。依赖闭包仅 IrqSpinLock 别名（framework::sync 直引）+ core 原子，0 unsafe。services 改 glob 壳。
- **devfs.rs → framework/fs/devfs/mod.rs**（863 行）：DevFS 设备表被 framework VFS 挂载机制直接消费（vfs/mount.rs 引用 `DEVFS_DATA`/`DevfsData`）— 反转。闭包闭合关键：Inode trait 已于 B09-12 迁回 `framework::fs::vfs::inode`（services::fs::inode 仅 re-export）+ `services::sync::once::OnceCell` 实为 `framework::sync::OnceLock` 别名 — 三处 services 引用全部可 framework 本地化，0 unsafe。删除 `framework/fs/devfs/devfs.rs` 旧壳层，mount.rs/test_devfs.rs 改直路径；services 改 glob 壳。
- **inotify.rs**（596 行，自 services/fs/inotify.rs）：inotify 实例表/事件队列被 framework VFS 机制内联消费（vfs/path.rs 与 vfs/handle.rs 文件操作路径直接调用 `inotify_notify`）— 反转，`sys_inotify_read`（用户缓冲区 unsafe 写入，SAFETY 逐块标注）一并归位。闭包闭合：Errno（framework::errno）、fd_alloc（framework::proc::fd_alloc，第十九批已迁回）均 framework 项。services 改 glob 壳。
- **ramfs 专项判定**：ramfs_core 闭包**不闭合**——`impl FileSystem for RamFsData` 构造 services 具象 `RamFsInode`（3 处）+ 依赖 `services::fs::dcache`（inode 缓存）+ `services::fs::vfs_types`，迁移将连带拖入 inode 具象层与 dcache 子系统 → 按 §"fs 系列"调研结论归专项批（backend_trait 注入方向，同 DECISION-K 项 5）。
- **nestfs 专项判定**：29 文件 ZFS 风格大型实现 + framework 仅消费 `nestfs::nestfs::{get_nestfs, nestfs_hotplug_register}` 挂载集成点 → 单列专项批（本批不动，18 行壳保留）。
- **host-test 同步**：fd_allocator_unified_test.rs（INOTIFY_FD_BASE/fd_at 断言改读 framework/fs/vfs/inotify.rs）+ fs_sync_trait_test.rs + test_runner_init_test.rs + plan_b_inode_test.rs（devfs 源码断言改读 framework/fs/devfs/mod.rs）。
- **引用计数**：生产反向依赖 16→13 文件、35→31 行（ramfs 1 行 + nestfs 18 行留专项）。
- **验证**：双架构 build.sh all Passed 5/0（含 host-tests）✅ / clippy 双架构 0 warning（pedantic lib + kernel_test + host-test 三维）✅ / audit quick 全过 ✅ / boundary + coupling + safety_coverage 100% + static_mut + repr_c + feature_semantics 全过 ✅ / host-tests 98 套件 749 通过 0 失败（debug+release 双档）✅ / QEMU x86_64 完整启动至 Ring 3（VFS ready）✅。预存问题登记：audit_volatile_access 报 pmm.rs:1212 `bitmap_size.get()` 非 volatile 访问（本批未触碰 mm 子树，待专项处置）。

### DECISION-O ② 执行记录：第二十二批 proc 批收尾（MemoryPressure 类型归位 + cfs 收敛单向）

> 执行 DECISION-O ② 裁决（proc 批残留欠账，非跨批顺手改）：① oomd.rs 反向引用 services::mm::memory_pressure；② framework/proc/cfs.rs 反向别名 services::config 的 CFS_* 常量。两项均为 proc 系壳批（第十九/二十批）反转后暴露的残留欠账。

**本批（MemoryPressure 反转 + classifier 注册注入）**：
- **framework/mm/pressure.rs 新建（权威归位）**：MemoryPressure 类型 + CURRENT/PREV_PRESSURE 状态 + current_pressure/previous_pressure 读取原语 + `update_pressure` 包装归 framework（机制持有：OOMD 是 scheduler tick 直接驱动的机制组件，压力状态是机制状态，同 DECISION-M 判据）。分级算法留 services：framework 留 `register_pressure_classifier` 注册口（`OnceLock<fn(u64,u64)->MemoryPressure>`，同 alloc_trait 注册 idiom），未注册 fallback Normal（早期启动窗口，同 FallbackAllocPolicy 风格）。
- **services/mm/memory_pressure.rs 改造**：删除类型/状态/update_pressure 定义，保留阈值（FREE_PAGES_THRESHOLD_*）+ `set_thresholds` + 纯函数 `classify_pressure`（4 级双阈值状态机）+ PressureAwareAllocPolicy；类型/读取/包装经 re-export 保持 API 兼容（services→framework 合法方向）。`services::mm::init` 增注册 `register_pressure_classifier`。
- **framework/proc/oomd.rs**：引用源改 `framework::mm::pressure::{MemoryPressure, update_pressure}`，framework→services 引用清零。
- **CFS 收敛单向**：`CFS_*` 7 常量权威自 services/config/sched.rs 迁回 framework/config/sched.rs（T6-9 迁出前提"仅被 services 消费"经 DECISION-J cfs 机制反转后失效——framework/proc/cfs.rs:338 + scheduler.rs + process.rs 均消费，按 DECISION-J 判据归机制常量）。framework/proc/cfs.rs 别名源改 framework::config；services/config/sched.rs 改纯 re-export；framework/config/mod.rs 顶层 re-export 扩 CFS_*。
- **host-test 同步**：memory_pressure_extraction_test.rs 契约反转改写（P1-I-01 D9 验收项 → DECISION-O ② 契约：机制在 framework/分类器注册在 services/oomd 零 services 引用，4 级状态机与双阈值契约保持）；framework/tests/test_config.rs 过时注释同步。
- **引用计数**：生产反向依赖 13→11 文件、31→29 行（oomd 1 行 + cfs 1 行清零；剩余为 nestfs 18 行/ramfs/sm_fi/ebpf verifier/execve 分发等已登记专项项）。
- **验证**：双架构 build.sh all Passed 5/0（含 host-tests）✅ / clippy 双架构 0 warning ✅ / 核心审计 8/8（boundary/safety_coverage/deadlock_matrix/coupling/comment_language/once_cell/c_naming/invariants）+ audit_reverse_deps ✅ / host-tests 全通过 ✅ / QEMU x86_64 完整启动至 Ring 3 ✅。预存 flaky 登记（§12.5，与本批无关）：host-tests/fsx_integration_test 的 test_fsx_stress 在系统高负载（并行跑多任务）下偶发 30 errors 阈值 panic，独占串行跑稳定通过（errors: 0）——host 侧 std 压力模拟器时序敏感，不触及本批内核改动，待单开处置。**交集确认依据（审核要求补充）**：fsx 依赖面为 `std::{collections, fs, path, sync::atomic}` 自包含模拟器（host-tests/src/fsx.rs，进程内 FsxFs 模拟 + 临时目录，零 `src/kernel` 源码引用、不编译内核）；对照本批 12 文件改动清单（framework mm/config/proc + services mm/config + plan + host-test 契约）——两清单无重叠，零交集坐实（对第二十一批 fs 系改动同理成立）。**处置方向（审核确认，待单开）**：定性 = flaky test（时序敏感，高负载下模拟时序偏移致 errors 超阈值），非内核 bug；单开方案选项：① 阈值容差放宽（30 errors 阈值 vs 负载灵敏度）② CI 标注 serial + 重试机制 ③ 模拟器时序隔离（固定 tick）；优先级 = 建议优先单开（flaky 污染 §2.3 host-tests 门槛，影响后续批次验收稳定性）。


### mm 专项 ① 执行记录：pmm.rs:1212 volatile 误报根因核实 + audit_volatile_access 规则修正（DECISION-O ①）

> 执行 DECISION-O ① 裁决登记的"待 mm 专项核实 audit_volatile_access 规则"。

**误报根因（复现坐实）**：`check_direct_access_violations` 的 fn 上下文豁免判定用 `re.search` 取 500 字符回溯窗口内**最左** `fn`——当包裹函数较短时，上一个函数头也会落入窗口（`fn test_bit` 距访问点 485 字符 < 500），遮蔽真正的包裹函数 `count_free_pages`（距 273 字符），豁免失配 → 1212 行误报。漂移触发源：`#[expect(clippy::cast_possible_truncation)]` 属性行加入后把 test_bit 头推过窗口边界。

**误报定性核实成立**：`bitmap_size: Cell<usize>` 唯一 `.get()` 点在 `count_free_pages`（init-only 非热路径），唯一 `.set()` 点在 `init_bitmap`；bitmap 位操作本体已走 MetaStore 裸指针路径（pmm.rs L1184-1186：该 LTO 错位面整体消除）+ `#[repr(C)]` 布局防御——DECISION-O ① "buddy 元数据普通内存读、非 volatile 场景" 定性成立，代码无需改动。

**规则修正**：豁免判定改取窗口内**最近**（最后一个）`fn`（`re.findall(...)[-1]`），精确化包裹函数识别；fail-closed 路径不变，并做负向验证（假豁免名下 1212 仍报 violation）。修复后 4/4 高风险字段全过，exit 0。

**观察项登记（不擅自实施）**：脚本自注释"Atomic 形态天然免疫 LTO 字段错位，优于 UnsafeCell+get()"——`bitmap_size` 可迁 `AtomicUsize`（load/store）消除 direct 模式对文本启发式豁免的依赖，属代码改动超出本专项"规则核实"范围，留委托人裁决单开。

**验证**：audit_volatile_access 4/4 通过 + fail-closed 负向验证 ✅（其余验证门槛不受影响——本批仅改审计脚本，无内核代码改动）。

### DECISION-K 项 5 执行记录：第二十三批 ramfs 专项批（ramfs_core 回迁 + FsBackend 工厂钩子注入）

> 执行第二十一批登记的 ramfs 专项判定（闭包不闭合 → backend_trait 注入方向）。施工前全量源码调研修正了闭包判定前提：ramfs_core 三文件对 services 的真实依赖仅剩 `RamFsInode` 构造点 ×3（fs_open/fs_create/fs_resolve_inode）——`services::fs::dcache` / `services::sync::irq_lock` / `services::fs::vfs_types` 经逐文件核实均为 framework 纯别名/壳；framework 侧 mount.rs 对 RamFsData 有机制级直接消费（含 'static 引用提升），坐实机制归属。

**本批（ramfs 2433 行回迁 + 钩子注入）**：
- **framework/fs/ramfs/ 扁平化回迁**（mod.rs + ramfs_data.rs + ramfs_node.rs，对齐 devfs 单层先例）：RamFsData/RamFsNode/RamFsDirEntry/RAMFS_DATA/init + `impl FileSystem for RamFsData` 整体归 framework（0 unsafe）。删除 framework/fs/ramfs/ramfs.rs 壳层（`ramfs::ramfs::X` 双层路径随之扁平化，mount.rs/handle.rs/inode.rs 共 9 处引用路径同步）。
- **FsBackend trait 扩展（DECISION-K 项 5 落地）**：新增 `make_ramfs_inode(inode_id, mount_idx) -> Result<Arc<dyn Inode>, KernelError>` 工厂钩子；`FallbackFsBackend` 实现 fail-closed（未注册返回 `NotInitialized`，早期启动窗口安全）；3 个 RamFsInode 构造点改经 `current_fs_backend().make_ramfs_inode`（fs_open/fs_create 先 drop 锁再调钩子，锁序不变）。具象 `RamFsInode` 留 services（TCB 最小化：约 130 行 Inode 适配器不进 framework）。
- **services 侧收尾**：`ServicesFsBackend` 实现钩子委托 `new_ramfs_inode`（该工厂由此获得首个生产使用路径）；删除 services/fs/ramfs_core 三文件 + 模块声明；消费者（tmpfs/overlayfs/anonymous/ramfs 薄包装）全部改 framework 直引（services→framework 合法方向）；ramfs_data.rs 内 dcache/vfs_types 别名引用还原为 framework 本地路径。
- **host-test 同步**：fs_sync_trait_test.rs（ramfs_inherits_default 源码断言改读 framework/fs/ramfs/mod.rs）+ plan_b_inode_test.rs（ramfs_implements_fs_resolve_inode 同步）。
- **引用计数**：生产反向依赖 **11→10 文件、29→28 行**（ramfs 壳清零；剩余 nestfs 18 行/sm_fi/ebpf verifier/execve 分发等已登记专项项）。
- **验证**：双架构 build.sh all Passed 5/0（含 host-tests）✅ / clippy pedantic 三维（lib + kernel_test + host-test）0 warning ✅ / audit quick 全过 ✅ / 独立审计 boundary + coupling + deadlock_matrix + safety_coverage + volatile_access + repr_c + static_mut + audit_reverse_deps 全过 ✅ / host-tests 98 套件全过（含 fsx，串行 0 失败）✅ / QEMU x86_64 完整启动至 Ring 3（VFS ready，钩子注册时序实测无恙）✅。

### DECISION-K 项 6 执行记录：第二十四批 nestfs 专项批（注入归零方案 B + services::fs::init 注册契约）

> 落实第二十一批 nestfs 专项判定（29 文件 ZFS 风格实现 + framework 仅消费挂载集成点）。方案对比后委托人选定**方案 B：注入归零**——nestfs 业务实现整体留 services（不拖 29 文件进 framework，TCB 最小化），framework 消费点改经注册表/trait 分发。同批修复回归：`services::fs::init()` 此前未接入内核初始化序列，`ServicesFsBackend` 注册（含 ramfs 钩子）在生产路径未生效。

**本批（注入归零 + 注册契约 + 回归修复）**：
- **backend_trait 注册表**：新增 `NESTFS_FS: OnceLock<&'static dyn FileSystem>` + `register_nestfs_fs()`（幂等）/ `nestfs_fs() -> Option<&'static dyn FileSystem>`；未注册语义 Option 可空（对齐 DECISION-K 项 2 降级先例，mount/format 路径未注册返回 `NotInitialized`，fail-closed）。
- **FileSystem trait 扩展**：新增 `fs_format()` 默认方法（默认 `NotSupported`）；services `nestfs_inode.rs` 实装格式化逻辑（驱动发现 + format_drive，代码自 mount.rs 原位搬移）。
- **mount.rs 消费点反转**：NestFS 挂载分支 `get_nestfs()` → `nestfs_fs()`（未注册 fail-closed）；format 路径改 `fs.fs_format()` trait 分发——framework 对 `services::fs::nestfs` 直接依赖清零。
- **driver 直调反转**：`framework/driver/mod.rs` 删除 `nestfs_hotplug_register()` 直调，hotplug 监听随注册契约统一由 services 侧发起。
- **删壳**：`framework/fs/nestfs/mod.rs` 18 行 re-export 壳删除，缩为 10 行模块仅保留 `arc_safe`（unsafe 封装机制留 framework，业务全在 services）；framework 测试（test_nestfs/test_nestfs_ext）改经 `services::fs::nestfs` 路径（§7.3 允许）。
- **注册契约（回归修复核心）**：新增 `services::fs::init()`（幂等）——注册 `ServicesFsBackend`/VFS poll 策略/`register_nestfs_fs(get_nestfs())`/`nestfs_hotplug_register()` 四项；`lib.rs` VFS init 之前插入调用（`// FsBackend/NestFS 注册契约点`）。依据：与 IpcStrategy/ConfigValidateHook 同一"机制 init 后立即注册策略"启动契约；注册零依赖（OnceLock 存指针），策略方法惰性调用。
- **回归测试**：test_vfs.rs 新增 3 用例——`fs_backend_registered_make_inode`（锁 make_ramfs_inode 命中 services 钩子，Fallback 即回归）/ `ramfs_fs_open_via_backend_hook`（fs_open 全链路经钩子返回 RamFsInode）/ `nestfs_fs_registered`（注册表 + name 断言；fs_format 有底层 IO 副作用不入单测，由 QEMU boot 覆盖挂载分发）。
- **host-test 修复**：`ramfs_fs_open_via_backend_hook` 首跑 FAIL（create_file 返回 None）——根因 `RAMFS_DATA` 初始为空、根目录未建，测试内补 `framework::fs::ramfs::init()`（幂等 mount("/")）修复；e04 共享测试集恢复 0 failed。
- **引用计数**：生产反向依赖 **10→9 文件、28→10 行**（nestfs 18 行壳清零，为单文件最大降幅项；剩余 credo sha256/types 壳、hdmi 壳、sm_fi、ebpf verifier、execve 分发等已登记专项项）。
- **验证**：双架构 build.sh all Passed 5/0 ✅ / clippy -D warnings 双架构 0 ✅ / 核心审计 11 项全过（boundary + safety_coverage + deadlock_matrix + coupling + comment_language + once_cell + c_naming + invariants + repr_c + volatile_access + static_mut）✅ / audit_reverse_deps 9 文件/10 行与登记一致 ✅ / host-tests 98 套件全过（e04 共享测试集 336 passed/7 skipped/0 failed）✅ / QEMU x86_64 完整启动至 Ring 3（VFS ready，注册时序实测无恙）✅。

### DECISION-K 项 5 执行记录：第二十五批 credo 三壳反转 + ebpf verifier 注册契约 + ExecveResult 迁回

> 落实第二十四批后剩余专项项的 credo 判据（capability/sha256/types 三模块依赖闭包闭合：纯常量/纯算法/纯数据定义，framework 内部 `secure_boot`/`process.rs` 直接消费）——反转归位 framework，services 改 re-export 壳。同批完成 ebpf verifier 注册契约与 ExecveResult 类型迁回。**生产反向依赖 9→2 文件、10→2 行**（仅剩 hdmi 壳 + sm_fi UDS 两项，均属 HDMI 平行实现统一专项批，方案 A services 收敛已获授权）。

**本批（credo 反转 + 注册契约 + 类型迁回 + 死锁根因修复）**：
- **capability 反转**：`framework/credo/capability.rs` 为权威（16 域能力位 + `VIABLE_FLOOR` viable 底线 + PWM/FS/PROC 等域常量，纯 const 闭包为空）；`services/credo/capability.rs` 改 re-export 壳（services→framework 公共路径合法方向，boundary 审计实测无需 PROXY_ALLOWANCE 登记）。
- **sha256 反转**：`framework/credo/sha256.rs` 为权威（SHA-256 纯算法，framework/credo/secure_boot 直接消费）；services 壳 re-export；PWM_DIGEST_LEN 与相关测试注册随迁。
- **types 反转**：`framework/credo/types.rs` 为权威（PWM 类型/能力矩阵/身份条目/审计类型，632 行纯数据定义；framework/proc/process.rs 进程表机制持有 PwmContext）；services 壳 glob re-export。`framework/credo/mod.rs` 顶层 capability re-export 改本地路径（删除 `crate::kernel::services::credo::capability` 反向引用）；identity.rs 两处同改。
- **ebpf verifier 注册契约**（预存欠账修复）：`services::debug::ebpf::init()` 扩展为 `bpf_init()` + `set_verifier(&STANDARD_VERIFIER)`（DECISION-K 统一模式：机制 init 后立即注册策略）；`framework/proc/sched_ops.rs` scheduler_init 删除对 services verifier 的反向直调；`lib.rs` kernel_init 在 scheduler ready 后统一接入。未注册窗口 prog_load fail-closed（verifier() 返回 None 拒绝），启动早期无用户态进程，窗口安全。
- **ExecveResult 迁回**：新增 `framework/syscall/execve.rs`（`ExecveResult` 枚举 + `from_ret`/`as_ret`，机制持有类型判据）；删除 `services/proc/execve.rs`（66 行）；`framework/syscall/dispatch.rs` QX_EXECVE 分支改 framework 本地路径。
- **host-test 同步**：`b07_creds_audit_test.rs` 源码断言 `CAP` 路径改读 framework 权威定义（`framework/credo/capability.rs`）。
- **test_vfs clippy 修复引入 e04 死锁——根因定位与修复（工具链行为陷阱，重要教训）**：
  - 现象：e04 共享测试集在 `vfs::backend::ramfs_fs_open_via_backend_hook` 处 100% CPU 自旋（全量与单独运行均复现；HEAD 基线通过，锁定为本批回归）。
  - 定位：gdb attach + 反汇编——自旋点为 `fs_open`（ramfs/mod.rs:90）内部 `RAMFS_DATA.lock()` 的 `lock cmpxchg` 获取循环，锁字恒为 1 而进程内仅自旋线程自身——同线程递归自锁；测试自身三处锁均有释放路径，仅 line 206 acquire 缺正常释放。
  - 根因：clippy `deref_addrof` 修复将 `&*(&*RAMFS_DATA.lock() as *const RamFsData)`（cast 断开延长链，守卫语句末释放）改写为 `let ramfs_ptr = &raw const *RAMFS_DATA.lock();`——rustc 1.98 nightly（RFC 3606 临时生命周期延长）下守卫存活至**绑定作用域结束**（微测试实证：无 cast 的 `&raw const *expr` 与 `&*expr` let 绑定均延长，cast 形式不延长），fs_open 内部重入 lock() 即同线程自锁。
  - 修复：守卫收窄至块作用域强制提前释放（`let ramfs_ptr = { let guard = RAMFS_DATA.lock(); &raw const *guard };`），SAFETY 注释补充 RFC 3606 陷阱说明；全库排查确认无其他同款模式（mount.rs:97 同款手法为 cast 形式，安全——项目此前"暂不迁移 &raw const"的保守决策恰好规避此坑）。
- **引用计数**：生产反向依赖 **9→2 文件、10→2 行**（credo 三壳 + ebpf verifier + execve 分发五项清零；剩余 hdmi 壳、sm_fi UDS 委托均属 HDMI 专项批）。
- **验证**：双架构 build.sh all Passed ✅ / clippy pedantic -D warnings 四维（x86_64+aarch64 × kernel_test+host-test）0 ✅ / 核心审计 12 项全过（boundary + safety_coverage + deadlock_matrix + coupling + comment_language + once_cell + c_naming + invariants + repr_c + volatile_access + static_mut + reverse_deps 2 文件/2 行与登记一致）✅ / host-tests 98 套件全过（e04 共享测试集 336 passed/7 skipped/0 failed，修复后单独+全量双跑验证）✅ / QEMU x86_64 完整启动至 Ring 3 ✅。

### DECISION-K 项 5 执行记录：第二十六批 sm_fi UDS 委托反转（SO_PASSCRED 钩子注册契约）

> 落实 §7 net 剩余的 sm_fi 项：`framework/net/init/sm_fi.rs` 对 `services::net::unix` 的最后一处生产反向依赖（`sm_setsockopt` 的 `SO_PASSCRED` 路由直调 `uds_svc::uds_setsockopt`，1 import + 1 调用点）。**生产反向依赖 2→1 文件、2→1 行**（仅剩 HDMI 壳，归 HDMI 专项批）。

**本批（钩子注册契约）**：
- **framework 侧**：sm_fi.rs 新增 `UDS_SETOPT_HOOK: OnceLock<fn(i32, bool) -> i32>` + `register_uds_setsockopt_hook()`（DECISION-K 统一模式，同 `register_nestfs_fs`/`register_pressure_classifier` idiom）；`sm_setsockopt` 的 `SO_PASSCRED` 路由改经钩子委托，未注册时 fail-closed 返回 `E_NOPROTOOPT`（-92，命名常量，与文件内 errno 常量族一致）——未注册窗口仅存在于启动早期（Ring 3 前），无用户态进程可触达，窗口安全。
- **services 侧**：`services::net::unix::uds_init()` 注册钩子（`uds_setsockopt` fn pointer，重复注册 Err 忽略幂等）；注册调用以 `#[cfg(not(feature = "kernel_test"))]` 门控——与 `framework::net::init` 模块门控对齐（kernel_test 下 init FFI 层整体不存在，sm_setsockopt 消费点同样不存在，两侧一致）。
- **接线无新增**：`lib.rs` 步骤 9-1 已有 `uds_init()` 调用（UDS subsystem initialized），注册契约无需改动启动序列。
- **预存问题修复（本批阻塞项）**：`services/fs/nestfs/dedup.rs` `CasIndex::ref_dec` 清零分支先持 `ref_counts` 再取 `hash_to_dva`，与 `insert` 的持锁顺序（`hash_to_dva → ref_counts`）相反，并发交错即 ABBA 死锁——host-tests `nestfs_stress_test` 双测试线程互等挂起复现，阻塞本批 host-tests 门槛。修复：锁序统一为 `hash_to_dva → ref_counts`（与 insert/invalidate 一致）；新增回归测试 `stress_cas_concurrent_insert_refdec_no_abba`（双线程各 200 轮高频交错两条持锁路径，独立 hash 保证断言确定性）。
- **引用计数**：生产反向依赖 **2→1 文件、2→1 行**（sm_fi 项清零；剩余 hdmi 壳 1 行，属 HDMI 平行实现统一专项批，方案 A services 收敛已授权）。
- **验证**：双架构 build.sh all Passed ✅ / quick 审计链全过（clippy pedantic lib + kernel_test/host-test 两 feature 维 0 warning + 6 不变式 + TCB 边界）✅ / audit_reverse_deps 1 文件/1 行与登记一致 ✅ / QEMU x86_64 完整启动至 Ring 3 ✅ / host-tests 98 套件全过（含 nestfs_stress_test 专项单套 10 轮压测稳定）✅。

### DECISION-K 项 5 执行记录：第二十七批 HDMI 专项批（孤儿目录删除，反向依赖归零）

> 落实 §6.4 备注行预授权 + DECISION-G 项 4 待核闭环，方案 A（services 收敛）经用户授权。**生产反向依赖 1→0 文件/行——DECISION-J→K 反向依赖整治全序列（79 文件/137 行起点）归零达成。**

**调研实证（删除依据）**：
- **整目录未挂载**：`framework/driver/display/mod.rs` 无 `pub mod hdmi;` 声明——hdmi/mod.rs（re-export 壳）与 7 子文件（edid/vendor/port/pixel_clock/safety_audit/sync_tmds/timing，合计 1537 行）均不参与编译，为 HDMI 实现迁移 services 时遗留的死文件。
- **消费面零依赖**：全库 `hdmi::` 引用（`services/driver/display/mod.rs` 声明、dp.rs 复用 VideoMode/lookup_dmt_timing、host-tests driver_display_test 消费 STANDARD_VIDEO_MODES）全部指向 services 权威实现；framework 路径零外部消费者（仅待删文件内部 doc 自引用）。
- **平行实现差异盘点（删除损失评估）**：孤儿文件中 HdmiPort/MultiHdmiPorts trait、Intel/Amd/Synopsys vendor DPLL trait 骨架、`new_with_iomem_pixel_clock` 为 framework 侧独有，但均未接线、零消费者、休眠状态——按方案 A 不迁并，随目录删除；后续 vendor 实装时按 DECISION-K 注册契约模式在 services 侧重立。

**本批施工**：删除 `framework/driver/display/hdmi/` 整目录（8 文件：1 re-export 壳 + 7 未挂载孤儿，-1537 行）。

- **验证**：audit_reverse_deps **0 文件/0 行**（测试上下文 15 文件/52 行按 §7.3 豁免不计数）✅ / 双架构 build.sh all ✅ / quick 审计链 ✅ / host-tests 98 套件 ✅ / QEMU x86_64 完整启动至 Ring 3 ✅（孤儿不参与编译，编译产物零变化，全链为门槛形式性复核）。
- **审计盲区扩展（用户裁决通过）**：`audit_deadlock_matrix.py` 扫描范围由仅 framework 扩展为 **framework + services 双子树**（368→718 文件）——扩展背景：第二十六批 nestfs ABBA 死锁位于 services 子树，原单根扫描不可见（fail-closed：不可检查 = 漏检）。同步增强：`services::sync::irq_lock::IrqSpinLock`（framework IrqSpinLock 的 services 层类型别名）纳入安全锁识别，覆盖全路径字段声明与 `as Mutex` 别名导入两种形态。扩展后**零新增发现**（唯一 HIGH 为 framework smp_init.rs `AP_STARTUP_LOCK` 预存人工审查项，扩展前已存在）。注：脚本 AB-BA 环检测仍为其文档声明的未实现项（需 lockdep-style 锁序声明机制），本次扩展不改变该边界。

### DECISION-L 终局验证：栏栈不下沉（2026-09-12 审核员，基于 barrier-stack-design.md）

> 审核员最终解释：**栏栈不整体下沉**——它已是"framework 机制 + services 策略"的正确分层样板，重构后依然如此。DECISION-L（阻塞 barrier 开发）得到设计文档三重证据验证，继续执行。

**证据（design 文档依据）**：
1. **现状已分层正确**（L42）：机制层 `framework/barrier/`（RecoveryDomain/UndoLog/屏障快照/BBR/BSR/BHR），策略层 `services/barrier/`（RecoveryPolicy/故障归属/健康监测/级联）——正是 F/S 形态样板，非下沉对象。
2. **机制必须留 framework**（服务对象准则）：重构后核心机制（受控锁 `DomainState<T>`、胶囊原语、BCB 全局控制块、panic→int 0x82 恢复入口、Isolated 架构抽象）服务"系统故障恢复"TCB 级职责，结构性/编译期强制，下沉即违规。
3. **TCB 边界白纸黑字**（L140）：framework 不因栏栈重建扩大 unsafe 面；services 0 unsafe、F1-F9 不变——重构后仍 framework/services 分层。

**§6.1/§6.3 的 7 个 barrier 项处置**：fault_inject/audit/bbr/layered/parallel + domain·apply_degradation/bsr·编排 本质是策略/编排，重构后在 services 落实是正确方向；**但现在不做**（重构用 DomainState/胶囊替代 RecoverableMutex/UndoLog，旧 trait 注入接口随机制更换失效）——保持"待重构后处理"标注，从本工程当前批次移除，不占验收。

**行动指令**：barrier 任务维持阻塞（不做下沉/改造/新开发）；运行时功能保持可用（panic→int 0x82/undo_log/域降级，安全基线，只禁开发不禁用）；主线（§7 反向依赖治理等）不受阻；重构时按 L0-L4 分层 + BCB 五区 + 服务对象准则自然落地。

**执行确认**（委托人）：DECISION-J 第八批反转的 `barrier/reset/config.rs`（RecoveryLayer/set_reset_in_progress 机制配置）方向符合"机制留 framework"，与终局结论一致，不回滚；services barrier 策略（RecoveryPolicy 等）未动，符合"策略在 services"。

**状态**: [X]（终局验证；barrier 项维持阻塞，从当前批次移除）

### DECISION-J 第二批执行记录：config 常量反转（memory + capacity）

> 依据 DECISION-J 统一判据（机制持有的常量归 framework）：`framework::config::{PAGE_SIZE, MAX_CPUS}` 等被 framework arch/mm/smp/cpu_local/rcu/irq 机制直接消费，属机制常量，迁回；services 侧改 re-export 保 API 兼容。本次为第二批发货（第一批 = ipc 类型，3519410e）。

- **迁回 framework**：`framework/config/memory.rs` 由 re-export 壳改为真实 20 项常量定义（PAGE_SIZE/PAGE_SHIFT/HUGE_PAGE_{2M,1G}_{SIZE,SHIFT}/USER_STACK_{SIZE,GUARD,TOP,MAX_SIZE}/USER_KSTACK_SIZE/USER_CODE_BASE/ASLR_{STACK,MMAP,HEAP,PIE}_BITS/USER_{MMAP,HEAP,PIE}_BASE/KERNEL_STACK_SIZE）+ 保留既有 ASLR 运行时函数与 kernel_test；`framework/config/capacity.rs` 由 re-export 壳改为真实 8 项常量定义（MAX_CPUS/MAX_IRQS/MAX_PROCESSES/MAX_THREADS/MAX_THREADS_PER_PROCESS/MAX_OPEN_FILES/MAX_SESSIONS）。均 0 unsafe，依赖闭包为空（纯常量）。
- **services 改 re-export**：`services/config/{memory,capacity}.rs` 改为从 `framework::config` **顶层**显式 re-export（`pub use crate::kernel::framework::config::{PAGE_SIZE, ...}`）。**关键约束**：framework/config 的 memory/capacity 子模块为私有（`mod capacity;`），services 无法路径访问 → 必须经 framework/config/mod.rs 既有顶层 re-export（L75-L85）转发；与 ipc 的 `pub mod types` 可直接 glob re-export 不同——两种 re-export 模式差异已确立。
- **策略逻辑未随迁**：ASLR 运行时函数本就保留在 framework/config/memory.rs；services 侧无策略逻辑需迁。
- **引用计数**：framework 文件级反向依赖 79→76、精确行数 137→135（memory/capacity 两壳引用消除；config 下仍余 8 壳待第三批：boot_image/caps/error/kaslr/procfs/sched/slab/validate）。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过（services 0 unsafe、6 不变式 PASS、注释/C 命名 0 违规）✅ / host-tests 全量通过 ✅ / QEMU 未跑（纯常量归位不触 boot，与下一批壳删除合并冒烟）。

### DECISION-J 第三批执行记录：config 其余 8 壳（caps/kaslr/sched/slab/boot_image 反转 + error/procfs 删壳 + validate 保留）

> 逐项调研 framework 机制消费点后按统一判据处理。**关键调研结论**：SCHED_\* 与 CFS_\* 分属不同消费方（前者被 framework proc 机制消费、后者仅 services sched_policy 消费）→ 拆分归属；KernelCapabilities 被 framework mm/vmm_x86_64 KPTI 决策直接消费 → 机制类型。

- **迁回 framework（5 壳真实定义 + services re-export）**：
  - `config/caps.rs`：`ConfigSummary`/`KernelCapabilities`(+`detect`) 迁回（0 unsafe，依赖闭包为空）；services re-export。
  - `config/kaslr.rs`：`KASLR_*` 常量 + `KASLR_BASE_OFFSET` 全局状态 + `set/get/is_aligned` 迁回（机制持有全局状态）；`validate_kaslr_offset`（启动自检，返回 services `KernelError`）**留 services**——注意 framework 顶层以 `is_kaslr_aligned` 别名暴露 `is_aligned`，services 侧用 `is_kaslr_aligned as is_aligned` 还原名称保 API 兼容。
  - `config/sched.rs`：**拆分**——`SCHED_*`（6 项，被 framework proc user_proc/scheduler_ex 消费）迁回；`CFS_*`（7 项，仅 services proc/sched_policy 消费）留 services；services/config/sched.rs 变混合（CFS_* 定义 + SCHED_* re-export），sched_policy.rs 导入改 services::config。
  - `config/slab.rs`：`SLAB_*` 4 常量迁回（被 framework mm/slab 机制消费），`SLAB_DEFAULT_SIZE` 改引用 framework 自身 `PAGE_SIZE`。
  - `config/boot_image.rs`：`encode_boot_image`/`read_boot_image`/`BOOT_IMAGE`/`encoded_len` 全部迁回（被 framework config::init() 机制消费，依赖闭包 `get_config_summary`+`IrqSpinLock` 全在 framework 内）；services glob re-export。
- **删除壳（services 独有策略项，调用点改 services 路径）**：
  - `config/error.rs`：`ConfigError` 为 services validate 策略返回类型，framework 生产代码不消费 → **删壳**；validate.rs/tests 改 `services::config::ConfigError`。
  - `config/procfs.rs`：`read_sys_config` 等为 /proc 用户态接口服务，framework 无生产消费 → **删壳** + 移除 `pub mod procfs`；services/fs/procfs_core.rs + framework/tests 改 `services::config::procfs` 路径。
- **保留壳（后续 trait 注入批次）**：`config/validate.rs`（`validate_system_config`/`validate_drivers` 被 framework config::init() 调用，需 ConfigValidateHook trait 注入，按 DECISION-I「先壳删 → 再 trait 注入」顺序留待 ipc 之后的批次）。
- **引用计数**：framework 文件级反向依赖 76→70、精确行数 135→132；config 目录仅剩 validate.rs 壳。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过（services 0 unsafe、6 不变式 PASS、SAFETY 覆盖 0 缺漏、注释/C 命名 0 违规）✅ / host-tests 全量通过 ✅ / QEMU 未跑（常量/类型归位不触 boot，与下一批壳删除合并冒烟）。

### DECISION-J 第四批执行记录：ipc 4 纯壳删除（sem/signal/scheduler_integration/async_ipc）

> 调研确认：framework 生产代码（非 cfg(test)）对 4 壳项均无消费——`sem/signal` 系统调用走 `framework::proc::do_signal_*` 与 services syscall 层，不经 `framework::ipc::{sem,signal}`；`scheduler_integration` 仅被 services 内部（msgq/sem/pipe）消费；`async_ipc` 仅 cfg async re-export。services/ipc 已完全自足（含 `IpcLock` 完整 API）。

- **删除壳**：framework/ipc/{sem,signal,scheduler_integration,async_ipc}.rs 删除；mod.rs 移除对应 `pub mod` 声明 + 顶层 re-export（`block_current_thread` 等 4 函数 + cfg async 的 `AsyncMsgSender` 等）。
- **cfg(test) 引用同步**（测试代码允许访问 services，§7.3 精神）：
  - mod.rs `mod tests` 加 `use crate::kernel::services::ipc::{sem, signal};`
  - stress_tests.rs `use ...::ipc::{msgq, pipe, sem, shm}` 补 sem
  - framework/tests/test_ipc.rs `use ...::services::ipc::{pipe, sem, shm}` 替换 framework 壳路径
  - api.rs 头注释同步（scheduler_integration/sem/signal 指向 services）
- **引用计数**：framework 文件级反向依赖 70→66、精确行数 132→129；ipc 目录生产代码反向依赖仅剩 pipe.rs(5)/shm.rs(4)/msgq.rs(4) FFI 边界 13 处——**DECISION-I IpcStrategy trait 注入对象**（下一批）。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过（含 kernel_test/host-test clippy 维）✅ / host-tests 97 套件全通过 ✅ / QEMU 未跑（纯壳删除不触 boot，与 trait 注入批次合并冒烟）。

### DECISION-I 首战执行记录：IpcStrategy trait 注入（ipc FFI 13 处收敛）

> DECISION-I 裁决："IpcStrategy trait 注入为 trait 化首战（ipc 24 处最大头，13 处 FFI 集中）；注册时序单独评审"。本次完成 trait 注入主体，注册时序设计留待评审（见下）。

- **framework 侧（契约 + 注册点）**：新增 `framework/ipc/strategy.rs` —— `IpcStrategy` trait（13 方法：pipe×5 / shm×4 / msgq×4，签名对齐 services `*_safe`，引用 framework 类型 `IpcNamespace`/`IpcId`）+ `static IPC_STRATEGY: OnceLock<&dyn IpcStrategy>` + `register_ipc_strategy()` + `current_ipc_strategy()`（**无内建回退**：策略方法依赖 services 实现，framework 无法安全回退，未注册即调用 panic——与 `services::ipc::global()` 的 expect 契约一致，IPC FFI 仅在 syscall 时触发）。mod.rs 顶层 re-export trait + 注册/获取入口。
- **services 侧（实现 + 注册）**：新增 `services/ipc/strategy.rs` —— `DefaultIpcStrategy` impl（包装 `pipe/shm/msgq` 的 `*_safe`，保持 T6 权威）+ `register_default_ipc_strategy()`（幂等，`let _ =` 风格，`#![deny(unsafe_code)]`）。
- **FFI 边界改造**：pipe.rs(5)/shm.rs(4)/msgq.rs(4) 共 13 处 `crate::kernel::services::ipc::*::*_safe` 改经 `current_ipc_strategy().*` 调用，framework→services 直接引用归零（ipc 生产代码）。
- **注册时序（待评审，DECISION-I 要求单独评审）**：lib.rs kernel_init 编排中、VFS init 后 / UDS 前插入 `register_default_ipc_strategy().expect(...)`。时序论证：framework ipc 机制（IPC_NAMESPACE）由启动早期初始化 → services 注册在 scheduler 之后 → FFI 仅在用户态 IPC syscall 时触发（用户态启动远晚于注册点）。**评审点**：注册是否应更贴近 framework ipc_init 时机（更早）？`current_ipc_strategy()` 未注册 panic 是否可接受（vs 返回 Err）？
- **引用计数**：framework 文件级反向依赖 66→63、精确行数 129→116（ipc FFI 13 处消除）。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过 ✅ / host-tests 97 套件全通过 ✅ / QEMU 未跑（本次含 lib.rs 启动编排改动，QEMU 冒烟与后续批次合并执行）。

### DECISION-J 第五批执行记录：sync/types 反转

> 首批"机制类型壳"反转（剩余壳多为同构，模式已确立：机制消费→反转归位，services 独有→删壳）。调研确认：`SpinLockInner`/`MutexInner`/`RwLockInner`/RAII 守卫/IrqSaveFlags 被 framework sync 机制（spinlock/rwlock/mutex FFI 层）直接消费，且 `#[repr(C)]` 与 C 版本布局兼容 — 属机制类型。

- **迁回 framework**：`framework/sync/types.rs` 由 re-export 壳改为真实定义（LockState/TryLockResult/SpinLockInner/MutexInner/RwLockInner/CondVarInner/IrqSaveFlags/LockStatistics + 5 个 RAII 守卫，0 unsafe，依赖闭包为空）。mod.rs 顶层既有 `pub use types::{...}` 现解析到 framework 自身。
- **services 改 re-export**：`services/sync/types.rs` 改为 `pub use crate::kernel::framework::sync::types::*`（framework/sync/types 为 `pub mod`，glob 可行，同 ipc/types 模式）。services/sync/mod.rs 的显式 re-export 不变（解析到 framework 项）。
- **引用计数**：framework 文件级反向依赖 63→62。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过 ✅ / host-tests 全量通过 ✅ / QEMU 未跑（纯类型归位不触 boot，合并冒烟）。

### DECISION-J 第六批执行记录：机制常量/状态 4 壳反转（net/types + barrier/reset_config + mm/numa + io/iouring）

> 批量处理同构"机制持有项"壳。逐项调研 framework 消费点后判定：4 项均被 framework 机制直接消费且依赖闭包全在 framework 内 → 反转归位。模式与第五批相同（cp + 头部 DECISION-J 注释 + services glob re-export）。

- **net/types**：`NET_READY`/`NET_CONFIGURED` 全局状态 + `FALLBACK_*` 常量迁回（被 framework net init/dns 机制消费）。services glob re-export。**host-test 同步**：`dhcp_fallback_const_test.rs` 的 `TYPES_RS` 路径 `services/net/types.rs` → `framework/net/types.rs`（跨模块接口变更，§8）。
- **barrier/reset_config**：`RecoveryLayer`/`RecoveryResult`/`set_reset_*` 等恢复配置迁回（被 framework proc/scheduler Barrier 恢复域路径消费，0 外部依赖）。services glob re-export（framework 路径 `barrier::reset::config`）。
- **mm/numa**：`NumaTopology`/`NumaNode`/`NumaMempolicy`/`sys_*` 迁回（`NumaMempolicy` 被 framework proc/process 持有、`numa_init` 被 framework mm 调用；依赖闭包 `mm::PAGE_SIZE`+`sync::IrqSpinLock` 在 framework 内）。services glob re-export（services/syscall 的 4 处 `numa::sys_*` 引用经 glob 保持可用，无需改）。
- **io/iouring**：`Sqe`/`Cqe`/`RingBuffer`/`IoUring` + `sys_io_uring_*` 迁回（被 framework syscall dispatch 直接调用；依赖闭包 `sync::IrqSpinLock`+`errno::Errno` 在 framework 内）。services glob re-export。
- **引用计数**：framework 文件级反向依赖 62→58。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过 ✅ / host-tests 97 套件全通过 ✅ / QEMU 未跑（纯常量/类型归位不触 boot，合并冒烟）。

### DECISION-J 第七批执行记录：wasm 5 壳删除

> 调研确认：framework 生产代码（非壳自身）对 `framework::wasm` 无任何消费；wasm 解释器 + wasi 为 services 完全自足实现（services/wasm 内部 + wasi 消费）。属"framework 无生产消费的纯转发壳" → 删除壳（同 config error/procfs 模式）。

- **删除壳**：framework/wasm/{mod,types,leb128,module,runtime,interpreter}.rs 全删（mod.rs 仅模块声明 + 文档）；framework/mod.rs 移除 `pub mod wasm;`；audit_services_boundary.py 黑名单残留条目 `framework::wasm` 同步移除（F2 黑名单维护）。
- **引用计数**：framework 文件级反向依赖 58→53。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过 ✅ / host-tests 97 套件全通过 ✅ / QEMU 未跑（纯壳删除不触 boot，合并冒烟）。

### DECISION-J 第八批执行记录：net 3 壳治理（wait_queue/netfilter 反转 + route 合并反转）

> 逐项调研 framework 消费点后判定：wait_queue 的 `SOCKET_WAIT_QUEUES` 全局表被 framework net/init `poll_network` 机制直接消费（host-test 注释亦声明应归 framework）；netfilter 的 `sys_nf_*` 被 syscall dispatch 调用（同 io/iouring）；route 的 `RouteEntry` 被 framework smoltcp 同步机制消费 + `sys_route_*` 被 syscall dispatch 调用——三项均反转归位。

- **wait_queue 反转**：`SocketWaitQueue`/`SocketWaitQueueTable`/`SOCKET_WAIT_QUEUES` 迁回 framework/net/wait_queue.rs（0 unsafe）。services glob re-export。**host-test 同步**：`socket_wait_queue_test.rs` 8 处源路径 `services/net/wait_queue.rs` → `framework/net/wait_queue.rs`。
- **netfilter 反转**：`NfRule`/`NfHook`/`nf_*`/`sys_nf_*` 迁回 framework/net/netfilter.rs（依赖闭包 `sync::IrqSpinLock`+`syscall::Errno` 在 framework 内）。services glob re-export。
- **route 合并反转**：原 framework/net/route.rs 已有 smoltcp 同步逻辑（`sync_route_to_smoltcp`/`rebuild_smoltcp_routes`，依赖 raw::stack_mut），壳部分 re-export services 路由表 CRUD——**合并**：路由表 CRUD/CIDR 匹配/syscall/类型（`RouteEntry`/`MAX_ROUTES`/`RouteQueryResult`）并入 framework 文件，删除 re-export 块；services 版 L110/L132 的 `framework::net::route::sync_route_to_smoltcp` 等改同文件直接调用。services glob re-export。**双向引用解除**（services↔framework 绕圈消除）。
- **引用计数**：framework 文件级反向依赖 53→50。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit.sh 核心审计全过 ✅ / host-tests 97 套件全通过 ✅ / QEMU 未跑（纯机制归位不触 boot 主路径，合并冒烟）。

### DECISION-J 第九批执行记录：fs 系列第一小批 — procfs 壳删除

> fs 系列按 DECISION-K 项 5 方向推进（机制类型迁回 + inode trait 注入不连带迁回）。本批先处理最简单的 procfs 壳：调研确认 framework 生产代码对 `framework::fs::procfs` **零消费**（仅 framework/tests 经 services 路径），属"framework 无生产消费的纯转发壳" → 删壳（同 wasm/config error 模式）。ramfs/devfs/nestfs/flock/inotify 因依赖闭包复杂（services dcache/inode 深度耦合），按 DECISION-K 归后续专项。

- **删除壳**：framework/fs/procfs/{mod.rs,procfs.rs} 全删（mod.rs 仅 `pub mod procfs; pub use procfs::*;` 转发）；framework/fs/mod.rs 移除 `pub mod procfs;`；audit_coupling.py fs 内部白名单移除 `framework::fs::procfs` 条目。
- **services 侧不变**：`services::fs::procfs`（含 framework/tests 用的 `init_global`）保持 services 权威，路径引用本就直连 services 无需改。
- **引用计数**：framework 文件级反向依赖 50→49。
- **验证**：双架构 0w0e ✅ / clippy -D warnings ✅ / audit.sh 全过（含 coupling 无循环）✅ / host-tests 98 套件全通过 ✅ / QEMU 未跑（纯壳删除不触 boot）。

### DECISION-J 第十批执行记录：mm/pressure 壳删除

> 调研确认：framework 生产代码对 `framework::mm::pressure`（`MemoryPressure`/`update_pressure`）**零消费**——`update_pressure` 定义于 services/mm/memory_pressure.rs、唯一消费方 services/proc/oomd.rs（services 内部），framework 侧仅转发链（pressure → api → mechanism → mod 顶层）。属"framework 无生产消费的纯转发壳" → 删壳。

- **删除壳**：framework/mm/pressure.rs 删除；mm/api.rs 移除 `pub use super::pressure::{MemoryPressure, update_pressure}`；mm/mechanism.rs 移除 2 个转发（恢复"页错误处理"注释，无多余改动）；mm/mod.rs 移除 `pub mod pressure` + 顶层 re-export 中 MemoryPressure/update_pressure。
- **调用点同步**：services/proc/oomd.rs 的 `mm_api::update_pressure`/`mm_api::MemoryPressure`（原经 framework::mm 别名）改为 `use crate::kernel::services::mm::memory_pressure::{MemoryPressure, update_pressure}` 直连 services（services 权威，合法方向）。
- **host-test 同步**：`memory_pressure_extraction_test.rs` 的 `framework_re_exports_memory_pressure` 改名为 `framework_pressure_shell_removed`——断言 framework/mm/pressure.rs 已删除 + mm/mod.rs 不再声明/re-export（P1-I-01 D9 契约随 DECISION-J 更新）。
- **引用计数**：framework 文件级反向依赖 49→48。
- **验证**：双架构 0w0e ✅ / clippy -D warnings ✅ / audit.sh 全过 ✅ / host-tests 98 套件全通过 ✅（memory_pressure_extraction_test 8 用例过）/ QEMU 未跑（纯壳删除不触 boot）。

### DECISION-K 项 2 执行记录：ConfigValidateHook trait 注入（validate 壳收口）

> 按 DECISION-K 项 2：validate 壳（re-export services validate_*）改为 `ConfigValidateHook` trait 注入，并入统一"机制 init 后立即注册策略"启动契约；未注册语义 **Option 可空**（跳过校验 + 日志，不 panic）。

- **ConfigError 迁回 framework**：`ConfigError` 为 `ConfigValidateHook` trait 返回类型（机制持有）→ 迁回 `framework/config/error.rs`（0 unsafe，纯类型 + Display）；services/config/error.rs 改经 framework 顶层显式 re-export；services/config/validate.rs 的 ConfigError 引用改回 framework（services→framework 合法）。
- **framework 侧（契约 + 注册点）**：新增 `framework/config/validate_hook.rs` —— `ConfigValidateHook` trait（4 方法：validate_system_config/validate_drivers 返回 u32，validate_pci_subsystem/validate_network_subsystem 返回 `Result<(), ConfigError>`）+ OnceLock 注册点 + `current_config_validate_hook()`（Option，未注册 None）；mod.rs 顶层 re-export trait + 注册/获取入口。
- **services 侧（实现 + 注册）**：services/config/validate.rs 新增 `DefaultConfigValidateHook` impl（包装既有 validate_* 函数，保持策略权威）+ `register_default_config_validate_hook()`（幂等）。
- **framework 消费点改造**：config::init() 的 validate_system_config/validate_drivers 改经 hook（未注册跳过 + 日志）；driver/bus/pci.rs 与 net/init.rs 的 validate_pci_subsystem/validate_network_subsystem 改经 hook（Option 可空，未注册跳过自检）。
- **删壳**：framework/config/validate.rs 删除；mod.rs 移除 validate_* 顶层 re-export；framework/tests/test_config.rs 的 validate_memory_config/validate_cross_module_consistency 改 services::config::validate 路径（§7.3 允许）。
- **注册时序**：lib.rs kernel_init 0.05 节注册点（klog + canary 后、framework config::init() 之前，`// ConfigValidateHook 注册契约点 (DECISION-K)`）。
- **引用计数**：framework 文件级反向依赖 48→47。
- **验证**：双架构 0w0e ✅ / clippy -D warnings ✅ / audit.sh 全过 ✅ / host-tests 98 套件全通过 ✅ / **QEMU x86_64 启动通过**（config::init 编排改动，实测正常）。

### DECISION-J 第十一批执行记录：fd_alloc 机制迁回

> 调研确认：`services/proc/fd_alloc.rs`（TD-02/I-51 全局统一 FD 分配器）是**用户态可见的全局 FD 编号内核机制**（集中基址规划 + 位图分配/释放/反查），被 framework 4 处（sm_fi/eventfd/signalfd/timerfd）+ services（inotify/pidfd）消费，依赖闭包为空（纯 core 原子 + 编译期 const 规划）——按 DECISION-J 迁回 framework。

- **迁回 framework**：`framework/proc/fd_alloc.rs` 由 re-export 壳改为真实定义（346 行，0 unsafe）；services/proc/fd_alloc.rs 改 glob re-export（framework/proc fd_alloc 为 `pub mod`，同 ipc/types 模式）。framework/proc/mod.rs 顶层 re-export（FdPlan/FdSubsystem/alloc_fd/fd_at/free_fd/idx_of）现解析到 framework 自身。
- **消费点**：services pidfd.rs 已走 `framework::proc::fd_alloc::` 路径（不变）；services inotify.rs 的 `services::proc::fd_alloc::*` 经 glob re-export 保持可用（无需改）。
- **host-test 同步**：`fd_allocator_unified_test.rs` + `td15_fd_idx_of_test.rs` 的源路径 `services/proc/fd_alloc.rs` → `framework/proc/fd_alloc.rs`。
- **引用计数**：framework 文件级反向依赖 47→46。
- **验证**：双架构 0w0e ✅ / clippy -D warnings ✅ / audit.sh 全过 ✅ / host-tests 全通过（fd 两套件 9+6 用例）✅ / QEMU x86_64 启动通过 ✅。

### DECISION-J 第十二批执行记录：madvise_mlock 迁回 + 串联回归修复（commit 待定）

- **madvise_mlock 迁回 framework**：`services/mm/madvise_mlock.rs`（sys_madvise/mlock/munlock/mlockall/munlockall/mincore，0 unsafe，依赖闭包全在 framework：errno/PAGE_SIZE/copy_user/vma_get_current_mm/userptr）为机制项迁回 `framework/proc/madvise_mlock.rs`（真实实现，同 io_uring/fd_alloc 模式）；services/mm/madvise_mlock.rs 改 glob re-export（framework/proc madvise_mlock 为 `pub mod`）。services/syscall/dispatch.rs 经 services::mm re-export 保持可用（合法方向）。
- **关联消引**：framework/syscall/madvise_mlock.rs 原 re-export 自 services::mm，改指 framework/proc::madvise_mlock（glob）。
- **serial 回归修复（DECISION-G §6.4 遗留 link 断裂）**：char serial 下沉 commit 9ba997e3 删除 `framework/driver/char/serial.rs` 时连带删除 `extern "C" serial_has_data/serial_getc`（原 L721/L764），但 framework/syscall 的 `sys_read(fd=0)` 仍经 `raw::read_serial_byte` 引用 → 链接 undefined reference，长期被 QEMU 增量链接掩盖。修复：移除 framework sys_read 串口 stdin 硬读分支（保留 x86_64 键盘 stdin，keyboard FFI 定义仍在 framework input 机制）+ 删 `raw::read_serial_byte`/serial extern 声明；`raw::write_u8` 加 `#[cfg(all(target_arch="x86_64", not(feature="kernel_test")))]`（仅 x86_64 键盘消费）。串口 stdin 待 devfs 桥接入 services char 权威（与 §6.2 SIMPLIFIED 一致，char 读写路径休眠）。
- **pedantic 补齐（DECISION-K 遗留）**：audit quick 曾报 16 处 pedantic（15 missing_errors_doc + config/mod.rs match 单分支 + redundant closure），为 DECISION-K 批次产物（IpcStrategy/ConfigValidateHook trait 注入）。补齐 #Errors 文档（framework/ipc/strategy.rs 12 方法 + services 侧 register_default_ipc_strategy/register_default_config_validate_hook 两个注册函数）、config/mod.rs match→if-let + map_or 直接函数引用。
- **引用计数**：framework 文件级反向依赖 46→44（madvise_mlock proc/syscall 两壳）。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit quick 全 0（pedantic lib + kernel_test + host-test 三维）✅ / host-tests 全通过 ✅ / QEMU x86_64 完整启动到 Ring 3（VFS ready，串口 242 行）✅。

### DECISION-J 第十三批执行记录：fd_alloc 迁回关联消引（commit 待定）

- **背景**：第十一批 fd_alloc 迁回 framework 后，framework 侧 4 个消费点（`syscall/eventfd.rs`/`signalfd.rs`/`timerfd.rs` + `net/init/sm_fi.rs`）仍写 `crate::kernel::services::proc::fd_alloc::` 路径——经 services glob re-export 壳解析成功（services→framework 合法方向），但 framework→services 构成反向依赖，属第十一批迁回遗漏。
- **修复**：framework 内 12 处引用统一改指 `crate::kernel::framework::proc::fd_alloc::`（`framework/proc/fd_alloc` 为 `pub mod`，纯路径替换零逻辑改动）。
- **引用计数**：framework 文件级反向依赖 44→41、行数 98→87。
- **sm_fi 剩余**：`uds_setsockopt` 委托（`services::net::unix`）留待 UDS/SocketStrategy 委托 trait 注入专项（文档 §7 net 剩余 2）。
- **验证**：双架构 0w0e ✅ / clippy -D warnings 双架构 0 ✅ / audit quick 全 0（pedantic 三维）✅ / host-tests 全通过 ✅ / QEMU x86_64 完整启动到 Ring 3（VFS ready，串口 242 行）✅。

### 前置核实执行记录（步骤 1，2026-09-12）

> 对 11 项 services 影子逐项核实"内容自足性"（0 unsafe / 硬件经 IoMem/IoPort/DmaStream/Chitin 机制 API / 业务自含不依赖 framework 内部）：

| 子类 | 项 | 核实结果 | 结论 |
|---|---|---|---|
| char | serial.rs | ✅ 0 unsafe；PIO 全经 `framework::ioport::IoPort`（new_safe）；业务自含 | **直接接线** |
| char | vga.rs | ✅ 0 unsafe；MMIO/PIO 经 `IoMem::from_pci_bar` + `IoPort::new_safe`；业务自含 | **直接接线** |
| virtio | blk.rs / net.rs | ⚠ 依赖 `framework::driver::virtio::queue::{DmaBuffer, VirtQueue}`（DMA 环机制，合法机制依赖）；blk/net 业务已完整核实（net RX 半成品迁业务执行记录见下文 virtio 净段） | **已核实**（下文 virtio 净段两记录） |
| storage | nvme.rs | ✅ 0 unsafe；wire 类型依赖可留；identify 解析 helper 迁 services（7 用例 host-test 通过）；MSI-X/IRQ 端到端实装（MSI-X 使能后建 I/O 队列时序契约 + services 分发契约）；NvmeBlockDevice 经 Chitin 注册 | **已核实**（批次 Y 完成，见执行记录） |
| storage | ahci.rs / ata.rs / mod.rs | ✅ ahci 0 unsafe 自足（IoMem 安全代理）+ AhciBlockDevice 适配注册；COMRESET/命令头布局/DMA 虚拟地址等首次带盘缺陷已修（执行记录）；**ata 已于阶段 3 迁 services**（framework `ata.rs`/`ata_block.rs` 已删，services `ata.rs` 真实 PIO 驱动经 `IoPort::new_safe` 0 unsafe，仅注册块设备 `ata0-3`） | **已核实**（批次 Y 完成 + 阶段 3 收尾，见执行记录） |

**接线改造的关键耦合点（步骤 2/3 设计确认）**：
- Chitin 注册安全路径 = `chitin_register_driver(name, proto, io_base, irq, Box<dyn Driver>)`，`Driver` trait **全 safe 方法**（framework/driver/framework.rs:287）→ **services 可 0 unsafe impl Driver 并注册**（合法方向）。
- 但**读写路径** `ChitinOps::Char(&CharOps)` 是 `extern "C" fn(driver_data: *mut u8, ...)` 指针表（framework/chitin/proto_char.rs:11-24），实现体需 unsafe 指针转换（framework 侧 serial.rs:687/718）→ **services 0 unsafe 无法直接构造** → 需 framework 提供**安全桥 trait**（framework 定义 `CharDeviceOps` + 构造 CharOps 的机制函数，unsafe 转换留在 framework；services impl trait）——即 §7.4 trait 注入模式，与审核员裁决一致。
- 接线编排：crate root `src/rust/src/lib.rs` 是合法双向编排者（L763 调 `framework::driver::init_all()`、L833 调 `services::syscall::init()`）→ char_init 迁至 services 后由 lib.rs 调用，framework init_all 移除 char 项。

### char 子类接线实施记录（步骤 4 首批，commit 9ba997e3）

- **services 侧（新增权威）**：`services/driver/char/serial.rs` + `vga.rs` 各加 `impl Driver`（name/device_type/init/shutdown 全 safe）；`services/driver/char/mod.rs` 新增 `char_init()`（x86_64）将 VgaConsole + COM1 SerialPort 注册进 Chitin（`chitin_register_driver`，合法方向）。
- **framework 退位**：删除 `framework/driver/char/{serial,vga}.rs`；`char/mod.rs` 仅保留 aarch64 pl011；`driver/mod.rs` 移除 char serial/vga 顶层 re-export（VgaColor/SerialPort/BaudRate/RingBuffer 等）与 init_all 中 x86_64 char 项。
- **接线**：`lib.rs` init_all 后新增 `#[cfg(x86_64)] services::driver::char::char_init()`。
- **测试同步**：framework/tests/driver.rs 移除 serial 测试块（framework serial 已删，纯逻辑测试迁 host-tests 为后续项）；tests/driver_test.rs 的 vga/serial 输出改走 services VgaConsole/SerialPort（§7.3 允许 framework/tests 访问 services）。
- **SIMPLIFIED（已登记）**：注册走 `chitin_register_driver` 无 CharOps 读写绑定——Chitin char 读写路径当前无生产消费者（休眠）；待 devfs char 读写接入时按 §6.2 补 framework 安全桥 trait。
- **验证**：双架构 0w0e ✅ / clippy -D pedantic 双架构 0 ✅ / 核心审计全 0 ✅ / host-tests 全量通过 ✅（fs_permissions_regression_test 单跑 28s 通过，为慢二进制非挂起）。

### virtio 子类接线实施记录（步骤 4 第二批，commit e47c04ad）

> 验证：双架构 0w0e ✅ / clippy -D pedantic 双架构 0 ✅ / 核心审计全 0 ✅ / host-tests 全量通过 ✅ / QEMU x86_64 完整启动到 Ring 3（blk_init 探测 0 设备干净跳过，Chitin 计数不变）。

- **前置核实**：services `virtio/blk.rs` 自足（0 unsafe，经 `transport::VirtioDevice` 安全代理 + framework `queue::{DmaBuffer,VirtQueue}` DMA 机制；完整 I/O 路径）→ **直接接线**。
- **services 侧（新增权威）**：`services/driver/virtio/blk.rs` 新增 `finalize()`（vq0 MMIO 配置 + DRIVER_OK，等价 framework `VirtioBlk::new` 收尾）+ `impl BlockDevice`（blk_read/write/is_present/total_sectors，IoMem/VirtQueue 均为 framework unsafe Send+Sync → 0 unsafe 可实现）；`services/driver/virtio/mod.rs` 新增 `blk_init()`（探测 virtio-mmio 区域，为块设备建 `VirtioBlkDriver`，finalize 后经 `proto_block::register_block_device` 注册）。
- **framework 退位**：删除 `framework/driver/virtio/blk.rs`；`virtio/mod.rs` 移除 `pub mod blk`（保留 `VirtioMmioDevice` 传输机制 + `queue` DMA 环机制）；**aarch64 `storage_init` 变空操作**（原 virtio-blk 探测注册迁 services；x86_64 storage_init 走 PCI AHCI/NVMe 不受影响；klog imports + 旧 #[expect] 随之 cfg 门控/清理）。
- **接线**：`lib.rs` init_all 后新增 `services::driver::virtio::blk_init()`（双架构；NestFS 块扫描之前，x86_64 无 virtio-mmio 即跳过）。
- **SIMPLIFIED（已登记）**：services blk 走 spin-loop 轮询（framework 版有 I-42 IRQ 事件驱动路径），功能等价、效率略低；IRQ 驱动为后续优化项。
- **待登记**：services `transport::VirtioDevice` 与 framework `VirtioMmioDevice` 存在**传输层双份**（各自 IoMem 探测）——按服务对象准则 transport 属机制应保留 framework，services 版是否删除/改为薄代理留待 §7 反向依赖治理阶段裁决。

### char/virtio-blk 前置核实结果（同 storage 标准，回报裁决，2026-09-12）

> 审核员要求：按 storage 标准核实 char/virtio-blk 的 services 实现是否"IRQ 路径 / _block 适配器 / 注册路径等值存在"，结果回报后定"直接接线 or 转专项"。

| 子项 | IRQ 路径 | 适配器/注册路径 | 结论 |
|---|---|---|---|
| char（vga/serial）| **不适用**——vga/serial 为轮询 console，framework 原实现同样无 IRQ（串口轮询）；services 无缺失 | 注册路径已建：`char_init` → Chitin ✓（9ba997e3）| ✅ **直接接线成立** |
| virtio-blk | **缺失**——framework VirtioBlk 有 I-42 `enable_irq`（IRQ 事件驱动完成），services VirtioBlkDriver 仅 `ack_interrupt`（清中断状态），I/O 走 spin-loop 轮询 | 适配器已建：`impl BlockDevice` ✓ + 注册路径 `blk_init` ✓（e47c04ad）| ⚠ **按严格标准 IRQ 路径不等值** |

**裁决请求**：virtio-blk 的 IRQ 路径（I-42）是否必须迁 services（→ 转专项，补 IRQ 完成路径）？还是接受 spin-loop 轮询为"功能等值、效率略低"（维持直接接线，SIMPLIFIED 已登记为后续优化项）？framework virtio-net 亦无 IRQ（同为轮询）可作旁证。

### storage 前置核实记录（步骤 1 续核，2026-09-12）

> 按 DECISION-G 规则核实 services storage 内容自足性，结论：**services storage 为"并行实现但内容不等价"（Phase 2.1.3/2.1.4 迁移半成品）——不满足"直接接线"，属"从 framework 迁业务"（B 形态），且迁移量显著大于 char/virtio**。

| 子项 | 核实结果 | 结论 |
|---|---|---|
| services nvme.rs | 依赖 framework C-FFI 机制（`nvme_submit_*_cmd`/`nvme_alloc_*`/`nvme_copy_*`，DMA/队列提交=机制留 framework 合理）+ `fw_nvme::NvmeCommand/Completion`（wire 类型）+ `fw_storage::nvme_read_identify_*`（解析 helper=业务，需迁 services）；**缺 MSI-X/IRQ 路径**（framework 版有 enable_msix + I-42 事件驱动） | 迁业务：identify helper + MSI-X |
| services ahci.rs | 仅依赖 IoMem/PhysAddr（自足 ✓）；**缺 _block 适配器与接线**（framework ahci_block.rs 是 active 注册） | 迁业务：_block 适配器 + 接线 |
| services ata.rs | ✅ **阶段 3 收尾**：services `ata.rs` 已是真实 PIO 驱动（0 unsafe，经 `IoPort::new_safe`，`AtaBlockDevice` 适配 + 仅注册块设备 `ata0-3`）；framework `ata.rs`/`ata_block.rs` 已删 | **已完成**（阶段 3 迁移落地，见执行记录） |
| services storage 整体 | **无 `impl BlockDevice`、无 `impl Driver`、无 init/注册入口**——控制器实现（队列/identify/I/O）与注册路径（_block 适配器）分离，注册全在 framework | 需补齐注册路径 |
| framework storage_init | x86_64 巨大函数（PCI 扫描 + AHCI/NVMe 创建 + **MSI-X 接入** + MSIX-03 测试钩子 + I-42）；aarch64 已空操作（virtio-blk 迁出） | 退位后 x86_64 需迁出业务 |

**风险提示**：storage 下沉若操之过急将**丢失功能**——MSI-X 中断驱动 NVMe（B07）、I-42 IRQ 路径、ATA PIO 真实驱动、MSIX-03 测试钩子均在 framework 侧且 services 无等价实现。**建议作为独立专项工程推进**（子步：identify helper 迁 services → services 补 _block 适配器 + MSI-X → 接线 → QEMU 存储冒烟），或与 §6.2/§7 并行规划。

### storage 专项 0 号子步实施记录（identify 解析迁 services，commit 05c9a648）

> 审核员裁决（2026-09-12）：storage 走 A 独立专项；0 号子步 = `nvme_read_identify_*` 解析 helper 迁 services（纯逻辑、可 host 测试、零风险）。

- **迁出（framework 删业务）**：删除 `framework/driver/storage/mod.rs` 的 `nvme_read_identify_controller` / `nvme_read_identify_namespace`（volatile 裸读 + 解析，-2 unsafe 块）。
- **迁入（services 纯函数）**：`services/driver/storage/nvme.rs` 新增 `parse_identify_controller(data: &[u8]) -> Option<(u32, [u8; 40])>`（nn@516 LE u32 + mn@24 40B）与 `parse_identify_namespace(data: &[u8]) -> Option<(u64, u8, u32)>`（nsze@0 LE u64 + flbas@26 + lbaf_data@128+idx*4；长度检查 192B 覆盖 LBA 表末项）。输入从裸 vaddr 改为字节切片 → 0 unsafe。
- **调用点改造**：`identify_controller` / `identify_namespace` 经 `nvme_copy_from_dma` 拷 DMA 字节到栈数组（520/192）后调用纯解析（与既有 read 路径一致，plain-copy 读 DMA）。
- **边界修复（测试驱动发现）**：原 framework 实现 lbaf_idx=15 时读 offset 188..192 但无界检查——services 版显式要求 192B，杜绝越界 panic。
- **host 测试（实际运行）**：新增 `host-tests/tests/storage_identify_parse_host_test.rs`（7 用例：controller 全解析/短缓冲/空缓冲 + namespace 全解析/lbaf_idx 15/lbaf 越表/短缓冲）直接 import 内核真实函数（B08-12 路线 C）→ **全部通过**。services nvme.rs 内 cfg(test) 同语义单测同步补齐（该文件既有 dormant 单测风格，kernel_test 构建下不可直接 cargo test——build-std 冲突，实测 `cargo test -p queenx` 失败，属既有工程问题另议）。
- **验证**：双架构 0w0e ✅ / clippy -D pedantic 双架构 0 ✅ / 核心审计全 0 ✅ / host-tests 全量 ✅ / QEMU x86_64 启动 ✅。

> 验证：双架构 0w0e ✅ / clippy -D pedantic 双架构 0 ✅ / 核心审计全 0 ✅ / host-tests 全量通过 ✅（RX 为硬件路径 host 无法功能测试，纯逻辑按 framework 同构迁移，实际验证依赖后续 aarch64/QEMU virt 冒烟）。

- **前置核实结论**（DECISION-G 规则）：services `VirtioNetDriver` 的 **RX 数据路径是半成品**——`try_receive` 原实现"无法访问 DMA 缓冲区内容，返回 0 丢弃包，需维护 desc_idx→DmaBuffer 映射"（L453-475）→ 不满足"内容完整"，**该项属"从 framework 迁业务"（仍是 B 形态）**。
- **RX 迁业务（已完成，0 unsafe）**：`services/driver/virtio/net.rs` 新增 `rx_buffers: [Option<DmaBuffer>; 32]`（desc_idx==槽位索引，同 framework `rx_buffers` 同构）+ `refill_rx()`（逐槽分配 DmaBuffer + 提交设备可写描述符）+ 重写 `try_receive`（pop used → 槽位校验 → `read_slice` 拷贝有效载荷 → 回收 + 同槽重提交）+ `refill_single_rx`（复用槽位缓冲区，取代原 `mem::forget` 泄漏式重填）。`new()` 初始化后预填 RX。
- **接线待办（未做，需 framework 安全桥）**：framework net init `nic_probe_all`（net/init/probe.rs:75）经 `ChitinNetDevice` + `VIRTIO_NET_OPS_STATIC`（extern "C" NetOps 指针表，unsafe 转换到 framework `VirtioNet`）接入 smoltcp——services 0 unsafe 无法直接构造 NetOps，需 **framework NetOps 安全桥 trait**（同 char CharOps 桥模式）。且 net init 为机制，services 设备需经注册表分发。
- **验证**：双架构 0w0e ✅ / clippy -D pedantic 双架构 0 ✅ / 核心审计待跑 / host-tests 待跑（RX 为硬件路径，host 无法功能测试——纯逻辑已按 framework 同构迁移，实际验证依赖后续 aarch64/QEMU virt 冒烟）。

### 阶段 1 首批（syscall brk/canary/posix_timer 下沉）验证结果

| 门槛 | 结果 |
|---|---|
| 双架构 cargo check --release 0w0e | ✅ x86_64 + aarch64（RUSTFLAGS=-D warnings） |
| clippy -D pedantic 0（除 cast_*） | ✅ x86_64 + aarch64 |
| 核心审计 | ✅ boundary 0 / safety 100% / coupling 0 / comment 0 / deadlock 0 / invariants 全 PASS |
| host-tests | ✅ 全过（exit 0）——首次全量并行 `test_fsx_stress` 偶发失败（单独跑 33s 通过，全量重跑通过），判定为并行负载 flaky，非本次改动引入 |

> 备注：`scripts/audit_coupling.py` 中 `framework::syscall::{brk,canary,posix_timer}` 检测模式随文件删除失效（不再匹配），无害保留。

### DECISION-P: 2-B fd 事件族复核（epoll/eventfd/signalfd/timerfd 保留 framework）

> **背景**：§6.2 批次 2-B 目标为 fd 事件族 4 文件（`epoll.rs` 588 行 / `eventfd.rs` 446 行 / `signalfd.rs` 482 行 / `timerfd.rs` 617 行）「封装+下沉」。按 §6.2 复核纪律（DECISION-F）逐文件核查"framework 侧保留代码是否被机制直接调用"后，判定四文件**全部保留 framework**，本批零代码下沉。

**裁决**：
1. **epoll 本身即机制**：`EpollInstance`（wait_queue）承载进程阻塞/唤醒调度（`process_block`/`scheduler_unblock`），`epoll_pwake(fd)` 由源码自述为"机制层职责"，且被 framework `fd_notify` 注册、`timerfd.rs:480`、`inotify.rs:588` 直接调用——下沉将造成 framework→services 反向依赖（违反 §6.3 禁止反向调用原则）。
2. **eventfd / signalfd 被 epoll 机制直接调用**：framework `epoll::check_fd_ready` 直接调用 `eventfd::is_eventfd_fd` / `signalfd::is_signalfd_fd` 及各 poll 函数取事件位；二者 close 路径亦调 `epoll_pwake`。单独下沉同样引入反向依赖。
3. **timerfd 回调整体不可 safe 化**：`timerfd_callback(&HrTimer)` 经 container_of 反推 `TimerFdSlot`（`HrTimer` 为首字段），与 `framework/proc/posix_timer.rs` 先例同构；`TimerFdSlot` 出表销毁依赖同一机制，回调保留 framework。
4. **否决"扩展 `HrTimer` 增 safe cookie"方案**（用户裁定）：为下沉 4 文件而给 `HrTimer` 增字段属 TCB 改动，收益不抵 TCB 上升与回归风险，不予采纳。
5. **收口为「经复核保留」**：四文件保留 framework；services 侧 `services/sync/{epoll,eventfd,signalfd}.rs` 与 `services/timer/timerfd.rs` 维持现有薄代理壳（转发 framework syscall 实现），不作为实装载体。
6. **路径订正**：§6.2 原表列目标 `services/syscall/{epoll,eventfd,signalfd,timerfd}`，实际壳落点为 `services/sync/{epoll,eventfd,signalfd}` + `services/timer/timerfd`，表内已标【复核】。

**状态**: [X]（裁决完成；四文件保留 framework，2-B 无代码变更，转推 2-C char/input）

### DECISION-Q: 2-C char/input 复核（pl011/keyboard 保留 framework）+ input_init 重复注册订正

> **背景**：§6.2 批次 2-C 目标为 char/input 二文件（`framework/driver/char/pl011.rs` 183 行 / `framework/driver/input/keyboard.rs` 1015 行）「封装+下沉」。按 §6.2 复核纪律（DECISION-F）逐文件核查「framework 侧保留代码是否被机制/FFI 桥直接绑定」后，判定二文件**全部保留 framework**，本批零代码下沉；另订正 `input_init()` 的重复注册（同批）。

**裁决**：
1. **pl011 保留 framework**：① `framework::arch::uart` 是 boot 早期控制台机制（klog/panic 输出依赖 `unsafe fn init/putc/getc`），必须留 framework；② `pl011_read`/`pl011_write` 为 `CharOps` FFI 桥（`extern "C" fn(driver_data: *mut u8, ...)`，裸指针），不可 safe 化，桥体须操作 `Pl011Driver` 类型——若类型下沉 services 则 framework 桥反向依赖 services（违 §6.3）；③ PL011 为固定平台基址（非 PCI），`IoMem` 唯一 safe 构造器 `from_pci_bar` 不适用（无固定基址 safe 构造器）。
2. **keyboard 保留 framework**：① `kb_input_read`/`kb_input_has`/`kb_input_irq` 为 `InputOps` FFI 桥（裸指针 `driver_data as *mut KeyboardDriver`），同 pl011 不可 safe 化；② `read_line` 依赖 `unsafe extern "C" scheduler_yield_ex`（调度器外部符号）；③ `SCANCODE_TABLE`/`SHIFT_TABLE` 等纯逻辑虽可 safe 化，但与硬件状态机 `KeyboardDriver` 强绑定且无独立价值。
3. **路径取舍**：下沉前提是新建 `CharOps`/`InputOps` **trait 注入机制**（同 DECISION-K `IpcStrategy` 模式），而当前 char/input ops **无活跃生产消费者**（`services/driver/char` 读写路径休眠，见其 SIMPLIFIED 注释）——为休眠路径新增 TCB trait 机制，收益不抵 TCB 上升与回归风险。与 2-B（DECISION-P）同源判据：**FFI ABI 桥 → framework 薄层**。
4. **订正 `input_init()` 重复注册**：原 `input_init()` 先调 `keyboard::keyboard_init()`（经 `chitin_register_with_ops` 注册 `ps2_keyboard` + `InputOps` + IRQ1），再调 `chitin_register_driver("ps2_keyboard", ...)` 二次注册同名无 ops 设备。Chitin 注册表**不按名去重**（`chitin_register*` 直接 `devices.push`），且 `chitin_register_driver` 内部会跑 `driver.init()`——二次注册既产生同名设备节点，又对新建实例重跑 PS/2 自检/扫描码协商（硬件副作用）。已删除该冗余调用，保留 `keyboard_init()` 为唯一注册入口（[input/mod.rs](../../src/kernel/framework/driver/input/mod.rs)）。

**状态**: [X]（裁决完成；pl011/keyboard 保留 framework，2-C 无代码下沉；`input_init()` 重复注册已订正；转推 2-D usb）

#### 附录订正: chitin_shutdown_all 类型混淆（同批发现的独立缺陷）

> **背景**：2-C 施工调研中发现 `chitin_shutdown_all()` 对**裸指针设备**误用 `driver_from_obj()`：`chitin_register_with_ops`（keyboard / e1000 路径）存入的 `driver_data` 为调用方自持的裸驱动指针（`*mut KeyboardDriver` / `*mut E1000Driver`），而 `chitin_shutdown_all()` 无条件将其强转为 `*mut DriverObject` 解引用 → 类型混淆 UB（经 poweroff / kexec 路径触发）。与 2-C 无因果关系，经用户裁定「本轮一并订正」。

**订正**：
1. **新增 `ChitinDevice::driver_owned: bool` 字段**区分驱动所有权：`chitin_register_driver` / `chitin_register_driver_with_ops` → `true`（`driver_data` 指向 Chitin 自有 `DriverObject`，经 `Box::into_raw` 写入）；`chitin_register` / `chitin_register_with_ops` / `chitin_register_block_dev` → `false`（`driver_data` 为调用方自持裸指针，Chitin 不管理其生命周期）。
2. **`chitin_init_all()` / `chitin_shutdown_all()` 增加 `driver_owned` 守卫**：仅对 `driver_owned == true` 的设备执行 `driver_from_obj` 解引用 / `init()` / `shutdown()` / `Box::from_raw` 回收；裸指针设备跳过（其生命周期由 E1000_DEVICE / KEYBOARD_DEVICE 等调用方全局自持）。
3. **SAFETY 注释更新**：`chitin_shutdown_all` 的 `Box::from_raw` 安全依据改写为基于 `driver_owned` 不变式（保证 `driver_data` 由 `chitin_register_driver*` 经 `Box::into_raw(DriverObject)` 写入，类型一致且归 Chitin 所有）。

**状态**: [X]（订正完成；六门槛全过）

### DECISION-R: 2-D USB 整体下沉（framework/driver/usb 整目录删除，services 权威）

> **背景**：§6.2 原列 `framework/driver/usb/{mod,xhci}.rs` 二文件「封装+下沉」，其中 `xhci.rs` 旧处方为「20 unsafe 集中，机制留框架」。经用户裁定改走 **USB 整体下沉**——xHCI/枚举/类驱动权威实装全部落 `services/driver/usb`，framework 侧 `driver/usb` 整目录删除（覆盖上述旧处方）。同时收口 §6.4 原列 `driver/usb` 5 文件（DECISION-G 曾将其归类为「🔒 壳 → §6.5 删壳」，与整体下沉殊途同归——framework 侧清零，services 唯一权威）。

**方案（分子步推进）**：
1. **子步①**（commit 7183c545）：`enumerate` / `ring` / `hid` / `mass_storage` 4 个 0-unsafe 文件下沉 `services/driver/usb`，framework 侧暂留接线不动。
2. **子步②**（commit daccd4ab）：`usb_core.rs` safe 权威实装 + `xhci.rs` 补 `Driver` / `HostController` trait impl + 4 纯逻辑单测；`services/driver/usb/mod.rs` 承接 PCI 发现 + `usb_init`（chitin proto=Bus）；crate root `lib.rs` 接线切换至 services `usb_init`（委托编排者），framework `driver/mod.rs` 移除旧 `usb_init` 调用。
   - **safe 化三决策**：Q1=C（`Urb` 采用「物理地址 + 长度 + 方向」，避免 framework 裸指针导出）；Q2=A（`UsbCore.controllers` 用 `Vec<Box<dyn HostController>>` + `mem::take`，规避自引用）；Q3=A（子步② 即切换启动路径，保留 fallback）。
3. **子步③**：删除 `framework/driver/usb/` 整目录（7 文件），清理残留引用——`framework/driver/mod.rs`（模块声明 + 文档树 + `init_all` 列表）、`framework/pci/api.rs` 文档契约、`services/driver/usb/xhci.rs` stale bullet；同步 `scripts/audit_coupling.py`（移除 stale `framework::driver::usb` 正则）；适配 `tests/integration/run_driver1_usb_xhci_test.py`（Layer 2 静态检查改指 services 权威位置）。

**影响面**：
- §6.2 usb 2 文件标 ✅；§6.4 原列 usb 5 文件一并收口（framework 侧删，services 唯一权威）；§6.7 影子双份列表移除 usb。
- `make test-kernel-host` 用例数 903 → 826（减少 77 = framework 侧 USB 副本内联测试随目录删除；services 侧 USB 权威测试 69 项全通过，无有效覆盖损失）。
- QEMU 冒烟确认 services 路径生效：`[USB] discovered 0 xHCI controller(s)`。

**验证**：§2.3 六门槛全绿——双架构 0 error/0 warning、clippy 0、核心审计（含 `audit_coupling` / `audit_services_boundary`）、host-tests、kernel-host（826 passed / 0 failed）、QEMU x86_64 boot 至 `VFS ready` + KPTI 断言。

**状态**: [X]（2-D 收口；framework 侧 USB 权威实装清零）

### DECISION-S: 2-E display 部分下沉（controller.rs 迁 services）+ Framebuffer 机制/策略拆分登记为后续

> **背景**：§6.2 批次 2-E 目标为 `framework/driver/display/mod.rs`（428 行，9 处 unsafe），处方「VBE 原语留框架，管理迁出」。按 §6.2 复核纪律（DECISION-F）先查服务对象，再定归属。

**长期演进分析（内核终局视角）**：
1. **显示控制器属"驱动"**（§4.1 归属决策树 Q2=功能，非机制），正确归宿为 services；其共享契约 trait 应跟随实现者留 services。
2. **framework 对 `controller.rs` 无机制需求**——`DisplayManager` 全仓无实现者/消费者（仅 `display_init` 内 `let _manager` 丢弃 + 自身内联测试），属空壳抽象。若保留框架即把管理策略错放进 TCB。
3. **`Framebuffer`/`Font`/`Color`/`Rect`/`colors` 不可迁**——被 framework `gfx_console.rs`（klog/panic 机制）以裸指针 `*mut Framebuffer` 直接绑定；`FB_PHYS_ADDR`/`FB_PHYS_SIZE`/`get_framebuffer` 被 `framework/syscall/dispatch.rs`（`sys_fb_open`/`sys_fb_mmap`）绑定。迁出将造成 framework→services 反向依赖（违 §6.3 禁止反向调用）。
4. **display 的 unsafe 全部集中于 `mod.rs`**（VBE 端口 I/O + MMIO）；`framebuffer.rs`（850 行）/ `font.rs` / `self_test.rs` 均 0 unsafe——即"机制原语 vs 绘图策略"已在文件粒度天然分层，是 §6.3 部分下沉的天然切口。

**裁决（用户）**：采纳**方案 B（部分下沉）**——`controller.rs` 迁 services；并**登记** display §6.3 部分下沉（Framebuffer 机制/策略拆分 + gfx_console 去裸指针重构）为后续独立条目。否决方案 A（整体留框架，管理策略错置 TCB）与方案 C（删除 controller.rs，未来需重建）。

**方案**：
1. `framework/driver/display/controller.rs`（508 行）整体迁 `services/driver/display/controller.rs`；仅改头注（迁移说明）+ imports（`use crate::framework::driver::{DeviceInfo, DeviceType, Driver, DriverError, DriverResult as Result, Framebuffer, PixelFormat}` + `alloc::vec::Vec`），0 unsafe 保持。
2. framework 侧清理：`display/mod.rs` 删 `pub mod controller` + 控制器 re-export + `display_init` 内 `let _manager = DisplayManager::new()` 死语句；`driver/mod.rs` 补 re-export `PixelFormat`（services controller 依赖）；删除 framework 源文件。
3. `services/driver/display/mod.rs` 挂载 `pub mod controller;`；host-tests `driver_display_test.rs` 改引——`{Color, PixelFormat}` 留 framework，`DisplayMode` 改 `services::driver::display::controller`.

**影响面**：
- §6.2 display 标 ✅（23 文件收口进度：3 完成 2-A + 4 保留 2-B + 2 保留 2-C + 2 完成 2-D + 1 完成 2-E + 11 待推进）；§6.7 策略实现列表补 `controller`；§6.3 新增 display Framebuffer 拆分登记项。
- framework `driver/display` 文件数 5→4（controller 移出）；TCB 侧 unsafe 不变（controller 本为 0 unsafe，9 处 unsafe 仍在 `mod.rs`）。

**验证**：§2.3 六门槛全绿——`./ci/build.sh all` 5/5（双架构 0 error/0 warning）+ `./ci/audit.sh quick`（clippy pedantic 0、核心审计含 FP-06 全过）+ `make test-host`（8+11 passed）+ `make test-kernel-host`（826 passed / 0 failed）+ QEMU x86_64 boot 至 `VFS ready` + KPTI 断言。

**状态**: [X]（2-E 收口；controller 管理策略落 services，VBE 原语/framebuffer/font 保留框架；Framebuffer 机制/策略拆分登记 §6.3 后续）

### DECISION-T: 2-F 复核（credo/storage + net query 保留 framework；credo/storage 登记后续）

> **背景**：§6.2 批次 2-F 目标为 `framework/credo/storage.rs`（433 行）与 `framework/net/init/query.rs`（162 行）。按 §6.2 复核纪律（DECISION-F）先查服务对象，再定归属。

**长期演进分析（内核终局视角）**：

1. **net/query — 终局保留 framework（非权宜）**：`is_network_initialized` / `is_network_configured` / `get_init_state` / `NetStatus::capture` / `get_mac_address` / `get_ipv4_address` / `get_default_gateway` / `get_dns_servers` 本质是 framework net TCB 状态（`G_INIT_STATE` / `G_MAC` / `G_IPV4` / `G_GATEWAY` / `G_DNS`，由 DHCP 状态机在 framework 内写入的全局 Atomic）的**只读访问面**。框内核范式下「状态的只读访问器应与状态定义同层」——若迁 services，则 services 须穿透读 framework 内部字段（违 F2 边界精神）。先例：`framework/net_socket.rs::reset_network_state` 即 framework 已向 services 提供的 safe 访问面。另 `net/api.rs`（对外契约面）与 `net/init/sm_fi.rs`（启动编排）均直接调用查询 → 属 framework 机制自身需求。
2. **credo/storage — 本轮保留 + 登记后续（终局应下沉，受前置阻塞）**：文件三部分组成——① 序列化算法（`w8/w16/w32/w64`/`r8…`/`serialize`/`deserialize` + v4→v5 迁移，0 unsafe 纯函数 = 策略）；② VFS I/O（`raw` 子模块 5 个 `unsafe extern "C"` 包装 `vfs_*_internal` = 机制）；③ 编排（`save/load/remove_database` = 功能）。按归属决策树，② 属机制留 framework，①③ 终局应迁 services。**但前置缺失**：framework 当前无 VFS safe API 面（属阶段 4 VFS 下沉），强行只迁 ① 需新增仅为过渡存在的 trait 注入，违 §12.3「不为将来预留扩展点」；且 `credo/api.rs` 三处 FFI（`pwm_try_load` / `pwm_save_to_disk` / `pwm_load_from_disk`）直接调 `storage::*`（安全导出面绑定），序列化又直读 credo TCB `PwmEntry` 全部原子字段——在 VFS safe 面就绪前整体迁出将造成 framework→services 反向依赖或大面积 TCB 字段穿透。

**裁决（用户）**：采纳「**保留 + 登记**」——2-F 本轮**零代码下沉**；`net/init/query.rs` 标 🔒（终局保留 framework）；`credo/storage.rs` 标 🔄（本轮保留）+ 登记 §6.3 后续条目（前置＝阶段 4 VFS safe API 就绪 → 编排+序列化整体下沉 `services/credo/persist`）。

**订正**：
1. §6.2 表 `framework/credo/storage.rs` 原目标路径 `services/credo/storage` **已被块设备代理占用**（`services/credo/storage/`：`disk.rs`+`mod.rs`，语义为块设备/格式化/分区），订正为 `services/credo/persist`。
2. §6.2 表 `framework/net/init/query.rs` 原依据「查询纯 Atomic；reset 薄层留」订正为「TCB 状态只读访问面，终局保留 framework」。

**影响面**：§6.2 计数 23 → 3 完成(2-A) + 4 保留(2-B) + 2 保留(2-C) + 2 完成(2-D) + 1 完成(2-E) + 2 保留(2-F) + 9 待推进；§6.3 新增 credo/storage 后续登记条目；framework 侧文件数不变（零下沉）；TCB 占比不变。

**状态**: [X]（2-F 复核收口；credo/storage + net query 均保留 framework，2-F 无代码下沉；credo/storage 登记后续条目；转推 2-G firmware·ftrace）。**后续更新**：登记项 2-F 的 credo/storage 已由**阶段 4b 全量下沉收口**——`framework/credo/storage.rs` 删除，序列化 + `save/load/remove_database` 编排整体迁 `services/credo/persist`，framework 侧无 `vfs_*_internal` C FFI 薄层残留（见 §7 阶段 4b 实施记录）；`net/init/query.rs` 终局保留 framework 判定不变。

### DECISION-U: 2-G firmware·ftrace 下沉 services（契约措辞细化 + 11+ unsafe 消除）

> **背景**：§6.2 批次 2-G 目标为 `framework/syscall/firmware.rs`（固件加载/查询/分离）与 `framework/syscall/ftrace_kgdb.rs`（ftrace 使能/读取/统计 + KGDB 进入）。按 §6.2 复核纪律（DECISION-F）先查服务对象（§2），再按「机制资源 vs 处理策略」二分定归属。

**长期演进分析（内核终局视角）**：

1. **两文件均为"处理策略"，非"机制资源"**——内容全部是"参数校验 + 用户指针拷贝 + 编排调用 framework 机制 + 错误码组装"，不直接触碰页表/中断/上下文切换/硬件原语。固件 blob 的权威存储与元数据管理在 `framework/chitin`（机制），各类机制原语（`vfs_open_safe`/`vfs_read_safe`/`copy_from_user`/`copy_to_user`/`ftrace_*`/`kgdb_*`）均已由 framework 提供 safe API 面。按 §4.1 归属决策树，机制留 framework、处理策略迁 services（0 unsafe），符合 B09-18 契约（§6.2 头注：`QX_*` 按机制资源 / 处理策略二分归属）。

2. **消除 11+ 处 unsafe 与跨层调用**——原 framework 侧 `firmware.rs`/`ftrace_kgdb.rs` 集中大量 `unsafe` 用户指针拷贝；下沉后改为 `copy_from_user`/`copy_to_user`（framework 安全代理，内部含 `is_user_buf` 校验 + 异常表兜底 + SMAP），services 侧达成 100% safe（F1）。同时 `services/debug/mod.rs` 原先反向调用 `framework::syscall::ftrace_kgdb::sys_ftrace_*`（跨层调 framework 内部 syscall 实现），改指向 `framework::debug` 机制原语，消除跨层耦合。

3. **契约措辞需细化**（B09-18 活契约，`framework/syscall/mod.rs` 头注）——原措辞"QX 独有编号留 framework 回退层"过于笼统，导致 `QX_*` 一律滞留 framework；细化为"框架持有的机制资源（页表/中断/上下文/硬件原语）留 framework 回退层；仅做参数校验 + 编排的处理策略（可 0-unsafe 且不直接调用 framework 内部机制）迁 services"，为后续 `QX_*` 归属提供明确判据。

4. **对齐问题解法**——保持"无对齐假设"：用户结构写入采用 `[u8; N]`（align 1）+ `copy_to_user`，字段用 `to_ne_bytes()` 逐字段序列化（先例 `services/fs/io.rs`、`services/fs/file_handle.rs`），避免依赖用户缓冲对齐。

**裁决（用户）**：采纳「**下沉 services + 细化契约**」——按 §6.2 处方把 firmware/ftrace 处理策略下沉 `services/syscall`，消除 11+ 处 unsafe 与跨层调用；同时把 B09-18 措辞细化为「机制资源留 framework / 处理策略可 safe 化则迁 services」。

**方案**：

1. **新建** `services/syscall/firmware.rs`（0 unsafe）：`sys_fw_load`/`sys_fw_get`/`sys_fw_get_info`/`sys_fw_detach`；用户指针经 `super::check_user_ptr`/`check_user_buf` 校验 + `copy_from_user`/`copy_to_user`；`FirmwareInfo` 经 `fw_info_bytes` 逐字段 `to_ne_bytes` 序列化（附单测）；私有 `read_path_data` 经 `vfs_open_safe`/`vfs_read_safe` 读取（上限 `MAX_FIRMWARE_SIZE`）。
2. **新建** `services/syscall/ftrace.rs`（0 unsafe）：`sys_ftrace_enable`/`disable`/`read`/`stat` + `sys_kgdb_enter`；`TraceEvent`（48 字节，6×`u64`）/统计（16 字节）经 `trace_event_bytes`/`ftrace_stat_bytes` 逐字段序列化（附单测）；KGDB 经 `framework::debug::kgdb_serial_ready`/`kgdb_breakpoint` 机制原语。
3. **services 接入**：`services/syscall/mod.rs` 挂 `pub mod firmware;` + `pub mod ftrace;`；`services/syscall/dispatch.rs` `dispatch_other` 接入 9 个 `QX_*`（`QX_FW_LOAD/GET/GET_INFO/DETACH`、`QX_FTRACE_ENABLE/DISABLE/READ/STAT`、`QX_KGDB_ENTER`）+ 头注"已迁移/待迁移"列表更新。
4. **services/debug 去跨层**：`ftrace_enable`/`ftrace_disable`/`kgdb_enter` 改指 `framework::debug` 机制原语（原指 `framework::syscall::ftrace_kgdb::sys_ftrace_*`）；头注与安全契约同步。
5. **framework 侧清理**：`framework/syscall/mod.rs` 删 `pub mod firmware;`/`pub mod ftrace_kgdb;`；`framework/syscall/dispatch.rs` 删 9 个回退分支 + import 去对应 `QX_*`；因函数体降至阈值内，删 `#[expect(clippy::too_many_lines)]`；删除源文件 `firmware.rs`/`ftrace_kgdb.rs`；`scripts/audit_coupling.py` 删悬空黑名单条目 `framework::syscall::firmware`/`ftrace_kgdb`。
6. **契约细化**：`framework/syscall/mod.rs` L29-40 头注 B09-18 措辞细化（机制资源 / 处理策略二分）。
7. `services/mod.rs` 悬空注释同步（去掉 `framework::syscall::ftrace_kgdb` 引用）。

**影响面**：

- §6.2 计数 23 → 3 完成(2-A) + 4 保留(2-B) + 2 保留(2-C) + 2 完成(2-D) + 1 完成(2-E) + 2 保留(2-F) + 2 完成(2-G) + 7 待推进。
- framework 侧 `syscall` 文件数 -2（`firmware.rs`/`ftrace_kgdb.rs` 删除）；TCB 侧 unsafe 下降（11+ 处用户指针拷贝迁出），services 侧新增 2 文件均 0 unsafe。
- 编号权威仍在 `framework/syscall/types.rs`（`QX_FW_*` = 730-733、`QX_FTRACE_*` = 800-803、`QX_KGDB_ENTER` = 804，本批次不动）。
- 预存项不动：`framework/syscall/api.rs` L40-46 `QX_*` re-export 为既有孤儿（全代码库无 `api::QX_` 用户），按 §12.2 不属本批次，未处理。

**验证**：§2.3 六门槛全绿——`./ci/build.sh all` 5/5（双架构 0 error/0 warning）+ `./ci/audit.sh quick`（clippy pedantic 三维 0、核心审计含 `audit_coupling`/`audit_services_boundary`/FP-06 全过）+ `make test-host`（全绿）+ `make test-kernel-host`（831 passed / 0 failed，含新增 5 单测）+ QEMU x86_64 boot 至 `VFS ready` + KPTI 断言。

**状态**: [X]（2-G 收口；firmware·ftrace 处理策略落 services（0 unsafe），framework 侧源文件删除，B09-18 契约措辞细化；转推 §6.2 剩余 7 文件）

### DECISION-V: 2-H info·wait4 + 2-J coredump 下沉 services；2-K rlimit/e1000/e1000_io/virtio 复核保留（§6.2 全表收口）

> **背景**：§6.2 末批 7 文件（coredump/rlimit/info/wait4/e1000/e1000_io/virtio）。按 §6.2 复核纪律（DECISION-F）先查服务对象（§2），再按「机制资源 vs 处理策略」二分定归属（B09-18 契约，判据见 DECISION-U）。分两批实施：2-H（info·wait4）+ 2-J（coredump）下沉，2-K（rlimit/e1000/e1000_io/virtio）复核保留。

**长期演进分析（内核终局视角）**：

1. **info·wait4 为"处理策略"，coredump 为"处理策略 + 编排"**——三者内容均为"参数校验 + 用户指针拷贝 + 编排调用 framework 机制 + 错误码/序列化"，不直接触碰页表/中断/上下文切换/硬件原语；所需机制原语（`vfs_*_safe`/`copy_from_user`/`copy_to_user`/用户指针写 safe 代理/VMA 快照/中断帧快照）均已有 framework safe API 面。按 §4.1 归属决策树，机制留 framework、处理策略迁 services（0 unsafe）。

2. **coredump 的核心难点是"框架 signal 回调需触发 services 编排"，用 trait 注入解耦**——`framework/proc/signal.rs` 在信号致死路径需调用 coredump 写出，但 coredump 编排属策略应在 services。解法延续本工程既有的 signal_policy 范式：framework 定义 `CoredumpSink` 契约（`framework/proc/coredump_trait.rs`）+ `OnceLock<&'static dyn CoredumpSink>` + `register_coredump_sink`/`current_coredump_sink`（含 Fallback）；`signal.rs` 改为 `current_coredump_sink().coredump(pid, sig, frame_addr)`；services 侧 `StandardCoredumpSink` 于 `proc::init` 注册。中断帧寄存器经 `read_interrupt_regs(frame_addr)->Option<RegSnapshot>`（POD `[u64;N]` 快照）、VMA 枚举经 `vma_snapshot_current()->Vec<VmaInfo>`——框架提供 POD 快照 API，services 侧 0 unsafe 完成 ELF Core 构建与写出。此设计满足 I1-I6（内核态寄存器/内存均经框架安全代理导出，services 无法篡改内核状态）。

3. **coredump 下沉一并修正 3 处预存缺陷**（属本轮改动直接触及路径，按 §9.3 本轮修复）——P0-17：`collect_segments` 原用当前 mm 而非目标 pid 的 mm（跨进程 core 收集错误）；P0-20：`core_limit` 未实质截断（写出超过 RLIMIT_CORE 上限）；note name 读取越界（OOB 读）。修正后以 guard 拒绝非当前进程请求（当前仅支持当前进程 core，属 SIMPLIFIED 边界，附 `// SIMPLIFIED:` 注释标注扩展点）。

4. **rlimit/e1000/e1000_io/virtio 复核保留**——①**rlimit**：`RlimitTable` 是 `framework/proc/process.rs::rlimit_table` 的 `Process` 机制字段，`RLIMIT_*` 常量与访问器由 framework proc TCB + syscall 契约面直接持有/读写，属「状态定义与其访问面同层」，services 侧 `services/proc/rlimit.rs` 仅为纯 re-export 壳。②**e1000/e1000_io**：E1000 驱动业务 + `dma_ring.rs` DMA 环机制 + `e1000_io.rs` MMIO 访问器整体位于 framework（B04-AUDIT-005 #4 v2 整体上移后未回迁），`Driver` 经 CharOps/NetOps FFI 桥绑定 `driver_data` 裸指针不可 safe 化，`E1000Io` 为 `IoMem` MMIO 封装的安全导出面供 framework 驱动消费；services 侧 `services/driver/net/e1000.rs` 仅保留描述符语义常量 + re-export 壳。DECISION-B 原「业务回迁 services」经复核不落地（维持现状）。③**virtio**：virtio transport（`mod.rs` + `queue.rs`）为设备发现/队列机制，与 framework driver/chitin 编排直接耦合，下沉将制造 framework→services 反向依赖，经用户裁定保留 framework。

**裁决（用户）**：采纳——2-H（info·wait4）与 2-J（coredump）下沉 services（0 unsafe），一并修正 coredump 预存 P0-17/P0-20 + OOB 读；2-K（rlimit/e1000/e1000_io/virtio）复核保留 framework。§6.2 全表收口。

**方案**：

1. **2-H 下沉**：`framework/syscall/info.rs`（`sys_getpid/gettid/getppid/getpgid/uname`）→ `services/proc/info.rs`（0 unsafe，用户指针写改 `framework::syscall::api::write_struct_to_user`，主机名/域名源 `framework::proc::namespace::uts_current`）；`framework/syscall/wait4.rs`（`sys_wait4/waitid` + `wait_reap`/`WaitOutcome` helper）→ `services/proc/wait4.rs`（0 unsafe，2 处用户指针写改 framework safe API）。framework 侧源文件删除 + `mod.rs` 声明 + `dispatch` 分支 + `audit_coupling.py` 悬空黑名单清理。
2. **2-J 前置 safe API**：framework 侧补 `framework/proc/coredump_trait.rs`（`CoredumpSink` + 注册/取用 + `RegSnapshot` + `read_interrupt_regs`，x86_64 len=27 / aarch64 len=34）+ re-export；`framework/mm` re-export `VmaInfo`/`vma_snapshot_current`/`copy_from_user_in_mm`；`framework/fs/vfs/handle.rs` 补 `vfs_write_pod<T: Copy>`；`Elf64Header`/`Elf64Phdr` 补 `#[derive(Clone, Copy)]`（满足 `vfs_write_pod` 约束）。
3. **2-J 下沉**：`framework/proc/coredump.rs` → `services/proc/coredump.rs`（0 unsafe 完整实装，612 行 + `#[cfg(test)]` 6 测试；ELF Core 构建经 `vfs_open_safe`/`vfs_write_pod`/`vfs_write_safe`/`vfs_close_safe`；signal→coredump 经 trait 注入），framework 源文件删除 + `pub mod coredump` 移除 + 审计悬空条目清理；`services/proc/mod.rs::init` 追加 `register_standard_coredump_sink()`。
4. **2-K 保留**：rlimit/e1000/e1000_io/virtio 维持 framework，services 侧维持既有 re-export 壳；仅同步 §6.2 表依据列 + 计数，无代码下沉。

**影响面**：

- §6.2 计数 23 → **11 完成下沉**（2-A×3 clone/io/sendfile + 2-D×2 usb + 2-E×1 display + 2-G×2 firmware/ftrace + 2-H×2 info/wait4 + 2-J×1 coredump）+ **11 复核保留**（2-B×4 epoll/eventfd/signalfd/timerfd + 2-C×2 pl011/keyboard + 2-F×1 net query + 2-K×4 rlimit/e1000/e1000_io/virtio）+ **1 复核保留并登记后续**（2-F credo/storage，**已由阶段 4b 全量下沉收口**）。**§6.2 全表收口**。
- framework 侧 `syscall` 文件数 -2（info/wait4 删除）、`proc` 文件数 -1（coredump 删除）；services 侧新增/扩建 3 文件均 0 unsafe；framework 侧 unsafe 块下降（coredump 用户指针/内存访问迁出），TCB 侧新增机制原语（`coredump_trait` 注册面、`read_interrupt_regs`、`vma_snapshot_current`、`vfs_write_pod`）均为 safe 导出面。
- 预存缺陷修正：P0-17（mm 选择）+ P0-20（core_limit 截断）+ note name OOB 读（属本轮改动直接触及路径）。
- 编号权威仍在 framework（`QX_*` = `framework/syscall/types.rs`，本批次不动）。

**验证**：§2.3 六门槛全绿——`./ci/build.sh all`（双架构 0 error/0 warning）+ `./ci/build.sh aarch64` + `./ci/audit.sh quick`（clippy pedantic 三维 0、6 安全不变式 PASS、SAFETY 100% 覆盖、FP-06 PASS、注释语言 TD-22 PASS）+ `make test-host`（全绿）+ `make test-kernel-host`（848 passed / 0 failed，含新增 coredump 6 测试）+ QEMU x86_64 boot 至 `VFS ready` + KPTI 断言 PASS。

**状态**: [X]（2-H/2-J 下沉 + 2-K 复核保留收口；§6.2 全表 23 文件闭环）

### DECISION-W: 阶段 4 VFS 下沉终局 = Asterinas 对齐·完整下沉（4a/4b/4c 子批）

> **背景**：阶段 4 原表述（§8）为「VFS 4 文件下沉 + backend_trait 扩展」（DECISION-A：`vfs.rs`/`dcache.rs`/`types.rs`/`open_file_table.rs`）。调研暴露两处口径张力：①§6.6 L209 将 `handle`/`mount`/`path` 列入「fs 契约（6）」保留清单，与 DECISION-A「VFS 下沉」方向矛盾；②`types.rs` 被 handle/mount/path/dcache/vfs/flock/inotify 深度消费，**无法单独下沉**（下沉即断裂 framework 内全部引用，且 framework 引用 services 类型违反依赖单向）——即「4 文件下沉」与「handle/mount/path 保留」字面不可共存。经用户裁定终局形态。

**长期演进分析（内核终局视角）**：

1. **Asterinas 参照系**：Asterinas 的 OSTD（=framework）不含任何文件系统抽象——只提供通用原语（UserPtr 安全读写 / 同步 / 内存 / interrupt / 锁），VFS + 各 FS + syscall 处理器全部位于 kernel（=services，`kernel/core/src/fs/vfs`）。
2. **两种终局对比**：(a) 保守路线——仅下沉 `types/dcache/vfs/open_file_table` 四文件，handle/mount/path 因「userptr + FFI 机制」留 framework：TCB 削减有限，且与「types 无法单独下沉」冲突，实际须把 handle/mount/path 一并卷入；(b) **Asterinas 对齐·完整下沉**——framework 只留通用原语 + 契约 trait + ABI 边界，handle/mount/path 的 syscall 处理器与逻辑全部下沉 services（userptr 交 framework 通用 safe API）。裁定取 (b)：最贴 Asterinas、TCB 削减最大，且消解 §6.6 口径矛盾。
3. **契约面必须留 framework（架构必需，非权宜）**：`backend_trait` / `inode` / `vfs_poll_trait` 是「services 向 framework 注册的契约」（framework 持 `OnceLock<&'static dyn Xxx>` + `register_`/`current_` + Fallback），是依赖方向单向的**必要条件**。据此 §6.6 L209 的「fs 契约（6）」终局收窄为 **2 契约**（`VfsOps` + `vfs_poll_trait`；DECISION-W 4a 阶段曾列 3，阶段 4b 实测 `FsBackend`/`Inode` 随具象类型一并下沉）；handle/mount/path 的 userptr 机制收敛为 framework 通用 safe API。
4. **契约 trait 只含 POD/framework 类型**：VFS 契约的方法签名不得引用 services 具象类型（如 `OpenFile`），否则 framework 契约反向依赖 services。故契约仅暴露 `u32/u64/usize/bool/&mut [u8]/Option<POD>` 等。
5. **入口拆解参照阶段 1/2 已落地模式**：brk/clone/io/sendfile/firmware/info/wait4/coredump 等已借 `SyscallDispatch` dispatch_trait 或 framework safe API 下沉 services（0 unsafe），VFS 入口同理。
6. **风险控制**：13 文件 + 71 userptr unsafe 一次性迁移风险高，按「契约先行 → 实现下沉 → 入口收敛」三批推进，每批独立跑 §2.3 门槛全绿。

**裁决（用户）**：采纳 **Asterinas 对齐·完整下沉**。长期 framework 只留通用原语 + 契约 trait + ABI 边界；handle/mount/path 的 syscall 处理器与逻辑全部下沉 services（userptr 交 framework 通用 safe API）。**覆盖 §6.6 L209 中 handle/mount/path 的「保留」判定**。首个子批 = 4a（契约先行，公共前置，树绿）。

**子批拆分**：

- **4a 契约先行（公共前置，不改文件归属，树绿）**：framework 新建 `VfsOps` 契约（`framework/fs/vfs/ops_trait.rs`：`pub trait VfsOps` + `OnceLock<&'static dyn VfsOps>` + `register_vfs_ops`/`current_vfs_ops` + `FallbackVfsOps`），方法集从 framework 保留机制的消费面客观推导（见下表）；Fallback 转发现有 `framework/fs` 函数；reroute 消费面（proc_ops/epoll/page_fault）。**4a 不改任何 TCB 归属、不改语义**，仅新增契约面 + 间接层。
- **4b 实现下沉**：`framework/fs/vfs` 的 VFS 实现整体迁 `services/fs`（含 `vfs.rs`/`dcache.rs`/`types.rs`/`open_file_table.rs` + `handle`/`mount`/`path`/`flock`/`inotify`）；services 于 `fs::init` 注册 `VfsOps` 实现替换 Fallback；services 内部引用重定向（mm/fs/wasm/proc 等消费者改路径）；framework 侧仅留 2 契约（`VfsOps` + `vfs_poll_trait`）+ POD `VfsFileType` + userptr 机制面。**前置**：4a 完成 + 4b 前对 asm/链接依赖 `#[no_mangle]` `vfs_*` 符号的调研（实测无 asm/链接脚本消费者，63 处壳删除）。
- **4c 边界收敛**：ramfs/devfs/initramfs 等 framework 内 fs 消费者的路径重定向；§6.6 fs 项收窄（6→2 契约，随 4b 实测确定终局 2 契约）收口；全量验证。

**4a 契约草案（VfsOps 方法集，从 3 处消费面 6 调用客观推导）**：

| 消费点 | 现有调用 | VfsOps 方法 |
|---|---|---|
| proc_ops.rs:407-408 | `flock_release_pid(pid)` + `posix_lock_release_pid(pid)` | `release_pid_locks(&self, pid: u32)` |
| proc_ops.rs:413 | `vfs_close_all_fds()` | `close_all_fds(&self)` |
| proc_ops.rs:686 | `vfs_close_cloexec_fds()` | `close_cloexec_fds(&self)` |
| proc_ops.rs:1046 | `OPEN_FILE_TABLE.inc_ref(hid)` | `inc_open_file_ref(&self, handle_id: u32)` |
| epoll.rs:505-508 | `vfs_get_fd_handle(fd)` + `OPEN_FILE_TABLE.with_file(hid, \|of\| of.file_type)` | `fd_file_type(&self, fd: i32) -> Option<u8>` |
| page_fault.rs:330 | `vfs_pread_inode(mount_idx, node_id, off, dst, pwm)` | `pread_inode(&self, mount_idx: Option<usize>, inode_id: u32, offset: u64, dst: &mut [u8], pwm: u64) -> i32` |

> 说明：`epoll.rs` 的 `(valid, file_type)` 二元组收敛为 `Option<u8>`（`None`→无效 fd，`Some(ft)`→有效；调用方 `VfsFileType::from_u8` 处理不变）。`vfs::init()`（lib.rs:785）为 boot 编排（crate root 属合法双向调用者），4a 暂不改，随 4b 重定向。proc/api.rs 的 `vfs_stat/open/read/close`（`user_proc_load_elf`）属 syscall 边界，随 4b/4c 迁移，4a 不动。

**影响面**：4a framework 侧新增 1 契约文件（safe，0 unsafe）+ reroute 3 文件 6 处调用；不改 TCB 归属、不改运行时语义；services 侧 4a 不变（4b 才注册实现）。

**4a 实施记录**：新建 [framework/fs/vfs/ops_trait.rs](../../src/kernel/framework/fs/vfs/ops_trait.rs)（`VfsOps` + `FallbackVfsOps` + `register_vfs_ops`/`current_vfs_ops`，照抄 `backend_trait.rs` 范式；Fallback 转发现有 `framework/fs` 函数）；`vfs/mod.rs` 声明并顶层 re-export；reroute 消费面：`proc_ops.rs` 3 处（`release_pid_locks`/`close_all_fds`/`close_cloexec_fds`/`inc_open_file_ref`）→ 4 处契约调用、`epoll.rs` 1 处（`fd_file_type`）、`page_fault.rs` 1 处（`pread_inode`）。伴随修正 `host-tests/tests/fd_cloexec_test.rs` 静态契约扫描（`vfs_close_cloexec_fds()` → `close_cloexec_fds()`，随契约路由更新断言）。**§2.3 六门槛全绿**：双架构 0w0e ✅ / clippy pedantic（含 kernel_test + host-test 维）✅ / 核心审计（6 不变式 + F1-F9）✅ / host-tests ✅ / kernel-host 848 passed ✅ / QEMU x86_64 完整启动 ✅。

**状态**: [X]（4a 契约先行 + 4b 实现下沉 + 4c 边界收敛均完成并过 §2.3 六门槛，见阶段 4b / 4c 实施记录）

### 阶段 4b 实施计划（VFS 完整下沉 · 无壳单批）

描述：按 DECISION-W + 用户裁定（逆转 DECISION-H13/DECISION-A；D2=A 完整下沉；D1=B 下沉编排；无壳单批做尽），把 `framework/fs` 的 VFS 实现整体下沉 `services/fs`。终局 framework fs 面仅剩 2 契约（`VfsOps` + `vfs_poll_trait`）+ POD `VfsFileType`；`ramfs`/`devfs`/`initramfs`/`nestfs` 与 `FileSystem`/`Inode`/`FsBackend` 具象类型及契约一并下沉。**登记：DECISION-W 覆盖 DECISION-H13（2026-08-31）/DECISION-A（「VFS 4 文件」口径）；§6.6 L209「终局 3 契约」修正为「终局 2 契约」。**

方案：

**（1）文件级迁移映射（framework/fs 合计 9702 行 → services/fs）**

| framework 源 | 行 | services 落点 | 处置 |
|---|---|---|---|
| `fs/vfs/vfs.rs` | 545 | `services/fs/vfs_manager.rs`（替换壳） | VFS 管理器实现整体迁入 |
| `fs/vfs/types.rs` | 717 | `services/fs/vfs_types.rs`（替换壳）+ `fs/vfs/types_pod.rs`（framework 新建） | POD `VfsFileType` 抽留 framework；`VfsStat`/`VfsDirEntry`/`VfsOpenFlags`/`VfsSeekWhence`/`FsType`/`FsOpenResult`/`FileSystem`/`OpenFile`/`KernelError` re-export 迁 services |
| `fs/vfs/dcache.rs` | 971 | `services/fs/dcache.rs`（替换壳） | 迁入 |
| `fs/vfs/open_file_table.rs` | 89 | `services/fs/open_file_table.rs`（替换壳） | 迁入 |
| `fs/vfs/flock.rs` | 727 | `services/fs/flock.rs`（替换壳） | 迁入 |
| `fs/vfs/inotify.rs` | 633 | `services/fs/inotify.rs`（替换壳） | 迁入 |
| `fs/vfs/inode.rs` | 174 | `services/fs/inode.rs`（合并） | `Inode` trait 定义并入既有 services 具象实现（`RamFsInode`/`AnonymousInode`/`LegacyInode`） |
| `fs/vfs/backend_trait.rs` | 159 | `services/fs/backend_trait.rs`（新建） | `FsBackend` 契约/注册表随具象 FS 下沉（framework 无残留消费者） |
| `fs/vfs/handle.rs` | 858 | `services/fs/handle.rs`（新建） | fd 句柄 VFS 实现（open/close/read/write/seek/dup/fstat/fchmod/...） |
| `fs/vfs/path.rs` | 722 | `services/fs/path.rs`（合并） | 路径/目录/链接/元数据/cwd 实现并入既有 services 处理器 |
| `fs/vfs/mount.rs` | 271 | `services/fs/mount.rs`（合并） | 挂载/生命周期/同步/格式化实现并入 |
| `fs/vfs/api.rs` | 104 | 拆解（`Vfs` trait 删；`ptr_to_str`/`split_parent_name`/`with_cstr`/`PCACHE_*` → `services/fs/api.rs` 新建） | `Vfs` trait = 声明性 dead code（F9）删除 |
| `fs/ramfs/{mod,ramfs_data,ramfs_node}.rs` | 2150 | `services/fs/ramfs.rs`（合并） | 框架 RamFS 机制整体迁 services |
| `fs/devfs/mod.rs` | 873 | `services/fs/devfs.rs`（替换壳） | 迁入 |
| `fs/initramfs.rs` | 336 | `services/fs/initramfs.rs`（新建） | unpack 入口随迁 |
| `fs/nestfs/{mod,arc_safe}.rs` | 34 | `services/fs/nestfs/`（合并）+ framework 保留 `nestfs/arc_safe.rs` | 具象 NestFS（29 文件）迁入 services；`arc_safe.rs`（ARC 裸指针→切片 safe 封装，框架层必要 unsafe）留 framework，services `arc.rs` 反向依赖（services→framework 合法方向）；framework `nestfs/mod.rs` 仅 `pub mod arc_safe;` |
| `fs/vfs/ops_trait.rs` | 114 | **framework 保留** | 4a 契约 |
| `fs/vfs_poll_trait.rs` | 153 | **framework 保留** | `epoll.rs` 消费 |
| `fs/vfs/types_pod.rs`（新） | — | **framework 保留** | 仅 `VfsFileType` |

**（2）framework POD 抽取**：framework 新建 `fs/vfs/types_pod.rs`，仅含 `VfsFileType`（`epoll.rs:502` 与 `vfs_poll_trait.rs:28` 消费）+ 必要 `impl`。`framework/fs/mod.rs` 收敛为 `pub mod nestfs; pub mod vfs; pub mod vfs_poll_trait;` + `pub use vfs::*`（`nestfs` 仅留 `arc_safe` 机制适配层）；`fs/vfs/mod.rs` 收敛为 `pub mod ops_trait; pub mod types_pod;` + 顶层 re-export。**删除 `pub use initramfs::unpack` 与 `pub mod {devfs,initramfs,ramfs}`（`nestfs` 仅保留 `arc_safe`）。**

**（3）真 unsafe 块消除策略**（实测 framework/fs 真 unsafe 块 ~18-20 处；`unsafe|no_mangle` 合计 89 处 / 8 文件）：迁移函数保持裸指针入参（入参本身无需 unsafe），仅把**解引用**替换为 framework 安全通道：
- 裸 C 字符串 → `CStrExt::as_kstr`（safe）/`as_kstr_opt`
- 用户缓冲区读写 → `UserReadPtr/UserWritePtr::checked_new`（safe，返回 `Option`）+ safe 读写方法
- 用户结构体 → 现有 `write_struct_to_user`/`read_struct_from_user`（safe）
- `vfs_write_pod` 的 `from_raw_parts(ptr::from_ref(val).cast(), size)` → framework 新增 safe 辅助 `mm::pod_as_bytes<T: Copy>(&T) -> &[u8]`（若缺失）
- `nestfs/arc_safe.rs` 的 `from_raw_parts`、`inotify.rs` 的 `ptr::write`、`mount.rs`/`devfs` 的 `&*(&STATIC as *const T)` → 经 framework safe 封装或直接 `&STATIC`
- `#[unsafe(no_mangle)] vfs_*` 壳（path 27/handle 22/mount 14 = 63 处）**删除**，services 内调用方由 `fw::vfs_*` 改本地 `vfs_*`。依据：无 asm/链接脚本消费者，唯一非 Rust 消费者 `credo/storage.rs`（Rust，reroute）。services 层禁 `#[unsafe(no_mangle)]`（unsafe attribute，F1）。

**（4）framework 内消费者处置（D1=B 下沉编排）**

| 消费者 | 现状 | 处置 |
|---|---|---|
| `proc/api.rs::user_proc_load_elf` | `fs::vfs_stat/open/read/close` 编排 | ELF 文件 I/O 下沉 services（services 调 framework `load_elf_from_memory` 纯机制），framework 仅留 `user_proc_load_elf_from_memory` |
| `proc/api.rs::launch_first_user_process` | `fs::vfs_mount(ramfs)` + `fs::unpack(initramfs)` | boot mount/unpack 编排下沉 services |
| `credo/storage.rs` | VFS 文件 I/O 持久化 | 经 VfsOps 契约或 framework safe API，编排下沉 services |
| `chitin/{composite,mod}.rs` | `fs::KernelError` | 重指向 `framework::error::KernelError` |
| `driver/block.rs` | `fs::{KernelError,KernelResult}` | 重指向 `framework::error` |
| `lib/cstr.rs:58` | 文档链接 `framework::fs::VFS_MAX_PATH` | 重指向 services 常量（或删除链接） |
| `syscall/epoll.rs` | `VfsFileType` + `current_vfs_ops` + `vfs_poll_trait` | 不变（保留 framework） |
| `proc/proc_ops.rs`、`mm/page_fault.rs` | 已 4a 契约化 | 不变 |
| `lib.rs:784-785` | `fs::vfs::init()` boot 编排 | 改指 services |

**（5）VfsOps 契约扩容复核**：4a 6 方法已覆盖 framework 残留消费面（proc_ops/page_fault/epoll）；`proc/api.rs` 编排下沉后 framework 无新增 VFS 需求 → **4b 实测为空操作**（迁移未暴露缺口，契约保持 6 方法无扩容）。

**（6）services 壳替换映射**：`vfs_manager`/`vfs_types`/`dcache`/`open_file_table`/`flock`/`inotify`/`devfs` 7 壳由 `pub use framework::...` 替换为真实现；新增 `handle`/`backend_trait`/`initramfs`/`api`；合并 `inode`/`mount`/`path`/`ramfs`/`nestfs`。

**（7）消费面 reroute**：services 侧 `crate::framework::fs::vfs::{...}` 71 处 / 22 文件 + `crate::framework::fs::{ramfs,devfs,initramfs,KernelError,...}` 改指本地 services 路径（机械替换）；`framework::fs::api as fw` 9 处改本地。全仓 312 处 / 97 文件（含 host-tests）复核。

**（8）中间态保持树绿策略**：按「framework 新建 POD/收敛 → services 侧新增模块与实现（framework 原文件暂存）→ services 消费面切本地 → framework 原文件删除 → framework 内消费者 D1=B 下沉 → 全仓 reroute → 测试迁移」顺序推进，每阶段 `cargo check` 保证可编译，末段跑 §2.3 六门槛。**删除 89 处 `no_mangle`/framework 文件须在同一批次内原子完成**（否则符号消失导致链接断裂）。

**（9）测试迁移**：`framework/tests/test_vfs.rs`、`test_devfs.rs` 迁 `host-tests/` 或随实现落 services 内联 `#[cfg(test)]`；`tests/mod.rs:515` `register_initramfs_tests()` 处置；`host-tests/tests/fs_sync_trait_test.rs`、`plan_b_inode_test.rs` 源码扫描断言随路径更新。

**验证**：§2.3 六门槛全绿（双架构 0w0e / clippy pedantic / 核心审计 / host-tests / kernel-host / QEMU）。

**4b 实施记录**：`framework/fs/vfs` 实现整体下沉 `services/fs`（`vfs_manager`/`vfs_types`/`dcache`/`open_file_table` 以真实现替换壳 + 新增 `handle`/`backend_trait`/`initramfs`/`api` + 合并 `inode`/`mount`/`path`/`ramfs`/`devfs`/`nestfs`）；**63 处 `#[unsafe(no_mangle)] vfs_*` 壳删除**（无 asm/链接脚本消费者，唯一非 Rust 消费者 `credo/storage.rs` 连锁下沉）。framework/fs 收敛为 4 文件 + 头注：`vfs/ops_trait.rs`（`VfsOps` 契约）+ `vfs_poll_trait.rs`（`epoll.rs` 消费）+ `vfs/types_pod.rs`（POD `VfsFileType`）+ `nestfs/arc_safe.rs`（ARC 裸指针→切片 safe 封装，framework 必要性 unsafe），`framework/fs/mod.rs` 仅 `pub mod nestfs; pub mod vfs; pub mod vfs_poll_trait; pub use vfs::*;`。**`credo/storage.rs` 连锁整体下沉 `services/credo/persist`**（登记项 2-F 收口；framework 侧源文件删除，无 `vfs_*_internal` C FFI 薄层残留）。**VfsOps 契约保持 6 方法无扩容**（framework 残留消费面 `proc_ops`/`page_fault`/`epoll` 已全覆盖）；`services/fs/mod.rs` 于 `fs::init` 注册 `VfsOps` 实现替换 Fallback。`audit_services_boundary.py` 白名单新增 `('credo','fs')`，登记 `services::fs ↔ services::credo` 双向依赖。**§2.3 六门槛全绿**：双架构 0w0e ✅ / clippy pedantic（含 kernel_test + host-test 维）✅ / 核心审计（6 不变式 + F1-F9 + TD-22）✅ / host-tests ✅ / kernel-host 848 passed ✅ / QEMU x86_64 完整启动（`VFS ready` + Ring 3 + KPTI）✅。

状态：[X]

### 阶段 4c 实施计划（边界收敛）

描述：4b 下沉收口后的残留边界处理——①framework/services 内陈旧 doc 注释仍指向已下沉的 `framework::fs` 路径，改写为 4b 后准确描述；②fs 域内核测试载体归属收敛，落实 DECISION-080 双轨（纯逻辑断言迁源侧 `#[cfg(test)]`，硬件路径留 kernel_test 注册表）；③全量验证。

方案：

**（1）陈旧 doc 注释路径改写（4b 后准确描述）**：framework 侧 2 文件 4 行（`chitin/mod.rs` 的 `framework::fs::KernelError`→`framework::error::KernelError`；`proc/proc_ops.rs` 3 行 framework 内联全限定路径/先例注释→`services::fs`）；services 侧 10 文件 10 行（`fs/{cgroupfs,configfs,devpts,file_handle,sysfs,systree,virtiofs}.rs` 头注「委托至 `framework::fs::vfs::api`」→`services::fs::api`；`fs/overlayfs.rs` 的 `framework::fs::ramfs::RAMFS_DATA`→`services::fs::ramfs_core::RAMFS_DATA`；`proc/coredump.rs`、`syscall/firmware.rs` 各 1 行）。fs 域 5 陈旧迁移头注文件（`fs/{dcache,devfs,flock,open_file_table,vfs_manager}.rs`）改写为「## 阶段 4b 归属收敛（当前权威）」+ 历史记录标注「已被阶段 4b 取代」。

**（2）fs 域内核测试载体归属收敛（DECISION-080 双轨）**：删 4 个注册表载体（合计 1489 行 / 92 例），断言迁源侧 `#[cfg(test)]`：

| 注册表载体（删） | 行 | 例 | 源侧落点 |
|---|---|---|---|
| `framework/tests/test_devfs.rs` | 128 | 9 | `services/fs/devfs.rs` |
| `framework/tests/test_vfs.rs` | 543 | 18 | `vfs_manager`/`backend_trait`/`ramfs_core`/`inotify`/`file_ops`/`proc::signal`/`sync::epoll`/`fs::mod`/`framework::proc::fd_table` |
| `framework/tests/test_nestfs.rs` | 535 | 49 | `services/fs/nestfs/*`（11 文件，含 arc/bp/checksum/compress/dataset/dmu/snapshot/spa/txg/zap/zil） |
| `framework/tests/test_nestfs_ext.rs` | 283 | 16 | `services/fs/nestfs/*`（同上） |

**（3）D 类全局态隔离**：3 例依赖全局进程/调度器态（`test_open_populates_fd_metadata`/`test_fd_to_inode_id_downstream`/`test_temporary_sigmask_swap`）迁源侧后并行执行存在竞争（可致 use-after-free）；经用户裁定引入 `FS_GLOBAL_TEST_LOCK`（`services/fs/mod.rs`）共享串行化 + `with_temp_process` 辅助。

**（4）注册表收敛**：`framework/tests/mod.rs` 移除 4 行 `mod` 声明 + 5 行 `register_all_tests` 调用（-15 行）。

**（5）fmt 定点修正**：本批新增测试代码 5 处 cargo fmt 违规（`framework/proc/fd_table.rs`、`nestfs/checksum.rs`、`nestfs/dataset.rs`、`nestfs/zap.rs`、`fs/vfs_manager.rs`）定点修正（§12.5）。

验证：§2.3 六门槛全绿（双架构 0w0e / clippy pedantic / 核心审计 / host-tests / kernel-host / QEMU）。

**4c 实施记录**：陈旧 doc 注释路径改写 framework 2 文件 4 行 + services 10 文件 10 行 + fs 域 5 文件陈旧头注改写；fs 域内核测试载体归属收敛——删 `test_vfs`/`test_devfs`/`test_nestfs`/`test_nestfs_ext` 4 文件（1489 行 / 92 例），断言迁源侧 `#[cfg(test)]`（nestfs 11 文件 65 例 + vfs/devfs 多落点）；`FS_GLOBAL_TEST_LOCK` 串行化 3 例全局态用例；`framework/tests/mod.rs` 移除声明与调用（-15 行）；本批新增 5 处 fmt 违规定点修正。**§2.3 六门槛全绿**：双架构 0w0e ✅ / clippy pedantic（含 kernel_test + host-test 维）✅ / 核心审计（6 不变式 + F1-F9）✅ / host-tests ✅ / kernel-host 947 passed ✅ / QEMU x86_64 完整启动（`VFS ready` + Ring 3 + KPTI）✅。

**预存问题（本批发现，未处置，待用户裁决）**：
- `cargo fmt --check` 全仓 36 文件差异：本批新增 5 处已修，其余 36 文件为预存 repo-wide 违规；fmt 不属 §2.3 六门槛，故未擅自修改（§12.2/§12.5）。
- 非 fs 域 doc 头注待核：framework 域 22 个源文件头注含「— framework 机制实现」字样（抽查为 4b 后准确描述，fs 域外、4b 未触碰）；如需全量复核属专项工作，非本批范围。

状态：[X]

### 阶段 5 实施记录（反向依赖整治收口 + 审计脚本维护）

阶段 5（§8）为反向依赖整治收口——framework 域**生产**反向依赖归零。本批完成 §9.2 文档状态同步 + `audit_reverse_deps.py` 正则失效修复 + §2.3 六门槛复核。

- **反向依赖整治全序列**：DECISION-J→K 第 1~27 批实施（含壳删除 82 文件、直接 use trait 化、保留文件收敛、HDMI 孤儿目录删除），framework 域生产反向依赖由 136 处/78 文件降至 0；终点批次为第二十七批 HDMI 专项（§11 记录，孤儿目录删除）。§7.3 测试上下文引用属合理豁免，不纳入验收。
- **`audit_reverse_deps.py` 正则失效修复（本批核心工具维护）**：脚本原正则 `crate::kernel::services|kernel::services::` 在方案 D 独立 crate 化（commit `3578b4e8`，全仓 `crate::kernel::X` → `crate::X`）后失配，恒报「0 文件/0 行」——属**假阴性**（保护能力丧失，非真实归零）。修复：①正则改 `\bcrate::services\b`；②新增整行注释行排除（`//` / `///` / `//!` 与 `/* */` 嵌套；行内代码后注释不整行排除，fail-closed）；③`scan_file` 返回 (生产引用, 测试引用, 注释引用) 三分类，`main` 分别聚合输出。修复后核验：扫描 320 文件，**生产反向依赖 0 文件/0 行**，测试上下文引用 8 文件/30 行（§7.3 合理），注释引用 3 文件/3 行（`framework/fs/vfs/types_pod.rs` / `framework/lib/cstr.rs` / `framework/proc/proc_ops.rs` 各 1 行文档交叉引用，不计违规）。
- **门禁 0.5j 权限缺陷（工程外预存，用户 `2bbdfb78` 引入）**：`ci/audit.sh` 0.5j 直执行 `scripts/audit_{repr_c,volatile_access,static_mut}.py`，但三脚本 git mode = 100644（非可执行），直执行返回 EACCES（rc=126），触发 `err()` 的 `exit 1` 阻塞门禁 ③。经用户裁定（方案 A）补执行位为 100755——对齐仓库其他直调用审计脚本惯例（均 100755）且三脚本自带 shebang；脚本逻辑本身手工 `python3` 运行 rc=0 无误，属"新脚本漏加执行位"。
- **§9.2 文档同步**：§3 验收项 2（framework→services 反向依赖 = 0 → [X]，注明生产口径与核验脚本）、§8 阶段 5 状态（[] → [X]）、§9 门槛 6 口径（`kernel::services` → `crate::services`，生产口径 + 核验脚本）。
- **§12.5 工程外问题（已发现，未处置）**：`audit_deadlock_matrix.py` L161-173 与 `audit_once_cell.py` L79/L83 仍含 crate 化前的陈旧 `crate::kernel::...` 前缀（前者有 `(?:crate::)?` 备选兜底、后者仅提示文案），未擅自修改，待用户裁定。

验证：**§2.3 六门槛全绿**——双架构 0w0e（`./ci/build.sh all` Passed 5 / Failed 0：x86_64 + aarch64 构建、host-tests、forbidden patterns、x86_64 链接）✅ / clippy pedantic 三维（lib + kernel_test + host-test，`-D warnings`）✅ / 核心审计（`./ci/audit.sh quick`，含 0.5j 三内存安全脚本、6 不变式、F1-F9、TD-22）✅ / host-tests ✅ / kernel-host 947 passed / 0 failed ✅ / QEMU x86_64 完整启动（`VFS ready` + Ring 3 + KPTI）✅。

状态：[X]

### 阶段 6 验证记录（全量验证 + 阶段 3 回归修复，实施：AI）

阶段 6 按 §3 验收六项 + §9 门槛七项全量核验，产出验证报告 `docs/report/framekernel-paradigm-validation.md`（自由描述风格，冻结当时事实）。

- **§9 七门槛**：达标 6 项——双架构 0w0e（`./ci/build.sh all` Passed 5 / Failed 0）；clippy pedantic 三维（`-D warnings`）0；核心审计全过；host-tests 全过 + `make test-kernel-host` **941 passed / 0 failed**；QEMU（`./scripts/qemu_boot_test.sh x86_64` 1/1 + `ci/audit.sh full` 7/7 双架构 2/2）；生产反向依赖 0 文件/0 行。未达标 1 项：TCB 占比 56.7% > 30%（`audit_tcb_ratio.py` Status EXCEEDED）。
- **§3 验收六项**：达标 4 项（项 2 反向依赖 = 0 / 项 3 services 权威 / 项 5 services 0 unsafe / 项 6 §2.3 门槛）；未达标 2 项（项 1 TCB < 30%、项 4 framework `.rs` 300 个 vs §6.6 规划 200）。
- **本轮修复（阶段 3 直接引入，§12.5 必修）**：`scripts/qemu_boot_test.sh` aarch64 分支批次 Z ④ 校验 grep 串 `"virtio-net: probed successfully (services bridge)"` 陈旧——阶段 3 收尾提交 `36b5de5d` 已将 framework 侧探测日志串改名为 `"nic: probed successfully (services bridge)"`（`framework/net/init/probe.rs:42`），致 `FAIL_OK=0` 下 `RESULT=1`、`ci/audit.sh full` `7/7` 误报"执行异常"。修复：脚本 grep 串对齐实际日志并补中文注释。复验 `ci/audit.sh full` `7/7` 恢复输出 "QEMU 双架构启动测试: 2/2 通过"。
- **环境性非阻断项**：`4/6` Lockbud 未安装（warn）；`6/6` 模块级 SAFETY 不变式文件数 2 < 5（warn）；`audit_deadlock_matrix.py` 1 项 HIGH `framework/arch/x86_64/smp_init.rs:196 AP_STARTUP_LOCK`（既有项，阶段 0 记录已列）。
- **门槛顺序敏感性登记**：`ci/audit.sh` 的 FP-06 读 `build/kernel.bin` 需 aarch64 链接产物，`1/6` 的 x86_64 维需 `build/stage1.bin` 存在；二者并存需 `./ci/build.sh aarch64` 后补 `make ARCH=x86_64 build/stage1.bin`（仅汇编引导码，不触碰 `kernel.bin`）。

状态：[X]（验证执行完成；§3 项 1 / 项 4 未达标，后续收敛待裁决）
