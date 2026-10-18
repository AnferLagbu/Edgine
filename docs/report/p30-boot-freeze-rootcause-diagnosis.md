# P-30 boot 冻结残留根因定位报告：两条真实的锁误用、一场 lockdep 观察者效应，以及一次 KPTI 读数伪影

> 本报告是一次排查工作区的结果快照。P-29 route C 移除并行调度器后，1000-boot soak 里 `boost_all` livelock 签名零复发，只剩 **1/1000 的 `delta=0` 整机冻结**（boot284 现场指向 freg `RECOVERY_MANAGER` × `cfs_rq` 跨核自旋）。这条残留单立为 **P-30**。经过多轮插桩取证，最终结论是一个**负结果**：该冻结在**生产（非 lockdep）构建**里不复现；此前追到的"四核死自旋 convoy"主要是 **lockdep 观察者效应**叠加 **KPTI tick 读数伪影**。排查途中确实坐实并根治了**两条真实的锁误用**（与冻结判据无关，但都是潜伏缺陷）。报告给出完整证据链、被证伪的假设、根治形状、验收数据，以及若干登记为遗留的独立项。
>
> 报告冻结当时事实，作为后续 plan 与修复工程的输入。工程条目与决策见 [progress-active-tasks.md](../plan/progress-active-tasks.md) 的 P-30 行。所有结论均附源码位置或命令证据。

## 一、判据：怎么才算"冻结"

P-29 定下的唯一硬判据是**全局单调计数器 `TICK_IRQ_COUNT` 在观察窗口内 `delta=0`**（每核 timer IRQ 都累加它，只要有任何一核还在收 tick 它就前进）。这条判据本身对，但**读法**极坑，两条坑都在本排查里咬过人：

- **idle 假冻**：日志增长判据完全无效 —— 用户态 `tcp_echo_server` 空闲时串口本就不打印，"日志不涨"≠"冻结"。必须读 tick 真值。
- **KPTI 读数伪影**（本报告最重要的一条方法学教训）：`TICK_IRQ_COUNT` 是内核 `.bss` 的**虚拟地址**（本 build = `0x340148`）。gdb `x/1xg` 走**被附加 CPU 的当前 CR3** 翻译；当那个 CPU 正处于用户态（KPTI 的 `user_cr3` 已排除内核 `.bss` 高映射）时，读回的是**用户内存里的指针值**（如 `0x401AC9`、`0x1554B8`，都是 ≥1M 的垃圾），而不是真 tick。把它当 `delta=0` 就误判成"冻结"。

最终可靠的读法（`other/p30_soak.sh` 的 `read_tick`）：用 `thread apply all bt` 选出一个 **`#0 rip` 落在内核文本窗口 `[1M,3M)` 的线程**，在该线程上读 tick，且**只有值 `< 1M` 才采信**（真 tick 量级是几万到几十万，KPTI 垃圾指针 ≥1M，天然可分）。冻结判据升级为三条**同时**成立才判 FROZEN：① tick `delta≤0`；② 两次采样值完全相同（排除抖动）；③ `bt` 里**没有任何**核处于 `[halted]`/`idle_entry`（排除"全核在用户态忙"的假冻）。

## 二、坐实并根治的两条真实锁误用

排查在 lockdep 插桩下确实抓到过内核 `#GP`，顺着 guard 生命周期收敛出两条**与插桩无关、在生产里也成立**的真实缺陷。它们不是那条 `delta=0` 残留的因（见 §三），但都是该根治的潜伏问题。

### 2.1 病灶一：中断上下文持 `IrqSpinLock`（`ISR_TABLE`）

`dispatch_irq`（IDT ISR stub 调用，**中断上下文**）对 `ISR_TABLE` 取 `IrqSpinLock::lock()`，违反了本类型 SAFETY 契约（[irq_spinlock.rs:11-14](../../src/kernel/privileged/sync/irq_spinlock.rs)："不可在中断上下文使用"）与本文件 B03-23 的禁锁名单。`ISR_TABLE` 实为**启动期单线程注册、运行期只读**的函数指针表。

根治：改为 `RacyCell` 无锁访问（对齐 `SOFTIRQ_HANDLERS` 与设备 IRQ 表的既有惯例），`register_isr` 用 `get_mut()`（启动期无并发），`dispatch_irq` 无锁读、**先拷出 `fn` 指针再调用**，不在 `handler()` 期间持有任何借用/guard（[irqline.rs:128-170](../../src/kernel/privileged/irqline.rs)）。

### 2.2 病灶二：`IrqSpinLock::Drop` 的跨核竞态（`fetch_sub` 晚于 `raw_unlock`）

旧 `Drop` 先 `raw_unlock()` 再 `depth.fetch_sub(1)`，两者之间有空窗：本核放锁后、递减前，对核可抢先 `lock()` → `fetch_add` 使 `depth=2` → 本核随后读到 `prev≥2` → **跳过 `restore_interrupts`** → 本核 `IF` 永久停在 0（**永久聋**，不再收 tick/IPI）。这条能直接制造"无 `#GP` 的 `delta=0` 冻结"。探针复验坐实：预修复 `CROSSCORE=45–218`、`UNDERFLOW=0`、`delta=0`。

根治：`Drop` 与 `lock()` 严格对称重排 —— 把 `fetch_sub` 移进"仍持底层锁"的窗口内、`raw_unlock` 之后（[irq_spinlock.rs:318-337](../../src/kernel/privileged/sync/irq_spinlock.rs)）。因 `raw_lock` 不可重入 + `lock()` 全程 `cli` ⇒ 同核同锁 `depth` 恒 ≤1 ⇒ 放锁前 `fetch_sub` 返回的 `prev` 恒为 1，`restore_interrupts` 必被执行。

> 排查中还试过 **P2（per-CPU held-guard 登记簿 + `kernel_panic` 前强制释放遗弃 guard）**。它是为"故障核遗弃持锁 → 对核死自旋"这一投机场景加的保险。§三的负结果证明其**前提被证为伪影**，属投机复杂度，依用户裁定**整体移除**（`git checkout HEAD -- sync/irq_spinlock.rs sync/mod.rs` 后仅重施 §2.2 的重排；`idt.rs` 的 `[P30-kprobe]` 打印与 `kernel_panic` 的 `force_release` 调用一并还原）。

## 三、负结果：生产不复现，convoy 主要是观察者效应

把上面两条锁误用根治后，用**生产（非 lockdep）镜像**在持续高压 TCP 负载下重 soak，得到的是干净通过：

| 场景 | 结果 |
|---|---|
| 改后回归 soak（16 boot，`SMP=4`，每轮 12s 高压 TCP） | **16/16 ALIVE，0 FROZEN，0 kprobe**，累计 18301 次 TCP echo 成功，tick 每 12s 稳定前进 ~48000–54000 |
| 早前加压 soak（30 boot，同判据） | **30/30 ALIVE，0 FROZEN，0 kprobe**，累计 35261 次 echo，tick 每 15s 前进 ~59000–65000 |

`bt` ground-truth（符号化后）显示四核健康分布：用户文本 `idle_entry` / 内核文本 / `[halted]`，**没有一核卡在 `SpinLock::raw_lock`**。据此纠正两条既有误判：

- **boot3 "四核死自旋 `SpinLock::raw_lock`（`processes`×2 / `LockDepMap` / `InterruptHistory`）convoy"** 主要是 **lockdep 观察者效应**：`LOCK_DEP_MAP` 元锁把每把 `IrqSpinLock` 的 acquire/release 簿记**串行化**，`LOG_EMIT` 嵌套进一步放大持锁窗口；`LockDepMap` / `InterruptHistory` 两把锁**仅 lockdep/debug 下存在**。被观察的系统因观察手段本身变慢、显病态，在生产里没有对应物。
- **"非 lockdep 构建 2/8 `delta=0`"**（todo `p30-verify-nonlockdep` 的旧结论）是 **KPTI 读数伪影**（§一），不是真冻结。

结论：那条 P-29 遗留的 `delta=0`（boot284，1/1000）**在生产里抓不到复现**。无法证伪也无法证实的 1/1000 稀有事件，不宜继续用投机保险（P2）去堵；转而消除已坐实的真实病灶 + 拆掉热路径上的耦合放大器。

## 四、根治形状（用户裁定的优雅路径）

依用户两次裁定（"加压重 soak 再收" + "摘除耦合(根治)" + "移除 P2"），最终落地：

| 处置 | 内容 | 依据 |
|---|---|---|
| **保留** | 病灶一 `ISR_TABLE` → `RacyCell`、病灶二 `Drop` 对称重排 | §二两条真实缺陷，探针与 soak 实证 |
| **保留** | `bsr.rs` `freeze/unfreeze/rollback_to_init` 改收 `&RecoveryManager`（`execute()` 单次持锁跑完整序列）、`parallel.rs` 随之适配 | 消除 BSR 序列逐步重复加锁的持锁窗口膨胀 |
| **保留** | FREG / UEFI / NET_STATE 各处 `named_lazy` 锁命名 | lockdep 可见性改进：匿名锁是 AB-BA 盲区（正是 P-29 lockdep 从未标记 freg 反向取锁边的原因）；非 lockdep 构建零成本 |
| **根治** | 摘除调度器 tick 里整块 `RECOVERY_MANAGER` 周期耦合（`manager.tick()` + BSR 升级序列）（[scheduler.rs:1814-1823](../../src/kernel/privileged/proc/scheduler.rs)） | 每 tick 在关中断热路径 `lock` 一把全局锁，FREG 域回调 inert 无收益，却在 lockdep 下放大成可见 convoy |
| **移除** | P2 held-guard 登记簿 + `force_release` + `sync/mod.rs` re-export + idt `kernel_panic` 调用；`[P30-kprobe]` 打印 | 前提被 §三证为伪影，属投机复杂度 |

摘除 tick 耦合的直接后果：`RecoveryManager::tick`（[manager.rs:99](../../src/kernel/privileged/freg/manager.rs)）与 `check_and_clear_bsr_escalation`（[freg/mod.rs:116](../../src/kernel/privileged/freg/mod.rs)）不再有调用者；`NEED_BSR_ESCALATION` 的唯一 set 点在 `RecoveryManager::tick` 的健康监控分支（[manager.rs:153](../../src/kernel/privileged/freg/manager.rs)），故 BSR 升级序列整体不可达。`check_and_clear_bsr_escalation` 作为"已知解耦的零引用 pub fn"登记进 `audit_unwired_pub_fn` 的台账已分类清单（`docs/plan/syscall-followup.md`），使 `--strict` 门禁通过、零引用事实保留。

## 五、遗留与独立项（登记，不在本轮激活）

- **FREG `proc_freg` 潜伏 UAF（inert latent）**：`RecoveryDomain::id` 由 `RecoveryDomain::new(id)` 在构造时固定，池里登记的域均为 `id=0`（[domain.rs:54](../../src/kernel/privileged/freg/domain.rs)、[manager.rs:90-96](../../src/kernel/privileged/freg/manager.rs)），于是按非零 `domain_id` 的 `find`（[manager.rs:167](../../src/kernel/privileged/freg/manager.rs)）恒 `None` → proc/pmm 的 capture/rollback 回调从未挂载 → `PROC_SNAPSHOT` 恒 `None` → rollback 落空。整条恢复路径**惰性不可达**，与 §三"生产不复现"一致。**若**日后补上 `domain.id` 赋非零值，会引爆 `proc_freg_capture` 裸拷贝 `NonNull<Process>`（无 pin/refcount）在进程释放后的 rollback use-after-free。依用户"FREG 阶段性版本不再开发"，登记为冻结版本已知潜伏项，**不激活、不修复**。
- **lockdep 观察者效应 / liveness 度量**：`LockDepMap` 元锁 + `LOG_EMIT` 嵌套把持锁窗口放大，使 lockdep build 上的 convoy 判读失真。宜单开工程：给 lockdep 路径设计"低扰动"簿记，并把"整机 liveness"做成可量化指标（而非依赖稀有 boot 的人工 `bt` 判读）。
- **dev-profile clippy lint 债（预存，本 PR 一并清偿）**：本地 `ci/audit.sh` step 2b 在 `kernel_test`/`host-test` 维以 **dev profile**（`debug_assertions` 开）跑 clippy，暴露 16 处 `ref_as_ptr` / `borrow_as_ptr`，全在 lockdep 同步原语簿记里（`self as *const Self as usize` 一类，散落于 `irq_spinlock.rs`/`mutex.rs`/`rwlock.rs`/`spinlock.rs`）。这些是**预存代码**（把本次其余改动 stash 掉、HEAD 干净树上同样报这 16 处）。**权威 CI 门禁**（`.github/workflows/ci-x86.yml` 的 `clippy-pedantic` job）用 `--release`（`debug_assertions` 关），这些 cfg 块不编译，故 CI 本就绿;且没有任何 workflow 调用 `ci/audit.sh`。**依用户裁定本 PR 一并清偿**：16 处改写为 `core::ptr::from_ref(self) as usize` / `&raw const self.inner as usize`（与旧写法取同一地址，语义不变），使 `audit.sh` 2b 转绿。这些改动仅落在 `#[cfg(debug_assertions/lockdep)]` 块内，release 生产镜像行为不变。

## 六、验收数据（§2.3 门槛）

| 门槛 | 结果 |
|---|---|
| 双架构 `cargo check --release` 0 error/0 warning | 通过（`./ci/build.sh all`：`Passed: 6 Failed: 0`，两架构均链接） |
| `cargo clippy --release -D warnings`（kernel x86_64 + aarch64） | 0 warning |
| dev-profile clippy（`audit.sh` step 2b：`kernel_test` + `host-test`） | 0 warning（16 处预存 lint 已清偿） |
| `cargo fmt --check`（kernel / host-tests / src/rust / src/user 四 crate） | 0 漂移 |
| 核心审计（`ci/audit.sh quick` §2.2 全套，含 `unwired_pub_fn --strict`） | AUDIT_RC=0 全绿（`check_and_clear_bsr_escalation` 已登记台账） |
| `make test-kernel-host` | 985 passed / 0 failed |
| `make test-host` | 全绿 |
| `./scripts/qemu_boot_test.sh x86_64` | 1/1（完整 boot / KPTI-09 / 双核 EL0 / TCP echo 3/3 / UDP / P4 里程碑） |
| 改后生产 soak 回归 | 16/16 ALIVE / 0 FROZEN / 0 kprobe / 18301 echo |
