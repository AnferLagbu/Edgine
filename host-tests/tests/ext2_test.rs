//! ext2 只读文件系统测试

use std::fs;
use std::path::Path;

#[test]
fn test_ext2_image_exists() {
    assert!(Path::new("ext2_test.img").exists(), "ext2 测试镜像不存在");
}

#[test]
fn test_ext2_superblock_magic() {
    // 读取超级块并验证 magic number
    let data = fs::read("ext2_test.img").unwrap();
    assert!(data.len() >= 1024 + 1024, "镜像太小");

    let magic = u16::from_le_bytes([data[1024 + 56], data[1024 + 57]]);
    assert_eq!(magic, 0xEF53, "ext2 magic number 不匹配");
}

#[test]
fn test_ext2_block_size() {
    let data = fs::read("ext2_test.img").unwrap();
    let log_block_size = u32::from_le_bytes([
        data[1024 + 24],
        data[1024 + 25],
        data[1024 + 26],
        data[1024 + 27],
    ]);
    let block_size = 1024u32 << log_block_size;
    assert!((1024..=65536).contains(&block_size), "块大小无效");
}

#[test]
fn test_ext2_inode_count() {
    let data = fs::read("ext2_test.img").unwrap();
    let inode_count =
        u32::from_le_bytes([data[1024], data[1024 + 1], data[1024 + 2], data[1024 + 3]]);
    assert!(inode_count > 0, "inode 数量为 0");
}

#[test]
fn test_ext2_block_count() {
    let data = fs::read("ext2_test.img").unwrap();
    let block_count = u32::from_le_bytes([
        data[1024 + 4],
        data[1024 + 5],
        data[1024 + 6],
        data[1024 + 7],
    ]);
    assert!(block_count > 0, "块数量为 0");
}

/// 提取 `src` 中 `sig` 起始的函数体 (至下一个同级 `fn` 定义前)
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let rest = &src[start + sig.len()..];
    let end = rest.find("\n    fn ").unwrap_or(rest.len());
    &rest[..end]
}

/// C1/T6-② 时间戳写回接线证据 (ext2 需块设备, host 无 mock 无法行为验证)
///
/// 断言 syscall 通路 `fs_utimensat` 真实存在, 且 `set_times` 真实写回磁盘 inode
/// 时间戳字段并经 `save_inode` 落盘 (而非只填内存元数据).
#[test]
fn test_ext2_utimensat_wires_to_disk_inode() {
    let src = fs::read_to_string("../src/kernel/services/fs/ext2/mount.rs").unwrap();

    let utimensat = fn_body(&src, "fn fs_utimensat(");
    assert!(
        utimensat.contains("lookup_path"),
        "fs_utimensat 必须按路径解析 inode 号"
    );
    assert!(
        utimensat.contains("set_times"),
        "fs_utimensat 必须委托 set_times 落盘"
    );

    let set_times = fn_body(&src, "fn set_times(");
    assert!(set_times.contains("i_atime"), "set_times 必须写 i_atime");
    assert!(set_times.contains("i_mtime"), "set_times 必须写 i_mtime");
    assert!(set_times.contains("i_ctime"), "set_times 必须写 i_ctime");
    assert!(
        set_times.contains("save_inode"),
        "set_times 必须经 save_inode 落盘"
    );
    assert!(
        set_times.contains("u64::MAX"),
        "set_times 必须实现 UTIME_OMIT (u64::MAX) 语义"
    );
    assert!(
        !set_times.contains("Err(KernelError::NotSupported)"),
        "set_times 不得停留于 NotSupported 空实现"
    );
}

/// 丙批审查 B2 门槛: ext2 `i_ctime` 取值须独立于 `i_mtime`.
///
/// 背景: `set_times` 曾直接 `i_ctime = i_mtime`, 丢失"元数据变更时刻"的独立
/// 语义. 修复后 ctime 取**当前秒值**, 换算与 `services/fs/stat.rs::utimensat_syscall`
/// 同源 (tick / frequency) — 不可用 `crate::arch!(timestamp())` (TSC 周期计数,
/// 落 32 位磁盘字段会截顶成 `u32::MAX`).
///
/// ext2 需块设备, host 无挂载载体, 故以源码结构门槛收口 (与 B1 同体例).
#[test]
fn test_ext2_ctime_source_independent_of_mtime() {
    let src = fs::read_to_string("../src/kernel/services/fs/ext2/mount.rs").unwrap();
    let set_times = fn_body(&src, "fn set_times(");

    assert!(
        !set_times.contains("i_ctime = inode.i_mtime"),
        "B2: i_ctime 不得直接抄 i_mtime (丢失独立语义)"
    );
    assert!(
        set_times.contains("get_ticks") && set_times.contains("get_frequency"),
        "B2: i_ctime 必须取当前秒值 (tick / frequency, 与 utimensat_syscall 同源)"
    );
    assert!(
        !set_times.contains("arch!(timestamp())"),
        "B2: i_ctime 不得用 TSC 计数 (落 32 位磁盘字段会截顶成 u32::MAX)"
    );
}
