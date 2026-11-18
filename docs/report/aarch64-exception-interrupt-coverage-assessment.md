# aarch64 异常与中断面覆盖评估

> 结论：aarch64 的**缺页恢复面已经实装**，旧结论「aarch64 没有缺页处理器」在当前树失效。真正剩下的缺口不再是缺页主路径，而是**向量表的十路未处理入口**（其中 SError / FIQ 进入一个无日志的 `wfi` 死循环）、**uffd 未接线**、**缺页风暴无限界**，以及中断路径上几处结构性耦合。现有 `gicv3-rt002-rootcause-and-index-model.md` 只回答了一个具体 root cause，不构成覆盖面评估。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读，未构建、未上机。

## 一、先更正一条旧结论

`privileged/arch/aarch64/exception.rs:827-880` 已是完整的 EL0 缺页恢复路径：

- `A64FaultClass::from_esr(esr).is_demand_pageable()` 判定「翻译故障（未映射 ⇒ 按需分页/栈增长）」与「权限故障（写保护 ⇒ COW）」；
- 故障进 `mm::page_fault::handle_user_page_fault`，与 x86_64 `PageFaultHandler` **共用同一实现**；
- 返回 `Fixed` 时直接 `return`，由 `handle_el0_sync` 以未改动的 `ELR_EL1` `eret` 重试；
- 内核态 uaccess 写 COW 页走 `break_cow_for_kernel_uaccess`，判据不含猜测分支，不满足即停机；
- 注释自述「本架构此前不存在缺页恢复路径」，即该能力是后补的。

因此「aarch64 与 x86_64 在缺页上不对等」这一条应停止引用。§七给出逐项对等现状。

## 二、向量表 16 路逐路

`exception.rs:40-93` 的 `exception_vector_table`，16 个 128-byte 槽位：

| # | 入口 | 指向 | 状态 |
|---|---|---|---|
| 0-3 | EL1t（SP_EL0）sync / irq / fiq / serror | `unexpected_exception` | **4 路未处理** |
| 4 | EL1h sync | `handle_el1h_sync` | 已处理（内核缺陷 ⇒ 打印 + 停机；uaccess COW 例外） |
| 5 | EL1h irq | `handle_el1h_irq` | 已处理 |
| 6-7 | EL1h fiq / serror | `unexpected_exception` | **2 路未处理** |
| 8 | EL0 aarch64 sync | `handle_el0_sync` | 已处理（含缺页/COW/栈增长） |
| 9 | EL0 aarch64 irq | `handle_el0_irq` | 已处理 |
| 10-11 | EL0 aarch64 fiq / serror | `unexpected_exception` | **2 路未处理** |
| 12-15 | EL0 aarch32 sync / irq / fiq / serror | `unexpected_exception` | **4 路未处理**（源码注释标「未使用」） |

合计 **10/16 路落入 `unexpected_exception`**。其中 4 路（EL1t）在当前设计下不可达（内核从不用 SP_EL0 作为当前 EL 栈），4 路（aarch32）在「不支持 32 位用户态」的前提下属声明空白。**真正有风险的是 2 路 FIQ 与 2 路 SError。**

## 三、`unexpected_exception` 的实际行为

`exception.rs:506-511`：

```asm
unexpected_exception:
    mrs  x0, esr_el1
    mrs  x1, elr_el1
    mrs  x2, far_el1
    wfi
    b    unexpected_exception
```

它把三个诊断寄存器读进 `x0-x2` 后**没有任何输出**，直接进入 `wfi` + 自循环。

后果：SError（总线错误、外部中止、内存错误）到达时，机器**静默挂死**——没有 `SYNC!` 行、没有 ESR/FAR 打印、没有可归因的现场。这比 panic 更难诊断：panic 至少留下栈与位置，`wfi` 死循环只留下「机器不动了」。相比之下，EL0 未恢复的同步异常至少会打 `SYNC!` 现场（`:867-870` 的注释明确该行是「未被恢复的同步异常专属签名」）。

这是本面**优先级最高的单点**：它不降低正常路径能力，但会吞掉所有 aarch64 硬件故障的可观测性。

## 四、FIQ 未处理

FIQ 在 ARM 上常被用作安全/调试/高性能定时源。当前 4 路 FIQ 全指向 `unexpected_exception`，即：一旦平台把任一路中断配成 FIQ（GIC 的 Group 0 在某些配置下走 FIQ），系统就会进入静默挂死，而不是「不支持」的可判别错误。

与 x86_64 对比：x86_64 有独立 NMI 处理面（`privileged/arch/x86_64/nmi.rs` 一族），aarch64 没有 SError/FIQ 的对应物。这是两架构**语义对等的真实缺口**，不是实现风格差异。

## 五、IRQ 路径与 EOI 次序

`handle_irq_inner`（`exception.rs:673-798`）的顺序：

1. `gic::acknowledge()` 读 `ICC_IAR1_EL1`；
2. `intid >= 1020` 判伪中断，直接返回（不 EOI，正确）；
3. SGI 7（FREG 恢复）→ 处理 → EOI → 返回；
4. SGI 13（TLB shootdown）→ `tlb_catch_up_local()` → 探针上报 → EOI → 返回；
5. SGI 14（重调度）→ `resched_ipi_handler()` → EOI → 返回 `true`；
6. PPI 30（timer）→ `timer::reload()` → `on_timer_interrupt()` → `poll_network()` → `scheduler_tick()` → **落到统一尾部**；
7. `dispatch_device_spi(intid)`（`:793`，**先于 EOI**，对电平触发中断是正确次序）；
8. `gic::end_of_interrupt(intid)`；
9. `do_softirq()`；
10. 出口：EL0 来源先 `do_signal_deliver`，再按需 `run_pending_resched`。

三条 SGI 路径都在返回前自行 EOI，timer/SPI 走统一尾部 EOI，伪中断不 EOI——EOI 覆盖完整，未发现「某路径漏 EOI 导致中断风暴」的结构缺陷。

`dispatch_device_spi`（`gic.rs:771-786`）按 `intid - SPI_BASE` 索引静态分发表，注册期后只读；越界与未注册均返回 `false`，不 panic。

## 六、真实缺口清单

| # | 缺口 | 承诺/证据位置 | 影响 |
|---|---|---|---|
| A1 | SError 静默挂死 | `exception.rs:506-511`、`:70-71` | 硬件错误不可观测、不可归因，整机无输出 |
| A2 | FIQ 未处理 | `exception.rs:62-63`、`:76-77`、`:84-85`（4 路） | 平台若用 FIQ 即挂死 |
| A3 | aarch32 EL0 未处理 | `exception.rs:82-93` | 当前不支持 32 位用户态，属声明空白；接 32 位程序前必须补 |
| A4 | uffd 出口未接线 | `exception.rs:863-866` | `UffdWait` 落到终止语义：用户态 uffd 程序在 aarch64 被误杀 |
| A5 | 缺页风暴无重试限界 | `exception.rs:871-873`（自述「单独立项」「本批不引入」） | 判「已修复」但重试仍触发时无限循环；x86_64 同形 |
| A6 | 内核态同步异常 = 停机 | `exception.rs:838-841` | 设计选择（内核缺陷必须暴露），非缺陷；但与 `panic=abort` 叠加后无 dump |
| A7 | 无中断嵌套 / 无线程化 IRQ | `handle_irq` 全程不重开 IRQ | 长 handler 阻塞同级与更低优先级中断 |
| A8 | timer IRQ 内 `poll_network()` | `exception.rs:776-781` | 网络轮询与 tick 强耦合：定时精度与网络负载互相拖累 |
| A9 | tickless 未接线 | `functions/timer/tickless.rs:504-517` 仅测试调用 | aarch64 与 x86_64 同样走周期 tick，空闲不省电 |

## 七、与 x86_64 的逐项对等

| 能力 | x86_64 | aarch64 | 对等 |
|---|---|---|---|
| EL0 缺页 / COW / 栈增长 | `idt::handlers::PageFaultHandler` | `handle_el0_sync` → 共用 `handle_user_page_fault` | ✅ |
| 内核 uaccess COW | 有 | `break_cow_for_kernel_uaccess` | ✅ |
| uffd 出口 | 已接线 | `UffdWait` 不恢复 | ❌ |
| 缺页风暴限界 | 无 | 无 | 同缺 |
| 非屏蔽中断类 | NMI 处理面 | SError/FIQ 未处理 | ❌ |
| 跨核 IPI | 向量 0xFD/0xFE/0x82 | SGI 13/14/7 | ✅ |
| 定时器 | LAPIC timer | PPI 30 + Generic Timer `reload` | ✅（同为周期） |

## 八、与既有报告的关系

- `docs/report/gicv3-rt002-rootcause-and-index-model.md`：解决 `GICD_CTLR.ARE` 未置位导致 SGI/SPI 不分发；是**单缺陷根因**，不覆盖向量表完整性、SError、uffd、风暴。
- `docs/report/smp-implementation-assessment.md`：覆盖 AP 启动与 IPI，不判异常向量。
- `docs/report/subsystem-gap-deep-audit.md`：aarch64 小节基于旧基线，其中「缺页处理器缺失」已被本文 §一 推翻；引用前须核对。

## 九、对计划的意义

这一面的缺口可按「是否吞掉可观测性」排序，而不是按实现工作量排序：

1. **A1/A2 先做**：让未处理异常至少留下 ESR/FAR/ELR 现场并进入可归因的停机路径。它们不增加功能，但决定后续所有 aarch64 问题是否能被诊断。
2. **A4/A5 再做**：uffd 出口与风暴限界都是「已有语义缺一块」，且 A5 与 x86_64 同形，适合两架构一起收。
3. **A3 属声明面**：在支持 32 位用户态之前，应把「不支持」写成显式检测（进入 aarch32 入口即打印并终止该进程），而不是静默 `wfi`。
4. **A7/A8 属架构选择**：不是缺陷，但需要在设计文档中明确「不嵌套、不线程化、网络轮询挂 tick」是有意取舍，否则会被后续维护者当成遗漏。
