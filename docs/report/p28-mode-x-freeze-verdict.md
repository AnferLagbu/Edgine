# Mode X 全局冻结三假说判型报告：关中断自旋死锁坐实，锁指向 PROCESS_TABLE

> 本报告是 P-28 取证工程的结果快照：对 P-27 遗留的 Mode X「整系统同一瞬间静默」形态（x86_64 端到端 TCP 回启 ~1.5% 就绪超时）做三假说判型。P-27 的 log+pcap 双观测面只能证明"全局冻结"这一现象，无法区分成因。本轮改用 **QEMU HMP monitor 现场快照**（零内核插桩），隔真实墙钟二次读 tick 计数 + 抓两核 RIP/HLT + 锁区/栈回溯符号化，把成因判到 **假说③（关中断自旋死锁）**，并把自旋点定位到 **`ProcessTable::with_process` 获取进程表锁** 的调用上下文。
>
> 报告冻结当时事实，作为后续制定 P-29 锁序修复 plan 的输入依据。工程条目见 [net-e2e-tcp-echo-flakiness.md](../plan/net-e2e-tcp-echo-flakiness.md) 的 P-27 / P-28 段，前序取证见 [p27-e2e-flake-forensic-report.md](p27-e2e-flake-forensic-report.md)。所有结论附现场样本或源码位置。

## 一、三假说与判型判据

P-27 坐实 Mode X 的现场形态是：内核串口 log 与 per-boot pcap **在同一时刻定格**，guest 停在 `[net6] TX ok` 之后、`[tcp] Listening` 里程碑之前，宿主 `connect` 成功但后续 `recv`/就绪检测 5s 超时。纯观测面无法区分三个成因：

| 假说 | 内容 | 判型判据（同一 fail 现场隔真实墙钟 Δt≈3s 二次读） |
|---|---|---|
| ① 全员空转 | 冻结是假象，tick 睡眠扫描丢唤醒，时基仍在走 | `TICK_IRQ_COUNT` **推进** |
| ② 时基停摆 | `get_ticks()` 冻结，到期判据永不成立，两核 idle | `TICK_IRQ_COUNT` **冻结** 且两核 **全 HLT=1** |
| ③ 关中断自旋死锁 | 某核关中断死自旋，饿死自身 tick 与对侧 | `TICK_IRQ_COUNT` **冻结** 且存在 **HLT=0 的运行核** |

`TICK_IRQ_COUNT`（timer IRQ 入口自增计数，`kernel.map` 实测符号 0x334d70）是"tick 时基是否还在走"的直接观测量；两核 RIP/HLT 靠 `info registers -a` 抓全（**关键：不带 `-a` 只能见当前核 CPU#0，正是上一轮误判的根源**，见 §四）。

## 二、判型手段：QEMU monitor 现场快照

不改内核一行代码，避免插桩自身引入锁序扰动（P-27 已吃过三轮教训）。做法：

- `scripts/qemu_boot_test.sh`（临对时插桩，已 revert）在 `E2E_MON=1` 时给 QEMU 挂 `-monitor unix:…,server,nowait`；
- 就绪超时触发时，`other/p28_mon_dump.py`（临对时工件，随插桩一并删除）作为 AF_UNIX 客户端连上 server-mode monitor：`server` 模式在客户端 `close()` 后自动重新 `accept`，故可在**同一冻结现场**开两次独立会话，中间隔 3s 真实墙钟，实现二次读；
- 会话内下 HMP：`info registers -a`（两核 RIP/HLT）+ `xp/6xg` 读 tick 时基区（0x334d40.. 六个 8 字节量）；判为 ③ 时追加 `xp/16xg` 锁 self 邻域 + `xp/64xg` 栈窗口，用 `kernel.map` 二分符号化落在代码段 [0x100000,0x300000] 的字为疑似返回地址（调用链）。

HMP 响应按 `(qemu)` prompt 计数取增量切片，规避命令回显污染与"错位一格"；`xp` 输出解析只取行首裸地址冒号后的数值。direct-map VA（≥0xFFFF800000000000）先减基址回落到 PA 再读。

## 三、现场证据：③ 主导，四例同形

采集量：P-27 的 forensic6（400 boot，1 例就绪超时现场）+ 本轮加采 forensic7 + forensic8（各 400 boot）。跨批复现率 ~1.5%–2%，与 P-27 同量级（插桩态 monitor 挂载使窗口略增）。落盘现场 **5 例**，判型分布 **4×③ + 1×②**：

**③ 主形态**（forensic6/fail28、forensic7/run1/fail8、forensic8/run2/fail13、forensic8/run6/fail28 四例逐字同形）：

```
CPU#0 RIP=0x00000000001e216f [<…::x86_64::X8664 +0x1]  HLT=1     # idle halt
CPU#1 RIP=0x0000000000154fcc [<…::sync::spinlock::SpinLock>::raw_lock +0x16]  HLT=0  # 死自旋
        RDI=0xffff800007fff308  （锁 self 指针，三例恒此地址）
COUNTERS_PER_TICK=2095xxx  ANCHOR_TICK=0
TICK_IRQ_COUNT  T0=T1（delta=0）      → tick 时基冻结
VERDICT = ③ 关中断自旋死锁
```

forensic8/run9/fail2 一例为 ②（两核全 HLT=1、tick 冻结在较高计数 7924）—— 无运行核，判 idle 时基停摆，作为次要形态见 §五。

## 四、符号化：卡在哪把锁，以及一处自我更正

对 ③ 三例的栈窗口用 **`kernel.bin`（未 strip ELF）addr2line（DWARF）** 精确化后，调用上下文（自外向内）稳定为：

```
sys_nanosleep → timer_sleep → scheduler_schedule
  → Scheduler::schedule                         # 入口 scheduler.rs:1054 即 interrupt_disable()
    → Scheduler::reap_off_cpu                    # scheduler.rs:1450，消费上一轮被切走者
      → ProcessTable::with_process               # scheduler.rs:1457，取进程表锁
        → IrqSpinLock<…>::lock → SpinLock::raw_lock +0x16   # 死自旋
```

**判型结论：CPU#1 卡在获取进程表锁（`PROCESS_TABLE` 的 `IrqSpinLock`）上。** `schedule()` 在 line 1054 进入即 `interrupt_disable()`，`reap_off_cpu`（line 1450）→ `with_process`（line 1457）取该锁取不到，于是在**关中断态死自旋**：自旋核自身 tick 无法进（关中断），对侧核（CPU#0）已 `halt` 等一次永不会来的唤醒 → 全系统定格，`TICK_IRQ_COUNT` delta=0。这与三假说③的定义完全吻合。

**自我更正（务必留档）**：上一轮采集曾报"两核全 halt、排除③"，是用 `info registers`（不带 `-a`）只读到当前核 CPU#0 的假象；改 `info registers -a` 抓到 CPU#1 后 ③ 立刻现形。再往前一版曾用 `kernel.map` 符号化把锁猜成 `IrqSpinLock<HrTimer>`——那是**泛型单体经 LLVM 相同代码折叠（ICF）后的同址假象**（`IrqSpinLock<T>::lock` 各 T 共享一份机器码，map 符号名对这类地址不具区分力）；以 DWARF + 调用者上下文（`with_process` 只取进程表锁这一条即可确定）纠正为 **PROCESS_TABLE 锁**。

**未定项（属 P-29 修复工程输入，非本报告结论）**：快照时刻持锁者不在两核 RIP 上可见（CPU#0 在 idle halt），故"谁持 PROCESS_TABLE 不放"未直接坐实。候选有二，需带帧指针精确 unwind 或运行时锁 owner 记录才能分辨：（甲）AB-BA 锁序倒置——项目 `ref-lock-order.md` 登记正序为 **cfs_rq(层级8) → PROCESS_TABLE(层级7)**，且明令"严禁持进程表锁临界区内取 rq 锁"；本栈帧同时出现 `cfs_deactivate`，若某路径反向持 PROCESS_TABLE 再取 rq 即构成倒置；（乙）同线程重入自死锁——archived 审计（G-11/G-12）记 `Mutex::raw_lock` **无 owner/深度重入检测**，fast path 仅查 `locked != 0`、slow path 死等，重入即无限自旋。

## 五、次要形态 ②（单例，维持观察）

forensic8/run9/fail2：两核均 `halt`、`TICK_IRQ_COUNT` 冻结在 7924。判 ②（时基停摆）。n=1 不足以定性，且不排除是 ③ 现场的另一种收敛（自旋核在采样间隙已解锁、系统落入 idle 但 tick 未重投）。**不据此单独立项**，随 P-29 修复后复采回归观察。

## 六、对 P-27 结论的因果补充

P-27 只证到"Mode X = 全局冻结"这一现象层，未触成因。本报告把成因判到 **③ 关中断自旋死锁**，并把自旋点收敛到 **调度器 `reap_off_cpu → with_process` 取 PROCESS_TABLE 锁** 的具体上下文。至此 Mode X 判型工程（P-28）闭合；锁序根因坐实与修复另立 P-29。

## 七、取证工件处置

本轮全部判型手段为临对时插桩，判型落文档后已一并撤除：`scripts/qemu_boot_test.sh` 的 `E2E_MON` monitor 挂载与超时二次快照钩子 revert 到基线；`other/p28_mon_dump.py` / `other/p28_mon_analyze.py` / `other/modeY_forensic.sh` 均为 `other/`（gitignore）下本地临时工件，不入库。现场样本保留于 `other/log/modeY_forensic{6,7,8}/run*/logs/*.mon`，作为本报告结论的原始证据。
