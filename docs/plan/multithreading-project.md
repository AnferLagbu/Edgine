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
  - 状态：[]
- **K-06. 线程组语义系统调用**
  - 描述：`exit` 与 `exit_group` 分离；`tgkill` 按 tgid 归属校验；`kill(tgid)` 组定向；`wait4` 只收割线程组；`gettid`/`getpid` 修正；`clear_child_tid` 与 futex 唤醒按线程收口；**并入 K-03b**：多线程 `exec` 的 de_thread（终止兄弟线程、组长接管）与共享表拆卸（fd/FS/sigaction unshare、`CLOEXEC` 关闭）。
  - 方案：按 P1-A（code-audit:3826）方案实装；`dispatch.rs` 不再把 `SYS_EXIT | SYS_EXIT_GROUP` 合并；`wait4.find_waitable_child` 只看组长；信号投递遵守每线程屏蔽字。exec 在 `proc_exec_replace` 之前完成 de_thread 与组长身份迁移，再从共享表拆出私有副本（复用 K-03a 的 `copy_*` 路径）并解除 K-03a 对 `CLONE_FILES`/`CLONE_FS` 的 `CLONE_THREAD` 限制。
  - 状态：[]
- **K-07. 测试与验证收口**
  - 描述：TLS 切换/继承/exec 重置；线程创建/共享/exit_group/信号定向/组收割；多线程压力与 G-17 场景；双架构 QEMU 回归。
  - 方案：§2.3 六条门槛 + 专项多线程压力；host-tests 覆盖组生命周期与共享资源引用计数；QEMU 用户态用例验证 `pthread` 风格 TLS 与 futex。
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
- K-06 依赖 K-01~K-04 与 K-03a；K-07 最后做全量收口。

## 5. 验证门槛（每阶段不可豁免）

- §2.3 六条门槛全量通过：双架构 `./ci/build.sh all` 0w0e、clippy/fmt、核心审计 F1-F9（含 `audit_deadlock_matrix` 锁顺序）、host-tests、QEMU、host 侧内核单元测试；审计档位显式 `full`。
- K-00（已闭）：QEMU 用户态 TLS 用例（切换保持 / fork 继承）双架构通过；exec 重置与 `CLONE_SETTLS` 接线由 host 侧源码契约测试覆盖（启动环境无第二个可执行 ELF）。
- K-02/K-03a：线程创建与组生命周期、共享 fd/cwd、信号共享与 fork 深拷贝语义用例通过；exec 多线程守卫（`EINVAL`）与单线程换表清零由 host 侧契约锁定。
- K-04：进程级身份（父组 / 子组账 / session / pgid）组级归一与退出 `last_member` 门控由 `host-tests/tests/thread_group_k04_test.rs` 契约 + `ThreadGroup` 行为用例锁定；线程不入父组子账，`wait4` 只见线程组。
- K-04 调度语义收口后：现有单进程调度用例全绿，且 `wait4` 不把线程当子进程。
- K-05 owner 审计：mutex_reentrant + 多线程并发互斥 + PID 复用窗口负控制通过。
- K-06：exit/exit_group、tgkill/kill、wait4 组收割、exec de_thread/组长接管/共享表拆卸、clear_child_tid/futex 唤醒逐项用例通过。
- K-07：多线程压力测试在双架构 QEMU 上稳定；与 EEVDF 的调度回归（IC1-IC3/IE1-IE6）交叉验证。

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
- **2026-11-01**：K-03a 闭（`[X]`）。用户裁定「A 拆两批」：本批只做共享资源语义与生命周期，`exec` 的 de_thread/组长接管/unshare 并入 K-06（K-03b）。落 `Arc<FdTable>`（显式 `users` 计数 + 源锁内 `inc_open_file_ref` 回调）、`Arc<FsContext>`（cwd/根前缀/umask）、`Arc<sigaction_table>`；clone 置位共享、未置位深拷贝，接管点在进程表登记成功之后；`CLONE_FILES`/`CLONE_FS` 仅接受 `CLONE_THREAD`，`CLONE_SIGHAND` 以 `CLONE_VM` 为前提；`process_exit` 由最后持有者关闭 fd 表；fork 两处失败路径经 `dec_open_file_ref` 回滚；exec 成员数 >1 fail-fast（`EINVAL`）+ 单线程先换新信号表再清零；`VFS_MANAGER` 全局 cwd/root 降级为无进程缺省，`chdir`/`getcwd`/`chroot`/`pivot_root`/`umask` 走进程 FS 上下文，freg 快照含进程 cwd/root。验证：`host-tests/tests/thread_group_k03_test.rs` 13 例 + `fd_table`/`fs_context` 内联单测 7 例；`docs/explain/ref-lock-order.md` 层级 9 同步新增锁域；A-6 闭。剩余缺口（每线程屏蔽字、组定向信号投递、exec 组语义）按 K-06。

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
  - 状态：[]
- **A-6. 全局 cwd 的多线程/多进程语义缺口**
  - 描述：`VFS_MANAGER.cwd` 是全局单例（[vfs_manager.rs:136](../../src/kernel/functions/fs/vfs_manager.rs#L136)），`chdir` 直接改全局（[vfs_path.rs:163-172](../../src/kernel/functions/fs/vfs_path.rs#L163-L172)）。当前所有进程共享同一 cwd，CLONE_FS 无法表达；线程组共享 cwd 时也没有独立对象可引用。并发访问由全局锁串行化，不存在数据竞争，但语义错误且会成为线程组共享资源的单点。
  - 方案：K-03 引入按 FS 对象/线程组持有的 cwd，`fork` 复制、线程共享；保留内核线程的默认全局 cwd 作为缺省值。
  - 状态：[X]
  - 详情：K-03a 落 `privileged/proc/fs_context.rs`（cwd / 根前缀 / umask）与 `Process::fs_context: Arc<FsContext>`；`CLONE_FS` 线程共享同一 `Arc`，fork / 未置位 clone 深拷贝。`VFS_MANAGER` 的 `current_cwd` / `current_root_string` / `current_root_bytes` / `set_current_cwd` / `set_current_root` 在有进程上下文时以进程 FS 上下文为准，全局 cwd/root 仅作无进程缺省；`chdir` / `getcwd` / `chroot` / `pivot_root` / `umask` 已全部接线；freg 快照包含进程 cwd/root 并在回滚时先根后 cwd。锁域登记在 `ref-lock-order.md` 层级 9。剩余项：组定向信号投递与每线程屏蔽字属 K-06，不在本条射程。
