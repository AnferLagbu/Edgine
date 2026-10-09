# TCP 回显 e2e 抖动根因定位报告：CFS 时基背离、运行队列记账泄漏，以及一段伪装的构建污染

> 本报告是一次排查工作区的结果快照：`scripts/qemu_boot_test.sh` 的 x86_64 端到端 TCP 回显阶段长期存在非确定性失败（宿主 `recv` 5s 超时、guest `accept` 计数缺失）。根因最终落在 **CFS 调度器的时基模型**，不在网络路径。报告给出完整证据链、四条被证伪的假设、修复前后的门禁数据，以及验收过程中一段险些把根因归错的**构建产物跨架构污染**插曲。
>
> 报告冻结当时事实，作为后续制定 plan 与启动修复工程的输入依据。所有结论均附源码位置或命令证据。工程条目与决策见 [net-e2e-tcp-echo-flakiness.md](../plan/net-e2e-tcp-echo-flakiness.md) 与 DECISION-098 / DECISION-099。

## 一、现象与判据

端到端阶段的判据是固定的三段：guest 侧打印 `[tcp] Listening on 0.0.0.0:80` 视为服务端就绪；宿主经 QEMU `hostfwd tcp::8080->:80` 连三轮，每轮 `create_connection(timeout=5)` → `sendall` → `shutdown(SHUT_WR)` → `recv` 校验载荷；guest 侧 `accepted`/`echoed`/`closed` 三计数均需 `>= E2E_ROUNDS`。

失败形态始终一致：服务端就绪判定**通过**，宿主 `connect` 也**成功**，随后 python 侧 `recv` 抛 `TimeoutError: timed out`；guest 日志里 `accept` 计数缺失或停在 1–2，且**没有任何** e1000 发送失败或 RX 描述符错误。

抖动强度用 `E2E_REPEATS` 承载（同一脚本、同一镜像、连跑 N 轮，失败轮串口日志另存 `*.failN.log`）。修复前的 20 轮基线：**1/20 失败（5%）**。

## 二、四条被证伪的假设

排查依次提出四个假设，全部被实验否掉。把它们留在这里，是因为每一条当时都"看起来更可能"。

**假设一：close 丢弃未发出数据。** 这是真实的 POSIX 语义缺口（`sock.close()` 与 `sockets.remove(handle)` 之间没有 egress 推进点，尾包与 FIN 随 socket 销毁作废），但撤销排空的单变量 A/B（1s 探针窗口，无排空 8/8 通过）表明它与抖动无因果。三步序按语义本身的理由保留（DECISION-098）。

**假设二：x86_64 egress 无独立驱动源。** 失实。`timer_irq0_handler`（[timer/irq.rs:30-45](../../src/kernel/privileged/timer/irq.rs)）第 42 行每个 PIT tick 都调 `poll_network()`，aarch64 同构（[exception.rs:738](../../src/kernel/privileged/arch/aarch64/exception.rs)）。该误判源于一次被 `head -20` 截断的 grep —— 归因类搜索不得截断输出。

**假设三：NIC 静默掉帧。** `EGDFTxToken::consume` 确实曾丢弃 `ops.send` 的返回值（[smoltcp_impl.rs:147-157](../../src/kernel/privileged/net/smoltcp_impl.rs)），驱动返 `-1` 时帧静默消失且无计数 —— 这是真实的**可观测性缺口**，但补上 `TX_SENT`/`TX_DROPPED` 计数后，25 轮（含全部失败轮）TX 失败次数恒为 0，故非本次故障因。另澄清一项早期误判：仓内并非"两份 e1000 驱动"，业务逻辑只有 `functions/driver/net/e1000.rs` 一份（0 unsafe），`privileged/driver/net/e1000*.rs` 只提供 `TxRing`/`RxRing` 与 MMIO 安全代理，符合 AGENTS.md §4.1 的机制/功能分离。

**假设四：slirp 入向丢帧。** 这条当时最自洽：`hostfwd` 语义使宿主 `connect()` 成功**不蕴含** SYN 抵达 guest（slirp 先与宿主 app 完成三次握手，再以自己的 emulated TCP 向 guest 发起连接），"宿主成功 + guest 不 accept"完全说得通。裁决办法是包级取证 —— 用 `-object filter-dump` 把 `n0` 双向帧写入 pcap（脚本 `E2E_PCAP` 口），结果失败轮的 SYN **已到 guest** 并被协议栈收下，缺的是 guest 侧的**消费**而非投递。入向通道就此排除。

`poll_at`→hrtimer 的缺失（`SmoltcpNetStack::poll_at()` 恒 `None`）也一度被当作候选解药。它是真实的机制缺口（低频空闲时 poll 全靠 tick 空转），但不产生秒级不确定性，降为独立优化项。

## 三、根因：两个时间域 + 两份可运行集

插桩落在 tick / `schedule` / `pick_next` / `poll_network` 路径后，失败轮的真实图景是：`tcp_echo_server` 子进程早已是 `Ready` 且在 CFS 树上，但**拿不到时间片 4~10 秒** —— 恰好跨过宿主 5 秒的超时线。两个独立缺陷叠加造成：

### 3.1 IC1 背离：入队钳制不回写任务

旧 `CfsRunQueue::enqueue` 把**树键**钳到 `min_vruntime` 附近，却不回写 `Process::cfs_vruntime`。于是"正在运行的任务"按自身 vruntime 前进，"树上的任务"按钳制键排队，两条时间轴各自漂移，且没有任何机制让它们收敛。实测失败轮：

```
curr vr=45   floor=5152
```

当前任务的 vruntime **远低于**队列下界。`cfs_should_preempt` 计算"树最左任务比当前任务落后多少"时用 `saturating_sub`，负差被压成 `0` ⇒ 恒不置 `need_reschedule` ⇒ 该核一旦切入长任务就永不重调度。用户态 `init` 的父进程 `busy_wait(b'+')` 正是这样一个长任务。

### 3.2 IC2 背离：并行计数器与红黑树失真

旧实现同时维护 `nr_running`（原子计数器）与 `tree`（红黑树）两份"可运行集"表示。跨核迁移与阻塞撤账的加减路径不完全对称，实测出现：

```
nr=108   tree=0
```

计数器认为有 108 个可运行任务，树是空的。于是 `is_empty()`（按计数器）判定"有活可干"，`pick_next`（按树）却取不到候选，回落到"保持当前任务"；抢占判据同样按树取最左键而无候选可比。树上的真实任务变成"隐形饥饿"。

### 3.3 为什么这不是调参问题

四条直觉缓解都无效：放宽宿主超时（饥饿无界，放到 10s 也能撞上）、加大抢占粒度（判据本身恒为 0，粒度无关）、给唤醒者加特判 unblock（已在 Ready 的任务不是"刚被唤醒"）、周期性整树 boost（旧 `boost_all_vruntime` 恰是第三条背离路径 —— 它抹掉相对次序）。缺陷在**模型**：同一个量（vruntime）被两处独立记账，同一个集合（可运行集）被两处独立表示，而代码没有任何不变式要求二者一致。

## 四、根治：单时基模型

重构按五条不变式从构造上封死背离路径（决策全文见 DECISION-099）：

| 不变式 | 内容 | 落地 |
|---|---|---|
| IC1 单一时基 | 任意任务的可比较 vruntime 落在 `[floor - CFS_MIN_GRANULARITY, +∞)` | `enqueue` 返回钳制落点且调用方**必须**回写 `Process::cfs_vruntime`；`placement()` 是该钳制的唯一实现 |
| IC2 结构即真相 | 可运行性判据一律取自 `tree` | 删除 `nr_running`；`total_weight` 依记账凭证精确加减，树内移动不动账 |
| IC3 下界单调 | floor 只由 tick 推进、只增不减 | floor 字段私有，仅 `advance_floor`（`fetch_max`）一条写路径；删除 `sync_min_vruntime` |
| IC4 可运行集 = 树 | 阻塞即出队撤账 | `cfs_deactivate` 幂等撤账；`pick_next` 摘除节点记账随迁；`load_balance` 记账随任务搬移 |
| IC5 相对次序不被抹掉 | 禁止整树折叠 | 删除 `boost_all_vruntime` |

投递路径收敛为单一入口 `cfs_enqueue_to`（新建 / 唤醒 / 迁入 / 抢占回落四条路全部经它），记账凭证改为 `Process::cfs_on_rq`（语义同 Linux `on_rq`：处于可运行记账中，上核运行期间仍为 `true`），使入队/撤账天然幂等。`unblock` 的 `need_enqueue` 判据取自"状态 + 凭证"，避免"已在树里再入一次"。锁序保持 `cfs_rq → PROCESS_TABLE` 不变，抢占判定改为同域比较。

被删除的三个旧机制（`sync_min_vruntime`、`boost_all_vruntime`、`nr_running`）各自对应一条被实测到的背离路径，其保留理由均不成立，登记在此以免被"顺手加回"。

## 五、修复前后数据与测试承载

| 阶段 | 强度 | 结果 |
|---|---|---|
| 修复前基线（干净镜像、无插桩） | `E2E_REPEATS=20` | **1/20 失败**（第 17 轮，`TimeoutError`） |
| 修复后验收（同一脚本、同一镜像构型） | `E2E_REPEATS=20` | **20/20 通过**，含 `accept=3 echo=3 close=3` 与 FD 回收/P4 里程碑全绿 |

原始日志：`other/build/log/before_baseline.log`、`other/build/log/after_acceptance.log`。

测试承载分两层。语义层是 [cfs.rs](../../src/kernel/privileged/proc/cfs.rs) 内联单测 15 例（placement 钳制、floor 单调、幂等入队/撤账、`pick_next` 摘除后再 `dequeue` 返回 `false`、`calc_time_slice` 除零/权重为 0 的边界）。接线层是 [cfs_single_time_base_test.rs](../../host-tests/tests/cfs_single_time_base_test.rs) 9 例源码契约：锁死"入队落点必须回写任务 vruntime"、"不得存在并行可运行计数器"、"floor 只有 `fetch_max` 一条写路径且字段私有"、"阻塞/死前代必须撤账"、"tick 必须推 floor 并同域比较"、"load_balance 记账随迁"，并断言内联用例存在。这一层是必要的：缺陷本身是跨文件的**关系**（队列 vs 任务、计数器 vs 树），只测队列语义抓不住"调用方忘了回写"这类回归。

## 六、插曲：一段伪装的构建污染

修复后首轮 `E2E_REPEATS=20` 验收出现**前若干轮全失败**，形态与抖动毫不相干：init（pid=5）进入 Ring 3 后首条指令即 `#PF`。

```
[USER] SELF-CHECK: user_code virt=0x400000 -> phys=0x7E49000
[USER] SELF-CHECK: user_code first 16 bytes: FF 83 00 D1 FE 0B 00 F9 08 0B 80 52 20 00 80 52
[IDT] user exception: vec=14 err=0x4 rip=0x400000 rsp=0x7FFFFFF1BFF8 cr2=0x13E36105
[PROC] exit: pid=5 code=5
```

正常轮同一行是 `55 41 57 41 56 41 55 41 54 53 48 81 EC 98 00 00`（合法 x86_64 序言 `push rbp; push r15; …; sub rsp,0x98`）。

### 6.1 先排除"页表装配错"

`enter()` 的这行自检是按 `get_physical_in_pml4(cr3, vaddr)` 取目标物理帧再加 `KERNEL_BASE` 读出的（[user_proc.rs:1352-1366](../../src/kernel/privileged/proc/user_proc.rs)），即**用户页表目标帧的真实内容**，不是内核恒等映射；`cr2` 取自异常帧的 `fault_address()`。两者可信，结论随之从"CR3 装错"改判为"代码页内容本身是垃圾 ⇒ 垃圾指令访问非法地址 ⇒ `#PF` present=0/read/user"，与 `err=0x4`、`cr2 != rip` 自洽。

### 6.2 垃圾字节是什么

```
FF 83 00 D1  ->  0xD10083FF  sub sp, sp, #0x20
FE 0B 00 F9  ->  0xF9000BFE  str x30, [sp, #16]
08 0B 80 52  ->  0x52800B08  mov w8, #1
```

这是 **AArch64 函数序言**。嵌进 x86_64 镜像的用户态 ELF 架构错了。

### 6.3 污染通道与复现

`other/build/user/init.bin` 是 x86_64 与 aarch64 **共享的同一路径、原地覆盖**，内核在编译期 `include_bytes!("../../../other/build/user/init.bin")` 把它嵌进镜像（[init.rs](../../src/kernel/functions/init.rs) 的 `bin` 回退路径）。`ci/build.sh all` 的执行序是：build x86_64 → build aarch64（把 `init.bin` 改写成 aarch64）→ host-tests → 禁串检查 → **`link_kernel "x86_64"`**。末段链接因此把 aarch64 用户态嵌进 x86_64 内核，而整个过程 **0 error / 0 warning / rc=0**。

复现与反向验证都成立。可靠判据是**镜像内嵌 ELF 的 `e_machine`**（同一固定偏移处只有一份真实 ELF，其余非法位置上的 ELF 魔数字节均为噪声）：

```
$ ./ci/build.sh all                      # rc=0，全程 0 error / 0 warning
$ file other/build/user/init.bin
ELF 64-bit LSB executable, ARM aarch64
$ python3 -c '…扫描 kernel.flat 内 ELF 头部 e_machine…'
  off=0x19120f e_machine=0xb7 (AArch64)    # 启动它 ⇒ 与失败轮同形的 #PF 日志

$ ./ci/build.sh x86_64                    # 仅重跑 x86_64
$ file other/build/user/init.bin
ELF 64-bit LSB executable, x86-64
$ python3 -c '…'
  off=0x19120f e_machine=0x3e (x86-64)     # 同一用例立即通过
```

**尺寸不是架构判据**（本轮一度误用）：排查时我以 `kernel.flat` 尺寸区分好坏镜像（2271808 对 2387864），后续实测推翻 —— 同一 `e_machine=0x3e` 的干净 x86_64 镜像在全量与增量构建构型下可取 2334616 与 2271808 两个尺寸（差约 60KB）。将尺寸归为架构信号属于过度推断，只能用作"产物发生了变动"的弱提示。

### 6.4 为什么没有任何防线拦住它

`verify_elf` 只校验 ELF 结构，**不校验 `e_machine`** —— aarch64 ELF 照样打印 `verify_elf OK, entry=0x400000`。`qemu_boot_test.sh` 里的 `sync_make_state`（`.arch` 戳记 + `file` 校验 .o，不符则删中间产物重建）与 `check_kernel_fresh` 都只覆盖"中间产物陈旧"，不覆盖"嵌入的用户态二进制属于另一架构"，且后者只 warn 不 fail。`src/kernel/build.rs` 的 `cargo:rerun-if-changed=other/build/user/init.bin` 用的是相对路径（基准为包根 `src/kernel`，该路径实际不存在），而 `include_bytes!` 用 `../../../other/build/…`，重编触发条件与真实依赖不同源。

### 6.5 归因过程中的一次方法失误

期间用 `grep -c "Compiling kernel" <build.log>` 判断"内核有没有重编"，结论恒为 0 且不可信 —— `ci/build.sh` 内部把 cargo/make 输出接了 `| tail -5`（第 49、68 行），外部重定向拿到的日志本身就是截断的。改为直接跑 cargo 才看到真实的 `Compiling kernel v0.1.0`。同类失误（截断输出导致误判）在假设二已经犯过一次，两次都是**搜索/输出管道**而非代码造成的。

另一处干扰是插桩自身的扰动：在 `load_elf_from_memory` 加打印后，物理帧落点从 `0x7E49000` 变回 `0x21ED000` 且用例通过，一度被误读成 Heisenbug（时序敏感），实际是当时的镜像已被污染、插桩改变了分配序列而已。撤掉插桩复测才拿到真结论。

第三处方法失误即 §6.3 末尾所述的尺寸误用。本轮实际犯了三类同根的方法错误 —— 输出截断（上面两条与假设二）、观测行为干扰实验（插桩）、把相关性指标当因果判据（尺寸）。三者都不是代码问题，但都会直接拉长归因路径，故一并存此。

## 七、结论与遗留

抖动根因是 CFS 时基背离与运行队列记账泄漏，已按 IC1-IC5 根治并由两层测试承载；20 连门禁从 5% 失败转 0 失败，该强度可作为调度器的长期回归基线。网络路径（slirp 转发、e1000 发送、smoltcp egress 推进、close 排空）经取证均与本抖动无因果，但其中两项暴露了真实缺口：TX 掉帧计数（本轮已补）与 `poll_at` 未接 hrtimer（登记为 S-3，本轮不做）。

构建产物跨架构污染是**门槛可信度**问题而非本抖动的一部分：只要按 `./ci/build.sh all` → QEMU 的顺序跑门禁，产出的 x86_64 镜像就不可信。本轮以"门禁中先 `all` 后 `x86_64` 重建"绕开，根治方案（产物按架构分目录、`verify_elf` 校验 `e_machine`、门禁前镜像架构自检 fail-fast、修 `build.rs` 路径基准）登记为 S-6，待授权后单独实施。

本报告发布后 S-6 已按上述方案施工完毕（三条防线 + `build.rs` 路径修正，用户裁定全做），并以 `e_machine` 取证关闭：修复后 `all` 产出的 x86_64 镜像仍逐字节包含本架构 `init.bin`；受控污染实验（异架构产物拷入本架构目录后重链接）使镜像立即改嵌 `0xB7`，证明 `build.rs` 的 `rerun-if-changed` 修正后重编触发与真实依赖同源；且该污染镜像启动时内核在 0.93s 就报 `[ELF] verify_elf failed` + `Failed to load init ELF`，不再直到入口地址执行非法指令才 #PF。实施细节与可重跑证据见 [net-e2e-tcp-echo-flakiness.md](../plan/net-e2e-tcp-echo-flakiness.md) §5 S-6 实施记录（本节保留的原陈述为发布时事实）。

附带发现的预存问题（详见该 plan §6）：两份并行 `TICK_COUNT`（`proc::scheduler` 与 `timer::tick` 各一份）、`proc_sleep_ms` 以 `ms.div_ceil(10)` 隐含 100Hz 时基而实际名义 1000Hz（10× 偏差）、`sched_should_reschedule` 无调用者、有效 tick 频率约 1400/s 与名义 1000Hz 不符、boot→监听就绪时长抖动（修复前样本 10.7s↔23.9s，根治后需重测）。
