#![deny(missing_docs)]
use super::fs_context::FsContext;
use super::rlimit::RlimitTable;
use super::scheduler::SchedPolicy;
use super::thread_group::ThreadGroup;
use super::types::{
    BlockReason, KERNEL_STACK_SIZE, MAX_PROCESSES, Pid, ProcessContext, ProcessFlags, ProcessId,
    ProcessPriority, ProcessState, USER_KSTACK_SIZE,
};
use crate::privileged::mm::{KERNEL_BASE, MmStruct, PAGE_SIZE, USER_ADDR_FLOOR};
use crate::privileged::sync::IrqSpinLock as Mutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicU32, AtomicU64, Ordering};

// ============================================================================
// 进程级 FD 分配策略 (P1-I-01 提取)
// ============================================================================
//
// D8: FdTable 分配策略 (first-fit, 上限 64) 经 DECISION-J (2026-09-13) 反转
// 迁回 privileged/proc/fd_table.rs (Process 机制字段归 privileged).
// 详见 [docs/plan/maintenance-2026-06-11.md] (P1-I-01).
pub use crate::privileged::proc::fd_table::{FdTable, MAX_FDS_PER_PROCESS};

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    fn pmm_alloc_pages(count: u64) -> *mut u8;
    fn vmm_create_user_page_table() -> u64;
    fn vmm_destroy_page_table(cr3: u64);
}

/// boot 栈与内核栈顶部 canary 的哨兵值; 被改写即判定栈溢出.
pub const KERNEL_STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;

/// [`Process::eevdf_rq_cpu`] 的"不在任何核的运行队列上"哨兵值.
///
/// 队列归属与 [`Process::eevdf_on_rq`] 是一对**必须同步更新**的记账凭证: 二者只在
/// 持有对应核 `eevdf_rq` 锁的临界区内改写 (见 `proc::scheduler::eevdf_place_on`).
/// 缺了本字段, 跨核搬运只能"先摘后投"分两段做, 中间窗口里任务既不在源树也不在
/// 目的树, 任何并发唤醒都会把它重复投进第三个队列 —— 同一任务同时挂在两棵树上
/// 即两核同时上核运行它, 进程上下文与 socket 状态随即被踩坏.
pub const NO_RQ_CPU: u32 = u32::MAX;

/// [`Process::eevdf_rq_cpu`] 的"归属已被某个搬动者认领"中间态.
///
/// 存本值期间, 该任务的队列归属由认领者独占 (它正持相应 rq 锁搬移树节点与记账),
/// 其他意图搬动同一任务的人**不得阻塞等待** —— 认领者正在等的 rq 锁可能就在等待者
/// 手里, 一等待即 AB-BA。因此认领一律非阻塞 (一次 CAS, 失败即走人), 且认领者保证
/// 在释放 rq 锁前把归属写回具体值 (成功 → 目的核, 放弃 → 原值), 不会把本态留住.
pub const MIGRATE_PENDING: u32 = u32::MAX - 1;

// boot 栈底部符号, 栈从 `stack_top` 向 `stack_bottom` 方向增长.
// canary 在 boot trampoline / aarch64 入口阶段写入 `stack_bottom` 处.
// 符号语义按架构而异:
//   x86_64 : 由 boot.asm 在 `.bootbss` 定义, 是**低 VMA** 符号, 须由 Rust 侧加
//            `KERNEL_BASE` 别名化;
//   aarch64: 内核真高半区链接, 低半区符号距内核代码超出 `adrp` 的 ±4GB 可达范围,
//            故由链接脚本直接把 `stack_bottom` 定义为高半区别名 (见 link/aarch64.ld).
// host-test 下 boot.asm 无产物且 canary 函数桩化, 声明一并排除 (符号契约归零).
// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
#[cfg(not(feature = "host-test"))]
unsafe extern "C" {
    static stack_bottom: u8;
}

/// 检查 boot 栈 canary 是否完整.
///
/// boot 栈位于低半区恒等映射区 (`.bootbss`), 经其高半区别名访问: x86_64 由本函数
/// 加 `KERNEL_BASE` 别名化, aarch64 的 `stack_bottom` 本身即链接脚本给出的高别名.
/// 返回 true 表示 canary 未被覆盖 (栈未溢出至栈底).
#[must_use]
pub fn check_boot_stack_canary() -> bool {
    // 符号桩化 (host-test): host 无 stack_bottom 汇编符号且无 boot 栈,
    // 常量中性返回 true (canary 完整语义的保守默认).
    #[cfg(not(feature = "host-test"))]
    {
        // SAFETY: stack_bottom 是 boot.asm / start.S 定义的静态符号,
        // 指向 boot 栈底部的 8 字节 canary 区域.
        // 读取操作是 volatile 的, 无数据竞争 (boot 阶段单核).
        unsafe {
            // 经 `&raw const` 取符号地址: 该符号声明为 `u8`, 若先构造 `&u8` 引用再
            // 当 `u64` 读写, 会触发 `invalid_reference_casting` (引用越界).
            #[cfg(target_arch = "aarch64")]
            let canary_addr = &raw const stack_bottom as u64;
            // x86_64: 符号为低 VMA, 加 `KERNEL_BASE` 得高半区别名.
            #[cfg(not(target_arch = "aarch64"))]
            let canary_addr = KERNEL_BASE + (&raw const stack_bottom as u64);
            let value = core::ptr::read_volatile(canary_addr as *const u64);
            value == KERNEL_STACK_CANARY
        }
    }
    #[cfg(feature = "host-test")]
    {
        true
    }
}

/// 写入 boot 栈 canary 到 `stack_bottom`.
///
/// 供 aarch64 入口在 `clear_bss` 之后调用 (`x86_64` 由 boot.asm trampoline 写入).
/// 必须在 BSS 清零之后调用, 否则 canary 会被清零覆盖.
pub fn write_boot_stack_canary() {
    // 符号桩化 (host-test): host 无 stack_bottom 汇编符号, 整段跳过
    // (host 无 boot 栈 canary 语义).
    #[cfg(not(feature = "host-test"))]
    {
        // SAFETY: stack_bottom 是 boot.asm / start.S 中定义的静态符号,
        // 写入 8 字节 canary 值, boot 阶段单核无竞争.
        // `&raw const` 取址理由同 `check_boot_stack_canary` (避免引用越界).
        unsafe {
            #[cfg(target_arch = "aarch64")]
            let canary_addr = &raw const stack_bottom as u64;
            // x86_64: 符号为低 VMA, 加 `KERNEL_BASE` 得高半区别名.
            #[cfg(not(target_arch = "aarch64"))]
            let canary_addr = KERNEL_BASE + (&raw const stack_bottom as u64);
            core::ptr::write_volatile(canary_addr as *mut u64, KERNEL_STACK_CANARY);
        }
    }
}

/// 检查内核栈顶 8 字节处的 canary 是否符合预期; 栈顶非法或地址越界时视为通过。
#[must_use]
pub fn kernel_stack_check_canary(stack_top: u64) -> bool {
    if stack_top < 8 {
        return true;
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let canary_ptr = (stack_top - 8) as *const u64;
        if (canary_ptr as u64) < USER_ADDR_FLOOR {
            return true;
        }
        let value = core::ptr::read_volatile(canary_ptr);
        value == KERNEL_STACK_CANARY
    }
}

/// 在内核栈顶 8 字节处写入 canary 哨兵值; 栈顶非法或地址越界时跳过。
pub fn kernel_stack_write_canary(stack_top: u64) {
    if stack_top <= 8 {
        return;
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let canary_ptr = (stack_top - 8) as *mut u64;
        if (canary_ptr as u64) < USER_ADDR_FLOOR {
            return;
        }
        core::ptr::write_volatile(canary_ptr, KERNEL_STACK_CANARY);
    }
}

/// 进程控制块 (PCB) — 保存标识、状态、调度信息、地址空间与资源统计等运行时状态。
pub struct Process {
    /// 进程标识符, 与进程表槽位下标一致.
    pub pid: ProcessId,
    /// 进程 PWM 身份/权限字 (由 SGEG 凭证子系统解释); 0 表示未初始化.
    pub pwm: AtomicU64,
    /// 进程状态 (`ProcessState` 的 u32 编码).
    pub state: AtomicU32,
    /// 进程优先级 (`ProcessPriority` 的 u32 编码).
    pub priority: AtomicU32,
    /// 进程标志位 (`ProcessFlags` 位掩码).
    pub flags: AtomicU32,

    /// 进程名 (受锁保护, 可在运行时改写).
    pub name: Mutex<String>,
    /// 所属线程组 (K-01): `tgid`、成员账、父/子进程账与进程组身份的权威.
    ///
    /// 单线程进程由 [`Process::new`] 自建单成员组 (`tgid == pid`); 组内线程共享
    /// 同一个 `Arc`, 最后一个 TCB 释放时组内存随之回收.
    pub thread_group: Arc<ThreadGroup>,

    /// 保存/恢复的 CPU 上下文 (寄存器快照, 受锁保护).
    pub context: Mutex<ProcessContext>,
    /// 用户页表根 (x86_64 `CR3` / aarch64 `TTBR0`); 0 表示未分配.
    pub cr3: AtomicU64,
    /// 进程地址空间描述符指针 ([`MmStruct`]); null 表示尚未挂载.
    ///
    /// 缺页与 VMA 系统调用经 `mm::api::vma_get_current_mm` 从**当前任务**读取本字段,
    /// 不再有跨进程共用的全局槽; fork/clone 按指针继承 (线程与 fork 子进程共享同一
    /// `MmStruct`, 与页表 COW 克隆分层). `MmStruct` 目前无释放路径, 由创建方长期持有.
    pub mm: AtomicPtr<MmStruct>,
    /// 内核栈顶高半区虚拟地址; 0 表示未分配.
    pub kernel_stack: AtomicU64,
    /// 内核栈占用页数; 0 表示未分配. 分配时写入, `Process::drop` 据此归还.
    ///
    /// 用户 TCB 用 `USER_KSTACK_SIZE` (16 KiB), 内核线程 (idle) 用
    /// `KERNEL_STACK_SIZE` (64 KiB); 两条分配路径的尺寸不同, 释放必须以
    /// 分配时记录的页数为准.
    pub kstack_pages: AtomicU32,
    /// 用户栈顶虚拟地址 (fork/exec 初始化时使用).
    pub user_stack: AtomicU64,

    /// 退出码 (由 `wait4` 报告给父进程).
    pub exit_code: AtomicU32,
    /// 累计 CPU 时间 (tick).
    pub cpu_time: AtomicU64,

    /// POSIX `times()` 报告的用户态 CPU 时间 (ticks)
    pub user_time: AtomicU64,
    /// POSIX `times()` 报告的内核态 CPU 时间 (ticks)
    pub sys_time: AtomicU64,
    /// 进程累积运行 tick 计数 (由调度器每 tick 增加)
    pub tick_count: AtomicU64,

    /// 阻塞原因 (`BlockReason` 编码), 仅在 `Blocked` 状态有意义.
    pub block_reason: AtomicU32,

    /// 调度策略 (`SchedPolicy` 编码).
    pub sched_policy: AtomicU32,
    /// 实时调度优先级 (仅实时策略使用).
    pub rt_priority: AtomicU32,

    /// EEVDF nice 值, 取值 `-20..=19`; 以 [`crate::privileged::glue::u32_from_i8`]
    /// 的符号扩展位模式存储 (读写必须与 [`crate::privileged::glue::low_i8_from_u32`] 配对).
    pub nice: AtomicU32,
    /// EEVDF 虚拟运行时间; 与运行队列共用同一时基 (不变式 IC1).
    pub eevdf_vruntime: AtomicU64,
    /// EEVDF 调度权重, 由 nice 派生.
    pub eevdf_weight: AtomicU64,
    /// 累计实际运行时间 (供 schedstat 统计).
    pub eevdf_sum_exec_runtime: AtomicU64,
    /// 可运行记账凭证: true 表示本任务已记入某核 `eevdf_rq` (含正上核运行期间).
    pub eevdf_on_rq: AtomicBool,
    /// 本任务可运行记账所在的核 `eevdf_rq` 索引; 不在任何队列上时为 [`NO_RQ_CPU`].
    ///
    /// 与 `eevdf_on_rq` 同生命周期: `eevdf_on_rq == true` 时本字段必为合法 CPU 索引,
    /// 且任务属于 `per_cpu_for(该索引).eevdf_rq` 的可运行集 (树上, 或正被该核拣出在
    /// 跑 —— 语义同 Linux 的 `task_cpu(p)` 配合 `on_rq`).
    pub eevdf_rq_cpu: AtomicU32,
    /// 投送屏障: 本任务正**占用某个核** (已上核运行, 或其上下文尚未保存完成),
    /// 不得被任何搬动者投进运行队列.
    ///
    /// 与 [`Process::eevdf_on_rq`] 正交, 各自回答一个问题: `eevdf_on_rq` = "在不在可运行
    /// 记账里", 本字段 = "占不占着某个核". 两者缺一不可 —— 旧实现只能按归属核索引
    /// ([`Process::eevdf_rq_cpu`]) 查"在核上", 而 `Scheduler::block` 在任务**仍是本核
    /// `current`** 时就把归属清成 [`NO_RQ_CPU`], 于是 `Scheduler::place_locked` 的在核
    /// 判据看不见它, 远核唤醒得以把一个仍在跑的任务投进树, 随后又被同一核的 prev 收尾
    /// 摘走 (P-17 取证的单一成因窗口, 现场与读数见
    /// `docs/report/net-e2e-offcore-dequeue-window.md`).
    ///
    /// 生命周期: `schedule()` 拣中 `next` 并改写 `per_cpu.current` **之前**置位;
    /// 首个用户进程不经 `schedule()` 上核 (iretq 直入用户态), 由
    /// `enter_user_process` 在进入前补置 (见其 P-21 注释). 清位只在
    /// `Scheduler::reap_off_cpu` —— 那里运行在**新任务**上下文, 确证上一次切换已
    /// 完成 (语义同 Linux `finish_task_switch` 的 `prev->on_cpu = 0`). 不得提前清:
    /// 上下文未保存完就允许别核投送 = 两个核交叠使用同一块进程内核栈.
    pub eevdf_on_cpu: AtomicBool,
    /// 本任务是否**曾经**进入过 EEVDF 可运行记账 (入树 / 被拣上核).
    ///
    /// 与 [`Process::eevdf_on_rq`] 不同, 置位后不再清除: 它界定 tick「Ready
    /// 隐形」恢复的射程 —— 恢复只对"曾在队列系统中的任务"生效. 从未入过队的
    /// `Ready` 任务是创建方的责任, **不是**丢失: 首个用户进程按
    /// `enter_user_process` 的契约不入树 (它经 iretq 直入用户态, 入树即成为
    /// 可被别核偷走的幽灵条目), 由 `schedule()` 的 keep-current 兜底, 首次被
    /// 抢占时才经 prev 收尾入树 (P-78 实测: 恢复网若不设此门, 启动期会把该
    /// 进程补投到次核, 两核交叠同一内核栈 ⇒ 三重故障).
    ///
    /// 置位点: 入树/拣中的 [`Self::eevdf_on_rq`] 写入处, 以及
    /// `Scheduler::reap_off_cpu` 确证 prev 已离核处 (补投腿认领失败时由 tick
    /// 恢复网接手, 不把"上过核但从未入队"的首进程漏在网外).
    pub eevdf_placed_once: AtomicBool,

    /// EEVDF 本轮虚拟截止期 (Q16.16); 0 表示尚未建账.
    pub eevdf_vdeadline: AtomicU64,
    /// EEVDF 本轮真实切片 (tick); 0 表示尚未建账.
    pub eevdf_slice_real: AtomicU64,
    /// EEVDF 本轮虚拟切片 (Q16.16).
    pub eevdf_slice_virt: AtomicU64,
    /// EEVDF 本轮剩余真实切片 (tick).
    pub eevdf_slice_left: AtomicU64,
    /// EEVDF 请求切片 (tick); 0 表示未请求, 在下次建账 (Wake/Requeue) 生效 (D8③).
    pub eevdf_requested_slice: AtomicU64,
    /// 最近一次 Wake 入队的内核时间戳 (ns); 0 表示无待测唤醒.
    ///
    /// K-07b 批3 观测面字段: Wake 入队成功后写入, 首次被 `pick_eevdf_task` 拣出时
    /// `swap(0)` 并计入该核的等待时延统计. 不参与任何调度决策.
    pub eevdf_wake_ns: AtomicU64,

    /// 截止时间调度 (`SCHED_DEADLINE`) 的每次激活运行时间预算 (纳秒).
    pub dl_runtime: AtomicU64,
    /// 截止时间调度相对截止期 (纳秒).
    pub dl_deadline: AtomicU64,
    /// 截止时间调度周期 (纳秒).
    pub dl_period: AtomicU64,
    /// 当前激活周期的绝对截止时刻 (tick 时基).
    pub dl_abs: AtomicU64,
    /// 当前周期内剩余可用运行时间 (纳秒).
    pub dl_remaining: AtomicU64,

    /// 本进程的文件描述符表 (K-03: 线程可经 `CLONE_FILES` 共享同一实例).
    pub fd_table: Arc<FdTable>,

    /// CPU 亲和性掩码 (C2 完整实现):
    ///   bit i (i < `MAX_CPUS`) 置位 = 允许在 CPU i 上运行
    ///   默认值 `u64::MAX` (前 64 个 CPU 都允许), 兼容单核
    ///   通过 `sys_sched_setaffinity` 修改
    ///   调度器 `select_cpu` 与跨 CPU 迁移时检查
    pub cpuset_allowed: AtomicU64,

    /// 阻塞睡眠到期时间 (tick 时基, 0 = 无睡眠): 由
    /// `timer::sleep::timer_sleep` 写入, 调度器 tick 的睡眠唤醒扫描消费后清零
    pub sleep_until: AtomicU64,

    /// PCB 引用计数; 归零后才允许实际释放.
    pub ref_count: AtomicU32,
    /// 待释放标志: PCB 已从进程表移除但仍有引用, 由最后一个 `dec_ref` 完成释放.
    pub pending_free: AtomicBool,
    /// 退出中标志 + 自引用锚点 (方案 B: exit/回收竞态根治).
    ///
    /// `exit()` 在把自身置为 `Zombie` **之前** 置位本标志, 并对本 PCB
    /// `try_inc_ref` 持有一次性自引用. 该自引用使父进程 `wait4` 的
    /// `remove_and_free` 只能置 `pending_free` 而不能真正释放 PCB, 从而保证
    /// 本进程在完成最后一次 `schedule()` 上下文切换前 PCB / 内核栈始终有效.
    /// 自引用由 `Scheduler::schedule()` 入口的 `reap_off_cpu` 钩子在确认本
    /// 进程已离开 CPU 后释放并完成实际回收 (否则退出进程返回用户态 = UAF).
    pub exiting: AtomicBool,
    /// 待处理信号位图 (bit i = 信号 i+1).
    pub pending_signals: AtomicU64,

    // --- POSIX 信号处理字段 ---
    /// 信号屏蔽字 (bit i = 信号 i+1 被屏蔽)
    pub blocked_mask: AtomicU64,

    /// 信号处理动作表 (索引 0..=63 对应 SIGHUP(1)..SIGRTMAX(64); B05-34 RT 信号扩展)
    /// 每项: 0 = `SIG_DFL`, 1 = `SIG_IGN`, 其他 = 用户态 handler 地址
    /// K-03: `CLONE_SIGHAND` 的线程共享同一实例.
    pub sigaction_table: Arc<Mutex<[u64; 64]>>,

    /// 进程文件系统上下文 (cwd / 根前缀 / umask, K-03)
    ///
    /// `CLONE_FS` 的线程共享同一实例; fork 与非共享 clone 各自深拷贝.
    pub fs_context: Arc<FsContext>,

    /// 信号替换栈 (sigaltstack), 0 = 未设置
    pub sigaltstack_addr: AtomicU64,
    /// 信号替换栈大小 (字节).
    pub sigaltstack_size: AtomicU64,
    /// 信号替换栈标志 (`SS_ONSTACK` / `SS_DISABLE`).
    pub sigaltstack_flags: AtomicU32,

    /// Per-process 资源限制表 (RLIMIT_*)
    pub rlimit_table: Mutex<RlimitTable>,

    /// Per-process 8 字节 stack canary (P1 #14)
    ///
    /// 低字节恒为 0 (Linux/glibc 兼容). 用户态编译器
    /// (`-fstack-protector`) 在 prologue 写入, epilogue 验证.
    /// 进程创建时由 [`crate::privileged::proc::canary::generate_canary`]
    /// 初始化, fork 继承父进程.
    pub stack_canary: AtomicU64,

    /// Per-process Seccomp 状态 (C7)
    ///
    /// 包含模式 (Disabled/Strict/Filter) + 过滤器链 + `no_new_privs` 位.
    /// fork 继承全部过滤器; execve 保留.
    pub seccomp: crate::privileged::proc::SeccompState,

    /// Per-process SGEG 域级行为门控标志 (分册 9 批次 4)
    ///
    /// `DomainFlags` 位掩码, 默认 0 (无门控). 在 syscall 咽喉点
    /// (`syscall_dispatch_impl`) 于 seccomp 检查之后裁决; fork 继承全部标志;
    /// execve 保留 (与 seccomp 同口径).
    pub domain_flags: AtomicU32,

    /// Per-process Namespace 集合 (D1)
    ///
    /// 包含 UTS/IPC/PID/Mount/User/Net/Cgroup 七种 namespace.
    /// fork 默认共享 (`Arc::clone`), `CLONE_NEW`* 创建新实例.
    /// 通过 `sys_unshare` / `sys_setns` 运行时切换.
    pub namespaces: Mutex<crate::privileged::proc::NamespaceSet>,

    /// Per-process cgroup ID (D2)
    ///
    /// 进程所属 cgroup 的 ID, 默认 0 (根 cgroup).
    /// fork 继承父进程的 cgroup; 可通过 `sys_cgroup_attach` 迁移.
    pub cgroup_id: AtomicU64,

    /// Per-process NUMA 内存策略 (D3)
    ///
    /// 控制进程的内存分配节点选择策略.
    /// fork 继承父进程策略; 可通过 `sys_set_mempolicy` 修改.
    pub numa_policy: Mutex<crate::privileged::mm::numa::NumaMempolicy>,

    /// Per-process 凭证会话上下文 (P2-I-30)
    ///
    /// 替代 `sgeg::session` 中 `static GLOBAL_SESSION` 的全局 `UnsafeCell` 单例.
    /// 每个进程拥有独立的 `PwmContext` (`uid/gid/euid/egid/saved_euid/saved_egid`/
    /// `domain/elevation_granted_pwm`). 在 SMP 下, 不同 CPU 上不同进程的会话
    /// 上下文天然隔离, 杜绝身份/权限串台.
    /// 进程退出时该字段随 Process 一起释放, 自动回收.
    pub session: Mutex<crate::privileged::sgeg::types::PwmContext>,

    /// Per-process SUID 提权栈 (P2-I-30)
    ///
    /// `try_setuid` / `elevate_for_suid` 推送 `PwmContext` 快照;
    /// `drop_elevation` 弹出. 栈深上限 8, 与原 `SessionManager` 保持一致.
    pub session_elev_stack: Mutex<[crate::privileged::sgeg::types::PwmContext; 8]>,

    /// Per-process SUID 提权栈深度 (P2-I-30)
    pub session_elev_depth: AtomicIsize,

    /// Per-process TLS 基址 (`x86_64`: `IA32_FS_BASE`, aarch64: `TPIDR_EL0`)
    ///
    /// 权威来源: `arch_prctl(ARCH_SET_FS)` / `clone(CLONE_SETTLS)` 写入,
    /// `fork` / 未指定 `CLONE_SETTLS` 的 `clone` 从父进程继承, `execve` 清零.
    /// 调度切换与用户态入口按本字段恢复硬件寄存器 (x86_64 在 `mov fs` 之后写
    /// MSR). 0 表示未设置.
    pub tls_base: AtomicU64,

    /// `CLONE_CHILD_CLEARTID` 登记的清除地址
    ///
    /// 进程退出时向该用户地址写 0 并对其实施 futex 唤醒.
    /// 0 表示未设置.
    pub clear_child_tid: AtomicU64,

    /// `set_robust_list` 登记的 robust list 头指针 (用户地址)
    ///
    /// 进程退出时遍历该链表, 对本进程持有的 futex 置 `FUTEX_OWNER_DIED`
    /// 并唤醒等待者. 0 表示未登记.
    pub robust_head: AtomicU64,

    /// robust list 头结构长度 (字节, 合法值 24)
    pub robust_len: AtomicU32,
}

// ✅ P0-5 修复: 添加详细的安全性不变性注释
//
// # Safety (Send)
// Process 可以安全地在线程间转移所有权, 因为:
// 1. 所有可变状态都通过 Mutex 或 AtomicX 保护
// 2. Mutex<String> 和 Mutex<Vec> 内部使用 spin::Mutex, 它实现了 Send
// 3. 原始指针字段 (cr3, kernel_stack, user_stack) 只通过原子操作访问
// 4. 不存在悬垂指针或数据竞争的风险
//
// # Safety (Sync)
// Process 可以安全地被多个线程共享引用 (&Process), 因为:
// 1. name, thread_group, context 等复合类型都被 Mutex / 原子量包装
//    - 访问这些字段必须先获取锁, 保证互斥
// 2. pid, pwm, state 等简单字段都是 Atomic 类型
//    - 使用 Ordering::SeqCst 或 Acquire/Release 保证可见性
// 3. 不存在内部可变性导致的未同步修改
// 4. 调度器在切换进程时通过 scheduler_lock 保护整个 ProcessTable
//
// 所有字段 (Mutex<T>, Atomic*, u32, u64, bool 等) 自动 Send+Sync.

impl Process {
    /// 创建指定 PID、名称与父进程组的进程控制块, 其余字段取默认值.
    #[must_use]
    pub fn new(pid: Pid, name: &str, parent: Option<ProcessId>) -> Self {
        Self {
            pid: ProcessId(pid),
            pwm: AtomicU64::new(0),
            state: AtomicU32::new(ProcessState::Created as u32),
            priority: AtomicU32::new(ProcessPriority::Normal as u32),
            flags: AtomicU32::new(0),
            name: Mutex::new(String::from(name)),
            thread_group: {
                let group = ThreadGroup::new_leader(pid);
                if let Some(parent) = parent {
                    group.set_parent(parent.0);
                }
                Arc::new(group)
            },
            context: Mutex::new(ProcessContext::new()),
            cr3: AtomicU64::new(0),
            mm: AtomicPtr::new(core::ptr::null_mut()),
            kernel_stack: AtomicU64::new(0),
            kstack_pages: AtomicU32::new(0),
            user_stack: AtomicU64::new(0),
            exit_code: AtomicU32::new(0),
            cpu_time: AtomicU64::new(0),
            user_time: AtomicU64::new(0),
            sys_time: AtomicU64::new(0),
            tick_count: AtomicU64::new(0),
            block_reason: AtomicU32::new(BlockReason::Unknown as u32),
            sched_policy: AtomicU32::new(SchedPolicy::Normal as u32),
            rt_priority: AtomicU32::new(0),
            nice: AtomicU32::new(0),
            eevdf_vruntime: AtomicU64::new(0),
            eevdf_weight: AtomicU64::new(super::eevdf::NICE0_WEIGHT),
            eevdf_sum_exec_runtime: AtomicU64::new(0),
            eevdf_on_rq: AtomicBool::new(false),
            eevdf_rq_cpu: AtomicU32::new(super::process::NO_RQ_CPU),
            eevdf_on_cpu: AtomicBool::new(false),
            eevdf_placed_once: AtomicBool::new(false),
            eevdf_vdeadline: AtomicU64::new(0),
            eevdf_slice_real: AtomicU64::new(0),
            eevdf_slice_virt: AtomicU64::new(0),
            eevdf_slice_left: AtomicU64::new(0),
            eevdf_requested_slice: AtomicU64::new(0),
            eevdf_wake_ns: AtomicU64::new(0),
            dl_runtime: AtomicU64::new(0),
            dl_deadline: AtomicU64::new(0),
            dl_period: AtomicU64::new(0),
            dl_abs: AtomicU64::new(0),
            dl_remaining: AtomicU64::new(0),
            fd_table: Arc::new(FdTable::new()),
            // C2 CPU 亲和性: 默认所有 CPU (前 64 个) 都允许
            cpuset_allowed: AtomicU64::new(u64::MAX),
            sleep_until: AtomicU64::new(0),
            ref_count: AtomicU32::new(1),
            pending_free: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            pending_signals: AtomicU64::new(0),
            blocked_mask: AtomicU64::new(0),
            sigaction_table: Arc::new(Mutex::new([0u64; 64])),
            fs_context: Arc::new(FsContext::new_default()),
            sigaltstack_addr: AtomicU64::new(0),
            sigaltstack_size: AtomicU64::new(0),
            sigaltstack_flags: AtomicU32::new(0),
            rlimit_table: Mutex::new(RlimitTable::new()),
            // P1 #14: 进程创建时分配独立 canary
            stack_canary: AtomicU64::new(crate::privileged::proc::generate_canary()),
            // C7: Seccomp 默认 Disabled
            seccomp: crate::privileged::proc::SeccompState::new(),
            // 分册 9 批次 4: 域级行为门控默认无标志 (不改变任何调用行为)
            domain_flags: AtomicU32::new(0),
            // D1: Namespace 默认 init namespace 集合
            namespaces: Mutex::new(crate::privileged::proc::NamespaceSet::new_init()),
            // D2: cgroup 默认根 cgroup (id=0)
            cgroup_id: AtomicU64::new(0),
            // D3: NUMA 策略默认 Default
            numa_policy: Mutex::new(crate::privileged::mm::numa::NumaMempolicy::new()),
            // P2-I-30: 进程级凭证会话上下文 (uid/gid/euid/egid/saved_*/domain)
            session: Mutex::new(crate::privileged::sgeg::types::PwmContext::default()),
            // P2-I-30: SUID 提权栈 (深度 0, 容量 8)
            session_elev_stack: Mutex::new(
                [crate::privileged::sgeg::types::PwmContext::default(); 8],
            ),
            session_elev_depth: AtomicIsize::new(0),
            tls_base: AtomicU64::new(0),
            clear_child_tid: AtomicU64::new(0),
            robust_head: AtomicU64::new(0),
            robust_len: AtomicU32::new(0),
        }
    }

    /// 从物理页分配器取一段内核栈并写入栈顶 canary; 分配失败返回 false.
    ///
    /// 内核线程 (idle) 路径, 栈尺寸为 `KERNEL_STACK_SIZE`.
    pub fn allocate_kernel_stack(&self) -> bool {
        self.allocate_kernel_stack_sized(KERNEL_STACK_SIZE)
    }

    /// 为用户 TCB (fork / clone 产物) 分配内核栈, 尺寸为 `USER_KSTACK_SIZE`.
    ///
    /// 用户 TCB 的栈尺寸必须与 `UserProcManager::create` 的创建路径一致 ——
    /// 后者按 `USER_KSTACK_SIZE` 分配, 而 fork/clone 的内核栈复制也以该尺寸为准.
    pub fn allocate_user_kernel_stack(&self) -> bool {
        self.allocate_kernel_stack_sized(crate::privileged::glue::usize_from_u64(USER_KSTACK_SIZE))
    }

    /// 按给定字节数分配内核栈; 记录页数供 [`Drop`] 单点归还.
    fn allocate_kernel_stack_sized(&self, size: usize) -> bool {
        let pages = size / crate::privileged::glue::usize_from_u64(PAGE_SIZE);
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let stack = pmm_alloc_pages(pages as u64);
            if stack.is_null() {
                return false;
            }
            // 将物理地址转换为高半段虚拟地址, 以便在用户页表加载时
            // 仍能访问 TSS RSP0.
            let stack_top = stack as u64 + KERNEL_BASE + size as u64;
            self.kernel_stack.store(stack_top, Ordering::SeqCst);
            self.kstack_pages.store(
                crate::privileged::glue::low_u32_from_usize(pages),
                Ordering::SeqCst,
            );
            kernel_stack_write_canary(stack_top);
            true
        }
    }

    /// 初始化内核态 idle 任务首次被调度时的执行上下文 (x86_64).
    ///
    /// idle 是 `cs = SELECTOR_KERNEL_CODE` (0x08) 的**内核线程**: 首次切入由
    /// `process_switch_asm` 的内核线程分支 (`mov rsp, [rsi+64]; jmp [rsi+56]`)
    /// 完成, **不经过 iretq** (同特权级 iretq 只弹 RIP/CS/RFLAGS, 不加载 RSP/SS
    /// —— x86 SDM, 无法完成内核线程的栈切换, 见 `proc/switch.asm`).
    ///
    /// 因此 `rsp` 必须是可直接使用的真实栈地址: 取 `allocate_kernel_stack()`
    /// 存下的**高半区**栈顶 (phys + `KERNEL_BASE` + `KERNEL_STACK_SIZE`). 该地址与
    /// TSS RSP0 (用户态中断入口栈, 见 `allocate_kernel_stack` 与
    /// `tss_set_kernel_stack`) 是同一个地址, 内核在直接映射区跑栈是既定约定;
    /// 内核线程的 save/restore 对称, 不再有 `add rsp, KERNEL_BASE` 别名修正.
    ///
    /// `cr3` 取内核页表, 保证 `entry` (低 LMA 代码地址) 与高半区内核栈均可寻址.
    #[cfg(target_arch = "x86_64")]
    pub fn init_kernel_idle_context(&self, entry: u64, cr3: u64) {
        let stack_top = self.kernel_stack.load(Ordering::SeqCst);
        if stack_top == 0 {
            return;
        }
        // 栈向下增长: 从高半区栈顶往下留一个字, 并保证 16 字节对齐.
        let sp = (stack_top - 16) & !0xF;

        let mut ctx = self.context.lock();
        ctx.rip = entry;
        ctx.rsp = sp;
        ctx.rflags = 0x202;
        ctx.cr3 = cr3;
        ctx.cs = u64::from(crate::privileged::arch::gdt::SELECTOR_KERNEL_CODE);
        ctx.ss = u64::from(crate::privileged::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.ds = u64::from(crate::privileged::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.es = u64::from(crate::privileged::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.fs = u64::from(crate::privileged::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.gs = u64::from(crate::privileged::arch::gdt::SELECTOR_KERNEL_DATA);
    }

    /// 初始化内核态 idle 任务首次被调度时的执行上下文 (aarch64).
    ///
    /// aarch64 复用 `ProcessContext` 的字段偏移但语义不同 (见
    /// `privileged/arch/aarch64/context.rs`): `ds`(96) = SP, `es`(104) = `TTBR0_EL1`,
    /// `fs`(112) = `SPSR_EL1`, `gs`(120) = `ELR_EL1`. `context_switch_asm` 末尾以
    /// `eret` 恢复 SPSR/ELR, 因此把入口写入 ELR、栈顶写入 SP 即可跳入 idle.
    ///
    /// aarch64 内核栈链接于高半区, `allocate_kernel_stack` 已把栈顶转为高半区别名
    /// (TTBR1 在内核页表中恒覆盖高半区), 此处直接写入 SP, 无需再转换.
    #[cfg(target_arch = "aarch64")]
    // SIMPLIFIED: SPSR_EL1 固定取 EL1h (M[3:0]=0b0101) 且 DAIF 清 0 (中断使能),
    // 未区分异常级别与 mask 状态; 影响面: 仅覆盖"内核态 idle 且需响应中断"这一
    // 场景, 未在 aarch64 实机验证; 何时需扩展: aarch64 上线时按实际异常级别/栈布局复核.
    pub fn init_kernel_idle_context(&self, entry: u64, cr3: u64) {
        let stack_top = self.kernel_stack.load(Ordering::SeqCst);
        if stack_top == 0 {
            return;
        }

        let mut ctx = self.context.lock();
        ctx.ds = stack_top & !0xF; // SP
        ctx.es = cr3; // TTBR0_EL1
        ctx.fs = 0x5; // SPSR_EL1: EL1h + 中断使能
        ctx.gs = entry; // ELR_EL1
    }

    /// 创建本进程独立的用户页表并记入 `cr3`; 创建失败返回 false.
    pub fn allocate_user_space(&self) -> bool {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let cr3 = vmm_create_user_page_table();
            if cr3 == 0 {
                return false;
            }
            self.cr3.store(cr3, Ordering::SeqCst);
            true
        }
    }

    /// 读取当前进程状态.
    pub fn get_state(&self) -> ProcessState {
        ProcessState::from_u8(crate::privileged::glue::low_u8_from_u32(
            self.state.load(Ordering::SeqCst),
        ))
    }

    /// ✅ 安全的状态设置 (带合法性检查和审计日志)
    ///
    /// # Arguments
    /// * `new_state` - 目标新状态
    ///
    /// # Returns
    /// * `Ok(())` - 状态转换成功
    /// * `Err(&str)` - 非法状态转换
    ///
    /// # Errors
    /// 当请求的 `new_state` 不属于状态机允许的合法转换时, 返回
    /// `Err("Illegal process state transition")`, 且不会修改任何状态.
    pub fn set_state_safe(&self, new_state: ProcessState) -> Result<(), &'static str> {
        let current = self.get_state();

        // ✅ 状态机合法性检查 (防止非法转换)
        // 允许的正常转换: 创建→就绪, 就绪↔运行, 运行→阻塞/僵尸/冻结, 就绪/阻塞→冻结,
        // 阻塞→就绪/僵尸, 僵尸→终止, 冻结→就绪/阻塞 (thaw 后仍需等待)
        match (current, new_state) {
            // 创建→就绪
            (ProcessState::Created, ProcessState::Ready)
            // 就绪→运行/冻结
            | (ProcessState::Ready, ProcessState::Running | ProcessState::Frozen)
            // 运行→就绪/阻塞/僵尸/冻结
            | (
                ProcessState::Running,
                ProcessState::Ready
                | ProcessState::Blocked
                | ProcessState::Zombie
                | ProcessState::Frozen,
            )
            // 阻塞→就绪/僵尸/冻结
            | (
                ProcessState::Blocked,
                ProcessState::Ready | ProcessState::Zombie | ProcessState::Frozen,
            )
            // 僵尸→终止
            | (ProcessState::Zombie, ProcessState::Terminated)
            // 冻结→就绪/阻塞
            | (ProcessState::Frozen, ProcessState::Ready | ProcessState::Blocked) => {}

            // ❌ 禁止的非法转换
            _ => return Err("Illegal process state transition"),
        }

        // 执行状态转换
        self.state.store(new_state as u32, Ordering::Release);

        Ok(())
    }

    /// 旧版兼容接口 (内部使用, 不建议新代码使用)
    #[deprecated(note = "Use set_state_safe() for state transitions with validation")]
    pub fn set_state(&self, state: ProcessState) {
        // 兼容旧代码, 但记录警告
        let _ = self.set_state_safe(state);
    }

    /// 读取进程优先级.
    pub fn get_priority(&self) -> ProcessPriority {
        ProcessPriority::from_u32(self.priority.load(Ordering::SeqCst))
    }

    /// 读取线程组 ID (tgid); 单线程进程等于本 TCB 的 `pid`.
    pub fn tgid(&self) -> Pid {
        self.thread_group.tgid()
    }

    /// 读取所属线程组 (共享身份与成员账).
    #[must_use]
    pub fn group(&self) -> &Arc<ThreadGroup> {
        &self.thread_group
    }

    /// 加入既有线程组 (K-02): 登记本 TCB 为成员并共享组身份.
    ///
    /// 替换 `Process::new` 自建的单线程组; 该新 TCB 的 `pid` 即组内 TID,
    /// `tgid` 取自目标组的组长. 返回 `false` 表示该组正在 `exec` `de_thread`
    /// 或已发起组退出 (`exit_group` / 致命信号), 本 TCB 未入组
    /// (调用方须回滚, 不得继续登记进程表).
    #[must_use]
    pub fn join_thread_group(&mut self, group: Arc<ThreadGroup>) -> bool {
        if !group.join(self.pid.0) {
            return false;
        }
        self.thread_group = group;
        true
    }

    // ========================================================================
    // 组共享资源接管 (K-03)
    // ========================================================================
    //
    // 每个字段二选一: 共享 (`CLONE_*` 置位, 接管父线程的同一 `Arc`) 或复制
    // (fork / 未置位, 深拷贝为独立实例). `Process::new` 先给出独立默认实例,
    // 接线方在子 TCB 登记成功后才改写 —— 登记失败时 PCB 自带实例随 Drop 释放,
    // 无需回滚共享计数.

    /// 接管父线程的 fd 表 (`CLONE_FILES`): 递增共享计数后共享同一实例.
    pub fn share_fd_table(&mut self, table: &Arc<FdTable>) {
        table.add_user();
        self.fd_table = Arc::clone(table);
    }

    /// 复制父进程 fd 表映射 (fork / 未置 `CLONE_FILES`).
    ///
    /// `on_handle` 在源表锁内逐句柄回调, 调用方负责增加底层 `OpenFile` 引用.
    pub fn copy_fd_table_from<F>(&mut self, src: &FdTable, on_handle: F)
    where
        F: FnMut(u32),
    {
        self.fd_table.copy_from(src, on_handle);
    }

    /// 共享父线程的信号处理动作表 (`CLONE_SIGHAND`).
    pub fn share_signal_actions(&mut self, table: &Arc<Mutex<[u64; 64]>>) {
        self.sigaction_table = Arc::clone(table);
    }

    /// 复制信号处理动作表 (fork / 未置 `CLONE_SIGHAND`).
    ///
    /// 先快照源表再写本表, 不在同一临界区嵌套两把同层锁.
    pub fn copy_signal_actions_from(&mut self, src: &Mutex<[u64; 64]>) {
        let snapshot = *src.lock();
        *self.sigaction_table.lock() = snapshot;
    }

    /// 共享父线程的文件系统上下文 (`CLONE_FS`).
    pub fn share_fs_context(&mut self, ctx: &Arc<FsContext>) {
        self.fs_context = Arc::clone(ctx);
    }

    /// 复制文件系统上下文 (fork / 未置 `CLONE_FS`).
    pub fn copy_fs_context_from(&mut self, src: &FsContext) {
        self.fs_context.copy_from(src);
    }

    /// 设置进程优先级.
    pub fn set_priority(&self, priority: ProcessPriority) {
        self.priority.store(priority as u32, Ordering::SeqCst);
    }

    /// 读取 EEVDF nice 值 (`-20..=19`).
    pub fn get_nice(&self) -> i8 {
        crate::privileged::glue::low_i8_from_u32(self.nice.load(Ordering::Acquire))
    }

    /// 是否为内核线程 (`ProcessFlags::IS_KERNEL`).
    pub fn is_kernel(&self) -> bool {
        let flags = self.flags.load(Ordering::SeqCst);
        (flags & ProcessFlags::IS_KERNEL.bits()) != 0
    }

    /// 设置或清除内核线程标志.
    pub fn set_kernel(&self, is_kernel: bool) {
        let mut flags = self.flags.load(Ordering::SeqCst);
        if is_kernel {
            flags |= ProcessFlags::IS_KERNEL.bits();
        } else {
            flags &= !ProcessFlags::IS_KERNEL.bits();
        }
        self.flags.store(flags, Ordering::SeqCst);
    }

    /// 读取调度策略.
    pub fn get_sched_policy(&self) -> SchedPolicy {
        SchedPolicy::from_u32(self.sched_policy.load(Ordering::SeqCst))
    }

    /// 设置调度策略.
    pub fn set_sched_policy(&self, policy: SchedPolicy) {
        self.sched_policy.store(policy as u32, Ordering::SeqCst);
    }

    /// 读取实时调度优先级.
    pub fn get_rt_priority(&self) -> u8 {
        crate::privileged::glue::low_u8_from_u32(self.rt_priority.load(Ordering::SeqCst))
    }

    /// 设置实时调度优先级.
    pub fn set_rt_priority(&self, priority: u8) {
        self.rt_priority
            .store(u32::from(priority), Ordering::SeqCst);
    }

    /// 读取进程 PWM 身份/权限字.
    pub fn get_pwm(&self) -> u64 {
        self.pwm.load(Ordering::SeqCst)
    }

    /// 设置进程 PWM 身份/权限字.
    pub fn set_pwm(&self, pwm: u64) {
        self.pwm.store(pwm, Ordering::SeqCst);
    }

    /// 尝试增加引用计数; 计数已归零时失败返回 false (阻止复活已释放的 PCB).
    pub fn try_inc_ref(&self) -> bool {
        self.ref_count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                if v > 0 { Some(v + 1) } else { None }
            })
            .is_ok()
    }

    /// 减少引用计数, 返回递减后的值.
    pub fn dec_ref(&self) -> u32 {
        self.ref_count.fetch_sub(1, Ordering::AcqRel) - 1
    }

    /// 置位指定信号的待处理位.
    pub fn signal_pending_set(&self, sig: u32) {
        self.pending_signals
            .fetch_or(1u64 << sig, Ordering::Release);
    }

    /// 读取待处理信号位图.
    pub fn signal_pending_get(&self) -> u64 {
        self.pending_signals.load(Ordering::Acquire)
    }

    /// 清除 `mask` 中置位的待处理信号.
    pub fn signal_pending_clear(&self, mask: u64) {
        self.pending_signals.fetch_and(!mask, Ordering::Release);
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let cr3 = self.cr3.load(Ordering::SeqCst);
        if cr3 != 0 {
            // A-8: 这是销毁路径**唯一**的 cr3 减计点 —— UserProc 镜像 destroy
            // 不再对 cr3 减计; 两处各减一次会在两次调用之间让帧被重新分配,
            // 第二次递减抹掉新持有者的计数并误销毁在用页表.
            // 计数归零才销毁: cr3 可能仍被 CLONE_VM 的兄弟进程持有, 无条件销毁
            // 会使对方在用的页表被回收 (UAF). `frame_dec` 对未计数帧 (计数 0)
            // 返回 false, 同时防止同一 PML4 被二次销毁.
            if crate::privileged::mm::pmm::get_pmm().frame_dec(crate::privileged::mm::PhysAddr(cr3))
            {
                // SAFETY: cr3 由 vmm_create_user_page_table / COW 克隆产生,
                // 且计数已归零 (无其他持有者), 本进程是最后持有者.
                unsafe {
                    vmm_destroy_page_table(cr3);
                }
            }
        }
        // K-06: 内核栈所有权单点归还. 用户 TCB (`USER_KSTACK_SIZE`) 与内核线程
        // (`KERNEL_STACK_SIZE`) 尺寸不同, 释放页数取分配时记录值, 不能按固定
        // 常量推断 —— 线程 TCB 没有 `UserProcess` 镜像, 此处是唯一的归还点.
        let kstack = self.kernel_stack.load(Ordering::SeqCst);
        let pages = self.kstack_pages.load(Ordering::Acquire);
        if kstack != 0 && pages != 0 {
            let base_phys = kstack - u64::from(pages) * PAGE_SIZE - KERNEL_BASE;
            crate::privileged::mm::pmm_free_pages_phys(
                crate::privileged::mm::PhysAddr(base_phys),
                crate::privileged::glue::usize_from_u64(u64::from(pages)),
            );
        }
    }
}

/// 全局进程表 — 以定长数组保存进程槽位, 并用 PID 位图管理编号分配与回收。
#[repr(C)]
pub struct ProcessTable {
    processes: Mutex<[Option<NonNull<Process>>; MAX_PROCESSES]>,
    /// PID 位图: true = 已分配, false = 空闲
    /// PID 0 (idle) 和 PID 1 (init) 在初始化时标记为已用
    pid_bitmap: Mutex<[bool; MAX_PROCESSES]>,
    /// 下次搜索起点 (环形扫描, 避免 O(n) 从头扫描)
    next_search: Mutex<u32>,
}

// SAFETY: ProcessTable 始终通过静态 PROCESS_TABLE 访问.
// Process 字段全是 Mutex/Atomic*/普通整数, Process 自动 Send+Sync.
// NonNull<Process> 在 nightly 1.97 不会自动 Send+Sync, 因此显式 impl.
// SAFETY: ProcessTable 含裸指针 HashMap, 但所有变更都通过 Mutex 进行.
unsafe impl Send for ProcessTable {}
// SAFETY: 同上, Mutex 保证并发安全.
unsafe impl Sync for ProcessTable {}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTable {
    /// 构造空进程表: 全部槽位为空, PID 位图清零, 从 PID 2 开始分配.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            processes: Mutex::named_lazy("PROCESS_TABLE.processes", [None; MAX_PROCESSES]),
            pid_bitmap: Mutex::new([false; MAX_PROCESSES]),
            next_search: Mutex::new(2), // 从 PID 2 开始搜索 (0=idle, 1=init 保留)
        }
    }

    /// 分配一个新的 PID (支持回收)
    ///
    /// 从 `next_search` 位置开始环形扫描位图，找到第一个空闲 PID。
    /// 如果位图已满，返回 `None`。
    pub fn allocate_pid(&self) -> Option<Pid> {
        let mut bitmap = self.pid_bitmap.lock();
        let mut search_idx = self.next_search.lock();

        let start = *search_idx as usize;
        let max = MAX_PROCESSES;

        // 环形扫描: 从 start 到 max, 再从 0 到 start
        for offset in 0..max {
            let idx = (start + offset) % max;
            if !bitmap[idx] {
                bitmap[idx] = true;
                // 更新下次搜索起点 (环形递增)
                *search_idx = crate::privileged::glue::low_u32_from_usize((idx + 1) % max);
                return Some(crate::privileged::glue::low_u32_from_usize(idx));
            }
        }

        // 位图已满
        None
    }

    /// 回收 PID (在进程被释放时调用)
    ///
    /// 将 PID 对应的位图位清零，使其可被重新分配。
    pub fn free_pid(&self, pid: Pid) {
        let mut bitmap = self.pid_bitmap.lock();
        let idx = pid as usize;
        if idx < MAX_PROCESSES && bitmap[idx] {
            bitmap[idx] = false;
        }
    }

    /// 按 `process.pid` 把裸指针放入对应槽位; 指针为空或 PID 越界返回 false.
    pub fn insert(&self, process: *mut Process) -> bool {
        // SAFETY: caller guarantees process is a valid non-null pointer.
        let Some(nn) = NonNull::new(process) else {
            return false;
        };
        let mut table = self.processes.lock();
        // SAFETY: nn is a valid non-null pointer.
        let pid = unsafe { nn.as_ref().pid.0 as usize };
        if pid >= MAX_PROCESSES {
            return false;
        }
        table[pid] = Some(nn);
        true
    }

    /// 按 PID 取进程裸指针; 槽位为空或越界返回 `None` (生命周期由调用方负责).
    pub fn get(&self, pid: Pid) -> Option<*mut Process> {
        let table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return None;
        }
        table[pid as usize].map(core::ptr::NonNull::as_ptr)
    }

    /// 在进程表锁保护下以只读引用调用 `f`; 找不到该 PID 返回 `None`.
    pub fn with_process<F, R>(&self, pid: Pid, f: F) -> Option<R>
    where
        F: FnOnce(&Process) -> R,
    {
        let table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return None;
        }
        // SAFETY: nn is a valid NonNull pointer inserted by insert().
        table[pid as usize].map(|nn| unsafe { nn.as_ref() }).map(f)
    }

    /// 在进程表锁保护下以可变引用调用 `f`; 找不到该 PID 返回 `None`.
    pub fn with_process_mut<F, R>(&self, pid: Pid, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return None;
        }
        // SAFETY: nn is a valid NonNull pointer inserted by insert().
        // Mutex 锁保证独占访问.
        table[pid as usize]
            .map(|mut nn| unsafe { nn.as_mut() })
            .map(f)
    }

    /// 从槽位摘除进程并返回其裸指针, 不释放 PCB 内存.
    pub fn remove(&self, pid: Pid) -> Option<*mut Process> {
        let mut table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return None;
        }
        table[pid as usize].take().map(core::ptr::NonNull::as_ptr)
    }

    /// 移除进程并释放 `Box<Process>` 内存
    /// 如果其他线程持有引用 (`ref_count` > 1), 则仅设置 `pending_free` 标志,
    /// 由最后的 `dec_ref_and_maybe_free` 调用完成实际释放。
    /// 全程持有 table lock 以防止与 `dec_ref_and_maybe_free` 竞争。
    pub fn remove_and_free(&self, pid: Pid) {
        crate::privileged::process_cleanup::notify_process_exit(pid);
        let mut table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return;
        }
        if let Some(nn) = table[pid as usize] {
            // SAFETY: nn is a valid NonNull pointer inserted by insert().
            let proc = unsafe { nn.as_ref() };
            proc.pending_free.store(true, Ordering::Release);
            let prev = proc.dec_ref();
            if prev == 0 {
                table[pid as usize] = None;
                drop(table);
                // 回收 UserProc 镜像记录 — 必须先于 Process 析构: 镜像持有指向
                // 本 PCB 的非拥有句柄, 镜像必须先于权威对象消失
                // (INV-USER-PROC #2). cr3 减计/销毁只在下面的 Process::drop (A-8).
                crate::privileged::proc::user_proc::USER_PROC_MANAGER.destroy_by_pid(pid);
                // SAFETY: nn 由 Box::into_raw 分配, 且我们持有唯一引用 (ref_count 归零).
                unsafe {
                    let boxed = Box::from_raw(nn.as_ptr());
                    drop(boxed);
                }
                // M5: 回收 PID
                self.free_pid(pid);
            }
        }
    }

    /// 若 PID 有效则尝试增加其进程引用计数; 无效或计数已归零返回 false.
    pub fn try_inc_ref(&self, pid: Pid) -> bool {
        let table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return false;
        }
        table[pid as usize].is_some_and(|nn| {
            // SAFETY: nn is a valid NonNull pointer inserted by insert().
            let proc_ref = unsafe { nn.as_ref() };
            proc_ref.try_inc_ref()
        })
    }

    /// 减少引用计数; 归零且已置 `pending_free` 时摘槽、回收镜像与 PID 并释放 PCB.
    pub fn dec_ref_and_maybe_free(&self, pid: Pid) {
        let mut table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return;
        }
        if let Some(nn) = table[pid as usize] {
            // SAFETY: nn is a valid NonNull pointer inserted by insert().
            // Mutex 锁保证独占访问.
            let proc = unsafe { nn.as_ref() };
            let prev = proc.dec_ref();
            if prev == 0 && proc.pending_free.load(Ordering::Acquire) {
                table[pid as usize] = None;
                drop(table);
                // 回收 UserProc 镜像记录 (延迟释放分支): 同 remove_and_free,
                // 镜像必须先于权威 PCB 析构 (INV-USER-PROC #2).
                crate::privileged::proc::user_proc::USER_PROC_MANAGER.destroy_by_pid(pid);
                // SAFETY: nn 由 Box::into_raw 分配, 且我们持有唯一引用 (ref_count 归零).
                unsafe {
                    let boxed = Box::from_raw(nn.as_ptr());
                    drop(boxed);
                }
                // M5: 回收 PID
                self.free_pid(pid);
            }
        }
    }

    /// 返回线程组已整体退出且全部 TCB 均已落 Zombie 时的组退出码.
    ///
    /// `None` 表示尚不可收割: 组未退出、组长 TCB 已不在表中, 或仍有成员处于
    /// 退出途中 (未落 Zombie). 复用于 `wait4`/`waitid` 的就绪判定与周期回收.
    ///
    /// §6③a: 两个 `None` 出口都在「已请求组退出」时落非僵尸成员取证行 (见
    /// `ProcessTable::diag_unreaped_group`) —— 登记的两类现场都要覆盖: 兄弟线程
    /// 始终没走到退出收尾 (`has_exited` 恒假), 以及整组已收齐却被某个 TCB 卡在非
    /// Zombie (已登记的 L-2「Zombie 被重新投入」形态). **只读诊断**, 本函数的判据
    /// 与返回值逐字未变.
    #[must_use]
    pub fn group_zombie_exit_code(&self, tgid: Pid) -> Option<u32> {
        let group = self.with_process(tgid, |p| Arc::clone(p.group()))?;
        if !group.has_exited() {
            self.diag_unreaped_group(tgid, &group);
            return None;
        }
        let mut all_zombie = true;
        self.for_each(|p| {
            if p.thread_group.tgid() == tgid && p.get_state() != ProcessState::Zombie {
                all_zombie = false;
                return false;
            }
            true
        });
        if all_zombie {
            return Some(group.exit_code());
        }
        self.diag_unreaped_group(tgid, &group);
        None
    }

    /// 列出该线程组尚未落 Zombie 的 TCB 及其状态 (§6③a 取证的成员面).
    ///
    /// 取「按 tgid 扫进程表」而不是「读成员账」: 成员账由 `Scheduler::exit` 的
    /// `leave` 摘除, Zombie 却在其**之后**才落, 且已退出的 TCB 也不再在账里 ——
    /// 整组已收齐 (账空) 却被某个 TCB 卡在非 Zombie 的形态只有表扫描看得见.
    /// 遍历全程持进程表锁, 返回即放锁, 打印由调用方在锁外完成.
    #[must_use]
    pub fn group_non_zombie_members(&self, tgid: Pid) -> alloc::vec::Vec<(Pid, ProcessState)> {
        let mut out = alloc::vec::Vec::new();
        self.for_each(|proc| {
            if proc.thread_group.tgid() == tgid {
                let state = proc.get_state();
                if state != ProcessState::Zombie {
                    out.push((proc.pid.0, state));
                }
            }
            true
        });
        out
    }

    /// 落「组未收齐」取证行: 每个非僵尸成员一行, 同一成员快照只放行一次 (§6③a).
    ///
    /// 判据收窄两处: ①`exit_requested` 为假即静默 (父进程在子组正常存活期的轮询
    /// 是背景噪声, 不是现场); ②签名去重 (`wait4` 每毫秒轮询一次且 tick 兜底也读
    /// 同一判据, 不去重即同一状态每次刷一行). 成员快照为空 (整组已落 Zombie 只是
    /// 无人收割) 同样静默. 打印在进程表锁**之外**完成, 不持表锁取控制台锁.
    fn diag_unreaped_group(&self, tgid: Pid, group: &ThreadGroup) {
        if !group.exit_requested() {
            return;
        }
        let stuck = self.group_non_zombie_members(tgid);
        if stuck.is_empty() {
            return;
        }
        let mut sig = u64::from(tgid);
        for (tid, state) in &stuck {
            sig = sig.wrapping_mul(1_000_003) ^ u64::from(*tid);
            sig = sig.wrapping_mul(1_000_003) ^ u64::from(*state as u32);
        }
        if !group.note_reap_diag(sig) {
            return;
        }
        for (tid, state) in stuck {
            crate::klog_info!(
                Process,
                "[REAP] tgid={} unreaped pid={} state={}",
                tgid,
                tid,
                state.name()
            );
        }
    }

    /// 收割一个已整体退出的线程组: 回收其全部僵尸 TCB 与 TID.
    ///
    /// 返回 `false` 表示本次未回收 (组未退出 / 仍有成员退出途中 / 已被其他
    /// 等待者认领). 只有认领成功者才执行 `remove_and_free` — 同一 TCB 被两方
    /// 回收会重复递减引用计数并提前释放 PCB.
    pub fn reap_zombie_group(&self, tgid: Pid) -> bool {
        let Some(group) = self.with_process(tgid, |p| Arc::clone(p.group())) else {
            return false;
        };
        if !group.has_exited() {
            return false;
        }
        let mut tids: alloc::vec::Vec<Pid> = alloc::vec::Vec::new();
        let mut all_zombie = true;
        self.for_each(|p| {
            if p.thread_group.tgid() == tgid {
                if p.get_state() == ProcessState::Zombie {
                    tids.push(p.pid.0);
                } else {
                    all_zombie = false;
                }
            }
            true
        });
        if !all_zombie || tids.is_empty() || !group.claim_reap() {
            return false;
        }
        // 先从父组子账摘除再释放 TCB: 释放后 tgid 可被复用, 迟到的摘除会误删
        // 同值的"新"子组条目 (父组看到的账是 tgid 列表, 无代号可区分).
        let parent = group.parent();
        if parent != 0 {
            let _ = self.with_process(parent, |p| p.group().remove_child(tgid));
        }
        for tid in tids {
            self.with_process(tid, |p| {
                let _ = p.set_state_safe(ProcessState::Terminated);
            });
            self.remove_and_free(tid);
        }
        true
    }

    /// 一次表锁内按槽位升序遍历在册 TCB (`f` 返回 false 时提前终止).
    ///
    /// 迭代面**唯一**的 `processes` 取锁点: `for_each` 与 `scan_into` 共用本函数,
    /// 避免出现两处独立的"持表锁遍历"形态而各写各的契约.
    fn walk_locked<F: FnMut(&Process) -> bool>(&self, mut f: F) {
        let table = self.processes.lock();
        for nn in table.iter().flatten() {
            // SAFETY: nn 由 insert() 按 process.pid 写入, 指向仍在册的 TCB, 且
            // `processes` 锁保证遍历期间该槽位不被摘除.
            let proc = unsafe { nn.as_ref() };
            if !f(proc) {
                break;
            }
        }
    }

    /// 遍历所有进程 (回调返回 false 时提前终止)
    pub fn for_each<F: FnMut(&Process) -> bool>(&self, f: F) {
        self.walk_locked(f);
    }

    /// 一次表锁内的**有界快照**: 按槽位升序把满足 `pred` 的条目收进 `buf`, 返回实收条数.
    ///
    /// 用途是"锁内收、锁外做": 需要在锁内读的只有判据所需的原子字段, 而后续动作
    /// (取 rq 锁 / 再取表锁 / 分配 / 打印) 全部留在锁外的结果集上做. 与
    /// [`Self::for_each`] 的分工即在于此 —— 后者让回调就地作用于每个在册任务, 本函数
    /// 只把满足判据的条目收进定长缓冲.
    ///
    /// # 谓词契约
    ///
    /// `pred` 在 `processes` 临界区内执行, 必须满足:
    /// - 只读 `Process` 的原子字段与内联标量;
    /// - **不得**回调 `PROCESS_TABLE` / `ProcessTable` 的任何方法 —— `processes`
    ///   是不可重入锁, 持锁闭包内再取即同核自死锁 (K-04 的现场教训);
    /// - **不得**分配 (含 `Vec`/`Arc` 触堆)、**不得**打印 (串口写出会把持锁窗口拉到毫秒级);
    /// - **不得**获取任何运行队列锁 (层级 8 `eevdf_rq` → 层级 7 `PROCESS_TABLE` 是
    ///   单向例外, 见 `docs/explain/ref-lock-order.md`).
    ///
    /// 遍历范围 = 全部**非空**槽位升序 (含 0 号槽; 位图从 PID 2 起分配, 故 0 号槽
    /// 实际不在册). 缓冲写满即停止调用谓词, 与旧形态"命中数达上限即 break"同形,
    /// 故谓词内部的定长去重表可以与 `buf` 同长而不会越界.
    pub fn scan_into<T, F>(&self, buf: &mut [T], mut pred: F) -> usize
    where
        F: FnMut(&Process) -> Option<T>,
    {
        let mut filled = 0;
        self.walk_locked(|proc| {
            if filled == buf.len() {
                return false;
            }
            if let Some(item) = pred(proc) {
                buf[filled] = item;
                filled += 1;
            }
            true
        });
        filled
    }
}

/// 全局进程表实例; 进程槽位与 PID 分配的唯一权威来源.
pub static PROCESS_TABLE: ProcessTable = ProcessTable::new();

#[derive(Clone, Copy)]
struct ProcSnapshot {
    pid_bitmap: [bool; MAX_PROCESSES],
    next_search: u32,
    slots: [Option<NonNull<Process>>; MAX_PROCESSES],
}

// SAFETY: ProcSnapshot 是进程表的快照. 它包含的 NonNull<Process>
// SAFETY: ProcSnapshot 含裸指针切片, 但指针在快照丢弃前一直有效.
//         仅在 PROC_SNAPSHOT Mutex 保护下访问.
unsafe impl Send for ProcSnapshot {}
// SAFETY: 同上, Mutex 保护并发访问.
unsafe impl Sync for ProcSnapshot {}

static PROC_SNAPSHOT: Mutex<Option<ProcSnapshot>> = Mutex::new(None);

/// 将进程表的 PID 位图、搜索起点与槽位快照保存到 `PROC_SNAPSHOT`。
pub fn proc_freg_capture() {
    let table = &PROCESS_TABLE;
    *PROC_SNAPSHOT.lock() = Some(ProcSnapshot {
        pid_bitmap: *table.pid_bitmap.lock(),
        next_search: *table.next_search.lock(),
        slots: *table.processes.lock(),
    });
}

/// 从 `PROC_SNAPSHOT` 快照回滚进程表状态 (位图/搜索起点/槽位)。
pub fn proc_freg_rollback() -> bool {
    if let Some(ref snap) = *PROC_SNAPSHOT.lock() {
        let table = &PROCESS_TABLE;
        *table.pid_bitmap.lock() = snap.pid_bitmap;
        *table.next_search.lock() = snap.next_search;
        *table.processes.lock() = snap.slots;
    }
    true
}

fn proc_freg_capture_cb() {
    proc_freg_capture();
}

fn proc_freg_rollback_cb() -> bool {
    proc_freg_rollback()
}

/// 进程表 FREG 恢复回调挂载的**安全护栏** (fail-closed, 默认关).
///
/// `proc_freg_capture` 直接快照进程表槽位内的 `NonNull<Process>` 裸指针,
/// 既不 pin 也不加 refcount: 一旦某进程在 capture 之后、rollback 之前被释放,
/// `proc_freg_rollback` 会把悬垂指针写回进程表 → rollback use-after-free.
/// 该生命周期保护 (pin / refcount) 尚未实装, 立项见
/// `docs/plan/freg-proc-rollback-uaf-guard.md`. 实装完成前 **显式拒绝挂载**
/// capture/rollback 回调: 即便恢复域 id 归属被补全 (使 `find(4)` 返回 Some),
/// 也不会把裸指针快照路径接入 FREG, 杜绝误激活引 UAF.
const PROC_FREG_ROLLBACK_SAFE: bool = false;

/// 注册进程表的故障恢复域 (域 id 4); capture/rollback 回调是否挂载由
/// `PROC_FREG_ROLLBACK_SAFE` 护栏决定, 当前默认不挂载 (见护栏文档).
pub fn proc_register_freg_domain() {
    crate::privileged::freg::recovery_domain_register(4);
    let mgr = crate::privileged::freg::RECOVERY_MANAGER.lock();
    match mgr.find(4) {
        // 护栏放行才挂载裸指针快照回调.
        Some(dom) if PROC_FREG_ROLLBACK_SAFE => {
            *dom.capture_cb.lock() = Some(proc_freg_capture_cb);
            *dom.rollback_cb.lock() = Some(proc_freg_rollback_cb);
        }
        // 误激活警报: id 归属被补全 (find 返回 Some) 但 pin/refcount 尚未实装,
        // 拒绝挂载并留 trace, 防 proc_freg_capture 的 NonNull<Process> rollback UAF.
        Some(_dom) => {
            crate::klog_ffi!(
                klog_ffi_warn,
                "[FREG] proc 恢复域回调按护栏拒绝挂载 (NonNull<Process> 快照无 pin/refcount)"
            );
        }
        // 当前常态: 恢复域 id 归属未补全, find(4) 恒 None, 不挂载, 静默.
        None => {}
    }
}
