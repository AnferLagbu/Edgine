//! wasmrun — 用户态 wasmi WASM 运行时 (WASM 退用户态迁移)
//!
//! `execve` 按魔数识别 `\0asm` 后, 经构建 feature `wasm_userspace` 选桩装载本
//! 映像 (过渡期与内核解释器桩 wasmstub 共存, P4 翻转默认). 本运行时在用户态:
//!
//! 1. 经 VFS open/read 读入 wasm 模块 (`argv[0]` 即模块路径);
//! 2. wasmi Engine/Module/Store/Linker 解释执行 (消费 P0 `global_allocator`);
//! 3. 完整 WASI preview1 host 层 (38 函数, 见 `host.rs`) 翻译到 Edgine syscall;
//! 4. 以模块退出码 (正常结束) 或 1 (装载/解析/trap 失败) 调 `proc_exit`.
//!
//! 与 wasmstub 的区别: 解释器在用户态 (wasmi), 不再经 `EG_WASM_*` 回环内核.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

extern crate alloc;

mod wasi;
#[cfg(not(test))]
mod host;

#[cfg(not(test))]
use alloc::vec::Vec;
#[cfg(not(test))]
use userlib::sys::{O_RDONLY, close, open, proc_exit, read};

#[cfg(not(test))]
use wasmi::{Linker, Store};

use crate::wasi::WasiCtx;

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
    // SAFETY: 由调用方保证 idx 在入口栈布局内 (仅用于 argc/argv/envp 槽).
    unsafe { core::ptr::read_volatile((sp as *const u64).add(idx)) }
}

/// 从用户态 NUL 终止字符串指针读入字节 (上限 4096, 防 runaway).
///
/// # Safety
/// `ptr` 必须是内核复制进用户地址空间的有效字符串指针.
#[cfg(not(test))]
unsafe fn read_cstr(ptr: u64) -> Option<Vec<u8>> {
    if ptr == 0 {
        return None;
    }
    let p = ptr as *const u8;
    let mut v = Vec::new();
    for i in 0..4096 {
        // SAFETY: ptr 指向内核布置的 argv/envp 字符串, 逐字节读到 NUL 或上限即止.
        let b = unsafe { core::ptr::read_volatile(p.add(i)) };
        if b == 0 {
            return Some(v);
        }
        v.push(b);
    }
    None
}

/// 读入口栈上的以 NULL 结尾的字符串向量.
///
/// # Safety
/// `start` 必须指向 argv/envp 数组首槽, 且数组以 NULL 结尾.
#[cfg(not(test))]
unsafe fn read_strvec(start: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for i in 0..256 {
        // SAFETY: 槽位在入口栈向量内, 由调用方保证.
        let ptr = unsafe { word_at(start, i) };
        if ptr == 0 {
            break;
        }
        // SAFETY: 槽位非空时是内核复制的有效字符串.
        if let Some(s) = unsafe { read_cstr(ptr) } {
            out.push(s);
        }
    }
    out
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

/// 解释执行模块: 正常结束返 0, 装载/解析/实例化/trap 失败返 1.
#[cfg(not(test))]
fn run(module_bytes: &[u8], ctx: WasiCtx) -> i32 {
    let engine = wasmi::Engine::default();
    let Ok(module) = wasmi::Module::new(&engine, module_bytes) else {
        return 1;
    };
    let mut store = Store::new(&engine, ctx);
    let mut linker = Linker::new(&engine);
    if host::register_wasi(&mut linker).is_err() {
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

    // SAFETY: argv 数组从 sp+8 开始, argc 已由内核保证.
    let args = unsafe { read_strvec(sp.wrapping_add(8)) };
    // SAFETY: argv 后是 NULL 槽, 再后是 envp; 入口栈总在有效区间内.
    let env_start = sp.wrapping_add(8u64.wrapping_mul(argc.wrapping_add(2)));
    let env = unsafe { read_strvec(env_start) };

    let mut ctx = WasiCtx::new(args, env);
    host::init_root(&mut ctx);
    proc_exit(run(&bytes, ctx));
}
