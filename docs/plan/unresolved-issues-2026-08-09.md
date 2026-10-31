# Edgine 未修复问题追踪清单 (2026-08-09)

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
| 运行时已知问题 | 3 (+2 后续登记) | P0×1 + P1×2 (+P1×2) | 🔄 RT-001 已修复 (`[X]`); RT-002 压测不可复现 (保留; 本轮消除 EL0/EL1 分发分叉并复压 50/50, 仍开放); RT-003 交付物就绪 (真机执行待硬件); **RT-004 已修复 (`[X]`)**; **RT-005 已修复 (`[X]`, EL0 中断可达)** |
| 源码未实现 (TODO) | ~43 | P1×16 + P2×22 + P3×5 | ❌ 未修复 |
| 跨文档矛盾 (code-review) | 8 | P1×3 + P2×3 + P3×2 | 🔄 已修复 (2026-09-26 复验; 归档快照冻结) |
| 远期工程 | 6 | 远期 | ❌ 未启动 |
| 本会话刻意维持 | 3 | 决策登记 | ⏸️ DECISION |
| 构建/工具问题 | 3 (+2 前轮登记) (+1 本轮) | 工具 | 🔄 TOOL-001/002 已修复；TOOL-003 重评结案不施工；TOOL-004/005 前轮登记即修复 (`[X]`)；❌ **TOOL-006 未修复**（`ci-rustdoc` 在 HEAD 上即红，不在 §2.3 六门槛内；用户裁定单开 ⇒ 正文已迁 [ci-rustdoc-closeout.md](./ci-rustdoc-closeout.md)） |
| 审计基线待清零 (2026-08-23) | 2 | F2×12 + F7×67 | 🔄 已处理 (2026-08-30) |
| 审计脚本门禁修复登记 (2026-08-25) | 8 | 门槛 fail-open×8 | 🔄 全部已修复；004~008 本轮（AGENTS 目录表维护期门槛复验）登记即修复，见 AUDIT-TOOL-004..008 |
| 分册 3 归档遗留 (2026-08-23) | 3 | 遗留×3 | ❌ 待下轮 |
| lint 副作用 (已修复) | 2 | — | 🔄 已修复 |
| 迁移中子系统状态 (2026-08-31) | 8 | MIG×8 | ⚠️ 迁移中 (有意识中间态) |
| 分册 6 调研预存问题 (2026-08-31) | 3 | B06-PRE×3 (1 安全) | ❌ 用户裁决登记待后续 |
| socket_max_sockets flaky 排查 (2026-08-31) | 1 | B06-PRE-004 | ✅ 已修复 (非内核问题) |
| **总计** | **~82 项 (2026-08-09 原登记)** | — | **复验订正: 真正仍开放 ≈ 25 项 + 3 项刻意维持** |

> **【本轮复验订正】** 按当前源码状态逐类复核 (原登记数保留上表, 不涂改):
> - **源码未实现 (TODO)**: 原 ~43 项 → 实测**仅剩 1 项**真实 TODO ([privileged/net/init.rs:601](file:///home/anfer/Code/Edgine/src/kernel/privileged/net/init.rs#L601), 即原 ISSUE-SRC-008, 行号由 `:822` 漂移); 另 1 项 ISSUE-SRC-028 所在文件 `functions/fs/vfs/api.rs` 已不存在 (拆分迁至 `handle.rs`). 其余 41 项全部消除 (源码仅残留 `TRACK-xxxxxx 消除` 说明注释).
> - **【本轮修复（修遗留工程）】** 上条所述**唯一**真实 TODO **ISSUE-SRC-008 已修复** → 第 2 类源码未实现项**全部清零** (eg 自有代码 0 活跃 TODO).
> - **跨文档矛盾**: 8 项实现侧均已落地 (见 §3), 归档快照按 §6 冻结 → 仍开放 **0**.
> - **审计基线**: 实测 [audit_functions_boundary.py](file:///home/anfer/Code/Edgine/scripts/audit_functions_boundary.py) **EXIT=0 / 0 违规**、`audit_comment_language.py` **724 文件 0 违规** → 仍开放 **0**.
> - **运行时已知问题**: 3 项仍成立 (RT-001 见 [qemu_boot_test.sh:168](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh#L168) 注释; RT-002 未修复但 `.gdb_debug_gic` 证据失效; RT-003 未运行).
> - **构建/工具**: TOOL-001 已修复; TOOL-002 QEMU 侧陈旧检测已实装 ([check_kernel_fresh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh#L133)); TOOL-003 的 E0152 前提已根治 (2026-09-14 build-std 显式化) → 待按新状态重评。**【本轮】TOOL-003 重评结案不施工；新增 TOOL-004（x86_64 AP 启动锁无竞争者 → deadlock 审计唯一 HIGH）与 TOOL-005（check_doc_paths 校验范围语义）登记即修复 → 第 6 类仍开放 0。**
> - **分册 3 归档遗留**: 3 项均仍成立 (COW TOCTOU 见 [cow.rs:464](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/cow.rs#L464); pmm/swap 与多核 tick 的 host-tests 缺口均无对应用例).
> - **迁移中子系统**: MIG-001/002/003/004/006/007/008 仍成立; MIG-005 文件名漂移 (privileged 侧实为 `ata.rs`/`nvme.rs`/`ahci.rs`/`ata_block.rs`, **无** `nvme_block.rs`/`ahci_block.rs`).
> - **分册 6 预存问题**: B06-PRE-001 已失效 (tmpfs 改用 `nodes.len()` + `VfsFileType::Dir.as_u8()`); B06-PRE-002 仍成立 (安全缺陷); B06-PRE-003 行号漂移.
> - **远期工程**: FUT-001~005 未启动; FUT-006 (WASM WASI) 已完成.
> - **刻意维持**: DEC-046 计数 1354/166 → **1406/173**; DEC-005 计数 345 → **492** (逐项见各节).

---

## 🔵 第 0 类：审计基线待清零（2026-08-23 追加，无分册负责）

> 两类审计基线违规**无任何分册（03-09）明确负责修复**，登记防止委派遗漏。
> 分册 01 声称"12 处 HIGH 后续分册 02-07 迁移范围"与"68 处行尾英文注释后续 commit 手工翻译"均未落实为具体条目。

### BASELINE-F2-012: audit_functions_boundary 12 处 HIGH（functions 访问 privileged 内部）

| 字段 | 数据 |
|---|---|
| **规则** | F2（functions 禁止访问 privileged 内部模块），META-P0-01 识别 |
| **数量** | 12 处 HIGH（黑名单补全后识别，commit 4ba454ab） |
| **文件** | `functions/debug/ebpf_verifier.rs`、`debug/mod.rs`、`io/iouring.rs`、`ipc/msgq.rs`、`mm/madvise_mlock.rs`、`proc/coredump.rs`、`proc/memfd.rs`、`proc/pidfd.rs`、`syscall/dispatch.rs`、`syscall/mod.rs` |
| **分册覆盖核查（2026-08-23）** | 分册 03-09 无条目明确负责修复这些文件的 F2 边界违规（各分册条目只修功能/逻辑，如 B05-32 pidfd、B07-18 ebpf）；分册 09 B09-11/12/13 的"F2 治理"仅覆盖 **privileged→functions 反向依赖（D8）**，方向相反不覆盖本项 |
| **处理（2026-08-30）** | 🔄 已处理——5 处代理层自拦截误报经 `PROXY_ALLOWANCE` 豁免（[audit_functions_boundary.py](file:///home/anfer/Code/Edgine/scripts/audit_functions_boundary.py) 白名单：debug/mod.rs、ebpf_verifier.rs、ipc/msgq.rs、proc/coredump.rs）；实测 `audit_functions_boundary.py` 当前 **0 违规**（黑名单补全后剩余 HIGH 均已合规或经豁免），见分册 10 B10-06 |
| **【本轮复验订正】** | `PROXY_ALLOWANCE` 实测为 **8 条**（原记"5 处"为 2026-08-30 首次登记数，后经 DECISION-J 第十七批等扩容：debug/mod.rs、ebpf_verifier.rs×2、ipc/msgq.rs、proc/coredump.rs、sync/types.rs、freg/reset_config.rs、syscall/types.rs）；本轮实测 `python3 scripts/audit_functions_boundary.py` **EXIT=0 / 0 违规**（另 2 项 MEDIUM `UNLISTED_INTER_MODULE_DEP` 非边界违规） |
| **来源** | archive/audit-fix-01 L227 + archive/audit-fix-02 L339/394 |

### BASELINE-F7-067: audit_comment_language 67 处违规（F7 中文注释强制）

| 字段 | 数据 |
|---|---|
| **规则** | F7（中文注释强制），70 → 67（2026-08-21 诊断删除 -2，2026-08-22 F7 修复 -1） |
| **数量** | 67 处，涉及 34 个文件 |
| **分布** | privileged 全树英文注释（acpi/uart/gic/mmu/edid 等，见 `audit_comment_language.py` 输出） |
| **分册覆盖核查（2026-08-23）** | 分册 03-09 无英文注释翻译条目；分册 01 声称"后续 commit 手工翻译"未落实；分册 09 仅覆盖 F1/F9/F2/D8 死代码，无 F7 条目 |
| **处理（2026-08-30）** | 🔄 已修复——按用户授权逐处中文化（34 文件，技术术语保留英文 + 中文说明），实测 `audit_comment_language.py` **0 违规**（"扫描 735 个 .rs 文件, 0 违规"），见分册 10 B10-03 |
| **【本轮复验订正】** | 实测输出为 "扫描 **724** 个 .rs 文件, 0 违规"（原记"735"随文件增删漂移）；结论（0 违规）不变 |
| **来源** | archive/audit-fix-01 L221 + archive/audit-fix-02 L342 |

---

## 🔵 第 0A 类：审计脚本门禁修复登记（2026-08-25 追加）

> 2026-08-25 修复 audit_invariants.py 误报与 CI fail-open 门禁失效，登记修复依据与验证结果。
> 本类后续追加同类条目：004~008 由门槛复验产生（QEMU 软门禁默认值、workflow 内 4 处退出码采集缺陷、aarch64 `.a` 陈旧检测），均**本轮登记即修复**并带本地实测验证。

### AUDIT-TOOL-001: audit_invariants.py I2 误报 127 处（已修复）

| 字段 | 数据 |
|---|---|
| **根因** | B01-21（commit c00e9e55）将 I2 检测范围从 functions 改为 privileged——**方向性错误**。I2 不变式（AGENTS.md §4.2）语义为"内核内存不可被 **functions** 非法访问"，守护对象是 functions；privileged 是合法 TCB，裸指针解引用是其职责（127 处全部位于 unsafe 块内），其安全由 F4（audit_safety_coverage.py SAFETY 100%）守护，不属 I2 范畴 |
| **修复** | B01-25：I2 恢复扫描 functions（实测 0 违规），删除 `_scan_privileged` 与 `PRIVILEGED` 变量 |
| **验证** | 脚本 6 项不变式全 PASS + 退出码 0；构造临时 functions 违规探针验证 I2 仍能捕获（检测能力未退化） |
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

### AUDIT-TOOL-004: qemu_boot_test.sh 默认软门禁使 §2.3 第 5 条假绿（已修复）

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复（本轮登记即修复） |
| **现象** | 里程碑缺失或 QEMU 启动失败时脚本仍返 0，`make test-qemu` / `Makefile.ci` / `ci-aarch64.yml` 三条调用方一律显示绿 |
| **根因** | `FAIL_OK="${FAIL_OK:-1}"` 默认放宽，各里程碑失败点写作 `[ "$FAIL_OK" = "0" ] && RESULT=1` ⇒ 默认值下失败不置 `RESULT`；其原注释理由为"e1000 已知挂起"，而该前提已被本文档 ISSUE-RT-001 的修复推翻（现 x86_64 全里程碑通过），默认值与注释同时失效 |
| **修复** | 默认改 `FAIL_OK:-0`（fail-closed）；诊断性跑仍可显式 `FAIL_OK=1` 放宽，但放宽时额外打 warn 横幅"本轮结果不可作为 §2.3 第 5 条验收依据"，汇总行同步打印当前门禁模式；端到端连通性那条（`:563`）原本即为硬门禁，不受放宽影响 |
| **验证** | `bash -n` 通过；`FAIL_OK=0 ./scripts/qemu_boot_test.sh all` 实跑 **2/2 全里程碑绿**（含 aarch64 `A64_E2E observe=pass accept=3 echo=3 addr=dhcp tx_timeout=0`），证严格口径可承受；调用方 Makefile / `Makefile.ci` / `ci-aarch64.yml` 不传 FAIL_OK 自动继承严格值，`ci/audit.sh:392` 原已显式传 0 |
| **后续** | 无（本轮闭环） |

### AUDIT-TOOL-005: ci-lint.yml audit-selftest job 永不失败（已修复）

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复（本轮登记即修复） |
| **根因** | 写作 `SELFTEST_OUT=$(cmd || true)` 之后，再从 `echo "$OUT" \| tee \| tail` 管道读 `${PIPESTATUS[0]}` —— 取到的是 `echo` 的 0（且 `|| true` 已先把真实码抹成 0），该 job 无论自测结果如何都打 ✅；与 AUDIT-TOOL-002/003 同一族，但当时未覆盖到本 job |
| **修复** | 改用本文件既有的 B01-25 模式：`set +e` / `SELFTEST_RC=$?` / `set -e` 后再回放输出并判 RC，非 0 则 `::error::` + `exit 1`；补 `mkdir -p other/build/log` |
| **验证** | `python3 scripts/tests/audit_selftest.py` 本机实跑 4/4 通过 RC=0（改后判据不会误红）；YAML 解析通过 + 内嵌 run 脚本 `bash -n` 语法校验 0 失败 |

### AUDIT-TOOL-006: 双架构 cargo check job 的判据 grep 不可达（已修复）

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复（本轮登记即修复） |
| **现象** | `ci-x86.yml` / `ci-aarch64.yml` 的 `cargo-check` job 从不真正判定编译结果 |
| **根因** | 双重缺陷叠加：① 本 job 无 `mkdir -p ../../other/build/log`，而 `other/` 属 gitignore（`git ls-files other/` = 0 个跟踪文件），fresh runner 上该目录不存在 ⇒ `cargo check \| tee <不存在的目录>` 的管道退出码 = tee 的 1，在 Actions 默认 `bash -e` 下**当场打断本步**，后续 grep 永不执行（编译结果从未被判定，job 恒红且原因与代码无关）；② 即使目录存在，判据 `grep -q "error\["` 也漏 `error:` 前缀的链接期/manifest 类失败（同文件 clippy job 用的是更严的 `^error\[\|^error:`） |
| **修复** | 两 job 均改 B01-25 模式：`mkdir -p` + `set +e` 捕获 cargo 真实退出码 + `echo "$OUT" \| tee \| tail -20` 回放 + `[ "$RC" -ne 0 ]` 阻断，判据从"文本形态"收紧为"退出码" |
| **验证** | 本地模拟 fresh runner 语义：`bash -e -c 'echo hi \| tee no/such/dir/x.txt; echo CONTINUES'` → 只打印 tee 报错、`CONTINUES` 未出现、RC=1（证伪原 grep 判据可达性）；YAML 与 run 脚本语法校验通过 |
| **关联** | 这些 workflow 在 Gitee remote 上根本不被触发（见 [net-e2e-tcp-echo-flakiness.md](./net-e2e-tcp-echo-flakiness.md) §6 P-37），修复的是"镜像到 GitHub 后判据是否可信"，不改变当前实际门槛只有本地 `ci/audit.sh` 这一事实 |

### AUDIT-TOOL-007: ci-x86.yml functions 编译 job 真 fail-open（已修复）

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复（本轮登记即修复） |
| **根因** | `cargo build ... \| tee log \| tail -5` 的管道退出码 = `tail` 的 0（该 job 有 mkdir，故不会像 AUDIT-TOOL-006 那样打断），随后仅 `grep -E "error.*unsafe\|functions/.*\.rs.*unsafe"` 判 unsafe 形态 ⇒ 与 unsafe 无关的编译失败照样打 ✅ 并 exit 0，属真 fail-open |
| **修复** | 保留 unsafe 形态 grep（它是该 job 的专项语义），在其之前补 B01-25 的 cargo 真实退出码判定，非 0 时打印 `^error(\|:)` 前 20 行并 `exit 1` |
| **验证** | YAML 解析 + `bash -n` 通过；判据形状与 AUDIT-TOOL-006 一致，不再依赖管道退出码 |

### AUDIT-TOOL-008: aarch64 `$(RUST_LIB)` 缺 kernel 源码前置依赖（已修复）

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复（本轮登记即修复，用户裁定本轮一并修） |
| **现象** | 只改 `src/kernel/**/*.rs` 时，`make ARCH=aarch64` 判定 `other/target/aarch64-unknown-none-softfloat/release/libkernel.a` "已是最新"并跳过 cargo 重建 ⇒ 链接静默沿用陈旧 `.a`（与 x86_64 侧同病灶，P-32 §4.4 "假象回归"一族） |
| **根因** | Makefile 的 aarch64 分支 `$(RUST_LIB): $(USER_BUILD_DIR)/init.bin` 未带 `$(shell find src/kernel -name '*.rs')`，而 x86_64 分支带、`$(RUST_LIB_TEST)` 也带（后者注释即记 E-06 踩坑） |
| **修复** | aarch64 分支补齐同一前置依赖，并加中文注释锚 E-06 先例；不顺手引入 x86 侧独有的 `$(STAGE1_BIN)`（aarch64 无 stage1 引导，保持最小改动） |
| **验证** | 负控制实测：`touch src/kernel/build.rs` 后 `make -n` —— 旧规则 aarch64 触发 cargo 重建次数 **0**（漏检）/ x86_64 **1**；改后两架构均 **1**（判据 `grep -c "cargo build"`） |

---

## 🔵 第 0B 类：分册 3 归档遗留（2026-08-23 归档登记）

> 分册 3（[archive/audit-fix-03-framework-mm-sync.md](./archive/audit-fix-03-framework-mm-sync.md)）归档时核查出 3 项"方案承诺未完全落地"的遗留，登记防止归档后丢失追踪。分册 3 主修复（B03-01~28）均已实装并通过验证门槛（双架构编译 0w0e + host-tests + 审计无回归）。

### B03-LEGACY-001: COW TOCTOU（cow_handle_fault 锁内判定+锁外执行）

| 字段 | 数据 |
|---|---|
| **来源** | archive/audit-fix-03 B03-06（方案明确"TOCTOU 修复留作下轮独立 PR（与 fork-exit host-tests 一并）"） |
| **位置** | `privileged/mm/cow.rs` `cow_handle_fault` |
| **问题** | 锁内判定 COW 页 + 锁外执行映射，存在 TOCTOU 窗口（多核并发下共享页状态可能变化） |
| **建议方案** | 判定与映射放入同一临界区（持 VMM_LOCK 下操作）；补 fork-exit 共享页引用计数 host-tests |

### B03-LEGACY-002: pmm/swap host-tests 缺口

| 字段 | 数据 |
|---|---|
| **来源** | archive/audit-fix-03 B03-04 + DECISION-050（"host-tests 留作下批补"、"pmm::find_contig_range host-tests + 回滚路径测试"） |
| **位置** | `privileged/mm/pmm.rs::find_contig_range`/`reserve_range`、`swap.rs::deinit` |
| **现状** | 代码实装完成（swap init 改 find_contig_range + reserve_range，deinit 走 unreserve_range 回滚），双架构编译 + 全量验证通过；但 host-tests 无对应用例 |
| **建议方案** | 补 find_contig_range 连续范围扫描、reserve_range 重叠拒绝、deinit unreserve 回滚路径测试 |

### B03-LEGACY-003: 多核 tick 计数器内存序测试

| 字段 | 数据 |
|---|---|
| **来源** | archive/audit-fix-03 B03-12（方案"补多核 tick 测试"） |
| **位置** | `privileged/timer/tick.rs` `TICK_COUNT`（fetch_add 已从 Relaxed 改 AcqRel） |
| **现状** | 代码 Ordering 修复完成（516a64d6），host-tests 无多核用例（host 单核难以覆盖） |
| **建议方案** | QEMU SMP kernel_test 补多核 tick 可见性测试，或按 ROE（Return-On-Effort）说明豁免 |

> **【本轮复验订正】** 3 项遗留均仍成立:
> - B03-LEGACY-001: [cow.rs:464](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/cow.rs#L464) 的 `frame_ref_count` 判定与 [cow.rs:470](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/cow.rs#L470) 的 `map_page_in_table` 映射仍不在同一临界区，TOCTOU 窗口存在.
> - B03-LEGACY-002: host-tests 全树无 `find_contig_range`/`reserve_range`/`unreserve_range` 用例（grep 0 命中），缺口成立.
> - B03-LEGACY-003: [tick.rs:165](file:///home/anfer/Code/Edgine/src/kernel/privileged/timer/tick.rs#L165) 已为 `AcqRel`，但无多核 host 用例（host 单核），缺口成立.
>
> **【本轮修复（修遗留工程）】** 3 项遗留已全部结案:
> - **B03-LEGACY-001**: [cow.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/cow.rs) 将 `cow_handle_fault` 拆为"取锁外壳 + `cow_handle_fault_locked`（要求调用方已持 `VMM_LOCK`）"两层; 帧计数判定 (`frame_ref_count`) 与其后的映射 (`map_page_in_table_locked`)、帧归还 (`release_frame_locked`) 现**在同一临界区内**完成, TOCTOU 窗口消除. 配套在 [vmm_x86_64.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/vmm_x86_64.rs) / [vmm_aarch64.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/vmm_aarch64.rs) 增设 `*_locked` 不自取锁变体（非重入 `VMM_LOCK` 保护）; 已持锁的调用点直接走 `_locked`.
> - **B03-LEGACY-002**: [pmm_buddy_host_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/pmm_buddy_host_test.rs) 新增 3 个用例 — `pmm_find_contig_range_scan_and_reject`（连续扫描 + 契约拒绝: size==0 / 非页对齐 / 超总空闲）、`pmm_reserve_range_rejects_overlap_and_misuse`（重叠/已分配/内核区/越界拒绝）、`pmm_unreserve_range_rolls_back`（预留→回滚→重新命中空闲池 + 重复回滚拒绝），覆盖 swap `init`/`deinit` 依赖路径.
> - **B03-LEGACY-003**: 新增 [timer_tick_concurrency_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/timer_tick_concurrency_test.rs) — host 真多核上直接驱动内核真实 `on_timer_interrupt()` 路径, 4 写线程并发递增 + 2 读线程并发校验, 断言读-读单调不回退与终值精确（无丢失更新）; 文件头以「判别力边界」显式声明: 本用例为并发健壮性 + 回归防护, **不声称**证明 `AcqRel` 相对 `Relaxed` 的必要性（x86_64 TSO 下 `lock` RMW 即全屏障, 二者运行时不可区分）, 弱序语义判别待 aarch64 SMP 落地后补.

---

## 🔴 第 1 类：运行时已知问题 (7 项)

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
| **调试入口** | [e1000_io.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/net/e1000_io.rs) (寄存器位域常量), [e1000.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/net/e1000.rs) (驱动实装) |
| **来源** | `scripts/qemu_boot_test.sh` 注释: "x86_64 走到 e1000 NIC 检测后因 smoltcp 初始化挂起, 已记录. e1000 调试见 driver/net/e1000.rs" |
| **关联** | ISSUE-SRC-002 (skb 投递到 smoltcp 未实现) |
| **建议方案** | (1) 隔离 NIC 后单独调试 smoltcp 初始化路径; (2) 检查 e1000 probe 与 smoltcp iface 创建的 race condition; (3) 逐步加 NIC 看具体哪个 packet 触发挂起 |
| **工作量** | 估计 1-2 周 |
| **【本轮结案】** | 根因为 [e1000_io.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/net/e1000_io.rs) 三处寄存器位域常量写错: `E1000_CTRL_RST` 误用 bit31 (该位实为 `E1000_CTRL_PHY_RST` PHY 复位, 82540EM 不因它触发全局复位 -> 复位轮询超时) -> 修为 bit26 (`0x0400_0000`, Global Reset); `E1000_CTRL_FRCDPX` 误用 bit14 -> 修为 bit12 (Force Duplex); `E1000_RCTL_BSIZE_2048` 误用 bit25 (该位实为 `RCTL_BSEX` 缓冲区尺寸扩展位, 非尺寸编码) -> 修为 `0x0` (BSEX=0, BSIZE=00b -> 2048B). 修复后 [qemu_boot_test.sh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh) 恢复 QEMU 默认 e1000 并新增 `e1000: 初始化完成` 回归断言; 静态契约回归用例 [driver_e1000_ctrl_bits_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/driver_e1000_ctrl_bits_test.rs) 固化上述三处位域值, 防止重现. 本轮 §2.3 六门槛复验: x86_64 侧 `FAIL_OK=0 ./scripts/qemu_boot_test.sh all` 通过 (VFS ready / `e1000: 初始化完成` / Entering Ring 3 / KPTI). |

### ISSUE-RT-002: aarch64 GICv3 挂起 ⚠️ **用户当前调试**

| 字段 | 数据 |
|---|---|
| **严重度** | P0 (用户当前调试中) |
| **状态** | 🟡 根因已定位并消除 (ARE 未显式置位), 真机复验待硬件 (`[X]` 代码侧) |
| **类型** | 运行时挂起 (中断控制器) |
| **现象** | GICv3 初始化或中断处理挂起 |
| **影响** | aarch64 启动可能挂起 (本次 QEMU 启动虽到 EL0, 但偶发 GICv3 相关问题) |
| **调试方式** | GDB 调试 (`.gdb_debug_gic` 文件) |
| **源码位置** | `src/kernel/privileged/arch/aarch64/gic.rs` (GICv3 初始化), `src/kernel/privileged/arch/aarch64/freg/mod.rs` (SGI 7 使能) |
| **用户当前活动** | 用户 IDE 打开 `.gdb_debug_gic` 文件表明**正在 GDB 调试 GICv3 挂起** |
| **【本轮复验订正】** | 现象与源码位置仍成立；但 `.gdb_debug_gic` 文件**当前不存在**于工作区（glob 0 命中），"用户 IDE 打开 `.gdb_debug_gic`"的活动证据已失效——「用户当前调试」应以用户实际状态为准，本台账不据过时证据断言 |
| **【本轮压测复现】** | 本轮对 aarch64 启动连续压测 62 次（`./scripts/qemu_boot_test.sh aarch64`, QEMU TCG, 断言到 EL0）, **62/62 全部通过, 无一次触发 GICv3 挂起**. 结合原始现象为"偶发", 判断为 QEMU TCG 时序相关的偶发现象, 当前环境**不可稳定复现**. 挂起条目**不闭合**, 保留待 aarch64 SMP / 真机 / 更多时序场景复验. |
| **【本轮复验装备与回归防护】** | 依用户裁决「方案 B 相对完整（A + 初始化自检）」, 本轮为不可稳定复现的挂起补足**复验装备 + 回归防护**, 把"静默挂起"转化为"确定性报错"; 条目**仍不闭合**, 保留待 aarch64 SMP / 真机复验. ① [gic.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/gic.rs) — `init()` 改 `-> Result<(), &'static str>`; redistributor 唤醒等待由静默 `break` 改为超限显式 `Err`（新增自旋上限 `REDIST_WAKE_SPIN_LIMIT`）; 新增 `verify_post_conditions()` 后置条件自检（回读 `GICD_CTLR.EnableGrp1` / `GICR_WAKER.ChildrenAsleep` / `GICR_ISENABLER0` Timer PPI / `ICC_IGRPEN1_EL1`）; 修正 `GICR_CTLR` 语义 — 其 bit0 实为 `EnableLPIs`（LPI 使能）而非 redistributor 使能位, 本项目未使用 LPI, 删除误写并移除随之失效的死常量（F9）. ② [entry.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/boot/aarch64/entry.rs) — GIC 初始化失败即 fail-fast（打印原因 + `halt`）, 成功打印 `GICv3 ready` 里程碑. ③ [qemu_boot_test.sh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh) — aarch64 分支新增 `GICv3 ready` 回归断言（缺失即失败, fail-closed）. ④ 新增静态契约回归用例 [aarch64_gic_contract_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_gic_contract_test.rs)（7 用例, 固化上述语义与自检项, 防重现）. ⑤ 新增启动压测脚本 [gic_stress_test.sh](file:///home/anfer/Code/Edgine/scripts/gic_stress_test.sh)（连续启动 + 里程碑判定, fail-closed）. 说明: aarch64 当前**无 SMP**（`smp::init` 仅登记 BSP, `CPU_COUNT=1`）, QEMU 单核, 故**真多核 GIC 压测待 SMP 落地**后补. |
| **【DECISION-082 订正】** | 上段「aarch64 当前**无 SMP**（`smp::init` 仅登记 BSP, `CPU_COUNT=1`）, QEMU 单核, 故真多核 GIC 压测待 SMP 落地后补」的前提**已解除**：aarch64 SMP bring-up 已落地（[aarch64-smp-bringup.md](./archive/aarch64-smp-bringup.md)，PSCI `CPU_ON` + `ap_entry_asm` + `ap_main` + `register_cpu`），`./scripts/qemu_boot_test.sh aarch64` 已改为 `-smp 2` 并断言 `[SMP] online CPUs: 2`（fail-closed）⇒ 真多核 GIC 压测自此**具备执行载体**（`scripts/gic_stress_test.sh` 的 `-smp` 升级为后续动作，不阻塞本工程）。本条目**仍不闭合**（原始偶发挂起未定位根因），保留待真多核 / 真机复验。 |
| **【真多核压测复验（载体升级完成）】** | 载体升级已落地：[gic_stress_test.sh](file:///home/anfer/Code/Edgine/scripts/gic_stress_test.sh) 由单核改以真多核（`-smp 2`）启动, 里程碑由单一 `GICv3 ready` 升级为 `GICv3 ready` && `[SMP] online CPUs: 2` **双断言**（fail-closed）。双断言即覆盖两核 GIC —— AP 的 per-CPU GIC（`gic::init_per_cpu`）初始化失败会 halt AP, BSP 有界自旋超时 ⇒ `online CPUs: 2` 不出现, 故该断言同时承载 AP 侧 GIC 健康。**实测 50 次, 50/50 全部通过, 无一次触发 GICv3 挂起**。结论与既有「QEMU TCG 时序偶发、当前环境不可稳定复现」一致；条目**仍不闭合**（根因未定位），保留待真机 / 更多时序场景。 |
| **【有界时序搜索（RT-002 复现尝试）】** | 依用户裁决「追加有界时序搜索」, 在真多核载体上以变换时序的 QEMU 场景做**有界**复现尝试 —— 载体新增 `GIC_STRESS_SMP`（核数）与 `GIC_STRESS_QEMU_EXTRA`（附加 QEMU 参数）两个旋钮, 使搜索可复现。五场景合计 **96 次启动, 96/96 通过, 0 次触发挂起**: ① `-smp 2` × 50; ② `-smp 4` × 15; ③ `-smp 8` × 10; ④ `-smp 2 -accel tcg,thread=single`（TCG 串行化） × 15; ⑤ `-smp 2 -icount shift=6,align=off,sleep=off`（指令计数定时） × 6。结果进一步支持「QEMU TCG 时序偶发、当前环境不可稳定复现」结论; 条目**仍不闭合**（根因未定位）, 保留待真机 / 具复现样本的环境。 |
| **【本轮复验（EL0/EL1 分发分叉消除 + 真多核压测 + 根因归属订正）】** | 依用户裁决「B 相对完整: 抽取单一 dispatch」, 本轮把 aarch64 两条 IRQ 路径收敛为**唯一实现** [handle_irq](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/exception.rs#L632)（`irq_handler` / `irq_handler_el0` 改为薄包装 `handle_irq(false)` / `handle_irq(true)`）, 消除 EL0 路径曾自带裁剪分发（仅 Timer PPI + 设备 SPI, 丢弃内核 SGI 7/13/14）的**平行实现分叉**; 新增静态契约回归 [aarch64_gic_contract_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_gic_contract_test.rs) 固化"两入口共用分发 + 分发覆盖全部内核 SGI", 防分叉复发; 并补有界运行期 SGI 接收诊断（`SGI intid= origin= count=`）。复验: 六门槛全绿（双架构 0w0e、`./ci/audit.sh quick` EXIT=0、`make test-host`、`make test-kernel-host` 949/0、`TIMEOUT_QEMU=30 ./scripts/qemu_boot_test.sh all` 2/2）; [gic_stress_test.sh](file:///home/anfer/Code/Edgine/scripts/gic_stress_test.sh) 真多核（`-smp 2`）**50/50 通过, 0 次挂起**。**根因归属订正（关键）**: 本轮实证 aarch64 EL0 全程 `PSTATE.I=1`（`SPSR_EL1=0x3C0`, 见 [mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/mod.rs#L326-L328)）⇒ EL0 IRQ 入口**运行期不可达**（实测 `origin=EL0` 计数 0）⇒ 上述"SGI 丢弃"实为**潜在（不可达）缺陷**, **不可能是 RT-002 偶发挂起的实际根因**。故本条目**仍不闭合**（真根因未定位）, 保留待真机 / 具复现样本环境; 「EL0 中断不可达」另立 **ISSUE-RT-005**（见下, 本轮经用户裁定仅登记不实施）。 |
| **【本轮确定性失败分支实证（判据纯化 + host 单测）】** | 依用户裁决「判据纯化 + host 单元测试」, 把 GICv3 初始化中的**纯判定逻辑**从 MMIO / 系统寄存器访问中剥离到架构中立模块 [privileged/arch/gic_logic.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/gic_logic.rs)，使失败分支可在 host 侧**确定性构造并断言**（不再依赖不可复现的 QEMU 时序）。① **单源化**: `GICD_CTLR_ARE_MASK` / `GICD_CTLR_ENABLE_GRP1_MASK` / `GICR_WAKER_CHILDREN_ASLEEP_MASK` / `TIMER_PPI` / `REDIST_WAKE_SPIN_LIMIT` 及三个纯函数 (`uses_affinity_routing` / `wake_timed_out` / `verify_post_conditions`) 集中于此；[gic.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/gic.rs) 只负责**读寄存器**并委托本模块判定, 生产路径与 host 测试判据**同源**, 无平行实现、无 feature flag、零行为变化。② **编译门控**: 模块声明 [arch/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/mod.rs) 用 `#[cfg(any(target_arch = "aarch64", feature = "host-test"))]` —— 裸 x86_64 不编译（避 F9 死代码）, host-test 下供 host-tests 经 `edgine::kernel::privileged::arch::gic_logic` 引用。③ **确定性分支覆盖**: 新增 [aarch64_gic_logic_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_gic_logic_test.rs)（10 用例, **10/10 通过**）逐支路断言 —— ARE 模式两臂（bit4 / bit5 / 0b110000 ⇒ 亲和路由; 0 与当前实测 `0x3` ⇒ ITARGETSR legacy, **固化"当前内核未置 ARE、恒走 ITARGETSR 臂"事实**）、唤醒超限边界（0/1/上限 ⇒ 未超时; 上限+1 / `u32::MAX` ⇒ 超时）、后置条件 4 项全过 + 逐项失败文案精确比对 + 多项失败返回**首个**；既有静态契约 [aarch64_gic_contract_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_gic_contract_test.rs)（8 用例）同步改指新单源位置。④ **顺带修复预存违规**: aarch64 clippy `missing_errors_doc` 4 处（`gic.rs` `init_redistributor`/`init_per_cpu`/`init` + [psci.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/psci.rs) `cpu_on`）经用户裁定本轮修复, 补 `# Errors` 中文文档段（纯文档, 零行为变化；经 `git show` 验证 baseline 已存在, 非本轮引入）。⑤ **复验**: §2.3 六门槛全绿 —— 双架构 `./ci/build.sh all` 0w0e / clippy 四维（x86 / aarch64 / kernel_test / host-test）EXIT=0 / `./ci/audit.sh` EXIT=0 / `make test-host` 全绿 / `make test-kernel-host` 949/0 / QEMU 双架构 EXIT=0（aarch64 含 GICv3 ready + `online CPUs: 2` + KPTI-09 + 双核并发 EL0）。本条目**仍不闭合**（真根因未定位；本轮把"静默挂起"补足为"确定性报错 + 确定性分支实证"）, 保留待真机 / 具复现样本环境。 |
| **【本轮根因定位 + 统一索引模型（阶段 B）】** | 依用户裁决「推进 RT-002 排查 / B 相对完整」, 本轮把条目从"不可稳定复现、真根因未定位"推进到"**定位并消除一处确定性根因缺陷**"。① **根因（ARE 未显式置位）**: [gic.rs](../../src/kernel/privileged/arch/aarch64/gic.rs) 的 `init_distributor()` 历史上从未写 `GICD_CTLR.ARE`（仅写 `0x3` = `EnableGrp0\|EnableGrp1NS`）, **依赖平台默认值**; QEMU 强制 `ARE=1` 掩盖该缺陷, 而在不强制 ARE 的真机上内核 SGI（`TLB_SHOOTDOWN=13` / `RESCHEDULE=14` / `FREG_RECOVERY=7`, 经 `ICC_SGI1R_EL1`）与 SPI（经 `GICD_IROUTER`）分发失效 ⇒ 跨核 IPI / 设备中断偶发丢失, 表现为偶发挂起。修复按 ARM IHI 0069「ARE 仅在全 Group 使能位为 0 时可写」分步: 禁用 Distributor（`GICD_CTLR←0`）→ **显式置 ARE** → 配 SPI 分组/优先级 → 使能（`ARE\|Grp0\|Grp1`）, 每步等新增的 `wait_ctlr_rwp()`（超限 fail-closed）; 删除失效的 `GICD_ITARGETSR` 死写; `route_spi_to_cpu0` 改 64 位 `GICD_IROUTER`; `verify_post_conditions()` 增 ARE 判据（初始化后未置位即 fail-fast）。判定逻辑集中到架构中立 [gic_logic.rs](../../src/kernel/privileged/arch/gic_logic.rs), 生产路径与 host 测试同源。② **统一索引模型**: 逻辑索引由 `register_cpu` 按上线次序稠密分配（`[0, MAX_CPUS)`）, 硬件 id（x86 Local APIC id / aarch64 紧凑亲和 `(Aff3<<24)\|(Aff2<<16)\|(Aff1<<8)\|Aff0`）经 `CPU_HW_IDS[逻辑索引]` 单一映射; BSP/AP 登记路径一致化; `get_current_cpu()` 语义修正为返回逻辑索引; IPI 入口 `send_tlb_invalidate_ipi` / `send_reschedule_ipi` 改以逻辑索引为键, 内部经 `get_hw_id` 解析硬件 id 后交架构层寻址（未登记槽位静默返回）。x86_64 `send_ipi` 内 `as u8` 为**受控硬件字段窄化**（附 `#[expect(clippy::cast_possible_truncation)]` + 中文说明）, 非缺陷, 保持不变。③ **GICR_TYPER 亲和扫描**: `redist_frames()` 由线性推算（`GICR_BASE + STRIDE*cpu_index`）改为逐帧回读 `GICR_TYPER[63:32]` 的 `Affinity_Value` 匹配本核紧凑亲和, 以 `GICR_TYPER.Last`（bit4）与 `REDIST_SCAN_MAX_FRAMES=1024` 界定扫描, 未命中返回 `Err`（fail-closed）, 不再回退线性推算; `init_per_cpu()` 随之去参, BSP/AP 共用同一实现。④ **BSP_ID 收口**: 预存 write-only 静态 `BSP_ID`（仅写无读）经用户裁定本轮随改删除（与新 `CPU_HW_IDS[0]` 语义重复）, 零行为变化。⑤ **复验**: §2.3 六门槛全绿 —— `./ci/build.sh all` 5/0、`./ci/build.sh aarch64` 2/0、`bash ci/audit.sh quick` EXIT=0（FP-06 白名单外 FP/SIMD=0、SAFETY 覆盖 1872/1872、6 不变式 PASS、clippy 三维、fmt 三 crate）、host-tests 11/0、`make test-kernel-host` 949/0、`TIMEOUT_QEMU=30 ./scripts/qemu_boot_test.sh all` 2/2（aarch64 含 `GICv3 初始化成功` + `online CPUs: 2` + KPTI-09 + 双核并发 EL0 + EL0 中断可达）、`./scripts/gic_stress_test.sh 20` 20/20。**本条目根因已在代码侧消除**; 原始偶发挂起在 QEMU/TCG 仍不可稳定复现, **真机复验**（ISSUE-RT-003 载体）为最终确认手段。详见 [gicv3-rt002-rootcause-and-index-model.md](../report/gicv3-rt002-rootcause-and-index-model.md)。 |
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
| **【本轮交付（三件套）】** | 本轮交付真机验证的**工具 + 指南 + 登记**三件套, 使真机验证可在拿到硬件后一次完成: ① 介质制作脚本 [make_boot_medium.sh](file:///home/anfer/Code/Edgine/scripts/make_boot_medium.sh)（统一入口, `x86_64` 产 GRUB2 混合 ISO / `aarch64` 产整盘 FAT32 + U-Boot distro boot 介质, 含写盘四重护栏与 fail-closed）; ② 真机验证权威指南 [guide-hardware-boot.md](file:///home/anfer/Code/Edgine/docs/explain/guide-hardware-boot.md)（介质制作 / 串口 checklist / aarch64 SoC 契约与边界 / 故障排查）; ③ 本台账登记. 真机**执行**本身依赖用户提供硬件, 故状态为"交付物就绪, 执行待硬件". |

### ISSUE-RT-004: aarch64 KPTI-09 里程碑未触发

| 字段 | 数据 |
|---|---|
| **严重度** | P1 (CI full 模式硬失败) |
| **状态** | ✅ 已修复 (`[X]`) |
| **类型** | 验证里程碑缺失 (KPTI EL0 隔离断言未触发) |
| **现象** | aarch64 QEMU `-smp 2` 启动日志中 KPTI 隔离相关的两类输出 (`[KPTI] EL0 kernel high-half access denied` 与 `[KPTI] FAIL: ...`) **出现次数均为 0**；而同源用例在 x86_64 上正常 (`[x86_64] KPTI 隔离断言通过`)。 |
| **事实依据 (本次实测)** | [build/log/qemu_boot_aarch64.log](file:///home/anfer/Code/Edgine/build/log/qemu_boot_aarch64.log): `grep -ac "denied"` = **0**、`grep -ac "KPTI] FAIL"` = **0**，且 `[KPTI]` 开头的日志行总数亦为 **0**；同轮 [build/log/qemu_boot_x86_64.log](file:///home/anfer/Code/Edgine/build/log/qemu_boot_x86_64.log): `[KPTI] EL0 kernel high-half access denied` = 1。 |
| **源码位置** | [src/user/init/src/main.rs](file:///home/anfer/Code/Edgine/src/user/init/src/main.rs) 的 KPTI 探针分支 —— 探针子进程以 EL0 读内核高半区别名，**两种结局各打印其一**: 被内核终止 ⇒ 父进程打印 `... denied`; 读到值 ⇒ 子进程打印 `FAIL: ... readable`, 父进程打印 `FAIL: ... NOT denied`。故两类计数皆 0 ⇒ 该分支的打印**丢失或被跳过**（探针路径未执行, 或输出未达串口）。 |
| **独立性 (关键)** | 该现象**在 AP 崩溃修复前即存在**（次核未修复时 QEMU 在次核崩溃后仍由 BSP 续跑, 但在 aarch64 侧同样观察不到 KPTI-09 断言）—— 原先被崩溃掩盖, 属**独立于本次次核修复的预存问题**。 |
| **CI 影响 (需优先处置)** | [ci/audit.sh](file:///home/anfer/Code/Edgine/ci/audit.sh) 的 QEMU 门禁**仅在 `full` 模式以 `FAIL_OK=0` 运行**（L297-301）; [qemu_boot_test.sh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh) aarch64 分支的 KPTI-09 断言在 `FAIL_OK=0` 时置 `RESULT=1`（fail-closed）⇒ 该 `warn` 在 CI full 模式下**构成硬失败**。 |
| **建议方案** | (1) 定位探针分支打印丢失/跳过点: 探针 `fork` 是否成功、`wait_pid` 返回值分支判定、aarch64 侧异常终止是否经父进程收割返回; (2) 比对 x86_64 与 aarch64 的进程终止/收割路径差异 (aarch64 终止路径是否因新增 AP 崩溃修复而变更); (3) 若确认探针路径未执行, 检查 init `_start` 在 aarch64 上的执行流是否提前中断。 |
| **工作量** | 估计 2-4 天 |
| **根因 (已定位)** | 探针分支本身无误 —— 真正成因是**调度链路断裂导致父进程饥饿**, 使 KPTI 探针的 `fork`/`wait` 路径根本没有推进到打印点。断链自洽为**双重缺陷**: ① **D6 tick 未接线**: `scheduler_tick()` 只驱线程级 `SCHEDULER_EX.tick()`, 而 `ThreadManager::create_thread` 无调用者 (进程无对应 `Thread`), 线程级实际空转 ⇒ 进程级 CFS 记账/抢占判定/睡眠唤醒/zombie 回收/周期均衡全部不推进; ② **softirq 内调度泄漏**: D6 接线后重调度经 `Sched` softirq handler 执行, 而 `do_softirq()` 以 per-CPU `running` 标志防重入且**在主循环结束后才复位**; handler 内 `schedule()` 一旦发生上下文切换, 本核将永远停在被切出任务上, `running` 不复位 ⇒ 此后本核所有 `do_softirq()` 直接 return, 重调度路径彻底失效。实测证据: `sched_softirq cpu=0 took=true` 后 `pick cpu=0 cur=5 -> next=3` 即切走, 此后 cpu=0 再无 `sched_softirq` 输出而 `mark_resched cpu=0` 持续登记请求。x86_64 因 resched IPI 分支本就不进 `do_softirq` 而未暴露该泄漏。 |
| **修复方案** | **调度点后移到 `do_softirq()` 返回之后 (EOI 已发)** —— 统一为"中断退出路径的延迟调度点": (1) [sched_ops.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/sched_ops.rs) `scheduler_tick()` 改为驱**进程级** `SCHEDULER.tick(get_current_cpu())` (内部仍调 `SCHEDULER_EX.tick_accounting()`, 线程级记账不丢失); (2) [scheduler.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/scheduler.rs) `Scheduler::tick` 新增 idle 判据 (`per_cpu.idle == current_pid && has_runnable()`) 后仅 `mark_resched_pending_local()` 登记请求; (3) [cpu_queue.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/cpu_queue.rs) 删除 `Sched` softirq 注册/handler, 新增 `run_pending_resched()` (`take_need_reschedule()` 为真则 `SCHEDULER.schedule()`); (4) 在 aarch64 [exception.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/exception.rs) (SGI14 分支 / `irq_handler` / `irq_handler_el0` 末尾) 与 x86_64 [idt.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/idt/idt.rs) (IPI 0xFE / MSI / IRQ<16 分支) `end_of_interrupt`/`send_eoi` 与 `do_softirq()` 之后统一调用 `run_pending_resched()`. |
| **验证结果** | 六门槛全绿: 双架构 `./ci/build.sh all` 0 error/0 warning; `./ci/audit.sh quick` (clippy pedantic + 双架构 check + 全部核心审计) 通过; `make test-host` 通过; `make test-kernel-host` 949/0; `TIMEOUT_QEMU=30 ./scripts/qemu_boot_test.sh all` 2/2 通过 —— **aarch64 QEMU 双核已触发** `[KPTI] EL0 kernel high-half access denied (KPTI-09)`, 且 `online CPUs: 2`、APS-05 双核并发 EL0 (cpu=0/cpu=1) 均通过; x86_64 无回归 (KPTI-09 / APS-05 保持通过)。 |

### ISSUE-RT-005: aarch64 EL0 中断不可达（用户态不可被抢占, 与 x86_64 不一致）

| 字段 | 数据 |
|---|---|
| **严重度** | P1 (aarch64 用户态无抢占式多任务; 跨架构行为不一致) |
| **状态** | ✅ 已修复 (`[X]`) |
| **类型** | 运行时能力缺失 (用户态中断投递) |
| **现象** | aarch64 EL0 全程 `PSTATE.I=1`, IRQ 异常被屏蔽 ⇒ EL0 IRQ 向量入口 (`handle_el0_irq` → `irq_handler_el0`) **运行期不可达**; 用户态既无定时器中断也无跨核 SGI 投递, 与 x86_64 用户态 `RFLAGS.IF=1` 行为不一致。 |
| **证据 (代码)** | ① [mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/mod.rs#L326-L328) `enter_user` 置 `SPSR_EL1 = 0x3C0`（EL0t + DAIF 全屏蔽, I=1）; ② [context.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/context.rs#L175-L176) 调度恢复路径亦按 `0x3C0` 恢复目标 EL0 状态; ③ 全仓无任何位置清 SPSR 的 I 位（`grep` 0 命中）; ④ 对照 [x86_64/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/x86_64/mod.rs#L644) 用户态 `RFLAGS=0x202`（IF=1）。 |
| **证据 (实测)** | aarch64 QEMU 长跑以有界诊断观测: `origin=EL0` 计数 **0**、`TIMER IRQ (EL0)` 计数 **0**（全部 `origin=EL1h`）; 既有登记 [aarch64-high-half-migration.md](file:///home/anfer/Code/Edgine/docs/plan/archive/aarch64-high-half-migration.md#L91) 亦称 `handle_el0_irq` 在既有用例中**未被触发**。 |
| **影响** | ① aarch64 用户任务只在 syscall / 异常边界被动让出, **无时间片抢占**; ② EL0 期间到达的内核 SGI (7/13/14) 被 PSTATE 屏蔽, 延迟到本核回到 EL1 才投递（非丢失, 但引入额外延迟与不可预期性）; ③ x86_64 / aarch64 调度语义不一致。 |
| **源码位置** | [arch/aarch64/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/mod.rs#L325-L369) (`enter_user`), [arch/aarch64/context.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/context.rs#L172-L232) (恢复路径) |
| **关联** | ISSUE-RT-002（本轮据此**订正其根因归属**: EL0 路径不可达 ⇒ SGI 丢弃非其实根因）; DECISION-084 (SMP AP user scheduling) |
| **建议方案** | 进入 / 恢复 EL0 时把 SPSR 的 I 位清零（对齐 x86_64 `IF=1`）, 使 EL0 可收中断 / 被抢占; 并补 EL0 IRQ 入口回归用例（含跨核 SGI 在 EL0 期投递）。属**架构关键路径**（中断投递 / 抢占 / KPTI 再入）, 须先获用户授权并完整复跑 §2.3 六门槛。 |
| **工作量** | 估计 3-5 天 (含回归用例与六门槛复验) |
| **根因 (已定位)** | EL0 侧 PSTATE 的 **I 位单点置位**: 进入 / 恢复 EL0 的 SPSR 常量写死为 `0x3C0`（EL0t + D/A/I/F 全屏蔽）, 全仓无任何位置清 I 位。两条传播链路共享该常量 —— ① **首进入**: [mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/mod.rs) `enter_user` 直写 `SPSR_EL1`; ② **后续恢复**: 硬件异常入口保存的 `frame.spsr` → [proc_ops.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/proc_ops.rs) `proc_save_user_regs_aarch64` 写 `ctx.fs`(@112) → [context.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/context.rs) 恢复路径按 `SPSR.M[3:0]` 分派回到 EL0。下游（EL0 IRQ 入口 `handle_el0_irq` + KPTI per-CPU 槽 + 统一 `handle_irq(from_el0)` + `run_pending_resched`）**均已就绪**, 唯一缺口即该 I 位。 |
| **修复方案** | 将 EL0 侧 SPSR 由 `0x3C0` 改为 **`0x340`**（D=1,A=1,**I=0**,F=1; `0x3C0 ^ 0x340 == 1<<7` 仅差 I 位）, 使 EL0 可接收 IRQ（定时器 PPI 30 / 跨核 SGI 7/13/14）, 与 x86_64 用户态 `RFLAGS.IF=1`(`push 0x202`) 对齐。同步订正 4 处受影响注释（mod.rs / context.rs / proc_ops.rs / init.rs）, 并在 [exception.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/exception.rs) `handle_irq` 新增 **EL0 来源有界里程碑**（`IRQ: delivered from EL0 count=N`, N≤5）供 QEMU fail-closed 断言。**边界论证**: 早期 EL0 首进入时序 —— EL1 加载期仍屏蔽 IRQ, `eret` 后 PSTATE 由 SPSR=0x340 决定即开 IRQ; EL0 IRQ 重入/嵌套 —— 异常入口硬件自动置 I, 内核处理期不再开中断, 故无嵌套, `TPIDRRO_EL0` scratch 仅入口使用且用完清空。 |
| **回归测试** | [aarch64_el0_irq_reachable_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_el0_irq_reachable_test.rs) 新增 6 用例: ① `enter_user` SPSR=`0x340` 且位域自检 + `0x3C0 ^ 0x340 == 1<<7`; ② x86_64 用户态 `push 0x202`(IF=bit9) 对齐; ③ `.Lctx_enter_el0` 恢复路径从 `[x1, #112]` 载入 SPSR 且不硬编码 `0x3C0`; ④ `proc_save_user_regs_aarch64` 传播硬件 `f.spsr`; ⑤ 三文件不含遗留 `0x3C0`; ⑥ exception.rs 含 EL0 里程碑。 |
| **验证结果** | 六门槛全绿: 双架构 `./ci/build.sh all` 0 error/0 warning(`Passed: 5  Failed: 0`); `./ci/audit.sh quick`（双架构 check + clippy 三维 + fmt + 全部核心审计, 含 FP-06）全 ✓; `make test-host` 通过（新增 6 用例全 ok）; `make test-kernel-host` 949/0; `TIMEOUT_QEMU=60 ./scripts/qemu_boot_test.sh all` **2/2 通过** —— aarch64 QEMU 触发 `✓ [aarch64] EL0 中断可达 (用户态可被抢占, ISSUE-RT-005)`（串口日志含 `IRQ: delivered from EL0 count=…`）, 且 KPTI-09 / APS-05 双核并发 EL0 / TLB shootdown 均无回归; x86_64 无回归。 |
| **决策登记** | 本轮经用户裁定标的=ISSUE-RT-005、实现路径=**B「相对完整」**（架构关键路径, §12.3 前置询问授权）。 |

### ISSUE-RT-006: x86_64 APS-05 偶发未成对观测（`sys_fork` 丢失 `UserProc` 注册）

| 字段 | 数据 |
|---|---|
| **严重度** | P1 (APS-05 成对判据在 x86_64 `-smp 2` 下 fail-closed 偶发硬失败) |
| **状态** | ✅ 已修复 (`[X]`) |
| **类型** | 运行时回归 (fork 子进程未注册 `UserProc` 镜像 → 次核 KPTI 装配缺失) |
| **现象** | x86_64 `-smp 2` 下 `[SMP] EL0 pid=N cpu=M` **偶发只出现 `cpu=0`**（未成对）; 失败率约 1/2 ~ 1/10, 非确定性。aarch64 不复发。 |
| **根因 (已定位)** | [proc_ops.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/proc_ops.rs) `sys_fork` 在 `PROCESS_TABLE.insert(...)` 后**未为子进程创建 `UserProc` 镜像记录**（未调用 `user_proc_clone(parent_pid, child_pid)`）。[scheduler.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/scheduler.rs) `Scheduler::schedule` 的 per-CPU 装配块以 `USER_PROC_MANAGER.get(next)` 为门控: 未注册的子进程被投送到次核时, per-CPU 用户 CR3 滞留旧值, 其"内核栈顶页"也不在其用户页表内; 用户态被硬件中断/异常打断时, CPU 按 `TSS.RSP0` 压 5 项 iretq 帧（发生在任何软件切 CR3 之前）即 `#PF`。 |
| **证据 (实测)** | 失败日志: `migrate pid=6 -> cpu=1` 后立即 `[IDT] user exception: vec=14 err=0x2 rip=0x400030 cr2=0xFFFF800007E2FFF8`（= 子进程内核栈顶-8）→ `exit: pid=6 code=6`（被内核终止; 正常应为 code=0）。 |
| **回归来源** | 该注册块在 commit `4557cd30`（"fix(fork): 修复 fork 系统调用崩溃", COW 崩溃临时改用共享页表时重写 `sys_fork`）被**附带删除**; 其后 `4acc01f4` 又删除由此悬空的 `user_proc_clone` import。拆分前 `api.rs` 与拆分提交 `688798f1` 均含该块。 |
| **修复方案** | 在 `sys_fork` 的 `PROCESS_TABLE.insert(...)` 之后恢复注册: `if USER_PROC_MANAGER.get(parent_pid).is_some() && user_proc_clone(parent_pid, child_pid) < 0 { PROCESS_TABLE.remove_and_free(child_pid); return 0; }`（fail-closed: 注册失败回滚进程表条目）。 |
| **回归测试** | [kpti_x86_user_table_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/kpti_x86_user_table_test.rs) 新增 `test_sys_fork_registers_child_in_user_proc_manager` —— 静态断言 `sys_fork` 在进程表插入之后经 `user_proc_clone` 注册子进程, 且失败分支回滚进程表条目。 |
| **验证结果** | 六门槛全绿（双架构 `./ci/build.sh all` 5/0; `./ci/build.sh aarch64 && ./ci/audit.sh quick` EXIT=0; `make test-host` 通过; `make test-kernel-host` 949/0; `FAIL_OK=0 ./scripts/qemu_boot_test.sh all` 2/2）; **x86_64 连续 20 轮 `FAIL_OK=0` 全部通过**（20/20）, 失败日志转为正常 `migrate pid=6 -> cpu=1` → `EL0 pid=6 cpu=1` → `exit: pid=6 code=0`。 |
| **关联** | APS-05 / P6 ([smp-ap-user-scheduling.md](./archive/smp-ap-user-scheduling.md)); ISSUE-RT-004 (同属 SMP 调度链路修复链) |
| **遗留** | ✅ 已修复：退出进程的 `USER_PROC_MANAGER` 镜像记录在生产路径无回收（原 `destroy_by_pid_no_kstack` 无调用者, 每次 fork 泄漏一条）。**修复**：于权威 `Process` 销毁唯一入口 [process.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/process.rs) 的 `remove_and_free` / `dec_ref_and_maybe_free`「引用归零即释放」分支、`Box::from_raw` **之前** 调 `USER_PROC_MANAGER.destroy_by_pid(pid)`（INV-USER-PROC #2: 镜像须先于权威 `Process` 移除）；`destroy` 内用户栈释放提前到页表销毁前、取句柄与 `destroy` 锁分离（避免同锁自锁死）, 回收时释放内核栈。**回归测试**: [user_proc_reclaim_contract_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/user_proc_reclaim_contract_test.rs)。**暴露并修复** aarch64 VMM 根表裸解引用预存缺陷（见 ISSUE-RT-007）。 |

---

### ISSUE-RT-007: aarch64 VMM 页表遍历根表裸物理地址解引用（L0 未 `phys_to_virt`）

| 字段 | 数据 |
|---|---|
| **严重度** | P0 (KPTI 激活后任何经 `cr3` 的页表遍历在 EL1 即数据 abort) |
| **状态** | ✅ 已修复 (`[X]`) |
| **类型** | 预存缺陷 (aarch64 VMM 误移植; 由 ISSUE-RT-006「镜像回收」接线新暴露) |
| **现象** | aarch64 `-smp 2` 启动后进程退出回收路径调 `raw::virt_to_phys(cr3, stack_virt)` → EL1 数据 abort: `SYNC! ESR=0000000096000005 FAR=000000005FEC57F8 ELR=FFFF0000401465E8`; 随后 `[SMP] TLB shootdown` 与 `[KPTI] EL0 kernel high-half access denied` 里程碑**双双缺失**（进程继续以损坏状态运行）。修订前基线 (HEAD) aarch64 全里程碑通过, 故为本次接线触发。 |
| **根因** | [vmm_aarch64.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/vmm_aarch64.rs) 的 `translate_in_pml4` / `get_pte_value` / `set_pte_value` 三处遍历入口, 其 L0 根表以 `root_paddr as *const u64` **裸解引用物理地址**, 而各层子表帧 (`l0_entry & mask` 等) 均统一走 `phys_to_virt`。`root_paddr` 的契约是**物理**地址（调用方传 `Process.cr3` / `kernel_l0`; 同文件其余函数均 `phys_to_virt(self.kernel_l0)`）。对照 x86_64 端口 [vmm_x86_64.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/vmm_x86_64.rs#L536-L537) 正确写法为 `PhysAddr(pml4).to_virt()` —— aarch64 系误移植。内核迁至高半区且 KPTI 激活后低物理地址无映射, 裸解引用即在 EL1 翻译故障。 |
| **证据 (实测)** | 故障符号由 `build/kernel.map` 定位: `ELR=0xFFFF0000401465E8` = `Aarch64Vmm::translate_in_pml4`+8 (函数 `0x...65e0`, size `0xbc`)。创建期 `virt_to_phys` 不崩（当时低地址仍映射）, 仅在 EL0/KPTI 后崩 —— 故长期潜伏。 |
| **修复方案** | 三处 L0 改为 `phys_to_virt(root_paddr)`（与各层子表帧、同文件其余入口、x86_64 端口一致, PA→VA 换算唯一入口）。 |
| **回归测试** | [aarch64_pa_va_conversion_unify_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_pa_va_conversion_unify_test.rs) 新增 `test_vmm_aarch64_root_table_converted_to_virt_before_deref` —— fail-closed: 禁 `root_paddr as *const/*mut` 裸解引用, 且要求 ≥3 处 `phys_to_virt(root_paddr)`。QEMU 端由 KPTI-09 / TLB shootdown 里程碑承担。 |
| **验证结果** | 六门槛全绿（双架构 `./ci/build.sh all` 5/0; `./ci/build.sh aarch64 && ./ci/audit.sh quick` EXIT=0; `make test-host` 通过; `make test-kernel-host` 949/0; `FAIL_OK=0 ./scripts/qemu_boot_test.sh all` 2/2）; aarch64 日志恢复 `TLB shootdown #1..#3` 与 `KPTI ... denied`, 且仅剩 1 条预期 KPTI 探针故障 (`ESR=0x92000007`, from-lower-EL)。 |
| **关联** | ISSUE-RT-006 (触发暴露); [cr3-lifetime-ownership.md](./archive/cr3-lifetime-ownership.md); [aarch64_pa_va_conversion_unify_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_pa_va_conversion_unify_test.rs) test 6 同族（`ESR=0x96000005` level-1 fault） |

---

## 🟠 第 2 类：源码未实现 (~43 个 TODO)

> **【本轮复验订正】** 逐条回源码复验（命令：`grep -rn "TODO\|FIXME\|XXX" src/kernel --include="*.rs" | grep -v "src/kernel/functions/net/smoltcp/"`，含 smoltcp 全树共 47 行，**排除 smoltcp 后仅 2 行**）:
> - **41 项已消除**——源码仅残留 `TRACK-xxxxxx 消除/解决` 说明注释（`grep TRACK-[0-9A-Fa-f]{6}` = 18 行，全为消除说明，无活跃 `TODO(TRACK-*)`）.
> - **1 项仍存在**: ISSUE-SRC-008（`privileged/net/init.rs` 的 skb 投递 TODO），行号由原记 `:822` 漂移为 **[:601](file:///home/anfer/Code/Edgine/src/kernel/privileged/net/init.rs#L601)**；它是当前 eg 自有代码中**唯一**真实 TODO.
> - **1 项文件已不存在**: ISSUE-SRC-028 所在 `functions/fs/vfs/api.rs` 已不存在（拆分迁至 `handle.rs`）.
> - **说明**: `privileged/config/mod.rs:75` 的 `XXX` 为路径占位符（`use crate::privileged::config::XXX`），**非 TODO**，不计入.

> **【本轮修复（修遗留工程）】** ISSUE-SRC-008 已修复，第 2 类源码未实现项**全部清零**:
> - 重写 [init.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/net/init.rs) `net_rx_softirq_handler`（原空 TODO）→ 调用 `poll_network()`；触发侧新增 privileged 机制层 [net/init/irq.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/net/init/irq.rs)（ISR → `raise_softirq(NetRx)` → 底半部收包），经 `privileged::net` 顶层 re-export 暴露 0 unsafe 注册入口 `net_register_msix_isr` / `net_register_intx_isr`.
> - functions 侧接线: e1000 覆写 `handle_irq`（ICR ack）+ 探测时 `msix_enable(dev,1)` 后注册 MSI-X ISR；virtio-net 覆写 `handle_irq`（InterruptACK）+ aarch64 分支注册 GIC SPI ISR.
> - 复核结论: 第 2 类原 ~43 项 → 41 项历史消除 + ISSUE-SRC-008 本轮修复 + ISSUE-SRC-028 文件不存在 → **0 活跃 TODO**（保留 `privileged/config/mod.rs:75` 占位符说明，非 TODO）.

### 2.1 P1 严重 — 阻塞核心功能

| # | 文件:行 | 描述 | 阻塞 |
|---|---|---|---|
| ISSUE-SRC-001 | `privileged/arch/shadow_stack.rs:304` | TODO(TRACK-4C9A12): 使用 PMM 分配实际物理页 | shadow stack 实现 |
| ISSUE-SRC-002 | `privileged/sgeg/secure_boot.rs:198` | TODO(TRACK-7A8BAB): 替换为真正的 Ed25519 验证 | 安全启动 |
| ISSUE-SRC-003 | `privileged/idt/safety.rs:25` | TODO(TRACK-2B3C56): 完整实现 CPUID 解析 | CPU 特性检测 |
| ISSUE-SRC-004 | `privileged/driver/power.rs:148` | TODO(TRACK-6F7A9A): 实现真正的 S3 挂起 | 电源管理 |
| ISSUE-SRC-005 | `privileged/driver/uefi.rs:264` | TODO(TRACK-4D5E78): 实际解析 EFI_SYSTEM_TABLE | UEFI 启动 |
| ISSUE-SRC-006 | `privileged/driver/uefi.rs:435` | TODO(TRACK-5E6F89): 调用 EFI_RUNTIME_SERVICES.SetTime | UEFI 时间服务 |
| ISSUE-SRC-007 | `privileged/driver/usb/xhci.rs:670` | TODO: 实现 Event Ring 处理 | xHCI USB 驱动 |
| ISSUE-SRC-008 | `privileged/net/init.rs:822` | TODO: 待 NAPI/中断驱动模式启用后, 此处实现 skb 投递到 smoltcp | **关联 ISSUE-RT-001** |
| ↳ **【本轮修复（修遗留工程）】** | `privileged/net/init.rs:601` | **已修复**: `net_rx_softirq_handler` → `poll_network()`；触发侧 privileged 新增 ISR→softirq 机制 + functions 侧 e1000/virtio-net 接线 ISR | — |
| ISSUE-SRC-009 | `privileged/timer/tickless.rs:237` | TODO(TRACK-3C4D67): 集成 hrtimer 获取最近到期时间 | tickless 模式 |
| ISSUE-SRC-010 | `functions/io/iouring.rs:314` | TODO(TRACK-8B9CBC): 集成 VFS fd 表 | io_uring VFS 集成 |
| ISSUE-SRC-011 | `functions/io/iouring.rs:319` | TODO(TRACK-9CADCD): 实现网络异步操作 | io_uring 网络 |
| ISSUE-SRC-012 | `functions/io/iouring.rs:323` | TODO(TRACK-ADBECDE): 实现超时等待 | io_uring 超时 |
| ISSUE-SRC-013 | `functions/io/iouring.rs:470` | TODO(TRACK-BECFEF): 实现缓冲区注册 / 文件注册 | io_uring 完整功能 |
| ISSUE-SRC-014 | `functions/ipc/sem.rs:102` | TODO(TRACK-21BAF1): 阻塞当前线程到 wait 队列 | semaphore 阻塞语义 |
| ISSUE-SRC-015 | `functions/ipc/signal.rs:84` | TODO(TRACK-48CC21): 将处理函数注册到 SignalPending | 信号注册 |
| ISSUE-SRC-016 | `functions/ipc/signal.rs:135` | TODO(TRACK-3A9016): 实现完整的信号分发逻辑 | 信号分发 |

### 2.2 P2 中等 — 部分功能缺失

| # | 文件:行 | 描述 |
|---|---|---|
| ISSUE-SRC-017 | `privileged/dma/engine.rs:426` | TODO(TRACK-1F2A45): 由 DmaStream 的 coherent 属性决定 |
| ISSUE-SRC-018 | `privileged/arch/shadow_stack.rs:540` | TODO(TRACK-6E7C34): 使用 #GP 异常处理来安全检测 |
| ISSUE-SRC-019 | `functions/driver/power.rs:355` | TODO(TRACK-7A3B01): 实际写 MSR/寄存器调整频率和电压 |
| ISSUE-SRC-020 | `functions/ipc/scheduler_integration.rs:103` | TODO(TRACK-8C5FFB): 实现基于定时器的超时等待 |
| ISSUE-SRC-021 | `functions/ipc/signal.rs:106` | TODO(TRACK-614BD5): blocked 位图设置 |
| ISSUE-SRC-022 | `functions/ipc/signal.rs:126` | TODO(TRACK-F806F4): blocked 位图清除 |
| ISSUE-SRC-023 | `functions/proc/oomd.rs:94` | TODO: 实际发送 SIGKILL 到最大 RSS 进程 |
| ISSUE-SRC-024 | `functions/proc/memfd.rs:58` | TODO: 使用 per-process fd 表 |
| ISSUE-SRC-025 | `functions/proc/memfd.rs:75` | TODO: 设置 fd 的 CLOEXEC 标记 |
| ISSUE-SRC-026 | `functions/proc/pidfd.rs:61` | TODO: 需要 Task 4 (OpenFile 系统) 完成后实现 |
| ISSUE-SRC-027 | `functions/io/iouring.rs:319` | (重复 ISSUE-SRC-011) |
| ISSUE-SRC-028 | `functions/fs/vfs/api.rs:218` | TODO: 使用 per-process fd 表 |
| ISSUE-SRC-029 | `functions/net/smoltcp_impl.rs` (多个) | smoltcp 集成相关 |
| ISSUE-SRC-030 | `functions/sgeg/sessions.rs` (多个) | 凭据会话管理 |
| ISSUE-SRC-031 | `functions/sgeg/grants.rs` (多个) | 凭据授权 |
| ISSUE-SRC-032 | `functions/sgeg/policy.rs` (多个) | 凭据策略 |
| ISSUE-SRC-033 | `functions/freg/attribution.rs` (多个) | 故障归属 |
| ISSUE-SRC-034 | `functions/freg/recovery_policy.rs` (多个) | 故障恢复 |
| ISSUE-SRC-035 | `functions/freg/cascade.rs` (多个) | 故障级联 |
| ISSUE-SRC-036 | `functions/debug/ebpf_verifier.rs` (多个) | eBPF 验证器 |
| ISSUE-SRC-037 | `functions/driver/display/dp.rs` (多个) | DisplayPort 协议 |
| ISSUE-SRC-038 | `functions/driver/storage/*` (多个) | 存储驱动 |

### 2.3 P3 轻微 — 时间戳更新

| # | 文件:行 | 描述 |
|---|---|---|
| ISSUE-SRC-039 | `functions/fs/unkfs/unkfs_inode.rs:92` | TODO: 未来可接入 UNKFS 时间戳更新 |
| ISSUE-SRC-040 | `functions/fs/ext2/mount.rs:102` | TODO: 未来可接入 ext2 inode 时间戳更新 |
| ISSUE-SRC-041 | `functions/fs/exfat/mount.rs:79` | TODO: 未来可接入 exFAT 目录项时间戳更新 |
| ISSUE-SRC-042 | `functions/fs/overlayfs.rs:205` | TODO: 未来可实现 copy-up + 时间戳更新 |
| ISSUE-SRC-043 | `functions/fs/devfs.rs` | (类似时间戳项) |

> **说明**: P2/P3 的具体位置可通过 `grep -rn "TODO\|FIXME\|XXX" src/kernel --include="*.rs" | grep -v "src/kernel/functions/net/smoltcp/"` 重新生成. eg 自有代码中**约 43 个 TODO** (排除 smoltcp vendored 527 个).
>
> **【本轮复验订正】** 上表为 **2026-08-09 原登记快照**（保留不涂改）；按当前源码状态复验，上表中除 ISSUE-SRC-008 外的条目均已消除，ISSUE-SRC-028 路径已不存在——详见本节开头【本轮复验订正】. 重新生成命令实测（排除 smoltcp）仅 2 行（1 占位符 + 1 真 TODO），非"约 43 个".
>
> **【本轮修复（修遗留工程）】** 上述残留的 1 项真 TODO ISSUE-SRC-008 本轮已修复；复验上表条目**全部结案**，eg 自有代码 0 活跃 TODO（详见本节开头【本轮修复（修遗留工程）】）.

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
| **冲突点** | `privileged/syscall/mod.rs:24-35` 称 "0-299 保留给未来 linuxulator" vs `ref-naming.md §三` 称 "直接使用 Linux syscall 编号" |
| **落地** | DECISION-037 (2026-08-03). 2026-09-26 复验: `privileged/syscall/mod.rs:22-30` 注释已统一为 "0-299 直接 Linux ABI; 500+ 为 EG_* 自由扩展"; `vision-hope.md` 已整篇重写, 原 linuxulator"风险 2"节不复存在 |
| **归档快照** | `archive/code-review-findings-2026-08-01.md` 保持 `[]` (AGENTS.md §6 冻结) |

#### REVIEW-FINDING-026: privileged 反向依赖 functions 类型 (userctx.rs re-export)

| 字段 | 数据 |
|---|---|
| **严重度** | P1 (违反 framekernel 单向数据流) |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | `privileged/userctx.rs:6-9` re-export functions 类型, `privileged/usermode.rs:38/58` 直接读取其字段 |
| **落地** | DECISION-039 (2026-08-03, A 方案). 2026-09-26 复验: `privileged/userctx.rs:28/55` 为两处 `#[repr(C)] UserContext` (x86_64/aarch64 cfg 分支) 正规定义; `functions/userctx.rs:11` 反向 `pub use crate::privileged::userctx::*;`. 单向数据流已恢复 |
| **归档快照** | `archive/code-review-findings-2026-08-01.md` 保持 `[]` (AGENTS.md §6 冻结) |

### 3.2 P2 文档失实 (3 项) — ✅ 已修复 (2026-09-26 源码复验)

#### REVIEW-FINDING-027: privileged 顶层文档声明 "~3000+ LoC" 严重失实

| 字段 | 数据 |
|---|---|
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | `privileged/mod.rs:10` 声明 `~3000+ LoC`, 实际 ~10 万行 |
| **落地** | progress B1 (2026-08-04). 复验: `privileged/mod.rs:10` 现为 `//! privileged/ (TCB, unsafe 允许)`, 无 LoC 数字 |

#### REVIEW-FINDING-028: functions/net + functions/fs 头注释过期

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

#### REVIEW-FINDING-030: privileged/sched task 抽象 Phase 1.4.2 未开工

| 字段 | 数据 |
|---|---|
| **严重度** | P3 |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **描述** | `privileged/sched/mod.rs:8` 注释: "task 抽象在 Phase 1.4.2 计划中但尚未实现", 阻塞 functions/proc 迁移 |
| **落地** | progress 阶段 5 (2026-08-04). 调研发现 `sched_trait.rs` 中 Task 抽象 (struct Task + 10 属性方法 + Scheduler trait + EdgineScheduler 委托) 早已完整实装, 仅 mod.rs 注释过期, 已修复 |

#### REVIEW-FINDING-031: IoMem 边界 expect panic + 固定上限硬编码

| 字段 | 数据 |
|---|---|
| **严重度** | P3 |
| **状态** | ✅ 已修复 (2026-09-26 复验) |
| **冲突点** | `iomem.rs:194/200/206/212` expect panic + `MAX_MMIO_MAPPINGS = 64` 硬编码 |
| **落地** | progress B6 (2026-08-04). 复验: `iomem.rs` 8 个 read_u*/write_u* 均带 `debug_assert!` 前置; 3 个 TCB 容量常量已集中至 `privileged/constants/limits.rs` |

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
| **状态** | ✅ `[X]` 结案（保留"维持原状不迁移"的裁定；其"未来可选"迁移路线已被 DECISION-080 吸收） |
| **数量** | 1354 个跨 166 文件 (含 527 个 smoltcp vendored) |
| **【本轮复验订正】** | 实测 kernel `#[test]` 现为 **1406 处 / 173 文件**（原记 1354/166 随代码增删漂移）；维持原状的理由（ROI 不匹配）不受影响 |
| **理由** | 范畴属测试架构工程非静态检查工程; ROI 不匹配 (仅 ~70 个纯算法值得迁移) |
| **commit** | `ebb985c0` |
| **未来可选** | ~~迁移 USB HID/MassStorage/XHCI/Enumerate/Ring 5 文件 ~70 个纯算法测试 (~5-7 天)~~ **【已被 DECISION-080 吸收，不再作为开放选项】** |
| **【本轮结案】** | 依用户裁决「按 DECISION-080 结案」，与 [kernel-unit-test-harness-unification.md](./archive/kernel-unit-test-harness-unification.md) DECISION-080 裁定对齐：**纯逻辑（可在 host 编译）测试的唯一归属 = 源文件 `#[cfg(test)]`**。USB 5 文件 69 例（`xhci` 4 / `mass_storage` 19 / `usb_core` 5 / `ring` 11 / `hid` 18 / `enumerate` 12）**当前已全部位于源文件 `#[cfg(test)] mod tests`**，经 §2.3 门槛 6 `make test-kernel-host` 实际执行 ⇒ **已处于 DECISION-080 目标态**，无需迁移。原「未来可选：迁移到 host-tests」与 DECISION-080 终态方向相反，且 `ring.rs` 等用例直接断言私有字段（`trbs`/`enqueue_index`/`dequeue_index`/`cycle`），迁往独立 crate（仅见 `pub`）必致断言弱化（与 DECISION-080 第 4 条登记的「覆盖弱化」同型）；故该路线作废。**无代码改动**。 |

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

## ⚫ 第 6 类：构建/工具问题 (6 项)

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
| **【本轮复验订正】** | 建议方案的第 2 条已实装: [qemu_boot_test.sh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh#L133) 新增 `check_kernel_fresh()`（`:133` 定义），当图像早于源码时告警/重建；但**仅 aarch64 分支调用**（`:204`），x86_64 路径未接入。Makefile 侧 mtime 检查仍未做 → 状态由 ❌ 上调为 ⚠️ |
| **【本轮修复（修遗留工程）】** | x86_64 分支已接入 `check_kernel_fresh`（[qemu_boot_test.sh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh) x86_64 段 `sync_make_state` 后新增 `check_kernel_fresh \|\| true`），与 aarch64 分支口径一致 — QEMU 侧双架构陈旧检测已闭环。Makefile 侧 mtime 检查仍未做（保持不做，QEMU 脚本入口已覆盖实际触发场景）。 |
| **工作量** | 估计 0.5 天 |

### ISSUE-TOOL-003: cargo test --tests 在裸机 target 失败

| 字段 | 数据 |
|---|---|
| **状态** | ✅ 已结案 (前提已变; E0152 整族根治后原建议无必要, 重评不施工) |
| **现象** | E0152 duplicate lang item (zerocopy/bitflags/byteorder/managed) |
| **应对** | 实际测试在 host-side 跑, lint-only 检查通过 |
| **建议方案** | 用 `#[cfg(target_os = "none")]` 隔离测试, 或 host-tests 引入独立测试目标 |
| **【本轮复验订正】** | E0152 整族已根治（2026-09-14 build-std 显式化，见 [src/kernel/Cargo.toml:17](file:///home/anfer/Code/Edgine/src/kernel/Cargo.toml#L17) 与 [framekernel-paradigm-enforcement.md:431](file:///home/anfer/Code/Edgine/docs/plan/framekernel-paradigm-enforcement.md#L431)）；`test = false` 相关前提亦已移除。原"裸机 target 失败"现象不再必然复现 → 本条前提已变，待按新状态重评是否需要 |
| **【本轮修复（修遗留工程）】** | 重评结案: E0152 整族根治后, 内核测试实际一直在 host 侧（`--features host-test`）运行并全绿（`make test-kernel-host` 941 passed），《`test = false` 隔离 host target》的原建议已无必要 — 无需再引入 `#[cfg(target_os = "none")]` 隔离或独立 host 测试目标。本条**结案, 不施工**。 |
| **工作量** | 估计 1 天 |

### ISSUE-TOOL-004: x86_64 AP 启动锁无竞争者 → deadlock 审计基线唯一 HIGH

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复（本轮登记即修复；`audit_deadlock_matrix.py` 基线首次清零） |
| **现象** | `audit_deadlock_matrix.py` 全仓唯一 HIGH `NON_IRQ_SAFE_LOCK_USE`：`privileged/arch/x86_64/smp_init.rs` 的 `AP_STARTUP_LOCK`。该项自审计基线建立起长期以“预存人工审查项”挂在各轮验证记录里（见 [framekernel-paradigm-enforcement.md](./framekernel-paradigm-enforcement.md) 审计盲区扩展记录与阶段 6 验证记录、[syscall-followup.md](./syscall-followup.md) 验证行），无法闭合 |
| **根因** | **锁无任何竞争者**。源码调研事实链：`start_ap` 全仓唯一调用点是同文件 `init()` 内对 MADT AP 列表的**串行循环**；`init()` 的唯一外部入口是 `Arch::interrupt_late_init()`（x86_64 `mod.rs`），仅由 BSP 在 `kernel_main` 调用一次；AP 侧走 `ap_entry`，不进入启动路径。脚本无抑制/白名单机制（fail-closed），面对“无竞争者但持非 IRQ 安全锁”只能保守报 HIGH。附带缺陷：`spin::mutex::SpinMutex::lock()` **不做** IRQ save，而临界区包含 INIT 10ms + 就绪 100ms + 完成 50ms 的忙等轮询（≈ 150ms/AP） |
| **审计表述订正** | 原 F-12 审计条目（[code-audit-final-summary.md](./code-audit-final-summary.md)）称“`AP_STARTUP_LOCK` 是 `SpinMutex<()>`，实现是 irq_spinlock，**lock() 时 save/restore IRQ**” — **不成立**；`spin` crate 只有 `lock_irqsave()` 才保存/恢复标志位，`lock()` 不碰 IF。该错判已原地订正 |
| **修复** | 用户裁定“删除 `AP_STARTUP_LOCK`”：删除锁声明与 `use spin::mutex::SpinMutex`，原地写调用图事实注释与重引条件（若引入 CPU hotplug，重引的锁必须是 `IrqSpinLock` 且忙等轮询须移出临界区）。复跑 `audit_deadlock_matrix.py`：`扫描文件数: 693 / 问题总数: 0` |
| **回归守卫** | 新增静态契约用例 [arch_ap_startup_lock_removed_test.rs](../../host-tests/tests/arch_ap_startup_lock_removed_test.rs)（6 例：锁彻底移除 / `start_ap` 体内无锁且无显式 `cli`/`sti` / 调用点唯一 / `ap_entry` 不启动 AP / 启动入口仅 BSP `interrupt_late_init` / 前瞻守卫“重引启动锁必须 IRQ 安全”）；已做负控制（临时加回锁 → 2 例 FAILED），非自证式断言 |
| **工作量** | 已闭合 |

### ISSUE-TOOL-005: check_doc_paths 将历史快照文档误入“路径必须存在”校验

| 字段 | 数据 |
|---|---|
| **状态** | 🔄 已修复（本轮登记即修复；订正对象是**校验工具的范围语义**，未改动任何文档散文） |
| **现象** | `tools/check_doc_paths.py` 报 368 条“文档路径漂移”（反引号源码路径磁盘上不存在），作为无法闭合的噪声长期存在 |
| **根因** | 校验范围把 `docs/plan/`（archive 272 + 活文档 96，集中在 9 份 plan）与 `docs/report/` 当作“路径必须与代码同步”的文档集合。plan 条目记录的是**执行当时**的代码形态（批次目标文件 / 实测行数 / 已下沉或已删除模块），其路径按定义会随后续批次失效。对 96 处逐条核后可确认**全部为历史记录**（无一条错指现存文件），关键词判据只能覆盖 51/95，机械替换会篡改历史（违 AGENTS.md §6）；§9.2 要求 plan 同步的是状态标记 `[]/[X]`，不是历史路径文本 |
| **修复** | 脚本收窄为校验“必须与代码同步”的文档集合：`docs/explain/` + `docs/design/` + 根文档（`AGENTS.md` / `README.md` / `README.en.md` / `docs/README.md`）；`docs/plan/`（含 archive）与 `docs/report/` 排除，排除理由写入脚本 docstring |
| **实测** | 收窄前全仓报 368 条 → 收窄后 `OK: 无文档路径漂移 (校验 14 份必须与代码同步的文档)` EXIT=0。反向验证：被纳入范围的 14 份活文档本身漂移 **0** → 收窄不是掩真实问题，而是删除范畴错误 |
| **建议方案** | （已实施）AGENTS.md §6 补写“代码同步范围（源码路径新鲜度）”条款，把该语义固化为文档规则。工具依用户裁定**保持按需运行，不接入 CI 门禁**（不新增 §2.2 核心审计表行） |
| **工作量** | 已闭合 |

### ISSUE-TOOL-006: 本地一键 CI 的 rustdoc 腿在 HEAD 上即红，且不在 §2.3 六门槛内

| 字段 | 数据 |
|---|---|
| **状态** | ✅ 已修（第七轮，用户裁 R-1 乙：error + private warning 一并收口，判据面加压到含 `rustdoc::private_intra_doc_links`。施工记录与 ③类 25 符号台账见 [ci-rustdoc-closeout.md §4](./ci-rustdoc-closeout.md)；序列指针 [net-e2e-tcp-echo-flakiness.md §6 P-39](./net-e2e-tcp-echo-flakiness.md) 已转 `[X]`。该册仍开放 R-6/R-7 两条，均属需用户裁定的新裁定项） |
| **现象** | `make -f Makefile.ci ci-rustdoc` 失败（RC=101）。判据 = `RUSTDOCFLAGS="-D rustdoc::broken-intra-doc-links"` 下的双架构 `cargo doc --no-deps` |
| **实测分布** | x86_64 腿 **30 error + 19 warning**，涉 **19 个文件**：`privileged/mm/pmm.rs` 11 / `functions/driver/usb/xhci.rs` 4 / `privileged/mm/mod.rs` 3 / `vmm_x86_64.rs`·`functions/proc/process_vm.rs`·`functions/driver/char/mod.rs` 各 2 / 其余 13 文件各 1。两类形态 = `unresolved link`（`` `idx` `` / `` `24` `` / `` `gs:0` `` 等被反引号包裹却无对应可解析项）与 `links to private item`（pub API 文档指向私有符号）**（第七轮两腿都真跑后的订正：x86_64 = 29 `unresolved link` error + 16 private warning + 3 条无源码位置 warning，aarch64 = 28 + 33 + 3；立案计数的第 30 条 error 是 cargo 收尾行 `could not document`，且当时 aarch64 腿因串联中断从未执行。归一后私有链接 = 36 站点 / 18 文件 / 25 符号）** |
| **与本轮改动的关系** | **无关**。本轮 Rust 侧只改 `src/kernel/lib.rs` 与 `functions/wasm/mod.rs` 的目录树注释，二者均不在失败名单；19 个失败文件本轮一个未碰。为验证该两处注释改动才首次实跑本腿，于是暴露历史红 |
| **放大后果** | ① 同一 recipe 串联，x86 先红即中断 ⇒ aarch64 腿**从未被执行**；② §2.3 六门槛不含 rustdoc ⇒ 历轮"六门槛全绿"的验收里，文档链接这一路一直是坏的；③ 对应 workflow（`ci-lint.yml` rustdoc-check）同样会红，只是它在 Gitee remote 上从不被触发（见 P-37），红也没人看见 |
| **门禁性质判定** | 本腿**不是** fail-open：`Makefile.ci` 有 `SHELL := /bin/bash -o pipefail`，`| tee | tail -5` 不掩盖退出码，报的是真红。问题在于"该门禁长期红，却不在验收集合内" |
| **建议方案** | 独立文档卫生工程。每处三选一定型：① 改为可解析的真实路径；② 去反引号/改写使不成链接；③ 把被引符号升 `pub` 并配中文文档注释（§5 F8）—— 第 ②/③ 之间是 API 可见性判定，不能由工具代答。深度：甲 = 只清 30 error 使门禁转绿；乙 = 连 19 warning 一起收口并把 `-D` 面加压到 warning 级 |
| **工作量** | 估 0.5–1 天（视甲/乙深度） |

---

## 🟢 第 7 类：lint 副作用 (2 项已修复)

| # | 描述 | 修复 commit |
|---|---|---|
| LINT-FIX-001 | plan_b_inode_test `vfs_read_uses_inode_trait` brittle substring 失效 (rustfmt 拆行) | `4f1a9d3e` |
| LINT-FIX-002 | plan_b_inode_test `vfs_write_uses_inode_trait` brittle substring 失效 (同上) | `4f1a9d3e` |

---

## 🟤 第 8 类：迁移中子系统状态（driver / egdf）

> 2026-08-31 追加：记录 driver / egdf 两个"迁移中"子系统的完整状态——历史脉络、当前形态、遗留事项。
> 触发：用户要求"记录有关迁移中的完整信息（driver 和 egdf 等）"。
> 来源：源码头注释（functions/driver/mod.rs + functions/egdf/mod.rs）+ [archive/driver-service-migration.md](./archive/driver-service-migration.md) + [archive/audit-fix-04-framework-net-drivers.md](./archive/audit-fix-04-framework-net-drivers.md) B04-19/D6。
> **关键认知**：迁移方向经历过一次反转——Phase 2.1/2.4 原方向为"privileged → functions"（业务逻辑迁往 functions 做成 safe API）；B04 审计（2026-08-24/25）纠正为"机制留 privileged + functions 安全代理"（E1000 反向回迁）。因此"未迁移"≠"待迁移到 functions"，多数模块的当前形态是**有意维持的中间态**。

### 8.1 迁移历史脉络（三阶段）

| 阶段 | 时间 | 方向 | 内容 |
|---|---|---|---|
| Phase 2.1 driver 迁移 | 2026-06 → 2026-07-22 | privileged → functions | 6/6 子系统迁移完成，统一 5 步路径（MMIO→`IoMem` / PIO→`IoPort` / DMA→`DmaStream` / IRQ→`IrqLine` / 暴露 safe API）；privileged -2317 行 / -37 unsafe。见 [archive/driver-service-migration.md](./archive/driver-service-migration.md) |
| Phase 2.4 net/egdf 迁移 | 2026-06-04 | privileged → functions | egdf mod + devtree + composite 迁移；net 独立迁往 functions/net |
| B04 审计反向纠正 | 2026-08-24/25 | functions → privileged | **B04-19**（F3 双向依赖）：E1000Driver/E1000Io 整体上移 `privileged/driver/net/e1000_io.rs:364-760`，`functions/driver/net/e1000.rs` 变 41 行纯 re-export shim；**D6/DECISION-062** 与 B04-09 合并理顺 privileged net/driver 边界 |

### 8.2 functions/driver 当前状态（三种形态）

| 形态 | 模块 | 位置 | 说明 |
|---|---|---|---|
| **A 完整安全代理**（业务逻辑在 functions） | char/vga + char/serial | `functions/driver/char/` | VGA 文本模式 + 16550 UART，经 IoMem + IoPort |
| | display/ddc + hdmi + dp | `functions/driver/display/` | DDC/HDMI/DP 业务逻辑（EDID 解析/时序/像素时钟），dp 已从 privileged 完全迁出（privileged 侧无 dp.rs） |
| | storage/nvme + ahci | `functions/driver/storage/` | 队列管理/命令提交/读写在 functions，DMA 走 privileged safe wrapper |
| | virtio/transport + blk + net | `functions/driver/virtio/` | VirtIO MMIO Transport + 块/网驱动 100% safe |
| | usb/xhci | `functions/driver/usb/xhci.rs` | Capability/Operational/Port/Doorbell 安全访问 |
| | power + acpi | `functions/driver/` | power：T4-4 策略主体（2026-06-16 从 privileged 提取）；acpi：x86_64 安全代理（aarch64 编译为 0 内容） |
| **B 纯 re-export shim**（逻辑上移 privileged） | net/e1000 | `functions/driver/net/e1000.rs` | B04-19 后 E1000Driver → `privileged/driver/net/e1000_io.rs`，仅留业务常量 + 描述符 re-export |
| | usb/enumerate + hid + ring + usb_core + mass_storage | `functions/driver/usb/` | 全部 `pub use privileged/driver/usb/*`（privileged 侧为 0 unsafe 模块） |
| | uefi + kexec + firmware | `functions/driver/` | D5/D10/D11 安全封装 |
| **C 桩模块** | storage/ata | `functions/driver/storage/ata.rs` | 仅类型/常量，实际 ATA 逻辑保留 privileged（IoPort 需 unsafe） |

### 8.3 functions/egdf 当前状态

| 模块 | 状态 | 说明 |
|---|---|---|
| mod（注册表/查找/块/字符/输入 IO） | ✅ 已迁移 | `functions/egdf/mod.rs`，强类型 DeviceId/Proto/DeviceState，封装 `privileged::egdf` |
| devtree | ✅ 已迁移 | `functions/egdf/devtree.rs`，DevTreeNodeId 强类型 + DevTreeError |
| composite | ✅ 已迁移 | `functions/egdf/composite.rs`，仅暴露探测入口（RAID 复合设备） |
| proto_*（proto_block/char/input/net） | ✅ 已迁移 | `functions/egdf/proto.rs`，强类型安全代理（去裸指针：`NetDevice` 持有 `&'static NetOps` + `driver_data: *mut u8`，经 `find_net_device` 返回；`input_read`/`input_has_data` 薄封装；block 侧 `register_block_device`/`unregister_block` 直通 re-export） |
| user_driver | ✅ 已迁移 | `functions/egdf/user_driver.rs`，强类型 `UserDriverError`（6 项 privileged `ERR_*` + `Unknown(i32)`）+ `UserDriverResult<T>` + `to_errno` POSIX 映射；`bind`/`unbind`/`map`/`unmap`/`forward_irq` 经 `map_err` 收敛错误类型 |

### 8.4 遗留事项

| # | 事项 | 位置 | 建议 |
|---|---|---|---|
| MIG-001 | 头注释过时：functions/driver/mod.rs 声称"⏳ 5/6 未迁移" + E1000 "138 行"（2026-06-04 状态表） | [functions/driver/mod.rs:4-35](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/mod.rs#L4-L35) | 同步为实际状态：多数模块已迁（A 形态）+ B04 后 E1000 为 re-export shim（B 形态） |
| MIG-002 | 头注释过时：functions/egdf/mod.rs 声称"已完成 1/4 子系统迁移" | [functions/egdf/mod.rs:4-11](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/mod.rs#L4-L11) | devtree/composite 实际已迁（devtree.rs 自标 3/4、composite.rs 自标 4/4），更新为"3/5" |
| MIG-003 | 缺 pl011（ARM 串口）安全代理 | `functions/driver/char/` | ✅ 已结案（【本轮修复（修遗留工程）】完整迁移到 functions：privileged 删 pl011.rs、暴露 `pl011_phys_base()` safe 面 + `IoMem::from_platform_device` safe 构造器；functions 新建 pl011.rs 0 unsafe 并经 egdf 注册） |
| MIG-004 | 缺 proto_* + user_driver 安全代理 | `functions/egdf/` | ✅ 已结案（【本轮修复（修遗留工程）】用户裁决「MIG-004 建安全代理（相对完整）」：新建 [proto.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/proto.rs) + [user_driver.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/user_driver.rs)，去裸指针 + 强类型错误映射，现有调用点改走 functions 封装） |
| MIG-005 | 双份代码/边界未理清 | `privileged/driver/storage/` ↔ `functions/driver/storage/` | nvme/ahci 两边并存（privileged 含 nvme_block.rs/ahci_block.rs/ata_block.rs 完整实现 + functions 业务层）；需明确"机制/策略"各自归属，消除重复 |
| | | `privileged/driver/display/hdmi/` ↔ `functions/driver/display/hdmi.rs` | privileged 侧孤儿目录（8 文件 1537 行）已随 DECISION-K 第二十七批删除，双份消解；functions 侧残留缺口（控制器未接入启动路径）转记 MIG-008 |
| MIG-006 | 迁移方向变化未入文档 | [archive/driver-service-migration.md](./archive/driver-service-migration.md) | 文档标记"✅ 已完成"但 B04 后方向反转（E1000 回迁 privileged），需补注 B04 后的状态 |
| MIG-007 | host-tests 无 egdf 专项集成测试 | `host-tests/tests/` | 已有 driver_display / driver_e1000_eeprom / nvme_ahci_activation / i43_block_bridge / virtio_net_arch_unify / nic_probe_arch_neutral，但 egdf 注册表/IO 无专项覆盖 |
| MIG-008 | functions 侧 HDMI/DP 控制器未接入驱动框架/启动路径 | [functions/driver/display/hdmi.rs:699](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/display/hdmi.rs#L699) / [dp.rs:500](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/display/dp.rs#L500) | `HdmiController`/`DpController` 内核内零调用者（仅模块内测试与 host-tests）；显示子系统启动实际走 privileged framebuffer（[privileged/driver/display/mod.rs:299](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/display/mod.rs#L299) `display_init()`）。TMDS 输出使能与同步极性在 functions 已实现（[hdmi.rs:56-58](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/display/hdmi.rs#L56-L58) 0x079/0x078），DP OUTPUT_ENABLE 亦已实现（[dp.rs:1188](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/display/dp.rs#L1188)）；缺失项为驱动注册/工厂接入路径，及厂商 PHY/DPLL 差异（Intel/AMD/Synopsys）实装——后者待真实硬件接入时按 DECISION-K 注册契约在 functions 侧重立 |

> **【本轮复验订正】** 逐项复验（保留原表不涂改）:
> - **MIG-005 文件名漂移**: privileged 侧实际为 [ata.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/storage/ata.rs)/[nvme.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/storage/nvme.rs)/[ahci.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/storage/ahci.rs)/ata_block.rs，**无** `nvme_block.rs`/`ahci_block.rs`；"双份代码/边界未理清"的结论仍成立.
> - **MIG-001/002/003/004/006/007/008 仍成立**: [functions/driver/mod.rs:4-35](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/mod.rs#L4-L35) 与 [functions/egdf/mod.rs:4-11](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/mod.rs#L4-L11) 头注释仍为 2026-06-04 旧状态；pl011/proto_*+user_driver 安全代理仍缺；迁移文档未补注 B04；egdf 无专项 host-tests；functions 侧 HDMI/DP 控制器仍零调用者.
>
> **后续行动建议**：MIG-001/002 为纯注释同步（低风险，可随下次 driver 改动顺手修复）；MIG-003/004 属功能补齐（需按 §12.3 评估"是否需要"——若当前无调用方，登记即可不施工）；MIG-005/006/007 属架构边界治理（涉及 privileged/functions 归属决策，按 AGENTS.md §12.1 决策灰色地带处理，需用户裁决）；MIG-008 属接线补齐（控制器与 TMDS/DP 输出使能已实现，仅缺驱动注册/工厂接入面，需驱动注册面决策）。
>
> **【本轮修复（修遗留工程）】** MIG-001/002/006/007 已结案:
> - **MIG-001/002 注释同步**: [functions/driver/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/mod.rs#L1-L22) 头注释由旧状态表（"Phase 2.1 在途" + E1000 "影子双份"）改写为当前形态（驱动业务层落 functions、0 unsafe；privileged 保留 MMIO/DMA 环/存储 wire/PL011 等**机制原语**，非影子双份；并补记 B04 反转）；[functions/egdf/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/mod.rs#L1-L20) 头注释由"已完成 1/4"更正为"3/5"（devtree/composite 标记已迁；proto_*/user_driver 注明 privileged 内部函数指针表 / privileged 已实现但 functions 无封装），并移除"评估日期"。
> - **MIG-006 结案（不改 archive）**: 依 AGENTS.md §6「archive/ 为历史快照不再修改」，[archive/driver-service-migration.md](./archive/driver-service-migration.md) **有意保持冻结**（已按 commit `fae02a38` 归档、标记"✅ 已完成"）。B04 反转已由 live 文档完整承载（functions/driver/mod.rs 头注释 + §8.1 三阶段历史表），故本条**据 live 文档结案**，不修改归档快照。
> - **MIG-007 补测**: 新增 [egdf_registry_io_host_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/egdf_registry_io_host_test.rs) — 覆盖 functions/egdf 注册表语义（`register`/`find_by_name`/`find_by_proto`/`list`/`count`/`set_state`/`unregister`）+ 块设备 IO dispatch（`blk_read`/`blk_write` 成功 round-trip 与 3 类错误路径 + `blk_is_present`/`blk_total_sectors`/`blk_count`）；以进程内 `Mutex` 串行化规避全局 `EGDF_DEVICES` 并行竞争。**2 passed**。
> - **MIG-008 结案（接线补齐）**: 依 commit `c677702d`，[privileged/driver/display/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/display/mod.rs) 新增控制器工厂槽 `register_display_controller_factory` + 单向拉取入口 `display_probe_controllers`（DECISION-K 单向注册契约，`privileged::driver` 顶层 re-export）；[functions/driver/display/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/display/mod.rs) 的 `display_init` 注册无捕获工厂回调（HDMI/DP/DisplayManager 经 EGDF 注册，0 unsafe），由 crate root [lib.rs](file:///home/anfer/Code/Edgine/src/kernel/lib.rs) 在 `display_init` 后调用 `display_probe_controllers` 触发。原"控制器内核内零调用者"缺口已闭合。
> - **MIG-003 结案（完整迁移到 functions）**: 用户裁决「完整迁移到 functions」。privileged 侧删除 [privileged/driver/char/pl011.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/char/)（原 183 行 unsafe MMIO 实现）；[privileged/driver/char/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/char/mod.rs) 暴露 aarch64 safe 面 `pl011_phys_base()`（`arch::uart::base() & !KERNEL_BASE` 归一为物理地址），[privileged/iomem.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/iomem.rs) 新增 safe 构造器 `from_platform_device(phys,len,name)`（零基址校验 + `unsafe { Self::new }`）。functions 侧新建 [functions/driver/char/pl011.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/char/pl011.rs)（0 unsafe，经 `IoMem` 安全代理访问 UARTDR/UARTFR/UARTCR/UARTIBRD/UARTFBRD/UARTLCR_H/UARTIMSC）；[functions/driver/char/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/char/mod.rs) `char_init()` aarch64 分支经 `egdf_register_driver("pl011", EGDFProto::Char, ...)` 注册；[lib.rs](file:///home/anfer/Code/Edgine/src/kernel/lib.rs) 取消 x86_64-only 限制，双架构均调 `char_init()`。privileged 保留 `arch::uart`（早期控制台，进入用户态前）。配套 host 用例并入 [mm_iomem_alias_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/mm_iomem_alias_test.rs)（`from_platform_device` 零基址拒绝 + 有效基址注册/字段断言/出作用域 unregister）。
> - **MIG-004 结案（proto_* + user_driver 安全代理）**: 用户裁决「MIG-004 建安全代理（相对完整）」。functions 侧新建 [proto.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/proto.rs)（block 侧 `register_block_device`/`unregister_block` 直通 re-export；net 侧 `NetDevice` 结构体去裸指针封装 `&'static NetOps` + `driver_data` + `mac`，提供 `mac()`/`send`/`try_receive`/`handle_irq` safe 方法，经 `find_net_device()` 返回；input 侧 `input_read()`/`input_has_data()` 薄封装）与 [user_driver.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/user_driver.rs)（强类型 `UserDriverError` 6 项 + `Unknown(i32)`、`UserDriverResult<T>`、`to_errno` POSIX 映射；`bind`/`unbind`/`map`/`unmap`/`forward_irq` 经 `map_err` 收敛 privileged 私有错误码）。[functions/egdf/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/mod.rs) 注册两子模块 + 顶层 re-export，旧 `find_net_device`（返回 privileged `NetOps` + 裸指针）删除。现有调用点改走 functions 封装（`unregister`/`unregister_block`/`find_net_device`）。[audit_functions_boundary.py](file:///home/anfer/Code/Edgine/scripts/audit_functions_boundary.py) 白名单补 `('driver','egdf')`。新增 host-tests [egdf_proto_proxy_host_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/egdf_proto_proxy_host_test.rs)（5 passed：net 桥 round-trip / input 代理 / block 墓碑 unregister / 空注册表降级 / user_driver 错误映射）。**注**：`bind`/`unbind`/`map`/`unmap`/`forward_irq` 依赖真实进程表与 `MmStruct`，host-tests 不可覆盖，零引用登记见 [syscall-followup.md](./syscall-followup.md) B-6。
> - **MIG-005 结案（实测无重复代码，机制/策略边界已固化）**: 依当前源码实测，`privileged/driver/storage/` 实际仅 [mod.rs](file:///home/anfer/Code/QueenX/src/kernel/privileged/driver/storage/mod.rs)/[nvme.rs](file:///home/anfer/Code/QueenX/src/kernel/privileged/driver/storage/nvme.rs)/[ahci.rs](file:///home/anfer/Code/QueenX/src/kernel/privileged/driver/storage/ahci.rs) 三文件，**不存在** `nvme_block.rs`/`ahci_block.rs`/`ata_block.rs`，亦无 `ata.rs`——原表「privileged 含完整实现 + functions 业务层」及上方【本轮复验订正】所列文件名（`ata.rs`/`nvme.rs`/`ahci.rs`/`ata_block.rs`）均属台账描述漂移。[privileged/driver/storage/mod.rs](file:///home/anfer/Code/QueenX/src/kernel/privileged/driver/storage/mod.rs#L1-L27) 头注释已明示 DECISION-H storage 专项 3 号子步（storage_init 退位）后的边界：**机制保留** = NVMe wire 类型 / 队列 DMA 分配 / 提交与排空 safe wrapper / AHCI DMA fill 原语 / xHCI TRB 原语 / NVMe MSI-X ISR 编排（IDT 注册 + functions 分发契约槽）；**业务退位** = PCI 控制器探测/初始化/块设备注册整体迁 `functions::driver::storage`；**ATA 回迁** = 传统 ATA PIO 驱动落 [functions/driver/storage/ata.rs](file:///home/anfer/Code/QueenX/src/kernel/functions/driver/storage/ata.rs)（经 privileged `IoPort` safe 代理）。实测 privileged 侧**无控制器状态机**（无 probe/init/submit 业务），functions 侧 `nvme`/`ahci`/`ata` 持完整控制器业务并经 `use` **复用** privileged wire 类型（非复制）。故「双份代码/边界未理清」的原始结论**不成立**。**处置：据 live 源码结案**；原表行与上方【本轮复验订正】为历史快照，依本文档「不涂改」惯例**保留**，以本条为最新事实。

---

## 🟤 第 9 类：分册 6 调研预存问题（2026-08-31）

> 2026-08-31 追加：分册 6（[audit-fix-06-services-fs.md](./archive/audit-fix-06-services-fs.md)）实施过程中调研发现的 3 个**分册 6 范围外**预存问题。用户裁决（2026-08-31）：统一登记待后续处理（选项 3A），不在本分册修复。

### B06-PRE-001: tmpfs.rs `<256` 硬编码（与 B06-09 同款）

| 字段 | 数据 |
|---|---|
| **位置** | [functions/fs/tmpfs.rs:79](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/tmpfs.rs#L79) |
| **问题** | `TmpFsInode::is_dir` 用硬编码 `< 256` 判断 inode 范围 + `fs.inner.nodes[...].file_type == 1` 魔法数，与 B06-09 修复前的 RamFsInode 同款缺陷（B06-09 只修了 ramfs，未修 tmpfs） |
| **建议** | 与 B06-09 同法：硬编码 256 → `RAMFS_MAX_NODES` 常量，魔法数 1 → `VfsFileType::Dir.as_u8()` |
| **状态** | 🔄 已失效 (本轮复验: 源码已改为 `nodes.len()` + `VfsFileType::Dir.as_u8()`) |
| **【本轮复验订正】** | [tmpfs.rs:91-96](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/tmpfs.rs#L91-L96) 现为 `idx < fs.inner.nodes.len() && fs.inner.nodes[idx].file_type == VfsFileType::Dir.as_u8()`（注释：越界节点一律视为非目录，避免索引 panic）——硬编码 `<256` 与魔法数 `1` 均已消除，本条缺陷已不复存在 |

### B06-PRE-002: fchown_syscall 缺权限校验（安全缺陷）

| 字段 | 数据 |
|---|---|
| **位置** | [functions/fs/misc.rs:115-126](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/misc.rs#L115-L126) → `vfs_fchown` → `inode().chown()` |
| **问题** | `fchown(fd, owner, group)` 直接把 owner/group 透传给底层 `Inode::chown`，**无任何权限校验**——任意进程可对自己已打开的 fd 修改为任意 owner/group（B06-02 修了 `chown` 的 uid 回退，但 `fchown` 路径的 owner 是 pwm 值、无"未注册回退"问题，却缺失"是否有权修改属主"的检查） |
| **影响** | 权限语义缺失（非直接提权，但违背"能力制"权限模型） |
| **建议** | 与 B06-02 对齐：fchown 前置 `FS_CAP_CHOWN` (bit5) 能力检查，或按提权语义评估 |
| **状态** | ✅ 已修复（本轮: fchown 前置 `FS_CAP_CHOWN` 能力校验） |
| **【本轮复验订正】** | 仍成立：[misc.rs:115-126](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/misc.rs#L115-L126) 的 `fchown_syscall` 仍无 `FS_CAP_CHOWN` 校验，owner/group 直接透传底层 `Inode::chown` |
| **【本轮修复（修遗留工程）】** | [misc.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/misc.rs) `fchown_syscall` 在 `current_pwm()` 之后、透传底层前新增 `pwm_has_capability(pwm, CAP_DOMAIN_FS, FS_CAP_CHOWN)` 前置校验，缺失即返回 `EPERM`（与 access/open 路径能力语义一致）；`# Errors` 文档注释同步更新。 |

### B06-PRE-003: LegacyInode 删除时机（架构清理）

| 字段 | 数据 |
|---|---|
| **位置** | [functions/fs/inode.rs:415-424](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/inode.rs#L415-L424) |
| **问题** | B06-12 方案 C（废弃标记 + 推动消除）落地：LegacyInode 已加废弃标记，当前全部 8 个 FS 均实现 `fs_resolve_inode`，LegacyInode 仅作 `open_by_handle_at` 防御性回退（正常路径不触发） |
| **建议** | 未来移除 `open_by_handle_at` 的 LegacyInode 回退分支（file_handle.rs:187）后删除整个 LegacyInode 类型；需确认各 FS `fs_resolve_inode` 覆盖所有挂载场景 |
| **状态** | ✅ 已修复（本轮: 移除 `open_by_handle_at` 回退分支 + 删除 `LegacyInode` 类型） |
| **【本轮复验订正】** | 结论仍成立，**行号漂移**: `LegacyInode` 定义现位于 [inode.rs:268](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/inode.rs#L268)，废弃标记注释在 `:260-264`（原记 `:415-424` 现为 `set_times`/`pread_inode`）；[file_handle.rs:199-205](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/file_handle.rs#L199-L205) 回退分支仍在（原记 `:187` 漂移） |
| **【本轮修复（修遗留工程）】** | [file_handle.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/file_handle.rs) `open_by_handle_at_syscall` 移除 `fs_resolve_inode(...).unwrap_or_else(|| LegacyInode ...)` 防御性回退，改为 `.ok_or(Errno::EINVAL)?`（句柄失效即 EINVAL，与相邻 `mount_idx` 校验一致）；[inode.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/inode.rs) 删除整个 `LegacyInode` 类型及其 `impl Inode`（-186 行），文件头"具象实现"注释同步去掉 `LegacyInode`。 |

### B06-PRE-004: socket_max_sockets_test flaky（已排查确认非内核问题，测试已修复）

| 字段 | 数据 |
|---|---|
| **位置** | [host-tests/tests/socket_max_sockets_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/socket_max_sockets_test.rs) |
| **现象** | 全量 host-tests 偶发失败（单独跑全过），高负载必现趋势 |
| **根因** | 测试自身 8 个 `#[test]` 共享同一 `static G_MAX_SOCKETS: AtomicUsize`（测试内镜像变量），Rust 测试默认并行执行导致测试间互相覆盖 |
| **内核侧排查** | ✅ **无真实缺陷**——(1) 内核 [sockets.rs:38](file:///home/anfer/Code/Edgine/src/kernel/privileged/net/init/sockets.rs#L38) 的 G_MAX_SOCKETS 是 `AtomicUsize`（非 static mut），store/load 用 AcqRel/Acquire，无数据竞争；(2) [sm_fi.rs:270-277](file:///home/anfer/Code/Edgine/src/kernel/privileged/net/init/sm_fi.rs#L270-L277) 的活动计数 + 上限检查在 `NET_STATE.lock()` 临界区内，无 TOCTOU；(3) `set_max_sockets` 当前无任何调用方，运行时并发写路径不存在 |
| **修复** | 删除共享 static，辅助函数参数化接收 `&AtomicUsize`，每测试独立实例（commit 28fd91d0） |
| **启示** | host-tests 镜像测试"复刻逻辑但不复刻锁/原子性"，镜像测试 flaky 优先怀疑"镜像丢了并发保护"而非内核缺陷 |
| **状态** | ✅ 已修复 (2026-08-31, commit 28fd91d0) |
| **【本轮复验订正】** | 当前测试形态已进一步收敛为**单一顺序测试** `max_sockets_config_semantics`（[socket_max_sockets_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/socket_max_sockets_test.rs)，随 B08-20 迁移），共享 static 并行覆盖的根因不再存在 |

---

## 📋 文档状态不一致清单 (跨文档矛盾专项)

> **原审计发现 (2026-08-09)**: `progress-active-tasks.md` 中 B1-B6 标记 `[X]` 完成, 但 `archive/code-review-findings-2026-08-01.md` 中对应 REVIEW-FINDING 仍 `[]` 未同步. 记为**文档漂移**.
>
> **2026-09-26 处置结论**: 逐项回源码复验, 实现侧已全部落地 (见 §3.1-3.3). 归档快照 `archive/code-review-findings-2026-08-01.md` 的 `[]` 依 AGENTS.md §6「archive/ 为历史快照不再修改」**有意冻结**, 不属待修漂移 — 因此本清单**结案**, 无需再同步归档文档.

| progress | code-review (归档快照) | 2026-09-26 复验结论 |
|---|---|---|
| B1 [X] | REVIEW-FINDING-027 `[]` | ✅ 落地 — `privileged/mod.rs:10` 无 LoC 数字 |
| B2 [X] | REVIEW-FINDING-028 `[]` | ✅ 落地 — net/fs/proc 头注释已更新 |
| B3 [X] | REVIEW-FINDING-029 `[]` | ✅ 落地 — README.md 整篇重写, 原 :21/:71 moot |
| B5 [X] | (无对应) | ✅ 落地 — sgeg/storage.rs + freg/api.rs |
| B6 [X] | REVIEW-FINDING-031 `[]` | ✅ 落地 — iomem.rs debug_assert! + constants/limits.rs |
| DECISION-037 [X] | REVIEW-FINDING-025 `[]` | ✅ 落地 — privileged/syscall/mod.rs:22-30 注释统一 |
| DECISION-038 [X] | REVIEW-FINDING-024 `[]` | ✅ 落地 — 全仓 0 引用 (2026-09-26 补清 host-tests/README.md 3 处) |
| DECISION-039 [X] | REVIEW-FINDING-026 `[]` | ✅ 落地 — UserContext 已迁回 privileged, functions 反向 re-export |

**结案说明**: 原"建议后续行动"(同步 code-review 文档状态) 经复验判定**不需执行** — 归档快照按规矩冻结, 实现侧无遗留.

---

## 🎯 优先级建议 (用户决策参考)

> **【本轮复验订正】** 下方 P0-P3 为 **2026-08-09 原登记**（保留不涂改）；按当前源码状态复验后，其中 **REVIEW-FINDING-026/027/028/029** 与 **ISSUE-SRC-001~016** 均已落地/消除，应从待办移出. 当前实际开放项建议排序如下:
> - **P0**: ISSUE-RT-002（aarch64 GICv3 挂起，未修复；`.gdb_debug_gic` 证据已失效，以用户实际状态为准）.
> - **P1**: ISSUE-RT-001（x86_64 e1000/smoltcp 挂起）; B06-PRE-002（fchown 缺权限校验，**安全缺陷**）; ISSUE-RT-003（真实硬件验证）.
> - **P2**: 第 8 类 MIG 边界治理（MIG-005/006/007）/注释同步（MIG-001/002）; 第 0B 类 B03-LEGACY-001/002/003（COW TOCTOU + host-tests 缺口）; ISSUE-TOOL-002（x86_64 侧陈旧检测）.
> - **P3**: 第 4 类远期工程 F1-F5; B06-PRE-003（LegacyInode 清理）; MIG-003/004/008; 刻意维持项 DEC-046/041/005.
>
> **【本轮修复（修遗留工程）后开放项变化】**: 下列项已从本节待办移出 — **B06-PRE-002**（fchown 能力校验，P1）、**B03-LEGACY-001/002/003**（COW TOCTOU + host-tests 缺口，P2）、**ISSUE-TOOL-002**（x86_64 侧陈旧检测，P2）、**B06-PRE-003**（LegacyInode 清理，P3）、**MIG-006/007**（迁移文档补注 / egdf 测试缺口，P2）；**ISSUE-TOOL-003** 重评结案不施工；**MIG-001/002** 注释同步完成。仍未闭合: **ISSUE-RT-001/002/003**（运行时挂起/真机验证）、**MIG-003/004/005/008**、第 4 类远期工程 F1-F5、刻意维持项.
>
> **【本轮结案同步（RT-001/003 + MIG-003/005/008）】**: 依 commit `8a10a36f`（结案运行时遗留 RT-001 并交付 aarch64 真机启动介质）、`c677702d`（MIG-008 显示控制器接线补齐）与 `cb086930`（存储驱动去类型副本，统一以 privileged 权威定义为准），下列项状态收敛 —
> - **ISSUE-RT-001 闭合**: 根因为 e1000 三处寄存器位域常量写错（`CTRL.RST` bit31→bit26 / `CTRL.FRCDPX` bit14→bit12 / `RCTL.BSIZE_2048` bit25 实为 BSEX→`0x0`），修复后 QEMU 默认 e1000 路径断言 `e1000: 初始化完成` + 完整进 Ring 3 通过；已由 [driver_e1000_ctrl_bits_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/driver_e1000_ctrl_bits_test.rs) 固化防回归。
> - **ISSUE-RT-003 交付物就绪（执行待硬件）**: 真机验证三件套已交付 — [make_boot_medium.sh](file:///home/anfer/Code/Edgine/scripts/make_boot_medium.sh)（介质制作）+ [guide-hardware-boot.md](file:///home/anfer/Code/Edgine/docs/explain/guide-hardware-boot.md)（串口 checklist / SoC 契约）+ 台账登记；真机**执行**待用户提供硬件。
> - **MIG-008 闭合**: 显示控制器经 DECISION-K 单向注册契约接线（privileged 工厂槽 + `display_probe_controllers` 单向拉取；functions `display_init` 注册无捕获回调），原"控制器零调用者"缺口消除。
> - **MIG-005 边界已理清**: [privileged/driver/storage/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/storage/mod.rs) 现仅保留机制原语（NVMe 队列 DMA 分配 / 提交与排空 safe wrapper / AHCI DMA fill / xHCI TRB / MSI-X ISR 编排 + wire 类型），业务（控制器探测/初始化/块设备注册）已退位 [functions/driver/storage/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/storage/mod.rs)（0 unsafe，crate root 编排）；privileged 侧无 `ata.rs`（ATA 已回迁 functions）。privileged 侧**无控制器/块设备实现**，"双份"不再存在，属机制/业务分野而非重复。
>
> **仍未闭合** 收敛为: **ISSUE-RT-002**（aarch64 GICv3 挂起，QEMU TCG 时序偶发不可复现，待 SMP/真机复验）、第 4 类远期工程 F1-F5、刻意维持项.
>
> **【MIG-004 收口同步】**: 依 commit `349167d5`，**MIG-004 已由「未闭合」移出** — 用户裁决「MIG-004 建安全代理（相对完整）」，functions/egdf 新建 [proto.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/proto.rs)（block 直通 re-export / net `NetDevice` 去裸指针封装 / input 薄封装）与 [user_driver.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/user_driver.rs)（强类型 `UserDriverError` + `to_errno`），现有调用点改走 functions 封装，[audit_functions_boundary.py](file:///home/anfer/Code/Edgine/scripts/audit_functions_boundary.py) 白名单补 `('driver','egdf')`，并补 host-tests（5 passed）。故本节"当前实际开放项"再收敛为: **ISSUE-RT-002** + 第 4 类远期工程 F1-F5 + 刻意维持项 DEC-046/041/005.

> **【DECISION-082 尾项登记（KPTI 单实例全局量）】**: aarch64 SMP bring-up（DECISION-082）收口时复核 [mm/kpti_aarch64.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/kpti_aarch64.rs) 的全局量内存序，发现**结构性缺陷** —— `user_ttbr0`（偏移 24）与 `tramp_save0`/`tramp_save1`（40/48）属**每核活跃状态**却置于单实例 `KPTI_GLOBALS`，入口/出口汇编按固定偏移 `str`/`ldr` 访问，多核并发 EL0 会**跨核互相覆盖**（用户 `x3`/`x4` 损坏 / 以他核页表 `eret`）。同库既有 per-CPU 范式为 [mm/copy_user.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/copy_user.rs) 的 `PER_CPU_EXCEPTION_CTX[cpu]`。boot 期发布字段（`ready`/`kernel_ttbr0`/`kernel_ttbr1`/`tramp_ttbr1`）经复核**内存序配对完整、通过**。当前 AP 停在 idle 未调度用户任务，故**潜伏未爆发**；用户已裁定**登记并立项**（本轮不改行为，AGENTS §9.1）。已在本轮修正 [kpti_aarch64.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/kpti_aarch64.rs) 的失真注释；专项（**KPTI-PCPU-01**，per-CPU 化）登记于 [aarch64-smp-bringup.md](./archive/aarch64-smp-bringup.md)「后续专项登记」章节，**待排期**（前置依赖 = AP 参与用户态调度）。
>
> **【DECISION-083 施工期实证登记（SGI 使能 + 编码修复）】**: aarch64 TLB shootdown 发送路径工程（DECISION-083，[aarch64-tlb-shootdown-send.md](./archive/aarch64-tlb-shootdown-send.md)）施工期，由 QEMU `-smp 2` 实证发现两条使该工程失效的前提缺陷，均按用户裁定「取长期最优」修复（ST-09）：
> 1. **每核 SGI 从未使能**：SGI 13（TLB shootdown）/14（reschedule）/7（FREG恢复）的使能位属 **per-CPU Redistributor 私有状态**（`GICR_ISENABLER0`，复位值 0），历史只在 BSP 侧经分散入口使能 Timer PPI 与 SGI 7，SGI 13/14 **从未置位** ⇒ AP 上线后接收侧 `intid == 13` 分支永不触发，延迟释放帧永久滞留。**修复**：SGI 编号集中定义于 [gic.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/gic.rs)，新增 `enable_sgi`，在**每核唯一中断入口** `gic::init_per_cpu` 内使能全部内核 SGI（BSP/AP 共用）；删除因此成为死代码的 `gic::gicr_sgi_write` 与 `freg::enable_freg_sgi`（F9）。
> 2. **`ICC_SGI1R_EL1` 目标编码错误**：[arch/aarch64/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/mod.rs) 的 `send_ipi` 把目标 Aff0 编入 `1 << (16 + aff0)`（实为 Aff1 字段，寻址到不存在的簇），SGI **永不投递**。**修复**：改为 `1 << (target_cpu & 0xF)`（TargetList[15:0] 的对应位），对齐 Linux `gic_send_sgi` 与 ICC_SGI1R_EL1 字段布局。
> **验证**：QEMU `-smp 2` 出现 `IRQ: intid=13 count=2..5` 与 `[SMP] TLB shootdown #1/#2/#3 gen=1/2/3`（两核均追平）；[scripts/qemu_boot_test.sh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh) aarch64 分支新增该里程碑断言（fail-closed）。
> **工程外发现（§12.5 报告并经用户裁定「本轮一并修复」）**：① [arch/aarch64/freg/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/freg/mod.rs) 的 `freg_trigger_recovery` 曾以 `1u64 << 16` 编码目标（同上条同族缺陷，因 `freg=off` 未触发），现改为按当前核 Aff0 编码 TargetList；② [exception.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/exception.rs) 第二处 `#[expect(clippy::borrow_as_ptr)]` 在 aarch64 clippy `-D warnings` 下报 unfulfilled（HEAD 即存在；CI 只跑 x86_64 clippy 故潜伏），现删除该过期 `#[expect]`。二者均为一行改动，修复后 aarch64 clippy `-D warnings` 通过。

> **【DECISION-084 登记（AP 参与用户态调度 + KPTI-PCPU-01 落地）】**: 由 [smp-ap-user-scheduling.md](./archive/smp-ap-user-scheduling.md)（**DECISION-084**，方案 C 相对完整）落地「AP 参与用户态调度」并结案 KPTI-PCPU-01 —— 该工程消除三处断链（AP 无调度身份 / 无 push 投送 / 无唤醒源），使 ≥2 个用户任务可在多核并发处于 EL0，并把 aarch64 KPTI 入口/出口每核活跃值 per-CPU 化。
> 1. **AP 调度闭环**：`adopt_cpu_idle`（APS-01）、AP idle 调度循环 + per-CPU 定时器/Busy IPI 唤醒（APS-02）、`cfs_enqueue_to` + `find_idle_cpu` push 投送（APS-03）。
> 2. **KPTI-PCPU-01 结案**（aarch64，APS-04）：`user_ttbr0`/`tramp_save0`/`tramp_save1` 迁入按核数组 `KPTI_CPU_GLOBALS`，槽基址经 `kpti_bind_cpu` 写入 `TPIDR_EL1`，汇编只 `mrs tpidr_el1` 取址；[aarch64-smp-bringup.md](./archive/aarch64-smp-bringup.md) §「后续专项登记」的 KPTI-PCPU-01 已置结案。
> 3. **收口期修复（次核 CPU 状态初始化，批次 P5）**：AP 上电路径遗漏 per-CPU CPU 状态初始化 —— aarch64 `ap_main` 补 `CPACR_EL1.FPEN=0b11`（缺则 `context_switch_asm` 首保存 V0-V31 触发 FP/ASIMD trap，`ESR_EL1.EC=0x07`，核永久离线）；x86_64 `ap_entry` 补 `cpu::init_msr`（CR4.OSFXSR/OSXMMEXCPT，缺则 `fxsave` #UD→#DF）并在 `gdt_init_ap` 补 `IA32_GS_BASE`（缺则内核态 `[gs:...]` 读垃圾）。修复后双架构 `-smp 2` 均出现 `[SMP] EL0 ... cpu=1`。
> **验证**：§2.3 六门槛全过（`./ci/build.sh all` 5/0；`./ci/build.sh aarch64 && ./ci/audit.sh quick` EXIT=0；`make test-host` 通过；`make test-kernel-host` 949/0；`TIMEOUT_QEMU=30 ./scripts/qemu_boot_test.sh all` 2/2，双架构命中成对 EL0）。前述「仍未闭合」收敛为: 第 4 类远期工程 F1-F5、刻意维持项（APS-05 判据对 x86_64 的 grep 漏匹配已随 NUL 字节 `-a` 修复消除）。
> **收口期新登记（独立于本修复）**：aarch64 `KPTI-09` 里程碑在本次实测中**未触发**（详见下方 **ISSUE-RT-004**）—— 该现象在 AP 崩溃修复前即被崩溃掩盖, 属预存问题。**后续已修复**：根因为调度链路断裂（D6 tick 未接线 + softirq 内调度泄漏 `do_softirq` 的 per-CPU `running` 标志），已按「调度点后移到 `do_softirq()` 之后」修复, aarch64 QEMU 双核已触发 KPTI-09。

### P0 — 立即关注 (1 项)

- **ISSUE-RT-002** (aarch64 GICv3 挂起) — 用户当前 GDB 调试中

### P1 — 本季度修复 (5 项)

- **ISSUE-RT-001** (x86_64 e1000/smoltcp 挂起)
- **ISSUE-RT-003** (真实硬件验证)
- **REVIEW-FINDING-026** (privileged→functions 反向依赖)
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

- **本轮（第二十六轮：P-71 结案 —— crate 拆分）**: 用户裁定「拆分 framework/services 两层为独立 crate」（其原话背景为「一直受代码维护工程的阻塞」）并授权起工。起手 **P-71**（新立并同轮收口）：阶段 1（privileged 独立成 crate）此前已完成，本轮完成阶段 2（functions 独立成 crate + glue 收缩）的收口验证与台账同步。
  - **裁定面（两项，经 AskUserQuestion，均采纳推荐项）**: crate 命名 = `kernel-privileged` / `kernel-functions`；跨层测试处置 = **甲・上移出 TCB**（迁入 glue 的 `kernel_tests/`，不留在 privileged 内）。
  - **施工形态（三成员 workspace；文件零移动 + 别名零改写）**: 根 `src/kernel/Cargo.toml` 的 `[workspace]` 为 `members = ["privileged"]` + `exclude = ["functions/net/smoltcp"]` + `resolver = "3"`；functions 经 root path 依赖自动纳入成员（实测若同时显式列入 members，exclude 不生效，vendored smoltcp 的 `[[example]]` 指向已裁掉文件而解析失败）。两个子 crate 的 `[lib]` 均直接指向既有 `mod.rs`（`kernel_privileged` / `kernel_functions`），glue 的 `lib.rs` 由 `pub mod …` 改为 `pub use kernel_privileged as privileged;` / `pub use kernel_functions as functions;`；crate 内则以 `pub use crate as privileged;` / `pub use crate as functions;` 维持自引用 ⇒ 全仓 1742 处 privileged 自引用、771 处 functions 自引用与 1078 处跨层引用**逐字节未改**。privileged 的 `alloc/` 因与 crate 根 `extern crate alloc;` 同名歧义 `git mv` 为 `mem_alloc/`。glue 的依赖收敛为两条 path 依赖（原 5 个直依赖 edgine-abi / bitflags / zerocopy / ed25519-dalek / smoltcp 已随代码归入子 crate，保留即触发 `unused_dependencies`）；全部 feature 逐项转发（`host-test` / `kernel_test` 双转发，`e1000-real-hw` / `async` / `initramfs` → functions，其余 → privileged），否则子 crate 内 cfg 分支静默消失。
  - **lint 豁免与可见性校准**: cargo `[lints]` 表优先级低于命令行 `-D clippy::pedantic`（实测保留原表会让 allow 全部失效、误报 1623 条）⇒ 原 `[lints]` 整段删除，改写成子 crate 根的 `#![allow(...)]`：privileged **6 rustc + 46 clippy**、functions **5 rustc + 46 clippy**，全部属**迁移**而非新增。`impl KernelError { as_i32 }` 因类型定义在 crate 外不可写 inherent impl（E0116）归位 `privileged/error.rs`；`pivot_root_plan` 因跨层测试载体不再共享 `pub(crate)` 而升 `pub` 并补 `# Errors`（触发 clippy `missing_errors_doc` 后修）；e1000 被 functions 引用的 22 个寄存器常量升 `pub` 并补中文 `///`，未被引用者收窄回 `pub(crate)`。
  - **门禁适配（拆分后 `-p` 单点不再等价于全量验证）**: `ci/audit.sh` 步骤 2 双架构 clippy、2b feature 维、2c fmt（改 `cargo fmt -p kernel -p kernel-privileged -p kernel-functions -- --check`）与 `scripts/check_kernel_test_release_dim.sh`、`Makefile` 的 `test-kernel-host` 一律加 `--workspace`（不用 `--all`，会连带 vendored smoltcp）—— 否则 privileged 内联用例静默不跑。`Makefile.ci` 的 `ci-rustdoc` 两腿加 `--workspace`，并把 `cargo doc … | tee | tail; rc=$?` 改为重定向日志后单独取 rc：原形态取的是 `tail` 退出码恒 0，属**既有 fail-open**（修后才首次拿到真实 rc）。
  - **两处门禁缺陷当场修复**: ① F9 侧车重导 —— crate 根复制 46 条 clippy allow 属迁移 ⇒ `scripts/audit_dead_code_comment.baseline.json` 由 1706 重导为 **1752 处 / 306 文件**；② 债册腿 `scripts/audit_missing_docs_debt.py` 单 target 目录会让拆出的 crate 交叉污染 —— 取 functions / kernel 时 privileged 作为依赖被**不带** `-Dmissing_docs` 编过并缓存，下一轮 `-p kernel-privileged` 命中指纹判 Fresh，而 cargo 对 Fresh unit **不复播诊断** ⇒ 读数 2430→0（实测总读数 2047 = 仅 functions 侧）。`cargo clean -p` 实测 `Removed 0 files`（该布局下不生效）⇒ 改为**每成员独立 target 子目录** `audit-missing-docs/<pkg>`；连跑两次均 **4477 处 / 237 文件 / 647 键** 与基线一致（首次冷启动约 1m18s，第二次约 20s）。
  - **台账同步**: `net-e2e-tcp-echo-flakiness.md` §6 末尾新增 P-71 条目（状态 `[X]`）；口径 A 实测 **65 条 = 50 `[X]` / 14 开放（11 `[ ]` + 3 `[]`）/ 1 `[~]`**（P-71 新立并同轮收口 ⇒ 开放 14→14、总数 64→65）；`docs/plan/progress-active-tasks.md:526` 四处对齐（计数行、实测行、轮次叙述、闭合枚举追加 `/P-71`）。
  - **门槛读数（本窗口同一棵树逐条实测）**: 门槛 1 `./ci/build.sh all` **RC=0**（`Passed: 6 / Failed: 0`，日志 `warning:` 计数 **0**，仅两行 cargo future-incompat 元信息）；门槛 2+3 `bash ci/audit.sh full` **RC=0**（`✓ 25 条文档点名腿全部在册`、`✓ P-69 腿清单与 AGENTS §2.2 反向对账: 通过`、`✓ 审计腿到位: 19/19`、`✓ P-49 存量审计腿到位: 9/9`、`✓ 编译器口径与债册一致 (4477 处)`、`✓ dead_code_comment` 死代码抑制 0 处且 clippy 抑制与冻结基线逐项一致〔1752 处 / 860 项〕、QEMU `2/2`、`本轮因环境缺失未执行腿数: 1` = lockbud 工具链不匹配，本机常态）；门槛 4 `make test-host` **RC=0**（**1106 passed / 0 failed**，141 套）；门槛 5 = full 内 QEMU `2/2`；门槛 6 `make test-kernel-host` **RC=0**（**1019 passed / 0 failed**：kernel 12 + kernel-functions 579 + kernel-privileged 428，合计与拆分前一致）；第七项 `make -f Makefile.ci ci-rustdoc` **RC=0**（两架构各 `rustdoc exit: 0`）。
  - **未达成（如实登记）**: 其一，`pub(crate)` 语义在跨 crate 的测试载体上失效是拆分的结构性代价，已用「上移 `kernel_tests`」消解，但**今后任何跨层 `pub(crate)` 依赖都会以编译错误暴露**，需按 `pub` + 中文 `///` 处理而非回退拆分；其二，三成员 workspace 下历史文档里的 `cargo test --features host-test --lib` 一类写法不再等价于全量（本轮已改 `Makefile` 与 CI 侧，但**未普查** `docs/` 内其余同形命令示例；`tools/check_doc_paths.py` 只核路径新鲜度、覆盖不到命令）；其三，本条不改 vendored smoltcp，两架构产物目录与 S-6/P-10 防线未动。下一件按 §6.0 A 序列为 **P-66**。

- **本轮（第二十五轮：P-67 结案 —— rustc lint 抑制不在 F9 射程）**: 承用户批次裁定「严格编号序逐件」＋「AGENTS.md 全部该改就改」。起手 **P-67**（P-59 落空面：债册腿只看编译器读数，写 `#![allow(missing_docs)]` 会让读数下降、重导快照即复绿 ⇒ 与真补写在棘轮里同形）。
  - **裁定面（一项，AskUserQuestion 被跳过，无代答）**: P-67 路线提问未返回文字回答（系统侧跳过），故按方案文本里的**推荐项乙**落地 —— 用户未对路线另作指示，本条如实登记「无显式裁定」而非记为已裁。方案文本三条：甲 扩 F9 射程（把一般 rustc lint 并入 `audit_dead_code_comment.py` 族清单）、乙 债册腿自带定点检查、丙 只写口径不执法。**零 AGENTS 改动** ⇒ 本批授权的第 13 次修改额度未消耗。
  - **施工形态（`scripts/audit_missing_docs_debt.py`）**: 新增 `scan_suppressions()`（`SCAN_ROOT = src/kernel`、`VENDORED_DIRS = ("smoltcp",)` 按路径组件排除），以 `ATTRIBUTE_RE = #!?\[([^\]]*)\]` 收集属性整段、`STRING_RE` 先剥字符串（`reason = "missing_docs"` 不算抑制）、`SUPPRESSION_RE = \b(allow|expect)\s*\(([^()]*)\)` 只判 `allow`/`expect`（`deny`/`forbid` 是 P-59 裁定 ④ 的合法硬执法，不判），命中即 `return 1` 并逐项打印 `路径:行号 片段`（上限 15 条后汇总）。调起点在 `run_compiler()` **之前**（零耗时，`--update-baseline` 同拦）；扫描根缺失/读失败抛 `OSError` ⇒ `return 2` fail-closed。模块 docstring 的「绕过面」条目同步改写为「本腿自带这一盏灯的定点检查（P-67 乙）」，并把「其它 rustc lint 抑制是否入 F9」明确留给 P-53/P-64 的既有裁定面（不在本条扩围）。
  - **结构锁**: `test_p67_missing_docs_suppression_is_policed_by_debt_leg`（`host-tests/tests/ci_entrypoint_unified_test.rs`，套件 20 → **21 passed**，5 臂）—— ① 射程与 vendored 排除字面量齐备；② `SUPPRESSION_RE` 定义行含 `(allow|expect)` 且**不含** `deny`/`forbid`；③ 字符串先剥（`STRING_RE` 定义 + `.sub` 调用）；④ `scan_suppressions()` 调用位置早于 `run_compiler()`；⑤ fail-closed 文案与 `return 2` 并存。
  - **负控制（两组运行时 + 三形态结构，测毕即复原，`diff -q` 逐次核对）**: **运行时** ① 在 `privileged/syscall/types.rs` 注入 `#![allow(missing_docs)]` ⇒ **RC=1 / 0.12s** 且点名 `:1`（对照：注入前同树 RC=0 读数 **4482 处 / 238 文件 / 648 键** 与侧车一致）; ② 在 `functions/fs/vfs_manager.rs` 注入 `#[expect(missing_docs)]` ⇒ **RC=1 / 0.11s**（跨 privileged / functions 两侧各证一次）。**结构** ③ 把扫描根改为 `src/rust` / ④ 给 `SUPPRESSION_RE` 加上 `deny` / ⑤ 不剥字符串 ⇒ 三次均 **0 passed / 1 failed**，复原后 **21 passed / 0 failed**。另以临时目录 + 改 `m.ROOT`/`m.SCAN_ROOT` 真调 `scan_suppressions()` 做 **7 例 fixture 单测**：allow / expect / `cfg_attr(..., allow(...))` 命中共 3 条；deny、forbid、`reason = "missing_docs"`、`missing_docs_in_private_items`、vendored smoltcp 均不命中。
  - **台账同步**: `net-e2e-tcp-echo-flakiness.md` P-67 状态 `[ ]` → `[X]` 并写结案行；口径 A 实测 **64 条 = 49 `[X]` / 14 开放（11 `[ ]` + 3 `[]`）/ 1 `[~]`**（P-67 转 `[X]` ⇒ 开放 15→14、总数 64 不变）；`docs/plan/progress-active-tasks.md:526` 四处对齐（计数行、实测行、轮次叙述、§6 件数「十五件」→「十四件」）。
  - **门槛读数（本窗口同一棵树逐条实测）**: 门槛 1 `./ci/build.sh all` **RC=0**（`Passed: 6 / Failed: 0`，日志 `warning:` 计数 **0**）；门槛 2+3 `bash ci/audit.sh full` **RC=0**（含 `✓ P-67 抑制检查`、`✓ 25 条文档点名腿全部在册`、`✓ P-69 腿清单与 AGENTS §2.2 反向对账: 通过`、`✓ 审计腿到位: 19/19`、`✓ P-49 存量审计腿到位: 9/9`、QEMU `2/2`、`本轮因环境缺失未执行腿数: 1` = lockbud 工具链不匹配，本机常态）；门槛 4 `make test-host` **RC=0**（**1106 passed / 0 failed**，较第二十四轮 1105 的 +1 即本轮结构锁）；门槛 6 `make test-kernel-host` **RC=0**（首跑 1018/1，唯一红为 `privileged::timer::sleep::tests::test_busy_wait_positive_duration` 高 load 下时基抖动，单跑与全量复跑均绿 ⇒ **1019 passed / 0 failed**，非本轮引入）；第七项 `make -f Makefile.ci ci-rustdoc` **RC=0**（两架构各 exit 0）。门槛 5 即 full 内 QEMU 腿，读数已含在门槛 2+3。
  - **未达成（如实登记）**: 其一，本腿只保 `missing_docs` **这一盏灯** —— 其它 rustc lint 的 `allow`/`expect` 是否入 F9 射程仍属 P-53/P-64 的裁定面，本条刻意不扩围；其二，射程只到 `src/kernel/`（`host-tests/`、`src/user/`、`src/rust/` 的 rustc lint 抑制不在本条视野，与 P-50 已登记的 F7/F8 扫描根边界同族）；其三，属性识别按「实参不嵌方括号」取值（`ATTRIBUTE_RE` 以 `[^\]]*` 收尾），本仓现状成立但若将来出现嵌套方括号实参则会漏取 —— 属实现口径边界而非当前缺口。下一件按 §6.0 A 序列为 **P-66**。

- **本轮（第二十四轮：P-69 结案 —— full 档到位断言自参照）**: 承用户批次裁定「严格编号序逐件」＋「AGENTS.md 全部该改就改」。起手 **P-69**（P-59 负控制 C5a 的正式立案：0.5n 的 `AUDIT_MISSING_LEGS` 与 2e 的 `AUDIT_MISSING_LEGS_FULL` 两处到位断言的期望值与实测值**同源于清单变量本身**，把整条腿从清单摘掉时 runtime 仍打「到位 8/8」全绿，只有 host-tests 的红能看见）。
  - **裁定面（一项，AskUserQuestion 被跳过，无代答）**: P-69 路线提问未返回文字回答（系统侧跳过），故按方案文本里的**推荐项乙**落地 —— 用户未对路线另作指示，本条如实登记「无显式裁定」而非记为已裁。**零 AGENTS 改动** ⇒ 本批授权的第 13 次修改额度未消耗。
  - **施工形态（纯 CI 侧，790 行 = HEAD 744 + 46）**: `ci/audit.sh` 在 `mark_leg()`（`:84`）之后、首个门禁（`step "0/6`）之前插入**无条件**反向对账块（`:86`–`:130`，约 44 行）：内联 Python 读 `AGENTS.md`，切片 `### 2.2` → `### 2.3`，用反引号正则 `(?:audit|scan)_[a-z_]+` 收集「文档点名腿」为**外部期望**，与 `AUDIT_REQUIRED_LEGS` + `AUDIT_REQUIRED_LEGS_FULL` 两档清单的腿名求差；差集非空即逐项打印缺失标识并 `exit 1`（fail-closed）。腿名映射由 `leg_names()` 双前缀候选实现（`audit_` / `scan_` 各剥一次，另保留全名）——首版只剥 `audit_` 导致 `scan_obsolete_naming` 被误报 missing（实测 25 条期望里 1 条假红），本轮已修。**反向不判**（清单腿未在文档点名不算违规）：§2.2 自述「本表为常用核心门禁项，非穷举」，如 quick 档的 unsafe 腿映射 `tools/audit_unsafe.py`（反引号内含路径前缀，不被正则收入，属预期）。缺 `python3` 时走 `else` 硬失败，与 13 条 python 审计腿的 fail-closed 同语义，**不**加 `command -v` 守卫（P-46 同族）。
  - **结构锁**: `test_p69_leg_manifest_is_reconciled_against_agents`（`host-tests/tests/ci_entrypoint_unified_test.rs`，套件 19 → **20 passed**，5 臂）—— ① 对账块存在且文案点名 §2.2 / P-69；② 块内同时把 `AGENTS.md` 与两档清单传入（少传任一即退回自参照）；③ 切片字面量 `### 2.2` / `### 2.3`、正则 `(?:audit|scan)_` 与两个前缀字符串齐备；④ 出口是 `err "P-69:` 且前置无 `command -v python3` 守卫；⑤ 位置在两档清单定义之后、`step "0/6` 之前 —— 挪到任一门禁之后即红。
  - **负控制（两组运行时 + 三形态结构，测毕即复原，`diff -q` 逐次核对）**: **运行时** ① 从 `AUDIT_REQUIRED_LEGS_FULL` 摘 `missing_docs_debt:audit_missing_docs_debt.py` ⇒ full 首两行内（0.031s）转红、RC=1、文案点名 `audit_missing_docs_debt`（对照：修复前同形态仍打 `✓ P-49 存量审计腿到位: 8/8`）; ② 从 `AUDIT_REQUIRED_LEGS` 摘 `coupling` ⇒ RC=1、点名 `audit_coupling`。**结构** ③ 删整块 / ④ 整块挪到 `step "0/6` 之后 / ⑤ 把 `### 2.3` 改 `### 2.4` ⇒ 三次均 **0 passed / 1 failed**，复原后 **20 passed / 0 failed**。
  - **台账同步**: `net-e2e-tcp-echo-flakiness.md` P-69 状态 `[ ]` → `[X]` 并写结案行；口径 A 实测 **64 条 = 48 `[X]` / 15 开放（12 `[ ]` + 3 `[]`）/ 1 `[~]`**（P-69 转 `[X]` ⇒ 开放 16→15、总数 64 不变）；`docs/plan/progress-active-tasks.md:526` 四处对齐（计数行、实测行、轮次叙述、§6 件数「十六件」→「十五件」）。
  - **门槛读数（本窗口同一棵树逐条实测）**: 门槛 1 `./ci/build.sh all` **RC=0**（`Passed: 6 / Failed: 0`，日志 `warning:` 计数 **0**）；门槛 2+3 `bash ci/audit.sh full` **RC=0**（`✓ 25 条文档点名腿全部在册`、`✓ P-69 腿清单与 AGENTS §2.2 反向对账: 通过`、`✓ 审计腿到位: 19/19`、`✓ P-49 存量审计腿到位: 9/9`、QEMU `2/2`、`本轮因环境缺失未执行腿数: 1` = lockbud 工具链不匹配，本机常态）；门槛 4 `make test-host` **RC=0**（**1105 passed / 0 failed**，较第二十三轮 1104 的 +1 即本轮结构锁）；门槛 6 `make test-kernel-host` **RC=0**（**1019 passed / 0 failed**）；第七项 `make -f Makefile.ci ci-rustdoc` **RC=0**（两架构各 exit 0）。门槛 5 即 full 内 QEMU 腿，读数已含在门槛 2+3。
  - **未达成（如实登记）**: 其一，只判「文档点名 → 清单」一向，清单有而文档未点名不判（§2.2 自述非穷举，刻意不判以免新增腿先要改文档）；其二，对账依赖 `### 2.2` / `### 2.3` 标题字面量，标题改写会让 `doc.index` 抛异常 ⇒ 仍 fail-closed（不会静默放过），但文案从「点名缺失」变成 Python 异常；其三，两处到位断言的**数值口径未改**（仍为 `清单 − 已登记`），摘腿的拦阻现在由 P-69 + host-tests 双保险承担，而非改断言本身。下一件按 §6.0 A 序列为 **P-67**。

- **本轮（第二十三轮：编号口径收口 —— P-68 结案 + 新立 P-70）**: 承用户批次裁定「严格编号序逐件」＋「AGENTS.md 全部该改就改」。起手 **P-68**（AGENTS §2.2 把 `audit_deadlock_matrix.py` 标为 F8，而 §5 的 F8 = 公共 API 文档注释；同一编号两表异义）。
  - **裁定面（一项，经 AskUserQuestion，无代答）**: P-68 路线 = **甲・改列 + 进脚注**（推荐项被采纳）。§5 的 F1–F9 一字节未动 ⇒ 零执法面变化。
  - **施工形态**: `AGENTS.md` **2 处具名改动**（本批授权的**第 13 次**修改）：§2.2 表行 `audit_deadlock_matrix.py` 的「对应 §5 硬规则」由 `F8` 改为 `附加门禁（锁顺序/中断上下文专项，P-68）`（`:52`）；§2.2「附加门禁」脚注把该脚本并入清单，并把「内存/风格专项防线」扩为「内存/风格/并发正确性专项防线，映射 I2/I5/I6 不变式与 `docs/explain/ref-lock-order.md` 的锁序约束」（`:65`）。同源文案三处归位（零执法变化）：`ci/audit.sh` 段头 `0.5k` 与失败文案各一处改述为「F1+F2 / F4 + 锁顺序专项」；`host-tests/tests/ci_entrypoint_unified_test.rs` 文件头与 `test_audit_sh_carries_hard_rule_audits` 注释各一处去掉 F8 表述。
  - **结构锁**: `test_p50_audit_scope_and_selftest_are_wired` 的 §2.2 needle 清单新增 `"附加门禁（锁顺序/中断上下文专项，P-68）"`（1 条），使「改回 F8」即红；套件仍 **19 passed / 0 failed**。
  - **负控制（一组，测毕即复原）**: 临时把 §2.2 该行回退为 `F8` ⇒ `cargo test --test ci_entrypoint_unified_test test_p50_audit_scope_and_selftest_are_wired` **0 passed / 1 failed / RC=101**，`panicked at tests/ci_entrypoint_unified_test.rs:622` 点名缺失 needle；`cp` 复原后 `diff -q` 一致、套件 **19 passed**。
  - **本轮新立一件（立案零施工）**: **P-70** —— `docs/explain/spec-engineering.md:15` 自述「6 条零容忍规则 F1…F6」，F6 = 三审计（含 `audit_deadlock_matrix`），与 AGENTS §5 的 F1–F9 不同源且把 deadlock 归入 F6，即 P-68 同一二义的**第二个副本**；该文属 must-sync 面，而 `tools/check_doc_paths.py` 只核路径新鲜度、覆盖不到规则编号表。三路线（甲 单源化指向 AGENTS §5 / 乙 同步补写到 F1–F9 / 丙 只写历史快照口径）待裁。普查边界：只实测这一处，其余文档未测。
  - **台账同步**: `net-e2e-tcp-echo-flakiness.md` P-68 状态 `[ ]` → `[X]` 并写结案行；口径 A 实测 **64 条 = 47 `[X]` / 16 开放（13 `[ ]` + 3 `[]`）/ 1 `[~]`**（P-68 转 `[X]` + 新立 P-70 ⇒ 开放 16→16、总数 63→64）；`docs/plan/progress-active-tasks.md:526` 总览行与 §6 cell 已对齐（该表行 GFM code span 内不得出现竖线，本轮复核仍为 8 cell）。
  - **引用副作用**: `ci/audit.sh` 行数不变（744 行，本轮只改两处文案）⇒ 第二十二轮写下的 `:NNN` 仍有效；`AGENTS.md` 行数不变（2 处原地改述）。
  - **门槛读数（本轮末尾同一棵树逐条实测）**: 门槛 1 `./ci/build.sh all` **RC=0**（`Passed: 6 / Failed: 0`）；门槛 2+3 `bash ci/audit.sh full` **RC=0**（`✓ 审计腿到位: 19/19`、`✓ P-49 存量审计腿到位: 9/9`、QEMU `2/2`、台账 `本轮因环境缺失未执行腿数: 1` = lockbud 工具链不匹配；`audit_dead_code_comment` 死代码抑制 0 处、clippy 抑制与侧车基线一致）；门槛 4 `make test-host` **RC=0**（**1104 passed / 0 failed**，较第二十二轮 1084 的 +20 来自本批管道阻塞工程的新回归）；门槛 6 `make test-kernel-host` **RC=0**（**1019 passed / 0 failed**）；第七项 `make -f Makefile.ci ci-rustdoc` **RC=0**（两架构各 exit 0）。门槛 5 即 full 内 QEMU 腿，读数已含在门槛 2+3。
  - **未达成（如实登记）**: 本轮只收编号口径，**不碰** `audit_deadlock_matrix.py` 的检测强度与 Lockbud 腿去留（归 P-56）；`spec-engineering.md` 的 F 表未同步 ⇒ P-70；P-69（full 档到位断言自参照）仍开放。

- **本轮（第二十二轮：F8 口径收口 —— P-59 结案 + 新立 P-66/P-67/P-68/P-69）**: 承用户批次裁定「严格编号序逐件」＋「AGENTS.md 全部该改就改」。起手 **P-59**（P-40 甲 **第十三** 次让位）。**本日志缺 第十三–二十一轮（P-50…P-58）的条目** —— 那九轮的读数与裁定写在 `net-e2e-tcp-echo-flakiness.md` §6 各行的 状态/详情 里，未回抄本日志；按 §6「历史记录不改写」**不补造**，本轮起继续写本日志（先例见第十二轮条目末段的同一登记）。
  - **裁定面（四项，全部经 AskUserQuestion，无代答）**: ① F8「公共 API 中文文档」的**存在性**由谁作准 = **rustc `missing_docs`**；② 脚本 `--strict` 的 533 处 = **基线冻结升阻断**；③ 行号偏到空行的检测缺陷 = **本轮顺带修**；④ 落地形态用户回「**a，但存量不应冻结，既然会报那就属于代码仍有问题**」⇒ 侧车基线的定性是**待办清单而非豁免面**：每批补写完即把该模块转 `#![deny(missing_docs)]` 硬执法，清零工程另立 **P-66**（本裁定否掉了我自己推荐的「冻结存量」默认项，属用户对判据强度的加压，不是施工路线自选）。
  - **立案前提两处订正（实测推翻第十二轮原文）**: 其一，原文称「§2.3 门槛 2 的**真实** F8 执法 = clippy `missing_docs_in_crate_items`」—— 该 lint 名**不存在**（`cargo clippy -D clippy::missing_docs_in_crate_items` 实测 `error[E0602]: unknown lint`），真名 `clippy::missing_docs_in_private_items` 属 **restriction 组**且门槛从不启用 ⇒ kernel crate 的编译器侧 F8 执法长期为**零**，`src/rust/clippy.toml` 里的 `missing-docs-in-crate-items = true` 是**惰性配置**（本轮已在该键处标注，不删以免与上游配置面混淆）；启用 rustc `missing_docs` 后全口径实测 **4582 处**，本腿射程（kernel `--lib` x86_64、排除 vendored smoltcp）= **4568 处**。其二，原文把 533 处称作「trait impl 方法」，实测**全是 inherent impl 方法**（trait impl 命中 **0**）—— 即第十二轮据此写的「继承 trait 文档所以不算违规」这条定性理由也不成立。
  - **施工形态**: `scripts/audit_public_api_docs.py`（**457 行**）**删除 `--strict` 旗标**（现为未知参数 ⇒ argparse RC=2），改为一次扫描派生两面 —— `:226` 给命中项打 `in_impl` 标记、`:248-249` 派生 `default_violations`（恒**硬零**、不进基线）与 `strict_violations`（= 533 处，与机器派生侧车 `scripts/audit_public_api_docs.baseline.json` 双向对账）⇒ 2e 的**无参裸调**同时判两面。此形是裁定 ② 的唯一可行落地：CI 按 `python3 scripts/<腿>.py` 调起是 P-49 结构锁钉死的形态，靠 flag 分流则 impl 面在门禁里永远没有执法点。新增第 **9** 条腿 `scripts/audit_missing_docs_debt.py`（**239 行**）= 编译器口径债册棘轮：`cargo clippy --release --lib --target x86_64-unknown-none --message-format=json -- -Dmissing_docs`（`:62`）取数、按 (文件, 项类型) 记数、与 `audit_missing_docs_debt.baseline.json` 双向对账（上升⇒红、下降未重导⇒快照过期红），三值退出码且取数失败/基线不可用一律 **fail-closed**；**首跑债册 4490 处 / 239 文件 / 650 键**（`privileged` 2437 + `functions` 2053；单文件最重 `privileged/syscall/types.rs` 260）。裁定 ③ 的修法 = 行首空白只吞 `[ \t]` 不吞 `\n` ⇒ **247 条错锚归位、命中数不变（仍 533）**。裁定 ④ 的第一批真补写 = `src/kernel/privileged/errno.rs` 的 **78** 个 variant 补 `///` + 文件级 `#![deny(missing_docs)]`（`:20`，diff 净 +81）⇒ 债册 4568 → **4490**。`src/kernel/build.rs` 加永久 crate 级 `//!` 是**取数前置而非风格改动**：尾旗标同时作用到 build script 这个 host 侧 crate，它缺 crate文档即**编译中止**（实测 `error: could not compile kernel (build script) due to 1 previous error`）⇒ lib 的诊断一条也拿不到；另实测 `cargo check … -- -D…` / `-W…` 在本仓被拒（`error: unexpected argument`）⇒ **只有 `cargo clippy` 转发尾旗标**，取数命令因此固定为 clippy，且必须用**独立 `--target-dir`**（尾旗标参与 cargo 单元指纹，借主 target 会与门槛 1 互相反复重编）。
  - **CI 与文档面**: `ci/audit.sh` 728 → **744 行**，`AUDIT_REQUIRED_LEGS_FULL`（`:80`）追加 `missing_docs_debt:audit_missing_docs_debt.py` ⇒ 2e 现 **9/9 全阻断**；耗时注释改本轮实测（九腿连跑 36.968s，逐腿 1.8/6.6/0.1/0.1/0.4/7.8/15.8/3.7/1.0，实测时 load avg ~17 ⇒ 只有量级可比）。**本批 AGENTS 授权的第 12 次修改（本轮 3 处）**：§2.2 表新增 `audit_public_api_docs.py` + `audit_missing_docs_debt.py` 行（F8・强制（full 档），`:62`）、§2.2「完整清单」脚注 26 → **27** 个 `audit_*.py`（`scripts/` 24 + `scripts/tests/` 2 + `tools/` 1，`git ls-files` 口径实测：新增文件须 `git add` 后才进 tracked 计数 ⇒ 与提交同批核对，`:66`）、§5 F8 的「检查方式」由不存在的 clippy lint 改为**两层链**（存在性 = rustc `missing_docs`，中文语言面 = `audit_public_api_docs.py`，`:135`）。另 `docs/explain/spec-engineering.md` 4.4 重写 + 10.2 指向 4.4、`docs/plan/framekernel-paradigm-enforcement.md` 语义口径条目末**追加** P-59 订正（历史文本不改写）。**§2.2 里 `audit_deadlock_matrix.py` 那行仍标 F8**（与 §5 的 F8 不同义，属编号真空）**本轮刻意未动** ⇒ 新立 **P-68**。
  - **结构锁**: `test_p59_f8_is_enforced_by_rustc_missing_docs`（`host-tests/tests/ci_entrypoint_unified_test.rs:1299`，套件 18 → **19**，5 组）—— ① AGENTS `F8` 行 + spec 4.4 + clippy.toml 三处口径一致且不指向不存在的 lint；② 脚本无 `--strict`、两面派生齐备、基线 fail-closed、裁定 ③ 的正则字面量在位；③ 债册腿 `LINT = "missing_docs"` + `-D{LINT}` + `--message-format=json` + smoltcp 排除 + 独立 target 目录（且禁止借主 target）+ `ScanError`/`return 2`；④ `errno.rs` 已带 `#![deny(missing_docs)]` 且**不出现在债册里**（补写与硬执法必须同落，否则「清零」只是快照搬家）；⑤ `AUDIT_LEG_FULL_TOTAL` 由清单派生（`set -- $AUDIT_REQUIRED_LEGS_FULL; echo $#`）且 P-49 锁的阈值与点名腿名同步（第 9 条具名断言 `:425`）。
  - **负控制（七组，测毕即还原，逐次 sha256 / `git status --porcelain` 核对）**: **C1** 注入未文档 impl 方法 ⇒ `public_api_docs` RC=1（新增 1 / 基线 0，impl 面 534 vs 533）**且** `missing_docs_debt` RC=1 ⇒ 两面同时拦住新增；**C2** 补写未重导 ⇒ 两腿各报「快照过期」，`--update-baseline` 各 RC=0，重导后对账 RC=0 且读数 **532 / 4489** ⇒ **棘轮闭环端到端可跑**（P-66 每批收尾动作即此形状）；**C3a** 侧车缺失 ⇒ 两腿 RC=2、**C3b** 手改 `totals` ⇒ RC=2「声明 vs 实算不自洽」；**C4** 剥 `errno.rs` 的 EPERM doc ⇒ **不带本腿的门槛形 clippy 就 RC=101 `error: missing documentation for a variant`** ⇒ 模块级 `#![deny]` 由门槛 1/2 直接硬执法，不依赖新腿；**C5a（关键边界，只记不修 ⇒ P-69）** 从 2e 清单摘掉本腿 ⇒ 运行时到位断言仍打 `✓ P-49 存量审计腿到位: 8/8 (无静默跳过)`、桩 harness `LEG_ERRS=0` ⇒ **该断言自参照**（拿清单比对「遍历同一清单产生的 mark」），整腿摘除在 runtime 侧零可见，真实执法点是 host-tests 的计数 + 命名锁；**C5b** 腿脚本文件缺失 ⇒ `✗ 审计腿未执行: missing_docs_debt —— … 不存在 (fail-closed)` ERRS=1；**C5c** 侧车缺失 ⇒ 腿 RC=2 ⇒ 循环阻断，还原后 `[OK] …: 通过` ERRS=0；**C6-i…iv** 四条锁臂各转红一次（回退 §5 F8 文案 `:1311`／正则退回 `^\s*` `:1350`／从清单摘腿 `:406`／债册腿改借主 target `:1370`），全部 `1 failed` RC=101，还原后全量 **19 passed / 0 failed**。
  - **一处自锁假阳性（方法学，写给自己后续轮）**: C6 首跑「还原后」仍红在 `:1350` 且 C6-iv 被误归同一行 —— 成因是我在锁臂里**同时禁了** `r'^\s*(?P<vis>'`，而 `_in_nonpub_mod`（`scripts/audit_public_api_docs.py:413`）合法地还在用同款锚定。**锁臂写「禁止某写法」之前，必须先数该写法在受控文件里的合法出现次数。**
  - **本轮新立四件（均立案零施工，属本轮施工面测出的边界）**: **P-66** 债册 4490 处 / 239 文件 / 650 键的分批补写工程（三路线待裁，另记 `mod.rs` 子树继承这一未测项）；**P-67** 债册只看编译器读数 ⇒ `#![allow(missing_docs)]` 与真补写在门禁里**同形**，F9 射程不含 rustc lint 抑制（存量实测 8 处非 clippy 抑制、`missing_docs` 的 allow/expect = **0** ⇒ 风险登记表非现行违规；本轮**未做**该绕路的负控制，已如实写入本条）；**P-68** AGENTS §2.2 与 §5 的 F8 编号真空；**P-69** full 档到位断言自参照（C5a 的正式立案）。
  - **台账同步**: `net-e2e-tcp-echo-flakiness.md` 口径 A 实测 **63 条 = 46 `[X]` / 16 开放（13 `[ ]` + 3 `[]`）/ 1 `[~]`**（P-59 转 `[X]` + 新立四件 ⇒ 开放 13→16、总数 59→63）；`docs/plan/progress-active-tasks.md:526` 总览行与 4/§6 两 cell 已对齐（该表行的 GFM code span 内**不得出现竖线**，第八轮踩过，本轮复核该行仍为 8 cell）。
  - **引用副作用**: `ci/audit.sh` 728 → **744 行** ⇒ 第十二轮至二十一轮写下的 `:NNN` 再次偏位（本轮 grep 实测现值：`AUDIT_REQUIRED_LEGS` `:64`、`AUDIT_REQUIRED_LEGS_FULL` `:80`、`mark_leg_full` `:82`、quick 早退 `:120`、0.5n 到位断言 `:451-467`、step 2e 块 `:608`、`step "2e/6"` `:639`、腿循环 `:640-653`、`AUDIT_LEG_FULL_TOTAL` `:663`、Lockbud `:670`/`:693`、QEMU `:715`/`:718`/调用 `:726`）。按 §6 规则历史文本不改写；据此施工前一律 grep 段名定位。
  - **门槛读数（本轮末尾同一棵树逐条实测；耗时只在无并发时取）**: 门槛 1 `./ci/build.sh all` **RC=0**（`Passed: 6 / Failed: 0`）；门槛 2+3 `bash ci/audit.sh full` **RC=0**（`✓ 审计腿到位: 19/19`、`✓ P-49 存量审计腿到位: 9/9`、`✓ impl 面与基线一致 (533 处存量记账)`、`✓ 编译器口径与债册一致 (4490 处待补写)`、`QEMU 真实启动测试: 2/2 通过 (门禁模式: FAIL_OK=0)`、台账 `本轮因环境缺失未执行腿数: 1` = lockbud 工具链不匹配；**该跑与并发争 CPU，`real 7m46.358s` 只作量级**，不与第十二轮空载 167s 比）；门槛 4 `make test-host` **RC=0**（140 个 result 行 **1084 passed / 0 failed**，`real 3m39.297s`，较第二十一轮 1083 的 **+1 即本条新结构锁**，且新锁以单点 `--test` 自证过而非只认总数）；门槛 6 `make test-kernel-host` **RC=0**（**1019 passed / 0 failed**，`real 36.236s`，本轮无 P-57 记的 timing flake 复发）；第七项 `make -f Makefile.ci ci-rustdoc` **RC=0**（两架构各 `rustdoc exit: 0` + `PASS: rustdoc 0 源码级 link 错误 + 0 私有项链接`，`real 22.152s`）。门槛 5 即 full 内 QEMU 腿，读数已含在门槛 2+3，不另计。

- **本轮（第十二轮：门禁治理按编号序收口，P-49 六脚本接线结案 + P-58/P-59 立案）**: 承用户批次裁定「严格编号序逐件」＋「AGENTS.md 全部该改就改」＋「推到 W-8」。起手 **P-49**（P-40 甲 第四次让位）。
  - **裁定面（九项，全部经 AskUserQuestion，无代答）**: (a) **甲・修正则 + 自测锁**；(b) **甲・不另立案，只更新 Z 基线数字**；(c) **丙・六脚本全接入 full**；E-03 前置定性 = **丙・名单 + 15 HIGH 一并定性收口**；implicit_deps 接入形 = **甲・§7.3 口径 + 基线冻结**；feature_semantics 阻断面 = **甲・CRITICAL+HIGH 都零**；施工期追加三项 —— 基线数字一问用户回「**有没有优雅且根治的**」（是要求不是选项）⇒ 落地形 = 侧车快照机器派生，代码与文档里不再存在任何手写债数字；antx 腿 = **报告腿接入 + 另立案 P-58**；public_api_docs = **默认口径接入 + 另立 strict 案 P-59**。
  - **施工形态**: `scripts/audit_implicit_deps.py` 重写（收割正则行首锚定 + §7.3 口径复用 `audit_reverse_deps` + 侧车基线 + `--update-baseline`；性能 59.0s → 1.13s，读数不变）；`scripts/tests/audit_implicit_deps_selftest.py` 新建 14 用例并**作为第 7 条腿接入**；`scripts/audit_feature_semantics.py`（+117/−34，2 CRITICAL 定性为脚本角色表错归、15 HIGH → 11 键/13 站点计数钉死注册表、修一处行号失真）；`ci/audit.sh` 584 → **648 行**（`AUDIT_REQUIRED_LEGS_FULL` `:68` + **step 2e** `:532` + full 档到位断言 `:563`，每腿 `cd $PROJECT_ROOT` 子 shell 调起）；`host-tests/tests/ci_entrypoint_unified_test.rs` +139（新锁 `test_audit_sh_full_only_legs_are_wired` 六臂）；`AGENTS.md` §2.2「完整清单」行 1 处（批次授权内、具名）。
  - **数字链（债一处未减，变化全部来自尺子变准）**: 122（2026-09-13 登记）→ 67（旧检测器 = 40 生产 + 17 正则伪命中 + 10 测试上下文）→ **48 处 / 18 项 / 17 文件 / 11 全局**。立案时我写的「40 处」在新检测器下不成立 ⇒ 已在 P-49 详情与本册条目 3 同步订正，取向是「把尺子做准 + 数字单源」而非把旧数字圆过去。
  - **负控制（九组，测毕即复原并逐次 `diff -q`）**: NC1/NC1′ 正则回退 ⇒ 自测 4 臂红 + 腿报 57 vs 48；NC2/NC3 篡改快照 ⇒ fail-closed；NC-A 行号真值 ⇒ 报 879/880 与真实 478/479 不符；NC-B 用 HEAD 形态整脚本 ⇒ **RC=1，2 CRITICAL / 15 HIGH / 6 INFO**（现 RC=0，19 INFO）；NC-C 删注册表项 ⇒ 该站点回 HIGH；NC-D 伪造注册表项 ⇒ STALE 红；新结构锁六臂各转红一次（删自测腿 `:300`／路径 typo `:326`／报告腿标注去「不判退出码」`:388`／豁免去真实腿名 `:396`／阻断腿去锚定 `:367`／2e 整块挪到 quick 早退前 `:343`，六臂一律 `9 passed; 1 failed` RC=101）。
  - **本轮自造债三处、当场消除**: ① 我在 `ci/audit.sh` 注释与 `framekernel-paradigm-enforcement.md` 各写进一个裸词 `antx`，`--actionable-only` 读数当场 53→54→55；两处改写措辞后回到 53。该现象本身即 **P-58** 的主体证据（扫描器把自己与一切文档性提及计入 actionable）。② 新锁手写未过 `rustfmt` ⇒ `bash ci/audit.sh quick` **首跑 RC=1**，红在 `2c/6` 而非新腿（本册第八轮已写下「改过源码先 `cargo fmt` 再跑门槛」这条次序纪律，本轮仍未遵守 ⇒ 属同一失误复发）。③ 为消 ② 而跑的 `cargo fmt` 重排了断言换行 ⇒ 初稿那六个结构负控制的 `panicked at :NNN` 行号全部失真，已重跑取数并作废旧值（见下方 负控制 项）。三处都在本轮内闭环，未留到下一轮。④ **同轮内即被工具口径反噬一次（只登记，不改写）**：为本轮新立的 P-58/P-59 撰写台账文本之后重跑 `scan_antx_residue.py --actionable-only`，读数由 **53 → 65**（自命中 46 处不变，外部 7 → 19，新增 12 处全来自这两段描述该问题的文档）。⇒ 本册与 `net-e2e` 里出现的「53 处」一律读作**接线时刻快照**，不是当前可复现值；**同轮内第二次漂移**：写「解释这个现象」的那段文本又把 65 推到 **69**（自命中 46 始终不变，本册 2 处未变、`net-e2e` 10→14）⇒ 该口径下读数不可收敛，此后只用带时刻的数。该现象正是 P-58 的主体主张，已补进其 描述 末段。
  - **台账同步**: `net-e2e-tcp-echo-flakiness.md` 口径 A 实测 **53 条 = 36 `[X]` / 16 开放（13 `[ ]` + 3 `[]` 第 6 形态）/ 1 `[~]`**（P-49 转 `[X]`、新立 P-58/P-59 ⇒ 开放 15→16、总数 51→53）；`docs/plan/progress-active-tasks.md` 总览行与之一并对齐。**另记一处日志缺口（本轮只记不修）**: 本册 变更历史 缺 **第十轮（P-47）** 与 **第十一轮（P-48）** 两条 —— 那两轮的读数写在 `net-e2e` §6 各行的 状态/详情 里，未回抄本日志。按 §6「历史记录不改写」不补造，此后每轮继续写本日志。
  - **引用副作用（`ci/audit.sh` 584 → 648 行，第九/十一轮实测写的 `:NNN` 全部偏位；施工前一律 grep 段名定位）**: `audit_leg_missing()` `:38`→**`:45`**；`AUDIT_REQUIRED_LEGS` `:52`→**`:59`**；0.5n 到位断言 `:346`→**`:389`**；clippy 步骤 2 头 `:377`→**`:420`**；quick 早退 `:445`→**`:488`**；**新增** step 2e `:532`（到位断言出口 `:563`）；步骤 3 `:469-484`→**`:567-581`**；Lockbud `:486-497`→**`:593-607`**；步骤 6 `:502-510`→**`:613-620`**；QEMU 腿 `:515-536`→**`:625-645`**。
  - **附带发现（只报告，未立案、未处置 —— §11.5 工程外问题）**: 测绘扫描器文件集时发现 `scripts/.tmp_slaac_smp1.sh` （7267 B，可执行位已置）是**被提交的临时脚本** —— 全仓（除 `other/` 与 `.git/`）对它的引用**零条**，其历史两次提交为 `8fb80544`（IPv6 SLAAC 联调）与 `e7936519`（P-10 产物分目录），文件名自述即 `.tmp_…1` 属一次性插桩产物。它不在任何门槛里（既不被 `ci/*.sh` 调起，也不在 `audit_*.py` 扫描根的判据内 ⇒ 只被命名残留扫描器按 `.sh` 收进文件集）。性质与 F9 死代码同源（零调用者的受控文件），但删它是**一次性 `git rm`**、不属于 P-49 的接线面 ⇒ 待用户定：直接删 / 归档到 `docs/plan/archive` 式的历史位 / 立案 P-60。
  - **门槛读数（本轮末尾在同一棵树全量复跑，逐条实测）**: `./ci/build.sh all` **RC=0**（158s，`Passed: 6 / Failed: 0`，rustc `warning:` **0 行** / `error` **0 行**，日志仅两行 cargo future-incompat 元信息 `core`/`sha2`）；`bash ci/audit.sh full` **RC=0 167s**（无并发复跑值）—— 同一日志里两行到位断言齐备：`:256` `✓ 审计腿到位: 18/18 (无静默跳过)`（P-46 乙）与 `:412` `✓ P-49 存量审计腿到位: 7/7 (无静默跳过)`（本轮新立），五条阻断腿各打 `✓ 审计腿 …: 通过`、报告腿打 `✓ 报告腿 …: 读数已播报 (不判退出码, 见 P-58)`，`2c` 四 crate fmt 全通过、`2d` release compile-only 通过、末腿 `✓ QEMU 双架构启动测试: 2/2 通过`、收尾台账 `本轮未执行腿数: 1`；`bash ci/audit.sh quick` **RC=0 105s** 且对段名 `2e/6` **零命中** ⇒ full-only 覆盖边界为实测而非宣称；`make test-host` **RC=0**（140 个 result 行，**1074 passed / 0 failed**，124s，较上轮 +1 即本条新锁）；`make test-kernel-host` **RC=0**（**1019 passed / 0 failed**，20.34s）；`make -f Makefile.ci ci-rustdoc` **RC=0**（10s，两架构各 `rustdoc exit: 0` + `PASS: rustdoc 0 源码级 link 错误 + 0 私有项链接`）。**两条自我更正一并落档（不静默改写）**：① 本批 `full` 首跑读数 260s 系与并发任务争 CPU 所致，无并发复跑 167s ⇒ 耗时只取无并发值；② quick 收尾打 `✓ 本轮未执行腿数: 0`，但同档结构性地不含 2e 的七条腿 ⇒ skip 台账只记「探到工具却没跑成」的腿，「本档不跑」的腿不进台账，此为 **P-57** 的具体读数（本册 变更历史 里 P-57 的既有描述据此从推断升级为实测）。

- **本轮（第九轮：门禁治理按编号序收口，P-46 fail-closed 结案 + P-55 立案）**: 用户令「继续推进」，按上轮刚立的读法解为**台账编号序** ⇒ 起手 **P-46**（P-40 甲 第三次让位）。路线与判据经 AskUserQuestion 三项裁定，不代答：① **丙・甲+乙双保险**；② `command -v python3` 为假时**一律硬失败**（缺解释器与缺脚本同判据，只区分文案）；③ 回归锁 = **结构锁 + 手工行为读数**。本轮 `AGENTS.md` 零改动（无授权，也不需要——只补执法路径，不动任何宣称）。
  - **施工形态**（[ci/audit.sh](../../ci/audit.sh) 净 +87 行）：**甲** = `err()` 之后新增共用出口 `audit_leg_missing()`（现 `:38`，两成因各打一行再 `exit 1`），13 处 `if command -v python3 … && [ -f 脚本 ]` 包装各补 `else audit_leg_missing "<腿名>" "<路径>"`（0.5j / 0.5k 两处循环腿传 `$MEMSAFETY_AUDIT` / `$HARD_RULE_AUDIT`）；**乙** = `AUDIT_REQUIRED_LEGS` 18 项（现 `:52`）+ 每腿 `mark_leg` 登记 + 新增 **0.5n 到位性断言**（现 `:346`），断言点同样置于 quick 早退分支（现 `:445`）**之前** ⇒ 两档同判。刻意不做的事：不改任何脚本判据、不把 0.5n 变成「期望腿数」的手写魔法数（清单以命名数组承载，由结构锁臂②③双向约束）。
  - **回归锁形态**（`host-tests/tests/ci_entrypoint_unified_test.rs` +82 行，新测试 `test_audit_sh_audit_legs_are_fail_closed` 三臂）：臂① 「`command -v python3 >/dev/null 2>&1 && [ -f` 出现次数」== 「以 `audit_leg_missing ` 开头的出口调用行数」⇒ 新增包装忘补 else 直接红；臂② `AUDIT_REQUIRED_LEGS` 行必须存在且 ≥18 项、每项须由 `mark_leg "name"` 或 `for … in …; do` 列表登记；臂③ 每项须映射到真实存在的 `../scripts/audit_<name>.py` 或 `../tools/audit_<name>.py`。踩到的一处实现细节：臂② 与 for 列表比对时末项 token 带分号（`static_mut;`），须 `trim_end_matches` 去非名字字符，否则误报未登记。
  - **六组负控制（每组测毕即复原，`diff -q` + `git status --porcelain` 逐次核对）**：**A/B/B2** 三臂各回退一处 ⇒ 依次 `panicked at ci_entrypoint_unified_test.rs:206`（「13 处包装但只有 12 处硬失败出口」，`6 passed; 1 failed`，RC=101）／`:215`／`:243`；B2 首跑**误命中**——替换字符串跨到了 B 刚删的清单行，`assert count==1` 先抛错，那次实际复现的是 B，先复原再取清单内子串才是真 B2。**C（甲的行为证明）** 改名 `scripts/audit_repr_c.py` ⇒ `bash ci/audit.sh quick` **EXIT=1** 且点名 `✗ 审计腿未执行: repr_c —— …/audit_repr_c.py 不存在 (fail-closed, 不接受假绿)`，即 P-46 原文那条「静默消失且 EXIT=0」的腿。**D（乙的行为证明）** 删一处 `mark_leg "once_cell"`（脚本仍在、该腿照常执行，仅登记缺失）⇒ **EXIT=1**「`✗ 审计腿未到位: once_cell —— 共 1 条`」。**E（两成因）** 用 `awk` 从仓内抽出真实函数体（不重写副本）在 `PATH=""` 下调用两次 ⇒ `环境无 python3` 与 `/x/audit_repr_c.py 不存在` 各 **RC=1**。
  - **一处必须写下的限度（据此立案 P-55 而非自称「假绿已根治」）**：双保险的面只到 `python3` 那一族 ⇒ **外部工具型腿两条都不拦**。实测① QEMU 腿（现 `:515-536`）由 `command -v qemu-system-*` 把住、`else` 只打黄字 ⇒ 提取真实块 + 显式空目录 `PATH=/tmp/p55bin` 后该腿**整条消失且 RC=0**，即 §2.3 门槛 5 在无 qemu 的环境可静默不执行（本机 qemu 已装、本轮 `full` 实测 `2/2 通过` ⇒ 定性为**环境依赖型潜在失效**，不是当前假绿）；实测② 同腿在 `set -euo pipefail`（`:9`）下 `if <脚本> 2>&1` 接 `tail -6` 管道，进入 `then` 时 `PIPESTATUS[0]` 必为 0 ⇒ 其 `err "…失败 (exit=$QEMU_RC)"` 分支（`:525`）**永不可达**，真失败一律落 `:528` 打「执行异常」⇒ 退出码仍 fail-closed 但**成因写丢了**（最小同形复现脚本实测走 ELSE）。Lockbud 腿同族但**已由 P-47 立**，不双计。P-55 给甲/乙/丙三路线待裁，本轮**零施工**。
  - **方法学三条（写给自己后续轮）**：① 模拟「可执行文件缺失」须用**显式空目录**作 PATH 并给解释器绝对路径——第一次探针用正常 PATH ⇒ 直接把真实 QEMU 腿又跑了一遍（分钟级，已中止）；② 从 `ci/audit.sh` 抽块做探针时 `sed` 范围必须含闭合 `fi`（`515,534` 少一行 ⇒ 语法错误），落盘后先 `bash -n` 自检；③ `pgrep -f '<模式>'` 会匹配到发起该命令自身的 shell wrapper，判「无残留进程」不可单用该式。
  - **一处上轮自造缺陷的修复**：第八轮我在 `progress-active-tasks.md` 总览表里写的 `OUT=$(python3 …) && RC=0 || RC=$?` 中，GFM 表内 code span 的竖线**仍会切 cell** ⇒ 该行分列由 8 变 10。本轮改写为无竖线表述并复核该表 21 行字段数一致（现全为 8）。
  - **门槛读数（本轮全量复跑，逐条实测）**：`./ci/build.sh all` **RC=0**（2m41.063s，两架构都链接；日志仅两行 cargo future-incompat 元信息 `core`/`sha2`，非项目代码告警）；`bash ci/audit.sh full` **RC=0**（2m44.918s）且 `✓ 审计腿到位: 18/18 (无静默跳过)` 见日志 `:255-256`，同批步骤 2/2b/2c/2d 与 7/7 逐腿真跑（QEMU `2/2`）；`bash ci/audit.sh quick` **RC=0**（1m29.843s，同一行 `18/18`）；`make test-host` **RC=0**（**1071 passed / 0 failed**，2m5.078s——该 recipe 走 `cargo test --quiet` 不打印套件名 ⇒ 新锁是否真跑另以 `cargo test --test ci_entrypoint_unified_test` **7 passed** 单点自证，不把「总数涨了」当证据）；`make test-kernel-host` **RC=0**（**1019 passed / 0 failed**，20.514s）；`make -f Makefile.ci ci-rustdoc` **RC=0**（9.398s，两架构 `rustdoc exit: 0` 同时打出）。顺序沿用第八轮定下的**先 fmt 后跑门禁栈**（`cargo fmt --manifest-path host-tests/Cargo.toml` 本轮 0 漂移）。
  - **台账同步**：`net-e2e-tcp-echo-flakiness.md` 口径 A 实测 **49 条 = 33 `[X]` / 15 开放（12 `[ ]` + 3 `[]` 第 6 形态）/ 1 `[~]`**（P-46 转 `[X]`、新立 P-55 ⇒ 开放件数 15 不变）；`docs/plan/progress-active-tasks.md` 总览行与之一并对齐。**引用副作用**：本轮使 `ci/audit.sh` 由 450 → **537 行**，本册与 `net-e2e` §6 里按第六/八轮实测写的 `:NNN` 再次偏位（现值：quick 早退 `:336`→`:360`→**`:445`**、clippy 头 **`:377`**、步骤 3 **`:469-484`**、Lockbud **`:486-497`**、步骤 6 **`:502-510`**、QEMU 腿 **`:515-536`**）；按 §6 规则历史文本不改写，对照表随 P-46 详情留档，据此施工前一律 grep 段名定位。

- **本轮（第八轮：门禁治理按编号序收口，P-45 接线结案 + P-54 立案）**: 用户令「授权一次改动，按序推进」。落点与授权面经 AskUserQuestion 取得，不代答：① 本轮收 **P-45・接线**（读「按序」为台账编号序，故 P-45 起手，P-40 甲 让到其后）；② **本次授权不含 `AGENTS.md`** ⇒ 本轮 `AGENTS.md` 零改动（文档保护继续生效）。
  - **施工形态（只补执法路径，不改判据）**：[ci/audit.sh](../../ci/audit.sh) 在 0.5k 硬规则循环之后新增 **0.5k2** 段调 `python3 scripts/audit_coupling.py`，三处刻意选择写进段内注释——接线点置于 quick 早退分支（现 `:360`）**之前** ⇒ quick/full 两档都覆盖（脚本单跑 `real 0m0.765s`，上一轮「乙 需先实测耗时」的未测项就此闭合）；退出码取 `OUT=$(cmd) && RC=0 || RC=$?`（与 0.5i 的 FP-06 腿同写法，**不走管道** ⇒ 不依赖 `pipefail`、也不可能把退出码取自 `tail`/`grep`）；**刻意不加** `if command -v python3 && [ -f … ]` 存在性包装，与 P-46 那 13 条腿的形态相反。`scripts/audit_coupling.py` 一个字节未动（判据强度属另一件事，见 P-54）。
  - **回归锁**：`host-tests/tests/ci_entrypoint_unified_test.rs::test_audit_sh_carries_hard_rule_audits` 的断言清单加入 `coupling`（该锁既有的失败文案「§2.2 宣称的强制门禁与本地入口分叉」正是本条病灶的表述）。
  - **四组负控制读数**：**A 回退接线**（把 `ci/audit.sh` 换回 HEAD 形态）⇒ 该测试 **FAILED**，`5 passed; 1 failed`，`panicked at ci_entrypoint_unified_test.rs:172`，RC=101；复原 ⇒ `6 passed`。**B1 内部访问判据**（探针 `src/kernel/privileged/mm/negctl_probe.rs` 一行 `use crate::privileged::sync::raw;`）⇒ 「⚠ 1 处跨子系统内部访问违规」⇒ quick **EXIT=1**。**B2 环阈值判据**（同探针加 7 行 mm→proc）⇒ `mm ↔ proc` 由 `2/12 (合计 14)` 变 `9/12 (合计 21)` 越过 `> 20` ⇒ quick **EXIT=1**。**B3 缺脚本对照**（移走 `scripts/audit_coupling.py`）⇒ 新腿 `python3: can't open file` 后 **EXIT=1**，而同一场景下 P-46 的 13 条腿会静默消失且仍 EXIT=0 ⇒ **本条为 P-46 修法提供在仓先例**。探针文件不在任何 `mod.rs` 声明中 ⇒ rustc 永不可见，零构建影响；四组结束后 `git status --porcelain` 只剩本轮两份受控改动。
  - **一处必须写下的限度（立案 P-54 而非自称根治）**：**B4** 只加 **1** 行 mm→proc 边（合计 15 ≤ 20）⇒ 脚本 **EXIT=0**「耦合审计通过」，`⚠ … 必须解耦` 只打印不阻断 ⇒ F3 的字面「**新增**代码禁止引入模块间循环依赖」在本腿**仍未被拦住**。据实读源另坐实三条边界：`detect_circular_deps` 只吃 `fw_matrix`（`:353`）⇒ **functions 层循环依赖从不判定**，补测该层已有 1 对（`fs↔syscall` 合计 3）⇒ 全仓现存禁止环实为 **5 对**（`mm↔proc 14` / `proc↔syscall 6` / `proc↔sgeg 3` / `arch↔mm 2` / `fs↔syscall 3`），且把阈值降到 `> 0` 即刻五处红墙；`INTERNAL_PATTERNS` 实测 **38 条硬编码路径**，属 P-50 同族的清单型定点锁（防回退不防新增），而它恰是本腿唯一恒阻断的一类 ⇒ 这条腿的真实战力应读作「清单内内部访问的新增 + 合计 >20 的大环」；建边只认 `^\s*use\s+(…);` 单行（`:137`），非 `use` 行的限定调用是盲区（本仓 privileged 内 659 条 `use crate::` 全部带 `privileged::` 前缀 ⇒ use 行侧暂成立，盲区量未量化）。P-54 给甲/乙/丙/丁四路线待裁，本轮**零施工**。
  - **两处口径自纠（写给自己上一轮的读数）**：① P-45 立案时记「audit.sh 实际调用的审计脚本共 **18** 个（不含 coupling）」，本轮以 `bash -x ci/audit.sh quick` **运行时枚举**订正为 **HEAD 17 个**、接线后 **18 个（含 coupling）**；差一的成因是静态正则按脚本名去重、与 `tools/audit_unsafe.py` 是否计入两口径混用 ⇒ 该类计数今后一律以运行时枚举为准。② 本轮第一版测量脚本把 `scripts/audit_*.py` 的 `audit_` 前缀与循环展开出的裸名混比，一度得出「实际调用 0 个」的荒谬读数——发现后重写而非采用，方法与 P-45 立案轮记的「变量拼接名必须展开」同源。
  - **台账同步**：`net-e2e-tcp-echo-flakiness.md` 口径 A 实测 **48 条 = 32 `[X]` / 15 开放 / 1 `[~]`**（P-45 转 `[X]`、新立 P-54 ⇒ 开放件数 15 不变）；`docs/plan/progress-active-tasks.md` 总览行与之一并对齐。**登记取向**：本轮未另发 `AUDIT-TOOL-009`，与 P-44（同为门禁腿新增）一致只在 §6 P-NN 记一处，避免同一事实两册双计。**引用副作用**：0.5k2 段插入使 `ci/audit.sh` 该点之下的所有行号后移 ⇒ 本册与 `net-e2e` §6 里按第六轮实测写的 `:279`/`:361-375`/`:378-388`/`:397-401`/`:411-419` 等引用偏位（现分别为 `:303`/`:385-397`/`:402-412`/`:418-424`/`:430-443`）；按 §6 规则历史文本不改写，改为在 P-45 详情里留一条「据此施工前先 grep」的对照表。
  - **门槛读数（本轮全量复跑，五条逐条实测，非推测）**：`./ci/build.sh all` **RC=0**（x86_64 与 aarch64 **两架构都链接**，`[CI] ARCH=aarch64: link passed`，0 error / 0 warning）；`bash ci/audit.sh full` **RC=0**，且新增腿**真跑**于日志 `:193`（段头）与 `:199`（`✓ coupling: F3 门禁通过`），同批可见 2c 四 crate `rustfmt` 全通过、2d `scripts/check_kernel_test_release_dim.sh` 打出 `[OK] release 测试维: 编译通过 + 本 crate 零告警 (compile-only, 不作为覆盖读数)`、末腿 QEMU `2/2 通过 (门禁模式: FAIL_OK=0)`；`make test-host` **RC=0**（140 个测试套，**1070 passed / 0 failed**，含本轮加压的 coupling 一致性断言）；`make test-kernel-host` **RC=0**（**1019 passed / 0 failed**，20.50s）；`make -f Makefile.ci ci-rustdoc` **RC=0**（`x86_64 rustdoc exit: 0` + `aarch64 rustdoc exit: 0` 两行同时打出 ⇒ 两腿真跑，`PASS: rustdoc 0 源码级 link 错误 + 0 私有项链接`）。**一次自身踩坑记录**（不是门槛缺陷，是本轮施工引起的假失败）：接线后首跑 `bash ci/audit.sh quick` **RC=1** 而新腿打出 `✓ coupling` —— 真因是 2c 报 `✗ fmt: host-tests/Cargo.toml 存在格式漂移`，即我手写的 `for hard in [ … ]` 断言清单超宽未过 rustfmt；`cargo fmt --manifest-path host-tests/Cargo.toml` 后转绿。这正是「早退步骤失败会静默 forfeit 后续所有腿」的既有陷阱形状，故本轮把**先 fmt 后跑门禁栈**的顺序固定下来，症状只有 EXIT=1 时一律回查日志而非归因新腿。

- **本轮（第七轮：ci-rustdoc R-1 乙 落地，P-39 结案 + 两处新立案）**: 由用户「开始推进工程」触发，施工项按第六轮已确认的standing取向取 `docs/plan/ci-rustdoc-closeout.md` **R-1・乙**（error 与 private-link warning 一并收口 + `-D` 面加压）。
  - **本轮三次裁定**（§9.1/§11.3，均由 AskUserQuestion 取得，不代答）：① R-1 深度 = **乙**（承第六轮）；② `links to private item` 一族 = **甲・全部去链接化**（不做任何 `pub` 提升，零 API 可见性面变化）；③ cfg/架构门控链接族（`pl011_phys_base` / `VecMetaStore` / `affinity_pack` / `init::raw`，共 7 站点——同一文档在两腿可解析性不同，无论怎么修必有一腿红）= **甲・降为纯文本代码跨度**（不引入 `#[cfg_attr(…, doc = …)]`，该形态全仓零先例）。
  - **施工形态**：`src/kernel/` **69 行文档注释**（33 个 `.rs`；`git diff` 逐行判定"非 `///`/`//!` 的增删行"= 0，增删行数相等 ⇒ 无行号位移），归族 = ①路径校正 5 / ②去链接化与补反引号 27 / ③私有站点 36（35 行 + 1 行改写为可解析路径）/ cfg 门控 7 / 三条无位置 warning 的病灶 2。判据面 `-D rustdoc::broken-intra-doc-links -D rustdoc::private_intra_doc_links` 四腿同步（`Makefile.ci:118`/`:126` + `ci-lint.yml:241`/`:249`），并把 `ci-rustdoc` recipe 从"串联即中断"改为**两腿都跑完再汇总退出码**（R-5 的落地形态）。零 `#[allow(rustdoc::…)]`、零 `#[allow(dead_code)]`（验收判据 4）。
  - **一次真实拦截（一致性锁的形状得到验证）**：加压后 `./ci/build.sh all` 与 `make test-host` 同时红于 `host-tests/tests/ci_entrypoint_unified_test.rs::test_ci_rustdoc_matches_workflow_command`——它硬编码期望 `RUSTDOCFLAGS="-D rustdoc::broken-intra-doc-links"`，判据改动无法悄悄逸出该锁。同步该锁的期望字符串后 build 转 `Passed: 6 / Failed: 0`。此事实写入该册 §「关系」：**加压 `-D` 面必须同轮改五处**（Makefile ×2、workflow ×2、该锁 ×1）。
  - **口径订正（我自己立案时的读数有两处失真，已在该册 §1.2 留痕）**：「30 error + 19 warning」→ 实测 x86_64 = **29** `unresolved link` error + **16** private warning + **3** 无位置 warning（19 文件），aarch64 = **28 + 33 + 3**（21 文件）。失真成因：第 30 条 error 行是 cargo 的收尾行 `could not document`，且立案时 aarch64 腿因串联中断**从未执行**，warning 面只看见一半。R-5 的预言坐实但**方向相反**：aarch64 的私有链接面比 x86 大（33 vs 16），因 `#[cfg(target_arch = "aarch64")]` 侧的 pub 项只有在 aarch64 腿才被文档化。私有链接族去重 = **36 站点 / 18 文件 / 25 个私有符号**（台账在该册 §4.2，最密一档 `privileged/arch/aarch64/gic.rs` 7 处）。
  - **回归锁（对"根治"宣称的一贯要求：回退必须让机器判据转红）**：临时把 `functions/proc/coredump.rs:151` 恢复成 `[`do_coredump`]`、`functions/driver/usb/xhci.rs:262` 恢复成裸 `PLS[8:5] | PP[9] | PIC[14] | WCE[24]` ⇒ `make -f Makefile.ci ci-rustdoc` **RC=2**，两腿日志各自打出 `error: public documentation … links to private item`（private 已由 warning 升为 error）与 4 条 `unresolved link`，且 `x86_64 rustdoc exit: 101` 与 `aarch64 rustdoc exit: 101` **同时**打出（证明两腿真跑）；还原 ⇒ RC=0、两腿 0。故本轮是"回退即转红"的收口，不是"这轮跑过一次绿"。
  - **两处新立案（零施工，等用户裁）**：**R-7** = 三条无源码位置 warning 的病灶本轮已全部消除（`privileged/mm/pmm.rs` 的裸 `Vec<u8>`、`functions/net/smoltcp_impl.rs:29` 的 `Box<dyn Device>`、`privileged/timer/sleep.rs:11` 未标 `text` 的 ASCII 表围栏），两腿现在 0 error 0 warning ⇒ 出现"是否把 `-D rustdoc::invalid_html_tags -D rustdoc::invalid_rust_codeblocks` 也纳入阻断面"的新裁定空间（反对面的实测依据：这两条 lint 无源码位置，定位靠反推；且 vendored smoltcp 的 `///` 里也有裸 `Vec<u8>`，加压可能与"smoltcp 锁定不可改"冲突）；**R-8** = 三处文档围栏自身断点（`functions/klog.rs:169` 的 ` ```json ` 未闭合、`privileged/sync/spinlock.rs:230` 的 ` ```rust,ignore ` 未闭合且 238 行标题被吞进代码块、`functions/net/unix.rs:1049` 行尾落单反引号），无任何门禁覆盖、渲染层出错，属工程外预存问题，按 §11.5 只登记。R-6（rustdoc 进不进 §2.3）仍未动——两个形态都要改 `AGENTS.md`，而 §2.2 的一次性订正授权已在 `613b324a` 消耗。
  - **一条读数精度的观察（不另立案，归入 P-48 同类）**：首跑 `bash ci/audit.sh full` 时步骤 7/7 打 `✗ QEMU 双架构启动测试执行异常`，而同一轮 `scripts/qemu_boot_test.sh all` 单跑 **RC=0**、`2/2 通过`——根因是当时门槛 4 的那条一致性锁正红，`ensure_arch_artifacts … || RESULT=1` 被产物步翻红，而 audit 的包装把"RESULT 非 0"一律念成"执行异常"（`ci/audit.sh:411-419`）。方向是 **fail-closed 而非假绿**，同步该锁后步骤 7/7 即转 `✓ QEMU 双架构启动测试: 2/2 通过`；残留缺陷只是文案把"产物/前置失败"与"QEMU 执行异常"混为一谈，与 P-48（门槛自述与真实判据不同源）同族。
  - **台账同步**：`ci-rustdoc-closeout.md` 由 `1 [X] / 5 开放` 转为 **`5 [X] / 3 开放`**（条目数 6 → 8，含 R-7/R-8）；`net-e2e-tcp-echo-flakiness.md` 口径 A 实测 **47 条 = 31 `[X]` / 15 开放 / 1 `[~]`**（P-39 由 `[~]` 转 `[X]`，开放件数 15 不变）；总览表两行随之改写。**一处未追根因的自述漂移**：`progress-active-tasks.md` 自述行写 `75 / 0（+1 [~]）`，本轮按 口径 A（行首状态字段、含第 6 形态 `[]`）实测 **73**（宽松口径 77，差的是 4 行 `**状态**:` 表格形态 + 散文内引用），差 2 无法归到具体两行，故只记读数不改自述行，留待下轮以原始命令复现。
  - **门槛读数（本轮全量复跑）**：`./ci/build.sh all` **RC=0**（`Passed: 6 / Failed: 0`，双架构链接通过，rustc `warning:` 0 行）+ `bash ci/audit.sh full` **RC=0**（含步骤 2d release 测试维 compile-only、7/7 QEMU `2/2 通过`）+ `make test-host` **RC=0**（140 个 test target 合计 1,070 passed / 0 failed）+ `make test-kernel-host` **RC=0**（1,019 passed / 0 failed）+ `make -f Makefile.ci ci-rustdoc` **RC=0**（两腿各 `exit: 0`，日志 0 error 0 warning）；四 crate `cargo fmt --check` 0 漂移。

- **本轮（第六轮：门禁覆盖面自查 + 九件立案 P-45..P-53，零施工）**: 由用户一问（「当前项目的门禁报告或测试等是否有对某个部分豁免或不执行的吗，这涉及到项目代码健康度问题」）触发。处置经询问裁定为**「全部立案 P-NN 后逐轮收口」**；同问「下一轮施工取向要不要改」答**「仍按 ci-rustdoc R-1 乙」**（其后 P-40 甲），故本轮不动任何源码、不动任何 CI 脚本，只立案。
  - **方法与一次自我约束**： breadth 侧派两个并行调研代理（审计工具内部豁免 / 源码侧未被编译与未被执行的空间），但**每条写进台账的结论都由我另行复跑判据**，代理给出的两处数被纠正——① 它报「29 个 feature」，`[features]` 段实测 **30**（含 `default`），零 cfg 引用者实测 **11** 个而非 12（`default` 不是真 feature）；② 它把 `audit_deadlock_matrix.py` 列为「唯一点在 workflow」，实为经 `ci/audit.sh:198` 的 `audit_${HARD_RULE_AUDIT}` 循环调用（该循环展开后 audit.sh 共调用 **18** 个审计脚本）。⇒ 代理结论只能当线索，"某脚本无调用点"这类断言必须展开变量拼接后再判。
  - **九件立案（§6 P-45..P-53）与其承重判据**：**P-45** F3 指定检查方式 `audit_coupling.py` 无执行路径（§2.2 标「强制」，唯一调用在从不触发的 `ci-lint.yml:286`）——手跑 **EXIT=0**「>>> 耦合审计通过 <<<」⇒ 定性为接线缺口而非存量违规。**P-46** `ci/audit.sh` 13 处 `if command -v python3 && [ -f 脚本 ]` **无 else**；**负控制实测**：把 `scripts/audit_repr_c.py` 改名后 `bash ci/audit.sh quick` 的输出里 `repr_c` 出现 **0 次**、步骤头未打印、**EXIT 仍为 0**（文件已复原，`git status --porcelain` 空）⇒ 「全绿」与「少跑几条腿」不可区分，属 AUDIT-TOOL-004..008 那族在包装层的残留。**P-47** Lockbud 腿空转：本机 `command -v cargo-lockbud` **未安装**，而 `:382-386` 把「发现问题」与「未安装」合并为同一 ⚠ 不阻断分支；同族两条只黄不红的统计腿（`:370` SAFETY 覆盖率 `<50%`、`:397` 魔数字符串文件数 `>=5`）。**P-48** 门槛自述三处漂移：`audit.sh:3/:7` 仍宣称 Miri（`:390` 已记 2026-06-26 弃用）、`Makefile.ci:71` 调 **quick** 却注为「§2.2 落地入口/含 20+ 审计脚本」（quick 在 `:336` 早退 ⇒ 不含步骤 2d、3/4、**也不含 QEMU 即门槛 5**）、AGENTS §2.3 第 3 条判据未标档位而脚本默认 `quick`。**P-49** 六个审计脚本零调用者（`audit_implicit_deps` / `audit_feature_semantics` / `audit_reverse_deps` / `audit_public_api_docs` / `audit_smoltcp_purity` / `scan_antx_residue`），手跑读数**三绿二红**：reverse_deps / public_api_docs / smoltcp_purity EXIT=0，**implicit_deps EXIT=1（67 处）**、**feature_semantics EXIT=1（23 项 = CRITICAL 2 / HIGH 15）**；67 处经**逐名回查 privileged 是否存在 `static NAME:` 定义**拆为 **50 真引用（8 名）+ 17 伪阳性（9 个 CamelCase 类型名）**，伪阳性成因 = `audit_implicit_deps.py:63-65` 的 `static_pattern` 允许以 `{` 收尾，把 `pub fn bpf_subsystem() -> &'static BpfSubsystem {` 的返回类型收进"全局变量名"集合；50 处真引用与 `framekernel-paradigm-enforcement.md:447` 已登记的「122 处 → 批次 Z §7 基线」同源（现降为 67），故**不另立第二笔账**。**P-50** 覆盖面被读成树级防线：`repr_c` 硬编码 **2 个结构体**、`volatile_access` **4 个字段**、`tlb_receive_order` **1 文件 1 函数** ⇒ 只防"那几处被改回去"、不防新增同类；扫描根一律 `src/kernel` ⇒ `host-tests/`、`src/user/`、`kani-harness/` 不在 F7/F8/c_naming 覆盖内；`audit_selftest.py:79/:242` 自述「手动模拟其检测逻辑」⇒ 检测器正则退化时自检仍绿。**P-51** feature 构建矩阵：30 个 feature 只点亮 `kernel_test`+`host-test` 两个，28 个从不编译；`lockdep` 78 处 cfg **全为** `any(debug_assertions, feature = "lockdep")`、纯 `feature = "lockdep"` **零处**，而 `Makefile:86` `KERNEL_FEATURES ?=` 为空 ⇒ **release + lockdep 组合无任何门禁构建，门槛 5 QEMU 真正启动的镜像里 lockdep 恒不存在**（P-43/P-44 那一类"共享掩码的析取支从不单独点亮"的上一层）；`smp`/`preempt`/`kaslr`/`freg` 经 `caps.rs:53-58` 的 `cfg!(feature = …)` 在门槛构建里**恒 false**；`#[cfg(all(test, target_arch = "x86_64"))]` 掩掉 3 条用例（`coredump_trait.rs:181`、`timer/irq.rs:107`）。**P-52** clippy 只跑 x86_64（`audit.sh:279` 目标三元组写死 + 2b 在 host target）⇒ 本仓 **147 处** aarch64 门控代码无 lint 门禁，仅 rustc check。**P-53** F9 无全仓脚本执法（兜底 = rustc dead_code lint 只覆盖被编译路径 + 4 个 host-tests 里手抄清单的 grep）；正面读数一并记录：自研 `src/kernel`（排除 smoltcp）实测 `allow(dead_code)`/`allow(unused*)`/`expect(dead_code)` **0 处**。
  - **一条独立探针实验（把一个中置信疑问变成事实，结论是「无需处置，只需固定口径」）**：门槛 2/2b 带 4 条 `-A clippy::cast_*`（DECISION-041 用户裁定），代码里另有 **223 处** `#[expect(clippy::cast_possible_truncation)]`——`-A` 是否会让这些 `expect` 变空转？在 `/tmp/expect_probe` 用同 toolchain 建最小 crate 实测：**带 `-A`** 时真会截断的位置仍判「已履行」（不报 unfulfilled），不触发位置报 `unfulfilled_lint_expectations` 且被 `-D warnings` 硬阻断 ⇒ 这 223 处**承重**，2b 注释所称「零 unfulfilled 作为门槛」成立。
  - **本轮另执行的一次授权**：AGENTS.md §2.2「仓库共 23 个 `audit_*.py`」订正为 **24**（受控文件实测口径：`scripts/` 22 + `scripts/tests/audit_selftest.py` 1 + `tools/audit_unsafe.py` 1），来自用户「允许订正一次agents文档」，**该一次性授权已消耗**（commit `613b324a`）；复跑 `tools/check_doc_paths.py` EXIT=0 与 `bash ci/audit.sh quick` EXIT=0 后提交推送。
  - **台账同步**：`progress-active-tasks.md` net-e2e 行 **`30 / 6` → `30 / 15`**（口径 A 实测 **47** 条状态字段行 = 30 `[X]` / 15 开放 / 2 `[~]`）。**一条既有细则本轮再被证必要**（不新增口径）：同一份文档用「含 `状态：[` 的行」（宽松）得 **50 行 / 33 闭合**，用「行首 `  - 状态：[`」（严格）得 **47 行 / 30 闭合**，差的 3 行全是**散文内把状态形态当被描述对象引用**的行（P-10/P-11 标题段与 P-36 描述段各 1 行）⇒ §对账方法 细则②「只统计**行首**状态字段行」正是为挡这一类虚高而存在，本轮按严格口径取数。
  - **门槛读数（本轮零 `.rs`、零 CI 脚本改动）**：沿用 `613b324a` 树上的实测 —— `bash ci/audit.sh quick` **EXIT=0**（0.5 系列 + `0.5k` 硬规则三条 + 双架构 check + clippy 三维 + 四 crate fmt 全绿）与 `f5cc4439` 树上的六条全量读数（`./ci/build.sh all` `Passed: 6 / Failed: 0` + rustc `warning:` 0 行、`bash ci/audit.sh full` EXIT=0 含步骤 2d、`make test-host` 140 套 1,070/0、QEMU 双架构 2/2 `FAIL_OK=0`、`make test-kernel-host` 1,019/0）；本轮改动仅 `docs/plan/` 三册，`docs/plan/` 按 AGENTS §6 不要求路径新鲜，故 `check_doc_paths.py` 不在其射程。**须诚实标注一条**：本轮那些"沿用"的读数，其成立前提是工作树除 markdown 外无差异（已由 `git status --porcelain` 核验）。
  - **下一轮取向（裁定重申）**：**ci-rustdoc R-1・乙** 不变，其后 P-40 甲；P-45..P-53 属新立的门禁执法面账，逐轮消化，不插队。

- **本轮（第五轮裁定执行：P-44 立案并同批闭合 —— 给上一轮的修复装回归锁）**: 由用户一问（「p43 算根治吗」）触发的自查，实测坐实 P-43 的修复自身无门禁守着；处置经两次询问裁定（先裁「立案 P-44 + compile-only 拦截」，负控制后再追加裁「甲・加本-crate 告警过滤」）。
  - **自查结论（把"哪些算根治、哪些不算"写清）**: P-43 甲对**病灶**（测试侧 cfg 与被测代码掩码不同形）是真因修复——改成因、无 `#[allow]`、无绕过、判据为实测；但对本册 P-43 描述里登记的**后果面①**（release 测试维无人持续验证）并无消解，甲只让该维"这次能编译"。三条实测支撑：门槛 6 两处配方都不带 `--release`（[Makefile:484](../../Makefile) / [Makefile.ci:94](../../Makefile.ci)），`ci/build.sh:85` 亦不带，`grep -rln debug_assertions scripts/*.py` 零命中 ⇒ 把掩码改回去六门槛照样全绿，与 AGENTS.md §8「每个 bug 修复加回归测试」低一档。
  - **实装**: 新脚本 [scripts/check_kernel_test_release_dim.sh](../../scripts/check_kernel_test_release_dim.sh) 两条判据——`cargo test --release --features host-test --lib --no-run` 退出码为 0，加"本 crate（相对路径诊断）零告警"；接线 [ci/audit.sh:348](../../ci/audit.sh) 步骤 2d（置于 quick 早退之后 ⇒ 仅 full 执行）与 [Makefile:492](../../Makefile) 新目标 `test-kernel-host-release-compile`，两处共用同一实现避漂移。**只编译不跑用例** ⇒ 不产出 release 覆盖读数，门槛 6 判据仍是 debug 配方（加压 release 腿属未选的乙路线）。成本：kernel crate 重编 ≈27s，无改动 ≈0.1s。
  - **负控制三组（证拦截承重）**: ① lockdep 掩码回退为裸 `#[cfg(test)]` ⇒ **exit=101**、`due to 13 previous errors`（E0425×3 / E0433×3 / E0599×7），连跑两次同读数；② spinlock 复原为 P-43 之前的历史形态（整条用例不掩 + 内层语句掩，只产告警）⇒ **exit=1 由判据 (2) 命中** `unused variable: lock`，且第二次 **0.03s 缓存命中**（零重编）仍 exit=1 且告警照旧重放 ⇒ **拦截不依赖重编**，此条是判据 (2) 可用性的前提（若不重放则该判据在增量构建里会假绿）；③ 两处掩码复原 ⇒ exit=0，且 `git diff -- src/kernel/` 为空。
  - **追加裁定的由来**: 首轮只裁 compile-only，负控制实测到 warning 形态返 0（P-43 的历史形态拦不住）后回问，用户裁「甲・加本-crate 告警过滤」。该判据两条边界已写入脚本顶部注释：若改从仓根 `--manifest-path` 调用，本 crate 诊断变绝对路径 ⇒ 识别静默失效；依赖侧告警不计入（其诊断为绝对路径）。
  - **上一轮记录订正一处**: P-43 结案行称 spinlock 那处失配产"unused variable 警告"，本轮实测"整条不掩且内层也不掩"的形态是硬错误 `error[E0599]`（no method named `assert_held`，[privileged/sync/spinlock.rs:457](../../src/kernel/privileged/sync/spinlock.rs)，`assert_held` 本身在 `debug_assertions` 掩下）；两种形态都是病灶但拦截强度不同（一 error 一 warning），这正是判据须两条的直接理由。
  - 台账同步：`progress-active-tasks.md` net-e2e 行 **`29 / 6` → `30 / 6`**（口径 A 实测 38 条状态字段行 = 30 `[X]` / 6 开放 / 2 `[~]`，新闭合件即 P-44）；正文入 [net-e2e-tcp-echo-flakiness.md](./net-e2e-tcp-echo-flakiness.md) §6 P-44。第 0A 类与第 6 类计数不变。
  - 门槛读数（六条全量重跑于最终树，同一批源码；既有次序教训是「改过源码先 `cargo fmt` 再跑门槛」，本轮零 `.rs` 改动故无需预跑）：① `./ci/build.sh all` `Passed: 6 / Failed: 0`，rustc `warning:` **0 行**（仅两条 cargo future-incompat 通告，非本仓 lint）；②③ `bash ci/audit.sh full` **EXIT=0**（新增步骤 2d 在位并取绿：`✓ 内核测试 release 维: 编译通过 + 本 crate 零告警`；同批含 clippy pedantic 三维 + 四 crate `rustfmt --check` + 全部 `audit_*.py`；`audit_unwired` CRITICAL=0 / HIGH=0 / WARN=0 / INFO=433，B-6 台账 **452** 项未变）；④ `make test-host` EXIT=0，140 套 **1,070 passed / 0 failed**；⑤ QEMU 双架构 **2/2**（`FAIL_OK=0` 门禁模式，由 audit.sh 第 7/7 步执行）；⑥ `make test-kernel-host` EXIT=0，**1,019 passed / 0 failed**（本轮零 `.rs` 改动，与上一轮同数）。 release 维 compile-only 读数由步骤 2d 单独取，该维用例数比 debug 维少 11（10 条 lockdep + 1 条 spinlock），**不并入 §2.3 验收读数**。

- **本轮（第四轮裁定执行：P-42 根治 + P-43 甲・同掩码，同一批源码同一次门槛跑）**: 用户以选项询问工具对三件待裁点裁定（P-42 路线 / P-43 路线 / 下一轮施工取项）。本条记前两件的实施，第三件见末段。
  - **P-42 根治（裁定原文「需要根治方案」⇒ 走甲，不做缓解式挡）**：[functions/wasm/leb128.rs](../../src/kernel/functions/wasm/leb128.rs) 三处末字节判定收紧——无符号版要求"末字节的 7 位载荷落在 32 位之外的部分为空"，有符号两版要求"越出 `size` 位宽的有效位必须是符号位的完整复制（全 0 或全 1）"，否则返 `Leb128Overflow`；三处判定成立的位移各自唯一（u32 侧 28 / i32 侧 35 / i64 侧 70），写法上的短路使 `32 - shift` 与 `7 - spare` 恒落在安全区间，不新增 panic 面；`read_name` 经 u32 长度前缀自动继承。新增 `#[cfg(test)] mod tests` **6 例**：畸形侧 `[0xFF×4,0x10]` / `[0x80×4,0x70]` / `[0xFF×4,0x7F]` 判溢出，合法侧必须保住 `u32::MAX` / `i32::MAX` / `i32::MIN` / `i64::MAX`（`[0xFF×9,0x00]`）/ `i64::MIN`（`[0x80×9,0x7F]`）的最长规范编码，另钉住截断语义与 `pos` 推进位置。
  - **负控制一轮（证回归锁承重）**：把三处判定改为恒假后重跑 ⇒ **4 例转红**，且 u32 用例逐字复现改造前的失实读数 `Ok(268435455)`（= `0x0FFFFFFF`）；恢复后 6 例全绿。
  - **被验证对象行为已变更 ⇒ harness 腿实跑**：`cd kani-harness && cargo kani` ⇒ **4 个 harness 全通过 / 55 项检查 0 失败 / VERIFICATION SUCCESSFUL**（"不 panic + `pos` 单调不减且不越界"三条不变式在收紧后仍成立）。该 harness **不在 §2.3 六门槛内**，是随甲路线附带的独立工具调用，其绿与否不改变门槛读数。wasm 目录内联测试数随之 24 → **30**。
  - **P-43 甲・测试模块同掩码**：[privileged/sync/lockdep.rs:721](../../src/kernel/privileged/sync/lockdep.rs) 的 `#[cfg(test)]` 改为 `#[cfg(all(test, any(debug_assertions, feature = "lockdep")))]`，与同文件机制侧 17 处掩码同形。判据实测：`cargo test --release --features host-test --lib` 由 **13 个编译错误** 转为跑完 **1,008 passed / 0 failed**。
  - **附带一项同族错配（由本条放行 release 维后才显形，不是既有门槛失守）**：release 测试构建首次编译到 [privileged/sync/spinlock.rs:453](../../src/kernel/privileged/sync/spinlock.rs) 的 `test_spinlock_debug_assert` 时报 `unused variable: lock`——其唯一断言面在 `#[cfg(debug_assertions)]` 之下而绑定未同掩。按同一甲路线把整条用例置于该掩下（release 维本就无事可断言），并把掩码内的冗余内层 `#[cfg]` 一并去掉，改后 release 维 **0 warning**。**两态口径（甲的已知代价）**：release 维与 debug 维用例数差 **11**（10 条 lockdep + 1 条 spinlock），lockdep 在 release 维用例数为 **0** ⇒ 不得读成 lockdep 覆盖已达成；本轮**未**给 §2.3 加压 release 维（那是未选的乙），只让它可编译、可运行、零警告。
  - 台账同步：`progress-active-tasks.md` net-e2e 行 **`27 / 8` → `29 / 6`**（口径 A 实测 37 条状态字段行 = 29 `[X]` / 6 开放 / 2 `[~]`，新闭合两件即 P-42 与 P-43）。同表另订正一处失真：总览原写"wasm 内联测试 0 → 23"，实测 W-2 为 **24**、本轮补 6 例后为 **30**。`wasm-runtime-wiring.md` 两处随之前行表述加时点限定——§1 基线限定补"F14 已按裁定根治，F14 行文本保留为该时点实测基线不回写"，W-2 结案行的"`leb128.rs` 一个字节未动"标注**止于 W-2 提交时点**。
  - **下一轮施工取向（裁定）**：**ci-rustdoc R-1・乙**（30 个 `unresolved link` error 与 19 个 `links to private item` warning 一并收口，再把 `RUSTDOCFLAGS` 加压到 `-D rustdoc::private_intra_doc_links`）。wasm **W-3 已被 W-2 解锁但未被选为下一轮**；P-40 甲深度已裁仍未开工。
  - 门槛读数（六条全量重跑，同一批源码，次序按上一轮教训先 `cargo fmt` 再跑）：① `./ci/build.sh all` `Passed: 6 / Failed: 0`，rustc `warning:` **0 行**（仅两条 cargo future-incompat 通告，非本仓 lint）；②③ `bash ci/audit.sh full` **EXIT=0**（clippy pedantic 三维 + 四 crate `rustfmt --check` + 全部 `audit_*.py`；`audit_unwired` CRITICAL=0 / HIGH=0 / WARN=0 / INFO=433，B-6 台账 **452** 项未变）；④ `make test-host` EXIT=0，140 套 **1,070 passed / 0 failed**；⑤ QEMU 双架构 **2/2**（`FAIL_OK=0` 门禁模式，由 audit.sh 第 7/7 步执行）；⑥ `make test-kernel-host` EXIT=0，**1,019 passed / 0 failed**（上一轮 1,013 + 本批 leb128 的 6 例）。另取 **release 维读数**（非 §2.3 门槛，属 P-43 判据）：`cargo test --release --features host-test --lib` **1,008 passed / 0 failed / 0 warning**，与 debug 维差 11 例，口径见 P-43 结案行。kani harness 单独跑，读数不混入门槛。

- **本轮（P-33/W-2 施工：解析器全函数化 + 畸形输入回归矩阵 + 两条新事实立案）**: 用户「可以，继续推进吧」授权后开工 §3 戊裁定的唯一施工项。
  - **W-2 闭合**：[functions/wasm/module.rs](../../src/kernel/functions/wasm/module.rs) 由 436 行改为 1,046 行（其中非测试解析体止于 `:498`，其后为 24 例回归锁）。核心手法是**先切段再解析**——`parse_wasm` 为每个段取子切片，段内一律走五条新边界安全原语（`take_byte` / `take_value_type` / `take_entry_count` / `take_slice` / `require_consumed`），使 leb128 的 `bytes.len()` 边界**就是**段边界，跨段读取在类型上不可能；`take_entry_count` 以"每条目至少占 1 字节"的段内剩余字节数为**分配上界**（旧码可被一句 `0xFFFF_FFFF` 打到 `Vec::with_capacity` 失败）；`require_consumed` 要求段内容一次消费干净。`WasmError` 由 23 → **27** 变体（新增 4 名全部有构造点，零死变体），其中非法值类型字节旧实现误记为与版本头同名的 `InvalidVersion`，现单列 `BadValueType(u8)`。F8（解析器零边界安全访问）闭合：非测试体裸索引归零、`unwrap`/`expect`/`panic!`/`assert` 归零。**`leb128.rs` 一个字节未动**（kani-harness 以 `#[path]` 直引它为被验证真源码）。
  - **两条负控制实测**：① 把 `take_byte` 还原为 `bytes[*pos]` ⇒ 3 例转红；② 只在 `parse_global_section` 的 mutable 字节处还原裸索引 ⇒ `test_every_prefix_is_total` 与 `test_every_single_byte_mutation_is_total` **均仍绿、仅确定性模糊例转红**，即第 24 例（xorshift64，20,000 轮合法模块多字节突变 / 截断 / 追加 + 纯随机字节各半）确实覆盖了两条穷举扫描的盲区，回归锁承重而非装饰。
  - **新事实两条**（均已回写 `wasm-runtime-wiring.md` §1）：**F13** 表格段 funcref 元素类型 `0x70` 不被 `ValueType::from_byte` 接受 ⇒ 含表格段的真实模块现在明确返 `BadValueType(0x70)`（W-2 前被伪装成 `InvalidVersion`，故从未被识别），归 W-4 的类型建模收口；**F14** `read_leb128_u32`/`_i32` 对末字节无续行位的超长效编码**静默截断**（`[0xFF×4,0x10]` 解为 `0x0FFFFFFF` 并接受，规范应返 `Leb128Overflow`），不 panic、不放大分配，属语义误读。
  - **P-42 立案（§11.5 处置）**：F14 只登记不修——是否变更被形式化验证对象的行为属 §9.1 灰色地带，且不在用户已裁的 wasm 五项范围内。已入 [net-e2e-tcp-echo-flakiness.md](./net-e2e-tcp-echo-flakiness.md) §6，路线甲（收紧解码器 + 重跑 harness）/ 乙（解析侧逐调用点挡）/ 丙（写进 design 能力边界，随 W-5/W-8 校验门收紧）待裁。
  - **自我订正一处**：`wasm-runtime-wiring.md` W-2 方案字段原写"现 22 变体"，按 `git show HEAD` 逐名清点实为 **23**（漏计一名），已订正；同册 W-4/W-9 状态字段随本轮裁定与施工结果同步（W-4 的"浮点范围待裁定"改为"已裁不纳入"，W-9 记录内联测试 0 → 24 的部分推进）。§1 表头新增"本表是 W-1 调研时点基线、非当前状态"的限定，防后续轮次把已闭合的 F8 读成仍在。
  - 台账同步：`progress-active-tasks.md` net-e2e 行 **`27 / 6` → `27 / 8`**（口径 A 实测 37 条状态字段行 = 27 `[X]` / 8 开放 / 2 `[~]`，新增开放件即 P-42 与 P-43）；`wasm-runtime-wiring.md` 由 9 开放 + 1 已闭转为 **8 开放 + 2 已闭**。第 0A 类与第 6 类计数不变。
  - **P-43 立案（§11.5 处置）**：取门槛 6 读数时误打了一次 `cargo test --release --features host-test --lib`，实测 **13 个编译错误**——[privileged/sync/lockdep.rs](../../src/kernel/privileged/sync/lockdep.rs) 的机制侧全在 `#[cfg(any(debug_assertions, feature = "lockdep"))]` 掩下（`MAX_HELD_LOCKS` 导入 `:59-60`、`HeldLockStack` `:359-361`、`add_dependency` `:225-226`），而其 `#[cfg(test)]` 模块只掩 `test` 不掩那条 `any(...)` ⇒ release 测试构建里三个符号不存在。现行门槛 6 配方（[Makefile:484](../../Makefile)，不带 `--release`）走 debug，现网不受影响；真实后果是**门槛 6 没有 release 维**、而门槛 1 的 release 构建不含测试代码，此类 cfg 掩码错配无人拦截。已入 net-e2e §6 待裁（甲 同掩测试模块 / 乙 release 维开 `lockdep` / 丙 只写清口径），本轮未动该文件一个字节。
  - 门槛读数（六条全量重跑，逐条读数登记于 `wasm-runtime-wiring.md` W-2 详情字段）：① `./ci/build.sh all` `Passed: 6 / Failed: 0` + rustc `warning:` 0 行；②③ `bash ci/audit.sh full` EXIT=0（clippy 三维 / 四 crate `rustfmt --check` / 全部 `audit_*.py`；`audit_unwired` CRITICAL=0 / HIGH=0 / WARN=0 / INFO=433，B-6 台账 452 项未变）；④ `make test-host` EXIT=0，140 套 1,070 passed / 0 failed；⑤ QEMU 2/2（`FAIL_OK=0`）；⑥ `make test-kernel-host` EXIT=0，1,013 passed / 0 failed。**门槛 3 曾在 `2c rustfmt` 处 EXIT=1 中断且不再执行其后步骤（含 QEMU 腿）**——首轮那一次"长跑"因两处手写格式漂移而整批作废，已据此把"改完源码先 `cargo fmt` 再跑门槛"记为固定次序。

- **本轮（第三轮裁定批量执行：P-41 试删闭合 + wasm 五项裁定落地 + 编号口径订正）**: 用户以选项询问工具对全部待裁点逐项裁定，执行后六门槛全量复跑绿。
  - **P-41 甲・试删**：`is_wait_op` / `is_wake_op` 已从 [functions/sync/futex.rs](../../src/kernel/functions/sync/futex.rs) 删除，B-6 区块同步撤回 **454 → 452**（登记在先、删除在后，符合裁定六①「新增删候选必须上报」的边界）。判据＝语义由 `privileged/syscall/futex.rs:260-264` 的 `futex_op(op)` 分类分支等价承担，且同文件 `futex_validate_op` 已用 `match futex_base_op` 分档 ⇒ 属重复策略逻辑；删前逐项核实 `futex_base_op` 与四条 `FUTEX_*` 常量仍有消费者（不孤立、无级联）。单跑 `audit_unwired_pub_fn.py` 实测 **CRITICAL=0 / HIGH=0 / WARN=0 / INFO=433**（B-6 452 项，未命中偏差仍 19 条，本轮未新增）；六门槛读数：`./ci/build.sh all` 0 error/0 warning、`bash ci/audit.sh full`（含 clippy pedantic + feature 维 + 四 crate `rustfmt --check` + 全部 `audit_*.py`）EXIT=0、host-tests EXIT=0、QEMU EXIT=0、`make test-kernel-host` EXIT=0。
  - **P-40 甲・仅五文件补测**：深度裁定为只给 `functions/mm/mprotect.rs` / `functions/mm/brk.rs` / `functions/syscall/brk.rs` / `functions/mm/mremap.rs` / `functions/proc/sysinfo.rs` 五个实测离群文件补 `#[cfg(test)]`，**不**做同类 `*_syscall` 全量扫齐（乙路径未选）。状态保持 `[]`（本轮只裁深度，未开工）。
  - **P-39/ci-rustdoc R-1 乙・全收 + 加压**：30 个 `unresolved link` error 与 19 个 `links to private item` warning 一并收口，收口后把 `RUSTDOCFLAGS` 的 `-D` 面加压到 `rustdoc::private_intra_doc_links`（此后该 warning 即阻断）；F9 零容忍禁止用 `#[allow(rustdoc::…)]` 走捷径，故 ③ 类须逐处做可见性判定。R-2 以下条目随之解锁，仍未开工。
  - **P-33/wasm §3 五项裁定**：**甲** 浮点不纳入本工程，遇浮点 opcode 显式返 `Unsupported` 并写进 design 的能力边界；**乙** 恶意字节码采**两态分离**（解析期一次校验 + 运行期 `.get()` 边界检查不得为快路径移除），不引入独立模块级校验门；**丙** loader 派发用 `exec.rs` 内**单点 `match`**，不抽 `ImageLoader` trait（与 §11.3 简约细则一致）；**丁** `EG_WASM_LOAD=766 / RUN=767 / GET_INFO=768 / UNLOAD=769`，新开 766-779 WASM 私有段；**戊** 本工程本轮**只推 W-2**（解析器全函数化的安全门槛），W-3..W-9 另起轮。
  - **号段推荐的自我作废（过程记录）**：AI 初次推荐 `EG_WASM_*` 占用"735-746 连续空块"，该结论只核了常量占用、**未核** [privileged/syscall/types.rs](../../src/kernel/privileged/syscall/types.rs) 段注释里已声明的功能分区（730-739 固件加载 / 740-745 POSIX Timer / 746-747 熵源），按此号段会同时侵入三个已声明段。发现后即向用户声明作废并重裁，实测空闲块为 660-699 / 748-759 / 766-779，用户改裁 **766-779**。作废与重裁两处均已在 [wasm-runtime-wiring.md](./wasm-runtime-wiring.md) §3 丁 内留痕。
  - **编号口径订正（用户授权「同时订正 explain 两处」+「改写为指向段表而非复抄」）**：`docs/explain/linux-compat-philosophy.md` 的"Edgine 私有扩展使用 500+"与 `docs/explain/ref-naming.md` 的"400+ 用户态 / 700+ 内核态"两处**互相矛盾且都与代码不符**，已按实测改写为指向段表的口径：Linux 原生号从 0 起（实测含 300 区间的真实 Linux 号），**≥400 全为 EG 私有并按功能分区**，段表唯一事实源 = `privileged/syscall/types.rs` 的段注释，正文不复抄号段；并记 `SYS_seteuid=597` / `setegid=598` 依其行内注释属 EG 私有、"400+/700+"分野被 `SYS_FB_*` 720-722 与 `EG_FW_*` 730-733 否证。同条订正已回写 `net-e2e-tcp-echo-flakiness.md` 的 P-33 方案字段（原写"500+ 走 `EG_*`"为失实）。
  - 台账同步：`progress-active-tasks.md` net-e2e 行 `26 / 7` → **`27 / 6`**（口径 A 实测 35 条状态字段行 = 27 `[X]` / 6 开放 / 2 `[~]`），P-41 从本轮开放件移入闭合件；`[~]` 两件的后述括注同步更新为"§3 五项已全裁"与"R-1 已裁乙"；[wasm-runtime-wiring.md](./wasm-runtime-wiring.md) §3 由「待裁定表」改为「裁定结果表」，W-2/W-7/W-8 状态字段与裁定对齐。第 0A 类与第 6 类计数不变。

- **本轮（四项裁定执行 + 两份工程单开 + 退役去掩蔽暴露 R1 真值）**: 用户对上一轮登记的四个决策点给出裁定「退役 / 改为本地全绿 / 单开 / 相对完整」，逐条执行并全部回源码复验。
  - **P-34 丙・退役**：`git rm -r src/rust/edgine-tests`（32 个受控文件）+ 清该 crate 324M 陈旧 `target/`；裁定前的"退役会净减覆盖"顾虑经核验**被否证**——该 crate `[dependencies]` 为空、29 个 `tests/*.rs` 全调自己 `src/lib.rs` 的镜像谓词、零引用 `src/kernel`，且镜像已漂移（[functions/mm/mremap.rs:46-51](../../src/kernel/functions/mm/mremap.rs) 的 `MAX_REMAP = 1 << 30` → `ENOMEM` 护栏在镜像里缺席）。真实残留缺口立案 **P-40**（`mprotect`/`brk`/`mremap`/`rlimit` 四域五个文件内联测试计数全 0，对照 `functions/` 层 129 文件 / 1,116 例的惯例属离群点）。
  - **P-41（本轮新立，退役的直接后果）**：门槛 3 复跑由绿转 `HIGH=2`，明细 `functions/sync/futex.rs` 的 `is_wait_op`(L50) / `is_wake_op`(L55)。成因是**审计口径而非代码回归**——R1 按全仓词元计数判零引用，被退役 crate 的 `tests/phase_b.rs:21/:25` 各写了一份**同名镜像函数**并被同文件用例调用，把这两个名字的全仓计数抬到 >1，使内核侧真函数长期不进 R1 名单；镜像消失后计数回落到 1 ⇒ 真值显形（与 P-40 同属"平行实现掩盖两侧"病理族）。按**裁定六①**「新增删候选必须上报」，本轮**只登记不删**：入 [syscall-followup.md](./syscall-followup.md) **B-6** 区块 **452 → 454**，实测回 `CRITICAL=0 / HIGH=0 / INFO=435`；删/接/留三选一待裁。附带登记一处账面偏差：B-6 区块 454 条中仅 435 条命中当前 R1 集合，余 **19 条未命中**（三态＝已接线 / 已不存在 / 被豁免，须逐条定性），按"预存失效须用户裁决后才清"的先例本轮未动。
  - **P-37 甲・判据改本地全绿**：`AGENTS.md` §2.3 第 3 条由"核心审计全部通过 + GitHub Actions"改为"核心审计全部通过（判据 = 本地 `bash ci/audit.sh` 全绿；远程仅 Gitee、无流水线配置，`.github/workflows/*` 从不触发，故不作验收依据）"；两处 `AGENTS.md` 改动（另 §1 目录表移除 `edgine-tests`）均**由本轮裁定直接要求**，属授权范围内的同步，非自主维护。五份 workflow 文件保留不动。
  - **P-39 单开 + P-33 相对完整**：新建 [ci-rustdoc-closeout.md](./ci-rustdoc-closeout.md)（条目 R-1..R-6，甲/乙收口深度待裁，rustdoc 仍红未修）与 [wasm-runtime-wiring.md](./wasm-runtime-wiring.md)（实现深度 **乙・相对完整**）。后者 §1 以 **12 条源码实证推翻 P-33 的"已实装"前提**：解析器无 `.get(` 边界访问（截断字节码即内核 panic）、`wasi_function_table()` 零消费者（WASI 挂不进实例）、`CallIndirect` 直接返 `Unreachable`、块结构只 `advance_pc(1)`、浮点 opcode 全缺、`wasm/` 目录 0 个 `cfg(test)`；故第一施工项是**安全门槛**（W-2 解析器全函数化）而非接线，§3 另列五项待裁定。
  - 台账同步：`progress-active-tasks.md` net-e2e 行 `26 / 6` → `26 / 7`（口径 A 实测 26 `[X]` / 4 `[ ]` / 3 第 6 形态 `[]` / 2 `[~]`）并把 P-41 计入本轮开放件；§对账方法 追加细则 ④（第 6 形态 `状态：[]`，实测使 33 份 plan 的开放项由 3 条跃至 31 条 ⇒ **28 条系统性漏计**）与细则 ⑤（须显式声明 A/B 口径，A＝466/46/4 对 B＝481/56/4）。第 0A 类计数不变（8）；第 6 类 **ISSUE-TOOL-006** 指针改指新册。

- **本轮（门槛 fail-open 与台账失真根治 + 五项新开放登记）**: 由 AGENTS.md §1 目录表维护（用户授权一次）触发的门槛复验，坐实并闭合第 0A 类 **AUDIT-TOOL-004..008** 五项（`qemu_boot_test.sh` 的 `FAIL_OK` 默认软门禁使 §2.3 第 5 条假绿；`ci-lint.yml` audit-selftest job 的 RC 取自 `echo` 恒 0；双架构 `cargo check` job 因 `other/` 属 gitignore 而 tee 先失败、判据 grep 永不执行；`ci-x86.yml` functions 编译 job 管道退出码取自 `tail` 恒 0 的真 fail-open；aarch64 `$(RUST_LIB)` 缺 kernel 源码前置依赖，负控制读数 0/1）。每项均带**本地实测验证**而非仅改文本（严格口径 QEMU 实跑 2/2、`bash -e` 下 tee-to-missing-dir 打断语义模拟、`make -n` 新旧规则对拍、YAML + 内嵌 run 脚本语法校验）。
  - 同时开放六项新预存问题，正文登记于 [net-e2e-tcp-echo-flakiness.md](./net-e2e-tcp-echo-flakiness.md) §6：**P-33** `functions::wasm` 4,575 行零内核侧调用者（用户已裁定单开工程接线实装，实现深度待 §11.3 询问）、**P-34** `src/rust/edgine-tests` 孤儿 crate（单独实测 381 passed / 0 failed，但全仓零门槛引用且属平行实现族；甲并入门槛 / 乙迁真源码 / 丙退役 三选一待裁）、**P-35** `test_call_rcu` 并行共享全局态 flaky（HANDOFF 记为"未立案"，本轮正式立案）、**P-36** plan 状态标记存在第 5 种形态（markdown 表格单元格 `[]`/`[X]`）不被 `progress-active-tasks.md` §对账方法 的归一化口径计数，致总览系统性低估、**P-37** 唯一 remote 为 Gitee 且仓内无 Gitee CI 配置 ⇒ `.github/workflows/` 5 份 workflow 从不被触发，AGENTS §2.3 第 3 条的"GitHub Actions"腿是纸面判据（处置方向待裁；`AGENTS.md` 本轮不再改动，授权额已用于 §1 一行）、**P-39** 本地一键 CI 的 `ci-rustdoc` 腿在 HEAD 上即红（30 error + 19 warning / 19 文件，非本轮改动引入；该腿不在六门槛内故历轮"全绿"从未覆盖它），同条已在本文档第 6 类追加 **ISSUE-TOOL-006**。
  - 台账同步：`progress-active-tasks.md` §现状总览表补 6 行缺失文档并把 net-e2e 行从 `22/4` 对账到实测计数；两处历史失实判定（"U5 组播未实装"已由 `474295a6` 实装、socket-option 行已闭环）按 §9.2 源码复验订正。第 0A 类计数 3 → 8（总览新增一行）。

- **本轮（第 6 类两项预存问题收敛 — TOOL-004/005 登记即修复）**: 用户裁定“这两个本轮修复”，同时登记并闭合两项工具/审计预存问题。
  - **ISSUE-TOOL-004**（deadlock 审计基线唯一 HIGH）：依用户裁定**删除** `privileged/arch/x86_64/smp_init.rs` 的 `AP_STARTUP_LOCK`（源码调研证明 `start_ap` 无第二个获取者：BSP 串行 `init()` 是唯一入口，AP 走 `ap_entry`；`spin::mutex::SpinMutex` 又**不做** IRQ save，因此脚本只能保守报 HIGH）。复跑 `audit_deadlock_matrix.py` → `扫描文件数: 693 / 问题总数: 0`（**审计基线建立以来首次全清**）；新增静态契约回归守卫 [arch_ap_startup_lock_removed_test.rs](../../host-tests/tests/arch_ap_startup_lock_removed_test.rs)（6 passed，含负控制）。同时订正 [code-audit-final-summary.md](./code-audit-final-summary.md) F-12 的“irq_spinlock 会 save/restore IRQ”错判。
  - **ISSUE-TOOL-005**（文档路径漂移 368 条）：逐条核证 96 处活 plan 漂移**全部为历史记录**（批次目标文件 / 实测行数 / 已删模块），按用户改授“**收窄校验范围，不改文档**”收敛：`tools/check_doc_paths.py` 校验集合收窄为 explain / design / 根文档（共 14 份，实测漂移 **0**），`docs/plan/`（含 archive）与 `docs/report/` 依 AGENTS.md §6 历史快照定位排除；§6 补写“代码同步范围（源码路径新鲜度）”条款。工具依用户裁定不接入 CI。未改动任何文档散文。
  - 第 6 类计数 3 → 5（总览行改 `3 (+2 本轮登记)`）；本轮无待留开放项。

- **本轮（修复 ISSUE-RT-004 — 调度链路断裂）**: 第 1 类 **ISSUE-RT-004 结案**（`[X]`）。根因**非** KPTI 探针本身，而是调度链路断裂使父进程饥饿、探针 `fork`/`wait` 路径未推进到打印点；断链自洽为双重缺陷——① **D6 tick 未接线**（`scheduler_tick()` 只驱空转的线程级 `SCHEDULER_EX.tick()`，进程级 CFS 记账/抢占/睡眠唤醒/zombie 回收/均衡全不推进）；② **softirq 内调度泄漏**（`do_softirq()` 的 per-CPU `running` 标志在主循环结束后才复位，若 `Sched` handler 内 `schedule()` 切走则本核永久泄漏，此后所有 `do_softirq()` 直接返回）。修法 = **调度点后移到 `do_softirq()` 返回之后（EOI 已发）**：[sched_ops.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/sched_ops.rs) `scheduler_tick()` 改驱进程级 `SCHEDULER.tick(get_current_cpu())`（内部仍调 `SCHEDULER_EX.tick_accounting()`）；[cpu_queue.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/proc/cpu_queue.rs) 删 `Sched` softirq 注册/handler、新增 `run_pending_resched()`；aarch64 [exception.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/exception.rs) ×3 与 x86_64 [idt.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/idt/idt.rs) ×3 在 EOI/`do_softirq()` 之后统一调用。§2.3 六门槛全过（双架构 build 0w0e、`audit.sh quick` 全绿、`make test-host`、`make test-kernel-host` 949/0、QEMU 2/2）—— **aarch64 QEMU 双核已触发 `[KPTI] EL0 kernel high-half access denied`**，x86_64 无回归。同步 [multithreading-project.md](./multithreading-project.md) D6（已提前理顺，D1 线程维度待办不变）与 [smp-ap-user-scheduling.md](./archive/smp-ap-user-scheduling.md) 裁定 2（tick 部分由本工程接管）。仍未闭合收敛为: ISSUE-RT-002、第 4 类远期工程 F1-F5、刻意维持项.

- **本轮（DECISION-084 登记 + 新增 ISSUE-RT-004）**: 登记「AP 参与用户态调度」工程（[smp-ap-user-scheduling.md](./archive/smp-ap-user-scheduling.md)，DECISION-084）——AP 调度闭环（APS-01/02/03）+ KPTI-PCPU-01 按核化结案（APS-04，[aarch64-smp-bringup.md](./archive/aarch64-smp-bringup.md) §「后续专项登记」同步结案）+ 收口期修复「次核 CPU 状态初始化」（批次 P5：aarch64 `CPACR_EL1.FPEN` / x86_64 `init_msr` + `IA32_GS_BASE`）；§2.3 六门槛全过（build 5/0、audit quick EXIT=0、`make test-host`、`make test-kernel-host` 949/0、QEMU 2/2 且双架构命中成对 EL0）。同时新增第 1 类 **ISSUE-RT-004**（aarch64 KPTI-09 里程碑未触发，独立于本修复的预存问题；CI full 模式下构成硬失败，需优先处置），第 1 类计数 3 → 4。仍未闭合收敛为: ISSUE-RT-002、ISSUE-RT-004、第 4 类远期工程 F1-F5、刻意维持项.
- **本轮（DEC-046 结案 — 与 DECISION-080 对齐）**: 依用户裁决「按 DECISION-080 结案」，将第 5 类 **ISSUE-DEC-046** 结案 — 其「未来可选：迁移 USB HID/MassStorage/XHCI/Enumerate/Ring 5 文件 ~70 个纯算法测试到 host-tests」与 [kernel-unit-test-harness-unification.md](./archive/kernel-unit-test-harness-unification.md) DECISION-080「纯逻辑测试唯一归属 = 源文件 `#[cfg(test)]`」终态方向相悖；USB 5 文件 69 例（`xhci` 4 / `mass_storage` 19 / `usb_core` 5 / `ring` 11 / `hid` 18 / `enumerate` 12）**当前已全部位于源文件 `#[cfg(test)] mod tests`**，经 §2.3 门槛 6 `make test-kernel-host` 实际执行，**已处于目标态**，无需迁移；`ring.rs` 等用例直断私有字段，迁往独立 crate 会致覆盖弱化（与 DECISION-080 第 4 条登记同型）。**无代码改动**，仅台账同步。仍未闭合收敛为: ISSUE-RT-002、第 4 类远期工程 F1-F5、刻意维持项（DEC-041/DEC-005）.
- **本轮（RT-002 复验装备与回归防护）**: 依用户裁决「启动 RT-002 复验工程」+「方案 B 相对完整（A + 初始化自检）」，为 QEMU TCG 下不可稳定复现的 aarch64 GICv3 挂起补足复验装备与回归防护（条目**不闭合**，保留待 SMP/真机复验）— [gic.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/arch/aarch64/gic.rs) `init()` 改 `Result` + redistributor 唤醒超时显式失败 + `verify_post_conditions()` 后置条件自检 + 修正 `GICR_CTLR` 语义（bit0 实为 EnableLPIs，删除误写与死常量）；[entry.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/boot/aarch64/entry.rs) GIC 初始化 fail-fast + `GICv3 ready` 里程碑；[qemu_boot_test.sh](file:///home/anfer/Code/Edgine/scripts/qemu_boot_test.sh) 新增里程碑断言；新增静态契约用例 [aarch64_gic_contract_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/aarch64_gic_contract_test.rs)（7 用例）与启动压测脚本 [gic_stress_test.sh](file:///home/anfer/Code/Edgine/scripts/gic_stress_test.sh)。仍未闭合收敛为: **ISSUE-RT-002**、第 4 类远期工程 F1-F5、刻意维持项.
- **本轮（MIG-004 安全代理 + 台账收口）**: MIG-004 由「未闭合」移出 — 用户裁决「MIG-004 建安全代理（相对完整）」，functions/egdf 新建 [proto.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/proto.rs)（block 直通 re-export / net `NetDevice` 去裸指针封装 / input 薄封装）+ [user_driver.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/egdf/user_driver.rs)（强类型 `UserDriverError` + `UserDriverResult<T>` + `to_errno`），现有调用点改走 functions 封装（`unregister`/`unregister_block`/`find_net_device`）；[audit_functions_boundary.py](file:///home/anfer/Code/Edgine/scripts/audit_functions_boundary.py) 白名单补 `('driver','egdf')`；新增 host-tests [egdf_proto_proxy_host_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/egdf_proto_proxy_host_test.rs)（5 passed）。§2.3 六门槛复验通过（双架构 build / clippy + `audit.sh quick` 全绿 / `make test-host` / `make test-kernel-host` 947 passed）。同步 [syscall-followup.md](./syscall-followup.md) B-6 区块（446 → 440 项）. 仍未闭合收敛为: ISSUE-RT-002、第 4 类远期工程 F1-F5、刻意维持项.
- **本轮（MIG-003 完整迁移 + 台账同步）**: MIG-003 由「未闭合」移出 — privileged 删 [pl011.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/driver/char/mod.rs) + 暴露 `pl011_phys_base()` safe 面 + [IoMem::from_platform_device](file:///home/anfer/Code/Edgine/src/kernel/privileged/iomem.rs) safe 构造器；functions 新建 [pl011.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/char/pl011.rs)（0 unsafe）+ [char/mod.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/char/mod.rs) aarch64 分支 egdf 注册 + [lib.rs](file:///home/anfer/Code/Edgine/src/kernel/lib.rs) 双架构调 `char_init()`。§2.3 六门槛全量验证通过（双架构 build 通过 / clippy + `audit.sh quick` 全绿 / `audit_functions_boundary` 通过 / `audit_safety_coverage` 100% / `audit_comment_language` 0 违规 / `audit_deadlock_matrix` 无 CRITICAL / `make test-host` 全 ok / `make test-kernel-host` 947 passed / QEMU 2/2）。仍未闭合收敛为: ISSUE-RT-002、MIG-004、第 4 类远期工程 F1-F5、刻意维持项.
- **本轮（修遗留工程）**: 按用户授权批量修复登记遗留项，逐条追加【本轮修复（修遗留工程）】标记；§2.3 六门槛全量验证通过（双架构 build 5/0、clippy + audit quick 全绿、`make test-host` 0 failed、`make test-kernel-host` 941 passed、QEMU x86_64 1/1）
  - 第 0B 类 3 项结案: B03-LEGACY-001（COW 判定+映射并入同一 `VMM_LOCK` 临界区，配套 `*_locked` 变体）/ B03-LEGACY-002（pmm `find_contig_range`/`reserve_range`/`unreserve_range` host 用例 ×3）/ B03-LEGACY-003（新增定时器 tick 并发 host 冒烟 + 文件头标注判别力边界，弱序语义判别待 aarch64 SMP）
  - 第 6 类: ISSUE-TOOL-002 x86_64 分支接入 `check_kernel_fresh`（QEMU 侧双架构闭环）；ISSUE-TOOL-003 重评结案（E0152 整族根治后原建议无必要，不施工）
  - 第 8 类: MIG-001/002 头注释同步（driver/egdf 头状态改写为当前形态）；MIG-006 据 live 文档结案（尊重 §6 archive 冻结，不改归档快照）；MIG-007 新增 egdf 注册表/IO 专项 host 测试（2 passed）
  - 第 9 类: B06-PRE-002 fchown 前置 `FS_CAP_CHOWN` 能力校验（安全缺陷修复）；B06-PRE-003 移除 `open_by_handle_at` LegacyInode 回退分支 + 删除 `LegacyInode` 类型
  - 第 2 类: ISSUE-SRC-008 结案（原 eg 自有代码唯一真实 TODO）。处理侧 `net_rx_softirq_handler` → `poll_network()`；触发侧 privileged 新增 ISR→softirq 机制层 `net/init/irq.rs`（x86_64 MSI-X / aarch64 GIC SPI 双架构），functions 侧 e1000 + virtio-net 接线 ISR 与中断 ack。第 2 类源码未实现项**全部清零**（0 活跃 TODO）
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
- **2026-08-31**: 新增第 8 类"迁移中子系统状态（driver / egdf）"
  - 记录 driver / egdf 两个迁移中子系统的完整状态：三阶段历史脉络（Phase 2.1/2.4 迁出 + B04 反向纠正）、functions/driver 三种形态（A 完整安全代理 / B re-export shim / C 桩）、functions/egdf 5 模块状态
  - 新增 MIG-001~007 遗留事项（头注释过时 / 缺 pl011 / 缺 proto_*+user_driver / 双份代码边界 / 迁移文档未补注 / egdf 测试缺口）
  - 总览计数 ~70 → ~77
- **2026-08-23**: 分册 3 归档登记
  - 新增第 0B 类"分册 3 归档遗留"3 项（COW TOCTOU / pmm-swap host-tests 缺口 / 多核 tick 测试）
  - ISSUE-TOOL-001（Makefile 跨架构清理）标记已修复（commit 3a1fba9b，戳记写入移至 arch-switch-clean 配方）
  - 总览计数 ~67 → ~70
- **2026-08-09**: 创建本文档 (审计触发: 用户问题"未被修复的问题有哪些?")
  - 来源: 本会话对所有静态 + 动态 + 文档 + 源码的全面审计
  - 产出: 65 项未修复问题分类登记
  - 推荐: 用户授权"仅记录不修复"的 code-review 8 项 + P0/P1 立即关注 + P2/P3 长期规划