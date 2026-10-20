# `functions::wasm` 接线工程（P-33，用户裁定实现深度 = 相对完整）

> **定位**：把 `src/kernel/functions/wasm/`（4,575 行 / 16 文件）从"内核侧零调用者"接成生产可达路径。用户已裁定**单开工程接线实装**，实现深度按 `AGENTS.md` §11.3 择定 **相对完整**——即路径 / 边界 / 错误分支完整闭合、具备单元测试覆盖、可长期稳定运行；**不**追工业级功能最全，**不**对齐 WASI 全部语义。
>
> **来源**：`docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-33（AGENTS 目录表维护期的全仓引用扫描附带发现）。
>
> **本文档的首要结论**：立项台账把本子系统记为"**已实装**未接线"，该前提经源码核验**不成立**（见 W-1）。因此本工程的真实范围不是"加一条 syscall 臂"，而是"先补全到可安全执行，再接线"。W-1 之后的条目顺序即依赖顺序，不可打乱。
>
> **关系**：编号空间依 DECISION-037（0–299 Linux 号、500+ `EG_*`）；单一测试 harness 依 DECISION-080（真源码 `#[cfg(test)]` + `host-tests` 集成）；分层判据依 §4.1（wasm 全树 `#![deny(unsafe_code)]`，属 functions 策略层，接线不得把 unsafe 引入 functions）；`audit_functions_boundary.py` 的 `ALLOWED_INTER_DEPS` 白名单目前**无任何 wasm 条目**，W-5 起会触碰（详见 §4 验收判据第 5 条）。

## 1. 现状事实（源码实证，非推断）

| # | 事实 | 位置 |
| --- | --- | --- |
| F1 | 全内核唯一引用是 `pub mod wasm;` 本身；syscall 分派表无 wasm 臂、`kernel_init` 无 wasm 接线 | [functions/mod.rs:99](../../src/kernel/functions/mod.rs) |
| F2 | 主路径形状 = `parse_wasm → instantiate → auto_register_wasi → call("_start")`，**无**"跑完整模块"的封装函数 | [interpreter.rs:1215](../../src/kernel/functions/wasm/interpreter.rs) |
| F3 | `start_section` 被解析但全仓零消费者 ⇒ start 段从不执行 | [module.rs:95](../../src/kernel/functions/wasm/module.rs) |
| F4 | `CallIndirect` 直接 `return Err(WasmError::Unreachable)` | [interpreter.rs:492-494](../../src/kernel/functions/wasm/interpreter.rs) |
| F5 | `Block` / `Loop` 的处理只有 `advance_pc(1)`——无 block 标签栈，`br/br_if/br_table` 的 `unwind_to` 弹的是**调用帧**而非 block 标签 | interpreter.rs:468-473, :809-815 |
| F6 | 主循环内 `F32` / `F64` 出现次数 = **0**（浮点 opcode 全缺） | `grep -c 'F32\|F64' interpreter.rs` = 0 |
| F7 | `wasi_function_table()`（36 项）**零消费者**；`register_named_host_function` 仅出现在一句文档注释里，无人调用 ⇒ WASI 函数目前**无法被注入**任何实例 | [wasi/mod.rs:70](../../src/kernel/functions/wasm/wasi/mod.rs), interpreter.rs:240 |
| F8 | 解析器**全无边界安全访问**：`module.rs` 内 `.get(` 出现 0 次，而 `:64` 为 `bytes[pos]` 直接索引 ⇒ 截断 / 恶意字节码在**内核态 panic** | [module.rs:64](../../src/kernel/functions/wasm/module.rs) |
| F9 | `wasm/` 目录 `cfg(test)` 计数 = 0；外部只有 `host-tests/tests/wasi_test.rs`（10 例，只测 fd 表与 errno 常量，**不测执行**） | `functions/wasm/`, `host-tests/tests/wasi_test.rs` |
| F10 | 编号空间：`EG_*` 现最高 898；500–729 区间仅 720–722 被 `SYS_FB_*` 占用；`EG_WASM` 全仓**无预留** | [privileged/syscall/types.rs:400-402](../../src/kernel/privileged/syscall/types.rs) |
| F11 | exec 路径无按格式派发抽象：`functions/proc/exec.rs` 读文件后**硬调** `prepare_elf_image`；ELF magic 只在 `privileged/proc/elf/verify.rs` 硬校验；wasm magic `\0asm` 在内核侧零引用 | [exec.rs:39-101](../../src/kernel/functions/proc/exec.rs), [privileged/proc/elf/verify.rs:36](../../src/kernel/privileged/proc/elf/verify.rs) |
| F12 | wasm 现在能过 §5 F9 的原因：`ci/audit.sh:245` 以 `--strict` 跑 `audit_unwired_pub_fn.py`，而 wasm 有 5 项登记在 `docs/plan/syscall-followup.md` B-6 台账、其余 `pub fn` 被脚本的**词元计数**（同名即算引用）判为有调用者 ⇒ 接线中新产生的零引用 `pub fn` 必须同批接上消费者或入台账，否则门槛变红 | `ci/audit.sh`, `scripts/audit_unwired_pub_fn.py` |

⇒ **"已实装"的真相**：能解析 11 个段、能跑不含块结构与浮点的整数指令子集；**不能**执行一个真实工具链产出的 wasm 模块（块结构 + `call_indirect` + 浮点缺失即挂），**不能**调用任何 WASI 宿主函数，且喂给它一段截断字节就能让内核 panic。

## 2. 工程条目

- **W-1 调研回填与前提推翻（本册 §1）**：接线前把"已实装"改为实证状态。
  - 描述：核对 16 个文件的真实完成度、接线缝、约束面与既有立场。
  - 方案：已由 §10 调研 + 本轮逐条源码复核闭环。既有立场方面：`docs/explain/` 与 `docs/design/` **无一处 wasm**，历史设计文档只存在于 `docs/plan/archive/2026-07-20-wasm-wasi-integration-design.md`（Task1-8 全 `[x]`，但**通篇未规划 syscall / loader / 进程接线**）与 `future-roadmap.md` 的"✅ 已完成"标记——即历轮的"完成"指的是这批未含接线的任务，与本文档 §1 的事实不矛盾但极易误读。
  - 状态：[X]

- **W-2 解析器全函数化（安全前置门槛，不得后置于接线）**：把 `parse_wasm` 变成对任意输入都不 panic 的全函数。
  - 描述：F8 意味着"接线"这个动作本身会把一条**用户可触达的内核 panic 路径**打开——只要有一条 syscall 能递交字节码，任意畸形输入即 DoS。按 §4.2 不变式与 §11.1.1 长远视角，这条必须在任何入口开放**之前**闭合，而不是与接线同批。
  - 方案：`module.rs` 全部切片访问改边界安全形式（`.get(..)` / `checked_*`），越界一律返 `WasmError::*` 而非 panic；同时补 `wasm/mod.rs` 层的 `WasmError → Errno` 映射所需错误变体（现 22 变体且无 `Display`）。回归锁 = 对每个段构造截断/超长/零长三类畸形样本的内核内单测（W-9 的 harness 就位后即可跑）。
  - 状态：[ ]（§3 戊裁定＝**本轮唯一施工项**；达标定义由 §3 乙钉死：解析期一次结清结构面并返 `Err`，运行期 linear memory 访问的 `.get()` 边界检查**不得**为快路径移除）

- **W-3 结构化控制流**：block / loop / if 的标签栈与 `br*` 的正确 unwind。
  - 描述：F5 使分支回溯落到调用帧级，真实模块（几乎所有 `wat` 编译产物都含块结构）必然得到错误结果或直接 trap。这不是"新特性"而是**修正现有实现的语义错误**。
  - 方案：引入 per-frame block 标签栈（记录 block 起点 pc 与 arity），`br/br_if/br_table` 改为弹标签而非弹调用帧；`Block`/`Loop` 的块类型字节（含 `0x40`）必须消费而非回落到 opcode 解释；`If`/`Else`/`End` 与已有 `execute_if` 对齐。配 host 单测覆盖"嵌套 block + 跨层 br + loop 早退"。
  - 状态：[ ]（依 W-2）

- **W-4 `call_indirect` 与缺失 opcode**：表调用 + i64 / 位运算族收口。
  - 描述：F4 的 `Unreachable` 使函数指针表调用整体不可用；types.rs 已声明但主循环不处理（落 `_ =>` trap）的有 I64 比较 / `I64Eqz` / `I64Load*` / `I64Store8/16/32` / `I32Rotl` / `I32Rotr` / `I32WrapI64` / `I64Extend*`。
  - 方案：`call_indirect` 需接 table 段并做**类型校验**（索引越界 → `Trap`，类型不符 → `Trap`，二者错误码不同）；opcode 族按已声明的语义补齐并逐个配单测。**浮点（F6）是否纳入本工程需用户裁定**（§3 甲/乙），"相对完整"的定义不自动蕴含 f32/f64 全套 IEEE 语义。
  - 状态：[ ]（依 W-3；浮点范围待 §3 裁定）

- **W-5 WASI 注入通路打通**：`wasi_function_table` → 实例，含上下文归属与参数序。
  - 描述：F7 是当前最硬的功能断层——36 个 WASI 函数写好了却没有任何机制把它们挂进解释器，`auto_register_wasi` 只从 `named_host_functions` 查表而该表永不被填。连带问题：host 调用的**实参弹出顺序**与注册序反（先弹即末参），`WasiContext{fd_table,args,env}` 无落脚处。
  - 方案：加适配层把 `(name, WasiFunc)` 包成解释器可调用的 host 闭包；`WasiContext` 与实例同生命周期归属（倾向新建 `WasmInstance{interp, ctx}` 而非让 `Interpreter` 直接持 ctx，避免 runtime 反向依赖 WASI）；修正参数序并为此写**能捕获该 bug 的**单测（当前 `wasi_test.rs` 抓不到，因为它不执行）。依赖 `functions::{fs,net,...}` 的部分触 `ALLOWED_INTER_DEPS`（§4 第 5 条）。
  - 状态：[ ]（依 W-4）

- **W-6 fd 表与 stdio / preopen**：WASI 侧的文件描述符语义接进内核 fd 表。
  - 描述：`wasi/fd_table.rs:88` 的 `new()` 不预置 fd 0/1/2，现有单测反而**断言** `get(0/1/2)` 皆 err（`wasi_test.rs:37-41`）——即"无 stdin/stdout/preopen"是被测试固化的当前行为，接线时要一并改判据。P-33 裁定里的"WASI fd 表接进 UNKFS"落在此条。
  - 方案：fd 表初值挂 stdio 与 preopen 根；`fd_write` / `path_open` 等改走真 VFS（这些实现已存在并调 `fs`/`net`，只是无可达路径）；回归锁覆盖"模块写 stdout 得到字节"。
  - 状态：[ ]（依 W-5）

- **W-7 装载格式派发抽象**：按 magic 选 loader（ELF vs `\0asm`）。
  - 描述：F11 的硬编码使"能装载 wasm"必须先动 exec 路径。落点选择（在 functions 侧做派发 vs 在 privileged 侧扩 loader 钩子）影响 TCB 边界，属 §9.1 灰色地带。
  - 方案：最小派发 = `exec.rs` 读字节后按前 4 字节判格式，ELF 走现路、wasm 走新 `wasm::load_into_current_process`。形状已由 §3 丙裁定为**一句按 magic 的 match**，不抽 `ImageLoader`  trait。**注意**：动 exec/进程替换即涉调度与地址空间关键路径，施工前按 §11.3 再确认一次边界。
  - 状态：[ ]（依 W-2；形状已定（§3 丙），本轮不施工（§3 戊＝只做 W-2））

- **W-8 syscall 入口与错误映射**：`EG_WASM_*` 编号 + dispatch 臂 + `WasmError → Errno`。
  - 描述：F1/F10——编号未预留、`dispatch.rs` 无臂、`WasmError` 22 变体无到 `Errno` 的转换（用户态拿不到可判别错误）。
  - 方案：按 §3 丁裁定，在 `privileged/syscall/types.rs` 新增段注释「766-779: WASM 运行时」并落 `EG_WASM_LOAD=766` / `EG_WASM_RUN=767` / `EG_WASM_GET_INFO=768` / `EG_WASM_UNLOAD=769`（实测 766–779 全空、不在任何已声明段内；**不占 0–299 Linux 号位**——Linux 无对应语义，占号既无兼容收益又破坏"0–299 == Linux"不变式）；`functions/syscall/dispatch.rs` 加臂；补 `impl From<WasmError> for Errno`；`abi` 侧同步 `src/rust/edgine-abi` 常量单源。**R2 审计会自动校验"编号已分发"**，故编号与分派必须同批落地，不能先加常量。
  - 状态：[ ]（依 W-2；号段已定（§3 丁），本轮不施工（§3 戊＝只做 W-2））

- **W-9 测试门槛**：内核内 `mod tests` + `host-tests` 集成 + 回归锁。
  - 描述：F9——wasm 目录零内联测试，唯一外部测试不执行模块。按 `docs/explain/spec-engineering.md` 的"公共 API 须单测 / 跨模块接口须集成测试"，"相对完整"路径下这条是硬门槛而非可选项。
  - 方案：(a) `functions/wasm/` 各文件补 `#[cfg(test)]`，端到端用例 = 一段手写最小 wasm 字节序列（含 block / br / 内存读写 / host 调用）跑通并断言结果；(b) 畸形输入矩阵（W-2 的截断/越界/非法 opcode）断言返错不 panic；(c) `host-tests` 加集成用例覆盖 syscall 号→dispatch→wasm 的跨模块链；(d) 每条 bug 修复附 issue 引用（§8）。
  - 状态：[ ]（依 W-2 起，与实装同批推进）

- **W-10 文档归位**：explain 说明 + design 立场。
  - 描述：`docs/explain/` 与 `docs/design/` 无一处 wasm（§1 既有立场项），接线后属"落地后稳定内容迁入 explain"（§6）；同时 wasm 的能力边界（支持哪些 opcode / WASI 子集、不做什么）值得写成设计立场文档，避免下一轮再次误读成"已实装"。
  - 方案：`docs/explain/` 增 wasm 运行时说明（自由描述风格，禁结构化字段）；`docs/design/wasm-runtime-design.md` 写能力边界与开放议题；同步 `AGENTS.md` §1（若新增顶层模块需另行授权）。
  - 状态：[ ]（依 W-9）

## 3. 裁定结果（用户 2026-10-20 裁毕，施工按此执行）

| 代号 | 问题 | 裁定 | 依据与代价 |
| --- | --- | --- | --- |
| 甲 | 浮点（F6）是否纳入 | **不纳入**：遇 F32/F64 opcode 明确返 `Unsupported`，并写进 design 的能力边界 | F1 使 functions 层 0 unsafe ⇒ 不能用 SSE/NEON 内联；纯 Rust 软浮点估 +600–1,200 行且需 libm 级用例背书；把 FP 原语下沉 privileged 则触 FREG 冻结立场与上下文切换的 FP 状态保存。"相对完整"的本项目定义不自动蕴含浮点 |
| 乙 | 敌意字节码的隔离立场 | **两态分离**：结构面（表索引 / 操作数栈 / 分支目标 / 段边界）在解析期一次校验，不合规返 `Err`；运行期每次线性内存访问**仍**走 `.get()` 边界检查 | 运行期检查是 wasm 规范对 linear memory 的固要求，与"是否信任字节码"无关 ⇒ **不得**为快路径移除。W-2 只落解析侧全函数化；此项是 W-2 的达标定义 |
| 丙 | loader 派发形状 | **`exec.rs` 内单点按 magic 的 match**，不抽 `ImageLoader` trait | 格式集短期就 ELF / wasm 两种；trait 属"为将来预留扩展点"（与 §11.3 简约细则相斥），且 trait / inherent 双入口已有 P-18 零消费者先例 |
| 丁 | wasm 的运行入口 | **`EG_WASM_*` 走私有段，新开「766-779: WASM 运行时」功能段**；MVP 取 766 LOAD / 767 RUN / 768 GET_INFO / 769 UNLOAD | 一个号段选择，不是一条 ABI 主张：Linux **没有**"进程内加载并执行 wasm"的 syscall ⇒ 占用 0–299 的兼容收益恰为零，却会破坏"0–299 == Linux 号"这条承重不变式（`docs/explain/linux-compat-philosophy.md` §一）。wasm 侧的生态兼容载荷在 **WASI 保真度**（W-5 / W-6），与号位无关。766–779 实测全空且不在 `privileged/syscall/types.rs` 任何已声明段内。**订正**：本项首次裁定（735–746）基于我只查常量占用、未读源码段表注释的丕完整读数，该区间横穿 730-739 固件加载 / 740-745 POSIX Timer / 746-747 熵源三条已声明段，故作废重裁 |
| 戊 | 本工程的分轮边界 | **本轮只推 W-2**（解析器全函数化 + 边界安全访问 + 畸形输入回归矩阵） | §1 实测的 DoS 面（截断字节码即内核 panic）是唯一的现网风险，先闭合它；W-3..W-8 待第二轮起，且届时受本表甲/乙两项约束 |

## 4. 验收判据

1. `AGENTS.md` §2.3 六条门槛全绿（双架构 0w0e / clippy+fmt / 核心审计 / host-tests / QEMU / 内核 host 单测），且**每轮**复跑。
2. 不存在任何用户可触达的 wasm panic：W-9(b) 的畸形输入矩阵全部返 `Err` 且作为回归锁常驻。
3. 一段真实工具链产出的 wasm（非手写）能在内核里跑通并产出可断言的结果——这是"接线完成"的唯一实证判据，不接受"单元里自造字节码通过"作为替代。
4. 零 `#[allow(dead_code)]` / `#[allow(unused)]`（§5 F9 零容忍）；新增 `pub fn` / `pub mod` 全部有内核侧消费者，或同批登记进 `docs/plan/syscall-followup.md` 台账（F12）。
5. `audit_functions_boundary.py` 保持 `--strict-medium` 语义下的通过：W-5/W-6 引入 `functions::wasm → functions::{fs,net,proc,syscall}` 的跨模块依赖时，需按该脚本 `ALLOWED_INTER_DEPS`（:412-509）的既有理由**申请新增条目**而非绕过审计；新增条目属 F/S 分层判断，须先报告。
6. `audit_coupling.py`（F3）不因 loader 抽象或 `WasmInstance` 引入新循环依赖。
7. TCB 占比不上升（wasm 全树留在 functions 层，任何"因为方便而搬到 privileged"的冲动都违反 §4.1 归属决策树）。
