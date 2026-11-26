# D7 T1：真实 RTC / 墙钟 epoch 收口报告

> 状态：2026-11-26 收口。`CLOCK_REALTIME` / `gettimeofday` / `time()` 已从平台 RTC 取得真实 Unix epoch；`settimeofday` 与 glibc 实际使用的 `clock_settime` 同源，并在成功后尽力回写 RTC。x86_64 与 aarch64 QEMU 启动均通过 epoch 合理性门禁。

## 1. 目标与结论

T1 原先的墙钟是 tick 派生值：`time()` 返回自启动秒数，不是真实日期。收口后：

- 启动时读取平台 RTC，把 Unix epoch 作为 `TimeSync` 的 `epoch_base_ns`；
- `CLOCK_REALTIME` / `gettimeofday` / `time()` 都走同一条墙钟计算；
- `CLOCK_MONOTONIC` 继续直接使用 uptime，不受 epoch 基准影响；
- `settimeofday` / `clock_settime` 成功设置墙钟后尽力回写 RTC；
- RTC 不可用或值不合理时保留原行为并记录告警，不阻塞启动。

## 2. RTC 硬件路径

### 2.1 x86_64：CMOS RTC

文件：`src/kernel/privileged/timer/rtc.rs`

- I/O 端口 `0x70`（索引）/ `0x71`（数据）；写入索引时保留调用方当前 NMI 屏蔽位。
- 读取前等待状态 A 的 UIP 位清零，并在读取前后比较秒字段；跨秒回卷自动重试，最多 3 次。
- 支持 BCD / 二进制、12 / 24 小时制、PM 位与世纪寄存器 `0x32`。
- 世纪寄存器缺失时按两位年份回退：`< 70 → 20xx`，否则 `19xx`。
- 回写时读取状态 B，按现有 BCD/二进制与 12/24 小时模式编码；写入后回读并允许 3 秒计数推进。
- `host-test` 下硬件路径短路：宿主 Linux 执行 `inb` 会 SIGSEGV；x86 CMOS 由 QEMU 启动腿验收，纯解码/日期换算由 host 单测覆盖。

### 2.2 aarch64：PL031 RTC

- 设备树扫描 `compatible` 含 `arm,pl031` 的节点，记录 `reg` 首地址；QEMU virt 缺省回退 `0x0901_0000`。
- 通过 `IoMem` 映射 4 KiB 寄存器帧，`IrqSpinLock<Option<IoMem>>` 缓存句柄；首次访问确保控制寄存器 bit0 使能。
- 读 `RTCDR`（偏移 `0x00`）得到 32 位 Unix 秒；写按“关计数器 → 写 `RTCLR` → 重新使能”的顺序，随后回读校验。
- QEMU virt 的 PL031 从 0 起算，不是宿主墙钟。启动门禁通过 `-append edgine.rtc_epoch=<host秒>` 注入引导器提示；真实硬件由固件/电池后备值提供。

### 2.3 引导器 epoch 提示

文件：`src/kernel/privileged/dtb.rs`、`boot/aarch64/entry.rs`

- `/chosen/edgine,rtc-epoch`（单元格）或 `/chosen/bootargs` 中的 `edgine.rtc_epoch=SECONDS`；
- 启动时若 RTC 低于 `2020-01-01`（`1_577_836_800`）则优先采用提示；
- 两者都没有时保留 RTC 低值，只记录告警，不伪造时间；
- 提示只用于 RTC 未初始化/无电池的平台，正常 RTC 路径优先。

## 3. 墙钟基准与设置语义

文件：`src/kernel/privileged/timer/time_sync.rs`、`src/kernel/functions/timer/time_sync.rs`

`ClockAdjState` 新增 `epoch_base_ns`：

- `get_adjusted_time_ns() = epoch_base_ns + uptime_ns + total_offset_adj + freq_compensation`；
- 频率补偿仍以 uptime 为时间轴，`last_sync_time` 存 uptime，避免把 epoch 基准当成 elapsed；
- `set_boot_epoch(epoch_ns)` 只设置基准并清空调整状态，不把 `synced` 置位（RTC 不是 NTP 证据）；
- `set_time(time_ns)` 以当前墙钟为参照把差值并入 `total_offset_adj`，因此 `CLOCK_MONOTONIC` 不变。

`settimeofday` / `clock_settime`：

- `apply_wall_clock_ns(ns)` 统一做特权检查、`set_time`、RTC 回写；
- `clock_settime` 仅支持 `CLOCK_REALTIME`，`CLOCK_MONOTONIC` 返回 `EINVAL`；
- 用户态 `struct timespec` 读取失败/空指针 `EFAULT`，负秒或 `tv_nsec` 越界 `EINVAL`；
- RTC 回写失败不回滚已成功的系统时间设置（Linux `settimeofday` 同语义）。

## 4. Syscall / ABI

- canonical `SYS_CLOCK_SETTIME = 227`，aarch64 `112`；`src/rust/edgine-abi/src/linux_syscall.rs` 双向映射。
- `src/kernel/privileged/syscall/types.rs` 增加常量并纳入编号检查列表。
- `src/kernel/functions/syscall/dispatch.rs` 增加 `SYS_CLOCK_SETTIME` 分支。
- host ABI 用例新增 `(227, 112)`；`aarch64_to_canonical(112) == 227`，不再落入 UNKNOWN。

首轮 G2 时间探针在 QEMU 内 `settimeofday` 返回 `ENOSYS`，根因是 glibc 把 `settimeofday` 转发到 `clock_settime`，而内核只实现了前者的编号。补 `SYS_CLOCK_SETTIME` 后，探针输出 `set rc=0 errno=0`。

## 5. 探针与门禁

### 5.1 `glibc_time`（x86_64）

文件：`scripts/fixtures/dynamic-glibc-x86_64/glibc_time.c`

新增：

- `[TIME] epoch now=<sec> floor=1735689600`，低于 2025-01-01 即失败；
- `clock_gettime(CLOCK_REALTIME)` / `gettimeofday` 也必须高于该门槛；
- root 下 `settimeofday(now+2)` → 读回 → 恢复原值，输出 `[TIME] SET PASS`；非 root 宿主跳过该腿。

QEMU 实测：

```text
[BOOT] RTC wall clock epoch: 1795682291s (UTC, source=rtc)
[TIME] epoch now=1795682301 floor=1735689600
[TIME] set rc=0 errno=0 target=1795682301 after=1795682301 restore=0
[TIME] SET PASS
```

宿主运行 `rc=0`，输出 `[TIME] EPOCH PASS` / `[TIME] PASS`；宿主非 root，不执行 SET 腿。

### 5.2 QEMU 门禁

文件：`scripts/qemu_boot_test.sh`

- `rtc_epoch_check(arch, log)`：串口必须出现 `RTC wall clock epoch: <sec>` 且 `<sec> >= 1735689600`；双架构共用。
- x86_64 glibc 门禁新增 `[TIME] EPOCH PASS`、`[TIME] SET PASS`，并禁止 `[TIME] SET FAIL`。
- aarch64 启动参数新增 `-append "edgine.rtc_epoch=$(date +%s)"`，覆盖 PL031 未初始化场景。
- QEMU 实测：
  - x86_64 `1/1` 通过，epoch `1795682291s (source=rtc)`；
  - aarch64 `1/1` 通过，epoch `1795681377s (source=hint)`；
  - x86_64 连续多次启动 epoch 分别为 `1795680845`、`1795681625`、`1795681910`、`1795682291`，均随宿主时间推进，满足跨启动真实 epoch 取证。
- aarch64 e2e 软观测不受影响，`3/3` 回显通过。

### 5.3 单测

- `src/kernel/privileged/timer/rtc.rs` 新增 8 项 host 单测：epoch 零点、2000-01-01、2024-02-29、2026-11-26、往返换算、非法字段、BCD/二进制/12 小时/世纪回退。
- `src/kernel/privileged/dtb.rs` 增加 PL031 与 `bootargs` epoch 提示解析断言。
- `make test-kernel-host`：`12 + 820 + 648 = 1480 passed / 0 failed`。
- `make test-host`：`1676 passed / 0 failed`。

## 6. 最终验证读数

- `./ci/build.sh all`：**7 / 0**
- K-08 x86_64：`_kernel_end_phys = 0x3F0000`，margin `0x10000` = **64 KiB**（门禁 32 KiB）
- `make test-kernel-host`：**1480 passed / 0 failed**
- `make test-host`：**1676 passed / 0 failed**
- `qemu_boot_test.sh x86_64`：**1/1**，RTC、epoch、settimeofday 腿全绿
- `qemu_boot_test.sh aarch64`：**1/1**，epoch 提示腿绿
- `bash ci/audit.sh full`：**RC=0**（审计腿到位 17/17、P-49 存量腿 11/11、QEMU 双架构 2/2、thread-stress 6/6、Lockbud 4/4、环境缺失未执行 0）
- 静态锁序：取锁点 **1327**（已分类 456 / 未分类 871，基线 871）/ 函数 **10668** / 违规 0；`rtc.rs::PL031` 为新增未分类点，已用 `python3 scripts/audit_deadlock_matrix.py --update-lock-order-baseline` 显式登记

## 7. 限制与后续

- RTC 按 UTC 使用，不处理本地时区/夏令时；需要时区语义时另立配置面。
- aarch64 QEMU 的 PL031 从 0 起算，CI 依赖 `edgine.rtc_epoch` 提示；真实硬件应有固件/电池后备值，缺失时行为为“低值 + 告警”。
- RTC 回写是 best-effort，失败只影响下次启动的初值，不影响本次 `CLOCK_REALTIME`。
- 未实现 NTP/PTP 对 RTC 的周期性校准；`synced` 仍只由时间同步路径置位。
- 未实现 `clock_adjtime` 的 RTC 联动；`adjtimex` 只调整系统墙钟。

## 8. 变更文件

- 新增：`src/kernel/privileged/timer/rtc.rs`
- 修改：
  - `src/kernel/privileged/timer/mod.rs`
  - `src/kernel/privileged/timer/time_sync.rs`
  - `src/kernel/privileged/dtb.rs`
  - `src/kernel/privileged/boot/aarch64/entry.rs`
  - `src/kernel/privileged/syscall/types.rs`
  - `src/kernel/functions/timer/clock.rs`
  - `src/kernel/functions/timer/time_sync.rs`
  - `src/kernel/functions/syscall/dispatch.rs`
  - `src/kernel/lib.rs`
  - `src/rust/edgine-abi/src/linux_syscall.rs`
  - `host-tests/tests/syscall_abi_boundary_test.rs`
  - `scripts/fixtures/dynamic-glibc-x86_64/glibc_time.c`
  - `scripts/fixtures/dynamic-glibc-x86_64/glibc_time`
  - `scripts/fixtures/dynamic-glibc-x86_64/README.md`
  - `scripts/fixtures/dynamic-glibc-x86_64/SHA256SUMS`
  - `scripts/qemu_boot_test.sh`
  - `docs/plan/dynamic-linker-project.md`
  - `docs/plan/progress-active-tasks.md`
  - `docs/explain/ref-lock-order.md`
- 证据日志：
  - `other/build/log/qemu_boot_x86_64.log`
  - `other/build/log/qemu_boot_aarch64.log`
  - `other/tests-reports/kernel_host_test_20261126_163955.log`
  - `other/tests-reports/host_test_20261126_164046.log`
