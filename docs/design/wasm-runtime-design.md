# WASM 运行时设计立场

> 本文回答 Edgine 的 **WASM**（WebAssembly）子系统"要演化成什么、为什么这么选"：能力边界的取舍根基、隔离立场、载体形态、与业界方案的对比、尚未闭合的开放议题。它不是现状说明（那是 [explain-wasm-runtime.md](../explain/explain-wasm-runtime.md)），也不是可执行任务清单（那是 [wasm-runtime-wiring.md](../plan/archive/wasm-runtime-wiring.md)，本文引用的 §3 甲/乙/丙/丁裁定与 §4 验收判据均出自该册）。

## 一、定位：为什么是内核内 safe 解释器

Edgine 的 wasm 运行时是一个落在 **functions** 层（100% safe Rust）的字节码**解释器**，而非 JIT（Just-In-Time，即时编译）或 AOT（Ahead-Of-Time，预编译）引擎。这不是性能上的权宜，而是框内核（Framekernel）架构约束的必然产物：

- **F1（functions 层 0 unsafe）排除了 JIT**。生成机器码需要可执行内存映射、需要把字节码翻译结果写进指令缓存——这些在 Rust 里都是 unsafe 操作，按归属决策树（见 [explain-framekernel.md](../explain/explain-framekernel.md)）必须落 privileged。把 JIT 放进 TCB（Trusted Computing Base，可信计算基）等于让一个庞大的代码生成器成为可信基的一部分，与 Minimalism 准则相悖。解释执行把"执行字节码"全程留在 safe 子树，TCB 不因 wasm 而增大。
- **浮点指令需要硬件寄存器状态**。SSE/NEON 内联是 unsafe，且涉及上下文切换时的 FP 状态保存——这触碰 FREG 冻结立场（见 [freg-design.md](./freg-design.md)）。纯 Rust 软浮点可行但代价显著（见 §二）。

与"用户态 wasm runtime + 微内核服务"路线相比，内核内解释器的取舍是：wasm 进程直接复用内核既有 POSIX 服务（VFS / 网络 / 进程），**WASI**（WebAssembly System Interface）适配层是一层薄翻译而非跨地址空间 IPC。这继承了框内核"单地址空间 + 函数调用替代 IPC"的性能立场，代价是解释器本身成为内核内一个需严格限界的执行体（gas / 深度 / 内存上限，见 §三）。

设计立场一句话：**wasm 是内核内一个被严格限界的 safe 解释执行体，能力边界由 TCB 最小化与 FREG 冻结推导，而非由"对齐业界最全功能"推导**。

## 二、能力边界：支持什么、排除什么、为什么

"相对完整"在本项目的定义是**代码逻辑稳定且相对可靠完备**（路径/边界/错误分支闭合、有单测、可长期稳定运行），**不**自动蕴含"工业级功能最全"或"对齐 WASM 全部规范"。能力边界据此划定：

**支持**：WASM 1.0 整数子集——值类型 `i32`/`i64`，控制流（block/loop/if/br*/end/return）、函数调用与 `call_indirect` 表调用、线性内存读写、全局/局部变量。WASI snapshot_preview1 的 38 个 host 函数（8 组，清单见 [explain-wasm-runtime.md](../explain/explain-wasm-runtime.md) 的 WASI 适配层一节）。

**明确排除**（每条返 `Unsupported` 而非静默忽略，使其成为可判别契约）：

| 排除项 | 为什么排除 | 何时需扩展 |
|---|---|---|
| 浮点（`f32`/`f64` 及浮点 opcode 族） | §3 甲裁定：F1 禁内联 SSE/NEON；软浮点估 +600–1,200 行且需 libm 级用例背书；下沉 privileged 触 FREG 冻结与上下文切换 FP 状态保存 | 当载荷生态确需浮点且用户裁定承担软浮点成本或解冻 FREG 时 |
| SIMD（`v128`） | 同浮点，需向量寄存器状态与更宽的上下文切换面 | 浮点立场松动后一并评估 |
| 引用类型（`funcref`/`externref`） | 需宿主对象模型与 GC 语义，超出整数子集 MVP | component model / 宿主对象互操作需求出现时 |
| 原子与线程（atomics） | wasm 线程模型与内核调度/共享内存耦合，需先定线程载体形态 | 多实例与共享线性内存语义裁定后 |
| bulk-memory / sign-ext / saturating-trunc 等扩展 opcode 族 | 非 1.0 核心，按需补；当前未进 opcode 枚举的字节返 `UnknownOpcode` | 真实工具链载荷产出这些 opcode 时按缺口补 |

浮点排除的落点是显式的：[interpreter.rs:491-493](../../src/kernel/functions/wasm/interpreter.rs#L491-L493) 的 `is_float_opcode` 把浮点族字节判为 `Unsupported`，与"未知指令"区分。这一区分的价值在于：用户态拿到 `ENOSYS`（未实现的合法结构）而非 `ENOEXEC`（畸形镜像），能据此判断"模块合法但本运行时不支持"，而非误以为字节码损坏。

**WASI 取 preview1 而非 preview2**：preview1 是当前工具链生态覆盖最广的快照，38 函数子集已能跑通 stdio/文件/时钟/参数环境/socket 的常见载荷。preview2 与 component model 是开放议题（见 §六），不在当前边界内——扩展它的判据是生态载荷的实际需求，而非"版本号更新"。

## 三、隔离立场：两态分离

§3 乙裁定的隔离立场是**两态分离**，这是 wasm 安全模型的核心，不可为性能合并：

- **结构面在解析期一次校验**：表索引、操作数栈深度、分支目标、段边界等结构性约束在 `parse_wasm` 阶段结清，不合规返 `Err`。解析器因此必须是**全函数**（total function）——任何字节输入都返回 `Ok` 或 `Err`，不存在用户可触达的 panic。这是 W-2 的达标定义：改造前截断字节码可在内核态触发 index-out-of-bounds panic 与巨额分配失败，是一条 DoS 路径；改造后四道边界闸（子切片即段边界 / 边界安全取字节 / 条目数上界 / 消费收尾对账）把它闭合。
- **运行期每次线性内存访问仍走边界检查**：linear memory 的 `.get()` 边界检查是 WASM 规范的固要求，与"是否信任字节码"**无关**。即便模块已通过解析期校验，运行期访问仍可能越界（动态计算的地址），故**不得**为快路径移除。

两态之外，**gas 计量**是运行期 DoS 的第三道防线：每执行一条指令计量一次，超预算即 `GasExhausted`（映射 `ETIME`）。它约束的是"合法但无限循环"的模块——解析期校验拦不住语义上的死循环，gas 能。

设计立场：**解析期校验消除结构畸形，运行期边界检查消除内存越界，gas 消除资源耗尽；三者各管一段，任一不可被其余替代**。

## 四、载体形态：进程级 + 单点派发

**进程级载体**（§3 丙/丁的施工前提）：wasm 模块经 `execve` 魔数识别后，装载的不是模块本体而是一个用户态入口桩，桩经 `EG_WASM_*` 回环驱动内核侧解释器。这样 wasm 进程仍是**普通可调度用户进程**——可被调度器抢占、可被信号中断、退出走标准 `proc_exit`。被放弃的替代是"exec 线程在内核态同步跑完解释器"：那会让解释器循环常驻内核态、阻塞调度，把一段不可信字节码的执行变成内核态的不可中断临界区。进程级载体的代价是多一次用户态/内核态往返，换来的是调度与信号语义的完整。

**单点 magic match，不抽 `ImageLoader` trait**（§3 丙）：格式集短期只有 ELF / wasm 两种，[exec.rs:106-119](../../src/kernel/functions/proc/exec.rs#L106-L119) 用一句按魔数的 match 分派。抽 trait 属"为将来可能用到预留扩展点"，与简约细则相斥；且本仓已有 P-18 的 trait/inherent 双入口零消费者先例——抽象一旦没有第二个真实消费者就是负债。何时需扩展：当第三种映像格式出现且确有共享分派逻辑时再抽，届时是重构而非预留。

**私有 syscall 段 766-779**（§3 丁）：`EG_WASM_LOAD/RUN/GET_INFO/UNLOAD`=766..769 不占 0–299 的 Linux 号位。Linux **没有**"进程内加载并执行 wasm"的 syscall，占号的兼容收益恰为零，却会破坏"0–299 == Linux 号"这条承重不变式（见 [linux-compat-philosophy.md](../explain/linux-compat-philosophy.md)）。wasm 侧的生态兼容载荷在 **WASI 保真度**，与号位无关——这是把"兼容"放在正确的层。

**one-shot gas 限界**：当前 `run_syscall` 一次把实例跑到结束（执行期实例从注册表条目取出，并发 RUN 返 `EBUSY`），无步进让出/恢复，标 SIMPLIFIED。扩展点是解释器的步预算——当需要长任务可中断/可恢复时，把"一次跑完"改为"按步预算分片 + 载体保存续跑状态"。这是开放议题（见 §六），不是当前缺陷。

## 五、与业界方案的取舍

| 方案 | 执行形态 | 隔离手段 | Edgine 的取舍 |
|---|---|---|---|
| Wasmtime / Wasmer | JIT/AOT（Cranelift/LLVM 等后端） | 用户态进程 + 软件故障隔离 | 性能高，但代码生成器需 unsafe + 可执行内存，TCB 大；Edgine 取 safe 解释器换 TCB 不增 |
| seL4 / 微内核 wasm 服务 | 解释或编译 | 地址空间隔离 + IPC | TCB 最小可形式化验证，但 IPC 性能损失；Edgine 单地址空间函数调用，无 IPC 开销 |
| Asterinas / RedLeaf / Theseus | 语言级隔离（Rust） | 类型系统 + 最小 unsafe | 同源立场；Edgine wasm 全树留 functions 是这一立场向字节码执行的延伸 |

Edgine 从这些方案吸纳的不是执行形态，而是两条经过验证的设计点：其一，**WASI 保真度是生态兼容的正确载体**（Wasmtime/Wasmer 的竞争力在 WASI 实现完整度，而非 syscall 号位）；其二，**语言级隔离可以把 unsafe 关进最小 TCB**（Asterinas 框内核论文的核心主张），wasm 解释器全程 safe 正是这一主张在"执行不可信字节码"场景下的落地。

独立推理的部分：业界 wasm runtime 几乎都走 JIT/AOT 因为它们运行在用户态、可自由使用 unsafe 与可执行内存；Edgine 的约束相反（内核内、functions 0 unsafe），故**解释执行不是落后，而是约束下的最优解**。当约束变化（如用户裁定解冻 FREG 或接受 privileged 内的代码生成器），形态才需重估。

## 六、开放议题

以下议题尚未闭合，记录于此作为后续裁定的输入；可执行分解一律归 [wasm-runtime-wiring.md](../plan/archive/wasm-runtime-wiring.md)，本文不持状态。

- **步进让出与恢复**：当前 one-shot gas 限界使长任务无法中途让出。扩展需解释器步预算 + 载体续跑状态，并与进程级载体的调度语义对齐。何时需扩展：出现需长时间运行且可被抢占的 wasm 载荷时。
- **浮点两难**：软浮点（+600–1,200 行 + libm 级用例）vs 下沉 privileged（触 FREG 冻结）vs 保持排除。三条路各有显著代价，需用户裁定方向，不擅自选。
- **WASI preview2 / component model**：preview1 子集之外的生态演进。判据是真实载荷需求，而非版本追新。
- **多实例与 fork 继承**：当前一进程至多一实例、fork 不继承。多实例需重估注册表容量与句柄语义；fork 继承需定线性内存与 WASI 上下文的复制语义。
- **真实工具链载荷验收**：§4 验收判据 3 把"一段真实工具链产出的 wasm（非手写）能在内核里跑通并产出可断言结果"定为接线完成的唯一实证，不接受"单元里自造字节码通过"作替代。这是能力边界是否够用的最终检验。
- **已知正确性边界（已修复，2026-11-13）**：`args_get`/`environ_get` 曾依赖线性内存零初始化来提供 NUL 终止符（写字符串字节后只前进 `len+1` 而不显式写 `0`），属隐式前提而非显式契约。已按设计立场「字符串终止应成为显式写入的契约」落地：两个宿主函数在写完字符串字节后显式写入 `0` 终止符；回归锁 `host-tests/tests/wasi_behavior_test.rs` 的 `args_environ_get_explicitly_write_nul` 以**预填 `0xFF`** 的线性内存证明该字节是显式写出而非零初始化副作用（负控制实测：摘掉显式写即红，left `[255]` vs right `[0]`）。W-9 发现记录见 [wasm-runtime-wiring.md](../plan/archive/wasm-runtime-wiring.md) 的 W-9 结案注。