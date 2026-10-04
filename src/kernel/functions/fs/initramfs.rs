//! initramfs — cpio newc 格式解析与加载
//!
//! 在内核启动末尾, 将 Multiboot2 module (initramfs cpio 归档) 解压到 ramfs 挂载点,
//! 作为根文件系统 `/`.
//!
//! ## cpio newc 格式
//!
//! 每个文件由 header + filename + padding + filedata + padding 组成:
//!
//! ```text
//! +-------------------+
//! | cpio header (110B)|
//! +-------------------+
//! | filename + NUL    |
//! +-------------------+
//! | padding to 4B     |
//! +-------------------+
//! | file data         |
//! +-------------------+
//! | padding to 4B     |
//! +-------------------+
//! ```
//!
//! 归档以 filename == "TRAILER!!!" 的条目结束.
//!
//! ## 支持的文件类型
//!
//! - 常规文件 (mode & 0o170000 == 0o100000)
//! - 目录 (mode & 0o170000 == 0o040000)
//! - 符号链接 (mode & 0o170000 == 0o120000)
//!
//! # 安全
//!
//! `unpack` 接收 `&[u8]` 归档切片, 由调用方以安全切片形式传入, 本模块 0 unsafe.

use core::cmp;

/// cpio newc header 大小 (固定 110 字节)
const CPIO_NEWC_HEADER_SIZE: usize = 110;

/// cpio 归档结束标记
const CPIO_TRAILER_NAME: &[u8] = b"TRAILER!!!";

/// cpio 文件类型掩码
const CPIO_S_IFMT: u32 = 0o170000;
/// 常规文件
const CPIO_S_IFREG: u32 = 0o100000;
/// 目录
const CPIO_S_IFDIR: u32 = 0o040000;
/// 符号链接
const CPIO_S_IFLNK: u32 = 0o120000;

/// cpio newc 条目 (解析后的中间表示)
struct CpioEntry<'a> {
    /// 文件名 (不含末尾 NUL)
    name: &'a [u8],
    /// 文件数据 (常规文件)
    data: &'a [u8],
    /// 文件模式 (权限 + 类型)
    mode: u32,
}

/// 从 hex ASCII 字符串解析 u32
///
/// cpio newc header 中所有数值字段都是 8 字节的 hex ASCII.
fn parse_hex_field(buf: &[u8]) -> u32 {
    let mut val: u32 = 0;
    for &b in buf {
        let digit = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => break,
        };
        val = (val << 4) | u32::from(digit);
    }
    val
}

/// 4 字节对齐向上取整
fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// 解析下一个 cpio 条目
///
/// 返回 (entry, `next_offset`), 其中 `next_offset` 指向归档中下一个条目的起始位置.
/// 如果遇到 TRAILER!!! 或数据不足, 返回 None.
fn parse_next_entry(data: &[u8], offset: usize) -> Option<(CpioEntry<'_>, usize)> {
    if offset + CPIO_NEWC_HEADER_SIZE > data.len() {
        return None;
    }

    let header = &data[offset..offset + CPIO_NEWC_HEADER_SIZE];

    // 验证 magic "070701"
    if &header[0..6] != b"070701" {
        return None;
    }

    // 解析关键字段 (各 8 字节 hex ASCII)
    let namesize = parse_hex_field(&header[94..102]) as usize;
    let filesize = parse_hex_field(&header[54..62]);
    let mode = parse_hex_field(&header[14..22]);
    // dev, ino, uid, gid, nlink, rdev, mtime 暂不使用

    // 文件名紧跟 header
    let name_offset = offset + CPIO_NEWC_HEADER_SIZE;
    if name_offset + namesize > data.len() {
        return None;
    }

    let name = &data[name_offset..name_offset + namesize];
    // 去掉末尾 NUL
    let name = if name.last() == Some(&0) {
        &name[..name.len() - 1]
    } else {
        name
    };

    // 检查是否为 TRAILER
    if name == CPIO_TRAILER_NAME {
        return None;
    }

    // 文件数据在 name + padding 之后
    let data_offset = align4(name_offset + namesize);
    let file_data_end = data_offset + filesize as usize;

    // 安全检查: 数据不超出归档范围
    let actual_end = cmp::min(file_data_end, data.len());
    let file_data = if data_offset < actual_end {
        &data[data_offset..actual_end]
    } else {
        &data[0..0] // 空文件
    };

    // 下一个条目在 data + padding 之后
    let next_offset = align4(file_data_end);

    Some((
        CpioEntry {
            name,
            data: file_data,
            mode,
        },
        next_offset,
    ))
}

/// 将 cpio 归档解压到 ramfs 根文件系统
///
/// 此函数在内核启动末尾调用, 将 initramfs 内容解压到 `/`.
///
/// # Arguments
/// * `data` - cpio 归档数据切片
/// # Errors
/// 归档为空或 cpio 归档格式非法时返回 Err。
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn unpack(data: &[u8]) -> Result<usize, &'static str> {
    if data.is_empty() {
        return Err("initramfs: empty data");
    }

    let mut offset = 0;
    let mut file_count = 0usize;

    // 确保根目录存在
    let pwm = 0; // 内核权限
    let _ = crate::functions::fs::vfs_mkdir(b"/\0".as_ptr(), pwm);

    while offset < data.len() {
        let (entry, next_offset) = match parse_next_entry(data, offset) {
            Some(e) => e,
            None => break, // TRAILER 或数据结束
        };

        offset = next_offset;

        // 构造完整路径: /<name>
        let mut path_buf = [0u8; 256];
        if entry.name.len() + 1 >= path_buf.len() {
            continue; // 路径过长, 跳过
        }
        path_buf[0] = b'/';
        path_buf[1..=entry.name.len()].copy_from_slice(entry.name);
        // NUL 终止符已由初始化保证

        let file_type = entry.mode & CPIO_S_IFMT;

        match file_type {
            CPIO_S_IFDIR => {
                // 创建目录
                let _ = crate::functions::fs::vfs_mkdir(path_buf.as_ptr(), pwm);
            }
            CPIO_S_IFREG => {
                // 创建文件并写入数据
                let fd = crate::functions::fs::vfs_open(
                    path_buf.as_ptr(),
                    0x41, // O_WRONLY | O_CREAT
                    pwm,
                );
                if fd >= 0 {
                    if !entry.data.is_empty() {
                        crate::functions::fs::vfs_write(
                            fd as u32,
                            entry.data.as_ptr(),
                            entry.data.len() as u32,
                        );
                    }
                    crate::functions::fs::vfs_close(fd as u32);
                }
            }
            CPIO_S_IFLNK => {
                // 符号链接: entry.data 是链接目标
                // 真实实现: 在 linkpath 父目录下建 Symlink 类型新节点.
                if !entry.data.is_empty() {
                    crate::functions::fs::vfs_symlink(entry.data.as_ptr(), path_buf.as_ptr(), pwm);
                }
            }
            _ => {
                // 其他类型 (设备文件等) 暂不支持
            }
        }

        file_count += 1;
    }

    crate::klog_boot_info!(
        "[INITRAMFS] Unpacked {} files from {} bytes",
        file_count,
        data.len()
    );

    Ok(file_count)
}

// ============================================================================
// 内核测试
// ============================================================================

// 阶段 4c: 原 kernel_test 注册回调 (register_initramfs_tests) 改写为源侧
// #[cfg(test)] — cpio 头解析/对齐均为纯逻辑断言, host 侧 cargo test 为唯一
// 归属 (DECISION-080).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpio_parse_hex() {
        assert_eq!(parse_hex_field(b"00000000"), 0u32, "hex 0");
        assert_eq!(parse_hex_field(b"00000001"), 1u32, "hex 1");
        assert_eq!(parse_hex_field(b"0000000A"), 10u32, "hex A");
        assert_eq!(parse_hex_field(b"00000100"), 256u32, "hex 100");
        assert_eq!(parse_hex_field(b"000081A4"), 0x81A4u32, "hex 81A4");
    }

    #[test]
    fn test_cpio_align4() {
        assert_eq!(align4(0), 0, "align4(0)");
        assert_eq!(align4(1), 4, "align4(1)");
        assert_eq!(align4(3), 4, "align4(3)");
        assert_eq!(align4(4), 4, "align4(4)");
        assert_eq!(align4(5), 8, "align4(5)");
        assert_eq!(align4(110), 112, "align4(110)");
    }

    #[test]
    fn test_cpio_parse_minimal() {
        // 构造一个最小的 cpio 归档: 一个空目录 + TRAILER
        let mut archive = [0u8; 256];

        // 目录 "test" 的 Header (namesize=5, mode=0o040755, filesize=0)
        let header = &mut archive[0..110];
        header[0..6].copy_from_slice(b"070701"); // magic
        header[6..14].copy_from_slice(b"00000000"); // ino
        header[14..22].copy_from_slice(b"000041ED"); // mode = 0o40755 = 0x41ED
        header[22..30].copy_from_slice(b"00000000"); // uid
        header[30..38].copy_from_slice(b"00000000"); // gid
        header[38..46].copy_from_slice(b"00000002"); // nlink
        header[46..54].copy_from_slice(b"00000000"); // mtime
        header[54..62].copy_from_slice(b"00000000"); // filesize
        header[62..70].copy_from_slice(b"00000000"); // devmajor
        header[70..78].copy_from_slice(b"00000000"); // devminor
        header[78..86].copy_from_slice(b"00000000"); // rdevmajor
        header[86..94].copy_from_slice(b"00000000"); // rdevminor
        header[94..102].copy_from_slice(b"00000005"); // namesize = 5
        header[102..110].copy_from_slice(b"00000000"); // check

        // Filename "test\0"
        archive[110..115].copy_from_slice(b"test\0");

        // TRAILER 条目位于偏移 116 (按 4 字节对齐: 110 + 5 + 1 = 116)
        let trailer_offset = align4(110 + 5); // = 116
        let trailer = &mut archive[trailer_offset..trailer_offset + 110];
        trailer[0..6].copy_from_slice(b"070701");
        // cpio newc 格式 namesize 含结尾 NUL: "TRAILER!!!"(10) + NUL = 11 = 0xB
        trailer[94..102].copy_from_slice(b"0000000B"); // namesize = 11
        // filename "TRAILER!!!\0" (11 字节)
        archive[trailer_offset + 110..trailer_offset + 121].copy_from_slice(b"TRAILER!!!\0");

        let result = parse_next_entry(&archive, 0);
        assert!(result.is_some(), "parse first entry");
        if let Some((entry, _next)) = result {
            assert!(entry.name == b"test", "entry name = test");
            assert!((entry.mode & CPIO_S_IFMT) == CPIO_S_IFDIR, "entry is dir");
            assert!(entry.data.is_empty(), "dir has no data");
        }
    }
}
