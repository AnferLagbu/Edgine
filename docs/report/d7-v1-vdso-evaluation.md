# D7 V1：最小 VDSO 触发条件评估

> 任务：`D7-DEFERRED-V1` — 是否需要在 Edgine 实现最小 VDSO。
> 结论：**当前不实现，维持 `AT_SYSINFO_EHDR = 0`**。截至本次复核，没有任何 glibc 时间/CPU 路径出现 VDSO 硬依赖；实现 VDSO 的成本和新增攻击面没有对应的当前收益。

## 1. 复核基线

- 内核 auxv 构造固定写入 `AT_SYSINFO_EHDR = 0`（`src/kernel/privileged/proc/user_proc.rs`）。
- 全树无 VDSO 映射、VDSO ELF 镜像、VDSO 符号解析或 `getauxval` 专用路径。
- x86_64 夹具为真实 glibc 动态程序；aarch64 夹具为 Debian 13 glibc 2.41 loader/libc（A1 收口）。两架构 G0/G1/G2/G3/G4/C1/L1 均已真跑。

## 2. 无 VDSO 运行证据

`glibc_time` 探针在 `AT_SYSINFO_EHDR=0` 下覆盖：

| 路径 | x86_64 | aarch64 |
|---|---|---|
| `clock_gettime(CLOCK_MONOTONIC)` | `rc=0` | `rc=0` |
| `clock_gettime(CLOCK_REALTIME)` | `rc=0` | `rc=0` |
| `gettimeofday` | `rc=0`，与 `time`/`CLOCK_REALTIME` 一致 | 同左 |
| `time()` | `rc=0`，真实 epoch | `rc=0`，真实 epoch |
| `getcpu` | `rc=0`，`cpu=0` | `rc=0`，`cpu=1` |
| `clock_getres` | `rc=0`，1ms | `rc=0`，1ms |
| 串口判据 | `[GLIBC] TIME PASS: no-vdso fallback` | 同左 |

关键读数（最近一轮 QEMU）：

- x86_64：`time=1795690747`、`consistency time-gettimeofday=0 real-gettimeofday=0`、`[TIME] PASS`。
- aarch64：`time=1795690903`、`consistency time-gettimeofday=0 real-gettimeofday=0`、`[TIME] PASS`。

除此之外，glibc 的启动、pthread、clone3、dlopen 腿在两架构均通过；没有出现“VDSO 缺失导致 loader 启动失败/符号解析失败/时间函数返回错误”的形态。

## 3. 触发条件逐项核对

| 触发条件 | 本次复核 | 结论 |
|---|---|---|
| 新 glibc 版本在启动期硬依赖 VDSO | x86_64 真实 glibc 与 aarch64 glibc 2.41 均正常启动；无 VDSO 时 `AT_SYSINFO_EHDR=0` 被接受 | 未触发 |
| 新时间/CPU 路径必须经 VDSO 才能工作 | `clock_gettime`/`gettimeofday`/`time`/`getcpu`/`clock_getres` 全部 syscall fallback | 未触发 |
| 应用或工具链显式要求 `AT_SYSINFO_EHDR != 0` | 现有夹具与 glibc 运行路径均未检查该值作为硬条件 | 未触发 |
| 性能指标要求绕开 syscall | 当前验收只要求语义正确与真实 epoch，没有 VDSO 性能预算 | 未触发 |
| 新架构/ABI 需要 VDSO 才能表达 | 当前两架构均无该需求 | 未触发 |

## 4. 不实现的理由

最小 VDSO 并不是“映射一页代码”这么简单，最低成本方案也必须同时具备：

1. 两架构各自的 VDSO ELF 镜像与 `PT_LOAD`/`PT_DYNAMIC`/`PT_GNU_EH_FRAME` 等结构；
2. 正确的符号表与版本表（glibc 按符号名/版本查找 `__vdso_*` 或 `__kernel_*`）；
3. 每进程地址随机化后的 VMA 映射、`AT_SYSINFO_EHDR` 写值、fork/exec 生命周期；
4. VDSO 代码自身的时间读取与 syscall fallback 路径；
5. 新增用户可执行页带来的权限/NX/COW/KPTI 回归面。

当前没有消费者需要这些能力。提前实现会扩大用户态可执行面和地址空间管理复杂度，收益仅是“以后可能用得上”，不符合按触发条件推进的原则。

## 5. 复核结论与再触发条件

`D7-DEFERRED-V1` 本次按**评估收口（不实现）**处理：

- 保持 `AT_SYSINFO_EHDR = 0`；
- 保持全部时间/CPU 路径 syscall fallback；
- 不新增 VDSO 镜像、映射或符号解析代码。

出现以下任一条件时重新开工：

- glibc 升级后在启动或时间路径上硬失败，且失败无法通过 syscall fallback 规避；
- 新应用/工具链明确读取 `AT_SYSINFO_EHDR` 并拒绝为 0；
- 性能验收明确要求 `clock_gettime`/`gettimeofday` 绕过 syscall；
- 新增架构或 ABI 需要 VDSO 才能表达时间/CPU 语义。

## 6. 残余风险

- syscall fallback 的每次时间读取有系统调用开销；当前没有性能门槛，故不作为缺陷。
- glibc 未来版本可能改变 VDSO 探测策略；升级夹具时必须重跑本评估的触发条件表。
