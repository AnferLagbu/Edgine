#![deny(unsafe_code)]
//! WASI FD 管理与 I/O: `fd_close`, `fd_seek`, `fd_tell`, `fd_sync`,
//! `fd_prestat_get`, `fd_prestat_dir_name`, `fd_stat_get`,
//! `fd_read`, `fd_write`, `fd_pread`, `fd_pwrite`, `fd_allocate`, `fd_advise`,
//! `fd_renumber`, `fd_dup`, `fd_readdir`

use super::fd_table::{read_iovec_from_memory, write_u32_to_memory};
use super::{WasiContext, WasiErrno, wasi_errno, wasi_success};
use crate::services::wasm::interpreter::Interpreter;
use crate::services::wasm::types::{Value, WasmError};

// ============================================================================
// G4: FD 管理
// ============================================================================

/// WASI `fd_close`: 关闭文件描述符
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_close(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    match ctx.fd_table.close(fd) {
        Ok(entry) => {
            // 调用 VFS 关闭底层 fd
            if entry.inner_fd >= 0 {
                crate::services::fs::api::vfs_close(entry.inner_fd as u32);
            }
            interp.stack.push(Value::I32(wasi_success()))?;
        }
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
        }
    }
    Ok(())
}

/// WASI `fd_seek`: 移动文件指针
///
/// WASI whence: 0=SET, 1=CUR, 2=END
/// VFS whence: 0=SET, 1=CUR, 2=END (相同)
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_seek(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let offset = interp.stack.pop_i64()? as i32;
    let whence = interp.stack.pop_i32()? as u32;
    let new_offset_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 调用 VFS seek
    let result = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, offset, whence);

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        write_u32_to_memory(interp, new_offset_ptr, result as u32);
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `fd_tell`: 获取文件指针位置
///
/// # Errors
///
/// 当栈弹出参数失败、写入线性内存失败或压栈结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_tell(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let offset_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // seek(0, SEEK_CUR) 获取当前位置
    let result = crate::services::fs::api::vfs_seek(
        entry.inner_fd as u32,
        0,
        1, // SEEK_CUR
    );

    if result < 0 {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
    } else {
        write_u32_to_memory(interp, offset_ptr, result as u32);
        interp.stack.push(Value::I32(wasi_success()))?;
    }
    Ok(())
}

/// WASI `fd_sync`: 同步文件数据到存储
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_sync(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;

    match ctx.fd_table.get(fd) {
        Ok(_entry) => {
            // 调用 VFS sync (全局同步所有已打开的文件)
            let result = crate::services::fs::api::vfs_sync();
            if result < 0 {
                interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
            } else {
                interp.stack.push(Value::I32(wasi_success()))?;
            }
        }
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
        }
    }
    Ok(())
}

/// WASI `fd_prestat_get`: 获取 preopen fd 信息
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
///
/// # Panics
///
/// 内部对 `entry.path` 使用 `unwrap`; 若在 `is_none` 检查之后仍出现
/// 路径被修改为 `None` 的并发情况则会 panic(当前实现中该检查保证
/// 走到 unwrap 时路径必然存在).
pub fn wasi_fd_prestat_get(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let buf_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    if entry.path.is_none() {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Badf)))?;
        return Ok(());
    }

    let path_len = entry.path.as_ref().unwrap().len() as u32;
    write_u32_to_memory(interp, buf_ptr, path_len);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `fd_prestat_dir_name`: 获取 preopen 目录名称
///
/// # Errors
///
/// 当栈弹出参数失败、写入线性内存失败或压栈结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_prestat_dir_name(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let path_ptr = interp.stack.pop_i32()? as u32;
    let path_len = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    let path = if let Some(p) = &entry.path {
        p.as_bytes()
    } else {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Badf)))?;
        return Ok(());
    };

    let copy_len = core::cmp::min(path.len() as u32, path_len) as usize;
    super::fd_table::write_bytes_to_memory(interp, path_ptr, &path[..copy_len]);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// WASI `fd_stat_get`: 获取 fd 状态信息
///
/// # Errors
///
/// 当栈弹出参数失败、写入线性内存失败或压栈结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_stat_get(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let buf_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 调用 VFS fstat 获取文件信息
    let stat = if let Some(s) = crate::services::fs::api::vfs_fstat_safe(entry.inner_fd as u32, 0) {
        s
    } else {
        interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
        return Ok(());
    };

    // 写入 WASI filestat 结构到线性内存
    // filestat: { dev: u64, ino: u64, filetype: u8, nlink: u64, size: u64, atim: u64, mtim: u64, ctim: u64 }
    if let Some(ref mut mem) = interp.memory {
        let base = u64::from(buf_ptr);

        // 辅助函数: 写入 u64 到线性内存
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        fn write_u64_to(
            mem: &mut crate::services::wasm::runtime::LinearMemory,
            base: u64,
            off: u64,
            val: u64,
        ) {
            let bytes = val.to_le_bytes();
            for i in 0..8u64 {
                let _ = mem.write_u8((base + off + i) as u32, bytes[i as usize]);
            }
        }

        write_u64_to(mem, base, 0, u64::from(stat.node_id)); // dev
        write_u64_to(mem, base, 8, u64::from(stat.node_id)); // ino
        let _ = mem.write_u8((base + 16) as u32, stat.file_type as u8); // filetype
        write_u64_to(mem, base, 17, 1); // nlink (默认 1)
        write_u64_to(mem, base, 25, u64::from(stat.size)); // size
        write_u64_to(mem, base, 33, stat.atime); // atim
        write_u64_to(mem, base, 41, stat.mtime); // mtim
        write_u64_to(mem, base, 49, stat.ctime); // ctim
    }

    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

// ============================================================================
// G5: FD I/O
// ============================================================================

/// WASI `fd_read`: 从 fd 读取数据到 iovec 数组
///
/// # Errors
///
/// 当栈弹出参数失败、读取 iovec 失败、线性内存访问越界或压栈结果
/// 失败时返回对应的 `WasmError`.
pub fn wasi_fd_read(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let iovs_ptr = interp.stack.pop_i32()? as u32;
    let iovs_len = interp.stack.pop_i32()? as u32;
    let nread_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    let iovecs = read_iovec_from_memory(interp, iovs_ptr, iovs_len)?;
    let mut total = 0u32;

    for iov in &iovecs {
        if iov.len == 0 {
            continue;
        }
        // 使用 safe 方法获取 WASM 线性内存切片
        let slice = interp
            .memory
            .as_mut()
            .ok_or(WasmError::MemoryOutOfBounds)?
            .get_slice_mut(iov.buf, iov.len)
            .map_err(|_| WasmError::MemoryOutOfBounds)?;

        // 使用 safe wrapper 调用 VFS read
        let n = crate::services::fs::api::vfs_read_safe(entry.inner_fd as u32, slice);

        if n < 0 {
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
            return Ok(());
        }
        total += n as u32;
    }

    write_u32_to_memory(interp, nread_ptr, total);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `fd_write`: 将 iovec 数组中的数据写入 fd
///
/// # Errors
///
/// 当栈弹出参数失败、读取 iovec 失败、线性内存访问越界或压栈结果
/// 失败时返回对应的 `WasmError`.
pub fn wasi_fd_write(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let iovs_ptr = interp.stack.pop_i32()? as u32;
    let iovs_len = interp.stack.pop_i32()? as u32;
    let nwritten_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    let iovecs = read_iovec_from_memory(interp, iovs_ptr, iovs_len)?;
    let mut total = 0u32;

    for iov in &iovecs {
        if iov.len == 0 {
            continue;
        }
        // 使用 safe 方法获取 WASM 线性内存切片
        let slice = interp
            .memory
            .as_ref()
            .ok_or(WasmError::MemoryOutOfBounds)?
            .get_slice(iov.buf, iov.len)
            .map_err(|_| WasmError::MemoryOutOfBounds)?;

        // 使用 safe wrapper 调用 VFS write
        let n = crate::services::fs::api::vfs_write_safe(entry.inner_fd as u32, slice);

        if n < 0 {
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
            return Ok(());
        }
        total += n as u32;
    }

    write_u32_to_memory(interp, nwritten_ptr, total);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `fd_pread`: 从 fd 指定偏移读取
///
/// # Errors
///
/// 当栈弹出参数失败、读取 iovec 失败、线性内存访问越界或压栈结果
/// 失败时返回对应的 `WasmError`.
pub fn wasi_fd_pread(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let iovs_ptr = interp.stack.pop_i32()? as u32;
    let iovs_len = interp.stack.pop_i32()? as u32;
    let offset = interp.stack.pop_i64()? as i32;
    let nread_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 保存当前位置，seek 到 offset，读取，再 seek 回原位
    let saved_pos = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, 0, 1);
    let _ = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, offset, 0);

    let iovecs = read_iovec_from_memory(interp, iovs_ptr, iovs_len)?;
    let mut total = 0u32;

    for iov in &iovecs {
        if iov.len == 0 {
            continue;
        }
        let slice = interp
            .memory
            .as_mut()
            .ok_or(WasmError::MemoryOutOfBounds)?
            .get_slice_mut(iov.buf, iov.len)
            .map_err(|_| WasmError::MemoryOutOfBounds)?;
        let n = crate::services::fs::api::vfs_read_safe(entry.inner_fd as u32, slice);
        if n < 0 {
            let _ = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, saved_pos, 0);
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
            return Ok(());
        }
        total += n as u32;
    }

    // 恢复原位置
    let _ = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, saved_pos, 0);

    write_u32_to_memory(interp, nread_ptr, total);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `fd_pwrite`: 向 fd 指定偏移写入
///
/// # Errors
///
/// 当栈弹出参数失败、读取 iovec 失败、线性内存访问越界或压栈结果
/// 失败时返回对应的 `WasmError`.
pub fn wasi_fd_pwrite(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let iovs_ptr = interp.stack.pop_i32()? as u32;
    let iovs_len = interp.stack.pop_i32()? as u32;
    let offset = interp.stack.pop_i64()? as i32;
    let nwritten_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 保存当前位置，seek 到 offset，写入，再 seek 回原位
    let saved_pos = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, 0, 1);
    let _ = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, offset, 0);

    let iovecs = read_iovec_from_memory(interp, iovs_ptr, iovs_len)?;
    let mut total = 0u32;

    for iov in &iovecs {
        if iov.len == 0 {
            continue;
        }
        let slice = interp
            .memory
            .as_ref()
            .ok_or(WasmError::MemoryOutOfBounds)?
            .get_slice(iov.buf, iov.len)
            .map_err(|_| WasmError::MemoryOutOfBounds)?;
        let n = crate::services::fs::api::vfs_write_safe(entry.inner_fd as u32, slice);
        if n < 0 {
            let _ = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, saved_pos, 0);
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
            return Ok(());
        }
        total += n as u32;
    }

    // 恢复原位置
    let _ = crate::services::fs::api::vfs_seek(entry.inner_fd as u32, saved_pos, 0);

    write_u32_to_memory(interp, nwritten_ptr, total);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `fd_allocate`: 预分配文件空间
///
/// WASI 语义: 确保文件至少有 offset+len 字节的空间。
/// 实现: 使用 `vfs_truncate` 扩展文件到目标大小 (offset+len)。
/// 若文件已足够大则不操作。
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_allocate(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let offset = interp.stack.pop_i64()? as u64;
    let len = interp.stack.pop_i64()? as u64;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 计算目标大小: offset + len
    let target_size = offset.saturating_add(len);

    // 获取当前文件大小
    let stat = crate::services::fs::api::vfs_fstat_safe(entry.inner_fd as u32, 0);

    let current_size = stat.map_or(0, |s| u64::from(s.size));
    if current_size < target_size {
        // 文件需要扩展, 使用 truncate
        let trunc_result =
            crate::services::fs::api::vfs_truncate_internal(entry.inner_fd as u32, target_size);
        if trunc_result < 0 {
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
            return Ok(());
        }
    }

    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `fd_advise`: 提示文件访问模式
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_advise(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let _offset = interp.stack.pop_i64()?;
    let _len = interp.stack.pop_i64()?;
    let _advice = interp.stack.pop_i32()?;

    match ctx.fd_table.get(fd) {
        Ok(_entry) => {
            // WASI advise 是提示性的，可忽略
            interp.stack.push(Value::I32(wasi_success()))?;
        }
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
        }
    }
    Ok(())
}

// ============================================================================
// G7: 高级 FD
// ============================================================================

/// WASI `fd_renumber`: 重编号 fd
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_renumber(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let from = interp.stack.pop_i32()? as u32;
    let to = interp.stack.pop_i32()? as u32;

    match ctx.fd_table.renumber(from, to) {
        Ok(()) => {
            interp.stack.push(Value::I32(wasi_success()))?;
        }
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
        }
    }
    Ok(())
}

/// WASI `fd_dup`: 复制 fd
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_dup(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    let new_entry = super::fd_table::WasiFdEntry {
        file_type: entry.file_type,
        rights: entry.rights,
        inner_fd: entry.inner_fd,
        path: None,
    };

    match ctx.fd_table.alloc(new_entry) {
        Ok(new_fd) => {
            interp.stack.push(Value::I32(new_fd as i32))?;
        }
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
        }
    }
    Ok(())
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
/// WASI `fd_readdir`: 读取目录内容
///
/// # Errors
///
/// 当栈弹出参数失败、写入线性内存失败或压栈结果失败时返回对应的 `WasmError`.
pub fn wasi_fd_readdir(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let buf_ptr = interp.stack.pop_i32()? as u32;
    let buf_len = interp.stack.pop_i32()? as u32;
    let _cookie = interp.stack.pop_i64()? as u64;
    let buf_used_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 调用 VFS readdir
    let mut dir_entry = crate::services::fs::vfs_types::VfsDirEntry::default();
    let result =
        crate::services::fs::api::vfs_readdir(entry.inner_fd as u32, &mut dir_entry as *mut _);

    if result < 0 {
        write_u32_to_memory(interp, buf_used_ptr, 0);
        interp.stack.push(Value::I32(wasi_success()))?;
        return Ok(());
    }

    // 将 VfsDirEntry 转换为 WASI dirent 格式写入缓冲区
    // WASI dirent: { inode: u64, next_cookie: u64, namlen: u16, filetype: u8, name: [u8] }
    // name 是 [u8; VFS_MAX_NAME] 固定数组, 找到 NUL 终止符确定长度
    let name_len = dir_entry
        .name
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(dir_entry.name.len());
    let copy_len = name_len.min(buf_len as usize - 19) as u16; // 19 = dirent header size

    if let Some(ref mut mem) = interp.memory {
        let base = u64::from(buf_ptr);
        // 写入 inode
        let inode_bytes = u64::from(dir_entry.node).to_le_bytes();
        for i in 0..8u64 {
            let _ = mem.write_u8((base + i) as u32, inode_bytes[i as usize]);
        }
        // 写入 next_cookie
        let cookie_bytes = (result as u64 + 1).to_le_bytes();
        for i in 0..8u64 {
            let _ = mem.write_u8((base + 8 + i) as u32, cookie_bytes[i as usize]);
        }
        // 写入 namlen
        let namlen_bytes = copy_len.to_le_bytes();
        let _ = mem.write_u8((base + 16) as u32, namlen_bytes[0]);
        let _ = mem.write_u8((base + 17) as u32, namlen_bytes[1]);
        // 写入 filetype
        let _ = mem.write_u8((base + 18) as u32, dir_entry.file_type as u8);
        // 写入文件名
        for i in 0..copy_len as usize {
            let _ = mem.write_u8((base + 19 + i as u64) as u32, dir_entry.name[i]);
        }
        write_u32_to_memory(interp, buf_used_ptr, 19 + u32::from(copy_len));
    }

    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}
