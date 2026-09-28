#![deny(unsafe_code)]
//! clone — services 层安全代理
//!
//! 为 clone 系统调用提供参数验证:
//! - flags 合法性检查
//! - `child_stack` 对齐检查
//! - `CLONE_VM` + `CLONE_THREAD` 必须同时设置 `CLONE_SIGHAND`
//!
//! ## 安全边界
//!
//! - services 层验证标量参数和标志组合
//! - 页表/进程操作委托给 framework 层 (TCB)

use crate::framework::proc::ProcessState;
use crate::framework::proc::api;
use crate::framework::proc::raw;
use crate::framework::syscall::Errno;
use core::sync::atomic::Ordering;

// clone 标志位 — 仅保留本模块实际消费项; 完整集合见 Linux `<linux/sched.h>`,
// 其余标志当前无实现路径 (F9 死代码零容忍, 不预留).
const CLONE_VM: u64 = 0x00000100; // 共享地址空间 (线程)
const CLONE_SIGHAND: u64 = 0x00000800; // 共享信号处理
const CLONE_THREAD: u64 = 0x00010000; // 同一线程组
const CLONE_PARENT_SETTID: u64 = 0x00100000; // 写 TID 到 parent tidptr
const CLONE_CHILD_CLEARTID: u64 = 0x00200000; // 子进程退出时清 tidptr
const CLONE_CHILD_SETTID: u64 = 0x01000000; // 写 TID 到 child tidptr

/// clone 安全代理
///
/// 验证: flags 合法, `CLONE_VM+CLONE_THREAD` 需要 `CLONE_SIGHAND`
///
/// # Errors
///
/// - `CLONE_VM`/`CLONE_THREAD` 未同时设置 `CLONE_SIGHAND`, 或 `child_stack`
///   非零但未按 16 字节对齐 → `EINVAL`
/// - 底层 clone 返回负值时转换为对应的 `Errno`
pub fn clone_syscall(
    flags: u64,
    child_stack: u64,
    parent_tidptr: u64,
    child_tidptr: u64,
    tls: u64,
) -> Result<usize, Errno> {
    // CLONE_VM + CLONE_THREAD 必须同时设置 CLONE_SIGHAND (POSIX 线程要求)
    // 注: Rust 中 `&` 优先级高于 `==`, 括号仅为明确语义 (B05-31 审计复核).
    if ((flags & CLONE_VM) != 0 || (flags & CLONE_THREAD) != 0) && (flags & CLONE_SIGHAND) == 0 {
        return Err(Errno::EINVAL);
    }

    // child_stack 如果非零, 必须对齐到 16 字节 (x86_64 ABI)
    if child_stack != 0 && !child_stack.is_multiple_of(16) {
        return Err(Errno::EINVAL);
    }

    let ret = clone_impl(flags, child_stack, parent_tidptr, child_tidptr, tls);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// clone 实现 (0 unsafe) — 页表/进程表/用户内存写入全部经 framework safe API.
///
/// 自 `framework/syscall/clone.rs` 下沉; 原三处 `unsafe` 用户态 TID 写入改用
/// `framework::syscall::api::write_struct_to_user` (I4: 用户内存经 framework 安全代理).
///
/// `flags`: 克隆标志 (`CLONE_VM` | ...)
/// `child_stack`: 子进程的用户栈地址 (0 = 与父进程相同)
/// `parent_tidptr`: 父进程 TID 指针
/// `child_tidptr`: 子进程 TID 指针
/// `tls`: TLS 地址
///
/// 成功返回子进程 PID, 失败返回负 `Errno`.
fn clone_impl(flags: u64, child_stack: u64, parent_tidptr: u64, child_tidptr: u64, tls: u64) -> i64 {
    let parent_pid = match api::process_get_current_pid() {
        0 => return Errno::ECHILD.as_ret(),
        p => p,
    };

    // 如果没有 CLONE_VM, 行为等同于 fork
    if flags & CLONE_VM == 0 {
        let child_pid = api::sys_fork();
        if child_pid == 0 {
            return Errno::ENOMEM.as_ret();
        }

        // 如果指定了 child_stack, 修改子进程的 RSP
        if child_stack != 0 {
            let _ = api::process_with_mut(child_pid, |p| {
                let mut ctx = p.context.lock();
                ctx.rsp = child_stack;
            });
        }

        // CLONE_PARENT_SETTID: 写 TID 到 parent_tidptr
        if flags & CLONE_PARENT_SETTID != 0 && parent_tidptr != 0 {
            let tid: i32 = child_pid as i32;
            let _ = crate::framework::syscall::api::write_struct_to_user(parent_tidptr, &tid);
        }

        // CLONE_CHILD_CLEARTID: 登记清除地址, 退出时写 0 并 futex 唤醒.
        // fork 路径不处理 CLONE_CHILD_SETTID (父上下文写入会落在父地址空间,
        // 子进程 COW 后不可见; Linux 同样仅在子上下文/共享地址空间写).
        if flags & CLONE_CHILD_CLEARTID != 0 && child_tidptr != 0 {
            let _ = api::process_with_mut(child_pid, |p| {
                p.clear_child_tid
                    .store(child_tidptr, core::sync::atomic::Ordering::Release);
            });
        }

        // D1: CLONE_NEW* — 为子进程创建新 namespace
        let new_ns_flags = flags & crate::framework::proc::namespace::CLONE_NEW_ALL;
        if new_ns_flags != 0 {
            let _ = api::process_with_mut(child_pid, |p| {
                let parent_ns = {
                    // 子进程已通过 fork 继承了父进程的 namespace
                    // 现在根据 CLONE_NEW* 创建新实例
                    let current_ns = p.namespaces.lock();
                    crate::framework::proc::NamespaceSet::clone_from(&current_ns, new_ns_flags)
                };
                *p.namespaces.lock() = parent_ns;
            });
        }

        return i64::from(child_pid);
    }

    // CLONE_VM: 共享地址空间 (创建线程)
    let parent_cr3 = api::process_with(parent_pid, |p| p.cr3.load(Ordering::SeqCst)).unwrap_or(0);
    if parent_cr3 == 0 {
        return Errno::ENOMEM.as_ret();
    }

    // 分配子进程 PID
    let child_pid = api::proc_alloc_pid();
    if child_pid == 0 {
        return Errno::ENOMEM.as_ret();
    }

    // 克隆父进程名称
    let name_str = api::process_with(parent_pid, |p| {
        let name = p.name.lock();
        alloc::string::String::clone(&*name)
    })
    .unwrap_or_default();

    // 创建子进程 (共享 CR3, 不 COW)
    let child_ptr = raw::alloc_process(
        child_pid,
        name_str.as_str(),
        Some(crate::framework::proc::ProcessId(parent_pid)),
    );
    let child = raw::process_ref_mut(child_ptr);

    // 共享地址空间: 子进程使用父进程的 CR3
    child.cr3.store(parent_cr3, Ordering::SeqCst);

    // G1 修复: 共享父进程地址空间必须登记一个持有者.
    // 父子各持同一个 PML4 物理地址 ⇒ 两者的 Process::drop 都会释放同一张页表;
    // 无持有计数时先退出者会把另一方仍在使用的页表销毁 (UAF). 计数登记处只此一处
    // (`child_ctx.cr3` 是上下文快照, 不承担所有权, 不重复计数).
    if !crate::framework::mm::pmm::get_pmm().frame_inc(crate::framework::mm::PhysAddr(parent_cr3)) {
        // 登记失败 (父 cr3 未处于计数态, 契约违反): 回滚已写入的 cr3,
        // 避免子进程 drop 时误释放父进程地址空间.
        crate::slog_err!(
            Kernel,
            "clone: CLONE_VM 无法登记 cr3 持有者 (cr3={:#X}), 拒绝共享地址空间",
            parent_cr3
        );
        child.cr3.store(0, Ordering::SeqCst);
        raw::drop_boxed_process(child_ptr);
        return Errno::ENOMEM.as_ret();
    }

    // 复制父进程属性
    let (parent_pwm, parent_sched_policy, parent_rt_priority) =
        api::process_with(parent_pid, |p| {
            (
                p.pwm.load(Ordering::SeqCst),
                p.sched_policy.load(Ordering::SeqCst),
                p.rt_priority.load(Ordering::SeqCst),
            )
        })
        .unwrap_or((0, 0, 0));
    child.pwm.store(parent_pwm, Ordering::SeqCst);
    child
        .sched_policy
        .store(parent_sched_policy, Ordering::SeqCst);
    child
        .rt_priority
        .store(parent_rt_priority, Ordering::SeqCst);

    // 添加到父进程的子进程列表
    api::process_with_mut(parent_pid, |p| {
        p.children
            .lock()
            .push(crate::framework::proc::ProcessId(child_pid));
    });

    // 分配内核栈
    if !child.allocate_kernel_stack() {
        raw::drop_boxed_process(child_ptr);
        return Errno::ENOMEM.as_ret();
    }

    // 复制父进程的内核栈
    {
        let parent_kstack =
            api::process_with(parent_pid, |p| p.kernel_stack.load(Ordering::SeqCst)).unwrap_or(0);
        let child_kstack = child.kernel_stack.load(Ordering::SeqCst);
        let stack_size: usize = 65536;
        raw::copy_kstack(child_kstack, parent_kstack, stack_size);
        crate::framework::proc::kernel_stack_write_canary(child_kstack);
    }

    // 上下文初始化 (分架构: x86_64 的 cr3/rax/rsp 与 aarch64 的 x29/x25/x27 复用
    // 同一偏移, 见 `arch/aarch64/context.rs` 头注释, 故必须按架构分别写)
    let parent_ctx = if let Some(ctx) = api::process_with(parent_pid, |p| *p.context.lock()) {
        ctx
    } else {
        crate::slog_err!(Kernel, "clone: 父进程 {} 在进程表中未找到", parent_pid);
        return Errno::ESRCH.as_ret();
    };
    {
        let mut child_ctx = child.context.lock();
        *child_ctx = parent_ctx;
        #[cfg(target_arch = "x86_64")]
        {
            child_ctx.cr3 = parent_cr3; // 共享 CR3
            child_ctx.rax = 0; // 子进程返回 0
            // 如果指定了 child_stack, 修改 RSP
            if child_stack != 0 {
                child_ctx.rsp = child_stack;
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // KPTI-17: 子线程 x0 = 0 (clone 返回值); 用户栈指针在 @128 (SP_EL0).
            child_ctx.extra_regs[0] = 0;
            if child_stack != 0 {
                child_ctx.ss = child_stack;
            }
            // SP_EL1 (@96) 必须是子线程**自己的**内核栈顶: 沿用父线程的值会
            // 让子线程首次陷入 EL1 时把异常帧压进父线程的栈页.
            child_ctx.ds = child.kernel_stack.load(Ordering::SeqCst) & !0xF;
            // `_fpu_pad`(@136, EL0 的 TTBR0) 与 `es`(@104, EL1 视图根) 随 ctx
            // 复制而来: 本路径恒共享父线程页表 (上文 cr3 = parent_cr3), 二者
            // 对父子线程同值, 无需改写.
        }

        // CLONE_SETTLS: 设置子进程 TLS 基址
        // 当前仅存储于 Process.tls_base, 切换恢复未实装; x86_64 需在切换时写
        // MSR_FS_BASE、aarch64 写 tpidr_el0 (无用户态依赖, 为 Linux 线程
        // 兼容面预留, 待用户态线程库出现时实装)
        if tls != 0 {
            child
                .tls_base
                .store(tls, core::sync::atomic::Ordering::Release);
        }
    }

    // 注册到进程表
    api::process_insert(
        child as *const crate::framework::proc::Process as *mut crate::framework::proc::Process,
    );

    // CLONE_PARENT_SETTID
    if flags & CLONE_PARENT_SETTID != 0 && parent_tidptr != 0 {
        let tid: i32 = child_pid as i32;
        let _ = crate::framework::syscall::api::write_struct_to_user(parent_tidptr, &tid);
    }

    // CLONE_CHILD_SETTID: 写子进程 TID 到 child_tidptr (共享地址空间, 子可直接读到)
    if flags & CLONE_CHILD_SETTID != 0 && child_tidptr != 0 {
        let tid: i32 = child_pid as i32;
        let _ = crate::framework::syscall::api::write_struct_to_user(child_tidptr, &tid);
    }

    // CLONE_CHILD_CLEARTID: 登记清除地址, 子进程退出时写 0 并 futex 唤醒
    if flags & CLONE_CHILD_CLEARTID != 0 && child_tidptr != 0 {
        child
            .clear_child_tid
            .store(child_tidptr, core::sync::atomic::Ordering::Release);
    }

    // 添加到调度器
    let _ = child.set_state_safe(ProcessState::Ready);
    api::scheduler_add_to_run_queue(child_pid);

    crate::slog_debug!(
        Process,
        "[clone] parent={} child={} flags=0x{:X} (CLONE_VM)",
        parent_pid,
        child_pid,
        flags
    );

    i64::from(child_pid)
}

/// `set_robust_list(head, len)` 策略 — 登记当前进程的 robust futex 链表头
///
/// `len` 必须为 `sizeof(struct robust_list_head)` = 24.
/// 登记后由退出路径 (`framework::proc::robust::exit_cleanup`) 消费:
/// 进程退出时遍历链表, 对本进程持有的 futex 置 `FUTEX_OWNER_DIED` 并唤醒.
///
/// # Errors
///
/// - `len != 24` → `EINVAL`
/// - 当前进程不存在 → `ESRCH`
pub fn set_robust_list_syscall(head: u64, len: u64) -> Result<usize, Errno> {
    const ROBUST_LIST_HEAD_SIZE: u64 = 24;
    if len != ROBUST_LIST_HEAD_SIZE {
        return Err(Errno::EINVAL);
    }

    let pid = crate::framework::proc::api::process_get_current_pid();
    crate::framework::proc::api::process_with(pid, |p| {
        p.robust_head.store(head, Ordering::Release);
        p.robust_len
            .store(ROBUST_LIST_HEAD_SIZE as u32, Ordering::Release);
    })
    .ok_or(Errno::ESRCH)?;
    Ok(0)
}

/// `get_robust_list(pid, head_ptr, len_ptr)` 策略 — 查询目标进程的 robust list 登记
///
/// `pid == 0` 表示当前进程. `head_ptr`/`len_ptr` 为 NULL 时跳过对应写回
/// (Linux 允许).
///
// SIMPLIFIED: 不做跨进程读权限校验 (Linux 要求 PTRACE_MODE_READ); 当前缺
// ptrace attach 关系跟踪 (uid/capability 模型已具备, 无统一判定入口);
// 影响面: 任意进程可探测他进程是否登记 robust list (仅元数据, 非内存内容);
// 何时需扩展: 引入 ptrace attach 关系跟踪与统一判定入口后补 EPERM 判定.
///
/// # Errors
///
/// - `pid < 0` (无负 pid 语义) 或目标进程不存在 → `ESRCH`
/// - 写回失败 → `EFAULT`
pub fn get_robust_list_syscall(pid: i32, head_ptr: u64, len_ptr: u64) -> Result<usize, Errno> {
    if pid < 0 {
        return Err(Errno::ESRCH);
    }

    let target = if pid == 0 {
        crate::framework::proc::api::process_get_current_pid()
    } else {
        pid as u32
    };
    let (head, len) = crate::framework::proc::api::process_with(target, |p| {
        (
            p.robust_head.load(Ordering::Acquire),
            p.robust_len.load(Ordering::Acquire),
        )
    })
    .ok_or(Errno::ESRCH)?;

    if head_ptr != 0 && !crate::framework::syscall::api::write_struct_to_user(head_ptr, &head) {
        return Err(Errno::EFAULT);
    }
    if len_ptr != 0 && !crate::framework::syscall::api::write_struct_to_user(len_ptr, &len) {
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

/// `arch_prctl(code, addr)` 安全代理 — 用户态 TLS 基址读写
///
/// 委托 framework `sys_arch_prctl` (TCB: x86_64 写 `MSR_FS_BASE` /
/// aarch64 仅归档 `tls_base`, 二者均更新当前进程 TLS 基址).
///
/// SIMPLIFIED: 仅转发 `ARCH_SET_FS` / `ARCH_GET_FS` 两个码; 影响面:
/// `ARCH_MAP_VDSO_*` / `ARCH_GET_CPUID` 等码返回 `EINVAL`; 何时需扩展:
/// 用户态出现 vdso 地址协商或 cpuid 查询需求时补码表.
///
/// # Errors
///
/// - 底层返回负值 → 转换为对应 `Errno` (如 `ESRCH`/`EFAULT`/`EINVAL`)
pub fn arch_prctl_syscall(code: u64, addr: u64) -> Result<usize, Errno> {
    let ret = crate::framework::syscall::api::sys_arch_prctl(code, addr);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}
