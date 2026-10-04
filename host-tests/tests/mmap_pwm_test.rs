//! B2.1: Vma.file_pwm 桥接模型测试
//!
//! 验证 file_pwm 从 mmap_syscall 入口到 Vma 存储的语义正确性.
//! 不链接 edgine (host-tests 是 mock 层), 通过复刻 Vma 数据结构
//! 验证模型语义与 edgine Vma 一致.
//!
//! ## 与 edgine Vma 的一致性
//! - 字段: start/end/flags/offset/inode_id/shared/file_pwm
//! - file_backed: file_pwm 参数, 匿名: file_pwm = 0
//! - insert_vma: 合并判断含 file_pwm (不同 pwm 不合并)
//!
//! ## 与单元测试的分工
//! - host-tests/src/unkfs/* 验证 unkfs 数据结构
//! - 本文件验证 mmap/pwm 桥接的模型语义

/// 简化的 VmaType (对应 edgine VmaType::FileBacked/Anonymous)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum VmaType {
    Anonymous = 0,
    FileBacked = 1,
}

/// 模型 Vma (镜像 edgine Vma 字段集, 用于语义测试)
#[derive(Debug, Clone)]
struct Vma {
    start: usize,
    end: usize,
    offset: u64,
    inode_id: u32,
    shared: bool,
    file_pwm: u64,
    /// P3-I-19: 挂载点索引, 决定 #PF miss 时调哪个 FileSystem trait.
    /// mock 中用 Option<usize> (与 edgine 一致), 匿名为 None.
    mount_idx: Option<usize>,
    vma_type: VmaType,
}

impl Vma {
    fn file_backed(
        start: usize,
        end: usize,
        offset: u64,
        inode_id: u32,
        pwm: u64,
        shared: bool,
    ) -> Self {
        // 默认挂载根 (RamFS, mount_idx = 0), 与 edgine mmap 退到根一致.
        Self::file_backed_with_mount(start, end, offset, inode_id, pwm, shared, Some(0))
    }

    fn file_backed_with_mount(
        start: usize,
        end: usize,
        offset: u64,
        inode_id: u32,
        pwm: u64,
        shared: bool,
        mount_idx: Option<usize>,
    ) -> Self {
        Self {
            start,
            end,
            offset,
            inode_id,
            shared,
            file_pwm: pwm,
            mount_idx,
            vma_type: VmaType::FileBacked,
        }
    }

    fn new_anon(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            offset: 0,
            inode_id: 0,
            shared: false,
            file_pwm: 0,
            mount_idx: None,
            vma_type: VmaType::Anonymous,
        }
    }
}

#[test]
fn file_backed_vma_stores_pwm() {
    let pwm: u64 = 0xDEAD_BEEF_CAFE_F00D;
    let vma = Vma::file_backed(0x1000, 0x5000, 0, 42, pwm, true);
    assert_eq!(vma.file_pwm, pwm);
    assert_eq!(vma.inode_id, 42);
    assert!(vma.shared);
    assert_eq!(vma.vma_type, VmaType::FileBacked);
}

#[test]
fn anon_vma_has_zero_pwm() {
    let vma = Vma::new_anon(0x6000, 0x7000);
    assert_eq!(vma.file_pwm, 0);
    assert_eq!(vma.inode_id, 0);
    assert!(!vma.shared);
    assert_eq!(vma.vma_type, VmaType::Anonymous);
}

/// 模拟 mmap_syscall 桥接: 接收用户 pwm, 写入 Vma.file_pwm
fn mock_mmap_file(addr: usize, len: usize, fd: i32, pwm: u64) -> Vma {
    // 简化: fd + 1 → inode_id (与内核 fd→inode 简化映射一致)
    let inode_id = (fd as u32).wrapping_add(1);
    Vma::file_backed(addr, addr + len, 0, inode_id, pwm, true)
}

#[test]
fn mmap_syscall_passes_pwm_to_vma() {
    let pwm: u64 = 0x1234_5678_9ABC_DEF0;
    let vma = mock_mmap_file(0x8000, 0x4000, 5, pwm);
    // 关键: file_pwm 必须从入口参数透传到 Vma 存储
    assert_eq!(
        vma.file_pwm, pwm,
        "pwm must round-trip through mmap_syscall"
    );
    // inode_id = fd + 1 (简化版, 待 fdtable 集成)
    assert_eq!(vma.inode_id, 6);
}

/// #PF 同步填 pcache 时, 读 vma.file_pwm 调用 vfs_pread_inode
/// 此处模拟该路径: 验证 vma.file_pwm 是被读取的, 不是其它字段
fn mock_handle_pf(vma: &Vma) -> u64 {
    // edgine page_fault::handle_file_fault miss 路径:
    // vfs_pread_inode(vma.inode_id, file_off, dst, vma.file_pwm)
    vma.file_pwm
}

#[test]
fn page_fault_reads_vma_file_pwm() {
    let pwm: u64 = 0xCAFE_BABE_DEAD_BEEF;
    let vma = Vma::file_backed(0, 0x1000, 0, 100, pwm, true);
    // 关键: #PF miss 路径必须用 vma.file_pwm, 不能用 0 或 TEST_PWM
    assert_eq!(mock_handle_pf(&vma), pwm, "PF must read vma.file_pwm");
}

/// 合并语义: 不同 file_pwm 的相邻 VMA 不可合并
fn should_merge(a: &Vma, b: &Vma) -> bool {
    a.vma_type == b.vma_type && a.file_pwm == b.file_pwm
}

#[test]
fn vma_merge_requires_same_pwm() {
    let pwm: u64 = 42;
    let v1 = Vma::file_backed(0x1000, 0x2000, 0, 1, pwm, true);
    let v2_same = Vma::file_backed(0x2000, 0x3000, 0, 1, pwm, true);
    let v2_diff = Vma::file_backed(0x2000, 0x3000, 0, 1, pwm + 1, true);
    assert!(should_merge(&v1, &v2_same), "same pwm 邻接可合并");
    assert!(
        !should_merge(&v1, &v2_diff),
        "不同 pwm 邻接不可合并 (权限隔离)"
    );
}

/// fork 后 Vma.file_pwm 继承 (vma.clone 镜像)
#[test]
fn vma_clone_preserves_file_pwm() {
    let pwm: u64 = 0xABCD_EF01_2345_6789;
    let v = Vma::file_backed(0x10000, 0x14000, 0, 7, pwm, false);
    let cloned = v.clone();
    assert_eq!(cloned.file_pwm, pwm, "fork COW clone 必须保留 file_pwm");
    assert_eq!(cloned.inode_id, 7);
    assert!(!cloned.shared);
}

/// mremap 后 Vma.file_pwm 保留
#[test]
fn mremap_preserves_file_pwm() {
    let pwm: u64 = 0xFEDC_BA09_8765_4321;
    let v_old = Vma::file_backed(0x20000, 0x24000, 0, 9, pwm, true);
    // mremap 模拟: 复制 old_vma → new_vma (在 edgine mremap 实现)
    let v_new = v_old.clone();
    assert_eq!(v_new.file_pwm, pwm, "mremap 必须继承 file_pwm");
    assert_eq!(v_new.inode_id, 9);
    assert!(v_new.shared);
}

/// P3-I-19: file_backed 默认挂载到根 (RamFS, mount_idx = 0),
/// 与 edgine mmap 退到根一致.
#[test]
fn file_backed_defaults_to_root_mount() {
    let v = Vma::file_backed(0x30000, 0x34000, 0, 11, 0xAA, true);
    assert_eq!(v.mount_idx, Some(0), "默认 mount_idx = 0 (RamFS)");
}

/// P3-I-19: 显式指定非根挂载 (例如 /dev 对应 DevFS), 必须原样保留.
#[test]
fn file_backed_with_explicit_mount() {
    let v = Vma::file_backed_with_mount(0x30000, 0x34000, 0, 12, 0xBB, false, Some(1));
    assert_eq!(v.mount_idx, Some(1), "非根挂载 mount_idx = 1 (DevFS)");
    assert!(!v.shared);
}

/// P3-I-19: 匿名 VMA 不携带挂载点 (None, 无文件后端).
#[test]
fn anon_vma_has_no_mount() {
    let v = Vma::new_anon(0x40000, 0x44000);
    assert_eq!(v.mount_idx, None, "匿名 VMA mount_idx = None");
}

/// P3-I-19: clone 必须保留 mount_idx (fork 后 #PF miss 仍能正确派发).
#[test]
fn vma_clone_preserves_mount_idx() {
    let v = Vma::file_backed_with_mount(0x50000, 0x54000, 0, 13, 0xCC, true, Some(2));
    let c = v.clone();
    assert_eq!(c.mount_idx, Some(2), "clone 必须保留 mount_idx");
}

/// 验证 Vma.start/end 字段保留用户映射地址区间
#[test]
fn vma_preserves_address_range() {
    let v = Vma::file_backed(0x1000, 0x5000, 0, 42, 0xAA, true);
    assert_eq!(v.start, 0x1000, "start 保留映射起始地址");
    assert_eq!(v.end, 0x5000, "end 保留映射结束地址");
    assert_eq!(v.end - v.start, 0x4000, "end - start = 映射长度 16KB");
}

/// 验证 Vma.offset 字段保留文件内偏移 (用于 #PF miss 时 vfs_pread_inode 定位)
#[test]
fn vma_preserves_file_offset() {
    let v = Vma::file_backed(0x1000, 0x5000, 0x2000, 42, 0xAA, true);
    assert_eq!(v.offset, 0x2000, "offset 保留文件内偏移 (8KB)");
    // #PF miss 时: file_off = vma.offset + (fault_addr - vma.start)
    let fault_addr = 0x3000;
    let file_off = v.offset + (fault_addr - v.start) as u64;
    assert_eq!(
        file_off, 0x4000,
        "file_off = offset + (fault - start) = 0x2000 + 0x2000"
    );
}

/// 验证匿名 VMA 的 start/end/offset 默认语义
#[test]
fn anon_vma_address_range_and_offset() {
    let v = Vma::new_anon(0x6000, 0x7000);
    assert_eq!(v.start, 0x6000, "匿名 VMA start 保留");
    assert_eq!(v.end, 0x7000, "匿名 VMA end 保留");
    assert_eq!(v.end - v.start, 0x1000, "匿名 VMA 长度 4KB");
    assert_eq!(v.offset, 0, "匿名 VMA offset = 0 (无文件后端)");
}

/// 验证 clone (fork COW) 保留 start/end/offset
#[test]
fn vma_clone_preserves_address_range_and_offset() {
    let v = Vma::file_backed(0x10000, 0x14000, 0x8000, 7, 0xABCD, true);
    let c = v.clone();
    assert_eq!(c.start, 0x10000, "clone 保留 start");
    assert_eq!(c.end, 0x14000, "clone 保留 end");
    assert_eq!(c.offset, 0x8000, "clone 保留 offset");
}
