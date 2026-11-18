# 门禁健康度收敛工程（自建正则腿 → 编译器 / clippy 执法）

> 一句话说清为什么有这个计划: 用户要求按「**有比项目自建门禁更好的 rust 生态检查工具即为不必要**」这一判据
> 体检全仓门禁。体检结论不是「删门禁」，而是**把判据从正则搬进编译器与 clippy**——搬过去之后判据更强
> （没有跨行 / 宏 / `cfg_attr` 盲区，`allow` 逃逸从「消音」变成硬错 E0453），自建脚本只保留生态工具
> 表达不了的那部分。用户裁定: **不落 report，立项分批**；并**预授权一次** `AGENTS.md` 改动。
>
> 本轮只施工**甲类**（可替代 / 结构恒真）。**乙类**（机制可升级但工具未安装、未实测）只登记不施工。
> **丙类**（生态无对应物）保留不动。

---

## 一、判据与分类口径

### 1.1 「不必要」的判据（用户定义，逐字采用）

存在 rust 生态检查工具能表达**同一判据**且**更强**，则该自建腿不必要。"更强"的可操作定义:

- 判据落在编译器 / clippy 的 HIR 或类型信息上 ⇒ 不受跨行语句、宏展开、`cfg_attr` 间接包裹影响;
- 抑制面从「可被 `allow` 消音」变为「`forbid` 下 `allow` 即 E0453 硬错」;
- 不需要人工维护扫描根 / 白名单 / 基线快照（少一个会静默缩小的扫描面）。

### 1.2 替代不是削弱（本工程的硬约束）

每条退役 / 收敛的腿，**必须先证明替代执法会红**（负控制: 注入违规 → 转红 → `cp` + `sha256` 复原），
才允许摘腿。只删不证 = 门禁净损失，按 §11.6 自审清单拒收。

### 1.3 三类划分

| 类 | 含义 | 本轮处置 |
|---|---|---|
| **甲** | 生态工具可表达同一判据，或该腿结构恒真（无需替代） | 分批施工 |
| **乙** | 机制上可升级，但候选工具未安装 / 未实测，收益不确定 | 登记为开放项，**不施工** |
| **丙** | 判据依赖项目语义（意图 / 盘上格式 / 真机行为 / 中文规范），生态无对应物 | 保留不动 |

---

## 二、实测取证（本轮已出数）

> 探针全部在 `/tmp/gate-probe`、`/tmp/gate-disc`，**仓内源码零改动**。读数一律取 `$?` 直读，
> 不经 `| head`（SIGPIPE 会把 RC 读成 141）；clippy 探针每例用**独立 `--target-dir`**，
> 避免读到上一例的缓存诊断（本轮曾因此得到三例同读数的假象，已重测）。

### 2.1 rustc lint 优先级（决定 forbid 化的价值与边界）

| 探针 | 形态 | 读数 | 结论 |
|---|---|---|---|
| P2 | `#![deny(unsafe_code)]` + 局部 `#[allow(unsafe_code)]` | **RC=0** | 现行 F1 的 `deny` **可被 allow 逃逸** |
| P3 | `#![forbid(unsafe_code)]` + 同一 allow | **RC=1**，E0453 | forbid 把逃逸变硬错 |
| P1b | forbid + CLI `-A dead_code` | RC=1 | CLI `-A` **不能**覆盖 crate 根 forbid |
| P1c | forbid + CLI `--cap-lints allow` | **RC=0** | `--cap-lints allow` **能**中和 forbid ⇒ 构建参数扫描腿必须保留 |
| P4 | `#![forbid(unused)]`（组）+ 具体 allow | `forbidden_lint_groups` 未来不兼容告警，且具体 allow **仍消音** | **组 forbid 有缺陷，不可用** |
| P6 | `#![forbid(dead_code, unused_imports, unused_variables)]` + 三个具体 allow | **RC=1，4 errors**（3×E0453 + 1 unused import） | **逐个 lint forbid 才有效** |

### 2.2 clippy 命中面（决定 safety 腿能收敛到哪一步）

| 探针 | 形态 | 读数 | 结论 |
|---|---|---|---|
| G | `-F clippy::undocumented_unsafe_blocks`，无 SAFETY 的 `unsafe {}` | error「unsafe block missing a safety comment」 | 块面可替代 |
| G | 同上，无 SAFETY 的 `unsafe impl Send for Probe` | error「unsafe impl missing a safety comment」 | impl 面可替代，且 **clippy 不豁免 auto trait**（自建脚本豁免） |
| H | 同上，`pub unsafe fn` / `unsafe trait` 无 `# Safety` | 由 `missing_safety_doc` 报（**默认 warn**，CI `-D warnings` 已升硬错） | fn / trait 面**今天已被编译器执法** |
| H | 同上，`unsafe extern "C" { ... }` 无 SAFETY | **两者都不报** | **extern 面是生态缺口，必须保留自建腿** |
| F | `-F` + 局部 `#[allow(clippy::undocumented_unsafe_blocks)]` | E0453，但**提前中断 lint pass**（其余违规不再报） | 取证时须先去掉 allow，否则读数被掩盖 |
| cp | `disallowed-types` / `disallowed-methods` | **只 `use` 不使用**、以及**定义点**都命中 | 比正则强（正则只看调用行） |
| cp | 路径写 `crate::bad::SpinOnce` | **不命中** | 路径必须 **crate 名绝对**（`gate_clippy_probe::bad::…`） |
| cp | 按文件豁免 | clippy **无此能力** | 带 allow-list 的腿不能直接转 disallowed-* |

### 2.3 clippy 配置发现（决定配置写进哪个文件，避免假绿）

| 探针 | 配置位置 | 读数 | 结论 |
|---|---|---|---|
| A3 | 只在 workspace root（`inner/clippy.toml`），cwd 无 | RC=101，9 条真诊断 | workspace root **会被读到** |
| B2 | 只在 cwd（`outer/clippy.toml`），workspace 无 | RC=101，真诊断 | cwd 是**回退**路径 |
| E | 两处都有，内容不同（cwd 禁 `size_of` / workspace 禁 `OnceCell`） | **只有 workspace 那条命中** | **workspace root 优先，cwd 配置被忽略** |
| C2 | `CLIPPY_CONF_DIR` 指向 cwd | 命中 | env 覆盖可用 |
| D | 两处皆无（负控制） | **RC=0，0 条** | 探针本身干净 |

### 2.4 仓内现状读数（源码 / 脚本直读）

| 事实 | 证据 |
|---|---|
| F1 现为 `deny`，**全仓无任何 `#![forbid(...)]`** | [functions/mod.rs:36](../../src/kernel/functions/mod.rs#L36)；`grep -rn "forbid(" src/ host-tests/ kani-harness/ --include=*.rs` 仅命中 shell 函数名针 |
| **F9 侧车存量已清零** ⇒ forbid 化与 clippy 配置化**零存量成本** | `scripts/audit_dead_code_comment.baseline.json` 的 `totals = {sites: 0, files: 0}`；侧车内 `missing_safety_doc` / `undocumented_unsafe_blocks` / `disallowed` 均 0 次 |
| `spin` **已不是任何树的直接依赖** ⇒ once_cell 腿当前射程为空 | [src/kernel/Cargo.toml:50-52](../../src/kernel/Cargo.toml#L50) 自述「原 `spin` 依赖已删」；四树 `Cargo.toml` 无 `spin =` |
| privileged unsafe **1953 处 / 100% 覆盖**，其中 **extern 127 + 其它 19** 是生态缺口面 | `python3 tools/audit_unsafe.py --summary` 实跑: 块 1517 / fn 290 / extern 127 / 其它 19 |
| auto-trait `unsafe impl` **84 处被自建脚本豁免**，clippy 不豁免 | [tools/audit_unsafe.py:242-246](../../tools/audit_unsafe.py#L242)；`grep` 计数 84（Send/Sync/Unpin）+ 1（其它） |
| `audit_reverse_deps` **结构恒真** | [audit_reverse_deps.py:32/36](../../scripts/audit_reverse_deps.py#L32) 扫 `privileged/` 找 `crate::functions`；而 P-71 拆 crate 后 [lib.rs:113/129](../../src/kernel/lib.rs#L113) 只是 `pub use … as …` 别名，privileged 作为独立 crate 编译时 `crate::functions` **不可解析**；全仓 3 处命中全是注释 |
| `audit_invariants` **只扫 functions**，且自述已被编译期覆盖 | [audit_invariants.py:23](../../scripts/audit_invariants.py#L23) `FUNCTIONS = src/kernel/functions`；[:56](../../scripts/audit_invariants.py#L56)「functions 层 `#![deny(unsafe_code)]` 编译期保证」 |
| I6 是 **safe API 边界**判据，不是 unsafe 面 | [audit_invariants.py:115-119](../../scripts/audit_invariants.py#L115) `dma_map\b(?!.*dma_buf)`，排除 `dma_buf` / `privileged` ⇒ forbid(unsafe_code) **不覆盖** |
| `block_registration` 的低层入口有 **host-tests 4 文件直调** | `egdf_register_block_dev` 出现在 `host-tests/src/framekernel_bench.rs`、`tests/hotplug_reenum_contract_test.rs`、`tests/egdf_registry_io_host_test.rs`、`tests/egfs_disk_persist_test.rs` ⇒ 可见性收紧会断测试面 |
| `c_naming` 的驼峰面**今天已被 rustc 执法** | [audit_c_naming.py:50](../../scripts/audit_c_naming.py#L50) `CAMEL_CASE_FN` 自述「近似 non_snake_case」；clippy 四树腿均带 `-D warnings` ⇒ `non_snake_case` 已是硬错 |
| **clippy 阈值配置对两树失效**（预存缺口，本轮取证顺带发现） | `src/kernel/clippy.toml` 与 `src/rust/clippy.toml` 存在，`host-tests/`、`src/user/` **无**；四树扩围腿在 `popd` 后以 `PROJECT_ROOT` 为 cwd（根也无配置）⇒ 按 §2.3 的 E 探针，这两树跑的是 **clippy 默认阈值**。AGENTS §3 只写 `src/rust/clippy.toml`，与实测不符 |

---

## 三、分类结论（含对上一轮口头评估的订正）

> **订正说明**: 上一轮口头评估报「甲类 5 整腿 + 3 部分」。本轮按 §二 实测后**下修**为
> **2 整腿 + 5 部分 + 1 不可零成本替代**。两处订正的依据:
> ① `audit_safety_coverage` / `tools/audit_unsafe` **不能整腿退役**——`unsafe extern`（127 处）
> 与「其它引用」（19 处）在 clippy 双 lint 下**都不报**（H 探针）;
> ② `audit_block_registration` **不能零成本替代**——`disallowed-methods` 无按文件豁免（cp 探针），
> 而可见性收紧会断 host-tests 4 处直调。

### 3.1 甲类・整腿退役（2 条）

| 腿 | 替代 | 依据 |
|---|---|---|
| `audit_reverse_deps.py` | **无需替代**（结构恒真，删腿即净收益） | §2.4；crate 拆分后判据永不可命中 |
| `audit_once_cell.py` | `clippy::disallowed-types`（禁 `spin::Once` / `spin::OnceCell`） | §2.2 cp 探针（use-only 也命中）+ §2.4（spin 已非依赖 ⇒ 零存量） |

### 3.2 甲类・部分收敛（5 条，**保留残余面**）

| 腿 | 交出去的面 | **必须保留**的面 |
|---|---|---|
| `audit_invariants.py` | I1–I5（全部要求 unsafe，且只扫 functions）→ `forbid(unsafe_code)` | **I6**（safe API 边界，forbid 不覆盖） |
| `audit_safety_coverage.py` + `tools/audit_unsafe.py` | 块 1517 / fn 290 / impl → `undocumented_unsafe_blocks` + `missing_safety_doc` | **extern 127 + 其它 19**；且 clippy 不豁免 auto-trait impl ⇒ 84 处需先补 SAFETY |
| `audit_dead_code_comment.py` | 源码 `dead_code` / `unused*` 族 → crate 根**逐个** `forbid` | **构建参数扫描**（`--cap-lints allow` 能中和 forbid，P1c）+ **clippy 抑制侧车双向对账** |
| `audit_functions_boundary.py` | unsafe 面 → `forbid(unsafe_code)` | **F2 黑白名单**（约 50 条跨模块依赖边，生态无对应物；可见性收紧是另一条路，属乙类） |
| `audit_c_naming.py` | 驼峰 fn 面（**今天已被 `non_snake_case` 覆盖**，属重复执法） | `uN_t` 类型后缀 + `kmalloc`/`kfree` 命名规则（含 allow-list） |

### 3.3 甲类・不可零成本替代（1 条，本轮保留）

`audit_block_registration.py`（I-43 单一桥接入口）。两条生态路都被实测堵住:
`disallowed-methods` 无按文件豁免；`pub(in …)` 可见性收紧会断 host-tests 4 文件直调。
⇒ 登记 **决策 D-1**，本轮**不动**。

### 3.4 乙类（登记不施工）

| 现腿 | 候选生态机制 | 未施工原因 |
|---|---|---|
| `audit_coupling.py` | `cargo-modules dependencies --acyclic` | 工具未安装；边定义（声明级 `use`）与工具口径是否等价未实测 |
| `audit_public_api_docs.py` | rustdoc JSON 输出判可达公共面 | 需自建 JSON 消费逻辑，不是「现成更好工具」 |
| `audit_implicit_deps.py` | `pub(crate)` 可见性收紧 | 改动面大，属结构工程而非门禁替换 |
| `perf_metric_test.rs` | `iai-callgrind` / `criterion` | 现腿已是「绝对预算测试腿」（用户偏好形态），替换收益不确定 |
| `audit_vacuous_needle.py` | `cargo-mutants` | 工具未安装；现腿廉价，宜保留 + 抽样 |
| `tools/check_doc_paths.py` | `lychee` | lychee 只查外链可达性，**不查仓内源码路径新鲜度** ⇒ 只能部分替代 |
| `scan_obsolete_naming.py` | `typos-cli` | 废弃代号是项目专有词表，收益有限 |

### 3.5 丙类（保留，生态无对应物）

`audit_comment_language`（F7 中文）、`audit_feature_semantics`（门控**意图**而非编译覆盖）、
`audit_smoltcp_purity`、`audit_aarch64_kernel_fp_free`（`global_asm!` 手写 FP 绕过编译 flag，只能扫二进制）、
`audit_repr_c` / `audit_volatile_access`（需 HIR/类型信息，现成工具无；`dylint` 或编译期 `offset_of!` 属乙类）、
`audit_tlb_receive_order`、`audit_orphaned_init` / `audit_unwired_pub_fn` R2、`audit_missing_docs_debt`（棘轮）、
`audit_deadlock_matrix`（lockbud 本身已是生态件）、QEMU / UEFI / EGFS 三相 / `verify_image_arch`、
以及 139 份源码契约锁。

---

## 四、批次划分

> **分批原则**: 批 1 / 批 2 **只加固执法，不动腿清单** ⇒ 不触 `AGENTS.md`；
> 批 3 是**唯一**改腿清单的批 ⇒ **那一次预授权的 AGENTS 改动落在批 3**。
> 依据: [ci/audit.sh:94-138](../../ci/audit.sh#L94) 的 **P-69 反向对账**把「AGENTS §2.2 反引号点名的脚本」
> 当外部期望，摘腿而不同步改 AGENTS 会**当场硬红**；故 AGENTS 改动无法延后到收口批。

### 批 0・取证补数（不改执法）

描述：把 §二 尚未精确取数的三项补成权威读数，作为批 1/2/3 的存量依据。
方案：① 四树 clippy 腿临时加 `-W clippy::undocumented_unsafe_blocks`（**不带 allow**，见 F 探针）跑一遍，
取 auto-trait impl 及其余 unsafe 面的**真实缺口清单**（§2.4 的「≤3 处」是 4 行窗口近似，非权威）；
② 核实 `dma_map` 的可见性与调用面，定 I6 是保留还是转 `disallowed-methods`；
③ 逐个核实拟 forbid 的 lint 名在本工具链（`nightly-2026-10-02`）存在且存量为 0。
状态：[]
详情：读数回填本册 §二，**不落 report**（用户裁定）。临时 flag 只在取证命令里，不写进 `ci/`。

### 批 1・编译器侧加固（forbid 化 + clippy 双 lint）

描述：把 F1 的 `deny` 升 `forbid`，把 F9 死代码族与 SAFETY 块面交给编译器/clippy，**不摘任何腿**。
方案：① [functions/mod.rs:36](../../src/kernel/functions/mod.rs#L36) `deny(unsafe_code)` → `forbid(unsafe_code)`；
② 四树 crate 根加**逐个** lint 的 `#![forbid(dead_code, unused_imports, unused_variables, …)]`（**禁用组形态**，P4/P6）；
③ clippy 四树腿加 `-F clippy::undocumented_unsafe_blocks`（`missing_safety_doc` 已由 `-D warnings` 执法）；
④ 按批 0 清单补 SAFETY 注释（含 84 处 auto-trait impl 中真缺的），**中文**（F7）；
⑤ 负控制: 注入 `#[allow(unsafe_code)]` / 无 SAFETY 块 / `#[allow(dead_code)]` 各一 → 必红 → `cp` + `sha256` 复原。
状态：[]
详情：本批**不动腿清单、不动 AGENTS**。侧车 `totals` 已为 0，故无存量抑制冲突；
若补注释过程中发现新的 clippy 抑制需求，按 F9 **不得新增**，改修代码。

### 批 2・clippy 配置化（disallowed-*）

描述：把 once_cell 与 I6（若批 0 判定可转）的判据写进 clippy 配置，**仍不摘腿**（双执法并存一轮回）。
方案：① `src/kernel/clippy.toml` 加 `disallowed-types = ["spin::Once", "spin::OnceCell"]`；
② 按 **决策 D-2** 给 `host-tests/`、`src/user/` 补配置（推荐**只放 disallowed-\***，不引入阈值，
避免这两树首次受 `cognitive-complexity` 等约束而冒出未知存量）；
③ 路径一律 **crate 名绝对**（cp 探针: `crate::…` 不命中）；
④ 负控制: 注入 `use spin::Once;`（只 use 不用）→ 必红 → 复原；
⑤ 顺带把 §2.4 末行的**配置覆盖缺口**读数写进本册，作为 D-2 的依据。
状态：[]
详情：`src/rust/clippy.toml` 与 `src/kernel/clippy.toml` 内容重复，按 E 探针**内核腿实际读前者以外的
`src/kernel/clippy.toml`**；两份都要改还是收敛成一份，属 D-2 范围。本批仍不动 AGENTS。

### 批 3・退役与收敛（**唯一一次 AGENTS 改动**）

描述：在批 1/2 的替代执法已证明会红之后，摘掉恒真腿、收敛部分腿，并同步全部连带面。
方案：① 删 `audit_reverse_deps.py` 整腿（脚本 + 清单 + 锁针 + selftest 用例）；
② 删 `audit_once_cell.py` 整腿（配置已替代）；
③ `audit_invariants.py` 收 I1–I5（保留 I6）；
④ `audit_safety_coverage.py` / `tools/audit_unsafe.py` 收敛为 **extern + 其它引用**面（块/fn/impl 交 clippy）；
⑤ `audit_dead_code_comment.py` 收源码死代码族面（**保留构建参数扫描 + 侧车对账**）；
⑥ `audit_functions_boundary.py` 收 unsafe 面（保留黑白名单）；⑦ `audit_c_naming.py` 收驼峰面；
⑧ **同批**同步 §五 连带面清单全部条目，含那一次 `AGENTS.md` 改动；
⑨ 每条退役腿做**替代执法负控制**（注入违规 → 新执法点转红 → 复原），证明不是净损失。
状态：[]
详情：本批是门禁清单的唯一变更点，故 AGENTS 授权在此一次性用掉。
`audit_block_registration.py` **不在本批**（D-1 未裁定前保留）。

### 批 4・收口

描述：台账与册内状态同步、memory 更新、按路径提交推送。
方案：① 本册状态字段全 `[X]`；② `docs/plan/progress-active-tasks.md` 甲表行回填读数；
③ 新文件**先 `git add`** 再跑 `scan_obsolete_naming`（扫 `git ls-files`）与 `check_doc_paths`；
④ 七门槛复跑取读数；⑤ 按路径 `git commit --only --pathspec-from-file`，不夹带其余暂存态。
状态：[]

---

## 五、连带面清单（批 3 必须同批改，漏一处即红）

| 面 | 具体位置 | 为什么必须同批 |
|---|---|---|
| 腿清单 | [ci/audit.sh:66](../../ci/audit.sh#L66) `AUDIT_REQUIRED_LEGS`（19 项）、[:88](../../ci/audit.sh#L88) `AUDIT_REQUIRED_LEGS_FULL`（10 项） | 到位断言按字面清单比对，摘腿不改清单即「未执行」硬红 |
| **P-69 反向对账** | [ci/audit.sh:94-138](../../ci/audit.sh#L94) | 以 AGENTS §2.2 反引号点名为外部期望 ⇒ **AGENTS 必须同批改** |
| 各腿调起块 | `ci/audit.sh` 的 invariants / unsafe / once_cell / c_naming / safety_coverage / functions_boundary / dead_code_comment 各 `if` 块 + `mark_leg` | 脚本删了而调起块还在 ⇒ `audit_leg_missing` fail-closed |
| **AGENTS.md**（一次授权） | §2.2 表与两条脚注、§5 F1/F4/F9 的「检查方式」栏、§3 clippy 配置口径 | 根文档属 §6 必同步面；P-69 与多枚锁针直接读它 |
| host-tests 结构锁 | `ci_entrypoint_unified_test.rs`: `test_audit_sh_carries_hard_rule_audits`（[:184](../../host-tests/tests/ci_entrypoint_unified_test.rs#L184)，[:105](../../host-tests/tests/ci_entrypoint_unified_test.rs#L105) 点名 `audit_safety_coverage.py`）、`test_audit_sh_audit_legs_are_fail_closed`（[:217](../../host-tests/tests/ci_entrypoint_unified_test.rs#L217)）、`test_p53_f9_dead_code_comment_gate_is_wired`（[:302](../../host-tests/tests/ci_entrypoint_unified_test.rs#L302)）、`test_p69_leg_manifest_is_reconciled_against_agents`（[:673](../../host-tests/tests/ci_entrypoint_unified_test.rs#L673)）、`test_p50_audit_scope_and_selftest_are_wired`（[:752](../../host-tests/tests/ci_entrypoint_unified_test.rs#L752)，含 once_cell BASES / safety arch 自述 / c_naming fail-closed 三组针）、`test_p56_statistics_legs_removed_steps_renumbered_and_lockbud_states`（[:2000](../../host-tests/tests/ci_entrypoint_unified_test.rs#L2000)） | 这些针把**脚本内部结构**与 AGENTS 文本钉死；机制替换按先例**改钉不删针** |
| 审计自测 | `scripts/tests/audit_selftest.py`（12 用例，其中 `test_audit_block_registration` / `test_audit_unsafe` 等 5 个指向被退役/收敛脚本） | 自测本身是 full 档阻断腿，指向已删脚本即红 |
| must-sync 文档 | `docs/explain/spec-engineering.md`（F 表指向 AGENTS，不自持副本）、`docs/README.md` 若点名脚本 | §6 要求 explain/根文档与代码同步；`check_doc_paths` 会查路径新鲜度 |

---

## 六、决策点（施工前须用户裁定）

| 编号 | 议题 | 选项 | 推荐 |
|---|---|---|---|
| **D-1** | `audit_block_registration` 处置 | 甲 保留自建腿（生态无按文件豁免）/ 乙 先把 host-tests 4 处直调改走桥接或 test-only 出口，再可见性收紧 / 丙 降为报告腿 | **甲**（乙属结构改造，超出「门禁替换」范围；丙违用户「防回归优先固化门禁」偏好） |
| **D-2** | clippy 配置统一方式 | 甲 每树最小配置（`host-tests/`、`src/user/` 只放 disallowed-*，阈值维持现状）/ 乙 统一 `CLIPPY_CONF_DIR` 单源（两树**首次**受阈值约束，需先取存量读数） | **甲**（外科手术，§11.2；乙会顺带改两树阈值口径，属另一件工程） |
| **D-3** | forbid 射程 | 甲 functions `unsafe_code` + 四树 `dead_code`/`unused*` 逐个 / 乙 更宽（含 clippy restriction 组） | **甲**（乙会把大量风格判据升硬错，存量未知） |
| **D-4** | 乙类是否后续立项 | 甲 本册登记为开放项，不施工 / 乙 另开一册逐项实测 | **甲**（用户本轮只授权甲类分批） |

---

## 七、每批验证门槛（§2.3 底线，任何批不豁免）

1. `./ci/build.sh all` 双架构 0 error / 0 warning
2. `bash ci/audit.sh full` **RC=0**（档位必写；quick 不含 2d/2e/2f/3/4，不构成验收）
3. `make test-host` 全绿
4. `make test-kernel-host` 0 failed（三成员，裸 `cargo test --lib` 属假绿）
5. `make -f Makefile.ci ci-rustdoc` 双架构 exit 0
6. QEMU 集成（本程不动 boot，但 full 档内含，随批跑）
7. 本批新增/改动执法点的**负控制**读数（注入 → 红 → 复原 → sha256 一致）

> 另: 新文件入库前不在 `git ls-files` 扫描面内 ⇒ 提交后须复跑 `scan_obsolete_naming` 与 `check_doc_paths`。
> 根目录裸 `cargo fmt` 会改写 vendored smoltcp ⇒ 只对目标 crate 用 `--manifest-path`。