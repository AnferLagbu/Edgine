# `ci-rustdoc` 文档链接门禁收口

> **定位**：把 P-39 立案的实测事实收成一个可执行工程——本地一键 CI 的 rustdoc 腿在 HEAD 上即红（立案口径「30 error + 19 warning / 19 文件」，两腿都真跑后订正为 x86_64 = 29 unresolved + 16 private + 3 无位置、aarch64 = 28 + 33 + 3，见 §1.2），且该腿不在 `AGENTS.md` §2.3 六门槛内，所以历轮"六门槛全绿"从未覆盖它。本文档是该病灶的唯一推进场所。**R-1 乙 已在第七轮落地**（§4 为施工记录），仍开放的是需要用户新裁定的 R-6 / R-7 与立案未施工的 R-8。
>
> **来源**：`docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-39（本轮按用户裁定「单开」毕业至此，原条目改为指针；该册 R-1 乙 结案后该条转 `[X]`）。
>
> **关系**：判据与工具链口径见 `AGENTS.md` §2.2/§2.3 与 §5 F8（公共 API 中文文档注释）；`docs/plan/progress-active-tasks.md` §对账方法。与 P-37（GitHub Actions 腿从不触发）正交：同一判据在 `ci-lint.yml` 的 `rustdoc-check` job 里也会红，但那条腿在 Gitee-only remote 上不可达，故本工程的验收只能走本地。判据字符串本身由 `host-tests/tests/ci_entrypoint_unified_test.rs::test_ci_rustdoc_matches_workflow_command` 把守（本地入口与 workflow 不同源即红）——加压 `-D` 面时必须同一轮改四处（`Makefile.ci` ×2、workflow ×2）并同步该锁，否则门槛 4 红。
>
> **不做什么**：不改 `Makefile.ci` 的 `-D` 判据来"让门禁变绿"——那属门槛假绿的另一类（对照 `docs/plan/unresolved-issues-2026-08-09.md` 第 0A 类 AUDIT-TOOL-* 的根治立场：修病灶，不修判据）。本轮的判据变动方向只有**加压**（把 private warning 升为阻断），源码侧病灶先清干净再由回归锁证明该判据咬人（§4.3）。

## 1. 立项依据

### 1.1 门禁是什么

`make -f Makefile.ci ci-rustdoc` = 双架构各跑一次 `cargo doc --no-deps`，立案时的判据是 `RUSTDOCFLAGS="-D rustdoc::broken-intra-doc-links"`（本轮加压后的现行面见 §4.3）。x86_64 腿直接失败（RC=101）。`Makefile.ci` 顶部有 `SHELL := /bin/bash -o pipefail`，故该 recipe 的 `| tee | tail -5` **不**掩盖退出码——报的是**真红而非假绿**（与本轮已根治的五处 workflow 假绿不同类）。

后果有二：一是 aarch64 腿从未被执行（同一 recipe 串联，x86 先红即中断）——**该中断形态本轮已随 R-5 消除**，`ci-rustdoc` 现在两腿都跑完再汇总退出码；二是 §2.3 不含 rustdoc，所以这条腿坏了多久无从追溯，历轮验收也不含它（这条仍在，归 R-6）。

### 1.2 失败分布（实测，HEAD 上）

两类形态：

- **`error: unresolved link`**：被反引号包裹却无对应可解析项，rustdoc 当 intra-doc link 解析。样本：`` `idx` ``、`` `24` ``、`` `gs:0` ``、`pl011_phys_base`。
- **`warning: public documentation for X links to private item Y`**：pub API 的文档注释指向私有符号。

**口径订正（本轮把两腿都真跑过一次之后）**：立案时写的「30 error + 19 warning / 19 文件」有两处失真——① 第 30 条 `error` 行其实是 `error: could not document \`kernel\``，cargo 的收尾行而非一个链接病灶；② 立案时 aarch64 腿从未执行（串联中断），warning 面只看见 x86 的一半。HEAD 上的真读数（对两腿原始日志逐条 grep 诊断行统计）：

| 架构腿 | `unresolved link` error | `links to private item` warning | 无源码位置 warning | 带诊断的文件数 |
| --- | --- | --- | --- | --- |
| x86_64-unknown-none | 29 | 16 | 3 | 19 |
| aarch64-unknown-none-softfloat | 28 | 33 | 3 | 21 |

两腿的 3 条无位置 warning 是同一组：`unclosed HTML tag \`u8\`` / `unclosed HTML tag \`dyn\`` / `could not parse code block as Rust code`（立案时把这三条与 16 条 private 合并计数，故得「19 warning」；单列为 R-7）。私有链接族两腿取并集、按 `-->` 的 文件:行 归一后 = **36 站点 / 18 文件 / 25 个私有符号**，最密的一档是 `privileged/arch/aarch64/gic.rs` 7 处。

文件分布（x86_64 腿）：`privileged/mm/pmm.rs` 11 处、`functions/driver/usb/xhci.rs` 4 处、`privileged/mm/mod.rs` 3 处；`vmm_x86_64.rs` / `functions/proc/process_vm.rs` / `functions/driver/char/mod.rs` 各 2 处；`dtb.rs` / `arch/x86_64/gdt.rs` / `arch/x86_64/apic.rs` / `mm/cow.rs` / `timer/clockevent.rs` / `timer/calibration.rs` / `syscall/dispatch.rs` / `smp/mod.rs` / `proc/user_proc.rs` / `proc/process.rs` / `net/init/dns.rs` / `functions/proc/coredump.rs` / `functions/net/syscall.rs` 各 1 处。

### 1.3 为什么不是机械替换

每处需三选一定型：

| 修法 | 动作 | 适用 | 代价 |
| --- | --- | --- | --- |
| ① | 引用改成可解析真实路径（`crate::path::to::Item`） | 被引对象确为公开项、只是路径写错 | 需逐个核符号可见性 |
| ② | 让该词不成链接（去反引号 / 改写为文字描述） | 被引者是散文用词（变量代称、寄存器名、十进制字面量） | 零 API 影响，最保守 |
| ③ | 把被引符号升为 `pub` 并配中文文档注释 | 被引者本应属公共 API 却被私有化 | **改 API 可见性面**，触 F8 与 §7.2 review 要求 |

`links to private item` 一族尤其落在 ② / ③ 之间，那是 **API 可见性判定**，不能由工具或实施者代答（§9.1 决策灰色地带）。

## 2. 工程条目

- **R-1 深度裁定（甲 / 乙）**：本工程实修到哪一层，须用户先裁。
  - 描述：甲 = 只清 30 个 `unresolved link` error，使 `ci-rustdoc` 转绿，warning 按现状放行；乙 = error 与 19 个 `links to private item` warning 一并收口，并把 `RUSTDOCFLAGS` 的 `-D` 面加压到 `rustdoc::private_intra_doc_links`（此后 warning 即阻断）。乙含 ③ 类改动的逐处可见性判定，改动面与 review 成本显著高于甲。
  - 方案：按 §11.3 前置询问后择一开工；本条未裁定前 R-2 以下不得启动。
  - 状态：[X]（**用户裁定乙**：error 30 + warning 19 一并收口，并把 `RUSTDOCFLAGS` 加压到含 `rustdoc::private_intra_doc_links` 的阻断面。⇒ R-2 以下全部解锁；乙 含 ③ 类逐处可见性判定，施工时按 §11.3 的"相对完整"口径走，禁用以 `#[allow(rustdoc::…)]` 抑制过关（§5 F9）。裁定文字里的「30 error + 19 warning」是立案口径，落地时按两腿真读数收口，订正见 §1.2）

- **R-2 ②类先行批次（散文用词去链接化）**：风险最低的一族先收。
  - 描述：把 `` `idx` `` / `` `24` `` / `` `gs:0` `` 这类被反引号包裹的散文用词改为不成链接的写法，零 API 影响、零行为影响，可先削掉 error 总数的相当份额。
  - 方案：逐处改写中文注释（§5 F7），跑 `make -f Makefile.ci ci-rustdoc` 复验；每批次以文件为单位提交，便于回退定位。
  - 状态：[X]（实测 27 行按 ② 定型：给寄存器位段/十进制字面量补反引号使其成为纯代码跨度（如 `functions/driver/usb/xhci.rs:262` 的 `PLS[8:5] | PP[9] | PIC[14] | WCE[24]` 四个位段一行产生 4 条 error），或把散文代称的反引号/方括号去掉（`privileged/arch/aarch64/mod.rs:232-235` 的 `[23:16]/[39:32]/[55:48]`）。**未使用 `\[` 转义形态**——转义在注释里可读性差且易被后续改写复原。）

- **R-3 ①类批次（路径校正）**：被引者已是公开项、只是 intra-doc 路径写错。
  - 描述：为每处确定当前可见路径并改写为 `crate::…::Item` 形式；这批改动天然可验证（rustdoc 不再报 unresolved）。
  - 方案：先 `cargo doc` 输出按文件分组，逐文件收口并复验；若某处发现被引者其实私有，转 R-4 处理，不就地升 `pub`。
  - 状态：[X]（5 行：`privileged/driver/infra.rs:8`（`super::egdf` → `crate::privileged::egdf`）、`privileged/mm/lazy_arena.rs:20`、`privileged/proc/user_proc.rs:1108`、`functions/fs/api.rs:24-25`。逐处先 grep 核对目标确为 `pub` 可达项再改写；改写后 rustdoc 不再报 unresolved 即天然可验证，无一处需回退为 `pub` 提升。）

- **R-4 ③类清单（可见性判定台账）**：把需要"升 `pub`"的候选集中列出让用户裁。
  - 描述：`links to private item` 的每一处对应一个"该符号是否属公共 API"的问题。逐处登记：符号、所属模块、当前可见性、若升 `pub` 是否需配中文 doc（F8）、是否触 TCB 边界（I1-I6）。
  - 方案：先出清单不改动，交用户逐条或批量裁定后再实施。
  - 状态：[X]（清单实出：**36 站点 / 18 文件 / 25 个私有符号**（两腿并集，按 `-->` 文件:行 归一），最密一档 `privileged/arch/aarch64/gic.rs` 7 处（同一私有字段 `redist_frames` 被 4 个 pub 项引用）。逐符号台账见 §4.2。**用户批量裁定甲・全部去链接化** ⇒ 零 `pub` 提升、零 API 可见性面变化，验收判据 2 的"每一处 `pub` 提升都有 F8 中文 doc"因提升数为 0 而空真。）

- **R-5 aarch64 腿从未执行的验证**：修完后必须让第二架构真跑一次。
  - 描述：现状是 x86 先红即中断，aarch64 腿的 rustdoc 状态**完全未知**——x86 修绿后 aarch64 可能新报一批（`dtb.rs` / `pl011_phys_base` 等显属 aarch64 侧）。
  - 方案：R-2/R-3 收口后跑完整 `make -f Makefile.ci ci-rustdoc` 双架构，并把 aarch64 独有问题按 ①②③ 归类续修；复验记录写进本报告式段落（不留状态字段）。
  - 状态：[X]（预言坐实且**方向相反**：aarch64 的私有链接面比 x86 大得多（33 vs 16 条 warning，21 vs 19 个文件），因 `#[cfg(target_arch = "aarch64")]` 侧的 pub 项只有在 aarch64 腿才被文档化。recipe 亦改造为**两腿都跑完再汇总退出码**（旧形态 x86 红即中断，第二腿永不测得）；负控制实测：注入一处私有链接 + 一处 unresolved 后 `x86_64 rustdoc exit: 101` 与 `aarch64 rustdoc exit: 101` **同时**打出，两腿各自报错清单一致。aarch64 独有的 8 个站点（并集 24 − 交集 16）全部落在 ①② 两类内，无需新的可见性裁定。）

- **R-6 门禁归位（可选，需另行授权）**：rustdoc 要不要进 §2.3。
  - 描述：rustdoc 现为 `Makefile.ci` 的 `ci` 聚合成员但不在 §2.3 六门槛，导致「`make -f Makefile.ci ci` 失败」与「本轮 §2.3 全绿」两个陈述可同时成立而不矛盾——这是口径漏洞而非代码缺陷。
  - 方案：两种收口：把 rustdoc 列为 §2.3 第 7 条（需改 `AGENTS.md`，属文档保护条款，须用户授权），或在 §2.3 明确注明 rustdoc 属本地可选腿。本工程默认后者（改动面小），前者由用户裁定。
  - 状态：[ ]（**未动**：两个收口形态都要改 `AGENTS.md`，而 §2.2 的一次性订正授权已在 `613b324a` 消耗完；需用户另行授权并裁定形态。本工程其余条目不依赖本条——门禁已在本地 `Makefile.ci` 聚合里 fail-closed。）

- **R-7 无源码位置的三条 warning：判据面是否再加压**：
  - 描述：两腿各有三条不带 `-->` 位置的 warning（`unclosed HTML tag \`u8\`` / `unclosed HTML tag \`dyn\`` / `could not parse code block as Rust code`），来自散文里的裸 `<T>` 写法与未标 `text` 的 ASCII 表格围栏。`ci-lint.yml` 的策略行明确写着"不阻断 rustdoc 内部 HTML tag 自动生成警告 (无源码位置)"，故 R-1 乙 的加压面只含两个 intra-doc link lint。
  - 方案：三条的**病灶本轮已全部消除**（`privileged/mm/pmm.rs` 的 `Vec<u8>` 随 ② 批反引号化、`functions/net/smoltcp_impl.rs:29` 的 `Box<dyn Device>` 反引号化、`privileged/timer/sleep.rs:11` 的围栏补 `text`），两腿现在 **0 error 0 warning**（唯一残留是 cargo 的 future-incompat 报告，属依赖侧非本 crate）。于是出现新的裁定空间：既然已无噪声，是否把 `-D rustdoc::invalid_html_tags -D rustdoc::invalid_rust_codeblocks` 一并纳入阻断面（此后任何裸 `<T>` 散文即红）。代价是这两条 lint 无源码位置，红的时候定位成本高（本轮定位靠逐类反推），且 vendored smoltcp 的 `///` 注释里也有裸 `Vec<u8>` 写法（`functions/net/smoltcp/src/iface/fragmentation.rs:47`），加压后若触发即与"smoltcp 锁定不可改"冲突（§1 表）。
  - 状态：[ ]（**须用户裁定**：加压 / 维持现状（判据面止于两个 intra-doc link lint，policy 行照旧）。甲＝维持现判据面，R-7 就此结案（三条已清零，无待修项）；乙＝加压到四条 lint，须先实测 vendored 侧是否红，红了只能开工程册另议。本工程默认把选择权交出，不自行加压。）

- **R-8 文档围栏自身的三处断点（工程外发现，按 §11.5 登记不施工）**：
  - 描述：为定位 R-7 的三条无位置 warning 而全量扫 doc 围栏时，另见三处**不影响任何门禁、但会让 rustdoc 渲染出错**的预存断点：`functions/klog.rs:169` 的 ` ```json ` 围栏未闭合、`privileged/sync/spinlock.rs:230` 的 ` ```rust,ignore ` 围栏未闭合（238 行的重复标题被吞进代码块，且 237 行行首多一个反引号）、`functions/net/unix.rs:1049` 行尾落单反引号。三者都在 HEAD 上就存在，与本轮 69 行改动无交集。
  - 方案：三处都是单行改写（补闭合围栏 / 去掉多余反引号），零 API 影响；但既无门禁覆盖，属"顺手改"范畴 ⇒ 按 §11.2 报告不处置，等用户授权或并入下一轮。
  - 状态：[ ]（立案，零施工。）

## 3. 验收判据

1. `make -f Makefile.ci ci-rustdoc` 双架构全绿（x86_64 **与** aarch64 都真跑过，不是串联中断）。
2. 若 R-1 选乙：`RUSTDOCFLAGS` 加压后仍全绿，且 R-4 台账每一处 `pub` 提升都有 F8 中文 doc 注释。
3. `AGENTS.md` §2.2/§2.3 的门槛复跑不受影响：F5 双架构 0w0e、F7 中文注释、F8 clippy `missing_docs` 全部仍绿。
4. 无 `#[allow(dead_code)]` / `#[allow(rustdoc::…)]` 类压制（§5 F9 零容忍；靠豁免注释让 rustdoc 变绿即拒收）。

## 4. 施工记录（报告式，不留状态字段）

### 4.1 改动面与判据读数

改动面两类：**69 行 `src/kernel/` 文档注释行**（33 个 `.rs`；`git diff` 逐行判定"非 `///`/`//!` 的增删行"= 0 处，且增删行数相等 ⇒ 无行号位移）+ `Makefile.ci` 与 `.github/workflows/ci-lint.yml` 各两条 `RUSTDOCFLAGS`（判据面，见 §4.3）。零 `.rs` 代码行改动、零 API 可见性改动。

按修法归族（判据 = 该行是否引入 `](路径)`、是否落在私有站点清单内）：

| 族 | 量 | 手法 |
| --- | --- | --- |
| ① 路径校正 | 5 行 | 被引者确为 `pub` 可达项，改写为可解析路径（`privileged/driver/infra.rs:8`、`privileged/mm/lazy_arena.rs:20`、`privileged/proc/user_proc.rs:1108`、`functions/fs/api.rs:24-25`） |
| ② 去链接化 / 补反引号 | 27 行 | 散文用词与位段记法降为纯代码跨度，或去掉让 rustdoc 误解析的方括号（`functions/driver/usb/xhci.rs:262` 一行的四个位段产生 4 条 error，是最密的单点） |
| ③ 私有链接 | 36 站点 | 用户批量裁定甲・全部去链接化 ⇒ 零 `pub` 提升（台账见 §4.2） |
| cfg/架构门控链接 | 7 站点 | 用户裁定甲・降为纯文本代码跨度（见 §4.4） |
| R-7 三病灶 | 2 行 | `privileged/timer/sleep.rs:11` 围栏标 `text`、`functions/net/smoltcp_impl.rs:29` 的 `Box<dyn Device>` 补反引号（第三条 `unclosed HTML tag \`u8\`` 由 ② 批的 `Vec<u8>` 反引号化顺带消除） |

判据读数：`make -f Makefile.ci ci-rustdoc` ⇒ `x86_64 rustdoc exit: 0` + `aarch64 rustdoc exit: 0`，两腿完整日志各 5 行，`error` / `warning: public documentation` / `unresolved link` / `unclosed HTML tag` / `could not parse code block` 命中 **0**；唯一残留是 cargo 的 future-incompat 报告（`core` / `sha2`，依赖侧，非本 crate 文档）。

### 4.2 ③类可见性台账（25 个私有符号 / 36 站点）

裁定甲 ⇒ 每一处的处置都是"文档不再链向私有项"，不产生 `pub` 提升，故验收判据 2 的"每一处 `pub` 提升都有 F8 中文 doc"因提升数为 0 而空真。台账按站点数降序：

| 私有符号 | 所在文件 | 引用它的 pub 项 | 站点 |
| --- | --- | --- | --- |
| `redist_frames` | privileged/arch/aarch64/gic.rs×5 | enable_sgi, enable_timer_ppi, init_per_cpu, init_redistributor | 5 |
| `Self::map_page_in_table_locked` | privileged/mm/vmm_aarch64.rs×2, privileged/mm/vmm_x86_64.rs×2 | map_page_in_table | 4 |
| `handle_irq` | privileged/arch/aarch64/exception.rs×3 | irq_handler, irq_handler_el0 | 3 |
| `affinity_pack` | privileged/arch/aarch64/mod.rs×2 | cpu_id, send_ipi | 2 |
| `process_vm_rw` | functions/proc/process_vm.rs×2 | process_vm_readv_syscall, process_vm_writev_syscall | 2 |
| `ApBootInfo` | privileged/arch/aarch64/smp_init.rs | smp_init | 1 |
| `CPU_HW_IDS` | privileged/smp/mod.rs | current_cpu_index | 1 |
| `DEFAULT_CALIBRATION_WINDOW_MS` | privileged/timer/calibration.rs | calibrate_counter_hz | 1 |
| `EL0_OBSERVED_PER_CPU` | privileged/syscall/dispatch.rs | observe_el0_syscall | 1 |
| `EL1_VIEW_SLOT` | privileged/mm/vmm_aarch64.rs | build_el1_view | 1 |
| `FDT_MAX_SIZE` | privileged/dtb.rs | decode_header_prefix | 1 |
| `GICR_SGI_OFFSET` | privileged/arch/aarch64/gic.rs | set_bases | 1 |
| `PROC_FREG_ROLLBACK_SAFE` | privileged/proc/process.rs | proc_register_freg_domain | 1 |
| `PhysicalMemoryManager::note_frame_retired_site` | privileged/mm/pmm.rs | pmm | 1 |
| `PsciError::from_i64` | privileged/arch/aarch64/psci.rs | cpu_on | 1 |
| `SPI_BASE` | privileged/arch/aarch64/gic.rs | register_device_spi | 1 |
| `Self::note_frame_alloc` | privileged/mm/pmm.rs | alloc_page | 1 |
| `ap_main` | privileged/arch/aarch64/smp_init.rs | smp_init | 1 |
| `check_access` | functions/proc/process_vm.rs | process_vm | 1 |
| `conduit` | privileged/arch/aarch64/psci.rs | psci | 1 |
| `cow_handle_fault_locked` | privileged/mm/cow.rs | cow_handle_fault | 1 |
| `dns_query_net` | privileged/net/init/dns.rs | dns_resolve | 1 |
| `do_coredump` | functions/proc/coredump.rs | StandardCoredumpSink | 1 |
| `single_core_fallback` | privileged/arch/aarch64/smp_init.rs | init | 1 |
| `verify_expiry_observed` | privileged/timer/clockevent.rs | rearm_local | 1 |

台账的形状本身说明"甲"是站得住的：这 25 个符号清一色是**锁内实现的辅助函数/内部字段/内部常量**（`*_locked` 后缀 5 个、`redist_frames`/`conduit`/`affinity_pack` 等机制内部件、`PROC_FREG_ROLLBACK_SAFE`/`EL0_OBSERVED_PER_CPU` 等护栏表），把它们升 `pub` 会扩大 functions 侧可见面而没有任何调用方需要——正是 §4.1 归属判据里"机制不外露"的一侧。

### 4.3 判据面（加压后的阻断面）与回归锁

`RUSTDOCFLAGS` 现为 `-D rustdoc::broken-intra-doc-links -D rustdoc::private_intra_doc_links`，四腿同步改：`Makefile.ci:118`/`Makefile.ci:126`（本地入口）与 `.github/workflows/ci-lint.yml:241`/`ci-lint.yml:249`（workflow，按该文件头部"命令与 workflow 保持逐字一致"的约束同改；该腿在 Gitee-only remote 上从不触发，见 P-37）。`Makefile.ci` 的策略 echo 与 `PASS` 文案同步反映两个 lint。

**回归锁（本工程的"回退即转红"证据）**：临时把两处改回 HEAD 形态——`functions/proc/coredump.rs:151` 恢复 `[`do_coredump`]`、`functions/driver/usb/xhci.rs:262` 恢复裸 `PLS[8:5] | PP[9] | PIC[14] | WCE[24]`——`make -f Makefile.ci ci-rustdoc` **RC=2**，且两腿日志各自打出 `error: public documentation for \`StandardCoredumpSink\` links to private item \`do_coredump\`` 与 `error: unresolved link to \`8:5\`` / `\`9\`` / `\`14\`` / `\`24\``，`x86_64 rustdoc exit: 101` 与 `aarch64 rustdoc exit: 101` 同时打出。还原后 RC=0。⇒ 判据确实咬人，且**两腿都真跑**（旧形态只有第一腿会跑到）。

### 4.4 cfg/架构门控链接族的处置（第二问裁定甲）

7 个站点的特殊性：被引对象本身是 `pub`，但被 cfg 门控——`#[cfg(target_arch = "aarch64")] pub fn pl011_phys_base`（3 站点）、`#[cfg(any(test, feature = "host-test"))] pub struct VecMetaStore`（1）、`pub(crate) mod raw`（1，`privileged/net/mcast.rs:18`）、`affinity_pack`（2，私有且在 aarch64 侧）。同一条文档在两腿的可解析性不同 ⇒ 无论怎么修，链向它的 intra-doc link 必在某一腿红。曾考虑的乙形态是 `#[cfg_attr(target_arch = "…", doc = "…")]` 分段注入，但全仓 `#[cfg_attr(…, doc = …)]` **零先例**（grep 实测），引入即属新的文档写法约定，须用户裁定；裁定甲 ⇒ 7 处全部降为纯文本代码跨度（`` `pl011_phys_base` `` 等），零新先例、零 API 面变化。
