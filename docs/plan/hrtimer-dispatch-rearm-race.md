# hrtimer OneShot 收尾与外部重挂竞态（rb_tree 自环）

> 登记背景: 2026-11-24 验证 U-1（[uefi-thread-exit-hang.md](./uefi-thread-exit-hang.md)）时,
> UEFI 腿偶发内核 panic `privileged/timer/rb_tree.rs:333 rb insert fixup cycle`。
> 用户裁定「趁现场热立即追踪复现」, 不并入已关闭的 U-1。

## 症状

UEFI(OVMF) + `-smp 2`, init 线程组探针阶段 panic:

```text
[PANIC LOC] privileged/timer/rb_tree.rs:333
panicked at privileged/timer/rb_tree.rs:333:13:
rb insert fixup cycle
RBP = 0x00000000003b1890   RSP = 0xffff80001f677810   CR2 = 0x0000000000409040
```

现场次序: `[TLS] PASS` 之后、`[THREAD] PASS` 之前; 线程组 3 个 TCB 已退出 2 个, 第 3 个
退出时撞上 panic。BIOS 腿未复现。

## 复现

原始日志已被后续门禁轮覆盖。按现场落点把 `thread_probe` 的 `thread_leg` 临时重复 20 轮
（每轮 clone 3 个线程 + futex 睡醒 + join/退出, 清空 `THREAD_*` 静态槽）, 不改内核逻辑:

- 第 9 次启动的第 10 轮复现, 签名与 full 轮逐字相同;
- 复现现场: `/tmp/threadstress_serial_9.log`;
- 临时改装只用于复现, 未提交。

## 根因

`dispatch_expired` 的 OneShot 收尾在 **base 锁外**执行:

```rust
if timer.state() == HrTimerState::Running {
    timer.set_state(HrTimerState::Inactive);
    timer.queue_seq.store(0, Ordering::Release);
}
```

回调返回后到这两行写入前, 另一 CPU 可经 `hrtimer_start` 把同一 timer 重挂:
状态置 `Pending`、分配新 `seq`, 且节点**已插入**对应 base 树。随后旧收尾把状态覆盖成
`Inactive`、序号清零, 形成“树里仍有节点、状态说未入队”的分裂态。下一次
`hrtimer_start` 走 `Inactive` 路径, 把**同一节点第二次插入**, `insert_fixup` 遇到
父链自环 —— 即 `rb insert fixup cycle`。

触发面是单实例、可被任意 CPU 重挂的 OneShot 定时器（如网络 poll 截止期）与线程退出/
软中断收尾并发; 线程组压力放大了窗口。

## 修复

新增 `collapse_running_to_inactive(timer, seq)`:

1. 取 timer 当前 `base_id` 对应的 base 锁;
2. 锁内复核 `base_id` 未迁移、`state == Running` 且 `activation_seq == seq`;
3. 只有三条同时成立才置 `Inactive` 并清 `queue_seq`。

外部重挂与收尾因此串行化: 重挂先完成则收尾看到 `Pending` 不动; 收尾先完成则外部 start
看到 `Inactive` 且节点已被收集阶段摘除, 只插一次。

OneShot、Periodic 的失败收敛与 `interval == 0` 收敛都改走该入口; Periodic 正常续期仍走
`hrtimer_start`（其 Pending 路径会先摘旧节点, 不会二次插入）。

回归用例 `test_one_shot_collapse_preserves_external_rearm` 钉住“外部重挂的 Pending/新序号
不得被 OneShot 收尾覆盖”。

## 验证

- 同一 20× 线程压力: 修复后 **20/20 启动、400 轮、1200 个线程 TCB** 无 panic
  （修复前 1/10 启动复现）;
- 标准 init: UEFI `-smp 2` 3/3 PASS, 含 `[THREAD] PASS` 与 `[UACCESS] PASS`;
- `make test-kernel-host`: 12 + 811 + 637 passed / 0 failed（新增 1 例）;
- `make test-host`: 1653 passed / 0 failed;
- `ci/audit.sh full`: rc=0, 未执行腿数 0（含 UEFI `[THREAD] PASS` / `[UACCESS] PASS`）。

## 边界

- 本缺陷与 U-1 的 uaccess 写前预检无因果关系; U-1 修复只是让线程组腿稳定走到退出阶段,
  从而把既有定时器竞态暴露得更频繁。
- 不改 5 s 预算、不放宽任何门禁判据。
