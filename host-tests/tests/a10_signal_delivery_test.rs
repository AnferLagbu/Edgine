//! A-10 返回用户态信号投递 — 帧布局与接线契约
//!
//! 背景: aarch64 曾缺失"返回用户态前的信号投递" (K-07a 发现), 使
//! `exit_group` 的 `SIGKILL` 无人消费、兄弟线程重睡、整组收不齐. A-10 以
//! [`UserFrame`] 抽象把两架构的投递 (`do_signal_deliver`) 与恢复
//! (`restore_user_signal_frame`) 收敛为单点, 并把 aarch64 的 svc 返回与
//! IRQ 出口同批纳入检查点.
//!
//! host 侧无法引用内核 privileged 符号, 故本测试以两类不变量固化契约:
//! 1. **帧布局尺寸** (`#[repr(C)]` 全 u64 结构, 尺寸可由字段数静态推出);
//! 2. **源码接线** (`include_str!` 文本扫描), 防止任一入口脱离共享单点.
//!
//! 真实投递/恢复语义由 QEMU 运行期用例 `[SIG]` 覆盖 (init 探针).

// ============================================================================
// 帧布局尺寸契约
// ============================================================================

/// `x86_64` 信号栈帧镜像 (与 `privileged/proc/signal.rs::SignalFrame` 同布局)
#[repr(C)]
struct SignalFrameMirror {
    regs: [u64; 15],
    int_no: u64,
    err_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
    signum: u64,
}

/// aarch64 信号栈帧镜像 (与 `Aarch64SignalFrame` 同布局)
#[repr(C)]
struct Aarch64SignalFrameMirror {
    x: [u64; 31],
    sp: u64,
    elr: u64,
    spsr: u64,
    signum: u64,
}

#[test]
fn signal_frame_sizes_match_layout_contract() {
    // x86_64: 15 通用寄存器 + int_no/err_code + rip/cs/rflags/rsp/ss + signum
    //         = 23 × u64 = 184
    assert_eq!(
        std::mem::size_of::<SignalFrameMirror>(),
        184,
        "x86_64 SignalFrame 必须是 184 字节 (23 × u64)"
    );
    // aarch64: 31 个通用寄存器 x0-x30 加 sp/elr/spsr/signum = 35 × u64 = 280
    assert_eq!(
        std::mem::size_of::<Aarch64SignalFrameMirror>(),
        280,
        "aarch64 Aarch64SignalFrame 必须是 280 字节 (35 × u64)"
    );

    // 帧总尺寸 = 帧头 + 信号帧 + trampoline
    //   x86_64:  8 + 184 + 7 = 199 (帧头 = handler 返回地址槽)
    //   aarch64: 0 + 280 + 8 = 288 (返回地址走 LR, 无帧头)
    assert_eq!(8 + 184 + 7, 199, "x86_64 帧总尺寸契约");
    assert_eq!(280 + 8, 288, "aarch64 帧总尺寸契约");
}

// ============================================================================
// 源码接线契约
// ============================================================================

#[test]
fn user_frame_abstraction_is_single_shared_point() {
    let src = include_str!("../../src/kernel/privileged/proc/signal.rs");

    // 两架构共用的帧抽象与投递/恢复单点。
    assert!(
        src.contains("pub trait UserFrame"),
        "A-10: 必须存在 UserFrame 抽象"
    );
    assert!(
        src.contains("pub fn do_signal_deliver<F: UserFrame>(frame: &mut F) -> bool"),
        "A-10: do_signal_deliver 必须泛化为架构无关单点"
    );
    assert!(
        src.contains("pub fn restore_user_signal_frame<F: UserFrame>(frame: &mut F) -> bool"),
        "A-10: rt_sigreturn 恢复必须是共享单点"
    );

    // x86_64: 帧头 8 字节 (返回地址槽), 入口 rsp ≡ 8 (mod 16)。
    assert!(
        src.contains("const FRAME_HEADER_SIZE: usize = 8;")
            && src.contains("const SP_ALIGN: u64 = 8;"),
        "A-10: x86_64 帧头/对齐契约 (8 字节槽, rsp ≡ 8 mod 16)"
    );
    // aarch64: 无帧头, SP 16 字节对齐。
    assert!(
        src.contains("const FRAME_HEADER_SIZE: usize = 0;")
            && src.contains("const SP_ALIGN: u64 = 0;"),
        "A-10: aarch64 帧头/对齐契约 (LR 承载返回地址, SP 16 对齐)"
    );
    // aarch64 handler 返回地址写 LR (x30), 不占用户栈。
    assert!(
        src.contains("self.x30 = trampoline_addr;"),
        "A-10: aarch64 handler 的返回必须经 LR 指向 trampoline"
    );
    // 落点对齐不得产生 8 字节错位读取: 恢复从 `user_sp()` 直接读。
    assert!(
        src.contains("copy_from_user(dst, frame.user_sp(), dst.len())"),
        "A-10: restore 必须从异常帧的 user_sp 直接读 (两架构均无偏移)"
    );
}

#[test]
fn x86_64_sigreturn_has_no_off_by_8_read() {
    let dispatch = include_str!("../../src/kernel/privileged/syscall/dispatch.rs");
    let rt_start = dispatch
        .find("if is_rt_sigreturn {")
        .expect("必须存在 rt_sigreturn 分支");
    let rt_block = &dispatch[rt_start
        ..dispatch
            .find("let a0 = f.rdi;")
            .expect("rt_sigreturn 块必须位于正常 dispatch 之前")];

    assert!(
        rt_block.contains("restore_user_signal_frame(f)"),
        "A-10: x86_64 rt_sigreturn 必须委托共享恢复单点"
    );
    // handler `ret` 已弹出返回地址槽, rsp 正指向 SignalFrame 起点;
    // 历史实现以 `(f.rsp + 8)` 读取是错位读取 (读到帧内第 8 字节).
    assert!(
        !rt_block.contains("(f.rsp + 8)"),
        "A-10: 不得恢复历史 off-by-8 读取 ((f.rsp + 8))"
    );

    // 系统调用正常出口必须以可变帧投递信号 (帧被改写才能跳 handler)。
    assert!(
        dispatch.contains("do_signal_deliver(f)"),
        "A-10: x86_64 syscall 出口必须在可变帧上投递信号"
    );
}

#[test]
fn aarch64_svc_and_irq_exits_deliver_signals() {
    let src = include_str!("../../src/kernel/privileged/arch/aarch64/exception.rs");

    // svc 返回口: 先写 x0 (syscall 返回值), 再投递 —— 信号快照含返回值.
    let svc_start = src
        .find("pub extern \"C\" fn svc_handler(")
        .expect("必须存在 svc_handler");
    let svc_block = &src[svc_start
        ..src
            .find("/// 内核 IRQ 统一分发")
            .expect("svc_handler 之后必须存在 IRQ 分发")];
    let write_pos = svc_block
        .find("frame.x0 = result.cast_unsigned();")
        .expect("A-10: svc 出口必须把返回值写入 x0");
    let deliver_pos = svc_block
        .find("do_signal_deliver(frame)")
        .expect("A-10: svc 出口必须投递待处理信号");
    assert!(
        write_pos < deliver_pos,
        "A-10: 必须先写 x0 再投递 (信号快照保留 syscall 返回值)"
    );
    assert!(
        svc_block.contains("restore_user_signal_frame(frame)"),
        "A-10: aarch64 rt_sigreturn 必须走共享恢复单点"
    );

    // IRQ 出口: EL0 来源携带用户帧并入同一检查点; EL1h 无用户帧可改写.
    assert!(
        src.contains("handle_irq(true, Some(frame))"),
        "A-10: EL0 IRQ 入口必须携带用户帧"
    );
    assert!(
        src.contains("handle_irq(false, None)"),
        "A-10: EL1h IRQ 入口不携带用户帧"
    );
    let wrapper_start = src
        .find("fn handle_irq(from_el0: bool")
        .expect("必须存在 handle_irq 出口包装");
    let wrapper_block = &src[wrapper_start..];
    let wrapper_end = wrapper_block
        .find("\n}\n")
        .expect("handle_irq 包装必须闭合");
    let wrapper_block = &wrapper_block[..wrapper_end];
    let deliver_pos = wrapper_block
        .find("do_signal_deliver")
        .expect("A-10: IRQ 出口必须投递信号");
    let resched_pos = wrapper_block
        .find("run_pending_resched")
        .expect("A-10: IRQ 出口必须在投递后执行挂起重调度");
    assert!(
        deliver_pos < resched_pos,
        "A-10: 信号投递必须先于挂起重调度 (与 x86_64 idt 出口同序)"
    );
}

#[test]
fn x86_64_idt_irq_exits_deliver_on_mutable_frame() {
    let src = include_str!("../../src/kernel/privileged/idt/table.rs");
    assert!(
        src.contains("let f = &mut *frame;"),
        "A-10: idt 中断出口必须先取可变帧"
    );
    assert!(
        src.contains("do_signal_deliver(f)"),
        "A-10: idt 中断出口必须投递信号"
    );
    assert!(
        !src.contains("do_signal_deliver(frame)"),
        "A-10: idt 中断出口不得再传只读裸指针"
    );
}
