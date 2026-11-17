//! 引导交付区早期预留 (memblock 式).
//!
//! 引导器把载荷 (multiboot 模块 / DTB / ACPI 表) 放在**它自己选的**物理地址上,
//! 这些地址与内核的固定布局 (内核映像 + kmalloc 堆窗 + PMM 元数据六区) 没有任何
//! 协商机制. 事后调 `reserve_range` 只能"发现"冲突而不能消解冲突: 该区已被
//!  blanket 置位, 于是返回 `Err(overlap)` —— 读起来像保护生效, 实际载荷仍与堆窗
//! 共址, 堆一旦分配到那一段就把归档覆写掉.
//!
//! 本模块给出 memblock 的两段式口径:
//!
//! 1. **登记** —— 位图建立前把每条交付区记进定长表 (`EarlyRegion`), 由
//!    `PhysicalMemoryManager::mark_reserved_pages` 统一置位, 不再有事后补预留.
//! 2. **搬移** —— 登记时标 `relocate` 的区, 若与引导窗 (`[RAM_BASE, early_current)`,
//!    即内核映像 + 固定堆窗 + 元数据六区的全部所有地) 相交, 则在位图建立前从
//!    `early_current` 线性拨出互斥的新址并整段复制, 表内 base 随之更新.
//!
//! 搬移只适用于**启动后仍要被读**的载荷 (initrd 归档). DTB 在 PMM 存在之前就已被
//! `apply_fdt_overrides` 解析完并持久化了拓扑/PSCI/initrd 三项, 故只登记不搬移;
//! firmware 保留区 (ACPI 表 / EFI runtime / MMIO 洞) 同理 —— 它们的位置由 firmware
//! 钉死, 搬移即失效, 只能原地置位不让 buddy 分配出去.
//!
//! 本模块只含记录类型与纯判定函数 (对齐 / 相交), 全部 host 可单测; 表本体与搬移
//! 编排在 `PhysicalMemoryManager` (与 `early_allocs` 同法), 载荷复制经 `MetaStore`
//! 载体 —— 生产走裸指针, host 走 Vec 影子, 搬移算法仅一份代码.

#![deny(missing_docs)]

/// 早期预留区表容量上限.
///
/// 登记方 = firmware 保留区 (MB2 内存图 `type != 1` 项, 上限单源
/// [`crate::privileged::constants::limits::MAX_FIRMWARE_REGIONS`]) + initrd + DTB
/// + MB2 信息块本体, 再加 5 条多模块余量.
///
/// 表满即启动期 fail-closed 停机, 不静默丢条目 (丢一条 = 该区被当空闲页分配出去).
pub const MAX_EARLY_REGIONS: usize = crate::privileged::constants::limits::MAX_FIRMWARE_REGIONS + 8;

/// 一条引导交付区记录.
#[derive(Debug, Clone, Copy)]
pub struct EarlyRegion {
    /// 起始物理地址 (搬移后为新址).
    pub base: u64,
    /// 字节长度.
    pub size: u64,
    /// 归因标签 (日志与读回定位用).
    pub label: &'static str,
    /// 与引导窗相交时是否搬移到互斥区.
    pub relocate: bool,
}

impl EarlyRegion {
    /// 构造空记录 (地址 0 / 长度 0), 供 `const` 数组初始化使用.
    #[must_use]
    pub const fn const_default() -> Self {
        Self {
            base: 0,
            size: 0,
            label: "",
            relocate: false,
        }
    }
}

/// 区首向下页对齐.
#[must_use]
pub const fn align_down_region(base: u64, page: u64) -> u64 {
    base & !(page - 1)
}

/// 区尾向上页对齐 (`base + size` 的和页).
#[must_use]
pub const fn align_up_region(base: u64, size: u64, page: u64) -> u64 {
    (base + size + page - 1) & !(page - 1)
}

/// 两个半开区间 `[base, base + size)` 是否相交.
///
/// 零长度区视为空集, 与任何区都不相交.
#[must_use]
pub const fn ranges_overlap(a_base: u64, a_size: u64, b_base: u64, b_size: u64) -> bool {
    a_size != 0 && b_size != 0 && a_base < b_base + b_size && b_base < a_base + a_size
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u64 = 4096;

    #[test]
    fn test_align_down_and_up_region() {
        assert_eq!(align_down_region(0x6B_2000, PAGE), 0x6B_2000);
        assert_eq!(align_down_region(0x6B_2001, PAGE), 0x6B_2000);
        assert_eq!(align_up_region(0x6B_2000, 1, PAGE), 0x6B_3000);
        assert_eq!(align_up_region(0x6B_2000, PAGE, PAGE), 0x6B_3000);
        assert_eq!(align_up_region(0x6B_2000, 0, PAGE), 0x6B_2000);
    }

    #[test]
    fn test_ranges_overlap_boundaries() {
        // 相邻不算相交 (半开区间)
        assert!(!ranges_overlap(0x1000, PAGE, 0x2000, PAGE));
        assert!(!ranges_overlap(0x2000, PAGE, 0x1000, PAGE));
        // 首尾各重叠一页
        assert!(ranges_overlap(0x1800, PAGE, 0x2000, PAGE));
        assert!(ranges_overlap(0x2000, PAGE, 0x1800, PAGE));
        // 包含
        assert!(ranges_overlap(0x1000, 4 * PAGE, 0x2000, PAGE));
        assert!(ranges_overlap(0x2000, PAGE, 0x1000, 4 * PAGE));
        // 零长度区是空集
        assert!(!ranges_overlap(0x1000, 0, 0x1000, PAGE));
        assert!(!ranges_overlap(0x1000, PAGE, 0x1000, 0));
    }

    #[test]
    fn test_module_inside_heap_window_overlaps() {
        // 实测形态: kernel_end=0x3E8000, 堆窗 [0x5E8000, 0xDE8000),
        // GRUB 模块落在 0x6B2000 —— 必须判为相交 (这正是事后 reserve_range 报
        // overlap 却消解不了冲突的那一例).
        let window_base = 0x3E_8000;
        let window_size = 0x00A0_0000;
        assert!(ranges_overlap(0x6B_2000, 0x1000, window_base, window_size));
        // 搬到引导窗之上即互斥
        assert!(!ranges_overlap(
            0x00E0_0000,
            0x1000,
            window_base,
            window_size
        ));
    }

    #[test]
    fn test_const_default_is_empty() {
        let r = EarlyRegion::const_default();
        assert_eq!(r.base, 0);
        assert_eq!(r.size, 0);
        assert!(!r.relocate);
        assert!(!ranges_overlap(r.base, r.size, 0, u64::MAX));
    }
}
