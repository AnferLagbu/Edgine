use super::rlimit::RlimitTable;
use super::scheduler::SchedPolicy;
use super::types::{
    BlockReason, KERNEL_STACK_SIZE, MAX_PROCESSES, Pid, ProcessContext, ProcessFlags, ProcessId,
    ProcessPriority, ProcessState,
};
use crate::framework::mm::{KERNEL_BASE, PAGE_SIZE, USER_ADDR_FLOOR};
use crate::framework::sync::IrqSpinLock as Mutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};

// ============================================================================
// 进程级 FD 分配策略 (P1-I-01 提取)
// ============================================================================
//
// D8: FdTable 分配策略 (first-fit, 上限 64) 经 DECISION-J (2026-09-13) 反转
// 迁回 framework/proc/fd_table.rs (Process 机制字段归 framework).
// 详见 [docs/plan/maintenance-2026-06-11.md] (P1-I-01).
pub use crate::framework::proc::fd_table::{FdTable, MAX_FDS_PER_PROCESS};

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    fn pmm_alloc_pages(count: u64) -> *mut u8;
    fn vmm_create_user_page_table() -> u64;
    fn vmm_destroy_page_table(cr3: u64);
}

pub const KERNEL_STACK_CANARY: u64 = 0xDEADBEEF_CAFEBABE;

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

pub struct Process {
    pub pid: ProcessId,
    pub pwm: AtomicU64,
    pub state: AtomicU32,
    pub priority: AtomicU32,
    pub flags: AtomicU32,

    pub name: Mutex<String>,
    pub parent: Option<ProcessId>,
    pub children: Mutex<Vec<ProcessId>>,

    pub context: Mutex<ProcessContext>,
    pub cr3: AtomicU64,
    pub kernel_stack: AtomicU64,
    pub user_stack: AtomicU64,

    pub exit_code: AtomicU32,
    pub cpu_time: AtomicU64,

    /// POSIX `times()` 报告的用户态 CPU 时间 (ticks)
    pub user_time: AtomicU64,
    /// POSIX `times()` 报告的内核态 CPU 时间 (ticks)
    pub sys_time: AtomicU64,
    /// 进程启动时刻 (jiffies)
    pub start_jiffies: AtomicU64,
    /// 进程累积运行 tick 计数 (由调度器每 tick 增加)
    pub tick_count: AtomicU64,

    /// POSIX `alarm()` 剩余秒数对应的到期时刻 (jiffies, 0 = 无 alarm)
    pub alarm_deadline: AtomicU64,
    /// alarm 触发时的 jiffies 快照 (用于 read 旧值)
    pub alarm_prev_remaining: AtomicU64,

    /// POSIX `setitimer(ITIMER_REAL)`: 到期时刻 (jiffies, 0 = 未启用)
    pub itimer_real_deadline: AtomicU64,
    /// 上次触发到当前的间隔 (interval)
    pub itimer_real_interval: AtomicU64,
    /// 距离到期剩余时间 (每次 setitimer 写入, getitimer 读)
    pub itimer_real_remaining: AtomicU64,

    pub block_reason: AtomicU32,

    pub sched_policy: AtomicU32,
    pub rt_priority: AtomicU32,

    pub nice: AtomicU32,
    pub cfs_vruntime: AtomicU64,
    pub cfs_weight: AtomicU64,
    pub cfs_sum_exec_runtime: AtomicU64,
    pub cfs_on_rq: AtomicBool,

    pub dl_runtime: AtomicU64,
    pub dl_deadline: AtomicU64,
    pub dl_period: AtomicU64,
    pub dl_abs: AtomicU64,
    pub dl_remaining: AtomicU64,

    pub session_id: AtomicU64,
    /// POSIX 进程组 ID (pgid); 0 表示未初始化 (实际默认为 pid).
    pub pgid: AtomicU32,
    pub fd_table: FdTable,

    /// CPU 亲和性掩码 (C2 完整实现):
    ///   bit i (i < `MAX_CPUS`) 置位 = 允许在 CPU i 上运行
    ///   默认值 `u64::MAX` (前 64 个 CPU 都允许), 兼容单核
    ///   通过 `sys_sched_setaffinity` 修改
    ///   调度器 `select_cpu` 与跨 CPU 迁移时检查
    pub cpuset_allowed: AtomicU64,

    /// 阻塞睡眠到期时间 (ticks), 用于 `proc_sleep_ms`
    pub sleep_until: AtomicU64,

    pub ref_count: AtomicU32,
    pub pending_free: AtomicBool,
    pub pending_signals: AtomicU64,

    // --- POSIX 信号处理字段 ---
    /// 信号屏蔽字 (bit i = 信号 i+1 被屏蔽)
    pub blocked_mask: AtomicU64,

    /// 信号处理动作表 (索引 0..=63 对应 SIGHUP(1)..SIGRTMAX(64); B05-34 RT 信号扩展)
    /// 每项: 0 = `SIG_DFL`, 1 = `SIG_IGN`, 其他 = 用户态 handler 地址
    pub sigaction_table: Mutex<[u64; 64]>,

    /// 信号替换栈 (sigaltstack), 0 = 未设置
    pub sigaltstack_addr: AtomicU64,
    pub sigaltstack_size: AtomicU64,
    pub sigaltstack_flags: AtomicU32, // SS_ONSTACK / SS_DISABLE

    /// Per-process 资源限制表 (RLIMIT_*)
    pub rlimit_table: Mutex<RlimitTable>,

    /// Per-process 8 字节 stack canary (P1 #14)
    ///
    /// 低字节恒为 0 (Linux/glibc 兼容). 用户态编译器
    /// (`-fstack-protector`) 在 prologue 写入, epilogue 验证.
    /// 进程创建时由 [`crate::framework::proc::canary::generate_canary`]
    /// 初始化, fork 继承父进程.
    pub stack_canary: AtomicU64,

    /// Per-process Seccomp 状态 (C7)
    ///
    /// 包含模式 (Disabled/Strict/Filter) + 过滤器链 + `no_new_privs` 位.
    /// fork 继承全部过滤器; execve 保留.
    pub seccomp: crate::framework::proc::SeccompState,

    /// Per-process Credo 域级行为门控标志 (分册 9 批次 4)
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
    pub namespaces: Mutex<crate::framework::proc::NamespaceSet>,

    /// Per-process cgroup ID (D2)
    ///
    /// 进程所属 cgroup 的 ID, 默认 0 (根 cgroup).
    /// fork 继承父进程的 cgroup; 可通过 `sys_cgroup_attach` 迁移.
    pub cgroup_id: AtomicU64,

    /// Per-process NUMA 内存策略 (D3)
    ///
    /// 控制进程的内存分配节点选择策略.
    /// fork 继承父进程策略; 可通过 `sys_set_mempolicy` 修改.
    pub numa_policy: Mutex<crate::framework::mm::numa::NumaMempolicy>,

    /// Per-process 凭证会话上下文 (P2-I-30)
    ///
    /// 替代 `credo::session` 中 `static GLOBAL_SESSION` 的全局 `UnsafeCell` 单例.
    /// 每个进程拥有独立的 `PwmContext` (`uid/gid/euid/egid/saved_euid/saved_egid`/
    /// `domain/elevation_granted_pwm`). 在 SMP 下, 不同 CPU 上不同进程的会话
    /// 上下文天然隔离, 杜绝身份/权限串台.
    /// 进程退出时该字段随 Process 一起释放, 自动回收.
    pub session: Mutex<crate::framework::credo::types::PwmContext>,

    /// Per-process SUID 提权栈 (P2-I-30)
    ///
    /// `try_setuid` / `elevate_for_suid` 推送 `PwmContext` 快照;
    /// `drop_elevation` 弹出. 栈深上限 8, 与原 `SessionManager` 保持一致.
    pub session_elev_stack: Mutex<[crate::framework::credo::types::PwmContext; 8]>,

    /// Per-process SUID 提权栈深度 (P2-I-30)
    pub session_elev_depth: AtomicIsize,

    /// Per-process TLS 基址 (`x86_64`: `MSR_FS_BASE`, aarch64: `tpidr_el0`)
    ///
    /// `clone(CLONE_SETTLS)` 设置. 当前仅存储, 切换恢复未实装; x86_64 需在
    /// 切换时写 MSR_FS_BASE、aarch64 写 tpidr_el0 (待用户态线程库出现时实装).
    /// 0 表示未设置.
    pub tls_base: AtomicU64,

    /// CLONE_CHILD_CLEARTID 登记的清除地址
    ///
    /// 进程退出时向该用户地址写 0 并对其实施 futex 唤醒.
    /// 0 表示未设置.
    pub clear_child_tid: AtomicU64,

    /// set_robust_list 登记的 robust list 头指针 (用户地址)
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
// 1. name, children, context 等复合类型都被 Mutex 包装
//    - 访问这些字段必须先获取锁, 保证互斥
// 2. pid, pwm, state 等简单字段都是 Atomic 类型
//    - 使用 Ordering::SeqCst 或 Acquire/Release 保证可见性
// 3. 不存在内部可变性导致的未同步修改
// 4. 调度器在切换进程时通过 scheduler_lock 保护整个 ProcessTable
//
// 所有字段 (Mutex<T>, Atomic*, u32, u64, bool, Option<ProcessId>) 自动 Send+Sync.

impl Process {
    pub fn new(pid: Pid, name: &str, parent: Option<ProcessId>) -> Self {
        Self {
            pid: ProcessId(pid),
            pwm: AtomicU64::new(0),
            state: AtomicU32::new(ProcessState::Created as u32),
            priority: AtomicU32::new(ProcessPriority::Normal as u32),
            flags: AtomicU32::new(0),
            name: Mutex::new(String::from(name)),
            parent,
            children: Mutex::new(Vec::new()),
            context: Mutex::new(ProcessContext::new()),
            cr3: AtomicU64::new(0),
            kernel_stack: AtomicU64::new(0),
            user_stack: AtomicU64::new(0),
            exit_code: AtomicU32::new(0),
            cpu_time: AtomicU64::new(0),
            user_time: AtomicU64::new(0),
            sys_time: AtomicU64::new(0),
            start_jiffies: AtomicU64::new(0),
            tick_count: AtomicU64::new(0),
            alarm_deadline: AtomicU64::new(0),
            alarm_prev_remaining: AtomicU64::new(0),
            itimer_real_deadline: AtomicU64::new(0),
            itimer_real_interval: AtomicU64::new(0),
            itimer_real_remaining: AtomicU64::new(0),
            block_reason: AtomicU32::new(BlockReason::Unknown as u32),
            sched_policy: AtomicU32::new(SchedPolicy::Normal as u32),
            rt_priority: AtomicU32::new(0),
            nice: AtomicU32::new(0),
            cfs_vruntime: AtomicU64::new(0),
            cfs_weight: AtomicU64::new(super::cfs::NICE0_WEIGHT),
            cfs_sum_exec_runtime: AtomicU64::new(0),
            cfs_on_rq: AtomicBool::new(false),
            dl_runtime: AtomicU64::new(0),
            dl_deadline: AtomicU64::new(0),
            dl_period: AtomicU64::new(0),
            dl_abs: AtomicU64::new(0),
            dl_remaining: AtomicU64::new(0),
            session_id: AtomicU64::new(0),
            pgid: AtomicU32::new(0),
            fd_table: FdTable::new(),
            // C2 CPU 亲和性: 默认所有 CPU (前 64 个) 都允许
            cpuset_allowed: AtomicU64::new(u64::MAX),
            sleep_until: AtomicU64::new(0),
            ref_count: AtomicU32::new(1),
            pending_free: AtomicBool::new(false),
            pending_signals: AtomicU64::new(0),
            blocked_mask: AtomicU64::new(0),
            sigaction_table: Mutex::new([0u64; 64]),
            sigaltstack_addr: AtomicU64::new(0),
            sigaltstack_size: AtomicU64::new(0),
            sigaltstack_flags: AtomicU32::new(0),
            rlimit_table: Mutex::new(RlimitTable::new()),
            // P1 #14: 进程创建时分配独立 canary
            stack_canary: AtomicU64::new(crate::framework::proc::generate_canary()),
            // C7: Seccomp 默认 Disabled
            seccomp: crate::framework::proc::SeccompState::new(),
            // 分册 9 批次 4: 域级行为门控默认无标志 (不改变任何调用行为)
            domain_flags: AtomicU32::new(0),
            // D1: Namespace 默认 init namespace 集合
            namespaces: Mutex::new(crate::framework::proc::NamespaceSet::new_init()),
            // D2: cgroup 默认根 cgroup (id=0)
            cgroup_id: AtomicU64::new(0),
            // D3: NUMA 策略默认 Default
            numa_policy: Mutex::new(crate::framework::mm::numa::NumaMempolicy::new()),
            // P2-I-30: 进程级凭证会话上下文 (uid/gid/euid/egid/saved_*/domain)
            session: Mutex::new(crate::framework::credo::types::PwmContext::default()),
            // P2-I-30: SUID 提权栈 (深度 0, 容量 8)
            session_elev_stack: Mutex::new(
                [crate::framework::credo::types::PwmContext::default(); 8],
            ),
            session_elev_depth: AtomicIsize::new(0),
            tls_base: AtomicU64::new(0),
            clear_child_tid: AtomicU64::new(0),
            robust_head: AtomicU64::new(0),
            robust_len: AtomicU32::new(0),
        }
    }

    pub fn allocate_kernel_stack(&self) -> bool {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let stack = pmm_alloc_pages((KERNEL_STACK_SIZE / PAGE_SIZE as usize) as u64);
            if stack.is_null() {
                return false;
            }
            // 将物理地址转换为高半段虚拟地址, 以便在用户页表加载时
            // 仍能访问 TSS RSP0.
            let stack_top = stack as u64 + KERNEL_BASE + KERNEL_STACK_SIZE as u64;
            self.kernel_stack.store(stack_top, Ordering::SeqCst);
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
    /// 存下的**高半区**栈顶 (phys + KERNEL_BASE + KERNEL_STACK_SIZE). 该地址与
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
        ctx.cs = u64::from(crate::framework::arch::gdt::SELECTOR_KERNEL_CODE);
        ctx.ss = u64::from(crate::framework::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.ds = u64::from(crate::framework::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.es = u64::from(crate::framework::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.fs = u64::from(crate::framework::arch::gdt::SELECTOR_KERNEL_DATA);
        ctx.gs = u64::from(crate::framework::arch::gdt::SELECTOR_KERNEL_DATA);
    }

    /// 初始化内核态 idle 任务首次被调度时的执行上下文 (aarch64).
    ///
    /// aarch64 复用 `ProcessContext` 的字段偏移但语义不同 (见
    /// `framework/arch/aarch64/context.rs`): `ds`(96) = SP, `es`(104) = TTBR0_EL1,
    /// `fs`(112) = SPSR_EL1, `gs`(120) = ELR_EL1. `context_switch_asm` 末尾以
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

    pub fn get_state(&self) -> ProcessState {
        ProcessState::from_u8(self.state.load(Ordering::SeqCst) as u8)
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
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
        match (current, new_state) {
            // 允许的正常转换
            (ProcessState::Created, ProcessState::Ready) => {}
            (ProcessState::Ready, ProcessState::Running) => {}
            (ProcessState::Running, ProcessState::Ready) => {} // 时间片耗尽/抢占
            (ProcessState::Running, ProcessState::Blocked) => {} // 阻塞系统调用
            (ProcessState::Running, ProcessState::Zombie) => {} // exit()
            (ProcessState::Running, ProcessState::Frozen) => {} // freeze
            (ProcessState::Ready, ProcessState::Frozen) => {}  // freeze
            (ProcessState::Blocked, ProcessState::Frozen) => {} // freeze
            (ProcessState::Blocked, ProcessState::Ready) => {} // 事件完成唤醒
            (ProcessState::Blocked, ProcessState::Zombie) => {} // 被 kill
            (ProcessState::Zombie, ProcessState::Terminated) => {} // wait() 回收
            (ProcessState::Frozen, ProcessState::Ready) => {}  // thaw 唤醒
            (ProcessState::Frozen, ProcessState::Blocked) => {} // thaw 后仍需等待

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

    pub fn get_priority(&self) -> ProcessPriority {
        ProcessPriority::from_u32(self.priority.load(Ordering::SeqCst))
    }

    pub fn set_priority(&self, priority: ProcessPriority) {
        self.priority.store(priority as u32, Ordering::SeqCst);
    }

    pub fn is_kernel(&self) -> bool {
        let flags = self.flags.load(Ordering::SeqCst);
        (flags & ProcessFlags::IS_KERNEL.bits()) != 0
    }

    pub fn set_kernel(&self, is_kernel: bool) {
        let mut flags = self.flags.load(Ordering::SeqCst);
        if is_kernel {
            flags |= ProcessFlags::IS_KERNEL.bits();
        } else {
            flags &= !ProcessFlags::IS_KERNEL.bits();
        }
        self.flags.store(flags, Ordering::SeqCst);
    }

    pub fn get_sched_policy(&self) -> SchedPolicy {
        SchedPolicy::from_u32(self.sched_policy.load(Ordering::SeqCst))
    }

    pub fn set_sched_policy(&self, policy: SchedPolicy) {
        self.sched_policy.store(policy as u32, Ordering::SeqCst);
    }

    pub fn get_rt_priority(&self) -> u8 {
        self.rt_priority.load(Ordering::SeqCst) as u8
    }

    pub fn set_rt_priority(&self, priority: u8) {
        self.rt_priority
            .store(u32::from(priority), Ordering::SeqCst);
    }

    pub fn get_pwm(&self) -> u64 {
        self.pwm.load(Ordering::SeqCst)
    }

    pub fn set_pwm(&self, pwm: u64) {
        self.pwm.store(pwm, Ordering::SeqCst);
    }

    pub fn try_inc_ref(&self) -> bool {
        self.ref_count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                if v > 0 { Some(v + 1) } else { None }
            })
            .is_ok()
    }

    pub fn dec_ref(&self) -> u32 {
        self.ref_count.fetch_sub(1, Ordering::AcqRel) - 1
    }

    pub fn signal_pending_set(&self, sig: u32) {
        self.pending_signals
            .fetch_or(1u64 << sig, Ordering::Release);
    }

    pub fn signal_pending_get(&self) -> u64 {
        self.pending_signals.load(Ordering::Acquire)
    }

    pub fn signal_pending_clear(&self, mask: u64) {
        self.pending_signals.fetch_and(!mask, Ordering::Release);
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let cr3 = self.cr3.load(Ordering::SeqCst);
        if cr3 != 0 {
            // 计数归零才销毁: cr3 可能仍被 CLONE_VM 的兄弟进程持有, 无条件销毁
            // 会使对方在用的页表被回收 (UAF). `frame_dec` 对未计数帧 (计数 0)
            // 返回 false, 同时防止同一 PML4 被二次销毁.
            if crate::framework::mm::pmm::get_pmm().frame_dec(crate::framework::mm::PhysAddr(cr3)) {
                // SAFETY: cr3 由 vmm_create_user_page_table / COW 克隆产生,
                // 且计数已归零 (无其他持有者), 本进程是最后持有者.
                unsafe {
                    vmm_destroy_page_table(cr3);
                }
            }
        }
    }
}

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

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            processes: Mutex::new([None; MAX_PROCESSES]),
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
                *search_idx = ((idx + 1) % max) as u32;
                return Some(idx as u32);
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

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn insert(&self, process: *mut Process) -> bool {
        // SAFETY: caller guarantees process is a valid non-null pointer.
        let nn = match NonNull::new(process) {
            Some(nn) => nn,
            None => return false,
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

    pub fn get(&self, pid: Pid) -> Option<*mut Process> {
        let table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return None;
        }
        table[pid as usize].map(core::ptr::NonNull::as_ptr)
    }

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
        crate::framework::process_cleanup::notify_process_exit(pid);
        let mut table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return;
        }
        match table[pid as usize] {
            Some(nn) => {
                // SAFETY: nn is a valid NonNull pointer inserted by insert().
                let proc = unsafe { nn.as_ref() };
                proc.pending_free.store(true, Ordering::Release);
                let prev = proc.dec_ref();
                if prev == 0 {
                    table[pid as usize] = None;
                    drop(table);
                    // SAFETY: nn 由 Box::into_raw 分配, 且我们持有唯一引用 (ref_count 归零).
                    unsafe {
                        let boxed = Box::from_raw(nn.as_ptr());
                        drop(boxed);
                    }
                    // M5: 回收 PID
                    self.free_pid(pid);
                }
            }
            None => {}
        }
    }

    pub fn try_inc_ref(&self, pid: Pid) -> bool {
        let table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return false;
        }
        table[pid as usize].map_or(false, |nn| {
            // SAFETY: nn is a valid NonNull pointer inserted by insert().
            let proc_ref = unsafe { nn.as_ref() };
            proc_ref.try_inc_ref()
        })
    }

    pub fn dec_ref_and_maybe_free(&self, pid: Pid) {
        let mut table = self.processes.lock();
        if pid as usize >= MAX_PROCESSES {
            return;
        }
        match table[pid as usize] {
            Some(nn) => {
                // SAFETY: nn is a valid NonNull pointer inserted by insert().
                // Mutex 锁保证独占访问.
                let proc = unsafe { nn.as_ref() };
                let prev = proc.dec_ref();
                if prev == 0 && proc.pending_free.load(Ordering::Acquire) {
                    table[pid as usize] = None;
                    drop(table);
                    // SAFETY: nn 由 Box::into_raw 分配, 且我们持有唯一引用 (ref_count 归零).
                    unsafe {
                        let boxed = Box::from_raw(nn.as_ptr());
                        drop(boxed);
                    }
                    // M5: 回收 PID
                    self.free_pid(pid);
                }
            }
            None => {}
        }
    }

    /// 遍历所有进程 (回调返回 false 时提前终止)
    pub fn for_each<F: FnMut(&Process) -> bool>(&self, mut f: F) {
        let table = self.processes.lock();
        for entry in table.iter() {
            if let Some(nn) = entry {
                // SAFETY: nn is a valid NonNull pointer inserted by insert().
                let proc = unsafe { nn.as_ref() };
                if !f(proc) {
                    break;
                }
            }
        }
    }
}

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

pub fn proc_barrier_capture() {
    let table = &PROCESS_TABLE;
    *PROC_SNAPSHOT.lock() = Some(ProcSnapshot {
        pid_bitmap: *table.pid_bitmap.lock(),
        next_search: *table.next_search.lock(),
        slots: *table.processes.lock(),
    });
}

pub fn proc_barrier_rollback() -> bool {
    if let Some(ref snap) = *PROC_SNAPSHOT.lock() {
        let table = &PROCESS_TABLE;
        *table.pid_bitmap.lock() = snap.pid_bitmap;
        *table.next_search.lock() = snap.next_search;
        *table.processes.lock() = snap.slots;
    }
    true
}

fn proc_barrier_capture_cb() {
    proc_barrier_capture();
}

fn proc_barrier_rollback_cb() -> bool {
    proc_barrier_rollback()
}

pub fn proc_register_barrier_domain() {
    crate::framework::barrier::recovery_domain_register(4);
    if let Some(dom) = crate::framework::barrier::RECOVERY_MANAGER.lock().find(4) {
        *dom.capture_cb.lock() = Some(proc_barrier_capture_cb);
        *dom.rollback_cb.lock() = Some(proc_barrier_rollback_cb);
    }
}
