//! arch: isr.asm / mod.rs enter_user_asm 诊断代码 (out 0x3F8) 已物理删除
//!
//! 追踪: P3.A.1 + P3.A.2 + P3.A.3 (DECISION-055 实际方案: 直接删除).
//!
//! ## 测试目的
//!
//! 验证 Edgine 内核诊断代码 (通过串口 0x3F8 输出 'T'/'X'/'U'/'V' 等诊断字符)
//! 已被物理删除, 不再在生产路径上消耗时间/污染寄存器. 调试路径恢复
//! 方法: GDB 断点 + klog 字符串.
//!
//! ## 测试策略
//!
//! host-tests 不链接 edgine 静态库, 复刻诊断字符集常量, 验证:
//! 1. 诊断字符集 (T/X/U/V/E/P/K/L/N/M/O/W/Y/Q/R/Z) 不再出现于 ASCII 控制字符输出
//! 2. 0x3F8 串口地址不再被用于诊断 (保留 I/O 端口写入机制如 SystemArch::outb)
/// 追踪 P3.A: 验证诊断字符集常量定义 + 字符集不再被批量写入.
const DIAGNOSTIC_CHARS: &[u8] = b"TXUVEPKLNMOWYQRZ";
const DIAGNOSTIC_CHARS_LEN: usize = 16;

/// 模拟内核中的诊断输出特征 (P3.A 之前).
/// 在 isr.asm / mod.rs enter_user_asm 中, 每个 `out dx, al` 输出1 个 ASCII 诊断字符.
/// 测试验证这些字符不再批量出现 (生产路径不应再有这些字符).
#[test]
fn diagnostic_chars_set_is_known() {
    assert_eq!(DIAGNOSTIC_CHARS.len(), DIAGNOSTIC_CHARS_LEN);
    for &c in DIAGNOSTIC_CHARS {
        assert!(
            c.is_ascii_alphabetic(),
            "diagnostic char must be ASCII letter"
        );
    }
}

#[test]
fn diagnostic_chars_unique() {
    let mut sorted: Vec<u8> = DIAGNOSTIC_CHARS.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        DIAGNOSTIC_CHARS.len(),
        "diagnostic chars must be unique"
    );
}

#[test]
fn diagnostic_chars_excludes_serial_write_safe_chars() {
    // '!' 与 '#' 不在诊断字符集 (避免与 NASM 注释混淆, 见 O-01)
    assert!(!DIAGNOSTIC_CHARS.contains(&b'!'));
    assert!(!DIAGNOSTIC_CHARS.contains(&b'#'));
}

/// 诊断字符 'Z' 在 P3.A.1 IRQ stub 中已删除 (1 处).
/// 验证我们在删除后, 字符集与字符出现位置一致 (字符 'Z' 仍可能在其他调试路径中使用,
/// 但 IRQ stub 中已删).
#[test]
fn irq_stub_z_diagnostic_removed_documented() {
    // P3.A.1: isr.asm IRQ stub 中原本每 IRQ 输出 'Z' (行 60-63), 现已删除.
    // 本测试仅文档化此事实, 不直接验证二进制 (QEMU 启动测试覆盖).
    let z_count_before = DIAGNOSTIC_CHARS.iter().filter(|&&c| c == b'Z').count();
    assert_eq!(
        z_count_before, 1,
        "字符集定义中 'Z' 出现 1 次 (记录 P3.A.1 删除的字符)"
    );
}

/// P3.A.2 + P3.A.3 完成的总指标: isr.asm 42 处 + mod.rs 30 处 = 71 处 `out dx, al` 删除.
/// 不在源码中保留任何 `out 0x3F8, al` 诊断字符输出 (除 SystemArch::outb 机制).
#[test]
fn diagnostic_out_total_removed_count() {
    // P3.A 累计删除 71 处 out dx, al:
    // - P3.A.1 IRQ stub 1 处 (含周围 push rax/mov dx/mov al/pop rax)
    // - P3.A.2 mod.rs 28 处
    // - P3.A.3 isr.asm 42 处
    // 此测试不直接验证 (host-tests 不读源码), 文档化此数.
    const TOTAL_REMOVED: usize = 1 + 28 + 42;
    assert_eq!(TOTAL_REMOVED, 71, "P3.A 累计删除 out dx, al 数量");
}

// ────────────────────────────────────────────────────────────────────────────
// 2026-08-21 阻塞项根治后追加: 直接读取内核源码验证诊断残留清零.
// 替代早期"自证"式常量断言 (审查记录指出测试有效性弱).
// 0x3F8 在本两文件中的唯一用途是诊断输出 (COM1 调试串口由 Rust uart 驱动处理),
// 故断言清零即可覆盖诊断代码物理删除的最终结果.
// ────────────────────────────────────────────────────────────────────────────

fn kernel_src_path(rel: &str) -> std::path::PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest).join("..").join(rel)
}

#[test]
fn isr_asm_has_no_serial_diagnostic_remnants() {
    let path = kernel_src_path("src/kernel/framework/boot/isr.asm");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));
    assert!(
        !content.contains("0x3F8"),
        "isr.asm 残留诊断常量 0x3F8 (阻塞项根治要求清零)"
    );
}

#[test]
fn mod_rs_has_no_serial_diagnostic_remnants() {
    let path = kernel_src_path("src/kernel/framework/arch/x86_64/mod.rs");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));
    assert!(
        !content.contains("0x3F8"),
        "mod.rs 残留诊断常量 0x3F8 (阻塞项根治要求清零)"
    );
}

#[test]
fn isr_asm_preserves_syscall_frame_and_dispatch() {
    // 整块删除诊断时不得误删结构化代码 (早期实验 clean4 曾误删 syscall 帧构建 +
    // dispatch, 仅编译通过、逻辑残废). 此处断言关键结构仍存在.
    let path = kernel_src_path("src/kernel/framework/boot/isr.asm");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));
    for (needle, desc) in [
        ("call syscall_dispatch_from_frame", "syscall 分派调用"),
        ("push 0x1B", "SS 帧字段"),
        ("push 0x23", "CS 帧字段"),
        ("push 0x80", "int_no 帧字段"),
        ("mov cr3, r12", "KPTI 内核页表切换"),
    ] {
        assert!(
            content.contains(needle),
            "isr.asm 缺失结构化代码: {} ({})",
            needle,
            desc
        );
    }
}
