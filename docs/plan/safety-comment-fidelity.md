# SAFETY 内容保真与注释失真收敛工程

> 一句话说清为什么有这个计划: [gate-health-convergence.md](./gate-health-convergence.md)
> 批 5a–5g 把四树 SAFETY **缺失**存量清零（棘轮 399 → 0 处 / 0 文件 / 0 键，侧车 `entries` 空，
> 三树 clippy 腿翻 `-D clippy::undocumented_unsafe_blocks` 硬执法）。但批 5g 的终态复核暴露了
> 一个**位置判据永远看不见的面**：clippy 与棘轮只判「有没有、在不在正确位置」，**从不判理由是否真实**。
> 占位短语（如「调用方保证」「此处安全」「见上文」）照样通过，且**不计入债册** ⇒
> **债册归零 ≠ 理由真实**。用户 2026-11-19 裁定 **⑮ 走甲「加内容面审计腿」**；
> 同批裁定的 D 组其余 28 项注释/文档失真与冗余项**另行立项**，本册即该立项。
>
> 姊妹册 [safety-gap-remediation.md](./safety-gap-remediation.md) 修**代码缺口**（27 项），
> 本册修**注释与文档的失真**（30 项）。两册项号都沿用 gate-health 册的 ①–58 全局编号，不重编号。

---

## 一、为什么内容面腿是「丙类・生态无对应物」

门禁健康度收敛的判据是「**有比项目自建门禁更好的 rust 生态检查工具即为不必要**」。
按该判据，本册 C1 要加的内容面腿属**丙类**，与批 4 退役的三条腿（invariants / once_cell /
reverse_deps）性质相反：

| 面 | 生态工具 | 结论 |
|---|---|---|
| SAFETY **存在性 / 位置** | clippy `undocumented_unsafe_blocks`（块 / impl）、`missing_safety_doc`（fn / trait） | 已收敛给生态（批 3–批 5） |
| `unsafe extern` / 引用面存在性 | clippy 两个 lint 都不报（生态缺口） | 保留 `audit_safety_coverage.py` |
| SAFETY **理由真实性 / 占位识别** | **无任何生态工具判内容** | **丙类：自建腿正当** |

故 C1 不是「重造轮子」，而是补生态确实不覆盖的一面。**但本册不得借 C1 之名扩围**：
内容面腿只判「理由是否为占位/空泛」，不判「理由在技术上是否正确」——后者无法机械判定，
只能靠批 5 的「先读实装再落笔」纪律与人工 review。

### 1.5 登记本身也须复核（批 6 立项时已订正 4 条）

gate-health 册的发现清单是**补写 SAFETY 当时**的快照。批 6 立项前逐项现树复核，
订正下列四类误判（**施工时仍须再核一次**）：

| 项 | 原登记 | 现树真相 |
|---|---|---|
| **⑲** | 「陈旧链接 `isr.asm`，该文件已不存在」 | **文件存在**于 `src/kernel/privileged/boot/isr.asm`；错的是 `idt/types.rs` 的 markdown 链接 `[isr.asm](../../kernel/isr.asm)`（从该文件出发解析到 `src/kernel/isr.asm`），正确相对路径是 `../boot/isr.asm`。**两道门禁都看不见**：显式 URL 非 intra-doc link ⇒ rustdoc 的 `broken-intra-doc-links` 不查；`check_doc_paths` 只扫 must-sync 文档、不扫源码注释 |
| **㉖ / ㉟** | 「冗余 auto-trait impl」 | 批 5e/5f 的 SAFETY **已显式披露**它们是「非类型必需项的显式声明」（`MultibootPtr: Sync` 明写「不是该 static 的类型必需项」；`KeyboardDriver: Sync` 明写「auto Sync 本可派生，本 impl 只是显式化」）⇒ **不是失真**，只裁定去留；删之前须先核实字段真能派生 auto trait |
| **㉗** | 「`set_driver_data` doc 失真 + 零调用方」 | `set_driver_data` **全仓无定义**，只被 `egdf/mod.rs` 的 `driver_as_mut` / `driver_as_ref` 两处 `# Safety` doc 引用 ⇒ 真缺口是 **doc 引用了不存在的函数名**；真实写入面是注册入口 `egdf_register` / `egdf_register_with_ops` 的 `driver_data` 形参 |
| **55** | 「`freg/domain.rs` doc 称 `spin::Mutex`」 | 本轮 grep 在该文件**未命中** `spin::Mutex`（全文只有 `IrqSpinLock`）⇒ 须复核该项是否仍成立，可能已改写或登记位置有误 |

**占位面不止 `glue/string.rs`**：`idt/table.rs` 的 `cli()` 上方同样是
「调用方保证指针/类型有效 (详见上下文)」⇒ C1 的窄口径 251 处有真实靶，
不是只在批 5g 改过的那一个文件里。

---

## 二、终态读数（C1 实装时实测）

扫描面 = `src/kernel`（排除 vendored `smoltcp`，含 `#[cfg(test)]`）：

| 口径 | 读数 | 含义 |
|---|---|---|
| SAFETY 注释块总量 | **2248 块 / 631 扫描文件** | 全树所有 `// SAFETY:` 标签（含连续 `//` 续行合并） |
| **窄口径**（禁占位短语清单命中） | **245 处 / 58 文件** | C1 侧车的初始存量 |
| **宽口径**（含空泛/无实质依据形态） | **469 处 / 99 文件** | 改写工程的总工作面 |

> 判据 = 标签行 + 紧随的连续 `//` 续行合并成块后做短语匹配；SAFETY 标签行总量用块计数，
> 与早先按单行粗测的读数（2786 / 251 / 463）差异来自「续行合并」与仓库演化，权威读数
> 以 `scripts/audit_safety_content.baseline.json` 的 `totals` 为准。

**两条不可违反的读数纪律**：

1. **该面 ⊃ 批 5 的补写面**（债册只计「缺失」，内容面计「已存在但空泛」）⇒
   **不可**把批 5d 的 39 / 批 5e 的 117 / 批 5f 的 14 相加当作本册工作面。
2. 窄/宽两口径的**判据清单必须与侧车同批钉下**（机器派生，`--update-baseline` 为唯一写者），
   本文不抄录逐项数量，避免双写漂移（与 F9 侧车同口径）。

---

## 三、批次条目

### 批 C1・内容面审计腿实装（⑮ 甲）

描述：新增 `scripts/audit_safety_content.py` + 机器派生侧车
`scripts/audit_safety_content.baseline.json`，与 `audit_safety_debt.py` **同构**：
禁占位短语清单（窄口径）+ 空泛形态清单（宽口径）→ SAFETY 注释位点收集（纯文本扫描
`src/kernel`，不跑 cargo）→ 双向对账（只拦新增）→ 存量清零后转硬零。接入 `ci/audit.sh`
`full` 档步骤 2e 的 `AUDIT_REQUIRED_LEGS_FULL`。
方案：① 判据清单先按 §二 的窄/宽两口径实测钉下，侧车由 `--update-baseline` 生成，
**不手改**；② 复用 `audit_safety_debt.py` 的**塌缩守卫**形态（total==0 且 baseline>0 且
扫描面不可读（0 文件 / 0 条 SAFETY 标签）⇒ RC=2 拦 `--update-baseline`）——内容判据是
注释文本、不依赖编译，故以「扫描面非空」作活性信号，防「取数失败被读成清零」；
③ **接线形状锁**：`host-tests/tests/ci_entrypoint_unified_test.rs` 补臂，钉新腿在
`AUDIT_REQUIRED_LEGS_FULL` 内、且 quick 档不含它；④ `audit_selftest` 补自测用例，
**项数标签有针**（`host-tests/tests/vacuous_needle_gate_test.rs:229`）⇒ 增删自测用例
必须同批改钉，否则 `./ci/build.sh all` 红；⑤ 两项负控制（注入占位理由 → 红；
摘旗标 → 形状锁红），RC=101 取证；⑥ 逐批改写放在 C2–C5，**C1 本身不改任何既有注释**。
**前置（已授权并已用掉）**：新增一条 full 档阻断腿改变 `AGENTS.md` §2.2 的两处计数
（「共 10 条腿」→ 11、「仓库共 26 个 `audit_*.py`」→ 27）及完整清单的腿名枚举；
本轮已取得用户授权并同批更新。批 4 与批 6 的两次授权此前已用掉。
**结果**：`scripts/audit_safety_content.py` 已实装（判据清单冻进侧车 `_meta.criteria`，
判据与侧车不一致即 fail-closed），初始存量窄口径 **245 处 / 58 文件**、宽口径
**469 处 / 99 文件**（侧车 totals **714 处 / 157 键**）；已接入 full 档
`AUDIT_REQUIRED_LEGS_FULL`（11 条腿），形状锁补臂、`audit_selftest` 第 14 项、
项数针改 `[Test 12/14]`。C1 未改任何既有 SAFETY 注释（逐批改写属 C2–C5）。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `bash ci/audit.sh full`
RC=0（未执行腿数 0、Lockbud 4/4、QEMU 2/2，含 `safety_content` 第 11 条腿通过）·
`make test-host` **212 套 / 1651 passed / 0 failed** · `make test-kernel-host`
**12 + 811 + 608 = 1431 / 0 failed** · `ci-rustdoc` 双架构 exit 0。两项负控制：
注入占位理由 ⇒ `audit_safety_content` **RC=1**（narrow / wide 各新增 1 键）；
从 `AUDIT_REQUIRED_LEGS_FULL` 摘掉该腿 ⇒ 形状锁 **RC=101**；均 `cp`+`sha256` 复原。
注：本环境 `CARGO_TERM_COLOR=always` 会让 Lockbud 的 `^   Compiling` 计数被 ANSI
前缀打断（首轮误判「实编 0/4」），full 档须以 `CARGO_TERM_COLOR=never bash ci/audit.sh
full` 取数；该环境敏感点是预存问题，不属 C1 改动面。
状态：[X]

### 批 C2・arch + cpu + idt + boot 占位改写与注释失真（⑧ ⑯ ⑱ ⑲ ㉑ ㉒ ㉔ ㉖）

描述：C1 侧车里 arch/cpu/idt/boot 面的占位理由改写（批 5e 曾在此面补写 73 处，
其中 **arch 面 117 处（92 块级）属内容空泛**，是宽口径的最大单块），并修同面 8 项注释失真。
方案：① 按批 5 方法逐处**先读内核实装再落笔**，重复形态单源化到模块级「unsafe 依据单源」段，
落笔后把注释引用的全部内核函数名一次性 grep 核实存在；② 8 项失真：
⑧ `arch/aarch64/exception.rs` doc 称 x1-x4 / asm 注释称 x1-x5，实装读 x1..x6；
⑯ `trampoline.asm` 布局框标 50 字节而 `ApStartupInfo` 实为 54 字节；
⑱ `IdtPtr` doc 称 base 在 +8 而 `repr(C, packed)` 下在 +2；
⑲ `idt/types.rs` 的 `[isr.asm](../../kernel/isr.asm)` **相对路径错**（解析到不存在的
`src/kernel/isr.asm`；真文件在 `privileged/boot/isr.asm`，应写 `../boot/isr.asm`）——
**两道门禁都看不见**（显式 URL 非 intra-doc link；`check_doc_paths` 不扫源码注释），见 §1.5；
㉑ `gic::init_distributor` 诊断位域标签与移位不一致；
㉒ `arch/aarch64/mod.rs` `interrupt_restore` 的 doc 把 DAIF 立即数位布局写成
bit0=D / bit1=A / bit2=I / bit3=F，真实是 **bit3=D / bit2=A / bit1=I / bit0=F**
（位序写反 + F/D 互换；**代码与批 5e 补的体内 SAFETY 都正确，只改 doc**）；
㉔ `smp_init` 注释称「80 字节对齐」而实装 `.align 3`（= 8 字节）；
㉖ `boot/mod.rs` `MultibootPtr` 的 `Sync` impl——**现树 SAFETY 已显式声明它非类型必需项**
（`IrqSpinLock<T>: Sync` 只要求 `T: Send`）⇒ 不是失真，**只裁定去留**；
注意 `MultibootPtr(*const u8)` 的裸指针本身 `!Sync`，故「auto trait 已自动成立」的原登记**不确**，
删之前须先核实真实派生面（§9.1 须问）。
③ **禁写占位理由**（C1 的腿会拦）；④ 纯注释批须验**零删除行**（`git diff --numstat`）。
**结果**：C2 面（`privileged/{arch,cpu,idt,boot}`）**窄口径 129 处 → 0**、**宽口径 269 处 → 30**
（全树侧车 714 处 / 157 键 → **475 处 / 129 键**），逐处改写为具体实装依据（锁名 / 寄存器 /
页表前提 / FFI 契约），并按 `arch!(interrupt_disable())`、`sched_ops::scheduler_tick`、
`net::poll_network`、`mmu::write_page_table_base`、`get_tss_mut` 等**实际符号名**回 grep 核实。
8 项失真：⑧ 两处 `x1-x4`/`x1-x5` 均改为 `x1-x6`（实装读 x1..x6）；⑯ `trampoline.asm` 布局框
50 → **54 字节**、补齐 `done` 字段并把代码起点改为 `0x803E`；⑱ `IdtPtr` 的 base 由 +8 改为 **+2**；
⑲ `idt/types.rs` 链接改 `../boot/isr.asm`（真文件在 `privileged/boot/isr.asm`）；㉑ 诊断位域标签
改为实际位移 `bits[9:5]` / `bits[10:8]`（只作日志近似，见下「遗留登记」）；㉒ DAIF 位序改为
**bit3=D / bit2=A / bit1=I / bit0=F** 并删除那条已失效的「预存文档缺陷」订正注；㉔ `.align 3`
= **8 字节**；㉖ `MultibootPtr` 的 `Sync` impl **保留**——裸指针 `*const u8` 本身 `!Sync`，该 impl
非空操作，且现树 SAFETY 已显式披露「非类型必需项」，只订正措辞不改去留。
**纯注释面判据**：`git diff --numstat` 删除列 308 行，逐行核验后**全部为注释/文档行**；
唯一非注释删除是 `gic.rs` 两行代码的尾注释改写（代码字节不变）。
**遗留登记（C2 读出、本批不改）**：`gic::init_distributor` 的 `GICD_TYPER` 取位为 `>>5 & 0x1F`
与 `>>8 & 0x07`，与 ARM 定义（ITLinesNumber=bits[4:0]、CPUNumber=bits[7:5]）不符 ⇒ 诊断日志
数值失真、无安全后果；按用户 2026-11-22 裁定**登记到姊妹册** `safety-gap-remediation.md` 的
批 **R10**（未开工），不在本批改代码。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `CARGO_TERM_COLOR=never bash
ci/audit.sh full` RC=0（未执行腿数 0、Lockbud 4/4、QEMU 2/2、`safety_content` 腿通过）·
`make test-host` **212 套 / 1651 passed / 0 failed** · `make test-kernel-host` **1431 / 0 failed** ·
`make -f Makefile.ci ci-rustdoc` 双架构 exit 0。侧车已 `--update-baseline` 重导（**475 处 / 129 键**）。
状态：[X]

### 批 C3・mm + sync 占位改写

描述：C1 侧车里 mm + sync 面的占位理由改写（批 5d 在此面补写 54 处，内容空泛存量 **39 处**）。
方案：① 同批 5 方法；② mm 面重点核**页表 / PMM / DMA 缓冲**的真实依据
（`DmaStream` / `alloc_coherent` 是 I6 的真实执法面，理由须指向它们而非空泛「已校验」）；
③ sync 面重点核**锁层级与中断上下文**（理由须与 `docs/explain/ref-lock-order.md` 的层级号一致，
不得写「调用方保证已持锁」而不指明哪把锁）；④ 本批**不改代码**——读出的代码缺口按 §11.5
登记并归姊妹册，不顺手修。
**结果**：C3 面（`privileged/{mm,sync}`）**窄口径 38 处 → 0**（计划册原文 39，现树实测 38，
差 1 为立项快照漂移）、**宽口径 104 处 → 1**（仅剩 `kmalloc::validate` 一条已具体的链表校验理由），
全树侧车 475 处 / 129 键 → **335 处 / 94 键**。mm 面逐处改写为页表 / PMM 实装依据：页表遍历
引用 `l0_index`..`l3_index` 的 `& 0x1FF ⇒ < 512` 与 `phys_to_virt` 的 HHDM 恒等映射；
`alloc_table` 清零引用 `get_pmm().alloc_page()` 的整页帧；COW / 缺页复制引用 `alloc_page()`
新帧与已映射 PTE 帧；`vmm_x86_64::get_pte_value` 引用入口判非 0；`swap_init` 删除一条悬在
安全调用 `find_contig_range` 上方、无对应 unsafe 块的错位 SAFETY 标签。sync 面逐处点名锁：
`Mutex::raw_lock` / `SpinLock::raw_lock` 的 Guard 临界区、`IrqSpinLock` 自身锁、`SeqLock` 的
sequence 奇偶协议、`OnceCellStorage` 的 `Once` 串行化，并把 `swap.rs` 八处统一写明 `SWAP.lock`
（`ref-lock-order.md` **层级 6**）。另订正两类注释失真：`rcu::RcuHead.func` 上方原 SAFETY
误写「`mut` 由调用方保证…只读访问」（字段无 `mut`、函数指针非只读访问），改为函数指针 ABI
与 `head` 存活期契约；`mutex.rs` 两处对 `&mut *data.get()` 误称「只读访问」，改为
`MutexGuard` 存活期内的独占可变借用。
**纯注释面判据**：`git diff` 删除行逐行核验后**全部为注释/文档行**（含 swap 错位 SAFETY 与
rcu 失真 SAFETY 两条删除）。
**读数缺口登记**：本批未读出需上姊妹册的代码缺口；`get_pte_value` 未做 `translate_in_pml4`
同款的 `root_paddr == 0` 早退，只影响零根入参语义、不构成 unsafe 前提缺失，按 §11.2 不顺手改。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `CARGO_TERM_COLOR=never bash
ci/audit.sh full` RC=0（未执行腿数 0、Lockbud 4/4、QEMU 2/2、`safety_content` 腿通过）·
`make test-host` **212 套 / 1651 passed / 0 failed** · `make test-kernel-host`
**3 套 / 1431 passed / 0 failed** · `make -f Makefile.ci ci-rustdoc` 双架构 exit 0。
侧车已 `--update-baseline` 重导（**335 处 / 94 键**）。
状态：[X]

### 批 C4・net + driver + egdf 占位改写与注释失真（㉗ ㉘ ㉙ ㉝ ㉞ ㉟ ㊳ ㊴）

描述：C1 侧车里 net/driver/egdf 面的占位理由改写（批 5f 补写 51 处，内容空泛 **14 处**），
并修同面 8 项失真。
方案：① 同批 5 方法；② 8 项失真：
㉗ `egdf/mod.rs` 的 `driver_as_mut` / `driver_as_ref` 两处 `# Safety` doc **引用不存在的函数名
`set_driver_data`**（真实写入面 = 注册入口 `egdf_register` / `egdf_register_with_ops` 的
`driver_data` 形参），且 doc 称「由驱动在 probe 阶段设置」与体内 SAFETY「由注册入口调用方传入」
不一致；两函数全仓零调用方（按 F9 消除方式处置，**不得加 allow(dead_code)**）；
㉘ `egdf/mod.rs` 的 `EGDFDevice` Send/Sync 上方 SAFETY 散文**截断**（首段在
「其它字段均为 Copy 类型或」处断句）+ 紧接第二条 SAFETY **重复**；
㉙ `net/init.rs` doc 称 `UninitCell` 而实装是 `static mut MaybeUninit`；
㉝ 三处 SAFETY 引用 `check_user_buf`，真名是 `userptr::validate_user_buf`（**引用他人注释前先 grep 确认**）；
㉞ `driver/input/keyboard.rs` 旧 SAFETY 称「单核内核」与 SMP 实装不符；
㉟ 三处 auto-trait impl（`driver/input/keyboard.rs` 的 `KeyboardDriver: Sync` /
`egdf/devtree.rs` 的 `EGDFNode: Send + Sync` / `net/init/state.rs` 的 `NetState: Sync`）——
`KeyboardDriver` 那条**已被批 5f 显式披露为「auto Sync 本可派生、只是显式化」**⇒
只裁定去留，不径行删（见 §1.5）；
㊳ `net/save.rs` 魔数 doc 与常量**双不符**：doc 写 `"ANXS"` / `0x584E414C`，
常量是 `0x584E_4153`（小端字节 `0x53 0x41 0x4E 0x58`）⇒ 字符串与 hex 都错——
**属冻结命名产物面，只改 doc 不改常量字节**（改字节即破坏盘上格式兼容）；
㊴ `driver/display/mod.rs` 的 DISPI 端口对（`VBE_DISPI_PORT_INDEX` / `VBE_DISPI_PORT_DATA`）
重复 IoPort safe 代理（收敛为单一代理，**不改端口时序**）。
③ net 面的「已校验」类假称若指向**真实代码缺口**，归姊妹册 R2，本册只改注释措辞使其不撒谎。
**结果**：C4 面（`privileged/{net,driver,egdf}`）**窄口径 14 处 → 0**、**宽口径 52 处 → 0**
（计划册原文约 48，现树实测 52），全树侧车 335 处 / 94 键 → **269 处 / 71 键**。8 项失真处置：
㉗ 经用户裁定**保留** `driver_as_mut` / `driver_as_ref`（全仓零调用方；privileged crate 的 pub API
不触发 dead_code）并订正 `# Safety` doc 为真实写入面（`egdf_register` / `egdf_register_with_ops`
的 `driver_data` 形参）；㉘ 把截断 + 重复的两条 SAFETY 合并为一条准确依据（裸指针 `driver_data`
所有权按 `driver_owned` 区分、`&'static mut dyn BlockDevice` 只经 `EGDF_DEVICES` 锁独占借用）；
㉙ doc 的 `UninitCell` 改为 `static mut MaybeUninit<SocketSet<'static>>`；㉝ 现树复核**无
`check_user_buf` 命中**（net 面 SAFETY 已统一引用 `userptr::validate_user_buf`），无需改；
㉞ keyboard 的「单核内核」改为「字段为 plain 数据 + `KEYBOARD_DEVICE` 的 `IrqSpinLock` 串行化
（持锁期间本核中断屏蔽）」；㉟ 经用户裁定三处 auto-trait impl **全保留**，注释写明「auto trait
本可派生 / 非类型必需（`IrqSpinLock<T>: Sync` 只要求 `T: Send`）」；㊳ doc 改为 `"SANX"` /
`0x584E_4153` 小端字节 `0x53 0x41 0x4E 0x58`，常量字节未动；㊴ 按用户裁定只订正注释，并把
DISPI 双代理收敛登记为姊妹册 **R11**（未开工）。另订正同面读出的注释失真：
`egdf/mod.rs::driver_from_obj` 两处把 `&mut DriverObject` / `&mut dyn Driver` 误称「只读访问」
（与 C3 的 rcu / mutex 同型）；`net/save.rs` 文件头把实装
`static NET_SNAPSHOT_LOCK: Mutex<NetSnapshot>` 写成旧述 `static mut NET_SNAPSHOT` / `NET_LOCK`。
**纯注释面判据**：`git diff` 删除行逐行核验后**全部为注释/文档行**（含 `egdf` 截断 SAFETY 与
`driver_from_obj` 失真 SAFETY 两条删除）。
**读数缺口登记**：本批未读出需上姊妹册的新代码缺口；㉝ 的现树复核结果与计划册登记不一致
（登记称三处引用 `check_user_buf`，现树零命中）⇒ 按 §1.5「施工时仍须再核一次」以现树为准，
不追改历史登记。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `CARGO_TERM_COLOR=never bash
ci/audit.sh full` RC=0（未执行腿数 0、Lockbud 4/4、QEMU 2/2、`safety_content` 腿通过）·
`make test-host` **212 套 / 1651 passed / 0 failed** · `make test-kernel-host`
**3 套 / 1431 passed / 0 failed** · `make -f Makefile.ci ci-rustdoc` 双架构 exit 0。
侧车已 `--update-baseline` 重导（**269 处 / 71 键**）。
状态：[X]

### 批 C5・kernel 其余面占位改写与注释失真（⑨ ㊷ 53 54 55 57）

描述：C1 侧车里 glue/string、klog、lib.rs、freg、proc、syscall、timer、privileged 根、
memory_allocator、ipc、sched、sgeg 面的占位理由改写（批 5g 补写 58 处），并修 6 项失真。
方案：① 同批 5 方法；② 6 项失真：
⑨ `functions/sgeg/storage/disk.rs` 的 `USER_DISK_INFO_SIZE = 76` 注释与实为 80 的结构不符
（**无安全后果**，但属常量注释失真；改注释或改常量须先核 ABI 单源，不得单侧改）；
㊷ `klog_init` doc 的初始化次序倒置（实装次序 = serial_init → register_defaults → KLOG_INIT.store）；
53 `user_proc.rs` doc 过称 header 校验范围；
54 `proc/scheduler.rs` 注释称「本函数开头 cli」而实际在调用方 `schedule()` 首行——
**下方已有一条就地订正注**⇒ 修法是改原句本身并删那条冗余注，不是再加一层；
55 `freg/domain.rs` doc 称 `spin::Mutex` 而实装是 `IrqSpinLock`——**本轮 grep 未命中
`spin::Mutex`**，须先复核该项是否仍成立（见 §1.5），不成立即撤销该项并回写 gate-health 册；
57 `glue/string.rs` 游离 SAFETY（不附着任何 unsafe 位点）+ **恰 12 处**测试模块窄占位
「调用方保证指针/类型有效 (详见上下文)」（已核实）；**同型占位亦在 `idt/table.rs` 的 `cli()`**
⇒ 归 C2 面，C1 的窄口径清单须覆盖全树而非只此文件。
③ **57 的测试模块占位**：测试面不在债册射程（`--lib` 不带 `--all-targets`），
但 C1 的内容面腿若扫测试模块则会计入 ⇒ 施工前须先裁定 C1 的扫描面是否含 `#[cfg(test)]`，
并把裁定写进侧车口径（**不得两侧不一致**）。
**结果**：C5 面（计划列出的 glue / klog / lib.rs / freg / proc / syscall / timer /
privileged 根 / memory_allocator / ipc / sched / sgeg）窄口径 **62 处 → 0**；施工时按
「内核其余面」口径一并吸收计划未列、但与 C2–C4 遗留同源的命中：pci **15**、dma **6**、
mm **2**、racy_cell **3**、net_socket **8**、smp **1**、ioport **1**、userptr **1**、
arch 遗留 **21**、idt 遗留 **9**、kernel_tests **1**，最终全树侧车 **窄 83 / 宽 186 → 0 / 0**
（`--update-baseline` 后 totals 0 处 / 0 键，该腿转硬零门禁）。
6 项失真：⑨ 按用户裁定**改常量**（`USER_DISK_INFO_SIZE` 76→80；内核与用户态两份
`UserDiskInfo` 实测均 80 字节，76 是孤立值，原判「无安全后果」不确 ——
`write_struct_to_user` 写满 80 而校验只 76，构成 4 字节越界写）；㊷
`klog_register_defaults` / `klog_init` doc 写明实际次序
`serial_init → register_defaults → KLOG_INIT.store`（原 doc 称「由 kernel 主入口调用」）；
53 `load_elf_from_memory` doc 改为「区间可读性由调用点保证，首部只判非空与最小长度」，
并删那条「批6 待裁」冗余注；54 原句改为「调用方 `schedule()` 首行 `interrupt_disable()`
关本核中断」并删下方就地订正注；55 **复核成立**（`domain.rs` 原句确写 `spin::Mutex`），
改原句为 `IrqSpinLock` 并删订正注；57 删 `glue/string.rs` 的游离 SAFETY 与 12 处测试
模块窄占位（`idt/table.rs` 的 `cli()` 占位已在 C2 改）。
**用户裁定的三处代码修复**（本批原定纯注释，经 AskUserQuestion 单独授权）：① ⑨ 常量
76→80；② 两处测试夹具（`proc/elf/mod.rs::test_elf_magic_validation`、
`tests/test_new_features.rs::test_elf_valid_minimal`）改为 `mem::zeroed::<Elf64Header>()`
+ u8 只读切片，消除 `[u8; N]` 强转 `&mut Elf64Header` 的对齐/独占 UB；③
`pci_rust_init` 的 `format!` `String` 改为 256 字节栈缓冲 + NUL 结尾，闭合
`klog_ffi_info` 的 C 串越界读。
**纯注释/删除核验**：`git diff` 非注释删除行仅上述 3 处授权代码修复 + `domain.rs`
一处 impl 行因注释重排而位移（代码字节不变）。本批未新增姊妹册项（R12/R13 已在批内
修复，未落册）。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `CARGO_TERM_COLOR=never
bash ci/audit.sh full` RC=0（未执行腿 0、审计腿 17/17、P-49 11/11、Lockbud 4/4、
QEMU 2/2）· `make test-host` **212 套 / 1651 passed / 0 failed** ·
`make test-kernel-host` **12 + 811 + 608 = 1431 / 0 failed** ·
`make -f Makefile.ci ci-rustdoc` 双架构 exit 0。侧车已 `--update-baseline`
重导（**0 处 / 0 键**）。
状态：[X]

### 批 C6・配置与工具自述、文档执法归属（① ② ③ ⑤ ⑦ 59）

描述：6 项**非源码注释**的失真：clippy 配置缺警示、编码规范假归属、设计册声称不存在的 deny 组、
工具重复面、配置级 warning、以及新发现的同型漂移 59。
方案：① ① `src/kernel/clippy.toml` 补 P-59 惰性配置警示（阈值不生效的原因）；
② ② `docs/explain/spec-engineering.md` 规则 8.3 把 `unwrap_used` 假归属给
`audit_safety_coverage`（该腿不判 unwrap），且 `unwrap_used` **全仓无执法点** ⇒
要么接执法点、要么把规范条文改为与现状相符（**二选一须问用户**，属决策灰色地带）；
③ ③ `docs/design/freg-design.md` 声称的 `indexing_slicing` / `panic` deny 组**不存在** ⇒
按 design 册口径（指向长期演化目标）改为「设计主张，尚未实装」或补实装（**须问用户**）；
④ ⑤ `tools/check_tcb.sh` 的 F1 预检与 functions crate 根 `#![forbid(unsafe_code)]` 重复 ⇒
按批 4 判据收敛（删重复面、留生态缺口面）；
⑤ ⑦ `src/user/clippy.toml` 的 `disallowed-types` 在 `wasmrun` 报 2 条配置级 warning ⇒
核清是成员不适用还是配置路径问题，**不得用 allow 压掉**（F9）；
⑥ **59（新登记）**：`ci/audit.sh` 的「改用 `--workspace` 覆盖七成员」注释 +
`host-tests/tests/ci_entrypoint_unified_test.rs` 两处「七个成员都是无 test harness 的裸机 crate」
——`src/user/Cargo.toml` 现为 **10 成员**，且「都无 test harness」对 `lib` **不成立**
（它有内联单测，须 `cd src/user/lib && cargo test`）。这是 ④ 的**同型漂移**：
批 6 只改了 AGENTS §2.3 的「七成员→十成员」，脚本注释与测试自述未同批改 ⇒
本项把三处口径一次对齐（**改测试自述须核该文件是否有针钉住这些字样**）。
**结果**：① `src/kernel/clippy.toml` 的 `missing-docs-in-crate-items` 上方补 P-59 注记
（与 `src/rust/clippy.toml` 孪生件同文）：该键只被 `clippy::missing_docs_in_private_items`
（restriction 组）读取，门槛从不启用该组 ⇒ 惰性配置，不构成 F8 执法点。
② 按用户裁定**改条文**（方案 A）：`spec-engineering.md` 规则 8.3 去掉对
`audit_safety_coverage.py` 的假归属，改为「**当前无机械执法点**，由 review 把关；
`audit_safety_coverage.py` 只判 privileged 的 SAFETY 注释覆盖，不判 unwrap，存量待专项治理」
（实测 `src/kernel` 328 处 `.unwrap()` + 307 处 `.expect()`，接执法点须先清存量，故不取）。
③ 按用户裁定标注**设计主张未实装**：`freg-design.md` 六契约第 3 条改为「目标契约，尚未实装」，
写明 `kernel-freg` leaf crate 落地时才写该 deny 组，当前 `src/kernel` 全仓无任何
`deny(clippy::…)` 组、现 freg 实装在 `privileged/freg`（crate 根只写 `#![deny(missing_docs)]`）。
④ ⑤ 按批 4 判据**整腿退役** `tools/check_tcb.sh`：F1 由 functions crate 根
`#![forbid(unsafe_code)]` 编译期硬执法（`-A` 压不住），无生态缺口面；`ci/audit.sh` 删除
步骤 0 并把 quick/full 档位自述改为 `0.5a–…`，接线形状锁的首步骤定位改 `step "0.5a/6`；
`scripts/requirements.sh` 项目工具清单去掉该脚本（8 类分类不变）；
`functions/mod.rs` 两处与 `sgeg/policy.rs` 一处自述改指 `forbid` + 步骤 0.5k 边界审计。
⑤ ⑦ 按用户裁定改表项形态：`src/user/clippy.toml` 的 `disallowed-types` 改为
`{ path = "spin::Once", allow-invalid = true }`（两条目），只静音 wasmrun 经 wasmi 传递依赖
spin 造成的「不可达」**配置级** warning，不改执法（实测 wasmrun 腿 0 warning）；同批把
D-7 丙 的引脚从「精确字符串命中」改为「取 `disallowed-types` 声明到首个 `]` 的文本」，
两形态都判两个类型（避免注释里提到类型名造成假绿）。
⑥ 59 三处口径一次对齐：`ci/audit.sh` 注释与 `ci_entrypoint_unified_test.rs` 两处改为
「该 workspace 的 bin 成员无 test harness + `lib` 内联单测另跑 `cd src/user/lib && cargo test`」，
不写死成员数（单源 = `src/user/Cargo.toml` 的 `[workspace].members`）；经用户授权同步
`AGENTS.md` §2.3「十成员」→「`--workspace` 覆盖全部成员（清单单源 = …）」（实测 11 个成员）。
**本批不适用「纯注释零删除行」判据**：面为非源码注释/配置/工具，含脚本删除与测试引脚改写。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）·
`CARGO_TERM_COLOR=never bash ci/audit.sh full` RC=0（审计腿 17/17、P-49 11/11、
Lockbud 4/4、QEMU 2/2、未执行腿数 0）· `make test-host` **212 套 / 1651 passed** ·
`make test-kernel-host` **3 套 / 1431 passed / 0 failed** ·
`make -f Makefile.ci ci-rustdoc` 双架构 exit 0。中途红两处并已修复：host-tests fmt 漂移
（`cargo fmt --manifest-path host-tests/Cargo.toml` 后复跑）；`scan_obsolete_naming`
扫 `git ls-files` 时仍见已删脚本（先暂存删除再复跑即绿）。
状态：[X]

### 批 C7・host-tests 定长字节窗口（㊵）

描述：批 5f 修 `socket_multicast_membership_test` 的定长字节窗口补注释后落进汉字中间 panic
（夹字符边界 + `.min(len)`），同批登记**同类窗口全仓 14 处 / 9 文件，修 1 登记 13**；
批 5g 复核 13 处均不在其改动面 ⇒ 13 处仍开放。
方案：① 逐处改为夹字符边界 + `.min(len)`（或改按字节窗口取 ASCII 子串），
**四条针不动**（`socket_multicast_membership_test` 的断言形态是契约锁）；
② 补窗口自测：构造「窗口恰落在多字节汉字中间」的输入，断言不 panic；
③ 本批属 host-tests 树，该树 clippy 腿已带 `-D clippy::undocumented_unsafe_blocks`
（批 5c 翻，**勿重复翻**）；④ 项数标签若有针须同批改钉。
**结果**：现树 grep 实测 **14 处**未夹字符边界的定长字节窗口（计划原文登记 13 处；
差异归因为 `socket_multicast_membership_test.rs` 的 2000 窗口一处未在原 13 处清单内，
原「修 1」的 3000 窗口已夹取），全部改为
`(start + N).min(len)` + `while !src.is_char_boundary(end) { end -= 1; }` 夹取：
td17 400 / td19 600·400·600·400·500 / td18 600·300 / p24 200 / virtio 1200 /
net_poll 400 / net_snapshot 5000 / socket_wait_queue 4000 / multicast 2000。
补 `fixed_byte_window_clamps_to_char_boundary` 自测（构造窗口末端落在三字节汉字 '中' 内部，
断言回退到字符边界并得到合法切片）；multicast 四条契约针的相对偏移未动。
四树 clippy 腿已带 `-D clippy::undocumented_unsafe_blocks`，本批未重复翻。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `CARGO_TERM_COLOR=never
bash ci/audit.sh full` RC=0（未执行腿数 0、Lockbud 4/4、QEMU 2/2、safety_content 腿通过）·
`make test-host` **212 套 / 1652 passed / 0 failed**（较 C6 的 1651 多 1＝新增窗口自测）·
`make test-kernel-host` **3 套 / 1431 passed / 0 failed** · `make -f Makefile.ci ci-rustdoc`
x86_64 与 aarch64 两腿各 exit 0。
状态：[X]

### 批 C8・收口

描述：本册状态字段全 `[X]`、`docs/plan/progress-active-tasks.md` 甲表行回填读数、
memory 更新、按路径提交推送。
方案：① C1 侧车窄/宽两口径读数归零后转硬零形态（与 `audit_safety_debt` 同构），
并复核塌缩守卫未被削弱；② 新文件先 `git add` 再跑 `scan_obsolete_naming`（扫 `git ls-files`）
与 `check_doc_paths`；③ 七门槛复跑取读数（判据 = 日志内 `*-RC=` 回显）；
④ `git commit --only --pathspec-from-file`，不夹带其余暂存态；
⑤ **复核 AGENTS 授权状态**：C1 若已取得授权并改了 §2.2 计数，须在 memory 记录该次授权已用掉。
状态：[]

---

## 四、验证门槛与硬约束

判据单源 = `AGENTS.md` §2.3（本册不复制清单，P-70 甲）。另加四条本册专属：

1. **禁写占位理由**：C2–C5 的每处理由必须指向**具体实装依据**（哪个函数、哪把锁、哪个硬件约束），
   且引用的内核符号名须 grep 核实存在。C1 的腿会机械拦占位短语。
2. **纯注释批验零删除行**：`git diff --numstat` 的删除列须为 0（C2–C5 的注释改写面）；
   有删除即说明动了代码，须归姊妹册或单独提问。
3. **不改代码**：本册只改注释/文档/配置/工具自述。读出的代码缺口按 §11.5 登记，
   归 [safety-gap-remediation.md](./safety-gap-remediation.md)，**不顺手修**。
4. **AGENTS.md 须授权**：C1 的前置见该条正文。批 4 与批 6 的两次授权均已用掉。

> **位置判据的已知陷阱**（批 5c–5g 实测，改写时同样适用）：
> SAFETY 写在 `assert_eq!(` / `check!(` / `return G {` **之前不覆盖**宏实参行或字段行的 unsafe，
> 须移进实参内；item 级 SAFETY 写在 `#[unsafe(no_mangle)]` 前只附到 item，体内块仍欠债；
> 语句级 `#[cfg(...)]` 夹在 SAFETY 与 `unsafe {` 之间**仍被接受**；
> rustfmt 把注释缩进归一到**语句级**（非下一行代码级）；
> F7/TD-22 是**逐行**判定，折行的纯英文标识符列表单独成行即违规（该腿早退 ⇒ 后续步骤全不跑）。
>
> 门槛运行期间不改它读取面的源文件；根目录裸 `cargo fmt` 会改写 vendored smoltcp ⇒
> 只对目标 crate 用 `--manifest-path`；共享工作树禁 stash/reset，负控制用 `cp` + `sha256`；
> `audit_safety_debt` 的对账输出**只打印前 15 行**（缺行 ≠ 未归零）。