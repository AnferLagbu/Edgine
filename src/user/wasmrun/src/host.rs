//! `wasi_snapshot_preview1` host 层: 读 wasm 线性内存, 翻译到 Edgine syscall.
//!
//! 仅非 test 构建编译 (wasmi 与 userlib syscall 只在真实运行时可用).
//! 38 个函数按 explain 文档的 G1-G8 分组注册; 编码/表结构在 `wasi.rs` 单测.

use alloc::vec;
use alloc::vec::Vec;

use userlib::sys;
use wasmi::{Caller, Extern, Memory};

use crate::wasi::{self, WasiCtx, WasiFdEntry};

const MAX_IO: u32 = 8 * 1024 * 1024;
const MAX_IOVECS: u32 = 1024;

fn guest_memory(caller: &Caller<'_, WasiCtx>) -> Result<Memory, i32> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or(wasi::ERRNO_FAULT)
}

fn read_bytes(caller: &Caller<'_, WasiCtx>, ptr: u32, len: u32) -> Result<Vec<u8>, i32> {
    if len > MAX_IO {
        return Err(wasi::ERRNO_FAULT);
    }
    let mem = guest_memory(caller)?;
    let mut buf = alloc::vec![0u8; len as usize];
    mem.read(caller, ptr as usize, &mut buf)
        .map_err(|_| wasi::ERRNO_FAULT)?;
    Ok(buf)
}

fn write_bytes(caller: &mut Caller<'_, WasiCtx>, ptr: u32, data: &[u8]) -> Result<(), i32> {
    let mem = guest_memory(caller)?;
    mem.write(caller, ptr as usize, data)
        .map_err(|_| wasi::ERRNO_FAULT)
}

fn write_u32(caller: &mut Caller<'_, WasiCtx>, ptr: u32, v: u32) -> Result<(), i32> {
    write_bytes(caller, ptr, &v.to_le_bytes())
}

fn write_u64(caller: &mut Caller<'_, WasiCtx>, ptr: u32, v: u64) -> Result<(), i32> {
    write_bytes(caller, ptr, &v.to_le_bytes())
}

fn read_iovecs(
    caller: &Caller<'_, WasiCtx>,
    iovs_ptr: u32,
    iovs_len: u32,
) -> Result<Vec<(u32, u32)>, i32> {
    if iovs_len > MAX_IOVECS {
        return Err(wasi::ERRNO_INVAL);
    }
    let mut out = Vec::with_capacity(iovs_len as usize);
    for i in 0..iovs_len {
        let off = iovs_ptr.wrapping_add(i.wrapping_mul(8));
        let b = read_bytes(caller, off, 8)?;
        out.push(wasi::iovec_from_le_bytes(b.try_into().unwrap()));
    }
    Ok(out)
}

fn fd_entry(caller: &Caller<'_, WasiCtx>, fd: u32) -> Result<WasiFdEntry, i32> {
    caller.data().fds.get(fd).cloned().ok_or(wasi::ERRNO_BADF)
}

fn fd_host(caller: &Caller<'_, WasiCtx>, fd: u32) -> Result<i32, i32> {
    let e = fd_entry(caller, fd)?;
    if e.host_fd < 0 {
        return Err(wasi::ERRNO_BADF);
    }
    Ok(e.host_fd)
}

fn alloc_fd(caller: &mut Caller<'_, WasiCtx>, entry: WasiFdEntry) -> Result<u32, i32> {
    caller.data_mut().fds.alloc(entry)
}

fn resolve(caller: &Caller<'_, WasiCtx>, dirfd: u32, rel: &[u8]) -> Result<Vec<u8>, i32> {
    caller.data().resolve_path(dirfd, rel)
}

fn read_path(caller: &Caller<'_, WasiCtx>, ptr: u32, len: u32) -> Result<Vec<u8>, i32> {
    if len > 4096 {
        return Err(wasi::ERRNO_NAMETOOLONG);
    }
    read_bytes(caller, ptr, len)
}

/// 把内核的 `-errno` 返回值转成 WASI errno.
///
/// 内核契约只返回小整数 errno; 越界说明返回契约被破坏, 按 `ERRNO_IO` 处理.
fn wasi_errno(ret: i64) -> i32 {
    let code = i32::try_from(ret.saturating_neg()).unwrap_or(i32::MAX);
    wasi::errno_from_linux(code)
}

/// 把已知有界的长度转成 WASI 的 `u32` 字段; 溢出按 `ERRNO_OVERFLOW` 返回.
fn u32_of(v: usize) -> Result<u32, i32> {
    u32::try_from(v).map_err(|_| wasi::ERRNO_OVERFLOW)
}

fn ret_errno(ret: i64) -> i32 {
    if ret < 0 {
        wasi_errno(ret)
    } else {
        wasi::ERRNO_SUCCESS
    }
}

fn ret_errno_i32(ret: i32) -> i32 {
    if ret < 0 {
        wasi::errno_from_linux(-ret)
    } else {
        wasi::ERRNO_SUCCESS
    }
}

fn stat_buf_len() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        144
    }
    #[cfg(target_arch = "aarch64")]
    {
        128
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        144
    }
}

fn filestat_for_fd(caller: &Caller<'_, WasiCtx>, fd: u32) -> Result<[u8; 64], i32> {
    let e = fd_entry(caller, fd)?;
    let mut st = vec![0u8; stat_buf_len()];
    let mut ret = if e.host_fd >= 0 {
        sys::fstat_fd(e.host_fd, &mut st)
    } else {
        -1
    };
    // 内核 fstat 对目录支持不完整; fd 带缓存路径时退回路径 stat,
    // 否则 opendir() 的 fd_filestat_get 会直接 EIO.
    if ret < 0
        && let Some(path) = e.path.as_deref()
    {
        ret = sys::stat_path(path, &mut st);
    }
    if ret < 0 {
        if e.path.is_none() && e.host_fd < 0 {
            return Err(wasi::ERRNO_BADF);
        }
        return Err(wasi_errno(ret));
    }
    wasi::filestat_from_linux(&st)
}

fn filestat_for_path(
    caller: &Caller<'_, WasiCtx>,
    dirfd: u32,
    rel: &[u8],
    follow: bool,
) -> Result<[u8; 64], i32> {
    let path = resolve(caller, dirfd, rel)?;
    let mut st = vec![0u8; stat_buf_len()];
    let ret = if follow {
        sys::stat_path(&path, &mut st)
    } else {
        sys::lstat_path(&path, &mut st)
    };
    if ret < 0 {
        return Err(wasi_errno(ret));
    }
    wasi::filestat_from_linux(&st)
}

fn filetype_from_vfs(v: u8) -> u8 {
    match v {
        0 => wasi::FILETYPE_REGULAR_FILE,
        1 => wasi::FILETYPE_DIRECTORY,
        2 => wasi::FILETYPE_CHARACTER_DEVICE,
        3 => wasi::FILETYPE_SYMBOLIC_LINK,
        _ => wasi::FILETYPE_UNKNOWN,
    }
}

// ============================================================================
// G1 进程控制
// ============================================================================
fn host_proc_exit(_caller: &mut Caller<'_, WasiCtx>, code: i32) {
    sys::proc_exit(code);
}

fn host_sched_yield(_caller: &mut Caller<'_, WasiCtx>) -> i32 {
    sys::sched_yield();
    wasi::ERRNO_SUCCESS
}

// ============================================================================
// G2 时钟/随机
// ============================================================================
fn host_clock_time_get(
    caller: &mut Caller<'_, WasiCtx>,
    clock_id: i32,
    _precision: i64,
    time_ptr: u32,
) -> i32 {
    let kid = match clock_id {
        0 => sys::CLOCK_REALTIME,
        1..=3 => sys::CLOCK_MONOTONIC,
        _ => return wasi::ERRNO_INVAL,
    };
    let mut ts = sys::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if sys::clock_gettime(kid, &mut ts) < 0 {
        return wasi::ERRNO_IO;
    }
    let ns = ts
        .tv_sec
        .cast_unsigned()
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec.cast_unsigned());
    match write_u64(caller, time_ptr, ns) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_random_get(caller: &mut Caller<'_, WasiCtx>, buf_ptr: u32, buf_len: u32) -> i32 {
    if buf_len > MAX_IO {
        return wasi::ERRNO_FAULT;
    }
    let mut buf = vec![0u8; buf_len as usize];
    let ret = sys::getrandom(&mut buf);
    if ret < 0 {
        return ret_errno_i32(ret);
    }
    match write_bytes(caller, buf_ptr, &buf[..ret.cast_unsigned() as usize]) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

// ============================================================================
// G3 参数/环境
// ============================================================================
fn host_args_sizes_get(caller: &mut Caller<'_, WasiCtx>, argc_ptr: u32, size_ptr: u32) -> i32 {
    let Ok(argc) = u32_of(caller.data().args.len()) else {
        return wasi::ERRNO_OVERFLOW;
    };
    let Ok(size) = u32_of(caller.data().args.iter().map(|s| s.len() + 1).sum()) else {
        return wasi::ERRNO_OVERFLOW;
    };
    if let Err(e) = write_u32(caller, argc_ptr, argc) {
        return e;
    }
    match write_u32(caller, size_ptr, size) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_args_get(caller: &mut Caller<'_, WasiCtx>, argv_ptr: u32, buf_ptr: u32) -> i32 {
    let args: Vec<Vec<u8>> = caller.data().args.clone();
    let mut off = 0u32;
    for (i, arg) in args.iter().enumerate() {
        let Ok(index) = u32_of(i) else {
            return wasi::ERRNO_OVERFLOW;
        };
        let Ok(arg_len) = u32_of(arg.len()) else {
            return wasi::ERRNO_OVERFLOW;
        };
        if let Err(e) = write_u32(
            caller,
            argv_ptr.wrapping_add(index.wrapping_mul(4)),
            buf_ptr.wrapping_add(off),
        ) {
            return e;
        }
        if let Err(e) = write_bytes(caller, buf_ptr.wrapping_add(off), arg) {
            return e;
        }
        off = off.wrapping_add(arg_len.wrapping_add(1));
        if let Err(e) = write_bytes(caller, buf_ptr.wrapping_add(off - 1), &[0]) {
            return e;
        }
    }
    wasi::ERRNO_SUCCESS
}

fn host_environ_sizes_get(caller: &mut Caller<'_, WasiCtx>, count_ptr: u32, size_ptr: u32) -> i32 {
    let Ok(count) = u32_of(caller.data().env.len()) else {
        return wasi::ERRNO_OVERFLOW;
    };
    let Ok(size) = u32_of(caller.data().env.iter().map(|s| s.len() + 1).sum()) else {
        return wasi::ERRNO_OVERFLOW;
    };
    if let Err(e) = write_u32(caller, count_ptr, count) {
        return e;
    }
    match write_u32(caller, size_ptr, size) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_environ_get(caller: &mut Caller<'_, WasiCtx>, env_ptr: u32, buf_ptr: u32) -> i32 {
    let env: Vec<Vec<u8>> = caller.data().env.clone();
    let mut off = 0u32;
    for (i, item) in env.iter().enumerate() {
        let Ok(index) = u32_of(i) else {
            return wasi::ERRNO_OVERFLOW;
        };
        let Ok(item_len) = u32_of(item.len()) else {
            return wasi::ERRNO_OVERFLOW;
        };
        if let Err(e) = write_u32(
            caller,
            env_ptr.wrapping_add(index.wrapping_mul(4)),
            buf_ptr.wrapping_add(off),
        ) {
            return e;
        }
        if let Err(e) = write_bytes(caller, buf_ptr.wrapping_add(off), item) {
            return e;
        }
        off = off.wrapping_add(item_len.wrapping_add(1));
        if let Err(e) = write_bytes(caller, buf_ptr.wrapping_add(off - 1), &[0]) {
            return e;
        }
    }
    wasi::ERRNO_SUCCESS
}

// ============================================================================
// G4 fd 管理
// ============================================================================
fn host_fd_close(caller: &mut Caller<'_, WasiCtx>, fd: u32) -> i32 {
    match caller.data_mut().fds.close(fd) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_seek(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    offset: i64,
    whence: i32,
    out_ptr: u32,
) -> i32 {
    let host = match fd_host(caller, fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    if !(0..=2).contains(&whence) {
        return wasi::ERRNO_INVAL;
    }
    let ret = sys::lseek(host, offset, whence);
    if ret < 0 {
        return ret_errno(ret);
    }
    match write_u64(caller, out_ptr, ret.cast_unsigned()) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_tell(caller: &mut Caller<'_, WasiCtx>, fd: u32, out_ptr: u32) -> i32 {
    let host = match fd_host(caller, fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let ret = sys::lseek(host, 0, 1);
    if ret < 0 {
        return ret_errno(ret);
    }
    match write_u64(caller, out_ptr, ret.cast_unsigned()) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_sync(caller: &mut Caller<'_, WasiCtx>, fd: u32) -> i32 {
    if let Err(e) = fd_host(caller, fd) {
        return e;
    }
    sys::fs_sync();
    wasi::ERRNO_SUCCESS
}

fn host_fd_prestat_get(caller: &mut Caller<'_, WasiCtx>, fd: u32, out_ptr: u32) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    let Some(path) = e.path.as_deref() else {
        return wasi::ERRNO_BADF;
    };
    if e.file_type != wasi::FILETYPE_DIRECTORY {
        return wasi::ERRNO_BADF;
    }
    let Ok(name_len) = u32_of(path.len()) else {
        return wasi::ERRNO_NAMETOOLONG;
    };
    match write_bytes(caller, out_ptr, &wasi::encode_prestat(name_len)) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_prestat_dir_name(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    path_ptr: u32,
    path_len: u32,
) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    let Some(path) = e.path.as_deref() else {
        return wasi::ERRNO_BADF;
    };
    if e.file_type != wasi::FILETYPE_DIRECTORY {
        return wasi::ERRNO_BADF;
    }
    let Ok(name_len) = u32_of(path.len()) else {
        return wasi::ERRNO_NAMETOOLONG;
    };
    if path_len < name_len {
        return wasi::ERRNO_NAMETOOLONG;
    }
    match write_bytes(caller, path_ptr, path) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_fdstat_get(caller: &mut Caller<'_, WasiCtx>, fd: u32, out_ptr: u32) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    let bytes = wasi::encode_fdstat(e.file_type, 0, e.rights_base, e.rights_inheriting);
    match write_bytes(caller, out_ptr, &bytes) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_filestat_get(caller: &mut Caller<'_, WasiCtx>, fd: u32, out_ptr: u32) -> i32 {
    let f = match filestat_for_fd(caller, fd) {
        Ok(f) => f,
        Err(e) => return e,
    };
    match write_bytes(caller, out_ptr, &f) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

// ============================================================================
// G5 fd I/O
// ============================================================================
fn host_fd_read(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    nread_ptr: u32,
) -> i32 {
    let host = match fd_host(caller, fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let iovs = match read_iovecs(caller, iovs_ptr, iovs_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut total = 0u32;
    for (ptr, len) in iovs {
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u8; len as usize];
        let n = sys::read(host, &mut buf);
        if n < 0 {
            return ret_errno_i32(n);
        }
        let n = n.cast_unsigned();
        if let Err(e) = write_bytes(caller, ptr, &buf[..n as usize]) {
            return e;
        }
        total = total.saturating_add(n);
        if n < len {
            break;
        }
    }
    match write_u32(caller, nread_ptr, total) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_write(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    nwritten_ptr: u32,
) -> i32 {
    let host = match fd_host(caller, fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let iovs = match read_iovecs(caller, iovs_ptr, iovs_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut total = 0u32;
    for (ptr, len) in iovs {
        if len == 0 {
            continue;
        }
        let data = match read_bytes(caller, ptr, len) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let n = sys::write(host, &data);
        if n < 0 {
            return ret_errno_i32(n);
        }
        let n = n.cast_unsigned() as u32;
        total = total.saturating_add(n);
        if n < len {
            break;
        }
    }
    match write_u32(caller, nwritten_ptr, total) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_pread(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    offset: i64,
    nread_ptr: u32,
) -> i32 {
    let host = match fd_host(caller, fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let iovs = match read_iovecs(caller, iovs_ptr, iovs_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut total = 0u32;
    let mut pos = offset;
    for (ptr, len) in iovs {
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u8; len as usize];
        let n = sys::pread(host, &mut buf, pos);
        if n < 0 {
            return ret_errno_i32(n);
        }
        let n = n.cast_unsigned();
        if let Err(e) = write_bytes(caller, ptr, &buf[..n as usize]) {
            return e;
        }
        total = total.saturating_add(n);
        pos = pos.wrapping_add(i64::from(n));
        if n < len {
            break;
        }
    }
    match write_u32(caller, nread_ptr, total) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn pwrite_no_seek(host: i32, data: &[u8], offset: i64) -> i64 {
    let cur = sys::lseek(host, 0, 1);
    if cur < 0 {
        return cur;
    }
    let ret = sys::lseek(host, offset, 0);
    if ret < 0 {
        return ret;
    }
    let n = sys::write(host, data);
    let _ = sys::lseek(host, cur, 0);
    i64::from(n)
}

fn host_fd_pwrite(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    offset: i64,
    nwritten_ptr: u32,
) -> i32 {
    let host = match fd_host(caller, fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let iovs = match read_iovecs(caller, iovs_ptr, iovs_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut total = 0u32;
    let mut pos = offset;
    for (ptr, len) in iovs {
        if len == 0 {
            continue;
        }
        let data = match read_bytes(caller, ptr, len) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let n = pwrite_no_seek(host, &data, pos);
        if n < 0 {
            return ret_errno(n);
        }
        let Ok(n) = u32::try_from(n) else {
            return wasi::ERRNO_OVERFLOW;
        };
        total = total.saturating_add(n);
        pos = pos.wrapping_add(i64::from(n));
        if n < len {
            break;
        }
    }
    match write_u32(caller, nwritten_ptr, total) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_allocate(caller: &mut Caller<'_, WasiCtx>, fd: u32, offset: i64, len: i64) -> i32 {
    if offset < 0 || len < 0 {
        return wasi::ERRNO_INVAL;
    }
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    if e.host_fd < 0 {
        return wasi::ERRNO_NOTCAPABLE;
    }
    let mut st = vec![0u8; stat_buf_len()];
    if sys::fstat_fd(e.host_fd, &mut st) < 0 {
        return wasi::ERRNO_IO;
    }
    // x86_64 / aarch64 glibc stat 的 st_size 都在偏移 48.
    let size = u64::from_le_bytes(st[48..56].try_into().unwrap());
    let end = offset.cast_unsigned().saturating_add(len.cast_unsigned());
    if end <= size {
        return wasi::ERRNO_SUCCESS;
    }
    let ret = sys::ftruncate(e.host_fd, end.cast_signed());
    ret_errno_i32(ret)
}

fn host_fd_advise(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    offset: i64,
    len: i64,
    advice: i32,
) -> i32 {
    if offset < 0 || len < 0 || !(0..=5).contains(&advice) {
        return wasi::ERRNO_INVAL;
    }
    match fd_entry(caller, fd) {
        Ok(_) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

// ============================================================================
// G7 高级 fd
// ============================================================================
fn host_fd_renumber(caller: &mut Caller<'_, WasiCtx>, from: u32, to: u32) -> i32 {
    match caller.data_mut().fds.renumber(from, to) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_fd_dup(caller: &mut Caller<'_, WasiCtx>, fd: u32) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    if e.socket || e.host_fd < 0 {
        return wasi::ERRNO_NOTSUP;
    }
    let new_host = sys::dup_fd(e.host_fd);
    if new_host < 0 {
        return wasi::errno_from_linux(-new_host);
    }
    let mut new_entry = e.clone();
    new_entry.host_fd = new_host;
    match alloc_fd(caller, new_entry) {
        Ok(new_fd) => i32::try_from(new_fd).unwrap_or(wasi::ERRNO_BADF),
        Err(errno) => errno,
    }
}

fn host_fd_readdir(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    buf_ptr: u32,
    buf_len: u32,
    cookie: u64,
    bufused_ptr: u32,
) -> i32 {
    let host = match fd_host(caller, fd) {
        Ok(h) => h,
        Err(e) => return e,
    };
    if buf_len < 24 {
        return match write_u32(caller, bufused_ptr, 0) {
            Ok(()) => wasi::ERRNO_SUCCESS,
            Err(e) => e,
        };
    }
    // 一次调用必须尽量填满调用方缓冲区: wasi-libc 的 `readdir()` 按 `bufused`
    // 判断「本批还有没有条目」, 旧实现每次只回 1 条, DIR* 循环读到第一条就
    // 认为结束 (P3 实测 n=1, 真实目录有 5 条).
    let mut entry = sys::UserDirEntry {
        node: 0,
        file_type: 0,
        name: [0u8; 256],
    };
    // cookie = 已消费条目数; 回到 0 (rewinddir) 时把目录游标复位.
    if cookie == 0 {
        let _ = sys::lseek(host, 0, 0);
    }
    if buf_len < 24 {
        // 连一个 dirent 头都放不下: 按「本次 0 条」返回, 由调用方换更大的缓冲区.
        return match write_u32(caller, bufused_ptr, 0) {
            Ok(()) => wasi::ERRNO_SUCCESS,
            Err(e) => e,
        };
    }
    let mut out: Vec<u8> = Vec::new();
    let mut index = cookie;
    loop {
        let ret = sys::fs_readdir(host, &mut entry);
        if ret < 0 {
            return wasi::ERRNO_IO;
        }
        if ret == 0 {
            break;
        }
        let name_len = entry
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(entry.name.len());
        let need = 24 + name_len;
        if out.len() + need > buf_len as usize {
            // 放不下: 已经攒了条目就先交出去 (调用方会用 d_next 续读); 一条都
            // 没攒下则按 WASI 允许的「名字截断」写法输出, 由调用方决定是否换个
            // 更大的缓冲区重读.
            if !out.is_empty() {
                break;
            }
            let fit = buf_len as usize - 24;
            let Ok(fit_u32) = u32_of(fit) else {
                return wasi::ERRNO_OVERFLOW;
            };
            let bytes = wasi::encode_dirent(
                index.wrapping_add(1),
                u64::from(entry.node),
                fit_u32,
                filetype_from_vfs(entry.file_type),
                &entry.name[..fit],
            );
            out.extend_from_slice(&bytes);
            break;
        }
        let Ok(name_u32) = u32_of(name_len) else {
            return wasi::ERRNO_OVERFLOW;
        };
        let bytes = wasi::encode_dirent(
            index.wrapping_add(1),
            u64::from(entry.node),
            name_u32,
            filetype_from_vfs(entry.file_type),
            &entry.name[..name_len],
        );
        out.extend_from_slice(&bytes);
        index = index.wrapping_add(1);
    }
    if let Err(e) = write_bytes(caller, buf_ptr, &out) {
        return e;
    }
    let Ok(used) = u32_of(out.len()) else {
        return wasi::ERRNO_OVERFLOW;
    };
    match write_u32(caller, bufused_ptr, used) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

// ============================================================================
// G6 路径
// ============================================================================
/// `path_open` 的 WASI 参数组 (ABI 签名 8 参, pedantic 上限 7).
#[derive(Clone, Copy)]
struct PathOpenArgs {
    dirfd: u32,
    path_ptr: u32,
    path_len: u32,
    oflags: u32,
    rights_base: i64,
    rights_inheriting: i64,
    fdflags: u32,
    opened_fd_ptr: u32,
}

fn host_path_open(caller: &mut Caller<'_, WasiCtx>, args: &PathOpenArgs) -> i32 {
    let PathOpenArgs {
        dirfd,
        path_ptr,
        path_len,
        oflags,
        rights_base,
        rights_inheriting,
        fdflags,
        opened_fd_ptr,
    } = *args;
    // WASI rights 是 64 位无符号位段, 经 wasm i64 传进来; 在边界处一次性还原.
    let rights_base = rights_base.cast_unsigned();
    let rights_inheriting = rights_inheriting.cast_unsigned();
    let rel = match read_path(caller, path_ptr, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = match resolve(caller, dirfd, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mut flags = 0i32;
    if oflags & u32::from(wasi::OFLAG_CREAT) != 0 {
        flags |= 0o100;
    }
    if oflags & u32::from(wasi::OFLAG_DIRECTORY) != 0 {
        flags |= 0o200_000;
    }
    if oflags & u32::from(wasi::OFLAG_EXCL) != 0 {
        flags |= 0o200;
    }
    if oflags & u32::from(wasi::OFLAG_TRUNC) != 0 {
        flags |= 0o1000;
    }
    if fdflags & u32::from(wasi::FDFLAG_APPEND) != 0 {
        flags |= 0o2000;
    }
    if fdflags & u32::from(wasi::FDFLAG_NONBLOCK) != 0 {
        flags |= 0o4000;
    }
    let read = rights_base & (wasi::RIGHTS_FD_READ | wasi::RIGHTS_FD_READDIR) != 0;
    let write = rights_base & (wasi::RIGHTS_FD_WRITE | wasi::RIGHTS_FD_ALLOCATE) != 0
        || oflags & u32::from(wasi::OFLAG_CREAT | wasi::OFLAG_TRUNC) != 0;
    flags |= match (read, write) {
        (true, true) => 2,
        (false, true) => 1,
        _ => 0,
    };
    // 内核 fs_create 当前不接 mode, 新建文件会落成 0100000 (无权限位),
    // 后续 path_unlink 会被 ramfs 的写权限检查拒绝. WASI 无 mode 参数,
    // 这里在 CREAT 后按常规 0644 回填 (既有文件也归一到同一权限).
    let host_fd = sys::open(&path, flags, 0o644);
    if host_fd < 0 {
        return wasi::errno_from_linux(-host_fd);
    }
    if oflags & u32::from(wasi::OFLAG_CREAT) != 0 {
        let _ = sys::chmod(&path, 0o644);
    }
    let ft = {
        let mut st = vec![0u8; stat_buf_len()];
        if sys::stat_path(&path, &mut st) >= 0 {
            let ft = wasi::filestat_from_linux(&st).map_or(wasi::FILETYPE_UNKNOWN, |f| f[16]);
            // 普通文件也可能携带 FD_READDIR 权利位 (wasi-libc 常见),
            // 只有显式 O_DIRECTORY 才强制要求目录类型.
            if oflags & u32::from(wasi::OFLAG_DIRECTORY) != 0 && ft != wasi::FILETYPE_DIRECTORY {
                let _ = sys::close(host_fd);
                return wasi::ERRNO_NOTDIR;
            }
            ft
        } else {
            // 部分目录路径的 stat 在内核侧仍失败; 先退回 fd stat, 再失败时
            // 按目录接受 — 能走到这里说明 open 已经成功, 普通文件的路径
            // stat 不该失败.
            let mut st = vec![0u8; stat_buf_len()];
            if sys::fstat_fd(host_fd, &mut st) >= 0 {
                wasi::filestat_from_linux(&st).map_or(wasi::FILETYPE_DIRECTORY, |f| f[16])
            } else {
                wasi::FILETYPE_DIRECTORY
            }
        }
    };
    let mut entry = WasiFdEntry::file(host_fd, path[..path.len().saturating_sub(1)].to_vec(), ft);
    if rights_base != 0 {
        entry.rights_base = rights_base;
        entry.rights_inheriting = rights_inheriting;
    }
    if ft == wasi::FILETYPE_DIRECTORY {
        // wasi-libc 的 fdopendir() 会检查 FD_READDIR 权利; open() 路径即使
        // 没显式传该位, 目录 fd 也应具备枚举能力.
        entry.rights_base |= wasi::RIGHTS_FD_READDIR;
    }
    match alloc_fd(caller, entry) {
        Ok(new_fd) => match write_u32(caller, opened_fd_ptr, new_fd) {
            Ok(()) => wasi::ERRNO_SUCCESS,
            Err(e) => e,
        },
        Err(e) => {
            let _ = sys::close(host_fd);
            e
        }
    }
}

fn host_path_create_directory(
    caller: &mut Caller<'_, WasiCtx>,
    dirfd: u32,
    path_ptr: u32,
    path_len: u32,
) -> i32 {
    let rel = match read_path(caller, path_ptr, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = match resolve(caller, dirfd, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    ret_errno_i32(sys::fs_mkdir(&path))
}

fn host_path_remove_directory(
    caller: &mut Caller<'_, WasiCtx>,
    dirfd: u32,
    path_ptr: u32,
    path_len: u32,
) -> i32 {
    let rel = match read_path(caller, path_ptr, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = match resolve(caller, dirfd, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    ret_errno_i32(sys::fs_rmdir(&path))
}

fn host_path_unlink_file(
    caller: &mut Caller<'_, WasiCtx>,
    dirfd: u32,
    path_ptr: u32,
    path_len: u32,
) -> i32 {
    let rel = match read_path(caller, path_ptr, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = match resolve(caller, dirfd, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    ret_errno_i32(sys::fs_unlink(&path))
}

fn host_path_symlink(
    caller: &mut Caller<'_, WasiCtx>,
    old_ptr: u32,
    old_len: u32,
    dirfd: u32,
    new_ptr: u32,
    new_len: u32,
) -> i32 {
    let target = match read_path(caller, old_ptr, old_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let rel = match read_path(caller, new_ptr, new_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = match resolve(caller, dirfd, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mut target_nul = target;
    target_nul.push(0);
    ret_errno_i32(sys::symlink(&target_nul, &path))
}

fn host_path_readlink(
    caller: &mut Caller<'_, WasiCtx>,
    dirfd: u32,
    path_ptr: u32,
    path_len: u32,
    buf_ptr: u32,
    buf_len: u32,
    bufused_ptr: u32,
) -> i32 {
    let rel = match read_path(caller, path_ptr, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = match resolve(caller, dirfd, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if buf_len == 0 {
        return wasi::ERRNO_INVAL;
    }
    let mut buf = vec![0u8; buf_len as usize];
    let n = sys::readlink(&path, &mut buf);
    if n < 0 {
        return ret_errno_i32(n);
    }
    let n = n.cast_unsigned().min(u32_of(buf.len()).unwrap_or(u32::MAX));
    if let Err(e) = write_bytes(caller, buf_ptr, &buf[..n as usize]) {
        return e;
    }
    match write_u32(caller, bufused_ptr, n) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_path_rename(
    caller: &mut Caller<'_, WasiCtx>,
    old_dirfd: u32,
    old_ptr: u32,
    old_len: u32,
    new_dirfd: u32,
    new_ptr: u32,
    new_len: u32,
) -> i32 {
    let old_rel = match read_path(caller, old_ptr, old_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let new_rel = match read_path(caller, new_ptr, new_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let old = match resolve(caller, old_dirfd, &old_rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let new = match resolve(caller, new_dirfd, &new_rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    ret_errno_i32(sys::fs_rename(&old, &new))
}

fn host_path_filestat_get(
    caller: &mut Caller<'_, WasiCtx>,
    dirfd: u32,
    flags: u32,
    path_ptr: u32,
    path_len: u32,
    out_ptr: u32,
) -> i32 {
    let rel = match read_path(caller, path_ptr, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let follow = flags & wasi::LOOKUP_SYMLINK_FOLLOW != 0;
    let f = match filestat_for_path(caller, dirfd, &rel, follow) {
        Ok(f) => f,
        Err(e) => return e,
    };
    match write_bytes(caller, out_ptr, &f) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn decode_times(atim: u64, mtim: u64, fst_flags: u32) -> Result<[i64; 4], i32> {
    let mut now = sys::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if fst_flags & (wasi::FSTFLAG_ATIM_NOW | wasi::FSTFLAG_MTIM_NOW) != 0
        && sys::clock_gettime(sys::CLOCK_REALTIME, &mut now) < 0
    {
        return Err(wasi::ERRNO_IO);
    }
    let mut out = [-1i64; 4];
    if fst_flags & wasi::FSTFLAG_ATIM != 0 {
        out[0] = (atim / 1_000_000_000).cast_signed();
        out[1] = (atim % 1_000_000_000).cast_signed();
    } else if fst_flags & wasi::FSTFLAG_ATIM_NOW != 0 {
        out[0] = now.tv_sec;
        out[1] = now.tv_nsec;
    }
    if fst_flags & wasi::FSTFLAG_MTIM != 0 {
        out[2] = (mtim / 1_000_000_000).cast_signed();
        out[3] = (mtim % 1_000_000_000).cast_signed();
    } else if fst_flags & wasi::FSTFLAG_MTIM_NOW != 0 {
        out[2] = now.tv_sec;
        out[3] = now.tv_nsec;
    }
    Ok(out)
}

/// `path_filestat_set_times` 的 WASI 参数组 (ABI 签名 7 参).
#[derive(Clone, Copy)]
struct PathTimesArgs {
    dirfd: u32,
    path_ptr: u32,
    path_len: u32,
    atim: u64,
    mtim: u64,
    fst_flags: u32,
}

fn host_path_filestat_set_times(caller: &mut Caller<'_, WasiCtx>, args: &PathTimesArgs) -> i32 {
    let PathTimesArgs {
        dirfd,
        path_ptr,
        path_len,
        atim,
        mtim,
        fst_flags,
    } = *args;
    let rel = match read_path(caller, path_ptr, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let path = match resolve(caller, dirfd, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let times = match decode_times(atim, mtim, fst_flags) {
        Ok(t) => t,
        Err(e) => return e,
    };
    ret_errno_i32(sys::utimensat(&path, &times))
}

/// `path_link` 的 WASI 参数组 (ABI 签名 7 参, `old_flags` 保留位).
#[derive(Clone, Copy)]
struct PathLinkArgs {
    old_dirfd: u32,
    old_ptr: u32,
    old_len: u32,
    new_dirfd: u32,
    new_ptr: u32,
    new_len: u32,
}

fn host_path_link(caller: &mut Caller<'_, WasiCtx>, args: &PathLinkArgs) -> i32 {
    let PathLinkArgs {
        old_dirfd,
        old_ptr,
        old_len,
        new_dirfd,
        new_ptr,
        new_len,
    } = *args;
    let old_rel = match read_path(caller, old_ptr, old_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let new_rel = match read_path(caller, new_ptr, new_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let old = match resolve(caller, old_dirfd, &old_rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let new = match resolve(caller, new_dirfd, &new_rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    ret_errno_i32(sys::link(&old, &new))
}

// ============================================================================
// G8 socket
// ============================================================================
fn host_sock_accept(caller: &mut Caller<'_, WasiCtx>, fd: u32, _flags: u32, out_ptr: u32) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    if e.host_fd < 0 || !e.socket {
        return wasi::ERRNO_NOTSOCK;
    }
    let new_host = sys::accept(e.host_fd, core::ptr::null_mut(), core::ptr::null_mut());
    if new_host < 0 {
        return ret_errno_i32(new_host);
    }
    let entry = WasiFdEntry {
        host_fd: new_host,
        path: None,
        file_type: wasi::FILETYPE_SOCKET_STREAM,
        rights_base: wasi::RIGHTS_ALL,
        rights_inheriting: wasi::RIGHTS_ALL,
        socket: true,
    };
    match alloc_fd(caller, entry) {
        Ok(new_fd) => match write_u32(caller, out_ptr, new_fd) {
            Ok(()) => wasi::ERRNO_SUCCESS,
            Err(err) => err,
        },
        Err(err) => {
            let _ = sys::close(new_host);
            err
        }
    }
}

fn host_sock_connect(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    addr_ptr: u32,
    addr_len: u32,
) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    if e.host_fd < 0 || !e.socket {
        return wasi::ERRNO_NOTSOCK;
    }
    if addr_len < 8 {
        return wasi::ERRNO_INVAL;
    }
    let raw = match read_bytes(caller, addr_ptr, 8) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let mut sa = [0u8; 16];
    sa[0..2].copy_from_slice(&2u16.to_le_bytes());
    sa[2..4].copy_from_slice(&raw[2..4]);
    sa[4..8].copy_from_slice(&raw[4..8]);
    ret_errno_i32(sys::connect(e.host_fd, sa.as_ptr(), 16))
}

fn host_sock_recv(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    flags: u32,
    nread_ptr: u32,
    roflags_ptr: u32,
) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    if e.host_fd < 0 || !e.socket {
        return wasi::ERRNO_NOTSOCK;
    }
    let iovs = match read_iovecs(caller, iovs_ptr, iovs_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(flags) = i32::try_from(flags) else {
        return wasi::ERRNO_INVAL;
    };
    let mut total = 0u32;
    for (ptr, len) in iovs {
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u8; len as usize];
        let n = sys::recv(e.host_fd, buf.as_mut_ptr(), buf.len(), flags);
        if n < 0 {
            return ret_errno(n as i64);
        }
        let n = n.cast_unsigned();
        let Ok(copied) = u32_of(n) else {
            return wasi::ERRNO_OVERFLOW;
        };
        if let Err(err) = write_bytes(caller, ptr, &buf[..n]) {
            return err;
        }
        total = total.saturating_add(copied);
        if copied < len {
            break;
        }
    }
    if let Err(e) = write_u32(caller, nread_ptr, total) {
        return e;
    }
    match write_u32(caller, roflags_ptr, 0) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

fn host_sock_send(
    caller: &mut Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    flags: u32,
    nwritten_ptr: u32,
) -> i32 {
    let e = match fd_entry(caller, fd) {
        Ok(e) => e,
        Err(e) => return e,
    };
    if e.host_fd < 0 || !e.socket {
        return wasi::ERRNO_NOTSOCK;
    }
    let iovs = match read_iovecs(caller, iovs_ptr, iovs_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(flags) = i32::try_from(flags) else {
        return wasi::ERRNO_INVAL;
    };
    let mut data = Vec::new();
    for (ptr, len) in iovs {
        if len == 0 {
            continue;
        }
        match read_bytes(caller, ptr, len) {
            Ok(part) => data.extend_from_slice(&part),
            Err(e) => return e,
        }
    }
    let n = sys::send(e.host_fd, data.as_ptr(), data.len(), flags);
    if n < 0 {
        return ret_errno(n as i64);
    }
    let Ok(nwritten) = u32_of(n.cast_unsigned()) else {
        return wasi::ERRNO_OVERFLOW;
    };
    match write_u32(caller, nwritten_ptr, nwritten) {
        Ok(()) => wasi::ERRNO_SUCCESS,
        Err(e) => e,
    }
}

/// 给 root preopen 建宿主目录 fd, 让 `fd_readdir(3)` 可枚举.
pub fn init_root(ctx: &mut WasiCtx) {
    let Some(root) = ctx.fds.get_mut(3) else {
        return;
    };
    if root.host_fd >= 0 {
        return;
    }
    let fd = sys::open(b"/\0", 0o200_000, 0);
    if fd >= 0 {
        root.host_fd = fd;
    }
}

/// 把 `&mut Caller` 形态的 host 函数登记为 `wasi_snapshot_preview1` 导入.
///
/// wasmi 的 `IntoFunc` 要求按值拿 `Caller`; 把「取可变借用」这一步集中在本宏,
/// host 函数本体只持 `&mut Caller`, ABI 适配只存在于注册边界.
macro_rules! wasi_host {
    ($linker:expr, $name:literal, $func:path $(, $arg:ident : $ty:ty)* $(,)?) => {
        $linker.func_wrap(
            "wasi_snapshot_preview1",
            $name,
            |mut caller: wasmi::Caller<'_, WasiCtx> $(, $arg: $ty)*| {
                $func(&mut caller $(, $arg)*)
            },
        )
        .map(|_| ())
    };
}

/// G1-G3: 进程控制、时钟/随机、参数/环境.
fn register_proc_env(
    linker: &mut wasmi::Linker<WasiCtx>,
) -> Result<(), wasmi::errors::LinkerError> {
    wasi_host!(linker, "proc_exit", host_proc_exit, code: i32)?;
    wasi_host!(linker, "sched_yield", host_sched_yield)?;
    wasi_host!(
        linker,
        "clock_time_get",
        host_clock_time_get,
        clock_id: i32,
        _precision: i64,
        time_ptr: u32
    )?;
    wasi_host!(linker, "random_get", host_random_get, buf_ptr: u32, buf_len: u32)?;
    wasi_host!(
        linker,
        "args_sizes_get",
        host_args_sizes_get,
        argc_ptr: u32,
        size_ptr: u32
    )?;
    wasi_host!(linker, "args_get", host_args_get, argv_ptr: u32, buf_ptr: u32)?;
    wasi_host!(
        linker,
        "environ_sizes_get",
        host_environ_sizes_get,
        count_ptr: u32,
        size_ptr: u32
    )?;
    wasi_host!(linker, "environ_get", host_environ_get, env_ptr: u32, buf_ptr: u32)
}

/// G4: fd 基础管理 (关闭/定位/同步/prestat/stat).
fn register_fd_basic(
    linker: &mut wasmi::Linker<WasiCtx>,
) -> Result<(), wasmi::errors::LinkerError> {
    wasi_host!(linker, "fd_close", host_fd_close, fd: u32)?;
    wasi_host!(
        linker,
        "fd_seek",
        host_fd_seek,
        fd: u32,
        offset: i64,
        whence: i32,
        out_ptr: u32
    )?;
    wasi_host!(linker, "fd_tell", host_fd_tell, fd: u32, out_ptr: u32)?;
    wasi_host!(linker, "fd_sync", host_fd_sync, fd: u32)?;
    wasi_host!(linker, "fd_prestat_get", host_fd_prestat_get, fd: u32, out_ptr: u32)?;
    wasi_host!(
        linker,
        "fd_prestat_dir_name",
        host_fd_prestat_dir_name,
        fd: u32,
        path_ptr: u32,
        path_len: u32
    )?;
    wasi_host!(linker, "fd_stat_get", host_fd_filestat_get, fd: u32, out_ptr: u32)?;
    wasi_host!(linker, "fd_filestat_get", host_fd_filestat_get, fd: u32, out_ptr: u32)?;
    wasi_host!(linker, "fd_fdstat_get", host_fd_fdstat_get, fd: u32, out_ptr: u32)
}

/// G5-G7: fd 读写、分配、重编号、目录枚举.
fn register_fd_io(linker: &mut wasmi::Linker<WasiCtx>) -> Result<(), wasmi::errors::LinkerError> {
    wasi_host!(
        linker,
        "fd_read",
        host_fd_read,
        fd: u32,
        iovs_ptr: u32,
        iovs_len: u32,
        nread_ptr: u32
    )?;
    wasi_host!(
        linker,
        "fd_write",
        host_fd_write,
        fd: u32,
        iovs_ptr: u32,
        iovs_len: u32,
        nwritten_ptr: u32
    )?;
    wasi_host!(
        linker,
        "fd_pread",
        host_fd_pread,
        fd: u32,
        iovs_ptr: u32,
        iovs_len: u32,
        offset: i64,
        nread_ptr: u32
    )?;
    wasi_host!(
        linker,
        "fd_pwrite",
        host_fd_pwrite,
        fd: u32,
        iovs_ptr: u32,
        iovs_len: u32,
        offset: i64,
        nwritten_ptr: u32
    )?;
    wasi_host!(linker, "fd_allocate", host_fd_allocate, fd: u32, offset: i64, len: i64)?;
    wasi_host!(
        linker,
        "fd_advise",
        host_fd_advise,
        fd: u32,
        offset: i64,
        len: i64,
        advice: i32
    )?;
    wasi_host!(linker, "fd_renumber", host_fd_renumber, from: u32, to: u32)?;
    wasi_host!(linker, "fd_dup", host_fd_dup, fd: u32)?;
    wasi_host!(
        linker,
        "fd_readdir",
        host_fd_readdir,
        fd: u32,
        buf_ptr: u32,
        buf_len: u32,
        cookie: u64,
        bufused_ptr: u32
    )
}

/// G6: 路径名操作 (建/删/链接/改名).
fn register_path_names(
    linker: &mut wasmi::Linker<WasiCtx>,
) -> Result<(), wasmi::errors::LinkerError> {
    wasi_host!(
        linker,
        "path_create_directory",
        host_path_create_directory,
        dirfd: u32,
        path_ptr: u32,
        path_len: u32
    )?;
    wasi_host!(
        linker,
        "path_remove_directory",
        host_path_remove_directory,
        dirfd: u32,
        path_ptr: u32,
        path_len: u32
    )?;
    wasi_host!(
        linker,
        "path_unlink_file",
        host_path_unlink_file,
        dirfd: u32,
        path_ptr: u32,
        path_len: u32
    )?;
    wasi_host!(
        linker,
        "path_symlink",
        host_path_symlink,
        old_ptr: u32,
        old_len: u32,
        dirfd: u32,
        new_ptr: u32,
        new_len: u32
    )?;
    wasi_host!(
        linker,
        "path_readlink",
        host_path_readlink,
        dirfd: u32,
        path_ptr: u32,
        path_len: u32,
        buf_ptr: u32,
        buf_len: u32,
        bufused_ptr: u32
    )?;
    wasi_host!(
        linker,
        "path_rename",
        host_path_rename,
        old_dirfd: u32,
        old_ptr: u32,
        old_len: u32,
        new_dirfd: u32,
        new_ptr: u32,
        new_len: u32
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "path_link",
        |mut caller: wasmi::Caller<'_, WasiCtx>,
         old_dirfd: u32,
         _old_flags: u32,
         old_ptr: u32,
         old_len: u32,
         new_dirfd: u32,
         new_ptr: u32,
         new_len: u32| {
            host_path_link(
                &mut caller,
                &PathLinkArgs {
                    old_dirfd,
                    old_ptr,
                    old_len,
                    new_dirfd,
                    new_ptr,
                    new_len,
                },
            )
        },
    )?;
    Ok(())
}

/// G6: 打开与 filestat (参数超 7 个, 用参数组收口).
fn register_path_open_meta(
    linker: &mut wasmi::Linker<WasiCtx>,
) -> Result<(), wasmi::errors::LinkerError> {
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "path_open",
        |mut caller: wasmi::Caller<'_, WasiCtx>,
         dirfd: u32,
         _dirflags: u32,
         path_ptr: u32,
         path_len: u32,
         oflags: u32,
         rights_base: i64,
         rights_inheriting: i64,
         fdflags: u32,
         opened_fd_ptr: u32| {
            host_path_open(
                &mut caller,
                &PathOpenArgs {
                    dirfd,
                    path_ptr,
                    path_len,
                    oflags,
                    rights_base,
                    rights_inheriting,
                    fdflags,
                    opened_fd_ptr,
                },
            )
        },
    )?;
    wasi_host!(
        linker,
        "path_filestat_get",
        host_path_filestat_get,
        dirfd: u32,
        flags: u32,
        path_ptr: u32,
        path_len: u32,
        out_ptr: u32
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "path_filestat_set_times",
        |mut caller: wasmi::Caller<'_, WasiCtx>,
         dirfd: u32,
         _flags: u32,
         path_ptr: u32,
         path_len: u32,
         atim: u64,
         mtim: u64,
         fst_flags: u32| {
            host_path_filestat_set_times(
                &mut caller,
                &PathTimesArgs {
                    dirfd,
                    path_ptr,
                    path_len,
                    atim,
                    mtim,
                    fst_flags,
                },
            )
        },
    )?;
    Ok(())
}

/// G8: socket (preview1 无 `sock_open`, 仅保留规范内的四个入口).
fn register_socket(linker: &mut wasmi::Linker<WasiCtx>) -> Result<(), wasmi::errors::LinkerError> {
    wasi_host!(linker, "sock_accept", host_sock_accept, fd: u32, _flags: u32, out_ptr: u32)?;
    wasi_host!(linker, "sock_connect", host_sock_connect, fd: u32, addr_ptr: u32, addr_len: u32)?;
    wasi_host!(
        linker,
        "sock_recv",
        host_sock_recv,
        fd: u32,
        iovs_ptr: u32,
        iovs_len: u32,
        flags: u32,
        nread_ptr: u32,
        roflags_ptr: u32
    )?;
    wasi_host!(
        linker,
        "sock_send",
        host_sock_send,
        fd: u32,
        iovs_ptr: u32,
        iovs_len: u32,
        flags: u32,
        nwritten_ptr: u32
    )
}

/// 注册 38 个 WASI host 函数 (含 `fd_stat_get`/`fd_filestat_get` 双名).
///
/// # Errors
/// 任一函数登记失败 (重名等) 返回 `LinkerError`.
pub fn register_wasi(
    linker: &mut wasmi::Linker<WasiCtx>,
) -> Result<(), wasmi::errors::LinkerError> {
    register_proc_env(linker)?;
    register_fd_basic(linker)?;
    register_fd_io(linker)?;
    register_path_names(linker)?;
    register_path_open_meta(linker)?;
    register_socket(linker)
}
