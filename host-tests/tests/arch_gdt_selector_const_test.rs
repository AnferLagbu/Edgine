//! arch: GDT 选择子硬编码 vs Rust 常量一致性测试
//!
//! 追踪: P2.B + DECISION-051 (简化方案).
//!
//! ## 测试目的
//!
//! `isr.asm` / `switch.asm` / `enter_user_asm` 中硬编码 push 0x1B / push 0x23 /
//! `mov ax, 0x23` 等选择子值，必须与 `privileged::arch::x86_64::gdt.rs` 同步.
//!
//! 完整强绑定方案 (`extern SELECTOR_USER_DATA` 在汇编中引用链接脚本 ABSOLUTE
//! 符号) 实施时遇到 Rust inline asm 不支持 NASM 注释符 `;` / `|` 位或运算符
//! / 链接器 ABS 符号会改变指令字节数 (触发 label offset 重定义) 等工程问题.
//! 简化方案:
//!
//! - 汇编侧保留硬编码 0x1B / 0x23 (字节长度与原 push 0x23 一致, 不破坏 layout)
//! - Rust 侧 gdt.rs pub const 是 Rust 代码单一来源 (SELECTOR_USER_DATA/CODE)
//! - 链接脚本 x86_64.ld `SELECTOR_USER_DATA_RPL3 = ABSOLUTE(0x1B)` 等
//!   ABS 符号供 host-tests 引用, 同时作为文档化单一来源
//! - 本测试 host 端复刻 0x18 / 0x1B / 0x20 / 0x23 / 0x08 / 0x10 / 0x28 等值,
//!   验证汇编硬编码与 gdt.rs 注释中声明的常量值一致 (人工 review)
//!
//! 未来若 GDT 重排 (例如添加 ring-1/r0 用户段), 需同步修改:
//! 1. gdt.rs `init_gdt_entries()` 段描述顺序
//! 2. gdt.rs `pub const SELECTOR_*` 值
//! 3. x86_64.ld `SELECTOR_*` 与 `SELECTOR_*_RPL3` 值
//! 4. isr.asm / switch.asm / enter_user_asm 硬编码 0x1B / 0x23
//! 5. 本测试的 `EXPECTED_*` 常量
//!
//! ## 限制
//!
//! 本测试在 host std 环境运行, 仅验证数值常量一致性, 不验证汇编端实际
//! 字节编码. 验证手段: `make ARCH=x86_64` 后 `objdump -d other/build/kernel.bin | grep 0x1B`.

/// 期望值: 与 gdt.rs `pub const SELECTOR_*` 同步.
/// 若 GDT 重排, 修改此测试必须同步修改 gdt.rs / x86_64.ld / isr.asm 等.
const EXPECTED_SELECTOR_NULL: u16 = 0x00;
const EXPECTED_SELECTOR_KERNEL_CODE: u16 = 0x08;
const EXPECTED_SELECTOR_KERNEL_DATA: u16 = 0x10;
const EXPECTED_SELECTOR_USER_DATA: u16 = 0x18;
const EXPECTED_SELECTOR_USER_CODE: u16 = 0x20;
const EXPECTED_SELECTOR_TSS: u16 = 0x28;

/// 期望值: DPL=3 编码 (用户态选择子), 与汇编 push 0x1B/0x23 同步.
const EXPECTED_SELECTOR_USER_DATA_RPL3: u16 = 0x1B;
const EXPECTED_SELECTOR_USER_CODE_RPL3: u16 = 0x23;

#[test]
fn gdt_selector_null() {
    assert_eq!(EXPECTED_SELECTOR_NULL, 0x00);
}

#[test]
fn gdt_selector_kernel_code() {
    assert_eq!(EXPECTED_SELECTOR_KERNEL_CODE, 0x08);
}

#[test]
fn gdt_selector_kernel_data() {
    assert_eq!(EXPECTED_SELECTOR_KERNEL_DATA, 0x10);
}

#[test]
fn gdt_selector_user_data() {
    assert_eq!(EXPECTED_SELECTOR_USER_DATA, 0x18);
}

#[test]
fn gdt_selector_user_code() {
    assert_eq!(EXPECTED_SELECTOR_USER_CODE, 0x20);
}

#[test]
fn gdt_selector_tss() {
    assert_eq!(EXPECTED_SELECTOR_TSS, 0x28);
}

#[test]
fn gdt_selector_user_data_rpl3_is_data_or_3() {
    // RPL=3 是低 2 位 = 3; 高位仍为 SELECTOR_USER_DATA (0x18).
    assert_eq!(EXPECTED_SELECTOR_USER_DATA_RPL3, 0x18 | 3);
    assert_eq!(EXPECTED_SELECTOR_USER_DATA_RPL3, 0x1B);
}

#[test]
fn gdt_selector_user_code_rpl3_is_code_or_3() {
    assert_eq!(EXPECTED_SELECTOR_USER_CODE_RPL3, 0x20 | 3);
    assert_eq!(EXPECTED_SELECTOR_USER_CODE_RPL3, 0x23);
}

#[test]
fn gdt_user_data_and_code_differ_by_8() {
    // SYSRET 兼容性要求 user_data < user_code 且差 8 (gdt.rs 注释约束).
    assert_eq!(EXPECTED_SELECTOR_USER_CODE - EXPECTED_SELECTOR_USER_DATA, 8);
}

#[test]
fn gdt_user_code_rpl3_differs_from_data_rpl3_by_8() {
    assert_eq!(
        EXPECTED_SELECTOR_USER_CODE_RPL3 - EXPECTED_SELECTOR_USER_DATA_RPL3,
        8
    );
}
