//! wasmrun — 用户态 wasmi WASM 运行时 (P1, WASM 退用户态迁移首里程碑)
//!
//! `execve` 按魔数识别 `\0asm` 后, 经构建 feature `wasm_userspace` 选桩装载本
//! 映像 (过渡期与内核解释器桩 wasmstub 共存, P4 翻转默认). 本运行时在用户态:
//!
//! 1. 经 VFS open/read 读入 wasm 模块 (`argv[0]` 即模块路径);
//! 2. wasmi Engine/Module/Store/Linker 解释执行 (消费 P0 `global_allocator`);
//! 3. 最小 WASI host 层把 `wasi_snapshot_preview1` 翻译到 Edgine POSIX syscall;
//! 4. 以模块退出码 (正常结束) 或 1 (装载/解析/trap 失败) 调 `proc_exit`.
//!
//! 与 wasmstub 的区别: 解释器在用户态 (wasmi), 不再经 `EG_WASM_*` 回环内核.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

extern crate alloc;

mod wasi;

#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
use userlib::sys::{
    CLOCK_MONOTONIC, O_RDONLY, Timespec, clock_gettime, close, open, proc_exit, read, write,
};
#[cfg(not(test))]
use wasmi::{Caller, Extern, Linker, Memory, errors::LinkerError};

// `extern "C" fn _start` 读不到入口 SP, 故用架构汇编把 SP 作为首参转发
// (与 wasmstub / exectest 同一形态).
#[cfg(all(not(test), target_arch = "x86_64"))]
core::arch::global_asm!(
    ".global _start",
    "_start:",
    "mov rdi, rsp",
    "call wasmrun_main",
);

#[cfg(all(not(test), target_arch = "aarch64"))]
core::arch::global_asm!(".global _start", "_start:", "mov x0, sp", "bl wasmrun_main",);

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    proc_exit(1);
}

/// 用户栈上第 `idx` 个 8 字节槽.
///
/// # Safety
/// `sp` 是内核写入的进程入口栈指针, `idx` 必须落在内核布置的 argc/argv/envp 区间内.
#[cfg(not(test))]
unsafe fn word_at(sp: u64, idx: usize) -> u64 {
    // SAFETY: 由调用方保证 idx 在入口栈布局内 (仅用于 argc/argv 槽).
    unsafe { core::ptr::read_volatile((sp as *const u64).add(idx)) }
}

/// 从用户态 NUL 终止字符串指针读入路径字节 (上限 4096, 防 runaway).
///
/// # Safety
/// `ptr` 必须是内核复制进用户地址空间的有效 argv 字符串指针.
#[cfg(not(test))]
unsafe fn read_cstr(ptr: u64) -> Option<Vec<u8>> {
    if ptr == 0 {
        return None;
    }
    let p = ptr as *const u8;
    let mut v = Vec::new();
    for i in 0..4096 {
        // SAFETY: ptr 指向内核布置的 argv 字符串, 逐字节读到 NUL 或上限即止.
        let b = unsafe { core::ptr::read_volatile(p.add(i)) };
        if b == 0 {
            return Some(v);
        }
        v.push(b);
    }
    None
}

/// 经 VFS 读入整个 wasm 模块字节 (argv[0] 路径).
#[cfg(not(test))]
fn load_module(path_ptr: u64) -> Option<Vec<u8>> {
    // SAFETY: path_ptr 取自入口栈 argv[0], 是内核复制的有效用户字符串指针.
    let path = unsafe { read_cstr(path_ptr)? };
    let fd = open(&path, O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = read(fd, &mut chunk);
        if n < 0 {
            let _ = close(fd);
            return None;
        }
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n.cast_unsigned() as usize]);
    }
    let _ = close(fd);
    Some(buf)
}

/// 取实例导出的线性内存 (WASI host 读写共用).
#[cfg(not(test))]
fn guest_memory(caller: &Caller<'_, ()>) -> Option<Memory> {
    caller.get_export("memory").and_then(Extern::into_memory)
}

/// WASI `proc_exit`: 直接以模块退出码终止进程 (WASI 语义即不返回).
#[cfg(not(test))]
fn host_proc_exit(_: Caller<'_, ()>, code: i32) {
    proc_exit(code);
}

/// WASI `fd_write`: 从线性内存读 iovec 数组, 逐块搬运到宿主 fd, 回写总写入字节数.
#[cfg(not(test))]
fn host_fd_write(
    mut caller: Caller<'_, ()>,
    fd: i32,
    iovs_ptr: i32,
    iovs_len: i32,
    nwritten_ptr: i32,
) -> i32 {
    const IO_CHUNK: usize = 1024;
    const IO_CHUNK_U32: u32 = 1024;
    let Some(mem) = guest_memory(&caller) else {
        return wasi::ERRNO_FAULT;
    };
    let mut total = 0u32;
    for i in 0..iovs_len.max(0) {
        let iov_off = iovs_ptr
            .cast_unsigned()
            .wrapping_add(i.cast_unsigned().wrapping_mul(8));
        let mut iov = [0u8; 8];
        if mem.read(&caller, iov_off as usize, &mut iov).is_err() {
            return wasi::ERRNO_FAULT;
        }
        let (buf_ptr, buf_len) = wasi::iovec_from_le_bytes(iov);
        let mut done = 0u32;
        while done < buf_len {
            let want = (buf_len - done).min(IO_CHUNK_U32);
            let mut chunk = [0u8; IO_CHUNK];
            let off = buf_ptr.wrapping_add(done) as usize;
            if mem.read(&caller, off, &mut chunk[..want as usize]).is_err() {
                return wasi::ERRNO_FAULT;
            }
            let written = write(fd, &chunk[..want as usize]);
            if written < 0 {
                return wasi::ERRNO_IO;
            }
            let written = written.cast_unsigned();
            total = total.saturating_add(written);
            done = done.saturating_add(written);
            if written < want {
                break;
            }
        }
    }
    if mem
        .write(
            &mut caller,
            nwritten_ptr.cast_unsigned() as usize,
            &total.to_le_bytes(),
        )
        .is_err()
    {
        return wasi::ERRNO_FAULT;
    }
    wasi::ERRNO_SUCCESS
}

/// WASI `fd_close`: 关闭宿主 fd.
#[cfg(not(test))]
fn host_fd_close(_: Caller<'_, ()>, fd: i32) -> i32 {
    if close(fd) < 0 {
        wasi::ERRNO_IO
    } else {
        wasi::ERRNO_SUCCESS
    }
}

/// WASI `fd_fdstat_get`: 标准流报字符设备 + stdio rights; 其余 fd 本子集不托管, 报 Badf.
#[cfg(not(test))]
fn host_fd_fdstat_get(mut caller: Caller<'_, ()>, fd: i32, buf_ptr: i32) -> i32 {
    if !(0..=2).contains(&fd) {
        return wasi::ERRNO_BADF;
    }
    let Some(mem) = guest_memory(&caller) else {
        return wasi::ERRNO_FAULT;
    };
    let bytes = wasi::encode_fdstat(wasi::FILETYPE_CHARACTER_DEVICE, 0, wasi::STDIO_RIGHTS, 0);
    if mem
        .write(&mut caller, buf_ptr.cast_unsigned() as usize, &bytes)
        .is_err()
    {
        return wasi::ERRNO_FAULT;
    }
    wasi::ERRNO_SUCCESS
}

/// WASI `fd_seek`: 本子集仅托管不可 seek 的标准流, 与内核失败路径同返 Io.
#[cfg(not(test))]
fn host_fd_seek(
    _: Caller<'_, ()>,
    _fd: i32,
    _offset: i64,
    _whence: i32,
    _newoffset_ptr: i32,
) -> i32 {
    wasi::ERRNO_IO
}

/// WASI `clock_time_get`: 读时钟并回写 u64 纳秒.
#[cfg(not(test))]
fn host_clock_time_get(
    mut caller: Caller<'_, ()>,
    _clock_id: i32,
    _precision: i64,
    time_ptr: i32,
) -> i32 {
    let Some(mem) = guest_memory(&caller) else {
        return wasi::ERRNO_FAULT;
    };
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SIMPLIFIED: Edgine 仅单调时钟, WASI realtime/monotonic 统一映射
    // CLOCK_MONOTONIC; 影响面: 墙钟时间不可得; 何时需扩展: 内核补
    // CLOCK_REALTIME 后按 clock_id 分映射.
    if clock_gettime(CLOCK_MONOTONIC, &mut ts) < 0 {
        return wasi::ERRNO_IO;
    }
    let ns = ts
        .tv_sec
        .cast_unsigned()
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec.cast_unsigned());
    if mem
        .write(
            &mut caller,
            time_ptr.cast_unsigned() as usize,
            &ns.to_le_bytes(),
        )
        .is_err()
    {
        return wasi::ERRNO_FAULT;
    }
    wasi::ERRNO_SUCCESS
}

/// 注册最小 WASI host 层 (`wasi_snapshot_preview1`).
///
/// # Errors
///
/// 任一 host 函数登记失败 (重名等) 时返回 `LinkerError`.
#[cfg(not(test))]
fn register_wasi(linker: &mut Linker<()>) -> Result<(), LinkerError> {
    // SIMPLIFIED: P1 仅 host hello demo 所需的 6 个 stdio 相关 import; 影响面:
    // 需 path_open/args/env/random 等的模块在 instantiate 阶段失败退 1; 何时需
    // 扩展: P2+ 接更广 WASI 程序时按 import 面逐项补 host.
    const MOD: &str = "wasi_snapshot_preview1";
    linker.func_wrap(MOD, "proc_exit", host_proc_exit)?;
    linker.func_wrap(MOD, "fd_write", host_fd_write)?;
    linker.func_wrap(MOD, "fd_close", host_fd_close)?;
    linker.func_wrap(MOD, "fd_fdstat_get", host_fd_fdstat_get)?;
    linker.func_wrap(MOD, "fd_seek", host_fd_seek)?;
    linker.func_wrap(MOD, "clock_time_get", host_clock_time_get)?;
    Ok(())
}

/// 解释执行模块: 正常结束返 0, 装载/解析/实例化/trap 失败返 1.
///
/// 模块若自行调 `proc_exit`, 进程已在 host 闭包内终止, 不会走到这里返回值.
#[cfg(not(test))]
fn run(module_bytes: &[u8]) -> i32 {
    let engine = wasmi::Engine::default();
    let Ok(module) = wasmi::Module::new(&engine, module_bytes) else {
        return 1;
    };
    let mut store = wasmi::Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    if register_wasi(&mut linker).is_err() {
        return 1;
    }
    let Ok(instance) = linker.instantiate_and_start(&mut store, &module) else {
        return 1;
    };
    let Ok(start) = instance.get_typed_func::<(), ()>(&store, "_start") else {
        return 1;
    };
    match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// 运行时入口 (由汇编 `_start` 转入).
///
/// 栈布局 (`sp` 起): `argc` → `argv[0..argc]` → `NULL` → `envp[0..]` → `NULL`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn wasmrun_main(sp: u64) -> ! {
    // SAFETY: 入口栈由内核按上述布局布置, 槽位均在有效区间内.
    let argc = unsafe { word_at(sp, 0) };
    if argc == 0 {
        proc_exit(1);
    }
    // SAFETY: argv[0] 为内核复制的模块路径指针.
    let path = unsafe { word_at(sp, 1) };
    if path == 0 {
        proc_exit(1);
    }
    let Some(bytes) = load_module(path) else {
        proc_exit(1);
    };
    proc_exit(run(&bytes));
}
