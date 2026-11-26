# D7 A1：aarch64 glibc 运行时载体收口

> 任务：`D7-DEFERRED-A1` — 为 aarch64 补齐真实 glibc 运行时载体，让 G2/G3/G4/C1/L1 在 aarch64 上不再只有 SKIP。
> 结论：**已收口**。aarch64 QEMU 串口 `[GLIBC] PASS` / `HELLO PASS` / `G2 PASS` / `TIME PASS` / `PTHREAD PASS` / `CLONE3 PASS` / `DLOPEN PASS` 全绿，x86_64 回归保持全绿。

## 1. 载体构成

使用 Debian 13（trixie）arm64 的真实 glibc 2.41 运行时，不自行编译 libc：

| 文件 | 来源 | 大小 | 说明 |
|---|---|---:|---|
| `ld-linux-aarch64.so.1` | `libc6 2.41-12+deb13u5` | 201,344 B | glibc 动态解释器 |
| `libc.so.6` | 同上 | 1,716,616 B | glibc 运行库 |
| `glibc_true` | Debian `coreutils 9.7-3` `/usr/bin/true` | 68,048 B | G0 最小动态程序 |
| `glibc_hello` | 交叉编译 | 68,040 B | G1 stdout |
| `glibc_g2` | 交叉编译 | 68,080 B | G2 原始 syscall 探针 |
| `glibc_time` | 交叉编译 | 68,112 B | G3 时间路径探针 |
| `glibc_pthread` | 交叉编译 | 68,208 B | G4 pthread/TLS/rseq |
| `glibc_clone3` | 交叉编译 | 68,136 B | C1 clone3 高级字段 |
| `glibc_dlopen` | 交叉编译 | 68,096 B | L1 第二共享对象 |
| `libdlprobe.so` | 交叉编译 | 66,888 B | `dlopen` 夹具，`NEEDED` 为空 |

全部 10 个文件记录在 `scripts/fixtures/dynamic-glibc-aarch64/SHA256SUMS`；来源、编译命令、ELF 事实与 initramfs 布局见同目录 `README.md`。七个探针均带 `PT_INTERP=/lib/ld-linux-aarch64.so.1`。

交叉工具链：Fedora 45 `gcc-aarch64-linux-gnu 16.2.1` + Debian trixie arm64 sysroot。编译参数使用 `--sysroot`、`-isystem .../usr/include/aarch64-linux-gnu`、`-L.../usr/lib/aarch64-linux-gnu`、`-fno-link-libatomic`；链接使用 `-O2 -s -fPIE -pie -Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1`。

## 2. 接入面

- `Makefile`：`GLIBC_FIXTURE_DIR` 按 `$(ARCH)` 取 `scripts/fixtures/dynamic-glibc-$(ARCH)`；loader 源按架构取 `ld-linux-x86-64.so.2` / `ld-linux-aarch64.so.1`；`GLIBC_PREBUILT`/`GLIBC_CPIO_DEPS`/拷贝规则双架构共用。
- `scripts/make_initramfs_cpio.py`：新增 aarch64 `/lib/ld-linux-aarch64.so.1`、`/lib/libc.so.6`；`collect_entries(src_dir, arch)` 跳过另一架构的 libc/loader；CLI 增加 `--arch`。
- `src/user/init/src/main.rs`：`GLIBC_LIBC_PATH` 按架构取 `/lib64/libc.so.6`（x86_64）/ `/lib/libc.so.6`（aarch64），仅用于静态观测。
- `scripts/qemu_boot_test.sh`：aarch64 glibc 门禁从“允许 SKIP/PASS”改为与 x86_64 同口径，要求全部子项 `PASS` 且禁止对应 `FAIL`。

## 3. 三个根因与修复

### 3.1 EL0 取指翻译缺页被当成不可恢复异常

首轮七个探针全部在 loader 首次取指处报：

`EL0 sync fault: ESR=0x82000007 FAR=...BF40 ELR=...BF40`

`ESR=0x82000007` 即 `EC=0x20`（EL0 指令中止）、`IFSC=0b0001`（L3 翻译故障）。glibc loader 用 `mmap` 按需映射 `libc.so.6`，首次取指必然缺页；旧 `A64FaultClass::from_esr` 把 EL0 取指异常一律归 `Other`，直接终止进程。

修复：仅 `EC=0x20` 且 `IFSC=0b0001` 归类 `Translation`，进入 `handle_user_page_fault`；`EC=0x20` 的权限/访问标志故障、同 EL 取指仍归 `Other`。VMA 不存在仍 `SignalSegv`，NX VMA 由既有 `handle_vma_fault_with_mm` 拒绝。同步更新 `exception.rs` 注释与 `test_a64_fault_class`。

### 3.2 aarch64 glibc `time()` 使用 `CLOCK_REALTIME_COARSE`

修复取指后，`[TIME]` 报 `time rc=0 errno=22`、`epoch now=0`，而 `clock_gettime(CLOCK_REALTIME)` 与 `gettimeofday` 正常。反汇编 aarch64 libc 的 `time@@GLIBC_2.17` 可见其调用 `__clock_gettime(5, &ts)`，即 `CLOCK_REALTIME_COARSE=5`；内核 `clock_now_ns` 原先仅支持 0/1。

修复：`clock.rs` 新增 `CLOCK_REALTIME_COARSE=5` / `CLOCK_MONOTONIC_COARSE=6`，分别映射墙钟/单调读数；`posix_timer.rs` 的 `sys_clock_getres` 同步接受 5/6，分辨率仍 1ms。`clock_settime` 仍只接受 `CLOCK_REALTIME`。

### 3.3 aarch64 主线程 TPIDR_EL0 被调度恢复覆盖成 0

`[PTHREAD]` 的 worker 能启动并正常退出，但 `pthread_join` 前后主线程触发：

`ESR=0x92000044 FAR=0xFFFFFFFFFFFFF9D8 ELR=0x7FFFF7072990`

`FAR` 是负地址，且 `ELR` 落在 libc 访问 `[TP, #-0x628]` 的代码上，说明 `TPIDR_EL0=0`。aarch64 glibc 主线程直接 `msr tpidr_el0` 设置 TLS，不经过 `arch_prctl`；内核 `Process::tls_base` 仍是 0。首次切到 worker 再切回主线程时，调度器用 0 恢复 `TPIDR_EL0`，主线程 TLS 访问即落入负地址。

`glibc_clone3` 子进程同源：父进程 `tls_base` 未同步，`clone3` 若未显式带 TLS 时子进程继承 0。

修复分两层：

1. `MmuArch` 新增 `read_user_tls_base()`（aarch64 `mrs tpidr_el0`，x86_64 `rdmsr IA32_FS_BASE`）。
2. aarch64 syscall 入口 `proc_save_user_regs_aarch64` 与调度器切走 `prev` 前，都把当前真实 `TPIDR_EL0` 收回 `Process::tls_base`；随后 `clone/fork` 继承与 `context_switch` 恢复均使用该真实值。

## 4. 验收读数

aarch64 QEMU（最新一轮）：

- `[GLIBC] PASS: dynamic glibc execve`
- `[GLIBC] HELLO PASS: dynamic glibc hello`
- `[GLIBC] G2 PASS: startup syscalls`，含 `SET_TID` / `PRLIMIT` / `GETRLIMIT` / `RSEQ` 各 `PASS`
- `[TIME] time rc=1795688212 errno=0`、`epoch now=1795688212 floor=1735689600`
- `[TIME] EPOCH PASS` / `[TIME] SET PASS`（`target=1795688215 after=1795688215 restore=0`）/ `[TIME] PASS`
- `[PTHREAD] join rc=0 errno=0 ret=0x1234 shared=1 tls=11` / `[PTHREAD] RSEQ EXIT PASS` / `[PTHREAD] PASS`
- `[CLONE3] PASS`（`exit_signal` / `pidfd` / `size=64` / `set_tid ENOSYS` / `cgroup ENOSYS` / `exit_signal=65 EINVAL` 全符合）
- `[GLIBC] CLONE3 PASS` / `[GLIBC] DLOPEN PASS`
- `[BOOT] RTC wall clock epoch: 1795688212s (UTC, source=hint)`

x86_64 回归：

- `[GLIBC] TIME PASS` / `[GLIBC] PTHREAD PASS` / `[GLIBC] CLONE3 PASS` / `[GLIBC] DLOPEN PASS`
- `[BOOT] RTC wall clock epoch: 1795688001s (UTC, source=rtc)`

构建与测试：

- `./ci/build.sh all`：**7 passed / 0 failed**；K-08 `_kernel_end_phys=0x3F0000`，margin `0x10000` = 64 KiB。
- `make test-kernel-host`：**12 + 820 + 648 = 1480 passed / 0 failed**。
- `make test-host`：**1676 passed / 0 failed**。
- `qemu_boot_test.sh x86_64`：1/1；`qemu_boot_test.sh aarch64`：1/1。
- `bash ci/audit.sh full`：**RC=0**，审计腿到位 **17/17** + P-49 **11/11**、QEMU **2/2**、thread-stress **6/6**、Lockbud **4/4**、未执行 **0**；静态锁序 **1327 取锁点（已分类 456 / 未分类 871，基线 871）/ 10671 函数 / 违规 0**。

修复过程中一次 full audit 曾因 `src/user/init/src/main.rs` 的 doc 注释裸写 `x86_64` 触发 `clippy::doc_markdown` 而失败；加反引号后复跑 RC=0。

## 5. 边界

- `TPIDR_EL0` 回收只发生在 aarch64 syscall 入口与调度器切走点；用户态纯计算期间内核字段允许滞后，但下一次陷入/切换前会同步。若后续出现不经 syscall/IRQ 的核间 TLS 读取需求，需再评估。
- `CLOCK_REALTIME_COARSE` / `CLOCK_MONOTONIC_COARSE` 复用 1ms 读数，不提供 Linux 的 tick 粒度差异；当前只影响精度，不影响语义。
- aarch64 glibc 载体现为预编译夹具（2.1 MiB），不进入内核镜像；initramfs 由 Makefile 按架构组装。
- `D7-DEFERRED-V1` 仍为 `[ ]`；A1 未出现 VDSO 硬依赖。
