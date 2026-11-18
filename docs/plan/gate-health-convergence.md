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
- 不需要人工维护扫描根 / 白名单 / 基线快照（少一个会静默缩小的扫描面）;
- **不得对合法的第三方宏展开硬红**（批1 实测追加的否决条件，见 §2.5 H）。上一条的「不受宏展开影响」
  只说*检出精度*，不说*可执法性*: `forbid` 会与宏体内上游自带的 `allow` 冲突成 E0453，
  把 vendored 依赖的合法 workaround 判成违规。HIR 级判据若因此在本仓不可用，则它**不比自建腿更强**，
  按用户判据该自建腿仍属必要。

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
| F1 **批 1 前**为 `deny`，全仓无任何 `#![forbid(...)]`（**批 1 后已改**: functions crate 根现为 `forbid`，见 [:38](../../src/kernel/functions/mod.rs#L38)） | 取证当时 [functions/mod.rs:36](../../src/kernel/functions/mod.rs#L36)；`grep -rn "forbid(" src/ host-tests/ kani-harness/ --include=*.rs` 仅命中 shell 函数名针 |
| **F9 侧车存量已清零**（`totals = {sites: 0, files: 0}`）⇒ clippy 配置化零存量成本。**但「forbid 化零存量成本」这半已被批 1 证伪** | `scripts/audit_dead_code_comment.baseline.json`；侧车内 `missing_safety_doc` / `undocumented_unsafe_blocks` / `disallowed` 均 0 次。**口径缺口**: 侧车只统计**本仓源码**的抑制注解，**不统计第三方宏展开发射的 allow** ⇒ 由「侧车 0」推不出「升 forbid 不会红」。死代码族实测 42 处 E0453 全部来自 vendored bitflags 宏（§2.5 H）。`unsafe_code` 那半仍成立（functions 树 0 unsafe，批 1 七门槛为证） |
| `spin` **已不是任何树的直接依赖**（无成员写 `spin =`）⇒ once_cell 判据零存量。**但「射程为空」只对三树成立**（批 2 订正） | [src/kernel/Cargo.toml:50-52](../../src/kernel/Cargo.toml#L50) 自述「原 `spin` 依赖已删」；四树 `Cargo.toml` 无 `spin =`。五份 `Cargo.lock` 实测: kernel / host-tests / src/rust / kani-harness **各 0 条 spin**，`src/user/Cargo.lock` **1 条 spin 0.9.9**（wasmi 2.0.0 传递依赖，[:89](../../src/user/Cargo.lock#L89)），且 `src/user/vendor/spin/` 已在树内 ⇒ 某成员一旦显式声明 spin 即可达，**src/user 的射程非结构性为空** |
| privileged unsafe **1953 处 / 自建口径 100% 覆盖**（该覆盖率**虚高**，见 §2.5） | `python3 tools/audit_unsafe.py --summary` 实跑: 块 1517 / fn 290 / extern 127 / 其它 19 |
| auto-trait `unsafe impl` **84 处被自建脚本豁免**，clippy 不豁免；豁免理由**事实错误** | [tools/audit_unsafe.py:242-246](../../tools/audit_unsafe.py#L242) 自述「Rust 编译器对 Send/Sync impl 自动验证类型安全」——显式 `unsafe impl Sync` 是**手工断言**，编译器只查 auto trait 的**自动推导**，不验证显式 impl |
| `audit_reverse_deps` **结构恒真** | [audit_reverse_deps.py:32/36](../../scripts/audit_reverse_deps.py#L32) 扫 `privileged/` 找 `crate::functions`；而 P-71 拆 crate 后 [lib.rs:113/129](../../src/kernel/lib.rs#L113) 只是 `pub use … as …` 别名，privileged 作为独立 crate 编译时 `crate::functions` **不可解析**；全仓 3 处命中全是注释 |
| `audit_invariants` **只扫 functions**，且自述已被编译期覆盖 | [audit_invariants.py:23](../../scripts/audit_invariants.py#L23) `FUNCTIONS = src/kernel/functions`；[:56](../../scripts/audit_invariants.py#L56)「functions 层 `#![deny(unsafe_code)]` 编译期保证」 |
| ~~I6 是 safe API 边界判据，forbid 不覆盖~~ **已证伪：I6 结构恒真** | [audit_invariants.py:111-120](../../scripts/audit_invariants.py#L111) 的三个模式 `dma_map` / `dma_unmap` / `dma_sync` 在**全仓 `.rs` 零命中**（真实 DMA 面是 `DmaStream` / `alloc_coherent`，见 §2.5）⇒ 该检查永不可命中。上一轮「必须保留 I6」的判断**错误**，据此裁定 **D-6 甲：整腿退役** |
| `block_registration` 的低层入口有 **host-tests 4 文件直调** | `egdf_register_block_dev` 出现在 `host-tests/src/framekernel_bench.rs`、`tests/hotplug_reenum_contract_test.rs`、`tests/egdf_registry_io_host_test.rs`、`tests/egfs_disk_persist_test.rs` ⇒ 可见性收紧会断测试面 |
| `c_naming` 的驼峰面**今天已被 rustc 执法** | [audit_c_naming.py:50](../../scripts/audit_c_naming.py#L50) `CAMEL_CASE_FN` 自述「近似 non_snake_case」；clippy 四树腿均带 `-D warnings` ⇒ `non_snake_case` 已是硬错 |
| **clippy 阈值配置对两树失效**（预存缺口，本轮取证顺带发现；**批 2 只补 disallowed 面，阈值面按 D-2 甲刻意不补**） | 取证当时 `src/kernel/clippy.toml` 与 `src/rust/clippy.toml` 存在，`host-tests/`、`src/user/` **无**；四树扩围腿在 `popd` 后以 `PROJECT_ROOT` 为 cwd（根也无配置）⇒ 按 §2.3 的 E 探针，这两树跑的是 **clippy 默认阈值**。AGENTS §3 只写 `src/rust/clippy.toml`，与实测不符。**批 2 后**: 两树各有 `clippy.toml` 且实测被发现（P2/P3），但**只含 `disallowed-types`**，阈值仍是 clippy 默认 ⇒ 缺口**部分闭合**，阈值面与 AGENTS §3 的四份配置清单同步归批 4 |

---

### 2.5 批 0 补数（权威读数，2026-11-18）

> 探针在 `/tmp/gate-b0`，**仓内源码零改动**；临时 flag 只出现在取证命令里，未写进 `ci/`。

**A. SAFETY 真实缺口 = 399 处（上一轮「≤3 处」的估计错了约两个数量级）**

七腿各加 `-W clippy::undocumented_unsafe_blocks`（不带 allow，见 F 探针）真跑，按 (文件, 行, 形态) 去重取并集:

| 取数腿 | 读数 | RC |
|---|---|---|
| kernel x86_64 | 172 | 0 |
| kernel aarch64 | 206 | 0 |
| host-tests（`--all-targets`） | 25 | 0 |
| src/user x86_64 | 128 | 0 |
| src/user aarch64 | 138 | 0 |
| src/rust 壳 | 0 | 0 |
| edgine-abi | 0 | 0 |
| **并集** | **399**（块 382 / impl 17） | — |

按树: kernel 236 / src/user 138 / host-tests 25。**74 处只在 aarch64 维可见** ⇒ 单架构取数会漏。

**归因**（自建腿报 100% 而 clippy 报 399，谁对）: [tools/audit_unsafe.py:190-218](../../tools/audit_unsafe.py#L190)
的块规则**穿透整个 fn 体**向上找 60 行，遇 fn 签名再找 60 行，且接受 fn 级 `/// # Safety`
⇒ 同一 fn 内**任意一处** SAFETY 注释即「覆盖」该 fn 内**全部** unsafe 块。clippy 的判据比
「紧邻上一行」**宽**（批 3 实测订正）: 块前的注释串**允许空行间隔**——`copy_user.rs` 四处
「占位 SAFETY + 空行 + 块」均不报，负控制 B 在唯一缺口前插注释并**故意隔一空行**，读数即
399→398；但注释与块之间隔了**非注释 token**（fn 签名 / 属性 / 语句）就不认
（`setup_recovery` 的 fn 级 `// SAFETY:` 在签名之前 ⇒ 体内首块仍报，是该文件唯一缺口）。
故两腿的差别不在「是否允许空行」，而在**回溯窗穿透整个 fn 体**与**接受 fn 级文档**。
399 不是 clippy 过报，是自建腿的窗口语义把缺口吞掉了 —— 这正是
「100% 覆盖」读数的成因，也是用户裁定改走 clippy 检测的依据。

**无配置逃逸口**: `accept-comment-above-statement` / `accept-comment-above-attributes`
（注意**复数**）两键全开重跑，读数**仍 172**，不减。附带正控制: 键名误写单数 ⇒
**RC=101「unknown field name」+ did-you-mean** ⇒ 既证明 `CLIPPY_CONF_DIR` 确被读到
（避免「配置没生效」的假绿），也证明 clippy 对未知配置键 **fail-closed**。

**B. 存量按模块分布（清零子批的 sizing 依据）**

| 树 | 模块分布 | 小计 |
|---|---|---|
| kernel | arch 58 / mm 38 / net 38 / glue 16 / sync 16 / klog 11 / driver 10 / cpu 7 / `lib.rs` 6 / boot 4 / freg 4 / idt 4 / proc 4 / syscall 4 / timer 4 / egdf 3 / privileged 根 3 / `memory_allocator.rs` 3 / ipc 1 / sched 1 / sgeg 1 | **236** |
| src/user | userlib 112 / proctest 7 / install 7 / exectest 6 / eash 3 / init 3 | **138** |
| host-tests | `tests/` 19 / `src/` 6 | **25** |

单文件 Top: `userlib/src/sys.rs` **110**、`privileged/net/init/sm_fi.rs` 27、
`privileged/arch/aarch64/gic.rs` 23、`privileged/glue/string.rs` 16、`privileged/klog/mod.rs` 11、
`privileged/arch/aarch64/mod.rs` 10、`host-tests/tests/elf_verify_unification_test.rs` 9。
⇒ userlib 一处占全仓 27.6%，且形态高度重复（syscall 包装），宜单独成批、理由单源化。

**C. I6 恒真取证（推翻上一轮「必须保留」的判断）**

`dma_map` / `dma_unmap` / `dma_sync` 三个模式在**全仓 `.rs` 零命中**。真实 DMA 面是
[dma_buf.rs:80](../../src/kernel/privileged/dma_buf.rs#L80) `pub struct DmaStream`、
[dma/api.rs:95](../../src/kernel/privileged/dma/api.rs#L95) `fn alloc_coherent`、
[prelude.rs:23](../../src/kernel/privileged/prelude.rs#L23) 的导出，以及
[functions/driver/mod.rs:25](../../src/kernel/functions/driver/mod.rs#L25) 自述
「所有 DMA 走 `privileged::dma_buf::DmaStream`」。
⇒ **I6 的真实执法 = privileged 的 safe API 封装 + F2 黑名单**（functions 不得直引 privileged
内部模块），不是文本扫描。删 `check_i6` 不损失任何执法。
另: `audit_vacuous_needle.py` **只扫 Rust 的 `.contains/.matches/.find` 反向针，不覆盖 Python
正则腿** ⇒ I6 的恒真长期无门禁看住，这是本次体检顺带暴露的**门禁盲区**（登记，不在本轮扩围）。

**D. 拟 forbid 的 lint 名与默认级（`rustc -W help` 直读，nightly-2026-10-02）**

`unused` 组共 25 成员，其中 **allow-by-default 2 个**（`unused_macro_rules` / `unused_extern_crates`）；
组外另有 warn 级 `unused_associated_type_bounds` / `unused_comparisons`，以及 allow-by-default
`unused_crate_dependencies` / `unused_import_braces` / `unused_lifetimes` / `unused_qualifications` /
`unused_results`。`dead_code` 与 `unsafe_code` 均存在（前者默认 warn，后者默认 allow）。

⇒ 本轮 `-F` 射程**曾拟**为 **25 个 warn 级名**（组内 23 + 组外 2）:
`dead_code` `unused_imports` `unused_variables` `unused_visibilities` `unused_assignments`
`unused_mut` `unreachable_cfg_select_predicates` `unreachable_code` `unreachable_patterns`
`unused_must_use` `unused_unsafe` `path_statements` `unused_attributes` `unused_macros`
`unused_allocation` `unused_doc_comments` `unused_features` `unused_labels` `unused_parens`
`unused_braces` `redundant_semicolons` `map_unit_fn` `repeated_reprs`
`unused_associated_type_bounds` `unused_comparisons`。
存量恒 0 由现有七腿的 `-D warnings` 证明（warn 级 lint 在那些腿上已是硬错）。
**allow-by-default 族不在射程**——升硬错会冒出未知存量，属另一件工程（D-3 甲的边界）。

> **本清单已作废（批1 实测证伪，见 §2.5 H）**。它作为「lint 名与默认级」的取证读数仍然有效，
> 但**不再作为执法射程**: 其中 `dead_code` / `unused_imports` / `unused_mut` /
> `unused_attributes` / `unused_doc_comments` 五名在本仓**无法**升 `forbid`（CLI 与源码两路
> 同红 42 处 E0453，病灶在 vendored bitflags 的宏展开）。死代码族整体撤回编译器面，
> 继续由 `audit_dead_code_comment.py` 执法。`unsafe_code` 不受此影响（默认 allow、
> 无第三方宏自带 allow），批1 只落它一个。

**E. lint 级交互（决定棘轮腿形态）**

| 探针 | 形态 | 读数 | 结论 |
|---|---|---|---|
| L1 | clippy CLI `-F dead_code` + 局部 `#[allow(dead_code)]` | **RC=101，E0453** | **CLI `-F` 与 crate 根 `#![forbid]` 等强** |
| L1b | rustc CLI `-F dead_code` + crate 根 `#![allow(dead_code)]` | **RC=1，E0453** | 内属性同样压不住 CLI `-F` |
| L2 | `-D warnings -W clippy::undocumented_unsafe_blocks` | **RC=101，error 级** | **不能**把 `-W` 加进现有 `-D warnings` 腿——399 存量会当场硬红 |
| L3 | 只 `-W clippy::undocumented_unsafe_blocks` | RC=0，warning 级 | 棘轮腿须用**自己的取数命令**（不带 `-D warnings`） |
| L4 | `--cap-lints allow -F dead_code` | **RC=0** | CLI `-F` 同样被中和 ⇒ **构建参数扫描腿必须保留** |
| L5 | `-W` 取数在**同一隔离 target 目录**连跑两次 | run1 20.61s / 165 条；run2 **0.12s / 0 重编 / 仍 165 条** | **本工具链 cargo 会复播 Fresh unit 的诊断** |
| L6 | crate 根 `#![forbid(unsafe_code)]` + 模块内 `#![deny(unsafe_code)]` | **RC=0**（只有 warning 级 dead_code）；同形换 `allow` ⇒ **RC=1 E0453** | **forbid 只与 allow 冲突，不与更弱的 deny 冲突** ⇒ functions 树内 **286 处**模块级 `#![deny(unsafe_code)]` 与约 30 枚钉它的 host-tests 契约针**全部不用动** |
| L7 | 死代码族升 `forbid`（**CLI `-F` 与源码 crate 根 `#![forbid]` 两路各跑一次**，kernel-privileged x86_64） | **两路读数逐字相同: 42 处 E0453，RC=101** | **死代码族在本仓不可交编译器**——`forbid` 与 vendored 宏体内上游自带的 `allow` 冲突。详见 §2.5 H |

L1/L1b 曾推出的结论「死代码族可以**只在命令行**加固，覆盖 host-tests 的 200 个集成测试 crate 根
与 src/user 的 10 个 workspace 成员，零源码改动」**已被 L7 证伪并撤回**。L1/L1b 本身的读数仍成立
（它们用的是**本仓源码里**的 allow），错在把「本仓 allow 压不住」外推成「本仓可以升 forbid」——
漏掉了**第三方宏展开也会发射 allow** 这一面。这正是 §1.1 追加第四条否决条件的由来。
`unsafe_code` 的命令行/源码加固不受影响: 它是 allow-by-default，vendored 宏不会自带
`#[allow(unsafe_code)]`（那等于自废 unsafe），故 L1/L1b 的等强结论对它仍然可用。

L5 推翻了「照搬先例」的设计: `audit_missing_docs_debt` 用 `-D` + 逐包隔离，是因为它要硬执法
且曾遭**跨包污染**（依赖方以普通 rustc 编过 ⇒ 下一轮 Fresh ⇒ 读数归零），**不是**因为 warn 级
不复播。本腿用 `-W` 不中断编译 ⇒ 一次 `--workspace` / `--all-targets` 即可取全树，七次调用足够。
但复播是**工具链行为**而非契约 ⇒ 脚本须带**读数塌缩守卫**（见批 3）。

**F. 两处自述与实现不符（预存问题，本轮记录不顺手改）**

- [src/kernel/clippy.toml:5](../../src/kernel/clippy.toml#L5) 自述「强制 SAFETY 注释紧邻 unsafe 块」，
  但**从未启用任何相关 lint** ⇒ 该自述长期为空话。批 3 接入棘轮后此话才成真，同批订正。
- [tools/audit_unsafe.py:243](../../tools/audit_unsafe.py#L243) 的 auto-trait 豁免理由事实错误（§2.4）。

**G. 扫描面: 自建脚本是编译器面的严格超集（批1 后升为主论据）**

`audit_dead_code_comment.py` 的 `SCAN_ROOTS` 是**五树**
（`src/kernel` / `host-tests` / `src/user` / `src/rust` / `kani-harness`），
而 clippy 腿只覆盖前四树 —— `kani-harness` **不在任何 clippy 腿**。
⇒ 死代码族若交编译器，该树会**失去执法**；脚本面则是编译器面的**严格超集**。
另: 脚本按源码文本判，能区分「本仓写的 allow」与「宏展开带进来的 allow」，
而 `forbid` 不能（§2.5 H）。两条合起来 ⇒ 按用户判据，该腿**必要**，不是「可零成本替代」。
（另补一条 kani clippy 腿属范围扩张，本轮不做。）

**H. 死代码族交编译器在本仓不可行（批1 决定性证伪）**

- **实测**: kernel-privileged x86_64 加死代码族 `forbid`，**CLI `-F` 路**与**源码 crate 根
  `#![forbid]` 路**各真跑一次，读数**逐字相同**: `42` 处 `error[E0453]: allow(<lint>) incompatible
  with previous forbid`，`RC=101`。涉及 5 个 lint 名: `dead_code` / `unused_imports` /
  `unused_mut` / `unused_attributes` / `unused_doc_comments`。
  （编译器尾行另报 "due to 72 previous errors"，与显示的 42 条不一致；本册记**显示条数**为读数，
  不追这个差值——它不影响「两路同红」的结论。）
- **病灶不在本仓源码**: 每条 E0453 都带 `originates in the macro` 指回
  [src/user/vendor/bitflags/src/lib.rs:497](../../src/user/vendor/bitflags/src/lib.rs#L497)
  与 `:588` 的 `__impl_public_bitflags_consts!`，以及 `public.rs:196` / `:553` 的
  `mod __bitflags_flag_names`。上游 **bitflags 2.13.2** 为绕
  `bitflags/bitflags#320` **无条件**发射那串 allow（注释逐字写着 "Workaround for"）。
  真实展开点 9 处 = privileged 6 + functions 3。
- **不可修**: vendored 子树锁定（AGENTS §1，升级须用户授权），且那是上游合法 workaround，
  改它等于自造分叉。`--cap-lints allow` 能中和（L4）但**同时中和掉所有 forbid**，
  等于自废门禁，且被 F9 构建参数腿硬零拦。
- **结论**: 死代码族的编译器面在本仓**比自建腿更弱**（会硬红合法代码 + 漏 kani-harness），
  按用户判据 `audit_dead_code_comment.py` 的**源码面必要、整腿保留**。
  批4 原收敛项⑤（把 warn 级死代码族面交 clippy）**撤回**；D-3 甲的 forbid 射程收缩到
  `unsafe_code` 一项。这是**实测驱动的订正**，不是方案 A/B 的酌情选择。
- **保留的加固**: `unsafe_code` 不受 H 影响（allow-by-default，vendored 宏不自带该 allow），
  批1 只落 functions crate 根 `#![forbid(unsafe_code)]`，负控制对见批1 详情。

**补记・F9 构建参数扫描腿对门禁脚本自身文本的约束（批 1 施工实测踩到）**

`audit_dead_code_comment.py` 的 `scan_build_flags` 扫 `ci/`、`scripts/` 下的 `.sh`/`.py`/`.mk`/`.toml`
与 `Makefile*`、`.cargo/config.toml`，**判据是文本正则、不区分注释与代码**（只跳过脚本自身）。
批 1 在 `ci/audit.sh` 的 `FORBID_LINTS` 说明注释里写出被禁旗标的**字面形态**（双横线前缀 +
`cap-lints` + `allow`），首轮 quick 即在步骤 0.5m2 转红：`构建参数抑制: 2 处 (硬零)`，
两条命中都是**我自己写的注释行**。改写为不含字面的表述后该腿复绿（`构建参数抑制: 0 处`）。
（该 `FORBID_LINTS` 方案随后按 §2.5 H 整体撤回，`ci/audit.sh` 的**执法面**已逐字节复原到 HEAD
（数组与 5 处 splat 全数反向 Edit 撤回），全文件只剩 1 行散文同步（`:184` 的 `deny` → `forbid`）；
但**这条约束对批 3 / 批 4 仍然生效**，因为它约束的是门禁脚本的说明文字，与方案去留无关。）
⇒ **批 3 新增 `scripts/audit_safety_debt.py`、批 4 改 `ci/audit.sh` 时，说明文字一律不得写出
被禁旗标的字面形态**；要指代它就写「把 lint 上限压到 allow 的那个构建旗标」。
这条约束同时说明该腿**不是**恒真针：它连门禁自己的说明文字都拦。

---

## 三、分类结论（含三次订正）

> **订正轨迹**: 上一轮口头评估报「甲类 5 整腿 + 3 部分」；§二 首轮实测后下修为
> 「2 整腿 + 5 部分 + 1 不可零成本替代」；批 0 补数（§2.5）后定为
> 「3 整腿 + 4 部分 + 1 不可零成本替代」；**批 1 实测（§2.5 H）后定为
> 「3 整腿 + 3 部分 + 2 不可替代」**。四次变化的依据:
> ① `audit_safety_coverage` / `tools/audit_unsafe` **不能整腿退役**——`unsafe extern`（127 处）
> 与「其它引用」（19 处）在 clippy 双 lint 下**都不报**（§2.2 H 探针）;
> ② `audit_block_registration` **不能零成本替代**——`disallowed-methods` 无按文件豁免（cp 探针），
> 而可见性收紧会断 host-tests 4 处直调（**D-1 甲已裁定长期保留**）;
> ③ `audit_invariants` 从「部分收敛（保留 I6）」**升为整腿退役**——I6 的三个模式全仓零命中，
> 结构恒真（§2.5 C），**D-6 甲已裁定**;
> ④ SAFETY 面从「补 84 处注释后交 clippy」改为「**clippy 检测 + 399 处存量棘轮**」——
> 真实缺口是 399 而非 ≤3（§2.5 A），赶工补注释只会产出占位理由、把假读数再灌一遍，
> **D-5 乙已裁定**;
> ⑤ `audit_dead_code_comment` 从「部分收敛（warn 级死代码族交 clippy 命令行 `-F`）」
> **改判为不可替代、整腿保留**——死代码族升 `forbid` 在本仓两路同红 42 处 E0453，
> 病灶是 vendored bitflags 2.13.2 宏体内上游自带的合法 allow（§2.5 H）；
> 且脚本扫描面是五树、clippy 只四树（§2.5 G），编译器面是**严格子集**。
> 按用户判据「生态工具更强才算不必要」，此处生态工具**更弱** ⇒ 该腿必要。
> 这是实测驱动的订正，D-3 甲的 forbid 射程随之收缩到 `unsafe_code` 一项。
>
> **第五次变化待裁定（批 2 实测新增）**: `audit_once_cell` 的 kani-harness 面 clippy **不可达**
> （该树 `cargo clippy` RC=101），与 ⑤ 的「五树 ⊃ 四树」同型 ⇒ 已立 **D-7**（§四）。
> 裁定甲/乙则计数再变（2 整腿 + 3 部分 + 3 不可替代 / 2 整腿 + 4 部分 + 2 不可替代），
> 裁定丙则维持现读数。**本册不在裁定前改动 §3.1–§3.3 的归属**，只在 §3.1 该行加了前提标注。

### 3.1 甲类・整腿退役（3 条）

| 腿 | 替代 | 依据 |
|---|---|---|
| `audit_reverse_deps.py` | **无需替代**（结构恒真，删腿即净收益） | §2.4；crate 拆分后判据永不可命中 |
| `audit_once_cell.py` | `clippy::disallowed-types`（禁 `spin::Once` / `spin::OnceCell`）**只覆盖四树中的三树 + 壳/abi**；**kani-harness 面 clippy 不可达**（批 2 实测，见批 2 详情三）⇒ **本行退役结论待 D-7 裁定，批 4 不得径行摘腿** | §2.2 cp 探针（use-only 也命中）+ §2.4（spin 非任何成员直接依赖 ⇒ 零存量）；批 2 P1–P5 实测四树配置全 live |
| `audit_invariants.py` | I1–I5 → `forbid(unsafe_code)`；**I6 无需替代**（结构恒真） | §2.5 C；I6 的真实执法是 privileged 的 `DmaStream` / `alloc_coherent` safe API + F2 黑名单，**不是文本扫描** |

### 3.2 甲类・部分收敛（3 条，**保留残余面**）

| 腿 | 交出去的面 | **必须保留**的面 |
|---|---|---|
| `audit_safety_coverage.py` + `tools/audit_unsafe.py` | 块 / fn / impl 的**检测器** → `undocumented_unsafe_blocks` + `missing_safety_doc`；存量 399 处交**新棘轮腿**（批 3） | **extern 127 + 其它 19**（生态缺口，§2.2 H 探针）；以及对账/汇总面 |
| `audit_functions_boundary.py` | unsafe 面 → `forbid(unsafe_code)`（**批 1 已落**） | **F2 黑白名单**（约 50 条跨模块依赖边，生态无对应物；可见性收紧是另一条路，属乙类） |
| `audit_c_naming.py` | 驼峰 fn 面（**今天已被 `non_snake_case` 覆盖**，属重复执法） | `uN_t` 类型后缀 + `kmalloc`/`kfree` 命名规则（含 allow-list） |

### 3.3 甲类・不可替代（2 条，**保留自建腿不动**）

共同形态: 生态**有**对应 lint，但它在本仓**表达不了同一判据**，故按用户判据不构成「更强」。

- **`audit_block_registration.py`**（I-43 单一桥接入口，**D-1 甲裁定长期保留**）。
  两条生态路都被实测堵住: `disallowed-methods` 无按文件豁免；`pub(in …)` 可见性收紧会断
  host-tests 4 文件直调。不再作为开放决策。
- **`audit_dead_code_comment.py`**（F9 死代码抑制 + 构建参数 + 侧车对账，**批 1 实测改判保留**）。
  原拟把 warn 级死代码族（25 名）交 clippy 腿命令行逐个 `-F`，**已撤回**: 死代码族升 `forbid`
  会与 vendored bitflags 宏体内上游自带的 allow 冲突成 42 处 E0453（§2.5 H，CLI 与源码两路同红），
  而脚本扫描面是五树、clippy 只四树（§2.5 G）。**整腿保留，批 4 不再收敛它**。
  保留的四张面各自不可替: 源码 allow 注解卫生（含 allow-by-default `unused*` 族与 `cfg_attr` 包裹）、
  构建参数扫描（把 lint 上限压到 allow 的那个旗标连 CLI `-F` 也中和，L4）、
  clippy/rustc 抑制侧车双向对账、kani-harness 单树。

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

> **分批原则**: 批 1 / 批 2 / 批 3 **只加固执法或新增脚本，不动腿清单** ⇒ 不触 `AGENTS.md`；
> 批 4 是**唯一**改腿清单的批（摘 2–3 腿 + 收敛 3–4 腿 + 接新腿，once_cell 那腿视 **D-7** 裁定）⇒ **那一次预授权的 AGENTS 改动落在批 4**。
> 依据: [ci/audit.sh:94-138](../../ci/audit.sh#L94) 的 **P-69 反向对账**把「AGENTS §2.2 反引号点名的脚本」
> 当外部期望，摘腿而不同步改 AGENTS 会**当场硬红**；故 AGENTS 改动无法延后到收口批。
> 批 5a–5g 是 SAFETY 存量清零子批（D-5 乙），批 6 收口。

### 批 0・取证补数（不改执法）

描述：把 §二 尚未精确取数的项补成权威读数，作为后续各批的存量依据。
方案：① 七条 clippy 腿临时加 `-W clippy::undocumented_unsafe_blocks`（**不带 allow**，见 F 探针）真跑，
取真实缺口清单并按位点去重；② 核实 `dma_map` 三模式的全仓命中面，定 I6 去留；
③ 逐个核实拟 forbid 的 lint 名在本工具链（`nightly-2026-10-02`）存在、默认级与存量；
④ 补测 lint 级交互（CLI `-F` vs 源码 allow、`-D warnings` 与 `-W` 的叠加、`--cap-lints`、Fresh 复播）。
状态：[X]
详情：读数全部回填 §2.5，**不落 report**（用户裁定）。临时 flag 只在取证命令里，未写进 `ci/`。
三项结论改变了后续批次形态: 真实缺口 **399**（不是 ≤3）⇒ D-5 乙棘轮；I6 **恒真** ⇒ D-6 甲整腿退役；
CLI `-F` **等强于 crate 根 forbid** 且 cargo **复播 Fresh 诊断** ⇒ 批 1 零源码改动、批 3 用 `-W` 单次取全树。

### 批 1・编译器侧加固（functions crate 根 `forbid(unsafe_code)`；死代码族 CLI `-F` 已证伪撤回）

描述：把 F1 的 `deny` 升 `forbid`，堵掉「下游 `#[allow(unsafe_code)]` 静默逃逸」这一条真实缺口；
**不摘任何腿**。原方案的第二半（F9 warn 级死代码族交 clippy 腿命令行逐个 `-F`）**实测证伪后整体撤回**。
方案：① [functions/mod.rs:38](../../src/kernel/functions/mod.rs#L38) `deny(unsafe_code)` → `forbid(unsafe_code)`
（唯一源码改动，含 3 行中文理由注释）；② 负控制**对**（NC-A 红 / NC-B 静默绿）证明升级真加了执法；
③ 死代码族两路 `forbid` 取证 → 42 处 E0453 → 撤回，`ci/audit.sh` **执法面**逐字节复原到 HEAD；
④ 同步本次改动造成的 **10 处散文失真**（`ci/audit.sh` 步骤 0 说明 1 处 + `tools/check_tcb.sh` 3 处
+ `docs/explain/` 6 处，后者属 §6 强制同步面）。
状态：[X]
详情：

**一、唯一落地的源码改动与负控制对**

**执法面改动 = `src/kernel/functions/mod.rs` 一个文件**（`deny` → `forbid` + 3 行中文理由注释）。
`ci/audit.sh` 的**执法面零改动**（`FORBID_LINTS` 数组与 5 处 splat 全部反向 Edit 撤回），
只同步 1 行说明散文；`tools/check_tcb.sh` 同步 3 处同类散文（含 1 处 `echo` 修复依据文本）。

**散文同步面（10 处，全部由本次改动直接导致，§11.5 本轮修复不另询问）**:
`ci/audit.sh:184`、`tools/check_tcb.sh:11/:47/:74`、
`docs/explain/guide-dev.md:47/:51`、`docs/explain/explain-framekernel.md:82/:86/:126`、
`docs/explain/spec-engineering.md:81`。其中 `explain-framekernel.md:82` 顺带补一句
「取 forbid 而非 deny」的理由（deny 可被消音、forbid 遇 allow 即 E0453），使该页不再只述形态。
**明确不改的三类**: ① 模块级 `#![deny(unsafe_code)]` 约定本身
（`spec-engineering.md:39` 的「新文件首行」、`explain-wasm-runtime.md:3` 的 wasm 全树、
`sgeg-design.md:809` 的泛指）—— 按 L6 它们仍合法且仍是现状；
② `scripts/audit_invariants.py:56` 的同类自述 —— 该脚本批 4 整腿删除，改它属白工；
③ `AGENTS.md` §5 F1 的 `deny` 字样 —— 归批 4 那一次授权改动，本批**不碰 AGENTS**。
**零针破损（已核）**: 全仓 `deny(unsafe_code)` 契约针（`process_vm_wiring_test` /
`fd_table_extraction_test` / `td14`/`td16`/`td17`/`td19` / `dhcp_policy_test` /
`memory_pressure_extraction_test` 等）钉的都是**各文件模块级内属性**，按 L6 一律不动；
`host-tests/` 与 `scripts/tests/` 对 `check_tcb` **零命中**，故那处 `echo` 文本改动无针可破。

负控制对（同一处注入: `intcast.rs` 加 `#[allow(unsafe_code)]` + 一个真 `unsafe { core::ptr::read }` 块）:

| 轮 | crate 根 | 读数 | 含义 |
|---|---|---|---|
| **NC-A** | `forbid`（本批改动） | **2 个 error**: `E0453: allow(unsafe_code) incompatible with previous forbid` + `error: usage of an unsafe block`；`could not compile kernel-functions` | 逃逸口**已堵死**，且 forbid 自身也对该 unsafe 块硬红 |
| **NC-B** | `deny`（HEAD 原状） | **RC=0，0 error**，真编译（`Checking kernel-functions` + `Checking kernel` + `Finished dev profile in 1.16s`） | 同一注入在 deny 下**静默通过** ⇒ 缺口真实存在，升级不是空转 |

NC-B 的假绿风险已前置堵掉: 脚本带 **PRE 断言**（deny 行=1 / forbid 行=0 / allow 注入=1 / unsafe 块=1）、
**真编译断言**（`Checking kernel-functions` 命中=1）、**POST 断言**（forbid 行=1 / allow 残留=0）
与 `cp` + `sha256` 逐字节复原（`NCB-RESTORED=sha256-ok`）。
⇒ RC=0 不是「什么都没注入」，是「注入了但 deny 拦不住」。两轮跑完 `intcast.rs` 探针残留 0。

**二、零连带面（L6 实证）**

crate 根升 forbid 后，functions 树内 **286 处**模块级 `#![deny(unsafe_code)]` 仍合法
（forbid 只与 allow 冲突，不与更弱的 deny 冲突，L6 实测 RC=0），
钉它们的约 **30 枚** host-tests 契约针**全部不用动**。
`audit_functions_boundary.py:186` 对 `#!` / `#[` 开头行一律跳过，故新 forbid 行不被它当违规。
`src/user/vendor`（wasmi 全树）是目录源替换、cargo 自动把 lint 上限压到 allow，
`cargo metadata --no-deps` 实测非 workspace 成员（成员恒 10）；`src/kernel` 的 vendored smoltcp
由 `exclude` 排除 ⇒ 两者都不受本改动影响。

**三、死代码族 CLI `-F` 的证伪与撤回（本批最大产出）**

原方案要在 `ci/audit.sh` 建单源数组 `FORBID_LINTS`（25 个 warn 级名）splat 进**全部 9 处** clippy
调用（七腿 + 2b feature 维 2 处）。施工后取证发现**该路在本仓走不通**:
kernel-privileged 加死代码族 forbid，**CLI 路与源码 crate 根路各真跑一次，读数逐字相同 —— 42 处
E0453、RC=101**，病灶是 vendored **bitflags 2.13.2** 为绕上游 issue #320 在宏体内**无条件**发射的
`allow(dead_code, unused_doc_comments, unused_attributes, unused_mut, unused_imports, …)`
（真实展开点 9 处 = privileged 6 + functions 3）。详见 §2.5 H。
⇒ 按用户判据，生态工具此处**更弱**（硬红合法第三方代码 + 漏 kani-harness 树），
`audit_dead_code_comment.py` **整腿保留**，批 4 收敛项⑤撤回，D-3 甲射程收缩到 `unsafe_code` 一项。
`ci/audit.sh` 的**执法面**已逐字节复原（`FORBID_LINTS` 数组与 5 处 splat 全数反向 Edit 撤回，
不碰 `git restore/reset`，守并发工作树约束），全文件只余 1 行散文同步（`:184`）。

**四、门槛读数与唯一红腿的归因（诚实口径）**

首轮串行七门槛（判据 = 日志内 `*-RC=` 回显，非后台通知退出码）:
`BUILD-ALL-RC=0` · `AUDIT-FULL-RC=1` · `TEST-HOST-RC=0` · `TEST-KERNEL-HOST-RC=0` · `RUSTDOC-RC=0`。
audit full 唯一红腿 = **UEFI(OVMF) 真跑腿**，缺 5 项硬断言
（`[SIG] PASS:` / `[EXEC] PASS: cwd+root` / `[WASM] PASS:` / `[SCHED] SNAP END` / `conserve=ok`），
其余约 20 项判据全符合；BIOS 侧 QEMU 各腿全绿（thread-stress 6/6、aarch64 COW 四判据、
lockdep 构建面双架构两侧）。

**归因（不推定预存，做了决定性负控制）**: 把 `mod.rs` 用 `cp` 还原成 HEAD 的 `deny` 版
（`git diff --stat` 对该文件为空）后单跑 `scripts/uefi_boot_test.sh` ⇒ **RC=0，25/25 全绿**；
再 `cp` 回 forbid 版（sha256 与改装前逐字节相同）重跑 ⇒ **RC=0，25/25 全绿**。
两轮的实测行都是 `[THREAD] FAIL: reap-timeout code=0`，即登记册 U-1 的**有界超时后继续**形态，
故下游 SIG/EXEC/WASM/SCHED 探针照常打印。
红轮那次的日志停在 t=1.43 s `[PROC] [REAP] tgid=13 unreaped pid=14 state=Ready`，
之后到 t=29.26 s 只有 `[HB] tick` 心跳、**没有 `[THREAD] FAIL` 行** ⇒ 有界超时**没触发**，
是更早的静默楔死，把下游 5 项硬断言一并压掉。该形态与登记册 U-2 详情里
「双核轮停在 `[TLS] PASS` 后、心跳仍走」逐字同形，但与 U-1 主条目的「`[REAP]` 缺席」**签名不同**。
⇒ **结论: 红腿是已登记 UEFI 缺陷的偶发更早楔死变体，非本批改动所致**
（本批只改 lint 级属性，functions 树 0 unsafe，codegen 不受影响；两态各一轮真跑同绿为直证）。

**顺带暴露的门禁口径缺口（登记，不在本批施工）**: UEFI 腿的豁免面只覆盖 `[THREAD]` 一行，
而该缺陷存在「楔死在有界超时之前」的变体，会连带压掉 5 项**硬断言** ⇒ 腿整体转红。
即豁免面**窄于**缺陷的实际形态面。这属 [uefi-thread-exit-hang.md](./uefi-thread-exit-hang.md)
的射程（U-1/U-2 取证改装待用户授权），本批**不改该册、不改该腿**，只在此记录观测，
处置方式交用户裁定（§11.5）。

**收口轮（归因闭合后整条串行重跑，判据同取日志内 `*-RC=` 回显）**:
`BUILD-ALL-RC=0` · `AUDIT-FULL-RC=0` · `TEST-HOST-RC=0` · `TEST-KERNEL-HOST-RC=0` · `RUSTDOC-RC=0`。
明细: audit full **本轮因环境缺失未执行腿数 = 0**，档位 full 覆盖步骤 0–4 全部腿；
UEFI(OVMF) 腿 **25 项 ✓ 全符合**，实测行仍是登记册 U-1 的 `[THREAD] FAIL: reap-timeout code=0`
（按横幅显式不判，下游 SIG/EXEC/WASM/SCHED 探针照常打印）⇒ 首轮那次的更早楔死变体**本轮未复现**，
与「偶发、随宿主状态而变」的归因一致。QEMU 真实启动 2/2、线程压力全过、aarch64 COW 四判据全符合、
aarch64 EGFS 三相判据全符合、lockdep 构建面双架构两侧符合、initramfs 载体腿全过。
host-tests **203 套 / 1599 passed / 0 failed**；内核 host 单测三成员 **12 + 807 + 556 = 1375 / 0 failed**；
rustdoc 双架构两腿都真跑，**0 源码级 link 错误 + 0 私有项链接**。
日志: `/tmp/b1r2-gates.log`（五腿 RC 回显 + 全量输出）。

### 批 2・clippy 配置化（disallowed-types 替 once_cell）

描述：把 once_cell 判据写进 clippy 配置，**仍不摘腿**（双执法并存一轮回）。
方案：① `src/kernel/clippy.toml` 与 `src/rust/clippy.toml` 加
`disallowed-types = ["spin::Once", "spin::OnceCell"]`；② 按 **D-2 甲** 新建 `host-tests/clippy.toml`、
`src/user/clippy.toml`，**只放 disallowed-\***，不引入阈值（避免两树首次受 `cognitive-complexity`
等约束而冒出未知存量）；③ 路径一律 **crate 名绝对**（cp 探针: `crate::…` 不命中）；
④ 负控制 + 配置生效正控制（见详情）；⑤ 复核 §2.4 末行的配置覆盖缺口已被本批补齐。
状态：[X]
详情：**负控制不能直接注入 `use spin::Once;`**——无成员直接声明 `spin`（§2.4 批 2 订正行），
注入会先报 unresolved import 而非 `disallowed_types`，证明不了配置生效。改用仓内**真实存在的类型**
临时登记进 `disallowed-types`，验证「只 `use` 不使用也命中」+「该树配置确被读到」，再 `cp` + `sha256` 复原。
spin 条目本身属**射程为空但防线在位**（src/user 例外，见下），这一限制已写进四份配置的中文注释，
**不伪称已验证 spin 命中**。

**一、配置发现机制的订正（原详情措辞不准）**

原文写「edgine-abi 独立 workspace 无配置 ⇒ 回退读 cwd 的 `src/rust/clippy.toml`」——**机制说错了**:
四树扩围腿的 cwd 是 `PROJECT_ROOT`，而仓根**没有** `clippy.toml`，按 cwd 回退根本读不到。
P1 实测（临时往 `src/rust/clippy.toml` 塞一个未知键，利用 clippy 对未知键 **fail-closed**）:
壳腿与 edgine-abi 腿**都 RC=101「unknown field name」** ⇒ 两份都确读到 `src/rust/clippy.toml`。
真实机制是**从该 crate 的 workspace root 向上走祖先目录**: edgine-abi 的 workspace root 是
`src/rust/edgine-abi`，自身无配置 ⇒ 上一级 `src/rust/clippy.toml` 命中。与 cwd 无关。

**二、五轮探针读数（全部 `cp` + `sha256` 复原，仓内源码零残留）**

| 探针 | 改装面 | 读数 | 证明 |
|---|---|---|---|
| P1 | `src/rust/clippy.toml` 加未知键 | 壳腿 RC=101 / edgine-abi 腿 RC=101 | 两腿都读该文件（祖先回退） |
| P2 | `host-tests/clippy.toml` 登记 `core::cell::UnsafeCell` + `tests/tmpfs_test.rs` 只 `use` | RC=101，命中该 use 行 | 新配置被发现 + use-only 命中 |
| P3 | `src/user/clippy.toml` 同上 + `proctest/src/main.rs` 只 `use` | RC=101，命中该 use 行 | 同上（用户态树） |
| P4 | `src/kernel/clippy.toml` 登记 `core::cell::UnsafeCell` | RC=101，**privileged 78 处真实使用点**全红 | 内核配置 live + 命中真实使用点 |
| P5 | 改登记 `core::num::NonZeroU128`（全树零使用）+ `functions/userctx.rs` 只 `use` | RC=101，命中 `functions/userctx.rs:13` | 内核树 **use-only** 也命中 |

P4 单独不够: `kernel-privileged` 先编译即 78 错中断，走不到 `kernel-functions`，故补 P5 用一个
全树零使用的类型把「use-only」这半在内核树也钉死。五轮合计证明: **四份配置全部 live、
`disallowed-types` 键全部生效、use-only 导入即命中**（与 §2.2 cp 探针同结论，覆盖面扩到四树）。
复原核验: `git status` 只剩四份 `clippy.toml`（2 M + 2 A），`userctx.rs` / `tmpfs_test.rs` /
`proctest/src/main.rs` 三个探针载体 sha256 与改装前逐字节相同。

**三、kani-harness 覆盖不对称（本批最大产出，登记不施工）**

`audit_once_cell.py` 的 `BASES` 是**四树**（P-60 甲），实跑扫 **1442 个 `.rs`**
= kernel 730 + host-tests 207 + src/user 503 + **kani-harness 2**。
而 clippy 侧: 七腿覆盖 kernel / host-tests / src/user / 壳 / edgine-abi，**没有 kani-harness 腿**；
且本批实测**该树根本 clippy 不了**——`cargo clippy --manifest-path kani-harness/Cargo.toml`
**RC=101**，`E0433 could not find privileged in the crate root` ×3（来自 `#[path]` 引入的
`functions/wasm/types.rs:739`）+ `unexpected_cfgs`（`cfg(kani)` 未声明）。
即「加一条 kani clippy 腿」这条替代路**已被实测堵死**，不是没接而是接不上。

⇒ 按用户判据「有比自建门禁更好的生态工具才算不必要」，**kani-harness 这 2 个文件的面生态工具更弱**
（与 `audit_dead_code_comment` 因五树 ⊃ 四树而改判保留**同型**）。
若批 4 照 §3.1 原结论整腿退役 `audit_once_cell.py`，这 2 个文件就**失去任何执法**。
**本批不摘腿**（方案明写双执法并存），故无即时损失；处置立为 **D-7 待用户裁定**（§四），
批 4 的 once_cell 项**以 D-7 为前提**，不得径行施工。

**顺带暴露的第二处不对称（同登记不施工）**: 脚本按 `rglob("*.rs")` 扫，故也扫 vendored 子树
（`src/user/vendor` 473 + `functions/net/smoltcp` 96 = 569 个文件，占 1442 的 39%），
而 clippy 四腿一律 `--no-deps` ⇒ vendored 代码不在其射程。vendored 是第三方锁定代码
（smoltcp 另有 `audit_smoltcp_purity` 守零修改），是否属本规则意图射程**本身可议**，
故不单独立项，只在 D-7 里作为口径选项一并交用户裁定。

**四、⑤ 配置覆盖缺口复核结论**

§2.4 末行的缺口**部分闭合**: `host-tests/`、`src/user/` 现各有 `clippy.toml` 且实测被发现，
`disallowed-types` 面已补齐；但按 **D-2 甲**两份**只放 disallowed-\***，
`cognitive-complexity` / `too-many-lines` 等阈值**仍是 clippy 默认**（刻意不引入，避免两树首次
受阈值约束冒出未知存量、超出本批范围）。阈值面与 AGENTS §3「Clippy 配置」只列一份的失真，
一并归批 4 那一次 AGENTS 改动处理。

**五、收口读数（七门槛全绿，改动面 = 4 份 `clippy.toml`，零摘腿）**

五腿 `*-RC=` 回显均 **0**（日志 `/tmp/b2-gates.log`，五腿 RC 汇总在尾）:
`./ci/build.sh all` 双架构 0 error / 0 warning；`bash ci/audit.sh full`
**本轮因环境缺失未执行腿数 0**、UEFI(OVMF) 引导真跑腿可达面判据全符合
（`[THREAD]` 按登记册**显式不判**，属 `uefi-thread-exit-hang` 的既有豁免面，非本批所致）；
host-tests **203 套 / 1599 passed / 0 failed**；内核 host 单测三成员
**12 + 807 + 556 = 1375 / 0 failed**；rustdoc 双架构两腿都真跑
（x86_64 exit 0 / aarch64 exit 0），**0 源码级 link 错误 + 0 私有项链接**。
读数与批 1 收口**逐项相同**——本批只改 clippy 配置的键与中文注释，**零 codegen 改动**，
故一致是预期而非巧合。执法面**一条腿都没摘**（方案明写双执法并存一轮回），
`audit_once_cell.py` 原样保留，实跑仍 **1442 文件 / 四树 0 处残留 / RC=0**。

### 批 3・SAFETY 存量棘轮腿实装（D-5 乙，**本批不接 CI**）

描述：新增 `scripts/audit_safety_debt.py` + 侧车，把 399 处存量冻成**待办清单**，只拦新增。
方案：① 七次取数，镜像 CI 七腿的编译面，但尾旗标**只** `-W clippy::undocumented_unsafe_blocks`
（**不带 `-D warnings` / pedantic**，L2/L3），`--message-format=json`；
② 每腿**独立隔离 target 目录** `other/target/audit-safety-debt/<leg>`（不污染主目录、不跨腿交叉指纹；
路径须由仓库根拼成**绝对路径**——相对路径按各腿 cwd 解析，内核两腿 cwd=`src/rust` 会把产物落到
`src/rust/other/...`，隔离根被劈成两处，首轮实测即如此）；
③ 只取 `code.code == clippy::undocumented_unsafe_blocks`，**位点** = (仓内相对路径, 行, 形态)，
形态由 rendered 文本分 `block` / `impl`（认不出的形态 fail-closed，不静默归「其它」）；
跨腿按位点**去重取并集**（跨架构同一处只记一次），再聚合成**侧车键 = (文件, 形态) 计数**；
④ 侧车 `audit_safety_debt.baseline.json` 冻 399，`--update-baseline` 为唯一写者，`totals` 自洽校验；
⑤ fail-closed（RC=2）: cargo 失败 / 非本 lint 的 error 级诊断 / 诊断无 span / 侧车缺失或不自洽；
⑥ **读数塌缩守卫**: 总读数 0 而侧车 > 0，且七腿 stderr 全无 `Checking`/`Compiling`
⇒ 判「疑似诊断不复播」RC=2，`--update-baseline` **同样拦**（防把侧车一键洗成 0）；
⑦ `--self-test` 内置夹具 + `scripts/tests/audit_selftest.py` 用例；
⑧ 负控制: 注入无 SAFETY 块 → over 红；给一处存量缺口补上**紧邻**的 SAFETY → under 红
（**删注释是 over 方向**——读数上升，不是 under）；复原源码后复绿（侧车不动）；`cp` + `sha256` 复原。
状态：[X]
详情：形态照 [audit_missing_docs_debt.py](../../scripts/audit_missing_docs_debt.py) 的棘轮先例
（双向对账 + 机器派生侧车 + fail-closed），但**不照搬其 `-D` 逐包隔离**——L5 已证本工具链复播
Fresh 诊断，`-W` 不中断编译 ⇒ 一次 `--workspace` / `--all-targets` 取全树，七次调用足够。
**抑制面不重复扫**: F9 侧车 `totals` 已 0，任何新增 `allow/expect(clippy::…)` 或
`allow/expect(<rustc lint>)` 当场红 ⇒ 本腿依赖它，docstring 写明这层依赖。
**侧车键不取行号**（原方案③的 (路径, 行, 形态) 已订正）: 补一条 `// SAFETY:` 就把该文件后续所有
行号推移，行号键会让批 5 每补一处都冒出成百上千条假新增/假过期；改取 (文件, 形态) 计数后，
行移对键**不变**（内置夹具有一项专钉此性质），只有该文件该形态的**处数**变化才判红。
清零子批仍须**同批重导**侧车（先例同形）。
**接线归批 4**，与那一次 AGENTS 改动同批（P-69 与腿清单到位断言）。

**收口读数（本批不接 CI；执法面零摘腿）**

落地物四件: [audit_safety_debt.py](../../scripts/audit_safety_debt.py)（468 行）+ 机器派生侧车
`audit_safety_debt.baseline.json`（**399 处 / 78 文件 / 83 键**）+ `audit_selftest.py` 第 13 用例
（内置夹具 **23/23** + 端到端注入 **5/5**，全套 **13/13 通过**）+ 一处 host-tests 契约锁**改钉**
（[vacuous_needle_gate_test.rs:229](../../host-tests/tests/vacuous_needle_gate_test.rs#L229)
`[Test 12/12]` → `[Test 12/13]`，负控制断言 `![Test 10/10]` 原样存活）。
**该改钉是首轮 `./ci/build.sh all` RC=1 的唯一红因**（`test_vacuous_needle_selftest_and_white_stock_are_locked`
判词「自测项数标注仍停在旧档位」）⇒ 原记「无针钉这些标签，属散文失真非硬红」**失实**，已订正：
标签有针，加用例必须同批改钉（全仓只此一处，已 grep 复核）。

七腿真编译读数（`--update-baseline` 首轮，`/tmp/b3-baseline.log`）: kernel-x86_64 **172** /
kernel-aarch64 **206** / host-tests **25** / user-x86_64 **128** / user-aarch64 **138** /
壳 **0** / edgine-abi **0**；每腿射程外来源 **0**、RC 全 **0**。并集 **399**（块 382 + impl 17）
与批 0 取证**逐项相同**，按树 kernel 236 / src/user 138 / host-tests 25；侧车内 smoltcp /
`src/user/vendor` / kani-harness 路径**零泄漏**（vendored 非 workspace 成员，天然不出）。

成本: 冷启动 **90s**、隔离根 `other/target/audit-safety-debt/` **510M**（七目录）；二次跑七腿
**全 Fresh 仅 1s** 且读数 399 不变 ⇒ L5「Fresh 复播诊断」在本腿形态下再次证实，塌缩守卫
只在工具链行为变化时才会触发（不是常设开销）。

负控制三档（真树注入，`/tmp/b3-neg-*.log`）: **over** 注入一个无 SAFETY 的 `unsafe` 块 ⇒
RC=**1**、并集 400、kernel-x86_64 173，判词钉到 `copy_user.rs — block: 当前 2 / 债册 1`；
**under** 给该文件唯一缺口补 SAFETY ⇒ RC=**1**、并集 398 / 77 文件 / 82 键，判词
`当前 0 / 债册 1`；**复原**两轮均 RC=**0**（399 / 78 / 83），`sha256` 与原件逐字节一致、
`git status` 无残留。

**七门槛读数（零 codegen；`/tmp/b3-gates.log` + 复跑 `/tmp/b3-build2.log`）**: `BUILD-ALL-RC=0`
（首轮 RC=1 即上述计数针，改钉后单独复跑，Passed: **6** / Failed: **0**）· `AUDIT-FULL-RC=0` ·
`TEST-HOST-RC=0`（**203 套 / 1599 passed / 0 failed**）· `TEST-KERNEL-HOST-RC=0`（三成员
**12+807+556=1375 / 0 failed**）· `RUSTDOC-RC=0`（双架构两腿真跑，**0 link 错误**）。
audit full **未执行腿数 0**、QEMU **2/2**、UEFI(OVMF) 可达面判据全符合（`[THREAD]` 显式不判，
属既有豁免面）、lockdep 构建面双架构正向/默认两侧符合、aarch64 COW 四判据 + EGFS 三相判据全符合。
**「本批不接 CI」的精确含义**: `audit_safety_debt` 不在 quick/full 两档腿清单（已 grep 复核，
接线归批 4）；但第 13 用例经既有的 `audit_selftest` 腿**已受 CI 执法**——本轮 audit full 内真跑
**13/13**，五项断言实测 `23/23` · `2` · `0/0` · `1` · `2/2`。

三处自捕缺陷（均已修，不是事后补记）: ① `load_baseline` 的默认实参在 `def` 时冻结真实侧车
路径 ⇒ 注入式自测改 `BASELINE_FILE` 读不到临时快照（首轮 13 用例中 2 项红），改为调用时取
模块常量；② 相对 `--target-dir` 按各腿 cwd 解析 ⇒ 内核两腿产物落到 `src/rust/other/...`，
隔离根被劈成两处，改由仓库根拼绝对路径并把已建目录归位；③ 侧车键原设计含行号（见上）。

一处口径订正连带两段: clippy 的注释位置判据**宽于**「紧邻上一行」（空行间隔被接受，隔非注释
token 才不认）⇒ 批 0 归因段与批 5a 方案①同批订正，批 5 的「紧邻上一行」明确为**项目约定**。

### 批 4・退役与收敛 + 接线（**唯一一次 AGENTS 改动**）

描述：在批 1/2/3 的替代执法已证明会红之后，摘恒真腿、收敛部分腿、接新腿，并同步全部连带面。
方案：① 删 `audit_reverse_deps.py` 整腿（**只脚本 + full 清单条目**；已核: 无独立调起块（只走 2e 泛型循环）、
host-tests 零锁针、selftest 零用例）；
② `audit_once_cell.py` 处置**以 D-7 裁定为前提，不得径行整腿退役**（批 2 实测: kani-harness 面
clippy 不可达，RC=101 / E0433 ×3）——D-7 甲＝整腿不动（本项取消）；乙＝`BASES` 收缩到 kani-harness
单树 + `test_p60` ①面改钉；丙＝整腿退役（连带面＝quick 清单 + `:276-285` 调起块 + `test_p60` ①面改钉，
**selftest 零用例**），并须在 AGENTS 显式记录该 2 文件面无执法；
③ 删 `audit_invariants.py` **整腿**（含 `check_i6`，D-6 甲）——I1–I5 已由批 1 的 `forbid(unsafe_code)` 执法，
I6 恒真且真实执法在 privileged safe API + F2；
④ `audit_safety_coverage.py` / `tools/audit_unsafe.py` 收敛为 **extern + 其它引用**面，
**删 auto-trait 豁免与穿透 fn 体的 60 行窗口**，订正 [:243](../../tools/audit_unsafe.py#L243) 的错误自述；
⑤ `audit_functions_boundary.py` 收 unsafe 面（保留 F2 黑白名单；批 1 已落 forbid，本批只删脚本内的重复检测面）；
⑥ `audit_c_naming.py` 收驼峰面；
⑦ 接 `audit_safety_debt` 进 `full` 档（**只改 `AUDIT_REQUIRED_LEGS_FULL` 清单**——2e 是泛型循环
[:771](../../ci/audit.sh#L771)，改清单即改接线，**无需另写调起块 / `mark_leg`**）；
⑧ 订正 [src/kernel/clippy.toml:5](../../src/kernel/clippy.toml#L5) 与
[src/rust/clippy.toml:5](../../src/rust/clippy.toml#L5) 的空话自述（**两处同形，漏一处即留一句假话**）；
⑨ **同批**同步 §五 连带面清单全部条目，含那一次 `AGENTS.md` 改动；
⑩ 每条退役/收敛腿做**替代执法负控制**（注入违规 → 新执法点转红 → 复原），证明不是净损失。
状态：[]
详情：本批是门禁清单的唯一变更点，故 AGENTS 授权在此一次性用掉。
**两条腿明确不在本批**: `audit_block_registration.py`（D-1 甲已裁定长期保留）、
`audit_dead_code_comment.py`（**批 1 实测证伪后整腿保留**，§2.5 H / §3.3；原⑤「收 warn 级死代码族面」
已撤回，本批**不碰该脚本一行**，其侧车基线与 selftest 用例同样不动）。
④ 之后「privileged unsafe 100% 覆盖」这一读数**不再成立也不再宣称**——块面改由棘轮腿报
「399 处待补写」，这是 D-5 乙裁定的本意: 宁可显式欠账，不要虚高读数。

### 批 5a・SAFETY 清零: userlib（112 处）

描述：清 `src/user/lib` 的 112 处（其中 `lib/src/sys.rs` 独占 110），形态高度重复（syscall 包装）。
方案：① 逐处补**紧邻上一行**的中文 SAFETY 注释（项目约定，**严于** clippy 判据——后者允许空行间隔，见批 0 归因订正）；② 重复形态的理由**单源化**（模块级说明 + 每处指向
具体不变量，不复制粘贴同一句空话）；③ 跑棘轮腿确认读数下降 → `--update-baseline` 收缩侧车；④ 七门槛。
状态：[]
详情：**不得写占位理由**（如现存的「调用方保证指针/类型有效 (详见上下文)」）——每处须给出
指针来源 / 对齐 / 生命周期 / 并发前提的具体依据。这正是 D-5 取乙而非甲的理由: 赶工 399 处
只会把假读数再灌一遍。本批清完 src/user 树仍余 26 处（批 5b），**不翻硬执法**。

### 批 5b・SAFETY 清零: 其余用户 crate（26 处）

描述：清 proctest 7 / install 7 / exectest 6 / eash 3 / init 3。
方案：同批 5a 四步；清完后 **src/user 树小计归零** ⇒ 该树 CI clippy 腿加
`-D clippy::undocumented_unsafe_blocks` 翻**硬执法**（不再靠侧车），侧车该树条目清空。
状态：[]
详情：翻硬执法后该树任何新增无 SAFETY 块在**门槛 2** 即红，无需等棘轮腿。

### 批 5c・SAFETY 清零: host-tests（25 处）

描述：清 `tests/` 19 + `src/` 6。
方案：同批 5a 四步；清完后 host-tests 腿翻 `-D clippy::undocumented_unsafe_blocks` 硬执法。
状态：[]
详情：测试代码的 SAFETY 理由须说明**被探针对象的不变量**，不是「测试用所以安全」。

### 批 5d・SAFETY 清零: privileged mm + sync（54 处）

描述：清 mm 38 + sync 16（含 `mm/cow.rs` 7、`mm/vmm_aarch64.rs` 7、`sync/atomic.rs` 7）。
方案：同批 5a 四步。
状态：[]
详情：TCB 核心，理由须落到页表所有权 / 内存序 / 锁序不变量（I1–I6 与 `ref-lock-order.md`）。
本批**不翻硬执法**（kernel 树尚未归零）。

### 批 5e・SAFETY 清零: privileged arch + cpu + idt + boot（73 处）

描述：清 arch 58（含 `aarch64/gic.rs` 23、`aarch64/mod.rs` 10、`aarch64/mmu.rs` 7）+ cpu 7 + idt 4 + boot 4。
方案：同批 5a 四步。
状态：[]
详情：最大子批。aarch64 独占位点须以 **aarch64 腿**取数验证（74 处只在 arm 维可见，§2.5 A）。

### 批 5f・SAFETY 清零: privileged net + driver + egdf（51 处）

描述：清 net 38（含 `net/init/sm_fi.rs` 27）+ driver 10 + egdf 3。
方案：同批 5a 四步。
状态：[]
详情：`sm_fi.rs` 单文件 27 处，宜先核其 unsafe 形态是否可**收敛到更少的 unsafe 块**
（减少块数比补注释更根本），但结构改造超出本工程范围 ⇒ 只补注释，发现的可收敛点**登记不顺手改**（§11.5）。

### 批 5g・SAFETY 清零: kernel 其余（58 处）

描述：清 glue 16 / klog 11 / `lib.rs` 6 / freg 4 / proc 4 / syscall 4 / timer 4 / privileged 根 3 /
`memory_allocator.rs` 3 / ipc 1 / sched 1 / sgeg 1（合计 58）。
方案：同批 5a 四步；清完后 **kernel 树小计归零** ⇒ 内核双架构腿翻
`-D clippy::undocumented_unsafe_blocks` 硬执法，侧车全表清空，棘轮腿转**硬零**形态。
状态：[]
详情：三树全清后 `audit_safety_debt` 的侧车 `totals` 归 0，该腿从「债册」升为「硬零门禁」，
与 F9 侧车清零后的处置同构。`glue/string.rs` 16 处属 FFI 胶水，理由须写明 C 侧契约。

### 批 6・收口

描述：台账与册内状态同步、memory 更新、按路径提交推送。
方案：① 本册状态字段全 `[X]`；② `docs/plan/progress-active-tasks.md` 甲表行回填读数
（**13 个行首状态字段** = 批 0/1/2/3/4/5a/5b/5c/5d/5e/5f/5g/6）；
③ 新文件**先 `git add`** 再跑 `scan_obsolete_naming`（扫 `git ls-files`）与 `check_doc_paths`；
④ 七门槛复跑取读数；⑤ 按路径 `git commit --only --pathspec-from-file`，不夹带其余暂存态。
状态：[]

---

## 五、连带面清单（批 4 必须同批改，漏一处即红）

| 面 | 具体位置 | 为什么必须同批 |
|---|---|---|
| 腿清单（quick） | [ci/audit.sh:66](../../ci/audit.sh#L66) `AUDIT_REQUIRED_LEGS` **19 → 17**（删 `invariants` / `once_cell`） | 到位断言按字面清单比对，摘腿不改清单即「未执行」硬红 |
| 腿清单（full） | [ci/audit.sh:88](../../ci/audit.sh#L88) `AUDIT_REQUIRED_LEGS_FULL` **10 → 10**（删 `reverse_deps:audit_reverse_deps.py`，加 `safety_debt:audit_safety_debt.py`） | 同上；**2e 是泛型循环**（[:771](../../ci/audit.sh#L771) `for P49_SPEC in …`）⇒ 改清单即改接线，新腿**无需另写调起块** |
| **P-69 反向对账** | [ci/audit.sh:94-138](../../ci/audit.sh#L94) | 以 AGENTS §2.2 反引号点名为外部期望 ⇒ **AGENTS 必须同批改** |
| quick 档各腿调起块 | `invariants` [:220-229](../../ci/audit.sh#L220)、`once_cell` [:276-285](../../ci/audit.sh#L276) 的 `if` 块 + `mark_leg` + `audit_leg_missing`；`reverse_deps` **无独立块**（只走 2e 循环） | 脚本删了而调起块还在 ⇒ `audit_leg_missing` fail-closed |
| 2e 注释块 | [ci/audit.sh:745-770](../../ci/audit.sh#L745) 的逐腿耗时表与「`missing_docs_debt` 是九腿里**唯一真编译**的腿」 | 新增 `audit_safety_debt` 后该叙述失真；须回填**七目录的实测磁盘与冷/热耗时**（先例: 143M / 冷 30.2s / 热 7.7s） |
| **AGENTS.md**（一次授权） | ① [:55](../../AGENTS.md#L55) `audit_invariants.py` 表行删；② [:56](../../AGENTS.md#L56) `audit_once_cell.py` 表行**按 D-7 裁定处置**（甲/乙＝保留，乙须把射程改指 kani-harness 单树 + clippy 三树；丙＝删行）；③ [:66](../../AGENTS.md#L66) 完整清单「28 个 / `scripts/` 25」→ **26 个 / 23**（D-7 丙）或 **27 个 / 24**（D-7 甲/乙），full 档列举删 `audit_reverse_deps` 加 `audit_safety_debt`（条数仍 10）；④ [:68](../../AGENTS.md#L68) 末句「OnceCell 统一性面（`audit_once_cell.py`）已按四树同范围扫描」→ 按 D-7 改写（乙＝脚本守 kani-harness、clippy `disallowed-types` 守其余三树；丙＝改指 clippy 并**显式记录 kani-harness 2 文件面无执法**）；⑤ **§4.2 的 I6** 须写明真实执法 = privileged `DmaStream` / `alloc_coherent` safe API + F2 黑名单，**不是文本扫描**（D-6 甲裁定的记录义务）；⑥ §5 **F1** 检查方式 `deny` → `forbid`；⑦ §5 **F4** 检查方式 → 收敛后 extern+ref 面 + clippy 棘轮腿；⑧ §3 Clippy 配置口径补**每树配置**（D-2 甲） | 根文档属 §6 必同步面；P-69 与多枚锁针直接读它。**八处一次改完**，授权只用一次。**§5 F9 不改**——原拟的「warn 级死代码族由 clippy 腿逐个 `-F` 执法」已按 §2.5 H 撤回，F9 现文（脚本 + 侧车 + 构建参数双路）与保留后的实现仍逐字相符 |
| host-tests 结构锁 | `ci_entrypoint_unified_test.rs`: **`test_p60_scope_closure_and_silent_skips_fixed`**（[:2481](../../host-tests/tests/ci_entrypoint_unified_test.rs#L2481)，once_cell BASES 三组针在 [:2482-2505](../../host-tests/tests/ci_entrypoint_unified_test.rs#L2482)）⇒ **按 D-7 裁定改钉**（丙＝整腿退役后改钉到 clippy 配置；乙＝改钉到「BASES 只含 kani-harness」+ clippy 三树配置；甲＝针不动）；**该测试是四面合针**（① once_cell 四树+BASES+fail-closed ② safety_coverage 不再声称过滤 arch ③ c_naming 读失败进 `read_errors` ④ AGENTS §2.2 写明 F8 射程），只改①，②③④ 须原样存活（②③ 指向的脚本批 4 只收敛不退役）；`test_audit_sh_carries_hard_rule_audits`（[:184](../../host-tests/tests/ci_entrypoint_unified_test.rs#L184)，[:105](../../host-tests/tests/ci_entrypoint_unified_test.rs#L105) 点名 `audit_safety_coverage.py`）、`test_audit_sh_audit_legs_are_fail_closed`（[:217](../../host-tests/tests/ci_entrypoint_unified_test.rs#L217)）、`test_p69_leg_manifest_is_reconciled_against_agents`（[:673](../../host-tests/tests/ci_entrypoint_unified_test.rs#L673)）、`test_p50_audit_scope_and_selftest_are_wired`（[:752](../../host-tests/tests/ci_entrypoint_unified_test.rs#L752)）、`test_p56_statistics_legs_removed_steps_renumbered_and_lockbud_states`（[:2000](../../host-tests/tests/ci_entrypoint_unified_test.rs#L2000)） | 这些针把**脚本内部结构**与 AGENTS 文本钉死；机制替换按先例**改钉不删针**。**`test_p53_f9_dead_code_comment_gate_is_wired`（[:302](../../host-tests/tests/ci_entrypoint_unified_test.rs#L302)）不在此列**——该腿整腿保留、接线不动，针无需改 |
| 审计自测 | `scripts/tests/audit_selftest.py`: **只有 `test_audit_unsafe`（[:547](../../scripts/tests/audit_selftest.py#L547)）需改钉**——它钉 `scan_file` 对「fn 体内 unsafe 块上方无 SAFETY」的命中，批 4 把块面交 clippy 后须改钉到 extern 面。**`test_functions_boundary`（[:81](../../scripts/tests/audit_selftest.py#L81)）不需改钉**：实读该用例只断言 `check_forbidden_imports` 的 `FORBIDDEN_PRIVILEGED_IMPORT`，即批 4 **保留**的 F2 面（本轮复核订正，原记「两例都改钉」过宽）。**新增 `audit_safety_debt` 用例（含塌缩守卫负控制）与 13 处 `[Test N/13]` 标签已在批 3 落地**（第 13 用例，5 项断言：内置夹具 / 侧车缺失 fail-closed / 生成后对账 / 新增判红 / 塌缩守卫双路），批 4 不再重复此项；**标签有针**（[vacuous_needle_gate_test.rs:229](../../host-tests/tests/vacuous_needle_gate_test.rs#L229) 钉 `[Test 12/13]`），此后增删用例须同批改钉，否则 `./ci/build.sh all` 的 host 测试腿当场红。指向 invariants / once_cell / reverse_deps 的用例 **0 个**（已核：现 13 用例点名 7 个脚本＝functions_boundary / deadlock_matrix×5 / block_registration / audit_unsafe / coupling×3 / vacuous_needle / safety_debt） | 自测本身是 full 档阻断腿，指向已删脚本即红 |
| 退役产物消费者 | `other/target/audit/invariants-report.json` **全仓零消费者**（只有 [audit_invariants.py:195](../../scripts/audit_invariants.py#L195) 这个写者） | 已核: 删腿无下游断点，不需要迁移消费面 |
| clippy 配置 | [src/kernel/clippy.toml:5](../../src/kernel/clippy.toml#L5) 空话自述订正；**[src/rust/clippy.toml:5](../../src/rust/clippy.toml#L5) 是逐字相同的孪生假自述**（本轮读两份配置时发现，同批一并订正）；批 2 新建的 `host-tests/clippy.toml`、`src/user/clippy.toml` | 自述与实现不符属预存问题，本工程正是使其成真的那一轮 |
| must-sync 文档 | `docs/explain/spec-engineering.md`（F 表指向 AGENTS，不自持副本）、`docs/README.md` 若点名脚本 | §6 要求 explain/根文档与代码同步；`check_doc_paths` 会查路径新鲜度 |

> **不要求同步的面**: `docs/plan/` 与 `docs/report/` 里点名 `audit_invariants` 的历史文档
> （`code-audit-final-summary.md` / `unresolved-issues-2026-08-09.md` / `multithreading-project.md` /
> `syscall-followup.md` 及多份 report）按 §6 **不要求路径新鲜**——它们记录执行当时的形态，
> 改它们等于篡改历史。`docs/plan/archive/` 同理。

---

## 六、决策点（2026-11-18 全部已裁定）

> 用户回复「授权」⇒ **D-1 甲 / D-2 甲 / D-3 甲 / D-4 甲**；批 0 出数后就两项新议题再裁定
> ⇒ **D-5 乙 / D-6 甲**。六项均已闭合，**无开放决策**。据此:
> `audit_block_registration` **保留自建腿不动**；clippy 配置走**每树最小化**；
> forbid 射程**只剩 functions `unsafe_code` 一项**（D-3 甲原含四树 warn 级 `dead_code`/`unused*`
> 逐个 25 名，**批 1 实测证伪后撤回该半**，见 §2.5 H 与下表 D-3 订正），
> 不含 clippy restriction 组、不含 allow-by-default `unused*`；乙类 7 项**只登记不施工**；
> SAFETY 面走 **clippy 检测 + 399 处存量棘轮**（不赶工补注释）；
> `audit_invariants` **整腿退役**（I6 恒真，删腿即净收益）；
> `audit_dead_code_comment` **整腿保留**（批 1 改判，生态工具在本仓更弱）。

| 编号 | 议题 | 选项 | 裁定 |
|---|---|---|---|
| **D-1** | `audit_block_registration` 处置 | 甲 保留自建腿（生态无按文件豁免）/ 乙 先把 host-tests 4 处直调改走桥接或 test-only 出口，再可见性收紧 / 丙 降为报告腿 | **甲**（乙属结构改造，超出「门禁替换」范围；丙违用户「防回归优先固化门禁」偏好） |
| **D-2** | clippy 配置统一方式 | 甲 每树最小配置（`host-tests/`、`src/user/` 只放 disallowed-*，阈值维持现状）/ 乙 统一 `CLIPPY_CONF_DIR` 单源（两树**首次**受阈值约束，需先取存量读数） | **甲**（外科手术，§11.2；乙会顺带改两树阈值口径，属另一件工程） |
| **D-3** | forbid 射程 | 甲 functions `unsafe_code` + 四树 `dead_code`/`unused*` 逐个 / 乙 更宽（含 clippy restriction 组） | **甲**（乙会把大量风格判据升硬错，存量未知）。批 0 进一步收窄到 **warn 级 25 名**，allow-by-default 族留脚本面。**批 1 实测再订正（非酌情改选，是证伪）**: 死代码族那半**整体撤回**——升 forbid 在 kernel-privileged 两路同红 42 处 E0453，病灶为 vendored bitflags 2.13.2 宏体内上游合法 allow（§2.5 H），且脚本五树 ⊃ clippy 四树（§2.5 G）。**终态射程 = functions `unsafe_code` 一项**；`audit_dead_code_comment.py` 由「部分收敛」改判「不可替代、整腿保留」（§3.3），批 4 原⑤撤回 |
| **D-4** | 乙类是否后续立项 | 甲 本册登记为开放项，不施工 / 乙 另开一册逐项实测 | **甲**（用户本轮只授权甲类分批） |
| **D-5** | SAFETY 面路径（批 0 出数 399 后新立） | 甲 立即补完 399 处注释再把块/fn/impl 面交 clippy（`-F` 硬零）/ **乙 clippy 检测 + 存量棘轮**（`-W` 取数、399 冻侧车、只拦新增、清零后翻硬） | **乙**。甲的成本基数比原估计大约 50 倍，赶工只会产出「调用方保证指针/类型有效 (详见上下文)」这类**占位理由**，把虚高读数再灌一遍；乙把欠账**显式化**，与 `audit_missing_docs_debt` 先例同构 |
| **D-6** | I6 与 `audit_invariants` 处置（批 0 证伪后新立） | **甲 删 `check_i6`，整腿退役** / 乙 保留 I6 并把模式改指真实 DMA 面（`DmaStream` / `alloc_coherent`） | **甲**。I6 三模式全仓零命中 ⇒ 结构恒真，删腿不损失执法（§3.1「删腿即净收益」）；乙等于新造一条文本扫描腿去重复 safe API + F2 已有的执法。**记录义务**: AGENTS §4.2 须写明 I6 的真实执法面 |
| **D-7** | `audit_once_cell` 处置（**批 2 实测后新立，待裁定**） | 甲 整腿保留不动（改判「不可替代」）/ **乙 部分收敛**：`BASES` 收缩到 **kani-harness 单树**，其余三树交 clippy / 丙 照原结论整腿退役，显式接受 kani-harness 2 文件失去执法 | **待用户裁定，批 4 以本项为前提**。病灶: 脚本扫四树 1442 文件，clippy 七腿**不含 kani-harness**，且该树实测 `cargo clippy` **RC=101**（E0433 ×3 + `unexpected_cfgs`）⇒「加一条 kani 腿」已堵死（批 2 详情三）。按用户判据该面生态工具**更弱**，与 `audit_dead_code_comment` 改判保留同型。取舍: 甲零成本但三树永久双执法；乙与另三条部分收敛腿同构、残余面最小（2 文件），且 vendored 扫描面不对称随之消失，代价是 `test_p60_scope_closure_and_silent_skips_fixed` 的①面针（BASES 四树 + fail-closed）须**改钉不删针**；丙损失执法，违「防回归优先固化门禁」偏好。**分类计数随裁定变**: 甲 → 2 整腿 + 3 部分 + 3 不可替代；乙 → 2 整腿 + 4 部分 + 2 不可替代；丙 → 维持 3/3/2 |

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