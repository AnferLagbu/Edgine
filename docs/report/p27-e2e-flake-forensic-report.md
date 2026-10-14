# P-27 e2e 残余 flake 四轮插桩取证报告

本报告记录 P-26（投送屏障 TOCTOU 原子性修复）合入后，x86_64 e2e
门禁残余概率性 flake（P-27 登记的 Mode X / Mode Y 两形态）的四轮
插桩取证过程与结论。取证基础设施为 `qemu_boot_test.sh` 的
per-boot pcap（filter-dump，commit 2c400661）与 `other/modeY_forensic*.sh`
采集/判读脚本；内核与 init 探针侧的全部 `[E2P]` 打点均为临对时
插桩，取证闭合后已 revert，不在树内。

## 采样方法与四轮数据

每轮 400 boot（40 × 10 outer），四轮概览：

| 轮 | 插桩形态 | FAIL | 命中率 | 主要产出 |
|---|---|---|---|---|
| 1 | 无内核打点（仅 per-boot pcap） | 4 | 1.0% | 模式清点：Mode X ×3 + 形态 Z ×1，Mode Y 零命中 |
| 2 | init 用户态差分探针（recv 后打 `r`、delay 后打 `d`） | 3 | 0.75% | Mode X 卡点二分：尾 `r` ⇒ 卡 `delay_ms`（睡眠唤醒链），尾 `d` ⇒ 卡 `recv` 本身 |
| 3 | 内核插桩首版：tick 睡眠扫描 WAK、`place_locked` DEF、`unblock` 失败 FAIL、`timer_sleep` SLP | 15 | 3.75% | 观测者效应放大命中率；发现全部 fail log 内核时间戳停在 2–4.7 s 的"全局冻结"形态 |
| 4 | 清理版：tick-IRQ 上下文零 printk（WAK/DEF 删除，SKIP 改锁内凭证 + `unblock` 尾部锁外消费），新增 NETIN（recv 阻塞腿）与 NETW（poll_network 唤醒腿） | 6 | 1.5% | 全局冻结形态原样复现——三轮"插桩自引入"假说被排除 |

三轮曾判停摆系插桩自引入（持 rq 锁 / tick IRQ 内 printk，与
`LOG_EMIT_LOCK`（IrqSpinLock）+ `serial_putc` 无超时自旋构成锁序死锁），
这是 AGENTS.md §11 点名的踩坑。四轮把全部打点移出 IRQ/锁域后，
6 个 FAIL 仍呈现完全相同的症状，说明**全局冻结是被测系统的真实形态，
不是观测者造成的假象**；三轮的量级放大（3.75%）才是插桩贡献。

## 四轮 FAIL 的共性形态

6/6 FAIL 全部为 Mode X，且逐例同构：

1. guest 依次点亮探针里程碑至 `[net6] TX ok`（部分到 `[mcast]`，
   即探针链不同位置），随后进入 recv/delay 循环，`r`/`d` 字符与
   `SLP`/`SKIP` 打点规整配对（睡眠→唤醒周期正常）；
2. 最后一条内核 log 恒为 `[E2P] SLP pid=5 wake_at=<tick>` —— pid=5
   （init 探针主进程）进入 `timer_sleep` 的睡眠入口打点；
3. 该打点后**全内核 log 静默**直至 40 s 超时：其它进程（pid=9 探针
   子进程不再出现）、tick 路径、网络路径均无任何输出；
4. 同 boot 的 per-boot pcap 时间线在**同一时刻**停止出帧（fail 轮
   全部 13–14 帧、末帧相对首帧 1.9–5.1 s；OK 轮对照 45–51 帧、
   7.2 s）—— guest 侧网络活动与串口输出同步消失。

即：失败不是"pid=5 单独饿死"，而是**整个系统在同一瞬间静默**，
串口与网络两条独立观测面同时定格。

## 结论（已坐实）

- **与 P-26 无关联**：四轮 400 boot 中 `FAIL pid=5 enqueue=false`
  （`cfs_enqueue_to` 投送失败）仅 2 例且均非致命（log 照常推进），
  屏障 TOCTOU 同族未复发。
- **`fcntl(F_SETFL, O_NONBLOCK)` 传播断层假设否证**：用户态 fcntl
  失败打印四轮 0 命中；且 NETIN（recv 进入内核阻塞腿）在全部
  400 boot 中 0 命中 —— Mode X 探针的 recv 从不进入内核阻塞路径。
- **`poll_network` 唤醒链不在卡死路径上**：NETW（poll_network 对
  pid=5 的 `scheduler_unblock`）同样 400 boot 0 命中 —— Mode X 期间
  pid=5 的唤醒只依赖 tick 睡眠扫描一条腿。
- **SKIP（`unblock` need_enqueue=false 吞唤醒）为正常现象**：OK 轮
  每 boot 约 40 次，非差异因子。
- **Mode Y（guest 六里程碑全过 + host recv 超时）四轮 1600 boot 零
  命中**：P-27 登记的那一例未再复现，暂不立项。
- **形态 Z**（SYN 到 guest 无 SYN-ACK，P-17 旧 Mode B 同族）仅一轮
  出现 1 例（1/1200），维持 P-27 残项观察，不并入 Mode X 工程。

## 全局冻结的候选机理（未坐实）

纯 log + pcap 两个观测面无法区分下面三种形态，这是本工程的收口
边界，也是后续取证的分岔点：

- **假说一（全员空转）**：pid=5 睡眠期间恰无其它可运行任务、亦无
  外部流量，系统真实活着但一切输出面空转 —— 冻结是假象，卡死点
  仍是"睡眠唤醒丢失"（tick 扫描未拾起 pid=5）。反驳线索：pcap 中
  停摆后连 host 侧经 slirp 的活动痕迹都同步消失，而正常轮该窗口
  本应有 guest 协议栈活动。
- **假说二（tick 时基停摆）**：`get_ticks()` 时基（PIT/APIC 或
  arch timer）在某刻停止推进，睡眠扫描的到期判据
  `wake_at <= get_ticks()` 永不成立，pid=5 永不醒；调度面随之空转。
- **假说三（关中断自旋死锁）**：某核持 `LOG_EMIT_LOCK` 在
  `serial_putc` 无超时自旋中 wedge（或 rq/PROCESS_TABLE 锁序倒置），
  另一核的同类打印在自旋上等锁 —— 两核先后钉死，所有输出面定格。
  与 fail 恒停在一条 printk（SLP）之后的形态吻合，但无直接证据。

## 后续建议

若继续本工程，第五轮插桩的判型目标是区分上述三假说，方向包括：
在**确定的任务上下文**（非 tick/IRQ 路径）加低频心跳打印与 tick
读数采样打印（如每 5 s 打一条 `[E2P] HB tick=<n>`），一次 boot 即可
判型"时基停摆 vs 锁 wedge vs 空转"；心跳必须避开一切锁域与 IRQ
上下文（三轮教训）。是否立项由 1.5% 残存率对 CI 门禁的实际痛感决定。

## 附：证据归档位置

四轮原始数据（fail log / per-boot pcap / 采集 stdout）归档于
`other/log/modeY_forensic{,2,3,4}/`（`other/` 不入 git），判读脚本为
`other/modeY_forensic.sh` 与 `other/modeY_forensic_analyze.sh`。
