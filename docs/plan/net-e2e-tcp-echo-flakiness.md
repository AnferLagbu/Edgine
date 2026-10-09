# TCP 回显 e2e 抖动工程（根因：CFS 时基背离 + 运行队列记账泄漏）

> **定位**：`scripts/qemu_boot_test.sh` 的 x86_64 端到端阶段（宿主经 `hostfwd` 连 guest :80 → guest `accept`/`recv`/`send` 回显 → 宿主校验载荷）出现**非确定性失败**，失败形态为宿主 `recv` 5s 超时、guest 侧 `accept` 计数缺失。本文件登记该抖动的**完整取证链与已根治的根因**，并保留此前**被对照实验证伪的四条假设**作为方法学记录，避免后人重走。
>
> **根因（已定论）**：不在网络路径，而在 **CFS 调度器**。`CfsRunQueue` 旧实现同时维护"队列 `min_vruntime`"与"任务自身 `cfs_vruntime`"两个时间域且互不收敛，加上并行计数器 `nr_running` 与红黑树背离，使 `cfs_should_preempt` 的判据失真 → 某个核上的 Ready 任务被饿死 4~10 s，恰好跨过宿主 5 s 的 `recv` 超时。已按 DECISION-099 的五条不变式根治。
>
> **目标**：`./scripts/qemu_boot_test.sh x86_64` 在 `E2E_REPEATS=20` 下 0 失败，并作为调度器的**长期回归门禁基线**。
>
> **来源**：P6（组播 / D11）收尾时门槛#6 复跑暴露；用户在 P6 期间裁定"只留 close 单点排空 + 定时器立项"，本文件按后续对照实验与插桩取证的结论**修正**了该裁定的技术前提（见 §1 D2/D3/D4）。
>
> **关联**：`docs/plan/socket-layer-completion.md`（D6 交接语义 / D11 组播 / DECISION-097~099）、`docs/report/net-e2e-tcp-echo-flakiness-diagnosis.md`（本次取证的完整证据链）、`docs/explain/ref-lock-order.md`（`NET_STATE` 临界区口径）。

## 1. 决策记录

| 决策点 | 选择 | 说明 |
|---|---|---|
| D1 探针窗口 | `mcast_probe` 有界重试 **120 轮(6s) → 20 轮(1s)** | 对照实验证实的**放大器**（§4.1），非根因。窗口决定监听 socket 就绪时刻与 fork/`busy_wait` 的相位，因而改变记账泄漏累积到"致命量"的概率 |
| D2 close 排空 | **保留**，但立场改为"保证尾包与 FIN 上线"，**不再**充当抖动解药 | 撤销排空的实验（§4.1 B 组 8/8 通过）证明它对抖动无贡献；保留理由是当前 `sm_close` 的 `close()` → `remove()` 之间无任何 egress 推进点，FIN 与尾包会随 socket 销毁作废（POSIX 语义缺口，与抖动正交） |
| D3 "egress 无独立驱动源"论断 | **作废** | x86_64 每个 PIT tick 都调 `poll_network`（§3 第 3 条），原论断源于一次被 `head -20` 截断的 grep，属调研方法失误 |
| D4 "NIC 静默掉帧"论断 | **作废** | 在驱动发送口补计数后，25 轮（含全部失败轮）TX 失败次数恒为 0（§3 第 2 条） |
| D5 "slirp 入向丢帧"假设 | **作废**（抓包证伪） | `-object filter-dump` 三视图对照显示失败轮的 SYN/ACK 已到 guest 侧并进入协议栈（S-1），丢失点在 guest **消费**侧而非投递侧 |
| D6 根治路径 | **单时基模型**（IC1-IC5），不做"调参式"缓解 | 用户授权"按根治方案施工"；不引入周期性整树 boost、不放宽超时、不加特判唤醒 |
| D7 唤醒/抢占最小粒度 | `CFS_MIN_GRANULARITY = 8` tick | 用户裁定；floor 钳制的容差即此值，决定"新唤醒者最多落后当前任务多少" |
| D8 验收强度 | `E2E_REPEATS=20` 连跑 + 前后失败率对比入 `docs/report/` | 用户裁定"更高强度"；单次通过不足以证伪 5% 量级的抖动 |
| D9 S-6 处置 | **三条防线全做**（产物分架构目录 + 内核 `e_machine` 严格校验 + QEMU 前镜像自检 fail-fast） | 用户裁定。门槛可信度属工程前置条件："只登记"会让后续所有 QEMU 数据继续暴露在污染风险下；"仅 fail-fast"保留共路径覆盖机制只把发现时间推到启动前 |

## 2. 根因（已定论）

### 2.1 IC1 背离：两个时间域无界发散

旧 `CfsRunQueue::enqueue` 把树键钳到 `min_vruntime` 附近，但**不回写** `Process::cfs_vruntime`。于是"正在运行的任务"按其自身 vruntime 前进，"树上的任务"按被钳制的键排队，两条时间轴各自漂移：

- 实测失败轮 `curr vr=45 / floor=5152` —— 当前任务的 vruntime 反而**远低于**队列下界。
- `cfs_should_preempt` 用 `saturating_sub` 计算"树上最左任务比当前任务落后多少"，负差被压成 `0` ⇒ 恒不置 `need_reschedule` ⇒ 该核一旦切入长任务就**永不重调度**。
- 后果：`tcp_echo_server` 子进程的 `accept` 路径在 Ready 队列上排队 4~10 s 得不到时间片，跨过宿主 `create_connection(timeout=5)` 的超时线。

### 2.2 IC2 背离：并行计数器与树失真

旧实现同时维护 `nr_running`（原子计数器）与 `tree`（红黑树）两份"可运行集"表示。跨核迁移 / 阻塞撤账路径不完全对称，实测出现 `nr=108 / tree=0`：计数器认为有 108 个可运行任务，树是空的。两个失真同时发生：

- `is_empty()` 按计数器判定 ⇒ 对空队列反复 `pick_next` 失败并回落到"保持当前任务"；
- 抢占判据按树取最左键 ⇒ 无候选可比 ⇒ 与 §2.1 叠加成"隐形饥饿"。

### 2.3 根治后的模型（DECISION-099）

| 不变式 | 内容 | 落地 |
|---|---|---|
| IC1 单一时基 | 任意任务的可比较 vruntime 落在 `[floor - CFS_MIN_GRANULARITY, +∞)` | `enqueue` 返回钳制后的落点，**调用方必须回写** `Process::cfs_vruntime`；`placement()` 为该钳制的唯一实现 |
| IC2 结构即真相 | 可运行性判据一律取自 `tree` | 删除 `nr_running`，`is_empty()`/`len()` 走树；`total_weight` 依 `Process::cfs_on_rq` 记账凭证精确加减 |
| IC3 下界单调 | floor 只由 tick 随运行任务 vruntime 推进，**只增不减** | `advance_floor` 用 `fetch_max`；删除 `sync_min_vruntime`（唤醒者可反推 floor 成棘轮） |
| IC4 可运行集 = 树 | 阻塞即出队撤账 | `cfs_deactivate` 幂等撤账；`pick_next` 摘除的节点记账随迁 |
| IC5 相对次序不被抹掉 | 禁止整树折叠 | 删除 `boost_all_vruntime` |

记账凭证由 `Process::cfs_on_rq` 承载（语义同 Linux `on_rq`：处于可运行记账中，上核运行期间仍为 `true`）。锁序仍只允许 `cfs_rq → PROCESS_TABLE`（见 `docs/explain/ref-lock-order.md`）。

## 3. 被证伪的假设（方法学记录，勿重走）

- **slirp 入向丢帧**：`hostfwd` 语义使宿主 `connect()` 成功**不蕴含** SYN 抵达 guest（slirp 先与宿主 app 完成三次握手，再以自己的 emulated TCP 向 guest 发起连接），故"宿主 connect 成功 + guest 不 accept"一度高度自洽；filter-dump 抓包（S-1）证明失败轮 SYN 已到 guest 并被协议栈收下，缺的是 guest 侧**消费**。
- **NIC 静默掉帧 / 发送路径故障**：驱动侧 `E1000NetDriver::send`（`functions/driver/net/e1000.rs:262-268`）把 `send_packet` 的错误压成 `-1`，`send_packet` 自旋等 tail 描述符 DD 位超 `E1000_TIMEOUT = 100_000`（`privileged/driver/net/e1000_io.rs:113`）返 `Timeout`；TX 环 64 槽（`privileged/driver/net/dma_ring.rs:69`）、`TxRing::alloc` 预置 DD，逻辑正确。smoltcp 设备层 `transmit()` 无条件返回 `Some`（无背压）、`EGDFTxToken::consume` 曾丢弃 `ops.send` 返回值（`privileged/net/smoltcp_impl.rs:147-157`）⇒ 驱动失败会静默掉帧且无计数。这是**真实**的可观测性缺口（已由 S-2 补 `TX_SENT`/`TX_DROPPED`），但 25 轮实测 TX 失败恒 0，非本次因。
  - 澄清一项早期误判：仓内并非"两份 e1000 驱动"。业务逻辑只有 `functions/driver/net/e1000.rs`（`E1000NetDriver`，0 unsafe）一份；`privileged/driver/net/e1000.rs` 与 `e1000_io.rs` 只提供 `TxRing`/`RxRing` 与 MMIO 安全代理，符合 AGENTS.md §4.1 机制/功能分离。
- **`poll_at` → hrtimer 缺失**：`SmoltcpNetStack::poll_at()` 恒 `None`（`privileged/net/iface_trait.rs:406-408`），即 smoltcp 的"下次该 poll 的时刻"未接入 hrtimer，重传/超时/NDP/DHCP 续约时限只能靠 tick 空转推进 —— **真实**机制缺口，但非本次因；降为独立优化项 S-3。egress 确有独立驱动源：x86_64 `timer_irq0_handler`（`privileged/timer/irq.rs:30-45`，第 42 行）每 tick 调 `poll_network()`，aarch64 同构（`privileged/arch/aarch64/exception.rs:738`）；tick 频率名义 1000Hz（`privileged/timer/pit.rs:38` 的 `DEFAULT_INTERRUPT_FREQ_HZ` + `lib.rs:753-757` 的 `timer_init(1000)`）。`poll_network` 以 `NET_STATE.try_lock()` 进入（`privileged/net/init.rs:234-243`），锁忙则本次 tick 放弃 —— 唯一已知的"egress 推进被推迟"窗口，量级为一次临界区，非秒级。
- **close 未排空**：撤销排空 8/8 通过，与抖动无因果（立场见 D2）。
- **探针窗口是根因**：窗口只改变相位（`src/user/init/src/main.rs` 的启动次序同步串行：`ipv6_udp_probe` → `udp_echo_probe` → `mcast_probe()` → fork `tcp_echo_server` → 父进程 `busy_wait(b'+')` 无限占用一个核；有界重试常量 `MAX_TRIES`），真正决定失败与否的是 §2 的饥饿累积量。C 组 `[tcp] Listening` 时间戳在 7.68s 与 9.47s 间摆动正是饥饿表征。

## 4. 对照与修复实验数据

### 4.1 修复前的窗口对照（同构建、单变量）

| 组 | 变量 | 结果 | 判读 |
|---|---|---|---|
| A | 1s 窗口 + close **前**排空 | 4/4 通过 | — |
| B | 1s 窗口 + **无**排空 | **8/8 通过** | 排空非必需 → D2 立场改 |
| C | **6s** 窗口 + 无排空 | **2/8 通过**（追加 5 轮全失败，累计 2/13） | 窗口是放大器 |
| D | 1s 窗口 + close→poll→remove 三步序 | 6/6 通过 | 发 FIN 不破坏门槛 |

C 组另一线索：同一构建下 `[tcp] Listening` 的内核时间戳在 7.68s 与 9.47s 间摆动 —— guest 侧本就有秒级不确定性，正是 §2 饥饿的表征而非网络抖动。

### 4.2 修复前 20 连基线

`E2E_REPEATS=20 ./scripts/qemu_boot_test.sh x86_64`（干净镜像，无插桩）：**20 轮 1 失败（5%）**，失败轮为第 17 轮，形态 `TimeoutError: timed out`（宿主 `recv` 5s 超时）。日志 `other/build/log/before_baseline.log`。

### 4.3 修复后 20 连验收

调度器根治 + 插桩全部清理后，同一脚本同一强度连跑 20 轮，与 §4.2 对比；结果与门禁承载写入 `docs/report/net-e2e-tcp-echo-flakiness-diagnosis.md`。

### 4.4 验收期间的"假象回归"（构建系统缺陷，非本抖动）

首轮 `E2E_REPEATS=20` 验收出现**前 4 轮全失败**，形态完全不同：init（pid=5）进入 Ring 3 后首条指令即 `#PF`（`vec=14 err=0x4 rip=0x400000 cr2=0x13E36105`）。曾误判为调度器改动引入的确定性回归，取证后证伪：

- `enter()` 自检打印的用户代码页首 16 字节为 `FF 83 00 D1 FE 0B 00 F9 …`，解码即 **AArch64 函数序言**（`sub sp,sp,#0x20` / `str x30,[sp,#16]` / `mov w0,#1`）—— 页表装配无误，是**嵌进镜像的用户态 ELF 架构错了**。
- 还原 HEAD 调度器 → 通过；恢复我方调度器并去插桩 → 3/3 通过 + 脚本 rc=0 ⇒ **调度器改动无罪**。
- 通道：`other/build/user/init.bin` 是 x86_64/aarch64 **共享同一路径、原地覆盖**，内核经 `include_bytes!("../../../other/build/user/init.bin")` 编译期嵌入。`ci/build.sh all` 的执行序为 build x86_64 → build aarch64（把 `init.bin` 改写成 aarch64）→ host-tests → **`link_kernel "x86_64"`**，故最终产出的 x86_64 镜像嵌入的是 aarch64 用户态，且**全程 rc=0 无告警**。
- 复现证明（以镜像内 ELF 的 `e_machine` 为判据，非尺寸）：`./ci/build.sh all`（rc=0）后盘上 `init.bin` = `ARM aarch64`，而 x86_64 `kernel.flat` 内嵌的那份 ELF 仍为 `e_machine=0xB7`（AArch64）；启动它得到与失败轮**逐字节同形**的日志。`./ci/build.sh x86_64` 重建后同一偏移处的 ELF 变回 `e_machine=0x3E`（x86-64），用例立即通过。
- **判据纠偏**：本轮曾以 `kernel.flat` **尺寸**作架构判据（2271808 vs 2387864），实测不成立 —— 同一 x86-64 构型在全量/增量构建状态下尺寸可差约 60KB（2334616 与 2271808 均为合法 x86_64 镜像）。可靠判据只有两个：盘上产物的 `file`，以及镜像内 ELF 头部的 `e_machine`。

该缺陷登记为 S-6（本文件），属**门槛可信度**问题：只要按 `all` → QEMU 的顺序跑门禁，结果就不可信。已按 D9 三条防线修复（见 §5 S-6 实施记录）：修复后即使直接按 `all` 产出的镜像跑 QEMU，也不会再嵌错架构的用户态（因 `.arch` 戳记而触发的镜像重建仍由 `sync_make_state` 负责，与本缺陷无关）。

### 4.5 判据原文（防后续漂移）

- 就绪与轮数旋钮：`E2E_HOST_PORT=8080` / `E2E_GUEST_PORT=80` / `E2E_READY_TIMEOUT=40` / `E2E_ROUNDS=3`（qemu_boot_test.sh:32-35），重复轮 `E2E_REPEATS`（S-5 新增）。
- 宿主每轮 `create_connection(timeout=5)` → `sendall` → `shutdown(SHUT_WR)` → `recv` 循环，轮间 `sleep(0.2)`。
- guest 侧断言：`accepted`/`echoed`/`closed` 三计数均 `>= E2E_ROUNDS`。

## 5. 施工条目

- **S-1. 抖动定位（抓包 + 插桩）**
  - 描述：失败发生在"宿主 app → slirp → guest"区间，但现有证据只到日志计数层，无法区分 slirp 未转发 / SYN 在 guest 侧未上送 / 监听 socket 未 `accept`。
  - 方案：`-object filter-dump` 抓包三视图对照 → 证伪入向丢失；再在 tick / schedule / poll_network 路径插桩判定停滞段 → 定位为调度饥饿（§2）。
  - 状态：[X]（结论见 §2；证据链见 `docs/report/net-e2e-tcp-echo-flakiness-diagnosis.md`；插桩已全部撤除）

- **S-2. TX 掉帧可观测性**
  - 描述：`EGDFTxToken::consume` 丢弃 `ops.send` 的返回值，驱动侧失败即静默掉帧，事后无从判断是否发生过。
  - 方案：`smoltcp_impl.rs` 的 consume 处保留返回值并累加 `TX_SENT`/`TX_DROPPED` 计数，经既有 net 统计出口可读；不为单次诊断引入 printk 风暴。
  - 状态：[X]（含 `host-tests/tests/net_tx_drop_observability_test.rs` 接线契约）

- **S-3. `poll_at` → hrtimer 周期驱动**
  - 描述：`SmoltcpNetStack::poll_at()` 恒 `None`，smoltcp 的重传/超时/NDP/DHCP 续约时限只能靠 tick 空转推进。
  - 方案：把 `iface.poll_at()` 返回的下次截止时刻注册到 hrtimer，到期触发 `poll_network`；tick 侧 poll 频率随之可降。**注意**：本项是效率与低功耗优化，不得再作为 e2e 抖动的解药表述。
  - 状态：[]（用户裁定本轮不做）

- **S-4. 用户态回显服务去掉 `FLUSH_MS` hack**
  - 描述：`FLUSH_MS = 50` 是对"close 丢弃尾包"的用户态兜底；内核侧三步序已闭合该缺口。
  - 方案：删除该延时后连跑 boot test 验证不回退，确认语义归属内核；同步检查 `MAX_RECV_TRIES`/`POLL_MS` 是否仍有等价必要。
  - 状态：[X]

- **S-5. 门槛稳定性回归承载**
  - 描述：单次通过不足以证明抖动消除。
  - 方案：脚本级重复选项 `E2E_REPEATS`（默认 1，验收/夜间作业设 >1），把"连续多轮 0 失败"变成可门禁的指标。
  - 状态：[X]

- **S-6. 构建产物架构污染（§4.4 派生，新立）**
  - 描述：`other/build/user/*.bin` 跨架构共享单一路径并原地覆盖，内核编译期 `include_bytes!` 嵌入；`ci/build.sh all` 末段再 `link_kernel x86_64`，产出嵌入 aarch64 用户态的 x86_64 镜像，**全程 0 error / 0 warning / rc=0**。`verify_elf` 不校验 `e_machine`（aarch64 ELF 照样 `verify_elf OK`），`qemu_boot_test.sh` 的 `sync_make_state`/`check_kernel_fresh` 只覆盖中间产物陈旧且仅 warn，均无法拦截。
  - 方案：三条防线（用户裁定 D9：全做）—— ① 构建产物按架构分目录（`other/build/x86_64/user/init.bin` 等），从根上消除原地覆盖；② `verify_elf` 校验 `e_machine` 与当前 `CONFIG_TARGET_ARCH` 一致；③ QEMU 门禁前对镜像做架构自检（扫描 `kernel.flat` 内嵌 ELF 的 `e_machine`，期望 `0x3E`/`0xB7` 与目标架构一致），不符则 **fail 而非 warn**（尺寸不得作判据，见 §4.4 判据纠偏）。附带修 `src/kernel/build.rs` 的 `cargo:rerun-if-changed` 相对路径（以包根 `src/kernel` 为基准，实际指向不存在路径）。
  - 状态：[X]（实施与取证见下方“实施记录”）
  - 实施记录：
    - 防线①：`Makefile` 新增 `USER_BUILD_DIR := $(BUILD_DIR)/$(ARCH)/user`，全部 43 处产物路径（`user` 目标 / 单文件规则 / iso 与启动介质拷贝）改用该变量；`arch-switch-clean` 不再删用户态产物（异架构产物已不在本架构读取路径上）。内核侧 `functions/init.rs` 两处 `include_bytes!` 分别改指 `other/build/x86_64/user/init.bin` 与 `other/build/aarch64/user/init.bin`（`initramfs.cpio` 同步迁目录）。
    - 防线②：`privileged/proc/elf/verify.rs` 新增 `EXPECTED_E_MACHINE`（裸机 = `Some(本架构)`，host = `None`）与纯函数 `machine_acceptable()`；`verify_elf` 的 machine 项由“∈ {0x3E, 0xB7}”收紧为“必等于本编译期目标架构”。旧写法正是 aarch64 ELF 照样 `verify_elf OK` 的原因。
    - 防线③：新增 `scripts/verify_image_arch.py`（判据 = 产物 `e_machine` + 镜像**逐字节包含**产物），`qemu_boot_test.sh` 新增同名函数并在 x86_64 / aarch64 两条启动链路上以 `elif ! verify_image_arch …; then RESULT=1` 接入 fail-fast（不通过即不启 QEMU）。`src/kernel/build.rs` 的 `rerun-if-changed` 改发绝对路径（`init.bin` 按 `CARGO_CFG_TARGET_ARCH` 拼目录，`stage1.bin` 同步）。
    - 测试承载：`host-tests/tests/elf_verify_unification_test.rs` 新增 `machine_acceptable` 两分支用例（Some/None 各一 + host 前提断言，共 3 例）；`scripts/tests/verify_image_arch_selftest.py` 13 例（合成 ELF，不依赖真实产物）。
    - 取证（逐条可重跑）：① 修复前 `./ci/build.sh all` 后 x86_64 镜像内嵌 ELF `e_machine=0xB7`（污染复现）；② 修复后 `all` 两架构产物共存（`x86_64/user/init.bin` = x86-64、`aarch64/user/init.bin` = ARM aarch64）且 `kernel.flat` 仍逐字节包含 x86-64 产物 @0x18d8c4 ⇒ 污染通道消失；③ 受控污染实验（把 aarch64 产物拷进 x86_64 目录后 `make ARCH=x86_64`）使镜像立即改嵌 `0xB7` ⇒ 证明 `build.rs` 绝对路径修复后重编触发与真实依赖同源（旧世界 cargo 判 Fresh 不重嵌）；④ 该污染镜像直启 QEMU 得到 `0.931971 [ELF] verify_elf failed` + `[ERR] Failed to load init ELF, pid=-1`，不再是“入口 0x400000 执行非法指令 ⇒ #PF” ⇒ 防线②在裸机生效（`EXPECTED_E_MACHINE = Some` 路径）。

## 6. 附带发现的预存问题（按需登记，非本抖动因）

- **P-1 双份 tick 计数**：`privileged::proc::scheduler::TICK_COUNT`（pub）与 `privileged::timer::tick::TICK_COUNT`（私有）并行存在、各自累加，属重复实现（`docs/plan/duplicate-impl-convergence.md` 范畴）。
- **P-2 `proc_sleep_ms` 时基假设错**：`proc_ops.rs:668` 用 `ms.div_ceil(10)`，隐含"1 tick = 10ms"（100Hz），而实际 PIT 名义 1000Hz ⇒ 睡眠长度存在 10× 偏差；应改为按 ns/绝对截止期记账（与 `sleep.rs` 的 `total_ns.div_ceil(NS_PER_MS)` 对齐）。
- **P-3 有效 tick 频率与名义不符**：实测约 1400 tick/s 对 `DEFAULT_INTERRUPT_FREQ_HZ = 1000`，需核（QEMU 定时器 + 中断处理耗时 + 是否存在双写）。
- **P-4 `sched_should_reschedule` 死 FFI 钩子**：`sched_ops.rs:257` 的 `extern "C"` 入口无调用者，属 §5 F9 零容忍范畴。
- **P-5 boot → `[tcp] Listening` 时长抖动**：10.7s ↔ 23.9s（修复前样本），根治后需重测。
- **P-6 每连接 2 次 SYN-ACK 重传**：guest 监听就绪前的重传属正常 TCP 行为，但次数偏多，待与 §S-3 的 poll 节奏一并评估。
