# TCP 回显 e2e 入向故障取证：RX 假设证伪，与"离核中 prev 无保护"的定因

> P-17 的故障**不在 e1000 RX，也不在信号路径**：承载网络服务的任务在 `block()` 已离开可运行记账、而 `schedule()` 的 prev 处理尚未完成的窗口内被并发唤醒投进运行队列，随后被同一次 prev 处理**摘除而不改状态**，从此停在 `Ready && !on_rq && rq_cpu==NO_RQ_CPU` —— 唤醒面再无选集条件拾起它，`accept` 永不发生。取证六批共 30 轮实测，软挂轮的"最近摘除"读数 100% 指向 `schedule()` 的 prev 撤账站点且瞬间状态恒为 `Ready`。原 P-17 的 RX 侧修复项（`receive_packet` 改动）据此取消；本文是后续调度工程的输入依据，不含修复决策。

## 一、复现口径与两种失败形态

判据沿用 `scripts/qemu_boot_test.sh` 的端到端段与取证跑批 `other/rx-fx3-run.sh`（本地留证，不入库）：guest 打印 `[tcp] Listening on 0.0.0.0:80` 记 `ready=1`；宿主经 `-netdev user,hostfwd=tcp::8080-:80` 连三轮 `sendall`/`recv`，记 `ok/3`；`-object filter-dump` 同步落 pcap 做包级交叉核对。x86_64 `-smp 2 -m 512`。

两种形态在**同一构建、同一脚本**上交替出现，且互斥：

| 形态 | 现场特征 | 串口 | 运行队列 | 结论口径 |
|---|---|---|---|---|
| **Mode B 软挂** | 内核全程健康、tick 正常、5s 取证窗口持续打印；服务任务不再上核 | 活着（实测打到 174s / 89s） | 两棵 CFS 树 `tree_len=0`；一核落 idle，另一核被 `keep_current` 兜底长期留驻同一个 current | 本轮定因闭合 |
| **Mode A 硬挂** | 串口在数秒内停更，之后不再有任何输出 | 死（判停阈值 12s 无新字节） | 采不到（停更早于首个 4s 窗口） | 仅取得部分证据，见第五节 |

Mode B 的病灶读数（`other/rx-fx3-evidence/plain1.log`，孤儿态持续 170s）：

```text
[fx-t] pid=5 st=1 rsn=3 slp=3451 now=174094 tk=0 vr=3119 onrq=0 rq=FFFFFFFF ks=FFFF800007EFC000
[fx-q] cpu=0 cur=9 idle=3 prev=0 pNrs=0 qNrs=0 mark=45 take=38
[fx-q] cpu=0 pickN=12326 pickS=20
[fx-q] cpu=1 cur=2 idle=2 prev=0 pNrs=0 qNrs=0 mark=66 take=49
[fx-q] cpu=1 pickN=334057 pickS=21
[fx-tree] cpu=0 tree_len=0 left=[0,0,0,0,0,0]
[fx-tree] cpu=1 tree_len=0 left=[0,0,0,0,0,0]
[fx-z] unb=46 ufail=1 claimf=1 reject=0 cand=43 wake=43 keep=6156 invRdy=35
```

孤儿不在任何树上，两核无候选可拣（cpu1 拣空 33.4 万次），于是 cpu1 落 idle、cpu0 靠 `keep_current` 反复留住 pid=9。

`st=1` 是 `Ready`，`onrq=0` 且 `rq=0xFFFFFFFF`（`NO_RQ_CPU`）表示不在任何队列、不属任何核，`rsn=3`（`BlockReason::Sleeping`）与早已过期的 `slp` 说明它曾经正常阻塞、并被正常唤醒过。`invRdy` 每窗递增即"扫描面反复看见这个不可见的 Ready 任务，却没有任何路径能救它"。

## 二、RX 假设的证伪

存档读数（`other/rx-fx-evidence/gdb.serial.log`）曾把瓶颈收敛到描述符层：`[fx-n] call=71717 frame=9 stoprdh=71707` 的字面读法是"45s 内 poll 空转 7.1 万次，几乎每次停在 RDH 指向的描述符不可读"。本轮给驱动补了**硬件视角直读**（`privileged/driver/net/e1000_io.rs` 直读 `RDLAH/RDLEN/TDT/RDBAL`，与 `functions/driver/net/e1000.rs` 的 `fx_ring_snapshot`/`fx_classify_stop` 配套），读数否掉了这个读法（`other/rx-fx3-evidence/plain1.log`）：

```
[fx-n] t=45055 call=80242 frame=10 stoprdh=80231 stopdd=0 err=0 ring=10/10 rdt=9 irq=0/b0/a0 icr=0x0 izero=0
[fx-r] t=45055 rdhReg=10 rdtReg=9 rdlen=2048 size=128 ddTail=0 lastRdtWrite=9
[fx-m] t=45055 ddCnt=0 ddmap=0x0000000000000000 frozen=192 tailAtFreeze=10 rel=3 frameAtRel=6 rdhBlkReady=0
```

- `rdhReg == 驱动自己的 tail`、`rdtReg=9` 且 `ring=10/10`（每轮都完成了描述符回收与 TDT 回写）⇒ 硬件没有扣留任何描述符，也没有向驱动出示任何可读帧。`stoprdh≈call` 的真实语义是"链路空闲"，不是"收不到"；判据 `rdhBlkReady=0`（从未出现"RDH 处 OWN 位仍归硬件"）与 `ddCnt=0`（无一个描述符被硬件写完）双向坐实。
- 包级交叉核对：同一轮 `tcpdump -nn -r plain1.pcap` 共 22 帧，其中内核对宿主 SYN **主动回了 RST**（`[R.]`），说明帧确实进了协议栈并被消费；驱动侧 `FX_FRAME_OK` 计数与 pcap 入向帧数逐一对齐。成功轮 plain2 的 pcap 是完整的三次握手 + 回显（50 帧）。
- `irq=0/b0/a0` 与 82540EM 无 MSI-X capability、`msix_enable` 返 `None` 后回落轮询模式一致（P-19 已订正的既定回退），不是缺陷。

RX 描述符层就此排除，`receive_packet` 的修复项取消。

## 三、六批取证的分水岭

归因不靠推演，靠逐轮加定点探针读数；下表每行都是一次"上一轮的假设被下一轮的读数改写"。

| 批次 | 探针增量 | 关键读数 | 改写了什么 |
|---|---|---|---|
| fx-evidence | 驱动/softirq/accept 四段计数 | `frame=9 / stoprdh≈call` | 立"RX 描述符层"假设 |
| fx2 | `[fx-t]` 任务态逐 pid 打印（尚无 `onrq`/`rq` 字段） | `other/rx-fx2-evidence/round3.e2e.log`：`pid=5 st=1 rsn=3 slp=4995 now=6101 tk=0` 逐秒重复 —— `slp` 早已过期而 `st=1` 恒定不动，醒着却永不上核 | 首次把矛头指向"Ready 但不可运行"，但无记账字段无法区分形态 |
| fx3 | 硬件视角直读 + 表锁栈锚持有者 + 双派/同栈/异归属 `requeue` 检测 | 硬件视角 + pcap ⇒ RX 作废；gdb2 原子读数 `FX_TBL_LOCK_N=91451663 / CONTEND=2086173 / HOLD_SP=0 / LONGWAIT=0 / SAMESTACK=0`、`FX_INVISIBLE_READY=37`；锁字为 0（空闲） | 病灶从 net 面整体迁到调度面 |
| fx4 | Ready **写入者**站点归因（`signal.rs` 三处裸写 + `place_locked` + `unblock` RT/DL + `set_state_safe` 通用捕获） | 孤儿轮 `[fx-orphan] pid=5 最近 Blocked→Ready 写者 site=6 计数=[0,0,0,37,0,0,37]`（`FX_RPLACE == FX_RAPI`） | **否证**"signal 裸写 `state.store(Ready)` 造成孤儿"——三个裸写站点全程为 0，每一次 `Blocked→Ready` 都发生在 `place_locked` 的同一临界区内（已入树、已置 `on_rq`） |
| fx5 | 反向归因：**谁把 Ready 任务从树上摘掉**（四处撤账站点 + 摘除瞬间状态） | 三次复现全部 `site=1 state=1 pid=孤儿本身`；`site3(steal)=0` | 孤儿是"入队后被摘走"，摘走者是 `schedule()` 的 prev 撤账（本批 `pick` 站点登记口径写错，见第六节） |
| fx6 | 修正 `pick` 登记口径 + 增"入树时仍在核上"反查 | 6 轮中 2 轮软挂均 `摘走Ready=1`（整轮仅这一次 Ready 摘除，且正是孤儿）；1 轮 `摘走Ready=51`；健康轮 `摘走Ready=0` 但 `入树仍在核上=1`、`doubleDisp=1`；`requeueForeign=0` | 定因闭合，并暴露同一窗口的另一面（Mode A 前提） |

## 四、Mode B 定因：离核中的 prev 不受任何凭证保护

链条上的四个既有事实（均可在撤桩后的源码里直接读到）：

1. **`timer_sleep` 的挂起是两步。** [`sleep.rs` 的 `timer_sleep`](../../src/kernel/privileged/timer/sleep.rs) 先写 `sleep_until`，再调 `scheduler_block(Sleeping)`，最后才 `scheduler_schedule()`。`block()`（见 [`scheduler.rs` 的 `block`](../../src/kernel/privileged/proc/scheduler.rs)）在返回前就已完成 `set_state_safe(Blocked)` + `cfs_deactivate(pid)` —— 后者把 `cfs_on_rq` 清成 false、把 `cfs_rq_cpu` 提交为 `NO_RQ_CPU`，**而此刻任务仍然是本核的 `current`**（真正的离核发生在紧随其后的 `schedule()`）。
2. **`place_locked` 的 `on_cpu` 闸门按归属核查 `current`。** [`scheduler.rs` 的 `place_locked`](../../src/kernel/privileged/proc/scheduler.rs) 拒入树的判据是 `per_cpu_for(dst_cpu).current == pid || (src_cpu != NO_RQ_CPU && per_cpu_for(src_cpu).current == pid)`。`block()` 刚把 `src_cpu` 清成 `NO_RQ_CPU` ⇒ 源侧那半条件被跳过，而 `dst_cpu` 是唤醒者自己的核 ⇒ **一个仍在核上跑的任务，在这一步得不到任何保护**。fx6 的反查探针直接量到了它：健康轮也至少命中一次（`入树仍在核上=1 (cpu=0 pid=5)`）。
3. **`schedule()` 的 prev 处理不是原子的。** 本函数在 `per_cpu.current.store(next)` **之后**才采样 `prev_state_code`，再据该快照二选一：可重排则 `cfs_rq.requeue(pid, vr)` 直插**本地**树（全程不经认领凭证），不可重排（Blocked/Zombie）则再调一次 `cfs_deactivate(pid)`。从快照到落地之间不持有任何能阻止并发搬动者的凭证；本核中断此时已关，但**拦不住另一个核的唤醒**（`Scheduler::tick` 的睡眠扫描、`poll_network` 的电平重扫、`wait_queue` 的出锁唤醒都会在远核调 `unblock`）。
4. **`cfs_deactivate` 摘走任务时不动状态。** 它只做 `dequeue` + `cfs_on_rq=false` + `commit(NO_RQ_CPU)`。若此刻任务已被并发唤醒置成 `Ready` 并入了远核的树，这次摘除就把它变成"`Ready` + 不在任何树 + 不属任何核"。

于是同一个窗口根据并发事件的**到达次序**分叉出两种后果：

```
cpu X (服务任务 pid=5)                       cpu Y (远核唤醒者)
------------------------------------         ----------------------------------
block(): state=Blocked
         cfs_deactivate(): on_rq=false
                           rq_cpu=NO_RQ_CPU                       ← 此刻 pid=5 仍是 cpu X 的 current
                                          place_locked() 闸门看不见 X (src=NO_RQ_CPU)
         current.store(next)  (已离核)
         采样 prev_state_code = Blocked(3)
                        [A] 唤醒先于采样落地 → 快照读成 Ready(2)
                            → requeue 插进 X 树，Y 树里还有一份 ⇒ 两棵树
                            ⇒ 同一块 64KB 内核栈被两核交叠使用 (Mode A)
                        [B] 唤醒落在采样之后
                            → cfs_deactivate() 从 Y 树上摘走它
                            → 状态仍是 Ready ⇒ 永久隐形 (Mode B)
```

fx6 软挂轮的读数就是 [B] 的现场（`other/rx-fx6-evidence/plain1.log`，整轮只发生一次 Ready 摘除）：

```
[fx-orphan] pid=5 最近 Blocked→Ready 写者 site=6 pid=5 计数=[0,0,0,4,0,0,4]
[fx-off]   最近摘除 site=1 state=1 pid=5 计数=[4,7,0,0] 摘走Ready=1 (孤儿 pid=5)
```

`site=1` 即 `schedule()` 的 prev 撤账，`state=1` 即摘除瞬间任务为 `Ready`，`pid` 与孤儿同号。`[4,7,0,0]` 四处站点分别是 `block()` 撤账 4 次、prev 撤账 7 次、`pick` 不可调度摘除 0 次、`steal` 摘除 0 次 —— 摘走 Ready 的只有 prev 撤账这一处。

### 4.1 为什么一次摘除就不可恢复

孤儿态在本内核里**没有任何自愈入口**，这是三条既有判据合取出来的：

- 睡眠唤醒扫描与 `poll_network` 电平重扫的选集条件都是 `state == Blocked`（见 [`scheduler.rs` 的 `tick` 内睡眠扫描](../../src/kernel/privileged/proc/scheduler.rs)）—— 任务一旦离开 `Blocked` 就永不再被重扫拾起；`unblock()` 自己的注释早已把这条讲清："一旦搬定失败，该任务既不在树上也不再会被重扫拾起 —— 一次可恢复的丢唤醒就变成永久隐形"。
- `unblock()` 确实能处理"`Ready` 且不在队"（`need_enqueue` 的第二个分支），但它只在**有人再次唤醒它**时才跑；而 [B] 路径恰恰是把那一次唤醒消费掉了。fx3 的 `unb=46 wake=43 keep=6156` 说明事后既没有新的唤醒投送、也没有任何候选可拣，两核只能反复 `keep_current`/落 idle。
- `invRdy`（`FX_INVISIBLE_READY`）每窗递增而其余计数不动，是"扫描面反复看见却无能"的直接计量。

`[fx-off] 摘走Ready` 在健康轮恒为 0，在软挂轮 ≥1。fx6 的 plain5 高达 51 次（末次命中 pid=10，该任务最终读数就是 `st=1 onrq=0 rq=FFFFFFFF`）—— 说明存在"摘走 → 事件唤回 → 再摘走"的 flapping 而不是只有一次性死亡。也就是说：**这一处摘除事件本身就是故障的充分判据**，不是与故障并存的旁证。

## 五、Mode A：同一窗口的另一面，证据只取到一半

[A] 分支的后果是同一任务挂进两棵树，两核各拣一份，于是两个 vCPU 同时使用**同一个进程的同一块 64KB 内核栈**（内核栈按进程分配于 `Process::kernel_stack`，`KERNEL_STACK_SIZE=65536`，故"两核同栈"等价于"同任务双派"）。fx3 的硬挂轮留下了当时唯一可信的活体读数：

```
[fx-tbl] SAMESTACK cpu=1 line=898/909 sp=0xffff800007ccfc01 holder_sp=0xffff800007ccfb68
[fx-rqf] cpu=0 requeue pid=… 但归属 rq_cpu=…      (FX_REQUEUE_FOREIGN 命中)
GPF external=false idt_flag=false table=IDT index=0 → [FREG] Rollback
```

持有者栈锚比等待者**更深**，而同一条控制流不可能在持锁期间把栈帧收回后再深进去 —— 只能是两个核共用同一块栈。fx4 的 gdb1 轮重复了这一形状（`SAMESTACK cpu=1 line=975 holder_line=964`，即 `with_process()` 在等一个仍在本栈上的 `get()` 持有者），其后串口立即变哑并出现乱码截断。

三条证据使它只能算"部分定因"，必须如实登记：

- **[A] 的直接计数器在多数轮不命中。** fx3 的 `[fx-y] requeueForeign/doubleDisp/sharedKstack` 在软挂轮全 0，fx6 六轮 `requeueForeign` 全 0、`doubleDisp` 只在健康轮 plain2 命中 1 次 —— 说明双派是**罕见事件**（且一旦被探针拖慢就更难命中），而 `[fx-rqf]` 活体打印在 fx3 硬挂轮确实出现过，两者不矛盾但样本极少。
- **硬挂轮的病灶时刻采不到任何读数。** fx6 的 plain3/plain6 串口分别止于 3.988s / 2.662s，早于首个 4s 取证窗口，因此这两轮的"硬挂"没有计数器支撑，只有形态学证据。
- **gdb 快照本身受扰动且不可完全信任。** 无 DWARF，`nm` 取址 + `x/1dg` 直读；表锁锁字偏移 `0x800` 系由停顿现场寄存器实测推得；fx3 gdb2 的快照里两核都在 `timer_tick_handler` 里（一核 `SpinLock::raw_lock` 自旋等表锁、一核在 `apic::get_id()` 读 MMIO），而同一时刻原子读数 `HOLD_SP=0 / LONGWAIT=0 / SAMESTACK=0` 且锁字为 0（空闲）—— 该快照无法区分"真死锁"与"gdb 停下恰好采样到繁忙的 tick 路径"。gdb 轮（`-s -S`）的 `ready`/`ok` 结果不可与 plain 轮同列计入通过率。

结论口径：Mode A 与 Mode B 共享第四节那个窗口（差异只在并发到达次序），但把 Mode A 一路推到 GPF/FREG 回滚之后"锁被带着泄漏"的完整链条，需要后续调度工程用新的定点手段（例如把同栈/双派检测的打印限额前移、给 prev 处理加事务边界断言）复现验证；本文不作为定论。

## 六、测量边界（取证方法学，供后续工程复用）

- **探针会放大故障，不会制造故障。** fx6（带完整站点探针）6 轮 plain 仅 1 轮回显成功，而未带站点探针的 fx3/fx4 同参数下通过率约 1/3：`place_locked` 新增的全核 `current` 反查、四处撤账站点新增的一次 `with_process` 表锁读写，都拉长了"采样→落地"窗口本身。所有结论只取自"探针只可能放大因而只会多报不会漏报"的读数（如 `摘走Ready`），不取自通过率估计。
- **打印一律限窗 + CAS 认领 + 限额**。上一轮教训仍然成立：在 `poll_network` 入口持 `PROCESS_TABLE` 锁期间做串口打印会把锁持有数十 ms（115200 波特），构成直接扰动；本批全部改为"锁内取值、锁外打印"，孤儿归因行全局限额 4 次。
- **热路径不碰 MMIO**。`per_cpu()` 经 `current_cpu_index()` → `Arch::cpu_id()` → `apic::get_id()` 是一笔 MMIO 读，而表锁每轮取到千万级（fx3 实测 `FX_TBL_LOCK_N=91,451,663`），因此探针必须用栈锚/快照而非现取核号（fx3 gdb2 快照里一核正卡在 `apic::get_id()`）。
- **一处登记口径错误及其修正**：fx5 把 `pick` 站点登记在"算 schedulable 之前"，导致每一个合法候选都被计了一次"摘走 Ready"（读数 `计数=[69,72,111,0] 摘走Ready=112` 中的 111 均属误计）；fx6 已改为仅在真摘除分支登记，误计消失（健康轮 `[7605,7607,0,0]摘走Ready=0` 即反证：两万余次撤账都不碰 Ready）。本批引用一律以 fx6 为准。
- **并发 printk 会丢串口字节**。多核同时写 115200 串口会出现内容跳接（实测出现 `state_coe`、`holder_lholder_sp`、`+++6[fx` 这类错乱行），归因行必须拆短、拆行，长读数一律走原子快照。
- **pcap 与计数器交叉核对是区分"无事件"与"丢事件"的唯一可靠手段**。`filter-dump` 的帧数与驱动 `FX_FRAME_OK` 对齐，才把"链路空闲"与"收不到"区分开；单看驱动内部的 `stoprdh≈call` 会误读（本次就是这么走错的）。

## 七、本轮发现的独立缺陷（不属本病灶，另开工程）

- **进程时间记账实际上从不执行**（已登记为工程条目 P-22）：六轮实测所有 `[fx-t]` 行 `tk=0`（该字段直读 `Process::tick_count`），用户态 `times()` 恒 0。链条是存在的：`Scheduler::tick` → `SCHEDULER_EX.tick_accounting()` → `proc_account_tick()`，但 `tick_accounting` 的记账分支被 `current != 0` 门控，而该字段语义是 `*mut Thread`——`ThreadManager::create_thread` 零生产调用者（仓内只剩注释引用），进程根本无对应 `Thread` 可解析 ⇒ 记账腿断线。调度器自己的 vruntime 推进走的是另一条路（`tick` 里钳 `min_vruntime`），所以"时基单源已完成"与"进程时间记账从未接上"同时成立。
- **`per_cpu()` 在热路径上走 APIC MMIO**（已登记为工程条目 P-23）：表锁取次数千万级而每次 `per_cpu()` 都是一笔 MMIO 读，是本工程里性价比最高的一项开销待治。
- **`keep_current` 兼底形态异常**：软挂轮 `keep=3076~6156` 而 `pickN` 高达 11561（fx3 gdb2），两核长期在"无候选但留住 current"与 idle 之间打转；该分支本身的正确性与它的计数规模需单独评估。
- **gdb 读数命令文件对不可读符号不容错**：fx3 的 gdb1 因一个 `Cannot access memory` 就中止了整个 `FX_*` 直读段，白丢一轮现场。`fx_readout` 需改成逐符号容错。
- **`sleep_until` 与 `thread.sleep_until` 双字段并存**（上一轮已登记，本轮未动），仍属待定论项。

## 八、给后续调度工程的输入（只列不变式与候选面，不做决策）

本轮取证把需求收敛到一条可验证的不变式主张：**一个任务从"决定离核"到"处理完 prev 账目"的整个区间，必须对并发搬动者不可见**。当前代码里承担这个职责的三个机制各自都有缺口：

- `cfs_on_rq` 同时兼任"在树上"与"受搬动保护"两个语义，而 `block()` 一清就把保护也清掉了；
- `place_locked` 的 `on_cpu` 闸门以 `rq_cpu` 为索引，恰好漏掉 `NO_RQ_CPU`（即"刚被 `block()` 撤账但仍在本核上跑"这个必然经停的中间态）；
- `RqClaim` 认领凭证只覆盖 `cfs_place_on`/`cfs_deactivate` 的**局部**，`schedule()` 的 prev 处理（采样与两个落地分支）全程在凭证之外。

可供工程阶段评估的候选面（不预设选择，需按 AGENTS.md §12.1/§12.3 先确认实现路径）：把 prev 处理纳入认领事务（凭证持有跨"采样→落地"）/ 引入显式 `on_cpu` 语义并将 prev 收尾推迟到切换完成后的新任务上下文（Linux `put_prev_task` + `finish_task_switch` 分工）/ 唤醒面对"仍在核上"的任务一律拒绝投送。验收手段上，`摘走Ready` 计数器（现属探针）建议以调试断言的形式固定下来，作为回归锁的判据：健康轮恒 0，故障轮 ≥1，是本轮最干净的区分信号。

工程条目与决策见 [net-e2e-tcp-echo-flakiness.md](../plan/net-e2e-tcp-echo-flakiness.md)：取证结论已写入 P-17（原 RX 假设与 RX 修复项据此作废），修复工程新立为 P-21。
