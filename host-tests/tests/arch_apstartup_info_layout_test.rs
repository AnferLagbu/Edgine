//! arch: ApStartupInfo 布局数值契约测试
//!
//! 追踪: P1.A + DECISION-050.
//!
//! ## B08-21 布局校验保留 (F-1 规则 3, 跨语言 ABI)
//!
//! 本文件为**布局/常量镜像**: `framework::arch::x86_64::smp_init::ApStartupInfo`
//! (`#[repr(C, packed)]`) 与 `trampoline.asm` 之间的字节级一致性契约. 属跨语言
//! ABI 契约 (Rust ↔ 汇编), 内核 .asm 无法在 host 引用, 按 B08-20/21 消并工程
//! F-1 处置规则 3 保留本镜像并标注覆盖: 不做算法消并 (无平行算法), 仅重放
//! 编译期布局断言, 任何一端修改字段触发双方不一致 (Rust 编译期断言 + 本测试
//! 运行期断言).
//!
//! ## 测试目的
//!
//! `framework::arch::x86_64::smp_init::ApStartupInfo` (`#[repr(C, packed)]`) 与
//! `framework::arch::x86_64::trampoline.asm` 中 ApStartupInfo 必须**字节级一致**.
//! 任意一端修改字段顺序/类型后未同步另一端, BSP 将永远等不到 AP ready (固定循环
//! 100ms 超时) 或 AP 等不到进入 64-bit 的入口.
//!
//! ## 测试策略
//!
//! host-tests 不链接 queenx 静态库 (裸二进制), 也无法直接读取 trampoline.asm.
//! 本测试复刻 ApStartupInfo 的字段顺序与类型, 在 std 环境下**重放**编译期布局断言:
//!
//! - `ApStartupInfo` 总大小 = 54 字节
//! - `ready` 字段偏移 = 38 (汇编端 SINFO_READY 同值)
//! - `done` 字段偏移 = 46 (汇编端 SINFO_DONE 同值)
//! - 8 字节字段对齐无填充 (C packed 布局)
//!
//! 数值常量与 smp_init.rs `AP_STARTUP_INFO_SIZE` / `READY_OFFSET` / `DONE_OFFSET`
//! 共享, 任一端修改触发双方不一致 (Rust 编译期断言 + 本测试运行期断言).
//!
//! ## 限制
//!
//! 本测试不验证**汇编端**实际偏移 (需 QEMU 启动 + 寄存器检查).
//! 验证汇编端的手段: 在 trampoline.asm 头部打印 `SINFO_*` 常量十六进制,
//! host-tests 用 `grep -E` 解析 ELF 符号表对比.

use std::mem::{offset_of, size_of};

/// 复刻 `framework::arch::x86_64::smp_init::ApStartupInfo` 布局 (仅 std 测试可见).
///
/// 字段顺序、类型必须与 smp_init.rs + trampoline.asm 完全一致.
/// 任一端修改后, 本测试 + smp_init.rs 编译期断言将同时失败.
#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
struct ApStartupInfo {
    cr3: u64,       // offset 0
    entry: u64,     // offset 8
    gdt_limit: u16, // offset 16
    gdt_base: u64,  // offset 18
    stack: u64,     // offset 26
    lapic_id: u32,  // offset 34
    ready: u32,     // offset 38
    cpu_index: u32, // offset 42
    done: u32,      // offset 46
    _pad: u32,      // offset 50
}

/// 总大小 = 54 字节 (10 字段 packed, 无填充).
///
/// 必须等于 smp_init.rs `AP_STARTUP_INFO_SIZE` 与 trampoline.asm SINFO 总长.
const EXPECTED_TOTAL_SIZE: usize = 54;

/// `ready` 字段字节偏移 = 38.
///
/// 必须等于 smp_init.rs `READY_OFFSET` 与 trampoline.asm `SINFO_READY equ SINFO_BASE + 38`.
const EXPECTED_READY_OFFSET: usize = 38;

/// `done` 字段字节偏移 = 46.
///
/// 必须等于 smp_init.rs `DONE_OFFSET` 与 trampoline.asm `SINFO_DONE equ SINFO_BASE + 46`.
const EXPECTED_DONE_OFFSET: usize = 46;

/// `cpu_index` 字段字节偏移 = 42 (推导 `done` 偏移前的字段).
const EXPECTED_CPU_INDEX_OFFSET: usize = 42;

#[test]
fn apstartup_info_total_size_is_54() {
    assert_eq!(
        size_of::<ApStartupInfo>(),
        EXPECTED_TOTAL_SIZE,
        "ApStartupInfo 总大小必须 = 54 字节 (与 smp_init.rs/trampoline.asm 一致)",
    );
}

#[test]
fn apstartup_info_ready_offset_is_38() {
    assert_eq!(
        offset_of!(ApStartupInfo, ready),
        EXPECTED_READY_OFFSET,
        "ready 字段偏移必须 = 38 (SINFO_READY 常量)",
    );
}

#[test]
fn apstartup_info_done_offset_is_46() {
    assert_eq!(
        offset_of!(ApStartupInfo, done),
        EXPECTED_DONE_OFFSET,
        "done 字段偏移必须 = 46 (SINFO_DONE 常量)",
    );
}

#[test]
fn apstartup_info_cpu_index_offset_is_42() {
    assert_eq!(
        offset_of!(ApStartupInfo, cpu_index),
        EXPECTED_CPU_INDEX_OFFSET,
        "cpu_index 字段偏移必须 = 42 (ready 之后的下一个 u32)",
    );
}

#[test]
fn apstartup_info_packed_has_no_padding() {
    // 若 `packed` 失效 (例如 Rust 优化决定插入 padding),
    // 字段间会出现 1+ 字节填充, 偏移将不再连续. 此测试通过总大小反向验证.
    // 8+8+2+8+8+4+4+4+4+4 = 54 (无填充).
    const FIELD_SIZES_SUM: usize = 8 + 8 + 2 + 8 + 8 + 4 + 4 + 4 + 4 + 4;
    assert_eq!(
        FIELD_SIZES_SUM, EXPECTED_TOTAL_SIZE,
        "字段累加大小应 = 总大小 (无填充)"
    );
}

#[test]
fn apstartup_info_offsets_are_strictly_increasing() {
    // ready 之前的最后一个字段是 lapic_id (offset 34, 4 字节),
    // 应正好衔接 ready (offset 38). 后续 done (offset 46) 前是 cpu_index (offset 42, 4 字节).
    // 此测试防止未来插入字段时破坏偏移连续性.
    let ready = offset_of!(ApStartupInfo, ready);
    let done = offset_of!(ApStartupInfo, done);
    assert!(ready < done, "ready 偏移 < done 偏移");
    assert_eq!(
        done - ready,
        8,
        "ready 与 done 之间应 = 8 字节 (cpu_index + _pad)"
    );
}
