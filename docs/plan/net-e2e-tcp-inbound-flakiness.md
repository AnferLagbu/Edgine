# TCP 回显 e2e 入向连接抖动（根因未定）工程

> **定位**：`scripts/qemu_boot_test.sh` 的 x86_64 端到端阶段（宿主经 `hostfwd` 连 guest :80 → guest `accept`/`recv`/`send` 回显 → 宿主校验载荷）在 P6 期间出现**非确定性失败**，失败形态为宿主 `recv` 5s 超时、guest 侧 `accept` 计数缺失。本文件登记**已用对照实验钉死的事实**与**已被证伪的三条假设**，并把"入向连接为何丢失"立为待查工程。
>
> **目标**：`./scripts/qemu_boot_test.sh x86_64` 连续 10 轮 0 失败，且失败判据不因用户态探针的活动时长而漂移；抖动根因定位到具体层（QEMU slirp / guest 协议栈 / 调度时序）并有回归测试承载。
>
> **来源**：P6（组播 / D11）收尾时门槛#6 复跑暴露；用户在 P6 期间裁定"只留 close 单点排空 + 定时器立项"，本文件按后续对照实验的结论**修正**了该裁定的技术前提（见 §1 D2/D3）。
>
> **关联**：`docs/plan/socket-layer-completion.md`（D6 交接语义 / D11 组播）、DECISION-097 / DECISION-098；`docs/explain/ref-lock-order.md`（`NET_STATE` 临界区口径）。

## 1. 决策记录

| 决策点 | 选择 | 说明 |
|---|---|---|
| D1 探针窗口 | `mcast_probe` 有界重试 **120 轮(6s) → 20 轮(1s)** | 唯一被对照实验证实的抖动变量（§2.2）。窗口同时决定监听 socket 就绪时刻，是 e2e 的输入而非旁证 |
| D2 close 排空 | **保留**，但立场改为"保证尾包与 FIN 上线"，**不再**充当抖动解药 | 撤销排空的实验（§2.1 B 组 8/8 通过）证明它对抖动无贡献；保留理由是当前 `sm_close` 的 `close()` → `remove()` 之间无任何 egress 推进点，FIN 与尾包会随 socket 销毁作废（POSIX 语义缺口，与抖动正交） |
| D3 "egress 无独立驱动源"论断 | **作废** | x86_64 每个 PIT tick（1000Hz）都调 `poll_network`（§2.3），原论断源于一次被 `head -20` 截断的 grep，属调研方法失误 |
| D4 "NIC 静默掉帧"论断 | **作废** | 在驱动发送口加计数后，25 轮（含全部失败轮）TX 失败次数恒为 0（§2.4） |
| D5 本轮范围 | **不做根因深挖**，仅收敛窗口使门槛可信 + 登记本文件 | 抖动发生在 QEMU/宿主网络仿真与用户态时序的交界处，继续用启动参数扫描的边际信息已趋零；须换成抓包级观测（§3 S-1） |

## 2. 调研结论（源码与实验依据，行号均已核实）

### 2.1 对照实验（同机型、同脚本、单变量）

| 组 | 变量 | 结果 | 判读 |
|---|---|---|---|
| A | 1s 窗口 + close **前**排空 | 4/4 通过 | — |
| B | 1s 窗口 + **无**排空 | **8/8 通过** | 排空非必需 → D2 立场改 |
| C | **6s** 窗口 + 无排空 | **2/8 通过**（追加 5 轮全失败，累计 2/13） | 窗口即触发项 |
| D | 1s 窗口 + close→poll→remove 三步序（最终态） | 6/6 通过 | 发 FIN 不破坏门槛 |

失败轮特征（保留日志逐轮核对）：guest 已打印 `[tcp] Listening on 0.0.0.0:80`，脚本"服务端就绪"判定通过，宿主 `create_connection` 亦成功，随后 **python `recv` 抛 `TimeoutError`**；guest 侧 `accept` 计数为 0 或停在 1–2（不足 3 轮），且日志中**没有**任何 `e1000` 发送失败或 RX 描述符错误。⇒ 连接请求在"宿主 app → slirp → guest 监听 socket"这一段里丢失，且丢失点不在 guest 的发送路径上。

注意 `hostfwd` 语义使 `connect()` 的成功**不蕴含**SYN 抵达 guest：slirp 先与宿主 app 完成三次握手，再以自己的 emulated TCP 向 guest 发起连接。故"宿主 connect 成功 + guest 不 accept"是自洽的失败形态。

### 2.2 探针窗口为何是输入而非旁证

`src/user/init/src/main.rs` 的启动次序是同步串行的：`ipv6_udp_probe` → `udp_echo_probe` → [`mcast_probe()`](../../src/user/init/src/main.rs#L97)（第 97 行）→ **fork `tcp_echo_server`**（[L106-L112](../../src/user/init/src/main.rs#L106-L112)）→ 父进程进入 [`busy_wait(b'+')`](../../src/user/init/src/main.rs#L113)（第 113 行，无限占用一个核）。探针的有界重试常量在 [L345](../../src/user/init/src/main.rs#L345)（`MAX_TRIES = 20`，每轮 50ms）。窗口直接后移监听 socket 的建立时刻，也改变 fork 与 `busy_wait` 抢占的相对相位 —— 因此它是 e2e 的**时序输入**。C 组另一点值得注意：同一构建下 `[tcp] Listening` 的内核时间戳在 7.68s 与 9.47s 间摆动，说明 guest 侧本身已有秒级不确定性。

### 2.3 egress 确有独立驱动源（D3 依据）

- x86_64：[`timer_irq0_handler`](../../src/kernel/privileged/timer/irq.rs#L30-L45)（[irq.rs:30-45](../../src/kernel/privileged/timer/irq.rs#L30-L45)）在每个 tick 里调用 `privileged::net::poll_network()`（第 42 行）。
- tick 频率 1000Hz：[`DEFAULT_INTERRUPT_FREQ_HZ = 1000`](../../src/kernel/privileged/timer/pit.rs#L38)（pit.rs:38），启动路径 [`timer_init(1000)` + `register_timer_irq()`](../../src/kernel/lib.rs#L753-L757)（lib.rs:753-757）。
- aarch64 同构：[exception.rs:738](../../src/kernel/privileged/arch/aarch64/exception.rs#L738) 每 tick 调 `poll_network`。
- `poll_network` 以 [`NET_STATE.try_lock()`](../../src/kernel/privileged/net/init.rs#L234-L243) 进入（[init.rs:234-243](../../src/kernel/privileged/net/init.rs#L234-L243)），锁忙则本次 tick 放弃 —— 这是唯一已知的"egress 推进被推迟"窗口，量级为一次临界区，非秒级。
- [`SmoltcpNetStack::poll_at`](../../src/kernel/privileged/net/iface_trait.rs#L406-L408) 恒返回 `None`（iface_trait.rs:406-408），即 smoltcp 的"下次该 poll 的时刻"提示未接入 hrtimer。这仍是一个**真实**的机制缺口（低频空闲时 poll 全靠 1ms tick 空转），但经 C/D 组实验它**不是**本次抖动的因，故不再以"poll_at→hrtimer"名义立项，改登记为 §3 S-3 的独立优化项。

### 2.4 发送路径的可观测事实（D4 依据，亦为后续排查入口）

- 驱动侧发送口：[`E1000NetDriver::send`](../../src/kernel/functions/driver/net/e1000.rs#L262-L268)（e1000.rs:262-268）把 `send_packet` 的错误压成 `-1`；`send_packet`（[L200-L225](../../src/kernel/functions/driver/net/e1000.rs#L200-L225)）自旋等 tail 描述符 DD 位，超 [`E1000_TIMEOUT = 100_000`](../../src/kernel/privileged/driver/net/e1000_io.rs#L113)（e1000_io.rs:113）返 `Timeout`。TX 环 64 槽（[dma_ring.rs:69](../../src/kernel/privileged/driver/net/dma_ring.rs#L69)），`TxRing::alloc` 预置 DD（[e1000.rs:63-65](../../src/kernel/privileged/driver/net/e1000.rs#L63-L65)），逻辑正确。
- smoltcp 设备层：[`transmit()` 无条件返回 `Some`](../../src/kernel/privileged/net/smoltcp_impl.rs#L121-L127)（无背压），[`TxToken::consume` 丢弃 `ops.send` 返回值](../../src/kernel/privileged/net/smoltcp_impl.rs#L147-L157)（第 153-154 行）⇒ **驱动失败会静默掉帧且无计数**，这是可观测性缺口（§3 S-2），但实测未触发。
- 澄清一项早期误判：仓内并非"两份 e1000 驱动"。业务逻辑只有 `functions/driver/net/e1000.rs`（`E1000NetDriver`，0 unsafe）一份；`privileged/driver/net/e1000.rs` 与 `e1000_io.rs` 只提供 `TxRing`/`RxRing` 与 MMIO 安全代理，符合 §4.1 机制/功能分离。

### 2.5 判据原文（防后续漂移）

- 就绪与轮数旋钮：[`E2E_HOST_PORT=8080` / `E2E_GUEST_PORT=80` / `E2E_READY_TIMEOUT=40` / `E2E_ROUNDS=3`](../../scripts/qemu_boot_test.sh#L32-L35)（qemu_boot_test.sh:32-35）。
- 宿主每轮 `create_connection(timeout=5)` → `sendall` → `shutdown(SHUT_WR)` → `recv` 循环，轮间 `sleep(0.2)`（[L252-L268](../../scripts/qemu_boot_test.sh#L252-L268)）。
- guest 侧断言：`accepted`/`echoed`/`closed` 三计数均 `>= E2E_ROUNDS`（[L288-L301](../../scripts/qemu_boot_test.sh#L288-L301)）。
- 用户态回显服务的既有兜底：[`FLUSH_MS = 50`](../../src/user/init/src/main.rs#L641) 在 `close` 前让 tick 去排空 —— D2 的三步序落地后该 hack 理论上可移除，本轮**不动**（属用户态测试程序，留待 §3 S-4 一并评估）。

## 3. 施工条目

- **S-1. 抓包级定位入向丢失点**
  - 描述：失败发生在"宿主 app → slirp → guest"区间，但现有证据只到日志计数层，无法区分 slirp 未转发 / SYN 在 guest 侧未上送 / 监听 socket 未 `accept`。
  - 方案：把 `-netdev user` 换成 `-netdev tap` + `tcpdump`（或保留 slirp 但在 hostfwd 上加 `-object filterdump`），对同一次失败采集三视图：宿主侧 `:8080` 握手、tap 上到 guest 的帧、guest 串口日志；再以 `MAX_TRIES` 双值（20 / 120）各跑 6 轮复现差异。
  - 状态：[]

- **S-2. TX 掉帧可观测性**
  - 描述：`EGDFTxToken::consume` 丢弃 `ops.send` 的返回值，驱动侧失败即静默掉帧，事后无从判断是否发生过。
  - 方案：在 `smoltcp_impl.rs` 的 consume 处保留返回值并累加 `TX_SENT/TX_DROPPED` 计数，经既有 net 统计出口可读；不为单次诊断引入 printk 风暴。
  - 状态：[]

- **S-3. `poll_at` → hrtimer 周期驱动**
  - 描述：`SmoltcpNetStack::poll_at()` 恒 `None`，smoltcp 的重传/超时/ NDP / DHCP 续约时限只能靠 1ms tick 空转推进。
  - 方案：把 `iface.poll_at()` 返回的下次截止时刻注册到 hrtimer，到期触发 `poll_network`；tick 侧 poll 频率随之可降。**注意**：本项是效率与低功耗优化，不得再作为 e2e 抖动的解药表述。
  - 状态：[]

- **S-4. 用户态回显服务去掉 `FLUSH_MS` hack**
  - 描述：`FLUSH_MS = 50` 是对"close 丢弃尾包"的用户态兜底；内核侧三步序已闭合该缺口。
  - 方案：删除该延时后连跑 6 轮 boot test 验证不回退，确认语义归属内核；同步检查 `MAX_RECV_TRIES`/`POLL_MS` 是否仍有等价必要。
  - 状态：[]

- **S-5. 门槛稳定性回归承载**
  - 描述：`qemu_boot_test.sh` 单次通过不足以证明抖动消除。
  - 方案：新增脚本级重复选项（如 `E2E_REPEATS`，默认 1，CI 夜间跑 5），把"连续多轮 0 失败"变成可门禁的指标。
  - 状态：[]
