# 定时器两档取消协议 + timerfd 回调叶子化

> 来源：`docs/plan/safety-gap-remediation.md` 批 R5 的 ㊹。
>
> R5 已按「A+」收口：队列锁内真出队、回调指针原子化、取消后状态收敛，并把
> 「`Pending` / `Running` 期间调用方须保活」写成硬契约。本册承接该契约的长期解 ——
> 让 `hrtimer_cancel` 具备「返回即可释放」的完整语义，同时不引入 AB-BA 死锁。
>
> 本册是独立工程，不并入 `safety-gap-remediation.md` 的 27 项清单；项号沿用该册的
> ㊹ 便于交叉对照。

## 一、问题与根因

R5 施工时确认：把 `hrtimer_cancel` 改成「等待在途回调退出」会与 timerfd 的死锁：

- `timerfd::set_time` 持 `TFD_TABLE` 调 `hrtimer_cancel`（`privileged/syscall/timerfd.rs`）；
- `timerfd_callback` 第一行就取 `TFD_TABLE`；
- 取消方持表锁等在途回调、回调持表锁路径等表锁 ⇒ AB-BA。

根因不是单个函数的实现错误，而是四条设计决策叠加：

1. `HrTimer` 是嵌入式对象，API 不拥有内存，生命周期外包给调用方；
2. 取消是异步的，调用方无法从 API 得知「现在能不能释放」；
3. 回调在队列锁释放后执行（允许回调内重挂），队列锁不能兼任生命周期锁；
4. 回调体可以取任意锁，框架无法在不了解调用方锁域的前提下安全等待。

因此需要一套**协议**，而不是把某个 cancel 改成阻塞。

## 二、业界形态

| 内核 | 取消 API | 等待语义 | 回调持锁约束 |
|---|---|---|---|
| Linux | `hrtimer_try_to_cancel`（非阻塞，返回 -1 = 回调正在跑） | `hrtimer_cancel` 阻塞；`hrtimer_cancel_wait_running` 单独等待 | 回调不得取取消方在等待期间持有的锁 |
| FreeBSD | `callout_stop`（非阻塞，0 = 正在服务不能停） | `callout_drain` 阻塞到回调返回且无引用，不能从中断上下文调用 | 回调在 `callout_lock` 之外执行 |
| NetBSD | `callout_stop`（非阻塞、任意上下文） | `callout_halt` 阻塞 | 回调不持 callout 锁 |
| Windows | `KeCancelTimer`（非阻塞） | `KeFlushQueuedDpcs` 等待 | DPC 对象延迟删除 |
| Zircon | `timer_cancel` 只取消排队 | 等待经 port 完成，生命周期由 handle 引用计数承担 | 无内核回调 |

Linux `do_timerfd_settime` 的形态是关键参考：

```c
for (;;) {
    spin_lock_irq(&ctx->wqh.lock);
    if (hrtimer_try_to_cancel(&ctx->t.tmr) >= 0)
        break;
    spin_unlock_irq(&ctx->wqh.lock);      // 失败先放锁
    hrtimer_cancel_wait_running(&ctx->t.tmr); // 锁外等待
}
```

它也**不在回调内重挂周期**：回调只置 tick 并唤醒，周期重挂放在 read 路径
（注释明确说明这是为避免短周期 DoS）。

## 三、目标设计

- 新增 `hrtimer_try_cancel(timer) -> CancelOutcome`：队列锁内非阻塞出队，返回
  `NotPending` / `Cancelled` / `Busy`（`Busy` = 回调已被收集或正在执行）。
- 新增 `hrtimer_cancel(timer)`：`try` 成功即返回；`Busy` 时**先释放队列锁**，等待
  在途回调退出，再回环重试。等待期间不得持有回调所需的任何锁。
- 新增 `hrtimer_shutdown(timer)`：取消 + 置关闭位，使回调的自重挂与框架的 Periodic
  重挂都不再生效（代际校验的强化形态）。
- 硬约束写入模块文档：回调不得取取消方在等待期间持有的锁；不得在自身回调内调用
  阻塞取消。
- host 侧回归：`try` 三态判定、`Busy` 回环、shutdown 后不复活；并发面用
  `std::thread` + 原子时序控制，避免依赖调度抖动。

## 四、条目

### 条目 1・两档取消 API 与 shutdown

描述：在 `privileged/timer/hrtimer.rs` 落地 `try_cancel` / 阻塞 `cancel` /
`shutdown` 三档语义，并把「回调锁域」约束写进模块 `# Safety`。

方案：`CancelOutcome` 枚举 + 队列锁内 `retain` 出队；`Busy` 由收集阶段的在途标记
（`in_flight`）判定；阻塞 `cancel` 只做「try → 锁外等待 → 重试」；`shutdown` 用
关闭位让 `dispatch_expired` 的 Periodic 分支与回调自重挂都失效。测试覆盖三态与
「取消返回后立即释放 timer」的 UAF 面。

状态：[]

### 条目 2・timerfd 回调叶子化

描述：把 `timerfd_callback` 对 `TFD_TABLE` 的依赖去掉，或把周期重挂移出回调，
使回调满足「不取取消方锁」的叶子约束。

方案：优先按 Linux 形态改 —— 回调只置原子 `ticks` / `expired` 并唤醒等待者，
周期重挂移到 `set_time` / `read` 路径；`set_time` 的取消改为
「持表锁 try → 失败放锁等待 → 回环」。需要同步收紧槽位代际校验与 `arm_seq` 语义，
并补「取消与回调交错」「槽位复用」两条并发回归。

状态：[]

### 条目 3・锁序登记与审计腿

描述：两档协议引入「等回调时不持调用方锁」的新约束，需要静态可查。

方案：在 `scripts/lock_class_order.json` 登记定时器队列锁类；为「阻塞取消调用点
不得位于持锁临界区」补静态判据（或 host 形状锁），并跑
`scripts/audit_deadlock_matrix.py` 与 full 档审计。

状态：[]
