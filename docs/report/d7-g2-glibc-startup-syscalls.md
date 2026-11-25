# D7 G2：glibc 启动面 `set_tid_address` / `prlimit64` 与 `rseq` fallback

日期：2026-11-25
关联：`docs/plan/dynamic-linker-project.md` D7 / `docs/report/d7-g1-glibc-hello.md`

## 目的

G1 已让真实 glibc 2.44 的 `hello` 完成 stdout 输出。G2 不再靠“glibc 能跑”
推断启动面完整，而是取得 glibc 启动序列的真实 syscall 证据，再按证据补
`set_tid_address`/`prlimit64`；`rseq` 先验证 `ENOSYS` 是否可被 glibc fallback
接受，不预先实现注册/退出语义。

## 先更正 G1 的一条事实

G1 报告原写 `set_tid_address`/`prlimit64`/`rseq` “在本腿未触发”。G2 用宿主
GDB 对同一份 `ld-linux` + `libc.so.6` + `glibc_hello` 重新取证，实际抓到了
glibc loader 启动序列的三次调用：

```text
set_tid_address  返回 tid
rseq             返回 0
prlimit64        pid=0, resource=RLIMIT_STACK(3), new=NULL, old=&rlimit
```

这说明 G1 不是“glibc 没调用”，而是**内核当时对三个号都返回 `-ENOSYS`，
glibc 仍容错走完 hello**。G1 的可见绿灯因此不能证明这三个入口“不需要”；
G2 的补齐是针对真实调用面，而不是猜测兼容面。

宿主 GDB 命令（抓调用与返回）：

```bash
gdb -q -batch \
  -ex "catch syscall set_tid_address" \
  -ex "catch syscall prlimit64" \
  -ex "catch syscall rseq" \
  -ex "run" \
  --args ./ld-linux-x86-64.so.2 --library-path . ./glibc_hello
```

## 探针

新增 `scripts/fixtures/dynamic-glibc-x86_64/glibc_g2.c`，用同一宿主 GCC 16.2.1 /
glibc 2.44 `-O2 -s -fPIE -pie` 编译为 `glibc_g2`（11,200 B，sha256
`339b3624336b8e022b6fbfae2b72d3c75d517e11c903646bc64394516d1d2f19`），
`PT_INTERP=/lib64/ld-linux-x86-64.so.2`，`NEEDED=libc.so.6`。

探针按原始 syscall 调用：

1. `SYS_set_tid_address(&tid_slot)`：要求返回值等于 `SYS_gettid`。
2. `SYS_prlimit64(0, RLIMIT_NOFILE, NULL, &old)`：要求 `rc=0`、`old.cur>0`、
   `old.max>=old.cur`；再以同一组 `[cur, max]` 做一次 set，要求 `rc=0`。
3. `SYS_rseq(&area, sizeof(area), 0, 0)`：接受 `-1/ENOSYS`，打印
   `[G2] RSEQ ENOSYS`；若返回其他值则打印 `[G2] RSEQ UNEXPECTED`。

前两项任一失败程序退出 1；`rseq` 只做载体态取证，不阻断退出码。宿主直跑：

```text
[G2] set_tid_address rc=1480202 errno=0 gettid=1480202
[G2] SET_TID PASS
[G2] prlimit64 rc=0 errno=0 nofile=524288/524288
[G2] prlimit64 set rc=0 errno=0
[G2] PRLIMIT PASS
[G2] rseq rc=-1 errno=22
[G2] RSEQ UNEXPECTED
```

宿主 `rseq` 的 `EINVAL` 是“同一线程已由 glibc 注册过 rseq，第二次注册被拒”，
不是内核缺少入口；真正要看的是 Edgine 串口。

## 内核实现

- canonical 编号：`SYS_SET_TID_ADDRESS=218`、`SYS_PRLIMIT64=302`；aarch64
  `218 ↔ 96`、`302 ↔ 261`，两个方向都加入 `linux_syscall` 映射与 ABI 互逆测试。
- `set_tid_address_syscall(tidptr)`：把 `tidptr` 写入当前进程
  `clear_child_tid`，返回当前 TID；入口不校验指针可写性，退出清理时静默失败，
  与 Linux 语义一致。
- `prlimit64_syscall(pid, resource, new_ptr, old_ptr)`：支持 `pid=0` 当前进程与
  显式当前 PID；非 init 改其他进程返回 `EPERM`；资源号越界/负 pid 返回
  `EINVAL`；用户缓冲不可访问返回 `EFAULT`；目标不存在返回 `ESRCH`；`new`/`old`
  均可为空，空 new 只查询、空 old 只设置。
- `rseq` **不实现、不映射**：`SYS_RSEQ=334` 与 aarch64 `293` 都不进入
  `linux_syscall` 表；aarch64 `293` 保持 `UNKNOWN`，内核入口返回 `-ENOSYS`。
  这样不会把 aarch64 的 `rseq` 误落到任何已实现 syscall。ABI 测试新增
  `aarch64_to_canonical(293) == UNKNOWN` 锁。
- `getrlimit` 仍保留旧的“恒 `RLIM_INFINITY`”实现；G2 只统一 `prlimit64` 的真实
  表读取，未改动旧入口，避免把无关语义混进本切片。

## 实测读数

x86_64 串口（`/bin/glibc_true` → `glibc_hello` → `glibc_g2`）：

```text
[GLIBC] PASS: dynamic glibc execve
hello dynamic glibc
[GLIBC] HELLO PASS: dynamic glibc hello
[G2] set_tid_address rc=29 errno=0 gettid=29
[G2] SET_TID PASS
[G2] prlimit64 rc=0 errno=0 nofile=32/18446744073709551615
[G2] prlimit64 set rc=0 errno=0
[G2] PRLIMIT PASS
[G2] rseq rc=-1 errno=38
[G2] RSEQ ENOSYS
[GLIBC] G2 PASS: startup syscalls
```

aarch64：`[GLIBC] SKIP: no carrier`，无 glibc 载体，G2 腿不执行。

门禁读数：

- `./ci/build.sh all`：`Passed: 6 / Failed: 0`。
- `make test-host`：RC=0，syscall ABI 互逆/未知号锁全过。
- `make test-kernel-host`：RC=0，`638 passed / 0 failed`。
- `bash scripts/qemu_boot_test.sh all`：RC=0，QEMU 双架构 `2/2`；x86_64
  `glibc 动态 true/hello + G2 syscall 通过 (D7 G0/G1/G2)`。
- `bash ci/audit.sh full`：RC=0；审计腿到位 `17/17`、QEMU 双架构 `2/2`、
  未执行腿数 `0`（`/tmp/audit_d7g2_run2.log`）。

## 结论与边界

1. glibc 2.44 启动序列确实调用 `set_tid_address`、`rseq`、`prlimit64`；
   G1 的“未触发”表述已由 G2 更正。
2. `set_tid_address` 与 `prlimit64` 现在返回成功；`rseq` 保持 `ENOSYS`，
   glibc 在该腿接受这个 fallback，没有阻断 hello。
3. `prlimit64` 只做“当前/init 限制”的保守权限面，未实现任意进程的完整
   Linux 权限语义；这是 D7 的单线程边界，等 G4 pthread 再扩。
4. `set_tid_address` 的 `tidptr` 不在入口校验，退出清理失败静默；当前探针与
   glibc 启动调用都传合法用户指针，尚未加独立退出清理集成断言。
5. `rseq` 未注册/未退出，D7 后续若进入 pthread 或 glibc 要求 rseq 注册，
   再单独补 334↔293 映射与注册语义。
6. aarch64 仍无 glibc 载体，G2 只在 x86_64 有实测读数；aarch64 侧的映射与
   `293 -> UNKNOWN` 由 host ABI 测试锁住。
7. `getrlimit` 仍恒返回 `RLIM_INFINITY`，与 `prlimit64` 读真实表暂时不一致；
   这是独立的 rlimit 收敛项，不在 G2 混改。

以上 5 项延后边界（`prlimit64` 跨进程、`rseq`、`set_tid` 退出清理断言、
`getrlimit` 统一、aarch64 G2 载体）已登记在
`docs/plan/dynamic-linker-project.md` 的 `D7-DEFERRED` 表，状态均为 `[ ]`；
后续 G3/G4 或 rlimit 切片开工前必须逐项核对，处理一项才可改 `[X]` 并回填证据。

## 下一步

- G3：先观察 `AT_SYSINFO_EHDR=0` 在 glibc 的 `clock_gettime`/`gettimeofday`
  路径是否继续 fallback；若出现硬依赖再做最小 VDSO。
- G4：`clone3` 常用字段、pthread/TLS、rseq 注册与退出语义，按真实调用面推进。
