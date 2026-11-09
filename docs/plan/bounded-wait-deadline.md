# 驱动等待改造：墙钟截止期（借鉴 Linux `read_poll_timeout` 语义）

> 把驱动栈与 SMP 核启动中的"迭代次数预算"忙等统一改写为墙钟截止期，时基未建立时回退调用方原有迭代上限；新原语 `Deadline` / `poll_until_ms` 落 `privileged::timer`，全仓 40 处等待点完成转换，并登记刻意不转换的等待点及其理由。

## 1. 立场与语义

### 1.1 病灶

驱动轮询的旧形态普遍是 `while timeout > 0 { ...; timeout -= 1 }` 或 `elapsed += 每次延时`：预算是**迭代次数**，而每轮物理耗时随主频、编译器优化、缓存状态与系统负载浮动。同一个上限在不同机器上可能对应几毫秒，也可能对应几十秒——既可能把慢设备误判为超时，也可能让"有上限"形同虚设。`aarch64/smp_init.rs` 的 1 亿轮等待与 `virtio-blk`/PS/2 的完全无界 `loop {}` 是同一问题的两个极端。

### 1.2 Linux 参考

- `read_poll_timeout`：以 `ktime` 截止期为权威，条件先于截止期判定；**到期后再读一次条件**（恰在截止瞬间就绪的设备不被误判超时）。
- `read_poll_timeout_atomic`：用于不可睡眠上下文；timekeeping 不可用时放弃时间维度，退化为延时累计——**宁可等完旧上限，也不编造频率去折算时长**。

### 1.3 Edgine 对应

- 时基源：`tick::get_uptime_ns()`（硬件计数器派生，自旋期间不依赖中断到达），能力查询 `tick::timebase_is_hardware()`。
- 回退立场（用户裁定）：时基未建立时**保留调用方原有迭代上限**，与 Linux atomic 变体的降级同构。
- 忙等待语义：不睡眠、不调度，供启动期/中断上下文/不可调度上下文使用；需要让出 CPU 的等待仍走 `timer::sleep::wait_with_timeout`。
- `budget_ns == 0` 定义为非阻塞单次探测。

### 1.4 原语形态

`src/kernel/privileged/timer/wait.rs` 提供两种形态：

| 形态 | 适用 | 语义 |
|---|---|---|
| `poll_until_ns` / `poll_until_ms(budget, fallback_iters, cond)` | 纯自旋轮询（条件可收进闭包） | 条件 → 截止期 → 到期最后重读；零预算 = 单次探测 |
| `Deadline::after_ns` / `after_ms(budget, fallback_iters)` + `expired()` | 每轮需要硬件延时或复杂状态机的自定义循环体 | 时基模式比墙钟；回退模式按 `expired()` 调用次数逼近旧迭代上限 |

时基是否可用在**构造时快照**（`Deadline`）或单次判定（`poll_until_*`），避免一次等待中途切换语义。`privileged::timer` 在 `scripts/audit_functions_boundary.py` 的 `SAFE_PRIVILEGED_APIS` 白名单内，functions 侧可直接调用。

## 2. 工程条目

- **W-1 墙钟等待原语与单测**
  - 描述：新增 `privileged/timer/wait.rs`，导出 `Deadline` / `poll_until_ns` / `poll_until_ms`；覆盖零预算、回退计数、墙钟主导、到期最后重读、计数器回退防御五类语义。
  - 方案：主机侧单测通过注入 `time_based` 与假时钟闭包覆盖两条路径，不触碰全局时基（tick 单测另有 `FREQ_TEST_LOCK`）。
  - 状态：[X]

- **W-2 存储栈转换**
  - 描述：NVMe 控制器状态/命令完成、AHCI 链路与引擎/命令/复位、ATA BSY/DRQ、MSI-X 自检 ISR 等待全部改为墙钟截止。
  - 方案：有规范时限的按规范取预算（NVMe enable/disable 5 s）；纯自旋用 `poll_until_ms`，带 400 ns/AHCI 沉降延时的自定义循环用 `Deadline`；原迭代上限原值作回退。AHCI 链路等待分两段：先等设备检出（`DET` 1..=3，窗口 100 ms），检出后才进入 5 s 链路预算 —— 空端口不烧满预算，见 §3.2。
  - 状态：[X]

- **W-3 网络 / 输入 / USB 转换**
  - 描述：e1000 复位/链路/TX、EEPROM DONE、virtio-net TX 完成、virtio-blk 请求完成、PS/2 输入/输出缓冲就绪、xHCI 控制器复位/停机/端口复位改为墙钟截止。
  - 方案：`virtio-blk` 与 PS/2 输入缓冲的**无界**等待获得首个硬上限（5 s / 100 ms）；其余按旧经验墙钟取预算，旧上限作回退。
  - 状态：[X]

- **W-4 显示栈转换**
  - 描述：DDC/EDID 事务、DP AUX 事务与 lane 对齐、HDMI PLL 锁定改为墙钟截止。
  - 方案：DDC 以**每毫秒级时序延时**为回退计数粒度（一轮 = 一次 `DDC_DELAY_ITERS` 延时），并借此修复旧预算的潜在缺陷；DP/HDMI 保持每轮硬件沉降自旋。
  - 状态：[X]

- **W-5 SMP 核启动转换**
  - 描述：x86_64 AP 就绪（1000 × 100 µs）与 AP 入口完成（500 × 100 µs）、aarch64 AP `done` 握手（旧 1 亿轮）改为墙钟截止。
  - 方案：保留每轮 `timer_udelay(100)` 硬件延时；墙钟预算取旧轮数 × 每轮延时（100 ms / 50 ms），aarch64 取 5 s（覆盖慢速固件下的 AP 上电）。
  - 状态：[X]

- **W-6 刻意不转换的等待点**
  - 描述：逐点甄别"无界即正确"的等待，登记理由，避免把语义性阻塞误改成超时。
  - 方案：见 §4 清单。
  - 状态：[X]

- **W-7 文档同步与验证门槛**
  - 描述：新增本册并把 DDC 旧预算缺陷留档；`docs/explain/` 无描述驱动等待的章节，无需同步；按 §2.3 七项门槛验证。
  - 方案：跑 `./ci/build.sh all`、`bash ci/audit.sh full`、`make test-host`、`make test-kernel-host`、双架构 QEMU、`make -f Makefile.ci ci-rustdoc`；读数见 §6。
  - 状态：[X]

## 3. 转换台账

共 17 个源文件、40 个等待点（另有新原语文件与其模块导出）。墙钟预算与回退值均落为具名常量。

| 域 | 文件 | 等待点 | 墙钟预算 | 回退上限（旧上限） |
|---|---|---|---|---|
| 存储 | `privileged/driver/storage/mod.rs` | NVMe admin / I/O CQ 完成（2 处） | 5 s | 5,000,000 轮 |
| 存储 | `functions/driver/storage/nvme.rs` | `wait_ready` / `wait_disabled` / 关机 SHST（3 处） | 5 s | 1,000,000 轮 |
| 存储 | `functions/driver/storage/ahci.rs` | 链路检出 / 链路 DET、FR/CR/FIS/空闲、命令完成、HBA 复位（11 处）+ COMRESET 保持 | 检出 100 ms；链路 5 s；引擎 1 s；命令 5 s；HBA 复位 1 s；保持 2 ms | 1,000,000 / 5,000,000 轮 |
| 存储 | `functions/driver/storage/ata.rs` | `wait_bsy` / `wait_drq`（2 处）+ 通道签名探活 | 1 s；探活 10 ms | 100,000 / 1,000 轮 |
| 存储 | `functions/driver/storage/mod.rs` | MSI-X 自检 ISR 等待 | 5 s | 5,000,000 轮 |
| 网络 | `functions/driver/net/e1000.rs` | 复位 / 链路 / TX（3 处） | 100 ms / 1 s / 100 ms | 100,000 / 500,000 / 100,000 轮 |
| 网络 | `privileged/driver/net/e1000_io.rs` | EEPROM `EERD.DONE` | 10 ms | 100,000 轮 |
| 网络 | `functions/driver/virtio/net.rs` | TX 完成 | 1 s | 100,000 轮 |
| 网络 | `functions/driver/virtio/blk.rs` | 请求完成（旧为无界 `loop`） | 5 s | 5,000,000 轮 |
| 输入 | `privileged/driver/input/keyboard.rs` | 输入缓冲空（旧无界）/ 输出缓冲满（2 处） | 100 ms | 100,000 轮 |
| USB | `functions/driver/usb/xhci.rs` | 控制器复位 / 停机 / 端口复位（3 处） | 1 s | 100,000 轮 |
| 显示 | `functions/driver/display/ddc.rs` | EDID 事务总预算 | 100 ms | 4,000 次时序延时 |
| 显示 | `functions/driver/display/dp.rs` | AUX 读 / AUX 写 / AUX 空闲（3 处）+ lane 对齐 | 5 ms；对齐 10 ms | 1,000 / 100 轮 |
| 显示 | `functions/driver/display/hdmi.rs` | PLL 锁定 | 100 ms | 10,000 轮 |
| SMP | `privileged/arch/x86_64/smp_init.rs` | AP 就绪 / AP 入口完成（2 处） | 100 ms / 50 ms | 1,000 / 500 轮 |
| SMP | `privileged/arch/aarch64/smp_init.rs` | AP `done` 握手 | 5 s | 100,000,000 轮 |

预算口径：有协议/规范时限的按规范取（NVMe 5 s）；无规范的取**不小于旧实现经验墙钟**后再留裕量（如 DDC 100 ms、HDMI 100 ms）；新获得上限的两处（virtio-blk、PS/2 输入缓冲）按同类设备的常见完成时间取 5 s / 100 ms。

### 3.1 DDC 旧预算的潜在缺陷

旧实现 `elapsed + DDC_DELAY_ITERS * 10 > 50_000` 的累计单位是 spin 次数，而每次时序延时记 50，等价于**约 1,000 次时序延时**。完整 EDID 读（2 START + 3 写字节 + 128 读字节 + STOP）约需 2.5k 次时序延时 ⇒ 旧预算会在约第 52 个读字节处误判超时。新墙钟预算 100 ms 覆盖完整事务；回退模式改为按"一次时序延时"计轮、上限 4,000 轮（约 2.6k 轮实际消耗），同时保留有界性与完整事务可完成性。

### 3.2 AHCI 空端口回归与修复（门槛实测）

首版转换把 `comreset` 的链路等待直接给了 5 s 墙钟预算。旧实现是 1M 次自旋（空端口几毫秒即放弃），新实现却在**每个空端口**上烧满 5 s：QEMU q35 的 ich9-ahci 有 6 个已实现端口、默认无盘，控制器初始化从 0.23 s 拖到 25.2 s（`[DRV] HBA 就绪` → `[DRV] 控制器初始化完成` 的日志时间戳差），正好吃掉线程压测的 25 s 单次超时（x86_64 3/3 FAIL 的根因，KPTI 探针的 `vec=14` 是预期负探针，非病灶）。

修复 = 链路等待拆成两段：先以 100 ms 窗口等设备检出（`PxSSTS.DET` 进入 1..=3），窗口内未检出即判空端口并返回（SATA 协议要求设备在 COMRESET 释放后 10 ms 内回应，100 ms 是 10× 裕量）；检出后才进入 5 s 链路预算。这与 Linux 对空端口即时打印 `SATA link down` 后继续枚举同向。修复后同一日志段 0.23 s → 0.745 s（5 个空端口 × 100 ms），线程压测 x86_64 单轮 PASS，`[SCHED] SNAP END` 在 2.17 s 到达。回归锁：`SataStatus::is_present` 语义单测（DET=0/4 非在位、1/3 在位）。

## 4. 刻意不转换的等待点

| 等待点 | 不转换理由 |
|---|---|
| 串口/PL011 控制台写（等待发送 FIFO） | 阻塞语义是控制台契约的一部分；输出失败无处可报，截断比等待更差 |
| kexec / 关机 / 重启 / PSCI / ACPI 致命路径 | 失败后的唯一正确行为是停住或复位，超时无收场动作 |
| x86 ICR 投递忙等（`wait_icr_delivery` 之外的短自旋） | 与 Linux 同形：投递窗口以总线周期计，超时语义无意义 |
| APIC `wait_icr_delivery` | 已有独立上限（1,000,000 轮），本轮不重复改造 |
| GIC RWP / redistributor 等待 | 判据已收进 host 侧可测的 `gic_logic` 纯函数，改成墙钟会让纯函数无人调用并失去单测 |
| 电源挂起 halt 循环 / UEFI 日历轮询 | 语义为"等外部事件"或固件服务步进，不是设备就绪轮询 |
| `nvme_drain_completions` | 由 CQ 相位位自然定界（读到相位翻转即停），无需超时 |
| 锁获取自旋 | 与 Linux 一致：无界等待，死锁由 lockdep/锁序门禁负责，而非超时掩盖 |
| 键盘 `read_line` 阻塞读 | 面向用户的阻塞读语义，不属于驱动就绪等待 |

## 5. 后续约定

新增驱动等待时：优先 `poll_until_ms(预算, 旧迭代上限, || …)`；循环体需要硬件延时或复杂状态机时用 `Deadline`；回退上限必须与旧实现的迭代上限同值同粒度，禁止只给墙钟预算而不给回退（`kernel_test` 等时基未建立环境会退化为无界）。

## 6. 验证

主机侧：`make test-kernel-host` 三条测试二进制 12 + 629 + 512 passed / 0 failed，含 `timer::wait` 的 7 条语义单测（零预算单探测、回退按轮计数与提前命中、墙钟主导轮数、到期最后重读、截止期边界与计数器回退防御、`Deadline` 回退计轮）与 `ahci::tests::test_sata_status_presence`（§3.2 回归锁）；`make test-host` 1441 passed / 0 failed。

七项门槛（同一棵树，串行执行）：

| # | 门槛 | 读数 |
|---|---|---|
| 1 | `./ci/build.sh all` | `Passed: 6  Failed: 0`（双架构编译与链接均过） |
| 2+3 | `bash ci/audit.sh full` | RC=0；`本轮因环境缺失未执行腿数: 0`；clippy 三腿与四 crate fmt 全绿 |
| 4 | `make test-host` | 1441 passed / 0 failed |
| 5 | QEMU（full 内） | `QEMU 真实启动测试: 2/2 通过` + `P-62 feature 运行维: 4/4 通过`；线程压力腿 6/6 |
| 6 | `make test-kernel-host` | 12 + 629 + 512 passed / 0 failed |
| 7 | `make -f Makefile.ci ci-rustdoc` | x86_64 `exit: 0` + aarch64 `exit: 0`，`0 源码级 link 错误 + 0 私有项链接` |

过程中三次红的归因：① clippy `doc_markdown` 两处（`NVMe` 未反引号化，已修）；② 线程压力腿 x86_64 3/3 红，取证为 AHCI 空端口烧满链路预算（§3.2，已修，修复后 x86_64 1/1 与 3/3 全绿）；③ aarch64 一例 `[THREAD] FAIL: reap-timeout code=0`（`[REAP] tgid=13 unreaped pid=14 state=Blocked` → 2.5 s 超时），属 `docs/plan/multithreading-project.md` 已登记的既有偶发家族（复现率约 1/8~1/11，落在调度/futex/线程路径，本册零涉及），复跑 6/6 全绿。
