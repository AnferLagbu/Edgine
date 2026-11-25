# D7 C1：glibc clone3 高级字段收口

> 日期：2026-11-26（x86_64）
> 范围：`D7-DEFERRED-C1` —— `clone3` 的 `exit_signal`、`CLONE_PIDFD`、
> `set_tid`/`set_tid_size`、`cgroup` 边界、clone3 ABI 短结构，以及 glibc
> 动态进程在 fork 后 COW 破页的回归门禁。

## 1. 结论

`clone3` 高级字段已按 Linux 语义落地到当前内核能力边界：

- `exit_signal` 独立于 `flags` 低字节存放；线程组最后成员退出时向父线程组投递一次，
  `CLONE_THREAD` 路径强制为 0。
- `CLONE_PIDFD` 在创建子进程前校验 `pidfd` 非 0 且 4 字节用户地址可写；创建后
  安装 pidfd、置 `FD_CLOEXEC` 并写回。写回失败时关闭 fd 并返回 `EFAULT`。
  pidfd 的 `poll(POLLIN)` 由对象状态快照报告；通用 poll/ppoll 的超时阻塞仍是
  既有 SIMPLIFIED 缺口，C1 探针以 `wait4` 后零超时 poll 验退出就绪态。
  注意：若 `pidfd_open` 在子进程创建后失败，当前直接返回错误而保留子进程。
- `set_tid`/`set_tid_size` 与 `cgroup`/`CLONE_INTO_CGROUP` 当前显式 `ENOSYS`，
  不静默忽略。
- clone3 `size` 至少 64；`64..71` 只读基础字段，`>=72/80/88` 分级读扩展字段。
- `exit_signal` 按 Linux `_NSIG=64` 允许 `0..=64`（64 = `SIGRTMAX`），65 及以上 `EINVAL`。
- `wait4` 接受 `__WALL`，使 `exit_signal != SIGCHLD` 的 clone 子进程可被回收。

QEMU 串口读数：

```text
[CLONE3] exit_signal PASS
[CLONE3] pidfd PASS
[CLONE3] size=64 PASS
[CLONE3] set_tid ENOSYS PASS
[CLONE3] cgroup ENOSYS PASS
[CLONE3] exit_signal=65 EINVAL PASS
[CLONE3] PASS
[GLIBC] CLONE3 PASS: glibc clone3
```

同轮 G0–G4 保持通过：`[GLIBC] PASS: dynamic glibc execve`、`HELLO PASS`、
`G2 PASS`、`TIME PASS`、`PTHREAD PASS`。

## 2. 一并根治：fork 后 COW 破页顺序

C1 首轮在 `clone3` 返回、父进程第一次 `printf` 前崩在用户态 `#PF RIP=0`。
取证链最终收敛到**页故障分类顺序**，不是 clone3 字段本身：

1. 父进程 GOT 页 `0x403000` 在 fork/clone 后是 COW 只读（PTE `...65`，`frame_cow_test=true`）。
2. `sysret` 时 GOT 仍有值；随后 `printf` 路径写 GOT 触发 write fault。
3. `handle_vma_fault_with_mm` 先命中 `FileBacked` / `!vma.flags.WRITABLE` 分支：
   - glibc 数据 VMA 本身是 `WRITABLE`，只读的是 PTE，按 VMA 判据漏掉 COW；
   - 文件映射页又被 `handle_file_fault` 用 page cache 页替换，`stdout` 的 vtable
     回到文件里的未重定位值 `0x1F3010`，GOT 则可能被零页替换。
4. 现象分别是 GOT 整页归零与 `Fatal error: glibc detected an invalid stdio handle`。

修复（`src/kernel/privileged/mm/page_fault.rs`）：

- 以「PTE present + 目标帧带 COW 待破标」为 COW 证据，**先于** `FileBacked` 与
  旧 `!VMA WRITABLE` 支路调用 `cow::cow_handle_fault`。
- `FileBacked` 仍先于旧 `!VMA WRITABLE` 支路，保证 **not-present 的文件页**
  继续走 page cache，不被 `do_cow_copy_with_mm` 误判成 `SIGSEGV`。

这不是 clone3 专属补丁：任何 fork 后写 glibc/GOT/stdio 数据页的路径都走同一条
COW 修复。

## 3. 实现清单

| 文件 | 改动 |
|---|---|
| `src/rust/edgine-abi/src/thread.rs` | `CLONE_PIDFD`、`CLONE_INTO_CGROUP` 常量 |
| `src/kernel/functions/syscall/types.rs` | re-export 新常量 |
| `src/kernel/privileged/proc/process.rs` | `Process.exit_signal: AtomicU32` |
| `src/kernel/functions/proc/clone.rs` | `clone_syscall` 第 6 参数 `exit_signal`；0..=64 校验；`CLONE_THREAD` 强制 0；fork/VM 路径透传 |
| `src/kernel/functions/syscall/dispatch.rs` | `SYS_CLONE` 取 flags 低字节；`SYS_CLONE3` 分级解析 `struct clone_args`、`CLONE_PIDFD`、`set_tid`/`cgroup` 边界 |
| `src/kernel/privileged/proc/scheduler.rs` | 组最后成员退出投递 `exit_signal` |
| `src/kernel/privileged/proc/proc_ops.rs` | `sys_fork_with_exit_signal`，子进程入 runqueue 前写字段 |
| `src/kernel/functions/proc/wait4.rs` | `__WALL` 接受并进入 `valid_opts` |
| `src/kernel/privileged/mm/page_fault.rs` | COW 判定前移，修 fork 后数据页破页顺序 |
| `scripts/fixtures/dynamic-glibc-x86_64/glibc_clone3.c` | 正式 C1 探针 |
| `Makefile` / `make_initramfs_cpio.py` / `init/src/main.rs` / `qemu_boot_test.sh` | fixture 构建、镜像、探针与门禁 |

## 4. 边界登记

- **`set_tid` / `set_tid_size`**：当前无 PID namespace 选号，显式 `ENOSYS`；
  后续实装 PID namespace 时替换。`set_tid` 指针本身不做解引用校验。
- **`cgroup` / `CLONE_INTO_CGROUP`**：当前无 cgroup attach，显式 `ENOSYS`。
- **`CLONE_PIDFD` 创建后失败**：`pidfd_open` 在子进程创建后失败时保留子进程，
  仅返回错误；未实现 kill/reap 回滚。调用方需按 Linux 语义自行处理已创建子进程。
- **`size > 88`**：超出已定义字段的尾部忽略；当前不校验未知扩展字段。
- **pidfd poll 阻塞**：`poll/ppoll` 当前为单次快照，`timeout` 不生效；pidfd
  只能轮询快照或先 `wait4` 再零超时 poll。该缺口属通用 VFS poll，不在 C1 内实装。
- **`clone3 size < 64`**：`EINVAL`，不读取任何字段。
- **legacy `clone` 低字节**：`a0 & 0xff` 作为 `exit_signal`；`CLONE_THREAD` 强制 0。
- **legacy `clone` 高级标志**：`CLONE_PIDFD` / `CLONE_INTO_CGROUP` 置位时显式
  `EINVAL`，不经 legacy clone 静默丢弃（这两者是 clone3 路径的高级语义）。
- **`__WALL`**：按「全部子进程可见」执行，不区分 `__WCLONE`。

## 5. 复现与验证

```bash
make ARCH=x86_64 all
source scripts/boot_module.sh && boot_module_iso_x86
# QEMU 串口断言
grep -a "[CLONE3] PASS" /tmp/qemu.log
grep -a "[GLIBC] CLONE3 PASS: glibc clone3" /tmp/qemu.log
```

完整门禁：

```bash
./ci/build.sh all
make test-kernel-host
bash ci/audit.sh full
python3 tools/check_doc_paths.py
bash scripts/qemu_boot_test.sh all
```
