//! P0-I-26 / B13-FL-01: Demand Paging 模型语义测试
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `PfResult` / `PageFaultInfo` / `PageFlags` / `Vma` / `VmaType` /
//! `decide_fallthrough` 平行实现, 改引内核真实类型:
//! - `edgine::kernel::framework::mm::page_fault::{PfResult, PageFaultInfo}`
//! - `edgine::kernel::framework::mm::PageFlags` (bitflags, NX = 1<<63)
//! - `edgine::kernel::framework::mm::{Vma, VmaType}`
//!
//! ## 因内核 mm 层 host 不可测已移除 (handle_user_page_fault fallthrough)
//! 原镜像的 fallthrough 决策测试 (no_vma_returns_sigsegv / guard_vma_returns_sigsegv /
//! readonly_vma_write_triggers_cow / writable_vma_uses_vma_flags / decide_fallthrough)
//! 已移除: 内核 `handle_user_page_fault`/`handle_page_fault` 依赖
//! `read_user_cr3_asm()` (链接 isr.asm 的 `USER_CR3_SAVE` 汇编符号, host 无此符号),
//! 以及 `vmm::get_vmm()` / `pmm::get_pmm()` / `swap` / `cow` / `get_current_mm()`
//! 等 MMU/全局状态, host 无法构造页表上下文, 不可调用. 保留纯类型级测试:
//! `PfResult` / `PageFaultInfo::from_error_code` / `PageFlags` / `Vma` 字段语义 /
//! `is_guard` (内核语义 = `vma_type == VmaType::Guard`) / 栈扩展区间.
//!
//! ## 与镜像的差异 (以内核为权威)
//! - 内核 `VmaType` 有 8 个变体 (Anonymous/FileBacked/Stack/Heap/Vdso/Vsvar/Guard/Device),
//!   原镜像仅 3 个.
//! - 内核 `Vma::is_guard()` = `vma_type == VmaType::Guard`; 原镜像误判为 "无 USER 位".
//! - 内核 `PageFlags::NX = 1<<63`; 原镜像误作 `NO_EXEC = 0x08`.

use edgine::kernel::framework::constants::limits::USER_ADDR_MAX;
use edgine::kernel::framework::mm::page_fault::{PageFaultInfo, PfResult};
use edgine::kernel::framework::mm::{PageFlags, Vma, VmaType};

// =============================================================================
// 类型级测试
// =============================================================================

#[test]
fn pf_result_enum_values() {
    assert_eq!(PfResult::Fixed as u8, 0);
    assert_eq!(PfResult::SignalSegv as u8, 1);
    assert_eq!(PfResult::SignalBus as u8, 2);
    assert_eq!(PfResult::Oom as u8, 3);
    assert_eq!(PfResult::Unhandled as u8, 4);
}

#[test]
fn pf_info_parses_error_code() {
    // 0x06 = present(0) | write(1) | user(1) → 缺页 + 写 + 用户态
    let info = PageFaultInfo::from_error_code(0x4000, 0x06);
    assert_eq!(info.fault_addr, 0x4000);
    assert!(info.write);
    assert!(info.user);
    assert!(!info.present);
    assert!(!info.reserved);
    assert!(!info.instruction);
}

#[test]
fn pf_info_reserved_bit_detection() {
    // 0x08 = reserved bit set
    let info = PageFaultInfo::from_error_code(0x1000, 0x08);
    assert!(info.reserved);
    assert!(!info.write);
}

#[test]
fn pf_info_not_present() {
    let info = PageFaultInfo::from_error_code(0x1000, 0x00);
    assert!(!info.present);
    assert!(!info.write);
    assert!(!info.user);
}

#[test]
fn stack_region_detected() {
    // 验证内核 USER_STACK_TOP (= constants::limits::USER_ADDR_MAX) 下 4096
    // 落在栈扩展候选区间; 区间下界 = USER_STACK_TOP - USER_STACK_DEFAULT_SIZE.
    // USER_STACK_DEFAULT_SIZE = 0x0080_0000 为 page_fault.rs 私有常量, 测试侧镜像.
    const USER_STACK_TOP: u64 = USER_ADDR_MAX;
    const USER_STACK_DEFAULT_SIZE: u64 = 0x0080_0000; // 与内核 page_fault.rs:67 同步
    let inside = (USER_STACK_TOP - 4096) as usize;
    let outside = (USER_STACK_TOP - USER_STACK_DEFAULT_SIZE - 4096) as usize;
    assert!((USER_STACK_TOP - USER_STACK_DEFAULT_SIZE..USER_STACK_TOP).contains(&(inside as u64)));
    assert!(
        !(USER_STACK_TOP - USER_STACK_DEFAULT_SIZE..USER_STACK_TOP).contains(&(outside as u64))
    );
}

#[test]
fn page_flags_nx_bit_distinct() {
    // 内核 PTE NX 位 = 1<<63; 与其他低 12 位标志不冲突
    let nx = PageFlags::NX;
    assert!(!nx.contains(PageFlags::PRESENT), "NX 与 PRESENT 不冲突");
    assert!(!nx.contains(PageFlags::WRITABLE), "NX 与 WRITABLE 不冲突");
    assert!(!nx.contains(PageFlags::USER), "NX 与 USER 不冲突");
    let combined = PageFlags::PRESENT | PageFlags::USER | PageFlags::NX;
    assert!(combined.contains(PageFlags::NX), "组合位含 NX");
    assert!(combined.contains(PageFlags::PRESENT), "组合位含 PRESENT");
    assert!(combined.contains(PageFlags::USER), "组合位含 USER");
}

#[test]
fn vma_file_backed_fields_roundtrip() {
    // 内核 Vma::file_backed 构造器保留文件后端语义字段
    let vma = Vma::file_backed(
        0x1000,
        0x2000,
        PageFlags::PRESENT | PageFlags::USER,
        0x100,
        42,
        0xCAFE,
        true,
        Some(0),
    );
    assert_eq!(vma.start, 0x1000, "start 保留映射起始地址");
    assert_eq!(vma.end, 0x2000, "end 保留映射结束地址");
    assert_eq!(vma.end - vma.start, 0x1000, "end - start = 映射长度 4KB");
    assert_eq!(
        vma.vma_type,
        VmaType::FileBacked,
        "vma_type 语义: FileBacked"
    );
    assert_eq!(vma.inode_id, 42, "inode_id 保留文件后端 inode 编号");
    assert!(vma.shared, "shared 标记共享映射");
    assert_eq!(vma.file_pwm, 0xCAFE, "file_pwm 保留进程凭证");
    assert_eq!(vma.offset, 0x100, "offset 保留文件内偏移");
    assert!(!vma.is_guard(), "FileBacked 不是 guard");
}

#[test]
fn vma_anonymous_fields_defaults() {
    // 内核 Vma::new 构造器: 匿名映射默认字段语义
    let vma = Vma::new(
        0x2000,
        0x3000,
        PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER,
        VmaType::Anonymous,
    );
    assert_eq!(vma.vma_type, VmaType::Anonymous, "vma_type 语义: Anonymous");
    assert_eq!(vma.inode_id, 0, "匿名映射 inode_id = 0");
    assert!(!vma.shared, "匿名映射默认非共享");
    assert_eq!(vma.file_pwm, 0, "匿名映射 file_pwm = 0");
    assert_eq!(vma.offset, 0, "匿名映射 offset = 0");
    assert_eq!(vma.mount_idx, None, "匿名映射 mount_idx = None");
}

#[test]
fn vma_guard_semantics_is_type_based() {
    // 内核 is_guard 语义: vma_type == VmaType::Guard (而非 "无 USER 位")
    let guard = Vma::new(0x7000, 0x8000, PageFlags::PRESENT, VmaType::Guard);
    assert!(guard.is_guard(), "Guard 类型 VMA 必须 is_guard");
    // 即使带 USER 位, Guard 类型仍判 guard (类型权威)
    let guard_with_user = Vma::new(
        0x7000,
        0x8000,
        PageFlags::PRESENT | PageFlags::USER,
        VmaType::Guard,
    );
    assert!(guard_with_user.is_guard(), "Guard 类型以类型为准");
    let stack = Vma::new(
        0x8000,
        0x9000,
        PageFlags::PRESENT | PageFlags::USER,
        VmaType::Stack,
    );
    assert!(!stack.is_guard(), "Stack 类型不是 guard");
    // 无 USER 位但类型非 Guard 也不是 guard (内核语义与旧镜像相反)
    let no_user_anon = Vma::new(0x9000, 0xA000, PageFlags::PRESENT, VmaType::Anonymous);
    assert!(!no_user_anon.is_guard(), "内核以类型判定, 不以 USER 位");
}

#[test]
fn vma_type_has_all_kernel_variants() {
    // 内核 VmaType 8 变体可区分
    assert_ne!(VmaType::Anonymous, VmaType::FileBacked);
    assert_ne!(VmaType::Stack, VmaType::Guard);
    assert_eq!(VmaType::from_u8(0), VmaType::Anonymous);
    assert_eq!(VmaType::from_u8(2), VmaType::Stack);
    assert_eq!(VmaType::from_u8(6), VmaType::Guard);
    // 非法值回退 Guard (内核语义)
    assert_eq!(VmaType::from_u8(200), VmaType::Guard);
}
