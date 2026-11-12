# WASM 运行时

> Edgine 在内核 **functions** 层（100% safe Rust）内解释执行 **WASM**（WebAssembly）1.0 整数子集与 **WASI**（WebAssembly System Interface）snapshot_preview1 子集；wasm 全树 `#![deny(unsafe_code)]` + `#![deny(missing_docs)]`，不扩大 **TCB**（Trusted Computing Base，可信计算基）。进程级载体经 `execve` 魔数派发 + 用户态入口桩 + `EG_WASM_*` 私有 syscall 驱动，可被调度器调度、可被信号中断。能力边界的取舍立场见 [wasm-runtime-design.md](../design/wasm-runtime-design.md)，施工分解见 [wasm-runtime-wiring.md](../plan/wasm-runtime-wiring.md)。

## 定位与边界

wasm 运行时是内核内的一个**字节码解释器**，不是 JIT（Just-In-Time，即时编译）也不是 AOT（Ahead-Of-Time，预编译）：模块以字节码形式被逐条解释，没有机器码生成、没有可执行内存映射。它整体落在 [functions/wasm/](../../src/kernel/functions/wasm/mod.rs)，属框内核的去特权半（见 [explain-framekernel.md](./explain-framekernel.md)）——解析、栈机、WASI 适配全是纯算法与策略，不碰硬件/页表/裸内存，故 0 unsafe。

当前形态有三条硬边界：

- **整数子集**：值类型只支持 `i32`（编码 `0x7F`）与 `i64`（`0x7E`）；浮点族 opcode 与 `f32`/`f64`/`v128`/`funcref`/`externref` 值类型明确返回 `Unsupported`，不是静默忽略。
- **进程级单实例**：一个进程至多持有一个 wasm 载体实例（`Process.wasm_handle` 非 0 即 `EBUSY`），fork 不继承、exec 清零。
- **解释器只在 syscall 内执行**：内核侧解释器由用户态桩经 `EG_WASM_RUN` 驱动，一次跑到结束（受 gas 上限），不在内核态常驻循环。

## 模块组成

[functions/wasm/mod.rs](../../src/kernel/functions/wasm/mod.rs) 声明九个子模块，按数据流分层：

| 子模块 | 职责 | 关键落点 |
|---|---|---|
| `module` | WASM 二进制解析器（11 个标准段） | [module.rs:139](../../src/kernel/functions/wasm/module.rs#L139) `parse_wasm` |
| `leb128` | LEB128（Little-Endian Base 128）变长整数编解码 | [leb128.rs](../../src/kernel/functions/wasm/leb128.rs) |
| `types` | 值类型 / opcode / 段编号 / `WasmError` 与到 `Errno` 的映射 | [types.rs](../../src/kernel/functions/wasm/types.rs) |
| `runtime` | 运行期模型：操作数栈 / 线性内存 / 调用帧 / 资源上限配置 | [runtime.rs](../../src/kernel/functions/wasm/runtime.rs) |
| `interpreter` | 栈机主体：取指 → gas 计量 → 按组分派 | [interpreter.rs:435](../../src/kernel/functions/wasm/interpreter.rs#L435) |
| `instance` | `WasmInstance` = 解释器 + 共享 WASI 上下文 | [instance.rs](../../src/kernel/functions/wasm/instance.rs) |
| `wasi` | WASI snapshot_preview1 适配层（38 个 host 函数） | [wasi/mod.rs:84](../../src/kernel/functions/wasm/wasi/mod.rs#L84) |
| `registry` | 进程级载体注册表 + 四个 syscall 入口 | [registry.rs](../../src/kernel/functions/wasm/registry.rs) |
| `stub` | 按架构内嵌的用户态入口桩映像 + 魔数判定 | [stub.rs](../../src/kernel/functions/wasm/stub.rs) |

`types`/`runtime`/`interpreter` 三者构成解释器核心，`wasi` 是其 host 侧适配，`registry`/`stub` 把解释器接进进程模型。原 `privileged/wasm` 子树已整体迁入 functions（T6-9），privileged 不再保留 wasm 机制层。

## 端到端执行链路

一个 wasm 模块从 `execve` 到退出码的完整路径：

1. **魔数派发**：[exec.rs:106-119](../../src/kernel/functions/proc/exec.rs#L106-L119) 读入文件后按前 4 字节单点 match——`\0asm`（`WASM_MAGIC`）走 wasm 分支，否则走既有 ELF 路径。wasm 分支不装载模块本体，而是取出内嵌入口桩映像（[stub.rs](../../src/kernel/functions/wasm/stub.rs) 的 `stub_image()`），桩为空则返 `Enoexec`；模块路径随 `argv[0]` 交给桩。
2. **进程替换**：桩映像经 [exec.rs:122](../../src/kernel/functions/proc/exec.rs#L122) 的 `proc_exec_replace`（privileged 机制）装成新映像，wasm 进程由此成为普通可调度用户进程。
3. **装载**：桩经 `EG_WASM_LOAD` 让内核读模块——[registry.rs:254](../../src/kernel/functions/wasm/registry.rs#L254) `load_syscall` 经 VFS 读字节（上限 1 MiB）、`parse_wasm` 解析、登记实例并复制 argv/envp 进 WASI 上下文，返回句柄。
4. **运行**：桩经 `EG_WASM_RUN` 驱动 [registry.rs:286](../../src/kernel/functions/wasm/registry.rs#L286) `run_syscall`，把实例交给解释器跑到结束。
5. **取终态 + 释放**：桩经 `EG_WASM_GET_INFO` 取状态/退出码/gas，经 `EG_WASM_UNLOAD` 释放实例。
6. **退出**：桩以模块退出码（正常结束）或 1（trap / 装载失败）调 `proc_exit`。

四个 syscall 编号在主分派链首命中处接入：[dispatch.rs:76](../../src/kernel/functions/syscall/dispatch.rs#L76) 调 [dispatch.rs:1217](../../src/kernel/functions/syscall/dispatch.rs#L1217) `dispatch_wasm`，四臂分别转 `load_syscall`/`run_syscall`/`get_info_syscall`/`unload_syscall`（[dispatch.rs:1224-1227](../../src/kernel/functions/syscall/dispatch.rs#L1224-L1227)）。

## 解析器：全函数化与边界安全

[module.rs:139](../../src/kernel/functions/wasm/module.rs#L139) 的 `parse_wasm` 是**全函数**（total function）：任何字节输入都返回 `Ok` 或 `Err`，不存在用户可触达的 panic 路径。这是 W-2 改造的达标定义——改造前段内取字节直接索引、条目数前缀无上界，截断字节码可在内核态触发 index-out-of-bounds panic 与巨额 `Vec::with_capacity` 分配失败（一条 DoS 路径）。

现行四道边界闸（逐条有回归锁）：

- **子切片即段边界**：每个段先切出子切片再交子解析器（[module.rs:174-175](../../src/kernel/functions/wasm/module.rs#L174-L175)），子解析器内的 `bytes.len()` 就是段尾，跨段读取在类型上不可能。
- **边界安全取字节原语**：`take_byte`/`take_slice` 等（[module.rs:52](../../src/kernel/functions/wasm/module.rs#L52)、[module.rs:103](../../src/kernel/functions/wasm/module.rs#L103)）越界一律返 `WasmError::Truncated`。
- **条目数上界**：`take_entry_count`（[module.rs:85](../../src/kernel/functions/wasm/module.rs#L85)）以"每条目至少占 1 字节"为语义，用段内剩余字节数约束声明数，杜绝 `count = 0xFFFF_FFFF` 式分配攻击。
- **消费收尾对账**：`require_consumed`（[module.rs:118](../../src/kernel/functions/wasm/module.rs#L118)）断言段已消费字节数等于声明尺寸，内容比声明短或长都判畸形。

解析器覆盖 WASM 1.0 的 11 个标准段（类型/导入/函数/表格/内存/全局/导出/启动/元素/代码/数据，段编号见 [types.rs:632-652](../../src/kernel/functions/wasm/types.rs#L632-L652)）；未知段（含 custom section id 0）按声明尺寸整体跳过，不参与模块结构。魔数 `0x6D73_6100` 与版本 1 在入口校验（[types.rs:23](../../src/kernel/functions/wasm/types.rs#L23)、[types.rs:25](../../src/kernel/functions/wasm/types.rs#L25)）。

## 解释器：栈机、gas 与指令分派

运行期模型在 [runtime.rs](../../src/kernel/functions/wasm/runtime.rs)：`ValueStack`（操作数栈，容量上限 4,096，满则 `StackOverflow`，见 [runtime.rs:47](../../src/kernel/functions/wasm/runtime.rs#L47)）、`LinearMemory`（线性内存，每页 65,536 字节，`WASM_PAGE_SIZE` 见 [types.rs:28](../../src/kernel/functions/wasm/types.rs#L28)）、`CallFrame`（调用帧）与 `InterpreterConfig`（资源上限）。线性内存对外只暴露 `get_slice`/`get_slice_mut` 安全包装（[runtime.rs:364](../../src/kernel/functions/wasm/runtime.rs#L364)、[runtime.rs:377](../../src/kernel/functions/wasm/runtime.rs#L377)），每次访问走 `check_access` 边界检查，越界返 `MemoryOutOfBounds`——WASI 模块据此读写线性内存而无需 unsafe。

执行主体 [interpreter.rs:435](../../src/kernel/functions/wasm/interpreter.rs#L435) `execute_call_stack` 是一个"取指 → 计量 gas → 分派单条指令"的循环：每执行一条指令 `gas_used += 1`，超过 `config.gas` 即返 `GasExhausted`（[interpreter.rs:445-448](../../src/kernel/functions/wasm/interpreter.rs#L445-L448)）。gas 计量是运行期 DoS 防线，与解析期结构校验互补。

单条指令经 [interpreter.rs:467](../../src/kernel/functions/wasm/interpreter.rs#L467) `execute_op` 按组顺序试探分派：控制流（`execute_control_op`，含 block/loop/if/br*/end/return/call/call_indirect）、变量与栈（`execute_var_op`）、内存（`execute_memory_op`）、`i32` 算术（`execute_i32_op`）、`i64` 算术（`execute_i64_op`）。`call_indirect`（[interpreter.rs:1118](../../src/kernel/functions/wasm/interpreter.rs#L1118)）经表格做类型校验后间接调用。

浮点排除是显式的：[interpreter.rs:491-493](../../src/kernel/functions/wasm/interpreter.rs#L491-L493) `is_float_opcode` 判定字节是否属浮点族（`0x2A`/`0x2B`/`0x38`/`0x39`/`0x43`/`0x44`/`0x8B..=0xBF`），命中即返 `Unsupported` 而非 `UnknownOpcode`；值类型读取处对 `0x7D`/`0x7C`/`0x7B`/`0x70`/`0x6F` 同样返 `Unsupported`（[interpreter.rs:763](../../src/kernel/functions/wasm/interpreter.rs#L763)）。这使"不支持浮点"成为可判别的契约，而非含混的未知指令。

`WasmInstance`（[instance.rs](../../src/kernel/functions/wasm/instance.rs)）把解释器与 WASI 上下文绑成一体：上下文以 `Arc<Mutex<WasiContext>>` 持有（锁类名 `WASI_CTX`），构造时调 `register_wasi_functions` 登记 host 函数名、再由 `auto_register_wasi` 按模块 import 段装配下标。用 `Arc<Mutex>` 而非 `Rc<RefCell>` 是为满足注册表对 `Send` 的要求。

## WASI 适配层

[wasi/mod.rs](../../src/kernel/functions/wasm/wasi/mod.rs) 实现 WASI snapshot_preview1 接口，使 WASI 工具链编译的模块能在内核里跑。每个实例一个 `WasiContext`（[wasi/mod.rs:48](../../src/kernel/functions/wasm/wasi/mod.rs#L48)），含 fd 表（容量 256）、命令行参数与环境变量。host 函数签名统一为 `fn(&mut WasiContext, &mut Interpreter) -> Result<(), WasmError>`，注册表 `wasi_function_table()`（[wasi/mod.rs:84](../../src/kernel/functions/wasm/wasi/mod.rs#L84)）给出 **38 个函数、8 组**：

| 组 | 语义 | 函数 |
|---|---|---|
| G1 | 进程控制 | `proc_exit`、`sched_yield` |
| G2 | 时钟/随机 | `clock_time_get`、`random_get` |
| G3 | 环境/参数 | `environ_sizes_get`、`environ_get`、`args_sizes_get`、`args_get` |
| G4 | FD 管理 | `fd_close`、`fd_seek`、`fd_tell`、`fd_sync`、`fd_prestat_get`、`fd_prestat_dir_name`、`fd_stat_get` |
| G5 | FD I/O | `fd_read`、`fd_write`、`fd_pread`、`fd_pwrite`、`fd_allocate`、`fd_advise` |
| G6 | 路径操作 | `path_open`、`path_create_directory`、`path_remove_directory`、`path_unlink_file`、`path_symlink`、`path_readlink`、`path_rename`、`path_filestat_get`、`path_filestat_set_times`、`path_link` |
| G7 | 高级 FD | `fd_renumber`、`fd_dup`、`fd_readdir` |
| G8 | Socket | `sock_accept`、`sock_connect`、`sock_recv`、`sock_send` |

这些 host 函数把 WASI 调用翻译成既有 POSIX 服务：fd I/O 与路径操作走 `functions::fs`（VFS），时钟走 `privileged::timer`，进程退出走 `functions::proc`，socket 走 `functions::net`。注入通路在 [wasi/inject.rs:25](../../src/kernel/functions/wasm/wasi/inject.rs#L25) `register_wasi_functions`：把 `(name, WasiFunc)` 适配为解释器可调用的 host 闭包并登记名称表，下标装配由 `auto_register_wasi` 按 import 声明序完成。

fd 表（[wasi/fd_table.rs](../../src/kernel/functions/wasm/wasi/fd_table.rs)）构造时预置三面固定的标准流与根 preopen：`WASI_STDIN`=0 / `WASI_STDOUT`=1 / `WASI_STDERR`=2（[wasi/fd_table.rs:10-14](../../src/kernel/functions/wasm/wasi/fd_table.rs#L10-L14)）映射到内核 fd 0/1/2；根 preopen fd 3 承载路径 `"/"` 供 `path_open` 等相对路径解析，但不预开目录 VFS fd（`inner_fd = -1`，[wasi/fd_table.rs:131-134](../../src/kernel/functions/wasm/wasi/fd_table.rs#L131-L134) 标 SIMPLIFIED）。首个动态分配落在 preopen 之后，不覆盖预置槽位。

## 进程级载体与 syscall 面

[registry.rs](../../src/kernel/functions/wasm/registry.rs) 是进程级载体的注册表：全局 `RUNTIMES` 以 `IrqSpinLock::named_lazy("WASM_REGISTRY", …)` 持有（[registry.rs:48](../../src/kernel/functions/wasm/registry.rs#L48)），按句柄索引 `RuntimeEntry`（owner 进程 / 入口名 / 状态 / 退出码 / gas / 实例）。四条容量与尺寸上限钉死在常量：实例数 `MAX_WASM_INSTANCES`=64、模块文件 `WASM_MAX_FILE_SIZE`=1 MiB、单用户串 `MAX_USER_STRING`=1,024 字节、串向量项数 `MAX_STRVEC_ITEMS`=64（[registry.rs:38-44](../../src/kernel/functions/wasm/registry.rs#L38-L44)）。

入口点选择（[registry.rs:89](../../src/kernel/functions/wasm/registry.rs#L89) `entry_point`）优先 `_start`、其次 `main`、皆无则 `ENOENT`。失效回收（[registry.rs:104](../../src/kernel/functions/wasm/registry.rs#L104) `reap_stale`）按"进程仍存在且 `wasm_handle` 仍等于该句柄"判据 retain，配合实例数上限保证回收前不会无界增长。`run_syscall` 执行期把实例从条目中取出（执行中条目为 `None`，并发 RUN 返 `EBUSY`），跑完回填终态——这是 one-shot gas 限界形态，无步进让出/恢复（标 SIMPLIFIED）。

`WASM_REGISTRY` 与 `WASI_CTX` 同属锁序**层级 7**（与进程表同层，见 [ref-lock-order.md:41-43](./ref-lock-order.md#L41-L43)）：先取本锁再取 VFS（层级 11）/网络（层级 12）为升序；运行期不在持注册表锁中执行解释器。

**ABI**（Application Binary Interface，应用二进制接口）单源在 [edgine-abi/src/wasm.rs](../../src/rust/edgine-abi/src/wasm.rs)：`EG_WASM_LOAD`=766 / `EG_WASM_RUN`=767 / `EG_WASM_GET_INFO`=768 / `EG_WASM_UNLOAD`=769，落在私有功能段「766-779: WASM 运行时」，不占 0–299 的 Linux 号位（Linux 无"进程内加载并执行 wasm"的 syscall，占号无兼容收益却破坏"0–299 == Linux"不变式）。状态常量 `READY`=0 / `FINISHED`=1 / `TRAPPED`=2；终态结构 `EgWasmInfo` 为 `#[repr(C)] { status: u32, exit_code: i32, gas_used: u64 }`。`WasmError` 到 `Errno` 的映射在 [types.rs:739](../../src/kernel/functions/wasm/types.rs#L739)：镜像结构类→`ENOEXEC`、运行期 trap 类→`EINVAL`、越界→`EFAULT`、分配/深度→`ENOMEM`、gas→`ETIME`、未实现→`ENOSYS`、内部不变量→`EIO`（除零→`EDOM`，`Terminated` 正常终态由 `run_syscall` 截获）。

## 用户态入口桩

[wasmstub](../../src/user/wasmstub/src/main.rs) 是 `execve` 识别 wasm 后实际装载的用户态程序。入口 `_start` 用架构汇编把入口栈指针 `sp` 作为首参转发（x86_64 `mov rdi, rsp` / aarch64 `mov x0, sp`，[wasmstub/src/main.rs:23-37](../../src/user/wasmstub/src/main.rs#L23-L37)），因为 `extern "C"` 函数读不到入口 SP。

[wasmstub/src/main.rs:57](../../src/user/wasmstub/src/main.rs#L57) `wasmstub_main` 按内核布置的栈布局（`argc` → `argv[0..argc]` → NULL → `envp[0..]` → NULL）取 `argv[0]` 即模块路径，随后顺序执行：`wasm_load`（[main.rs:72](../../src/user/wasmstub/src/main.rs#L72)）→ `wasm_run`（[main.rs:82](../../src/user/wasmstub/src/main.rs#L82)）→ `wasm_get_info`（[main.rs:88](../../src/user/wasmstub/src/main.rs#L88)）→ `wasm_unload`（[main.rs:89](../../src/user/wasmstub/src/main.rs#L89)）→ `proc_exit`（[main.rs:96](../../src/user/wasmstub/src/main.rs#L96)）。退出码取 `FINISHED` 态的 `info.exit_code`，否则 1。桩映像按架构 `include_bytes!` 内嵌（[stub.rs](../../src/kernel/functions/wasm/stub.rs)，host 目标为空切片），产物落 `other/build/<arch>/user/wasmstub.bin`，与内核同架构嵌入（S-6 三条防线）。

## 资源上限

解释器与载体的全部硬上限（运行期 DoS 与资源耗尽防线）：

| 上限 | 值 | 落点 |
|---|---|---|
| 调用深度 | 256 | [runtime.rs:397-404](../../src/kernel/functions/wasm/runtime.rs#L397-L404) `InterpreterConfig::default` |
| gas 预算 | 10,000,000 | 同上 |
| 线性内存页数 | 256 | 同上 |
| 操作数栈容量 | 4,096 | [runtime.rs:47](../../src/kernel/functions/wasm/runtime.rs#L47) |
| 线性内存页大小 | 65,536 字节 | [types.rs:28](../../src/kernel/functions/wasm/types.rs#L28) |
| WASI fd 表容量 | 256 | [wasi/mod.rs:62](../../src/kernel/functions/wasm/wasi/mod.rs#L62) |
| 进程级实例数 | 64 | [registry.rs:38](../../src/kernel/functions/wasm/registry.rs#L38) |
| 模块文件尺寸 | 1 MiB | [registry.rs:40](../../src/kernel/functions/wasm/registry.rs#L40) |

隔离立场是**两态分离**：结构面（表索引/操作数栈/分支目标/段边界）在解析期一次校验，不合规返 `Err`；运行期每次线性内存访问仍走边界检查。后者是 WASM 规范对 linear memory 的固要求，与"是否信任字节码"无关，不为快路径移除。

## 测试与验证

wasm 目录内联测试合计 **75** 例（`module` 26 / `interpreter` 26 / `leb128` 6 / `registry` 4 / `instance` 2 / `wasi/fd_table` 2 / `wasi/inject` 1 / `types` 1 / `runtime` 7），覆盖畸形输入矩阵（全前缀/全单字节突变两条穷举式全函数性证明）、控制流、表调用、WASI 注入与端到端 stdout 落盘。`types` 的 1 例对 `WasmError`→`Errno` 做 34 变体穷举，`runtime` 的 7 例钉栈/内存/调用帧 helper 的边界。

host 侧集成测试两册：[wasm_syscall_test.rs](../../host-tests/tests/wasm_syscall_test.rs) 5 例（ABI 编号同源 / 四编号 dispatch 接管 / 10 组错误映射 / VFS→LOAD→RUN→GET_INFO→UNLOAD 全链 / 畸形模块 `ENOEXEC`），[wasi_behavior_test.rs](../../host-tests/tests/wasi_behavior_test.rs) 11 例（WASI 公共面行为级端到端：args/environ 往返、clock/random、proc_exit/sched_yield、fd read/seek/tell/close/prestat、path_open）。QEMU 双架构 exec 端到端断言默认启动 `[WASM] SKIP: no carrier`、initramfs 载体腿 `[WASM] PASS: exec+run`。