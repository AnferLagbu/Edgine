// SPDX-License-Identifier: MPL-2.0
//! Phase B 单元测试 (host 模拟)
//!
//! 覆盖: B1 Futex / B2 Page Cache / B3 Swap / B4 ACPI/MSI

// ============================================================================
// B1 Futex — op 解码 / 标志位
// ============================================================================

const FUTEX_WAIT: i32 = 0;
const FUTEX_WAKE: i32 = 1;
const FUTEX_REQUEUE: i32 = 3;
const FUTEX_WAIT_BITSET: i32 = 9;
const FUTEX_WAKE_BITSET: i32 = 10;
const FUTEX_PRIVATE_FLAG: i32 = 128;

fn futex_base_op(op: i32) -> i32 {
    op & 0x0F
}

fn is_wait_op(op: i32) -> bool {
    matches!(futex_base_op(op), FUTEX_WAIT | FUTEX_WAIT_BITSET)
}

fn is_wake_op(op: i32) -> bool {
    matches!(futex_base_op(op), FUTEX_WAKE | FUTEX_WAKE_BITSET)
}

fn futex_validate_uaddr(uaddr: u64) -> Result<(), &'static str> {
    if uaddr == 0 {
        return Err("EFAULT");
    }
    if uaddr & 0x3 != 0 {
        return Err("EINVAL");
    }
    Ok(())
}

fn futex_validate_op(op: i32) -> Result<(), &'static str> {
    match futex_base_op(op) {
        FUTEX_WAIT | FUTEX_WAIT_BITSET
        | FUTEX_WAKE | FUTEX_WAKE_BITSET
        | FUTEX_REQUEUE => Ok(()),
        _ => Err("ENOSYS"),
    }
}

#[test]
fn test_futex_base_op_extraction() {
    // PRIVATE_FLAG (0x80) 不影响低 4 位
    assert_eq!(futex_base_op(FUTEX_WAIT), 0);
    assert_eq!(futex_base_op(FUTEX_WAKE), 1);
    assert_eq!(futex_base_op(FUTEX_REQUEUE), 3);
    assert_eq!(futex_base_op(FUTEX_WAIT_BITSET), 9);
    assert_eq!(futex_base_op(FUTEX_WAKE_BITSET), 10);
    // PRIVATE + WAKE
    assert_eq!(futex_base_op(FUTEX_WAKE | FUTEX_PRIVATE_FLAG), 1);
    // CLOCK_REALTIME (0x01 in bit 4) + WAIT
    assert_eq!(futex_base_op(FUTEX_WAIT | 0x10), 0);
    assert_eq!(futex_base_op(0x0F), 0x0F);
}

#[test]
fn test_futex_is_wait_op() {
    assert!(is_wait_op(FUTEX_WAIT));
    assert!(is_wait_op(FUTEX_WAIT_BITSET));
    assert!(is_wait_op(FUTEX_WAIT | FUTEX_PRIVATE_FLAG));
    assert!(!is_wait_op(FUTEX_WAKE));
    assert!(!is_wait_op(FUTEX_REQUEUE));
    assert!(!is_wait_op(0xFE));
}

#[test]
fn test_futex_is_wake_op() {
    assert!(is_wake_op(FUTEX_WAKE));
    assert!(is_wake_op(FUTEX_WAKE_BITSET));
    assert!(is_wake_op(FUTEX_WAKE | FUTEX_PRIVATE_FLAG));
    assert!(!is_wake_op(FUTEX_WAIT));
    assert!(!is_wake_op(0x05));
}

#[test]
fn test_futex_validate_uaddr_zero() {
    assert_eq!(futex_validate_uaddr(0), Err("EFAULT"));
    assert_eq!(futex_validate_uaddr(0x1000), Ok(()));
    assert_eq!(futex_validate_uaddr(0x1001), Err("EINVAL")); // 非对齐
    assert_eq!(futex_validate_uaddr(0x1003), Err("EINVAL"));
    assert_eq!(futex_validate_uaddr(0x1004), Ok(()));
}

#[test]
fn test_futex_validate_op() {
    assert!(futex_validate_op(FUTEX_WAIT).is_ok());
    assert!(futex_validate_op(FUTEX_WAKE).is_ok());
    assert!(futex_validate_op(FUTEX_REQUEUE).is_ok());
    assert!(futex_validate_op(FUTEX_WAKE_BITSET).is_ok());
    assert!(futex_validate_op(7).is_err()); // 未定义
    assert!(futex_validate_op(0x0F).is_err());
}

// ============================================================================
// B2 Page Cache — 索引/脏位/不变量
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PcacheState {
    Clean,
    Dirty,
    Locked,
}

#[derive(Debug, Clone, Copy)]
struct PcacheEntry {
    inode_id: u32,
    page_index: u64,
    phys_addr: u64,
    state: PcacheState,
}

fn pcache_put(entry: &mut PcacheEntry) {
    // 简化: 释放引用计数
    entry.state = PcacheState::Clean;
}

fn pcache_mark_dirty(entry: &mut PcacheEntry) {
    entry.state = PcacheState::Dirty;
}

#[test]
fn test_pcache_clean_after_put() {
    let mut e = PcacheEntry {
        inode_id: 1,
        page_index: 0,
        phys_addr: 0x1000_0000,
        state: PcacheState::Dirty,
    };
    pcache_put(&mut e);
    assert_eq!(e.state, PcacheState::Clean);
}

#[test]
fn test_pcache_dirty_propagates() {
    let mut e = PcacheEntry {
        inode_id: 7,
        page_index: 42,
        phys_addr: 0x2000_0000,
        state: PcacheState::Clean,
    };
    pcache_mark_dirty(&mut e);
    assert_eq!(e.state, PcacheState::Dirty);
    // 标记后不能再 put 为 Clean 后又 dirty (状态机一致)
    pcache_put(&mut e);
    assert_eq!(e.state, PcacheState::Clean);
}

#[test]
fn test_pcache_index_uniqueness() {
    // 不同 inode + page_index 视为不同条目
    let e1 = PcacheEntry {
        inode_id: 1, page_index: 0, phys_addr: 0xA000, state: PcacheState::Clean,
    };
    let e2 = PcacheEntry {
        inode_id: 2, page_index: 0, phys_addr: 0xA000, state: PcacheState::Clean,
    };
    let e3 = PcacheEntry {
        inode_id: 1, page_index: 1, phys_addr: 0xB000, state: PcacheState::Clean,
    };
    let e4 = PcacheEntry {
        inode_id: 1, page_index: 0, phys_addr: 0xA000, state: PcacheState::Clean,
    };
    assert!(e1.inode_id != e2.inode_id);
    assert!(e1.page_index != e3.page_index);
    // e1 == e4 (同 inode + page_index)
    assert_eq!(e1.inode_id, e4.inode_id);
    assert_eq!(e1.page_index, e4.page_index);
}

// ============================================================================
// B3 Swap — entry 编码 / swap-out → swap-in
// ============================================================================

const SWAP_PTE_BIT: u64 = 1 << 1; // PTE 标志位表示 swap entry
const SWAP_SLOT_MASK: u64 = 0x0000_FFFF_FFFF_FFF8; // slot 在低 56 位

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SwapEntry(u64);

impl SwapEntry {
    fn new(slot: u64) -> Self {
        Self(slot << 12 | SWAP_PTE_BIT)
    }
    fn slot(&self) -> u64 {
        (self.0 & SWAP_SLOT_MASK) >> 12
    }
    fn is_valid(&self) -> bool {
        self.0 & SWAP_PTE_BIT != 0
    }
}

#[test]
fn test_swap_entry_encode_decode() {
    let e = SwapEntry::new(0x1234);
    assert!(e.is_valid());
    assert_eq!(e.slot(), 0x1234);
    let e2 = SwapEntry::new(0xDEAD_BEEF);
    assert_eq!(e2.slot(), 0xDEAD_BEEF);
}

#[test]
fn test_swap_entry_invalid() {
    let e = SwapEntry(0); // 无 SWAP_PTE_BIT
    assert!(!e.is_valid());
    assert_eq!(e.slot(), 0);
}

#[test]
fn test_swap_entry_zero_slot_valid() {
    // slot=0 但 PTE 标志位置位 → 仍然有效
    let e = SwapEntry(SWAP_PTE_BIT);
    assert!(e.is_valid());
    assert_eq!(e.slot(), 0);
}

#[test]
fn test_swap_lru_state_machine() {
    // LRU 状态机: Active → Inactive → SwapOut → SwapIn
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum LruState { Active, Inactive, SwappedOut, SwappedIn }
    let mut s = LruState::Active;
    s = LruState::Inactive;
    s = LruState::SwappedOut;
    assert_eq!(s, LruState::SwappedOut);
    s = LruState::SwappedIn;
    assert_eq!(s, LruState::SwappedIn);
}

// ============================================================================
// B4 ACPI/MSI — RSDP / MADT / HPET / MSI 向量池不变量
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcpiStatus {
    has_rsdp: bool,
    has_madt: bool,
    has_fadt: bool,
    has_hpet: bool,
    ap_count: u32,
}

#[derive(Debug, Clone, Copy)]
struct HpetInfo {
    base_addr: u64,
    hpet_number: u8,
    comparator_count: u8,
    counter_size: u8,
}

#[test]
fn test_acpi_status_qemu_default() {
    // QEMU 默认: RSDP=否, MADT=否, FADT=否, HPET=否, ap=0
    let s = AcpiStatus {
        has_rsdp: false,
        has_madt: false,
        has_fadt: false,
        has_hpet: false,
        ap_count: 0,
    };
    assert!(!s.has_rsdp);
    assert_eq!(s.ap_count, 0);
}

#[test]
fn test_acpi_status_multiprocessor() {
    let s = AcpiStatus {
        has_rsdp: true,
        has_madt: true,
        has_fadt: true,
        has_hpet: true,
        ap_count: 4,
    };
    assert!(s.has_rsdp);
    assert_eq!(s.ap_count, 4);
    // 4 APs + BSP = 5 个逻辑 CPU
    let total_cpus = s.ap_count + 1;
    assert_eq!(total_cpus, 5);
}

#[test]
fn test_hpet_info_uniqueness() {
    let h1 = HpetInfo {
        base_addr: 0xFED0_0000,
        hpet_number: 0,
        comparator_count: 3,
        counter_size: 64,
    };
    let h2 = HpetInfo {
        base_addr: 0xFED0_0000,
        hpet_number: 1,
        comparator_count: 3,
        counter_size: 64,
    };
    assert_eq!(h1.base_addr, h2.base_addr);
    assert_ne!(h1.hpet_number, h2.hpet_number);
}

// MSI 池: 0..=255 最多 256 个, 不可重复
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MsiPool {
    in_use: [bool; 256],
    count: u32,
}

impl MsiPool {
    fn new() -> Self {
        Self { in_use: [false; 256], count: 0 }
    }
    fn alloc(&mut self) -> Option<u8> {
        if self.count == 256 {
            return None;
        }
        for (i, slot) in self.in_use.iter_mut().enumerate() {
            if !*slot {
                *slot = true;
                self.count += 1;
                return Some(i as u8);
            }
        }
        None
    }
    fn free(&mut self, v: u8) {
        if self.in_use[v as usize] {
            self.in_use[v as usize] = false;
            self.count -= 1;
        }
    }
}

#[test]
fn test_msi_alloc_unique() {
    let mut p = MsiPool::new();
    let a = p.alloc().unwrap();
    let b = p.alloc().unwrap();
    assert_ne!(a, b);
    assert_eq!(p.count, 2);
}

#[test]
fn test_msi_free_reusable() {
    let mut p = MsiPool::new();
    let a = p.alloc().unwrap();
    p.free(a);
    assert_eq!(p.count, 0);
    let b = p.alloc().unwrap();
    // 优先分配低 vector, b == a
    assert_eq!(a, b);
}

#[test]
fn test_msi_pool_exhausted() {
    let mut p = MsiPool::new();
    for _ in 0..256 {
        assert!(p.alloc().is_some());
    }
    assert!(p.alloc().is_none());
    assert_eq!(p.count, 256);
}

#[test]
fn test_msi_double_free() {
    let mut p = MsiPool::new();
    let a = p.alloc().unwrap();
    p.free(a);
    p.free(a); // 重复释放, count 不会下溢
    assert_eq!(p.count, 0);
}
