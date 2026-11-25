# D7 G1：x86_64 glibc 动态 hello 的 stdout 路径

日期：2026-11-25
关联：`docs/plan/dynamic-linker-project.md` D7 / `docs/report/d7-g0-glibc-true.md`

## 目的

G0 只证明 glibc `true` 能完成启动并 `exit(0)`；G1 用带 stdout 的真实 glibc
动态 hello，把验收面从“装载 + 退出”推进到“装载 + libc stdio + write + 退出”。
判据是子进程 `wstatus == 0` **且**串口出现 `hello dynamic glibc`。

## 载荷

`glibc_hello` 是本目录 `glibc_hello.c` 用宿主 GCC 16.2.1 / glibc 2.44
`-O2 -s -fPIE -pie` 编译，11,176 B，sha256
`9aafb5ee74847ad39d6704447e6197f0545b8894964bd53f9815b1c763c58709`。
`PT_INTERP=/lib64/ld-linux-x86-64.so.2`，`NEEDED=libc.so.6`；loader/libc 与
G0 同一份 Fedora 45 glibc 2.44 副本。

宿主预跑：

```text
$ ld-linux-x86-64.so.2 --library-path . ./glibc_hello
hello dynamic glibc
$ echo $?
0
```

## 探针

`glibc_probe()` 在 G0 的 `true` 腿之后执行第二条腿：

```text
/bin/glibc_true   -> [GLIBC] PASS: dynamic glibc execve
/bin/glibc_hello  -> [GLIBC] HELLO PASS: dynamic glibc hello
```

两条腿都按 `fork` → `execve` → 父进程 `wait_pid_status()` 的 `wstatus == 0`
判定；hello 腿额外要求串口出现 `hello dynamic glibc`。任一 `[GLIBC] FAIL`
阻断；aarch64 无 glibc 载体时打印 `[GLIBC] SKIP: no carrier`，QEMU 门禁接受
SKIP/PASS，不接受 FAIL。

## 实测读数

x86_64 串口日志：

```text
[GLIBC] stat libc rc=0 fd=3 fstat rc=0
[GLIBC] PASS: dynamic glibc execve
hello dynamic glibc
[GLIBC] HELLO PASS: dynamic glibc hello
```

aarch64 串口日志：

```text
[GLIBC] SKIP: no carrier
```

门禁读数：

- `./ci/build.sh all`：`Passed: 6 / Failed: 0`。
- `bash scripts/qemu_boot_test.sh all`：QEMU 双架构 `2/2`，x86_64
  `glibc 最小动态 true/hello 通过 (D7 G0/G1)`；aarch64 SKIP/PASS 合规。
- `make test-host` / `make test-kernel-host`：RC=0（本轮未改内核代码）。
- `bash ci/audit.sh full`：RC=0；审计腿到位、Lockbud 零发现、QEMU 双架构 2/2、
  UEFI 判据通过，本轮未执行腿数 0。

## 本次没有补的 auxv

G1 故意没有先写 `AT_PLATFORM`/`AT_HWCAP*` 再跑，而是先用当前 auxv 取证：

- `AT_PLATFORM` 缺失、`AT_HWCAP=0`、`AT_HWCAP2=0`、`AT_SYSINFO_EHDR=0`
  的现状下，glibc 2.44 的 loader + hello 仍能启动并写出 stdout。
- 因此 `AT_PLATFORM`/`AT_HWCAP*` **不是这条腿的根因**，不预先写死；等
  G4 的 hwcaps/ifunc 或真实失败出现时再补，避免把“猜测的兼容面”混进 ABI。
- `set_tid_address`、`prlimit64`、`rseq` 在本腿也未触发；G2 应先构造能真正
  调用它们的探针，再决定是否实现，而不是先加空桩。

## 边界

- 只覆盖 x86_64；aarch64 仍是 SKIP。
- 只覆盖单线程 `printf("hello dynamic glibc\n")` + `return 0`；没有参数解析、
  locale、NSS、`fopen`/`malloc` 大路径、`dlopen`、pthread、rseq、VDSO。
- `AT_RANDOM` 仍是非密码学派生值；G1 只证明功能可用，不证明安全强度。
- `libc.so.6` 仍只由 loader `mmap`，内核不整文件装载。
- D7 是可选工程；T7 已于 2026-11-25 按方案一收口为 `[X]`，本腿不回退 T7。

## 下一步

1. **G2 取证**：先做一个会调用 `set_tid_address`/`prlimit64` 的 glibc 单线程
   探针，确认是否真的返回 `ENOSYS`；只有真实调用才补入口。
2. **G2 线程**：若 G4 前要碰 pthread，再补 `clone3` 常用字段与 rseq；不把线程
   面混进 G1。
3. **G3 VDSO**：继续观察 `AT_SYSINFO_EHDR=0` 在 `clock_gettime`/`gettimeofday`
   路径上是否真的 fallback 成功，失败再做最小 VDSO。
