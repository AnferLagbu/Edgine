//! 系统调用分发实现
//!
//! 从 `mod.rs` 拆分而来, 包含主分发函数和所有 `sys_*` 处理函数。

#[cfg(target_arch = "x86_64")]
use crate::framework::idt::InterruptFrame;
use core::sync::atomic::Ordering;

use super::raw;
use super::types::{
    Errno, QX_CET, QX_CGROUP_ATTACH, QX_CGROUP_CREATE, QX_CGROUP_DESTROY, QX_CGROUP_GET_STAT,
    QX_CGROUP_SET_LIMIT, QX_IO_URING_SUBMIT, QX_NF_ADD_RULE, QX_NF_DEL_RULE, QX_PM, QX_ROUTE_ADD,
    QX_ROUTE_DEL, QX_ROUTE_QUERY, QX_SECURE_BOOT, QX_TICKLESS, QX_TIMESYNC, QX_TPM, QX_UEFI,
    SYS_accept, SYS_bind, SYS_connect, SYS_getpeername, SYS_getsockname, SYS_getsockopt,
    SYS_listen, SYS_recvfrom, SYS_recvmsg, SYS_sendmsg, SYS_sendto, SYS_setsockopt, SYS_shutdown,
    SYS_socket,
};
// SYS_CREDO_DISK_INSTALL 分支已迁至 services (T2 批 5), 编号常量仅在 types.rs 保留
// (aarch64 生产构建不引用, 与迁移前 cfg 门控语义一致)

/// fb_mmap 目标虚拟地址上界 — 集中定义于 `framework::constants::limits`
/// (与用户指针校验边界语义不同, 见该常量注释).
use crate::framework::constants::limits::FB_MMAP_ADDR_MAX;

/// `make test-smp` 门槛埋点: 是否已收到首个来自用户态 (CPL3) 的 syscall.
///
/// 该埋点是一次性的 (避免刷屏), 用于证明"用户态确实执行过指令" ——
/// `Entering Ring 3` 日志打印在真正 iretq 之前, 不能作为该证据.
#[cfg(target_arch = "x86_64")]
static FIRST_USER_SYSCALL_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// APS-05: 双核并发 EL0 观测计数 (每核一份, 有界).
///
/// 用户态 syscall 只能由 EL0 任务发起, 故"某核观测到 EL0 syscall"即"该核确有
/// 用户任务运行于 EL0"的证据. 本计数使每核前 [`EL0_OBSERVE_LIMIT`] 次 syscall
/// 各打印一行 `[SMP] EL0 pid=N cpu=M`; `scripts/qemu_boot_test.sh` 以 `-smp 2`
/// 断言 `cpu=0` 与 `cpu=1` 成对出现 (双核并发 EL0 判据). 上限化避免忙等用户
/// 任务刷屏; 索引 = `arch::cpu_id` 取模 `MAX_CPUS`.
static EL0_OBSERVED_PER_CPU: [core::sync::atomic::AtomicU32; crate::framework::config::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU32::new(0) }; crate::framework::config::MAX_CPUS];

/// 每核打印上限 (见 [`EL0_OBSERVED_PER_CPU`]).
const EL0_OBSERVE_LIMIT: u32 = 4;

/// 记录一次来自 EL0 的 syscall 观测 (有界诊断, 见 [`EL0_OBSERVED_PER_CPU`]).
///
/// 由两个架构的 EL0 syscall 入口调用: x86_64 `syscall_dispatch_from_frame`、
/// aarch64 `arch/aarch64/exception.rs::svc_handler` (刻意不置于架构中立的
/// `syscall_dispatch`, 以免内核侧 `usermode::dispatch_syscall` 调用混入观测).
pub fn observe_el0_syscall() {
    let cpu = crate::framework::cpu::arch::cpu_id() as usize % crate::framework::config::MAX_CPUS;
    let counter = &EL0_OBSERVED_PER_CPU[cpu];
    // 同核 syscall 路径不在中断中重入 ⇒ 计数无并发写者, load/store 足够.
    let seen = counter.load(Ordering::Relaxed);
    if seen >= EL0_OBSERVE_LIMIT {
        return;
    }
    counter.store(seen + 1, Ordering::Relaxed);
    let pid = crate::framework::proc::SCHEDULER.current().unwrap_or(0);
    crate::klog_info!(Kernel, "[SMP] EL0 pid={} cpu={}", pid, cpu);
}

#[cfg(target_arch = "x86_64")]
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// 调用者处于内核上下文. `ptr` 是已校验的用户态字符串指针.
pub unsafe extern "C" fn syscall_dispatch_from_frame(frame: *mut InterruptFrame) {
    // APS-05: 双核并发 EL0 观测 (有界诊断, 见 `observe_el0_syscall`).
    observe_el0_syscall();
    // ═══ 诊断: syscall dispatch 入口 (仅调试构建, 生产不包含) ═══
    #[cfg(all(target_arch = "x86_64", feature = "debug_syscall"))]
    unsafe {
        core::arch::asm!(
            "push rax",
            "push rdx",
            "mov dx, 0x3F8",
            "mov al, 0x4A", // 'J' - dispatch entered
            "out dx, al",
            "pop rdx",
            "pop rax",
            options(nomem, preserves_flags),
        );
    }
    // ═══ 诊断结束 ═══

    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
    unsafe {
        if frame.is_null() {
            return;
        }
        let f = &mut *frame;

        // make test-smp 门槛埋点: 首次收到来自用户态 (CPL3) 的 syscall 时打印一行
        // 固定文案, 证明用户态确实执行过指令 (仅首次打印, 避免刷屏).
        if !FIRST_USER_SYSCALL_LOGGED.swap(true, Ordering::SeqCst) {
            let pid = crate::framework::proc::SCHEDULER.current().unwrap_or(0);
            crate::klog_info!(Kernel, "[SMP] first user syscall from pid={}", pid);
        }

        let syscall_num = f.rax;

        // B05-55 根治: 每次 syscall 进入, 把用户寄存器保存到当前进程 p.context.
        // fork/clone 复制 p.context 时即得真实用户状态 (否则 init 等直接进入
        // 用户态的进程 context 全零, 子进程 iretq 用全零帧 → 未进用户态).
        // 需在 rt_sigreturn 处理前保存 (sigreturn 之后 frame 被恢复为 signal 帧,
        // 保存的是恢复后的用户寄存器, 同样正确).
        {
            let cur = crate::framework::proc::SCHEDULER.current().unwrap_or(0);
            if cur != 0 {
                crate::framework::proc::proc_save_user_regs(cur, f);
            }
        }

        // rt_sigreturn 特殊处理: 需要直接修改 frame, 不走正常 dispatch
        // Linux x86_64 编号 15 / aarch64 编号 139
        #[cfg(target_arch = "x86_64")]
        let is_rt_sigreturn = syscall_num == crate::framework::syscall::types::SYS_rt_sigreturn;
        #[cfg(target_arch = "aarch64")]
        let is_rt_sigreturn = syscall_num == 139;

        if is_rt_sigreturn {
            let sigframe_ptr = (f.rsp + 8) as *const crate::framework::proc::SignalFrame;
            if !sigframe_ptr.is_null() {
                let sigframe = core::ptr::read_unaligned(sigframe_ptr);
                f.r15 = sigframe.r15;
                f.r14 = sigframe.r14;
                f.r13 = sigframe.r13;
                f.r12 = sigframe.r12;
                f.r11 = sigframe.r11;
                f.r10 = sigframe.r10;
                f.r9 = sigframe.r9;
                f.r8 = sigframe.r8;
                f.rdi = sigframe.rdi;
                f.rsi = sigframe.rsi;
                f.rbp = sigframe.rbp;
                f.rdx = sigframe.rdx;
                f.rcx = sigframe.rcx;
                f.rbx = sigframe.rbx;
                f.rax = sigframe.rax;
                f.rip = sigframe.rip;
                f.cs = sigframe.cs;
                f.rflags = sigframe.rflags;
                f.rsp = sigframe.rsp;
                f.ss = sigframe.ss;
            }
            // P1-I-45: sigreturn 清除替代栈 SS_ONSTACK 标志 (原 sys_rt_sigreturn
            // 死分支中的清除逻辑, T3 迁移至本可达路径——sigreturn 返回后不再处于
            // 替代栈上, POSIX 语义).
            {
                let cur = crate::framework::proc::process_get_current_pid();
                if cur != 0 {
                    crate::framework::proc::process_with_mut(cur, |proc| {
                        let flags = proc.sigaltstack_flags.load(Ordering::Acquire);
                        proc.sigaltstack_flags.store(
                            flags & !crate::framework::proc::SS_ONSTACK,
                            Ordering::Release,
                        );
                    });
                }
            }
            return;
        }

        let a0 = f.rdi;
        let a1 = f.rsi;
        let a2 = f.rdx;
        let a3 = f.r10;
        let a4 = f.r8;
        let a5 = f.r9;
        let result = syscall_dispatch(syscall_num, a0, a1, a2, a3, a4, a5);
        f.rax = result as u64;

        // 返回用户态前检查待投递信号
        // SAFETY: frame 有效, 当前在当前 CPU 的 syscall 上下文
        crate::framework::proc::do_signal_deliver(frame);
    }
}

macro_rules! dispatch {
    ($num:expr_2021, $name:expr_2021) => {{
        let ret = $num;
        // SAFETY: klog_write 是 C-ABI 日志函数，$name 是 Rust 静态字符串
        // (字节切片)，传给 C 时按指针 + 长度传递。
        unsafe {
            crate::framework::klog::klog_write(
                0,
                7,
                core::ptr::null(),
                core::ptr::null(),
                0,
                $name.as_ptr() as *const u8,
            );
        }
        ret
    }};
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// 从中断上下文 (int 0x80) 调用. 所有寄存器值来自被打断的用户上下文.
pub unsafe extern "C" fn syscall_dispatch(
    num: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> i64 {
    // TD-10: 进入内核态, tick 期间 sys_time 累加.
    crate::framework::proc::proc_set_in_kern(1);
    let result = syscall_dispatch_impl(num, a0, a1, a2, a3, a4, a5);
    // 出口恢复用户态, tick 期间 user_time 累加.
    crate::framework::proc::proc_set_in_kern(0);
    result
}

fn syscall_dispatch_impl(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    // 直接 Linux ABI: syscall 编号直接使用 Linux 标准编号, 无需翻译

    // C7: Seccomp 过滤检查 (在 dispatch 之前)
    let args = [a0, a1, a2, a3, a4, a5];
    if let Some(ret) = crate::framework::proc::seccomp_check(num, &args) {
        return ret;
    }

    // 分册 9 批次 4: Credo 域级行为门控 (在 seccomp 之后, 策略分发之前)
    if let Some(ret) = crate::framework::proc::domain_gate_check(num, &args) {
        return ret;
    }

    // L-01: 优先委托 services 层策略分发
    let svc_ret = super::dispatch_trait::current_syscall_dispatch().dispatch(num, args);
    if svc_ret != crate::framework::syscall::types::ENOSYS_RET {
        return svc_ret;
    }

    // framework 回退: 处理尚未迁移到 services 的 syscall
    match num {
        // ==================== 信号 ====================
        // T3 (syscall-followup): SYS_rt_sigreturn 分支已删除——pre-dispatch 特殊路径
        // (L89-119) 无条件拦截编号 15 并直接恢复 sigframe 返回, 本分发器永不可达.

        // ==================== C7: Seccomp / prctl ====================
        // T2 批 2 (syscall-followup): SYS_seccomp / SYS_prctl 分支已迁至
        // services (services::proc::seccomp::seccomp_syscall / prctl_syscall).

        // ==================== C5: 路由表 ====================
        QX_ROUTE_ADD => dispatch!(
            crate::framework::net::route::sys_route_add(a0, a1, a2),
            b"route_add\0"
        ),
        QX_ROUTE_DEL => dispatch!(
            crate::framework::net::route::sys_route_del(a0, a1, a2),
            b"route_del\0"
        ),
        QX_ROUTE_QUERY => dispatch!(
            crate::framework::net::route::sys_route_query(a0),
            b"route_query\0"
        ),

        // ==================== C5: Netfilter ====================
        QX_NF_ADD_RULE => dispatch!(
            crate::framework::net::netfilter::sys_nf_add_rule(a0, a1, a2, a3, a4, a5),
            b"nf_add_rule\0"
        ),
        QX_NF_DEL_RULE => dispatch!(
            crate::framework::net::netfilter::sys_nf_del_rule(a0, a1),
            b"nf_del_rule\0"
        ),

        // ==================== C4: io_uring ====================
        // T2 批 3 (syscall-followup): SYS_io_uring_setup / SYS_io_uring_enter
        // 分支已迁至 services (services::io::iouring::io_uring_setup_syscall /
        // io_uring_enter_syscall).
        // T3 (syscall-followup): SYS_io_uring_register 分支已删除——原实现为恒
        // ENOSYS 桩 (iouring.rs), 删除后落 `_ =>` 兜底 ENOSYS, 行为不变.
        // 实装注册缓冲区/文件语义时在 services 层接线 (T2 批 3).
        QX_IO_URING_SUBMIT => dispatch!(
            crate::framework::io::iouring::sys_io_uring_submit_sqe(a0, a1, a2, a3, a4, a5),
            b"io_uring_submit\0"
        ),

        // ==================== D1: Namespace ====================
        // T2 批 4 (syscall-followup): SYS_unshare / SYS_setns 分支已迁至
        // services (services::proc::namespace::unshare_syscall / setns_syscall).

        // ==================== D2: cgroup ====================
        QX_CGROUP_CREATE => dispatch!(
            crate::framework::proc::sys_cgroup_create(a0, a1, a2),
            b"cgroup_create\0"
        ),
        QX_CGROUP_DESTROY => dispatch!(
            crate::framework::proc::sys_cgroup_destroy(a0),
            b"cgroup_destroy\0"
        ),
        QX_CGROUP_ATTACH => dispatch!(
            crate::framework::proc::sys_cgroup_attach(a0, a1),
            b"cgroup_attach\0"
        ),
        QX_CGROUP_SET_LIMIT => dispatch!(
            crate::framework::proc::sys_cgroup_set_limit(a0, a1, a2),
            b"cgroup_set_limit\0"
        ),
        QX_CGROUP_GET_STAT => dispatch!(
            crate::framework::proc::sys_cgroup_get_stat(a0, a1),
            b"cgroup_get_stat\0"
        ),

        // ==================== D4: eBPF ====================
        // T2 批 4 (syscall-followup): SYS_bpf 分支已迁至 services
        // (services::debug::ebpf::bpf_syscall, 委托 framework debug::sys_bpf).

        // ==================== D5: 电源管理 ====================
        QX_PM => dispatch!(crate::framework::driver::sys_pm(a0, a1, a2), b"pm\0"),

        // ==================== D6: 安全启动 + TPM ====================
        QX_SECURE_BOOT => dispatch!(
            crate::framework::credo::sys_secure_boot(a0, a1, a2, a3),
            b"secure_boot\0"
        ),
        QX_TPM => dispatch!(crate::framework::credo::sys_tpm(a0, a1, a2, a3), b"tpm\0"),

        // ==================== D7: Shadow Stack (CET) ====================
        QX_CET => dispatch!(
            crate::framework::arch::shadow_stack::sys_cet(a0, a1, a2),
            b"cet\0"
        ),

        // ==================== D8: 无 tick 模式 (NO_HZ) ====================
        QX_TICKLESS => dispatch!(
            crate::framework::timer::sys_tickless(a0, a1, a2),
            b"tickless\0"
        ),

        // ==================== D9: NTP/PTP 时钟同步 ====================
        QX_TIMESYNC => dispatch!(
            crate::framework::timer::sys_timesync(a0, a1, a2),
            b"timesync\0"
        ),

        // ==================== D10: kexec ====================
        // T2 批 4 (syscall-followup): SYS_kexec_load 分支已迁至 services
        // (services::driver::kexec::kexec_syscall, 委托 framework driver::sys_kexec).

        // ==================== D11: UEFI ====================
        QX_UEFI => dispatch!(crate::framework::driver::sys_uefi(a0, a1, a2), b"uefi\0"),

        // ==================== 进程 ====================
        // T2 批 2 (syscall-followup): SYS_tcgetpgrp / SYS_tcsetpgrp 分支已迁至
        // services (services::proc::session::tcgetpgrp_syscall / tcsetpgrp_syscall).

        // ==================== 网络 (services 代理) ====================
        // 分层契约 (docs/plan/syscall-dispatch-cleanup.md B1/B3): 网络 syscall 由
        // services::syscall::dispatch_net 真实实现 (无 cfg 门控, 所有构建配置下
        // 优先命中); 本回退层仅保留 not(net) 哨兵, 禁止新增与 services 重叠的
        // 真实实现分支 (见 mod.rs 分层契约).
        #[cfg(not(feature = "net"))]
        SYS_socket | SYS_connect | SYS_accept | SYS_sendto | SYS_recvfrom | SYS_shutdown
        | SYS_bind | SYS_listen | SYS_sendmsg | SYS_recvmsg | SYS_setsockopt | SYS_getsockopt
        | SYS_getsockname | SYS_getpeername => {
            dispatch!(Errno::ENOSYS.as_ret(), b"net_nosys\0")
        }

        // ==================== 时间 ====================
        // T3 (syscall-followup): SYS_tgkill 分支已删除——原实现忽略 _tgid 且
        // 语义错位 (将 tid 当 pid 发信号), 属半成品; 用户态 0 调用方. 恢复
        // ENOSYS 安全态, 实装线程组语义时在 services 层接线 (T1/T2).
        // T2 (syscall-followup): SYS_execve / SYS_setrlimit 分支已迁至
        // services (services::proc::exec / services::proc::sysinfo).

        // ==================== sendfile / splice ====================
        // T2 批 3 (syscall-followup): SYS_sendfile / SYS_splice 分支已迁至
        // services (services::fs::sendfile::sys_sendfile / sys_splice).
        // 阶段 2-A (framekernel 范式): 该实现已完整下沉 services (0 unsafe),
        // framework 侧 sendfile 机制与其 re-export 已删除.

        // ==================== Credo 私有 syscall ====================
        // T2 批 5 (syscall-followup): SYS_CREDO_DISK_INSTALL / SYS_CREDO_HOTPLUG_STATUS
        // 分支已迁至 services (services::credo::storage::disk::boot_install_syscall /
        // hotplug_status_syscall), 委托本层机制 (sys_boot_install / sys_hotplug_status).

        // ==================== 帧缓冲设备 ====================
        // T2 批 5 (syscall-followup): SYS_FB_OPEN / SYS_FB_MMAP / SYS_FB_RELEASE 分支
        // 已迁至 services (services::driver::fb::fb_*_syscall), 委托本层机制
        // (机制函数: sys_fb_open / sys_fb_mmap / sys_fb_release).

        // 未匹配的 syscall 编号
        _ => Errno::ENOSYS.as_ret(),
    }
}

// ============================================================================
// 时间
// ============================================================================

pub(crate) fn sys_nanosleep(req: u64, rem: u64) -> i64 {
    if req == 0 || !raw::check_user_ptr(req) {
        return Errno::EINVAL.as_ret();
    }
    #[repr(C)]
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    // SAFETY: `const` 由调用方保证为有效指针; 只读访问
    let ts = unsafe { core::ptr::read_volatile(req as *const Timespec) };
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return Errno::EINVAL.as_ret();
    }

    let total_ns = ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;
    if total_ns == 0 {
        return 0;
    }

    // 睡眠策略统一由 timer 机制持有 (sleep_ns), 与 SYS_clock_nanosleep 共用
    crate::framework::timer::sleep_ns(total_ns);

    let _ = rem;
    0
}

pub(crate) fn sys_kill(pid: i32, sig: i32) -> i64 {
    if !(0..=31).contains(&sig) {
        return Errno::EINVAL.as_ret();
    }
    match crate::framework::proc::do_signal_send_extended(pid, sig as u8) {
        Ok(_) => 0,
        Err(-1) => Errno::EINVAL.as_ret(),
        Err(-2) => Errno::ESRCH.as_ret(),
        Err(_) => Errno::EPERM.as_ret(),
    }
}

// ============================================================================
// 信号框架
// ============================================================================

const SIG_BLOCK: i32 = 0;
const SIG_UNBLOCK: i32 = 1;
const SIG_SETMASK: i32 = 2;

pub(crate) fn sys_rt_sigaction(signum: i32, act: u64, oact: u64) -> i64 {
    if !(1..=31).contains(&signum) {
        return Errno::EINVAL.as_ret();
    }

    let pid = match crate::framework::proc::process_get_current_pid() {
        0 => return Errno::ESRCH.as_ret(),
        p => p,
    };

    if oact != 0 {
        if !raw::check_user_buf(oact, 8) {
            return Errno::EFAULT.as_ret();
        }
        let old = crate::framework::proc::get_sigaction(pid, signum as u8);
        match old {
            // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
            Some(v) => unsafe { raw::write_u64(oact as *mut u64, v) },
            None => return Errno::EINVAL.as_ret(),
        }
    }

    if act != 0 {
        if !raw::check_user_buf(act, 8) {
            return Errno::EFAULT.as_ret();
        }
        // SAFETY: `const` 由调用方保证为有效指针; 只读访问
        let new_action = unsafe { raw::read_u64(act as *const u64) };
        match crate::framework::proc::set_sigaction(pid, signum as u8, new_action) {
            Some(_) => {}
            None => return Errno::EINVAL.as_ret(),
        }
    }

    0
}

pub(crate) fn sys_rt_sigprocmask(how: i32, set: u64, oset: u64) -> i64 {
    let pid = match crate::framework::proc::process_get_current_pid() {
        0 => return Errno::ESRCH.as_ret(),
        p => p,
    };

    if oset != 0 {
        if !raw::check_user_buf(oset, 8) {
            return Errno::EFAULT.as_ret();
        }
        let old = crate::framework::proc::get_blocked_mask(pid);
        // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
        unsafe { raw::write_u64(oset as *mut u64, old) };
    }

    if set != 0 {
        if !raw::check_user_buf(set, 8) {
            return Errno::EFAULT.as_ret();
        }
        // SAFETY: `const` 由调用方保证为有效指针; 只读访问
        let new_set = unsafe { raw::read_u64(set as *const u64) };
        let old = crate::framework::proc::get_blocked_mask(pid);
        let updated = match how {
            SIG_BLOCK => old | new_set,
            SIG_UNBLOCK => old & !new_set,
            SIG_SETMASK => new_set,
            _ => return Errno::EINVAL.as_ret(),
        };
        crate::framework::proc::set_blocked_mask(
            pid,
            crate::framework::proc::sanitize_blocked_mask(updated),
        );
    }

    0
}

pub(crate) fn sys_sigaltstack(ss: u64, old_ss: u64) -> i64 {
    use crate::framework::proc::{SS_DISABLE, SS_ONSTACK};

    let pid = match crate::framework::proc::process_get_current_pid() {
        0 => return Errno::ESRCH.as_ret(),
        p => p,
    };

    let result = crate::framework::proc::process_with_mut(pid, |proc| {
        if old_ss != 0 {
            if !raw::check_user_buf(old_ss, 24) {
                return Errno::EFAULT.as_ret();
            }
            let cur_addr = proc.sigaltstack_addr.load(Ordering::Acquire);
            let cur_size = proc.sigaltstack_size.load(Ordering::Acquire);
            let cur_flags = proc.sigaltstack_flags.load(Ordering::Acquire);
            // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
            unsafe {
                raw::write_u64(old_ss as *mut u64, cur_addr);
                raw::write_u64((old_ss + 8) as *mut u64, u64::from(cur_flags));
                raw::write_u64((old_ss + 16) as *mut u64, cur_size);
            }
        }

        if ss != 0 {
            if !raw::check_user_buf(ss, 24) {
                return Errno::EFAULT.as_ret();
            }
            // SAFETY: `const` 由调用方保证为有效指针; 只读访问
            let new_addr = unsafe { raw::read_u64(ss as *const u64) };
            let new_flags_in = unsafe { raw::read_u64((ss + 8) as *const u64) } as u32;
            let new_size = unsafe { raw::read_u64((ss + 16) as *const u64) };

            if (new_flags_in & SS_DISABLE) != 0 {
                proc.sigaltstack_addr.store(0, Ordering::Release);
                proc.sigaltstack_size.store(0, Ordering::Release);
                let cur = proc.sigaltstack_flags.load(Ordering::Acquire);
                proc.sigaltstack_flags
                    .store(cur | SS_DISABLE, Ordering::Release);
            } else {
                proc.sigaltstack_addr.store(new_addr, Ordering::Release);
                proc.sigaltstack_size.store(new_size, Ordering::Release);
                let cur = proc.sigaltstack_flags.load(Ordering::Acquire);
                proc.sigaltstack_flags
                    .store(cur & !(SS_ONSTACK | SS_DISABLE), Ordering::Release);
            }
        }
        0i64
    });

    result.unwrap_or_else(|| Errno::ESRCH.as_ret())
}

/// `arch_prctl` 机制实现 (TCB: 用户态 TLS 基址读写).
///
/// SIMPLIFIED: 仅支持 `ARCH_SET_FS` / `ARCH_GET_FS` 两个子码 (用户态 TLS 基址);
/// 其余子码 (`ARCH_SET_GS` / `ARCH_GET_GS` / `ARCH_SET_CPUID` 等) 返回 `EINVAL`.
/// 影响面: 依赖 GS 基址或 CPUID 屏蔽语义的用户态程序不可用; 何时需扩展: 引入
/// per-process `gs_base` 字段与上下文切换恢复后补齐.
///
/// aarch64 侧仅归档 `Process.tls_base` (对应 `tpidr_el0`), 当前不写系统寄存器
/// (与 `clone(CLONE_SETTLS)` 的既有口径一致).
pub(crate) fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    /// `ARCH_SET_FS` 子码 (Linux x86_64): 设置 FS 段基址.
    const ARCH_SET_FS: u64 = 0x1002;
    /// `ARCH_GET_FS` 子码 (Linux x86_64): 查询 FS 段基址.
    const ARCH_GET_FS: u64 = 0x1003;
    /// `IA32_FS_BASE` MSR 地址 (用户态 FS 段基址).
    #[cfg(target_arch = "x86_64")]
    const IA32_FS_BASE: u32 = 0xC000_0100;

    let pid = match crate::framework::proc::process_get_current_pid() {
        0 => return Errno::ESRCH.as_ret(),
        p => p,
    };

    match code {
        ARCH_SET_FS => {
            if crate::framework::proc::process_with_mut(pid, |proc| {
                proc.tls_base.store(addr, Ordering::Release);
            })
            .is_none()
            {
                return Errno::ESRCH.as_ret();
            }
            // 当前进程立即生效: 写本 CPU 的 FS 段基址 MSR.
            #[cfg(target_arch = "x86_64")]
            {
                // SAFETY: 内核态执行, IA32_FS_BASE 为用户态 FS 段基址 MSR;
                // 写入当前 CPU 仅影响当前进程的用户态 TLS 访问.
                unsafe {
                    crate::framework::cpu::msr::write_msr(IA32_FS_BASE, addr);
                }
            }
            0
        }
        ARCH_GET_FS => {
            let Some(base) = crate::framework::proc::process_with(pid, |proc| {
                proc.tls_base.load(Ordering::Acquire)
            }) else {
                return Errno::ESRCH.as_ret();
            };
            // safe 包装: 内部先 check_user_buf 校验 8 字节可写后写入.
            if !raw::write_u64_to_user(addr, base) {
                return Errno::EFAULT.as_ret();
            }
            0
        }
        _ => Errno::EINVAL.as_ret(),
    }
}

// ============================================================================
// 热插拔 / 帧缓冲
// ============================================================================

/// `sys_hotplug_status` — 读取热插拔状态 (机制: 用户 buffer 写入 + 驱动状态读取)
///
/// T2 批 5 (syscall-followup): syscall 策略入口迁至 services
/// (services::credo::storage::disk::hotplug_status_syscall), 本函数保留为
/// 机制库 (unsafe 用户指针写入), 经 framework::syscall 顶层 re-export 消费.
pub fn sys_hotplug_status(buf: *mut u8, buf_size: u32) -> i64 {
    if buf.is_null() || buf_size == 0 {
        return Errno::EINVAL.as_ret();
    }
    if !raw::check_user_buf(buf as u64, u64::from(buf_size)) {
        return Errno::EFAULT.as_ret();
    }

    let status = crate::framework::driver::hotplug::HOTPLUG_MANAGER.status();

    let mut offset: u32 = 0;

    let header: [u8; 16] = [
        u8::from(status.enabled),
        0,
        0,
        0,
        (status.slot_count & 0xFF) as u8,
        ((status.slot_count >> 8) & 0xFF) as u8,
        ((status.slot_count >> 16) & 0xFF) as u8,
        ((status.slot_count >> 24) & 0xFF) as u8,
        (status.blk_device_count & 0xFF) as u8,
        ((status.blk_device_count >> 8) & 0xFF) as u8,
        ((status.blk_device_count >> 16) & 0xFF) as u8,
        ((status.blk_device_count >> 24) & 0xFF) as u8,
        0,
        0,
        0,
        0,
    ];
    if offset + 16 > buf_size {
        return i64::from(offset);
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::ptr::copy_nonoverlapping(header.as_ptr(), buf.add(offset as usize), 16);
    }
    offset += 16;

    let slot_size: u32 = 8;
    for slot in &status.slots {
        if offset + slot_size > buf_size {
            break;
        }
        let info: [u8; 8] = [
            slot.bus,
            slot.device,
            slot.function,
            slot.slot_number,
            u8::from(slot.presence),
            (u8::from(slot.surprise_capable) << 1) | u8::from(slot.hotplug_capable),
            0,
            0,
        ];
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::ptr::copy_nonoverlapping(
                info.as_ptr(),
                buf.add(offset as usize),
                slot_size as usize,
            );
        }
        offset += slot_size;
    }

    let dev_size: u32 = 16;
    for dev in &status.blk_devices {
        if offset + dev_size > buf_size {
            break;
        }
        let info: [u8; 16] = [
            dev.drive,
            u8::from(dev.present),
            u8::from(dev.removing),
            0,
            (dev.io_count & 0xFF) as u8,
            ((dev.io_count >> 8) & 0xFF) as u8,
            ((dev.io_count >> 16) & 0xFF) as u8,
            ((dev.io_count >> 24) & 0xFF) as u8,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::ptr::copy_nonoverlapping(
                info.as_ptr(),
                buf.add(offset as usize),
                dev_size as usize,
            );
        }
        offset += dev_size;
    }

    i64::from(offset)
}

// ============================================================================
// 帧缓冲设备
// ============================================================================

/// fb_mmap 建立的映射记录 (cr3, 起始 vaddr, 页数) — 供 fb_release 解除映射
///
/// SIMPLIFIED: 仅单槽记录 (当前唯一用户 fbterm 单次 mmap); 影响面: 多进程/
/// 多映射场景 release 仅清最后一条映射; 何时需扩展: 引入 per-process 映射表
/// (per-fd fb 句柄状态) 后替换.
static FB_MAP_RECORD: crate::framework::sync::IrqSpinLock<Option<(u64, u64, u64)>> =
    crate::framework::sync::IrqSpinLock::new(None);

#[repr(C)]
#[derive(Copy, Clone)]
struct FbInfo {
    phys_addr: u64,
    size: u64,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u8,
    _pad: [u8; 3],
}

/// `sys_fb_open` — 查询帧缓冲信息 (机制: FB 驱动读取 + 用户结构写入)
///
/// T2 批 5 (syscall-followup): syscall 策略入口迁至 services
/// (services::driver::fb::fb_open_syscall), 本函数保留为机制库.
#[expect(
    clippy::missing_panics_doc,
    reason = "missing_panics_doc: get_framebuffer 守卫在 fb_addr/ENODEV 前置检查后必有值 (FB 已初始化), unwrap 不会实际触发"
)]
pub fn sys_fb_open(info_ptr: u64, _flags: u64) -> i64 {
    if info_ptr == 0 || !raw::check_user_ptr(info_ptr) {
        return Errno::EFAULT.as_ret();
    }

    let fb_addr =
        crate::framework::driver::FB_PHYS_ADDR.load(core::sync::atomic::Ordering::Acquire);
    if fb_addr == 0 {
        return Errno::ENODEV.as_ret();
    }

    let fb_size =
        crate::framework::driver::FB_PHYS_SIZE.load(core::sync::atomic::Ordering::Acquire);

    let (width, height, pitch, bpp) = match crate::framework::driver::get_framebuffer() {
        Some(guard) => {
            let fb = guard.as_ref().unwrap();
            (
                fb.width(),
                fb.height(),
                fb.pitch(),
                fb.format().bits_per_pixel() as u8,
            )
        }
        None => return Errno::ENODEV.as_ret(),
    };

    let info = FbInfo {
        phys_addr: fb_addr,
        size: fb_size,
        width,
        height,
        pitch,
        bpp,
        _pad: [0; 3],
    };

    let dst = info_ptr as *mut FbInfo;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe { raw::write_struct(dst, &info) };
    0
}

/// `sys_fb_mmap` — 建立帧缓冲页映射 (机制: 页表操作)
///
/// T2 批 5 (syscall-followup): syscall 策略入口迁至 services
/// (services::driver::fb::fb_mmap_syscall), 本函数保留为机制库
/// (页表映射, unsafe). 成功建立映射后记录到 `FB_MAP_RECORD` 供 release 解除.
pub fn sys_fb_mmap(target_vaddr: u64, size: u64, _prot: u64) -> i64 {
    if target_vaddr == 0 || target_vaddr & 0xFFF != 0 {
        return Errno::EINVAL.as_ret();
    }
    if target_vaddr > FB_MMAP_ADDR_MAX - size {
        return Errno::EINVAL.as_ret();
    }

    let fb_phys =
        crate::framework::driver::FB_PHYS_ADDR.load(core::sync::atomic::Ordering::Acquire);
    if fb_phys == 0 {
        return Errno::ENODEV.as_ret();
    }

    let fb_total =
        crate::framework::driver::FB_PHYS_SIZE.load(core::sync::atomic::Ordering::Acquire);
    if size > fb_total {
        return Errno::EINVAL.as_ret();
    }

    let cr3 = crate::framework::proc::user_proc::user_entry_cr3
        .load(core::sync::atomic::Ordering::SeqCst);
    if cr3 == 0 {
        return Errno::ENODEV.as_ret();
    }

    let vmm = crate::framework::mm::get_vmm();
    let flags = crate::framework::mm::PageFlags::PRESENT
        | crate::framework::mm::PageFlags::WRITABLE
        | crate::framework::mm::PageFlags::USER
        | crate::framework::mm::PageFlags::WRITE_THROUGH;

    let phys_page_aligned = fb_phys & !(crate::framework::mm::PAGE_SIZE - 1);
    let offset = fb_phys - phys_page_aligned;
    let pages = (size + offset).div_ceil(crate::framework::mm::PAGE_SIZE);

    for i in 0..pages {
        let pa =
            crate::framework::mm::PhysAddr(phys_page_aligned + i * crate::framework::mm::PAGE_SIZE);
        let va = crate::framework::mm::VirtAddr(target_vaddr + i * crate::framework::mm::PAGE_SIZE);
        vmm.map_page_in_table(cr3, va, pa, flags);
    }

    // 记录映射区间供 release 解除 (单槽, 见 FB_MAP_RECORD SIMPLIFIED 注释)
    *FB_MAP_RECORD.lock() = Some((cr3, target_vaddr, pages));

    target_vaddr as i64
}

/// `sys_fb_release` — 解除帧缓冲页映射 (机制: 页表操作)
///
/// T2 批 5 (syscall-followup): 自空 stub 实装 — 依 `FB_MAP_RECORD` 解除
/// `sys_fb_mmap` 建立的映射并清记录. syscall 策略入口迁至 services
/// (services::driver::fb::fb_release_syscall).
pub fn sys_fb_release(vaddr: u64) -> i64 {
    let vmm = crate::framework::mm::get_vmm();
    let mut record = FB_MAP_RECORD.lock();
    if let Some((cr3, va, pages)) = *record {
        if vaddr != va {
            return Errno::EINVAL.as_ret();
        }
        for i in 0..pages {
            vmm.unmap_page_in_table(
                cr3,
                crate::framework::mm::VirtAddr(va + i * crate::framework::mm::PAGE_SIZE),
            );
        }
        *record = None;
        0
    } else {
        Errno::EINVAL.as_ret()
    }
}

// ============================================================================
// 引导安装 (仅 x86_64)
// ============================================================================

#[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
fn write_le32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset] = val as u8;
    buf[offset + 1] = (val >> 8) as u8;
    buf[offset + 2] = (val >> 16) as u8;
    buf[offset + 3] = (val >> 24) as u8;
}

#[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
const BOOT_PART_SECTORS: u32 = 16384;

#[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// `sys_boot_install` — 引导安装 (机制: 磁盘扇区写 + stage1/内核拷贝, 仅 x86_64)
///
/// T2 批 5 (syscall-followup): syscall 策略入口迁至 services
/// (services::credo::storage::disk::boot_install_syscall), 本函数保留为机制库.
pub fn sys_boot_install(disk_id: u32) -> i64 {
    let pwm = crate::framework::credo::pwm_get_current();
    if !crate::framework::credo::pwm_has_capability(pwm, 4, 0) {
        return Errno::EACCES.as_ret();
    }
    // 产物桩化: stage1.bin 是裸机产物 (other/build/ 被 gitignore, 仅由 make 生成).
    // 判据用 target_os 而非 feature —— 本函数 gate 为 all(not(kernel_test), x86_64),
    // host target 的 host-test / kernel_test 两个 lint 维度与 host-tests 都会编译它,
    // 若直接 include_bytes 即让 host 侧编译硬依赖裸机产物 (build.rs G-06「消除隐式
    // make 耦合」的漏项). 该函数在 host 不可达 (host-tests 零引用 boot_install /
    // CREDO_DISK_INSTALL, 见 docs/plan/syscall-followup.md 可达性矩阵簇 4), 取空切片.
    #[cfg(target_os = "none")]
    let stage1 = include_bytes!("../../../../other/build/stage1.bin");
    #[cfg(not(target_os = "none"))]
    let stage1: &[u8] = &[];
    if !crate::framework::driver::hdd_is_present(disk_id as u8) {
        return Errno::ENOENT.as_ret();
    }
    let mut mbr = [0u8; 512];
    if crate::framework::driver::hdd_read_sector(disk_id as u8, 0, &mut mbr) < 0 {
        return Errno::EIO.as_ret();
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe { core::ptr::copy_nonoverlapping(stage1.as_ptr(), mbr.as_mut_ptr(), 440) };
    let total_sectors = crate::framework::driver::hdd_total_sectors(disk_id as u8);
    let nestfs_start = BOOT_PART_SECTORS;
    let nestfs_sectors = if total_sectors > u64::from(nestfs_start) + 1 {
        total_sectors - u64::from(nestfs_start)
    } else {
        0xFFFFFFFFu64
    };
    write_le32(&mut mbr, 446, 0x00000800);
    write_le32(&mut mbr, 450, 0x06FEFFFF);
    write_le32(&mut mbr, 454, 64u32);
    write_le32(&mut mbr, 458, BOOT_PART_SECTORS - 64);
    write_le32(&mut mbr, 462, nestfs_start);
    write_le32(&mut mbr, 466, 0x83FEFFFF);
    write_le32(&mut mbr, 470, nestfs_start);
    let nestfs_len = if nestfs_sectors > 0xFFFFFFFF {
        0xFFFFFFFFu32
    } else {
        nestfs_sectors as u32
    };
    write_le32(&mut mbr, 474, nestfs_len);
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    if crate::framework::driver::hdd_write_sector(disk_id as u8, 0, &mbr) < 0 {
        return Errno::EIO.as_ret();
    }
    let kernel_ptr = raw::kernel_start_ptr();
    let kernel_len = {
        const HHDM_OFFSET: usize = 0xFFFF_8000_0000_0000;
        let phys_end = raw::kernel_end_phys(HHDM_OFFSET);
        phys_end - (kernel_ptr as usize)
    };
    let total_kernel_sectors = kernel_len.div_ceil(512) as u32;
    let max_sectors = 2047u32;
    let copy_sectors = if total_kernel_sectors > max_sectors {
        max_sectors
    } else {
        total_kernel_sectors
    };
    for s in 0..copy_sectors {
        let offset = s as usize * 512;
        let remaining = kernel_len.saturating_sub(offset);
        if remaining == 0 {
            break;
        }
        let n = if remaining < 512 { remaining } else { 512 };
        let mut buf = [0u8; 512];
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::ptr::copy_nonoverlapping(kernel_ptr.add(offset), buf.as_mut_ptr(), n);
        }
        if crate::framework::driver::hdd_write_sector(disk_id as u8, u64::from(1 + s), &buf) < 0 {
            return Errno::EIO.as_ret();
        }
    }
    let mut cfg = [0u8; 512];
    cfg[0] = b'A';
    cfg[1] = b'N';
    cfg[2] = b'T';
    cfg[3] = b'X';
    write_le32(&mut cfg, 4, BOOT_PART_SECTORS);
    cfg[510] = 0x55;
    cfg[511] = 0xAA;
    crate::framework::driver::hdd_write_sector(disk_id as u8, 2046, &cfg);
    0
}
