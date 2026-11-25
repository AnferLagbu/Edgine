//! D4: `MAP_FIXED` / `MAP_FIXED_NOREPLACE` 与 `PROT_NONE` 形状锁。
//!
//! 这里只测不依赖当前进程页表根的路径：无重叠的固定落点、
//! NOREPLACE 冲突拒绝、页对齐/下界校验、`PROT_NONE` 的 VMA 表示。
//! `MAP_FIXED` 拆除旧映射后的页表动作由内核 `mmap.rs` 单测 + QEMU 腿覆盖。

use edgine::kernel::functions::mm::mmap::{
    MAP_ANONYMOUS, MAP_FIXED, MAP_FIXED_NOREPLACE, MAP_PRIVATE, MmapRequest, mmap_syscall,
};
use edgine::kernel::privileged::mm::{
    MmStruct, PAGE_SIZE, PageFlags, Vma, VmaType, user_va_lower_bound,
};
use edgine::kernel::privileged::syscall::Errno;

fn init_boot_info() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        edgine::kernel::privileged::boot::init();
    });
}

fn request(addr: u64, prot: i32, flags: i32) -> MmapRequest {
    MmapRequest {
        addr_hint: addr,
        length: PAGE_SIZE,
        prot,
        flags: MAP_PRIVATE | MAP_ANONYMOUS | flags,
        fd: -1,
        offset: 0,
        pwm: 0,
    }
}

#[test]
fn map_fixed_places_mapping_at_exact_address() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    let addr =
        mmap_syscall(&mm, request(bound, 0x01, MAP_FIXED)).expect("MAP_FIXED 无重叠必须成功");
    assert_eq!(u64::try_from(addr).unwrap(), bound);
    let vma = mm
        .find_vma(usize::try_from(bound).unwrap())
        .expect("VMA 必须落位");
    assert!(vma.flags.contains(PageFlags::PRESENT | PageFlags::USER));
    assert!(!vma.flags.contains(PageFlags::WRITABLE));
}

#[test]
fn map_fixed_noreplace_rejects_overlap() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    let page = usize::try_from(PAGE_SIZE).unwrap();
    mm.insert_vma(Vma::new(
        usize::try_from(bound).unwrap(),
        usize::try_from(bound).unwrap() + page,
        PageFlags::PRESENT | PageFlags::USER | PageFlags::NX,
        VmaType::Anonymous,
    ))
    .expect("预置 VMA");

    let err = mmap_syscall(&mm, request(bound, 0x01, MAP_FIXED_NOREPLACE)).unwrap_err();
    assert_eq!(err, Errno::Eexist, "NOREPLACE 冲突必须 EEXIST");
    assert!(
        mm.find_vma(usize::try_from(bound).unwrap()).is_some(),
        "冲突时不得拆除旧映射"
    );
}

#[test]
fn map_fixed_noreplace_places_when_free() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    let addr =
        mmap_syscall(&mm, request(bound, 0x03, MAP_FIXED_NOREPLACE)).expect("空闲落点必须成功");
    assert_eq!(u64::try_from(addr).unwrap(), bound);
}

#[test]
fn map_fixed_rejects_unaligned_or_below_bound() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    assert_eq!(
        mmap_syscall(&mm, request(bound + 1, 0x01, MAP_FIXED)).unwrap_err(),
        Errno::Einval,
        "MAP_FIXED 非页对齐必须 EINVAL"
    );
    assert_eq!(
        mmap_syscall(&mm, request(bound - PAGE_SIZE, 0x01, MAP_FIXED)).unwrap_err(),
        Errno::Einval,
        "MAP_FIXED 低于用户 VA 下界必须 EINVAL"
    );
}

#[test]
fn prot_none_vma_has_no_present_bit() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    let addr = mmap_syscall(&mm, request(bound, 0, 0)).expect("PROT_NONE 映射必须成功");
    let vma = mm.find_vma(addr).expect("VMA 必须落位");
    assert!(vma.flags.is_empty(), "PROT_NONE VMA 不得置 PRESENT 或 USER");
}

/// 文件映射的页缓存身份 (inode/offset) 不能因相邻同权限被合并掉.
#[test]
fn file_backed_adjacent_mappings_are_not_merged() {
    use edgine::kernel::privileged::mm::FileBackedVma;

    let mm = MmStruct::new();
    let base = 0x60_0000;
    let page = usize::try_from(PAGE_SIZE).unwrap();
    let flags = PageFlags::PRESENT | PageFlags::USER | PageFlags::NX;

    mm.insert_vma(Vma::file_backed(&FileBackedVma {
        start: base,
        end: base + page,
        flags,
        offset: 0,
        inode_id: 11,
        pwm: 7,
        shared: false,
        mount_idx: Some(0),
    }))
    .expect("第一段文件映射");
    mm.insert_vma(Vma::file_backed(&FileBackedVma {
        start: base + page,
        end: base + 2 * page,
        flags,
        offset: u64::try_from(page).unwrap(),
        inode_id: 12,
        pwm: 7,
        shared: false,
        mount_idx: Some(0),
    }))
    .expect("第二段文件映射");

    let vmas = mm.vmas.lock();
    assert_eq!(vmas.len(), 2, "不同 inode 的相邻文件映射不得合并");
    assert_eq!(vmas[0].inode_id, Some(11));
    assert_eq!(vmas[1].inode_id, Some(12));
}

#[test]
fn file_backed_overlap_is_rejected_even_with_same_flags() {
    use edgine::kernel::privileged::mm::FileBackedVma;

    let mm = MmStruct::new();
    let base = 0x70_0000;
    let page = usize::try_from(PAGE_SIZE).unwrap();
    let flags = PageFlags::PRESENT | PageFlags::USER | PageFlags::NX;
    mm.insert_vma(Vma::file_backed(&FileBackedVma {
        start: base,
        end: base + page,
        flags,
        offset: 0,
        inode_id: 21,
        pwm: 7,
        shared: false,
        mount_idx: Some(0),
    }))
    .expect("第一段文件映射");

    let err = mm
        .insert_vma(Vma::file_backed(&FileBackedVma {
            start: base,
            end: base + page,
            flags,
            offset: 0,
            inode_id: 22,
            pwm: 7,
            shared: false,
            mount_idx: Some(0),
        }))
        .unwrap_err();
    assert!(
        err.contains("file backing"),
        "同权限不同 inode 的文件重叠必须拒绝: {err}"
    );
}
