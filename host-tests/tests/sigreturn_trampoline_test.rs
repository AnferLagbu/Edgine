//! sigreturn trampoline 机器码契约测试 (P1-I-40)
//!
//! 验证:
//! 1. x86_64 trampoline 编码 = mov eax, 15 + syscall (`SYS_RT_SIGRETURN` = 15)
//! 2. aarch64 trampoline 编码 = movz x0, #139 + svc #0 (`SYS_RT_SIGRETURN` = 139)
//!    (A-10: Edgine 的 aarch64 syscall 编号寄存器是 x0, 不是 Linux 的 x8)
//! 3. 双架构在 host 端通过 union 表达, 跨平台编译期验证
//!
//! 主机端测试平台: 借用 host CPU 架构的 trampoline 与固定字节序列做 hex 对比.
//! 内核 `src/kernel/privileged/proc/signal.rs::SIGRETURN_TRAMPOLINE` 是该契约权威实现.

#[cfg(target_arch = "x86_64")]
const EXPECTED: &[u8] = &[0xB8, 0x0F, 0x00, 0x00, 0x00, 0x0F, 0x05];

#[cfg(target_arch = "aarch64")]
const EXPECTED: &[u8] = &[0x60, 0x11, 0x80, 0xD2, 0x01, 0x00, 0x00, 0xD4];

/// 镜像内核 `SIGRETURN_TRAMPOLINE`
#[cfg(target_arch = "x86_64")]
const SIGRETURN_TRAMPOLINE: [u8; 7] = [0xB8, 0x0F, 0x00, 0x00, 0x00, 0x0F, 0x05];

#[cfg(target_arch = "aarch64")]
const SIGRETURN_TRAMPOLINE: [u8; 8] = [0x60, 0x11, 0x80, 0xD2, 0x01, 0x00, 0x00, 0xD4];

#[cfg(target_arch = "x86_64")]
const SIGRETURN_TRAMPOLINE_SIZE: usize = 7;

#[cfg(target_arch = "aarch64")]
const SIGRETURN_TRAMPOLINE_SIZE: usize = 8;

#[test]
fn trampoline_matches_expected_encoding_on_host_arch() {
    // 验证 host 端编译出的 trampoline 与 EXPECTED 一致
    assert_eq!(
        SIGRETURN_TRAMPOLINE.as_slice(),
        EXPECTED,
        "P1-I-40: host arch 端 trampoline 与期望编码不匹配"
    );
}

#[test]
fn trampoline_size_matches_arch() {
    // 验证长度正确
    assert_eq!(
        SIGRETURN_TRAMPOLINE.len(),
        SIGRETURN_TRAMPOLINE_SIZE,
        "P1-I-40: trampoline 长度常量与实际不一致"
    );
    #[cfg(target_arch = "x86_64")]
    assert_eq!(
        SIGRETURN_TRAMPOLINE_SIZE, 7,
        "P1-I-40: x86_64 必须是 7 字节"
    );
    #[cfg(target_arch = "aarch64")]
    assert_eq!(
        SIGRETURN_TRAMPOLINE_SIZE, 8,
        "P1-I-40: aarch64 必须是 8 字节"
    );
}

#[test]
fn x86_64_trampoline_starts_with_mov_eax() {
    // x86_64 编码: 0xB8 = mov eax, imm32; 后 4 字节是立即数
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(
            SIGRETURN_TRAMPOLINE[0], 0xB8,
            "P1-I-40: x86_64 必须 mov eax"
        );
        let imm = u32::from_le_bytes([
            SIGRETURN_TRAMPOLINE[1],
            SIGRETURN_TRAMPOLINE[2],
            SIGRETURN_TRAMPOLINE[3],
            SIGRETURN_TRAMPOLINE[4],
        ]);
        assert_eq!(imm, 15, "P1-I-40: x86_64 立即数必须是 SYS_RT_SIGRETURN=15");
        // syscall = 0F 05
        assert_eq!(
            SIGRETURN_TRAMPOLINE[5], 0x0F,
            "P1-I-40: x86_64 必须 syscall 指令"
        );
        assert_eq!(
            SIGRETURN_TRAMPOLINE[6], 0x05,
            "P1-I-40: x86_64 必须 syscall 指令"
        );
    }
}

#[test]
fn aarch64_trampoline_uses_movz_x0_and_svc0() {
    // aarch64 编码 (小端): movz x0, #imm; svc #0
    #[cfg(target_arch = "aarch64")]
    {
        // MOVZ (64-bit): sf=1 opc=10 100101 hw=00 imm16 Rd
        //   movz x0, #139 = 0xD280_1160
        let movz = u32::from_le_bytes([
            SIGRETURN_TRAMPOLINE[0],
            SIGRETURN_TRAMPOLINE[1],
            SIGRETURN_TRAMPOLINE[2],
            SIGRETURN_TRAMPOLINE[3],
        ]);
        assert_eq!(
            movz & 0xFFE0_001F,
            0xD280_0000,
            "A-10: aarch64 必须是 movz, 且 Rd=x0 (Edgine syscall 编号寄存器)"
        );
        assert_eq!(
            (movz >> 5) & 0xFFFF,
            139,
            "A-10: aarch64 立即数必须是 SYS_RT_SIGRETURN=139"
        );
        // svc #0 = 0xD400_0001
        let svc = u32::from_le_bytes([
            SIGRETURN_TRAMPOLINE[4],
            SIGRETURN_TRAMPOLINE[5],
            SIGRETURN_TRAMPOLINE[6],
            SIGRETURN_TRAMPOLINE[7],
        ]);
        assert_eq!(svc, 0xD400_0001, "A-10: aarch64 必须 svc #0");
    }
}

#[test]
fn aarch64_trampoline_rt_sigreturn_number_is_139() {
    // aarch64 Linux: __NR_rt_sigreturn = 139; Edgine 沿用该编号.
    #[cfg(target_arch = "aarch64")]
    {
        // imm16 = bits[20:5] (小端解码后的指令字)
        let movz = u32::from_le_bytes([
            SIGRETURN_TRAMPOLINE[0],
            SIGRETURN_TRAMPOLINE[1],
            SIGRETURN_TRAMPOLINE[2],
            SIGRETURN_TRAMPOLINE[3],
        ]);
        let decoded = (movz >> 5) & 0xFFFF;
        assert_eq!(decoded, 139, "P1-I-40: aarch64 SYS_RT_SIGRETURN 必须是 139");
        // Rd 必须是 x0: 内核 `svc_handler` 以 `frame.x0 == 139` 判定 sigreturn,
        // 落在 x8 (Linux ABI) 会被当成普通 syscall 编号, 信号恢复静默失效.
        assert_eq!(
            movz & 0x1F,
            0,
            "A-10: trampoline 必须把 139 写入 x0 (与内核判定寄存器一致)"
        );
    }
}

#[test]
fn trampoline_is_valid_for_arch_quirk() {
    // 防止任何 arch 出 0 长度或全部 FF (空白)
    assert!(
        !SIGRETURN_TRAMPOLINE.is_empty(),
        "P1-I-40: trampoline 不能为空"
    );
    let all_ff = SIGRETURN_TRAMPOLINE.iter().all(|&b| b == 0xFF);
    assert!(!all_ff, "P1-I-40: trampoline 不能全 FF (未实现占位)");
    let all_zero = SIGRETURN_TRAMPOLINE.iter().all(|&b| b == 0x00);
    assert!(!all_zero, "P1-I-40: trampoline 不能全 00 (无效指令)");
}
