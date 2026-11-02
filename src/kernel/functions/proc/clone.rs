#![deny(unsafe_code)]
//! clone — functions 层安全代理
//!
//! 为 clone 系统调用提供参数验证:
//! - flags 合法性检查
//! - `child_stack` 对齐检查
//! - `CLONE_VM` + `CLONE_THREAD` 必须同时设置 `CLONE_SIGHAND`
//! - `CLONE_THREAD` 必须同时设置 `CLONE_VM`
//!
//! ## 安全边界
//!
//! - functions 层验证标量参数和标志组合
//! - 页表/进程操作委托给 privileged 层 (TCB)

use crate::functions::config::memory::USER_KSTACK_SIZE;
use crate::privileged::proc::api;
use crate::privileged::proc::raw;
use crate::privileged::proc::{Process, ProcessState};
use crate::privileged::syscall::Errno;
use core::sync::atomic::Ordering;

// clone 标志位 — 单一权威源 `edgine_abi::thread`, 经 `functions::syscall::types`
// 的 glob re-export 消费 (与调度属性 ABI 同口径). 仅引入本模块实际使用项.
use crate::functions::syscall::types::{
    CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID, CLONE_FILES, CLONE_FS, CLONE_PARENT_SETTID,
    CLONE_SIGHAND, CLONE_THREAD, CLONE_VM,
};

/// clone 安全代理
///
/// 验证: flags 合法, `CLONE_VM+CLONE_THREAD` 需要 `CLONE_SIGHAND`,
/// `CLONE_THREAD` 需要 `CLONE_VM`
///
/// # Errors
///
/// - `CLONE_VM`/`CLONE_THREAD` 未同时设置 `CLONE_SIGHAND`, `CLONE_THREAD`
///   未同时设置 `CLONE_VM`, 或 `child_stack` 非零但未按 16 字节对齐 → `EINVAL`
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
        return Err(Errno::Einval);
    }

    // CLONE_THREAD 必须同时共享地址空间 (Linux: SIGHAND 又要求 VM 同时置位).
    // 缺 VM 时无法表达同一地址空间内的线程, 按非法组合拒绝而非静默走 fork 路径.
    if (flags & CLONE_THREAD) != 0 && (flags & CLONE_VM) == 0 {
        return Err(Errno::Einval);
    }

    // 共享信号动作表以共享地址空间为前提 (Linux: SIGHAND ⇒ VM).
    if (flags & CLONE_SIGHAND) != 0 && (flags & CLONE_VM) == 0 {
        return Err(Errno::Einval);
    }

    // SIMPLIFIED: CLONE_FILES 仅接受线程 (CLONE_THREAD).
    // 影响面: 非线程 clone 无法共享 fd 表 (Linux 允许); CLONE_FS 已放开 ——
    // exec 无论是否共享都先拆出私有 FS 副本 (unshare), 独立共享者不受影响.
    // 何时需扩展: 实装 fd 表 unshare (exec 前复制私有句柄表) 后再放开 CLONE_FILES;
    // 在此之前非线程共享 fd 表会让 exec 的 CLOEXEC 关闭波及共享者.
    if (flags & CLONE_FILES) != 0 && (flags & CLONE_THREAD) == 0 {
        return Err(Errno::Einval);
    }

    // SIMPLIFIED: fork 路径 (无 CLONE_VM) 不接受 CLONE_FS —— 子进程 fork 后即可
    // 在别核运行, 事后改写其 FS 上下文的 Arc 与并发 chdir 存在竞态;
    // CLONE_VM 路径的共享在子进程入队前一次闭合.
    if (flags & CLONE_FS) != 0 && (flags & CLONE_VM) == 0 {
        return Err(Errno::Einval);
    }

    // child_stack 如果非零, 必须对齐到 16 字节 (x86_64 ABI)
    if child_stack != 0 && !child_stack.is_multiple_of(16) {
        return Err(Errno::Einval);
    }

    let ret = clone_impl(flags, child_stack, parent_tidptr, child_tidptr, tls);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(crate::functions::intcast::usize_from_i64(ret))
    }
}

/// clone 实现 (0 unsafe) — 页表/进程表/用户内存写入全部经 privileged safe API.
///
/// 自 `privileged/syscall/clone.rs` 下沉; 原三处 `unsafe` 用户态 TID 写入改用
/// `privileged::syscall::api::write_struct_to_user` (I4: 用户内存经 privileged 安全代理).
///
/// `flags`: 克隆标志 (`CLONE_VM` | ...)
/// `child_stack`: 子进程的用户栈地址 (0 = 与父进程相同)
/// `parent_tidptr`: 父进程 TID 指针
/// `child_tidptr`: 子进程 TID 指针
/// `tls`: TLS 地址
///
/// 成功返回子进程 PID, 失败返回负 `Errno`.
fn clone_impl(
    flags: u64,
    child_stack: u64,
    parent_tidptr: u64,
    child_tidptr: u64,
    tls: u64,
) -> i64 {
    let parent_pid = match api::process_get_current_pid() {
        0 => return Errno::Echild.as_ret(),
        p => p,
    };

    // 如果没有 CLONE_VM, 行为等同于 fork
    if flags & CLONE_VM == 0 {
        return clone_fork_path(flags, child_stack, parent_tidptr, child_tidptr);
    }

    // CLONE_VM: 共享地址空间 (创建线程)
    clone_vm_path(
        flags,
        child_stack,
        parent_tidptr,
        child_tidptr,
        tls,
        parent_pid,
    )
}

/// fork 路径 (`flags & CLONE_VM == 0`): 复制地址空间 (COW) 并处理 TID 标志
fn clone_fork_path(flags: u64, child_stack: u64, parent_tidptr: u64, child_tidptr: u64) -> i64 {
    let child_pid = api::sys_fork();
    if child_pid == 0 {
        return Errno::Enomem.as_ret();
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
        let tid: i32 = child_pid.cast_signed();
        let _ = crate::privileged::syscall::api::write_struct_to_user(parent_tidptr, &tid);
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
    let new_ns_flags = flags & crate::privileged::proc::namespace::CLONE_NEW_ALL;
    if new_ns_flags != 0 {
        let _ = api::process_with_mut(child_pid, |p| {
            let parent_ns = {
                // 子进程已通过 fork 继承了父进程的 namespace
                // 现在根据 CLONE_NEW* 创建新实例
                let current_ns = p.namespaces.lock();
                crate::privileged::proc::NamespaceSet::clone_from(&current_ns, new_ns_flags)
            };
            *p.namespaces.lock() = parent_ns;
        });
    }

    i64::from(child_pid)
}

/// `CLONE_VM` 路径 (创建线程): 分配 PCB → 继承属性/内核栈/上下文 → 注册调度
fn clone_vm_path(
    flags: u64,
    child_stack: u64,
    parent_tidptr: u64,
    child_tidptr: u64,
    tls: u64,
    parent_pid: u32,
) -> i64 {
    let parent_cr3 = api::process_with(parent_pid, |p| p.cr3.load(Ordering::SeqCst)).unwrap_or(0);
    if parent_cr3 == 0 {
        return Errno::Enomem.as_ret();
    }

    // K-02: CLONE_THREAD 走"入组"语义; 仅 CLONE_VM 仍是共享地址空间的独立进程.
    let joins_group = flags & CLONE_THREAD != 0;

    // 分配子进程 PID
    let child_pid = api::proc_alloc_pid();
    if child_pid == 0 {
        return Errno::Enomem.as_ret();
    }

    // 登记前的任何失败都必须归还 PID 位图槽位: PCB 由各回滚分支自行释放,
    // 但 `Process::drop` 不碰位图 (只有 `remove_and_free` 才释放), 漏还即永久
    // 占用一个 PID (与 `sys_fork` 失败路径同口径).
    let Some(child_ptr) = vm_child_alloc(parent_pid, child_pid, parent_cr3, joins_group) else {
        api::proc_free_pid(child_pid);
        return Errno::Enomem.as_ret();
    };
    let child = raw::process_ref_mut(child_ptr);

    // 继承父进程属性 / 内核栈 / 上下文 (失败时内部已回滚 PCB)
    if let Err(ret) = vm_child_inherit(parent_pid, &mut *child, parent_cr3, child_stack, tls) {
        api::proc_free_pid(child_pid);
        return ret;
    }

    if let Err(ret) = vm_child_register(
        flags,
        &mut *child,
        child_pid,
        parent_pid,
        parent_tidptr,
        child_tidptr,
        joins_group,
    ) {
        // `vm_child_register` 的失败分支全部在进程表登记之前, PID 仍归本路径持有.
        api::proc_free_pid(child_pid);
        return ret;
    }

    i64::from(child_pid)
}

/// 分配子进程 PCB 并登记父进程 cr3 持有者; 失败返回 `None` (PCB 已回滚)
///
/// `joins_group` 为真时子 TCB 是父线程同组的线程: 其新组不登记父组 (线程不进入
/// 任何 `children` 列表, 父子关系归线程组, 见 K-04/K-06).
fn vm_child_alloc(
    parent_pid: u32,
    child_pid: u32,
    parent_cr3: u64,
    joins_group: bool,
) -> Option<*mut crate::privileged::proc::Process> {
    // 克隆父进程名称
    let name_str = api::process_with(parent_pid, |p| {
        let name = p.name.lock();
        alloc::string::String::clone(&*name)
    })
    .unwrap_or_default();

    // 创建子进程 (共享 CR3, 不 COW)
    // K-04: 父进程关系落在线程组上; 独立进程组挂到父线程组 tgid 名下.
    let parent_record = if joins_group {
        None
    } else {
        Some(crate::privileged::proc::ProcessId(
            api::process_with(parent_pid, Process::tgid).unwrap_or(parent_pid),
        ))
    };
    let child_ptr = raw::alloc_process(child_pid, name_str.as_str(), parent_record);
    let child = raw::process_ref_mut(child_ptr);

    // 共享地址空间: 子进程使用父进程的 CR3
    child.cr3.store(parent_cr3, Ordering::SeqCst);

    // G1 修复: 共享父进程地址空间必须登记一个持有者.
    // 父子各持同一个 PML4 物理地址 ⇒ 两者的 Process::drop 都会释放同一张页表;
    // 无持有计数时先退出者会把另一方仍在使用的页表销毁 (UAF). 计数登记处只此一处
    // (`child_ctx.cr3` 是上下文快照, 不承担所有权, 不重复计数).
    if !crate::privileged::mm::pmm::get_pmm().frame_inc(crate::privileged::mm::PhysAddr(parent_cr3))
    {
        // 登记失败 (父 cr3 未处于计数态, 契约违反): 回滚已写入的 cr3,
        // 避免子进程 drop 时误释放父进程地址空间.
        crate::slog_err!(
            Kernel,
            "clone: CLONE_VM 无法登记 cr3 持有者 (cr3={:#X}), 拒绝共享地址空间",
            parent_cr3
        );
        child.cr3.store(0, Ordering::SeqCst);
        raw::drop_boxed_process(child_ptr);
        return None;
    }

    Some(child_ptr)
}

/// 继承父进程属性、复制内核栈并初始化子进程上下文
///
/// 返回 `Err(ret)` 时 `ret` 即 `clone_impl` 的返回值 (语义与拆分前一致);
/// 失败路径释放半成品 PCB (含共享 cr3 引用).
fn vm_child_inherit(
    parent_pid: u32,
    child: &mut crate::privileged::proc::Process,
    parent_cr3: u64,
    child_stack: u64,
    tls: u64,
) -> Result<(), i64> {
    // 复制父进程属性
    let (parent_pwm, parent_sched_policy, parent_rt_priority, parent_tls_base) =
        api::process_with(parent_pid, |p| {
            (
                p.pwm.load(Ordering::SeqCst),
                p.sched_policy.load(Ordering::SeqCst),
                p.rt_priority.load(Ordering::SeqCst),
                p.tls_base.load(Ordering::Acquire),
            )
        })
        .unwrap_or((0, 0, 0, 0));
    child.pwm.store(parent_pwm, Ordering::SeqCst);
    child
        .sched_policy
        .store(parent_sched_policy, Ordering::SeqCst);
    child
        .rt_priority
        .store(parent_rt_priority, Ordering::SeqCst);
    // 未指定 CLONE_SETTLS 时子线程继承父线程的 TLS 基址 (Linux 语义)
    child.tls_base.store(parent_tls_base, Ordering::Release);

    // 分配内核栈 (用户 TCB 统一 USER_KSTACK_SIZE, 与 fork 路径和释放口径同源)
    if !child.allocate_user_kernel_stack() {
        raw::drop_boxed_process(core::ptr::from_ref(child).cast_mut());
        return Err(Errno::Enomem.as_ret());
    }

    // 复制父进程的内核栈
    vm_child_copy_kstack(parent_pid, child);

    // 上下文初始化 (分架构: x86_64 的 cr3/rax/rsp 与 aarch64 的 x29/x25/x27 复用
    // 同一偏移, 见 `arch/aarch64/context.rs` 头注释, 故必须按架构分别写)
    let Some(parent_ctx) = api::process_with(parent_pid, |p| *p.context.lock()) else {
        crate::slog_err!(Kernel, "clone: 父进程 {} 在进程表中未找到", parent_pid);
        raw::drop_boxed_process(core::ptr::from_ref(child).cast_mut());
        return Err(Errno::Esrch.as_ret());
    };
    vm_child_init_context(child, &parent_ctx, parent_cr3, child_stack, tls);

    Ok(())
}

/// 复制父进程内核栈并重写栈底 canary
fn vm_child_copy_kstack(parent_pid: u32, child: &mut crate::privileged::proc::Process) {
    let parent_kstack =
        api::process_with(parent_pid, |p| p.kernel_stack.load(Ordering::SeqCst)).unwrap_or(0);
    let child_kernel_sp = child.kernel_stack.load(Ordering::SeqCst);
    let stack_size = crate::privileged::glue::usize_from_u64(USER_KSTACK_SIZE);
    // 源/目标都从"顶部向下 size"起算: 栈顶上方不属于本栈, 正向拷贝会越界读写.
    raw::copy_kstack(
        child_kernel_sp - stack_size as u64,
        parent_kstack - stack_size as u64,
        stack_size,
    );
    crate::privileged::proc::kernel_stack_write_canary(child_kernel_sp);
}

/// 初始化子进程上下文 (共享 CR3, 子进程返回 0) 并设置 TLS 基址
fn vm_child_init_context(
    child: &mut crate::privileged::proc::Process,
    parent_ctx: &crate::privileged::proc::ProcessContext,
    parent_cr3: u64,
    child_stack: u64,
    tls: u64,
) {
    let mut child_ctx = child.context.lock();
    *child_ctx = *parent_ctx;
    // aarch64 的页表根随上下文整体复制 (TTBR0 在 `*child_ctx = *parent_ctx` 中继承),
    // 故该共享 CR3 入参仅在 x86_64 的显式 CR3 写入路径使用
    #[cfg(not(target_arch = "x86_64"))]
    let _ = parent_cr3;
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
        // `fpu_pad`(@136, EL0 的 TTBR0) 与 `es`(@104, EL1 视图根) 随 ctx
        // 复制而来: 本路径恒共享父线程页表 (上文 cr3 = parent_cr3), 二者
        // 对父子线程同值, 无需改写.
    }

    // CLONE_SETTLS: 子线程显式指定的 TLS 基址覆盖上面继承自父线程的值.
    // 切换恢复已实装: x86_64 由 process_switch_asm 在装载 fs 段后写
    // IA32_FS_BASE, aarch64 由 context_switch 写 TPIDR_EL0, 取值即本字段.
    if tls != 0 {
        child
            .tls_base
            .store(tls, core::sync::atomic::Ordering::Release);
    }
}

/// 注册子进程到进程表、写回 TID 指针并加入就绪队列
///
/// `joins_group` 为真时先加入父线程所在的线程组, 再登记进程表 —— 写入顺序保证
/// `getpid` 自首次上核起即读到正确的 tgid. 任一步失败都回滚 PCB 与组成员身份
/// (同时释放共享 cr3 引用), 不在进程表中留下半成品. 非入组的 `CLONE_VM` 子进程
/// 在登记成功后进入父线程组的 `children`, 成为可被 wait 收割的独立进程.
fn vm_child_register(
    flags: u64,
    child: &mut crate::privileged::proc::Process,
    child_pid: u32,
    parent_pid: u32,
    parent_tidptr: u64,
    child_tidptr: u64,
    joins_group: bool,
) -> Result<(), i64> {
    if joins_group {
        let Some(group) = api::process_with(parent_pid, |p| alloc::sync::Arc::clone(p.group()))
        else {
            raw::drop_boxed_process(
                core::ptr::from_ref::<crate::privileged::proc::Process>(child).cast_mut(),
            );
            return Err(Errno::Esrch.as_ret());
        };
        // 组正在 exec de_thread 或已发起组退出: 拒绝入组 (否则 de_thread 完成后
        // 会留下一个停在旧映像的线程; 组退出后会有个永不退出的幽灵线程).
        // 两者都是短暂状态, 可重试, 按 EAGAIN 返回.
        if !child.join_thread_group(group) {
            raw::drop_boxed_process(
                core::ptr::from_ref::<crate::privileged::proc::Process>(child).cast_mut(),
            );
            return Err(Errno::Eagain.as_ret());
        }
    }

    // 注册到进程表; 失败时回滚组成员身份与 PCB (含内核栈 / cr3 引用).
    if !api::process_insert(
        core::ptr::from_ref::<crate::privileged::proc::Process>(child).cast_mut(),
    ) {
        let _ = child.thread_group.leave(child_pid);
        raw::drop_boxed_process(
            core::ptr::from_ref::<crate::privileged::proc::Process>(child).cast_mut(),
        );
        return Err(Errno::Enomem.as_ret());
    }

    // 独立进程 (不带 CLONE_THREAD) 登记到父线程组的 children; 线程不进入
    // `children` — 父进程只收割线程组 (K-02/K-04), 线程退出不产生可 wait 的子进程.
    if !joins_group {
        api::process_with(parent_pid, |p| {
            p.group().add_child(child_pid);
        });
    }

    // K-03: 登记成功后才改写资源归属 —— 此前任何失败路径都无需回滚共享计数.
    vm_child_adopt_resources(flags, child, parent_pid);

    // CLONE_PARENT_SETTID
    if flags & CLONE_PARENT_SETTID != 0 && parent_tidptr != 0 {
        let tid: i32 = child_pid.cast_signed();
        let _ = crate::privileged::syscall::api::write_struct_to_user(parent_tidptr, &tid);
    }

    // CLONE_CHILD_SETTID: 写子进程 TID 到 child_tidptr (共享地址空间, 子可直接读到)
    if flags & CLONE_CHILD_SETTID != 0 && child_tidptr != 0 {
        let tid: i32 = child_pid.cast_signed();
        let _ = crate::privileged::syscall::api::write_struct_to_user(child_tidptr, &tid);
    }

    // CLONE_CHILD_CLEARTID: 登记清除地址, 子进程退出时写 0 并 futex 唤醒
    if flags & CLONE_CHILD_CLEARTID != 0 && child_tidptr != 0 {
        child
            .clear_child_tid
            .store(child_tidptr, core::sync::atomic::Ordering::Release);
    }

    // 添加到调度器
    //
    // K-06 竞态收口: 本线程可能已入组 (`join` 在组退出请求发布前成功) 但登记表
    // 晚于 `process_exit_group` 的成员快照 —— 该快照遍历到时进程表里还没有本
    // TID, SIGKILL 会落空, 组里留下一个永不退出的"幽灵线程". 入队前复查一次组
    // 退出请求: 命中即自挂 SIGKILL, 本线程返回用户态时按致命信号退出.
    if joins_group && child.thread_group.exit_requested() {
        child.signal_pending_set(u32::from(crate::privileged::proc::signal::SIGKILL));
    }
    let _ = child.set_state_safe(ProcessState::Ready);
    api::scheduler_add_to_run_queue(child_pid);
    Ok(())
}

/// 接管或复制父线程的组共享资源 (K-03).
///
/// - `CLONE_SIGHAND` / `CLONE_FILES` / `CLONE_FS` 置位时与父线程共享同一实例;
/// - 未置位时按 fork 语义深拷贝快照 (fd 表逐句柄递增 `OpenFile` 引用).
///
/// 调用点在子 TCB 登记成功之后、置 `Ready` 之前: 子进程已可被进程表查到, 但
/// 尚未进入运行队列, 资源归属改写无并发读者; 此调用之后无失败路径, 不需要
/// 回滚已递增的共享计数.
fn vm_child_adopt_resources(
    flags: u64,
    child: &mut crate::privileged::proc::Process,
    parent_pid: u32,
) {
    let _ = api::process_with(parent_pid, |p| {
        if flags & CLONE_SIGHAND != 0 {
            child.share_signal_actions(&p.sigaction_table);
        } else {
            child.copy_signal_actions_from(&p.sigaction_table);
        }
        if flags & CLONE_FILES != 0 {
            child.share_fd_table(&p.fd_table);
        } else {
            // 递增必须在持源表锁期间完成, 否则并发 close 可先释放该句柄.
            let ops = crate::privileged::fs::current_vfs_ops();
            child.copy_fd_table_from(&p.fd_table, |handle| ops.inc_open_file_ref(handle));
        }
        if flags & CLONE_FS != 0 {
            child.share_fs_context(&p.fs_context);
        } else {
            child.copy_fs_context_from(&p.fs_context);
        }
    });
}

/// `set_robust_list(head, len)` 策略 — 登记当前进程的 robust futex 链表头
///
/// `len` 必须为 `sizeof(struct robust_list_head)` = 24.
/// 登记后由退出路径 (`privileged::proc::robust::exit_cleanup`) 消费:
/// 进程退出时遍历链表, 对本进程持有的 futex 置 `FUTEX_OWNER_DIED` 并唤醒.
///
/// # Errors
///
/// - `len != 24` → `EINVAL`
/// - 当前进程不存在 → `ESRCH`
pub fn set_robust_list_syscall(head: u64, len: u64) -> Result<usize, Errno> {
    const ROBUST_LIST_HEAD_SIZE: u64 = 24;
    if len != ROBUST_LIST_HEAD_SIZE {
        return Err(Errno::Einval);
    }

    let pid = crate::privileged::proc::api::process_get_current_pid();
    crate::privileged::proc::api::process_with(pid, |p| {
        p.robust_head.store(head, Ordering::Release);
        p.robust_len.store(
            crate::functions::intcast::low_u32_from_u64(ROBUST_LIST_HEAD_SIZE),
            Ordering::Release,
        );
    })
    .ok_or(Errno::Esrch)?;
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
        return Err(Errno::Esrch);
    }

    let target = if pid == 0 {
        crate::privileged::proc::api::process_get_current_pid()
    } else {
        pid.cast_unsigned()
    };
    let (head, len) = crate::privileged::proc::api::process_with(target, |p| {
        (
            p.robust_head.load(Ordering::Acquire),
            p.robust_len.load(Ordering::Acquire),
        )
    })
    .ok_or(Errno::Esrch)?;

    if head_ptr != 0 && !crate::privileged::syscall::api::write_struct_to_user(head_ptr, &head) {
        return Err(Errno::Efault);
    }
    if len_ptr != 0 && !crate::privileged::syscall::api::write_struct_to_user(len_ptr, &len) {
        return Err(Errno::Efault);
    }
    Ok(0)
}

/// `arch_prctl(code, addr)` 安全代理 — 用户态 TLS 基址读写
///
/// 委托 privileged `sys_arch_prctl` (TCB: x86_64 写 `MSR_FS_BASE` /
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
    let ret = crate::privileged::syscall::api::sys_arch_prctl(code, addr);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(crate::functions::intcast::usize_from_i64(ret))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_thread_without_vm_is_rejected() {
        let ret = clone_syscall(CLONE_THREAD | CLONE_SIGHAND, 0, 0, 0, 0);
        assert!(matches!(ret, Err(Errno::Einval)));
    }

    #[test]
    fn clone_thread_without_sighand_is_rejected() {
        let ret = clone_syscall(CLONE_THREAD | CLONE_VM, 0, 0, 0, 0);
        assert!(matches!(ret, Err(Errno::Einval)));
    }

    #[test]
    fn clone_vm_without_sighand_is_rejected() {
        let ret = clone_syscall(CLONE_VM, 0, 0, 0, 0);
        assert!(matches!(ret, Err(Errno::Einval)));
    }
}
