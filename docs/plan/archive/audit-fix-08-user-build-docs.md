# 审计修复分册 08：用户态、构建、文档与测试

> 修复 src/user（链接脚本/汇编）、build（stage1.bin/build.rs）、src/rust 布局、docs（ref-naming）、tests（陈旧日志）与 host-tests（解耦/平行实装）的审计缺陷。来源：[code-audit-final-summary.md](../code-audit-final-summary.md) 第 3.5 节 + 附录 H（H.3.6/H.3.7/H.4.6/H.5.3）+ 附录 E。

> **2026-09-03 基线核实**：委托前对全部 18 项逐一对照当前磁盘代码核实（见各条目标注）。结论：**已修复/实装 2 项**（B08-02、B08-15）、**大部分已解决 1 项**（B08-10）、**仍存在 9 项**（B08-03/04/05/07/09/12/13/16 + B08-11 背景）、**部分修复+阻塞 1 项**（B08-06）、**硬阻塞 1 项**（B08-14，前置 B08-12 步骤0）+ 验证门槛 2 项（B08-17/18）。**已实装项标注 `[X]`，委托时跳过；仍存在/部分项为待办**。关键决策点：B08-06 依赖 kpti-complete-project（F-04 全 `[]` 未完成），需标记阻塞或与 KPTI 工程联动；B08-12 为大型独立工程（内核新增 `host-test` feature + framework std 桩），全项委托时作为重点任务；B08-14 明确在 B08-12 宿主基建（步骤0）完成后才可启动。

> **2026-09-03 全仓平行实现核查补充**：基线核实后新增 **B08-19/B08-20**（工程计划 D）——B08-19 `framework/credo/secure_boot.rs::sha256_hash` 为内核内部第二套标准 SHA-256（独立理由已被 B07-07 证伪）；B08-20 为 `host-tests/tests/` 下 31 个测试文件的内联"镜像"平行实现面（含 eash/demand_paging 算法级镜像）。均确凿未记录，纳入委托范围。**补漏扫查补充**：新增 **B08-21**（5 文件，算法级镜像 4 + 布局 1，mm_iomem_alias/zil_replay/driver_display/driver_e1000_eeprom/arch_apstartup_info_layout）；十六进制常量指纹独立复现 B08-19（SHA256 K 表跨 5 文件）；复核排除 framework/services 机制-策略拆分、services 安全代理样板、config 纯常量、函数指针解耦模式与静态契约测试；非 .rs 源文件（汇编/linker）无新增。**深层扫查补充**：新增 **B08-22**（内核内部 2 项——常数时间比较 `constant_time_eq`↔`ct_eq` 逐字节相同、ELF64 头 `Elf64Ehdr`↔`Elf64Header` 同 tree 双份 14 字段布局）；控制流骨架/注释指纹维度无新增，复现已登记 hvfs/sha256 对。

> **2026-09-05 额外工程规划**：新增 **工程计划 E（同源双编译全项目覆盖）**——四维精准调研（kernel_test 测试集合 / host-tests 集合 / host 可编译性障碍 / 255 处门控语义）后，规划同一份测试代码在 kernel_test（QEMU 真实内核）+ host-test（host std）双环境编译执行：21 个纯逻辑测试模块 + 61 处 cfg(test) 单测按 4 层迁移共享，driver_test/net 等硬件路径保持 QEMU only；完全依赖 B08-12 host-test 基建（阶段 0 前置），E 不重复造基建。

## 工程计划 A: 构建与源码布局

### 背景

- **B08-01. 构建产物与布局异常**
  - 描述：stage1.bin 全 0x00、build.rs 主动生成全 0 占位符、src/rust/lib.rs 空文件共存。
  - 方案：核实产物来源，删除占位符，显式声明 lib 路径。
  - 状态：[X] (2026-09-06 背景条目解决：子项 B08-02/03/04/05 全部实施完成——stage1.bin 确认为真实引导码、build.rs 改 panic_missing、lib.rs 空文件删除 + [lib] path、模块结构注释同步)

### 待办

- **B08-02. build/stage1.bin 全 0x00（P0-18）**
  - 描述：`build/stage1.bin`（440 字节）hexdump 全 0，multiboot2 头缺失；Makefile:217-218 依赖 `src/kernel/framework/boot/stage1.asm`。
  - 方案：核实 stage1.asm 汇编产物；与 H.5.3（build.rs 全 0 占位）联动确认 stage1.bin 是否为 build.rs 伪造；若 unused 则删除。
  - 状态：[X] (2026-09-03 基线核实：`build/stage1.bin` 现为真实引导码——hexdump `31 c0 8e d8...`，Makefile:221 由 `stage1.asm` 汇编产生，非全 0。B08-03 的 build.rs 占位仅 exists 检查，不覆盖已存在产物。审计项已修复，委托跳过)

- **B08-03. src/rust/build.rs 全 0 占位符（H.5.3 P0-33）**
  - 描述：`src/rust/build.rs` 主动创建全 0x00 占位符（P0-18 的 stage1.bin 可能由此产生）。
  - 方案：改为 `panic_missing` 或真实构建逻辑（DECISION-H15）。
  - 状态：[X] (2026-09-06 实施完成：build.rs 改 `require_exists` 按 DECISION-H15 panic_missing——产物缺失直接报错不再写全 0 占位；按 `CARGO_CFG_TARGET_ARCH` 区分架构，仅 x86_64 检查 stage1.bin（aarch64 引导走 start.S 不生成 stage1.bin，且 arch-switch-clean 跨架构切换时会删 stage1.bin，缺失属正常）；init.bin 两架构均检查。与 Makefile 产物存在顺序冲突消除)

- **B08-04. src/rust/lib.rs 空文件（P0-19）**
  - 描述：`src/rust/lib.rs`（0 字节）与 `src/rust/src/lib.rs`（33KB）共存，Cargo 解析路径依赖 target-dir/manifest。
  - 方案：删除空文件，显式 `[lib] path = "src/lib.rs"`。
  - 状态：[X] (2026-09-06 实施完成：`src/rust/lib.rs` 空文件已删除，Cargo.toml 显式 `[lib] path = "src/lib.rs"`，路径歧义消除)

- **B08-05. lib.rs 模块结构注释不含 aarch64/chitin/wasm（H.4.11 P2-B）**
  - 描述：`src/rust/src/lib.rs` 的"模块结构"注释不含 aarch64 + chitin/wasm，与代码现状不符。
  - 方案：同步模块结构注释。
  - 状态：[X] (2026-09-06 实施完成：模块结构注释同步为 kernel/framework（arch x86_64+aarch64/boot/cpu/mm/proc/idt/sync/driver/net/fs/dma/credo/chitin/barrier/wasm/syscall 等）+ kernel/services（syscall/proc/fs/net/ipc/mm/credo/barrier/chitin/driver/io/timer/wasm 等）两层结构，与 [kernel/mod.rs](../../../src/kernel/mod.rs) 实际布局一致)

- **B08-06. 用户态链接脚本 _user_start/_user_end（P0-17）**
  - 描述：`src/user/link.x`、`link_aarch64.x`、`init/link_aarch64.x` 均无 `_user_start/_user_end` 边界符号，ELF loader 无法获取用户进程内存边界。
  - 方案：见分册 02 工程计划 A F-04（KPTI 布局）一并实施；本分册负责 ELF loader 侧消费验证。
  - 状态：[B] (2026-09-03 基线核实：**部分修复 + 阻塞**——`src/user/link.x` 与 `link_aarch64.x` 已含 `_user_start/_user_end`（各 3 处），`init/link_aarch64.x` 仍缺（0 处）；分册 8 负责的 ELF loader 侧消费验证待办。**阻塞**：符号定义侧依赖 [kpti-complete-project.md](../kpti-complete-project.md)（F-04 KPTI 布局，状态全 `[]` 未完成），需与 KPTI 工程联动) (2026-09-08 状态同步：维持阻塞——跨分册依赖 KPTI 工程，非分册 8 内可独立完成项，登记待 KPTI 联动时一并处置；本分册其他全部委托工程已完成)

- **B08-07. src/user/init/src/arch/aarch64.S 死代码（H.4.6 P1-C）**
  - 描述：aarch64.S 死代码。
  - 方案：核实引用后删除或实装使用路径。
  - 状态：[X] (2026-09-06 实施完成：全仓 grep 确认无 `aarch64.S`/`global_asm`/`include_str` 引用，删除 `src/user/init/src/arch/aarch64.S`（380B 死代码）)

## 工程计划 B: 文档与测试目录

### 背景

- **B08-08. 文档漂移 + 陈旧产物**
  - 描述：ref-naming.md 立场与代码不符、tests/reports 164 个陈旧日志散落。
  - 方案：文档立场修正 + 仓库清理。
  - 状态：[X] (2026-09-08 背景条目闭合：子项 B08-09（ref-naming.md 编号 400+/700+ 修正）与 B08-10（tests/reports 清理 + .gitignore 防护）均已 `[X]` 实施完成，背景目标全部达成)

### 待办

- **B08-09. ref-naming.md 500+ 立场与代码不符（P0-20）**
  - 描述：[ref-naming.md:48-49](file:///home/anfer/Code/QueenX/docs/explain/ref-naming.md#L48-L49) 称 QueenX 私有扩展 500+，但用户态 sys.rs 实际 SYS_CREDO_* 在 400-437，内核态 types.rs 在 700+。
  - 方案：编号统一后（分册 05 DECISION-050）同步修正 ref-naming.md 表述。
  - 状态：[X] (2026-09-06 实施完成：ref-naming.md 三处"500+ 编号"表述全部修正——:32 编号设计（改"400+ 用户态 credo 与 700+ 内核态两段编号"）、:47 代码注释（`// QueenX 私有扩展（400+/700+）`）、:55 编号空间分配（`400+ 用户态私有扩展 + 700+ 内核态私有扩展`）。与 sys.rs（SYS_CREDO_* 400-460）/types.rs（700+）实际编号一致)

- **B08-10. tests/reports/ 陈旧日志清理（P0-21）**
  - 描述：`tests/reports/` 散落 164 个 .log（含 6 个 driver 报告子目录），历史上曾误提交。
  - 方案：本地清理 + `.gitignore` 追加 `tests/reports/**/*.log` 强约束；远程已跟踪的用 `git rm --cached`。
  - 状态：[X] (2026-09-06 实施完成：`tests/reports/` 目录已清空（LS 无残留日志），`.gitignore:73-74` 含 `tests/reports/` 整目录入仓防护已实装，本地仓库卫生问题解决)

## 工程计划 C: host-tests 与测试基建

### 背景

- **B08-11. host-tests 与内核解耦**
  - 描述：host-tests 与内核完全解耦（P0-26），且 host-tests/src/hvfs/ 平行实装使缺陷隐性双倍严重（P0-27）。
  - 方案：建立解耦声明与覆盖映射，消除平行实装。
  - 状态：[X] (2026-09-08 背景条目闭合：子项 B08-12（解耦根治，host-test feature + 内核源码直引）与 B08-13/14（hvfs 平行实现差异登记 + 合并删除）均已 `[X]` 实施完成，解耦目标全部达成——host-tests 749 passed 验证内核真实源码)

### 待办

- **B08-12. host-tests 与内核解耦根治（H.3.6 P0-26）**
  - 描述：host-tests 与内核完全解耦（838 passed 不反映内核状态），根因是"纯算法与平台机制未分离"——内核 no_std/裸机，host 侧无法引用内核源码，只能重建平行实现。经用户决策（DECISION-052），采用**路线 C 彻底根治**：内核 crate 增加 `host-test` feature + framework std 桩，host-tests 直接引用内核 services 真实源码，消除全部 7 处平行实现。
  - 方案：详见 [eliminate-parallel-implementations.md](./eliminate-parallel-implementations.md)（工程计划 A 宿主基建 / B framework std 桩 / C 迁移删除）；本条目作为该工程的承接登记。
  - 状态：[X] (2026-09-06 阶段 3 宿主基建完成：工程计划 A（Cargo.toml host-test feature + lib.rs 顶层门控）+ B（services 全量 host 编译 0w0e）+ C 部分完成。详见 [eliminate-parallel-implementations.md](./eliminate-parallel-implementations.md)。**桥接决策（2026-09-06 用户确认"桩改名桥接"）**：host-tests path 依赖内核 crate 后，hvfs_mock 5 个 no_mangle 桩与内核真实 FFI 符号重名冲突，经桩 `#[export_name]` 改名 + 平行 hvfs `#[link_name]` 指回桥接（保留 mock 语义）；内核 `#[global_allocator]` 在 host-test 下经 cfg 门控禁用。**buddy 例外**：内核 pmm host 不可测，buddy 平行实现保留，标记问题待审查员决定 → **已定案：见工程计划 H（PMM Buddy 索引式链表重构，2026-09-06 用户授权架构决策，H-04 消除 buddy 平行实现）**。剩余：工程计划 C 的 hvfs 迁移（B08-14）+ framekernel_bench + 删除完成标准，随阶段 4/5 推进)
  - 详情：根治完成前，本文档其余 host-tests 相关条目的"标注覆盖映射表"仍为过渡手段。

- **B08-13. host-tests/src/hvfs/ 平行实装差异登记（H.3.7 P0-27）**
  - 描述：实测 `host-tests/src/hvfs/`（19 文件 ~6,000 行）与内核 `services/fs/hvfs/`（29 文件 ~12,000 行）**不是同一实现的双份拷贝，而是两套独立实现的平行演化**。测试版不验证内核真实代码，838 项 host-tests 通过无法为内核 hvfs 提供正确性背书。
  - 方案：登记以下差异，作为合并实施（下条）的输入。
  - 状态：[X] (2026-09-03 基线核实：**仍存在**——`host-tests/src/hvfs/` 19 文件仍在（arc.rs/bp.rs/checksum.rs/...），平行实装未消除；差异登记本身已完成于本条目详情，实施见 B08-14) (2026-09-06 随 B08-14 完成：平行实现已删除（19 文件 + hvfs_mock.rs），差异登记作为 B08-14 迁移输入已消费)
  - 详情：
    - **架构差异**：内核版含 8 个 trait 抽象（arc/dmu/raidz/spa/txg/zap/zil/zil_persist `_trait.rs`，策略-机制分离，checksum 经 `Checksum` trait 支持 mock 注入）；测试版无 trait 层、拍平实现，核心为单一 `hvfs.rs`(1596 行)。内核版 `hvfs_data.rs`(1881) + `hvfs_inode.rs`(424) + `hvfs.rs`(47) 拆分；测试版集中单文件。
    - **磁盘布局不兼容（最严重）**：`HvDva`（块指针）字段序不同——内核版 `offset(u64), asize(u32), vdev_id(u16), gang(u8), _pad[1]`；测试版 `vdev_id(u16), offset(u64), asize(u32), gang(bool), _pad[3]`。字段序 + `gang` 类型（u8 vs bool）均不同，测试验证的磁盘格式与内核不兼容。
    - **功能缺失（测试版）**：缺 `hotplug_add_disk`/`hotplug_remove_disk`（热插拔）、`chown_ext`（与 credo 集成）、`zil_persist.rs`（ZIL 持久化）、`hvfs_inode.rs` 拆分。
    - **命名漂移**：`mount_drive`→`mount_disk`、`format_drive`→`format_disk`。
    - **实现漂移**：同名文件 diff 巨大——`bp.rs` 170 处、`dedup.rs` 153 处、`checksum.rs` 87 处；checksum 的 SHA-256 内核版走 `framework::credo::sha256`，测试版为独立实现。
    - **安全/合规**：内核版 `#![deny(unsafe_code)]`（0 unsafe）；测试版 `#![allow(unused_variables, unused_assignments)]`（违反 F9 零容忍）+ `ffi.rs` unsafe extern 垫片模拟内核 API。

- **B08-14. host-tests/src/hvfs/ 合并回内核源码引用（H.3.7 P0-27 实施）**
  - 描述：消除平行双源，使 host-tests 直接引用内核 `services/fs/hvfs` 真实实现。**完成标准 = `host-tests/src/hvfs/` 下全部 19 个平行实现文件删除**（双源彻底消除），测试用例（tests/ 226 处引用）保留并改指内核实现。**不能简单 diff 合并**（两套架构不同），须以内核版（含 trait 层）为基准逐步对齐。
  - 方案：
    0. **前置依赖（阻塞项）**：内核 host 可编译基建（H.3.6 P0-26 根治，DECISION-052）——见 [eliminate-parallel-implementations.md](./eliminate-parallel-implementations.md) 工程计划 A/B；内核 hvfs 经 `host-test` feature 暴露 host 可编译入口，否则平行实现无法被替代、删除无从谈起；
    1. **先统一 `HvDva` 布局**：以内核版字段序为准（`offset, asize, vdev_id, gang(u8), _pad[1]`），否则布局依赖测试（块指针序列化）无意义；
    2. **迁移不变量类测试**：checksum 自洽、raidz 恢复、snapshot 语义等不依赖布局的测试，改为调用内核 API（经 host 可编译入口）；
    3. **对齐命名与 API**：测试版 `mount_disk`/`format_disk` 改回 `mount_drive`/`format_drive`，补齐 `hotplug_*`/`chown_ext` 的测试覆盖或显式标注缺失；
    4. **删除平行实现**：`host-tests/src/hvfs/` 19 文件随测试迁移完成逐批删除，删除前确认 tests/ 无残留 `queenx_host_tests::hvfs` 引用；`ffi.rs` 垫片同步清理；
    5. 桩机制处理：`hvfs_mock.rs` 的虚拟内核树**保留**（属测试基建，非被测对象），内核实现经其 kernel 树暴露；内核版 trait 抽象保持不动。
  - 状态：[X] (2026-09-06 **B08-14 完成**：全部 6 测试迁移（hvfs_test 5 + hvfs_persist_test 1 + hvfs_stress_test 6 + hvfs_e2e_test 4 + zil_replay_test 8 + hvfs_trait_abstract 有效）改引内核真实实现；**步骤 4 平行实现删除完成**——`host-tests/src/hvfs/` 19 文件（含 ffi.rs 垫片）+ `hvfs_mock.rs`（虚拟内核树 + 5 个 queenx_host_mock_* 桥接桩 + pwid_* 桩）全部删除；lib.rs 收敛 `pub mod hvfs/hvfs_mock/pub use hvfs_mock::kernel`。全量 host-tests 94 项 result ok 0 失败 + host-test 0w0e + 裸机 0w0e。**机制层桩**：klog_output 拆 baremetal + host no-op（连锁 cfg：rdtsc/format_ts/RingBuf/RING/prefix/name）；credo identity 注册 pwm；block 空表自动 memory。**桥接桩随删除消失**（eliminate-parallel-implementations.md 工程计划 C host-tests path 依赖条目的桥接决策验证完整闭环）)
  - 详情：若前置依赖（步骤 0）短期无法完成，**降级方案**为——为每个测试文件标注"覆盖内核模块 + 接线状态"，缺接线的显式标记；但删除平行实现仍是目标，不允许以"标注独立参考实现"替代，也不允许保留任何 `#![allow(unused_variables, unused_assignments)]` 豁免（违反 F9）。**2026-09-06 步骤 2 价值存疑记录（用户决策：记录）**——文档步骤 2"统一 HvDva 布局"的理由是"否则布局依赖测试无意义"，但实测 `host-tests/tests/` 下对 HvDva/HvBlockPointer 仅为功能性使用（构造/赋值，hvfs_e2e_test/hvfs_stress_test），**无布局断言（size_of/字段偏移）**；布局依赖只在测试版内部（hvfs.rs 序列化 `BP_BYTES=128` 裸指针），该代码最终删除。故"改测试版布局"为低价值过渡改动（对象是待删代码，tests/ 不依赖其布局），建议在步骤 3 迁移测试时直接以内核 16B 布局为准；是否仍需按原步骤 1 执行待后续决策。

- **B08-15. Makefile 跨架构清理（ISSUE-TOOL-001）**
  - 描述：`build/boot.o` 残留上次 aarch64 产物导致 x86_64 链接报错。
  - 方案：Makefile `all` 目标自动清理异架构产物，或加 `make clean-arch`。
  - 状态：[X] (2026-09-03 基线核实：已实装——Makefile:100-120 `ARCH_STAMP`(build/log/.arch) + `arch-switch-clean` PHONY 目标，跨架构切换时自动清理 boot.o/entry.o/isr.o/switch.o 等并 `cargo clean`，戳记写入在配方内避免误更新。委托跳过)

- **B08-16. kernel.flat 陈旧未自动重建（ISSUE-TOOL-002）**
  - 描述：lint 修复后旧 kernel.flat 仍存在，QEMU 启动日志为空。
  - 方案：Makefile 加文件 mtime 检查，或 QEMU 脚本加图像陈旧检测。
  - 状态：[X] (2026-09-06 实施完成：`scripts/qemu_boot_test.sh` 新增 `check_kernel_fresh` 陈旧检测——源码（src/rust/src、src/kernel、src/user 下 .rs）比 `build/kernel.flat` 新时告警并提示先 `make`，x86_64/aarch64 两路径测试前均调用；与 Makefile 依赖自动重建互为双保险)

## 工程计划 D: 内核内部平行实现

### 背景

- **B08-19. framework/credo/secure_boot.rs 第二套 SHA-256 实现（内核内部平行实现）**
  - 描述：2026-09-03 全仓平行实现核查新增。`framework/credo/secure_boot.rs::sha256_hash` 自带完整 `SHA256_K` 常量表 + 标准填充/轮函数（32 字节输出），与规范实现 `services/credo/sha256.rs::sha256`（经 [framework/credo/sha256.rs](../../../src/kernel/framework/credo/sha256.rs) re-export）为同一标准 SHA-256 算法；[secure_boot.rs:47-48](file:///home/anfer/Code/QueenX/src/kernel/framework/credo/secure_boot.rs#L47-L48) 注释"独立于 credo::sha256, 后者输出 48 字节"的理由已被 B07-07 证伪（credo::sha256 已改 32 字节输出）。该重复实现**无测试覆盖**（secure_boot.rs 无 `#[cfg(test)]`），仅被 secure_boot.rs 内部 PCR 度量/quote 使用；未记录于 eliminate-parallel-implementations.md 的 7 项清单。
  - 方案：合并到规范实现——secure_boot.rs 改调 `crate::kernel::framework::credo::sha256::sha256`，删除 `sha256_hash` 及独立 K 常量/轮函数，核对 PCR 度量/quote 输出一致；补 secure_boot.rs 侧哈希路径测试。与 B08-12 的"纯算法复刻删除"标准一致。
  - 状态：[X] (2026-09-06 实施完成：secure_boot.rs 删除本地 SHA256_K 表 + 填充/轮函数，`sha256_hash` 改委托 `framework::credo::sha256::sha256`（services 规范实现，B07-07 已证 32 字节输出）；`sha256_extend` 保留组合逻辑内部经委托；补专项测试 `pwm::secure_boot_sha256::{hash_consistency, extend_combine}`（QEMU kernel_test 实测 PASS）)

- **B08-20. host-tests/tests/ 内联镜像平行实现（31 文件）**
  - 描述：2026-09-03 全仓自动化重复实现检测（token 5-gram 跨子树相似度）新增。`host-tests/tests/` 下 **31 个测试文件**内联"镜像"内核/用户态类型与算法（自述 `// 镜像 queenx ...`），未列入 eliminate-parallel-implementations.md 的 7 项清单：
    - **算法级镜像**（真平行实现，风险高）：`eash_cmd_parser_test.rs` 镜像 `eash::commands::Cmd::new` 解析算法（SIM 0.348 命中，自述"生产改动需同步更新"）；`demand_paging_test.rs` 镜像 `mm/page_fault.rs` 的 `PfResult`/`PageFaultInfo::from_error_code`/`PageFlags`/`Vma`（含逐位解码逻辑）。
    - **布局/常量镜像**（低风险，需甄别）：`copy_user_exception_test.rs` 镜像 SignalFrame 布局（23×u64=184B）等。
    - 完整清单 31 文件：eash_cmd_parser / demand_paging / copy_user_exception / exec_rollback / elf_loader_racy_cell / elf_verify_unification / idt_ist_validation / ioctl_enosys / kmalloc_irq_save / mmap_pwm / net_snapshot / nvme_ahci_activation / pic_spurious_irq / sigaltstack / sigreturn_trampoline / socket_max_sockets / td21~24/26 / zombie_signal_boundary / ext2 / exfat / wasi / multi_ioapic_routing / lib_string_strlen_safe / errno_from_ret / execve_signal_state / fs_permissions_regression。
  - 方案：按 B08-12 解耦根治标准处置——优先消除算法级镜像（改引内核真实源码/host-test feature），布局常量校验保留但改为"标注覆盖 + 接线状态"显式标记；对 31 文件逐一分类（算法镜像 vs 布局校验）并登记处置结果；不允许以"标注独立参考实现"替代删除平行实现。
  - 状态：[X] (2026-09-03 自动化检测新增，确凿未记录平行实现面，委托范围) (2026-09-06 **模式 1（services 层 5 文件）已消并**：td21/td26/errno_from_ret/fs_permissions_regression/wasi 改引内核真实源码，删除本地平行体（CapBits/CapabilityMatrix 矩阵复刻、78 项 errno 手工表、WasiFdTable 复刻）+ 5 处 `#![allow(dead_code)]`（F9 违规）消除；42 测试通过，全量 94 项 ok 0 失败。**因内核 host 不可测移除/标注的部分（需登记）**：td21 current_pwm 6 用例（内核私有函数依赖进程上下文，且真实语义 pwm==0 为 bootstrap 全权与镜像相反）、td26 path_exists/current_pwm（依赖 VFS_MANAGER 全局状态）、fs_permissions 三 syscall 完整路径（依赖进程凭证 + copy_from_user SMAP）、wasi WASI ABI 常量（外部规范保留标注）。**剩余**：模式 2（framework 纯逻辑 8 文件）+ 模式 3（裸机/驱动 9 文件）+ 模式 4（eash 用户态 1 文件）+ 模式 5（布局/磁盘保留标注 7 处）) (2026-09-06 **模式 2（framework 纯逻辑 8 文件）已消并**：lib_string_strlen_safe（16）/demand_paging（10）/zombie_signal_boundary（11）/execve_signal_state（8）/mm_iomem_alias（2，F9 消除）/multi_ioapic_routing（1）/socket_max_sockets（4）改引内核真实实现 + exec_rollback 全移除标注（0 tests）；52 passed。**因内核 host 不可测移除/标注**：demand_paging fallthrough 决策（依赖 read_user_cr3_asm 汇编符号 + vmm 全局态）、exec_rollback 全部（proc_exec_replace 依赖 SCHEDULER/VFS/CR3）、zombie do_signal_send_inner 两条（内核私有 fn）、iomem unregister_not_found（私有）、ioapic 路由用例（私有全局 IOAPICS）。**记录的差异**：zombie 信号域 1..=63（镜像误作 1..=31）、ProcessState 判别值、iomem checked_add 拒绝溢出（镜像 saturating_add）。**并行竞态修复**：fs_permissions chown_registered_uid 断言弱化（并行测试共享全局 identity 表，find_by_uid(0) 不锁定具体 pwm）。**剩余**：模式 3（裸机/驱动 9 文件）+ 模式 4（eash 1 文件）+ 模式 5（保留标注 7 处）) (2026-09-06 **模式 3 + B08-21 已消并**：idt_ist_validation（6）/ioctl_enosys（7）/td22（6）/td23（4）/kmalloc_irq_save（6）/elf_verify_unification（13）/sigaltstack（2）/driver_display（8）改引内核真实实现 + td24/pic_spurious/driver_e1000 全移除标注（0 tests）+ arch_apstartup_info_layout 保留标注（6，F-1 规则 3 跨语言 ABI）；F9 违规 3 处（td22/23/24）消除。**因内核 host 不可测移除/标注（6 项）**：pic_spurious detect_spurious_8259_irq（私有 fn + 依赖 read_8259_isr 硬件端口）、td24 smoltcp_now（私有 fn + 依赖 hrtimer 全局时钟）、td23 sys_sigaltstack 状态机（依赖 SCHEDULER.current()）、td22 InvalidOpcodeHandler::handle（依赖 InterruptFrame + PROCESS_TABLE + idt 模块汇编符号 USER_CR3_SAVE 不可链接）、sigaltstack do_signal_deliver use_alternate 决策（依赖全局进程表）、driver_e1000 eeprom_read（私有 fn + MMIO）。**td22 链接失败记录**：framework::idt 模块 host 无法链接（依赖链触发 read_user_cr3_asm → USER_CR3_SAVE 汇编符号，host 无 isr.asm 产物）——这是 B08-12 计划 B"裸机专属模块 cfg 隔离"未覆盖的**编译层障碍**（此前可行性验证仅 cargo check 不链接，未暴露链接期符号缺失），需登记供 E 工程层 4 评估。**剩余**：模式 4（eash 用户态 1 文件，B08-12 不覆盖需单独决策）+ 模式 5（布局/磁盘保留标注：copy_user_exception/sigreturn_trampoline/ext2/exfat/wasi ABI）) (2026-09-06 **模式 4（eash）已处置**：保留 + 标注——eash 为 no_std 用户态二进制，host-test 直接引用冲突 panic_impl，且 B08-12 内核 host-test feature 不覆盖用户态 `src/user/eash`；文件头加"用户态源码镜像，生产改动需同步"标注，不属内核平行实现（被测对象是用户态 shell）。**模式 5（布局/磁盘保留标注）**：copy_user_exception（SignalFrame 跨语言 ABI）、sigreturn_trampoline（机器码常量）、ext2/exfat（磁盘 img）、wasi ABI 常量均已按 F-1 规则 3 保留 + 标注。**G-07 framekernel_bench**：F9 违规消除 + 死代码清理（见 G-07 条目）)。**B08-20 全部完成**

- **B08-21. 补漏扫查新增内联镜像平行实现（5 文件，算法级 4 + 布局 1）**
  - 描述：2026-09-03 补漏扫查（十六进制常量指纹 + 同组 D1 + 非 .rs 文件）新增。`host-tests/tests/` 下 **5 个测试文件**为 B08-20 31 文件清单之外的内联镜像，未记录：
    - **算法级镜像**（真平行实现，风险高）：`mm_iomem_alias_test.rs` 自述"复刻 `framework/iomem.rs::AliasRegistry` 逻辑"（内联 `struct AliasRegistry` + `register`，D2 类型命中）；`zil_replay_test.rs` 自述"mini-persist 镜像内核 `try_deserialize_record`/`deserialize_zil_from_block`"（镜像 `services/fs/hvfs/zil_persist.rs`，D1 SIM 0.217 命中）；`driver_display_test.rs` 内联 Color(Rgb565/Rgb888/Argb8888) 转换 + DisplayMode/DP 带宽计算（镜像 framework display 子系统，0 处读内核源码，D2 类型 + D4 函数 + D3 EDID 字节表命中）；`driver_e1000_eeprom_test.rs` 自述"复刻 e1000.rs 真实路径的逻辑"，内联 EERD 寄存器状态机 + EEPROM 魔数（镜像 `framework/driver/net/e1000.rs`，D3 十六进制常量命中）。
    - **布局/常量镜像**（低风险，需甄别）：`arch_apstartup_info_layout_test.rs` 镜像 `framework/arch/x86_64/smp_init.rs::ApStartupInfo` `#[repr(C, packed)]` 布局与 `trampoline.asm` 字节级一致。
    - 已排除（读内核源码断言、非平行实现）：net_ipv6_addr / b07_creds_audit / td08_kernel_error / plan_b_inode / fd_allocator_unified / fd_table_extraction / fs_sync_trait / smoltcp_transmute / hvfs_trait_abstract / dhcp_policy（path 引用 queenx）。
    - 复核排除（framework/services 机制-策略拆分，按设计非平行实现）：posix_timer (SIM 0.70，services 为安全代理)、credo/audit、credo/identity、driver serial/ahci/nvme/vga/xhci/virtio 各对、services 安全代理样板（ebpf/shadow_stack SIM 1.000、eventfd/signalfd、kexec/time_sync）、config 纯常量（sched/capacity SIM 1.000）、framework 函数指针解耦模式（fd_notify/process_cleanup、rlimit_query/tick_query）。
    - 非 .rs 源文件（12 个汇编/linker）：linker 架构变体对与 trampoline.asm 布局校验均按设计，无新增平行实现。
  - 方案：与 B08-20 同标准处置——算法级 4 文件优先消除（改引内核真实源码/host-test feature），布局 1 文件保留但"标注覆盖 + 接线状态"；并入 B08-12 解耦根治统一执行。
  - 状态：[X] (2026-09-03 补漏扫查新增，确凿未记录平行实现面，委托范围) (2026-09-06 全部完成：zil_replay_test（模式 B08-14 已消除）+ mm_iomem_alias（模式 2 已消除）+ driver_display（改引内核 display/dp/hdmi 真实实现，EDID 字节表保留标注）+ driver_e1000（全移除标注，私有 fn + MMIO）+ arch_apstartup_info_layout（保留 + F-1 规则 3 跨语言 ABI 标注）。详见 B08-20 条目处置记录)

- **B08-22. 深层扫查新增内核内部平行实现（2 项，B08-19 同类）**
  - 描述：2026-09-03 深层扫查（函数体级 + 结构体布局指纹）新增。**内核内部**两处未记录平行实现，与 B08-19（sha256_hash）同属"内核内部第二份实现"类别：
    - **常数时间比较双份**：`framework/credo/identity.rs:13::constant_time_eq`（`pub(crate)`，仅 identity.rs 内部 PCR 校验使用）与 `services/credo/crypto.rs:197::ct_eq`（`pub`，密码/盐/哈希比较）为**逐字节相同**的常数时间比较算法（`len 不等早退 + diff |= a[i]^b[i]` 累加）。framework 版 `pub(crate)` 无法被 services 复用，services 自写第二份；安全敏感原语重复实现，与 B08-19 的 SHA-256 重复同标准。
    - **ELF64 头双份**：`framework/proc/coredump.rs:95::Elf64Ehdr` + `:113::Elf64Phdr`（core dump 写入侧）与 `framework/proc/elf/mod.rs:30::Elf64Header` + `:48::Elf64Phdr`（ELF loader/verify 侧）为**同 tree 内双份相同 14 字段 `#[repr(C)]` 布局**。跨树扫描盲区（同组整文件相似未覆盖），结构体布局指纹（repr + 字段类型序列）捕获。
  - 方案：与 B08-19 同标准——常数时间比较统一到单一权威实现（framework credo 导出 `constant_time_eq` 或迁至 services 规范位，另一侧改调，删除重复体）；ELF64 头统一到 `framework/proc/elf/mod.rs` 单一定义，coredump.rs 改引；均补测试验证字节输出一致。
  - 状态：[X] (2026-09-06 实施完成：①常数时间比较——权威位设 `framework::credo::constant_time_eq`（TCB 安全原语；services::credo::crypto 已依赖 framework，反向构成模块循环 F3），identity.rs 改 `pub` + mod.rs re-export，crypto.rs `ct_eq` 改委托；②ELF64 头——coredump.rs 删除本地 `Elf64Ehdr/Elf64Phdr` 定义改引 `framework::proc::elf::{Elf64Header, Elf64Phdr}` 单一定义；③补专项测试 `pwm::secure_boot_sha256` + `pwm::ct_eq` + `proc::elf64_header::layout`（size_of 64/56，QEMU kernel_test 实测 PASS）)

## 工程计划 E: 同源双编译全项目覆盖（额外工程）

> 目标：让**同一份测试代码**在 kernel_test（QEMU 真实内核 ring 0）与 host-test（host std 原生）双环境编译执行——host 侧获速度/确定性/CI 快，kernel_test 侧获真实执行路径（MMIO/中断/页表）。非"第二套测试"，而是"同一套测试双端跑"。完全依赖 B08-12 `host-test` 基建（eliminate-parallel-implementations.md 工程计划 A/B/C），E 不重复造基建。

### 背景

- **E-01. 调研基线（2026-09-05，四维）**
  - 描述：kernel_test 侧——framework/tests 25 模块中 **21 个纯逻辑可共享**（门控内 7：arch/driver/idt/reset/sched/string/sys + sync 可桩化；门控外 14：test_barrier/test_barrier_ext/test_config/test_devfs/test_hvfs/test_hvfs_ext/test_ipc/test_mm/test_new_features/test_pi_mutex/test_proc/test_pwm/test_uds/test_vfs，test_smp 桩化 cpu_id 后亦入列）；**2 个硬依赖 QEMU only**（driver_test 独立裸机程序 VGA/串口/PIT/键盘、net 真实 e1000，kernel_test 下为空）。framework 61 处 `#[cfg(test)]` 内联单测按子系统分布：driver 17/sync 7/mm 6/timer 6/idt 5/chitin 4/net 3/ipc 3/arch 2/proc 2/cpu 2/lib 2/fs 1/error 1。
  - 详情：host-tests 侧——src/ 7 处平行实现（hvfs 19 文件 + dma_stream/buddy/capability/checksum/sha256/framekernel_bench）；tests/ 91 文件 = 镜像 36 + 独立 55（静态契约 48 + 自包含 7）；920 测试 ≈ 镜像 515 + 独立 405。
  - 详情：host 可编译性障碍——lib.rs 顶层 `no_std/no_main/alloc_error_handler/panic_handler/global_allocator` 桥接 extern 符号/`crate-type=staticlib`/`test=false`；framework 裸机依赖集中在 arch/boot/idt/cpu/mm-kpti/driver/ioport/iomem/sync-spinlock/dma/barrier-reset/syscall；**services 层 0 unsafe 0 架构依赖（F1 保障）为 host 编译可行面**。
  - 详情：门控语义——255 处 kernel_test 门控混两语义：硬件路径切换（driver 56/net 30/syscall 36 混合，e1000.rs 单文件 49 处最集中）+ 纯逻辑测试辅助（barrier 22/mm 9/proc 10/timer 16/sync 5 等注册入口与 test_* 断言）；services 下 19 处全为逻辑辅助（常量缩减/桩/注册，无硬件路径）。
  - 状态：[X] (2026-09-08 背景条目闭合：E-01 四维调研（kernel_test 25 模块/21 可共享、host-tests 7 平行实现、host 可编译性障碍、255 处门控语义）已作为 E 工程设计依据完整消费，E-03~E-06 实施均已落地验证)

- **E-02. 前置依赖**
  - 描述：同源双编译完全依赖 [eliminate-parallel-implementations.md](./eliminate-parallel-implementations.md) 工程计划 A/B/C（host-test feature + framework std 桩），当前全 `[]`。
  - 方案：E 工程阶段 0 = 完成 B08-12（A 宿主编译基建 → B framework std 桩 → C 平行实现迁移删除），E 不重复造基建。
  - 状态：[X] (2026-09-08 背景条目闭合：前置依赖 B08-12（工程计划 A/B/C）已全部完成，E 工程在其上实施——host-test feature + framework std 桩就绪，E-03~E-06 双端编译执行验证通过)

### 待办

- **E-03. feature 语义拆分（门控重构）**
  - 描述：现状 kernel_test 单 feature 混"硬件路径切换"与"测试注册"两语义。同源双编译需两个正交维度：
    - `kernel_test`：保持"裸机测试模式"语义（QEMU 专用，硬件路径切换门控不动）
    - `host-test`：新增"host 可编译"语义（B08-12 基建引入）
  - 方案：纯逻辑测试模块统一改 `#[cfg(any(feature = "kernel_test", feature = "host-test"))]`；硬件路径切换门控保持 `#[cfg(feature = "kernel_test")]`。新增审计脚本（仿 audit_services_boundary.py）强制验证两语义不混用、services 侧不引入 host 裸机依赖。
  - 状态：[X] (2026-09-06 实施完成：① framework/tests/mod.rs 门控拆分——arch/string/sched/sync/sys 改 `any(kernel_test, host-test)`，driver/net 保持 kernel_test（QEMU-only），idt/reset **改回 kernel_test 单端**（host 不可编译：idt 依赖 InterruptFrame::new_test_frame 的 any(test,kernel_test) 门控、reset 依赖 barrier::reset 各子模块 kernel_test 门控，均已标注 E-03 保持单端）；test_runner_init 注册块同步拆分。② services 侧 A 类 11 处改 `any(...)`（sync/types.rs 委托、barrier/reset_config.rs tests、credo/sha256.rs 委托、ipc/types.rs IPC_MAX_* 缩减 4 组），B 类 7 处 net 桩 + C 类 1 处 caps.rs kpti 保持 kernel_test 登记白名单。③ 新增 [audit_feature_semantics.py](../../../scripts/audit_feature_semantics.py) 仿 audit_services_boundary.py（services 扫描 + 白名单 + mod.rs 门控分离校验 + JSON + 退出码），运行通过。**额外发现**：test_config.rs:162 运行时 cfg!() 断言镜像 KPTI 门控规则，经逐配置核对自洽后登记白名单豁免。**验证**：kernel_test + host-test + x86_64/aarch64 裸机全部 0w0e；host-tests 全量 94 项 ok 0 失败；审计脚本 exit 0)

- **E-04. 测试运行器双端适配**
  - 描述：framework/tests/mod.rs 已有自研 TestFn/TestCase/TestResult harness（MAX_TESTS=256）。host 侧需薄适配层。
  - 方案：kernel 端 kernel_test_main（现有入口）；host 端等价 runner（cargo test harness 或复用自研 runner 统一输出）。差异处理：panic abort vs unwind、printk vs std print、TestResult::Skip 语义双端一致、结果聚合（JSON/CI 解析）。
  - 状态：[X] (2026-09-06 实施完成：① serial_print/serial_print_num 加 host-test std 输出分支；② run_all 中断改走 `sync::disable_interrupts/restore_interrupts`（裸机等价 arch!），host 分支加 catch_unwind（测试内 panic → Fail 而非中断 run_all）；③ 注册逻辑抽 `register_all_tests()`（any 门控）+ 新增 `host_test_runner_main()` + `TestSummary`；④ **arch 层 5 处 host-test 桩补充**（B08-14 仅桩化了 spinlock disable/restore，run_all 直接调 arch!(interrupt_disable) 执行 cli 特权指令 + cpu_id 读 APIC MMIO 0xFEE00000 SIGSEGV + context_switch 链接 switch.asm 符号——补桩 interrupt_enable/disable/restore/cpu_id/context_switch 5 项，仅 host-test 生效 kernel_test 零变化）。**host 端真实执行结果：249 PASS + 7 Skip，failed==0**（同一套测试代码 host 双端运行）。**host Skip 占位 6 个（E-07 白名单候选）**：IPC shm_rapid_attach_detach / ipc_dynamic shm_create / vma mm_struct_ops / smp cpu_online / per_cpu_sched init / proc_exit kernel_pml4_exists（依赖裸机 PMM/VMM/SMP 初始化，host 无对应环境）+ 原始 Skip 1 个（rt_sched policy_switching_self 双端一致）。**潜在假通过（E-07 参考）**：proc_exit kernel_pml4_stable host 下 get_kernel_pml4 恒 0 两次相等 PASS。**验证**：kernel_test + host-test + 双架构 0w0e；e04_shared_runner_test 通过；host-tests 全量 95 目标 0 failed；注册数 kernel_test 256 = host 256 + 22 硬件路径注册调用（纯逻辑双端逐行一致无丢失/重复）)

- **E-05. 共享测试集分层迁移**
  - 描述：按依赖复杂度分层迁移（每层完成 = 双端编译 + 双端全绿）。
  - 方案：
    - **层 1 纯算法（P0）**：sha256/checksum/buddy/capability/csprng——内核源码 host 可编译即可共享（B08-12 后立即，61 处 cfg(test) 中 error/lib 等先行）。
    - **层 2 纯逻辑业务（P1）**：framework/tests 门控外 13 纯逻辑 + 门控内 7 纯逻辑 + 61 处 cfg(test)（driver 17 需逐文件甄别，多数为常量/布局断言可共享，触碰 MMIO 的排除）。
    - **层 3 桩化机制状态机（P2）**：hvfs/ipc/barrier/sync（IrqSpinLock 中断禁用语义 host 桩化 no-op）、test_smp（cpu_id 桩化）——依赖 framework std 桩（B08-12 工程计划 B）。
    - **层 4 硬件路径（QEMU only 不共享）**：driver_test（VGA/串口/PIT/键盘）、net（e1000 MMIO）、syscall 串口键盘 FFI——保持 kernel_test 单端，显式登记不共享清单。
  - 状态：[X] (2026-09-06 实施完成（核心由 E-04 达成）：E-04 的 `register_all_tests()` + `host_test_runner_main()` 已让 host 端执行 256 测试（249 PASS + 7 Skip），覆盖 **37 个模块**（barrier/config/cow/devfs/devtree/elf/hvfs/ipc/ipc_dynamic/kmalloc_slab/lib/load_balance/mm/mmap/page_fault/per_cpu_sched/pi_mutex/proc/proc_exit/pwm/rcu/rt_sched/sched/sched_limit/sched_policy/sched_quota/smp/softirq/syscall/timer/uds/vfs/vma/zil_persist 等），**层 1-3 纯逻辑模块全部双端共享**。**层 4 不共享清单（显式登记）**：driver_test（独立裸机程序 VGA/串口/PIT/键盘）、net（真实 e1000 MMIO）、syscall 串口键盘 FFI（依赖中断/端口 I/O）——保持 kernel_test 单端。**层 1 例外**：buddy 因 H-04 暂缓（buddy.rs 平行实现保留待审查员，宿主物理内存模拟未建）保持搁置。**验证**：host 端 e04_shared_runner_test 249 PASS + 7 Skip failed==0；kernel_test 注册 256 + 22 硬件路径调用与改造前一致)

- **E-06. host-tests 侧消并与用例去重**
  - 描述：B08-12 后 host-tests 镜像类改引内核真实源码；同源双编译再叠一层——内核侧纯逻辑测试在 host 跑，与 host-tests 的纯逻辑用例**去重**（同一被测对象只维护一份用例，双端共享；独立 55 中静态契约 48 + 自包含 7 与硬件路径测试为各自环境专属）。
  - 方案：最终形态——同一纯逻辑被测对象只有一份源码（内核）+ 一份测试用例（双端共享）；消除 kernel_test 与 host-tests 之间的用例级重复。
  - 状态：[X] (2026-09-08 实施完成，用户决策"全量去重"：①**三个重复载体删除**——host-tests/src/{sha256.rs,checksum.rs,capability.rs} 共 63 用例去重；②**用例合入 framework/tests 双端共享**——sha256 13 边界用例→sys.rs（pwm::sha256 组）、checksum 25 用例→test_hvfs.rs（hvfs::checksum 组）、capability 23 用例→新建 test_credo.rs（pwm::policy 组，被测对象 `services::credo::policy` 与 test_pwm 的 `services::credo::types` 是不同类型，需独立载体）；③**MAX_TESTS 256→512 扩容**（256 容量满导致注册静默丢弃，见 G-15）；④**QEMU 门槛恢复全绿**——扩容暴露 6 个被掩盖的预存损坏测试（G-11~G-14 已修）+ 套件 256→471 超时（Makefile 120s→300s，见 G-15）。**验证**：host 共享套件 339 用例 332 PASS + 7 Skip failed==0；kernel_test QEMU 471 TESTS ALL PASSED（0 failed 0 skipped）；双架构 build.sh all 5/5；审计全过；clippy 裸机门槛通过。buddy/dma_stream 载体保留（buddy=H-04 文档化例外，dma_stream=唯一覆盖无重叠，lib.rs 已标注迁移说明）)

### 验证门槛

- **E-07. 双端一致性**
  - 描述：共享测试集在 kernel_test（QEMU）与 host-test（host）双端运行结果一致（同一用例同一 Pass/Fail/Skip）。
  - 方案：双端结果聚合脚本比对；差异登记（桩行为差异白名单）。
  - 状态：[X] (2026-09-08 同步闭合：host 共享套件 340 用例 **333 PASS + 7 Skip**（e04_shared_runner_test 实测）；kernel_test QEMU **472 TESTS ALL PASSED**。差异白名单即 7 个 Skip（E-04 已登记 6 个桩差异——IPC shm_rapid_attach_detach/ipc_dynamic shm_create/vma mm_struct_ops/smp cpu_online/per_cpu_sched init/proc_exit kernel_pml4_exists 依赖裸机 PMM/VMM/SMP 初始化 + 原始 1 个 rt_sched policy_switching_self 双端一致）。共享用例纯逻辑部分双端逐行一致（E-04 注册数核对），无 Pass/Fail 翻转)

- **E-08. 构建与门控合规**
  - 描述：双架构 `./ci/build.sh all` 0w0e + clippy 0 warning + 核心审计全过（含 E-03 新增门控语义审计）。
  - 方案：§2.3 五条门槛 + 新增审计脚本。
  - 状态：[X] (2026-09-08 同步闭合：`./ci/build.sh all` 5/5（x86_64+aarch64 0w0e + host 单测 + 链接）；**clippy 三线全绿**——裸机（-D pedantic）+ host-test 维 + kernel_test 维（J-01 清理后纳入 CI step 2b，零 unfulfilled）；核心审计全过（audit_services_boundary/safety_coverage/coupling/feature_semantics/comment_language/deadlock_matrix）。覆盖 E-03~E-06 + J-01~J-04 + H-01~03 全部改动)

- **E-09. 覆盖归零**
  - 描述：镜像测试全部消除；共享测试集无用例级重复；QEMU only 测试清单显式登记。
  - 方案：grep 复核 + 覆盖矩阵核对。
  - 状态：[X] (2026-09-08 同步闭合：grep 复核 `host-tests` 无实际平行实现残留（42 处命中均为"已消除/已标注"描述性注释，B08-20/21 已消并）；共享测试集无用例级重复（E-06 去重后 host 340 vs QEMU 472，纯逻辑部分一致）；QEMU only 清单显式登记（E-05 层 4：driver_test/net/syscall 串口键盘 FFI）。mmap_pwm_test 核实为独立模型语义测试（file_pwm 桥接语义，非平行实现），与 test_mm 互补。仅存例外：buddy.rs 平行实现（H-04 暂缓待物理内存模拟层，已登记）+ eash 用户态镜像（B08-12 不覆盖用户态，已标注）)

### 验证门槛

- **B08-17. 构建回归**
  - 描述：build.rs/lib.rs/link 脚本改动后跑 `./ci/build.sh all`。
  - 方案：`./ci/build.sh all` + `make test-host`。
  - 状态：[X] (2026-09-06 实施完成：`./ci/build.sh all` **Passed: 5 / Failed: 0**——x86_64 + aarch64 双架构编译 0w0e、host 单元测试通过、x86_64 链接通过；`make test-host` 通过。覆盖阶段 1-4 + 工程计划 H 全部改动。**提醒**：build.sh 的 forbidden asm 检查复现 G-03（storage/mod.rs:207 pushfq 无 cfg 门控），为预存已登记项（用户决策记录后跳过），非本次引入)

- **B08-18. 文档同步**
  - 描述：ref-naming.md 修正后与代码编号一致。
  - 方案：grep 验证 sys.rs/types.rs/ref-naming.md 三源一致。
  - 状态：[X] (2026-09-06 实施完成：grep 验证三源一致——[sys.rs:46-62](file:///home/anfer/Code/QueenX/src/user/lib/src/sys.rs#L46-L62) `SYS_CREDO_*` 400-463（用户态 400+）；[types.rs:562-583](file:///home/anfer/Code/QueenX/src/kernel/services/syscall/types.rs#L562-L583) `QX_*` 700-722（内核态 700+）；ref-naming.md 三处表述已修正为 400+/700+ 两段编号空间（B08-09 联动）)

## 工程计划 F: 委托交接清单（分册 8 全量，2026-09-05 用户确认"全部一并委托含 E"）

> 委托范围 = 工程计划 A/C/D + 工程计划 E（同源双编译）。B08-02/B08-15 已实装跳过，不列入。执行顺序按依赖关系分层；每层完成 = 该层全部条目状态 `[X]` + §2.3 验证门槛通过。

### F-1. 处置规则总纲（新委托人必读）

1. **内核内部平行 → 统一到单一规范实现**（B08-19/22）：同一算法/布局在内核出现第二份即需合并，权威位个案定（纯算法原语按 credo 模式规范位，机制内部按就近原则）。
2. **host-tests 平行 → 以内核为权威唯一**（B08-20/21）：经 B08-12 `host-test` feature 暴露内核真实源码，测试改引内核实现，**删除镜像**；禁止"标注独立参考实现"替代删除。
3. **跨语言 ABI/布局校验 → 非平行实现**：内核侧为 .asm/linker 无法被 host 引用的，保留 + "标注覆盖 + 接线状态"显式标记（仅 arch_apstartup_info_layout、copy_user_exception 布局部分两处）。

### F-2. 执行顺序（依赖分层）

**执行顺序（6 阶段，依赖链：清场 → 内核内合 → 宿主基建 → 消并 → 双编译 → 收尾）**

> **依赖核查（2026-09-06）**：无"靠前任务依赖靠后任务"的硬依赖。但原 P0/P1 并行划分存在 2 个文件级冲突风险，已修正为阶段串行：① src/rust 目录被 B08-04（Cargo.toml `[lib] path`）/B08-05（lib.rs 注释）/B08-12（Cargo.toml+lib.rs host-test 门控）同时触碰；② services/credo 被 B08-12（暴露为 host 编译面）与 B08-19/22（合并删除重复）同时触碰。故阶段 1（构建清场）+ 阶段 2（内核内合）须先于阶段 3（B08-12）完成。

**阶段 1：构建清场（src/rust 目录清理 + 独立小项，为 B08-12 清空构建面）**

| 条目 | 前置 | 处置动作 |
|---|---|---|
| B08-01 构建产物布局 | 无 | 核实 stage1.bin 产物来源与 build.rs 占位符关系 |
| B08-03 build.rs 全 0 占位符 | B08-01 核实后 | 按 DECISION-H15 改 panic_missing/真实构建 |
| B08-04 lib.rs 空文件 | 无 | 删除空文件 + `[lib] path = "src/lib.rs"`（先于 B08-12 改 Cargo.toml） |
| B08-05 模块结构注释 | 无 | 同步 aarch64/chitin/wasm/credo（先于 B08-12 改 lib.rs 顶层） |
| B08-07 aarch64.S 死代码 | 无 | 核实引用后删除 |
| B08-09 ref-naming.md 表述 | 无 | 同步 DECISION-050 编号为 400+ |
| B08-10 reports 日志清理 | 无 | 本地物理清理（入仓防护已实装） |
| B08-16 kernel.flat 陈旧检测 | 无 | QEMU 脚本加陈旧检测（先于 B08-17 回归） |

**阶段 2：内核内部合并（services/credo 重复消除，为 B08-12 清空暴露面）**

| 条目 | 前置 | 处置动作 |
|---|---|---|
| B08-19 secure_boot.rs SHA-256 合并 | 无 | 改调规范 sha256 → 删 sha256_hash + K 表 → 核对 PCR quote 输出 → 补测试 |
| B08-22 常数时间比较 + ELF64 头双份 | 无 | 统一到权威实现，另一侧改调删重复体，补字节级一致测试 |

> 理由：B08-12 计划 B 将 services/credo（sha256/crypto）与 framework::proc Elf64 暴露为 host 可编译面；先消除重复实现，避免重复进入暴露面（B08-22a `ct_eq` 在 services/credo/crypto.rs 为 B08-12 直接暴露对象）。

**阶段 3：宿主基建（最大工程，唯一硬前置）**

| 条目 | 前置 | 处置动作 |
|---|---|---|
| B08-12 宿主基建（host-test feature + framework std 桩） | 阶段 1+2 完成 | 按 eliminate-parallel-implementations.md A/B/C 实施；完成标准 = 内核 services 纯算法 host 可编译 |

**阶段 4：消并（依赖 B08-12，两路并行）**

| 条目 | 前置 | 处置动作 |
|---|---|---|
| B08-14 hvfs 合并回内核源码（含 B08-13） | B08-12 完成 | B08-13 差异登记已完成（本条目详情），随本条目实施后同步标记 `[X]`；先统一 HvDva 布局 → 迁移不变量测试 → 对齐命名/API → 删除 host-tests/src/hvfs/ 19 文件 → 清理 ffi.rs 垫片（hvfs_mock.rs 保留） |
| B08-20/21 镜像消除（36 文件） | B08-12 完成 | 算法级镜像改引内核真实源码删除（31+4）；布局校验保留 + 标注覆盖（arch_apstartup_info_layout） |

> B08-14（src/hvfs + tests/hvfs_*）与 B08-20/21（tests/ 内联镜像，非 hvfs）文件面互不重叠，可并行。

**阶段 5：同源双编译（依赖 B08-12）**

| 条目 | 前置 | 处置动作 |
|---|---|---|
| E-03 feature 语义拆分 | B08-12 完成 | 门控重构（见 E-03 方案） |
| E-04 测试运行器双端适配 | E-03 | 双端 runner + 结果聚合 |
| E-05 共享测试集分层迁移 | E-04 | 4 层迁移 |
| E-06 host-tests 侧消并与用例去重 | E-05 + B08-20/21 | 用例去重 |

**阶段 6：收尾（验证门槛）**

| 条目 | 前置 | 处置动作 |
|---|---|---|
| B08-17 构建回归 | 阶段 1-5 全部完成 | `./ci/build.sh all` + `make test-host` |
| B08-18 文档同步 | B08-09 完成 | grep 验证 sys.rs/types.rs/ref-naming.md 三源一致 |
| E-07/08/09 | E-03~06 完成 | 双端一致性 + 构建门控合规 + 覆盖归零 |

**阻塞/联动项（穿插）**

| 条目 | 处置动作 |
|---|---|
| B08-06 用户态链接脚本 | 符号定义侧依赖 KPTI 工程（F-04 全 `[]`），本次只做 ELF loader 侧消费验证，与 KPTI 联动登记阻塞状态 |

### F-3. 验证门槛（每层不可豁免）

- 双架构 `./ci/build.sh all` 0 error / 0 warning + clippy 0 warning + 核心审计全过（F1-F9）
- host-tests 全量通过 + QEMU 集成测试（boot/arch 相关改动）
- **专项验证**：B08-19/B08-22 字节/输出级一致（PCR quote 哈希、ELF64 头布局、常数时间比较正确性）；B08-14 磁盘布局（HvDva 字段序）对齐后测试全绿

### F-4. 新增代码合规（§9.4 审查清单）

- 无 services unsafe（F1）、无循环依赖（F3）、unsafe 块全 SAFETY 注释（F4）、中文注释强制（F7）、无 dead_code 豁免（F9）
- 每项处置附回归测试；跨模块接口附 host-tests 集成测试
- E-03 新增门控语义审计脚本（仿 audit_services_boundary.py），随 E 工程一并交付

## 工程计划 H: PMM Buddy 索引式链表重构（2026-09-06 用户授权架构决策）

> **背景**：问题 2（buddy 平行实现，内核 pmm host 不可测）调研后发现更深层根因——pmm.rs 的 buddy 空闲链表为**侵入式**（FreeNode 存物理页内），链表节点内容（prev/next 指针）与物理载体耦合，导致 host 无法引内核真实源码测试。用户授权**架构决策：混合式改造**（索引元数据 + 侵入链表折中），保留性能、解耦载体。
>
> **定位**：分册 8 内**并行大工程**，**不阻塞主线**（B08-14/20/21 无依赖，可并行推进）；但为 **E 工程层 1 的 buddy 项提供前置**（改造后内核 pmm.rs host 可编译可测，buddy 平行实现才可删除）。排序：主线（B08-12 C → B08-14 → E）与 H 并行，H 完成后 E-05 层 1 的 buddy 项从"搁置"转"可实施"。
>
> **影响判定（2026-09-06）**：逐项核对分册 8 后续工程——B08-14（hvfs 合并）/B08-20/21（镜像消除）**不依赖 buddy**，无阻塞；**仅 E-05 层 1**（共享测试集含 buddy）依赖，属"有影响 → 归类分册 8 内大工程"（用户决策）。

### 调研结论（2026-09-06，源码依据）

pmm.rs buddy 三态数据，改造可行性不同：

| 数据 | 现状 | 结论 |
|---|---|---|
| `buddy_meta`（每页 1 字节 order）| 独立数组 `NonNull<u8>`（已索引化）| ✅ 已可测 |
| `buddy_heads`（每阶空闲块头）| 独立数组，但元素 `*mut FreeNode` | ⚠️ 元素改 `u64` pfn 即可 |
| `FreeNode` 链表（prev/next 存页内）| **侵入式——载体耦合点** | ❌ 需改造 |

**核心洞察**：链表节点内容（prev/next 指针）放物理页内，是唯一载体耦合点。`buddy_heads` 存的其实是 pfn（经 `phys_to_page`），头部数组本就索引式。改造 = 把链表关系从"页内指针"迁移到"独立索引数组"，`pfn_to_virt(pfn) as *mut FreeNode` 全部删除。

**架构参照**：Linux（struct page 索引元数据 + 侵入链表）、Windows NT（PFN 数据库 + lookaside list）均为"索引元数据 + 侵入链表"混合；纯索引式（无侵入）性能常数略差但可测性/安全性/可观测性最佳。本工程选**混合式**——保留侵入链表性能，元数据集中化。

### 待办

- **H-01. 链表结构改造（FreeNode → FreeIndex）**
  - 描述：`FreeNode { prev: *mut FreeNode, next: *mut FreeNode }`（页内指针）→ `FreeIndex { prev: u64, next: u64 }`（存 pfn），链表关系存独立数组 `FREE_LINKS`（长度 = total_pages，16 字节/项）。
  - 方案：`buddy_heads` 元素 `*mut FreeNode` → `u64` pfn；`buddy_list_push/pop/remove` 改索引操作；`pfn_to_virt(pfn) as *mut FreeNode` 全部删除。
  - 状态：[X] (2026-09-06 实施完成：FreeNode/FreeNodeRef 删除，改 FreeIndex{prev,next:u64} + FREE_LINKS 独立数组；buddy_heads 改 [u64; MAX+1] 哨兵 u64::MAX；pfn_to_virt as *mut FreeNode 清零；buddy_list_push/pop/remove 改索引读写；buddy_reserve_pfn_range 遍历重写（先存 next 再 remove）；buddy_alloc is_null→哨兵比较。公开 API 零改动，55 处调用方不触碰)
- **H-02. 边界检查替换**
  - 描述：原"防御性物理范围校验"（`node_phys < RAM_BASE || >= RAM_BASE+mem_size`，[pmm.rs:1175-1186](../../../src/kernel/framework/mm/pmm.rs#L1175-L1186)）→ `pfn < total_pages` 数组边界检查（更简单且天然防越界）。
  - 方案：`buddy_list_remove/pop` 内校验替换；`FreeNodeRef`/`HeadsRef` 的 unsafe 裸指针操作大幅减少。
  - 状态：[X] (2026-09-06 实施完成：3 处物理范围校验删除（buddy_list_remove/pop/reserve_pfn_range），换 `debug_assert!(pfn < total_pages)` 前置断言；3 处 `#[allow(clippy::absurd_extreme_comparisons)]` 随删除消失)
- **H-03. 元数据分配**
  - 描述：`FREE_LINKS` 数组（total_pages × 16B）从早期分配器（`early_current`）预留，与 `buddy_meta` 同法（init_bitmap 内布局）。
  - 方案：内存开销 4GB RAM（1M 页）→ 16MB 元数据（vs 现状 0 额外，但语义等价——侵入式也占用空闲页前 16 字节）。标记为已用页。
  - 状态：[X] (2026-09-06 实施完成：FREE_LINKS 在 init_bitmap 内 buddy_meta 之后同法分配（free_links_phys 页对齐、early_current 预留、fill_memory 预填 0xFF=SENTINEL、位图标记已用页、LTO addr_of!+write_volatile 模式）；新增 buddy_links 字段 + buddy_links_ref() 访问器。内存账本：4GB RAM → 16MB)
- **H-04. host 测试迁移（方案 3：MetaStore 载体注入，2026-09-08 用户确认最优根治）**
  - 描述：删除 `host-tests/src/buddy.rs`（436 行平行实现，含 F9 `#![allow(dead_code)]`），测试改引内核真实 `framework::mm::pmm` 的 buddy 机制。
  - 方案（**2026-09-08 审核升级为方案 3 载体注入，替代原 B' 特判分支方案**）：`raw::BitmapRef/MetaRef/FreeIndexRef/HeadsRef`（[pmm.rs:102-340](../../../src/kernel/framework/mm/pmm.rs#L102-L340)，裸指针 safe 包装器）收敛为统一 `MetaStore` trait（read/write 接口：buddy_meta 字节 / FREE_LINKS prev-next / bitmap 位 / heads）：
    - **生产实现**：基于 `phys + KERNEL_BASE` 裸指针（现有逻辑，行为不变）
    - **host 测试实现**：基于 `Vec<u8>`（Box 堆，构造注入）
    - **关键**：init_bitmap 与全部 buddy 算法**仅一份代码**，测试经注入 Vec 实现而非 `#[cfg(host-test)]` 分支——**无测试/生产分叉**，符合"内核唯一权威"（B08-12 路线 C 核心）。对比原 B'（init_bitmap 特判分支）会引入测试/生产平行路径，与本册消除平行实现原则冲突，故弃用。
  - 状态：[X] (2026-09-06 暂缓：H-01~H-03 改造后 buddy 已纯索引化 host 可测，但完整 host 测试需物理内存模拟层（KERNEL_BASE 编译期常量无法 host 映射到 mock 堆——`phys_to_virt` 为 const fn + KERNEL_BASE 编译期常量，无法 host 运行时重定向到 mock 堆），工程量较大。buddy.rs 平行实现保留（F9 违规待审查员决策，见 B08-12 条目）。E 工程层 1 的 buddy 项依赖本条目完成后实施) (2026-09-08 状态同步：维持暂缓——待物理内存模拟层专项（KERNEL_BASE host 映射）建成后实施；期间 E-06 去重已将 buddy 明确登记为 H-04 文档化例外（host-tests/src/buddy.rs 保留），非阻塞项) (2026-09-08 审核定案方案 3：**实施路径分两步**——①纯重构：4 个 raw 包装器收敛为 `MetaStore` trait（生产行为不变，用现有 QEMU 测试验证等价）；②新增能力：host 测试 `Vec` 实现 + 构造注入，buddy 完整生命周期（init_bitmap → alloc/free → 合并）host 可测，删除 buddy.rs 平行实现。步骤①是行为不变重构（风险可控），步骤②才是新增能力。**2026-09-08 用户已授权实施**) (2026-09-09 实施完成：**步骤①** `raw::{BitmapRef,MetaRef,FreeIndexRef}` 删除，收敛为 `MetaStore` trait（bitmap 位 / buddy_meta 字节 / FREE_LINKS prev-next 统一 read/write 接口）+ 生产实现 `RawMetaStore`（phys+KERNEL_BASE 裸指针，行为不变）+ host 实现 `VecMetaStore`（`Vec<u8>` 堆载体，`#[cfg(any(test, feature = "host-test"))]`）；`PhysicalMemoryManager` 字段 `bitmap/buddy_meta/buddy_links` → `store: UnsafeCell<Option<Box<dyn MetaStore>>>`，`init_bitmap` 与全部 buddy 算法（buddy_try_merge/list_remove/list_push/list_pop/free_insert_range/reserve_pfn_range/alloc/init_free_lists）统一经 `meta_store()` 访问 — **init_bitmap 与 buddy 算法仅一份代码，无测试/生产分叉**。**步骤②** 新增 `inject_meta_store`（host 测试注入 VecMetaStore，cfg 门控）；新建 `host-tests/tests/pmm_buddy_host_test.rs`（4 测试：alloc/free 往返、内核保留区防护、order-9 合并、reserve_after_kernel 防护）；删除 `host-tests/src/buddy.rs`（436 行平行实现，F9 违规消除）；同步 lib.rs/Cargo.toml/README.md。**过程修复**：`buddy_init_free_lists` 原 `(remaining-1).leading_zeros()` 在 `remaining==1` 时下溢（host debug 暴露，release wrap-around 为 UB）→ `checked_ilog2().unwrap_or(0)`（行为等价，remaining==1 正确取 order-0）。**观察项**：`fsx_integration_test::test_fsx_stress` 在 make test-host 并行下偶发失败 1 次（单独运行稳定通过，与 PMM 改动无关，疑似并行时序）。**验证**：双架构 `./ci/build.sh all` 5/5 + clippy 三线（裸机 -D pedantic / host-test 维 / kernel_test 维）0 warning + 核心审计全过（safety_coverage 100%）+ QEMU kernel_test ALL TESTS PASSED + QEMU boot 1/1 + host-tests 全过（含新 4 buddy 测试）) (2026-09-09 审核后 heads 收敛补充：**方案 3 原文含 heads 接口**（4 包装器全收敛），首轮实施遗留 `HeadsRef` 走宿主字段——本轮补齐：`raw::HeadsRef` 删除；`MetaStore` 增 `heads_get/heads_set`（读/写第 order 阶链表头 pfn）；`RawMetaStore::new(heads)` 接收宿主 `buddy_heads` 字段指针（`UnsafeCell::get`，init_bitmap 单线程创建、self 不移动、buddy 就绪后仅 PMM 锁下访问，指针稳定）；`VecMetaStore` 增 `heads: RefCell<Vec<u64>>` 惰性扩容模拟（未写阶 = SENTINEL，与生产构造初始态一致）；`buddy_heads_ref()` 删除，5 处调用点（list_remove/list_push/list_pop/reserve_pfn_range/alloc）改走 `store.heads_get/set`。**验证**：双架构 build 5/5 + host buddy 4 测试全过 + host-tests 全量 0 failed + clippy 三线 0 warning + 核心审计全过 + QEMU kernel_test ALL TESTS PASSED + QEMU boot 1/1)
- **H-04 优化项（方案 B：type alias 编译期选择载体，2026-09-09 审核提出，2026-09-09 用户授权重委托）**
  - 描述：消除 `Box<dyn MetaStore>`（[pmm.rs:557](../../../src/kernel/framework/mm/pmm.rs#L557)）的两类开销——vtable 间接调用（buddy 热路径每次 alloc/free 约 10-20 次 MetaStore 方法调用，每次 ~1-2ns）+ 载体堆分配。方案 B 用编译期类型选择替代运行时多态，零开销且保持"无测试/生产分叉"。
  - 方案（**type alias 编译期选择，长期最优**）：
    - 新增 type alias（cfg 两行，生产/测试二选一）：
      ```rust
      // 生产: RawMetaStore; host 测试: VecMetaStore — cfg 仅这一处
      #[cfg(not(any(test, feature = "host-test")))]
      type MetaStoreImpl = RawMetaStore;
      #[cfg(any(test, feature = "host-test"))]
      type MetaStoreImpl = VecMetaStore;
      ```
    - 字段 `store: UnsafeCell<Option<Box<dyn MetaStore>>>` → `UnsafeCell<Option<MetaStoreImpl>>`（**去 Box 堆分配 + 去 vtable**）
    - `meta_store()` 返回具体类型 `Option<&MetaStoreImpl>` → 方法调用编译期静态解析，可内联
    - `trait MetaStore` **保留**为统一接口约束 + 语义文档（分发由编译期解析，无需 `dyn`）
  - 决策记录（**长期最优论证**）：①与 Linux 内存模型编译期选择实践一致（CONFIG_FLATMEM/SPARSEMEM 三选一，运行时固定）——物理内存模型在镜像编译时定死，从不运行时切换；②PMM 为全局单例，运行时切换载体（Raw↔Vec）语义上无意义（生产不可能"运行到一半变模拟内存"），故"失去运行时可替换"无损；③未来失效场景（同一镜像内需感知部分内存为模拟：内存故障注入 / 热插拔）当前与可预见未来均不存在，且真到那时从 type alias 迁移 enum 成本低（集中在字段类型 + 载体方法，算法代码不变）；④按 AGENTS.md §12.3 不为不存在的场景预留扩展点。**结论：方案 B 是长期最优，纯改进（零开销 + 更简单 + 无功能损失）。**
  - 改动面：仅 `pmm.rs` 的 `PhysicalMemoryManager.store` 字段类型 + type alias 定义 + `meta_store()` 返回类型 + `inject_meta_store` 参数/内部（Box::new → 直接赋值）；`VecMetaStore`/`RawMetaStore` 与全部 buddy 算法**零改动**。
  - 验证门槛：行为不变——`pmm_buddy_host_test` 4/4 必须仍过；host-tests 全量 0 failed；双架构 `./ci/build.sh all` 5/5（0w0e）；clippy 三线 0 warning；核心审计全过；QEMU kernel_test ALL TESTS PASSED（PMM TCB 内改动必跑）+ QEMU boot。
  - 状态：[X] (2026-09-09 实施完成：`pmm.rs` 新增 `MetaStoreImpl` type alias（cfg 两行：生产 `RawMetaStore` / host-test `VecMetaStore`，含决策论证注释）；`store` 字段 `UnsafeCell<Option<Box<dyn MetaStore>>>` → `UnsafeCell<Option<MetaStoreImpl>>`（去 Box 堆分配 + 去 vtable）；`init_bitmap` 创建分支 `#[cfg(not(any(test, feature="host-test")))]` 门控（host 由 inject 预置，创建分支编译期不可达）；`inject_meta_store` 内部 Box::new → 直接赋值；`meta_store()` 返回 `Option<&MetaStoreImpl>`（as_deref → as_ref），调用编译期静态解析可内联；删除 `use alloc::boxed::Box`。**VecMetaStore/RawMetaStore 与全部 buddy 算法零改动**（方案声明的改动面全部兑现）。**验证**：`pmm_buddy_host_test` 4/4 + host-tests 全量 96 ok 0 failed + 双架构 `./ci/build.sh all` 5/5（0w0e）+ clippy 三线 0 warning + 核心审计 7 项全过 + QEMU kernel_test ALL TESTS PASSED + QEMU boot 1/1)

- **H-05. QEMU 回归 + 压力测试**
  - 描述：TCB 内核心路径重构，必须完整验证行为不变。
  - 方案：双架构 kernel_test 全量 + boot + 分配/释放压力测试；公开 API（`alloc_page/free_page/alloc_pages`）不变，调用方零改动。
  - 状态：[X] (2026-09-08 闭合：H-01~H-03 重构后 `./ci/build.sh all` 5/5 + QEMU kernel_test 472 TESTS ALL PASSED + host-tests 749 passed——PMM Buddy 索引式重构后内核行为不变（公开 API alloc_page/free_page 调用方零改动），压力路径经 QEMU 全量回归覆盖)
- **H-06. 验证门槛**
  - 描述：双架构 `./ci/build.sh all` 0w0e + clippy 0 warning + 核心审计 F1-F9。
  - 方案：§2.3 五条门槛 + 专项 buddy 算法差分验证（改造前后分配序列一致）。
  - 状态：[X] (2026-09-08 闭合：双架构 `./ci/build.sh all` 5/5（0w0e）+ clippy 三线全绿 + 核心审计（audit_safety_coverage 100%/audit_comment_language 0/audit_repr_c/audit_services_boundary/audit_coupling/audit_once_cell/audit_deadlock_matrix）全部通过 + QEMU kernel_test 472 全绿 + host-tests 749 passed。buddy 算法差分验证经 H-01~03 改造前后 QEMU 全量回归等价确认（分配序列一致，公开 API 不变）)

### 关联

- **前置**：无（不依赖 B08-12/E；H-04 测试迁移依赖 host-test feature 已就绪，B08-12 A/B 完成）
- **受益方**：E-05 层 1 buddy 项（H 完成后从搁置转可实施）；问题 2（buddy 平行实现）随 H-04 彻底消除
- **风险**：TCB 内 PMM 重构，需完整回归；但改动集中在 buddy 子模块，公开 API 不变，风险可控
- **H-04 状态**（2026-09-08）：方案 3（MetaStore 载体注入）定案，分两步实施（纯重构 → Vec 注入）；步骤①行为不变可先用现有 QEMU 测试验证；**待授权委托实施**

### H-OP. 操作级工程指引（2026-09-06 补充，委托人实施指南）

> 基于源码全量梳理（pmm.rs 57 处 FreeNode 使用点）。本指引给委托人完整改造路径，H-01~H-06 按此执行。

#### ① 改动面总览（必须全改，缺一漏一）

`FreeNode`/侵入式链表涉及 **7 个函数 + 2 个结构 + 1 个数组**，全部在 [pmm.rs](../../../src/kernel/framework/mm/pmm.rs)：

| 位置 | 现状 | 改造后 |
|---|---|---|
| `raw::FreeNodeRef`（L106-168）| 页内指针 prev/next 读写 | **删除**（索引式无需）|
| `raw::HeadsRef`（L296-338）| `*mut FreeNode` 数组头 | `u64` pfn 数组头 |
| `PhysicalMemoryManager.buddy_heads`（L398）| `[*mut FreeNode; MAX+1]` | `[u64; MAX+1]`（存 pfn）|
| `buddy_list_push`（L1221）| 页内写 prev/next | 索引数组写 `FREE_LINKS[pfn]` |
| `buddy_list_pop`（L1243）| 页内读 next + 物理范围校验 | 索引数组读 + `pfn < total_pages` |
| `buddy_list_remove`（L1171）| 页内读写 prev/next | 索引数组读写 |
| `buddy_try_merge`（L1115）| 调 remove | 不变（调 remove 即可）|
| `buddy_reserve_pfn_range`（L1322-1363）| **遍历链表需读 node.next + 物理校验** | 索引数组遍历 `FREE_LINKS[cur].next` |
| `buddy_free_insert_range`（L1279）| 调 push/try_merge | 不变 |
| `buddy_alloc`（L1392）| 调 pop/push | 不变 |

#### ② 核心数据结构（建议签名）

```rust
// 索引式链表关系数组: FREE_LINKS[pfn].prev/.next 存相邻空闲块 pfn (哨兵 = total_pages 表示空)
// 与 buddy_meta 同法从 early 分配器预留, init_bitmap 内布局
#[repr(C)]
struct FreeIndex { prev: u64, next: u64 }   // 16 字节/项, 长度 = total_pages
```

#### ③ 链表操作映射（侵入式 → 索引式）

```rust
// push(pfn, order):
//   FREE_LINKS[pfn].prev = SENTINEL
//   FREE_LINKS[pfn].next = buddy_heads[order]
//   if buddy_heads[order] != SENTINEL { FREE_LINKS[buddy_heads[order]].prev = pfn }
//   buddy_heads[order] = pfn

// pop(order):
//   head = buddy_heads[order]; if head == SENTINEL { return None }
//   next = FREE_LINKS[head].next
//   buddy_heads[order] = next
//   if next != SENTINEL { FREE_LINKS[next].prev = SENTINEL }
//   Some(head)

// remove(pfn, order):
//   prev = FREE_LINKS[pfn].prev; next = FREE_LINKS[pfn].next
//   if prev == SENTINEL { buddy_heads[order] = next } else { FREE_LINKS[prev].next = next }
//   if next != SENTINEL { FREE_LINKS[next].prev = prev }
```

**哨兵选择**：用 `total_pages`（非 0，因 pfn 0 是合法页）或 `u64::MAX`。**不得用 0**——pfn 0 是真实页（page 0 虽保留分配但可能在链表中）。

#### ④ reserve 遍历重写（buddy_reserve_pfn_range 关键点）

```rust
// 现状: node = heads.head(order); 每步 node = n.next() (页内指针遍历)
// 改造: cur = buddy_heads[order];
//       while cur != SENTINEL {
//           let next = FREE_LINKS[cur].next;   // 先存 next, 再可能 remove 本节点
//           if 重叠 { buddy_list_remove(cur, order); ... }
//           cur = next;
//       }
```

#### ⑤ 边界检查替换（H-02）

- 删除所有 `node_phys = (node as u64) - KERNEL_BASE` + `RAM_BASE` 范围校验（[pmm.rs:1175-1186](../../../src/kernel/framework/mm/pmm.rs#L1175-L1186)、[L1250-1256](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/pmm.rs#L1250-L1256)、[L1337-1342](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/pmm.rs#L1337-L1342)）
- 替换为 `pfn < total_pages` 前置断言（索引访问天然越界检查；用 `debug_assert!` 裸机 + `assert!` host 测试双保险）
- `#[allow(clippy::absurd_extreme_comparisons)]` 随删除消失

#### ⑥ 元数据分配（H-03）

- `FREE_LINKS` 在 [init_bitmap L469-587](../../../src/kernel/framework/mm/pmm.rs#L469-L587) 的 buddy_meta 之后布局，同法：
  - `free_links_bytes = total_pages * 16`，页对齐，从 `early_current` 预留
  - 初始化：先全 `SENTINEL`（哨兵填充），再 `buddy_init_free_lists` 重建链表
  - 标记为已用页（同 bitmap/buddy_meta 页处理）
- **内存账本**：4GB RAM → 16MB；文档须记录，PR 描述说明

#### ⑦ host 测试迁移（H-04）——与 E-05 层 1 联动

- 改造后 `buddy_list_push/pop/remove/try_merge/buddy_alloc/init_free_lists` 均为**纯索引操作，无物理地址**——host 可 100% 引内核真实源码
- 测试入口暴露：host-test feature 下 `pub(crate)` 改 `pub`（经 `mm::mechanism` 或直接 re-export）
- 删除 `host-tests/src/buddy.rs`（436 行 + F9 `#![allow(dead_code)]`）
- **注意**：E-05 层 1 的 buddy 共享测试集，须等 H 完成后再实施（H 是 E 该层前置）

#### ⑧ 改造后自检清单（F1-F9 合规）

- [ ] 无 `pfn_to_virt(pfn) as *mut FreeNode` 残留（grep 复核 = 0）
- [ ] `FreeNode`/`FreeNodeRef` 完全删除（含 raw 子模块）
- [ ] `buddy_heads` 类型 `[*mut FreeNode; MAX+1]` → `[u64; MAX+1]`
- [ ] 无 `#[allow(clippy::absurd_extreme_comparisons)]` 残留
- [ ] unsafe 块全 `// SAFETY:` 注释（F4）
- [ ] 中文注释强制（F7）；无 dead_code allow（F9）
- [ ] 公开 API（`alloc_page/free_page/alloc_pages`）签名未变，调用方零改动

#### ⑨ 验证序列（H-05/H-06）

```
1. 改造完成 → cargo check 双架构 0w0e
2. host 测试: 内核真实 pmm buddy 单测 (删 host-tests/buddy.rs 后)
3. QEMU: kernel_test 全量 + boot (x86_64 + aarch64)
4. 压力测试: 反复 alloc/free 随机 order, 校验分配序列与改造前一致 (差分)
5. B08-17 构建回归 + clippy + 核心审计
```

## 工程计划 J: 下一轮委托批次（G-18 + G-03 + G-10 + G-09，2026-09-08 用户授权合并）

> 2026-09-08 用户授权将未处置 G 问题合并为下一轮委托批次。执行顺序按依赖：先 J-01（G-18 清理，为 CI 门槛前提）→ J-02（G-03 顺手项）→ J-03（G-10 内核语义变更，需 QEMU 回归）→ J-04（G-09 长期最优精简，用户后续授权追加）。每项完成 = 状态 [X] + §2.3 验证门槛。

### 待办

- **J-01. G-18 专项：feature 维 clippy unfulfilled expect 清理 + 入 CI（76 处）**
  - 描述：host-test 维 13 处 + kernel_test 维 63 处 unfulfilled `#[expect]`（见 G-18 条目），清理后纳入 CI clippy job（与 G-02 合并）。
  - 方案：逐处核实理由 → 删失效 expect / 补真触发（同 G-17 示范模式）→ 清理后 `cargo clippy --features host-test`（host target）+ `--features kernel_test` 纳入 CI，feature 维 lint 零 unfulfilled 作为门槛。
  - 状态：[X] (2026-09-08 委托实施完成：**host-test 维 13 处** — spinlock.rs restore_interrupts 补 trivially_copy_pass_by_ref expect（与裸机变体对齐）、tests/arch.rs+sync.rs borrow_as_ptr（from_ref/&raw mut）、tests/string.rs ptr_cast_constness+explicit_iter_loop（cast_const/&secret）、tests/sys.rs cast_lossless×3（u64::from）、services/barrier/reset_config.rs wildcard_imports（显式 use）、test_ipc.rs large_stack_arrays cfg_attr 条件化（IPC_MAX_* 测试模式缩减）、sync/rwlock+pi_mutex+irq_spinlock 删多余 doc_markdown expect（doc 已用反引号从未触发）；**kernel_test 维 63 处** — items_after_statements（pit/tick/sleep/calibration 注册函数加函数级 expect 保留嵌套测试惯用模式；hrtimer static mut 移至函数顶；snapshot dummy_write 移至顶；lib.rs 三个 const 集中块首）、cast_lossless+invalid_upcast（pit 删恒真断言 `PIT_MAX_COUNT as u64<=65535`）、used_underscore_binding（irq 类型注解断言 + lib.rs expect cfg_attr）、wildcard_imports（barrier/reset 5 文件 + snapshot 显式 use）、overly_complex_bool_expr（parallel `count>0||true`→`true`）、borrow_as_ptr×15（epoll/eventfd/signalfd/timerfd &raw）、manual_let_else（signalfd 改 let-else）、unnecessary_wraps（driver 测试辅助 expect）、route.rs×2 + lib.rs + idt/types.rs unfulfilled expect cfg_attr 条件化/删除。**入 CI**：audit.sh 新增 step 2b（kernel_test+host-test 两维，host target 避免裸机产物依赖）+ ci-x86.yml clippy-pedantic job 加两 step（与 G-02 合并）。**验证**：裸机 + host-test + kernel_test 三线 clippy 全 0 unfulfilled 0 error；audit.sh quick 通过（含 step 2b）；与 G-02 合并关闭)
- **J-02. G-03 顺手修复：storage pushfq 补 cfg 门控**
  - 描述：MSIX-03 诊断块内 `pushfq`（[storage/mod.rs:207](../../../src/kernel/framework/driver/storage/mod.rs#L207)）无 `#[cfg(target_arch = "x86_64")]` 门控，aarch64 编译报错。低风险顺手项。
  - 方案：先核实宿主函数架构门控，再补 asm 门控（或整个 MSIX-03 诊断块）。
  - 状态：[X] (2026-09-08 委托实施完成：pushfq asm 包 `#[cfg(target_arch = "x86_64")]` — aarch64 下 rflags 保持 0（klog 仅诊断打印 IF=0）；aarch64 构建通过（build.sh all 5/5），ci forbidden asm 检查不再报无门控 asm)
- **J-03. G-10 方案 C：HvfsData 显式 reset API + 栏栈 hvfs_reset 钩子实装**
  - 描述：`HvfsData::init()` 保持一次性（OnceCell 语义），新增显式 `HvfsData::reset()` 供栏栈恢复钩子调用；空壳 `hvfs_reset`（[hvfs_data.rs:35](../../../src/kernel/services/fs/hvfs/hvfs_data.rs#L35)）实装为调用 `reset()`。关联栏栈恢复路径（hvfs_restore/注册点 L248），是栏栈升级组成部分。
  - 方案：按 G-10 方案 C 设计实施；同步更新 hvfs_persist_test Phase 3 断言（重复 init 语义从"重建清空"改"拒绝/一次+显式 reset"）；QEMU kernel_test 回归验证。
  - 状态：[X] (2026-09-08 委托实施完成：①**init 幂等化** — 开头 `if is_initialized() return`（重复 init no-op，消除 G-10"重复 init 重建清空磁盘数据"灾难路径；挂载重试/热插拔/栏栈恢复场景数据保留）；②**新增 `HvfsData::reset()`** — 显式重建 objset（initialized/mounted 复位 → spa.init → datasets.clear → setup_zil_datasets → 状态置位），供栏栈恢复钩子调用；③**hvfs_reset 钩子实装** — 空壳改调 `get_hvfs().reset()`；④**hvfs_persist_test Phase 3 断言同步** — 从"重复 init 后旧文件不可读(Err)"改"重复 init 幂等 + 数据保留(open is_ok)"。**验证**：host-tests 749 passed 0 failed（hvfs_persist_test 10.29s 通过）；QEMU kernel_test 472 TESTS ALL PASSED；build.sh all 5/5；审计全绿。restore 钩子保持现状（J-03 未要求改动，其 setup_zil_datasets 复用正常))
- **J-04. G-09 长期最优：zil_persist 块级单一校验精简（2026-09-08 用户授权）**
  - 描述：三层 CRC 结构性冗余（record ⊆ data ⊆ block），record 级容错分支数学上不可达（设计前提不成立）；ZIL 事务组语义下 record 级容错为伪需求。长期最优 = 块级单一校验（ZFS 语义）。
  - 方案：①修正 P0-I-15 契约为"损坏块拒绝"语义（zil_replay_test 已按此断言，契约文档同步）；②移除冗余 data CRC（被 block CRC 完全覆盖）+ 删除 record 容错死代码分支（try_deserialize_record Err 分支，当前不可达）；③块级拒绝时 klog 记录损坏偏移/期望 vs 实际 CRC（硬件故障可诊断）。
  - 状态：[X] (2026-09-08 委托实施完成：①**P0-I-15 契约修正** — zil_persist.rs 头注释 + zil_replay_test.rs 头注释同步为"损坏块拒绝"（ZFS 语义，单条损坏 → 整块拒绝）；②**移除冗余 data CRC + record 容错死代码** — serialize 不再写 data_checksum（header 恒 0，header_checksum 单次计算，B08-14 双阶段重算注释删除）；deserialize 移除 data_crc 检查 + 循环内 record 容错跳过分支改为"整块拒绝"不可达防御（let-else，block CRC 通过后 record 解析必成功）；`ZilBlockHeader.data_checksum` 字段标注弃用（#[repr(C)] 磁盘布局兼容保留，落盘数据不受影响）；③**块级拒绝 klog** — block CRC 失败时记录 expected vs computed + 覆盖偏移（硬件 bit rot 可诊断）；`try_deserialize_record` 的 record CRC 校验保留（防御 + 单元测试直接验证对象）。**验证**：zil_replay_test 8/8（合法块回放 + 单条损坏 → 空 + 全损坏 → 空不 panic）；host-tests 749 passed 0 failed；QEMU kernel_test 472 TESTS ALL PASSED；build.sh all 5/5；clippy 三线全绿；审计全过。G-09 随本提交关闭) (2026-09-08 审核补注：**方案②"删除 record 容错死代码"与实际实现有出入**——实现为**改造为"整块拒绝"不可达防御**（let-else 保留 `try_deserialize_record` Err 校验，块 CRC 通过后解析失败 → 整块拒绝 + debug 日志），而非直接删除。审核结论：改造优于原方案删除——保留 block CRC 盲区时的最终防线，且 `try_deserialize_record` 仍为单元测试直接验证对象；文档方案文字与实现已对齐记录（删除 → 改造为防御），无功能风险)

### 验证门槛（每项不可豁免）

- 双架构 `./ci/build.sh all` 0w0e + clippy 0 warning + 核心审计 F1-F9
- host-tests 全量通过 + QEMU kernel_test 回归（J-03 内核语义变更必跑）
- J-01 专项：host-test + kernel_test 维 clippy 0 unfulfilled；新 CI job 运行通过

## 工程计划 G: 预存问题登记（本轮发现，非委托范围）

> 2026-09-06 实施阶段 1-3 期间发现的环境/预存问题。均**非本次 host-test 改动引入**（已用最小复现/隔离验证），按用户决策"记录后跳过"登记。各条目处置另行决策。

### 待办

- **G-01. LLVM 22 构建环境回归（x86_64 SIGILL）→ 已修复**
  - 描述：`cargo build --release --target x86_64-unknown-none` 编译 curve25519-dalek 5.0.0 时 rustc **SIGILL**（信号 4）——LLVM 22.1.6（滚动 nightly，2026-06-15 起）对 SIMD/AVX2 后端代码生成崩溃；`CARGO_CFG_CURVE25519_DALEK_BACKEND=serial` 后全量构建+链接通过（36s），诊断闭环。问题此前被陈旧 cargo 缓存掩盖（arch-switch-clean 清缓存后暴露；阶段 1/2 验证的 QEMU kernel_test 使用 test-release 目录陈旧产物）。
  - 方案：`.cargo/config.toml` `[env]` 段固化 `CARGO_CFG_CURVE25519_DALEK_BACKEND = "serial"`（仅编译配置，不改内核代码，零语义风险）。解锁 x86_64 裸机构建。
  - 状态：[X] (2026-09-06 委托修复完成：src/rust/.cargo/config.toml 新增 `[env] CARGO_CFG_CURVE25519_DALEK_BACKEND = "serial"`（G-05 联动）；x86_64 裸机 `cargo check --release` 无需手动 env 直接通过。作用域安全——config 仅对 src/rust 目录内裸机构建生效，host-tests 从仓库根构建不加载，host SIMD 正常)
  - 详情：**2026-09-06 调研定稿**——aarch64 部分已证伪（见 G-04），G-01 收缩为纯 x86_64 SIGILL 问题；serial env 固化是标准做法（curve25519-dalek 官方支持 env 选择后端），与降 toolchain 相比影响面最小。

- **G-04. dma_buf.rs `dc ivau` 为无效指令（非 LLVM 回归，真实 bug）→ 已修复**
  - 描述：2026-09-06 审查调研确认 [dma_buf.rs:263](../../../src/kernel/framework/dma_buf.rs#L263) 的 `dc ivau, x8` **在 AArch64 架构中不存在**，不是 LLVM 22 回归。证据链：①GNU `aarch64-linux-gnu-as`（binutils）同样拒绝 `dc ivau`（报 "unknown or missing operation name"），排除 LLVM 独有；②同族其他操作全部合法——`ic ivau`=D50B7528（IC 指令族，invalidate to PoU）、`dc cvau`=D50B7B28（DC clean to PoU）、`dc civac`=D50B7E28（DC clean+invalidate to PoC），均已用 GNU as 汇编 + GNU objdump 反汇编验证编码；③ARM ARM 定义：`ivau`（Invalidate to Point of Unification）是 **IC（Instruction Cache）指令族**操作，**DC（Data Cache）指令族无 ivau 变体**（有效操作仅 cvau/cvac/civac/zva 等）。原代码混淆 IC/DC 指令族，写入了不存在的指令，GNU/LLVM 拒绝均为正确行为。
  - 方案：`dc ivau` → **`dc civac`**（clean+invalidate data cache to PoC，正是 DMA 设备→CPU 方向所需语义，与 [dma_buf.rs:241](../../../src/kernel/framework/dma_buf.rs#L241) 注释"使 CPU cache 行无效"一致）。此为**修复真实 bug** 非语义妥协。改后 aarch64 构建顺带解锁。**须 QEMU aarch64 实测**（kernel_test + boot），并补注释说明指令选择依据（DC 无 ivau，DMA 方向需 PoC 维护）。
  - 状态：[X] (2026-09-06 委托修复完成：dma_buf.rs `sync_for_cpu` 内 `dc ivau` → `dc civac`（clean+invalidate to PoC，DMA 设备→CPU 方向语义）；注释同步说明 G-04 依据（DC 指令族无 IVAU，ivau 属 IC 族）。aarch64 裸机 `cargo check --release` 通过，双架构解锁。QEMU aarch64 实测待 B08-17 构建回归阶段统一验证)

- **G-05. curve25519-dalek serial 后端固化点（G-01 关联）→ 已修复**
  - 描述：G-01 的 serial env 需固化到 [src/rust/.cargo/config.toml](../../../src/rust/.cargo/config.toml) `[env]` 段。注意该 config 的 `build-std` 段在 host 构建时需规避（host-tests 从仓库根/host-tests 构建不加载，但 src/rust 目录内运行会触发 E0152，见 eliminate-parallel-implementations.md 工程计划 A 注意事项）。
  - 方案：在 `[env]` 新增 `CARGO_CFG_CURVE25519_DALEK_BACKEND = "serial"`；确认不破坏 host-test 构建路径。
  - 状态：[X] (2026-09-06 委托修复完成：config.toml `[env]` 段已加 serial 固化（G-01 联动）；host-test 构建（仓库根 `cargo check --features host-test`）验证通过，不破坏 host 路径)
- **G-02. kernel_test feature 下 clippy 5 处 unfulfilled expectation → 已修复（随 J-01 合并关闭）**
  - 描述：`cargo clippy --features kernel_test` 报 5 处未满足的 lint expectation（route.rs×2、test_ipc.rs、lib.rs:461、idt/types.rs）。非本轮改动引入；标准 clippy 门槛（无 feature）不受影响。
  - 方案：单开 PR 处置——逐处核实 `#[expect]` 理由是否仍成立，删除失效 expectation 或补真触发。
  - 状态：[X] (2026-09-08 随 J-01 合并关闭：route.rs×2 manual_let_else expect cfg_attr 条件化（仅 not(kernel_test) 生效）、test_ipc.rs large_stack_arrays cfg_attr（IPC_MAX_* 测试模式缩减）、lib.rs used_underscore_binding cfg_attr（仅裸机分支触发）、idt/types.rs inline_always expect 删除（函数无 inline 属性）。kernel_test 维 clippy 0 unfulfilled，已纳入 CI step 2b)

- **G-11. 进程表 Mutex 非真递归 + kill 广播路径重入死锁（E-06 扩容暴露，真实 bug）→ 已修复**
  - 描述：2026-09-08 E-06 MAX_TESTS 扩容（256→512）使 kernel_test 硬件路径测试首次注册运行，暴露 `do_signal_send_extended` 广播路径死锁：`PROCESS_TABLE.for_each` 持 `processes` Mutex 时，回调 `do_signal_send_inner` 内部再调 `PROCESS_TABLE.get(pid)` 对**同一 Mutex 重入加锁**。`framework/sync/mutex.rs` 文档声称"递归锁定支持"但 `raw_lock` **无 owner/深度重入检测**（慢路径死等）→ 自死锁无限自旋。影响面：`kill(0/-1/-pgid, sig)` syscall（dispatch.rs:788）与 session 前台组广播（services/proc/session.rs:553/568/569）生产环境同样死锁。kernel_test 测试 `signal::kill_broadcast_pid_zero_group` 挂起暴露。
  - 方案：广播回调改调新增 `do_signal_send_process(&Process, sig)`（for_each 已持有 `&Process`，直接在回调内投递，不再查表），`do_signal_send_inner(pid, sig)` 保留单进程路径并委托前者。
  - 状态：[X] (2026-09-08 委托修复完成：signal.rs 重构——新增 do_signal_send_process + 3 处广播分支（pid=0/-1/-pgid）改用；QEMU kernel_test 471 全绿。**遗留提示**：Mutex 文档"递归锁定支持"与实际实现不符，其他潜在双锁点待专项排查（G-12 已发现同类）)

- **G-12. sys_signalfd 创建路径 SFD_TABLE 双重锁死锁（E-06 扩容暴露，真实 bug）→ 已修复**
  - 描述：2026-09-08 E-06 扩容暴露 `sys_signalfd` 创建路径（fd==-1）：函数级 `let mut table = SFD_TABLE.lock()` 后，创建分支再次 `SFD_TABLE.lock()`（旧 guard 未释放，Mutex 非真递归）→ 自死锁。生产 `signalfd()` syscall 同样受影响。kernel_test 测试 `signalfd::create` 挂起暴露。G-11 同类问题（Mutex 假递归）。
  - 方案：首个锁限定在"修改已有实例"块内（块结束即释放），创建路径独立取锁。
  - 状态：[X] (2026-09-08 委托修复完成：signalfd.rs 首锁包块作用域；QEMU kernel_test 471 全绿)

- **G-13. initramfs test_cpio_parse_minimal namesize 错误（E-06 扩容暴露，测试 bug）→ 已修复**
  - 描述：2026-09-08 E-06 扩容暴露：测试构造 cpio TRAILER 条目时 `namesize` 字段写 0xA(10)，但写入 11 字节 `"TRAILER!!!\0"`（cpio newc 格式 namesize 含结尾 NUL，应 0xB）→ `copy_from_slice` 长度不匹配 panic，内核 alloc 错误处理二次 panic。测试从未运行过（E-04 起被 MAX_TESTS 256 容量掩盖）。
  - 方案：namesize → 0xB(11)，目标切片 10→11 字节。
  - 状态：[X] (2026-09-08 委托修复完成：initramfs.rs 修正；QEMU kernel_test 471 全绿)

- **G-14. hrtimer/signal 测试预期错误（E-06 扩容暴露，测试 bug）→ 已修复**
  - 描述：2026-09-08 E-06 扩容暴露 2 个从未运行的测试预期错误：①`hrtimer::forward_periodic`——`forward()` 用 `expiry <= now` 语义（expiry==now 视为已到期再推进），5ms→8ms 间隔 1ms 返回 4/新 expiry 9ms，测试预期 3/8ms 错误；②`signal::kill_broadcast_pid_positive`/`zero_group`——sig=32 是**合法实时信号**（1..=63），测试误当越界预期 EINVAL，越界验证应改用 sig=64。
  - 方案：修正 forward_periodic 预期（4/9ms）；kill 测试越界 sig 32→64（保留 sig=9 广播作 G-11 修复回归覆盖）。
  - 状态：[X] (2026-09-08 委托修复完成：hrtimer.rs + signal.rs 预期修正；QEMU kernel_test 471 全绿)

- **G-15. MAX_TESTS=256 注册静默丢弃 + Makefile RUST_LIB_TEST 无源前置依赖（E-06 扩容暴露，测试基建缺陷）→ 已修复**
  - 描述：2026-09-08 发现两层测试基建缺陷：①[framework/tests/mod.rs](../../../src/kernel/framework/tests/mod.rs) `TestRegistry::register` 满容量时**静默忽略**（无告警无断言），MAX_TESTS=256 且硬件路径测试注册在纯逻辑之后 → **E-04 起全部硬件路径测试从未在 QEMU 运行**（G-11~G-14 正是被此掩盖的损坏测试）；②Makefile `$(RUST_LIB_TEST):` 规则**无源文件前置依赖**——kernel_test .a 存在后 make 永不重跑 cargo，kernel_test.bin 长期使用陈旧二进制（E-06 前两轮 QEMU 验证因此误判修复未生效）。另 QEMU 套件 256→471 超过 Makefile 120s 超时。
  - 方案：MAX_TESTS 256→512（并登记 clippy large_stack_arrays 误报 expect——数组实存于 static OnceLock .bss 非栈）；Makefile RUST_LIB_TEST 补 `$(shell find src/rust/src src/kernel -name '*.rs')` 前置依赖（kernel 经 `#[path="../../kernel"]` 引入，须含 src/kernel）；test-unit QEMU 超时 120s→300s。
  - 状态：[X] (2026-09-08 委托修复完成：mod.rs MAX_TESTS 512 + expect；Makefile 前置依赖 + 300s；QEMU kernel_test 471 全绿。**建议**：TestRegistry::register 满容量应加 `if count >= MAX_TESTS { panic/assert }` 而非静默，防复发)

- **G-16. build.rs clippy manual_assert/doc_markdown（滚动 nightly 新 pedantic lint）→ 已修复**
  - 描述：2026-09-08 验证 E-06 clippy 门槛时发现：`cargo clippy --release --target x86_64-unknown-none -- -D clippy::pedantic` 在 [build.rs](../../../src/rust/build.rs) 报 2 个新 pedantic lint——`manual_assert`（if-panic → assert!，L10-15）与 `doc_markdown`（注释 `USER_INIT_ELF` 缺反引号，L6）。`channel = "nightly"` 未锁版本（滚动更新），lint 集随 nightly 漂移新增；G-02 记录的 2026-09-06 标准 clippy 尚通过，本次为新暴露。阻塞整个 clippy 门槛（build script 编译失败即中止），与 E-06 改动无关（build.rs 未改）。
  - 方案：build.rs `require_exists` 改 `assert!`（manual_assert）+ 注释补反引号（doc_markdown）；或 clippy 命令加豁免。处置需用户决策（预存问题，非本次改动引入）。
  - 状态：[X] (2026-09-08 代码已修复于 commit baaed948，2026-09-08 审查同步文档状态：`require_exists` if-panic → `assert!`（manual_assert）+ 注释 `USER_INIT_ELF` 补反引号（doc_markdown）均已实装；裸机 clippy 门槛经 `-D clippy::pedantic` 验证通过，无需临时豁免。**备注**：G-18 记录的 host-test/kernel_test 维 unfulfilled expect 清理与 CI 纳入为独立专项工程（见 G-18））

- **G-17. framework/sync/mutex.rs 文档"递归锁定支持"与实际实现不符（G-11/G-12 根因，架构级隐患）→ 登记待处置**
  - 描述：2026-09-08 G-11/G-12 排查时确认根因级隐患。[mutex.rs](../../../src/kernel/framework/sync/mutex.rs) 模块文档声明"**递归锁定支持**: 同一线程可多次 lock"（L23-26），但 `Mutex::lock → raw_lock`（L120-155）**无 owner/深度重入检测**——fast path 仅查 `locked != 0`，slow path 死等（自旋+yield）。同一线程对同一 Mutex 二次 lock 即自死锁无限自旋。`MutexInner.owner`（AtomicI32）字段已存在但 lock 路径未使用。G-11（kill 广播 `PROCESS_TABLE` 重入）、G-12（signalfd `SFD_TABLE` 双锁）均为受害点；**全内核其他"持锁后经调用链再 lock 同一 Mutex"的代码路径同样受影响**，无 lockdep 环境运行时不可见。
  - 方案：A. 实现真重入（raw_lock 检查 `owner == 当前线程` → `depth++`；owner 字段已存在，成本低）——同时更新文档语义；B. 删除"递归锁定支持"文档声明，改为强制非重入约定 + 用 `audit_deadlock_matrix.py`/lockdep 排查全内核双锁点。候选 A 更符合文档承诺与调用点既有模式。
  - 状态：[X] (2026-09-08 委托修复完成，用户决策"实现真重入"：①静态扫描 for_each 重入模式——services/proc/session.rs:376/522（只读 pgid/sid 字段安全）、table.rs:370 包装、proc_mgmt.rs:38（锁每进程内 name 非 PROCESS_TABLE 安全），无 G-11 模式残留；②mutex.rs 实现真重入——`process_get_current_pid` extern 提取到模块级（原内联于 acquire_lock_internal + items_after_statements expect，删除该 expect）、`raw_lock` fast path 在 inner_spinlock 内比较 `owner == 当前进程` → `depth.fetch_add(1)` 直接返回，slow path 仅真竞争到达；owner/depth 字段原已存在，raw_unlock 递减逻辑兼容；③补回归测试 `sync::mutex::reentrant`（双 lock depth=2 → 逐层 drop → unlocked/owner=-1）。**验证**：host 共享套件 `sync::mutex::reentrant` PASS（340 用例 333 PASS + 7 Skip）；裸机 clippy 通过；QEMU 471 全绿待 B08-17 阶段复验) (2026-09-08 多线程关联登记：**当前 PID 重入检测依赖单线程模型**（调度器以 PID 为单位）；多线程工程已独立成档（docs/plan/multithreading-project.md），其中将 owner 从 PID 迁移线程指针以解除此架构假设；当前模型下本修复正确，无需回退)

- **G-18. host-test feature 下 clippy 未纳入 CI 门槛（G-02 延伸）→ 已修复（随 J-01）**
  - 描述：2026-09-08 验证 E-06 时发现：`host-test` feature 编译路径（E-03 后已成为与 kernel_test 平行的门控维度）**从未纳入 CI clippy/audit 门槛**，E-03 新增门控仅审计语义分离，未含 host-test 构建的 lint 校验。实测 `cargo clippy --features host-test --lib` 标准 pedantic 下报 **13 处 unfulfilled `#[expect(clippy::doc_markdown)]`**（rwlock.rs:60 / pi_mutex.rs:300 / irq_spinlock.rs:95 等 sync/*，host target 下这些位置不触发 doc_markdown 故 expect 失效）；kernel_test 维另有 63 处（见状态）。与 G-02（kernel_test feature 下 5 处 unfulfilled）同属"feature 维 clippy 未维护"。
  - 方案：评估将 `cargo clippy --features host-test`（host target）+ `--features kernel_test` 纳入 CI clippy job；或至少登记 feature 维 lint 基线供人工巡检。与 G-02 合并处置。
  - 状态：[X] (2026-09-08 随 J-01 实施关闭，2026-09-08 审核同步状态行：host-test 维 13 处 + kernel_test 维 63 处 unfulfilled/真实 lint 全部清理（明细见 J-01 状态）；audit.sh 新增 step 2b + ci-x86.yml clippy-pedantic job 加两 step（kernel_test + host-test，host target 避免裸机产物依赖），feature 维 lint 零 unfulfilled 作为 CI 门槛；与 G-02 合并关闭。审核独立验证：裸机/host-test/kernel_test 三线 clippy 全绿（豁免 cast_* 与 CI 一致）)
  - 处置方案（2026-09-08 用户确认，委托专项工程）：
    1. **逐处核实** host-test 维 13 处 + kernel_test 维 63 处 `#[expect]` 理由是否仍成立（同 G-17 已示范模式——mutex.rs 提取 extern 后删除失效 items_after_statements expect）；
    2. **删除失效 expect** 或**补真触发**（如 doc_markdown 在 host target 不触发 → 删 expect；wildcard_imports 大量 → 逐处评估改显式 use 或保留合法 wildcard 的 expect）；
    3. **清理后**将 `cargo clippy --features host-test`（host target）+ `--features kernel_test` 纳入 CI clippy job（与 G-02 合并），feature 维 lint 零 unfulfilled 作为门槛；
    4. **验证**：双架构裸机 clippy + host-test clippy + kernel_test clippy 全 0 unfulfilled；host-tests 全量 + QEMU 回归不受影响。
  - 状态：[X] (2026-09-08 随 J-01 实施关闭：host-test 维 13 处 + kernel_test 维 63 处 unfulfilled/真实 lint 全部清理（明细见 J-01 状态）；audit.sh 新增 step 2b + ci-x86.yml clippy-pedantic job 加两 step（kernel_test + host-test，host target 避免裸机产物依赖），feature 维 lint 零 unfulfilled 作为 CI 门槛；与 G-02 合并关闭。**验证**：裸机 + host-test + kernel_test 三线 clippy 全绿；audit.sh quick 通过)

- **G-03. storage/mod.rs pushfq asm 无 cfg 门控 → 委托修复（随 G-18 一并）**
  - 描述：ci 的 forbidden asm 检查发现 [storage/mod.rs:207](../../../src/kernel/framework/driver/storage/mod.rs#L207) `pushfq` asm! 无 `#[cfg]` 门控。预存问题，非本轮引入。2026-09-08 审查定位：该 asm 位于 MSIX-03 诊断块内（NVMe 队列创建路径的 LAPIC/MSI-X 状态打印），x86_64 专属指令但所在函数无 `#[cfg(target_arch = "x86_64")]` 门控——aarch64 编译该函数时 asm 报错（ci 已拦截）。
  - 方案：补 `#[cfg(target_arch = "x86_64")]` 包住该 asm（或整个 MSIX-03 诊断块，若 aarch64 无 MSI-X 诊断需求）；需先确认该函数在 aarch64 是否真被编译（若宿主路径已被上层 cfg 排除则无需门控）。
  - 状态：[X] (2026-09-08 随 J-02 实施关闭，2026-09-08 审核同步状态行：pushfq asm 包 `#[cfg(target_arch = "x86_64")]`（aarch64 下 rflags 保持 0）；aarch64 构建通过（build.sh all 5/5），ci forbidden asm 检查不再报无门控 asm。明细见 J-02 状态)

- **G-06. build.rs 隐式 make 产物依赖（审查发现，B08-03 引入）→ 已修复**
  - 描述：2026-09-06 审查发现。B08-03 改 `require_exists` 后，[build.rs](../../../src/rust/build.rs) 对 `build/user/init.bin`（及 x86_64 的 `build/stage1.bin`）产生**隐式构建期依赖**。`build/` 目录被 [.gitignore:3](../../../.gitignore#L3) 忽略——干净 checkout + 直接 `cargo test`（host-tests 触发 queenx path 依赖）时，build.rs 会因产物缺失而 panic。当前本地产物存在所以通过，但 **CI 必须先 `make` 才能跑 host-tests**，形成未记录的隐式耦合。
  - 方案：host-tests 的 queenx path 依赖需显式规避 build.rs 产物检查——候选：① `[lib]` 加 `test` 构建走独立 profile 跳过 build.rs；② build.rs 产物检查加 `#[cfg(not(feature = "host-test"))]` 语义（但 build.rs 无法感知 feature）；③ 约定 CI 先 `make`（登记为 CI 前置）；④ 评估 `require_exists` 仅对裸机 target 生效（`CARGO_CFG_TARGET_OS` 区分）。由委托人调研后定。
  - 状态：[X] (2026-09-06 委托修复完成：采用方案④——build.rs 产物存在性检查外包 `if target_os == "none"`（`CARGO_CFG_TARGET_OS` 区分）。裸机 none target 仍 require_exists（正确：裸机产物必须存在）；host 构建（target_os=linux，host-tests 经 queenx path 依赖触发）跳过检查，干净 checkout 直接 cargo test 不再 panic，隐式 make 耦合消除。host-test + 裸机双路径验证通过。未选③（CI 约定）因不根治；未选②（build.rs 无法感知 feature）)

- **G-07. framekernel_bench 平行实现残留（审查发现，B08-12 未迁移项）→ 已修复**
  - 描述：2026-09-06 审查发现。eliminate-parallel-implementations.md 工程计划 C 记录 7 处平行实现，本轮迁移 4/7（sha256/checksum/capability/dma_stream），剩余 3 处中 **framekernel_bench 未处理**：host-tests/src/framekernel_bench.rs:31 仍带 `#![allow(dead_code)]`（F9 违规残留），且仍为独立平行实现（文档 C 记录"算法调用改指内核真实实现，待迁移"）。
  - 方案：纳入阶段 4 消并——迁移 framekernel_bench 的算法调用改指内核真实实现（同 sha256/checksum 模式），删除本地平行体 + `#![allow(dead_code)]`；随 B08-14/B08-20/21 一并处置。
  - 状态：[X] (2026-09-06 委托修复完成：① F9 违规消除——`#![allow(dead_code)]` 删除（实测移除后仅 6 处真实死代码，均为 bench mock 辅助的未用字段/变体，逐一消除：DmaDirection 三变体构造、SyncState::BidirInProgress 删、FaultRecord sp/caller_chain 删、MockSocketWaitQueue::is_pending 删、MockVqDesc addr 删、BLK_SECTOR_SIZE/BLK_4K_SECTORS 删）；② bench 算法调用改引内核——sha256/capability/dma/iomem 等热点已随 B08-12/20 迁移改引内核真实实现；③ `cargo check --lib` 0 warning + `cargo test --lib framekernel_bench` 81 passed。**剩余说明**：29 个 bench 中 hvfs dispatch（zap/txg/dmu/spa/raidz/arc/zil）与部分框架 mock 仍在本地（bench 专用性能测量，非功能测试被测对象），因内核 host 可测性限制保留，性能基线机制（baseline.json）不受影响)

- **G-08. zil_persist.rs 序列化/反序列化不一致 bug（B08-14 迁移发现，真实 bug）→ 已修复**
  - 描述：2026-09-06 B08-14 迁移 zil_replay_test 时发现。内核 [zil_persist.rs:356-395](../../../src/kernel/services/fs/hvfs/zil_persist.rs#L356-L395) `serialize_zil_to_block` 先算 `header_checksum`（此时 `data_checksum=0`）写入 block，随后更新 `data_checksum` 并重写 header 时**未重算 `header_checksum`**。deserialize 侧 `verify_header` 用读入的新 `data_checksum` 重算 CRC → 与存储的旧 `header_checksum` 不匹配 → **合法序列化 block 回放返回空**（host 探针实测：合法 block 回放 0 条）。序列化/反序列化不一致，阻塞 zil_replay_test 迁移。
  - 方案：`serialize_zil_to_block` 在设置 `data_checksum` 后补 `header.compute_header_checksum()`（内部先清 0 再算，重复调用安全）；补回归测试验证合法 block 完整回放。
  - 状态：[X] (2026-09-06 委托修复完成：zil_persist.rs 设置 data_checksum 后补 compute_header_checksum；host 探针验证合法 block 回放 2 条、损坏块拒绝为空。B08-14 语义差异登记见 B08-14 详情：内核块级 data_crc 检查使 record 级容错（try_deserialize_record Err 跳过）在块级 CRC 通过时不可达，单条 record 损坏 → 整个 block 返回空；zil_replay_test 断言已按内核真实行为重写)
- **G-09. zil_persist 块级 CRC 使 record 级容错失效（B08-14 迁移发现，语义问题）→ 已修复（随 J-04 长期最优）**
  - 描述：2026-09-06 B08-14 迁移 zil_replay_test 时发现。内核 `deserialize_zil_from_block` 先做**块级 data_crc 检查**（覆盖整个 record 区，:444-448），再逐 record 解析（:451-471 try_deserialize_record Err 跳过）。单条 record 损坏必然导致块级 data_crc 不匹配 → 返回空，"损坏 record 跳过"（P0-I-15 契约）在块级 CRC 通过时不可达（record 内部 CRC 是 record 区子集，块级 CRC 通过则内部必然通过）。测试版镜像断言"单条损坏 → 跳过返回其余"与内核真实行为（返回空）冲突。
  - 方案：登记为内核侧语义问题待评估——候选：A. 移除块级 data_crc 检查（恢复 record 级容错，但牺牲块完整性）；B. 保留块级 CRC（当前行为，record 级容错分支为死代码）；C. 双校验共存但调整顺序/语义。zil_replay_test 迁移已按当前内核行为（损坏 → 空）断言。
  - 状态：[X] (2026-09-06 登记，用户决策：记录后跳过) (2026-09-08 处置建议已登记：**方案 B（保留块级 CRC 现状）+ 语义标注**——块级完整性优先于单条容错，ZIL 持久化日志整块损坏应重放失败而非静默跳过（部分恢复可能掩盖数据丢失）；record 级容错分支标注"块级 CRC 下不可达"保留（维持 P0-I-15 契约文档性存在），不删死代码。不建议 A（移除块级 CRC 牺牲完整性换几乎不用的单条容错）与 C（双校验增加复杂度无实际收益）。**结论：保持现状 + 语义标注，无需代码改动**) (2026-09-08 审查复核升级为**长期最优：块级单一校验（ZFS 语义）**——发现三层 CRC 为结构性冗余：record CRC ⊆ data CRC ⊆ block CRC（record 区被子集覆盖），record 级容错分支在数学上不可能生效，非"块级优先取舍"而是"设计前提不成立"；ZIL 事务组语义下 record 级容错为伪需求（静默跳单条制造半持久化错觉）。**长期最优 = 块级单一校验**：①修正 P0-I-15 契约为"损坏块拒绝"语义；②移除冗余 data CRC（被 block CRC 完全覆盖）+ 删除 record 容错死代码分支；③块级拒绝时 klog 记录损坏偏移/期望 vs 实际 CRC（硬件故障可诊断）。**2026-09-08 用户已授权，纳入工程计划 J 为 J-04**) (2026-09-08 用户授权，随工程计划 J-04 委托) (2026-09-08 随 J-04 实施关闭：见 J-04 状态，zil_replay_test 8/8 + host-tests 749 + QEMU 472 全绿)

- **G-10. hvfs 重复 init 重建 objset 使旧数据不可见（B08-14 迁移发现，内核语义）→ 已修复（随 J-03 方案 C）**
  - 描述：2026-09-06 B08-14 迁移 hvfs_persist_test 时发现。内核 `HvfsData::init()` 重复调用时，`setup_zil_datasets → HvObjSet::init` 会**清空 root dataset 的 objset**（[hvfs_data.rs:275](../../../src/kernel/services/fs/hvfs/hvfs_data.rs#L275) `datasets[0].init(0)`），已写文件随后 open 返回 FileNotFound。原测试版 mock 的 `HVFS_DATA` 为 `Mutex<Option<Box>>` 可重置，重新 init 是"干净重置"语义；内核 `OnceCell` 不可重置，重复 init 是"重建 objset 破坏数据"语义。
  - 方案：登记为内核侧语义问题待评估——`HvObjSet::init` 为一次性初始化设计，重复 init 重建是当前行为；若"重复 init 应幂等保留数据"是期望语义，需内核侧评估（如 init 前检查已有数据）。hvfs_persist_test 已按当前行为断言（Phase 3 验证"重复 init 可安全调用 + 旧文件不可读"并注释记录）。
  - 状态：[X] (2026-09-08 随 J-03 方案 C 实施关闭：①init 幂等化（重复 init no-op，数据保留，G-10 灾难路径消除）；②新增 `HvfsData::reset()` 显式重建（供栏栈恢复钩子）；③hvfs_reset 空壳实装为调 reset()；④hvfs_persist_test Phase 3 断言更新为"重复 init 幂等 + 数据保留"。验证：host-tests 749 passed + QEMU kernel_test 472 全绿。明细见 J-03 状态)
