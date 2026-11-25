# D7 G4：glibc pthread / clone3 / TLS 取证

日期：2026-11-25
关联：`docs/plan/dynamic-linker-project.md` D7 / `docs/report/d7-g3-glibc-time-vdso.md`

## 目的

G0–G3 已证明 glibc 能在 `AT_SYSINFO_EHDR=0` 下启动、写 stdout、走时间 syscall
fallback。G4 的第一条腿验证更难的线程面：真实 glibc 2.44 的
`pthread_create`/TLS/`pthread_join` 是否可用；并确认它是否走 `clone3`、`clone3`
的栈参数是否按 Linux 约定转换。

本切片只覆盖 pthread、`clone3` 常用栈字段和 TLS；`dlopen` 多共享对象留到
G4 后续腿（`D7-DEFERRED-L1`），不在本报告声称通过。

## 载荷

新增 `scripts/fixtures/dynamic-glibc-x86_64/glibc_pthread.c`，用宿主 GCC 16.2.1 /
glibc 2.44 `-O2 -s -fPIE -pie -pthread` 编译为 `glibc_pthread`（11,344 B，
sha256 `466bd228a8f0c7648a1e73b736c35c96697563168ae4aa08de39b3d271868c08`）。
`PT_INTERP=/lib64/ld-linux-x86-64.so.2`，`NEEDED=libc.so.6`。

探针覆盖：

1. `pthread_self` / `gettid` / `__thread` 初始值与主线程 TLS；
2. `pthread_key_create` / `pthread_setspecific`；
3. `pthread_create` 新线程，线程内读取自己的 TLS、写 TLS、取 key；
4. `pthread_mutex_lock/unlock` 保护共享计数；
5. `pthread_join` 校验返回值 `0x1234`、共享计数 `1`、主线程 TLS 仍为 `11`。

宿主直跑（节选）：

```text
[PTHREAD] main self=... tid=... tls=11
[PTHREAD] key_create rc=0 errno=0
[PTHREAD] setspecific rc=0 errno=0
[PTHREAD] create rc=0 errno=0
[PTHREAD] worker self=... tid=... tls=7
[PTHREAD] worker key=-1
[PTHREAD] join rc=0 errno=0 ret=0x1234 shared=1 tls=11
[PTHREAD] PASS
```

`worker key=-1` 是预期：`pthread_key` 的值是每线程独立的，主线程设置的
`99` 不会自动继承到新线程；探针只验证 key API 可用，不要求跨线程继承。

## clone3 栈参数修复

`clone3` 的 `struct clone_args.stack` 是**栈底**，`stack_size` 是大小；Linux
按 `stack_size` 设置子线程栈，`clone(2)` 的 `child_stack` 才是初始 RSP。内核
原实现直接把 `args.stack` 传给 `clone_syscall`，等价于把栈底当栈顶，`stack_size`
完全未消费。

glibc 自己的 `__clone_internal` fallback 也按这个口径计算：

```text
rsi = args.stack + args.stack_size
call __clone
```

修复：

- 新增 `functions::proc::clone::clone3_stack_top(stack, stack_size)`：
  - `stack == 0 && stack_size == 0` → `0`（fork 语义，子进程沿用父栈）；
  - `stack == 0 && stack_size != 0` → `EINVAL`（有栈大小但无栈底）；
  - `stack_size == 0` → 保留“stack 即 RSP”的 clone(2) 口径；
  - 非零 → `stack + stack_size`，溢出返回 `EINVAL`。
- `dispatch_proc_create` 的 `SYS_CLONE3` 改用该函数，再委托
  `clone_syscall`；另拒绝 `CLONE_VM` 且 `child_stack == 0` 的组合（线程必须
  有显式栈，不能继承父栈）。
- 补 `clone3_stack_top` 的零长度、正常换算、无栈底有大小、溢出单测。
- 调试期串口确认 glibc 实际走 `clone3`：

```text
[KERN] clone3: flags=0x3D0F00 stack=0x5555557A4000 size=0x7FFF80 exit=0 tls=0x555555FA46C0
[KERN] clone3: ret=Ok(35)
```

`flags=0x3D0F00` 即 `CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_THREAD|
CLONE_SYSVSEM|CLONE_SETTLS|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID` 的组合；
`ret=Ok(35)` 是子线程 TID。日志仅用于本轮取证，已从最终代码移除，避免每线程
刷屏。

## 修复后 QEMU 读数

x86_64 串口：

```text
[PTHREAD] main self=93824994701760 tid=33 tls=11
[PTHREAD] key_create rc=0 errno=0
[PTHREAD] setspecific rc=0 errno=0
[PTHREAD] create rc=0 errno=22
[PTHREAD] worker self=93825003095744 tid=35 tls=7
[PTHREAD] worker key=-1
[PTHREAD] join rc=0 errno=0 ret=0x1234 shared=1 tls=11
[PTHREAD] PASS
[GLIBC] PTHREAD PASS: glibc pthread
```

`pthread_create` 的返回值为 `0` 即成功；`errno=22` 是 glibc 内部探测/其他调用
留下的值，不是 `pthread_create` 的返回码，探针和门禁只按返回值/功能判定。
该 errno 来源未单独定位，若要严格“返回后 errno 归零”应另开探针，不并入本腿
（已知观察，不构成功能阻断）。

门禁读数：

- `./ci/build.sh all`：`Passed: 6 / Failed: 0`，日志 `/tmp/build_d7g4_final3.log`。
- `bash scripts/qemu_boot_test.sh all`：RC=0，QEMU 双架构 `2/2`；x86_64
  `glibc 动态 true/hello + G2 syscall + G3 时间 + G4 pthread 通过
  (D7 G0/G1/G2/G3/G4)`，日志 `/tmp/qemu_d7g4_final3.log`。
- `make test-kernel-host`：RC=0，`638 passed / 0 failed`（另有 functions
  `817 passed`、独立 `12 passed`），日志 `/tmp/host_d7g4_final3.log`。
- `bash ci/audit.sh full`：RC=0，审计腿 `11/11`，未执行腿 `0`，日志
  `/tmp/audit_d7g4_run2.log`。
- `python3 tools/check_doc_paths.py`：RC=0。

## 结论与边界

1. 真实 glibc 2.44 的 `pthread_create`/TLS/`pthread_join`/`pthread_mutex` 在
   当前内核上通过；glibc 实际走 `clone3`，不是只靠 `clone` fallback。
2. `clone3` 的栈字段已按 Linux 语义换算；`stack_size` 不再被忽略。
3. `rseq` 仍是 `ENOSYS`；本腿证明 glibc pthread 当前可接受该 fallback。
   `D7-DEFERRED-R1` 保持 `[ ]`，不据此实现半套 rseq。
4. `clone3` 的 `pidfd`/`exit_signal`/`set_tid`/`cgroup` 仍未消费，登记为
   `D7-DEFERRED-C1`。
5. `dlopen`/多共享对象未验证，登记为 `D7-DEFERRED-L1`；G4 后续腿再开。
6. aarch64 仍无 glibc 载体，`[GLIBC] SKIP: no carrier`。

## 下一步

- G4 后续：`dlopen`/`dlsym`/`dlclose` 第二个 `.so` 装载与重定位链。
- 或按需先处理 `clone3` 高级字段（`CLONE_PIDFD`、`exit_signal`）。
