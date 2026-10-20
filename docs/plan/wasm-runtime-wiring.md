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
| F13 | **表格段（id 4）在解析期即无法通过**：`element_type` 走 `take_value_type`，而 funcref 规范字节 `0x70` 不在 `ValueType::from_byte` 的接受集（只收 `0x7F/0x7E/0x7D/0x7C`）⇒ 任何含表格段的真实模块返 `BadValueType(0x70)`。这是 W-2 全函数化的**直接产物**——旧实现（`:230`）把它误记为 `InvalidVersion`，与版本头检查同名，故从未被识别为独立缺口 | [types.rs:36-44](../../src/kernel/functions/wasm/types.rs), [module.rs:325-339](../../src/kernel/functions/wasm/module.rs) |
| F14 | `read_leb128_u32` / `read_leb128_i32` 对末字节不带续行位的**超长效编码静默截断**（`[0xFF×4, 0x10]` 解为 `0x0FFFFFFF` 并接受，规范要求返 `Leb128Overflow`）；不 panic、不放大分配（截断方向是变小，仍被段界卡住），但属语义误读 | [leb128.rs:21-40](../../src/kernel/functions/wasm/leb128.rs)，另见台账 [P-42](./net-e2e-tcp-echo-flakiness.md) |

⇒ **"已实装"的真相**：能解析 11 个段、能跑不含块结构与浮点的整数指令子集；**不能**执行一个真实工具链产出的 wasm 模块（块结构 + `call_indirect` + 浮点缺失即挂），**不能**调用任何 WASI 宿主函数，且喂给它一段截断字节就能让内核 panic。

> **本表是 W-1 调研时点（W-2 施工前）的只读基线，不是当前状态**。W-2（本轮，§3 戊裁定的唯一施工项）已闭合 **F8**，并顺带把 F13 从"伪装成 `InvalidVersion` 的静默失败"显形为独立事实；F1–F7、F9–F12 的读数**仍然有效**（接线面、块结构、浮点、WASI 注入均按 §3 戊留给 W-3 起各轮）。F14 为 W-2 过程中新发现的既有解码器宽容度，已单列台账并按 §9.1 上报待裁。

## 2. 工程条目

- **W-1 调研回填与前提推翻（本册 §1）**：接线前把"已实装"改为实证状态。
  - 描述：核对 16 个文件的真实完成度、接线缝、约束面与既有立场。
  - 方案：已由 §10 调研 + 本轮逐条源码复核闭环。既有立场方面：`docs/explain/` 与 `docs/design/` **无一处 wasm**，历史设计文档只存在于 `docs/plan/archive/2026-07-20-wasm-wasi-integration-design.md`（Task1-8 全 `[x]`，但**通篇未规划 syscall / loader / 进程接线**）与 `future-roadmap.md` 的"✅ 已完成"标记——即历轮的"完成"指的是这批未含接线的任务，与本文档 §1 的事实不矛盾但极易误读。
  - 状态：[X]

- **W-2 解析器全函数化（安全前置门槛，不得后置于接线）**：把 `parse_wasm` 变成对任意输入都不 panic 的全函数。
  - 描述：F8 意味着"接线"这个动作本身会把一条**用户可触达的内核 panic 路径**打开——只要有一条 syscall 能递交字节码，任意畸形输入即 DoS。按 §4.2 不变式与 §11.1.1 长远视角，这条必须在任何入口开放**之前**闭合，而不是与接线同批。
  - 方案：`module.rs` 全部切片访问改边界安全形式（`.get(..)` / `checked_*`），越界一律返 `WasmError::*` 而非 panic；同时为本轮新暴露的失败形态补 `WasmError` 变体（改造前实数 **23** 变体、无 `Display`；本条原记"22"系漏计一名，已按 `git show HEAD` 逐名清点订正）。`WasmError → Errno` 映射**不在本条**——它随 syscall 入口落在 W-8。回归锁 = 对每个段构造截断/超长/零长三类畸形样本的内核内单测（W-9 的 harness 就位后即可跑）。
  - 状态：[X]（§3 戊裁定＝**本轮唯一施工项**，已闭合；§3 乙 的达标口径见下"两态分离现状"。**改造形态**：`module.rs` 436 → 1,046 行（非测试解析体止于 `:498`，其后 24 例回归锁）。手法是**先切段再解析**——`parse_wasm` 为每个段取子切片，段内一律走五条新边界安全原语（`take_byte` / `take_value_type` / `take_entry_count` / `take_slice` / `require_consumed`），于是 leb128 的 `bytes.len()` 边界**就是**段边界，跨段读取在类型上不可能；`take_entry_count` 以"每条目至少占 1 字节"的段内剩余字节数作**分配上界**（旧码可被一句 `0xFFFF_FFFF` 打到 `Vec::with_capacity` 失败）；`require_consumed` 要求段内容一次消费干净，比声明短或长都判畸形；两处重复的 init-expr 读写收口为 `skip_init_expr` / `read_init_expr`。**边界安全面实测**：非测试体 `.get(` 由 0 → 5 处、裸索引 `bytes[` 归零、`unwrap` / `expect` / `panic!` / `assert` 归零 ⇒ **F8 闭合**。**错误面**：`WasmError` 23 → **27** 变体（`SectionSizeMismatch` / `TooManyEntries { declared, available }` / `BadValueType(u8)` / `BadFuncTypeTag(u8)`，四名全部有构造点，零死变体）；非法值类型字节旧实现误记为与版本头同名的 `InvalidVersion`，现单列 `BadValueType`。**回归锁（W-9(b)）24 例**：全前缀遍历、单字节篡改遍历（8 探针 × 每个字节位）、每段截断样本、条目数爆炸（含表格段，证上界先于 `with_capacity`）、段声明尺寸超界、尺寸与内容不符、参数表嵌套条目数、非法值类型 / 导入种类 / 导出种类、init-expr 缺 `0x0B` 与非法指令、LEB 半终止与溢出、全零字节流、未知段 id 跳过、零长已知段拒绝 + 零长未知段跳过，另加一条**确定性 xorshift64 模糊**（20,000 轮：合法模块多字节突变 / 截断 / 追加 与纯随机字节各半）。wasm 目录内联测试数 **0 → 24**。**负控制两轮**：① `take_byte` 还原为 `bytes[*pos]` ⇒ 3 例转红；② 只在 `parse_global_section` 的 mutable 字节处还原裸索引 ⇒ 两条穷举扫描例**均仍绿、仅模糊例转红**，即第 24 例承重、且穷举扫描确有盲区。**两态分离现状（§3 乙）**：解析期一次结清"段边界 / 条目数 / 字节消费 / 类型字节 / init-expr 终结"五类结构面；**表索引 / 操作数栈 / 分支目标**三类属解释器运行期结构面，本轮 `interpreter.rs` 一行未动 ⇒ 归 W-3 / W-4；运行期 linear memory 的 `.get()` 检查未被移除（本条不触碰）。**范围纪律**：`leb128.rs` 一个字节未改（kani-harness 以 `#[path]` 直引它为被验证真源码），其超长效编码宽容度按 §11.5 立案 **P-42** 待裁；本轮新显形的表格段 funcref `0x70` 缺口记为 **F13** 并归 W-4 的类型建模。**未达成项**：§4 判据 3（真实工具链产物跑通）仍未达成，其阻塞在 W-3/W-4/W-5 而非解析面；F1（内核侧零调用者）不变——本轮**未**开放任何入口，全函数化是接线前的门槛而非接线本身。）
  - 详情（六门槛全量复跑读数；本轮改动源码，故六条全部重跑，不复用上轮数）：① `./ci/build.sh all` **`Passed: 6 / Failed: 0`**，构建日志内 rustc `warning:` 行 = **0**（仅余 `cargo` 的 future-incompat **包级**提示 `core`/`sha2`，非本 crate 代码告警）；②+③ `bash ci/audit.sh full` **EXIT=0**——clippy pedantic(lib) / `kernel_test` 维 / `host-test` 维三维通过、`rustfmt --check` 四 crate 全通过、全部 `audit_*.py` 通过，其中 `audit_unwired_pub_fn.py --strict` 实测 **CRITICAL=0 / HIGH=0 / WARN=0 / INFO=433**（扫描 600 文件 / 5,768 个 `pub fn`；B-6 台账 452 项、命中 433，未命中偏差仍 19 条，本轮零新增 ⇒ §4 判据 4 未被触碰）；④ `make test-host` **EXIT=0**，140 套合计 **1,070 passed / 0 failed**；⑤ 门槛 3 内的 `7/7 QEMU 真实启动测试` **2/2 通过**（门禁模式 `FAIL_OK=0`，非容错模式）；⑥ `make test-kernel-host` **EXIT=0**，**1,013 passed / 0 failed**（上轮 1,012 → 本轮 1,013，增量即第 24 例模糊锁）。**过程记录一条**：首次跑门槛 3 时在 `2c rustfmt` 处即 EXIT=1 中断（手写测试体有两处格式漂移），而 `ci/audit.sh` 在 2c **不再执行其后任何步骤**——含 QEMU 腿——即一次"看起来跑完"的长时可被一个纯格式问题整批作废；已 `cargo fmt` 修正并在最终形态上重跑全部六条。**另附带发现**：门槛 6 的 `--release` 维不可编译（lockdep 的 `#[cfg(test)]` 未随被测代码同掩），已按 §11.5 立案 **P-43**，本轮未动 `lockdep.rs`。

- **W-3 结构化控制流**：block / loop / if 的标签栈与 `br*` 的正确 unwind。
  - 描述：F5 使分支回溯落到调用帧级，真实模块（几乎所有 `wat` 编译产物都含块结构）必然得到错误结果或直接 trap。这不是"新特性"而是**修正现有实现的语义错误**。
  - 方案：引入 per-frame block 标签栈（记录 block 起点 pc 与 arity），`br/br_if/br_table` 改为弹标签而非弹调用帧；`Block`/`Loop` 的块类型字节（含 `0x40`）必须消费而非回落到 opcode 解释；`If`/`Else`/`End` 与已有 `execute_if` 对齐。配 host 单测覆盖"嵌套 block + 跨层 br + loop 早退"。
  - 状态：[ ]（依 W-2（**已闭** ⇒ 本条解锁，但 §3 戊 裁定本轮不推，留待第二轮）；block 类型字节的消费缺口须在 W-2 的段边界之上做，否则标签栈本身不可信）

- **W-4 `call_indirect` 与缺失 opcode**：表调用 + i64 / 位运算族收口。
  - 描述：F4 的 `Unreachable` 使函数指针表调用整体不可用；types.rs 已声明但主循环不处理（落 `_ =>` trap）的有 I64 比较 / `I64Eqz` / `I64Load*` / `I64Store8/16/32` / `I32Rotl` / `I32Rotr` / `I32WrapI64` / `I64Extend*`。
  - 方案：`call_indirect` 需接 table 段并做**类型校验**（索引越界 → `Trap`，类型不符 → `Trap`，二者错误码不同）；opcode 族按已声明的语义补齐并逐个配单测。**另含 F13 的收口**：`TableType.element_type` 现在的类型是 `ValueType`，而 funcref 不是值类型 ⇒ 本条要么给 `ValueType` 增 `FuncRef` 变体、要么引入独立的 `RefType`/`ElementType` 枚举，并让 `parse_table_section` 按段语义接受 `0x70`；这是类型建模问题而非边界问题，W-2 故意不在此处放宽（放宽即掩盖）。**浮点（F6）已裁不纳入**（§3 甲：遇 F32/F64 opcode 明确返 `Unsupported`）。
  - 状态：[ ]（依 W-3；浮点范围已裁＝不纳入（§3 甲）；F13 的 funcref 类型建模归本条）

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
  - 状态：[ ]（依 W-2 起，与实装同批推进。**已部分推进**：(b) 的畸形输入矩阵随 W-2 落地——[module.rs](../../src/kernel/functions/wasm/module.rs) 的 `#[cfg(test)] mod tests` 计 23 例，wasm 目录内联测试数由 **0 → 23**（F9 的"零内联测试"对该文件已失效），并含全前缀 / 全单字节突变两条穷举式全函数性证明；(a) 的端到端执行用例（需 W-3/W-4 的块结构与表调用先就位）、(c) 的 syscall→dispatch→wasm 集成链（需 W-8）均未开工；(d) 本轮无 bug 修复项）

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
