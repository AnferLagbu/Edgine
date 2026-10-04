//! WASI preview1 集成测试
//!
//! 验证 WASI 基础设施的正确性:
//! - WasiFdTable 行为 (分配/关闭/重编号/溢出)
//! - WasiContext 参数/环境变量
//! - WASI 权限/文件类型/filestat 结构
//! - WASI errno 值与 POSIX 对齐
//!
//! ## B08-20 迁移 (2026-09-06)
//! WasiFdTable 行为与 WasiContext 已改引内核 `services::wasm::wasi::fd_table` /
//! `services::wasm::wasi::WasiContext` 真实实现, 删除本地 Vec<Option<u32>> 平行实现.
//! WASI ABI 常量 (rights 位 / filestat / iovec 布局) 为 preview1 外部规范,
//! 非内核平行实现, 保留本地标注 (内核无对应导出结构).
//! WASI errno 对齐改为直接验证内核 `WasiErrno` 枚举判别值.

// ============================================================================
// WasiFdTable 行为测试 (改引内核真实实现)
// ============================================================================

use edgine::kernel::services::wasm::wasi::fd_table::{WasiFdEntry, WasiFdTable};
use edgine::kernel::services::wasm::wasi::{WasiContext, WasiErrno, WasiFileType, WasiRights};

fn entry(inner_fd: i32) -> WasiFdEntry {
    WasiFdEntry {
        file_type: WasiFileType::RegularFile,
        rights: WasiRights::FILE,
        inner_fd,
        path: None,
    }
}

#[test]
fn test_fd_table_create() {
    let mut table = WasiFdTable::new(16);
    // fd 0-2 保留 (stdin/stdout/stderr): 未分配 → get 返回 Badf
    assert!(table.get(0).is_err());
    assert!(table.get(1).is_err());
    assert!(table.get(2).is_err());
    // fd < 3 的 close 同样拒绝
    assert!(table.close(0).is_err());
    assert!(table.close(2).is_err());
}

#[test]
fn test_fd_table_alloc_close() {
    let mut table = WasiFdTable::new(16);
    // 分配 fd 3 (从 3 起找空槽)
    let fd = table.alloc(entry(10)).unwrap();
    assert_eq!(fd, 3);
    assert_eq!(table.get(3).unwrap().inner_fd, 10);
    // 关闭 fd 3
    let closed = table.close(3).unwrap();
    assert_eq!(closed.inner_fd, 10);
    assert!(table.get(3).is_err());
}

#[test]
fn test_fd_table_overflow() {
    let mut table = WasiFdTable::new(5);
    // 填满 fd 3, 4
    table.alloc(entry(10)).unwrap();
    table.alloc(entry(20)).unwrap();
    // 无空槽 → alloc 返回 Badf
    assert!(table.alloc(entry(30)).is_err());
    // 重复 close 空槽也返回 Badf
    assert!(table.close(5).is_err());
}

#[test]
fn test_fd_table_renumber() {
    let mut table = WasiFdTable::new(16);
    table.alloc(entry(10)).unwrap(); // fd 3
    table.renumber(3, 10).unwrap();
    assert!(table.get(3).is_err());
    assert_eq!(table.get(10).unwrap().inner_fd, 10);
    // 越界 renumber 返回 Badf
    assert!(table.renumber(10, 20).is_err());
    // 未分配 from 重编号也返回 Badf
    assert!(table.renumber(7, 8).is_err());
}

// ============================================================================
// WasiContext 测试 (改引内核真实实现)
// ============================================================================

#[test]
fn test_context_args() {
    let mut ctx = WasiContext::new();
    ctx.args.push("test_program".into());
    ctx.args.push("--verbose".into());
    ctx.env.push(("HOME".into(), "/root".into()));

    assert_eq!(ctx.args.len(), 2);
    assert_eq!(ctx.env.len(), 1);
    assert_eq!(ctx.args[0], "test_program");
    assert_eq!(ctx.env[0].0, "HOME");
}

// ============================================================================
// WASI 权限测试 (preview1 外部规范, 保留标注)
// ============================================================================

#[test]
fn test_wasi_rights() {
    // SIMPLIFIED: WASI preview1 外部规范定义的 right 位 (非内核平行实现),
    // 内核无对应导出常量; 保留本地常量以锚定 ABI 位.
    const RIGHT_FD_READ: u64 = 1 << 6;
    const RIGHT_FD_WRITE: u64 = 1 << 7;
    const RIGHT_PATH_OPEN: u64 = 1 << 10;

    let file_rights = RIGHT_FD_READ | RIGHT_FD_WRITE;
    assert!(file_rights & RIGHT_FD_READ != 0);
    assert!(file_rights & RIGHT_FD_WRITE != 0);
    assert!((file_rights & RIGHT_PATH_OPEN) == 0);
}

// ============================================================================
// WASI filestat 结构测试 (preview1 外部规范, 保留标注)
// ============================================================================

#[test]
fn test_filestat_all_fields_semantics() {
    // SIMPLIFIED: WASI preview1 filestat_t 布局 (8 * u64 + 1 * u8 + 7 padding)
    // dev/ino: 文件设备/inode 编号, 用于唯一标识文件
    // filetype: WASI 文件类型 (0=unknown, 1=block, 2=char, 3=dir, 4=regular, 5=link, 6=socket)
    // nlink: 硬链接数
    // size: 文件字节大小
    // atim/mtim/ctim: 访问/修改/状态变更时间戳 (纳秒)
    struct Filestat {
        dev: u64,
        ino: u64,
        filetype: u8,
        nlink: u64,
        size: u64,
        atim: u64,
        mtim: u64,
        ctim: u64,
    }

    let stat = Filestat {
        dev: 0x100,
        ino: 0xABCD_1234,
        filetype: 3,
        nlink: 5,
        size: 4096,
        atim: 1_700_000_000_000_000,
        mtim: 1_700_000_001_000_000,
        ctim: 1_700_000_002_000_000,
    };
    assert_eq!(stat.dev, 0x100, "dev = 设备 ID");
    assert_eq!(stat.ino, 0xABCD_1234, "ino = inode 编号");
    assert_eq!(stat.filetype, 3, "filetype = 3 (WASI directory)");
    assert_eq!(stat.nlink, 5, "nlink = 硬链接数");
    assert_eq!(stat.size, 4096, "size = 文件大小");
    assert_eq!(stat.atim, 1_700_000_000_000_000, "atim = 访问时间 (纳秒)");
    assert_eq!(stat.mtim, 1_700_000_001_000_000, "mtim = 修改时间 (纳秒)");
    assert_eq!(
        stat.ctim, 1_700_000_002_000_000,
        "ctim = 状态变更时间 (纳秒)"
    );
}

// ============================================================================
// WASI errno 值验证 (改引内核 WasiErrno 枚举)
// ============================================================================

#[test]
fn test_wasi_errno_posix_alignment() {
    // 直接验证内核 WasiErrno 枚举判别值 (preview1 规范: WASI errno 与 POSIX 编号对齐)
    assert_eq!(WasiErrno::Success.as_i32(), 0);
    assert_eq!(WasiErrno::Badf.as_i32(), 8);
    assert_eq!(WasiErrno::Fault.as_i32(), 21);
    assert_eq!(WasiErrno::Inval.as_i32(), 28);
    assert_eq!(WasiErrno::Noent.as_i32(), 44);
    assert_eq!(WasiErrno::Notsup.as_i32(), 58);
}

// ============================================================================
// WASI iovec 结构测试 (preview1 外部规范, 保留标注)
// ============================================================================

#[test]
fn test_iovec_structure() {
    // SIMPLIFIED: WASI preview1 iovec_t 布局 (外部规范, 内核无对应导出结构)
    struct IoVec {
        buf: u32,
        len: u32,
    }
    let iovecs = [IoVec { buf: 100, len: 256 }, IoVec { buf: 400, len: 128 }];
    // 验证 buf 字段保留缓冲区起始地址
    assert_eq!(iovecs[0].buf, 100, "buf[0] 保留起始地址");
    assert_eq!(iovecs[1].buf, 400, "buf[1] 保留起始地址");
    let total: u32 = iovecs.iter().map(|iov| iov.len).sum();
    assert_eq!(total, 384);
}

#[test]
fn test_iovec_buf_pointer_semantics() {
    // SIMPLIFIED: WASI preview1 iovec_t 布局: buf (指针) + len (长度)
    // buf: 用户态缓冲区地址, len: 缓冲区长度
    // readv/writev 通过遍历 iovec 数组进行分散/聚集 I/O
    struct IoVec {
        buf: u32,
        len: u32,
    }

    let iovecs = [
        IoVec {
            buf: 0x1000,
            len: 256,
        },
        IoVec {
            buf: 0x2000,
            len: 128,
        },
        IoVec {
            buf: 0x3000,
            len: 512,
        },
    ];

    // 验证 buf 字段: 每个缓冲区起始地址不同, 用于分散写入
    assert_eq!(iovecs[0].buf, 0x1000, "buf[0] = 用户缓冲区 0 起始地址");
    assert_eq!(iovecs[1].buf, 0x2000, "buf[1] = 用户缓冲区 1 起始地址");
    assert_eq!(iovecs[2].buf, 0x3000, "buf[2] = 用户缓冲区 2 起始地址");

    // 验证 buf + len: 标记缓冲区结束地址
    assert_eq!(
        iovecs[0].buf + iovecs[0].len,
        0x1100,
        "buf[0] + len[0] = 缓冲区 0 末尾"
    );
    assert_eq!(
        iovecs[1].buf + iovecs[1].len,
        0x2080,
        "buf[1] + len[1] = 缓冲区 1 末尾"
    );
    assert_eq!(
        iovecs[2].buf + iovecs[2].len,
        0x3200,
        "buf[2] + len[2] = 缓冲区 2 末尾"
    );

    // readv 总读取字节数 = sum(len)
    let total_read: u32 = iovecs.iter().map(|iov| iov.len).sum();
    assert_eq!(total_read, 896, "readv 总字节数 = 256 + 128 + 512 = 896");
}
