//! PMM 引导交付区早期预留与搬移 host 集成测试 (D4-h, memblock 式)
//!
//! 背景: 引导器把载荷 (multiboot 模块 / DTB) 放在它自选的物理地址上, 与内核固定
//! 布局 (映像 + kmalloc 堆窗 + 元数据六区) 无协商机制. 实测 GRUB 把 initramfs 放在
//! 0x6B2000, 恰落在堆窗 `[kernel_end+2MB, kernel_end+10MB)` 内; 事后 `reserve_range`
//! 只能返回 `Err(overlap)` —— 读起来像保护生效, 实际归档仍与堆共址, 堆一旦分配到
//! 那一段就把它覆写掉.
//!
//! 本测试经公共 API 驱动内核真实 `PhysicalMemoryManager` (与 `pmm_buddy_host_test`
//! 同法: 注入 `VecMetaStore`), 覆盖:
//! 1. 原地预留: 登记区在位图建立时统一置位, buddy 不再把它当空闲页
//! 2. 窗外可搬移区: 不触发搬移, base 不变
//! 3. 窗内可搬移区: 搬到互斥新址, 载荷字节完整存活, 新址同样置位
//! 4. 搬移目的区不被 buddy 分配出去 (穷举分配验证)
//! 5. 只预留不搬移的区 (DTB 语义): 即便落在窗内也不动
//! 6. 多条搬移区目的址互斥
//! 7. fail-closed: 零长度 / 表满 / 索引越界一律停机, 不静默丢条目

use edgine::kernel::privileged::mm::early_reserve::MAX_EARLY_REGIONS;
use edgine::kernel::privileged::mm::pmm::{PhysicalMemoryManager, VecMetaStore};
use edgine::kernel::privileged::mm::{PAGE_SIZE, PhysAddr};

/// 模拟物理内存 64 MB
const MEM_SIZE: u64 = 64 * 1024 * 1024;
/// 模拟内核映像末尾 4 MB
const KERNEL_END: u64 = 4 * 1024 * 1024;
/// 固定堆窗预留 = `GAP`(2MB) + `KMALLOC_HEAP`(8MB) + `BITMAP_GAP`(2MB), 与 lib.rs 同值
const RESERVED_AFTER_KERNEL: u64 = 10 * 1024 * 1024;
/// 引导窗上界 (blanket 置位覆盖 `[0, KERNEL_END + RESERVED_AFTER_KERNEL)`)
const WINDOW_UPPER: u64 = KERNEL_END + RESERVED_AFTER_KERNEL;
/// 实测 GRUB 模块落点 —— 在堆窗内, 正是事后 `reserve_range` 报 overlap 的那一例
const GRUB_MODULE_BASE: u64 = 0x6B_2000;
/// 窗外一段空闲区 (32 MB 处), 用作"不触发搬移"的对照
const OUTSIDE_BASE: u64 = 32 * 1024 * 1024;

const PAGE: usize = 4096;

/// 构造已注入空载体并完成 init 的 PMM (位图尚未建立, 供登记早期预留区)
fn setup_pmm() -> PhysicalMemoryManager {
    let pmm = PhysicalMemoryManager::new();
    pmm.inject_meta_store(VecMetaStore::new());
    pmm.init(MEM_SIZE, KERNEL_END);
    pmm
}

/// 同 [`setup_pmm`], 但先把若干段载荷预置进物理内存影子 (模拟引导器装入).
fn setup_pmm_seeded(seeds: &[(u64, &[u8])]) -> PhysicalMemoryManager {
    let pmm = PhysicalMemoryManager::new();
    let store = VecMetaStore::new();
    for (phys, bytes) in seeds {
        store.seed_ram(*phys, bytes);
    }
    pmm.inject_meta_store(store);
    pmm.init(MEM_SIZE, KERNEL_END);
    pmm
}

/// 构造一段可辨识的载荷 (非页整数倍, 用于检验搬移的页取整与字节完整性)
fn payload(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).expect("模 251 必在 u8 内"))
        .collect()
}

/// 断言一段物理区已被置位 (重复预留必被拒).
///
/// 调用方须另留一个已知空闲区做对照 (`reserve_range(...).is_ok()`), 否则本探针
/// 在"任何区都返回 Err"的实现下恒真.
fn assert_reserved(pmm: &PhysicalMemoryManager, base: u64, bytes: usize) {
    assert!(
        pmm.reserve_range(PhysAddr(base), bytes).is_err(),
        "早期预留区 0x{base:X} 应已置位, 重复预留须被拒"
    );
}

#[test]
fn early_reserve_in_place_marks_pages_used() {
    let pmm = setup_pmm();
    let idx = pmm.early_reserve(OUTSIDE_BASE, 3 * PAGE_SIZE, "dtb");
    assert_eq!(pmm.early_region_count(), 1);
    pmm.init_bitmap(RESERVED_AFTER_KERNEL);

    let region = pmm.early_region(idx);
    assert_eq!(region.base, OUTSIDE_BASE, "只预留不搬移: base 不变");
    assert_eq!(region.size, 3 * PAGE_SIZE);
    assert!(!region.relocate);
    assert_reserved(&pmm, OUTSIDE_BASE, 3 * PAGE);

    // 负控制: 紧邻其后的空闲区仍可预留, 证明上面的 Err 不是恒真
    assert!(
        pmm.reserve_range(PhysAddr(OUTSIDE_BASE + 3 * PAGE_SIZE), PAGE)
            .is_ok(),
        "对照区应仍空闲 (否则 assert_reserved 是恒真探针)"
    );
}

#[test]
fn relocatable_outside_window_stays_in_place() {
    let data = payload(5000);
    let pmm = setup_pmm_seeded(&[(OUTSIDE_BASE, &data)]);
    let idx = pmm.early_reserve_relocatable(OUTSIDE_BASE, 5000, "initrd");
    pmm.init_bitmap(RESERVED_AFTER_KERNEL);

    let region = pmm.early_region(idx);
    assert_eq!(region.base, OUTSIDE_BASE, "窗外区不触发搬移");
    assert!(region.relocate);
    assert_eq!(pmm.host_read_ram(region.base, data.len()), data);
    assert_reserved(&pmm, OUTSIDE_BASE, 2 * PAGE);
}

#[test]
fn relocatable_inside_window_moves_and_payload_survives() {
    let data = payload(5000);
    let pmm = setup_pmm_seeded(&[(GRUB_MODULE_BASE, &data)]);
    let idx = pmm.early_reserve_relocatable(GRUB_MODULE_BASE, 5000, "initrd");
    pmm.init_bitmap(RESERVED_AFTER_KERNEL);

    let region = pmm.early_region(idx);
    assert_ne!(region.base, GRUB_MODULE_BASE, "窗内区必须搬移");
    assert!(
        region.base >= WINDOW_UPPER,
        "新址 0x{:X} 须落在引导窗 (上界 0x{WINDOW_UPPER:X}) 之外",
        region.base
    );
    assert_eq!(region.base % PAGE_SIZE, 0, "新址须页对齐");
    assert_eq!(region.size, 5000, "搬移不改长度");
    assert_eq!(
        pmm.host_read_ram(region.base, data.len()),
        data,
        "搬移后载荷字节须完整存活"
    );
    // 5000 B 向上取整到 2 页, 新址整段置位
    assert_reserved(&pmm, region.base, 2 * PAGE);
}

#[test]
fn relocated_pages_never_handed_out_by_buddy() {
    let data = payload(5000);
    let pmm = setup_pmm_seeded(&[(GRUB_MODULE_BASE, &data)]);
    let idx = pmm.early_reserve_relocatable(GRUB_MODULE_BASE, 5000, "initrd");
    pmm.init_bitmap(RESERVED_AFTER_KERNEL);
    let region = pmm.early_region(idx);
    let lo = region.base;
    let hi = region.base + 2 * PAGE_SIZE;

    let mut allocs = 0u64;
    while let Some(addr) = pmm.alloc_page() {
        assert!(
            addr.0 < lo || addr.0 >= hi,
            "buddy 分配出搬移目的区内的页 0x{:X} (区 [0x{lo:X}, 0x{hi:X}))",
            addr.0
        );
        allocs += 1;
    }
    assert!(allocs > 0, "应能分配到空闲页 (否则本测试是空转)");
}

#[test]
fn non_relocatable_inside_window_stays() {
    // DTB 语义: 在 PMM 存在之前就已解析完并持久化, 故即便落在窗内也只预留不搬移.
    let pmm = setup_pmm();
    let idx = pmm.early_reserve(GRUB_MODULE_BASE, PAGE_SIZE, "dtb");
    pmm.init_bitmap(RESERVED_AFTER_KERNEL);

    let region = pmm.early_region(idx);
    assert_eq!(region.base, GRUB_MODULE_BASE);
    assert!(!region.relocate);
    assert_reserved(&pmm, GRUB_MODULE_BASE, PAGE);
}

#[test]
fn two_relocations_get_disjoint_destinations() {
    let a = payload(5000);
    let b = payload(9000);
    let pmm = setup_pmm_seeded(&[(GRUB_MODULE_BASE, &a), (GRUB_MODULE_BASE + 0x10_0000, &b)]);
    let ia = pmm.early_reserve_relocatable(GRUB_MODULE_BASE, 5000, "initrd");
    let ib = pmm.early_reserve_relocatable(GRUB_MODULE_BASE + 0x10_0000, 9000, "module2");
    pmm.init_bitmap(RESERVED_AFTER_KERNEL);

    let ra = pmm.early_region(ia);
    let rb = pmm.early_region(ib);
    // 两条都必须真的搬走 (原址不同, 只判互斥会在"搬移被关掉"时恒真)
    assert_ne!(ra.base, GRUB_MODULE_BASE, "第一条须搬离原址");
    assert_ne!(rb.base, GRUB_MODULE_BASE + 0x10_0000, "第二条须搬离原址");
    assert!(
        ra.base >= WINDOW_UPPER && rb.base >= WINDOW_UPPER,
        "两条新址都须落在引导窗 (上界 0x{WINDOW_UPPER:X}) 之外: 0x{:X} / 0x{:X}",
        ra.base,
        rb.base
    );
    assert_ne!(ra.base, rb.base);
    let a_hi = ra.base + 2 * PAGE_SIZE;
    let b_hi = rb.base + 3 * PAGE_SIZE;
    assert!(
        ra.base >= b_hi || rb.base >= a_hi,
        "两条搬移目的区须互斥: [0x{:X},0x{:X}) 与 [0x{:X},0x{:X})",
        ra.base,
        a_hi,
        rb.base,
        b_hi
    );
    assert_eq!(pmm.host_read_ram(ra.base, a.len()), a);
    assert_eq!(pmm.host_read_ram(rb.base, b.len()), b);
}

#[test]
#[should_panic(expected = "zero size")]
fn zero_size_fails_closed() {
    let pmm = setup_pmm();
    let _ = pmm.early_reserve(OUTSIDE_BASE, 0, "bad");
}

#[test]
#[should_panic(expected = "table full")]
fn table_full_fails_closed() {
    let pmm = setup_pmm();
    for i in 0..=MAX_EARLY_REGIONS {
        let _ = pmm.early_reserve(
            OUTSIDE_BASE + u64::try_from(i).expect("表容量必在 u64 内") * PAGE_SIZE,
            PAGE_SIZE,
            "fill",
        );
    }
}

#[test]
#[should_panic(expected = "out of range")]
fn early_region_index_out_of_range_fails_closed() {
    let pmm = setup_pmm();
    let _ = pmm.early_region(0);
}
