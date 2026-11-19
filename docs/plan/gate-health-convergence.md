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
**2026-11-19 扩围**：用户裁定先退役现有 kani ⇒ 本批新增前置项 ⓪（kani-harness 整树退役），
它同时把 D-7 的前提消解掉（那棵 clippy 不可达的树不存在了），故 ② 由「待裁」定为**丙 整腿退役**。
方案：⓪ **退役 `kani-harness/` 整树**（4 条 `wasm/leb128` 证明因 `functions/wasm/types.rs:739` 的跨层
`impl From<WasmError> for crate::privileged::syscall::Errno` 在 harness crate 内解析不到 `privileged`
而 E0433 ×3，crate 级不可编译 ⇒ 4 条证明**一条都跑不了**；`make test-kani` 是手动腿、不在七门槛、
无 CI 接线；内核源码零 `cfg(kani)`）。连带面＝`Makefile` 目标与 `.PHONY`、**三个审计脚本的扫描根**
（`audit_c_naming` / `audit_comment_language` 的 `BASES`、`audit_dead_code_comment` 的 `SCAN_ROOTS`
五树→四树，三者**逐根 fail-closed**，删树不改根即当场红）、`audit_safety_debt` 三处自述与侧车口径串、
内核源码 3 处注释、`docs/design` 2 处、host-tests 契约锁改钉、AGENTS §1 布局表与 §2.2 F8 射程。
**4 条证明的性质须留档**（对象是 `functions/wasm/leb128.rs` 的 `read_leb128_u32` / `read_leb128_i32` /
`read_leb128_i64` / `read_name`，**不是 cpio**；每条同一形态: `CAP=12` 的符号字节数组 + `kani::assume`
约束 `len <= bytes.len()` 与 `start <= slice.len()`，调用后 `assert!(pos >= start)` 与
`assert!(pos <= slice.len())`，即「不 panic（含移位/算术/切片索引/`to_vec`）+ `pos` 单调不减 + 不越界」，
unwind 依次 8/8/12/14；crate 根 `#![deny(unsafe_code)]`，harness 自述「试点阶段不接入 CI」），以免静默丢失；
`privileged/sync/memorder.rs:24` 的「那属 Kani/模型检查」是**工具类别泛指**而非本仓 harness，保留不动。
① 删 `audit_reverse_deps.py` 整腿（**原记「只脚本 + full 清单条目／已核无连带」不完整，施工前复核订正**：
[audit_implicit_deps.py:53](../../scripts/audit_implicit_deps.py#L53) `import audit_reverse_deps as reverse_deps`
并在 `:197-198` 调 `scan_comment_lines` / `scan_inline_cfg_test_blocks` ⇒ 径行删脚本会打断
**full 档阻断腿 implicit_deps**。处置＝两个分类器连同 `RE_CFG_TEST_ATTR` / `RE_INLINE_MOD`
**行为不变地迁入** `audit_implicit_deps.py`（实测迁前迁后输出**逐字节相同**: 生产 38 处 / 14 项 /
注释 207 / 测试 61，自测腿同绿），再删脚本 + full 清单条目 + 其 `:7` 的「与 audit_reverse_deps 同源」自述；
另两处连带面＝`ci/audit.sh` 2e 计时表行、[net/syscall.rs:580](../../src/kernel/functions/net/syscall.rs#L580)
点名该腿的注释。host-tests 零锁针、selftest 零用例这两条原记**成立**）；
② `audit_once_cell.py` **整腿退役（D-7 丙，2026-11-19 派生裁定）**——连带面＝quick 清单 +
`:276-285` 调起块 + `test_p60` ①面**改钉到 clippy `disallowed-types` 配置**（不删针），
selftest 零用例；kani-harness 已随 ⓪ 退役，故**不再有「2 文件无执法」的代价**，
AGENTS 只须把 OnceCell 执法面改指 clippy 三树 + 壳/abi；
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
状态：[X]
详情：本批是门禁清单的唯一变更点，故 AGENTS 授权在此一次性用掉。
**两条腿明确不在本批**: `audit_block_registration.py`（D-1 甲已裁定长期保留）、
`audit_dead_code_comment.py`（**批 1 实测证伪后整腿保留**，§2.5 H / §3.3；原⑤「收 warn 级死代码族面」
已撤回，其**判据、侧车基线与 selftest 用例一律不动**）。
**⓪ 带来的唯一例外（须显式记账，不是顺手改）**: 该脚本 `SCAN_ROOTS` 含 `kani-harness` 且
`scan()` 对缺失根 `raise ScanError`（fail-closed）⇒ 删树后该腿必红，故本批必须把
`SCAN_ROOTS` 五树改四树 + 三处「五树」自述（`:52` 注释 / `:191` docstring / `:406` 打印）同步，
并同批把 [ci_entrypoint_unified_test.rs:313](../../host-tests/tests/ci_entrypoint_unified_test.rs#L313)
的逐字针改钉到四树（**改钉不删针**）。侧车 `totals={sites:0,files:0}` / `entries=[]` / `_meta` 不含根清单
⇒ 收缩扫描根**不改任何读数**，无需重录基线（已核）。
④ 之后「privileged unsafe 100% 覆盖」这一读数**不再成立也不再宣称**——块面改由棘轮腿报
「399 处待补写」，这是 D-5 乙裁定的本意: 宁可显式欠账，不要虚高读数。

**施工实测（2026-11-19，全部为真跑读数）**

⓪ kani-harness 整树退役已落：`git rm` 4 文件（`Cargo.toml` / `Cargo.lock` / `src/lib.rs` / `src/wasm/mod.rs`）、
`Makefile` 的 `test-kani` 目标与 `.PHONY` 摘除、三个审计脚本扫描根收缩（`audit_c_naming` /
`audit_comment_language` 的 `BASES`、`audit_dead_code_comment` 的 `SCAN_ROOTS` 五树→四树 + 三处自述）、
`audit_safety_debt` 三处口径串 + 侧车重导（**totals 逐字不变: 399 处 / 78 文件 / 83 键**，plain run RC=0
⇒ 退役未损失任何 SAFETY 读数）、内核源码 3 处注释、`docs/design` 2 处、host-tests 契约锁改钉。
必同步面（`docs/explain/` / `docs/design/` / `docs/README.md` / `README*.md` / `AGENTS.md`）
kani 与三个退役脚本名**残留 0 处**；余下命中全在 host-tests 散文与扫描根注释的历史面。

① 两个分类器迁入 `audit_implicit_deps.py` 后输出**逐字节相同**（生产 38 处 / 14 项 / 注释 207 / 测试 61）。
② / ③ 整腿退役落地，quick 清单 17 条、full 清单 10 条（`safety_debt` 占第 5 槽）。
④ 收敛后实测：`audit_safety_coverage.py` **56 模块 / 322 文件 / 146 处 / 缺 0 / RC=0**；
`tools/audit_unsafe.py --summary` **146 = extern 127 + 引用 19**（19 处全是 `unsafe fn` 函数指针类型位置），
`DELEGATED_KINDS = {block, fn, impl, trait}`，`_find_enclosing_fn` 与 60 行窗口、auto-trait 假豁免均已删。
⑤ `is_unsafe_in_functions`（73 行）与 CRITICAL 级退役，**`check_raw_pointer_access` 保留**（F2 面不动）。
⑥ 驼峰正则与检测块删除，`BASES` 三根。⑧ 两处 `clippy.toml:5` 同形空话自述均已订正。
⑨ AGENTS **八处一次改完**（授权用掉，此后不得再改）：§1 布局表删 kani 行、§2.2 三行改写 + 一行新增
（`audit_safety_debt.py`）+ 两行删除（invariants / once_cell）、附加门禁注与完整清单注（**26 个 / `scripts/` 23**）、
F8 射程注（三树→两树）、新增「F4 执法分工」与「退役与收敛」两注、§3 四份 clippy 配置口径、
§4.2 新增「执法点口径」注（I6 真实执法 = `dma_buf.rs` 的 `DmaStream` + `dma/engine.rs` 的 `alloc_coherent` + F2 黑名单）、
§5 F1 改 `forbid` / F4 改四面分工；**§5 F9 逐字未动**。
同批连带：`docs/explain/spec-engineering.md` 规则 4.1 / 4.2、`.github/workflows/ci-lint.yml` 删 Job 6（once-cell）
与 Job 9（audit-invariants + artifact）共 −65 行、`docs/design/freg-design.md` 的 OnceCell 行改指四树 clippy、
`net/syscall.rs:580` 注释改指编译期不可解析、`audit_orphaned_init.py:68` docstring 订正。
**P-69 约束实测生效**：退役脚本名一旦以反引号出现在 AGENTS §2.2 即被反向对账判红（清单里已无该腿），
故「退役与收敛」注以**描述式**写三条退役腿而不点名脚本；`tools/check_doc_paths.py` 改后 RC=0。
2e 十腿连跑 **43.3s**（热跑 / 起始 load avg 0.66），其中 `safety_debt` **1.4s**、`missing_docs_debt` 11.8s
——十腿里**两条真编译**；棘轮腿七个隔离 target 目录合计 **510M**。

⑩ **六条替代执法负控制**（注入 → 读数 → `cp` + `sha256` 复原，三处复原全 `成功`、复原后 coverage RC=0 / debt RC=0）：

| 控制 | 注入 | 新执法点读数 | 结论 |
|---|---|---|---|
| NC-1 | `dma_buf.rs` 追加无 SAFETY 的 `pub unsafe extern "C" fn` | 首轮 coverage **RC=0（假绿）**、`audit_unsafe --summary` 缺 SAFETY **1**；归因修复后 coverage **RC=1** 点名 `dma_buf.rs:286 (extern)` | 见下「NC-1 归因」，**发现并修掉一个真实盲区** |
| NC-2 | 同文件追加无 SAFETY 的 `unsafe` 块 | `audit_safety_debt` **RC=1**：`✗ 新增 src/kernel/privileged/dma_buf.rs — block: 当前 1 / 债册 0` | 棘轮真拦新增 |
| NC-3 | `functions/mod.rs` 加 `pub fn ncProbeCamel()` | clippy **RC=101**：`error: function ncProbeCamel should have a snake case name` | rustc `non_snake_case` 顶替驼峰面 |
| NC-4 | `functions/mod.rs` 加真 `unsafe` 块 | clippy **RC=101**，诊断指向 `functions/mod.rs:38` 的 `#![forbid(unsafe_code)]` | crate 根 forbid 顶替脚本 unsafe 面 |
| NC-5 | `dma_buf.rs` 加 `use crate::functions;` | `cargo check` **RC=101**：`error[E0432]: unresolved import crate::functions` / `no functions in the root` | 反向依赖由编译期保证；**实测码是 E0432 不是 E0433**（E0433 是 kani 那条 `crate::privileged::syscall::Errno` 路径形态） |
| NC-6 | `src/kernel/clippy.toml` 的 `disallowed-types` 登记真实类型 | 三轮见下「NC-6 归因」；外部路径轮 **RC=101 / 78 处** | 配置 live，退役无净损失 |

**NC-1 归因（本批唯一的功能性修复）**：`audit_safety_coverage.py` 的 `collect_rs_files` 只归集
`base/mod.rs` + 每个声明模块的**目录**（`<mod>/mod.rs` 与 `<mod>/**/*.rs`），
**顶层单文件模块**（`pub mod dma_buf;` → `dma_buf.rs`，无同名目录）从来不在射程内 ⇒
实测漏 **25 个文件**（`cpu_local` / `dma_buf` / `dtb` / `iomem` / `ioport` / `userptr` / `page_table` …），
本腿射程是 `tools/audit_unsafe.py`（`FW_DIR.rglob("*.rs")` 全树）的**真子集**。
属 B01-13 那次「8 文件→动态发现」修复的遗留缺口，**当前潜伏**：25 个文件里 extern/引用面命中合计 **0**，
故 146/146 读数不受影响、也没有真实缺口被藏住。处置＝`collect_rs_files` 补顶层单文件分支
（**297 → 322 文件，146/146/缺 0 读数不变**），两腿自此同面。
门禁侧本就双消费者：0.5 组 `unsafe` 腿取 `audit_unsafe --summary` 的「缺 SAFETY」数与
`EXPECTED_MAX_SAFETY_MISSING=0` 比较（该工具自身 RC 恒 0 是**报告工具**语义，判红在消费侧），
`safety_coverage` 腿取脚本 RC ⇒ 修复前 NC-1 已由前者拦下，修复后两者都拦。
**回归锁**：`audit_selftest.py` 第 4 用例增两项断言（25 个单文件模块全在射程 / 归集面 == rglob 全树），
**用例数仍 13**，`[Test N/13]` 标签与 `vacuous_needle_gate_test.rs:229` 的针不动；
锁自身的负控制＝临时撤掉该分支 ⇒ **12/13、RC=1**，两项断言点名漏扫文件，复原后 13/13、RC=0。

**NC-6 归因（探针形态错，不是执法缺口）**：三轮均**真编译**（`Checking kernel-privileged`，8.2s，非 Fresh）⇒
「cargo 不把 `clippy.toml` 纳入指纹所以不重放」的假设**被证伪**。真因是路径形态：
① `crate::privileged::sync::once_lock::OnceLock`（re-export 别名，`privileged/mod.rs:76` 有 `pub use crate as privileged;`）
⇒ RC=0 / 0 命中；② 规范定义路径 `crate::sync::once_lock::OnceLock` ⇒ 仍 RC=0 / 0 命中
⇒ **clippy `disallowed-types` 不解析本 crate 局部路径**（生态限制，须留档）；
③ 复现批 2 P4 的外部路径 `core::cell::UnsafeCell` ⇒ **RC=101 / 78 处 `use of a disallowed type`**，
与批 2 读数一致 ⇒ 内核配置在批 4 改动后仍 live。
**退役无净损失的判据**：`git show HEAD:scripts/audit_once_cell.py` 实读，该腿**全部**执法面就是
禁 `spin::Once` / `spin::OnceCell`（use 形态 + 行内代码形态，注释跳过），**不含**「必须用本项目类型」的正向要求
⇒ 与 `disallowed-types = ["spin::Once", "spin::OnceCell"]` 的表达面重合，且 clippy 版更强
（定义点与 use-only 导入都命中，批 2 cp 探针已证）；局部路径限制与本退役无关（禁单全是外部 crate 路径）。
复原后再真编一次 **RC=0 / 0 命中**，避免 Fresh 单元重放本轮错误诊断（L5）造成门禁假红。

**七门槛读数（串行三轮，判据 = 日志内 `*-RC=` 回显；`/tmp/gates_b4.out` → `/tmp/gates_b4_r2.out` → 收口轮 `/tmp/gates_b6.out`）**：
首轮 G2 红一处，**病因是本批自己写的 doc 注释**——`ci_entrypoint_unified_test.rs:2476` 的
`OnceCell` 未加反引号 ⇒ `-D clippy::pedantic` 下 `clippy::doc_markdown` 硬错、host-tests clippy 腿 exit=101；
补反引号后第二轮全绿：`G1-RC=0`（Passed 6 / Failed 0，双架构链接均过）、`G2-RC=0`
（`bash ci/audit.sh full`：**P-69 反向对账通过（文档点名腿无摘除）**、`audit_selftest` **13/13**、
`safety_debt` 腿通过（债册 **399 处 / 83 键**）、`missing_docs_debt` **0 处 / 0 键**、
QEMU **2/2**（FAIL_OK=0）、lockdep 构建面**双架构正向/默认两侧符合**、UEFI(OVMF) **可达面判据全符合**
（`[THREAD]` 显式不判＝已登记预存缺陷）、aarch64 EGFS **三相判据全符合**、**本轮因环境缺失未执行腿数 0**）、
`G3-RC=0`（`make test-host` **203 套 / 1599 passed / 0 failed**）、
`G4-RC=0`（`make test-kernel-host` 三成员 **12 + 807 + 556 = 1375 / 0 failed**）、
`G5-RC=0`（`ci-rustdoc` x86_64 与 aarch64 **两腿都真跑完**，0 源码级 link 错误 + 0 私有项链接）。
**收口轮**（本册状态翻 `[X]` + 台账行回填之后，即提交前终态）复跑五腿 `*-RC=` **全 0**，逐项读数与第二轮**相同**
（P-69 通过 / selftest 13/13 / 债册 399 处・83 键 / missing_docs_debt 0 处・0 键 / QEMU 2/2 / lockdep 双架构两侧 /
UEFI 可达面全符合 / EGFS 三相 / 未执行腿数 0 / host-tests 203 套・1599・0 / 内核 host 1375・0 / rustdoc 双架构 0）；
两条便宜文档腿另在终态单跑真 RC：`scan_obsolete_naming` **1720 文件 / 违规 0**（历史面 KEEP 316）、
`check_doc_paths` **19 份 / 0 漂移**（设计册计划新建路径免报 3 条）。

**工程外预存问题（§11.5 只报告不处置，待用户裁定）**：
① `src/kernel/clippy.toml:22-23` 缺其 `src/rust` 孪生件所有的 P-59「惰性配置」警示；
② `docs/explain/spec-engineering.md:55` 规则 8.3 把「非 test 禁 unwrap」归给 `audit_safety_coverage.py`（**假归属**），
且 `unwrap_used` 在 `src/kernel` 任何配置里都未启用 ⇒ 该规则**无执法点**；
③ `freg-design.md` 声称的 `indexing_slicing` / panic deny 组在 `src/kernel` 不存在；
④ AGENTS §2.3「`src/user` 七成员」已漂移到 **10**（实测 lib / init / eash / install / fbterm / httpsrv /
proctest / exectest / wasmstub / wasmrun）——**AGENTS 授权已用掉，本批不改**；
⑤ `tools/check_tcb.sh`（步骤 0 的 F1 预检）在 crate 根 forbid 落地后亦属重复面，在本工程射程外，仅登记。

### 批 5a・SAFETY 清零: userlib（112 处）

描述：清 `src/user/lib` 的 112 处（其中 `lib/src/sys.rs` 独占 110），形态高度重复（syscall 包装）。
方案：① 逐处补**紧邻上一行**的中文 SAFETY 注释（项目约定，**严于** clippy 判据——后者允许空行间隔，见批 0 归因订正）；② 重复形态的理由**单源化**（模块级说明 + 每处指向
具体不变量，不复制粘贴同一句空话）；③ 跑棘轮腿确认读数下降 → `--update-baseline` 收缩侧车；④ 七门槛。
状态：[X]
详情：**不得写占位理由**（如现存的「调用方保证指针/类型有效 (详见上下文)」）——每处须给出
指针来源 / 对齐 / 生命周期 / 并发前提的具体依据。这正是 D-5 取乙而非甲的理由: 赶工 399 处
只会把假读数再灌一遍。本批清完 src/user 树仍余 26 处（批 5b），**不翻硬执法**。
**施工结果（2026-11-19）**：112 处全清（`lib/src/sys.rs` 110 + `lib/src/heap.rs` 1 +
`lib/src/str.rs` 1），零占位——每处先读内核对应 syscall 实装再落笔，按**实际入口**写
（`copy_from_user` / `validate_user_buf` + `write_struct_to_user` / `check_user_ptr` + 只读 /
`UserWritePtr::checked_new` / `as_kstr` 4096 字节上限扫描），不写笼统「内核会校验」。
**单源化**落在 `sys.rs` 模块级新增「unsafe 依据单源」段：门 **G**（寄存器与 clobber 契约）
+ 包装层五类 **S** 纯标量 / **B** 借用缓冲 / **P** 路径串 / **T** 结构体 / **R** 裸指针；
逐处注释只引用类别 + 写该处特有的不变量（如 `proc_list` 的写入上界是
`max_entries × 内核定长条目` 而**非** `buf.len()`、`recvfrom` 的 UDS v1 分支显式不回填源地址、
`fb_open` 内核只校验**起始地址**后即写整个结构、`str.rs` 的全局暂存区不可重入且返回切片
只在下次调用前有效）。**连带订正 2 行**（属本次单源化的组成部分，非顺手改）：模块头寄存器
契约原写 `aarch64 x1-x4=args` 且 x86_64 缺 `r9=a6`，与 `sys6` / `svc_handler` 实装不符 ⇒
改为 `x1-x6` 并补 `r9=a6`；门 G 的 aarch64 依据按 `arch/aarch64/exception.rs:597` 的
`svc_handler`（取帧 `x0` 编号 / `x1..x6` 参数 / 结果写回 `x0`，陷入帧全量保存弹回 `x0..x30`）
与 `proc/proc_ops.rs:940`、`arch/aarch64/context.rs:229` 的「用户库 asm 只声明 x0 为输出」
注释核实后落笔（原稿把该注释误挂在 exception.rs，已订正）。
**棘轮读数**：399 → **287 处 / 75 文件 / 80 键**（userlib 三键 `sys.rs` 110 / `heap.rs` 1 /
`str.rs` 1 全归零），`--update-baseline` 收缩侧车后复跑双向对账 **RC=0**；src/user 树仍余
**26 处**（x86_64 腿 23 / aarch64 腿 26，并集 26）⇒ 按方案**不翻硬执法**，归批 5b。
**userlib 内联单测**（标准门槛不覆盖，须 `cd src/user/lib && cargo test`）：**10 passed / 0 failed**。
`cargo fmt --manifest-path src/user/Cargo.toml --all -- --check` **RC=0**；src/user 双裸机三元组
`--release --workspace --no-deps -- -D warnings -D clippy::pedantic` 两腿 **RC=0**。
**两项工程外发现只报告不处置（§11.5，待用户裁定）**：⑥ `auth_login` 的 note/password 在
userlib↔内核 ABI 两侧**参数序颠倒**——userlib `sys2(SYS_SGEG_LOGIN, n.ptr, p.ptr)` → 内核
`auth_login_syscall(password_ptr=a0, note_ptr=a1)` → `pwm_login(note=a1, password=a0)` →
`session::login` → `find_by_note(note)`；唯一调用方 `src/user/eash/src/commands/identity.rs:22`
按 (note, password) 传 ⇒ 内核拿**密码串**查用户、拿 **note 串**验密码，`login` 命令应恒失败。
本批 SAFETY 注释刻意不写「哪个参数是 note」，以免把错误映射固化进单源说明。
⑦ `src/user/clippy.toml` 的 `disallowed-types` 在不依赖 spin 的 `wasmrun` 腿报 2 条**配置级**
warning（`spin::Once` / `spin::OnceCell` does not refer to a reachable type），两腿 RC 仍 **0**、
不影响执法（该 crate 本就无 spin 使用面），属批 2 四树配置化的已知副作用。
⑧ aarch64 syscall 参数寄存器**注释漂移**（内核侧，非本批改动面）：`arch/aarch64/exception.rs:594`
的 `svc_handler` doc 写 `x1-x4=args`、`:314` 汇编注释写 `x1-x5`，而实装 `:627-632` 读
`frame.x1..x6` 六个参数（结果写回 `frame.x0`）——本批单源说明按**实装**落笔，未沿用该注释。
**自审订正 1 处（本批自己写错的断言）**：`proc_exit` 的 aarch64 兜底原写「EL0 执行 wfi 合法」，
经核 `boot/aarch64/start.S:123`（`msr sctlr_el1, xzr`）+ `arch/aarch64/mmu.rs:272-281`（只置
M/C/I 三位）⇒ **NTWI=0 ⇒ EL0 WFI 陷入 EL1**，且 EL0 同步向量只特判 `EC=0x15`(SVC)、其余一律进
`sync_exception_handler`（`exception.rs:302-310`）⇒ 改为与 `exit_group` 同形的条件式表述。
该订正在门槛 2 跑完后落笔（避免改动门禁正在读取的面），随后复跑受影响腿。
**七门槛（终态）**：`./ci/build.sh all` **RC=0**（Passed **6** / Failed **0**）·
`bash ci/audit.sh full` **RC=0**（**未执行腿数 0**；P-69 通过 / selftest **13/13** /
`safety_debt` 通过・债册 **287 处 / 80 键** / `missing_docs_debt` **0 处 / 0 键** / QEMU **2/2** /
lockdep 构建面双架构两侧符合 / UEFI(OVMF) 可达面全符合且 `[THREAD]` 显式不判 /
aarch64 COW 四判据 + EGFS 三相全符合）· `make test-host` **RC=0**（**203 套 / 1599 passed /
0 failed**）· `make test-kernel-host` **RC=0**（三成员 **12+807+556=1375 / 0 failed**）·
`make -f Makefile.ci ci-rustdoc` **RC=0**（双架构两腿真跑，**0 源码级 link 错误 + 0 私有项链接**）。
wfi 订正后复跑：src/user 双裸机三元组 clippy 两腿 **RC=0**、
`cargo fmt --manifest-path src/user/Cargo.toml -- --check` **RC=0**、棘轮腿 **RC=0**（287/80 不变）、
userlib 内联单测 **10 passed / 0 failed**。
**终态复跑**（wfi 订正 + 本册/台账回填后）：`./ci/build.sh all` **RC=0**（Passed **6** / Failed **0**）、
`scan_obsolete_naming` **RC=0**（受检 **1720 文件** / 历史面 KEEP **316** / 违规 **0**）、
`check_doc_paths` **RC=0**（**19 份** / **0 漂移**，设计册计划新建路径免报 3 条）。

### 批 5b・SAFETY 清零: 其余用户 crate（26 处）

描述：清 proctest 7 / install 7 / exectest 6 / eash 3 / init 3。
方案：同批 5a 四步；清完后 **src/user 树小计归零** ⇒ 该树 CI clippy 腿加
`-D clippy::undocumented_unsafe_blocks` 翻**硬执法**（不再靠侧车），侧车该树条目清空。
状态：[X]
详情：翻硬执法后该树任何新增无 SAFETY 块在**门槛 2** 即红，无需等棘轮腿。
**施工结果（2026-11-19）**：26 处全清、**零占位**——proctest 7 / install 7 / exectest 6 / init 3
逐处补紧邻上一行的中文 SAFETY；eash 3 处中 **2 处补注释 + 1 处 unsafe 直接消除**（见下缺陷修复）。
方法照批 5a 三步：先读内核对应实装再落笔、重复形态单源化、落笔后 grep 核实全部引用名与具体断言。
**单源化**落在三个 crate 的模块级「unsafe 依据单源」段（proctest 计数器探针 / install 设备探测
状态 / exectest 入口栈槽位），逐处只引类别 + 写该处特有不变量；init 3 处形态各异故逐处直书。
**核实过的关键断言**（不是「内核会校验」式笼统话）：① exectest 的入口栈槽序按
`privileged/proc/user_proc.rs:1697` 的 `setup_user_stack_in_table`（argc + argv 指针 + argv NULL +
envp 指针 + envp NULL + 串区），argc=3 时槽 1/2/3 是 argv、4 是 argv NULL、5/6 是 envp、7 是 envp NULL；
栈顶指针按 `:1027` 的 `initial_rsp = stack_virt + USER_STACK_GUARD + USER_STACK_SIZE - 8` 与
`stack_virt = aslr_stack_top - USER_STACK_SIZE - USER_STACK_GUARD` 推得 **`initial_rsp ==
aslr_stack_top() - 8`**，而 `config/memory.rs:126` 的 `aslr_stack_top()` 是 `USER_STACK_TOP` 减
**页对齐**随机偏移 ⇒ sp 恒 8 字节对齐。② install 的 `disk_list` 写入上界是
`min(max_count, block_device_count())`（`functions/sgeg/storage/disk.rs:64`）而**非**用户缓冲容量；
`disk_info` 经 `api::write_struct_to_user` → `check_user_buf(ptr, size_of::<UserDiskInfo>())`
做**全尺寸**复校（故下面 ⑨ 的 76 字节常量失真无安全后果）。
**eash 预存缺陷修复（用户裁定「本批内修复」，非顺手改）**：`Segment` 原持
`argv: [*const u8; 16]`，在 `parse_single_segment` 的**函数局部**结构里取
`&raw const seg.cmd_buf[pos]`，再把该结构**按值返回**、由调用方移进 `segments` 数组槽 ⇒ 那些指针
仍指向已死的 parse 帧，`run_pipeline_child` 的 `CStr::from_ptr` 读悬垂指针；是否「看起来能跑」
取决于 LLVM 是否恰好省掉返回拷贝，不是可依赖的契约。改为存 **`argv_off: [usize; 16]` 偏移**、
指针留到使用点再取（此时 `seg` 已是数组槽内最终位置），与同 crate `commands/mod.rs` 的
`run_external` 既有写法同构。**连带消除第二处缺陷**：argc 上限与数组容量同为 16，收尾写
`argv[argc]` 哨兵在满 16 个参数时越界 panic ⇒ 哨兵改由使用点的 **17 槽**指针数组承担。
修复后 `run_pipeline_child` 的 argv 与路径构造**全程 safe**（`CStr::from_bytes_until_nul` +
`as_ptr`，路径切片连同 NUL 交出以满足 `proc_exec` 的 NUL 结尾契约），该处 unsafe 块整个消失。
**新增静态形状锁** `host-tests/tests/eash_pipeline_argv_offset_test.rs`（3 用例：Segment 存偏移
不存自指指针 / parse 不写 16 槽哨兵 / 指针与路径串在使用点取且函数内无 unsafe）+ `host-tests/README.md`
索引行。取形状锁而非行为测试的理由：该解析器活在 no_std 用户态 bin crate、依赖 fork/exec/pipe，
host 侧无法直接编译执行；既有 `eash_cmd_parser_test.rs` 锁的是 `Cmd` 模型，覆盖不到 `Segment`
的指针来源。
**翻硬执法**：`ci/audit.sh` 的 src/user 双裸机三元组腿加
`-D clippy::undocumented_unsafe_blocks`（+3 行中文理由）。**lint 名显式点名是必需的**——
该 lint 默认 allow，单靠 `-D warnings` 不会把它升成错误（批 0 实测 L2 的反面）。接线形状锁
`ci_entrypoint_unified_test.rs` 的 `assert_user_leg_is_dual_arch` 旗标清单同批加该项。
**两项负控制（均 `cp` + `sha256` 逐字节复原）**：① 往 src/user 真文件注入一个无 SAFETY 的
unsafe 块 ⇒ 门槛 2 该树腿 **RC=101**、判词 `error: unsafe block missing a safety comment`
（证硬执法真在位，不是只改了旗标文本）；② 从 `ci/audit.sh` 摘掉新旗标 ⇒ 形状锁 **RC=101**、
判词「`src/user` pedantic 腿缺 `-D clippy::undocumented_unsafe_blocks`」（证接线有锁，
静默退回棘轮形态会被拦）。
**自捕订正 2 处（本批自己造成的红，均已修）**：① 缺陷修复引入的
`path_off..path_off + path_slice.len() + 1` 触发 `clippy::range_plus_one`（`-D clippy::pedantic` 下硬错）
⇒ 改 `..=` 闭区间（F9 禁新增 `allow`），形状锁的针**同批重指**；② 新测试文件缺文件末换行 ⇒
门槛 2 **步骤 2c** fmt 漂移硬红（首轮 `G2-AUDIT-FULL-RC=1` 的**唯一**红因；2c 在 quick 早退之前，
故该轮 2d/2e/2f/2g/3/4 **全未执行**，不存在 QEMU 与棘轮读数），
`cargo fmt --manifest-path host-tests/Cargo.toml` 修复后该腿 RC=0、形状锁复跑 **3/3**。
**棘轮读数**：287 → **261 处 / 70 文件 / 75 键**（−26；块 244 + impl 17），侧车 `src/user/` 条目
**0**，余 host-tests 25 + kernel 236；`--update-baseline` 收缩后复跑双向对账 **RC=0**。
**七门槛（终态）**：`./ci/build.sh all` **RC=0**（Passed **6** / Failed **0**）·
`bash ci/audit.sh full` **RC=0**（**未执行腿数 0**；P-69 通过 / selftest **13/13** /
`safety_debt` 通过・债册 **261 处 / 75 键** / `missing_docs_debt` **0 处 / 0 键** / QEMU **2/2** /
P-62 feature 运行维 **4/4** / lockdep 构建面双架构两侧符合 / UEFI(OVMF) 可达面全符合且
`[THREAD]` 显式不判 / aarch64 COW 四判据 + EGFS 三相全符合 / 线程压力 3 轮 × 2 架构 **6/6**）·
`make test-host` **RC=0**（**204 套 / 1602 passed / 0 failed**，较批 5a **+1 套 +3 用例**＝本批
新增形状锁）· `make test-kernel-host` **RC=0**（三成员 **12+807+556=1375 / 0 failed**）·
`make -f Makefile.ci ci-rustdoc` **RC=0**（双架构两腿真跑，**0 源码级 link 错误 + 0 私有项链接**）。
**终态复跑**（本册与台账回填后，避免文档改动引入新漂移）：`./ci/build.sh all` **RC=0**
（Passed **6** / Failed **0**）· `scan_obsolete_naming` **RC=0**（受检 **1721 文件** /
历史面 KEEP **316** / 违规 **0**；较批 5a **+1 文件**＝新增形状锁，已先 `git add` 入索引）·
`check_doc_paths` **RC=0**（**19 份** / **0 漂移**，设计册计划新建路径免报 **3** 条）。
**新增一项工程外发现（§11.5 只报告不处置，与批 4 五项 + 批 5a 三项合并待批 6 裁定）**：
⑨ `functions/sgeg/storage/disk.rs:44` 的 `USER_DISK_INFO_SIZE = 76` 与 `:28` / `:73` 的
「76 字节」注释**失真**——`UserDiskInfo` 实为 4 × `u32` + `[u8; 64]` = **80 字节**；
**无安全后果**（`write_struct_to_user` 按 `size_of::<T>()` 全尺寸复校，76 只是提前放行的下界），
属常量与注释漂移，本批 install 的 SAFETY 按**实际复校入口**落笔、未沿用该常量。

### 批 5c・SAFETY 清零: host-tests（25 处）

描述：清 `tests/` 19 + `src/` 6。
方案：同批 5a 四步；清完后 host-tests 腿翻 `-D clippy::undocumented_unsafe_blocks` 硬执法。
状态：[X]
详情：测试代码的 SAFETY 理由须说明**被探针对象的不变量**，不是「测试用所以安全」。
**施工结果（2026-11-19）**：25 处全清、**零占位**——`tests/` 19 = elf_verify_unification 9 +
lib_string_strlen_safe 7 + mm_iomem_alias 1 + copy_user_exception 2；`src/` 6 =
framekernel_bench 5 + dma_stream 1。方法照批 5a/5b 三步：先读内核对应实装再落笔、重复形态
单源化、落笔后 grep 核实全部引用名与具体断言。**单源化**落在四段「unsafe 依据单源」
（elf 的 `verify_elf` 三条路径 / strlen 的实际读取下标区间 / `SignalFrame` → 字节切片的
`from_raw_parts` 四条前置 / `IoMem::new` 的四步实装），逐处只引类别 + 写该处特有不变量。
**本批最重要的发现是形态订正而非补注释**：elf 9 处中 **7 处原本就有 SAFETY 注释**，但写在
`assert_eq!(` **之前** ⇒ clippy **不认**（注释与 `unsafe {` 之间隔了非注释 token）。实测把注释
**移进宏实参、紧贴 `unsafe {` 上一行**即被接受；`let x = unsafe {` 语句行的上一行同样被接受。
故这 7 处是**位置错**、另 2 处（helper `call_verify_elf` 与 PhdrOutOfBounds 的 `assert_eq!`）
才是真缺失。⇒ 项目「紧邻上一行」约定在**宏调用形态**下必须落到实参内部，写在宏名之前无效。
**核实过的关键断言**（不是「内核会校验」式笼统话）：① `proc/elf/verify.rs` 的 `verify_elf`
先判 `is_null() || elf_size < 64` **早返回 `TooSmall` 且该路径不解引用**，唯一读点是
`cast::<Elf64Header>().read_unaligned()`（**不要求对齐**、范围恰 header 64 字节），phdr 表只做
`checked_mul` / `checked_add` 的**算术**边界校验、**不读表内容** ⇒ PhdrOutOfBounds 用例缓冲
100 字节而 `phdr_end = 176` **不构成越界读**（缺的 76 字节不被访问），TooManyPhdr 缓冲为
`64 + 129 × 56 = 7288` 字节。② `privileged/iomem.rs` 的 `IoMem::new` 四步（零长 / 4 字节对齐 /
`checked_add` 溢出 → `ALIAS_REGISTRY` 区间注册 → `mmio_virt` 地址算术 → `NonNull::new` 包装）
**全程不解引用 phys**，本用例 6 个调用点只断言 Ok/Err、从不 read/write，句柄 Drop 即 unregister；
`0x8000_1800` 恰为 a 区间末端 ⇒ **半开区间相接不算重叠**。③ `SignalFrame` 23 个字段全为 `u64`
⇒ 无 padding、`size_of` = **184** 即对象实占字节数（由既有布局用例钉住），目标元素 `u8` 对齐要求 1、
结构无 `UnsafeCell` / `Cell`、切片不逃逸出函数体。framekernel_bench 的 5 处补齐之外，**1 处既有
笼统理由**（「phys 为纯算术载体」）同批重写为上述可核对的四步细节。
**翻硬执法**：`ci/audit.sh` 的 host-tests 腿（`--release --all-targets --no-deps --target-dir
other/target`）加 `-D clippy::undocumented_unsafe_blocks`（+3 行中文理由）。**lint 名显式点名是
必需的**——该 lint 默认 allow，单靠 `-D warnings` 升不动它。接线形状锁
`ci_entrypoint_unified_test.rs` 的 `test_audit_sh_clippy_pedantic_covers_four_trees` **臂①**
旗标清单同批加该项（断言点 `:1240`；臂② 的 src/user 双架构腿已在批 5b 加过，不重复翻）。
**两项负控制（均 `cp` + `sha256` 逐字节复原）**：① 往 `copy_user_exception_test.rs:494` 注入一个
无 SAFETY 的 unsafe 块 ⇒ 门槛 2 该树腿 **RC=101**、判词 `error: unsafe block missing a safety
comment`；② 从 `ci/audit.sh` 摘掉新旗标 ⇒ 形状锁 **RC=101**、判词「`host-tests` pedantic 腿缺
`-D clippy::undocumented_unsafe_blocks`」（证接线有锁，静默退回棘轮形态会被拦）。
**自捕订正 1 处（本批自己造成的红，已修）**：首轮 `G2-AUDIT-FULL-RC=1` 的**唯一**红因是我新写的
elf 模块 doc 里裸写 `elf_size` 触发 `clippy::doc_markdown`（`-D clippy::pedantic` 隐含该 lint，
且它**只作用于 doc 注释**、不管 `// SAFETY:` 行注释）⇒ 加反引号后先以**门槛原形命令**复验
该树腿 RC=0，再重跑 `audit full` 得第二轮 RC=0。教训：手工检测跑只带
`-W clippy::undocumented_unsafe_blocks`、**不带 pedantic**，故新增 doc 散文必须按门槛原形验一次。
**棘轮读数**：261 → **236 处 / 64 文件 / 69 键**（−25；块 219 + impl 17），侧车 `host-tests/`
条目 **0**，余量全在 kernel 树（x86_64 腿 172 / aarch64 腿 206，并集 236）；
`--update-baseline` 收缩后复跑双向对账 **RC=0**。
**七门槛（终态）**：`./ci/build.sh all` **RC=0**（Passed **6** / Failed **0**）·
`bash ci/audit.sh full` **RC=0**（第二轮；**未执行腿数 0**、档位 full 覆盖步骤 0–4；
selftest **13/13** / `safety_debt` 通过・并集 **236 处 / 64 文件 / 69 键** vs 债册 **236 / 69** /
`missing_docs_debt` **0 处 / 0 键** / 废弃命名扫描受检 **1721 文件**・历史面 KEEP **316**・违规 **0** /
`vacuous_needle` 受检 **1313 .rs**・违规 **0** / `check_doc_paths` **19 份**・**0 漂移**・免报 **3** /
性能指标腿全表在绝对预算内 / Lockbud **4/4** 零发现 / QEMU **2/2**（FAIL_OK=0）/
P-62 feature 运行维 **4/4** / 线程压力 **6/6** / lockdep 构建面探针双架构正向与默认两侧符合）·
`make test-host` **RC=0**（**204 套 / 1602 passed / 0 failed**，与批 5b 同数——本批只补注释与
旗标清单，未新增用例）· `make test-kernel-host` **RC=0**（三成员 **12+807+556=1375 / 0 failed**）·
`make -f Makefile.ci ci-rustdoc` **RC=0**（双架构两腿真跑，**0 源码级 link 错误 + 0 私有项链接**）。
**终态复跑**（本册与台账回填后）：`./ci/build.sh all` **RC=0**（Passed **6** / Failed **0**）·
`scan_obsolete_naming` **RC=0**（受检 **1721 文件** / 历史面 KEEP **316** / 违规 **0**）·
`check_doc_paths` **RC=0**（**19 份** / **0 漂移**，设计册计划新建路径免报 **3** 条）。
本批**无新建文件**（11 个改动文件均已在索引内），故无需先 `git add` 再跑命名扫描。
**新增一项工程外发现（§11.5 只报告不处置，与批 4 五项 + 批 5a 三项 + 批 5b 一项合并待批 6 裁定）**：
⑩ `privileged/glue/string.rs:61` 的 `while *ptr != 0 && len < MAX_CSTR_LEN` **求值次序颠倒**——
`&&` 左侧先解引用，故 `len` 已达上限时**仍多读一次**才由右侧退出，实际读取下标闭区间是
`[0, min(MAX_CSTR_LEN, 首个 NUL 下标)]`；调用方缓冲**恰为 4096 字节且无 NUL** 时即越界读 1 字节
（下标 4096）。同族 `glue/cstr.rs:106` 的扫描是 `while n < MAX_CSTR_LEN && *ptr.add(n) != 0`
（**先判上限再解引用**）⇒ string.rs 这处是次序颠倒的**孤例**，修法现成。另 `:48-49` 的 `# Safety`
只写「此函数通过 FFI 暴露给 C 代码使用」，**未陈述任何调用方义务**（能过 `missing_safety_doc`
但内容空转）。本批 strlen 测试的 SAFETY 按**实际读取范围**落笔（`at_max` 用例正是该临界形态，
靠 `to_cstr` 追加的 NUL 兜住），**未改实装**。

### 批 5d・SAFETY 清零: privileged mm + sync（54 处）

描述：清 mm 38 + sync 16（含 `mm/cow.rs` 7、`mm/vmm_aarch64.rs` 7、`sync/atomic.rs` 7）。
方案：同批 5a 四步。
状态：[X]
详情：TCB 核心，理由须落到页表所有权 / 内存序 / 锁序不变量（I1–I6 与 `ref-lock-order.md`）。
本批**不翻硬执法**（kernel 树尚未归零）。
**施工结果（2026-11-19）**：54 处棘轮位点全清、本批新写理由**零占位**——mm 38 = cow 7 +
vmm_aarch64 7 + slab 5 + swap 5 + kmalloc 4 + vmm_x86_64 3 + pmm 3 + kpti 2 + copy_user 1 +
pcache 1；sync 16 = atomic 7 + rcu 4 + pi_mutex 2 + irq_spinlock 1 + mutex 1 + rwlock 1。
方法照批 5a/5b/5c 三步：先读内核对应实装再落笔、重复形态单源化、落笔后 grep 核实全部引用名
与具体断言。**4 处属位置订正而非缺失**（kpti 2 + pi_mutex 2）：原注释已存在，但被
`let cur_cr3: u64;` / `let new_cr3 = …;` 语句行、或 `return PiMutexGuard {` /
`Some(PiMutexGuard {` 结构体首行与 `unsafe {` 隔开 ⇒ clippy 不认；pi_mutex 的订正形态与批 5c
elf 同源——注释须移进结构体字面量内部、紧贴 `data:` 字段行。其余 50 处为真缺失。
**单源化**落两段「unsafe 依据单源」：`sync/atomic.rs` 类别 A（i32 槽位重解释为 `AtomicI32`：
core 保证同尺寸 / 同位有效性、对齐恒等于尺寸，调用方契约 ptr 有效 / 对齐 / 持续有效，七函数
一律 SeqCst）；`mm/cow.rs` 类别 P / R / W（新建子页表帧清零 / 按索引读页表项 / 写回页表项，
互斥由 `VMM_LOCK`）。逐处只引类别 + 写该处特有不变量。
**核实过的关键断言**（不是「内核会校验」式笼统话）：① `kmalloc::HeapHeader::from_data_ptr`
只做 `sub(size_of::<Self>())` 的**纯算术逆变换、不解引用**，`HeaderRef::from_data_ptr` 委托它；
两处 `copy_nonoverlapping` 的调用面是 `reallocate`（**不是** `realloc`，落笔前 grep 订正）与
`process_early_allocations`，dst 均为独立新块。② `slab.rs` 的 `raw::zero_memory` 解析到
**slab 自己的** `pub(crate) mod raw`，不是 kmalloc 的同名模块（落笔前 grep `\braw\b|^use |^mod `
核实）；唯一调用面 `KmemCache::new_slab` 在调用前已判 `bitmap_end <= page_end`。③ kswapd 两处
ctx 块**不构成锁序嵌套**——`get_free_pages` / `get_total_pages` 直读 `self.info.get()`、**不取**
PMM 锁；而 `alloc_page` 内部 `acquire_lock()` **取** PMM 锁 ⇒ 后者构成下面 ⑭ 的降序嵌套。
两条都 grep 到实装后才落笔，未写「无锁」式假保证。④ `pmm.rs` 3 处落在 `pub(crate) mod raw`，
三段文本互不相同——`host-tests/tests/safety_boilerplate_test.rs` 的 `MAX_DUPLICATES = 5` 只对
`pmm.rs` 硬执法（全仓清单腿仅报告）。⑤ `copy_user::setup_recovery` 外层块只写「两架构 asm
各自另有注释 + 恢复点簿记走 `PER_CPU_EXCEPTION_CTX` 的 SeqCst 原子槽」，不重复内层的硬件断言。
**棘轮读数**：236 → **182 处 / 48 文件 / 52 键**（−54；块 168 + impl 14）。kernel x86_64 腿
**125** / aarch64 腿 **157**，并集 182；host-tests 与 src/user 两树 `entries` 均 `[]`（批 5b/5c
已翻 `-D`，本批不动）。**本批不翻硬执法**，翻 `-D` 在批 5g。
**七门槛（终态）**：① `./ci/build.sh all` RC=0（Passed 6 / Failed 0）；② `bash ci/audit.sh full`
RC=0（自测 13/13；`safety_debt` 通过、债册 182 处 / 52 键；`missing_docs_debt` 0 处 / 0 键；
废弃命名扫描 受检 1721 文件 / 历史面 KEEP 316 / 违规 0；`vacuous_needle` 受检 1313 `.rs` 通过；
`check_doc_paths` 校验 19 份、设计册免报 3 条、无漂移；性能指标腿全部在绝对预算内；Lockbud
4/4 crate 零发现；QEMU 2/2（FAIL_OK=0）+ P-62 feature 运行维 4/4 + thread-stress 6/6；FP-06
白名单外 FP/SIMD 0；**因环境缺失未执行腿数 0**）；③ `make test-host` RC=0（204 套 / 1602 passed /
0 failed，与批 5c 同读数——纯注释批）；④ `make test-kernel-host` RC=0（三成员 12 + 807 + 556 =
1375 / 0 failed）；⑤ `make -f Makefile.ci ci-rustdoc` RC=0（x86_64 与 aarch64 两腿都真跑、各 exit 0）。
**本批无新建文件**（17 个改动文件均已在索引内），故无需先 git add 再跑命名扫描。
**文档回填后终态复跑**（`/tmp/b5d-terminal.log`，判据 = 日志内 `*-RC=` 回显，非后台通知退出码）：
`T1-BUILD-RC=0`（Passed 6 / Failed 0）、`T2-NAMING-RC=0`（受检 1721 文件 / 历史面 KEEP 316 /
违规 0）、`T3-DOCPATHS-RC=0`（校验 19 份必须与代码同步的文档 / 设计册免报 3 条 / 无漂移）——
本册与台账两处文档改动未引入废弃命名违规或源码路径漂移。
**新增四项工程外发现（§11.5 只报告不处置，与批 4 五项 + 批 5a 三项 + 批 5b 一项 + 批 5c 一项
合并待批 6 裁定）**：⑪ [sync/rwlock.rs:53](../../src/kernel/privileged/sync/rwlock.rs#L53) 的
`unsafe impl<T: ?Sized + Send> Sync for RwLock<T>` **缺 `T: Sync` 约束**——`read()` / `try_read()`
把 `&T` 同时交给多个读者，`T: Send` 不足以让 `&T` 跨线程共享；同仓 vendored spin 用的是
`T: Send + Sync`。唯一泛型消费面 [functions/sync/scoped.rs:72](../../src/kernel/functions/sync/scoped.rs#L72)
未以 `!Sync` 的 `T` 实例化 ⇒ latent。⑫ [mm/slab.rs:1032](../../src/kernel/privileged/mm/slab.rs#L1032)
的 `slab_dump_cache`：`# Safety` 只要求「合法 C 字符串」却用 `from_utf8_unchecked`（非 UTF-8
字节即 UB），且逐字节扫描**无长度上限**（未终止缓冲区会越界读）；FFI 导出、全仓无调用方 ⇒ latent。
⑬ [mm/vmm_x86_64.rs:1814](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1814) 的
`Vmm::ensure_pml4_user` **不取 `VMM_LOCK`** 就对共享 `KERNEL_PML4` 槽位取 `&mut` 做读-改-写，
同索引并发调用即两个 `&mut` 别名；唯一入口是 FFI 导出 `vmm_ensure_pml4_user`、全仓无调用方 ⇒ latent。
⑭ [mm/swap.rs](../../src/kernel/privileged/mm/swap.rs) 的 `swap_in` 在持 `SWAP.lock`（层级 6）
期间调 `pmm::get_pmm().alloc_page()`，后者内部 `acquire_lock()` 取 PMM 锁（层级 1）⇒ **6→1 降序
嵌套**，与 `ref-lock-order.md` 声明的升序相反（`swap_out` 同形）。四项均已在源码内就地登记，
本批不改约束面 / 锁面 / 接线。
**另登记一项检测器盲区（⑮，本批不改判据，待批 6 裁定）**：棘轮与 clippy 只判「`unsafe {` 上方
有无紧邻 SAFETY」，**不判内容** ⇒ mm + sync 面内另有 **39 处存量占位文本**
（`调用方保证指针/类型有效 (详见上下文)` / `调用方保证指针合法` 等）被位置判据接受、**不计入债册**：
mm 34 = vmm_aarch64 15 + copy_user 5 + slab 5 + vmm_x86_64 4 + api 1 + cow 1 + kmalloc 1 +
page_fault 1 + swap 1；sync 5 = mutex 2 + atomic 1 + rcu 1 + seqlock 1。这正是 D-5 乙裁定词里
预言的「占位理由把虚高读数再灌一遍」的**存量形态**：债册归零 ≠ 理由真实。故本批的「零占位」
只对**本批新写的 54 条**成立，不宣称 mm + sync 全占位面已清。处置两条路（待裁定）：甲 加一条
**内容面**审计腿（禁占位短语清单 + 存量棘轮，与 `audit_safety_debt` 同构）并把改写分到批 5e–5g
各自面内；乙 不加腿，只在批 5e–5g 顺手改写本批面内的占位文本，接受「无执法、靠 review」。
**另有三项只登记不处置的观察**：`pcache_copy_to_user` 全仓零引用（`audit_unwired_pub_fn` R1
已机器跟踪，本批不改接线）；`unsafe impl Send for SwapState` 与编译器自动推导同结论（冗余面，
本批只补注释不删）；`vmm_aarch64.rs` 的 `protect_page` 等路径把 `// SAFETY:` 写在**无 unsafe 的
`let l0 = …` 语句**上方（标签错位，该处无 unsafe 可报故不计入棘轮）。

### 批 5e・SAFETY 清零: privileged arch + cpu + idt + boot（73 处）

描述：清 arch 58（含 `aarch64/gic.rs` 23、`aarch64/mod.rs` 10、`aarch64/mmu.rs` 7）+ cpu 7 + idt 4 + boot 4。
方案：同批 5a 四步。
状态：[X]
详情：最大子批。aarch64 独占位点须以 **aarch64 腿**取数验证（74 处只在 arm 维可见，§2.5 A）。
本批**不翻硬执法**（kernel 树尚未归零）。
**施工结果（2026-11-19）**：73 处棘轮位点全清、本批新写理由**零占位**——arch 58 =
aarch64 52（gic 23 + mod 10 + mmu 7 + uart 5 + exception 3 + smp_init 3 + context 1）+
x86_64 6（smp_init 3 + gdt 2 + acpi 1）；cpu 7 = msr 6 + cpuid 1；idt 4 = table 3 + mod 1；
boot 4 = aarch64/entry 2 + boot/mod 2（block 1 + **impl 1**，后者是 `unsafe impl Sync for
MultibootPtr`，本批唯一的 impl 面位点）。16 个改动文件全部纯增注释，`git diff` 零删除行、
零非注释新增行（一处误缩的 uart.rs 分隔线已复原）。
方法照批 5a–5d：先读内核对应实装再落笔、重复形态单源化、落笔后 grep 核实全部引用名与具体
断言。**单源化**落八段「unsafe 依据单源」：`aarch64/gic.rs` 类别 D（GICD MMIO）/ R（GICR
按亲和定位帧）/ S（ICC_* MRS/MSR）/ C（组合）；`aarch64/mod.rs` I（DAIF 立即数映射）/
W（WFI 提示）；`aarch64/mmu.rs` T（静态 AlignedPageTable 所有权）/ R（TTBR/TCR/MAIR/SCTLR
MSR）/ B（TLBI 序列）；`aarch64/uart.rs` U（PL011 MMIO）/ C；`aarch64/exception.rs` V
（VBAR/DAIFCLR）/ X（诊断兜底直写 UART）；`aarch64/smp_init.rs` H（次核启动槽握手）；
`cpu/msr.rs` M（RDMSR/WRMSR）/ F（`cpu_read_msr` 出参写）/ W（宽范围合法性检查）；
`idt/table.rs` P（8259A 端口 I/O）。单位点文件（context / acpi / gdt / x86_64 smp_init /
idt mod / boot 两处 / cpuid）改写逐处内联长注释，不建单源段。
**落笔前被核实推翻的四条草稿断言**（方法③的价值实例）：① 原稿写「packed 布局使 ready/done
4 字节对齐」——实测偏移 38 / 46 ⇒ `0x802E` / `0x8036` 均 2 mod 4，**非对齐**，改写成「x86 在
EFLAGS.AC=0 下允许非对齐访问故无 #AC 面，这也正是必须走裸指针 volatile 而非引用的原因」。
② 原稿写「对 busy 的 TSS 描述符重复 ltr 触发 #GP」——仓内无从证实，改为「装载后描述符即
busy，不具幂等性」。③ 原稿写「QEMU i440fx/virt 的 FADT reset reg 是 I/O 0xCF9 故此分支不会
跑」——`grep 0xCF9` 全仓零命中，断言撤回。④ 原以为 Rust `entry()` 运行时 MMU 尚未启用
（因 entry 第 2 步才调 `mmu::init`）——读 `boot/aarch64/start.S` 的 `el1_entry` 4.3–4.7 证明
**MMU 已启用**（引导页表在 `.bootbss`，MAIR/TCR 与 `mmu.rs::init` 同值，TTBR0=TTBR1=
`_boot_l0`，末步 `ldr x9, =entry; br x9` 跳高半区 VMA），故 `mmu::init()` 是**同值重建**而非
首次启用，注释按此写。另核实 `ProcessContext` 只有 align/offset 三条编译期断言、**无 size
断言**，未写不存在的断言。
**核实过的关键断言**（举其要）：`clear_bss` 的 `[__bss_start, _kernel_end)` 由
`link/aarch64.ld` 定义为 `.bss` + COMMON + 末尾页对齐填充，**低半区 `.bootbss` 不在区间内**
⇒ 正在使用的引导栈 / `_fdt_addr` / `_ap_boot_info` / 三张引导页表存活；x86_64 trampoline
目的址 `0x8000` 恒不会被 PMM 分配出去（`mark_reserved_pages` 预留 `[0, kernel_end]`，内核
LMA 起点 `0x100000` ⇒ page 8 恒在预留面），且必须恰为 `0x8000`（`SINFO_BASE equ 0x8008`
绝对寻址 + SIPI vector `0x08 << 12`）；`start_ap` 的 `sfence` 声明 `options(nomem)` ⇒ **只约束
CPU 侧 store 次序、不是编译器屏障**，跨核可见性靠 x86 TSO + SIPI 前 10 ms 延时；GICR 扫描
以 `REDIST_SCAN_MAX_FRAMES`=1024 与 `GICR_TYPER.Last` 双限界、最坏触到 base+128KiB×1023 ⇒
映射须覆盖全程且 fail-closed 返 `Err`；`enable_sgi` 的 `intid % 32` 只限界移位、**不校验
intid < 16**；DAIF 立即数正确映射是 imm bit3=D / bit2=A / bit1=I / bit0=F（`#8` Debug /
`#4` SError / `#2` IRQ / `#1` FIQ）；`MultibootPtr` 的 Sync 风险面全在「指向数据是否被并发
改写」，而 MB2 信息块经 `multiboot2_info_region()` 进 PMM 早期预留（`"mb2-info"`）⇒ 运行期
无别名写者。
**棘轮读数**：182 → **109 处 / 32 文件 / 35 键**（−73；块 96 + impl 13）。kernel x86_64 腿
**106** / aarch64 腿 **100**，并集 109；host-tests 与 src/user 两树 `entries` 仍 `[]`。17 个
目标键全部归零、**零新增位点**（脚本 `under[:15]` 只显示前 15 行，idt 两键被显示截断而非
未归零，已按侧车逐键核对）。**本批不翻硬执法**，翻 `-D` 在批 5g。
**⑮ 检测器盲区在本批面内的读数（本批不改判据，待批 6 裁定）**：按与批 5d 同一窄短语集
（`调用方保证指针/类型有效 (详见上下文)` / `调用方保证指针合法`）测 arch+cpu+idt+boot 四面，
得 **117 处 / 17 文件**存量占位标签（本面 `调用方保证指针合法` 零命中，117 全是主
boilerplate）；其中 **92 处是块级**——即**在棘轮射程内、却因已有占位标签而被位置判据接受、
不计入债册**（idt/table 13 + x86_64/mod 11 + acpi 10 + aarch64/timer 8 + mmu 7 +
exception 7 + boot/mod 7 + x86_64/smp_init 5 + gdt 4 + cpu/mod 4 + uart 3 + gic 3 + apic 3 +
idt/safety 3 + psci 2 + freg/mod 1 + tss 1），另 25 处为 fn / extern 级（不在块/impl 射程）。
放宽到 8 条短语的宽口径为 **160 处 / 21 文件**。⇒ 本批「零占位」只对**新写的 73 条**成立；
本批面内的占位存量（92 块级）比批 5d 的 mm+sync 面（39）更大，⑮ 的甲案（加内容面审计腿）
射程须按此读数估。按裁定**本批不改写既有占位文本**，fn 级占位行原样保留、新注释写在其下。
**另登记只报告不处置的观察（§11.5，与批 4/5a/5b/5c/5d 各项合并待批 6 裁定）**：
⑯ `arch/x86_64/trampoline.asm` 头部布局框写「50 字节 / `_pad`@+0x2E / 无 `done`」，与 54 字节
`ApStartupInfo` 及 asm 本体自己的 +46/+50 偏移矛盾（注释陈旧，代码正确）。⑰ `acpi_reboot`
的 SystemMemory 分支只取 FADT GAS 的低 32 位地址、不消费 RegisterBitWidth/AccessSize（恒
字节写）、也不校验该地址是否在恒等映射窗内（窗外即缺页而非重启）——已在新 SAFETY 内就地
登记为已知局限。⑱ `IdtPtr` 的 doc 写「+8 Base address」而 `repr(C, packed)` 布局 base 在 +2。
⑲ `idt/types.rs` 陈旧链接 `[isr.asm](../../kernel/isr.asm)`（真路径 `privileged/boot/isr.asm`）。
⑳ `remap_pic` 读旧主/从掩码后以 `let _ = (m, s)` 丢弃（不恢复）。㉑ `gic.rs::init_distributor`
诊断行的位域标签与移位不一致（`(typer>>5)&0x1F` 标 "bits [5:0]"、`((typer>>8)&0x07)+1` 标
"bits [10:8]"），仅日志面。㉒ `aarch64/mod.rs::interrupt_restore` 的 doc 段把立即数位序写反
且 F/D 标签互换，**代码正确**（已按 Linux `daifclr #2`=IRQ 与本仓 `interrupt_enable` 交叉
核实）⇒ 只登记文档缺陷，不改代码。㉓ `gic::verify_post_conditions` 读引导核静态别名，而配置
路径用按亲和定位的帧，多核下二者可能不同核。㉔ `aarch64/smp_init.rs` 注释「80 字节对齐」
措辞 vs 槽位 `.align 3`（8 字节对齐）实况。㉕ `parse_multiboot2` 不校验 `tag_size >= 16`，
tag 3 载荷 +12 的越界防护只到 8 字节标签头（已就地登记为已知局限）。㉖ `unsafe impl Sync
for MultibootPtr` 不是该 static 的类型必需项（`IrqSpinLock<T>: Sync` 只要求 `T: Send`）⇒
冗余面，本批只补注释不删。另 `gic::deactivate` 全仓零调用方（`audit_unwired_pub_fn` R1
报告面已机器跟踪，本批不改接线）。
**七门槛（终态）**：① `./ci/build.sh all` RC=0（Passed 6 / Failed 0）；② `bash ci/audit.sh full`
RC=0（自测 13/13；`safety_debt` 通过、债册 **109 处 / 35 键**；`missing_docs_debt` 0 处 / 0 键；
废弃命名扫描 受检 1721 文件 / 历史面 KEEP 316 / 违规 0；`vacuous_needle` 受检 1313 `.rs`、
违规 0；P-49 存量审计腿 10/10 无静默跳过；`check_doc_paths` 校验 19 份、设计册免报 3 条、
无漂移；性能指标腿条目全部在绝对预算内；Lockbud 实编并分析 4/4 crate 零发现；QEMU 2/2
（FAIL_OK=0）+ P-62 feature 运行维 4/4 + thread-stress 6/6；FP-06 白名单外 FP/SIMD 0；
**因环境缺失未执行腿数 0**）；③ `make test-host` RC=0（204 套 / 1602 passed / 0 failed，
与批 5c/5d 同读数——纯注释批）；④ `make test-kernel-host` RC=0（三成员 12 + 807 + 556 =
1375 / 0 failed）；⑤ `make -f Makefile.ci ci-rustdoc` RC=0（x86_64 与 aarch64 两腿都真跑、
各 exit 0）。**本批无新建文件**（16 个改动文件 + 侧车 + 本册 + 台账均已在索引内），故无需
先 git add 再跑命名扫描。
**文档回填后终态复跑**（`/tmp/b5e_terminal.log`，判据 = 日志内 `*-RC=` 回显，非后台通知退出码）：
`T1-BUILD-RC=0`（Passed **6** / Failed **0**）、`T2-NAMING-RC=0`（受检 **1721 文件** / KEEP **316** /
违规 **0**）、`T3-DOCPATHS-RC=0`（校验 **19 份** / 设计册免报 **3** 条 / **0 漂移**）——本册与台账
两处文档改动未引入废弃命名违规或源码路径漂移。

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

描述：台账与册内状态同步、memory 更新、按路径提交推送；并**合并提交 §11.5 十五项工程外发现
（①–⑮）的处置裁定**——其中 ⑥ `auth_login` 参数序颠倒与 ⑩ `strlen` 上限多读 1 字节是真缺陷，
⑪–⑭ 是 latent 的约束 / 锁序 / 别名缺口，⑮ 是 SAFETY 检测器的**内容面盲区**（甲 加内容面审计腿 /
乙 只逐批改写不加腿）。
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
| **施工前复核新增的连带面**（原清单漏记） | ① [audit_implicit_deps.py:53](../../scripts/audit_implicit_deps.py#L53) 的 `import audit_reverse_deps`（**唯一 importer**，两个分类器已行为不变迁入，见批 4 ①）；② [.github/workflows/ci-lint.yml:211](../../.github/workflows/ci-lint.yml#L211) 调 `audit_once_cell.py`、`:309-347` 的 Job 9 调 `audit_invariants.py` 并上传其日志工件；③ [net/syscall.rs:580](../../src/kernel/functions/net/syscall.rs#L580) 注释点名 `audit_reverse_deps` 作反向依赖执法；④ [freg-design.md:887](../design/freg-design.md) 表行以 `audit_once_cell.py` 的行号论证「四树扫描自动纳入」；⑤ [audit_orphaned_init.py:68](../../scripts/audit_orphaned_init.py#L68) docstring 以 `audit_once_cell.py` 作行注释剥离口径的出处 | ①是**硬断点**（删脚本即打断 full 档阻断腿）；②虽从不触发（远程仅 Gitee）但留着即指向已删脚本的陷阱；③④⑤是失真引用，属 §6 必同步面（design 册在 `check_doc_paths` 射程内） |
| clippy 配置 | [src/kernel/clippy.toml:5](../../src/kernel/clippy.toml#L5) 空话自述订正；**[src/rust/clippy.toml:5](../../src/rust/clippy.toml#L5) 是逐字相同的孪生假自述**（本轮读两份配置时发现，同批一并订正）；批 2 新建的 `host-tests/clippy.toml`、`src/user/clippy.toml` | 自述与实现不符属预存问题，本工程正是使其成真的那一轮 |
| must-sync 文档 | `docs/explain/spec-engineering.md`（F 表指向 AGENTS，不自持副本）、`docs/README.md` 若点名脚本 | §6 要求 explain/根文档与代码同步；`check_doc_paths` 会查路径新鲜度 |
| **kani 整树退役（⓪）** | `kani-harness/` 4 文件（`Cargo.toml` / `Cargo.lock` / `src/lib.rs` / `src/wasm/mod.rs`）；[Makefile:216](../../Makefile#L216) `.PHONY` 里的 `test-kani` + `:651-659` 目标块 | 该 crate 自带 `[workspace]`，不是任何 workspace 成员 ⇒ 删它不动构建图；但目标与 `.PHONY` 留着即指向已删目录 |
| **三个扫描根（逐根 fail-closed）** | [audit_c_naming.py:26-33](../../scripts/audit_c_naming.py#L26) / [audit_comment_language.py:32-40](../../scripts/audit_comment_language.py#L32) 的 `BASES`；[audit_dead_code_comment.py:52-53](../../scripts/audit_dead_code_comment.py#L52) 的 `SCAN_ROOTS` | 三者对缺失根一律 fail-closed（返 1 / 返 2 / `raise ScanError`）⇒ **删树不改根即三腿当场红**，这是 ⓪ 最硬的连带面 |
| **kani 自述与侧车口径串** | [audit_safety_debt.py:39](../../scripts/audit_safety_debt.py#L39) / `:72` / `:228`（侧车 `_meta.口径` 由 `:228` 的脚本常量派生）⇒ 改常量后 `--update-baseline` 重导，**须验 totals 仍 399** | 树退役后「该树 clippy 不可达」的自述失真；侧车是机器派生件，手改即违单源纪律 |
| **源码与设计册散文** | [leb128.rs:255](../../src/kernel/functions/wasm/leb128.rs#L255) / [module.rs:993](../../src/kernel/functions/wasm/module.rs#L993) / [lib.rs:55](../../src/kernel/lib.rs#L55)；[freg-design.md:1298](../design/freg-design.md) / [sgeg-design.md:50](../design/sgeg-design.md) | 前三处是「以 kani-harness 为形式化验证载体」的失真注释；design 册属 §6 必同步面（`check_doc_paths` 射程）。`privileged/sync/memorder.rs:24` 属**工具类别泛指**，不动（§11.2） |
| **kani 相关契约锁** | `ci_entrypoint_unified_test.rs`: `:295`/`:313`（F9 五树逐字针）、`:735`/`:762-770`（F7 与 I-07 的四根针）、`:2467`/`:2487-2506`（P-60 ①面）、`:2549`（AGENTS F8 三树针）；[td25_comment_language_test.rs:72-76](../../host-tests/tests/td25_comment_language_test.rs#L72)（临时骨架建根） | 一律**改钉不删针**；td25 是行为测试，骨架多建一个空根不会红但属 stale，同批清 |

> **不要求同步的面**: `docs/plan/` 与 `docs/report/` 里点名 `audit_invariants` 的历史文档
> （`code-audit-final-summary.md` / `unresolved-issues-2026-08-09.md` / `multithreading-project.md` /
> `syscall-followup.md` 及多份 report）按 §6 **不要求路径新鲜**——它们记录执行当时的形态，
> 改它们等于篡改历史。`docs/plan/archive/` 同理。

---

## 六、决策点（2026-11-19 全部已裁定）

> 用户回复「授权」⇒ **D-1 甲 / D-2 甲 / D-3 甲 / D-4 甲**；批 0 出数后就两项新议题再裁定
> ⇒ **D-5 乙 / D-6 甲**；2026-11-19 用户裁定「**先退役现有的 kani吧，以及先完成门禁的退役工程，
> 这些测试等这些工程完成后再谈**」⇒ **D-7 丙**（派生，见下表）+ **kani-harness 整树退役**并入批 4
> 作前置项。**七项均已闭合，无开放决策**。据此:
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
| **D-7** | `audit_once_cell` 处置（批 2 实测后新立，**2026-11-19 已裁定＝丙**） | 甲 整腿保留不动（改判「不可替代」）/ **乙 部分收敛**：`BASES` 收缩到 **kani-harness 单树**，其余三树交 clippy / 丙 照原结论整腿退役，显式接受 kani-harness 2 文件失去执法 | **丙**（2026-11-19）。**裁定路径不是直接选项，而是用户「先退役现有的 kani」的派生结论**：kani-harness 整树退役后，四树面里那棵「clippy 不可达」的树不复存在，余下三树全由批 2 已实测 live 的 `disallowed-types` 执法 ⇒ 整腿退役**不再是执法净损失**，丙的原代价（2 文件无执法）随树消失，甲/乙 的前提（保住该树执法）已不成立。分类计数维持 **3 整腿退役 / 3 部分收敛 / 2 不可替代**。原病灶留档: 脚本扫四树 1442 文件，clippy 七腿**不含 kani-harness**，且该树实测 `cargo clippy` **RC=101**（E0433 ×3 + `unexpected_cfgs`）⇒「加一条 kani 腿」已堵死（批 2 详情三）。按用户判据该面生态工具**更弱**，与 `audit_dead_code_comment` 改判保留同型。取舍: 甲零成本但三树永久双执法；乙与另三条部分收敛腿同构、残余面最小（2 文件），且 vendored 扫描面不对称随之消失，代价是 `test_p60_scope_closure_and_silent_skips_fixed` 的①面针（BASES 四树 + fail-closed）须**改钉不删针**；丙损失执法，违「防回归优先固化门禁」偏好。**分类计数随裁定变**: 甲 → 2 整腿 + 3 部分 + 3 不可替代；乙 → 2 整腿 + 4 部分 + 2 不可替代；丙 → 维持 3/3/2 |

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