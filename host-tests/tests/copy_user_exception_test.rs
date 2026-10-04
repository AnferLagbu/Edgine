//! P0-I-36/37/38: exception table 缺失修复验证测试
//!
//! 验证 3 处内核写用户空间 (coredump / socket / signal) 已切换到
//! 异常表保护版 copy_from_user / copy_to_user:
//! - 用户 munmap 正在传输的缓冲区, 内核返回 EFAULT 而非 panic
//! - socket send/recv 缓冲区失效, 返回 EFAULT
//! - 信号栈帧写入失败, 信号不投递但进程继续运行
//!
//! host-tests 不链接 edgine; 复刻 copy_user 的核心接口契约来
//! 验证调用方语义: 返回 Result<usize, ()>, 失败时调用方必须
//! 把 EFAULT / SIG_IGN 路径走通.

// 单元素 &[Range] 是 unmapped 区间的合理表示 (测试用单区间), 抑制 clippy 误报
#![allow(clippy::single_range_in_vec_init)]

// ============================================================================
// 模型: 异常表保护版 copy API 契约
// ============================================================================

#[derive(Debug, PartialEq, Eq)]
enum CopyOutcome {
    Ok(usize),
    Efault,
}

/// 模拟 framework/mm::copy_user::copy_to_user
/// 失败场景: user_dst 落在 "已 munmap 区间" 集合中.
fn mock_copy_to_user(
    user_dst: u64,
    src: &[u8],
    len: usize,
    unmapped: &[std::ops::Range<u64>],
) -> CopyOutcome {
    if unmapped
        .iter()
        .any(|r| r.contains(&user_dst) || (user_dst < r.end && user_dst + len as u64 > r.start))
    {
        return CopyOutcome::Efault;
    }
    CopyOutcome::Ok(len.min(src.len()))
}

/// 模拟 framework/mm::copy_user::copy_from_user
fn mock_copy_from_user(
    kernel_dst: &mut [u8],
    user_src: u64,
    len: usize,
    unmapped: &[std::ops::Range<u64>],
) -> CopyOutcome {
    if unmapped
        .iter()
        .any(|r| r.contains(&user_src) || (user_src < r.end && user_src + len as u64 > r.start))
    {
        return CopyOutcome::Efault;
    }
    let n = len.min(kernel_dst.len());
    CopyOutcome::Ok(n)
}

// ============================================================================
// I-36: coredump 用户地址读
// ============================================================================

#[test]
fn coredump_user_buf_mapped_succeeds() {
    let unmapped: Vec<std::ops::Range<u64>> = vec![];
    let mut buf = [0u8; 256];
    let r = mock_copy_from_user(&mut buf, 0x1000, 256, &unmapped);
    assert_eq!(r, CopyOutcome::Ok(256));
}

#[test]
fn coredump_user_buf_munmapped_returns_efault() {
    // 用户在 coredump 遍历 VMA 过程中 munmap 了当前段
    let unmapped: &[std::ops::Range<u64>] = &[0x1000..0x2000];
    let mut buf = [0u8; 256];
    let r = mock_copy_from_user(&mut buf, 0x1000, 256, unmapped);
    assert_eq!(
        r,
        CopyOutcome::Efault,
        "I-36 修复: coredump 读 munmap 段必须 EFAULT, 不能 panic"
    );
}

// ============================================================================
// I-37: socket send/recv 用户缓冲
// ============================================================================

#[test]
fn socket_send_user_buf_munmapped_returns_efault() {
    let unmapped: &[std::ops::Range<u64>] = &[0x4000_0000..0x4000_1000];
    let buf = [0xABu8; 512];
    let r = mock_copy_to_user(0x4000_0000, &buf, 512, unmapped);
    assert_eq!(
        r,
        CopyOutcome::Efault,
        "I-37 修复: send 缓冲区失效必须 EFAULT"
    );
}

#[test]
fn socket_recv_user_buf_munmapped_returns_efault() {
    let unmapped: &[std::ops::Range<u64>] = &[0x5000_0000..0x5000_1000];
    let mut buf = [0u8; 512];
    let r = mock_copy_from_user(&mut buf, 0x5000_0000, 512, unmapped);
    assert_eq!(
        r,
        CopyOutcome::Efault,
        "I-37 修复: recv 缓冲区失效必须 EFAULT"
    );
}

#[test]
fn socket_sockaddr_user_buf_munmapped_returns_efault() {
    // sockaddr_in 仅 8 字节, 跨越 munmap 边界
    let unmapped: &[std::ops::Range<u64>] = &[0x6000_0004..0x6000_0008];
    let mut buf = [0u8; 8];
    let r = mock_copy_from_user(&mut buf, 0x6000_0000, 8, unmapped);
    assert_eq!(
        r,
        CopyOutcome::Efault,
        "I-37 修复: sockaddr 跨越 munmap 边界必须 EFAULT"
    );
}

// ============================================================================
// I-38: signal 栈帧写入
// ============================================================================

#[derive(Clone, Copy)]
struct SignalFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    int_no: u64,
    err_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
    signum: u64,
}

/// 模拟 I-38 修复后的 signal 投递:
/// 三段写入 (ret_addr / SignalFrame / trampoline) 任一失败,
/// 不投递信号, 进程继续运行.
fn try_deliver_signal(
    frame_rsp: u64,
    sigframe: &SignalFrame,
    trampoline: &[u8],
    unmapped: &[std::ops::Range<u64>],
) -> bool {
    let frame_size = core::mem::size_of::<SignalFrame>();
    let ret_addr_bytes = (frame_rsp + 8 + frame_size as u64).to_ne_bytes();
    let sigframe_bytes = unsafe {
        core::slice::from_raw_parts(sigframe as *const SignalFrame as *const u8, frame_size)
    };
    let ok_ret = mock_copy_to_user(frame_rsp, &ret_addr_bytes, 8, unmapped);
    let ok_frame = mock_copy_to_user(frame_rsp + 8, sigframe_bytes, frame_size, unmapped);
    let ok_tramp = mock_copy_to_user(
        frame_rsp + 8 + frame_size as u64,
        trampoline,
        trampoline.len(),
        unmapped,
    );
    if matches!(ok_ret, CopyOutcome::Efault)
        || matches!(ok_frame, CopyOutcome::Efault)
        || matches!(ok_tramp, CopyOutcome::Efault)
    {
        return false;
    }
    true
}

#[test]
fn signal_stack_mapped_delivers() {
    let unmapped: &[std::ops::Range<u64>] = &[];
    let sigframe = SignalFrame {
        r15: 0,
        r14: 0,
        r13: 0,
        r12: 0,
        r11: 0,
        r10: 0,
        r9: 0,
        r8: 0,
        rdi: 0,
        rsi: 0,
        rbp: 0,
        rdx: 0,
        rcx: 0,
        rbx: 0,
        rax: 0,
        int_no: 0,
        err_code: 0,
        rip: 0,
        cs: 0,
        rflags: 0,
        rsp: 0,
        ss: 0,
        signum: 9,
    };
    let trampoline = [0u8; 16];
    assert!(try_deliver_signal(
        0x7000_0000,
        &sigframe,
        &trampoline,
        unmapped
    ));
}

#[test]
fn signal_stack_munmapped_rolls_back() {
    // 用户栈在信号投递前被 munmap
    let unmapped: &[std::ops::Range<u64>] = &[0x7000_0000..0x7000_1000];
    let sigframe = SignalFrame {
        r15: 0,
        r14: 0,
        r13: 0,
        r12: 0,
        r11: 0,
        r10: 0,
        r9: 0,
        r8: 0,
        rdi: 0,
        rsi: 0,
        rbp: 0,
        rdx: 0,
        rcx: 0,
        rbx: 0,
        rax: 0,
        int_no: 0,
        err_code: 0,
        rip: 0,
        cs: 0,
        rflags: 0,
        rsp: 0,
        ss: 0,
        signum: 9,
    };
    let trampoline = [0u8; 16];
    let delivered = try_deliver_signal(0x7000_0000, &sigframe, &trampoline, unmapped);
    assert!(
        !delivered,
        "I-38 修复: 栈帧写入失败时, 信号必须不投递, 进程继续运行"
    );
}

#[test]
fn signal_trampoline_page_munmapped_rolls_back() {
    // ret_addr 写入成功, 但 trampoline 所在页被 munmap
    // frame_rsp=0x7000_0000, frame_size=184, trampoline 起点 = 0x7000_00C0
    let unmapped: &[std::ops::Range<u64>] = &[0x7000_00C8..0x7000_00D8];
    let sigframe = SignalFrame {
        r15: 0,
        r14: 0,
        r13: 0,
        r12: 0,
        r11: 0,
        r10: 0,
        r9: 0,
        r8: 0,
        rdi: 0,
        rsi: 0,
        rbp: 0,
        rdx: 0,
        rcx: 0,
        rbx: 0,
        rax: 0,
        int_no: 0,
        err_code: 0,
        rip: 0,
        cs: 0,
        rflags: 0,
        rsp: 0,
        ss: 0,
        signum: 9,
    };
    let trampoline = [0u8; 16];
    let delivered = try_deliver_signal(0x7000_0000, &sigframe, &trampoline, unmapped);
    assert!(!delivered);
}

// ============================================================================
// SignalFrame 寄存器字段布局与语义测试
// 镜像内核 I-38 signal trampoline: 保存完整 CPU 上下文供 sigreturn 恢复
// ============================================================================

#[test]
fn signal_frame_layout_size_matches_push_order() {
    // 镜像内核 isr.asm: SignalFrame 由 push 顺序填充, 23 个 u64 字段 = 184 字节
    // 字段顺序: r15, r14, r13, r12, r11, r10, r9, r8,        (8)
    //          rdi, rsi, rbp, rdx, rcx, rbx, rax,              (7)
    //          int_no, err_code, rip, cs, rflags, rsp, ss, signum (8)
    assert_eq!(
        core::mem::size_of::<SignalFrame>(),
        23 * 8,
        "SignalFrame 必须为 23 个 u64 = 184 字节"
    );
}

#[test]
fn signal_frame_preserves_general_registers() {
    // 镜像内核语义: 信号投递时保存通用寄存器, sigreturn 时恢复
    // 验证各寄存器字段构造后值被正确保留 (非零值, 区分字段)
    let sigframe = SignalFrame {
        r15: 0x1001,
        r14: 0x1002,
        r13: 0x1003,
        r12: 0x1004,
        r11: 0x1005,
        r10: 0x1006,
        r9: 0x1007,
        r8: 0x1008,
        rdi: 0x2001,
        rsi: 0x2002,
        rbp: 0x2003,
        rdx: 0x2004,
        rcx: 0x2005,
        rbx: 0x2006,
        rax: 0x2007,
        int_no: 14,
        err_code: 0x0006,
        rip: 0x7FFF_0010,
        cs: 0x1B,
        rflags: 0x0202,
        rsp: 0x7FFF_0100,
        ss: 0x23,
        signum: 11,
    };
    assert_eq!(sigframe.r15, 0x1001, "r15 保留");
    assert_eq!(sigframe.r14, 0x1002, "r14 保留");
    assert_eq!(sigframe.r13, 0x1003, "r13 保留");
    assert_eq!(sigframe.r12, 0x1004, "r12 保留");
    assert_eq!(sigframe.r11, 0x1005, "r11 保留");
    assert_eq!(sigframe.r10, 0x1006, "r10 保留");
    assert_eq!(sigframe.r9, 0x1007, "r9 保留");
    assert_eq!(sigframe.r8, 0x1008, "r8 保留");
    assert_eq!(sigframe.rdi, 0x2001, "rdi 保留");
    assert_eq!(sigframe.rsi, 0x2002, "rsi 保留");
    assert_eq!(sigframe.rbp, 0x2003, "rbp 保留");
    assert_eq!(sigframe.rdx, 0x2004, "rdx 保留");
    assert_eq!(sigframe.rcx, 0x2005, "rcx 保留");
    assert_eq!(sigframe.rbx, 0x2006, "rbx 保留");
    assert_eq!(sigframe.rax, 0x2007, "rax 保留");
}

#[test]
fn signal_frame_preserves_exception_context() {
    // 镜像内核语义: int_no/err_code 标记触发信号的事件 (如 #PF int_no=14, err_code=err)
    // rip/cs/rflags/rsp/ss 保存中断时 CPU 上下文, sigreturn 恢复
    let sigframe = SignalFrame {
        r15: 0,
        r14: 0,
        r13: 0,
        r12: 0,
        r11: 0,
        r10: 0,
        r9: 0,
        r8: 0,
        rdi: 0,
        rsi: 0,
        rbp: 0,
        rdx: 0,
        rcx: 0,
        rbx: 0,
        rax: 0,
        int_no: 14,
        err_code: 0x0006,
        rip: 0x7FFF_0010,
        cs: 0x1B,
        rflags: 0x0202,
        rsp: 0x7FFF_0100,
        ss: 0x23,
        signum: 11,
    };
    assert_eq!(sigframe.int_no, 14, "int_no = 14 (#PF 触发 SIGSEGV)");
    assert_eq!(
        sigframe.err_code, 0x0006,
        "err_code = 0x6 (user + write + present)"
    );
    assert_eq!(sigframe.rip, 0x7FFF_0010, "rip 保留用户态返回地址");
    assert_eq!(sigframe.cs, 0x1B, "cs = 0x1B (用户态 code segment, RPL=3)");
    assert_eq!(sigframe.rflags, 0x0202, "rflags 保留 (IF=1, 保留位=1)");
    assert_eq!(sigframe.rsp, 0x7FFF_0100, "rsp 保留用户态栈指针");
    assert_eq!(sigframe.ss, 0x23, "ss = 0x23 (用户态 stack segment, RPL=3)");
    assert_eq!(sigframe.signum, 11, "signum = 11 (SIGSEGV)");
}

#[test]
fn signal_frame_bytes_roundtrip_preserves_register_values() {
    // 镜像内核 try_deliver_signal: 将 SignalFrame 序列化为字节流写入用户栈
    // sigreturn 时反序列化恢复, 验证字节流可正确还原寄存器值
    let sigframe = SignalFrame {
        r15: 0xDEAD_BEEF,
        r14: 0xCAFE_BABE,
        r13: 0xFEED_FACE,
        r12: 0x1234_5678,
        r11: 0,
        r10: 0,
        r9: 0,
        r8: 0,
        rdi: 0xABCD_0001,
        rsi: 0,
        rbp: 0,
        rdx: 0,
        rcx: 0,
        rbx: 0,
        rax: 0,
        int_no: 13,
        err_code: 0,
        rip: 0x401000,
        cs: 0x1B,
        rflags: 0x0202,
        rsp: 0x7FFF_F000,
        ss: 0x23,
        signum: 11,
    };
    let frame_size = core::mem::size_of::<SignalFrame>();
    let bytes = unsafe {
        core::slice::from_raw_parts(&sigframe as *const SignalFrame as *const u8, frame_size)
    };
    // 验证 r15 首字段 (小端序低字节在前)
    assert_eq!(
        bytes[0..8],
        0xDEAD_BEEFu64.to_ne_bytes(),
        "r15 字节 0..8 保留"
    );
    // 验证 r14 第二字段
    assert_eq!(
        bytes[8..16],
        0xCAFE_BABEu64.to_ne_bytes(),
        "r14 字节 8..16 保留"
    );
    // 验证 signum 末字段 (offset = 22 * 8 = 176, 23 个字段最后一个)
    assert_eq!(
        bytes[176..184],
        11u64.to_ne_bytes(),
        "signum 字节 176..184 保留"
    );
    // 验证 int_no 字段 (offset = 15 * 8 = 120, 在 rax 之后)
    assert_eq!(
        bytes[120..128],
        13u64.to_ne_bytes(),
        "int_no 字节 120..128 保留"
    );
}
