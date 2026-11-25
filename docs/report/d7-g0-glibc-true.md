# D7 G0：x86_64 真实 glibc 动态 `true`

日期：2026-11-25
关联：`docs/plan/dynamic-linker-project.md` D7 / `docs/plan/progress-active-tasks.md`

## 目的

用真实上游 glibc（Fedora 45，glibc 2.44）的 `ld-linux` + `libc.so.6` 装载
`/bin/true` 的未剥离运行副本，验证 Edgine 的动态 ELF 路径能撑过 glibc 的启动
链，并把首个失败点收敛到具体内核面。判据是子进程 `wstatus == 0`，不是“内核启动
到某里程碑”。

## 载荷

| 文件 | 大小 | sha256 |
|---|---:|---|
| `glibc_true` | 36,424 B | `684bc05bc973d1bfec940279475ece99740a78765696fcd3ea2dbd540df16fb5` |
| `ld-linux-x86-64.so.2` | 260,320 B | `6c3d7bdd7c5b6155fb6f7660bd49955f993580aac951e16bbe6eb004f87429e4` |
| `libc.so.6` | 2,187,808 B | `958416ed2a6831d8c739c75da15ebe0ed570299a768967dcff3fc736ca62aadb` |

`glibc_true` 是 `ET_DYN` PIE，`PT_INTERP=/lib64/ld-linux-x86-64.so.2`，
`NEEDED=libc.so.6`。`libc.so.6` 只由上游 loader 通过 `mmap` 读取，内核不整文件
读它，因此不受 `ELF_MAX_SIZE = 1 MiB` 的整文件读限制；内核只读 36 KB 主程序与
260 KB 解释器。fixture 与复现命令见
`scripts/fixtures/dynamic-glibc-x86_64/README.md`。

## initramfs 与探针

- 布局：`/bin/glibc_true`、`/lib64/ld-linux-x86-64.so.2`、`/lib64/libc.so.6`。
- `init` 在 `dyn_badfmt_probe()` 后执行 `glibc_probe()`：
  - 先 `stat`/`open`/`fstat` `/lib64/libc.so.6`，打印一行诊断；
  - `fork` 后子进程真实 `execve("/bin/glibc_true")`；
  - 父进程按 `wait_pid_status()` 的 `wstatus == 0` 打印
    `[GLIBC] PASS: dynamic glibc execve`，否则打印 `[GLIBC] FAIL` 与 status/reap。
- aarch64 当前没有 glibc 载体，探针打印 `[GLIBC] SKIP: no carrier`；QEMU 门禁
  接受 SKIP 或 PASS，任何 `[GLIBC] FAIL` 一律阻断。

## G0 期间收敛的缺口

| 失败面 | 根因 | 修复 |
|---|---|---|
| loader 报 `libc.so.6: cannot stat` | `SYS_NEWFSTATAT` 的 dispatch 仍按旧的 `(fd, statbuf)` 取参，而 Linux ABI 是 `(dirfd, path, statbuf, flags)`，`a1` 被当作 fd、`a2` 被当作 statbuf | `dispatch_fs_at` 改传 `a0/a1/a2/a3`，新增 `newfstatat_syscall`；`AT_FDCWD` 走路径 stat，`AT_EMPTY_PATH` 走 fd stat |
| glibc 读到错误的 size/mode | `stat`/`fstat` 把内部 `VfsStat` 直接写进用户缓冲，而 Linux `struct stat` 在 x86_64 是 144 B、aarch64 是 128 B | 新增按架构的 `LinuxStat`，由 `linux_stat_from_vfs()` 转换后写回；补 `size_of`/字段映射单测 |
| 内核侧 `fstat` 恒 `EIO` | `vfs_fstat_safe` 复用 `vfs_fstat`，把内核栈上的 `VfsStat` 当用户缓冲传入，`write_struct_to_user` 必拒 | 拆出 `vfs_fstat_core()` 返回 `Option<VfsStat>`，safe 包装不再跨用户边界 |
| RELRO/文件映射片段读回错误偏移 | `mprotect`/拆分 VMA 时复制了原 VMA 的 `offset`，中段或后段重新缺页会从文件开头读，而不是对应片段 | 文件映射的 `offset` 按片段起点前移；保留 `inode_id`/`file_pwm`/`mount` |
| glibc `_dl_start` 的 `movaps` #GP | 初始栈 `%rsp` 落在 8 模 16，未满足 Linux 入口的 16 字节对齐 | `setup_user_stack_in_table()` 以 16 字节向下取整 `new_sp`，溢出判据保留 16 B 余量 |

## 实测读数

x86_64 串口日志：

```text
[GLIBC] stat libc rc=0 fd=3 fstat rc=0
...
[GLIBC] PASS: dynamic glibc execve
```

aarch64 串口日志：

```text
[GLIBC] SKIP: no carrier
```

门禁读数：

- `scripts/qemu_boot_test.sh all`：x86_64 `[GLIBC] PASS` 通过；aarch64 SKIP/PASS
  合规；双架构 `2/2`，无 `[GLIBC] FAIL`。
- `./ci/build.sh all`：`Passed: 6 / Failed: 0`。
- `make test-host`：RC=0（218 个套件 / 1676 passed / 0 failed）。
- `make test-kernel-host`：RC=0（3 个套件 / 1464 passed / 0 failed；含新增
  `fs::stat::tests` 两例）。
- `bash ci/audit.sh full`：RC=0；审计腿到位、QEMU 双架构 2/2、UEFI 判据通过，
  本轮未执行腿数 0。

## 边界

- 只覆盖 x86_64。aarch64 没有 glibc 载体，当前是 SKIP，不是通过。
- 载荷是 `true`，只验证“能启动并 `exit(0)`”；没有 stdout 输出、没有参数解析、
  没有 `AT_PLATFORM`/`AT_HWCAP` 依赖验证。
- 仍有 glibc 启动面未覆盖：G0 本腿没有直接断言 `set_tid_address`/`prlimit64`/
  `rseq`；这些已由 G2 的原始 syscall 探针与宿主 GDB 复核补齐（`rseq` 保持
  `ENOSYS`）；pthread、`dlopen`、VDSO 仍在 G3/G4。
- `libc.so.6` 大于 `ELF_MAX_SIZE`，本腿证明它可由 loader `mmap`，不证明内核可把
  它当主程序整文件装载。
- 该结果属 D7 可选工程；T7 已于 2026-11-25 按方案一以双架构 musl 收口，状态仍为
  `[X]`，不因本腿回退。

## 下一步

1. **G1（已完成）**：带 stdout 的 x86_64 动态 glibc hello 已于 2026-11-25 通过，
   当前 auxv 不需要补 `AT_PLATFORM`/`AT_HWCAP*` 也能跑；见
   [d7-g1-glibc-hello.md](d7-g1-glibc-hello.md)。
2. **G2（已完成）**：`glibc_g2` 探针已确认 `set_tid_address`/`prlimit64` 返回
   成功、`rseq` 保持 `ENOSYS`；宿主 GDB 证实 glibc 启动实际调用三者。见
   [d7-g2-glibc-startup-syscalls.md](d7-g2-glibc-startup-syscalls.md)。
3. **G3**：仅当 `AT_SYSINFO_EHDR=0` 在 `clock_gettime`/`gettimeofday` 上失败时
   才做最小 VDSO。
4. **G4**：再评估 `clone3`/pthread/TLS/`dlopen`，不把线程面混进 G1。
