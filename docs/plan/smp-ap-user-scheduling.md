# AP 参与用户态调度工程（次核调度闭环 + KPTI per-CPU 化）

> 本文件是 [aarch64-smp-bringup.md](./archive/aarch64-smp-bringup.md)（DECISION-082）与 [aarch64-tlb-shootdown-send.md](./archive/aarch64-tlb-shootdown-send.md)（DECISION-083）之后的后续独立工程。
>
> 目标：让**次核（AP）参与用户态调度** —— 使 ≥2 个可运行用户任务能在多核上并发处于 EL0；并把 aarch64 KPTI 入口/出口的每核活跃状态 per-CPU 化（KPTI-PCPU-01），使双核并发 EL0 不产生跨核污染。以 QEMU `-smp 2` 运行验证。
>
> 实现路径：**方案 C（相对完整）** —— 用户裁定。
>
> 关联裁定：DECISION-084（本文件）。关联登记：[unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md)。

## 1. 根因与性质

**前提订正（§10 源码复核结论）**：原登记项「AP 上线后停在 idle、不调度用户任务」被表述为 **aarch64 缺口**。复核取证后订正为：

> **该缺口不是 aarch64 独有，而是双架构共有的「缺少 AP 调度闭环」** —— x86_64 与 aarch64 的 AP 入口收尾完全同构，均不参与用户态调度。二分依据见 E1/E2。

**三处断链**（任一未通，任务必滞留 BSP）：

1. **无调度身份**：`ap_main`/`ap_entry` 只调 `SCHEDULER.init_per_cpu_idle(idx)`（创建 idle 进程），但**不置** `PerCpuSched.current`。`Scheduler::schedule()`（[scheduler.rs:933](../../src/kernel/privileged/proc/scheduler.rs#L933)）仅在 `!prev_ctx_ptr.is_null()` 时 `context_switch`，而 `current == 0` ⇒ `prev_ptr` 为 `None` ⇒ **永不切换**（只更新 `current` 字段，仍在本核 boot 栈上继续跑）。
2. **无投送路径（push）**：`cfs_enqueue`（[scheduler.rs:489](../../src/kernel/privileged/proc/scheduler.rs#L489)）只把任务入**当前核**队列；fork 后 `add_to_run_queue`（[scheduler.rs:1159](../../src/kernel/privileged/proc/scheduler.rs#L1159)，调用点 [proc_ops.rs:978](../../src/kernel/privileged/proc/proc_ops.rs#L978)、[clone.rs:316](../../src/kernel/functions/proc/clone.rs#L316)）亦只入本核。无任何代码把任务投送到空闲核。
3. **无唤醒源**：AP 停在裸 `loop { halt() }`；`resched_cpu()`（[cpu_queue.rs:150](../../src/kernel/privileged/proc/cpu_queue.rs#L150)）**无生产调用者**；且 AP 的 per-CPU 定时器**从未装载**（aarch64 `timer::start_interval` 仅在 BSP 调用，[lib.rs:887](../../src/kernel/lib.rs#L887)）⇒ 空闲 AP 无周期性唤醒去调 `schedule()` 做 pull 平衡。

**性质判定**：**能力级缺口（跨架构）**。单核语义自洽；多核下只有 BSP 承载全部用户任务，AP 恒 idle。`load_balance()`（[scheduler.rs:1545](../../src/kernel/privileged/proc/scheduler.rs#L1545)）已存在，但其为**拉**（只从别的核偷到本核），且必须先有人在本核调 `schedule()` 触发。

## 2. 实证结论（源码复核）

| # | 命题 | 复核结论 |
|---|---|---|
| E1 | AP 入口双架构同构、均不调度用户任务 | **成立**。aarch64 [`ap_main`](../../src/kernel/privileged/arch/aarch64/smp_init.rs#L236-L301) 与 x86_64 [`ap_entry`](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L297-L366) 均以 `interrupt_enable(); loop { arch!(halt()); }` 收尾 |
| E2 | 「AP 不调度」非 aarch64 独有 | **成立**。断链 1/2/3 在双架构一致；`make test-smp` 的 `first user syscall` 断言由 **BSP** 满足，未证明 AP 参与调度 |
| E3 | AP 无调度身份（断链 1 的直接证据） | **成立**。`schedule()` 的 `context_switch` 由 `!prev_ctx_ptr.is_null()` 门控（[scheduler.rs:933](../../src/kernel/privileged/proc/scheduler.rs#L933)）；`init_per_cpu_idle` 只创建 idle、不置 `current` ⇒ AP `current == 0` |
| E4 | 任务入队只入当前核 | **成立**。`cfs_enqueue` 用 `per_cpu()`（[scheduler.rs:490](../../src/kernel/privileged/proc/scheduler.rs#L490)）；`add_to_run_queue` → `cfs_enqueue`（[scheduler.rs:1159-1163](../../src/kernel/privileged/proc/scheduler.rs#L1159-L1163)） |
| E5 | resched IPI 接收侧双架构已就绪、发送侧无调用者 | **成立**。x86_64 [idt.rs:804-808](../../src/kernel/privileged/idt/idt.rs#L804-L808) / aarch64 [exception.rs:842-846](../../src/kernel/privileged/arch/aarch64/exception.rs#L842-L846) → `resched_ipi_handler` → `raise_softirq(Sched)` → `SCHEDULER.schedule()`；`resched_cpu` 仅其自身 FFI `cpq_resched_cpu` 为调用者（无生产调用） |
| E6 | `load_balance` 是「拉」而非「推」 | **成立**。`load_balance` 从最忙核偷到 `this_cpu`（[scheduler.rs:1545-1597](../../src/kernel/privileged/proc/scheduler.rs#L1545-L1597)）；`select_cpu_for`（[scheduler.rs:1501](../../src/kernel/privileged/proc/scheduler.rs#L1501)）**优先返回 hint（当前核）**，不能用于「投送到空闲核」 |
| E7 | AP 的 per-CPU 定时器未装载 | **成立**。aarch64 `timer::start_interval` 在 [lib.rs:887](../../src/kernel/lib.rs#L887) 仅对 BSP 调用；AP 的 `init_per_cpu` 只 `enable_timer_ppi`（使能 GIC PPI，不装载 CNTP） |
| E8 | aarch64 上下文切换不依赖 `set_kernel_stack` | **成立**。aarch64 [`set_kernel_stack`](../../src/kernel/privileged/cpu/arch.rs#L92) 为空实现；`SP_EL1` 由 `context_switch` 从 ctx@96 `mov sp, x2` 直接装载（[context.rs:142-143](../../src/kernel/privileged/arch/aarch64/context.rs#L142-L143)）⇒ AP 从 boot 栈切进用户任务无需额外寄存器设置 |
| E9 | aarch64 首次进 EL0 与内核续跑由 SPSR.M 分派 | **成立**。fork 子进程 ctx 的 SPSR.M=0 → `.Lctx_enter_el0`（[context.rs:186-188](../../src/kernel/privileged/arch/aarch64/context.rs#L186-L188)）经 trampoline 切表后 eret |
| E10 | KPTI-PCPU-01 缺陷（每核活跃值置单实例全局量） | **成立（P3 已修）**。原状：`KPTI_GLOBALS` 为单实例，`user_ttbr0`(@24)/`tramp_save0`(@40)/`tramp_save1`(@48) 属每核活跃值，被入口/出口汇编按固定偏移访问（`exception.rs`、`context.rs`）；双核并发 EL0 将跨核覆盖。修后：三者迁入按核数组 `KPTI_CPU_GLOBALS`（槽内偏移 0/8/16），槽基址经 `TPIDR_EL1` 按核绑定（见 APS-04 详情） |
| E11 | 任务投送到 CPU1 的充分条件 | **成立**。需四者同时满足：目标核 CFS 成为入队对象 + `resched_cpu` 送 IPI + AP `current != 0` + AP 被唤醒；缺任一任务滞留 BSP |
| E12 | 编号可用 | **成立**。现存最新裁定为 DECISION-083，本工程取 **DECISION-084** |

## DECISION-084（AP 参与用户态调度实施裁定）

- **描述**：让 AP 参与用户态调度涉及调度器核心路径；施工前需裁定实现路径、与既有登记工程的关系、以及 aarch64 KPTI 的处置。
- **方案（用户裁定 + 本文件落实）**：
  1. **实现路径 = 方案 C（相对完整）**（用户裁定）：`AP 调度身份 + idle 调度循环 + push 投送 + 空闲核唤醒（IPI）+ per-CPU 定时器 pull 平衡 + KPTI-PCPU-01 + 双核并发 EL0 运行验证`。
  2. **范围排除 = 不动 tick 链路统一（D6）与线程级调度统一（D1）**：二者已由 [multithreading-project.md](./multithreading-project.md)（K-04，D1=A / D6）登记为**待设计**的独立工程。本工程**不**改 `scheduler_tick()` FFI、**不**改 `SCHEDULER_EX`、**不**做线程维度统一 —— 以 fork 时 push + AP idle 循环 pull 达成 AP 参与调度，避免抢占该登记工程的设计空间（AGENTS §12.2）。（**后续更新**：D6 的 tick 部分已由独立修复工程 [ISSUE-RT-004](./unresolved-issues-2026-08-09.md) 接管并落地——`scheduler_tick()` 改为驱进程级 `SCHEDULER.tick()`；该改动**不属本工程范围**，亦不改变 D1 的线程维度统一待办。）
  3. **KPTI-PCPU-01 并入本工程**（aarch64）：把 `user_ttbr0`/`tramp_save0`/`tramp_save1` 按核隔离（首选按核数组，对齐 `mm/copy_user.rs` 的 `PER_CPU_EXCEPTION_CTX[cpu]` 范式），因本工程使双核 EL0 成为现实，该缺陷必须先修。
  4. **验证强度**：满足 §2.3 六门槛；专项判据 = QEMU `-smp 2` 下可观测**两个用户任务分处 CPU 0 与 CPU 1 且均处于 EL0**（日志成对出现），且启动无回归。
- **状态**：[X]
- **详情（施工结论）**：APS-01..APS-06 全部落地（P1→{P2,P3}→P4 收口），六门槛全过，专项判据双架构命中；收口期发现并修复的次核 CPU 状态初始化缺陷见批次 P5。

## 任务清单

- **APS-01. AP 调度身份（`adopt_cpu_idle`）**
  - 描述：消除断链 1 —— AP 必须先拥有本核 idle 身份，`schedule()` 才会真正 `context_switch`（E3）。
  - 方案：`Scheduler` 新增 `pub fn adopt_cpu_idle(&self, cpu_id: u32) -> Option<Pid>`：调 `init_per_cpu_idle(cpu_id)` 取 idle pid，置 `per_cpu_for(cpu_id).current = idle_pid`，返回该 pid；失败返回 `None`。双架构 AP 入口改用它（替换裸 `init_per_cpu_idle`），失败则放弃上线（fail-fast，与既有语义一致）。
  - 状态：[X]
  - 详情（施工结论）：`scheduler.rs` 新增 `adopt_cpu_idle`（真实调用点 = aarch64 `ap_main` 步骤 5、x86_64 `ap_entry` 的 idle 建立处，两处均改为失败即放弃上线）。
- **APS-02. AP idle 调度循环 + 空闲核 per-CPU 定时器（双架构）**
  - 描述：消除断链 3 的唤醒面 —— 让空闲 AP 周期性调 `schedule()`（pull 平衡）并在 push 时被 IPI 唤醒。
  - 方案：
    - aarch64 `ap_main` / x86_64 `ap_entry`：开中断后进入 `loop { SCHEDULER.schedule(); arch!(halt()); }`（`halt` 为 wfi/hlt，timer PPI / resched IPI 均可唤醒）；被切走的任务阻塞后经 per-CPU idle 兜底切回本循环。
    - aarch64：AP 在 `ap_main` 内调 `timer::start_interval(TIMER_INTERVAL_TICKS)` 装载**本核** CNTP（per-PE 系统寄存器，E7 前提：BSP 的装载不影响 AP）。
    - x86_64：确认/补齐 AP 本核 LAPIC 周期定时器（`ap_entry` 的 `apic::init()` 是否含周期模式，施工期实测补）。
  - 状态：[X]
  - 详情（施工结论）：
    - aarch64：`ap_main` 末尾装载本核 `timer::start_interval(TIMER_INTERVAL_TICKS)`（CNTP 为 per-PE，BSP 的装载不影响 AP），随后进入 idle 调度循环。
    - 唤醒窗口处理：两架构均改为「关中断 → `schedule()` → **原子地开中断并停机**」——aarch64 用 `msr daifclr, #2; wfi`、x86_64 用 `sti; hlt`（对齐既有 `idle_entry` 融合手法），消除「`schedule()` 返回后、停机前」到达的中断被错过唤醒。**注意**：此处必须显式开中断，因 `schedule()` 的中断恢复判据 `saved_flags & 0x200` 取的是 x86_64 IF 位语义，对 aarch64 DAIF（bit9=D）恒为假 ⇒ 在 aarch64 上 `schedule()` 不会自行恢复中断（**预存问题②**，本轮不扩大、不改共享调度器代码，仅在 AP 循环内显式开中断规避）。
    - x86_64：实测确认 **AP 本核无周期定时器** —— x86_64 的 tick 源为 PIT（8254，仅 BSP 可用），LAPIC 周期定时器未标定（`APIC_TIMER_HZ` 仅在 `apic::calibrate_timer` 内赋值，生产路径无调用，`get_timer_hz()` 恒为 0 ⇒ tickless 的 `program_periodic` 直接返回）。故 x86_64 AP 的唤醒源取 **resched IPI（P2 push）+ 其它核负载均衡**，本轮**不补** LAPIC 定时器（补装需引入 AP 侧 LAPIC 频率标定，超本工程范围）。
- **APS-03. push 投送（fork → 空闲核）**
  - 描述：消除断链 2 —— 让任务能落到空闲 AP 而非恒留 BSP。
  - 方案：
    - `Scheduler` 新增 `fn cfs_enqueue_to(&self, pid: Pid, cpu_id: u32)`（现有 `cfs_enqueue` 仅入 `per_cpu()`）。
    - 新增 `fn find_idle_cpu(&self, pid: Pid, hint: u32) -> u32`：在 allowed cpuset 内优先选「`current == idle` 且 `cfs_rq` 空」的核；无则返回 `hint`。**不复用 `select_cpu_for`**（其优先 hint，语义不符，E6）。
    - `add_to_run_queue` 改为：选核 → 入目标核队列 → 若目标 ≠ 当前核则 `cpu_queue::resched_cpu(target)`（启用既有 E5 接收路径，消除 F9 隐患）。
  - 状态：[X]
  - 详情（施工结论）：`cfs_enqueue` 拆为 `cfs_enqueue`（当前核）+ `cfs_enqueue_to(pid, cpu_id)`（任意核，只持目标核队列）；新增 `find_idle_cpu(pid, hint)`（判据 = `current == idle != 0` 且目标核 `cfs_rq` 空）；`add_to_run_queue` 选核入队 + 跨核时 `resched_cpu(target)`，并加有界诊断 `[SMP] migrate pid=K -> cpu=M`（前 8 次）。QEMU `-smp 2` 实测命中 `migrate pid=6 -> cpu=1`。
- **APS-04. KPTI-PCPU-01（aarch64 入口/出口每核活跃值 per-CPU 化）**
  - 描述：E10 的结构性缺陷；本工程使双核 EL0 现实化，须先修。
  - 方案：保留 `KPTI_GLOBALS` 的 boot 期只读字段（`ready`/`kernel_ttbr0`/`kernel_ttbr1`/`tramp_ttbr1`）；把 `user_ttbr0`/`tramp_save0`/`tramp_save1` 迁入按核数组（`MAX_CPUS`），索引与 `register_cpu` 一致；`kpti_set_user_ttbr0` 改为按核；布局用 `const _: () = assert!` 锁定。
  - 状态：[X]
  - 详情（施工结论）：
    - **状态二分**：boot 期一次性发布、此后只读的 4 个字段留在单实例 `KPTI_GLOBALS`（`tramp_ttbr1`@0 / `kernel_ttbr1`@8 / `kernel_ttbr0`@16 / `ready`@24）；每核活跃值（`user_ttbr0`/`tramp_save0`/`tramp_save1`）迁入 `KPTI_CPU_GLOBALS: KptiCpuStateArray`（`[KptiCpuState; MAX_CPUS]`，槽内偏移 0/8/16，元素 size=24）。
    - **索引方式订正（实施决策，替代方案原文的「汇编现算 `MPIDR_EL1 & 0xFF`」）**：入口时刻 x0-x30 全是用户态活跃值、无空闲 GPR，且 `adrp/add` 求数组基址需第 2 个 scratch 寄存器（会 clobber 用户 x5），故**索引提前固化到上电路径** —— 新增 `kpti_bind_cpu(cpu_index)` 把「本核槽的高半区别名地址」写入 `TPIDR_EL1`（每 PE 私有、EL0 不可见），BSP 由 `kpti_init` 步骤 5 调用、AP 由 `ap_main` 步骤 4.1 调用；汇编一律 `mrs x, tpidr_el1` 取基址。同一基址单一来源，Rust 写侧（`kpti_cpu_state()` 按 `arch::cpu_id()` 索引）与汇编读侧必然落在同一槽。
    - **tramp 表映射精确化**：`KptiCpuStateArray` 用 `#[repr(C, align(4096))]` 使数组页对齐且大小为页整数倍（24×1024=24576=6×4096），`kpti_init` 步骤 3.1 按整页精确映射，不多映射相邻 `.bss` 页、不扩大 Meltdown 可见面。
    - **改动面**：`mm/kpti_aarch64.rs`（新增按核数组/绑定/按核 setter + 全部 `offset_of!`/`size_of!`/`align_of!` 静态断言）；`arch/aarch64/exception.rs`（`handle_el0_sync` 入口与精化块、`el0_return`、`handle_el0_irq` 入口与精化块、`kpti_enter_user_trampoline` 共 6 处 asm）；`arch/aarch64/context.rs`（保存侧 @136、内核续跑刷新、`.Lctx_enter_el0` 共 3 处 asm；`kpti_globals` 命名操作数随之删除）；`arch/aarch64/smp_init.rs`（AP 步骤 4.1）。
    - **契约测试**：`host-tests/tests/aarch64_el0_frame_symmetry_test.rs` 偏移 `#40`/`#48` → `#8`/`#16`，并新增 `mrs x3, tpidr_el1` 及「不得再用单实例偏移 40/48」的 fail-closed 断言（契约 3）。
- **APS-05. 双核并发 EL0 可观测点 + QEMU `-smp 2` 断言**
  - 描述：专项判据的载体。
  - 方案：内核侧在任务首次进入 EL0 时打印有界诊断（如每核前若干次）`[SMP] EL0 pid=N cpu=M`；`src/user/init` 增加一个**不 yield 的忙等子进程**（fork 后各自自增计数）以强制并发；`scripts/qemu_boot_test.sh` 两分支补 `-smp 2`（x86_64 现无）并新增成对判据（见到 `cpu=0` 与 `cpu=1` 的 EL0 记录），fail-closed。
  - 状态：[X]
  - 详情（施工结论）：
    - **内核侧有界诊断**：`src/kernel/privileged/syscall/dispatch.rs` 新增 `observe_el0_syscall()`，打印 `[SMP] EL0 pid=N cpu=M`；每核上限 `EL0_OBSERVE_LIMIT = 4` 行（`EL0_OBSERVED_PER_CPU` 按核计数，索引 = `arch::cpu_id() % MAX_CPUS`），避免忙等任务刷屏。由两架构的 EL0 syscall 入口各调用一次：x86_64 `syscall_dispatch_from_frame`、aarch64 `arch/aarch64/exception.rs::svc_handler`（刻意不置于架构中立的 `syscall_dispatch`，避免内核侧 `usermode::dispatch_syscall` 混入）。
    - **用户态载体**：`src/user/init/src/main.rs` 新增不 yield 的忙等路径 `busy_wait(mark)` —— `fork()` 出一个忙等子进程（打印 `.`），父进程自身也进入忙等（打印 `+`），两者低频发 `print_char` syscall（各上限 8 次）后静默自增；任务是 fork 时经 push 投送到空闲次核（P2），故两核各自长期持有 EL0 任务。
    - **QEMU 成对判据**：`scripts/qemu_boot_test.sh` 两分支均补 `-smp 2`（x86_64 分支此前无），并新增 APS-05 成对判据 —— 以 `grep -aoE` 抽取 `[SMP] EL0 pid=N cpu=M`，要求 `cpu=0` 与 `cpu=1` 成对出现，缺失即 `warn`（`FAIL_OK=0` 时置 `RESULT=1`，fail-closed）。
    - **`-a` 必要性（本轮修）**：日志含 NUL 字节（串口并发写）时 `grep` 默认将文件视为二进制、只输出 "Binary file ... matches" 而不输出匹配行，会使 `EL0_CPUS` 为空而误报；两分支 `grep` 均加 `-a` 强制按文本处理，不改变判据语义。
- **APS-06. 文档同步 + 验证收口**
  - 描述：更新登记项与前提。
  - 方案：订正 [aarch64-smp-bringup.md](./archive/aarch64-smp-bringup.md) §「后续专项登记」KPTI-PCPU-01（状态置结案并指向本工程）；在 [unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) 登记本工程；在 [progress-active-tasks.md](./progress-active-tasks.md) 关联。
  - 状态：[X]
  - 详情（施工结论）：① [aarch64-smp-bringup.md](./archive/aarch64-smp-bringup.md) §「后续专项登记」的 KPTI-PCPU-01 已置结案，并注明由本工程（DECISION-084）落地；② [unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) 已追加本工程登记（DECISION-084）；③ 关联的进行中任务清单文件实际路径为 [docs/plan/progress-active-tasks.md](./progress-active-tasks.md)（与预期一致，非漂移），已在该文件 §现状的「本轮更新」中登记本工程。

## 施工规划

| 批次 | 含任务 | 依赖 | 出口判据 | 状态 |
|---|---|---|---|---|
| **P1** | APS-01 + APS-02 | 无 | AP 具备调度身份并进入 idle 调度循环；双架构 QEMU 启动至 EL0 无回归（单核不带 `-smp` 仍正常）；六门槛全过 | [X] |
| **P2** | APS-03 | P1 | fork 任务可投送到空闲核；`-smp 2` 出现 `[SMP] migrate pid=K -> cpu=1`；六门槛全过 | [X] |
| **P3** | APS-04 | P1（可与 P2 并行） | KPTI 每核活跃值按核索引；host 契约（布局/索引）全过；六门槛全过 | [X] |
| **P4** | APS-05 + APS-06 | P2 + P3 | `-smp 2` 成对出现 `EL0 ... cpu=0` 与 `cpu=1`；文档前提订正；六门槛全过 | [X] |
| **P5** | 次核 CPU 状态初始化（本轮修复） | P4 | 双架构 `-smp 2` 出现 `[SMP] EL0 ... cpu=1`（次核不再崩溃离线）；六门槛全过 | [X] |
| **P6** | APS-05 x86_64 偶发未成对根因修复 | P5 | x86_64 `-smp 2` 连续多轮成对观测无未成对；回归测试锁定；六门槛全过 | [X] |

依赖：`P1 → {P2, P3} → P4 → P5 → P6`。

### P5. 次核 CPU 状态初始化

- 描述：AP 上电路径遗漏 per-CPU CPU 状态初始化，导致首个被投送到次核的任务在次核首次上下文切换时崩溃、核永久离线。
- 方案：aarch64 在 `arch/aarch64/smp_init.rs::ap_main` 开头使能 `CPACR_EL1.FPEN=0b11`；x86_64 在 `arch/x86_64/smp_init.rs::ap_entry` 调 `cpu::get_cpu_info()` + `cpu::init_msr()`，并在 `arch/x86_64/gdt.rs::gdt_init_ap` 补 `IA32_GS_BASE=&ap.syscall` 与 `IA32_KERNEL_GS_BASE=0`；`cpu::init_msr` 由私有提为 `pub(crate)`（供 AP 复用 BSP 探测到的特性集合）。
- 状态：[X]
- 详情（崩溃证据）：
  - **aarch64**：AP 未使能 `CPACR_EL1.FPEN`（BSP 在 `boot/aarch64/start.S` el2_entry 已设，AP stub `ap_entry_asm` 未复制），`context_switch_asm` 首次保存 V0-V31 即触发 FP/ASIMD trap（`ESR_EL1.EC=0x07`，实测 `SYNC! ESR=0x1FE00000`）；EL1 同步异常处理落入 `loop { wfi }`，核永久离线。
  - **x86_64**：AP 未使能 CR4.OSFXSR/OSXMMEXCPT（`process_switch_asm` 的 `fxsave` 在 CR4.OSFXSR=0 下触发 #UD → #DF，实测 `DoubleFault count=1`），且内核态 `IA32_GS_BASE` 未设（保持 0，`mov rdi,[gs:TRAMPOLINE_TOP_OFF]` 读到线性地址 0x20 垃圾 → 用户态出口栈错乱）。
  - 崩溃均在 AP 首次 `schedule()` → `context_switch` 路径；BSP 存活继续 fork。
- 详情（修后实测）：双架构 `[SMP] EL0 ... cpu=1` 出现（次核承载 EL0 任务），`find_idle_cpu` 恢复复选次核，日志 `SYNC!`/`DoubleFault`/`FREG` 计数为 0。
- 详情（门槛实测）：`./ci/build.sh all` 5/0；`./ci/build.sh aarch64 && ./ci/audit.sh quick` EXIT=0；`make test-host` 通过；`make test-kernel-host` 949/0；`TIMEOUT_QEMU=30 ./scripts/qemu_boot_test.sh all` 2/2 且双架构命中成对 EL0（`cpu=0` 与 `cpu=1`）。
- 详情（判据健全性）：`scripts/qemu_boot_test.sh` 的 EL0 判据 grep 加 `-a` —— 日志含 NUL 字节（串口并发写）时 `grep` 默认按二进制处理、只输出 "Binary file ... matches" 而漏输出匹配行；加 `-a` 后判据如实反映内核行为（详见 APS-05 详情）。

### P6. APS-05 x86_64 偶发未成对根因修复

- 描述：APS-05 成对判据（`EL0 ... cpu=0` 与 `cpu=1` 成对）在 x86_64 `-smp 2` 下**偶发只观测到 `cpu=0`**（失败率约 1/2 ~ 1/10, 非确定性; aarch64 不复发）—— 即 P4/P5 收口后残留的 x86_64 侧偶发未成对。
- 方案：恢复 `sys_fork` 为子进程调用 `user_proc_clone(parent_pid, child_pid)` 注册 `UserProc` 镜像记录, 并 fail-closed（注册失败回滚进程表条目）。**不采用**"扩大判据宽松度/掩盖偶发"的规避路径。
- 状态：[X]
- 详情（根因）：[proc_ops.rs](../../src/kernel/privileged/proc/proc_ops.rs) `sys_fork` 在 `PROCESS_TABLE.insert(...)` 后**未注册子进程的 `UserProc` 镜像**（该注册块在 commit `4557cd30` 重写 `sys_fork`、COW 崩溃临时改用共享页表时被附带删除）。`Scheduler::schedule` 的 per-CPU 装配块以 `USER_PROC_MANAGER.get(next)` 为门控, 未注册的子进程被投送到次核时 per-CPU 用户 CR3 滞留旧值、其"内核栈顶页"也不在其用户页表内; 用户态被硬件中断/异常打断时 CPU 按 `TSS.RSP0` 压 5 项 iretq 帧（先于任何软件切 CR3）即 `#PF`（cr2 = 内核栈顶-8）⇒ 子进程被内核终止、次核 EL0 观测缺失。
- 详情（证据）：失败日志 `migrate pid=6 -> cpu=1` 后立即 `[IDT] user exception: vec=14 err=0x2 rip=0x400030 cr2=0xFFFF800007E2FFF8`（= 子进程内核栈顶-8）→ `exit: pid=6 code=6`（被内核终止; 正常应为 code=0）。
- 详情（回归测试）：[kpti_x86_user_table_test.rs](../../host-tests/tests/kpti_x86_user_table_test.rs) 新增 `test_sys_fork_registers_child_in_user_proc_manager` —— 静态断言 `sys_fork` 在进程表插入之后经 `user_proc_clone` 注册子进程, 且失败分支回滚进程表条目。
- 详情（修后实测）：x86_64 **连续 20 轮 `FAIL_OK=0` 全部通过**（20/20）; 日志转为 `migrate pid=6 -> cpu=1` → `EL0 pid=6 cpu=1` → `exit: pid=6 code=0`（正常退出）。
- 详情（门槛实测）：`./ci/build.sh all` 5/0; `./ci/build.sh aarch64 && ./ci/audit.sh quick` EXIT=0; `make test-host` 通过; `make test-kernel-host` 949/0; `FAIL_OK=0 ./scripts/qemu_boot_test.sh all` 2/2。

## 关键约束

- **不抢占登记工程**：不改 `scheduler_tick()`/`SCHEDULER_EX`（DECISION-084 裁定 2）。**例外**：D6 tick 部分已由独立工程 ISSUE-RT-004 接管（见裁定 2 后续更新），其 `scheduler_tick()` 改动**不属本工程**。
- **单写者/锁序**：`cfs_rq` 为 per-CPU 锁；跨核入队须只持**目标核** `cfs_rq`，不得同时持两核队列（避免 AB-BA）；`resched_cpu` 在**锁外**调用（其内部锁 `cpu_queue`）。过 `audit_deadlock_matrix.py`。
- **F9**：`adopt_cpu_idle`/`cfs_enqueue_to`/`find_idle_cpu`/`resched_cpu` 均须有真实调用路径。
- **F4/F7**：新增 unsafe 仅限 `privileged/`；汇编索引改动补 `// SAFETY:`；中文注释。
- **host-tests 禁平行实现**：契约测试经 `host-test` 特性复用内核源码。
- **不提交**：本轮用户明确「先不提交」。

## 验证门槛

- AGENTS §2.3 六门槛全过（双架构 0w0e / clippy 0 warning / 核心审计 / `make test-host` / `make test-kernel-host` / QEMU 双架构）。
- 专项：`-smp 2` 下成对 `EL0 ... cpu=0` 与 `cpu=1`；单核（不带 `-smp`）无回归。

## 风险与回退

- **AP 首次调度路径**：AP 的 `current == 0 → idle` 迁移必须保证 `prev_ptr` 有效（APS-01）；若 `adopt_cpu_idle` 失败须 fail-fast 不上线（不静默回退，避免跨核混叠）。
- **x86_64 `get_current_cpu()` 返回 LAPIC ID 而 `sched_slot` 按 `cpu_index` 取模**（[scheduler.rs:200-203](../../src/kernel/privileged/proc/scheduler.rs#L200-L203)）：QEMU `-smp 2` 下 ID=0/1 恰好一致；非顺序 ID 硬件会错槽（**预存问题**，本工程不扩大、登记①）。
- **`halt` 与「schedule 后立即 halt」的唤醒窗口**：对齐既有 `idle_entry` 注释手法（`sti; hlt` 融合），避免丢唤醒。
- **KPTI 汇编偏移改写**：本工程最大回归面（EL0 入口/出口）。回退 = 恢复单实例固定偏移访问。
- **回退**：P2 可单独回退（`add_to_run_queue` 还原为仅本核入队）；P1 回退 = AP 恢复裸 `halt` 循环；P3 回退 = 恢复 `KPTI_GLOBALS` 单实例。P3 若独立保留，须与 P1 同步（否则偏移不一致）。

## 预存问题登记

- **① x86_64 `cpu_index` 与 LAPIC ID 语义不一致**（见上）：`sched_for` 在非顺序 LAPIC ID 硬件上错槽并回退 BSP 状态（表面可用、实则跨核混叠）。本工程不修，登记待独立处置。
- **② 退出进程 `UserProc` 镜像记录无回收路径**（✅ 已修复）：`user_proc.rs::destroy_by_pid_no_kstack` 在生产路径无调用者 ⇒ 进程退出时 `USER_PROC_MANAGER` 镜像记录不被回收；P6 恢复 fork 注册后, 每次 fork 多一条不回收记录。属预存架构缺口（历史注册子进程时同样如此）。
  - **修复**：在权威 `Process` 销毁唯一入口 [process.rs](../../src/kernel/privileged/proc/process.rs) 的 `remove_and_free` / `dec_ref_and_maybe_free`「引用归零即释放」分支、`Box::from_raw` **之前** 调 `USER_PROC_MANAGER.destroy_by_pid(pid)`（镜像须先于权威 `Process` 被移除, 见 INV-USER-PROC #2；镜像 `destroy` 经 `cr3` 翻译用户栈, 而 `Process::drop` 会销毁该页表）。全部 reap 路径（scheduler 周期僵尸回收 / exit 孤儿回收 / wait4 / fork 回滚）均经此收口, 故为单一权威回收点。`destroy` 内用户栈释放提前到页表销毁**之前**（否则 `cr3` 失效 ⇒ 静默漏释放）, 并以独立语句取句柄、与 `destroy` 的锁分离（避免 `if let` scrutinee guard 存活到 then 块造成同锁自锁死）；回收时释放内核栈（`keep_kstack=false`）。
  - **回归测试**：[user_proc_reclaim_contract_test.rs](../../host-tests/tests/user_proc_reclaim_contract_test.rs)（静态契约 fail-closed）。
  - **副产物**：接线新暴露 aarch64 VMM 页表遍历根表裸物理地址解引用预存缺陷, 一并修复（见 ISSUE-RT-007）。