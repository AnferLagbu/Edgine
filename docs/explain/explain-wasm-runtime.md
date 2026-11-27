# WASM 运行时

> Edgine 的 wasm 执行**全部在用户态**：`execve` 识别 `\0asm` 魔数后把进程映像替换为用户态 wasmi 运行时 [`wasmrun`](../../src/user/wasmrun/src/main.rs)，由它在用户态解释执行模块；WASI（WebAssembly System Interface）snapshot_preview1 的 38 个宿主函数经既有 POSIX syscall 翻译落地。**内核侧不再包含任何字节码解释器**——原 `functions/wasm` 子树、私有 syscall 段 766-779（`EG_WASM_*`）与 `wasmstub` 桩已随 P4 真删（退役清单见本文末节）。设计立场见 [wasm-runtime-design.md](../design/wasm-runtime-design.md) §七，迁移施工分解与读数见 [wasm-userspace-migration.md](../plan/wasm-userspace-migration.md)。

## 定位与边界

wasm 运行时是**普通用户进程**，不扩大 **TCB**（Trusted Computing Base，可信计算基）：解释器（wasmi）是 vendored 的纯 Rust `no_std` 库，字节码、线性内存、模块字节全在用户地址空间，越界由 wasmi 自身的边界检查与 Edgine 的页表权限共同兜住。与内核内解释器时期相比，内核只保留两条机制性职责：

- **单点魔数派发**：`execve` 看到映像前 4 字节为 `\0asm` 时，改装载 `/bin/wasmrun`，模块路径随 `argv[0]` 传入（[exec.rs](../../src/kernel/functions/proc/exec.rs)）。
- **syscall 面**：运行时的 VFS / 时钟 / 随机 / 进程 / 网络调用全部走既有 Linux 兼容 syscall，不为 wasm 单开私有号段。

当前形态的边界：

- **WASI preview1 全量 38 函数**：进程、时钟随机、环境参数、fd 管理、fd I/O、路径、高级 fd、socket 八组全部注册；socket 组受 preview1 结构限制——协议无 `sock_open`，普通 wasi-sdk 载荷拿不到 socket fd，该路径只能等扩展或宿主注入，不做假绿。
- **无 JIT**：wasmi 解释执行，不产生机器码、不需要 `PROT_EXEC` 映射，因此不触碰可执行内存与信号 trap 的缺口。
- **单实例**：一个 wasm 进程一个运行时实例；`execve` 换映像即换运行时进程，不涉及内核侧句柄继承。

## 组成

| 组件 | 职责 |
|---|---|
| [wasmrun/main.rs](../../src/user/wasmrun/src/main.rs) | 入口：取入口栈 `sp`、读 `argv[0]` 模块路径、VFS 读模块、wasmi 引擎/模块/Store/Linker、调 `_start`、`proc_exit` 退出码 |
| [wasmrun/host.rs](../../src/user/wasmrun/src/host.rs) | 38 个 `wasi_snapshot_preview1` 宿主函数注册与逐函数翻译（`wasi_host!` 宏统一借用适配，含 `fd_readdir` 填满缓冲契约、`fd_dup` 语义） |
| [wasmrun/wasi.rs](../../src/user/wasmrun/src/wasi.rs) | WASI errno 表、fd 表、preopen、路径归一、双架构 `filestat` 编解码（`cfg!` 分派保两套偏移表均被引用）、10 个宿主单测 |
| [userlib/sys.rs](../../src/user/lib/src/sys.rs) | 用户态 syscall 包装（`open/read/write/stat/lseek/pread/pwrite/link/symlink/utimensat/getrandom/mmap/...`） |
| [userlib/heap.rs](../../src/user/lib/src/heap.rs) | P0 最小 `#[global_allocator]`：K&R 地址升序 first-fit free-list，按需 `mmap` 64 KiB 粒度增长 |
| vendored `wasmi` 2.0.0 | 纯 Rust `no_std` 解释器，`libm` + `validate`；锁定版本治理同 smoltcp（升级须授权） |

**ABI 边界分层**：wasmi `IntoFunc` 要求注册闭包按值取 `Caller<'_, WasiCtx>`，而翻译函数体内只需要借用。
故借用形态按边界切开——host 层 38 个函数签名统一为 `caller: &mut Caller<'_, WasiCtx>`（体内直接传 `caller`），
注册边界由 `wasi_host!` 宏展开成 `func_wrap(…, |mut caller: Caller<…>, args…| f(&mut caller, args…))`，
把「按值 → 借用」的适配集中在一处；WASI 规范超 7 参的 ABI 用参数结构体承载
（`PathOpenArgs` / `PathTimesArgs` / `PathLinkArgs` / `WasiFilestat`），闭包只做解包。
这样既满足 wasmi 的注册契约，也不需要任何 `#[allow(clippy::too_many_arguments)]` 或借用抑制。

## 端到端执行链路

1. **魔数派发**：`execve` 读入映像后按前 4 字节单点 match；`\0asm` 分支从 VFS 读 `/bin/wasmrun`（上限同 `ELF_MAX_SIZE` 1 MiB），读不到返 `ENOEXEC`，随后交 `proc_exec_replace_with_interp` 替换进程映像。
2. **运行时入口**：架构汇编把入口栈指针作为首参转发（x86_64 `mov rdi, rsp` / aarch64 `mov x0, sp`），`argv[0]` 即模块路径。
3. **装载模块**：运行时 `open` + 循环 `read` 读入模块字节；wasmi `Module::new` 在 `validate` 门控下解析与校验。
4. **实例化**：`Store<WasiCtx>` + `Linker` 注册 38 个宿主函数，`instantiate_and_start` 建实例。
5. **执行与退出**：取 `_start: () -> ()` 调用；正常结束 `proc_exit(0)`，装载/解析/实例化/trap 任一失败 `proc_exit(1)`，模块 `proc_exit` 的退出码原样透传。

## WASI 适配层

翻译目标是既有 syscall，不新开内核面：

| 组 | 代表入口 | 内核落点 |
|---|---|---|
| G1 进程 | `proc_exit` / `proc_raise` / `sched_yield` | `exit` / `kill` / `sched_yield` |
| G2 时钟随机 | `clock_time_get` / `random_get` | `clock_gettime` / `getrandom` |
| G3 环境参数 | `args_get` / `args_sizes_get` / `environ_*` | 运行时自持（入口栈复制），不落 syscall |
| G4 fd 管理 | `fd_close` / `fd_renumber` / `fd_dup` / `fd_fdstat_get/set` | `close` / `dup2` 等价 / 内核 `fd_dup` 语义 |
| G5 fd I/O | `fd_read` / `fd_write` / `fd_pread` / `fd_pwrite` / `fd_seek` / `fd_allocate` / `fd_advise` | `read` / `write` / `pread` / `pwrite` / `lseek` / `posix_fallocate` / `posix_fadvise` |
| G6 路径 | `path_open` / `path_create_directory` / `path_unlink_file` / `path_rename` / `path_filestat_get/set_times` / `path_link` / `path_symlink` / `path_readlink` | `openat` 等价 / `mkdir` / `unlink` / `rename` / `stat` / `utimensat` / `link` / `symlink` / `readlink` |
| G7 高级 fd | `fd_prestat_get` / `fd_prestat_dir_name` / `fd_readdir` / `fd_filestat_get/set_times` / `fd_sync` / `fd_datasync` | `fstat` / `getdents` 等价（`lseek` + `readdir`）/ `futimens` / `fsync` / `fdatasync` |
| G8 socket | `sock_accept` / `sock_connect` / `sock_recv` / `sock_send` / `sock_shutdown` | `accept` / `connect` / `recvfrom` / `sendto` / `shutdown`；preview1 无 `sock_open`，普通载荷不可达 |

两处非标准取舍已固化在代码注释里：`fd_dup` 保留 wasmrun「直接返回 fd / 负 errno」语义而非 WASI 的 `fd` 出参形态；`fd_readdir` 按 `buf_len` 尽量填满多条（`cookie` = 已消费条目数，`cookie==0` 先回卷支持 `rewinddir`），条目放不下且已攒有内容时丢弃该条、调用方按 `d_next` 续读可重取。

## 载体与门禁

- **载体**：`/bin/wasmrun` 由 `make user` 产出并按架构 `objcopy --strip-all`，打进 initramfs cpio（[make_initramfs_cpio.py](../../scripts/make_initramfs_cpio.py)），内核镜像零嵌入。
- **探针载荷**：`bin/hello.wasm`（36 字节确定性最小模块，`_start: () -> ()`）钉 exec 魔数派发；`bin/hello_wasi.wasm`（wasi-sdk 真产物，最小 WASI）钉 stdio；`bin/wasi_p3.wasm`（wasi-sdk 34 真产物，36 项断言）钉逐函数覆盖。
- **专项腿**：[wasmi_boot_test.sh](../../scripts/wasmi_boot_test.sh) 断言 `hello from wasmi` + `[WASMI] exit=0` + mmap 用户 VA 下界；[wasmi_wasi_test.sh](../../scripts/wasmi_wasi_test.sh) 断言 `P3 done fails=0` + `[WASMI-P3] exit=0`。两腿都以 `wasm_userspace` feature 构建——该 feature 现在只用于打印 mmap 用户 VA 诊断行，不再选桩。
- **载体腿**：initramfs / QEMU / UEFI 三条启动腿断言 `[WASM] PASS: exec+run` 且无 `[WASM] FAIL`，证明默认构建下魔数派发与 wasmi 执行链完整。

## 退役记录（P4，2026-11-27）

内核侧 wasm 解释器已**真删**（F9 口径：非 `#[allow(dead_code)]` 抑制，`scripts/audit_dead_code_comment.py` 复验 0 处抑制）：

- `src/kernel/functions/wasm/` 全子树 19 个文件（解析器 / LEB128 / 栈机 / 运行时 / WASI 适配 / 注册表 / 入口桩选择 / 类型与错误映射）；
- `EG_WASM_LOAD/RUN/GET_INFO/UNLOAD`（766-769）与「766-779: WASM 运行时」私有段、`dispatch_wasm` 分派臂；
- `edgine-abi/src/wasm.rs`（ABI 单源）与用户库 4 个 `wasm_*` 包装；
- `src/user/wasmstub` crate、Makefile 桩产物规则、cpio `bin/wasmstub` 条目；
- `Process.wasm_handle` 载体标记与 exec 清零逻辑。

退役后 `execve` 的 `\0asm` 分支只指向 `/bin/wasmrun`；wasmi 的 P1/P3 双架构读数、宿主单测 10 例与三条启动腿在新树上复跑全绿（读数见 [wasm-userspace-migration.md](../plan/wasm-userspace-migration.md) P4 详情）。
