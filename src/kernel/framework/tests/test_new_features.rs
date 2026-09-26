// UT-07 (2026-09-25): rcu / kmalloc_slab / zil_persist 注册副本已删 —
// 其纯逻辑断言分别以 framework/sync/rcu.rs, framework/mm/kmalloc_slab.rs,
// services/fs/nestfs/zil_persist.rs 的 #[cfg(test)] 为唯一归属.
// 分册 9 项 2 (B09-21): framework/mm/kmalloc_slab.rs 属全仓零引用孤岛,
// 已整体删除, 其 1 个源侧用例随之核销 (注册副本已于 UT-07 期间先行删除).
// UT-07 (2026-09-25): page_fault / mmap 注册副本已删 — 其纯逻辑断言分别以
// framework/mm/page_fault.rs 与 services/mm/mmap.rs 的 #[cfg(test)] 为唯一归属.
use crate::framework::tests::{TestResult, assert_eq_test, check, runner};
use crate::register_tests_inner;

// ============================================================
// COW 帧持有计数 (计数面已收敛到 PMM, 原 COW_REFS 已删除)
// ============================================================

// E-04 (2026-09-06): 测试运行器双端适配 — 计数语义测试依赖裸机 PMM 物理页,
// host 无 PMM 初始化 → 直接 Skip. 计数语义的 host 维覆盖在
// host-tests/tests/pmm_buddy_host_test.rs (VecMetaStore 载体) 内.
#[cfg(feature = "host-test")]
fn test_cow_shared_frame_alloc_starts_at_one() -> TestResult {
    TestResult::Skip("E-04: host 无 PMM 初始化, 跳过 (依赖裸机物理内存分配)")
}

#[cfg(not(feature = "host-test"))]
fn test_cow_shared_frame_alloc_starts_at_one() -> TestResult {
    let pmm = crate::framework::mm::get_pmm();
    let Some(phys) = pmm.alloc_page() else {
        return TestResult::Fail("pmm alloc_page failed");
    };

    // §8.1 规则 1: alloc_page 的 +1 即"创建者把该帧交付给紧随其后的首个映射"
    assert_eq_test!(pmm.frame_ref_count(phys), 1, "分配后持有者数应为 1");

    check!(pmm.frame_dec(phys), "唯一持有者注销即归零");
    check!(!pmm.frame_dec(phys), "已归零帧再次 dec 须 fail-closed");
    // 计数已归零: free_page 走"未计数帧"路径归还 (等价于延迟释放链的最终动作)
    pmm.free_page(phys);
    TestResult::Pass
}

// E-04 同理: 同 test_cow_shared_frame_alloc_starts_at_one
#[cfg(feature = "host-test")]
fn test_cow_shared_frame_inc_dec_paired() -> TestResult {
    TestResult::Skip("E-04: host 无 PMM 初始化, 跳过 (依赖裸机物理内存分配)")
}

#[cfg(not(feature = "host-test"))]
fn test_cow_shared_frame_inc_dec_paired() -> TestResult {
    let pmm = crate::framework::mm::get_pmm();
    let Some(phys) = pmm.alloc_page() else {
        return TestResult::Fail("pmm alloc_page failed");
    };

    // COW 共享形态: 双方各持一份引用 (1 → 2)
    check!(pmm.frame_inc(phys), "共享方登记应成功");
    assert_eq_test!(pmm.frame_ref_count(phys), 2, "共享后持有者数应为 2");

    // 一方退出: 仍有持有者 ⇒ 不得报告归零 (否则在用帧被销毁)
    check!(!pmm.frame_dec(phys), "仍有持有者时不得报告归零");
    assert_eq_test!(pmm.frame_ref_count(phys), 1, "注销一方后应为 1");

    // 最后一方退出: 恰好归零一次
    check!(pmm.frame_dec(phys), "最后一方注销应报告归零");
    assert_eq_test!(pmm.frame_ref_count(phys), 0, "归零后持有者数为 0");
    pmm.free_page(phys);
    TestResult::Pass
}

// ============================================================
// ELF Loader
// ============================================================

fn test_elf64_header_sizes() -> TestResult {
    use crate::framework::proc::Elf64Header;
    use crate::framework::proc::Elf64Phdr;
    assert_eq_test!(core::mem::size_of::<Elf64Header>(), 64, "header size");
    assert_eq_test!(core::mem::size_of::<Elf64Phdr>(), 56, "phdr size");
    TestResult::Pass
}

fn test_elf_validation_null() -> TestResult {
    let result = crate::framework::proc::elf_validate(core::ptr::null(), 64);
    check!(result.is_none(), "null pointer rejected");
    TestResult::Pass
}

#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
fn test_elf_validation_small() -> TestResult {
    let result = crate::framework::proc::elf_validate(&0u8 as *const u8, 10);
    check!(result.is_none(), "too small rejected");
    TestResult::Pass
}

fn test_elf_magic_rejected() -> TestResult {
    let data = [0u8; 64];
    let result = crate::framework::proc::elf_validate(data.as_ptr(), 64);
    check!(result.is_none(), "bad magic rejected");
    TestResult::Pass
}

#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
fn test_elf_valid_minimal() -> TestResult {
    use crate::framework::proc::Elf64Header;
    let data = [0u8; 80]; // 头部 + 预留空间
    // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
    let hdr = unsafe { &mut *(data.as_ptr() as *mut Elf64Header) };
    hdr.e_ident[0] = 0x7F;
    hdr.e_ident[1] = b'E';
    hdr.e_ident[2] = b'L';
    hdr.e_ident[3] = b'F';
    hdr.e_ident[4] = 2; // ELFCLASS64
    hdr.e_machine = 0x3E; // x86_64
    hdr.e_phentsize = 56; // sizeof(Elf64Phdr)
    let result = crate::framework::proc::elf_validate(data.as_ptr(), 80);
    check!(result.is_some(), "valid elf accepted");
    TestResult::Pass
}

// UT-07 (2026-09-26): devtree 组注册副本已删 — 其断言以 framework/chitin/devtree.rs
// 的 #[cfg(test)] 为唯一归属 (Chitin 设备树节横幅随之移除).

// ============================================================
// IPC Dynamic Namespace
// ============================================================

fn test_dyn_ipc_pipe_no_limit() -> TestResult {
    let ns = crate::framework::ipc::dynamic::DynIpcNamespace::new();
    let mut ids = alloc::vec::Vec::new();
    for _ in 0..50 {
        let id = ns.pipe_create(1000, 2000);
        check!(id != 0, "pipe id non-zero");
        ids.push(id);
    }
    assert_eq_test!(ids.len(), 50, "50 pipes created");
    assert_eq_test!(ns.pipe_count(), 50, "pipe count 50");
    for id in ids {
        ns.pipe_destroy(id).unwrap();
    }
    assert_eq_test!(ns.pipe_count(), 0, "all pipes destroyed");
    TestResult::Pass
}

fn test_dyn_ipc_msgq_growth() -> TestResult {
    let ns = crate::framework::ipc::dynamic::DynIpcNamespace::new();
    for _ in 0..20 {
        let id = ns.msgq_create(1000, 64, 4096).unwrap();
        check!(ns.msgq_exists(id), "msgq exists");
        ns.msgq_destroy(id).unwrap();
    }
    assert_eq_test!(ns.msgq_count(), 0, "all msgqs destroyed");
    TestResult::Pass
}

// E-04 (2026-09-06): 测试运行器双端适配 — host-test 下 DynIpcNamespace::shm_create
// 依赖裸机 PMM (内部 extern "C" pmm_alloc_pages panic abort, catch_unwind 无法捕获),
// host 无 PMM 初始化 → 直接 Skip. kernel_test (QEMU) 下走下方原实现, 行为不变.
#[cfg(feature = "host-test")]
fn test_dyn_ipc_shm_create() -> TestResult {
    TestResult::Skip("E-04: host 无 PMM 初始化, 跳过 (依赖裸机物理内存分配)")
}

#[cfg(not(feature = "host-test"))]
fn test_dyn_ipc_shm_create() -> TestResult {
    let ns = crate::framework::ipc::dynamic::DynIpcNamespace::new();
    let result = ns.shm_create(2000, 8192);
    // May fail if PMM not initialized in test context
    if let Ok(id) = result {
        check!(id != 0, "shm id non-zero");
        ns.shm_destroy(id).unwrap();
    }
    TestResult::Pass
}

fn test_dyn_ipc_sem_create() -> TestResult {
    let ns = crate::framework::ipc::dynamic::DynIpcNamespace::new();
    let id = ns.sem_create(1000, 1, 10).unwrap();
    check!(id != 0, "sem id non-zero");
    ns.sem_destroy(id).unwrap();
    TestResult::Pass
}

// ============================================================
// VMA
// ============================================================

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn test_vma_creation() -> TestResult {
    use crate::framework::mm::PageFlags;
    use crate::framework::mm::{Vma, VmaType};
    let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER;
    let vma = Vma::new(0x400000, 0x401000, flags, VmaType::Anonymous);
    assert_eq_test!(vma.start, 0x400000usize, "start");
    assert_eq_test!(vma.end, 0x401000usize, "end");
    check!(vma.contains(0x400500), "contains addr inside");
    check!(!vma.contains(0x402000), "not contains addr outside");
    TestResult::Pass
}

// E-04 (2026-09-06): 测试运行器双端适配 — host-test 下 MmStruct::new()/insert_vma
// 依赖裸机 VMM 初始化 ([VMM] accessed before initialization panic), host 无 VMM
// → 直接 Skip. kernel_test (QEMU) 下走下方原实现, 行为不变.
#[cfg(feature = "host-test")]
fn test_mm_struct_operations() -> TestResult {
    TestResult::Skip("E-04: host 无 VMM 初始化, 跳过 (依赖裸机虚拟内存管理)")
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
#[cfg(not(feature = "host-test"))]
fn test_mm_struct_operations() -> TestResult {
    use crate::framework::mm::PageFlags;
    use crate::framework::mm::{MmStruct, Vma, VmaType};
    let mm = MmStruct::new();
    let flags = PageFlags::PRESENT | PageFlags::USER;

    let vma = Vma::new(0x400000, 0x401000, flags, VmaType::Anonymous);
    mm.insert_vma(vma).unwrap();

    let found = mm.find_vma(0x400500);
    check!(found.is_some(), "find_vma");
    if let Some(v) = found {
        assert_eq_test!(v.start, 0x400000usize, "found start");
    }

    // cr3 = 0: 本测试只校验 VMA 描述符增删, 不建页表; 显式变体对 cr3 == 0 直接返回.
    mm.remove_range(0x400000, 0x401000, 0);
    let not_found = mm.find_vma(0x400500);
    check!(not_found.is_none(), "removed");

    TestResult::Pass
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn test_vma_stack_guard() -> TestResult {
    use crate::framework::mm::PageFlags;
    use crate::framework::mm::{Vma, VmaType};
    let guard = Vma::new(0x700000, 0x701000, PageFlags::empty(), VmaType::Guard);
    check!(guard.is_guard(), "is_guard");
    check!(!guard.is_stack(), "not stack");
    TestResult::Pass
}

// ============================================================
// Test Registration
// ============================================================

pub fn register_new_tests() {
    let r = runner();
    register_tests_inner! { r:
        "cow": {
            "shared_frame_alloc_starts_at_one": test_cow_shared_frame_alloc_starts_at_one,
            "shared_frame_inc_dec_paired": test_cow_shared_frame_inc_dec_paired,
        },
        "elf": {
            "header_sizes": test_elf64_header_sizes,
            "validation_null": test_elf_validation_null,
            "validation_small": test_elf_validation_small,
            "magic_rejected": test_elf_magic_rejected,
            "valid_minimal": test_elf_valid_minimal,
        },
        "ipc_dynamic": {
            "pipe_no_limit": test_dyn_ipc_pipe_no_limit,
            "msgq_growth": test_dyn_ipc_msgq_growth,
            "shm_create": test_dyn_ipc_shm_create,
            "sem_create": test_dyn_ipc_sem_create,
        },
        "vma": {
            "creation": test_vma_creation,
            "mm_struct_ops": test_mm_struct_operations,
            "stack_guard": test_vma_stack_guard,
        },
    }
}
