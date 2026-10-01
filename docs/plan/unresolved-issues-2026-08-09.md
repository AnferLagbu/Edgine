# QueenX 未修复问题追踪清单 (2026-08-09)

> **本会话审计产生的"未修复问题"专项追踪文档**.
>
> 区别于 [stage-engineering-master.md](./stage-engineering-master.md)（静态检查工程）, [progress-active-tasks.md](./progress-active-tasks.md)（活跃任务进度）, [future-roadmap.md](./future-roadmap.md)（远期规划）, 本文档**专门追踪"已识别但当前未修复"的问题**, 包括:
> - **运行时已知问题** (网卡/GICv3 挂起等)
> - **源码未实现** (TODO/FIXME)
> - **跨文档矛盾** (code-review 发现)
> - **构建/工具问题**
> - **本会话刻意维持** (DECISION 登记)
>
> **状态字段约定**:
> - ❌ `[]` 未修复 (当前待办)
> - ⏸️ `[~]` 已识别但刻意维持 (DECISION 登记)
> - 🔄 `[X]` 已修复 (commit hash 已登记)
> - 🚫 `[永久]` 永久搁置 (有意识决策)
>
> **建立日期**: 2026-08-09 (本会话审计产出)
> **审计触发**: 用户问题"未被修复的问题有哪些？我是指如于类似网卡挂起等包括但不限于预存问题的问题"
> **来源 commit**: `ebb985c0` (DECISION-046), `a656c91e` (expect 兜底根治), `4f1a9d3e` (brittle 修复)

---

## 📊 总览 (2026-08-09)

| 类别 | 数量 | 严重度分布 | 状态 |
|---|---|---|---|
| 运行时已知问题 | 3 | P0×1 + P1×2 | 🔄 RT-001 已修复 (`[X]`); RT-002 压测不可复现 (保留); RT-003 交付物就绪 (真机执行待硬件) |
| 源码未实现 (TODO) | ~43 | P1×16 + P2×22 + P3×5 | ❌ 未修复 |
| 跨文档矛盾 (code-review) | 8 | P1×3 + P2×3 + P3×2 | 🔄 已修复 (2026-09-26 复验; 归档快照冻结) |
| 远期工程 | 6 | 远期 | ❌ 未启动 |
| 本会话刻意维持 | 3 | 决策登记 | ⏸️ DECISION |
| 构建/工具问题 | 3 | 工具 | ❌ 未提交 |
| 审计基线待清零 (2026-08-23) | 2 | F2×12 + F7×67 | 🔄 已处理 (2026-08-30) |
| 分册 3 归档遗留 (2026-08-23) | 3 | 遗留×3 | ❌ 待下轮 |
| lint 副作用 (已修复) | 2 | — | 🔄 已修复 |
| 迁移中子系统状态 (2026-08-31) | 8 | MIG×8 | ⚠️ 迁移中 (有意识中间态) |
| 分册 6 调研预存问题 (2026-08-31) | 3 | B06-PRE×3 (1 安全) | ❌ 用户裁决登记待后续 |
| socket_max_sockets flaky 排查 (2026-08-31) | 1 | B06-PRE-004 | ✅ 已修复 (非内核问题) |
| **总计** | **~82 项 (2026-08-09 原登记)** | — | **复验订正: 真正仍开放 ≈ 24 项 + 3 项刻意维持** |

> **【本轮复验订正】** 按当前源码状态逐类复核 (原登记数保留上表, 不涂改):
> - **源码未实现 (TODO)**: 原 ~43 项 → 实测**仅剩 1 项**真实 TODO ([framework/net/init.rs:601](file:///home/anfer/Code/QueenX/src/kernel/framework/net/init.rs#L601), 即原 ISSUE-SRC-008, 行号由 `:822` 漂移); 另 1 项 ISSUE-SRC-028 所在文件 `services/fs/vfs/api.rs` 已不存在 (拆分迁至 `handle.rs`). 其余 41 项全部消除 (源码仅残留 `TRACK-xxxxxx 消除` 说明注释).
> - **【本轮修复（修遗留工程）】** 上条所述**唯一**真实 TODO **ISSUE-SRC-008 已修复** → 第 2 类源码未实现项**全部清零** (qx 自有代码 0 活跃 TODO).
> - **跨文档矛盾**: 8 项实现侧均已落地 (见 §3), 归档快照按 §6 冻结 → 仍开放 **0**.
> - **审计基线**: 实测 [audit_services_boundary.py](file:///home/anfer/Code/QueenX/scripts/audit_services_boundary.py) **EXIT=0 / 0 违规**、`audit_comment_language.py` **724 文件 0 违规** → 仍开放 **0**.
> - **运行时已知问题**: 3 项仍成立 (RT-001 见 [qemu_boot_test.sh:168](file:///home/anfer/Code/QueenX/scripts/qemu_boot_test.sh#L168) 注释; RT-002 未修复但 `.gdb_debug_gic` 证据失效; RT-003 未运行).
> - **构建/工具**: TOOL-001 已修复; TOOL-002 QEMU 侧陈旧检测已实装 ([check_kernel_fresh](file:///home/anfer/Code/QueenX/scripts/qemu_boot_test.sh#L133)); TOOL-003 的 E0152 前提已根治 (2026-09-14 build-std 显式化) → 待按新状态重评.
> - **分册 3 归档遗留**: 3 项均仍成立 (COW TOCTOU 见 [cow.rs:464](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/cow.rs#L464); pmm/swap 与多核 tick 的 host-tests 缺口均无对应用例).
> - **迁移中子系统**: MIG-001/002/003/004/006/007/008 仍成立; MIG-005 文件名漂移 (framework 侧实为 `ata.rs`/`nvme.rs`/`ahci.rs`/`ata_block.rs`, **无** `nvme_block.rs`/`ahci_block.rs`).
> - **分册 6 预存问题**: B06-PRE-001 已失效 (tmpfs 改用 `nodes.len()` + `VfsFileType::Dir.as_u8()`); B06-PRE-002 仍成立 (安全缺陷); B06-PRE-003 行号漂移.
> - **远期工程**: FUT-001~005 未启动; FUT-006 (WASM WASI) 已完成.
> - **刻意维持**: DEC-046 计数 1354/166 → **1406/173**; DEC-005 计数 345 → **492** (逐项见各节).

---

## 🔵 第 0 类：审计基线待清零（2026-08-23 追加，无分册负责）

> 两类审计基线违规**无任何分册（03-09）明确负责修复**，登记防止委派遗漏。
> 分册 01 声称"12 处 HIGH 后续分册 02-07 迁移范围"与"68 处行尾英文注释后续 commit 手工翻译"均未落实为具体条目。

### BASELINE-F2-012: audit_services_boundary 12 处 HIGH（services 访问 framework 内部）

| 字段 | 数据 |
|---|---|
| **规则** | F2（services 禁止访问 framework 内部模块），META-P0-01 识别 |
| **数量** | 12 处 HIGH（黑名单补全后识别，commit 4ba454ab） |
| **文件** | `services/debug/ebpf_verifier.rs`、`debug/mod.rs`、`io/iouring.rs`、`ipc/msgq.rs`、`mm/madvise_mlock.rs`、`proc/coredump.rs`、`proc/memfd.rs`、`proc/pidfd.rs`、`syscall/dispatch.rs`、`syscall/mod.rs` |
| **分册覆盖核查（2026-08-23）** | 分册 03-09 无条目明确负责修复这些文件的 F2 边界违规（各分册条目只修功能/逻辑，如 B05-32 pidfd、B07-18 ebpf）；分册 09 B09-11/12/13 的"F2 治理"仅覆盖 **framework→services 反向依赖（D8）**，方向相反不覆盖本项 |
| **处理（2026-08-30）** | 🔄 已处理——5 处代理层自拦截误报经 `PROXY_ALLOWANCE` 豁免（[audit_services_boundary.py](file:///home/anfer/Code/QueenX/scripts/audit_services_boundary.py) 白名单：debug/mod.rs、ebpf_verifier.rs、ipc/msgq.rs、proc/coredump.rs）；实测 `audit_services_boundary.py` 当前 **0 违规**（黑名单补全后剩余 HIGH 均已合规或经豁免），见分册 10 B10-06 |
| **【本轮复验订正】** | `PROXY_ALLOWANCE` 实测为 **8 条**（原记"5 处"为 2026-08-30 首次登记数，后经 DECISION-J 第十七批等扩容：debug/mod.rs、ebpf_verifier.rs×2、ipc/msgq.rs、proc/coredump.rs、sync/types.rs、barrier/reset_config.rs、syscall/types.rs）；本轮实测 `python3 scripts/audit_services_boundary.py` **EXIT=0 / 0 违规**（另 2 项 MEDIUM `UNLISTED_INTER_MODULE_DEP` 非边界违规） |
| **来源** | archive/audit-fix-01 L227 + archive/audit-fix-02 L339/394 |

### BASELINE-F7-067: audit_comment_language 67 处违规（F7 中文注释强制）

| 字段 | 数据 |
|---|---|
| **规则** | F7（中文注释强制），70 → 67（2026-08-21 诊断删除 -2，2026-08-22 F7 修复 -1） |
| **数量** | 67 处，涉及 34 个文件 |
| **分布** | framework 全树英文注释（acpi/uart/gic/mmu/edid 等，见 `audit_comment_language.py` 输出） |
| **分册覆盖核查（2026-08-23）** | 分册 03-09 无英文注释翻译条目；分册 01 声称"后续 commit 手工翻译"未落实；分册 09 仅覆盖 F1/F9/F2/D8 死代码，无 F7 条目 |
| **处理（2026-08-30）** | 🔄 已修复——按用户授权逐处中文化（34 文件，技术术语保留英文 + 中文说明），实测 `audit_comment_language.py` **0 违规**（"扫描 735 个 .rs 文件, 0 违规"），见分册 10 B10-03 |
| **【本轮复验订正】** | 实测输出为 "扫描 **724** 个 .rs 文件, 0 违规"（原记"735"随文件增删漂移）；结论（0 违规）不变 |
| **来源** | archive/audit-fix-01 L221 + archive/audit-fix-02 L342 |

---

## 🔵 第 0A 类：审计脚本门禁修复登记（2026-08-25 追加）

> 2026-08-25 修复 audit_invariants.py 误报与 CI fail-open 门禁失效，登记修复依据与验证结果。

### AUDIT-TOOL-001: audit_invariants.py I2 误报 127 处（已修复）

| 字段 | 数据 |
|---|---|
| **根因** | B01-21（commit c00e9e55）将 I2 检测范围从 services 改为 framework——**方向性错误**。I2 不变式（AGENTS.md §4.2）语义为"内核内存不可被 **services** 非法访问"，守护对象是 services；framework 是合法 TCB，裸指针解引用是其职责（127 处全部位于 unsafe 块内），其安全由 F4（audit_safety_coverage.py SAFETY 100%）守护，不属 I2 范畴 |
| **修复** | B01-25：I2 恢复扫描 services（实测 0 违规），删除 `_scan_framework` 与 `FRAMEWORK` 变量 |
| **验证** | 脚本 6 项不变式全 PASS + 退出码 0；构造临时 services 违规探针验证 I2 仍能捕获（检测能力未退化） |
| **状态** | [X]（2026-08-25，commit 见 git log） |

### AUDIT-TOOL-002: ci-lint.yml audit-invariants job fail-open（已修复）

| 字段 | 数据 |
|---|---|
| **根因** | `INVARIANTS_OUT=$(python3 scripts/audit_invariants.py 2>&1 || true)` —— `|| true` 使命令替换退出码恒为 0，后续 `${PIPESTATUS[0]}` 恒为 0，CI 永不因违规失败（fail-open） |
| **修复** | 移除 `|| true`，改用 `set +e` / 显式捕获 `$?` 判断 |
| **验证** | bash 模拟非零退出码时门禁正确捕获（RC=7 → FAIL） |
| **状态** | [X]（2026-08-25） |

### AUDIT-TOOL-003: ci-lint.yml audit-coupling job 同模式 fail-open（已修复）

| 字段 | 数据 |
|---|---|
| **根因** | 与 AUDIT-TOOL-002 同模式：`COUPLING_OUT=$(python3 scripts/audit_coupling.py 2>&1 || true)`，`|| true` 掩盖退出码 |
| **修复** | 与 AUDIT-TOOL-002 同样修复（移除 `|| true` + 显式捕获 `$?`），2026-08-25 与 AUDIT-TOOL-002 一并提交 |
| **验证** | audit_coupling.py 当前返回 0（通过），修复后退出码可真实传递至 CI 判断 |
| **状态** | [X]（2026-08-25） |

---

## 🔵 第 0B 类：分册 3 归档遗留（2026-08-23 归档登记）

> 分册 3（[archive/audit-fix-03-framework-mm-sync.md](./archive/audit-fix-03-framework-mm-sync.md)）归档时核查出 3 项"方案承诺未完全落地"的遗留，登记防止归档后丢失追踪。分册 3 主修复（B03-01~28）均已实装并通过验证门槛（双架构编译 0w0e + host-tests + 审计无回归）。

### B03-LEGACY-001: COW TOCTOU（cow_handle_fault 锁内判定+锁外执行）

| 字段 | 数据 |
|---|---|
| **来源** | archive/audit-fix-03 B03-06（方案明确"TOCTOU 修复留作下轮独立 PR（与 fork-exit host-tests 一并）"） |
| **位置** | `framework/mm/cow.rs` `cow_handle_fault` |
| **问题** | 锁内判定 COW 页 + 锁外执行映射，存在 TOCTOU 窗口（多核并发下共享页状态可能变化） |
| **建议方案** | 判定与映射放入同一临界区（持 VMM_LOCK 下操作）；补 fork-exit 共享页引用计数 host-tests |

### B03-LEGACY-002: pmm/swap host-tests 缺口

| 字段 | 数据 |
|---|---|
| **来源** | archive/audit-fix-03 B03-04 + DECISION-050（"host-tests 留作下批补"、"pmm::find_contig_range host-tests + 回滚路径测试"） |
| **位置** | `framework/mm/pmm.rs::find_contig_range`/`reserve_range`、`swap.rs::deinit` |
| **现状** | 代码实装完成（swap init 改 find_contig_range + reserve_range，deinit 走 unreserve_range 回滚），双架构编译 + 全量验证通过；但 host-tests 无对应用例 |
| **建议方案** | 补 find_contig_range 连续范围扫描、reserve_range 重叠拒绝、deinit unreserve 回滚路径测试 |

### B03-LEGACY-003: 多核 tick 计数器内存序测试

| 字段 | 数据 |
|---|---|
| **来源** | archive/audit-fix-03 B03-12（方案"补多核 tick 测试"） |
| **位置** | `framework/timer/tick.rs` `TICK_COUNT`（fetch_add 已从 Relaxed 改 AcqRel） |
| **现状** | 代码 Ordering 修复完成（516a64d6），host-tests 无多核用例（host 单核难以覆盖） |
| **建议方案** | QEMU SMP kernel_test 补多核 tick 可见性测试，或按 ROE（Return-On-Effort）说明豁免 |

> **【本轮复验订正】** 3 项遗留均仍成立:
> - B03-LEGACY-001: [cow.rs:464](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/cow.rs#L464) 的 `frame_ref_count` 判定与 [cow.rs:470](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/cow.rs#L470) 的 `map_page_in_table` 映射仍不在同一临界区，TOCTOU 窗口存在.
> - B03-LEGACY-002: host-tests 全树无 `find_contig_range`/`reserve_range`/`unreserve_range` 用例（grep 0 命中），缺口成立.
> - B03-LEGACY-003: [tick.rs:165](file:///home/anfer/Code/QueenX/src/kernel/framework/timer/tick.rs#L165) 已为 `AcqRel`，但无多核 host 用例（host 单核），缺口成立.
>
> **【本轮修复（修遗留工程）】** 3 项遗留已全部结案:
> - **B03-LEGACY-001**: [cow.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/cow.rs) 将 `cow_handle_fault` 拆为"取锁外壳 + `cow_handle_fault_locked`（要求调用方已持 `VMM_LOCK`）"两层; 帧计数判定 (`frame_ref_count`) 与其后的映射 (`map_page_in_table_locked`)、帧归还 (`release_frame_locked`) 现**在同一临界区内**完成, TOCTOU 窗口消除. 配套在 [vmm_x86_64.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/vmm_x86_64.rs) / [vmm_aarch64.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/mm/vmm_aarch64.rs) 增设 `*_locked` 不自取锁变体（非重入 `VMM_LOCK` 保护）; 已持锁的调用点直接走 `_locked`.
> - **B03-LEGACY-002**: [pmm_buddy_host_test.rs](file:///home/anfer/Code/QueenX/host-tests/tests/pmm_buddy_host_test.rs) 新增 3 个用例 — `pmm_find_contig_range_scan_and_reject`（连续扫描 + 契约拒绝: size==0 / 非页对齐 / 超总空闲）、`pmm_reserve_range_rejects_overlap_and_misuse`（重叠/已分配/内核区/越界拒绝）、`pmm_unreserve_range_rolls_back`（预留→回滚→重新命中空闲池 + 重复回滚拒绝），覆盖 swap `init`/`deinit` 依赖路径.
> - **B03-LEGACY-003**: 新增 [timer_tick_concurrency_test.rs](file:///home/anfer/Code/QueenX/host-tests/tests/timer_tick_concurrency_test.rs) — host 真多核上直接驱动内核真实 `on_timer_interrupt()` 路径, 4 写线程并发递增 + 2 读线程并发校验, 断言读-读单调不回退与终值精确（无丢失更新）; 文件头以「判别力边界」显式声明: 本用例为并发健壮性 + 回归防护, **不声称**证明 `AcqRel` 相对 `Relaxed` 的必要性（x86_64 TSO 下 `lock` RMW 即全屏障, 二者运行时不可区分）, 弱序语义判别待 aarch64 SMP 落地后补.

---

## 🔴 第 1 类：运行时已知问题 (3 项)

### ISSUE-RT-001: x86_64 e1000 + smoltcp 初始化挂起

| 字段 | 数据 |
|---|---|
| **严重度** | P1 (阻塞 x86_64 进入 Ring 3) |
| **状态** | 🔄 已修复 (`[X]`) |
| **类型** | 运行时挂起 (网络栈初始化) |
| **现象** | QEMU 默认 e1000 NIC 触发 smoltcp 栈初始化挂起 |
| **触发条件** | 此前 x86_64 启动时不加 `-nic none` |
| **影响** | x86_64 仅能到 Network Subsystem Init, **无法进入 Ring 3** |
| **当前应对** | 启动脚本恢复 QEMU 默认 e1000; 断言驱动初始化里程碑 (`e1000: 初始化完成`) + 完整进 Ring 3 |
| **调试入口** | [e1000_io.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/driver/net/e1000_io.rs) (寄存器位域常量), [e1000.rs](file:///home/anfer/Code/QueenX/src/kernel/services/driver/net/e1000.rs) (驱动实装) |
| **来源** | `scripts/qemu_boot_test.sh` 注释: "x86_64 走到 e1000 NIC 检测后因 smoltcp 初始化挂起, 已记录. e1000 调试见 driver/net/e1000.rs" |
| **关联** | ISSUE-SRC-002 (skb 投递到 smoltcp 未实现) |
| **建议方案** | (1) 隔离 NIC 后单独调试 smoltcp 初始化路径; (2) 检查 e1000 probe 与 smoltcp iface 创建的 race condition; (3) 逐步加 NIC 看具体哪个 packet 触发挂起 |
| **工作量** | 估计 1-2 周 |
| **【本轮结案】** | 根因为 [e1000_io.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/driver/net/e1000_io.rs) 三处寄存器位域常量写错: `E1000_CTRL_RST` 误用 bit31 (该位实为 `E1000_CTRL_PHY_RST` PHY 复位, 82540EM 不因它触发全局复位 -> 复位轮询超时) -> 修为 bit26 (`0x0400_0000`, Global Reset); `E1000_CTRL_FRCDPX` 误用 bit14 -> 修为 bit12 (Force Duplex); `E1000_RCTL_BSIZE_2048` 误用 bit25 (该位实为 `RCTL_BSEX` 缓冲区尺寸扩展位, 非尺寸编码) -> 修为 `0x0` (BSEX=0, BSIZE=00b -> 2048B). 修复后 [qemu_boot_test.sh](file:///home/anfer/Code/QueenX/scripts/qemu_boot_test.sh) 恢复 QEMU 默认 e1000 并新增 `e1000: 初始化完成` 回归断言; 静态契约回归用例 [driver_e1000_ctrl_bits_test.rs](file:///home/anfer/Code/QueenX/host-tests/tests/driver_e1000_ctrl_bits_test.rs) 固化上述三处位域值, 防止重现. 本轮 §2.3 六门槛复验: x86_64 侧 `FAIL_OK=0 ./scripts/qemu_boot_test.sh all` 通过 (VFS ready / `e1000: 初始化完成` / Entering Ring 3 / KPTI). |

### ISSUE-RT-002: aarch64 GICv3 挂起 ⚠️ **用户当前调试**

| 字段 | 数据 |
|---|---|
| **严重度** | P0 (用户当前调试中) |
| **状态** | ❌ 未修复 (`[]`) |
| **类型** | 运行时挂起 (中断控制器) |
| **现象** | GICv3 初始化或中断处理挂起 |
| **影响** | aarch64 启动可能挂起 (本次 QEMU 启动虽到 EL0, 但偶发 GICv3 相关问题) |
| **调试方式** | GDB 调试 (`.gdb_debug_gic` 文件) |
| **源码位置** | `src/kernel/framework/arch/aarch64/gic.rs` (GICv3 初始化), `src/kernel/framework/arch/aarch64/barrier/mod.rs` (SGI 7 使能) |
| **用户当前活动** | 用户 IDE 打开 `.gdb_debug_gic` 文件表明**正在 GDB 调试 GICv3 挂起** |
| **【本轮复验订正】** | 现象与源码位置仍成立；但 `.gdb_debug_gic` 文件**当前不存在**于工作区（glob 0 命中），"用户 IDE 打开 `.gdb_debug_gic`"的活动证据已失效——「用户当前调试」应以用户实际状态为准，本台账不据过时证据断言 |
| **【本轮压测复现】** | 本轮对 aarch64 启动连续压测 62 次（`./scripts/qemu_boot_test.sh aarch64`, QEMU TCG, 断言到 EL0）, **62/62 全部通过, 无一次触发 GICv3 挂起**. 结合原始现象为"偶发", 判断为 QEMU TCG 时序相关的偶发现象, 当前环境**不可稳定复现**. 挂起条目**不闭合**, 保留待 aarch64 SMP / 真机 / 更多时序场景复验. |
| **建议方案** | (1) GDB `break gic_init` 单步跟踪; (2) 检查 GICR_SGI_BASE 寄存器访问; (3) 检查 SGI 7 触发时 Redistributor 状态 |
| **工作量** | 估计 3-5 天 |

### ISSUE-RT-003: 真实硬件启动验证未运行

| 字段 | 数据 |
|---|---|
| **严重度** | P1 |
| **状态** | 🟡 交付物就绪 (`[X]` 工具/文档侧); 真机执行待硬件 |
| **类型** | 运行时验证缺失 |
| **现象** | 仅 QEMU 模拟启动, 真实硬件未验证 |
| **影响** | 可能有 QEMU 兼容但真实硬件失败的问题 |
| **来源** | stage-engineering-master.md:301 `[ ] QEMU 实际验证` |
| **建议方案** | (1) 选定 x86_64 + aarch64 各一款硬件 (如 Intel NUC + Raspberry Pi); (2) 制作可启动介质; (3) 串口观察启动日志 |
| **工作量** | 估计 2-4 周 (含硬件采购) |
| **【本轮交付（三件套）】** | 本轮交付真机验证的**工具 + 指南 + 登记**三件套, 使真机验证可在拿到硬件后一次完成: ① 介质制作脚本 [make_boot_medium.sh](file:///home/anfer/Code/QueenX/scripts/make_boot_medium.sh)（统一入口, `x86_64` 产 GRUB2 混合 ISO / `aarch64` 产整盘 FAT32 + U-Boot distro boot 介质, 含写盘四重护栏与 fail-closed）; ② 真机验证权威指南 [guide-hardware-boot.md](file:///home/anfer/Code/QueenX/docs/explain/guide-hardware-boot.md)（介质制作 / 串口 checklist / aarch64 SoC 契约与边界 / 故障排查）; ③ 本台账登记. 真机**执行**本身依赖用户提供硬件, 故状态为"交付物就绪, 执行待硬件". |

---

## 🟠 第 2 类：源码未实现 (~43 个 TODO)

> **【本轮复验订正】** 逐条回源码复验（命令：`grep -rn "TODO\|FIXME\|XXX" src/kernel --include="*.rs" | grep -v "src/kernel/services/net/smoltcp/"`，含 smoltcp 全树共 47 行，**排除 smoltcp 后仅 2 行**）:
> - **41 项已消除**——源码仅残留 `TRACK-xxxxxx 消除/解决` 说明注释（`grep TRACK-[0-9A-Fa-f]{6}` = 18 行，全为消除说明，无活跃 `TODO(TRACK-*)`）.
> - **1 项仍存在**: ISSUE-SRC-008（`framework/net/init.rs` 的 skb 投递 TODO），行号由原记 `:822` 漂移为 **[:601](file:///home/anfer/Code/QueenX/src/kernel/framework/net/init.rs#L601)**；它是当前 qx 自有代码中**唯一**真实 TODO.
> - **1 项文件已不存在**: ISSUE-SRC-028 所在 `services/fs/vfs/api.rs` 已不存在（拆分迁至 `handle.rs`）.
> - **说明**: `framework/config/mod.rs:75` 的 `XXX` 为路径占位符（`use crate::framework::config::XXX`），**非 TODO**，不计入.

> **【本轮修复（修遗留工程）】** ISSUE-SRC-008 已修复，第 2 类源码未实现项**全部清零**:
> - 重写 [init.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/net/init.rs) `net_rx_softirq_handler`（原空 TODO）→ 调用 `poll_network()`；触发侧新增 framework 机制层 [net/init/irq.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/net/init/irq.rs)（ISR → `raise_softirq(NetRx)` → 底半部收包），经 `framework::net` 顶层 re-export 暴露 0 unsafe 注册入口 `net_register_msix_isr` / `net_register_intx_isr`.
> - services 侧接线: e1000 覆写 `handle_irq`（ICR ack）+ 探测时 `msix_enable(dev,1)` 后注册 MSI-X ISR；virtio-net 覆写 `handle_irq`（InterruptACK）+ aarch64 分支注册 GIC SPI ISR.
> - 复核结论: 第 2 类原 ~43 项 → 41 项历史消除 + ISSUE-SRC-008 本轮修复 + ISSUE-SRC-028 文件不存在 → **0 活跃 TODO**（保留 `framework/config/mod.rs:75` 占位符说明，非 TODO）.

### 2.1 P1 严重 — 阻塞核心功能

| # | 文件:行 | 描述 | 阻塞 |
|---|---|---|---|
| ISSUE-SRC-001 | `framework/arch/shadow_stack.rs:304` | TODO(TRACK-4C9A12): 使用 PMM 分配实际物理页 | shadow stack 实现 |
| ISSUE-SRC-002 | `framework/credo/secure_boot.rs:198` | TODO(TRACK-7A8BAB): 替换为真正的 Ed25519 验证 | 安全启动 |
| ISSUE-SRC-003 | `framework/idt/safety.rs:25` | TODO(TRACK-2B3C56): 完整实现 CPUID 解析 | CPU 特性检测 |
| ISSUE-SRC-004 | `framework/driver/power.rs:148` | TODO(TRACK-6F7A9A): 实现真正的 S3 挂起 | 电源管理 |
| ISSUE-SRC-005 | `framework/driver/uefi.rs:264` | TODO(TRACK-4D5E78): 实际解析 EFI_SYSTEM_TABLE | UEFI 启动 |
| ISSUE-SRC-006 | `framework/driver/uefi.rs:435` | TODO(TRACK-5E6F89): 调用 EFI_RUNTIME_SERVICES.SetTime | UEFI 时间服务 |
| ISSUE-SRC-007 | `framework/driver/usb/xhci.rs:670` | TODO: 实现 Event Ring 处理 | xHCI USB 驱动 |
| ISSUE-SRC-008 | `framework/net/init.rs:822` | TODO: 待 NAPI/中断驱动模式启用后, 此处实现 skb 投递到 smoltcp | **关联 ISSUE-RT-001** |
| ↳ **【本轮修复（修遗留工程）】** | `framework/net/init.rs:601` | **已修复**: `net_rx_softirq_handler` → `poll_network()`；触发侧 framework 新增 ISR→softirq 机制 + services 侧 e1000/virtio-net 接线 ISR | — |
| ISSUE-SRC-009 | `framework/timer/tickless.rs:237` | TODO(TRACK-3C4D67): 集成 hrtimer 获取最近到期时间 | tickless 模式 |
| ISSUE-SRC-010 | `services/io/iouring.rs:314` | TODO(TRACK-8B9CBC): 集成 VFS fd 表 | io_uring VFS 集成 |
| ISSUE-SRC-011 | `services/io/iouring.rs:319` | TODO(TRACK-9CADCD): 实现网络异步操作 | io_uring 网络 |
| ISSUE-SRC-012 | `services/io/iouring.rs:323` | TODO(TRACK-ADBECDE): 实现超时等待 | io_uring 超时 |
| ISSUE-SRC-013 | `services/io/iouring.rs:470` | TODO(TRACK-BECFEF): 实现缓冲区注册 / 文件注册 | io_uring 完整功能 |
| ISSUE-SRC-014 | `services/ipc/sem.rs:102` | TODO(TRACK-21BAF1): 阻塞当前线程到 wait 队列 | semaphore 阻塞语义 |
| ISSUE-SRC-015 | `services/ipc/signal.rs:84` | TODO(TRACK-48CC21): 将处理函数注册到 SignalPending | 信号注册 |
| ISSUE-SRC-016 | `services/ipc/signal.rs:135` | TODO(TRACK-3A9016): 实现完整的信号分发逻辑 | 信号分发 |

### 2.2 P2 中等 — 部分功能缺失

| # | 文件:行 | 描述 |
|---|---|---|
| ISSUE-SRC-017 | `framework/dma/engine.rs:426` | TODO(TRACK-1F2A45): 由 DmaStream 的 coherent 属性决定 |
| ISSUE-SRC-018 | `framework/arch/shadow_stack.rs:540` | TODO(TRACK-6E7C34): 使用 #GP 异常处理来安全检测 |
| ISSUE-SRC-019 | `services/driver/power.rs:355` | TODO(TRACK-7A3B01): 实际写 MSR/寄存器调整频率和电压 |
| ISSUE-SRC-020 | `services/ipc/scheduler_integration.rs:103` | TODO(TRACK-8C5FFB): 实现基于定时器的超时等待 |
| ISSUE-SRC-021 | `services/ipc/signal.rs:106` | TODO(TRACK-614BD5): blocked 位图设置 |
| ISSUE-SRC-022 | `services/ipc/signal.rs:126` | TODO(TRACK-F806F4): blocked 位图清除 |
| ISSUE-SRC-023 | `services/proc/oomd.rs:94` | TODO: 实际发送 SIGKILL 到最大 RSS 进程 |
| ISSUE-SRC-024 | `services/proc/memfd.rs:58` | TODO: 使用 per-process fd 表 |
| ISSUE-SRC-025 | `services/proc/memfd.rs:75` | TODO: 设置 fd 的 CLOEXEC 标记 |
| ISSUE-SRC-026 | `services/proc/pidfd.rs:61` | TODO: 需要 Task 4 (OpenFile 系统) 完成后实现 |
| ISSUE-SRC-027 | `services/io/iouring.rs:319` | (重复 ISSUE-SRC-011) |
| ISSUE-SRC-028 | `services/fs/vfs/api.rs:218` | TODO: 使用 per-process fd 表 |
| ISSUE-SRC-029 | `services/net/smoltcp_impl.rs` (多个) | smoltcp 集成相关 |
| ISSUE-SRC-030 | `services/credo/sessions.rs` (多个) | 凭据会话管理 |
| ISSUE-SRC-031 | `services/credo/grants.rs` (多个) | 凭据授权 |
| ISSUE-SRC-032 | `services/credo/policy.rs` (多个) | 凭据策略 |
| ISSUE-SRC-033 | `services/barrier/attribution.rs` (多个) | 故障归属 |
| ISSUE-SRC-034 | `services/barrier/recovery_policy.rs` (多个) | 故障恢复 |
| ISSUE-SRC-035 | `services/barrier/cascade.rs` (多个) | 故障级联 |
| ISSUE-SRC-036 | `services/debug/ebpf_verifier.rs` (多个) | eBPF 验证器 |
| ISSUE-SRC-037 | `services/driver/display/dp.rs` (多个) | DisplayPort 协议 |
| ISSUE-SRC-038 | `services/driver/storage/*` (多个) | 存储驱动 |

### 2.3 P3 轻微 — 时间戳更新

| # | 文件:行 | 描述 |
|---|---|---|
| ISSUE-SRC-039 | `services/fs/nestfs/nestfs_inode.rs:92` | TODO: 未来可接入 NestFS 时间戳更新 |
| ISSUE-SRC-040 | `services/fs/ext2/mount.rs:102` | TODO: 未来可接入 ext2 inode 时间戳更新 |
| ISSUE-SRC-041 | `services/fs/exfat/mount.rs:79` | TODO: 未来可接入 exFAT 目录项时间戳更新 |
| ISSUE-SRC-042 | `services/fs/overlayfs.rs:205` | TODO: 未来可实现 copy-up + 时间戳更新 |
| ISSUE-SRC-043 | `services/fs/devfs.rs` | (类似时间戳项) |

> **说明**: P2/P3 的具体位置可通过 `grep -rn "TODO\|FIXME\|XXX" src/kernel --include="*.rs" | grep -v "src/kernel/services/net/smoltcp/"` 重新生成. qx 自有代码中**约 43 个 TODO** (排除 smoltcp vendored 527 个).
>
> **【本轮复验订正】** 上表为 **2026-08-09 原登记快照**（保留不涂改）；按当前源码状态复验，上表中除 ISSUE-SRC-008 外的条目均已消除，ISSUE-SRC-028 路径已不存在——详见本节开头【本轮复验订正】. 重新生成命令实测（排除 smoltcp）仅 2 行（1 占位符 + 1 真 TODO），非"约 43 个".
>
> **【本轮修复（修遗留工程）】** 上述残留的 1 项真 TODO ISSUE-SRC-008 本轮已修复；复验上表条目**全部结案**，qx 自有代码 0 活跃 TODO（详见本节开头【本轮修复（修遗留工程）】）.

---

## 🟡 第 3 类：跨文档矛盾 (8 项)

### 3.1 P1 跨文档战略矛盾 (3 项) — ✅ 已修复 (2026-09-26 源码复验)

> **来源**: `docs/plan/archive/code-review-findings-2026-08-01.md`. 用户 2026-08-01 授权**仅记录不修复**, 状态 `[]`.
>
> **2026-09-26 复验结论**: 3 项 P1 的实现侧均已由 DECISION-037/038/039 于 2026-08-03 落地. 归档快照 `archive/code-review-findings-2026-08-01.md` 的 `[]` 按 AGENTS.md §6「archive/ 为历史快照不再修改」冻结, **不属于待修漂移**.

#### REVIEW-FINDING-024: CHANGELOG.md 缺失但 README/AGENTS 多处引用

| 字段 | 数据 |
|---|---|
| **严重度** | P1 (违反 AGENTS.md 硬规则) |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | README.md:11/163/210 + AGENTS.md:48/363 引用不存在的 `docs/CHANGELOG.md` |
| **方案** | (b) 删除全部引用 (采纳). git commit 本身即变更日志 |
| **落地** | DECISION-038 (2026-08-03). 2026-09-26 复验: README.md / AGENTS.md / scripts / ci 均已无引用; `host-tests/README.md` 实际残留 3 处 (原记载"2 处"不准), 本轮已归零 |
| **【本轮复验订正】** | 断链 (指向不存在的 `docs/CHANGELOG.md`) 确已清零；但 `host-tests/README.md:309/317` 与 `scripts/scan_antx_residue.py:116/118` 仍含 `CHANGELOG.md` **合法字样**（描述性文字/扫描规则，非文件引用），故"全仓 0 引用"表述不准，应理解为"0 处失效链接" |
| **归档快照** | `archive/code-review-findings-2026-08-01.md` 保持 `[]` (AGENTS.md §6 冻结) |

#### REVIEW-FINDING-025: syscall 编号空间立场两份权威文档互相矛盾

| 字段 | 数据 |
|---|---|
| **严重度** | P1 |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | `framework/syscall/mod.rs:24-35` 称 "0-299 保留给未来 linuxulator" vs `ref-naming.md §三` 称 "直接使用 Linux syscall 编号" |
| **落地** | DECISION-037 (2026-08-03). 2026-09-26 复验: `framework/syscall/mod.rs:22-30` 注释已统一为 "0-299 直接 Linux ABI; 500+ 为 QX_* 自由扩展"; `vision-hope.md` 已整篇重写, 原 linuxulator"风险 2"节不复存在 |
| **归档快照** | `archive/code-review-findings-2026-08-01.md` 保持 `[]` (AGENTS.md §6 冻结) |

#### REVIEW-FINDING-026: framework 反向依赖 services 类型 (userctx.rs re-export)

| 字段 | 数据 |
|---|---|
| **严重度** | P1 (违反 framekernel 单向数据流) |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | `framework/userctx.rs:6-9` re-export services 类型, `framework/usermode.rs:38/58` 直接读取其字段 |
| **落地** | DECISION-039 (2026-08-03, A 方案). 2026-09-26 复验: `framework/userctx.rs:28/55` 为两处 `#[repr(C)] UserContext` (x86_64/aarch64 cfg 分支) 正规定义; `services/userctx.rs:11` 反向 `pub use crate::framework::userctx::*;`. 单向数据流已恢复 |
| **归档快照** | `archive/code-review-findings-2026-08-01.md` 保持 `[]` (AGENTS.md §6 冻结) |

### 3.2 P2 文档失实 (3 项) — ✅ 已修复 (2026-09-26 源码复验)

#### REVIEW-FINDING-027: framework 顶层文档声明 "~3000+ LoC" 严重失实

| 字段 | 数据 |
|---|---|
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | `framework/mod.rs:10` 声明 `~3000+ LoC`, 实际 ~10 万行 |
| **落地** | progress B1 (2026-08-04). 复验: `framework/mod.rs:10` 现为 `//! framework/ (TCB, unsafe 允许)`, 无 LoC 数字 |

#### REVIEW-FINDING-028: services/net + services/fs 头注释过期

| 字段 | 数据 |
|---|---|
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | net/mod.rs:4-19 / fs/mod.rs:4-19 头注释过期 (v2.7/v2.5, 2026-06-04) |
| **落地** | progress B2 (2026-08-04). 复验: net/fs/proc 三文件头注释已替换为当前真实状态描述 + 指向 [progress-active-tasks.md](./progress-active-tasks.md), 仅保留一行"历史: 2026-06 之前 vN 状态评估已过时" |

#### REVIEW-FINDING-029: README.md remote 命名与 kernel-roadmap 链接过期

| 字段 | 数据 |
|---|---|
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | README.md:21 `git remote rename origin Gitee` 矛盾 + README.md:71 失效链接 `kernel-roadmap.md` |
| **落地** | progress B3 (2026-08-04); README.md 后续已整篇重写 (无 `git remote` 指令、无 `kernel-roadmap.md` 链接), 原 :21/:71 不复存在 |
| **【本轮复验订正】** | README.md 实测为 **23 行**（原记"18 行"漂移）；重写与失效链接清除的结论不变 |

### 3.3 P3 已知未完成 (2 项) — ✅ 已修复 (2026-09-26 源码复验)

#### REVIEW-FINDING-030: framework/sched task 抽象 Phase 1.4.2 未开工

| 字段 | 数据 |
|---|---|
| **严重度** | P3 |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **描述** | `framework/sched/mod.rs:8` 注释: "task 抽象在 Phase 1.4.2 计划中但尚未实现", 阻塞 services/proc 迁移 |
| **落地** | progress 阶段 5 (2026-08-04). 调研发现 `sched_trait.rs` 中 Task 抽象 (struct Task + 10 属性方法 + Scheduler trait + QueenXScheduler 委托) 早已完整实装, 仅 mod.rs 注释过期, 已修复 |

#### REVIEW-FINDING-031: IoMem 边界 expect panic + 固定上限硬编码

| 字段 | 数据 |
|---|---|
| **严重度** | P3 |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | `iomem.rs:194/200/206/212` expect panic + `MAX_MMIO_MAPPINGS = 64` 硬编码 |
| **落地** | progress B6 (2026-08-04). 复验: `iomem.rs` 8 个 read_u*/write_u* 均带 `debug_assert!` 前置; 3 个 TCB 容量常量已集中至 `framework/constants/limits.rs` |

---

## 🔵 第 4 类：远期工程 (6 项)

> **来源**: `docs/plan/future-roadmap.md` + `docs/plan/ipv6-dual-stack.md`.

| # | 编号 | 标题 | 状态 | 工作量 |
|---|---|---|---|---|
| ISSUE-FUT-001 | F1 | mdBook 文档体系 | ❌ 未启动 | ~2 周 |
| ISSUE-FUT-002 | F2 | RISC-V 64 架构支持 | ❌ 未启动 | ~6-8 周 |
| ISSUE-FUT-003 | F3 | TDX 机密计算支持 | ❌ 未启动 | ~4-6 周 |
| ISSUE-FUT-004 | F4 | NFS 网络文件共享 | ❌ 未启动 | ~6-8 周 |
| ISSUE-FUT-005 | F5 Phase 6 | DHCPv6 / SLAAC | ❌ 未启动 (smoltcp 依赖) | 待定 |
| ISSUE-FUT-006 | WASM WASI | 已完成 ✅ | 🔄 已 [X] | — |

---

## 🟣 第 5 类：本会话刻意维持 (3 项) ⏸️

### ISSUE-DEC-046: kernel `#[test]` 迁移 (DECISION-046)

| 字段 | 数据 |
|---|---|
| **状态** | ⏸️ `[~]` DECISION-046 维持原状 |
| **数量** | 1354 个跨 166 文件 (含 527 个 smoltcp vendored) |
| **【本轮复验订正】** | 实测 kernel `#[test]` 现为 **1406 处 / 173 文件**（原记 1354/166 随代码增删漂移）；维持原状的理由（ROI 不匹配）不受影响 |
| **理由** | 范畴属测试架构工程非静态检查工程; ROI 不匹配 (仅 ~70 个纯算法值得迁移) |
| **commit** | `ebb985c0` |
| **未来可选** | 迁移 USB HID/MassStorage/XHCI/Enumerate/Ring 5 文件 ~70 个纯算法测试 (~5-7 天) |

### ISSUE-DEC-041: cast 类 1700+ 处永久保留

| 字段 | 数据 |
|---|---|
| **状态** | 🚫 `[永久]` DECISION-041 |
| **数量** | 1910 处 cast 警告 (真实风险 < 200 处) |
| **理由** | 已知安全 cast (APIC ID < 256, 循环变量 i < 8 等); 全量 try_from 是无价值工作 |
| **处理** | clippy 警告作为"提醒", CI 不阻断 |

### ISSUE-DEC-005: brittle 测试 345 处潜在风险

| 字段 | 数据 |
|---|---|
| **状态** | ⏸️ `[~]` 维持现状 |
| **数量** | `src.find()` 37 处 (高风险) + `src.contains()` 308 处 (中风险) = 345 处 |
| **【本轮复验订正】** | 实测 host-tests 现为 `src.find(` = **52** + `src.contains(` = **440** = **492 处**（原记 37+308=345 随测试增删漂移）；维持现状策略不变 |
| **策略** | 不批量机械改 (易引入 false negative); 仅在**真实测试失败时**针对性改用 `split_whitespace + 关键 token 匹配` |
| **已修复** | 2 处 (`vfs_read/write_uses_inode_trait`, commit `4f1a9d3e`) |
| **真实风险分布** | td19_proc_kernel_error_test 8 处, usermode_ring3_test 6 处, td10/td09 各 4 处, 其他各 1-3 处 |

---

## ⚫ 第 6 类：构建/工具问题 (3 项)

### ISSUE-TOOL-001: Makefile 缺乏跨架构清理

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复 (commit `3a1fba9b`) |
| **现象** | `build/boot.o` 残留上次 aarch64 编译产物, x86_64 链接时 ld 报错 "Relocations in generic ELF" |
| **根因** | Makefile L122 解析期无条件覆写 `.arch` 戳记, `make test-host` 等无链接 make 也会清掉戳记, 导致下次同 ARCH 链接误用残留的异架构 boot.o |
| **修复** | 戳记写入移至 `arch-switch-clean` 配方, 仅真实跨架构切换时更新; 已回归验证 aarch64→test-host→x86_64 序列 (2026-08-23) |
| **建议方案** | （已实施）Makefile `all` 目标自动清理异架构产物 |

### ISSUE-TOOL-002: x86_64 kernel.flat 陈旧未自动重建

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复 (QEMU 脚本双架构均接入陈旧检测; Makefile 侧按需不做) |
| **现象** | lint 修复后旧 kernel.flat 仍存在, QEMU 启动"日志为空, 内核未进入 Rust 入口" |
| **临时处理** | 手动 `make ARCH=x86_64 all` |
| **建议方案** | Makefile 加入文件 mtime 检查, 或 QEMU 启动脚本加入图像陈旧检测 |
| **【本轮复验订正】** | 建议方案的第 2 条已实装: [qemu_boot_test.sh](file:///home/anfer/Code/QueenX/scripts/qemu_boot_test.sh#L133) 新增 `check_kernel_fresh()`（`:133` 定义），当图像早于源码时告警/重建；但**仅 aarch64 分支调用**（`:204`），x86_64 路径未接入。Makefile 侧 mtime 检查仍未做 → 状态由 ❌ 上调为 ⚠️ |
| **【本轮修复（修遗留工程）】** | x86_64 分支已接入 `check_kernel_fresh`（[qemu_boot_test.sh](file:///home/anfer/Code/QueenX/scripts/qemu_boot_test.sh) x86_64 段 `sync_make_state` 后新增 `check_kernel_fresh \|\| true`），与 aarch64 分支口径一致 — QEMU 侧双架构陈旧检测已闭环。Makefile 侧 mtime 检查仍未做（保持不做，QEMU 脚本入口已覆盖实际触发场景）。 |
| **工作量** | 估计 0.5 天 |

### ISSUE-TOOL-003: cargo test --tests 在裸机 target 失败

| 字段 | 数据 |
|---|---|
| **状态** | ✅ 已结案 (前提已变; E0152 整族根治后原建议无必要, 重评不施工) |
| **现象** | E0152 duplicate lang item (zerocopy/bitflags/byteorder/managed) |
| **应对** | 实际测试在 host-side 跑, lint-only 检查通过 |
| **建议方案** | 用 `#[cfg(target_os = "none")]` 隔离测试, 或 host-tests 引入独立测试目标 |
| **【本轮复验订正】** | E0152 整族已根治（2026-09-14 build-std 显式化，见 [src/kernel/Cargo.toml:17](file:///home/anfer/Code/QueenX/src/kernel/Cargo.toml#L17) 与 [framekernel-paradigm-enforcement.md:431](file:///home/anfer/Code/QueenX/docs/plan/framekernel-paradigm-enforcement.md#L431)）；`test = false` 相关前提亦已移除。原"裸机 target 失败"现象不再必然复现 → 本条前提已变，待按新状态重评是否需要 |
| **【本轮修复（修遗留工程）】** | 重评结案: E0152 整族根治后, 内核测试实际一直在 host 侧（`--features host-test`）运行并全绿（`make test-kernel-host` 941 passed），《`test = false` 隔离 host target》的原建议已无必要 — 无需再引入 `#[cfg(target_os = "none")]` 隔离或独立 host 测试目标。本条**结案, 不施工**。 |
| **工作量** | 估计 1 天 |

---

## 🟢 第 7 类：lint 副作用 (2 项已修复)

| # | 描述 | 修复 commit |
|---|---|---|
| LINT-FIX-001 | plan_b_inode_test `vfs_read_uses_inode_trait` brittle substring 失效 (rustfmt 拆行) | `4f1a9d3e` |
| LINT-FIX-002 | plan_b_inode_test `vfs_write_uses_inode_trait` brittle substring 失效 (同上) | `4f1a9d3e` |

---

## 🟤 第 8 类：迁移中子系统状态（driver / chitin）

> 2026-08-31 追加：记录 driver / chitin 两个"迁移中"子系统的完整状态——历史脉络、当前形态、遗留事项。
> 触发：用户要求"记录有关迁移中的完整信息（driver 和 chitin 等）"。
> 来源：源码头注释（services/driver/mod.rs + services/chitin/mod.rs）+ [archive/driver-service-migration.md](./archive/driver-service-migration.md) + [archive/audit-fix-04-framework-net-drivers.md](./archive/audit-fix-04-framework-net-drivers.md) B04-19/D6。
> **关键认知**：迁移方向经历过一次反转——Phase 2.1/2.4 原方向为"framework → services"（业务逻辑迁往 services 做成 safe API）；B04 审计（2026-08-24/25）纠正为"机制留 framework + services 安全代理"（E1000 反向回迁）。因此"未迁移"≠"待迁移到 services"，多数模块的当前形态是**有意维持的中间态**。

### 8.1 迁移历史脉络（三阶段）

| 阶段 | 时间 | 方向 | 内容 |
|---|---|---|---|
| Phase 2.1 driver 迁移 | 2026-06 → 2026-07-22 | framework → services | 6/6 子系统迁移完成，统一 5 步路径（MMIO→`IoMem` / PIO→`IoPort` / DMA→`DmaStream` / IRQ→`IrqLine` / 暴露 safe API）；framework -2317 行 / -37 unsafe。见 [archive/driver-service-migration.md](./archive/driver-service-migration.md) |
| Phase 2.4 net/chitin 迁移 | 2026-06-04 | framework → services | chitin mod + devtree + composite 迁移；net 独立迁往 services/net |
| B04 审计反向纠正 | 2026-08-24/25 | services → framework | **B04-19**（F3 双向依赖）：E1000Driver/E1000Io 整体上移 `framework/driver/net/e1000_io.rs:364-760`，`services/driver/net/e1000.rs` 变 41 行纯 re-export shim；**D6/DECISION-062** 与 B04-09 合并理顺 framework net/driver 边界 |

### 8.2 services/driver 当前状态（三种形态）

| 形态 | 模块 | 位置 | 说明 |
|---|---|---|---|
| **A 完整安全代理**（业务逻辑在 services） | char/vga + char/serial | `services/driver/char/` | VGA 文本模式 + 16550 UART，经 IoMem + IoPort |
| | display/ddc + hdmi + dp | `services/driver/display/` | DDC/HDMI/DP 业务逻辑（EDID 解析/时序/像素时钟），dp 已从 framework 完全迁出（framework 侧无 dp.rs） |
| | storage/nvme + ahci | `services/driver/storage/` | 队列管理/命令提交/读写在 services，DMA 走 framework safe wrapper |
| | virtio/transport + blk + net | `services/driver/virtio/` | VirtIO MMIO Transport + 块/网驱动 100% safe |
| | usb/xhci | `services/driver/usb/xhci.rs` | Capability/Operational/Port/Doorbell 安全访问 |
| | power + acpi | `services/driver/` | power：T4-4 策略主体（2026-06-16 从 framework 提取）；acpi：x86_64 安全代理（aarch64 编译为 0 内容） |
| **B 纯 re-export shim**（逻辑上移 framework） | net/e1000 | `services/driver/net/e1000.rs` | B04-19 后 E1000Driver → `framework/driver/net/e1000_io.rs`，仅留业务常量 + 描述符 re-export |
| | usb/enumerate + hid + ring + usb_core + mass_storage | `services/driver/usb/` | 全部 `pub use framework/driver/usb/*`（framework 侧为 0 unsafe 模块） |
| | uefi + kexec + firmware | `services/driver/` | D5/D10/D11 安全封装 |
| **C 桩模块** | storage/ata | `services/driver/storage/ata.rs` | 仅类型/常量，实际 ATA 逻辑保留 framework（IoPort 需 unsafe） |

### 8.3 services/chitin 当前状态

| 模块 | 状态 | 说明 |
|---|---|---|
| mod（注册表/查找/块/字符/输入 IO） | ✅ 已迁移 | `services/chitin/mod.rs`，强类型 DeviceId/Proto/DeviceState，封装 `framework::chitin` |
| devtree | ✅ 已迁移 | `services/chitin/devtree.rs`，DevTreeNodeId 强类型 + DevTreeError |
| composite | ✅ 已迁移 | `services/chitin/composite.rs`，仅暴露探测入口（RAID 复合设备） |
| proto_*（proto_block/char/input/net） | ❌ 未迁移 | 协议族函数指针表（类 Linux `struct ops`），framework 内部结构 |
| user_driver | ❌ 未迁移 | 用户态驱动接口，`framework/chitin/user_driver.rs` 已实现，services 无封装 |

### 8.4 遗留事项

| # | 事项 | 位置 | 建议 |
|---|---|---|---|
| MIG-001 | 头注释过时：services/driver/mod.rs 声称"⏳ 5/6 未迁移" + E1000 "138 行"（2026-06-04 状态表） | [services/driver/mod.rs:4-35](file:///home/anfer/Code/QueenX/src/kernel/services/driver/mod.rs#L4-L35) | 同步为实际状态：多数模块已迁（A 形态）+ B04 后 E1000 为 re-export shim（B 形态） |
| MIG-002 | 头注释过时：services/chitin/mod.rs 声称"已完成 1/4 子系统迁移" | [services/chitin/mod.rs:4-11](file:///home/anfer/Code/QueenX/src/kernel/services/chitin/mod.rs#L4-L11) | devtree/composite 实际已迁（devtree.rs 自标 3/4、composite.rs 自标 4/4），更新为"3/5" |
| MIG-003 | 缺 pl011（ARM 串口）安全代理 | `services/driver/char/` | char/mod.rs 头注释自标"后续添加"（Phase 2.1.5 后续） |
| MIG-004 | 缺 proto_* + user_driver 安全代理 | `services/chitin/` | framework 已实现，services 无安全封装；若用户态驱动/协议族需开放，先评估边界 |
| MIG-005 | 双份代码/边界未理清 | `framework/driver/storage/` ↔ `services/driver/storage/` | nvme/ahci 两边并存（framework 含 nvme_block.rs/ahci_block.rs/ata_block.rs 完整实现 + services 业务层）；需明确"机制/策略"各自归属，消除重复 |
| | | `framework/driver/display/hdmi/` ↔ `services/driver/display/hdmi.rs` | framework 侧孤儿目录（8 文件 1537 行）已随 DECISION-K 第二十七批删除，双份消解；services 侧残留缺口（控制器未接入启动路径）转记 MIG-008 |
| MIG-006 | 迁移方向变化未入文档 | [archive/driver-service-migration.md](./archive/driver-service-migration.md) | 文档标记"✅ 已完成"但 B04 后方向反转（E1000 回迁 framework），需补注 B04 后的状态 |
| MIG-007 | host-tests 无 chitin 专项集成测试 | `host-tests/tests/` | 已有 driver_display / driver_e1000_eeprom / nvme_ahci_activation / i43_block_bridge / virtio_net_arch_unify / nic_probe_arch_neutral，但 chitin 注册表/IO 无专项覆盖 |
| MIG-008 | services 侧 HDMI/DP 控制器未接入驱动框架/启动路径 | [services/driver/display/hdmi.rs:699](file:///home/anfer/Code/QueenX/src/kernel/services/driver/display/hdmi.rs#L699) / [dp.rs:500](file:///home/anfer/Code/QueenX/src/kernel/services/driver/display/dp.rs#L500) | `HdmiController`/`DpController` 内核内零调用者（仅模块内测试与 host-tests）；显示子系统启动实际走 framework framebuffer（[framework/driver/display/mod.rs:299](file:///home/anfer/Code/QueenX/src/kernel/framework/driver/display/mod.rs#L299) `display_init()`）。TMDS 输出使能与同步极性在 services 已实现（[hdmi.rs:56-58](file:///home/anfer/Code/QueenX/src/kernel/services/driver/display/hdmi.rs#L56-L58) 0x079/0x078），DP OUTPUT_ENABLE 亦已实现（[dp.rs:1188](file:///home/anfer/Code/QueenX/src/kernel/services/driver/display/dp.rs#L1188)）；缺失项为驱动注册/工厂接入路径，及厂商 PHY/DPLL 差异（Intel/AMD/Synopsys）实装——后者待真实硬件接入时按 DECISION-K 注册契约在 services 侧重立 |

> **【本轮复验订正】** 逐项复验（保留原表不涂改）:
> - **MIG-005 文件名漂移**: framework 侧实际为 [ata.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/driver/storage/ata.rs)/[nvme.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/driver/storage/nvme.rs)/[ahci.rs](file:///home/anfer/Code/QueenX/src/kernel/framework/driver/storage/ahci.rs)/ata_block.rs，**无** `nvme_block.rs`/`ahci_block.rs`；"双份代码/边界未理清"的结论仍成立.
> - **MIG-001/002/003/004/006/007/008 仍成立**: [services/driver/mod.rs:4-35](file:///home/anfer/Code/QueenX/src/kernel/services/driver/mod.rs#L4-L35) 与 [services/chitin/mod.rs:4-11](file:///home/anfer/Code/QueenX/src/kernel/services/chitin/mod.rs#L4-L11) 头注释仍为 2026-06-04 旧状态；pl011/proto_*+user_driver 安全代理仍缺；迁移文档未补注 B04；chitin 无专项 host-tests；services 侧 HDMI/DP 控制器仍零调用者.
>
> **后续行动建议**：MIG-001/002 为纯注释同步（低风险，可随下次 driver 改动顺手修复）；MIG-003/004 属功能补齐（需按 §12.3 评估"是否需要"——若当前无调用方，登记即可不施工）；MIG-005/006/007 属架构边界治理（涉及 framework/services 归属决策，按 AGENTS.md §12.1 决策灰色地带处理，需用户裁决）；MIG-008 属接线补齐（控制器与 TMDS/DP 输出使能已实现，仅缺驱动注册/工厂接入面，需驱动注册面决策）。
>
> **【本轮修复（修遗留工程）】** MIG-001/002/006/007 已结案:
> - **MIG-001/002 注释同步**: [services/driver/mod.rs](file:///home/anfer/Code/QueenX/src/kernel/services/driver/mod.rs#L1-L22) 头注释由旧状态表（"Phase 2.1 在途" + E1000 "影子双份"）改写为当前形态（驱动业务层落 services、0 unsafe；framework 保留 MMIO/DMA 环/存储 wire/PL011 等**机制原语**，非影子双份；并补记 B04 反转）；[services/chitin/mod.rs](file:///home/anfer/Code/QueenX/src/kernel/services/chitin/mod.rs#L1-L20) 头注释由"已完成 1/4"更正为"3/5"（devtree/composite 标记已迁；proto_*/user_driver 注明 framework 内部函数指针表 / framework 已实现但 services 无封装），并移除"评估日期"。
> - **MIG-006 结案（不改 archive）**: 依 AGENTS.md §6「archive/ 为历史快照不再修改」，[archive/driver-service-migration.md](./archive/driver-service-migration.md) **有意保持冻结**（已按 commit `fae02a38` 归档、标记"✅ 已完成"）。B04 反转已由 live 文档完整承载（services/driver/mod.rs 头注释 + §8.1 三阶段历史表），故本条**据 live 文档结案**，不修改归档快照。
> - **MIG-007 补测**: 新增 [chitin_registry_io_host_test.rs](file:///home/anfer/Code/QueenX/host-tests/tests/chitin_registry_io_host_test.rs) — 覆盖 services/chitin 注册表语义（`register`/`find_by_name`/`find_by_proto`/`list`/`count`/`set_state`/`unregister`）+ 块设备 IO dispatch（`blk_read`/`blk_write` 成功 round-trip 与 3 类错误路径 + `blk_is_present`/`blk_total_sectors`/`blk_count`）；以进程内 `Mutex` 串行化规避全局 `CHITIN_DEVICES` 并行竞争。**2 passed**。

---

## 🟤 第 9 类：分册 6 调研预存问题（2026-08-31）

> 2026-08-31 追加：分册 6（[audit-fix-06-services-fs.md](./audit-fix-06-services-fs.md)）实施过程中调研发现的 3 个**分册 6 范围外**预存问题。用户裁决（2026-08-31）：统一登记待后续处理（选项 3A），不在本分册修复。

### B06-PRE-001: tmpfs.rs `<256` 硬编码（与 B06-09 同款）

| 字段 | 数据 |
|---|---|
| **位置** | [services/fs/tmpfs.rs:79](file:///home/anfer/Code/QueenX/src/kernel/services/fs/tmpfs.rs#L79) |
| **问题** | `TmpFsInode::is_dir` 用硬编码 `< 256` 判断 inode 范围 + `fs.inner.nodes[...].file_type == 1` 魔法数，与 B06-09 修复前的 RamFsInode 同款缺陷（B06-09 只修了 ramfs，未修 tmpfs） |
| **建议** | 与 B06-09 同法：硬编码 256 → `RAMFS_MAX_NODES` 常量，魔法数 1 → `VfsFileType::Dir.as_u8()` |
| **状态** | 🔄 已失效 (本轮复验: 源码已改为 `nodes.len()` + `VfsFileType::Dir.as_u8()`) |
| **【本轮复验订正】** | [tmpfs.rs:91-96](file:///home/anfer/Code/QueenX/src/kernel/services/fs/tmpfs.rs#L91-L96) 现为 `idx < fs.inner.nodes.len() && fs.inner.nodes[idx].file_type == VfsFileType::Dir.as_u8()`（注释：越界节点一律视为非目录，避免索引 panic）——硬编码 `<256` 与魔法数 `1` 均已消除，本条缺陷已不复存在 |

### B06-PRE-002: fchown_syscall 缺权限校验（安全缺陷）

| 字段 | 数据 |
|---|---|
| **位置** | [services/fs/misc.rs:115-126](file:///home/anfer/Code/QueenX/src/kernel/services/fs/misc.rs#L115-L126) → `vfs_fchown` → `inode().chown()` |
| **问题** | `fchown(fd, owner, group)` 直接把 owner/group 透传给底层 `Inode::chown`，**无任何权限校验**——任意进程可对自己已打开的 fd 修改为任意 owner/group（B06-02 修了 `chown` 的 uid 回退，但 `fchown` 路径的 owner 是 pwm 值、无"未注册回退"问题，却缺失"是否有权修改属主"的检查） |
| **影响** | 权限语义缺失（非直接提权，但违背"能力制"权限模型） |
| **建议** | 与 B06-02 对齐：fchown 前置 `FS_CAP_CHOWN` (bit5) 能力检查，或按提权语义评估 |
| **状态** | ✅ 已修复（本轮: fchown 前置 `FS_CAP_CHOWN` 能力校验） |
| **【本轮复验订正】** | 仍成立：[misc.rs:115-126](file:///home/anfer/Code/QueenX/src/kernel/services/fs/misc.rs#L115-L126) 的 `fchown_syscall` 仍无 `FS_CAP_CHOWN` 校验，owner/group 直接透传底层 `Inode::chown` |
| **【本轮修复（修遗留工程）】** | [misc.rs](file:///home/anfer/Code/QueenX/src/kernel/services/fs/misc.rs) `fchown_syscall` 在 `current_pwm()` 之后、透传底层前新增 `pwm_has_capability(pwm, CAP_DOMAIN_FS, FS_CAP_CHOWN)` 前置校验，缺失即返回 `EPERM`（与 access/open 路径能力语义一致）；`# Errors` 文档注释同步更新。 |

### B06-PRE-003: LegacyInode 删除时机（架构清理）

| 字段 | 数据 |
|---|---|
| **位置** | [services/fs/inode.rs:415-424](file:///home/anfer/Code/QueenX/src/kernel/services/fs/inode.rs#L415-L424) |
| **问题** | B06-12 方案 C（废弃标记 + 推动消除）落地：LegacyInode 已加废弃标记，当前全部 8 个 FS 均实现 `fs_resolve_inode`，LegacyInode 仅作 `open_by_handle_at` 防御性回退（正常路径不触发） |
| **建议** | 未来移除 `open_by_handle_at` 的 LegacyInode 回退分支（file_handle.rs:187）后删除整个 LegacyInode 类型；需确认各 FS `fs_resolve_inode` 覆盖所有挂载场景 |
| **状态** | ✅ 已修复（本轮: 移除 `open_by_handle_at` 回退分支 + 删除 `LegacyInode` 类型） |
| **【本轮复验订正】** | 结论仍成立，**行号漂移**: `LegacyInode` 定义现位于 [inode.rs:268](file:///home/anfer/Code/QueenX/src/kernel/services/fs/inode.rs#L268)，废弃标记注释在 `:260-264`（原记 `:415-424` 现为 `set_times`/`pread_inode`）；[file_handle.rs:199-205](file:///home/anfer/Code/QueenX/src/kernel/services/fs/file_handle.rs#L199-L205) 回退分支仍在（原记 `:187` 漂移） |
| **【本轮修复（修遗留工程）】** | [file_handle.rs](file:///home/anfer/Code/QueenX/src/kernel/services/fs/file_handle.rs) `open_by_handle_at_syscall` 移除 `fs_resolve_inode(...).unwrap_or_else(|| LegacyInode ...)` 防御性回退，改为 `.ok_or(Errno::EINVAL)?`（句柄失效即 EINVAL，与相邻 `mount_idx` 校验一致）；[inode.rs](file:///home/anfer/Code/QueenX/src/kernel/services/fs/inode.rs) 删除整个 `LegacyInode` 类型及其 `impl Inode`（-186 行），文件头"具象实现"注释同步去掉 `LegacyInode`。 |

### B06-PRE-004: socket_max_sockets_test flaky（已排查确认非内核问题，测试已修复）

| 字段 | 数据 |
|---|---|
| **位置** | [host-tests/tests/socket_max_sockets_test.rs](file:///home/anfer/Code/QueenX/host-tests/tests/socket_max_sockets_test.rs) |
| **现象** | 全量 host-tests 偶发失败（单独跑全过），高负载必现趋势 |
| **根因** | 测试自身 8 个 `#[test]` 共享同一 `static G_MAX_SOCKETS: AtomicUsize`（测试内镜像变量），Rust 测试默认并行执行导致测试间互相覆盖 |
| **内核侧排查** | ✅ **无真实缺陷**——(1) 内核 [sockets.rs:38](file:///home/anfer/Code/QueenX/src/kernel/framework/net/init/sockets.rs#L38) 的 G_MAX_SOCKETS 是 `AtomicUsize`（非 static mut），store/load 用 AcqRel/Acquire，无数据竞争；(2) [sm_fi.rs:270-277](file:///home/anfer/Code/QueenX/src/kernel/framework/net/init/sm_fi.rs#L270-L277) 的活动计数 + 上限检查在 `NET_STATE.lock()` 临界区内，无 TOCTOU；(3) `set_max_sockets` 当前无任何调用方，运行时并发写路径不存在 |
| **修复** | 删除共享 static，辅助函数参数化接收 `&AtomicUsize`，每测试独立实例（commit 28fd91d0） |
| **启示** | host-tests 镜像测试"复刻逻辑但不复刻锁/原子性"，镜像测试 flaky 优先怀疑"镜像丢了并发保护"而非内核缺陷 |
| **状态** | ✅ 已修复 (2026-08-31, commit 28fd91d0) |
| **【本轮复验订正】** | 当前测试形态已进一步收敛为**单一顺序测试** `max_sockets_config_semantics`（[socket_max_sockets_test.rs](file:///home/anfer/Code/QueenX/host-tests/tests/socket_max_sockets_test.rs)，随 B08-20 迁移），共享 static 并行覆盖的根因不再存在 |

---

## 📋 文档状态不一致清单 (跨文档矛盾专项)

> **原审计发现 (2026-08-09)**: `progress-active-tasks.md` 中 B1-B6 标记 `[X]` 完成, 但 `archive/code-review-findings-2026-08-01.md` 中对应 REVIEW-FINDING 仍 `[]` 未同步. 记为**文档漂移**.
>
> **2026-09-26 处置结论**: 逐项回源码复验, 实现侧已全部落地 (见 §3.1-3.3). 归档快照 `archive/code-review-findings-2026-08-01.md` 的 `[]` 依 AGENTS.md §6「archive/ 为历史快照不再修改」**有意冻结**, 不属待修漂移 — 因此本清单**结案**, 无需再同步归档文档.

| progress | code-review (归档快照) | 2026-09-26 复验结论 |
|---|---|---|
| B1 [X] | REVIEW-FINDING-027 `[]` | ✅ 落地 — `framework/mod.rs:10` 无 LoC 数字 |
| B2 [X] | REVIEW-FINDING-028 `[]` | ✅ 落地 — net/fs/proc 头注释已更新 |
| B3 [X] | REVIEW-FINDING-029 `[]` | ✅ 落地 — README.md 整篇重写, 原 :21/:71 moot |
| B5 [X] | (无对应) | ✅ 落地 — credo/storage.rs + barrier/api.rs |
| B6 [X] | REVIEW-FINDING-031 `[]` | ✅ 落地 — iomem.rs debug_assert! + constants/limits.rs |
| DECISION-037 [X] | REVIEW-FINDING-025 `[]` | ✅ 落地 — framework/syscall/mod.rs:22-30 注释统一 |
| DECISION-038 [X] | REVIEW-FINDING-024 `[]` | ✅ 落地 — 全仓 0 引用 (2026-09-26 补清 host-tests/README.md 3 处) |
| DECISION-039 [X] | REVIEW-FINDING-026 `[]` | ✅ 落地 — UserContext 已迁回 framework, services 反向 re-export |

**结案说明**: 原"建议后续行动"(同步 code-review 文档状态) 经复验判定**不需执行** — 归档快照按规矩冻结, 实现侧无遗留.

---

## 🎯 优先级建议 (用户决策参考)

> **【本轮复验订正】** 下方 P0-P3 为 **2026-08-09 原登记**（保留不涂改）；按当前源码状态复验后，其中 **REVIEW-FINDING-026/027/028/029** 与 **ISSUE-SRC-001~016** 均已落地/消除，应从待办移出. 当前实际开放项建议排序如下:
> - **P0**: ISSUE-RT-002（aarch64 GICv3 挂起，未修复；`.gdb_debug_gic` 证据已失效，以用户实际状态为准）.
> - **P1**: ISSUE-RT-001（x86_64 e1000/smoltcp 挂起）; B06-PRE-002（fchown 缺权限校验，**安全缺陷**）; ISSUE-RT-003（真实硬件验证）.
> - **P2**: 第 8 类 MIG 边界治理（MIG-005/006/007）/注释同步（MIG-001/002）; 第 0B 类 B03-LEGACY-001/002/003（COW TOCTOU + host-tests 缺口）; ISSUE-TOOL-002（x86_64 侧陈旧检测）.
> - **P3**: 第 4 类远期工程 F1-F5; B06-PRE-003（LegacyInode 清理）; MIG-003/004/008; 刻意维持项 DEC-046/041/005.
>
> **【本轮修复（修遗留工程）后开放项变化】**: 下列项已从本节待办移出 — **B06-PRE-002**（fchown 能力校验，P1）、**B03-LEGACY-001/002/003**（COW TOCTOU + host-tests 缺口，P2）、**ISSUE-TOOL-002**（x86_64 侧陈旧检测，P2）、**B06-PRE-003**（LegacyInode 清理，P3）、**MIG-006/007**（迁移文档补注 / chitin 测试缺口，P2）；**ISSUE-TOOL-003** 重评结案不施工；**MIG-001/002** 注释同步完成。仍未闭合: **ISSUE-RT-001/002/003**（运行时挂起/真机验证）、**MIG-003/004/005/008**、第 4 类远期工程 F1-F5、刻意维持项.

### P0 — 立即关注 (1 项)

- **ISSUE-RT-002** (aarch64 GICv3 挂起) — 用户当前 GDB 调试中

### P1 — 本季度修复 (5 项)

- **ISSUE-RT-001** (x86_64 e1000/smoltcp 挂起)
- **ISSUE-RT-003** (真实硬件验证)
- **REVIEW-FINDING-026** (framework→services 反向依赖)
- **ISSUE-SRC-001~016** (16 个 P1 TODO)

### P2 — 半年内修复 (~30 项)

- P2 TODO 22 项
- REVIEW-FINDING-027/028/029 (文档同步)
- 构建工具问题 3 项

### P3 — 长期/远期 (无固定期限)

- 远期工程 F1-F5
- P3 TODO 5 项
- REVIEW-FINDING-030/031

---

## 📚 关联文档

- [stage-engineering-master.md](./stage-engineering-master.md) — 静态检查工程权威跟踪
- [progress-active-tasks.md](./progress-active-tasks.md) — 活跃任务进度
- [future-roadmap.md](./future-roadmap.md) — 远期工程 F1-F5
- [ipv6-dual-stack.md](./ipv6-dual-stack.md) — Phase 6 DHCPv6
- [unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) — 未修复问题专项追踪
- 已归档: [archive/code-review-findings-2026-08-01.md](./archive/code-review-findings-2026-08-01.md), [archive/handoff-2026-08-07.md](./archive/handoff-2026-08-07.md), [archive/handoff-2026-08-09.md](./archive/handoff-2026-08-09.md)

---

## 变更历史

- **本轮（修遗留工程）**: 按用户授权批量修复登记遗留项，逐条追加【本轮修复（修遗留工程）】标记；§2.3 六门槛全量验证通过（双架构 build 5/0、clippy + audit quick 全绿、`make test-host` 0 failed、`make test-kernel-host` 941 passed、QEMU x86_64 1/1）
  - 第 0B 类 3 项结案: B03-LEGACY-001（COW 判定+映射并入同一 `VMM_LOCK` 临界区，配套 `*_locked` 变体）/ B03-LEGACY-002（pmm `find_contig_range`/`reserve_range`/`unreserve_range` host 用例 ×3）/ B03-LEGACY-003（新增定时器 tick 并发 host 冒烟 + 文件头标注判别力边界，弱序语义判别待 aarch64 SMP）
  - 第 6 类: ISSUE-TOOL-002 x86_64 分支接入 `check_kernel_fresh`（QEMU 侧双架构闭环）；ISSUE-TOOL-003 重评结案（E0152 整族根治后原建议无必要，不施工）
  - 第 8 类: MIG-001/002 头注释同步（driver/chitin 头状态改写为当前形态）；MIG-006 据 live 文档结案（尊重 §6 archive 冻结，不改归档快照）；MIG-007 新增 chitin 注册表/IO 专项 host 测试（2 passed）
  - 第 9 类: B06-PRE-002 fchown 前置 `FS_CAP_CHOWN` 能力校验（安全缺陷修复）；B06-PRE-003 移除 `open_by_handle_at` LegacyInode 回退分支 + 删除 `LegacyInode` 类型
  - 第 2 类: ISSUE-SRC-008 结案（原 qx 自有代码唯一真实 TODO）。处理侧 `net_rx_softirq_handler` → `poll_network()`；触发侧 framework 新增 ISR→softirq 机制层 `net/init/irq.rs`（x86_64 MSI-X / aarch64 GIC SPI 双架构），services 侧 e1000 + virtio-net 接线 ISR 与中断 ack。第 2 类源码未实现项**全部清零**（0 活跃 TODO）
- **本轮复验（源码状态核实）**: 按当前源码状态逐条复核本文档，保留原登记不涂改，各节追加【本轮复验订正】标记
  - 第 2 类 41 项 TODO 已消除 / 1 项仍存在（ISSUE-SRC-008 行号 822→601）/ 1 项文件不存在（ISSUE-SRC-028 `vfs/api.rs`）
  - 第 0 类审计基线 EXIT=0 / 0 违规（PROXY_ALLOWANCE 实测 8 条，原记 5 处）；comment_language 724 文件 0 违规（原记 735）
  - 第 1 类 RT-001/RT-003 仍成立，RT-002 未修复但 `.gdb_debug_gic` 证据失效
  - 第 6 类 TOOL-001 已修复；TOOL-002 QEMU 侧已实装 `check_kernel_fresh`（仅 aarch64 调用）；TOOL-003 E0152 前提已根治
  - 第 8 类 MIG-001~008 仍成立，MIG-005 文件名漂移；第 9 类 B06-PRE-001 已失效 / 002 仍成立 / 003 行号漂移 / 004 已修复
  - 第 3 类 024「全仓 0 引用」与 029「18 行」表述漂移；第 5 类 DEC-046 计数 1354/166→1406/173，DEC-005 计数 345→492
  - 文末优先级建议段已订正（移出已修复项，给出当前 P0-P3）
- **2026-08-31**: 新增 B06-PRE-004（socket_max_sockets flaky 排查）
  - 根因确认为测试自身共享镜像 static（非内核缺陷），内核侧 AtomicUsize + NET_LOCK 临界区均安全
  - 测试已修复（commit 28fd91d0）；总览计数 ~80 → ~81
- **2026-08-31**: 新增第 9 类"分册 6 调研预存问题"3 项（B06-PRE-001/002/003）
  - 用户裁决 3A：tmpfs `<256` 硬编码 / fchown 缺权限校验（安全缺陷，建议 P1）/ LegacyInode 删除时机，统一登记待后续处理
  - 总览计数 ~77 → ~80
- **2026-08-31**: 同步"审计基线待清零"状态（2 项已处理）
  - BASELINE-F7-067（67 处英文注释）→ 🔄 已修复（2026-08-30 中文化清零，见分册 10 B10-03）
  - BASELINE-F2-012（12 处 HIGH）→ 🔄 已处理（5 处 PROXY_ALLOWANCE 豁免 + 实测 boundary 0 违规，见分册 10 B10-06）
  - 总览表对应行状态更新
- **2026-08-31**: 新增第 8 类"迁移中子系统状态（driver / chitin）"
  - 记录 driver / chitin 两个迁移中子系统的完整状态：三阶段历史脉络（Phase 2.1/2.4 迁出 + B04 反向纠正）、services/driver 三种形态（A 完整安全代理 / B re-export shim / C 桩）、services/chitin 5 模块状态
  - 新增 MIG-001~007 遗留事项（头注释过时 / 缺 pl011 / 缺 proto_*+user_driver / 双份代码边界 / 迁移文档未补注 / chitin 测试缺口）
  - 总览计数 ~70 → ~77
- **2026-08-23**: 分册 3 归档登记
  - 新增第 0B 类"分册 3 归档遗留"3 项（COW TOCTOU / pmm-swap host-tests 缺口 / 多核 tick 测试）
  - ISSUE-TOOL-001（Makefile 跨架构清理）标记已修复（commit 3a1fba9b，戳记写入移至 arch-switch-clean 配方）
  - 总览计数 ~67 → ~70
- **2026-08-09**: 创建本文档 (审计触发: 用户问题"未被修复的问题有哪些?")
  - 来源: 本会话对所有静态 + 动态 + 文档 + 源码的全面审计
  - 产出: 65 项未修复问题分类登记
  - 推荐: 用户授权"仅记录不修复"的 code-review 8 项 + P0/P1 立即关注 + P2/P3 长期规划