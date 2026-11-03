//! sigaltstack 替代栈信号投递契约测试 (P1-I-45)
//!
//! 验证信号投递路径对 sigaltstack 的支持:
//! 1. 进程设置 sigaltstack (addr!=0, size>=frame) 后,
//!    `do_signal_deliver` 必须使用替代栈顶部, 不写主栈 (主栈溢出场景不死锁)
//! 2. `SS_ONSTACK` 标记位: 进入信号 handler 前置位, 防止重入信号再次落回替代栈
//! 3. `SS_DISABLE` 标记位: 用户禁用替代栈时, 投递回退到主栈
//! 4. 替代栈容量不足时, 回退到主栈
//! 5. sigreturn 时清除 `SS_ONSTACK` 标记 (允许下一次信号再次落回替代栈)
//!
//! ## B08-20 处置 (2026-09-06): 算法镜像移除, 静态契约保留
//!
//! 原 `pick_frame_rsp` 镜像 `do_signal_deliver` 的 `use_alternate` 决策
//! (privileged/proc/signal.rs:552-567). 评估结论: **该决策 host 不可直接测** —
//! 它内联于 `do_signal_deliver` 函数体, 依赖全局 `PROCESS_TABLE` (当前进程
//! `sigaltstack_*` 字段) + `InterruptFrame` 指针 + `do_signal_default_action`
//! (可能终止进程), 无法在 host 环境以函数形式调用.
//!
//! 按 B08-20/21 消并规则: 本地 `MockSigaltstack` / `pick_frame_rsp` / `make_ss`
//! 平行实现与对应用例已删除. **保留**已有效的 `include_str` 静态契约 (源码文本
//! 扫描, 验证 signal.rs 实现替代栈字段读取/决策/sigreturn 清位), 真实投递
//! 语义由 QEMU 集成测试覆盖.
//!
//! 待内核将 `use_alternate` 决策提炼为 pub 纯函数后可恢复 host 侧算法验证 (记录待办).

#[test]
fn source_signal_uses_sigaltstack() {
    // P1-I-45 源码静态扫描: signal.rs 必须实现替代栈判定
    let source = include_str!("../../src/kernel/privileged/proc/signal.rs");
    assert!(
        source.contains("sigaltstack_addr")
            && source.contains("sigaltstack_size")
            && source.contains("sigaltstack_flags"),
        "P1-I-45: signal.rs 必须读取 sigaltstack 字段"
    );
    assert!(
        source.contains("use_alternate")
            && source.contains("SS_DISABLE")
            && source.contains("SS_ONSTACK"),
        "P1-I-45: signal.rs 必须实现 SS_DISABLE / SS_ONSTACK 决策"
    );
    assert!(
        source.contains("ss_addr + ss_size - total as u64"),
        "P1-I-45: 替代栈顶部必须为 ss_addr + ss_size - total"
    );
}

#[test]
fn source_syscall_clears_onstack_on_sigreturn() {
    // P1-I-45 源码静态扫描 + A-10 收敛: 两架构的 rt_sigreturn 入口
    // (x86_64 dispatch.rs / aarch64 exception.rs) 都必须委托
    // `restore_user_signal_frame`, 由该单点清除 SS_ONSTACK.
    let signal_src = include_str!("../../src/kernel/privileged/proc/signal.rs");
    let restore_start = signal_src
        .find("pub fn restore_user_signal_frame")
        .expect("必须存在 restore_user_signal_frame 单点恢复函数");
    let restore_block = &signal_src[restore_start..];
    let restore_end = restore_block
        .find("\n}\n")
        .expect("restore_user_signal_frame 函数体必须有结束");
    let restore_block = &restore_block[..restore_end];
    assert!(
        restore_block.contains("sigaltstack_flags") && restore_block.contains("!SS_ONSTACK"),
        "P1-I-45: restore_user_signal_frame 必须清除 SS_ONSTACK 标记"
    );

    let dispatch_src = include_str!("../../src/kernel/privileged/syscall/dispatch.rs");
    let rt_start = dispatch_src
        .find("if is_rt_sigreturn {")
        .expect("必须存在 rt_sigreturn 特殊处理路径");
    let rt_block = &dispatch_src[rt_start
        ..dispatch_src
            .find("let a0 = f.rdi;")
            .expect("必须存在 rt_sigreturn 块结束标记")];
    assert!(
        rt_block.contains("restore_user_signal_frame"),
        "P1-I-45: x86_64 rt_sigreturn 必须走共享恢复单点 (SS_ONSTACK 清除随迁)"
    );

    let exception_src = include_str!("../../src/kernel/privileged/arch/aarch64/exception.rs");
    let svc_start = exception_src
        .find("pub extern \"C\" fn svc_handler(")
        .expect("必须存在 aarch64 svc_handler");
    let svc_block = &exception_src[svc_start
        ..exception_src
            .find("/// 内核 IRQ 统一分发")
            .expect("必须存在 svc_handler 之后的 IRQ 分发文档")];
    assert!(
        svc_block.contains("restore_user_signal_frame"),
        "A-10: aarch64 rt_sigreturn 必须走共享恢复单点 (SS_ONSTACK 清除随迁)"
    );
}
