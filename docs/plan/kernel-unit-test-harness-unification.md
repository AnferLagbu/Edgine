# 内核单元测试 harness 统一工程（孤儿测试复活 + 双轨收敛）

> 本文件是**新立项**的独立工程文档，承接 [audit-fix-09-hard-rules-deadcode.md](./archive/audit-fix-09-hard-rules-deadcode.md) 的 **B09-19（孤儿测试治理）** 条目，并**变更其技术方向**。
>
> B09-19 原文（含批次 1/波 1B 的历史验证记录）**保留不动**，作为交接依据与方向变更的依据链；本文件是当前唯一权威实施计划。

## 1. 根因与普查数据

**根因**：`src/kernel/` 下的 `#[cfg(test)] mod tests` 内联单元测试**从不编译、从不执行**。三条机制叠加：

1. `src/kernel/Cargo.toml` 的 `[lib] test = false` —— kernel crate 自身不构建单元测试 harness（历史成因见 [archive/test-compile-issues-2026-07-31.md](./archive/test-compile-issues-2026-07-31.md) 的 DECISION-021：E0152 双 `core` lang item 冲突）；
2. `make test-host` 走 `host-tests`，它以**普通依赖**引用 `queenx`（`src/rust`）→ `kernel`，依赖编译不设 `cfg(test)`；
3. `make test-unit` 走 `cargo build --features kernel_test`（不是 `cargo test`），测试发现依靠 `framework/tests/mod.rs::register_all_tests()` 的**手工注册表**（115 个注册组），与 `#[cfg(test)]` 是两套互不相通的 harness。

**实测规模（排除 vendored `src/kernel/services/net/smoltcp/`）**：

| 口径 | 数值 | 取证方式 |
|---|---|---|
| `^#[cfg(test)]` 出现处 | **109 处 / 103 文件** | grep 计数 |
| 另有变体 `#[cfg(all(test, target_arch))]` | 1 文件（`framework/timer/irq.rs`） | grep `mod tests` |
| 死 `#[test]` 用例 | **770 例 / 104 文件** | grep `^\s*#\[test\]` 计数 |
| 活测试（注册表侧） | 115 注册组，双端执行 | `register_all_tests()` |

**普查结论（覆盖判定，口径：注册表中是否存在针对该模块/文件级命名空间的注册组）**：

- **完全无覆盖：31 文件 / 342 例** —— 无任何门槛保护；
- **部分覆盖：** 有同子系统注册组但覆盖范围不等价（`credo/*`、`barrier/*`、`services/sync/*`、`driver::ata`、`dcache`、`cfs`/`scheduler_ex`、`sysctl` 等）；
- **有等价覆盖：** 同命名空间注册组存在（`sync/{spinlock,seqlock,rwlock,mutex,atomic,types,rcu}`、`idt/*`、`arch/{gdt,tss}`、`lib/{string,cstr}`、`ipc/*`、`mm/{cow,frame,slab,kmalloc_slab,page_fault}`、`nestfs/{arc,zil}`、`timer/*`、`net::e1000`、`cpu*`、`chitin/devtree` 等），其中含**迁移后未删的旧副本**（与 `framework/tests/*.rs` 断言重复）。

**完全无覆盖清单（最高风险批，路径相对 `src/kernel/`；⚠ = 安全敏感或核心子系统）**：

| framework/ | 例数 | services/ | 例数 |
|---|---|---|---|
| ⚠ mm/copy_user.rs | 13 | ⚠ net/smoltcp_impl.rs | 48 |
| ⚠ sync/lockdep.rs | 10 | driver/display/dp.rs | 30 |
| ⚠ userptr.rs | 5 | ⚠ debug/ebpf_verifier.rs | 27 |
| chitin/mod.rs | 14 | driver/storage/nvme.rs | 14 |
| net/iface_trait.rs | 27 | net/dhcp_policy.rs | 11 |
| usb/mass_storage.rs | 19 | ⚠ mm/pmm_policy.rs | 6 |
| usb/hid.rs | 18 | driver/storage/ahci.rs | 6 |
| usb/xhci.rs | 12 | mm/slab_policy.rs | 5 |
| usb/enumerate.rs | 12 | init.rs | 2 |
| usb/ring.rs | 11 | debug/mod.rs | 2 |
| net/init.rs | 11 | | |
| net/save.rs | 5 | | |
| net/wait_queue.rs | 4 | | |
| driver/storage/{ahci,nvme}.rs | 4 / 4 | | |
| driver/display/{framebuffer,controller}.rs | 6 / 5 | | |
| usb/usb_core.rs | 5 | | |
| chitin/{composite,user_driver}.rs | 2 / 2 | | |
| error.rs | 2 | | |

## 2. 实证（本轮探针，决定性）

**E1：E0152 障碍已消失（B09-19 的立论基础失效）**

`cd src/kernel && cargo test --features host-test --lib --no-run`（host 目标、无 build-std）实测：

- **未出现任何 E0152**（`duplicate lang item ... core`），直接进入代码错误阶段；
- 说明 `[lib] test = false` 的存在理由（DECISION-021）已随「构建模式显式化」（全局 build-std 从 `.cargo/config.toml` 移除，见 `src/rust/.cargo/config.toml` 注释）消失 ⇒ `cfg(test)` 在本仓**已可用**。

**E2：死测试不只未执行，且已全量腐化 —— 122 处编译错误 / 43 文件**

错误码分布：

| 错误码 | 数量 | 含义 | 性质 |
|---|---|---|---|
| E0793 | 39 | packed 结构体字段引用未对齐 | 实现改 packed 后测试未跟随（`arch/x86_64/tss.rs` 等） |
| E0599 | 24 | 方法不存在 | API 已改名/移除（如 `devtree_create_node` 返回 `u32` 后仍 `.unwrap()`） |
| E0308 | 19 | 类型不匹配 | 签名漂移 |
| E0277 | 9 | trait 约束不满足 | 同上 |
| E0061 | 7 | 参数个数不符 | 同上 |
| E0425 / E0433 | 6 / 3 | 名字 / 路径未解析 | 模块被删或改名 |
| E0502 / E0596 | 6 / 3 | 借用冲突 | 行为演化 |
| E0560 / E0609 | 4 / 1 | 缺字段 / 无该字段 | 结构体演化 |

热点文件（错误数）：`chitin/devtree.rs`(16)、`usb/enumerate.rs`(11)、`lib/cstr.rs`(10)、`lib/string.rs`(10)、`barrier/attribution.rs`(10)、`chitin/mod.rs`(9)、`barrier/health_monitor.rs`(8)、`arch/x86_64/tss.rs`(7)、`credo/grants.rs`(7)、`fs/vfs/dcache.rs`(6)、`services/driver/storage/nvme.rs`(6)。

**E3：双体系防腐化能力不对称**

- 注册表侧断言一直在跑 ⇒ 未腐化；
- `cfg(test)` 侧 770 例从不跑 ⇒ 全量腐化（E2）。
- 机制差异：`#[test]` 由 **cargo 自动发现**（新增即执行，零登记负担）；注册表为**手工注册**（`register_all_tests()` 漏一行即静默不执行 —— B09-19 自身批次计划中即含「修 net.rs 死载」类处置项）。

**E4：注册表体系不可替代的部分只有一个**

裸机 target 无 test harness ⇒ **必须**裸机执行的硬件路径断言（MMIO / 中断 / STAC-CLAC / 页表实机行为）只能由 `kernel_test` 载体承载。纯逻辑断言在 QEMU 内执行无增量价值（同代码，host 更快、可过滤、可并行）。

## DECISION-080（实施裁定）

- **描述**：孤儿测试的终态归属与统一形态。
- **方案（用户裁定）**：
  1. **形态 = 方案 A 完整版**：删 `[lib] test = false` + 修复 122 处编译错误，使 `cargo test -p kernel --features host-test --lib` 全绿（770 例就地复活）。
  2. **门槛 = 新增 AGENTS §2.3 第 6 条**：host 侧内核单元测试全绿。
  3. **长期目标态 = 双轨，且每份断言只存在一处**：
     - **纯逻辑（可在 host 编译）→ 源文件 `#[cfg(test)]` 唯一一份**（cargo 自动发现，无注册负担）；
     - **硬件路径（需裸机）→ `kernel_test` 注册表载体**（QEMU 侧，不可替代）；
     - 已有等价注册组中与 `cfg(test)` 重复的纯逻辑断言：**逐例收敛为一处**（详见 UT-07）。
  4. **方向变更理由**：B09-19 的判据「源码零 `cfg(test)`」建立在「`cfg(test)` 不可用」的前提（DECISION-021/E0152）之上；该前提已由 E1 证伪。且其「私有断言必须改写为公共 API」判据在执行记录中造成覆盖弱化（`lockdep` 两个私有精确断言弱化、`dcache invalidate_entry` 断言丢弃），与 AGENTS §8（约束集成测试）的本意不符。
- **状态**：[ ]
- **详情（对 B09-19 的继承）**：B09-19 的**处置判据**（已覆盖 → 删副本；独特断言 → 保留）、**私有 API 边界登记**、**调研盲区登记**仍有效，作为 UT-07 收敛的输入；其**载体迁移批次计划（批次 2–5）不再执行**。

## 任务清单

- **UT-01. 门槛骨架落地**
  - 描述：使 host 侧内核单元测试可被一条命令执行并进入门槛。
  - 方案：
    - 删 `src/kernel/Cargo.toml` 的 `[lib] test = false`（保留 `crate-type` 不变）；
    - `AGENTS.md` §2.1 构建命令补 `cargo test -p kernel --features host-test --lib`（在 `src/kernel` 目录内执行），§2.3 验证门槛新增**第 6 条**「host 侧内核单元测试 0 failed」；
    - Makefile 新增 `test-kernel-host` target（`test-host` 之外独立，避免拖慢既有门槛语义）；CI 侧接入方式在 UT-08 定稿。
  - 状态：[X]
  - 详情（本步骤**不要**同时修 122 错误 —— 骨架先落地，错误修复按 UT-02..UT-06 分批）。
  - 详情（落地形态）
    - `src/kernel/Cargo.toml`：删 `test = false`，`crate-type` 不动；原位留 4 行中文注释说明删除理由（DECISION-021 前提失效）与本文件指针，防止后人按旧结论重新加回。
    - `AGENTS.md`：§2.1 构建命令增 `make test-kernel-host`；§2.3 新增第 6 条「host 侧内核单元测试 0 failed」。
    - `Makefile`：新增 `test-kernel-host` target（`test-host` 之后、`test-unit` 之前），并在 `.PHONY` 登记；配方为 `cd src/kernel && cargo test --features host-test --lib`（**必须**在该目录内执行，以加载其 `.cargo/config.toml` 的 target-dir 与 curve25519 后端固化），日志落 `tests/reports/kernel_host_test_<时间戳>.log`；**未**并入 `test:` 聚合目标（保持既有门槛语义不变，CI 接入留 UT-08）。
  - 详情（验证实测）
    - 删除生效确认：`cd src/kernel && cargo test --features host-test --lib --no-run` 输出 `kernel (lib test)` 目标并报 **122 error / 13 warning**，与 E2 预测逐数吻合 ⇒ 骨架确实让 770 例进入编译；**`duplicate lang item` 命中 0** ⇒ E0152 未复现。
    - 全量门槛复跑：`./ci/build.sh all` → **Passed: 5 / Failed: 0**（x86_64 build / aarch64 build / host-tests / forbidden patterns / x86_64 link 全过）⇒ 删 `test = false` 不破坏双架构构建、链接与既有 host-tests。
  - 详情（前置条件 —— 与 UT-08 联动）
    - 上条「Passed: 5」是在 `build/stage1.bin` **存在**时取得。首次复跑曾出现 x86_64 build 与 host-tests 双失败，根因是该裸机产物缺失（`build/` 被 gitignore，仅由 `make` 生成）。
    - 机制：`framework/syscall/dispatch.rs` 的 `include_bytes!(".../build/stage1.bin")` 所在函数 gate 为 `all(not(feature = "kernel_test"), target_arch = "x86_64")`，**host 构建满足该条件** ⇒ host 侧编译硬依赖裸机产物。此即 build.rs G-06「消除隐式 make 耦合」未覆盖的残余项，详见下方「风险与回退」与 UT-08。
    - **该项已于本轮处置完成**（判据改用 `target_os = "none"`，共两处产物引用），见 UT-10。

- **UT-02. 修复批次 1 —— chitin + lib（实测 30 处）**
  - 描述：`chitin/devtree.rs`(16)、`chitin/mod.rs`(9)、`chitin/composite.rs`(2)、`chitin/user_driver.rs`(2)、`lib/cstr.rs`(10)、`lib/string.rs`(10)。
  - 方案：逐处判定「测试过时（改测试）」vs「实现回归（登记，不擅自改实现）」，按 §12.5 分级处置。
  - 状态：[X]
  - 详情（实测基数与清单修正）：探针中该批实际错误 **30 处**（非计划估算的 49 —— 原数值按 `-->` 行计数，含 note 内的重复行）。构成：`chitin/devtree.rs` 7、`chitin/mod.rs` 9、`lib/cstr.rs` 5、`lib/string.rs` 5、**`framework/idt/types.rs` 4（原批次清单未列入，本批一并处置）**。`chitin/composite.rs`、`chitin/user_driver.rs` 探针中 0 错误（其 `-->` 行来自别处 note）。
  - 详情（新增错误大类 E0793）：本批及后续批次出现 **39 处 E0793「reference to field of packed struct is unaligned」**（`assert_eq!(packed.field, ..)` 展开为 `&packed.field`）。这是 `[lib] test = false` 期间从未编译过的测试代码首次暴露的**硬错误**，非实现回归。处置形态：改为按值取出字段再断言（`assert_eq!({ x.field }, ..)` 或先 bind 到局部变量），语义不变。
  - 详情（逐处判定结论 —— 全部为「测试过时」，**0 处实现回归**）
    - `lib/cstr.rs`：`CStrExt` 仅实现于 `*const u8` / `*mut u8`，测试用 `CString::as_ptr()`（`*const i8`）⇒ 补 `as *const u8` 转型。
    - `lib/string.rs`：`strchr`/`strrchr`/`strstr` 形参为 `*const i8`，测试传 `*const u8` ⇒ 改用 `*const i8`。
    - `chitin/devtree.rs`：测试调用的是**旧的原生 API** `devtree_create_node(&'static str, ChitinProto, Option<NodeId>) -> Option<NodeId>`，该 API 后续被改为 FFI 包装 `devtree_create_node(*const u8, u32, u32) -> u32`，原生实现移入 `devtree_create_node_impl` ⇒ 测试改调 `devtree_create_node_impl`（crate 内可见）。
    - `chitin/mod.rs`：(a) `NetOps` 字面量仍用旧字段名 `recv` / `irq_ack`（现为 `try_receive` / `handle_irq: Option<NetIrqFn>`），且字段类型是 `extern "C" fn` 而**闭包不能强转为 C fn** ⇒ 改为具名 `extern "C" fn` 桩函数；(b) `chitin_register_block_dev` 形参为 `&'static mut dyn BlockDevice`，测试把 `&'static mut` 局部变量传入后仍读该变量（E0502 借用冲突）⇒ 改为 `Box::leak` 后以裸指针持有并加 `// SAFETY:` 读回观测字段。
    - `idt/types.rs`：E0793 形态（`IdtEntry` / `IdtPtr` 为 packed）。
  - 详情（验证实测）：`cargo test --features host-test --lib --no-run` 错误数 **121 → 91**（降幅 30，与本批改动数一致），批次内文件残留 `-->` 行为 0。剩余 91 条归 UT-03..UT-05。

- **UT-03. 修复批次 2 —— usb + storage + arch（实测 41 处）**
  - 描述：`usb/enumerate.rs`(11)、`usb/hid.rs`(3)、`usb/ring.rs`(2)、`usb/xhci.rs`(3)、`driver/storage/nvme.rs`(4)、`services/driver/storage/nvme.rs`(6)、`driver/mod.rs`(3)、`display/framebuffer.rs`(1)、`arch/x86_64/tss.rs`(7)。
  - 状态：[X]
  - 详情（实测基数修正）：该批实际 **41 处**（E0793 共 35：enumerate 11 / tss 7 / services nvme 6 / nvme 4 / xhci 3 / ring 2 / hid 2；其余 6：`driver/mod.rs` 3、`hid.rs` 1 字段缺失、`services/debug/ebpf_verifier.rs` 2 —— 末者原批次清单未列，本批一并处置）。`display/framebuffer.rs` 探针中**无 error**（其 `-->` 行来自 unused-variable warning），归入 warning 清理项。
  - 详情（E0793 的判据特征 —— 用于后续批次识别）：仅**字段对齐要求高于 packed 对齐**时触发。故 u8 字段（`opcode` / `request` / `length` / `num_configurations`）不报错，而 u16/u32/u64 字段（`value` / `index` / `nsid` / `cdw*` / `usb_version` / `rsp*` / `iomap_base` / `parameter`）报错；`cmd.cdw10 & 0xFFFF` 这类「值上下文」表达亦不报错。处置形态统一为 `assert_eq!({ x.field }, ..)`（块表达式强制按值求值）或先 bind 局部变量；已用独立 `rustc` 探针验证该写法对 packed 结构合法。
  - 详情（逐处判定结论 —— 全部为「测试过时」，**0 处实现回归**）
    - `driver/usb/hid.rs`：`HidDriver` 已不含 `device_address` 字段（现存字段见结构体定义），断言随字段移除而删除。
    - `framework/driver/mod.rs`：测试引用 `SerialPort` —— 该类已于 2026-09-12 随「x86_64 字符设备业务下沉 services」迁至 `services/driver/char/serial.rs`（见 `framework/driver/char/mod.rs` 顶部说明）。framework 测试**不得反向依赖 services**（架构方向约束），故改为在测试模块内定义本地 `MockCharDriver` 实现 `Driver`，保留 `DeviceType::Char` 的 trait 多态覆盖。注意实现 `init`/`shutdown` 须用本模块别名 `DriverResult<()>`（`Result` 名被 `core::result::Result` 占用）。
    - `services/debug/ebpf_verifier.rs`：`BpfInsn` 已把 `dst`/`src` 打包进 `dst_reg`，构造统一走 `BpfInsn::new` ⇒ 测试 helper 改用构造器。
    - 其余（usb/nvme/tss）均为 E0793 形态。
  - 详情（验证实测）：`cargo test --features host-test --lib --no-run` 错误数 **91 → 50**（降幅 41，与本批改动数一致），批次内文件残留 `-->` 行为 0。剩余 50 条归 UT-04/UT-05。

- **UT-04. 修复批次 3 —— barrier + credo + ipc + syscall（实测 18 处）**
  - 描述：`barrier/attribution.rs`(10)、`barrier/health_monitor.rs`(8)、`credo/grants.rs`(7)、`credo/sessions.rs`(3)、`credo/policy.rs`(2)、`ipc/stress_tests.rs`(5)、`ipc/mod.rs`(1)、`ipc/dynamic.rs`(1)、`services/ipc/pipe.rs`(3)、`services/ipc/sem.rs`(1)、`services/syscall/mod.rs`(2)、`syscall/dispatch.rs`(1)。
  - 状态：[X]
  - 详情（实测基数修正）：该批实际 **18 处**，远低于计划估算的 43。原因是计划清单来自 E2 的 `-->` 行粗计（会把 note 块内的重复定位行计入，且部分条目实为 warning）—— 实测 18 处中：`barrier/health_monitor.rs` 8、`services/syscall/mod.rs` 1、`ipc/stress_tests.rs` 4、`services/credo/sessions.rs` 2、`services/credo/grants.rs` 1、`services/barrier/attribution.rs` 1、`framework/ipc/mod.rs` 1。清单中 `credo/policy.rs`、`syscall/dispatch.rs`、`services/ipc/pipe.rs`、`services/ipc/sem.rs`、`ipc/dynamic.rs` 实测 **0 error**（后两者为「测试过时」的**调用方**侧，被调用方签名本身无错），未做任何改动。
  - 详情（新增错误大类 —— 非 Copy 类型不能用数组重复语法）：`DomainFailureRecord` 含 `AtomicU32/U64` 字段 ⇒ 非 `Copy` ⇒ `[DomainFailureRecord::new(1); N]` 与 `core::array::from_fn` 均不可用（后者因 `HealthMonitor::new` 取切片 `&mut [DomainFailureRecord]`，元素个数 N 无法从上下文推断）。处置形态统一为 `[(); N].map(|()| DomainFailureRecord::new(id))`。
  - 详情（逐处判定结论 —— 全部为「测试过时」，**0 处实现回归**）
    - `barrier/health_monitor.rs`：3 处 `record_failure(50)` 的签名已扩为 `(&self, current_tick: u64, heartbeat_gap: u64, dependents: u32) -> u32` ⇒ 补零；5 处数组重复改 `[(); N].map(..)`。
    - `services/barrier/attribution.rs`：同因 —— 数组重复改 `[(); 16].map(|| DomainFailureRecord::new(0))`。
    - `services/credo/grants.rs`：`is_valid` 经 `DelegationEngine` 的 `table` 字段访问（`eng.table.is_valid(gen2, 100)`），原 `table.is_valid(..)` 因 `table` 已被 `&mut` 借出而失效（NLL 下仅此一处冲突，末次 `eng.revoke` 之后的部分不报）。
    - `services/credo/sessions.rs`：`get`/`set` 是 `CapabilityMatrix` 的 **trait 方法** ⇒ 测试模块需显式 `use crate::services::credo::policy::CapabilityMatrix;` 才能调用。
    - `framework/ipc/mod.rs`：`MSG_MAX_SIZE` 的权威定义在 `ipc/types.rs` ⇒ 测试模块显式引入 `use super::types::MSG_MAX_SIZE;`。
    - `framework/ipc/stress_tests.rs`：pipe 族 safe API 以 `fd: i32` 标识端点（资源 ID 为 `IpcId`，语义不同）⇒ 无效 ID 用例引入 `let invalid_fd: i32 = invalid_id as i32;`；`sem_create_safe` 现为 5 参（不收 `pid`）⇒ 删末参。
    - `services/syscall/mod.rs`：`SyscallArgs::new` 需 6 参 ⇒ 4 参用例改走具名构造函数 `SyscallArgs::four(..)`。
  - 详情（随批清理的编译警告）：本批文件内 9 处警告一并清零 —— `credo/grants.rs` 6 处 `variable does not need to be mutable`（`CapabilityMatrix::set` 取 `&self`，`let mut from` 的 `mut` 冗余）、`credo/sessions.rs` / `ipc/dynamic.rs` 各 1 处循环变量未用、`ipc/stress_tests.rs` 1 处未用写端句柄；另清 `display/framebuffer.rs`（UT-03 遗留）1 处未用 `red`、`sync/rcu.rs` 1 处未用回调参数、`proc/scheduler_ex.rs` 1 处未用 import。
  - 详情（验证实测）：`cargo test --features host-test --lib --no-run` 错误数 **50 → 32**（降幅 18，与本批改动数一致），批次内文件残留 `-->` 行为 0，编译 warning **12 → 0**。剩余 32 条归 UT-05。

- **UT-05. 修复批次 4 —— fs + mm + net + proc + sync + 其余（实测 32 处）**
  - 描述：`proc/cfs.rs`(5)、`sync/spinlock.rs`(3)、`net/init.rs`(3)、`mm/page_fault.rs`(3)、`fs/vfs/dcache.rs`(3)、`sync/rcu.rs`(1)、`sync/mutex.rs`(1)、`mm/frame.rs`(2)、`mm/copy_user.rs`(2)、`net/iface_trait.rs`(2)、`cpu/mod.rs`(2)、`error.rs`(2)、`timer/mod.rs`(1)、`services/fs/nestfs/arc_trait.rs`(2)。
  - 状态：[X]
  - 详情（实测基数修正）：该批实际 **32 处**，计划清单的 16 文件/「约 25 处」为 E2 的 `-->` 行粗计（含 note 块内重复定位行）。精确口径（`^error` 行后紧随的 `-->` 行，逐条归属）合计 32，与错误总数一致。清单中 `proc/scheduler_ex.rs`、`net/init/query.rs` 实测 **0 error**（前者属 warning 清理项，见下；后者的 error 归属在调用方 `net/init.rs`，见下条）。错误码构成：E0599 13、E0308 6、E0425 5、E0277 3、E0596 3、E0061 2。
  - 详情（逐处判定结论 —— 全部为「测试过时」，**0 处实现回归**）
    - `net/init.rs`(3，E0425)：三处调用 `ipv4_from_atomic` 找不到函数 —— B04-09 拆分时该函数移入子模块 `net/init/query.rs` 并收窄为**私有**，测试留在父模块 `init`（经 `pub use query::*` 引入，私有项不被 re-export）。处置：可见性恢复为 `pub(crate)`（1 行生产改动解 3 处 error）。已排除「改测试引用 services 同名私有实现」——违反 framework 不得反向依赖 services。
    - `fs/vfs/dcache.rs`(3)：① `invalidate_entry` 无此方法（E0599）⇒ 删 `test_dcache_invalidate_entry`（**断言删除，理由登记见下条**）；② 两处 `icache.insert` 实参个数 5 vs 8（E0061，签名已扩为 `ino, file_type, perm, size, mtime, ctime, owner_pwm, group_pwm`）⇒ 补零。
    - `mm/frame.rs`(2，E0425)：`KERNEL_BASE` 权威定义在父模块 `framework::mm`，不在 `frame` 命名空间 ⇒ 测试模块显式引入。
    - `mm/copy_user.rs`(2)：E0308 + E0277 同源 —— `USER_ADDR_MAX`（u64）与 `PAGE_SIZE`（usize）相减 ⇒ 统一转 `u64`。
    - `mm/page_fault.rs`(3)：`stack_top`(2) / `stack_default_size`(1) 是 `PageFaultPolicy` 的 **trait 方法** ⇒ 测试模块显式引入 trait。
    - `net/iface_trait.rs`(2，E0308)：`NetEndpoint::new` 已取统一 `IpAddr`（双栈），IPv4 字面量改走 `new_v4` 辅助。
    - `proc/cfs.rs`(5)：`time_slice` → `time_slice_for`（后者为 `SchedDecision` trait 方法）；文档注释同步改名。
    - `sync/mutex.rs`(1)：`trylock` → `try_lock`。
    - `sync/rcu.rs`(1，E0308)：`call_rcu` 的 `func` 形参为 `unsafe fn(*mut RcuHead)`（**Rust ABI**），测试回调原为 `unsafe extern "C" fn` ⇒ 去 `extern "C"`。
    - `sync/spinlock.rs`(3，E0596)：`raw_lock`/`raw_unlock` 取 `&mut self`（底层原语不提供共享访问）⇒ 三个用例的绑定改 `let mut lock`；第 4 个用例（`debug_assert` 路径）不动。
    - `timer/mod.rs`(1，E0599)：`is_ok` 施于 `()` —— 模块自身 `pub extern "C" fn timer_sleep` 返回 `()`，带 `Result` 的是 re-export 别名 `timer_sleep_safe` ⇒ 改调别名。
    - `cpu/mod.rs`(2，E0308)：`CpuVendor::from_vendor_string` 取**定长 12 字节**数组，原字面量为 13 字节；QEMU 分支判据是 `&vendor_str[..9] == b"TCGTCGTCG"` ⇒ 字面量截为 12 字节且保留前 9 字节前缀（`"TCGTCGTCGXYZ"`）。
    - `error.rs`(2，E0277)：无 `From<KernelError> for i32` 实现 ⇒ 反向映射改经 `as_errno().as_i32()`。
    - `services/fs/nestfs/arc_trait.rs`(2)：枚举变体真名为 `NestArcBufType::Metadata`（`Data = 0, Metadata = 1`）⇒ 测试的 `Meta` 改名。
    - `proc/cfs.rs`（**合规修正，非编译修复**）：测试模块原 `use crate::services::config::{SCHED_LEVEL_*}`。该引用**不产生 error**（`services::config` 尚存 re-export 兼容层），但违反「framework 不得反向依赖 services」⇒ 改引 `framework::config`（DECISION-J 归属反转后的权威定义处）。登记为随批合规修正，取证为基线该文件 5 处 error 全为 `time_slice`。
  - 详情（断言删除登记 —— `dcache::test_dcache_invalidate_entry`）
    - 依据：`DCache::invalidate_entry` 已于 `dd5fa9c6`（refactor: eliminate dead code and implement pending features）**有意删除**，经 `git log -S 'invalidate_entry'` 溯源确认，当前**零调用方**。
    - 判定：属「能力已不存在 ⇒ 断言无等价入口」，**非重复副本**。`as_errno` 式「改写为公共 API」路径不可用（无任何等价公共入口）。
    - 与 DECISION-080 第 4 条的张力（如实登记）：该条把「dcache invalidate_entry 断言丢弃」列为 B09-19 造成**覆盖弱化**的依据之一；当时的弱化源于 B09-19 的「私有断言必须改写为公共 API」判据。本次丢弃的成因不同 —— **API 本身已从实现中移除**，故不构成同类弱化。此项交 UT-07 收敛清单时复核，如用户认为该能力应恢复则须单独立项（涉及生产代码，本轮不擅改）。
  - 详情（link 期 164 个 undefined symbol 的处置 —— 本批暴露的第二层问题）
    - 现象：编译 0 error 后链接失败，`rust-lld: error: undefined symbol` 共 **164 个**（`isr0..isr31` 32 + `irq0..irq127` 128 + `irq253` + `irq254` + `isr0x82` + `syscall_handler`）。lld 默认只报 20 条，须 `RUSTFLAGS="-C link-arg=-Wl,--error-limit=9999"` 才能枚举全量（注意不可写成 `-C link-arg=--error-limit=...`，该形态被直接传给 `cc` 而报 `unrecognized command-line option`）。
    - 根因：全部符号由 `framework/idt/mod.rs` 的 `idt_init` 内 `unsafe extern "C"` 块声明（由 `framework/boot/isr.asm` 在裸机链接期提供），唯一 host 触发点是测试用例 `test_ffi_interface_compiles`。已确认无其它 host 可达代码引用（`mm/kpti.rs` 仅注释提及 `isr0`）。
    - 该用例对 host **双重不适用**：① 链接期缺 isr/irq 汇编桩；② `idt_init` 路径经 `remap_pic`(outb) 与 `IdtManager::init` 末尾的 `load_idt`(lidt) 执行**特权指令**，ring 3 必炸；且 x86_64 host 会先命中 `IdtManager::init` 的 TSS IST 未初始化校验提前返回 `MODULE_INIT_FAILURE`，断言必失败。
    - 处置（用户裁定前置 —— 合规性提问后确认）：**方案 B** —— 用例改名 `test_dump_functions_no_panic`，仅保留 host 适用的 `idt_dump_state` / `idt_print_interrupt_stats` 两项冒烟断言（klog 在 host-test 下为 no-op，见 `klog/mod.rs`），删去 `idt_init()` 断言。
    - 未采纳方案及理由：**桩化 164 个符号违反「host-test 不平行实现」** —— 该纪律的权威口径是「桩体只允许常量中性返回或整段不执行，桩内无实现即无两份实现」（syscall-followup.md T1），而 164 个汇编 stub 的 host 替身是在 host 侧重造 `isr.asm` 的符号面；且 T1 已把方向定为「**消灭 host 占位符号，把守卫交还链接器**」（符号契约 7 → 0），桩化是反向回退。保留 dump 冒烟与删 `idt_init` 断言均**不新增任何 host 实现**，符合「同源双编译」（cfg 分叉一份源，非两份源副本）。
    - 与 T1「gate 调用点无效」记录的差异（已实测澄清）：T1 场景的符号引用点（`kpti_init` / `handle_user_page_fault` 等）在 host 仍可达，符号随可达函数进入目标文件；本例移除唯一 host 引用后 `idt_init` 整函数被 `--gc-sections` 丢弃，**实测链接成功**。若将来被 host 误引用，将**链接期硬失败**（即 T1 想要的响亮守卫）。
    - 覆盖归属（DECISION-080 第 3 条）：`idt_init` 属**硬件路径**，其载体是裸机轨 —— 真实覆盖来自 BSP 启动路径（`arch/x86_64/mod.rs` 的两处 `idt_init()` 调用，附带 AP 侧 `load_idt_on_current_cpu`）+ `./ci/build.sh all` 的链接阶段（缺任一符号即链接失败）+ QEMU boot。本轮**不新建** kernel_test 用例：在 kernel_test 内重调 `idt_init` 会在 `IF=1` 下重清 IDT 条目（`init` 先清后填），存在 #GP/#DF 风险，属生产风险且超出本工程范围。
  - 详情（随批清理的编译警告，10 → 0）
    - `warning[E0133]` ×1（`proc/scheduler_ex.rs:1069`）：edition 2024 下 `unsafe fn` 体内仍需 `unsafe` 块 ⇒ `drop(Box::from_raw(t))` 收进块内并补 `// SAFETY:`。
    - `warning: comparison is useless due to type limits` ×9：`pit.rs`(1，`PIT_MAX_COUNT <= 65535`)、`tick.rs`(4)、`sleep.rs`(2)、`timer/mod.rs`(2)。处置统一为**删除恒真断言**并保留调用（冒烟：不 panic），先例为 `pit.rs` 内 J-01 对 registry 轨**同一条断言**的既有处置（本批 host 轨这份正是漏改的双胞胎）。连带消除变量未用：`timer/tick.rs` 解构改 `(_ticks, .., _uptime, ..)`、`timer/sleep.rs` 改 `(result, _duration_ns)` / `(result, _duration_ticks)`。
  - 详情（验证实测）
    - `cargo test --features host-test --lib --no-run`：错误数 **32 → 0**（降幅与本批改动数一致），编译 warning **10 → 0**，批次内文件残留 `-->` 行为 0，**链接成功**（输出 `Executable unittests lib.rs`）。
    - 与 UT-04 收尾口径一致：探针 33 条 `^error` 行含 1 条 `could not compile ... due to 32 previous errors` 汇总行 ⇒ 权威口径取 32。
  - 详情（转 UT-06 的运行期候裁项）
    - `error.rs::round_trip_common_errnos`：raw 列表含 `95`，但 `as_errno()` 把 `NotSupported` 映射到 `Errno::ENOSYS`(38) 而非 `EOPNOTSUPP`(95)。该用例**编译通过、运行期必失败**。属「断言与实现不符」的运行期裁决，按 §12.5 **登记不擅改语义** ⇒ 转 UT-06 逐例裁决。

- **UT-06. 全量编译全绿裁决**
  - 描述：`cargo test -p kernel --features host-test --lib --no-run` 0 error 后，运行并裁决**运行期失败**。
  - 方案：对运行期失败逐例判定 —— 「断言过时」（改断言）/「实现回归」（登记候裁清单）/「host 环境不适用」（标注理由后 `#[cfg]` 门控或迁 kernel_test，禁止静默 `#[ignore]`）。
  - 状态：[X]
  - 详情（等级裁定，施工前经用户确认）
    - A 类（断言过时）→ 改测试；B 类（实现缺陷）→ **经用户裁定本轮一并修复实现侧**（原 §12.5 口径为登记候裁，本轮显式授权从宽）；C 类（host 环境不适用）→ 迁 `framework/tests` 注册表 + QEMU 裸机实跑。
    - 单例裁定：B-12 → 用户选「accessor 改条目数」；B-14 → 用户答「最优是？」，由 AI 定为方案 (a)；C3 smoltcp socket 族 8 例 → 用户选「删除 + 登记为未来功能」。
  - 详情（实测基数与收尾）
    - 删 `test = false` 后首次真正执行 **763 例**（原预估 770）—— 初测 **693 OK / 68 FAIL / 2 HANG**；逐例裁决处置后收尾 **748 passed / 0 failed**，8 轮复跑零偶发。
    - 计数差额 15 = C3 删除 8 例 + C 类迁出内联 7 例（3 端口 I/O 类 + 4 PMM 类）；`ipc::tests::test_signal_validation` 保留（转 A 类环境容差）。
    - 逐例台账（分类/判据/行号）见施工期临时记录；本文只登记结论。
  - 详情（A 类 34 例 —— 断言过时，只改测试侧）
    - ① 类型推断陷阱（`framework/lib/string.rs` 4 例）：测试数组未标类型 ⇒ 被推断为 `[i32; N]`，`memmove`/`memcmp`/`secure_zero` 的字节语义随之偏移（gdb 实证）。
    - ② 常量/口径过时：`arch/x86_64/tss.rs` 2 例（`TSS_SIZE=104`，`104 % 16 = 8` 取整断言不成立；iomap base 104 是「禁用」哨兵）、`cpu/mod.rs`（effective family 漏加 base family）、`sync/types.rs`（IF 位应 `0x2`）、`mm/cow.rs`（pml4_idx(0x7FFF_0000_0000)=255）、`net/iface_trait.rs`（IPv6 CIDR 按 u16 写）、`idt/handlers.rs`（PF 的 P 位）、`services/syscall/mod.rs` 2 例（零长语义已由 B03-21 改为 ptr 非 NULL；user_ptr 界换 `USER_ADDR_MAX`）、`services/credo/policy.rs`（改不含保护下界的位）、`services/mm/pmm_policy.rs`（order 截断，127 → 9）、`proc/cfs.rs` 3 例、`proc/scheduler_ex.rs`（`make_test_thread` 初值即 Ready）。
    - ③ 实现侧已定契约：`chitin` 3 例（错误码 `-5`/`-22`）、`usb` 5 例（描述符偏移与 `device_data[4]`）、`usb/ring.rs`（Link 位置回绕 + 翻 cycle）、`timer` 2 例（hrtimer forward 边界、freq=0 未设）、`e1000`（`virt_to_phys` 仅对 `>= KERNEL_BASE` 有效）、`smoltcp_impl` dhcp 3 例。
  - 详情（B 类 19 例 + 2 观察项 —— 实现缺陷，用户裁定本轮一并修复）
    - 已修实现（代表性根因）：`services/debug/ebpf_verifier.rs`（ALU 分支先查 dst 已初始化再判 MOV ⇒ `MOV R0,1` 被拒，全部合法程序被拒，8 例同一根因）；`services/credo/audit.rs`（verify 用 buffer 全量重建 nodes，未写槽以 hash=0 覆盖 nodes[0] ⇒ verify 恒 false）；`services/driver/storage/nvme.rs` + `framework/driver/storage/nvme.rs` 2 例（SQE 尺寸与规范 64 不符、`cdw10` 断言自相矛盾）；`services/ipc/pipe.rs`（count==0 早退先于 fd 校验，违反自身 doc 与 POSIX EBADF）；`framework/error.rs`（`NotSupported` 映射 `ENOSYS`(38) → `ENOTSUP`(95)，errno 往返自洽）；`proc/cfs.rs`（min_vruntime 对齐）；`timer/tick.rs`（`ticks * 1000` 溢出 ⇒ `saturating_mul`，契约「不 panic」）；`proc/scheduler_ex.rs`（`boost_all` 活锁 —— 2 例 HANG 的根因）；`driver/net/e1000.rs`（`new()` 后 `is_ready()` expect panic）。
    - B-12（`services/fs/nestfs/arc.rs` + `arc_trait.rs`）：用户选「accessor 改条目数」—— `max_size` 量纲由字节改条目数，消除与 `mru_size`/`mfu_size` 的单位冲突。
    - B-14（`smoltcp_impl::record_dhcp_bound` 不更新 `dhcp_state`）：用户裁定取方案 (a) —— **不造调用点**，仅文档诚实化 + 测试意图修正 + 本文件登记（Bound 后 state 仍 Idle ⇒ `dhcp_decide_default` 恒 Continue，T1/T2 renew 分支当前不可达，属未实装路径）。
    - B-1/B-15（`idt/handlers.rs` 未知 vector 回落默认处理器的命名口径）：随 A 类同步按实测实现侧取值，未改语义。
    - B-13（`test_dhcp_handle_protects_one_slot`）：归入 C3 删除（socket 槽位依赖在 host/kernel_test 均不可满足）。
  - 详情（C 类 15 例 —— host 环境不适用）
    - 端口 I/O / 中断注册 3 例（`keyboard`/`ata` 的 `test_driver_trait_impl`、`timer/irq.rs::test_register_timer_irq_interface`）：host 执行 `init()` 触发端口 I/O ⇒ SIGSEGV；内联例删除，覆盖归 `framework/tests` 注册表（QEMU 裸机轨）。
    - PMM 依赖 4 例（`ipc/dynamic.rs::test_dyn_shm_alloc_and_free`、`ipc/stress_tests.rs::boundary_tests::test_zero_permissions`、`ipc/stress_tests.rs::stress_tests::test_shm_rapid_attach_detach`、`ipc/mod.rs::test_shm_lifecycle`）：host 无 PMM init ⇒ panic；迁入 `framework/tests/test_ipc.rs` 并注册，host 侧以 `TestResult::Skip` 占位，真实逻辑在裸机轨执行。内联副本删除以避免双份维护（与注册表同名用例断言等价）。
    - 环境容差 1 例（`ipc/mod.rs::test_signal_validation`）：`signal_send_safe(1,100)` 需目标进程存在，host/kernel_test 均无进程表 ⇒ 接受 `Ok || Err(-2)`（口径同 `framework/proc/signal.rs::kill_broadcast` 先例），但不得为范围错误 `Err(-1)`。内联保留。
  - 详情（C3 重裁定 —— smoltcp socket 族 8 例改「删除 + 登记未来功能」）
    - 原方案「迁注册表 + QEMU 裸机实跑」经实证**不可行**：`kernel_test` 下 `services/net/mod.rs` 把 `smoltcp_net_stack_socket_open` 替换为返回 `None` 的 no-op 桩，且 `kmalloc` 真实堆 init 被 `#[cfg(not(any(feature = "kernel_test", feature = "host-test")))]` 门控 ⇒ host 与 kernel_test 两环境都只有 4 KiB `early_buffer`；`TCP_BUF_SIZE = 4096` + HeapHeader ⇒ `early_allocate` 必为 None。两环境均不满足。
    - 依此重报用户，用户选「**删除 + 登记为未来功能**」。删除 8 例：`test_socket_open_returns_valid_handle_w32_stub` / `test_socket_open_all_kinds_succeed_w32_stub` / `test_socket_open_close_cycle` / `test_socket_open_until_full_returns_no_free` / `test_socket_close_frees_slot_for_reuse` / `test_dhcp_handle_protects_one_slot` / `test_socket_handle_allocated_distinct` / `test_socket_handle_invalid_id_skipped`（均为 socket_open 成功路径）。
    - 保留失败路径 4 例：`test_socket_open_before_init_fails` / `test_socket_close_invalid_handle_is_idempotent` / `test_dhcp_handle_cannot_be_closed` / `test_socket_handle_invalid_zero`。
    - **未来功能登记**：启用上述 8 例须先具备三项前置 —— 内核堆 (>4 KiB，供 TCP 双缓冲) + `framework::init_sockets()` + 非桩 `socket_open`；届时在裸机 `kernel_test` 轨恢复。源码处已同步登记（`smoltcp_impl.rs` §4 裁定注释）。
  - 详情（D 类 5 处 —— 逐例裁决外新增的并行/顺序隔离修复）
    - `services/net/unix.rs` **真实实现缺陷**：`uds_accept` 预分配 FD 后所有失败路径不回收 ⇒ 空队列 accept 每次泄漏 1 个 UDS FD（累积至 16 后 `alloc_fd` 恒 None，表现为并行 6 例 / 串行 3 例 `NoMem`）。已修失败路径回收 + `uds_reset_for_test()` 同步归零本子系统 FD 位图。
    - `unix.rs` 用例互斥：用例共享 `UDS_STATE`/FD 位图，新增 `static UDS_TEST_LOCK`（口径同 `framework/timer/tick.rs::FREQ_TEST_LOCK`）。
    - `framework/timer/hrtimer.rs` 顺序耦合 2 例：断言依赖「框架未初始化」，但同文件 `test_hrtimer_sleep_init` 会入队栈上定时器且 host 无 tick 消费 ⇒ 改为首行 `hrtimer_init()` 清队列。
    - `framework/driver/usb/xhci.rs` MMIO 区间重叠：10 例共用 `0xFE000000`，`ALIAS_REGISTRY` 拒绝区间重叠（Drop 会注销故串行通过）⇒ 改为每例独立基址。
    - `framework/chitin/mod.rs` 注册表错位：用例共享 `CHITIN_DEVICES` 且普遍以 `clear()` 开场并按返回的 `idx` 回查，并行 runner 下清表/注册互相错位下标 ⇒ 新增 `static CHITIN_TEST_LOCK`。
  - 详情（验证实测 —— §2.3 六门槛 + 双架构启动）
    - `./ci/build.sh all` **Passed: 5 / Failed: 0**；`./ci/audit.sh quick` 全绿（SAFETY 1994/1994、6 不变式、I-43/I-16/I-07/TD-22 0 违规、S-14、FP-06 0、双架构 check、clippy pedantic(lib) + kernel_test/host-test 两维）；`make test-host` exit 0；`make test-kernel-host` **748 passed / 0 failed**；`make test-unit` QEMU **ALL TESTS PASSED (exit 33)**；`./scripts/qemu_boot_test.sh x86_64` 与 `aarch64` 均「VFS ready + Ring 3 / EL0 + KPTI-09 断言通过」。
    - FP-06 需 aarch64 产物：因双架构共用 `build/kernel.bin` 且 `build.sh all` 最后链接 x86_64，审计前须先跑 `./ci/build.sh aarch64`（脚本已注明该次序要求）。
    - 连带修复（本轮新增注释/改动的自造问题）：TD-22 报 5 处英文段落（UT-06 删除清单的函数名续行）⇒ 改写为含中文描述；clippy `single_match_else` 2 处（`unix.rs`、`tests/test_ipc.rs`）⇒ 改 `let-else`；`error.rs` 的 `NotSupported` 注释由 `ENOSYS=95`（既存错误，名字与值不符）同步为 `EOPNOTSUPP/ENOTSUP=95`。
  - 详情（错误基数修正）：E2 的 122 条含 1 条裸机产物缺失错误，已由 UT-10 消除 ⇒ UT-02..UT-05 实际待修 **121** 条。

- **UT-07. 双轨收敛（消除双份断言）**
  - 描述：对已有等价注册组的约 72 文件逐例收敛为「一处一份」。
  - 方案：以 B09-19 的处置判据为输入 —— 纯逻辑断言以 `cfg(test)` 侧为准（删注册表副本或删源副本，取覆盖更强者）；硬件路径断言保留在 `kernel_test` 载体。
  - 状态：[X]
  - 详情：**先出收敛清单再动手**（逐例标注保留侧与删除侧），避免历史那种「半成品」；本步可分批，且允许按用户裁定延期执行。
  - 详情（调研口径 —— 本清单的数据来源与判定方法）
    - 规模（实测，排除 vendored `services/net/smoltcp/`）：注册表侧 **108 命名空间组 / 459 条用例 / 25 个 `framework/tests/*.rs`**；源 `#[cfg(test)]` 侧 **104 文件 / 754 例**。
    - 匹配方法：case 名与注册 fn 名同时归一化（去 `test_` 前缀 + 去下划线 + 小写），再与「同 basename 源文件」的源侧 test 名比对。
    - **判定粒度 = 命名空间，不是逐例** —— 断言可跨不同函数名分布（实例：注册侧 `timer::pit::frequency_bounds` 的 `PIT_MAX_COUNT == 65535` 实由源侧 `test_pit_constants` 覆盖），逐例比会大量误报。命名空间亦可能跨多个注册文件（`pwm::sha256` 分布在 `sys.rs` + `test_pwm.rs`），须聚合全部承载文件。
    - 断言原子比对（归一化后再比集合包含关系）：`check!`/`assert!` → `assert`；`assert_eq_test!`/`assert_eq!` → `assert_eq`；丢弃尾部消息串；归一 `&raw mut x` / `&mut x as *mut T`、`{ x.field }`（packed 解引用规避）、`alloc::format!("{}",x)` / `x.to_string()`、`as u32` / `as u8`、`assert(a!=b)` / `assert_ne!(a,b)`。
    - 硬件路径判据（唯一可靠信号）：注册 fn 若存在 `#[cfg(feature = "host-test")]` 的 `TestResult::Skip` 桩变体 ⇒ 依赖裸机 PMM/VMM/SMP ⇒ **保留注册载体，不参与收敛**。
    - 机器配对脚本（可重跑）：`/tmp/ut07_enum3.py`（枚举，出 `/tmp/ut07_matrix3.json`）；`/tmp/ut07_bodies.py`（逐例函数体对照）；`/tmp/ut07_ns.py`（命名空间级覆盖比对，出 `/tmp/ut07_ns.json`）。
    - 已知盲区（如实登记）：源侧 `#[cfg(feature = "kernel_test")] pub mod tests { pub fn ...() -> bool }` 形态的断言（如 `framework/barrier/reset/audit.rs`、`bbr.rs`、`bsr.rs`）**不是 cargo 可发现的 `#[test]`**，且 `cfg(test)` 关闭时不编译 ⇒ 其特征是"注册侧仅有薄包装 `check!(tests::xxx())`"。此类须**改写为源侧 `#[cfg(test)] #[test]`** 才算真正收敛（见 C 类 `barrier::audit`）。
  - 详情（分类结果 —— 34 个双份命名空间）
    - **A 类：可整组删注册副本（10 组，源侧断言 ⊇ 注册侧，零登记负担）**：`arch::gdt`(4 组/16 条)、`idt::statistics`(7/20)、`kmalloc_slab`(1/0)、`mm::slab`(5/15)、`rcu`(2/0)、`sync::atomic`(2/19)、`sync::seqlock`(3/7)、`sync::spinlock`(3/8)、`sync::types`(5/11)、`zil_persist`(1/2)。
    - **A′ 类：等价但须逐例登记理由（5 组）**：
      - `idt::types`(8 组/23 条)：4 条 MISSING 为 packed 解引用归一化残留（`entry.offset_low` vs `{ entry.offset_low }`），语义等价。
      - `page_fault`(3/12)：3 条为 `as u32` / `as u8` 判别式转换差异，值断言相同。
      - `net::e1000`(4/12)：`virt_to_phys(0x12345678)` 低地址形态**源侧已按 UT-06 实测修正**为 `virt_to_phys(KERNEL_BASE + 0x12345678)`（原断言会下溢 panic）⇒ 源侧更强。
      - `lib::string`(13/34)：2 条 `safe_memcmp` 由源侧 `test_rust_safe_interfaces` 以**同性质不同数据**覆盖（Equal/Less 各一）；raw `memcmp` 由源侧 `test_memcmp` 覆盖。
      - `mmap`(1/5)：注册侧 `test_prot_to_vma_flags` 因函数私有**只测 `PageFlags` 位运算**（弱代理），源侧 `test_prot_to_flags` 直调真实 `prot_to_vma_flags` ⇒ 删注册即**覆盖增强**（与 DECISION-080 第 4 条所指 B09-19 弱化同型，方向相反）。
    - **B 类：硬件路径，保留注册副本（3 组，不收敛）**：`mm::cow`（`child_write_isolated_from_parent` / `shared_frame_survives_owner_exit` / `unique_mapping_fault_reuses_frame`）、`mm::frame`（`handle_clone_drop_pairing` / `dma_buffer_raii_release`）、`cow`（`shared_frame_alloc_starts_at_one` / `shared_frame_inc_dec_paired`）—— 全部依赖裸机 PMM/VMM 页表与物理帧，host 变体为 `Skip` 桩。
    - **C 类：注册轨独有纯逻辑断言 → 先迁入源侧 `cfg(test)`，再删注册副本（16 组）**，逐组见下条。
  - 详情（C 类迁移清单 —— 逐组「独有断言 → 源侧目标文件」）
    - `arch::tss` → `framework/arch/x86_64/tss.rs`：真独有 1 条（`tss.get_ist(i) == Some(0)`，注册侧走公有取值器，源侧现只读裸字段 → 源侧改用取值器，API 级断言不降级）；其余 7 条为 packed 归一化残留。另 `TSS_SIZE >= 92` 与 `TSS_SIZE % 2 == 0` 两条**弱于**源侧 `TSS_SIZE >= TSS_MINIMUM_SIZE`(104) 与 `== TSS_MINIMUM_SIZE` ⇒ 登记理由"被更强断言蕴含"后丢弃。
    - `barrier::audit` → `framework/barrier/reset/audit.rs`：注册侧为薄包装 `check!(tests::test_audit_log())` / `check!(tests::test_audit_count_by_layer())`，实际断言在 `#[cfg(feature = "kernel_test")] pub mod tests` 内（host 不编译、非 cargo 可发现）⇒ 须改写为 `#[cfg(test)] #[test]`，并连带解除 `framework/tests/reset.rs` 的 `cfg(feature = "kernel_test")` 整模块门控依赖（`mod.rs` E-03 注记）。
    - `devtree` → `framework/chitin/devtree.rs`：3 条（`id > 0`、`node.is_some()`、`found.is_some()`）。
    - `driver::ata` → `framework/driver/storage/ata.rs`：3 条（`name()`、`device_type() == Block`、`!status().is_empty()`）。
    - `driver::framework` → `framework/driver/framework.rs`：12 条（`DeviceInfo` 构造/builder 字段面 7 条 + `DeviceType`/`DriverError` 的 `Display` 3 条 + 2 条 builder 覆写）。删注册侧 `device_info_creation` / `device_info_builder` / `result_type` 三组。
    - `driver::keyboard` → `framework/driver/input/keyboard.rs`：4 条（`name()`、`device_type() == Input`、`!status().is_empty()`、`!is_ready()`）。删 `driver_trait` 等 5 组。
    - `idt::handlers` → `framework/idt/handlers.rs`：3 条 —— ① `handler99.category() == ExceptionCategory::Unknown`（源侧只断 `name()`）；② `analyze_error_code(0x02)` 场景的 `access_type == Write` / `mode == Kernel`（源侧只覆盖 UT-06 修正后的 `0x04` Read/User 场景，**两侧输入不同 → 互补而非重复**，须把 0x02 场景一并补入源侧）。
    - `idt::safety` → `framework/idt/safety.rs`：`address_validation` 的 7 条地址谓词断言**迁源侧**；`cpu_features_no_panic`（5 条）依赖 CPUID 读宿主 CPU ⇒ **待裁定**（建议保留注册载体，见末条）。
    - `pwm::audit` → `services/credo/audit.rs`：3 条（`entry.pwm/as_u64() == 42`、`action.as_u32() == 3`、`result.as_u32() == 0`）。
    - `pwm::policy` → `services/credo/policy.rs`：**最大批** —— 注册侧 23 组 `CapBits`/`CapMatrix`/`InMemoryMatrix` 用例（45 条独有断言），源侧现只覆盖 `PolicyEngine`（13 fn）；须在源侧补 `CapBits`/`CapMatrix` 的 `cfg(test)` 用例组后删注册侧 23 组。
    - `pwm::sha256` → `framework/credo/sha256.rs`：10 条独有（`known_vectors` 的 `hash[0..2]` 精确字节、boundary 55/56/63/64 分块等），源侧现仅 5 fn / 2 条 ⇒ 须整组迁入。
    - `pwm::types` → `framework/credo/types.rs`：17 条（`PwmId`/`CapDomain`/`CapBits` newtype 行为：`is_valid`/`as_u64`/`as_u16`/`as_usize`/`contains`），源侧现只测 `PwmEntry`（10 fn）⇒ 须新增 newtype 用例组。
    - `vfs::types` → `framework/fs/vfs/types.rs`：**源侧该文件 `#[cfg(test)]` 数为 0** ⇒ 须新建 `cfg(test)` 模块，迁入 17 条（`FsType::from_name/as_str`、`VfsFileType`/`VfsSeekWhence` 的 `from_u8/from_u32` + 往返、`VfsDirent` 字段）。
    - `sync::mutex` → `framework/sync/mutex.rs`：4 条（`reentrant` 用例的 `depth() == 2/1`、`owner() == -1`、内层 guard 取值）⇒ 源侧新增递归锁定回归用例（G-17）。
    - `sync::rwlock` → `framework/sync/rwlock.rs`：8 条（`multiple_readers` / `write_blocks_read` / `read_blocks_write` 三用例的 `try_read`/`try_write` 取得与阻塞判据）⇒ 源侧新增（源侧现仅 `test_rwlock_concurrent_readers`）。
    - `timer::pit` → `framework/timer/pit.rs`：2 条 —— `u64::from(PIT_MAX_COUNT) == 65535` 由源侧 `test_pit_constants` 覆盖（**登记理由**）；`(actual_freq as i64 - 1000).abs() < 5` 的**双侧容差**在源侧被写成单侧 `(actual_freq - 1000) < 5`（更弱）⇒ 源侧改回 `.abs()` 形态后删注册副本。
  - 详情（门槛核算 —— AGENTS §2.3 第 6 条 + UT-07「断言净增不净减」）
    - A 类删注册断言 ≈ 98 条（全部被源侧同断言或更强断言覆盖，逐条由脚本核验 `源 ⊇ 注册`）。
    - A′ 类删注册断言 ≈ 86 条（逐例理由已在上条列明）。
    - C 类为**先迁后删**：迁入源侧的断言数 ≥ 删掉的注册断言数（迁入时以源侧口径为准，遇 UT-06 已修正的口径取修正版），净不减少。
    - B 类零改动。
    - 收敛后 `MAX_TESTS = 640` 有富余，无需调整；但注册表用例数下降约 250 条，须核对 `framework/tests/mod.rs` 各 `register_*` 调用点是否有随之失效的薄包装函数（F9 死代码零容忍：被删组的专属 fn 必须一并删除，不留空壳与 `#[allow(dead_code)]`）。
  - 详情（待用户裁定项）
    - `idt::safety::cpu_features_no_panic`：断言「宿主 CPUID 必须暴露 APIC / x2APIC 蕴含 APIC」。CPUID 非 DECISION-080 列举的硬件路径（MMIO/中断/STAC-CLAC/页表实机行为），但断言内容依赖运行环境 ⇒ 迁源侧会在 CI runner 上执行真实 CPUID（x86_64 runner 通常满足）。**建议保留注册载体**（属"裸机不变量"）。请裁定。
    - 分批节拍：建议先 A 类（零登记负担，风险最低）→ A′ 类（逐例理由已备）→ C 类（改动面最大，可再按「源侧已有同名文件」与「源侧零覆盖」两小批）。每批跑 §2.3 门槛（至少 `make test-kernel-host` + `make test-unit` + `./ci/build.sh all`）。
  - 详情（裁定结果 —— 用户裁定）
    - `idt::safety::cpu_features_no_panic`（5 条断言）⇒ **保留注册载体**，归入 **B 类**（B 类由 3 组增为 4 组，见上条分类结果）。理由：其断言为"运行环境必须满足的硬件不变量"，迁源侧后随 CI runner 的 CPUID 差异产生环境相关脆弱性，与 DECISION-080「硬件路径留 `kernel_test` 载体」的取向一致。
    - 施工节拍 ⇒ **先 A 类，逐批停下确认**。
  - 详情（A 类施工完成 —— 10 组 / 33 用例 / 98 条断言）
    - 处置形态：逐组删注册副本 + 删对应的 `#[cfg(feature = "kernel_test")] pub fn register_*_tests()` 转发 shim（F9 死代码零容忍），并在承载文件顶部加中文注记指向源侧唯一归属。
    - 明细：`arch::gdt` 4 / `idt::statistics` 7 / `kmalloc_slab` 1 / `mm::slab` 5 / `rcu` 2 / `sync::atomic` 2 / `sync::seqlock` 3 / `sync::spinlock` 3 / `sync::types` 5 / `zil_persist` 1 = **33 用例**（对应门槛核算中的 98 条断言）。
    - 改动文件：`framework/tests/{arch,idt,sync,sys,test_new_features}.rs`（删注册组/用例）+ `framework/{arch/x86_64/gdt,idt/statistics,mm/slab,sync/{types,atomic,seqlock,spinlock}}.rs`（删转发 shim）。
    - 零覆盖损失核验（逐组）：`rcu`（2 用例）与 `kmalloc_slab`（1 用例）的注册侧**断言数为 0**（仅调用后返回 `Pass`，无任何 `assert`），源侧 `framework/sync/rcu.rs::test_rcu_read_lock_unlock`（3 条断言，含嵌套锁场景）与 `framework/mm/kmalloc_slab.rs::test_cache_index_selection` 为唯一且更强的载体 ⇒ 删注册副本无断言损失。
    - 后续（分册 9 项 2，B09-21）：`framework/mm/kmalloc_slab.rs` 因属**全仓零引用孤岛**已**整体删除**（含上条所述唯一载体 `test_cache_index_selection`）⇒ 上条「唯一载体」表述随之核销；删除判据、连带清理与六门槛实测见 [syscall-followup.md](syscall-followup.md) **B-10.10** 与 [audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **B09-21**。
    - 随删清理（本次删除直接导致，非工程外）：`framework/tests/sys.rs` 的 `use crate::framework::mm::slab::{..}` 整块随之失效 ⇒ 一并删除；`framework/tests/test_new_features.rs` 三处空节横幅（RCU / Kmalloc-Slab / ZIL Persistence）在用例删除后一并删除。
    - 明确**不删**：`services/fs/nestfs/zil_persist.rs::crc32_test_wrapper` —— `host-tests/tests/zil_replay_test.rs` 仍以它为入口（源侧与 host-tests 双载体，非本次收敛对象）。
  - 详情（A 类验证实测 —— 六门槛本机复跑）
    - `make test-kernel-host`：**749 passed / 0 failed**。
    - `make test-unit`（QEMU）：**489/489 ALL TESTS PASSED**；注册用例数 **522 → 489（-33）**，与上条 A 类明细逐组吻合。
    - `./ci/build.sh all`：**Passed: 5 / Failed: 0**（x86_64 build / aarch64 build / host-tests / forbidden patterns / x86_64 link）。
    - `./ci/audit.sh quick`：通过（含 clippy `kernel_test` 维 + `host-test` 维与全部核心审计）。
    - clippy pedantic（x86_64 release 裸机维）：0 error / 0 warning。
    - fmt 核验：本次改动引入的 `cargo fmt --check` 差异为 **0**（改动行区间与 fmt 报告行号无交集；曾出现的 4 处空行残留已归一）。
  - 详情（A 类期间暴露的预存问题，登记不擅改 —— §12.5）
    - `make test-unit` 首次运行报 `构建产物缺失: build/user/init.bin`，须先 `make` 生成裸机产物 —— 与 UT-08「前置条件」同源（隐式 make 耦合残余），非本次改动导致。
    - `cargo fmt --manifest-path src/kernel/Cargo.toml -- --check`（CI `clippy-pedantic` job 末步）在本机对**未改动**文件亦报差异，全库 **415 处 / 163 文件**。已核验本次 UT-07 改动贡献 0 处 ⇒ 属预存问题。**已按用户裁定单开处置完成**：根因更正为「kernel 独立 crate 化（`3578b4e8`, 09-14）后未再跑 rustfmt，而 CI 该步已改指 `../kernel/Cargo.toml`」——非 rustfmt 版本/配置漂移（佐证：`src/rust` 侧 `cargo fmt --check` 实测 0 差异）；整改落 `style(kernel)` 一笔，含折行连带项（`framework/ipc/msgq.rs` 的 `semicolon_if_nothing_returned` 分号、`host-tests/ipc_strategy_registration_test.rs` 脆测试标记去尾随 `()`），改后 §2.3 六门槛复跑全过（fmt 0 差异 / kernel host 749-0 / QEMU 489-489 / boot 双架构 1-1）。
  - 详情（A 类之后的剩余批次）
    - A′ 类 5 组（已完成，见下条）→ C 类 16 组（先迁源侧 `cfg(test)` 再删注册副本，最大批为 `pwm::policy` 23 组/45 条）。按裁定节拍，每批完成后停下确认。
  - 详情（A′ 类施工完成 —— 5 组 / 17 用例 / 86 条断言）
    - `idt::types`（8 用例）：`framework/tests/idt.rs` 删除整组 + 随之失效的 `use crate::framework::idt::{ErrorFlags, GDT_KERNEL_CODE, IDT_ENTRIES, IDT_TYPE_INTERRUPT, IRQ_BASE, IdtEntry, IdtPtr, InterruptFrame, InterruptStatistics, get_exception_name, get_irq_name}` 整块；`framework/idt/types.rs` 删除未被调用的 `register_idt_types_tests` 转发 shim。源侧 `framework/idt/types.rs` 的 8 个 `#[test]` 为唯一归属（源侧另含更强项：`test_user_mode_detection` 的 RIP 低半区回归防护断言）。
    - `page_fault`（3 用例）：`framework/tests/test_new_features.rs` 删除整组 + 空节横幅「缺页异常 / 按需分页」；源侧 `framework/mm/page_fault.rs` 3 个 `#[test]`（`PfResult as u8` 与注册侧 `as u32` 值等价；源侧另有 `test_stack_expansion_candidate` 两条更强断言）。
    - `net::e1000`（4 用例）：删除整组 + 4 个专属 helper fn + 随之失效的 `E1000*` / `KERNEL_BASE` / `Driver` / `check` 导入；源侧 `framework/driver/net/e1000.rs` 4 个 `#[test]`（`virt_to_phys` 已按 UT-06 修正为 `KERNEL_BASE + 0x12345678`，强于注册侧会下溢的低地址形态）。
    - `lib::string`（13 用例）：注册侧 `framework/tests/string.rs` 为该命名空间唯一内容 ⇒ **整文件删除**，并同步删 `framework/tests/mod.rs` 的 `pub mod string;` 声明与 `string::register_tests();` 调用（F9 不留空壳）；`framework/lib/string.rs` 删除未被调用的 `register_string_tests` 转发 shim。源侧 13 个 `#[test]` 为唯一归属。
    - `mmap`（1 用例）：删除 `test_prot_to_vma_flags`（因函数私有 → 只测 `PageFlags` 位运算的**弱代理**）+ 空节横幅「mmap syscall」；源侧 `services/mm/mmap.rs::test_prot_to_flags` 直调真实 `prot_to_vma_flags` ⇒ 删注册即**覆盖增强**（与 DECISION-080 第 4 条所指 B09-19 弱化同型、方向相反）。
    - 断言净增不净减核验：删注册侧 86 条（逐例理由见「分类结果」），源侧断言数不变且逐组 ⊇ 注册侧 ⇒ 无覆盖损失。
    - 改动面：7 文件（6 改 1 删），`+10 / -468` 行。
  - 详情（A′ 类验证实测 —— 六门槛本机复跑）
    - `./ci/build.sh all`：**Passed: 5 / Failed: 0**。
    - `cargo fmt --manifest-path ../kernel/Cargo.toml -- --check`：**0 差异**（新写代码 rustfmt 合规）。
    - `make test-kernel-host`（= `cargo test --features host-test --lib`）：**749 passed / 0 failed**（`#[cfg(test)]` 用例数不受注册侧收敛影响，与 A 类后一致）。
    - clippy kernel_test 维（按 `ci-x86.yml` 实际写法：host target + `--lib` + `cast_*` 豁免）：**0 error / 0 warning** —— 该维是 `tests/idt.rs` 的唯一编译入口，本次删除导入块未留 unused import。
    - `./ci/build.sh aarch64` + `./ci/audit.sh quick`：exit 0，核心审计全过。
    - `make` + `make test-unit`（QEMU）：**489 → 464，0 failed / 0 skipped**（-25 = 8+3+1+13，与逐组明细吻合）。
    - `./scripts/qemu_boot_test.sh x86_64|aarch64`：各 **1/1** 通过。
  - 详情（A′ 类期间发现的预存问题 —— 已按裁定修复，§12.5）
    - `framework/tests/net.rs` 的注册**从不生效**：`tests/mod.rs` 的 `pub mod net` 声明与 `net::register_tests()` 调用均在 `#[cfg(feature = "kernel_test")]` 块内，而文件内条目门控 `#[cfg(not(feature = "kernel_test"))]` ⇒ 两分支互斥，kernel_test 下 `register_tests()` 为空函数、非 kernel_test 下模块不存在。
    - 用户裁定：**本轮一并规划并修复**（裁定门控字符串 + 删除零断言空壳用例）。处置：
      - `net::e1000` 组按 A′ 计划删除；残留 `net::utils` 的 `hton_ntoh` / `mac_formatting` 两例**零断言且其工具函数全库已无定义**（grep 确认 `fn hton/ntoh/mac_format` 不存在）⇒ 删空壳。
      - 剩余 `net::utils::byteorder` 为纯逻辑（仅比较 std `u16::to_be` / `to_le`），无裸机依赖 ⇒ 按 E-03「纯逻辑测试模块双端编译」约定，将 `pub mod net` 门控由 `kernel_test` 专属改为 `any(kernel_test, host-test)`，`net::register_tests()` 调用从 kernel_test 块移入 any 块；文件内条目改为无门控（模块门控即足够），双版本 `register_tests()` 合并为一。
      - 效果：`net::utils::byteorder` 首次**真正注册**（QEMU 侧 +1），文件不再是 `not(kernel_test)` 死岛。
  - 详情（C 类施工完成 —— 16 组，分 8 批落地）
    - 处置形态（两类）：
      - **源侧已有 `#[cfg(test)]` 模块者**（`sync::mutex` / `sync::rwlock` / `arch::tss` / `timer::pit` / `chitin::devtree` / `driver::ata` / `driver::keyboard` / `driver::framework` / `idt::handlers` / `idt::safety` / `framework::credo::sha256` / `framework::credo::types` / `services::credo::policy`）：把注册侧独有断言按源侧口径补入源模块的 `#[cfg(test)]`，再删注册侧整组 + 随之失效的转发 shim / 专属 fn / 导入与空节横幅（F9）。
      - **源侧 `#[cfg(test)]` 数为 0 或形态非 cargo 可发现者**：`framework/fs/vfs/types.rs` 新建 `#[cfg(test)] mod tests`；`barrier/reset/{audit,bbr,bsr,parallel}.rs` 把原 `#[cfg(feature = "kernel_test")] pub mod tests { pub fn .. -> bool }`（host 不编译、非 `#[test]`）改写为 `#[cfg(test)] #[test]`。
    - 批次与注册侧删减（用例名数，合计 **102**）：
      - C1（18）：`sync::mutex` 4 / `sync::rwlock` 6 / `arch::tss` 5 / `timer::pit` 3。
      - C2（15）：`devtree` 2 / `driver::ata` 6 / `driver::keyboard` 7。
      - C3（5）：`driver::framework` 5。
      - C4（9）：`idt::handlers` 8 / `idt::safety` 1。
      - C5（20）：`pwm::audit` 1 / `pwm::sha256` 16 / `pwm::types` 3。
      - C6（23）：`pwm::policy` 23（最大批）。
      - C7（5）：`vfs::types` 5。
      - C8（7）：`barrier::audit` 2 / `bbr` 2 / `bsr` 1 / `parallel` 2 —— 经用户裁定**连带全量收敛 reset 子树**，`framework/tests/mod.rs` 的 `pub mod reset` 门控由 `kernel_test` 专属改为 `any(kernel_test, host-test)`，`reset::register_tests()` 调用一并移入 any 块（`config::tests` 本就是 any 双端）。
    - 源侧新增 `#[test]` 增量（分 8 批，合计 **+59**）：C1–C4 **+9**（mutex 1 / rwlock 3 / ata 1 / keyboard 1 / framework 2 / safety 1；`tss`、`pit`、`devtree`、`handlers` 为在既有 test 内补断言，不新增 fn）→ C5 **+14**（sha256 10 / types 4）→ C6 **+23**（policy）→ C7 **+5**（vfs::types）→ C8 **+8**（audit 2 / bbr 2 / bsr 1 / parallel 3）。
    - 改动面：30 文件（27 改 3 删 —— `tests/driver.rs`、`tests/sync.rs`、`tests/test_credo.rs` 整文件删除），`+722 / -1590` 行。
  - 详情（C 类验证实测 —— 六门槛本机复跑）
    - `./ci/build.sh all`：**Passed: 5 / Failed: 0**。
    - `cargo +nightly fmt --check`：**0 差异**（C 类新写代码首轮有 3 处 rustfmt 差异，已格式化）。
    - `make test-kernel-host`（= `cargo test --features host-test --lib`）：**808 passed / 0 failed**（749 → 808，+59，与上条逐批增量吻合）。
    - clippy `kernel_test` 维（host target + `--lib` + `cast_*` 豁免）：0 error / 0 warning；`./ci/audit.sh quick` 的 `kernel_test` + `host-test` 两维同样通过。
    - `./ci/build.sh aarch64` + `./ci/audit.sh quick`：exit 0，核心审计全绿（SAFETY 覆盖 1980/1980、6 不变式、I-43/I-16/I-07/TD-22 0 违规、FP-06 0、双架构 check、clippy 两维）。
    - `make` + `make test-unit`（QEMU）：**464 → 360，360/360 ALL TESTS PASSED，0 failed / 0 skipped**。删减侧 102 个用例名与上条逐批吻合；另有 `net::utils::byteorder`（A′ 类 net 门控修复）首次真正注册（+1）；基数内 3 条重名注册随删清理。注册表**无静默丢弃**（`MAX_TESTS = 640` 富余）。
    - `./scripts/qemu_boot_test.sh x86_64|aarch64`：各 **1/1** 通过（x86_64 Ring 3 / aarch64 EL0 + KPTI-09 双架构断言均过）。
  - 详情（C 类期间的自造问题修复 —— §9.3）
    - `framework/chitin/mod.rs`：C2 删除 `devtree` 注册组后，`pub(crate) use devtree::devtree_create_node_impl;`（注释明写"供同 crate 测试使用"）失去唯一使用点 ⇒ 触发 `unused_imports`（F5）。该 `pub(crate)` 项在 `services/chitin/devtree.rs` 经**全路径** `chitin::devtree::devtree_create_node_impl` 访问，不依赖此 re-export ⇒ 删除该 re-export 与其注释。
    - `framework/credo/sha256.rs`：C5 迁入注记的续行（原 `single_byte / boundary_55/... / multi_block / ...`）为**纯英文段落** ⇒ TD-22（F7）违规，改写为含中文描述的单行注记。
    - `framework/fs/vfs/types.rs`、`framework/tests/test_pwm.rs`、`framework/tests/idt.rs`：C 类新写代码的 rustfmt 差异 3 处 ⇒ `cargo +nightly fmt` 归一。
  - 详情（C 类期间登记项 —— 计划与实际偏差 / 待报告，不擅改）
    - `driver::framework`：计划栏（该条分类清单）列"删注册侧 3 组"，实际按收敛（等价即删）原则删 **5 组**（`device_info_builder` / `device_info_creation` / `device_types` / `error_codes` / `result_type`）—— 多出的 2 组断言同属源侧 `driver/framework.rs` 已有覆盖面，删除不降级。
    - `pwm::audit`：计划栏写目标文件 `services/credo/audit.rs`，实际 3 条断言归入 **`framework/credo/types.rs`** —— 注册侧 `audit_entry` 的判据对象（`AuditAction`/`AuditResult`）定义在 `framework::credo::types`，归源侧对应文件即"就近原则"。
    - C8 连带扩面：计划栏原本只要求 `barrier::audit` 一组，经用户裁定扩为 reset 子树 4 组（`audit`/`bbr`/`bsr`/`parallel`），已在上条登记。
    - C8 占位型用例无断言登记：`bbr::compute_fingerprint`（形参为 `&PanicInfo`，stable 无法构造 ⇒ 保留"可调用性占位"语义，空体 + 注释）、`bsr::freeze_unfreeze`（仅簿记调用，原返回恒 `true`）、`parallel::compute_layers`（原判据 `count > 0 || true`）—— 三者本质为"可调用性占位"而非断言，未以 `assert!(true)` 伪装（会触发 clippy `assertions_on_constants`），属"注册侧断言数为 0"先例（类比 A 类 `rcu` / `kmalloc_slab`）。
    - 待报告（预存，非本次改动引入）：`framework/tests/driver_test.rs`（255 行）与 `framework/barrier/reset/layered.rs` 的 `#[cfg(feature = "kernel_test")] pub mod tests` 块**全库无引用**（grep 确认），属孤儿文件 / 孤儿模块 —— 已经用户裁定清理，见下条。
  - 详情（孤儿清理 —— UT-07 收尾）
    - `framework/tests/driver_test.rs` **整文件删除**：其形态为独立裸机程序（`#![no_std]` / `#![no_main]` / `#[no_mangle] extern "C" fn _start` / `#[panic_handler]`），非 `framework/tests/` 模块树成员（`tests/mod.rs` 无 `mod driver_test;` 声明），Cargo 无 `[[bin]]` / `[[test]]` 指向，构建规则亦无 —— 自早期"驱动示例程序"演进后即成死件；且体内引用未定义符号（`Color` 等），若被纳入编译会与真实 `_start` / `panic_handler` 冲突。Makefile 的 `driver-test` 目标同名但**只跑主内核** `build/kernel.flat` 并落日志 `driver_test_*.log`，与该文件无关。
    - `framework/barrier/reset/layered.rs` 的 `#[cfg(feature = "kernel_test")] pub mod tests` 块**删除**：`test_recovery_status()` 零断言（仅 `let _ = status.bbr_count;` 等，恒返回 `true`），全库无引用；其目标 API `get_recovery_status` 的行为断言由注册侧 `framework/tests/reset.rs` 的 `barrier::reset::status_api` 以**更强判据**覆盖（`reset_stats()` 后 bbr/bsr/bhr 计数归零），删除无覆盖损失。源文件留中文注记指向该覆盖点。
    - 验证实测（六门槛复跑）：`./ci/build.sh all` **Passed 5 / Failed 0**；`fmt --check` **0 差异**；`make test-kernel-host` **808 passed / 0 failed**；clippy `kernel_test` 维 0 warning（本次删除直接影响该维）；`./ci/build.sh aarch64` + `./ci/audit.sh quick` exit 0；`make` + `make test-unit`（QEMU）**360/360 passed，0 skipped**（与清理前一致，佐证两处均未被注册）；`qemu_boot_test.sh x86_64|aarch64` 各 **1/1**。
    - UT-07 至此收口：A 类 10 组 / A′ 类 5 组 / C 类 16 组均已收敛，B 类 4 组按裁定保留 `kernel_test` 载体，无残留待办 ⇒ 状态置 `[X]`。

- **UT-08. §2.3 六门槛全跑 + CI 接入**
  - 描述：`./ci/build.sh all`、`./ci/audit.sh quick`、`make test-host`、`make test-unit`、`./scripts/qemu_boot_test.sh all` + 新第 6 条 host 内核单测。
  - 方案：CI 侧接入点待定 —— 候选为 `ci/build.sh` 的 host tests 步骤内并联，或 `.github/workflows/ci-x86.yml` 新增一步；须先实测耗时再定，避免阻塞。
  - 状态：[X]
  - 详情（CI 接入点选定 —— ci-x86.yml 新增独立 job，未改 build.sh）
    - 候选二选一实测后选定 **`ci-x86.yml` 新增 job `kernel-host-tests`**（Job 4，原 clippy job 顺延为 Job 5）。理由：① `grep` 实测三个 workflow **无一处调用 `ci/build.sh` / `ci/audit.sh`**，故接入 build.sh 只能服务本地、无法形成 CI 强制；② 独立 job 与既有 job 并行，不延长 `host-tests` job 时长（无阻塞风险）。
    - 实测耗时（本机）：冷编译 18–20s + 运行 20s ≈ 40s；热态 `make test-kernel-host` 全程 41s ⇒ 未构成阻塞，不再考虑并入既有 job。
    - 形态：`working-directory: src/kernel`（必须在该目录内执行以加载其 `.cargo/config.toml`）；缓存 `src/rust/target`（独立 key `kernel-host-tests-*`，与 clippy job 隔离以免并发 save 冲突；该目录即 `src/kernel/.cargo/config.toml` 的 `target-dir`）；`PIPESTATUS` 捕获 cargo 真实退出码（B01-25 fail-open 模式）；日志落 `build/log/kernel_host_tests.txt` 并上传 artifact。
    - 依赖确认：host 构建**不需要** `build/` 裸机产物（UT-10 的 `target_os = "none"` 判据与 `build.rs` 的 G-06 判据均为裸机专属）⇒ CI 干净 checkout 可直接跑；`RUSTFLAGS: -D warnings`（workflow 全局）下实测 0 error / 0 warning。
    - 附带同步：`ci.yml` 合规报告的两处描述文本补「内核 host 单测」。
  - 详情（验证实测 —— 六门槛全跑，本机）
    - ① `./ci/build.sh all` → **Passed: 5 / Failed: 0**；② clippy pedantic（lib）+ `kernel_test` / `host-test` 两维 → 全过；③ `./ci/audit.sh quick` → 全绿（含 FP-06 aarch64 白名单外 FP/SIMD = 0；该步前置需先 `./ci/build.sh aarch64`）；④ host-tests 随 ① 通过；⑤ `make test-unit` QEMU **ALL TESTS PASSED (exit 33)** + `./scripts/qemu_boot_test.sh all` **2/2 通过**（x86_64 Ring 3 / aarch64 EL0 + KPTI-09 双架构断言均通过）；⑥ `make test-kernel-host` **749 passed / 0 failed**。
    - 计数口径修正：UT-06 记载的 748 现为 **749**，差额 1 例为 `PolicyEngine::check` 零下限域回归测试（P1 修复时随修新增，见 `services/credo/policy.rs::tests::policy_zero_floor_domain_allowed`）。
  - 详情（本轮暴露的预存易用性缺陷，登记不擅改 —— §12.5）
    - 非规范顺序下 `make test-unit` 失败：`test-unit` 的 prereq 序列为 `build/kernel_test.bin user`，而 `build/kernel_test.bin` → `RUST_LIB_TEST` → `build.rs` 要求 `build/user/init.bin` **已存在** ⇒ 若构建树刚被 arch-switch-clean 清空（例：先 `./ci/build.sh aarch64` 再 `make test-unit`），make 会先编 lib 后建 user 产物，panic 于 `build.rs:10`。
    - 影响面：规范流程（先 `make`）与 `qemu_boot_test.sh`（`sync_make_state` 内部先 `make all`）均不受影响；仅"跨架构切换后直接 `make test-unit`"触发。是否调整 prereq 顺序或显式加 `user` 前置，待用户裁定。

- **UT-09. 文档回写**
  - 描述：B09-19 补方向变更指针与状态；`docs/plan/progress-active-tasks.md` 同步；本文件 UT 状态回写。
  - 状态：[X]
  - 详情（回写范围与判据）
    - `audit-fix-09-hard-rules-deadcode.md` B09-19：方向变更指针早前已落盘（该条「方向变更（本轮，用户重新裁定）」段）；本次补**状态注记**（承接工程进度），原文与历史验证记录保持不动。
    - `progress-active-tasks.md` 工程计划 C：把**规范性**表述「§2.4 验证门槛 5 条」同步为现行 **§2.3 验证门槛 6 条**（AGENTS.md 现文），并在活跃文档全景表补一行登记本工程。
    - 判据（历史记录 vs 规范性表述）：该文件内**历史验证记录**行（形如「§2.4 5 条门槛全过 ... host-tests 838 passed」）**不改** —— 它们记录当时实跑口径，改写即伪造历史；只改**当前生效的规范引用**。
    - 本文件：UT-08 / UT-09 置 `[X]`；UT-07 保持 `[]`（收敛清单待出，见该条详情）。【订正】UT-07 已于后续批次收口（A 类 10 组 / A′ 类 5 组 / C 类 16 组全收敛）并置 `[X]` ⇒ 本条 `[]` 记载随之核销。

- **UT-10. host 侧编译裸机产物依赖消除（G-06 漏项，本轮插入项）**
  - 描述：UT-01 验证过程中实测发现：host 侧编译（host-tests / 新第 6 条门槛 / `ci/audit.sh` 的两个 host clippy 维）**硬依赖裸机产物**。经用户裁定「本轮一并修复」。
  - 方案：把两处裸机产物 `include_bytes!` 改为「绑定处 cfg 分叉 + 空切片桩」，判据用 `target_os = "none"`（而非 feature）。
  - 状态：[X]
  - 详情（两处点位与形态）
    - `framework/syscall/dispatch.rs`（`sys_boot_install` 内的 `build/stage1.bin`）、`framework/proc/api.rs`（`launch_first_user_process` 的 `not(feature = "initramfs")` 块内的 `build/user/init.bin`）。
    - 形态选定为**绑定处 cfg 分叉**（`#[cfg(target_os = "none")] let x = include_bytes!(..)` / `#[cfg(not(target_os = "none"))] let x: &[u8] = &[];`），**不能**改为「整块/整函数 cfg 排除」——`launch_first_user_process` 返回 `!`，须保留块内 `bin_size == 0 → qemu_exit(false)` 的分歧路径，否则 host 侧类型检查失败。
    - 与既有 `mod.rs` 的「符号桩化」形态（`#[cfg(not(feature = "host-test"))]`）刻意不同，理由已写入源码注释：符号引用是「host 无该链接器符号」，产物引用是「非裸机构建不需要该产物」——后者用 `target_os` 一次覆盖**所有** host 维度（含 `kernel_test` 维），feature 判据只能覆盖 `host-test` 一维。
  - 详情（判据覆盖面 —— 为什么不是 feature）
    - `ci/audit.sh` 步骤 2b 以 `--features kernel_test` 与 `--features host-test` **在 host target 上**各跑一次 clippy（无 `--target`）。若判据取 `not(feature = "host-test")`，则 `kernel_test` 维仍会编译产物引用 ⇒ 该维在干净 checkout 上依旧失败。
    - 未在 `kernel_test` 维上排除「裸机 kernel_test 构建」语义：`target_os = "none"` 在裸机构建为真，故 `build/kernel_test.bin`（`make test-unit`）的内嵌 init 行为**完全不变**；仅 host 维度走空切片。
  - 详情（验证实测）
    - 移走 `build/stage1.bin` + `build/user/init.bin` 后：① host-test 维 `--no-run` **无 `couldn't read`**（错误数 122 → **121**，降幅即被消除的产物缺失错误）；② `kernel_test` 维 host clippy 通过；③ `host-test` 维 host clippy 通过。
    - 裸机路径回归：`./ci/build.sh all` 复跑 → **Passed: 5 / Failed: 0**（双架构 build + host-tests + forbidden patterns + link）⇒ `target_os = "none"` 判据未影响裸机构建与 `make test-unit` 语义；`./ci/audit.sh quick` 见 UT-08 全量复跑。

## 验证门槛

- **双架构 0 warning / 0 error**：`./ci/build.sh all`（删 `test = false` 不影响 `crate-type`，未验证前不得假设 —— 列为必跑项）。
- **§2.3 第 6 条（新增）**：`cd src/kernel && cargo test --features host-test --lib` 0 failed。
- **既有五条不回归**：`./ci/audit.sh quick` / `make test-host` / `make test-unit`（QEMU）/ `./scripts/qemu_boot_test.sh all`（双架构）。
- **断言净增不净减**：复活的 770 例中被删除的断言须逐例登记理由（重复副本 / 私有 API 无等价 / host 不适用）。
- **F9 死代码零容忍**：被收敛删除的注册组副本不得留空壳函数或 `#[allow(dead_code)]`。

## 风险与回退

- **E0152 可能以其他模式复现**：E1 只证伪了 `cargo test --features host-test --lib`（host 目标）这一模式。若删 `test = false` 后在 `cargo check --tests`、CI 既有 job 或其他 target 下复现，则保留 `test = false` 并改用**显式 test target**（`[[test]]`）或其他入口。**UT-01 必须以全量 CI 复跑收尾**。
- **运行期失败面未知**：770 例首次真正执行，失败数未知；UT-06 专设裁决步。若失败集中于 host 环境不适用者，则按 `#[cfg]` 门控而非删断言。
- **双份维护窗口期（已关闭）**：UT-07 未完成前，约 72 文件存在两份断言。窗口期内**修改任一契约必须同步两处**；该约定写入本文件，避免漏改。UT-07 收口后双份断言已逐例收敛为一处 ⇒ 窗口期关闭，本条不再适用。
- **与 AGENTS §8 的关系**：本工程恢复「单元测试可测私有实现」，§8「测用户可见行为」的约束对象是集成测试（`host-tests/`）。若需在 AGENTS 中澄清，属 UT-09 的可选动作，须先经用户确认（不改用户规则文件原文）。
- **§12.5 处置边界**：UT-02..UT-06 中凡判定为「实现回归」者**一律登记不擅改实现**（列入候裁清单交用户）。
- **回退**：恢复 `[lib] test = false` + 撤销 UT-01 的门槛条目 ⇒ 回到当前状态；UT-02..UT-05 的测试侧改动为独立可回退项（不触生产代码）。
- **host 侧编译隐式依赖裸机产物（本轮实测发现并已修复，预存缺陷）**：`framework/syscall/dispatch.rs`（`build/stage1.bin`）与 `framework/proc/api.rs`（`build/user/init.bin`）两处 `include_bytes!` 使 host 构建（既有门槛 4、新第 6 条门槛、`ci/audit.sh` 的两个 host clippy 维）**硬依赖** `build/` 下被 gitignore 的裸机产物 ⇒ 干净 checkout 上 host 侧编译失败。G-06（archive/audit-fix-08-user-build-docs.md）的目标正是「消除隐式 make 耦合」，但其修复只覆盖 `build.rs`，漏了这两处 `include_bytes` ⇒ 该耦合实际未消除，且随本工程新增第 6 条门槛而扩大影响面（门槛 4 → 6）。**已按用户裁定于本轮修复，判据 `target_os = "none"`，验证与形态见 UT-10。**
- **同类残余（未纳入本轮，登记待裁）**：上述两处之外，`framework/proc/api.rs` 的 `initramfs` 分支引用 `build/user/initramfs.cpio`（`#[cfg(all(target_arch = "x86_64", feature = "initramfs"))]`）。该 feature 默认关闭、两个 host lint 维与 host-tests 均不启用，故当前不构成失败；但若将来在 host 上启用 `initramfs`，会重现同类缺陷。是否一并桩化待用户裁定。