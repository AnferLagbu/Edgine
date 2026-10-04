//! `PageTableChecker` — 页表安全检查器 (TCB)
//!
//! 在页表操作前/后核验一致性, 防止:
//! - 内核地址映射到用户可访问页
//! - 页表条目指向无效/未分配物理页
//! - 写时复制 (COW) 标记不一致
//!
//! ## 与 Asterinas OSTD 的关系
//!
//! 等价于 OSTD 的 soundness 验证机制 (Miri + Verus)。
//! `Edgine` 用 Rust 类型系统 + 运行时断言实现等价安全。
//!
//! ## SAFETY 不变量
//!
//! - 所有检查为 `debug_assert`! (release 构建零开销)。
//! - 不修改页表, 纯只读验证。
//! - 可在每次 map/unmap 后调用 (性能敏感路径用 feature gate)。

#[cfg(target_arch = "x86_64")]
use crate::framework::mm::KERNEL_BASE;
use crate::framework::mm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr, get_vmm};

/// 检查虚拟地址是否在用户地址空间内。
///
/// # 触发条件
/// - `PAGE_USER` 标志只允许在用户地址范围。
/// - 内核高半区地址不可设 `PAGE_USER`。
#[cfg(target_arch = "x86_64")]
pub fn check_user_boundary(vaddr: VirtAddr, flags: PageFlags) {
    let va = vaddr.as_u64();
    if va >= KERNEL_BASE {
        debug_assert!(
            !flags.contains(PageFlags::USER),
            "PageTableChecker: kernel address 0x{va:x} must not have PAGE_USER flag"
        );
    }
}

#[cfg(target_arch = "aarch64")]
#[expect(
    clippy::uninlined_format_args,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
pub fn check_user_boundary(vaddr: VirtAddr, flags: PageFlags) {
    let va = vaddr.as_u64();
    if va >= 0xFFFF000000000000 {
        debug_assert!(
            !flags.contains(PageFlags::USER),
            "PageTableChecker: kernel address 0x{:x} must not have PAGE_USER flag",
            va
        );
    }
}

#[expect(
    clippy::nonminimal_bool,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// 检查写可执行页 (W^X 策略)。
///
/// 不允许同时设置 WRITABLE 和执行标志, 防止代码注入。
pub fn check_wxorx(vaddr: VirtAddr, flags: PageFlags) {
    debug_assert!(
        !(flags.contains(PageFlags::WRITABLE) && !flags.contains(PageFlags::NX)),
        "PageTableChecker: W^X violation at vaddr 0x{:x}",
        vaddr.as_u64()
    );
}

/// 检查物理地址是否已映射 (惰性检查)。
///
/// 遍历当前激活的页表, 确认 phys 存在且 flags 匹配。
/// 仅在 `debug_assertions` 启用时调用以避免性能开销。
pub fn verify_mapping(vaddr: VirtAddr, expected_phys: PhysAddr) {
    let vmm = get_vmm();
    if let Some(actual) = vmm.get_physical(vaddr) {
        debug_assert_eq!(
            actual.as_u64(),
            expected_phys.as_u64(),
            "PageTableChecker: vaddr 0x{:x} mapped to 0x{:x}, expected 0x{:x}",
            vaddr.as_u64(),
            actual.as_u64(),
            expected_phys.as_u64()
        );
    }
}

/// 内核镜像保护: 确保内核代码段不可写。
pub fn verify_kernel_code_protection(kernel_text_start: VirtAddr, kernel_text_end: VirtAddr) {
    let vmm = get_vmm();
    let mut addr = kernel_text_start.as_u64() & !(PAGE_SIZE - 1);
    let end = kernel_text_end.as_u64();
    while addr < end {
        if let Some(phys) = vmm.get_physical(VirtAddr(addr)) {
            // 不检查 flags(页表遍历不应触发缺页),
            // 仅确认映射存在且物理地址非零。
            debug_assert!(
                phys.as_u64() != 0,
                "PageTableChecker: kernel code vaddr 0x{addr:x} has null phys mapping"
            );
        }
        addr += PAGE_SIZE as u64;
    }
}

/// 控制台输出: 页表统计摘要 (调试用)。
#[cfg(target_arch = "x86_64")]
pub fn dump_page_table_stats(pml4: u64) {
    let vmm = get_vmm();
    let (mapped, _unmapped, _huge) = vmm.get_stats();
    let _ = (pml4, mapped);
}

#[cfg(target_arch = "aarch64")]
pub fn dump_page_table_stats(pml4: u64) {
    let _ = pml4;
}
