# 消除 host-tests 平行实现（内核源码 host 可编译根治）

> 用户决策：直接上路线 C（内核 crate 增加 `host-test` feature + privileged std 桩），让 host-tests 直接引用内核 functions 真实源码，彻底消除全部 7 处平行实现（unkfs 被测对象 / dma_stream / buddy / capability / checksum / sha256 / framekernel_bench 复刻）。来源：[audit-fix-08](./archive/audit-fix-08-user-build-docs.md) H.3.6 P0-26 + H.3.7 P0-27 相关条目。

## 工程计划 A: 内核 crate host-test 编译基建

### 背景

- **现状障碍**
  - 描述：[src/rust/Cargo.toml](file:///home/anfer/Code/Edgine/src/rust/Cargo.toml) 中 `[lib] crate-type = ["staticlib"]`、`test = false`、`[profile.*] panic = "abort"`；[lib.rs](file:///home/anfer/Code/Edgine/src/rust/src/lib.rs) 顶层 `#![no_std] #![no_main]` + `#![feature(alloc_error_handler)]`。这些使内核 crate 无法在 host 环境编译/测试。
  - 方案：新增 `host-test` feature，用 `cfg(feature)` 门控剥离裸机专属约束。
  - 状态：[X] (已解决：`host-test` feature 已加，`crate-type`/`test`/`panic` 三项障碍经 cfg 门控与独立 profile 剥离，裸机双架构构建不受影响——实测见本计划 A「待办」三条与 §验证门槛)

- **host 可编译性已被证明**
  - 描述：[host-tests/src/unkfs_mock.rs](file:///home/anfer/Code/Edgine/host-tests/src/unkfs_mock.rs) 已用极小模拟面（std Mutex + KernelError + 5 个 extern 桩）让 unkfs 平行实现在 host 编译，证明"内核风格代码 host 编译"可行。
  - 方案：将这套桩机制升级为内核 crate 自身的 host-test 实现，替换平行实现。
  - 状态：[X] (已落地：该桩机制未被升级复用——工程计划 A 可行性验证证明**全 crate host 编译零障碍**，故改为「内核 crate 自身 + `host-test` feature」直接承载，`unkfs_mock.rs` 与平行 unkfs 已随 B08-14 一并删除)

### 待办

- **Cargo.toml host-test 配置**
  - 描述：`crate-type` 需支持 rlib（host 测试链接）；`test` 需可启用；`panic = "abort"` 与测试断言冲突（host 下需 unwind 或经测试 harness 处理）。
  - 方案：`[features] host-test = []`；`crate-type` 加 `"rlib"`（裸机仍 staticlib）；host-test profile 用 `panic = "unwind"`（独立 profile 或 feature 内 profile 覆盖）；`[lib] test` 经 cfg 无法直接切换——评估用 `--cfg host-test` 编译 + 独立 test target 替代 `lib.test`。
  - 状态：[X] (2026-09-06 实施完成：`[features] host-test = []` 已加；`crate-type = ["staticlib", "rlib"]`——裸机 Makefile 仍取 staticlib 不受影响，rlib 供 host-tests path 依赖链接；`[lib] test = false` 保持（host-tests 为独立测试 crate，不依赖本 crate test 配置）；`panic = "unwind"` profile 推迟到 E-04 测试运行器阶段（host-tests 根 profile 可覆盖依赖 panic 策略，届时按需设))
- **lib.rs 顶层门控**
  - 描述：`#![no_main]`、`alloc_error_handler`、`#![feature(...)]` 在 host-test 下不可用或多余。
  - 方案：`#![cfg_attr(not(feature = "host-test"), no_main)]`；alloc_error_handler 包 `#[cfg(not(feature = "host-test"))]`；顶层 feature gate 按 host-test 分流。
  - 状态：[X] (2026-09-06 实施完成：`no_std/no_main/feature(alloc_error_handler)` 三顶层约束 cfg_attr 门控；`panic_handler`/`alloc_error_handler` 加 `#[cfg(not(feature = "host-test"))]`；`extern crate alloc` 保持无条件（std 模式下经 extern prelude 可见；此前观察的 E0152 duplicate lang item 是 src/rust/.cargo/config.toml build-std 配置在 src/rust 目录内运行的假象，host-tests 从仓库根/host-tests 构建时不加载该 config）；panic_handler 私有符号（PanicInfo/Ordering/write_hex_to_buf）同步门控消除 host-test 下 dead_code 警告)
- **host-test 目标可行性验证**
  - 描述：在实施前先验证最小目标：`cargo check --features host-test`（默认 host target）能否编译内核 crate 骨架。
  - 方案：先只门控顶层约束，跑 `cargo check`，暴露第一批裸机依赖（asm/MMIO/arch 模块）清单，作为工程计划 B 的输入。
  - 状态：[X] (2026-09-06 实施完成：**0 error 0 warning**。关键发现——**全 crate（privileged 裸机代码含 asm/MMIO/arch）在 host 编译期零障碍**：`core::arch::asm!`/volatile 访问在 host 可编译（仅运行期会执行特权指令崩溃），extern FFI 符号在 cargo check 不链接不报错。这大幅降低工程计划 B 范围：编译层桩基本不需要，仅运行期语义桩（IrqSpinLock 中断禁用、MMIO 访问等）在 C 迁移测试时按需补充。注意事项：必须从仓库根或 host-tests 目录以 `--manifest-path` 运行，避免 src/rust/.cargo/config.toml 的 build-std 在 host target 下重复构建 core/alloc（E0152）)

### 验证门槛

- **宿主编译骨架**
  - 描述：`cargo check --features host-test`（std target）0 error。
  - 方案：作为阶段 1 完成标准。
  - 状态：[X] (2026-09-06 实测：`cargo check --manifest-path src/rust/Cargo.toml --features host-test` 0 error 0 warning，全 crate 骨架 host 编译通过；工程计划 A 完成)

## 工程计划 B: privileged 机制层 std 桩

### 背景

- **functions 依赖面**
  - 描述：functions 对 privileged 依赖 400+ 处（syscall 84 / fs 43 / sync 41 / proc 37 / mm 31 / driver 27 / sgeg 15 / config 10 / iomem 9 …），依赖的 privileged 机制层（IrqSpinLock/分配器/进程表/MMIO 封装）在 host 下无裸机实现。
  - 方案：为 functions 实际依赖的 privileged 公共 API 提供 host-test 下的 std 桩实现（mock 语义），按依赖面分批。
  - 状态：[X] (2026-09-06 范围修正：工程计划 A 可行性验证证明**编译层零障碍**（全 crate host 编译通过，含 asm/MMIO/arch 模块——仅运行期会执行特权指令）。故本背景的"无裸机实现"仅指**运行期语义**，桩需求大幅收缩：纯算法/表结构 API（checksum/sha256/位图/表查询）直接用真实实现即可；仅 IrqSpinLock 中断禁用语义、MMIO 访问等运行期特权路径需要 std 桩)
- **与框架架构红利**
  - 描述：functions 层 0 unsafe、无架构依赖；privileged/functions 单向依赖已由 `audit_functions_boundary.py` 门禁保障——host 桩只需覆盖 privileged 顶层公共 API（re-export 面），不必覆盖内部模块。
  - 方案：以 `SAFE_PRIVILEGED_APIS`（审计脚本 allow-list，见分册 01）为桩覆盖清单的权威来源。
  - 状态：[X] (已定型为常驻约定：桩覆盖清单以 `SAFE_PRIVILEGED_APIS` 为权威来源；本项无独立待办动作，C 迁移时按实际触达的 API 分批补桩)

### 待办

- **sync 桩先行**
  - 描述：functions 41 处依赖 privileged::sync（IrqSpinLock/Mutex/OnceCell/原子），host 下需 std 替代。
  - 方案：host-test cfg 下 `privileged/sync` 提供 std 实现（Mutex→std::sync::Mutex，OnceCell→std::sync::OnceLock），仿 unkfs_mock 模式；保留中断禁用语义为 no-op（host 无中断）。
  - 状态：[X] (2026-09-06 实施：`privileged/sync/spinlock.rs` 的 `disable_interrupts`/`restore_interrupts` 加 `#[cfg(feature = "host-test")]` no-op 变体——host 无中断语义且 cli 特权指令在用户态 SIGSEGV；IrqSpinLock/SpinLock 的原子自旋在 host 多线程下仍正确互斥。`OnceCell`（privileged::sync::OnceLock）为原子 Once 实现，host 原生兼容无需桩。**可行性验证**：临时探针测试调用内核 `functions::fs::unkfs::zap::UnkfsZap`（含 IrqSpinLock Mutex）在 host 运行通过，证明内核 unkfs 纯逻辑模块可 host 运行)
- **fs/syscall/proc/mm 桩分批**
  - 描述：functions 依赖的 fs(43)/syscall(84)/proc(37)/mm(31) 公共 API 需 host 桩（多数为"表结构 + 查询"类，可 mock）。
  - 方案：按 `cargo check --features host-test` 暴露的缺失清单分批实现桩；纯算法类 API（checksum/sha256/位图）直接用内核真实实现，不桩化。
  - 状态：[X] (已定型：编译层无缺失清单（全 crate host 编译通过）；余量仅为运行期触达路径的桩，随 C 批次按需补齐，无独立待办动作)
- **裸机专属模块 cfg 隔离**
  - 描述：privileged 的 arch/asm/MMIO/IDT 等模块在 host-test 下必须整体 cfg 掉（functions 不依赖它们的内部，只依赖顶层 re-export）。
  - 方案：host-test feature 下 `privileged/arch`、`privileged/idt`、`privileged/iomem` 等提供空/桩模块，保证顶层 re-export 符号可解析。
  - 状态：[X] (已定型：编译期已证无需 cfg 隔离（模块全部可编译）；运行期仅对实际触达的 MMIO/asm 路径在测试侧 stub 或避开，不做大规模 cfg 重构)

### 验证门槛

- **functions 全量 host 编译**
  - 描述：`cargo check --features host-test` 下 `functions/` 全部模块编译通过。
  - 方案：阶段 2 完成标准。
  - 状态：[X] (2026-09-06 实测：`cargo check --features host-test` 0 error 0 warning，functions 全模块 host 编译通过——工程计划 B 编译层门槛达成)

## 工程计划 C: 平行实现迁移与删除（全部 7 处）

### 背景

- **平行实现清单**
  - 描述：host-tests/src/ 下 7 处复刻：unkfs/（19 文件，被测对象）、dma_stream.rs（自认复刻 dma_buf）、buddy.rs、capability.rs、checksum.rs、sha256.rs、framekernel_bench.rs（10 个内核算法复刻）。
  - 方案：按依赖面从易到难迁移，逐处删除平行实现，测试改指内核真实源码。
  - 状态：[X] (7/7 已归零：sha256/checksum/capability/dma_stream 于 B08-12/B08-20 迁移；unkfs 于 B08-14 删除；buddy 于 H-04 (2026-09-09) 处置；framekernel_bench 改引内核真实实现。实测 `host-tests/src/` 仅剩 `dma_stream.rs`（无 privileged/tests 重叠，唯一覆盖）+ `framekernel_bench.rs`（G-07 收尾轮已清零 host-only mock，见下「迁移 framekernel_bench」）+ `lib.rs` + `fsx.rs`（宿主侧 `std::fs` 工具）+ `bin/`)

### 待办

- **host-tests 改为 path 依赖内核 crate**
  - 描述：host-tests 的 `crate::kernel::*`（unkfs_mock 虚拟树）改为引用真实内核 crate（`edgine::kernel::*`）。
  - 方案：host-tests Cargo.toml 加 `edgine = { path = "../src/rust", features = ["host-test"] }`；`unkfs_mock.rs` 降级为仅保留 std 适配（或删除，若内核自带桩）。
  - 状态：[X] (2026-09-06 实施完成：host-tests Cargo.toml 加 `edgine = { path = "../src/rust", features = ["host-test"] }` path 依赖。**链接触发符号冲突修复**——unkfs_mock 5 个 no_mangle 桩（timer_get_ticks/ata_*×3/klog_ffi_info）与内核真实 FFI 符号重名导致 duplicate symbol，经桩 `#[export_name]` 改名 `edgine_host_mock_*` + 平行 unkfs extern 声明 `#[link_name]` 指回桥接（保留 mock 语义，host 安全）；另修复内核 `#[global_allocator]`（memory_allocator）在 host-test 下仍安装导致 std 分配走内核 kmalloc→cli SIGSEGV，lib.rs 加 `#[cfg(not(feature = "host-test"))]` 门控。桥接随 B08-14 迁移平行 unkfs 时删除)

- **迁移 sha256/checksum/buddy/capability/dma_stream（5 模块）**
  - 描述：这 5 个是纯算法复刻，对应内核真实实现（sgeg/sha256.rs、unkfs/checksum.rs、pmm.rs buddy、sgeg capability、dma_buf.rs 状态机）。
  - 方案：host-tests 的测试改为 `use edgine::kernel::...` 调用内核真实实现；删除 host-tests/src/{sha256,checksum,buddy,capability,dma_stream}.rs；`#![allow(dead_code)]`（F9 违反）随删除消失。
  - 状态：[X] (2026-09-06 实施完成 4/5：sha256/checksum/capability/dma_stream 四模块迁移完成——本地实现删除，测试改引内核真实源码，`#![allow(dead_code)]` 随删除消失；host-tests lib 测试 186 passed 全绿（含这 4 模块）+ tests/ 集成测试全量通过。**buddy 例外**：内核 pmm 基于裸指针操作真实物理内存，host 不可测，buddy 平行实现保留，已标记问题待审查员决定处置（不迁移不删除）。**E-06 延伸去重（2026-09-08，见 audit-fix-08 E-06）**：同源双编译后，capability/sha256/checksum 三载体用例进一步合入 privileged/tests 套件双端共享并删除 host-tests 侧文件；dma_stream 因无 privileged/tests 重叠且为唯一覆盖保留)

- **迁移 unkfs（被测对象）**
  - 描述：unkfs 平行实现（19 文件）删除，tests/ 226 处引用改指内核 `edgine::kernel::functions::fs::unkfs`。
  - 方案：依赖工程计划 B 的 functions host 编译；删除前先统一 UnkfsDva 布局（内核版为准）；`ffi.rs` 垫片删除；`unkfs_mock.rs` 的 kernel 树按需收敛。
  - 状态：[X] (2026-09-06 完成：6 个 unkfs 测试文件全部改引内核真实实现（unkfs_test/persist/stress/e2e/zil_replay + trait_abstract 静态契约）；**host-tests/src/unkfs/ 19 文件 + unkfs_mock.rs（虚拟内核树 + 桥接桩）全部删除**；lib.rs 收敛。B08-14 步骤 4 完成（详见 audit-fix-08 B08-14 状态）。注：文档原步骤 1"统一 UnkfsDva 布局"经调研价值存疑（tests/ 无布局断言），迁移时直接以内核 16B 布局为准，见 audit-fix-08 B08-14 详情)

- **迁移 framekernel_bench**
  - 描述：bench 复刻 10 个内核算法热点，目标"与内核版本位一致"。
  - 方案：bench 的算法调用改指内核真实实现；保留 JSON 输出与 baseline 机制；确认性能基线不因引用方式改变而失真（同算法应同结果）。
  - 状态：[X] (2026-09-06 完成：sha256/capability/dma/iomem 等热点已随 B08-12/20 改引内核真实实现；F9 `#![allow(dead_code)]` 删除 + 6 处 mock 死代码消除；`cargo check --lib` 0 warning + 81 bench 测试 passed。unkfs dispatch 等 29 个 bench 中依赖内核 host 不可测的部分保留本地 mock（bench 专用性能测量，非功能测试被测对象），见 G-07 条目)

- **G-07 收尾：剩余 host-only mock 清零**
  - 描述：G-07 登记的 host-only mock 残留（archive 记为 29 个 bench，实测 HEAD 编排器注册 28 项）中，除已迁移的热点外仍存三类内核逻辑复刻：8 组 unkfs dispatch mock（`HostZapStore`/`HostTxgManager`/`HostDmuManager`/`HostSpaManager`/`HostRaidzEngine`/`HostArcCache`/`HostZilLog`/`HostZilPersist` 及其 `StandardHostXxx` 复刻）、`MockEGDFDevice`/`HostBlockDevice` 块设备复刻、`MockVfsPollPolicy`/`MockEpollInstance`/`MockEpollPwake` 等 poll/epoll 复刻。三者均属"内核逻辑在 host 侧的平行实现"。
  - 方案：8 组 unkfs dispatch 与块设备/poll 复刻改为直引内核真实实现（`UnkfsZap`/`UnkfsTxgGroup`/`UnkfsObjSet`/`UnkfsSpa`/`UnkfsRaidzMap`/`StandardArc`/`UnkfsZil`/`UnkfsZilPersist`；`egdf_blk_read`/`egdf_blk_write` 注册表；`StandardVfsPollPolicy` + `VfsPollPolicyRef`）；无内核对应物的纯合成 bench 直接删除，不留复刻。
  - 状态：[X] (收尾完成：**10 组改引内核真实实现**（`blk_dev_dispatch` 经 `egdf` 块设备注册表；`vfs_poll_dispatch` 经内核 `StandardVfsPollPolicy` + `VfsPollPolicyRef`；8 组 unkfs dispatch 直引内核类型）；**5 组纯合成 bench 删除**（`btree_id_lookup`/`context_switch_latency`/`pipe_throughput`/`vfs_open_close`/`loopback_rtt`）；**epoll mock 块删除**（`MockEpollInstance`/`instance_watches_fd`/`enqueue_ready_for_fd`/`MockEpollPwake` + 8 个单测）；本地复刻体删除后 `bitflags` 直接依赖随之移除；bench 项 28 → 23。迁移中发现并修复 1 处本轮自造问题（`privileged/debug/ebpf.rs` `register_verifier` 持 `IrqSpinLockGuard` 重入 `verifier()` 自死锁）。验证：`cargo check --all-targets` 0 warning、`cargo test --lib` 95 passed / 0 failed、bench 运行输出 23 条、baseline.json 重录 23 条且 `check_bench_regression.py` PASS。剩余唯一测试替身为 `BenchBlockDevice`（实现内核 `BlockDevice` trait 的 bench 载具，非内核逻辑复刻）。**遗留**：`measure()` 度量口径缺陷（0ns 折叠）经用户裁定另立独立任务，见 [framekernel-bench-measure-fix.md](./archive/framekernel-bench-measure-fix.md))

- **删除完成标准**
  - 描述：host-tests/src/ 下不再存在任何与内核功能重叠的平行实现；`#![allow(dead_code)]` 清零（联动分册 09 F9）。
  - 方案：删除后全量 `cargo test`（host-tests）+ 内核双架构构建 + 既有 host-tests 用例全部通过（此时通过 = 内核源码正确性）。
  - 状态：[X] (实测：`host-tests/src/` 与 `tests/` 均无 `#![allow(dead_code)]` 属性实体（grep 命中的仅为注释/README 记述与 `nvme_ahci_activation_test.rs` 中的反向断言）；`crate::kernel` 引用 0 处；`make test-host` 全量通过 + `./ci/build.sh all` 双架构通过)

### 验证门槛

- **host-tests 全量通过（真实代码）**
  - 描述：所有 host-tests 用例改指内核源码后全量通过，且通过即验证内核真实实现。
  - 方案：`cd host-tests && cargo test` + `./ci/build.sh all`（内核不受影响）。
  - 状态：[X] (2026-09-06 实测：`cd host-tests && cargo test` 全量通过——lib 186 passed（sha256/checksum/capability/dma_stream 四模块改引内核真实源码）+ tests/ 集成测试全部通过；`cargo check --manifest-path src/rust/Cargo.toml --features host-test` 0 error 0 warning。内核裸机双架构构建回归待阶段 6 B08-17 统一验证)

- **平行实现归零**
  - 描述：grep 确认 host-tests/src/ 无复刻模块；`crate::kernel` 引用全部为 `edgine::kernel`。
  - 方案：删除完成后 grep 复核。
  - 状态：[X] (实测 grep：`host-tests/src/` 无内核逻辑复刻模块，`crate::kernel` 0 处；仅存 `dma_stream.rs` 与 `fsx.rs`（宿主侧 `std::fs` 工具，非内核逻辑复刻）；`framekernel_bench.rs` 的 host-only mock 已随 G-07 收尾轮清零，余下唯一测试替身为 `BenchBlockDevice`（实现内核 `BlockDevice` trait 的 bench 载具，非内核逻辑复刻）)

## 决策记录

- **DECISION-052**
  - 描述：彻底根治平行实现采用**路线 C**（内核 crate `host-test` feature + privileged std 桩），范围覆盖**全部 7 处**平行实现；优先于渐进式 A/B 方案。
  - 方案：理由——A/B 仅消灭算法复刻层，unkfs 被测对象级平行实现仍存在；C 一劳永逸，且 unkfs_mock 已验证内核风格代码 host 编译可行，privileged/functions 分离架构降低了桩覆盖难度。风险——privileged 机制层桩化工作量与 cfg 复杂度最高，须以工程计划 A 可行性验证为先导，逐步暴露依赖面。
  - 状态：[X] (已落地：路线 C 的 `host-test` feature、顶层门控、双架构构建回归均已实施并实测通过；7/7 平行实现归零，见上「平行实现清单」。桩工作实际量远低于预估——可行性验证证明编译层零障碍)
