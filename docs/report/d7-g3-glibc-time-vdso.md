# D7 G3：glibc 时间路径无 VDSO fallback 取证

日期：2026-11-25
关联：`docs/plan/dynamic-linker-project.md` D7 / `docs/report/d7-g2-glibc-startup-syscalls.md`

## 目的

`AT_SYSINFO_EHDR=0` 是目前 glibc 腿的既定边界。G3 不先实现最小 VDSO，而是用真实
glibc 时间调用确认：没有 VDSO 时，`clock_gettime`/`gettimeofday`/`time`/
`clock_getres`/`getcpu` 是否都能按 syscall fallback 成功。若所有路径都通过，则
G3 的结论是“当前 glibc 单线程时间面不需要 VDSO”，而不是“VDSO 已实现”。

## 载荷

新增 `scripts/fixtures/dynamic-glibc-x86_64/glibc_time.c`，用宿主 GCC 16.2.1 /
glibc 2.44 `-O2 -s -fPIE -pie` 编译为 `glibc_time`（11,232 B，sha256
`1676f0f12b4090a06c687d7d6e6fdc57d57d7b215dfda240a95b731a78d77394`）。
`PT_INTERP=/lib64/ld-linux-x86-64.so.2`，`NEEDED=libc.so.6`。

探针依次调用：

1. `clock_gettime(CLOCK_MONOTONIC/CLOCK_REALTIME)`
2. `gettimeofday`
3. `time`
4. 原始 `SYS_getcpu(cpu*, node*, tcache*)`
5. `clock_getres(CLOCK_MONOTONIC)`

并断言 `time`、`gettimeofday`、`CLOCK_REALTIME` 三者的秒级读数彼此相差不超过
1 秒；否则打印 `[TIME] FAIL`。

宿主直跑（节选）：

```text
[TIME] clock_gettime mono rc=0 errno=0 sec=241637 nsec=652427134
[TIME] clock_gettime real rc=0 errno=0 sec=1795599226 nsec=358462930
[TIME] gettimeofday rc=0 errno=0 sec=1795599226 usec=358467
[TIME] time rc=1795599226 errno=0
[TIME] consistency time-gettimeofday=0 real-gettimeofday=0
[TIME] getcpu rc=0 errno=0 cpu=8 node=0
[TIME] clock_getres rc=0 errno=0 sec=0 nsec=1
[TIME] PASS
```

## 首次 QEMU 读数暴露的两个 ABI 缺口

首轮只断言 `rc == 0` 时，时间腿打印了：

```text
[TIME] clock_gettime real rc=0 ... sec=11
[TIME] gettimeofday rc=0 ... sec=11
[TIME] time rc=11536
```

`time()` 比墙钟大约 1000 倍，根因是 `functions::fs::misc::time_syscall()` 直接返回
`api::get_ticks()`（毫秒计数），而不是墙钟秒；`gettimeofday`/`CLOCK_REALTIME` 走
`time_sync` 的墙钟纳秒。修复为新增 `timer::clock::wall_clock_seconds()`，让
`time()` 与 `gettimeofday()` 共用同一墙钟口径，并保留 `tloc` 写回。

同一轮把原始 `SYS_getcpu` 加入探针后，发现旧实现把所有参数忽略、返回
`cpu | node << 32` 的打包值，既不写用户 `cpu*`/`node*`，也不符合 Linux
`getcpu(unsigned *cpu, unsigned *node, struct getcpu_cache *tcache)`。修复为
`sys_getcpu(cpu_ptr, node_ptr, tcache_ptr)`：分别写回 32 位 CPU/节点，任一指针
可为空；`tcache` 只做 8 字节可访问性检查，当前不消费；非法用户指针返回
`EFAULT`。

## 修复后 QEMU 读数

x86_64 串口：

```text
[TIME] clock_gettime mono rc=0 errno=0 sec=12 nsec=642000000
[TIME] clock_gettime real rc=0 errno=0 sec=12 nsec=710000000
[TIME] gettimeofday rc=0 errno=0 sec=12 usec=710000
[TIME] time rc=12 errno=0
[TIME] consistency time-gettimeofday=0 real-gettimeofday=0
[TIME] getcpu rc=0 errno=0 cpu=0 node=0
[TIME] clock_getres rc=0 errno=0 sec=0 nsec=1000000
[TIME] PASS
[GLIBC] TIME PASS: no-vdso fallback
```

aarch64 仍无 glibc 载体，打印 `[GLIBC] SKIP: no carrier`。

门禁读数：

- `./ci/build.sh all`：`Passed: 6 / Failed: 0`，日志 `/tmp/build_d7g3_run3.log`。
- `bash scripts/qemu_boot_test.sh all`：RC=0，QEMU 双架构 `2/2`；x86_64
  `glibc 动态 true/hello + G2 syscall + G3 时间 fallback 通过 (D7 G0/G1/G2/G3)`，
  日志 `/tmp/qemu_d7g3_run3.log`。
- `make test-kernel-host`：RC=0，`638 passed / 0 failed`，日志 `/tmp/host_d7g3.log`。
- `bash ci/audit.sh full`：RC=0，审计腿 `11/11`，未执行腿 `0`，日志
  `/tmp/audit_d7g3.log`；`python3 tools/check_doc_paths.py` RC=0。

## 结论与边界

1. `AT_SYSINFO_EHDR=0` 下，glibc 2.44 的 `clock_gettime`/`gettimeofday`/`time`/
   `clock_getres`/`getcpu` 都能走 syscall fallback；**G3 不实现最小 VDSO**。
2. G3 同时修复了 `time()` 返回毫秒 tick 和 `SYS_getcpu` 不写用户指针两个 ABI
   缺口；这不是 VDSO 代码，而是 syscall fallback 的前提。
3. 墙钟仍是 tick 派生值（QEMU 中 `CLOCK_REALTIME` 为启动后秒数），不是真实
   RTC epoch；G3 只证明 fallback 与三入口一致性，不声明真实纪元。该限制登记为
   `D7-DEFERRED-T1`。
4. 只覆盖 x86_64；aarch64 没有 glibc 载体，运行时仍 SKIP。
5. `getcpu` 的 `tcache` 只做指针检查、不消费；glibc fallback 传 NULL，当前足够。

## 下一步

- G4：`clone3` 常用字段、pthread/TLS、`rseq` 注册与退出语义，按真实 pthread
  调用面推进。
- G3 若后续出现新的 glibc 时间路径失败，再按 `D7-DEFERRED-V1` 重开最小 VDSO；
  不把“当前未失败”误写成“VDSO 已实现”。
