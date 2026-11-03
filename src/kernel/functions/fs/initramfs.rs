//! initramfs — cpio newc 格式解析与加载
//!
//! 在内核启动末尾, 把编译期 `include_bytes!` 进镜像的 initramfs cpio 归档解压到
//! ramfs 挂载点, 作为根文件系统 `/`.
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
//! ## 条目名的路径形态
//!
//! 打包工具写的名字前缀不统一 (GNU `cpio` 写 `./bin/eash`, 自制生成器可能写
//! `bin/eash` 或 `/bin/eash`), 故 `normalize_entry_path` 先把名字归一为根内
//! 绝对路径, 再由 `ensure_parent_dirs` 逐级建出父目录 —— ramfs 的 `mkdir` 只
//! 建叶子, 父目录不存在即 `ENOENT`, 不建父目录则嵌套条目静默丢失.
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

use super::vfs_types::VFS_MAX_PATH;

/// cpio newc header 大小 (固定 110 字节)
const CPIO_NEWC_HEADER_SIZE: usize = 110;

/// cpio 归档结束标记
const CPIO_TRAILER_NAME: &[u8] = b"TRAILER!!!";

/// cpio 文件类型掩码
const CPIO_S_IFMT: u32 = 0o17_0000;
/// 常规文件
const CPIO_S_IFREG: u32 = 0o10_0000;
/// 目录
const CPIO_S_IFDIR: u32 = 0o04_0000;
/// 符号链接
const CPIO_S_IFLNK: u32 = 0o12_0000;

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

/// 把 cpio 条目名归一为根内绝对路径, 写入 `out` 并返回其长度 (不含末尾 NUL).
///
/// 归一规则:
/// - 逐段丢弃空段与 `.` 段 ⇒ `./bin/eash` / `bin//eash/` / `/./bin/eash` 均归一为
///   `/bin/eash`;
/// - 出现 `..` 段整条拒收 (返回 `None`): 归档不得借相对段逃出根前缀;
/// - 结果为空 (归档里的 `.` 根条目) 返回 `None` —— 根目录由 [`unpack`] 预建;
/// - 加前导 `/` 与末尾 NUL 后超出 `out` 长度则返回 `None`.
///
/// `out` 由调用方零初始化, 本函数只写路径字节, 末尾 NUL 由零初始化保证.
fn normalize_entry_path(name: &[u8], out: &mut [u8]) -> Option<usize> {
    if out.len() < 2 {
        return None;
    }
    out[0] = b'/';
    let mut len = 1usize;
    let mut wrote_any = false;
    for comp in name.split(|&byte| byte == b'/') {
        match comp {
            b"" | b"." => continue,
            b".." => return None,
            _ => {}
        }
        let sep = usize::from(wrote_any);
        // +1 = 末尾 NUL 占位, 路径不得写满整个缓冲区
        if len + sep + comp.len() + 1 > out.len() {
            return None;
        }
        if wrote_any {
            out[len] = b'/';
            len += 1;
        }
        out[len..len + comp.len()].copy_from_slice(comp);
        len += comp.len();
        wrote_any = true;
    }
    if wrote_any { Some(len) } else { None }
}

/// 逐级建出 `path` 的父目录 (`path_len` 为路径长度, 末尾 NUL 由缓冲区零值承担).
///
/// ramfs 的 `mkdir` 只建叶子且不自动建父目录, 而 cpio 归档里目录条目的出现次序
/// 由打包工具决定 (GNU cpio 按名排序, 目录一般先于其内容, 但不保证), 故对每个
/// 条目都先把各级前缀 mkdir 一遍. 已存在 (`EEXIST`) 与失败一并忽略 —— 后续的
/// 叶子创建会给出真正的错误码, 本函数只负责"尽力把父链补齐".
///
/// 实现上就地截断: 把分隔符临时改成 NUL 调 `vfs_mkdir`, 调用后还原, 避免第二份
/// 路径缓冲.
fn ensure_parent_dirs(path: &mut [u8], path_len: usize, pwm: u64) {
    for idx in 1..path_len {
        if path[idx] == b'/' {
            let saved = path[idx];
            path[idx] = 0;
            let _ = crate::functions::fs::vfs_mkdir(path.as_ptr(), pwm);
            path[idx] = saved;
        }
    }
}

/// 将 cpio 归档解压到 ramfs 根文件系统
///
/// 此函数在内核启动末尾调用, 将 initramfs 内容解压到 `/`.
///
/// # Arguments
/// * `data` - cpio 归档数据切片
/// # Errors
/// 归档为空或 cpio 归档格式非法时返回 Err。
pub fn unpack(data: &[u8]) -> Result<usize, &'static str> {
    if data.is_empty() {
        return Err("initramfs: empty data");
    }

    let mut offset = 0;
    let mut file_count = 0usize;

    // 确保根目录存在
    let pwm = 0; // 内核权限
    let _ = crate::functions::fs::vfs_mkdir(c"/".as_ptr().cast(), pwm);

    while offset < data.len() {
        let Some((entry, next_offset)) = parse_next_entry(data, offset) else {
            break; // TRAILER 或数据结束
        };

        offset = next_offset;

        // 条目名 → 根内绝对路径 (剥 `./` 前缀、折叠 `.`/空段、拒收 `..`)
        let mut path_buf = [0u8; VFS_MAX_PATH];
        let Some(path_len) = normalize_entry_path(entry.name, &mut path_buf) else {
            continue; // 根条目 / 越界相对段 / 路径过长: 整条跳过
        };
        // 先补齐父目录链, 否则嵌套条目会因父目录缺失而落不了盘
        ensure_parent_dirs(&mut path_buf, path_len, pwm);
        // 路径字节来自归档, 不保证 UTF-8; 非法即整条丢弃 (与 VFS 入口同口径)
        let Ok(path_str) = core::str::from_utf8(&path_buf[..path_len]) else {
            continue;
        };

        let file_type = entry.mode & CPIO_S_IFMT;

        // 归档权限位: ramfs 建目录默认 0777 / 建文件默认 0644, 不 chmod 则归档
        // 写出的 mode 位 (生成器 0755) 丢失 —— execve 的 x 位判定会随之失败.
        let archive_perm = crate::functions::intcast::low_u16_from_u32(entry.mode & 0o7777);

        match file_type {
            CPIO_S_IFDIR => {
                // 创建目录
                let _ = crate::functions::fs::vfs_mkdir(path_buf.as_ptr(), pwm);
                let _ = crate::functions::fs::vfs_chmod(path_buf.as_ptr(), archive_perm, pwm);
                file_count += 1;
            }
            CPIO_S_IFREG => {
                // 创建文件并写入数据 —— 无 fd 路径: 解压发生在首个用户进程建立
                // 之前, `vfs_open` 要在当前进程 fd 表里分配句柄, 此时必然失败.
                if super::api::write_file_kernel(path_str, entry.data, pwm) {
                    let _ = crate::functions::fs::vfs_chmod(path_buf.as_ptr(), archive_perm, pwm);
                    file_count += 1;
                }
            }
            CPIO_S_IFLNK => {
                // 符号链接: entry.data 是链接目标
                // 真实实现: 在 linkpath 父目录下建 Symlink 类型新节点.
                if !entry.data.is_empty() {
                    crate::functions::fs::vfs_symlink(entry.data.as_ptr(), path_buf.as_ptr(), pwm);
                    file_count += 1;
                }
            }
            _ => {
                // 其他类型 (设备文件等) 暂不支持
            }
        }
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

    /// 归一化后应得到 `expect` (不含前导 `/` 之外的分隔差异).
    fn norm_to(name: &[u8], expect: &[u8]) -> bool {
        let mut buf = [0u8; VFS_MAX_PATH];
        normalize_entry_path(name, &mut buf).is_some_and(|len| &buf[..len] == expect)
    }

    /// 归一化应整条拒收 (返回 `None`).
    fn norm_rejected(name: &[u8]) -> bool {
        let mut buf = [0u8; VFS_MAX_PATH];
        normalize_entry_path(name, &mut buf).is_none()
    }

    #[test]
    fn test_cpio_normalize_entry_path() {
        // 三种打包工具写法的同一目标必须归一到同一路径
        assert!(norm_to(b"./init", b"/init"), "./init -> /init");
        assert!(norm_to(b"init", b"/init"), "init -> /init");
        assert!(norm_to(b"/init", b"/init"), "/init -> /init");
        assert!(norm_to(b"./bin/eash", b"/bin/eash"), "嵌套名保序");
        // 空段 / `.` 段 / 末尾斜杠一律折叠
        assert!(norm_to(b"./bin//eash/", b"/bin/eash"), "空段与尾斜杠折叠");
        assert!(norm_to(b"./bin/./eash", b"/bin/eash"), "内联 . 段折叠");
        // 根条目 (`.` / `/`) 由 unpack 预建, 整条跳过
        assert!(norm_rejected(b"."), "根条目 . 跳过");
        assert!(norm_rejected(b"/"), "根条目 / 跳过");
        assert!(norm_rejected(b""), "空名跳过");
        // `..` 段不得逃出根前缀
        assert!(norm_rejected(b"../etc/passwd"), "前导 .. 拒收");
        assert!(norm_rejected(b"./bin/../eash"), "内联 .. 拒收");
        // 缓冲区边界: 必须给末尾 NUL 留位
        assert!(norm_rejected(&[b'a'; VFS_MAX_PATH]), "满长度名字拒收");
        let mut edge = [0u8; VFS_MAX_PATH];
        assert_eq!(
            normalize_entry_path(&[b'b'; VFS_MAX_PATH - 2], &mut edge),
            Some(VFS_MAX_PATH - 1),
            "VFS_MAX_PATH-2 长度名字应刚好落盘"
        );
        assert_eq!(edge[VFS_MAX_PATH - 1], 0, "路径后一位必须保持 NUL");
    }

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

    /// newc 头部的 8 位大写 hex ASCII 字段 (与 [`parse_hex_field`] 互逆).
    fn hex8(value: u32) -> [u8; 8] {
        const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = [b'0'; 8];
        for (i, slot) in out.iter_mut().enumerate() {
            // 高位在左: out[0] 取最高 nibble, out[7] 取最低 (与 parse_hex_field 同序)
            let nibble =
                crate::functions::intcast::low_u16_from_u32((value >> ((7 - i) * 4)) & 0xF);
            *slot = DIGITS[usize::from(nibble)];
        }
        out
    }

    /// 按 newc 线上格式追加一个条目, 返回下一条目的起始偏移.
    ///
    /// 与 `scripts/make_initramfs_cpio.py::build_entry` 同形: 110 字节头 + 名字
    /// (含末尾 NUL) + 补白, 再数据 + 补白. 补白按**归档绝对偏移**取齐 (头本身 110
    /// 字节非 4 的倍数, 这正是最容易写错的一处).
    fn push_test_entry(
        archive: &mut alloc::vec::Vec<u8>,
        name: &[u8],
        mode: u32,
        data: &[u8],
    ) -> usize {
        let mut header = [0u8; CPIO_NEWC_HEADER_SIZE];
        header[0..6].copy_from_slice(b"070701");
        let fields = [
            0u32, // ino
            mode,
            0,                                                             // uid
            0,                                                             // gid
            1,                                                             // nlink
            0,                                                             // mtime
            crate::functions::intcast::low_u32_from_usize(data.len()),     // filesize
            0,                                                             // devmajor
            0,                                                             // devminor
            0,                                                             // rdevmajor
            0,                                                             // rdevminor
            crate::functions::intcast::low_u32_from_usize(name.len() + 1), // namesize (含 NUL)
            0,                                                             // check
        ];
        for (i, value) in fields.iter().enumerate() {
            header[6 + i * 8..14 + i * 8].copy_from_slice(&hex8(*value));
        }
        archive.extend_from_slice(&header);
        archive.extend_from_slice(name);
        archive.push(0);
        while !archive.len().is_multiple_of(4) {
            archive.push(0);
        }
        archive.extend_from_slice(data);
        while !archive.len().is_multiple_of(4) {
            archive.push(0);
        }
        archive.len()
    }

    #[test]
    fn test_cpio_parse_entry_with_data() {
        // 带数据的常规文件条目: 名字与数据**各自**补齐到 4 字节, 两处 padding 是
        // cpio 解析最典型的 off-by-one 位置 (生成器与本解析器必须逐字节同形).
        let mut archive = alloc::vec::Vec::new();
        let next = push_test_entry(&mut archive, b"bin/eash", CPIO_S_IFDIR | 0o0755, b"");
        // 头 110 + 名字块 9 = 119 ⇒ 按绝对偏移取齐到 120
        assert_eq!(next, align4(110 + 9), "名字块按绝对偏移 4 对齐");
        assert_eq!(archive.len(), next, "目录条目无数据, 下一条目紧接名字块");

        let file_next = push_test_entry(&mut archive, b"a", CPIO_S_IFREG | 0o0644, b"XYZ");
        let (entry, advanced) = parse_next_entry(&archive, next).expect("解析带数据的文件条目");
        assert_eq!(entry.name, b"a", "名字不含末尾 NUL");
        assert_eq!(entry.data, b"XYZ", "数据按 filesize 切出, 不含尾部 padding");
        assert_eq!(entry.mode & CPIO_S_IFMT, CPIO_S_IFREG, "常规文件类型位");
        assert_eq!(entry.mode & 0o7777, 0o0644, "权限位可读");
        assert_eq!(advanced, file_next, "推进偏移与生成器一致");

        // 末尾挂 TRAILER: 解析器必须在那里停住 (返回 None), 否则 unpack 会越读
        push_test_entry(&mut archive, CPIO_TRAILER_NAME, 0, b"");
        assert!(
            parse_next_entry(&archive, file_next).is_none(),
            "TRAILER 必须被当成归档结束"
        );
    }

    #[test]
    fn test_cpio_parse_minimal() {
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
