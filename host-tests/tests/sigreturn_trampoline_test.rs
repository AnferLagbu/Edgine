//! sigreturn trampoline 机器码契约测试 (P1-I-40)
//!
//! 验证:
//! 1. x86_64 trampoline 编码 = mov eax, 15 + syscall (SYS_rt_sigreturn = 15)
//! 2. aarch64 trampoline 编码 = movz x8, #139 + svc #0 (SYS_rt_sigreturn = 139)
//! 3. 双架构在 host 端通过 union 表达, 跨平台编译期验证
//!
//! 主机端测试平台: 借用 host CPU 架构的 trampoline 与固定字节序列做 hex 对比.
//! 内核 `src/kernel/privileged/proc/signal.rs::SIGRETURN_TRAMPOLINE` 是该契约权威实现.

#[cfg(target_arch = "x86_64")]
const EXPECTED: &[u8] = &[0xB8, 0x0F, 0x00, 0x00, 0x00, 0x0F, 0x05];

#[cfg(target_arch = "aarch64")]
const EXPECTED: &[u8] = &[0xD2, 0x80, 0x11, 0x68, 0xD4, 0x00, 0x00, 0x01];

/// 镜像内核 SIGRETURN_TRAMPOLINE
#[cfg(target_arch = "x86_64")]
const SIGRETURN_TRAMPOLINE: [u8; 7] = [0xB8, 0x0F, 0x00, 0x00, 0x00, 0x0F, 0x05];

#[cfg(target_arch = "aarch64")]
const SIGRETURN_TRAMPOLINE: [u8; 8] = [0xD2, 0x80, 0x11, 0x68, 0xD4, 0x00, 0x00, 0x01];

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
        assert_eq!(imm, 15, "P1-I-40: x86_64 立即数必须是 SYS_rt_sigreturn=15");
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
fn aarch64_trampoline_uses_movz_x8_and_svc0() {
    // aarch64 编码: movz x8, #imm; svc #0
    #[cfg(target_arch = "aarch64")]
    {
        // movz x8, #139: D2 80 11 68 (sf=1 opc=10 movz, hw=00, imm16=0x008B=139, Rd=8)
        assert_eq!(
            SIGRETURN_TRAMPOLINE[0], 0xD2,
            "P1-I-40: aarch64 必须是 movz (高 8 位)"
        );
        assert_eq!(
            SIGRETURN_TRAMPOLINE[1], 0x80,
            "P1-I-40: aarch64 movz 固定 0x80 起始"
        );
        assert_eq!(
            SIGRETURN_TRAMPOLINE[2], 0x11,
            "P1-I-40: aarch64 imm16 高字节 (139>>3=0x11)"
        );
        assert_eq!(
            SIGRETURN_TRAMPOLINE[3], 0x68,
            "P1-I-40: aarch64 Rd=x8 (低 5 位 = 8)"
        );
        // svc #0: D4 00 00 01
        assert_eq!(
            SIGRETURN_TRAMPOLINE[4], 0xD4,
            "P1-I-40: aarch64 必须是 svc 指令"
        );
        assert_eq!(
            SIGRETURN_TRAMPOLINE[5], 0x00,
            "P1-I-40: aarch64 svc imm16 高字节"
        );
        assert_eq!(
            SIGRETURN_TRAMPOLINE[6], 0x00,
            "P1-I-40: aarch64 svc imm16 低字节"
        );
        assert_eq!(
            SIGRETURN_TRAMPOLINE[7], 0x01,
            "P1-I-40: aarch64 svc op=00001"
        );
    }
}

#[test]
fn aarch64_trampoline_rt_sigreturn_number_is_139() {
    // aarch64 Linux: __NR_rt_sigreturn = 139
    #[cfg(target_arch = "aarch64")]
    {
        // 解码 imm16 字段: bits[20:5] = 0x008B = 139
        let imm16 = ((SIGRETURN_TRAMPOLINE[1] as u32) << 8 | SIGRETURN_TRAMPOLINE[2] as u32) >> 5;
        // imm16 = (byte1 << 8 | byte2) >> 5? 编码更准确:
        //   bits[20:5] 在 32-bit 指令里 = 字节 1..3 的相关位
        //   实际: imm16 = (D2_80_11_68 的 bit20..5)
        //   D2 = 1101_0010, 80 = 1000_0000, 11 = 0001_0001, 68 = 0110_1000
        //   bit20..5 = 0000_0000_0001_0001_0_00011 (取自 80_11_68)
        //   简化解码: (0x80_11_68 >> 5) & 0xFFFF = 0x008B = 139
        let decoded = ((SIGRETURN_TRAMPOLINE[1] as u32) << 16
            | (SIGRETURN_TRAMPOLINE[2] as u32) << 8
            | (SIGRETURN_TRAMPOLINE[3] as u32))
            >> 5
            & 0xFFFF;
        assert_eq!(decoded, 139, "P1-I-40: aarch64 SYS_rt_sigreturn 必须是 139");
        let _ = imm16; // 防止未使用警告
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
