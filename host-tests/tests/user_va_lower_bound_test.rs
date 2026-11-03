//! K-08: 用户 VA 下界统一回归锁
//!
//! ## 背景
//!
//! x86_64 的 kview (每进程内核视图根表) 在低半区 `[0, boundary)` 保留 boot 恒等
//! 映射 (`boundary = ceil(kernel_end / 2MiB)`), 用于内核态经用户根表访问内核映像.
//! 用户映射一旦落进该窗口, 内核态 `copy_*_user` / 缺页路径就会解析到恒等物理页
//! 而非用户帧 (I4 代理面失真). 故三条用户映射入口 (hint-less mmap / 带 hint 的
//! mmap / ELF `PT_LOAD`) 必须共用同一底线下界:
//! [`edgine::kernel::privileged::mm::user_va_lower_bound`].
//!
//! ## 判别力
//!
//! - 下界低于 `USER_CODE_BASE` 或未覆盖恒等窗口上界 → 首个用例失败;
//! - `find_free_range` 仍从 0 起搜 → 第二个用例返回 `< 下界` 的地址;
//! - `mmap` 低于下界的 hint 被接受 → `Einval` 用例失败;
//! - ELF `PT_LOAD` 低于下界仍装载 → 段错误用例返回的不是下界错误;
//! - 下界之上的 ELF 段被误拒 → 控制用例返回下界错误.

use edgine::kernel::functions::mm::mmap::{MAP_ANONYMOUS, MAP_PRIVATE, MmapRequest, mmap_syscall};
use edgine::kernel::privileged::mm::{
    MmStruct, PAGE_SIZE, PageFlags, Vma, VmaType, user_va_lower_bound,
};
use edgine::kernel::privileged::proc::elf::verify::EM_X86_64;
use edgine::kernel::privileged::proc::elf::{Elf64Header, Elf64Phdr, elf_load};
use edgine::kernel::privileged::syscall::Errno;

/// ELF 装载路径的专属错误串 (与 `elf_load_segment` 逐字一致).
const FLOOR_ERR: &str = "ELF: PT_LOAD below user VA lower bound";

/// 构造只含一个 `PT_LOAD` 段的合法 ELF64 (`ET_EXEC`), 段数据完整落在文件内.
///
/// 段属性为只读 (`PF_R`), 文件偏移 0x200 / `filesz` 0x100 / `memsz` 4KiB;
/// `vaddr` 由调用方指定, 用于命中/放行下界检查.
fn make_elf_with_load_vaddr(vaddr: u64) -> Vec<u8> {
    const FILE_SIZE: usize = 0x300;
    const SEG_OFFSET: u64 = 0x200;
    const SEG_FILESZ: u64 = 0x100;
    const PT_LOAD: u32 = 1;
    const PF_R: u32 = 4;

    let mut words = vec![0u64; FILE_SIZE.div_ceil(8)];

    // SAFETY: words 为 8 字节对齐 (与两个 `#[repr(C)]` 结构同对齐) 且容量 ≥
    // FILE_SIZE; header (偏移 0) 与 phdr (偏移 64 = 8 个 u64) 均落在缓冲内.
    // 写后整体按 FILE_SIZE 字节只读导出.
    unsafe {
        let header = &mut *words.as_mut_ptr().cast::<Elf64Header>();
        header.ident[0..4].copy_from_slice(b"\x7FELF");
        header.ident[4] = 2; // ELFCLASS64
        header.ty = 2; // ET_EXEC
        header.machine = EM_X86_64;
        header.entry = vaddr;
        header.phoff = u64::try_from(core::mem::size_of::<Elf64Header>()).expect("头部尺寸可表示");
        header.phentsize =
            u16::try_from(core::mem::size_of::<Elf64Phdr>()).expect("PHDR 尺寸可表示");
        header.phnum = 1;

        let phdr = &mut *words
            .as_mut_ptr()
            .add(core::mem::size_of::<Elf64Header>() / core::mem::size_of::<u64>())
            .cast::<Elf64Phdr>();
        phdr.ty = PT_LOAD;
        phdr.flags = PF_R;
        phdr.offset = SEG_OFFSET;
        phdr.vaddr = vaddr;
        phdr.paddr = vaddr;
        phdr.filesz = SEG_FILESZ;
        phdr.memsz = PAGE_SIZE;
        phdr.align = PAGE_SIZE;
    }

    // SAFETY: 缓冲已按 ELF 头/PHDR 布局初始化, FILE_SIZE ≤ 实际容量.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), FILE_SIZE).to_vec() }
}

/// 匿名私有映射请求模板.
fn anon_request(addr_hint: u64) -> MmapRequest {
    MmapRequest {
        addr_hint,
        length: PAGE_SIZE,
        prot: 0x01 | 0x02, // PROT_READ | PROT_WRITE
        flags: MAP_PRIVATE | MAP_ANONYMOUS,
        fd: -1,
        offset: 0,
        pwm: 0,
    }
}

/// 初始化 host 侧启动信息 (`OnceLock` 幂等).
///
/// `user_va_lower_bound` 读 `boot_get_kernel_end()`; host-test 下 `boot::init()`
/// 以 `kernel_end = 0` 桩化, 初始化后下界 = `USER_CODE_BASE`.
fn init_boot_info() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        edgine::kernel::privileged::boot::init();
    });
}

/// 下界必须覆盖用户 ELF 装载基址, 且保持页对齐.
#[test]
fn lower_bound_covers_user_code_base() {
    init_boot_info();
    let bound = user_va_lower_bound();
    let user_code_base = edgine::kernel::privileged::config::memory::USER_CODE_BASE;
    assert!(
        bound >= user_code_base,
        "用户 VA 下界 {bound:#X} 低于 USER_CODE_BASE {user_code_base:#X}"
    );
    assert_eq!(bound % PAGE_SIZE, 0, "下界必须页对齐: {bound:#X}");
}

/// hint-less 分配必须从下界起搜, 而不是 `mmap_base` 的 0.
#[test]
fn find_free_range_starts_at_lower_bound() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    let page = usize::try_from(PAGE_SIZE).expect("页大小可表示");
    assert_eq!(
        mm.find_free_range(page),
        Some(usize::try_from(bound).expect("下界可表示")),
        "空地址空间的 hint-less 分配必须落在用户 VA 下界"
    );
}

/// 低于下界的 mmap hint 属恒等窗口/ELF 基址之下, 必须 `EINVAL` (fail-closed).
#[test]
fn mmap_hint_below_lower_bound_rejected() {
    init_boot_info();
    let mm = MmStruct::new();
    let below = user_va_lower_bound() - PAGE_SIZE;
    let err = mmap_syscall(&mm, anon_request(below)).unwrap_err();
    assert_eq!(err, Errno::Einval, "低于下界的 hint 必须拒绝");
}

/// 恰在下界的 mmap hint 是合法的精确落点 (下界本身可映射).
#[test]
fn mmap_hint_at_lower_bound_allowed() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    let addr = mmap_syscall(&mm, anon_request(bound)).expect("下界处 hint 必须成功");
    assert_eq!(
        u64::try_from(addr).expect("地址可表示"),
        bound,
        "hint 是精确落点, 不得被抬升"
    );
}

/// hint-less mmap 返回地址必须 ≥ 下界 (`mmap_base` 为 0 时不会落进恒等窗口).
#[test]
fn mmap_hintless_address_at_or_above_lower_bound() {
    init_boot_info();
    let mm = MmStruct::new();
    let addr = mmap_syscall(&mm, anon_request(0)).expect("hint-less 分配必须成功");
    assert!(
        u64::try_from(addr).expect("地址可表示") >= user_va_lower_bound(),
        "hint-less 分配 {addr:#X} 低于用户 VA 下界"
    );
}

/// 低于下界的 `PT_LOAD` 必须在装载期被拒 (不给缺页路径兜底的机会).
#[test]
fn elf_pt_load_below_lower_bound_rejected() {
    init_boot_info();
    let mm = MmStruct::new();
    let elf = make_elf_with_load_vaddr(0x1000);
    let Err(err) = elf_load(&mm, &elf) else {
        panic!("低于下界的 PT_LOAD 必须被拒");
    };
    assert_eq!(err, FLOOR_ERR, "低于下界的 PT_LOAD 必须报下界错误");
}

/// 控制用例: 恰在下界的 `PT_LOAD` 不得被下界门拒绝.
///
/// host 无 PMM/页表语义 (映射阶段会触碰特权指令), 故预置一冲突 VMA 让加载在
/// `insert_vma` 处提前失败 — 该失败发生**在下界检查之后**, 从而证明下界门放行.
#[test]
fn elf_pt_load_at_lower_bound_passes_floor_gate() {
    init_boot_info();
    let mm = MmStruct::new();
    let bound = user_va_lower_bound();
    let conflict = Vma::new(
        usize::try_from(bound).expect("下界可表示"),
        usize::try_from(bound + PAGE_SIZE).expect("下界尾可表示"),
        PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER,
        VmaType::Anonymous,
    );
    mm.insert_vma(conflict).expect("预置冲突 VMA 必须成功");

    let elf = make_elf_with_load_vaddr(bound);
    let Err(err) = elf_load(&mm, &elf) else {
        panic!("控制用例应在 insert_vma 处冲突失败 (host 无 PMM 语义)");
    };
    assert_ne!(err, FLOOR_ERR, "下界之上的 PT_LOAD 不得被下界门拒绝");
}
