# 多线程模型工程（独立大工程）

> **定位**：独立工程，非分册 8 内容。因涉及线程模型深入设计（调度器统一 / 线程组结构 / 共享资源语义 / 同步原语迁移），需要专门的设计过程与演进路径。
>
> **目标**：EG 从"多进程单线程"演进为 **1:1 多线程模型**（每用户线程 = 一个内核线程，进程 = 线程组 tgid）。与 Linux NPTL 对齐。
>
> **来源**：分册 8 审查中识别（G-17 多线程失效条件 + 双轨调度器结构性事实），2026-09-08 用户决策独立成档。
>
> **关联**：G-17（Mutex 重入检测 owner PID → 线程指针，本工程子项）。

## 1. 决策记录（2026-09-08 用户确认 D1-D6）

| 决策点 | 选择 | 说明 |
|---|---|---|
| D1 调度统一路线 | **A 统一线程级（每线程 = 调度实体）** | 用户已确认。现生产唯一调度器是 `privileged/proc/scheduler.rs` 的 PID 级 DL/RT/CFS；`scheduler_ex.rs` 已由 P-29 route C 删除（commit `96ee9beb`）。目标仍是每线程作为调度实体；在 D7 实体口径下现 `Process` 就是每线程 TCB，因此 K-04 是语义收口，不是调度器结构重写 |
| D2 G-17 owner 迁移 | **线程唯一标识** | 用户已确认。目标 owner = 线程 TCB 指针（当前即 `Process` 指针），数值 PID 只作索引；K-05 先审计 PID-keyed 同步的回收窗口，仅在确认复用会造成误判时升级存储 |
| D3 实施复杂度 | **循序渐进的相对完整** | 用户已确认；分阶段实施相对完整语义，每阶段验证 |
| D4 资源共享 | **CLONE_VM + FILES + SIGHAND + FS + CLONE_SETTLS 等** | 用户已确认；完整 Linux clone 共享语义（pthread 必需） |
| D5 tgid 结构 | **独立 ThreadGroup 结构** | 用户已确认；显式组对象，含组内线程列表与共享资源句柄；`Process` 不退化为纯容器，它就是每线程 TCB |
| D6 tick 错位 | **一并处理** | 用户已确认；tick 链路已在进程级理顺（见 §2），线程维度不再单独引入第二条 tick 路径 |
| D7 实体承载口径 | **Process = 每线程 TCB（PID=TID），ThreadGroup 独立** | 用户已认可分析结论。避免 `Process → Thread` 结构迁移与 TCB 重写：K-04 改为线程组语义收口与进程级捷径审计，K-05 改为 owner 审计；EEVDF D5① 与本口径同源 |

**线程映射模型**（2026-09-08）：**1:1**（每用户线程 = 内核 Thread），与 Linux NPTL 对齐。超轻量并发留待用户态协程库（独立，不与内核线程模型耦合）。

## 2. 调研结论（源码依据）

- **单轨、进程级调度**：当前唯一生产调度器是 PID 级 DL → RT → CFS（[scheduler.rs:1155](../../src/kernel/privileged/proc/scheduler.rs#L1155)）。并行实现 `scheduler_ex.rs` 已由 P-29 route C 删除（commit `96ee9beb`），`privileged/proc/thread.rs` 与 `privileged/syscall/clone.rs` 均不在现树；「双轨调度器」是历史快照，不能作为现状前提。
- **CLONE_VM 路径是现状“伪线程”**：`functions/proc/clone.rs` 的 `clone_vm_path`（[clone.rs:153](../../src/kernel/functions/proc/clone.rs#L153)）分配新 PID、共享父 cr3 并登记持有者（[clone.rs:216-235](../../src/kernel/functions/proc/clone.rs#L216-L235)），但只继承 `pwm/sched_policy/rt_priority`（[clone.rs:252-268](../../src/kernel/functions/proc/clone.rs#L252-L268)），把子任务推进 `parent.children`（[clone.rs:270-275](../../src/kernel/functions/proc/clone.rs#L270-L275)），复制父内核栈（[clone.rs:283-284](../../src/kernel/functions/proc/clone.rs#L283-L284)），未继承 fd/安全状态，也未建立线程组。
- **CLONE_THREAD 未建模**：只定义 6 个标志常量（[clone.rs:22-27](../../src/kernel/functions/proc/clone.rs#L22-L27)），`CLONE_THREAD` 仅参与组合校验（[clone.rs:45-49](../../src/kernel/functions/proc/clone.rs#L45-L49)），不参与分组；`CLONE_VM`（无论是否带 `CLONE_THREAD`）走同一路径；`CLONE_FILES`/`CLONE_FS`/`CLONE_SETTLS` 常量不存在。
- **无 tgid/线程组**：`Process` 无 tgid/线程组字段，`fd_table`/`sigaction_table` 挂 Process（[process.rs:251](../../src/kernel/privileged/proc/process.rs#L251)、[process.rs:286](../../src/kernel/privileged/proc/process.rs#L286)）；`gettid` 返回当前 PID（[info.rs:48-52](../../src/kernel/functions/proc/info.rs#L48-L52)）；`tgkill` 以 `tgid == tid` 等价校验（[signal.rs:525-555](../../src/kernel/functions/proc/signal.rs#L525-L555)）；`exit` 与 `exit_group` 共用同一处理（[dispatch.rs:790](../../src/kernel/functions/syscall/dispatch.rs#L790)）；`wait4` 遍历 `parent.children`（[wait4.rs:124-160](../../src/kernel/functions/proc/wait4.rs#L124-L160)），会把 CLONE_VM 子任务当作可收割子进程。
- **TLS 基址未随切换恢复**：`tls_base` 仅由 `arch_prctl` 写入（[dispatch.rs:594-635](../../src/kernel/privileged/syscall/dispatch.rs#L594-L635)）与 clone 归档（[clone.rs:345-353](../../src/kernel/functions/proc/clone.rs#L345-L353)）；x86_64 上下文切换只恢复 `fs` 选择子，不写 `IA32_FS_BASE`（[switch.asm:132-133](../../src/kernel/privileged/proc/switch.asm#L132-L133)）；aarch64 不在切换时写 `TPIDR_EL0`。用户态 TLS 在任务切换/迁移后不可靠。
- **cwd 是全局单例**：`VFS_MANAGER` 持有唯一 cwd（[vfs_manager.rs:136](../../src/kernel/functions/fs/vfs_manager.rs#L136)、[vfs_manager.rs:375-388](../../src/kernel/functions/fs/vfs_manager.rs#L375-L388)），`chdir` 直接改全局（[vfs_path.rs:163-172](../../src/kernel/functions/fs/vfs_path.rs#L163-L172)）。这既无法表达 CLONE_FS，也使当前所有进程共享同一 cwd。
- **PID 可复用**：`allocate_pid` 用位图回收（[process.rs:781-816](../../src/kernel/privileged/proc/process.rs#L781-L816)）；Mutex/PI/robust 以数值 PID 作 owner（[mutex.rs:158-160](../../src/kernel/privileged/sync/mutex.rs#L158-L160)、[mutex.rs:236-238](../../src/kernel/privileged/sync/mutex.rs#L236-L238)、[robust.rs:105](../../src/kernel/privileged/proc/robust.rs#L105)）。每线程独立 PID 的现状下自洽，但线程组共享锁后必须审计复用窗口。
- **tick 链路**：timer IRQ 单点驱动 `scheduler_tick` → 进程级 `Scheduler::tick`（[sched_ops.rs:81-87](../../src/kernel/privileged/proc/sched_ops.rs#L81-L87)）；重调度在中断退出路径执行；不存在第二条线程级 tick。
- **已登记线索**：code-audit:3826-3848 P1-A exit_group 线程组方案（DECISION-H21 → P1-L 未实施）。

## 3. 深入设计主题（独立设计过程，先设计后实施）

> 本工程核心价值在于**深入设计**，以下主题需在实施前完成设计论证（设计文档 / 决策记录 / 方案对比），不直接进入施工。

### 3.1 线程组模型（D5 + D7）

- **线程实体**：每个用户线程 = 一个内核 TCB（现 `Process`），`pid` 即 `tid`，调度器直接调度该 TCB；不引入第二套 `Thread` 结构与 `Process → Thread` 迁移。
- **ThreadGroup**：显式组对象，含 `tgid`（组长 TID）、成员列表、共享资源句柄（fd 表 / sigaction 表 / FS 状态）、退出状态与父进程信息；每个 TCB 持有 `tgid` 及组引用。
- **组长与 ID**：首个线程为组长，`tgid = 组长 pid`；`getpid()` 返回 `tgid`，`gettid()` 返回 `pid`；单线程进程中两者相等。
- **生命周期**：非组长线程退出不产生可 wait 的子进程；组内最后一个线程退出时组进入僵尸态，由父进程 `wait4` 收割；组对象在最后一个引用释放后销毁。
- **边界**：`Process` 不退化为纯容器——它承载每线程的调度、上下文、TLS、信号屏蔽字等线程级状态；`ThreadGroup` 只承载跨线程共享与组生命周期。

### 3.2 调度统一（D1 + D7）

- 现调度器已经按 `Process` 调度，而 D7 令 `Process` = 每线程 TCB，因此**不需要把 DL/RT/CFS 字段迁到新结构**；K-04 的工作是确认并收口所有“一个进程只有一个执行流”的隐含假设。
- 线程级调度字段（`nice`/`cfs_weight`/`sched_policy`/`rt_priority`/affinity）留在 TCB；线程组不持有调度策略，组内线程可各自设置。
- tick/current 链路保持单一路径（§2）；负载均衡与迁移以 TID 为键；`Scheduler::current` 仍返回当前 TCB 的 PID。
- 必须审计并修正的进程级捷径：`parent.children` 与 `wait4`、`process_exit` 的 fd 全关、信号组投递、全局 cwd、`session/pgid`、exec 的“替换自身”语义。
- 与 EEVDF 的协同：EEVDF D5① 先落现 TCB；K-04 只做本节的语义收口，E-08 只做口径对齐，不复制一套调度字段。

### 3.3 共享资源语义（D4）

- **CLONE_FILES**：fd 表改为引用计数共享；`close` 影响组内可见性；线程退出只释放自己的引用，最后一个引用才关闭 fd；`exec` 需解除共享并关闭 `CLOEXEC`。
- **CLONE_SIGHAND**：sigaction 表共享；待处理信号与屏蔽字为**每线程**状态；组定向信号选择一个未屏蔽目标线程投递，无可投递线程时挂到组级待处理集。
- **CLONE_FS**：新增 FS 状态对象（cwd/root/umask）并引用计数；`fork` 复制、线程共享；`chdir` 只影响本组。当前全局 `VFS_MANAGER.cwd` 必须先改为按组/FS 对象访问，否则 CLONE_FS 无法表达。
- **CLONE_SETTLS**：TLS 基址随 TCB 保存与切换恢复（K-00）；`tls != 0` 写子线程，`CLONE_VM` 且 `tls == 0` 时继承父线程。
- **创建语义**：新线程组（fork）继承父进程的 fd/安全状态；新线程加入已有组并共享组资源；当前 `clone_vm_path` 既不继承 fd/安全状态，也不区分建组与入组，K-02/K-03a 必须拆开。
- **exec**：终止组内其他线程、成为组长、解除 fd 表共享并按 `CLOEXEC` 关闭；当前事务式 `proc_exec_replace` 只替换地址空间，需补组语义。

### 3.4 同步原语迁移（D2）

- 现状 Mutex/PI/robust/futex 以数值 PID 作 owner；在 D7 下 PID 即 TID，且每线程唯一，因此**现有比较逻辑对存活线程仍自洽**。
- K-05 先做全量审计：`mutex.rs` 重入判定、`pi_mutex.rs` 持有表与移交、`robust.rs` 的 `FUTEX_TID_MASK` 比较、futex 等待队列键；确认线程退出清理与组退出清理的边界。
- 唯一实质风险是 PID 复用：位图回收后，旧锁记录若仍以裸 PID 比较可能误判。审计结论只有两种——证明回收窗口被清理协议封死（保持数值 PID），或改为 TCB 指针/带代号的 ID；后者才需要改存储结构。
- 不改锁序、不新增锁；线程组共享锁的并发正确性仍由现有 `Mutex<T>`/`IrqSpinLock` 纪律与 §8 的防线承担。

### 3.5 线程组系统调用语义

- `gettid` 返回 TID；`getpid` 返回 tgid；组长两者相等。
- `exit` 只退出当前线程；`exit_group` 终止组内全部线程；非组长退出不成为 waitable 子进程；组长退出后组状态由最后一个线程闭合。
- `tgkill(tgid, tid, sig)` 定向投递到组内线程；`kill(tgid)` 为组定向；`tgkill` 的 `tgid` 归属校验改为读取目标 TCB 的 `tgid`，不再使用 `tgid == tid` 等价。
- `wait4` 只收割线程组（组长），不把组内线程当子进程；`CLONE_CHILD_SETTID`/`PARENT_SETTID` 写入的是子线程 TID，`CLONE_CHILD_CLEARTID` 在线程退出时清零并 futex 唤醒。
- `rt_sigprocmask` 操作每线程屏蔽字；`sched_*`/`nice` 作用于每线程 TCB；`exec` 的组语义见 §3.3。

### 3.6 TLS 与用户态入口（Phase 0 / K-00）

- 每线程 `tls_base` 为线程级状态；上下文切换在切到 next 前写 x86_64 `IA32_FS_BASE` / aarch64 `TPIDR_EL0`。
- `arch_prctl(ARCH_SET_FS)` 仍立即写当前 CPU 系统寄存器；`fork` 继承父 TLS；`CLONE_SETTLS` 写子线程；`exec` 清零并写回系统寄存器。
- 用户态首次进入（`enter_user`）必须装载目标线程的 TLS；否则新进程/新线程从第一条指令起 TLS 就是错的。
- 验证至少覆盖：两进程交替切换后各自 TLS 不变、fork 子进程继承、exec 重置、aarch64 与 x86_64 同断言。QEMU 运行时用例覆盖切换保持与 fork 继承；启动环境只有一个内嵌 init ELF（未启用 initramfs），无法构造成功的 `execve` 目标，exec 重置由 host 侧源码契约测试覆盖。

## 4. 实施阶段（循序渐进，每阶段 = 状态 [X] + 验证门槛）

- **K-00. TLS 基址生命周期（Phase 0，先决）**
  - 描述：让 `tls_base` 真正随线程切换生效：x86_64 写 `IA32_FS_BASE`、aarch64 写 `TPIDR_EL0`；`arch_prctl` 立即生效；fork 继承、`CLONE_SETTLS` 写子线程、exec 清零；用户态首次进入装载 TLS。
  - 方案：在 privileged 增加按架构的 TLS 写入口；在 `context_switch_tail` 切到 next 前写 next 的 `tls_base`；`inherit_parent_state` 与 `clone_vm_init_context` 补继承；`proc_exec_replace` 重置；host 侧源码契约测试 + QEMU 用户态 TLS 用例。该阶段不引入线程组，先独立闭环。
  - 状态：[X]
  - 详情：TLS 基址经第三/第六参数传给切换与入口汇编, x86_64 在 `mov fs` 之后写 `IA32_FS_BASE`, aarch64 写 `TPIDR_EL0`; `arch_prctl` 常量收口到 `edgine-abi` 单源。QEMU 双架构实测覆盖 `ARCH_SET_FS` 立即生效、fork 继承、父子进程交替调度后各自 magic 不变（`[TLS] PASS` 门禁）；`exec` 重置由 `host-tests/tests/tls_base_restore_test.rs` 锁定（该启动环境无第二个可执行 ELF）。
- **K-01. 线程组基础结构（tgid/ThreadGroup）**
  - 描述：`Process` 增加 `tgid` 与组引用；新增显式 `ThreadGroup`（tgid、成员、共享资源句柄、退出状态）；`getpid` 返回 tgid、`gettid` 返回 TID；组长 = 首个线程。
  - 方案：组对象放 privileged（生命周期与引用计数需原子/锁）；`Process::new` 默认自建单线程组；组销毁由最后一个线程退出触发；父子关系仍以组为单位。
  - 状态：[X]
  - 详情：`privileged/proc/thread_group.rs` 落 `ThreadGroup`（`tgid` / `members: IrqSpinLock<Vec<Pid>>` / 退出态与退出码），`Process::thread_group: Arc<ThreadGroup>` 是组身份的唯一权威（不另建重复 `tgid` 字段），`Process::new` 自建单成员组（`tgid == pid`）。`getpid` 改经 `process_get_current_tgid()` 返回 tgid，`gettid` 仍返回 TCB 的 `pid`；`Scheduler::exit` 在进程表锁临界区内移除成员（层级 7 → 9），最后成员离开时置组退出状态（首个退出码生效），组内存随最后一个 TCB 的 `Arc` 引用消失回收。成员表只存 TID、不持 `Process` 引用，避免组与回收协议成环。**共享资源句柄（fd/信号/FS）与父子关系的组级归一按 K-02/K-03 推进，本阶段不建占位字段**（无读者字段会被 F9 判死）。同批修掉 `Scheduler::create_process` 插入失败时裸 `dealloc` 绕过析构的泄漏（含新 `Arc`）。验证：`thread_group` 内联单测 4 例 + `host-tests/tests/thread_group_k01_test.rs` 6 例（单成员组 / 组私有性 / 离开幂等与首退出码 / getpid-gettid 接线 / 退出路径锁域）；`docs/explain/ref-lock-order.md` 层级 9 与退出路径示例同步。门槛读数：`./ci/build.sh all` 6/6 通过（仅上游 `core`/`sha2` future-incompat 元信息）；`bash ci/audit.sh full` EXIT=0（审计腿 19/19、存量腿 9/9、Lockbud 4/4 crate 零发现、QEMU 2/2 + P-62 4/4，x86_64 TCP 回显 e2e 通过）；`make test-host` 1207 passed / 0 failed；`make test-kernel-host` 1101 passed / 0 failed；`make -f Makefile.ci ci-rustdoc` 两架构各 exit 0（0 源码级 link 错误）。
- **K-02. CLONE_THREAD 语义实现（线程创建接线）**
  - 描述：`clone_vm_path` 拆分为「入组」与「独立进程共享 VM」两条：带 `CLONE_THREAD` 时共享 tgid、加入线程组、分配独立 TID/内核栈/上下文；不带 `CLONE_THREAD` 时仍是共享地址空间的独立进程。
  - 方案：以现 `functions/proc/clone.rs` 的 `clone_vm_path` 为基础（不存在 `create_thread`）；线程不得推入 `parent.children`；补齐 `CLONE_SETTLS`/`PARENT_SETTID`/`CHILD_SETTID`/`CHILD_CLEARTID`；新组走 fd/安全状态继承，新线程共享组资源。
  - 状态：[X]
  - 详情：`clone_syscall` 增 `CLONE_THREAD` ⇒ `CLONE_VM` 校验（缺 VM 按非法组合返回 `EINVAL`，不静默走 fork）。`clone_vm_path` 以 `joins_group = flags & CLONE_THREAD != 0` 拆两路：线程的 `parent` 置 `None`（`vm_child_alloc`），`vm_child_register` 先取父线程组 `Arc` 入组（`Process::join_thread_group` / `ThreadGroup::join` 登记 TID）再插进程表，保证 `getpid` 首次上核即读到 tgid；独立 CLONE_VM 进程在登记成功后挂入 `parent.children`，线程不入 `children`。任一步失败回滚组成员身份 + 释放 PCB（含共享 cr3 引用），父组缺失返回 `ESRCH`、进程表满返回 `ENOMEM`。`vm_child_inherit` 收为 5 参（`child_ptr`/`child_pid`/`joins_group` 由 `child` 与登记阶段派生），内核栈分配失败与父上下文缺失两处改为释放半成品 PCB（原泄漏）。`CLONE_SETTLS` 沿用 K-00 的入参语义（`tls` 非零即覆盖继承值），`CLONE_PARENT_SETTID`/`CLONE_CHILD_SETTID`/`CLONE_CHILD_CLEARTID` 在共享 VM 路径写入，fork 路径维持不写 `CLONE_CHILD_SETTID`。**线程退出语义（`exit`/`exit_group` 分离、组收割）按 K-06，本阶段不越界**；共享 fd/信号/FS 句柄按 K-03。验证：`clone.rs` 内联单测 3 例 + `host-tests/tests/thread_group_k02_test.rs` 5 例（THREAD⇒VM 校验 / 入组先于登记与失败回滚 / 线程不挂父账 / 组身份共享）。门槛读数：`./ci/build.sh all` 6/6 通过（仅上游 `core`/`sha2` future-incompat 元信息）；`bash ci/audit.sh full` EXIT=0（审计腿 19/19、存量腿 9/9、Lockbud 4/4 crate 零发现、QEMU 2/2 + P-62 4/4，x86_64/aarch64 TCP 回显 e2e 各 3 轮通过）；`make test-host` 1212 passed / 0 failed；`make test-kernel-host` 1105 passed / 0 failed；`make -f Makefile.ci ci-rustdoc` 两架构各 exit 0（0 源码级 link 错误）。
- **K-03a. 共享资源语义（CLONE_FILES/SIGHAND/FS，第一批）**
  - 描述：线程共享 fd 表/信号处理/FS 状态；待处理信号与屏蔽字保持每线程；`exec` 在组成员数 >1 时 fail-fast（`EINVAL`），de_thread（终止兄弟线程、组长接管）与共享表拆卸移入 K-06。
  - 方案：fd 表、信号动作表、FS 上下文改 `Arc` 持有，clone 置位共享、未置位深拷贝；fd 表以显式持有者计数决定最后关闭者；`process_exit` 的 fd 全关改为最后持有者收口；全局 `VFS_MANAGER.cwd` 降级为无进程缺省，路径解析改按进程 FS 上下文；信号组定向投递与组级待处理集按 K-06。
  - 状态：[X]
  - 详情：用户裁定「A 拆两批」——本阶段只做共享语义与生命周期，exec 的组语义（de_thread / 组长接管 / unshare）并入 K-06。`FdTable` 增显式 `users: AtomicU32`（构造即 1，`add_user` / `release_user() -> bool`）：不用 `Arc::strong_count` 是因为瞬时借用会放大持有者数、使「最后用户」判定失真。`FdTable::copy_from(src, on_handle)` 在**持源表锁期间**逐句柄回调，fork/clone 回调用 `inc_open_file_ref` —— 先快照再放锁再 inc 会被并发 `close` 击穿（句柄已释放，inc 复活/误用槽位）。`Process` 三字段改 `pub fd_table: Arc<FdTable>` / `pub sigaction_table: Arc<Mutex<[u64; 64]>>` / `pub fs_context: Arc<FsContext>`，新增 `share_fd_table` / `copy_fd_table_from` / `share_signal_actions` / `copy_signal_actions_from` / `share_fs_context` / `copy_fs_context_from`；共享路径先 `add_user` 再换 `Arc`，复制路径先快照源锁再写本表（不嵌套两把同层锁）。`clone_vm_path` 在进程表登记成功后调用 `vm_child_adopt_resources` 分流共享/深拷贝，此前任何失败路径都无需回滚共享计数、之后无失败路径；`CLONE_FILES` / `CLONE_FS` 仅接受 `CLONE_THREAD`（否则 `EINVAL`，SIMPLIFIED：独立进程共享无拆卸机制，待 K-06 实装 exec unshare 后放开），`CLONE_SIGHAND` 以 `CLONE_VM` 为前提。fork 侧 `inherit_parent_fds` 走同一回调，新增 `VfsOps::dec_open_file_ref` 与 `release_inherited_fd_refs` 覆盖进程表登记失败 / UserProc 注册失败两条回滚；`inherit_parent_state` 深拷贝信号动作表与 FS 上下文。`process_exit` 仅当 `release_user()` 判为最后持有者才 `close_all_fds`（`release_pid_locks` 仍按 TID，组语义归 K-06）。exec：成员数 >1 直接 `EINVAL`（SIMPLIFIED），单线程路径先换新 `Arc<sigaction_table>` 再 `reset_signal_state_on_exec`，不清零潜在共享实例。新增 `privileged/proc/fs_context.rs`（cwd / 根前缀 / umask，`FS_MAX_PATH` 与 `VFS_MAX_PATH` 编译期断言绑定，`set_root` 重置 cwd）；`VFS_MANAGER` 增 `current_cwd` / `current_root_string` / `current_root_bytes` / `set_current_cwd` / `set_current_root`，有进程上下文时以进程为准、全局仅作无进程缺省；`chdir` / `getcwd` / `chroot` / `pivot_root` / `umask` 全部改走进程 FS 上下文；freg 快照增 `process_fs` 并在回滚时先恢复根再恢复 cwd。**每线程屏蔽字与组定向信号投递按 K-06**，本阶段不越界。验证：`fd_table` 内联单测 3 例 + `fs_context` 内联单测 4 例 + `host-tests/tests/thread_group_k03_test.rs` 13 例（clone 共享/复制分流与登记顺序、users 计数、源锁内 inc、exit 最后用户、fork 回滚、exec 守卫与换表、路径/umask 接线、fd/信号/FS 共享与副本解耦）。fork 另补内核栈分配失败路径的 `free_pid`（原 PID 位图泄漏）；`host-tests/tests/kpti_x86_user_table_test.rs` 的插入锚点由整条语句放宽为插入调用本身（外层现为 fail-closed 守卫），顺序与回滚断言不变。门槛读数：`./ci/build.sh all` 6/6 通过（仅上游 `core`/`sha2` future-incompat 元信息）；`bash ci/audit.sh full` EXIT=0（审计腿 19/19、存量腿 9/9、Lockbud 4/4 crate 零发现、QEMU 2/2 + P-62 4/4、本轮未执行腿 0）；`make test-host` 1225 passed / 0 failed（156 套）；`make test-kernel-host` 1111 passed / 0 failed；`make -f Makefile.ci ci-rustdoc` 两架构各 exit 0（0 源码级 link 错误）。
- **K-04. 调度语义收口（D1 + D7）**
  - 描述：确认每线程 TCB 即调度实体，**不做字段结构迁移**；审计并修正所有“一个进程只有一个执行流”的隐含假设。
  - 方案：逐项核对 `parent.children`/`wait4`、`process_exit`、信号投递、cwd、session/pgid、exec；tick/current/负载均衡仍走单一进程级实现，以 TID 为键；`SCHEDULER_EX` 已删除，无合并工作。
  - 状态：[X]
  - 详情：**“不做字段结构迁移”的口径**：不新建 `Thread` 结构、不迁移调度字段，每个 TCB 仍是唯一调度实体（tick / current / 负载均衡单轨、以 TID 为键）；本阶段唯一的字段移动是**进程级身份按 D5 与 K-01 遗留的“父子关系组级归一”收进 `ThreadGroup`**——`parent` / `children` / `session_id` / `pgid` 从 `Process` 移除，改由 `ThreadGroup` 承载（`parent`/`session_id`/`pgid` 为原子量，`children` 为层级 9 锁；`pgid()` 在 0 时回落 tgid，新组默认 `pgid == tgid`）。tick / current / 负载均衡（`tick_current_cfs` / `Scheduler::current` / `load_balance`）复核后保持单轨、以 TID 为键，不存在第二条执行流路径；cwd 已由 K-03a 的 `Arc<FsContext>` 按进程承载，本阶段复核无残留全局单流假设；exec 维持 K-03a 的多线程 `EINVAL` fail-fast，de_thread / 组长接管 / unshare 按 K-06。接线：`Process::new` 构造组时落父组；`sys_fork` 以父组 tgid 建子组、登记成功后 `group().add_child`，`inherit_parent_state` 从父组复制 session/pgid，`proc_get_ppid` 读组父；`clone_vm_path` 仅独立进程（非 `CLONE_THREAD`）入父组 `children`，线程不入账，`wait4.find_waitable_child` 只遍历线程组子账并按子组 pgid 匹配，线程因此不可被 wait。退出路径在进程表锁临界区内一次算定 `last_member`：每个 TCB 仍各自置退出码与 Zombie，而 `session_leader_exit` / `dec_limit` / 父组唤醒 / 子组改挂（已退出子组回收，否则改挂 pid 1）全部只在最后成员退出时执行；cgroup 成员身份按 TCB 各自摘除（每个退出 TCB 只摘自己的 PID 记录，未显式 attach 时为幂等空操作）；`tick_reap_zombies` 的父存活判定读组父。信号 `kill(0)` / `kill(-pgid)` 广播、`setsid` / `getsid` / `setpgid` / `getpgid` / `TIOCSCTTY` / `session_leader_exit` / `tcsetpgrp`、coredump 的 `pr_ppid`/`pr_pgrp`/`pr_sid`、procfs 两处 `PPid` 全部改读组身份；`proc_init_pgid` 退役（组构造已落 pgid）。**仍按 TID / 每 TCB 或归 K-06 的项**：定时器撤防与 `release_pid_locks` 仍按 TID（组定向投递归 K-06）；`exit` / `exit_group` 分离、组收割锚点、`tgkill`、每线程屏蔽字、组定向信号投递、exec `de_thread` / 组长接管 / unshare 均归 K-06；rlimit / seccomp / namespaces / cgroup / pwm 按 TCB 保留（D5 只归一进程身份与共享资源，策略状态未在 K-04 射程）；procfs 的 `Tgid` / `Pid` / `Threads: 1` 仍按单 TCB 输出（外观缺口，随 K-06/K-07 收口）。验证：`thread_group` 内联单测 9 例（新增组级父/子/会话/pgid 与入组可见性 3 例）+ `host-tests/tests/thread_group_k04_test.rs` 12 例（字段归属 / fork-clone-wait4-getppid 接线 / 退出 last_member 门控 / 僵尸回收父组 / 信号与会话组身份 / coredump+procfs / 组属性共享行为）；K-01/K-02 既有契约同步到新的退出与入账形状。门槛读数：`./ci/build.sh all` 6/6 通过（仅上游 `core`/`sha2` future-incompat 元信息）；`bash ci/audit.sh full` EXIT=0（审计腿 19/19、存量腿 9/9、Lockbud 4/4 crate 零发现、QEMU 2/2 + P-62 4/4、本轮未执行腿 0）；`make test-host` 1237 passed / 0 failed（157 套）；`make test-kernel-host` 1114 passed / 0 failed；`make -f Makefile.ci ci-rustdoc` 两架构各 exit 0（0 源码级 link 错误）。
- **K-05. G-17 owner 审计（D2）**
  - 描述：审计 Mutex/PI/robust/futex 等以数值 PID 为 owner 的同步原语在 D7 口径下是否仍成立；确认线程退出/组退出清理与 PID 复用窗口。
  - 方案：若审计证明存活线程内 PID 唯一且回收协议已封死误判，保持数值 PID；否则改为 TCB 指针或带代号的 ID。同步更新 `mutex_reentrant` 与多线程互斥用例；不改锁序。
  - 状态：[X]
  - 详情：**审计结论：保持数值 PID。** 存活线程内 PID 即 TID 且唯一（D7）；僵尸保留 TID，`free_pid` 只在 `remove_and_free` 的 `notify_process_exit`（PI 强制释放）与 `USER_PROC_MANAGER.destroy_by_pid` 之后执行，复用方看到的是已清干净的锁；创建失败路径的 `free_pid` 从未持锁；futex 等待者由阻塞线程唤醒后自摘或被唤醒者摘除，robust futex / `clear_child_tid` 按 TCB 字段在 `process_exit` 切页表前清理。owner 记账口径逐项确认按 TID：普通 `Mutex` 的重入比较与持有者记录都取 `process_get_current_pid`（`Scheduler::current()`，非 tgid），FFI `mutex_*` 同源；futex 入队/出队、robust futex 字比较均按 TID。**修复（K-05 直接相关缺陷）**：① `PI_MUTEX_HELD` 原来每次加锁 push 一条且正常解锁不摘除，登记表随加锁次数增长、对象析构后留悬垂地址 → 改为按 `(pid, 锁地址)` 去重，完全释放/移交时摘除原持有者条目，新增 `PiMutex::drop` 兜底 `untrack_all`；② `do_unlock` 移交原来先把 `locked` 清 `false` 再置 `true`，该窗口内并发 `try_lock` 可抢到锁、随后又被本函数覆盖 holder（双持有者）→ 移交保持 `locked=true`，holder/优先级一次落给目标，最后写 `handoff` 标记，目标在 `try_lock` 入口经 `accept_handoff` 确认接收并撤销自身可能的重入队；③ 完全释放改为先清 holder 再清 `locked`，避免释放窗口内的 CAS 胜者被旧 holder 覆盖；④ FFI `mutex_unlock` 完全释放原写 owner=0（PID 0 是合法无进程上下文，会让其误判为持有者）→ 改回 -1 无主哨兵，与 `MutexInner` 构造 / `MutexGuard` 一致。锁序不变：`PI_MUTEX_HELD` 与 `waiters` 不嵌套获取，移交标记为纯原子写。**边界登记见 A-7**：PI 退出清理时点是 reap 而非线程 `exit`（僵尸持锁者会阻塞等待者直至被 wait4/周期收割）；`try_lock` 失败返回前已登记等待者；PI 状态迁移未收敛为单一临界区（当前无生产使用者）。验证：`PI_MUTEX` 内联单测新增 4 例（完全释放摘除登记 / 移交登记与接收 / 待接收移交的退出释放 / 析构清登记）+ `host-tests/tests/multithreading_k05_test.rs` 7 例（同组不同 TID 互斥与同 TID 重入、FFI -1 哨兵、退出清理按 PID 归属且幂等、PID 排空→回收→重分配同一数值 PID 的持有权负控制、PI 登记生命周期 / 退出清理先于 PID 回收 / TID owner 源码契约）。门槛读数：`./ci/build.sh all` 6/6 通过（仅上游 `core`/`sha2` future-incompat 元信息）；`bash ci/audit.sh full` EXIT=0（审计腿 19/19、存量腿 9/9、Lockbud 4/4 crate 零发现、QEMU 2/2 + P-62 4/4、本轮未执行腿 0）；`make test-host` 1244 passed / 0 failed（158 套）；`make test-kernel-host` 1114 passed / 0 failed；`make -f Makefile.ci ci-rustdoc` 两架构各 exit 0（0 源码级 link 错误）。
- **K-06. 线程组语义系统调用**
  - 描述：`exit` 与 `exit_group` 分离；`tgkill` 按 tgid 归属校验；`kill(tgid)` 组定向；`wait4` 只收割线程组；`gettid`/`getpid` 修正；`clear_child_tid` 与 futex 唤醒按线程收口；**并入 K-03b**：多线程 `exec` 的 de_thread（终止兄弟线程、组长接管）与共享表拆卸（fd/FS/sigaction unshare、`CLOEXEC` 关闭）。
  - 方案：按 P1-A（code-audit:3826）方案实装；`dispatch.rs` 不再把 `SYS_EXIT | SYS_EXIT_GROUP` 合并；`wait4.find_waitable_child` 只看组长；信号投递遵守每线程屏蔽字。exec 在 `proc_exec_replace` 之前完成 de_thread 与组长身份迁移，再从共享表拆出私有副本（复用 K-03a 的 `copy_*` 路径）并解除 K-03a 对 `CLONE_FILES`/`CLONE_FS` 的 `CLONE_THREAD` 限制。
  - 状态：[X]
  - 详情：**exit/exit_group 分离**：`dispatch_proc_lifecycle` 两条独立匹配臂分别调用 `exit_syscall`（只 `process_exit` 当前 TCB）与 `exit_group_syscall`（`process_exit_group`）。`process_exit_group` 先 `ThreadGroup::request_exit` 落组权威退出码（首个生效），再遍历成员逐个登记不可屏蔽 `SIGKILL` 并 `Scheduler::unblock`，最后自身 `process_exit`；`Scheduler::exit` 的组感知 `effective_code` 取 `requested_exit_code`，兄弟线程随后因 SIGKILL 退出不会覆盖首个退出码。**组退出与入组并发收口**（自审发现，非门槛暴露）：① 退出请求必须在进程表锁内发布——`process_exit_group` 在 `PROCESS_TABLE.with_process` 回调里一次完成 `Arc::clone(group)` + `request_exit`，否则「取组」与「置位」之间可被 `clone` 插入；② `request_exit` 与 `join` 同处 `members` 锁域互斥（首个 `request_exit` 持锁置位、`join` 在锁内复查 `exit_requested` 即拒），成功入组者必进退出快照；③ `vm_child_register` 在入队前复查一次 `exit_requested`，命中即给子线程自挂不可屏蔽 `SIGKILL`，覆盖「入组早于退出请求、登记晚于成员快照」的漏网线程；④ `clone_vm_path` 的三条登记前失败路径（PCB 分配 / 属性继承 / 表登记）各归还一次 PID 位图槽位——HEAD 上 `clone.rs` 无一处 `free_pid`，登记前的每条失败路径都漏还（`Process::drop` 不碰位图，只有 `remove_and_free` 才释放），归还经 `api::proc_free_pid` 门面（与 `api::proc_alloc_pid` 对称），functions 层不直接引用 `PROCESS_TABLE` 全局，隐式依赖基线 39 处/15 项未扩。**信号**：`tgkill` 先查目标 TID 的 `group().tgid()` 校验归属（`sig == 0` 探测同样校验），再 `do_signal_send(tid, ...)` 线程定向；`kill` 经 `do_signal_send_extended` 统一按线程组分发——`pid > 0` 映射到该 TID 所属 tgid，`0` 当前组，`-1` 全部（init 除外），`< -1` 指定进程组；广播路径用 `collect_matching_tgids` 按 tgid 去重后在进程表锁外逐组投递。`do_signal_send_group` 组内择线程遵守每线程 `blocked_mask` 与 `SIG_IGN`，不可捕获信号（SIGKILL/SIGSTOP）无视屏蔽投首个存活成员，全员屏蔽时挂起到首个未忽略成员；`sys_kill` 放行 `0..=63`（pending 位图为 64 位，含 RT 信号）。致命默认动作 `terminate_from_fatal_signal` 统一走组退出（对齐 Linux `complete_signal` 的「fatal signal kills the whole thread group」），退出码 `128 + signum`；唯一例外是 de_thread 期间兄弟线程收到的 SIGKILL 只结束自身。**wait4**：就绪条件改为 `group_zombie_exit_code`（组已退出且全部 TCB 落 Zombie），`process_reap_group` 用 `claim_reap` CAS 保证单一回收者；回收前先从父组 `children` 摘除 tgid——释放后 tgid 可复用，迟到的摘除会误删同值的新子组；`find_waitable_child` 只遍历组子账，非组长 TID 不可等待；`tick_reap_zombies` 的孤儿组回收同源。**exec de_thread（并入 K-03b）**：`begin_exec_de_thread_snapshot` 在同一成员表锁域置 `exec_de_thread` 标记并快照成员，`join` 在 `exec_de_thread` 或 `exit_requested` 任一置位下拒绝新线程（clone 回滚 PCB 与 PID 槽位并返回 `EAGAIN` 可重试），兄弟线程登记 SIGKILL 后等其落 Zombie，随后只回收非组长 TCB，组长 TCB 保留为 tgid 锚点，最后 `end_exec_de_thread` 恢复入组；等待循环区分「已回滚成员」与「已入组未落表」两种 `None`。替换映像后：信号动作表换新 `Arc` 再 `reset_signal_state_on_exec`、FS 上下文 new Arc + `copy_from` unshare、`close_cloexec_fds` 维持 POSIX CLOEXEC；`replace_user_space` 在调用线程无镜像时先补建镜像再接管。`update_user_context` 改为读权威 `Process.cr3`（镜像仅作 current 查询缓存，缺失不再阻止线程切换）。`CLONE_FS` 对共享 VM 的独立进程放开；**`CLONE_FILES` 未按方案解除**——fd 表 unshare 尚未实装，非线程共享会让 exec 的 `CLOEXEC` 关闭波及其他持有者，保持线程限定（SIMPLIFIED 注释在 `clone.rs`）。内核栈所有权单点归 `Process::drop`，按 `kstack_pages` 归还（线程无镜像也能覆盖）。**语义口径**：`kill(-1)` 不杀 init；广播目标全灭时 `deliver_to_groups` 返回 `ESRCH`；`sig == 0` 组存在性探测按 `PROCESS_TABLE.get(tgid)`。**预存问题登记**（本轮发现、非 K-06 引入，见 A-8/A-9）：UserProc 镜像 `destroy` 与权威 `Process::drop` 对同一 cr3 各 `frame_dec` 一次；镜像 `alloc_zeroed` 的内存未归还。验证：`thread_group` 内联单测 14 例（新增 de_thread 拒绝入组/恢复 1 例、组退出请求拒绝后入组 1 例，既有 join 用例同步返回值断言）+ `host-tests/tests/thread_group_k06_test.rs` 16 例（exit/exit_group 分派、getpid/gettid、tgkill 归属、kill 四路径、组内择线程与每线程屏蔽字、wait4 组收割与父组子账摘除、de_thread 标记/快照/组长锚点/入组 EAGAIN、exec unshare 与镜像补建、权威 cr3 切换、致命信号组退出、yield 等待方退出、clear_child_tid 唤醒、`exit_group` 与 `clone` 并发闭合；含组收割与组信号两组行为用例）。门槛读数：`./ci/build.sh all` 6/6 通过（仅上游 `core`/`sha2` future-incompat 元信息）；`bash ci/audit.sh full` EXIT=0（审计腿 19/19、存量腿 9/9、Lockbud 4/4 crate 零发现、QEMU 2/2 + P-62 4/4、本轮未执行腿 0）；`make test-host` 1260 passed / 0 failed（159 套）；`make test-kernel-host` 1120 passed / 0 failed；`make -f Makefile.ci ci-rustdoc` 两架构各 exit 0（0 源码级 link 错误）。
- **K-07a. 线程组运行时判据与 ABI 单源**
  - 描述：`clone` / `futex` / `CLONE_*` / 线程相关 syscall 编号收进 `edgine_abi::thread` 单源；userlib 补齐 `clone_thread` / `futex_wait` / `futex_wake` / `exit_group` / `tgkill` 包装与 clone 子分支汇编；init 内嵌 ELF 增加线程组运行时探针（每线程 TLS、tid/tgid 身份、futex 双向往返、join、exit_group、`sig == 0` 存在性探测）；`scripts/qemu_boot_test.sh` 双架构断言 `[THREAD] PASS`；新增 `scripts/thread_stress_test.sh` 专项多线程压力，并以固定轮次接入 `bash ci/audit.sh full`。
  - 方案：载体取内嵌 `init` ELF（用户裁定「最优解且长期优解」——探针与既有 `[TLS]` / `[KPTI]` 判据同域，不需先建 initramfs 装载链）；压力维度取「`full` 档内固定轮次 + 专项脚本」（用户裁定 A）。
  - 状态：[X]
  - 详情：**ABI 单源**：新增 `src/rust/edgine-abi/src/thread.rs`，把线程族编号收为一份——`SYS_CLONE` 56 / `SYS_KILL` 62 / `SYS_GETTID` 186 / `SYS_FUTEX` 202 / `SYS_EXIT_GROUP` 231 / `SYS_TGKILL` 234、`CLONE_VM`/`CLONE_FS`/`CLONE_FILES`/`CLONE_SIGHAND`/`CLONE_THREAD`/`CLONE_SETTLS`/`CLONE_PARENT_SETTID`/`CLONE_CHILD_SETTID`/`CLONE_CHILD_CLEARTID`、`FUTEX_WAIT`/`FUTEX_WAKE`/`FUTEX_REQUEUE`/`FUTEX_WAIT_BITSET`/`FUTEX_WAKE_BITSET`/`FUTEX_PRIVATE_FLAG`。privileged 侧 `syscall/types.rs` 的 6 处硬编码编号改为引用单源、并新增 `pub use edgine_abi::thread::{...}` 出口（与 `SchedAttr` 同口径），`syscall/futex.rs` 的 6 条操作码同改；functions 层经 `functions::syscall::types` 的 glob re-export 取值（F2 的合法方向，不直连 `edgine-abi`），`functions/proc/clone.rs` 的 7 条本地 `const CLONE_` 与 `functions/sync/futex.rs` 的 6 条本地 `pub const FUTEX_` 随之退役——内核分派、内核机制层与用户态 wrapper 三方从此共享同一数值。**userlib**：`clone_thread` 双架构裸机包装——entry/arg 偷渡在被调用者保存寄存器（x86_64 `r12/r13`、aarch64 `x21/x22`，由 `proc_save_user_regs*` 随上下文整体复制），`clone` 返回后父侧拿 TID、子侧在 `asm!` 块内直接跳入 `thread_trampoline`，入口返回后走 `SYS_EXIT` 只终止本线程（K-06 的 `exit` 单 TCB 语义）；另有 `futex` / `futex_wait` / `futex_wake` / `exit_group` / `tgkill` 包装。**init 探针六判据**（`[THREAD] PASS:` / `[THREAD] FAIL:` 同 `[TLS]`/`[KPTI]` 判据域）：① `CLONE_*SETTID` 回填与 `clone` 返回值一致；② 每线程 TLS（各线程 `fs_base`/`TPIDR_EL0` 读到自己的块，`THREAD_TLS_MAGIC_BASE + idx`）；③ 身份（`gettid` 逐线程唯一且 ≠ tgid/组长 TID，`getpid` 恒等于 tgid）；④ futex 双向往返（子线程报到唤醒父线程、父线程放行唤醒子线程）；⑤ join（`CLONE_CHILD_CLEARTID` 写 0 + futex 唤醒）与 `exit_group` 组退出码 7 上浮；⑥ `sig == 0` 存在性探测（组内活 TID 全返 0、越界 TID 返负、`kill(tgid, 0)` 组定向返 0）。所有等待以 `THREAD_RETRY_LIMIT` 限界，失败路径仍放行并 join 已建线程——判据失败只打印 `FAIL`，绝不把启动拖挂。**真跑发现的两个缺陷（x86_64 单侧跑测掩盖）**：① 用户线程栈落 `.rodata`——无内部可变性的 `static` 被 rustc 归入 `.rodata`，aarch64 该 PT_LOAD 不带 `PF_W`，装载成 EL0 只读页，子线程首次压栈即触发 level-3 权限错误（`ESR=0x9200004F`、`FAR` 落在栈块内），运行时表现为 `[THREAD] FAIL: settid code=10`；x86_64 的 `link.x` 把 `.rodata` 并入可写 PT_LOAD，同形代码两架构结论相反。修复为 `ThreadStack(UnsafeCell<[u8; THREAD_STACK_BYTES]>)` 新类型（配 `unsafe impl Sync`：每个实例只服务一条线程，共享的只是地址，写入方始终是该栈所属线程），判据 = `nm` 里 `THREAD_STACKS` / `GROUP_HELPER_STACK` 落 `b`（`.bss`）。② aarch64 返回用户态前的信号投递未接线——`process_exit_group` 对睡在 futex 上的兄弟线程只投递一次 `SIGKILL` + `Scheduler::unblock`，该线程醒来后重睡同一等待字且无投递点消费致命信号，整组收不齐、父进程 `wait4` 永久挂死（x86_64 有 syscall/IRQ 返回前的 `do_signal_deliver`，故缺口只在 aarch64 暴露）。修复为 `process_block` 在 `Scheduler::schedule()` 返回处按「`SIGKILL` pending 且（`group().exit_requested()` 或 `group().exec_de_thread()`）」收口，判据与 `Scheduler::yield_current` 的致命信号检查同源（内核内等待循环都不是投递点，这是兜底而非本体）；根因登记为 **A-10**。**语义口径**：内核以 `tls != 0` 作为 `CLONE_SETTLS` 触发（K-00 既定，非按 `CLONE_SETTLS` 位），故需要「继承父 TLS、不换基址」的线程必须显式传 `tls = 0`——init 的组退出 helper 即此例，两侧注释锚定该口径，并由 host 契约锁定。**运行时断言与压力**：`scripts/qemu_boot_test.sh` 在 x86_64 与 aarch64 分支各加 fail-closed 断言（缺 `[THREAD] PASS:` 即 `RESULT=1`），匹配只取 `PASS:` 前缀（串口与内核日志并发写会插进同一行，长文案易被截断），并用 `grep -a` 规避日志 NUL 字节导致的「二进制文件」误判；新增 `scripts/thread_stress_test.sh`（判据与启动断言同源）以 `THREAD_STRESS_ARCH`（both/x86_64/aarch64）、`THREAD_STRESS_SMP`（核数）、`THREAD_STRESS_TIMEOUT`、`THREAD_STRESS_QEMU_EXTRA`（如 `-accel tcg,thread=single` / `-icount`）组合重复启动，命中里程碑即提前 kill、失败日志逐次保留，可按轮次/时序扰动复现多线程挂死；该脚本同时接入 `ci/audit.sh` 步骤 4 的 QEMU 段之后作 full 档固定轮次（默认每架构 3 轮，`THREAD_STRESS_ROUNDS` 可上调；判据与启动断言同源），分工是 `qemu_boot_test.sh` 证「单次能过」、压力子腿证「重复能过」——丢唤醒/join 竞态/跨核时序这类偶发缺陷只有重复启动才现形，单次断言不足以判稳。**验证**：`host-tests/tests/thread_group_k07a_test.rs` 9 例（ABI 单源与内核 re-export / `tls != 0` 口径 / 两架构 `clone_thread` 子分支跳 `thread_trampoline` 且偷渡寄存器就位 / userlib 包装齐备 / 线程栈内部可变与 `Sync` 归属 / `process_block` 唤醒收口 / init 六判据锚点与 `_start` 接线 / 启动断言与压力脚本判据同源且 fail-closed / `ci/audit.sh` full 档固定轮次接线）；`thread_group_k03_test.rs` 的 clone 标志锚点随单源迁移同步（本地 `const` 退役）。K-07b 的 initramfs 载体与 EEVDF 调度回归交叉验证不在本批射程。门槛读数：`./ci/build.sh all` 6/6 通过（仅上游 `core`/`sha2` future-incompat 元信息）；`bash ci/audit.sh full` EXIT=0（审计腿 19/19、存量腿 9/9、Lockbud 4/4 crate 零发现、QEMU 2/2、线程组多轮压力 6/6（3 轮 x 2 架构，`[THREAD] PASS` 判据）、P-62 feature 运行维 4/4、本轮未执行腿 0）；`make test-host` 1269 passed / 0 failed（160 套）；`make test-kernel-host` 1120 passed / 0 failed；`make -f Makefile.ci ci-rustdoc` 两架构各 exit 0（0 源码级 link 错误）。
- **K-07b. 运行时载体扩围与多线程压力全量收口**
  - 描述：initramfs 载体与独立测试 ELF（`execve` 运行时用例、多线程压力与 G-17 场景、与 EEVDF 的调度回归 IC1-IC3/IE1-IE6 交叉验证）。
  - 方案：把探针从内嵌 `init` 迁到可由 initramfs 装载的独立 ELF，使 `execve` 后的线程/TLS/组语义进入运行时射程（K-00 的 exec 重置与 `CLONE_SETTLS` 接线此前只有 host 侧源码契约）；压力腿在同一载体上叠加轮次与核数，并与 EEVDF 工程的调度断言交叉跑。
  - 状态：[]

### 依赖关系

```
K-00 → K-01 → K-02 → K-03a ─┐
                 │           ├→ K-06 → K-07
                 └→ K-04 → K-05 ┘
```

- K-00 先独立完成：TLS 是线程/进程切换正确性的前置，且可先于线程组落地。
- K-01/K-02 可先行（不依赖调度收口）；K-03a 依赖 K-02 的组结构；K-03b（exec de_thread/组长接管/unshare）并入 K-06。
- K-04 是语义审计而非结构迁移；K-05 依赖 K-04 的线程身份结论。
- K-06 依赖 K-01~K-04 与 K-03a；K-07a 用内嵌 `init` 载体先把线程组判据接入双架构真跑，K-07b 再扩到 initramfs 载体与压力全量收口。

## 5. 验证门槛（每阶段不可豁免）

- §2.3 六条门槛全量通过：双架构 `./ci/build.sh all` 0w0e、clippy/fmt、核心审计 F1-F9（含 `audit_deadlock_matrix` 锁顺序）、host-tests、QEMU、host 侧内核单元测试；审计档位显式 `full`。
- K-00（已闭）：QEMU 用户态 TLS 用例（切换保持 / fork 继承）双架构通过；exec 重置与 `CLONE_SETTLS` 接线由 host 侧源码契约测试覆盖（启动环境无第二个可执行 ELF）。
- K-02/K-03a：线程创建与组生命周期、共享 fd/cwd、信号共享与 fork 深拷贝语义用例通过；exec 多线程守卫（`EINVAL`）与单线程换表清零由 host 侧契约锁定。
- K-04：进程级身份（父组 / 子组账 / session / pgid）组级归一与退出 `last_member` 门控由 `host-tests/tests/thread_group_k04_test.rs` 契约 + `ThreadGroup` 行为用例锁定；线程不入父组子账，`wait4` 只见线程组。
- K-04 调度语义收口后：现有单进程调度用例全绿，且 `wait4` 不把线程当子进程。
- K-05 owner 审计：mutex_reentrant + 多线程并发互斥 + PID 复用窗口负控制通过。
- K-06（已闭）：exit/exit_group、tgkill/kill、wait4 组收割、exec de_thread/组长接管/共享表拆卸、clear_child_tid/futex 唤醒逐项用例通过。
- K-07a（已闭）：线程组六条判据在内嵌 `init` 探针上双架构真跑通过，并进入 `scripts/qemu_boot_test.sh` 的 fail-closed 断言；专项压力脚本 `scripts/thread_stress_test.sh` 可按轮次/核数/时序扰动重复复验，且以固定轮次（默认每架构 3 轮）接入 `bash ci/audit.sh full` 的 QEMU 段——单次启动证「能过」，重复启动判「稳不稳」。
- K-07b：多线程压力测试在双架构 QEMU 上稳定；与 EEVDF 的调度回归（IC1-IC3/IE1-IE6）交叉验证；`execve` 后运行时用例进入射程。

## 6. 关联与风险

- **G-17**：本工程子项（K-05）。D7 口径下每线程 PID 唯一，现有 owner 比较对存活线程自洽；K-05 先审计 PID 复用与退出清理，再决定是否升级为 TCB 指针/代号。
- **风险（实体口径）**：Process=每线程 TCB 避免了结构迁移，但把线程语义隐含在既有结构里；必须逐项审计进程级捷径（children/wait、fd 关闭、cwd、信号、exec），否则多线程下会出现静默错误。
- **风险（共享资源）**：fd 表/FS/sigaction 的引用计数与并发访问是本工程最重的改动面；必须与 `fd_alloc`、信号子系统、VFS 交互逐项设计，不能只加锁了事。
- **风险（exec）**：多线程 exec 需要终止兄弟线程并解除共享，当前事务式 `proc_exec_replace` 未覆盖；K-03a 以成员数 >1 fail-fast（`EINVAL`）拦住，de_thread / 组长接管 / unshare 按 K-06 实装，并与 K-03a 的共享资源引用计数联合验证。
- **风险（TCB 内重构）**：调度器/同步原语改动需完整回归 + 压力测试；分阶段降险（K-00~03a 先做线程与 TLS，K-04 再收口调度语义）。
- **依赖**：无外部依赖；可独立启动；与 EEVDF 工程按 D5① 并行，K-04/E-08 为交接点。

## 7. 变更历史

- **2026-09-08**：创建本工程（从分册 8 独立成档）。用户决策 D1-D6 确认；线程映射模型定为 1:1；调研结论登记；阶段拆分 K-01~K-07 建立。
- 用户认可实体重解释：新增 D7「Process = 每线程 TCB（PID=TID），ThreadGroup 独立」，K-04 由调度字段迁移改为语义收口，K-05 由 owner 重写改为审计；新增 K-00 TLS 生命周期为 Phase 0。
- 现状调研重写：移除已删除的 `scheduler_ex.rs`/`thread.rs`/`privileged/syscall/clone.rs` 引用，改按现树（`functions/proc/clone.rs`、`functions/proc/info.rs`、`functions/proc/signal.rs`、`functions/proc/wait4.rs`、`functions/fs/vfs_manager.rs`）登记；补充 CLONE_VM 伪线程、TLS 未恢复、全局 cwd、PID 复用四项缺口。
- 阶段与门槛修正：新增 K-00，K-02 改按 `clone_vm_path` 拆分入组/独立进程共享 VM，K-03 纳入 exec 组语义与每线程信号状态，K-06 引用改为现 `dispatch.rs`；验证门槛补齐 §2.3 六条与 TLS/线程组专项。
- **2026-11-01**：K-01 闭（`[X]`）。落 `ThreadGroup`（tgid/成员账/退出状态）与 `Process::thread_group: Arc<ThreadGroup>`；`Process::new` 自建单成员组；`getpid` 返回 tgid、`gettid` 返回 TID；`Scheduler::exit` 在进程表锁内移除成员并在最后一个成员离开时置组退出状态。共享资源句柄按 K-03、线程入组按 K-02 推进，K-01 不建占位字段。
- **2026-11-01**：K-02 闭（`[X]`）。`clone_vm_path` 按 `CLONE_THREAD` 拆「入组」与「独立进程共享 VM」：线程 `parent=None`、不入 `parent.children`，`ThreadGroup::join` 在进程表登记前登记 TID 并带回滚；`CLONE_THREAD` 缺 `CLONE_VM` 返回 `EINVAL`；顺带修掉 `vm_child_inherit` 两处失败路径的 PCB 泄漏。线程退出语义与组收割按 K-06 推进。
- **2026-11-02**：K-04 闭（`[X]`）。口径：不做 `Thread` 结构/调度字段迁移，`Process` 仍是唯一调度实体（TID 为键）；仅把进程级身份（`parent` / `children` / `session_id` / `pgid`）按 D5 迁入 `ThreadGroup`。fork/clone 以父组 tgid 建子组并只把独立进程挂入父组子账，线程不入账；`wait4` 只见线程组；退出拆卸（会话 / 计数 / 父唤醒 / 子组改挂）只在组最后成员退出时执行，cgroup 成员身份按 TCB 各自摘除，每 TCB 仍各自置 Zombie 与退出码；信号广播、会话 syscall、coredump、procfs 全部改读组身份，`proc_init_pgid` 退役。定时器撤防与 `release_pid_locks` 仍按 TID，`exit`/`exit_group`、组收割、`tgkill`、每线程屏蔽字、组定向信号、exec `de_thread` 等按 K-06。验证：`host-tests/tests/thread_group_k04_test.rs` 12 例 + `thread_group` 内联 9 例；`docs/explain/ref-lock-order.md` 层级 9 与 fork/exit 示例同步 `thread_group.children`。
- **2026-11-02**：K-05 闭（`[X]`）。owner 审计结论：保持数值 PID（存活线程内 PID 即 TID 且唯一；`free_pid` 只在 `notify_process_exit` 的 PI 强制释放与 `destroy_by_pid` 之后，复用方看到已清干净的锁）。同批修复 4 个直接相关缺陷：`PI_MUTEX_HELD` 按 `(pid, 锁地址)` 去重并在完全释放/移交时摘除、`PiMutex::drop` 兜底；`do_unlock` 移交保持 `locked=true` 一次落 holder 并最后写 `handoff`，目标在 `try_lock` 入口 `accept_handoff` 接收；完全释放先清 holder 再清 `locked`；FFI `mutex_unlock` 完全释放 owner 回 `-1` 无主哨兵。残留边界登记 A-7（PI 退出清理时点在 reap、`try_lock` 失败已登记等待者、状态迁移未收敛单临界区）。验证：`PI_MUTEX` 内联 4 例 + `host-tests/tests/multithreading_k05_test.rs` 7 例（多 TID 互斥/同 TID 重入、FFI 哨兵、PID 排空复用负控制、登记生命周期与源码契约）。门槛读数见 K-05 详情。
- **2026-11-01**：K-03a 闭（`[X]`）。用户裁定「A 拆两批」：本批只做共享资源语义与生命周期，`exec` 的 de_thread/组长接管/unshare 并入 K-06（K-03b）。落 `Arc<FdTable>`（显式 `users` 计数 + 源锁内 `inc_open_file_ref` 回调）、`Arc<FsContext>`（cwd/根前缀/umask）、`Arc<sigaction_table>`；clone 置位共享、未置位深拷贝，接管点在进程表登记成功之后；`CLONE_FILES`/`CLONE_FS` 仅接受 `CLONE_THREAD`，`CLONE_SIGHAND` 以 `CLONE_VM` 为前提；`process_exit` 由最后持有者关闭 fd 表；fork 两处失败路径经 `dec_open_file_ref` 回滚；exec 成员数 >1 fail-fast（`EINVAL`）+ 单线程先换新信号表再清零；`VFS_MANAGER` 全局 cwd/root 降级为无进程缺省，`chdir`/`getcwd`/`chroot`/`pivot_root`/`umask` 走进程 FS 上下文，freg 快照含进程 cwd/root。验证：`host-tests/tests/thread_group_k03_test.rs` 13 例 + `fd_table`/`fs_context` 内联单测 7 例；`docs/explain/ref-lock-order.md` 层级 9 同步新增锁域；A-6 闭。剩余缺口（每线程屏蔽字、组定向信号投递、exec 组语义）按 K-06。

- **2026-11-02**：K-07 按用户裁定拆为 K-07a/K-07b。载体取内嵌 `init` ELF（探针与 `[TLS]`/`[KPTI]` 判据同域，initramfs 装载链留 K-07b），压力维度取「`full` 档内固定轮次 + 专项脚本」。

- **2026-11-02**：K-07a 闭（`[X]`）。线程族 ABI 收进 `edgine_abi::thread` 单源（6 个 syscall 编号 + 9 个 `CLONE_*` + 6 个 `FUTEX_*`），privileged `syscall/types.rs`/`syscall/futex.rs` 改引用单源并新增 re-export 出口，functions 层经 `functions::syscall::types` 消费，`clone.rs` 的 7 条本地 `const CLONE_` 与 `functions/sync/futex.rs` 的 6 条本地 `pub const FUTEX_` 退役；userlib 落 `clone_thread`（双架构 `asm!`，entry/arg 偷渡 `r12/r13` 与 `x21/x22`，子分支直接跳入 `thread_trampoline`）与 `futex`/`futex_wait`/`futex_wake`/`exit_group`/`tgkill` 包装；init 落线程组六判据探针（settid 回填、每线程 TLS、tid/tgid 身份、futex 双向往返、join 与 `exit_group` 退出码、`sig == 0` 探测），全部等待限界、失败不拖挂启动。真跑暴露并修复两缺陷：用户线程栈因 `.rodata` 无 `PF_W` 在 aarch64 只读（改 `ThreadStack(UnsafeCell<..>)` + `unsafe impl Sync`，判据 = `nm` 落 `b`）；aarch64 返回用户态前无信号投递点，使 `exit_group` 的一次性 SIGKILL 无法被 futex 睡眠线程消费、父 `wait4` 永久挂死（`process_block` 唤醒返回处按致命信号收口，根因登记 A-10）。`scripts/qemu_boot_test.sh` 双架构加 `[THREAD] PASS:` fail-closed 断言，新增 `scripts/thread_stress_test.sh`（`THREAD_STRESS_ARCH`/`SMP`/`TIMEOUT`/`QEMU_EXTRA` 可扰动），并作 full 档固定轮次接入 `ci/audit.sh` 步骤 4（默认每架构 3 轮）。验证：`host-tests/tests/thread_group_k07a_test.rs` 9 例 + `thread_group_k03_test.rs` 锚点随单源同步。门槛读数见 K-07a 详情。

- **2026-11-02**：K-06 闭（`[X]`）。`exit`/`exit_group` 拆独立分派与独立机制入口（`process_exit` vs `process_exit_group`，后者 `request_exit` 首码权威 + 逐成员 SIGKILL + 自身退出）；`tgkill` 按目标 TID 的 `group().tgid()` 校验归属后线程定向，`kill` 四路径统一按线程组去重投递，组内择线程遵守每线程屏蔽字/忽略，SIGKILL/SIGSTOP 无视屏蔽，sig 范围放宽到 0..=63；`wait4` 改为组就绪 + `claim_reap` 单回收者 + 回收前摘父组子账；exec de_thread（标记+快照同锁域、入组 EAGAIN、兄弟 SIGKILL、组长锚点、FS/sigaction 拆卸、无镜像补建、权威 cr3 切换）；`CLONE_FS` 放开、`CLONE_FILES` 保持线程限定（fd unshare 未实装）。自审补口：`exit_group` 的退出请求改在进程表锁内发布，`request_exit`/`join` 共 `members` 锁互斥，`vm_child_register` 入队前复查组退出请求并对漏网线程自挂 SIGKILL，`clone_vm_path` 三条登记前失败路径经新门面 `api::proc_free_pid` 归还 PID 位图槽位（HEAD 上该文件无一处 `free_pid`，登记前失败全部漏还；隐式依赖基线未扩）。预存问题登记 A-8（镜像与进程对同一 cr3 双 `frame_dec`）与 A-9（镜像内存未归还）。验证：`thread_group` 内联 14 例 + `host-tests/tests/thread_group_k06_test.rs` 16 例。门槛读数见 K-06 详情。

## 8. 并发安全能力现状与缺口（登记）

> 现状三层防线：**L1 构造性排除**（锁纪律 + `Mutex<T>` 数据封装 + functions `#![deny(unsafe_code)]` + IrqSpinLock 加锁即关中断，永远生效）→ **L2 CI 静态审计**（audit_deadlock_matrix / audit_invariants / audit_static_mut / audit_volatile_access / audit_tlb_receive_order）→ **L3 运行时 lockdep**（仅 debug）。下列为本轮核对发现的缺口，登记为待办。

- **A-1. lockdep 在生产 release 构建被编译掉（运行时零检测）**
  - 描述：lockdep 所有调用点均为 `#[cfg(debug_assertions)]` 门控（[spinlock.rs:83-86](../../src/kernel/privileged/sync/spinlock.rs#L83-L86)、[rwlock.rs:240](../../src/kernel/privileged/sync/rwlock.rs#L240)、[mutex.rs:237](../../src/kernel/privileged/sync/mutex.rs#L237)，pi_mutex 同理）。release 生产构建 `debug_assertions` 关闭 → AB-BA / 递归 / 中断睡眠锁等运行时检测全部消失；[lockdep.rs:29-34](../../src/kernel/privileged/sync/lockdep.rs#L29-L34) 自认此为刻意零开销取舍。release 上的并发安全实际只依赖 L1 结构性保证。
  - 方案：评估 release + `feature = "lockdep"` 的可选检测通路（与 A-2 协同），并在文档明确「release 无运行时锁检测」这一事实及由此产生的风险面。
  - 状态：[]
- **A-2. lockdep 文档与实现口径不一致（`feature = "lockdep"` 未接线）**
  - 描述：[lockdep.rs:3](../../src/kernel/privileged/sync/lockdep.rs#L3) 声明「`debug_assertions` **或** `feature = "lockdep"` 启用时」生效，但调用点仅 `#[cfg(debug_assertions)]`，无 `feature = "lockdep"` 分支 → 该 feature 实际不启用任何检测，声明与实现不一致。
  - 方案：二选一——① 调用点改 `#[cfg(any(debug_assertions, feature = "lockdep"))]`，使声明成立并保留生产可选检测能力（倾向）；② 修订文档删除该 feature 说法。
  - 状态：[]
- **A-3. AB-BA 死锁的静态检测未实现**
  - 描述：[audit_deadlock_matrix.py:12-14](../../scripts/audit_deadlock_matrix.py#L12-L14) 自认「AB-BA 死锁环检测：需先建立锁顺序声明机制」为未实现项，脚本仅覆盖中断上下文非法锁 / 非中断安全锁 / sleep 锁原子上下文。故 F8 静态面对 AB-BA 无兜底，叠加上 A-1 后 release 上 AB-BA 无任何自动检测。
  - 方案：建立锁顺序声明机制（锁类序表 / annotation）+ 静态环检测；或明确降级为「仅 debug 期 lockdep 覆盖」并在文档登记该边界。
  - 状态：[]
- **A-4. 缺数据竞争检测器与内存序自动证明**
  - 描述：无 KASAN/TSAN 类工具；无 Acquire/Release 内存序的自动证明；volatile / static mut / LTO 布局仅靠模式审计（audit_volatile_access / audit_static_mut / audit_repr_c）。
  - 方案：随本工程（多线程启用前）评估引入 debug 期内存序断言 hook 等轻量增强；对无法自动化的盲区在文档登记为已知覆盖边界。
  - 状态：[]
- **A-5. PID-keyed 同步标识的复用窗口**
  - 描述：Mutex/PI/robust/futex 以数值 PID 作 owner（[mutex.rs:158-160](../../src/kernel/privileged/sync/mutex.rs#L158-L160)、[robust.rs:105](../../src/kernel/privileged/proc/robust.rs#L105)），而 `allocate_pid` 会回收 PID 位图（[process.rs:781-816](../../src/kernel/privileged/proc/process.rs#L781-L816)）。D7 口径下每线程 PID 唯一，存活线程内比较正确；但线程退出与新线程复用同一 PID 时，旧锁记录/等待队列若未清理会造成误判。release 构建无运行时检测（A-1），该风险只能靠审计与用例封死。
  - 方案：K-05 列出全部 PID-keyed 表与清理路径，做「线程退出 → PID 回收 → 新线程申请同 PID」负控制；若无法证明清理完备，改为 TCB 指针或带代号的 ID。
  - 状态：[X]
  - 详情：K-05 审计结论为**保持数值 PID**（依据见 K-05 详情）：存活 TID 唯一、僵尸保留 TID、`free_pid` 只在 PI 退出强制释放与 UserProc 镜像销毁之后；PI 持有登记按 `(pid, 锁地址)` 去重并在完全释放/移交/析构时摘除；futex 等待者由阻塞线程自摘或被唤醒者摘除。负控制落在 `host-tests/tests/multithreading_k05_test.rs`：排空 PID 位图制造唯一空位 → 释放 → 重分配得到同一数值 PID → 新化身取得的是无主锁，而非旧化身仍持有的锁（同时断言登记表已清空）。遗留边界（PI 退出清理时点为 reap、`try_lock` 失败已入队、状态迁移非单临界区）登记为 A-7，不改为 TCB 指针/代号。
- **A-6. 全局 cwd 的多线程/多进程语义缺口**
  - 描述：`VFS_MANAGER.cwd` 是全局单例（[vfs_manager.rs:136](../../src/kernel/functions/fs/vfs_manager.rs#L136)），`chdir` 直接改全局（[vfs_path.rs:163-172](../../src/kernel/functions/fs/vfs_path.rs#L163-L172)）。当前所有进程共享同一 cwd，CLONE_FS 无法表达；线程组共享 cwd 时也没有独立对象可引用。并发访问由全局锁串行化，不存在数据竞争，但语义错误且会成为线程组共享资源的单点。
  - 方案：K-03 引入按 FS 对象/线程组持有的 cwd，`fork` 复制、线程共享；保留内核线程的默认全局 cwd 作为缺省值。
  - 状态：[X]
  - 详情：K-03a 落 `privileged/proc/fs_context.rs`（cwd / 根前缀 / umask）与 `Process::fs_context: Arc<FsContext>`；`CLONE_FS` 线程共享同一 `Arc`，fork / 未置位 clone 深拷贝。`VFS_MANAGER` 的 `current_cwd` / `current_root_string` / `current_root_bytes` / `set_current_cwd` / `set_current_root` 在有进程上下文时以进程 FS 上下文为准，全局 cwd/root 仅作无进程缺省；`chdir` / `getcwd` / `chroot` / `pivot_root` / `umask` 已全部接线；freg 快照包含进程 cwd/root 并在回滚时先根后 cwd。锁域登记在 `ref-lock-order.md` 层级 9。剩余项：组定向信号投递与每线程屏蔽字属 K-06，不在本条射程。
- **A-7. PI Mutex 退出释放时点与状态机边界（K-05 登记）**
  - 描述：PI 的退出强制释放经 `ProcessTable::remove_and_free → notify_process_exit → pi_mutex_process_exit` 触发，时点是**僵尸被收割**而非线程 `exit`：持锁线程退出后若尚未被 `wait4` 或周期回收，等待者会持续自旋（当前 `PiMutex` 无生产使用者，风险不落地）。另两处边界：`try_lock` 失败路径在返回 `false` 前已把调用者登记为等待者（调用方若放弃重试，条目保留到下一次 unlock/exit）；`locked` / `holder` / `effective_priority` / `waiters` 的迁移依赖原子与重试，未收敛为单一临界区。
  - 方案：若 K-06/K-07 启用 PI，把退出释放前移到 `Scheduler::exit` 的每 TCB 清理（需先把优先级撤销与 `yield` 从释放路径分离，避免在调度退出前让出），并用单一状态锁收敛 `try_lock` / `do_unlock` / `accept_handoff`；在此之前保持现状，由 `PI_MUTEX` 内联 4 例与 K-05 host 回归锁锁定现有语义。
  - 状态：[]
- **A-8. UserProc 镜像与权威 Process 对同一 cr3 重复 `frame_dec`（K-06 登记，预存）**
  - 描述：`remove_and_free` 先 `USER_PROC_MANAGER.destroy_by_pid`，镜像 `destroy` 对记录的 cr3 调一次 `frame_dec`（计数归零则 `destroy_user_page_table`）；随后 `Box::from_raw` 释放 `Process`，`Process::drop` 对同一 cr3 再调一次。正常时序下第二次落在已归零的计数槽上是 no-op，但两次调用之间该帧可被其他核重新分配，第二次递减会把新持有者的计数抹到 0 并触发误销毁；该路径与 K-06 的改动无关（K-06 仅让无镜像的线程 TCB 依赖 `Process::drop` 单点归还）。fork/exec 产物仍在射程。
  - 方案：收敛 cr3 销毁所有权——建议保留 `Process::drop` 为唯一减计点，删除镜像 `destroy` 的 cr3 分支，并复核 exec 旧 cr3 的显式转移；补 host 侧帧计数负控制（同一 cr3 只允许一次归零销毁）。
  - 状态：[]
- **A-9. UserProc 镜像分配未归还（K-06 登记，预存）**
  - 描述：`alloc_user_process` 经 `raw::alloc_zeroed` 分配 `UserProcess`，镜像 `destroy` 只从 `BTreeMap` 移除条目，未释放该指针；每次进程回收泄漏一块镜像内存。K-06 未改变该路径。
  - 方案：镜像 `destroy` 在移除条目后与 `alloc_user_process` 配对释放指针；确认释放时 `Process` 仍存活（镜像字段为非拥有句柄，不触发析构）。
  - 状态：[]
- **A-10. aarch64 返回用户态前的信号投递未接线（K-07a 发现，预存）**
  - 描述：`do_signal_deliver` 只有 x86_64 两个消费点——syscall 返回前（[dispatch.rs:179](../../src/kernel/privileged/syscall/dispatch.rs#L179)）与中断返回前（[table.rs:830](../../src/kernel/privileged/idt/table.rs#L830)、[table.rs:893](../../src/kernel/privileged/idt/table.rs#L893)）。aarch64 的 `svc_handler`（[exception.rs:586](../../src/kernel/privileged/arch/aarch64/exception.rs#L586)）直接返回 `x0`，既不投递信号也不构造信号帧，`rt_sigreturn` 分支的注释自认「aarch64 信号投递尚未实现，此拦截为预留」。后果：aarch64 上任何捕获型信号（含用户装 handler 的 SIGILL/SIGSEGV）都不会被投递，屏蔽字与 `SIG_IGN` 的择线程逻辑在运行时形同虚设；致命信号只能靠下一次阻塞唤醒收口。K-07a 的线程探针首先在 aarch64 真跑上暴露该缺口的连锁反应：`process_exit_group` 对睡在 futex 上的兄弟线程只投递一次 `SIGKILL` + `unblock`，该线程醒来后若无投递点就会重睡同一等待字，整组收不齐、父进程 `wait4` 永久挂死。本轮以 `process_block` 唤醒返回处的致命信号收口解掉挂死（见 K-07a 详情），属「内核等待循环」侧的兜底，不是投递点本体。
  - 方案：为 aarch64 实装返回用户态前的 `do_signal_deliver` 等价路径——`ExceptionFrame`（x0-x30 / elr / spsr / sp）与 `InterruptFrame` 的抽象统一（帧类型收敛为架构相关的 `UserFrame`），致命默认动作直接终止、捕获型信号在用户栈构造 `aarch64` 版 `SignalFrame` 并改写 `elr`/`spsr`，`rt_sigreturn` 从该帧恢复；同时把中断返回路径（`handle_irq` 出口）纳入同一检查点，使运行中的线程可被 SIGKILL 打断而不必等待下一次阻塞。补 host 侧源码契约 + QEMU 运行时用例（aarch64 下 `kill(tid, SIGTERM)` 装 handler 收到并返回原指令）。
  - 状态：[]
