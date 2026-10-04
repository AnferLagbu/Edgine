#![deny(unsafe_code)]
//! WASI Socket: `sock_accept`, `sock_connect`, `sock_recv`, `sock_send`
//!
//! Socket 函数桥接到 `functions::net` 层。WASI fd 通过 `WasiFdTable` 映射到
//! 内部 POSIX fd，再调用 `functions::net` 的安全 API。

use super::fd_table::{read_iovec_from_memory, write_i32_to_memory, write_u32_to_memory};
use super::{WasiContext, WasiErrno, wasi_errno, wasi_success};
use crate::functions::wasm::interpreter::Interpreter;
use crate::functions::wasm::types::{Value, WasmError};

/// WASI `sock_accept`: 接受连接
///
/// # Errors
///
/// 当栈弹出参数失败或向解释器栈压入结果失败时返回对应的 `WasmError`.
pub fn wasi_sock_accept(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let _flags = interp.stack.pop_i32()? as u32;
    let _addr_ptr = interp.stack.pop_i32()? as u32;
    let _addr_len_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 调用 functions::net accept
    match crate::functions::net::socket::accept(entry.inner_fd) {
        Ok(new_fd) => {
            // 创建新的 WASI fd 表条目
            let new_entry = super::fd_table::WasiFdEntry {
                file_type: super::fd_table::WasiFileType::Socket,
                rights: super::fd_table::WasiRights::ALL,
                inner_fd: new_fd,
                path: None,
            };
            match ctx.fd_table.alloc(new_entry) {
                Ok(wasi_fd) => {
                    interp.stack.push(Value::I32(wasi_fd as i32))?;
                }
                Err(e) => {
                    interp.stack.push(Value::I32(wasi_errno(e)))?;
                }
            }
        }
        Err(_) => {
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
        }
    }
    Ok(())
}

/// WASI `sock_connect`: 连接到地址
///
/// # Errors
///
/// 当栈弹出参数失败、解释器未配置线性内存或压栈结果失败时
/// 返回对应的 `WasmError`.
pub fn wasi_sock_connect(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let addr_ptr = interp.stack.pop_i32()? as u32;
    let _addr_len = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    // 从 WASM 线性内存读取 sockaddr_in 结构
    // sockaddr_in: { sin_family: u16, sin_port: u16 (big-endian), sin_addr: [u8; 4] }
    let mem = interp.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
    let base = u64::from(addr_ptr);

    let read_u16 = |off: u64| -> u16 {
        let lo = mem.read_u8((base + off) as u32).unwrap_or(0);
        let hi = mem.read_u8((base + off + 1) as u32).unwrap_or(0);
        u16::from_le_bytes([lo, hi])
    };

    let sin_port = read_u16(2);
    let sin_addr = [
        mem.read_u8((base + 4) as u32).unwrap_or(0),
        mem.read_u8((base + 5) as u32).unwrap_or(0),
        mem.read_u8((base + 6) as u32).unwrap_or(0),
        mem.read_u8((base + 7) as u32).unwrap_or(0),
    ];

    let addr = crate::functions::net::socket::SockAddrIn::new(sin_port, sin_addr);

    match crate::functions::net::socket::connect(entry.inner_fd, &addr) {
        Ok(()) => {
            interp.stack.push(Value::I32(wasi_success()))?;
        }
        Err(_) => {
            interp
                .stack
                .push(Value::I32(wasi_errno(WasiErrno::Connrefused)))?;
        }
    }
    Ok(())
}

/// WASI `sock_recv`: 从 socket 接收数据
///
/// # Errors
///
/// 当栈弹出参数失败、读取 iovec 失败或压栈结果失败时返回对应的 `WasmError`.
pub fn wasi_sock_recv(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let iovs_ptr = interp.stack.pop_i32()? as u32;
    let iovs_len = interp.stack.pop_i32()? as u32;
    let _flags = interp.stack.pop_i32()? as u32;
    let nread_ptr = interp.stack.pop_i32()? as u32;
    let roflags_ptr = interp.stack.pop_i32()? as u32;

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
        // 分配临时缓冲区接收数据
        let mut buf = alloc::vec![0u8; iov.len as usize];
        if let Ok(n) = crate::functions::net::socket::recv(entry.inner_fd, &mut buf) {
            // 将数据写回 WASM 线性内存
            if let Some(ref mut mem) = interp.memory {
                for (i, &byte) in buf[..n].iter().enumerate() {
                    let _ = mem.write_u8(iov.buf + i as u32, byte);
                }
            }
            total += n as u32;
        } else {
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
            return Ok(());
        }
    }

    write_u32_to_memory(interp, nread_ptr, total);
    write_i32_to_memory(interp, roflags_ptr, 0);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `sock_send`: 向 socket 发送数据
///
/// # Errors
///
/// 当栈弹出参数失败、读取 iovec 失败或压栈结果失败时返回对应的 `WasmError`.
pub fn wasi_sock_send(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let fd = interp.stack.pop_i32()? as u32;
    let iovs_ptr = interp.stack.pop_i32()? as u32;
    let iovs_len = interp.stack.pop_i32()? as u32;
    let _flags = interp.stack.pop_i32()? as u32;
    let nwritten_ptr = interp.stack.pop_i32()? as u32;

    let entry = match ctx.fd_table.get(fd) {
        Ok(e) => e,
        Err(e) => {
            interp.stack.push(Value::I32(wasi_errno(e)))?;
            return Ok(());
        }
    };

    let iovecs = read_iovec_from_memory(interp, iovs_ptr, iovs_len)?;

    // 收集所有 iovec 数据到一个连续缓冲区
    let mut send_buf = alloc::vec::Vec::new();
    for iov in &iovecs {
        if iov.len == 0 {
            continue;
        }
        if let Some(ref mem) = interp.memory {
            for i in 0..iov.len {
                let byte = mem.read_u8(iov.buf + i).unwrap_or(0);
                send_buf.push(byte);
            }
        }
    }

    match crate::functions::net::socket::send(entry.inner_fd, &send_buf) {
        Ok(n) => {
            write_u32_to_memory(interp, nwritten_ptr, n as u32);
            interp.stack.push(Value::I32(wasi_success()))?;
        }
        Err(_) => {
            interp.stack.push(Value::I32(wasi_errno(WasiErrno::Io)))?;
        }
    }
    Ok(())
}
