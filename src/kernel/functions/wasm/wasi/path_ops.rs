#![deny(unsafe_code)]
//! WASI 路径操作: `path_open`, `path_create_directory`, `path_remove_directory`,
//! `path_unlink_file`, `path_relative_path`, `path_symlink`, `path_readlink`,
//! `path_filestat_get`, `path_filestat_set_times`, `path_link`
//!
//! 本模块使用 privileged 层的 safe wrapper (vfs_*_safe) 调用 VFS，
//! 无需 unsafe 块。

use super::fd_table::{
    WasiFdEntry, WasiFileType, WasiRights, read_bytes_from_memory, write_u32_to_memory,
};
use super::{WasiContext, WasiErrno, wasi_errno, wasi_success};
use crate::functions::wasm::interpreter::Interpreter;
use crate::functions::wasm::types::{Value, WasmError};
use alloc::string::String;

/// 从 WASM 线性内存读取路径字符串 (NUL 终止)
fn read_path(interp: &Interpreter, ptr: u32, len: u32) -> Result<String, WasiErrno> {
    let bytes = read_bytes_from_memory(interp, ptr, len)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..end])
        .map(String::from)
        .map_err(|_| WasiErrno::Inval)
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 解析路径: 将 dirfd + relative path 组合为绝对路径
///
/// WASI 语义: dirfd 是 preopen fd (通过 `fd_prestat_get` 获取), relative path
/// 相对于 dirfd 的 preopen `路径。AT_FDCWD` (0xffffff9c) 表示使用当前工作目录。
fn resolve_path(ctx: &WasiContext, dirfd: u32, path: &str) -> Result<String, WasiErrno> {
    if dirfd == 0xffffff9c {
        return Ok(String::from(path));
    }

    let entry = ctx.fd_table.get(dirfd)?;
    entry.path.as_ref().map_or_else(
        || Ok(String::from(path)),
        |base_path| {
            if path.starts_with('/') {
                Ok(String::from(path))
            } else if base_path.ends_with('/') {
                Ok(alloc::format!("{base_path}{path}"))
            } else {
                Ok(alloc::format!("{base_path}/{path}"))
            }
        },
    )
}

/// WASI `o_flags` → VFS flags 映射
fn wasi_o_flags_to_vfs(o_flags: u32) -> u32 {
    let mut flags = o_flags & 0x03; // 低 2 位: O_RDONLY/O_WRONLY/O_RDWR
    if o_flags & 0x100 != 0 {
        flags |= 0x100;
    } // O_CREAT
    if o_flags & 0x200 != 0 {
        flags |= 0x200;
    } // O_EXCL
    if o_flags & 0x400 != 0 {
        flags |= 0x400;
    } // O_TRUNC
    if o_flags & 0x008 != 0 {
        flags |= 0x008;
    } // O_APPEND
    flags
}

/// WASI `path_open`: 打开路径上的文件/目录
///
/// # Errors
///
/// 当栈弹出参数失败、路径读取/解析失败或写入线性内存失败时
/// 返回对应的 `WasmError`.
pub fn wasi_path_open(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let dirfd = interp.stack.pop_i32()? as u32;
    let _dirflags = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;
    let o_flags = interp.stack.pop_i32()? as u32;
    let _fs_rights_base = interp.stack.pop_i64()? as u64;
    let _fs_rights_inheriting = interp.stack.pop_i64()? as u64;
    let _fd_flags = interp.stack.pop_i32()? as u32;
    let fd_ptr = interp.stack.pop_i32()? as u32;

    let path = read_path(interp, path_ptr, path_len)?;
    let abs_path = resolve_path(ctx, dirfd, &path)?;
    let vfs_flags = wasi_o_flags_to_vfs(o_flags);

    // 使用 safe wrapper 调用 VFS
    let vfs_fd = crate::functions::fs::api::vfs_open_safe(&abs_path, vfs_flags, 0);

    if vfs_fd < 0 {
        interp
            .stack
            .push(Value::I32(wasi_errno(WasiErrno::Noent)))?;
        return Ok(());
    }

    let entry = WasiFdEntry {
        file_type: WasiFileType::RegularFile,
        rights: WasiRights::FILE,
        inner_fd: vfs_fd,
        path: Some(path),
    };

    match ctx.fd_table.alloc(entry) {
        Ok(new_fd) => {
            write_u32_to_memory(interp, fd_ptr, new_fd);
            interp.stack.push(Value::I32(wasi_success()))?;
        }
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
        }
    }
    Ok(())
}

/// WASI `path_create_directory`: 创建目录
///
/// # Errors
///
/// 当栈弹出参数失败或路径读取/解析失败时返回对应的 `WasmError`.
pub fn wasi_path_create_directory(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let dirfd = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;

    let path = read_path(interp, path_ptr, path_len)?;
    let abs_path = resolve_path(ctx, dirfd, &path)?;

    let result = crate::functions::fs::api::vfs_mkdir_safe(&abs_path, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `path_remove_directory`: 删除目录
///
/// # Errors
///
/// 当栈弹出参数失败或路径读取/解析失败时返回对应的 `WasmError`.
pub fn wasi_path_remove_directory(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let dirfd = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;

    let path = read_path(interp, path_ptr, path_len)?;
    let abs_path = resolve_path(ctx, dirfd, &path)?;

    let result = crate::functions::fs::api::vfs_rmdir_safe(&abs_path, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `path_unlink_file`: 删除文件
///
/// # Errors
///
/// 当栈弹出参数失败或路径读取/解析失败时返回对应的 `WasmError`.
pub fn wasi_path_unlink_file(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let dirfd = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;

    let path = read_path(interp, path_ptr, path_len)?;
    let abs_path = resolve_path(ctx, dirfd, &path)?;

    let result = crate::functions::fs::api::vfs_unlink_safe(&abs_path, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `path_symlink`: 创建符号链接
///
/// # Errors
///
/// 当栈弹出参数失败或路径读取/解析失败时返回对应的 `WasmError`.
pub fn wasi_path_symlink(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let old_path_ptr = interp.stack.pop_i32()? as u32;
    let old_path_len = interp.stack.pop_i32()? as u32;
    let dirfd = interp.stack.pop_i32()? as u32;
    let new_path_ptr = interp.stack.pop_i32()? as u32;
    let new_path_len = interp.stack.pop_i32()? as u32;

    let old_path = read_path(interp, old_path_ptr, old_path_len)?;
    let new_path = read_path(interp, new_path_ptr, new_path_len)?;
    let _abs_new = resolve_path(ctx, dirfd, &new_path)?;

    // WASI: old_path = target, new_path = linkpath
    let result = crate::functions::fs::api::vfs_symlink_safe(&old_path, &new_path, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `path_readlink`: 读取符号链接目标
///
/// # Errors
///
/// 当栈弹出参数失败、路径读取/解析失败或写入线性内存失败时
/// 返回对应的 `WasmError`.
pub fn wasi_path_readlink(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let dirfd = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;
    let buf_ptr = interp.stack.pop_i32()? as u32;
    let buf_len = interp.stack.pop_i32()? as u32;
    let buf_used_ptr = interp.stack.pop_i32()? as u32;

    let path = read_path(interp, path_ptr, path_len)?;
    let abs_path = resolve_path(ctx, dirfd, &path)?;

    // 分配临时缓冲区接收 readlink 结果
    let mut link_buf = alloc::vec![0u8; buf_len as usize];
    let result = crate::functions::fs::api::vfs_readlink_safe(&abs_path, &mut link_buf, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        // 将结果写回 WASM 线性内存
        if let Some(ref mut mem) = interp.memory {
            for i in 0..result as usize {
                let _ = mem.write_u8(buf_ptr + i as u32, link_buf[i]);
            }
        }
        write_u32_to_memory(interp, buf_used_ptr, result as u32);
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `path_rename`: 重命名文件/目录
///
/// # Errors
///
/// 当栈弹出参数失败或路径读取/解析失败时返回对应的 `WasmError`.
pub fn wasi_path_rename(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let old_dirfd = interp.stack.pop_i32()? as u32;
    let old_path_ptr = interp.stack.pop_i32()? as u32;
    let old_path_len = interp.stack.pop_i32()? as u32;
    let new_dirfd = interp.stack.pop_i32()? as u32;
    let new_path_ptr = interp.stack.pop_i32()? as u32;
    let new_path_len = interp.stack.pop_i32()? as u32;

    let old_path = read_path(interp, old_path_ptr, old_path_len)?;
    let new_path = read_path(interp, new_path_ptr, new_path_len)?;
    let old_abs = resolve_path(ctx, old_dirfd, &old_path)?;
    let new_abs = resolve_path(ctx, new_dirfd, &new_path)?;

    let result = crate::functions::fs::api::vfs_rename_safe(&old_abs, &new_abs, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// WASI `path_filestat_get`: 获取文件/目录状态
///
/// # Errors
///
/// 当栈弹出参数失败、路径读取/解析失败或写入线性内存失败时
/// 返回对应的 `WasmError`.
pub fn wasi_path_filestat_get(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let dirfd = interp.stack.pop_i32()? as u32;
    let _flags = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;
    let buf_ptr = interp.stack.pop_i32()? as u32;

    let path = read_path(interp, path_ptr, path_len)?;
    let abs_path = resolve_path(ctx, dirfd, &path)?;

    let stat = crate::functions::fs::api::with_cstr(&abs_path, |ptr| {
        crate::functions::fs::api::vfs_stat_safe(ptr, 0)
    });
    let stat = if let Some(s) = stat {
        s
    } else {
        interp
            .stack
            .push(Value::I32(wasi_errno(WasiErrno::Noent)))?;
        return Ok(());
    };

    // 写入 WASI filestat 结构
    if let Some(ref mut mem) = interp.memory {
        let base = u64::from(buf_ptr);
        let write_u64 =
            |mem: &mut crate::functions::wasm::runtime::LinearMemory, off: u64, val: u64| {
                let bytes = val.to_le_bytes();
                for i in 0..8u64 {
                    let _ = mem.write_u8((base + off + i) as u32, bytes[i as usize]);
                }
            };

        write_u64(mem, 0, u64::from(stat.node_id));
        write_u64(mem, 8, u64::from(stat.node_id));
        let _ = mem.write_u8((base + 16) as u32, stat.file_type as u8);
        write_u64(mem, 17, 1);
        write_u64(mem, 25, u64::from(stat.size));
        write_u64(mem, 33, stat.atime);
        write_u64(mem, 41, stat.mtime);
        write_u64(mem, 49, stat.ctime);
    }

    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `path_filestat_set_times`: 设置文件/目录时间戳
///
/// # Errors
///
/// 当栈弹出参数失败或路径读取/解析失败时返回对应的 `WasmError`.
pub fn wasi_path_filestat_set_times(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let dirfd = interp.stack.pop_i32()? as u32;
    let _flags = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;
    let atim = interp.stack.pop_i64()? as u64;
    let mtim = interp.stack.pop_i64()? as u64;

    let path = read_path(interp, path_ptr, path_len)?;
    let abs_path = resolve_path(ctx, dirfd, &path)?;

    let result = crate::functions::fs::api::vfs_utimensat_safe(&abs_path, atim, mtim, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `path_link`: 创建硬链接
///
/// # Errors
///
/// 当栈弹出参数失败或路径读取/解析失败时返回对应的 `WasmError`.
pub fn wasi_path_link(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let old_dirfd = interp.stack.pop_i32()? as u32;
    let _old_flags = interp.stack.pop_i32()? as u32;
    let old_path_ptr = interp.stack.pop_i32()? as u32;
    let old_path_len = interp.stack.pop_i32()? as u32;
    let new_dirfd = interp.stack.pop_i32()? as u32;
    let new_path_ptr = interp.stack.pop_i32()? as u32;
    let new_path_len = interp.stack.pop_i32()? as u32;

    let old_path = read_path(interp, old_path_ptr, old_path_len)?;
    let new_path = read_path(interp, new_path_ptr, new_path_len)?;
    let old_abs = resolve_path(ctx, old_dirfd, &old_path)?;
    let new_abs = resolve_path(ctx, new_dirfd, &new_path)?;

    let result = crate::functions::fs::api::vfs_link_safe(&old_abs, &new_abs, 0);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}
