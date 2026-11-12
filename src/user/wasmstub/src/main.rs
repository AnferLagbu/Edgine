//! wasmstub — 进程级 wasm 载体的用户态入口桩 (W-7 批次 B)
//!
//! `execve` 在 `exec.rs` 按魔数识别 `\0asm` 后, 并不装载 wasm 本体, 而是把
//! 本桩装成新映像 (argv 原样保留: `argv[0]` 即 wasm 模块路径). 桩随后:
//!
//! 1. 经 `EG_WASM_LOAD` 让内核经 VFS 读模块、解析并登记实例 (argv/envp 一并交给
//!    内核复制进 WASI 上下文);
//! 2. `EG_WASM_RUN` 把实例跑到结束 (受解释器 gas 上限);
//! 3. `EG_WASM_GET_INFO` 取终态与退出码, `EG_WASM_UNLOAD` 释放实例;
//! 4. 以模块退出码 (正常结束) 或 1 (trap / 装载失败) 调 `proc_exit`.
//!
//! 这样 wasm 进程仍由调度器按普通用户进程调度, 内核侧解释器只在 syscall 内执行.

#![no_std]
#![no_main]

use userlib::sys::{
    EG_WASM_STATUS_FINISHED, EgWasmInfo, proc_exit, wasm_get_info, wasm_load, wasm_run, wasm_unload,
};

// `extern "C" fn _start` 读不到入口 SP, 故用架构汇编把 SP 作为首参转发
// (与 `exectest` 同一形态).
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".global _start",
    "_start:",
    "mov rdi, rsp",
    "call wasmstub_main",
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".global _start",
    "_start:",
    "mov x0, sp",
    "bl wasmstub_main",
);

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    proc_exit(1);
}

/// 用户栈上第 `idx` 个 8 字节槽.
///
/// # Safety
/// `sp` 是内核写入的进程入口栈指针, `idx` 必须落在内核布置的 argc/argv/envp 区间内.
unsafe fn word_at(sp: u64, idx: usize) -> u64 {
    // SAFETY: 由调用方保证 idx 在入口栈布局内 (仅用于 argc/argv/envp 槽).
    unsafe { core::ptr::read_volatile((sp as *const u64).add(idx)) }
}

/// 桩入口 (由汇编 `_start` 转入).
///
/// 栈布局 (`sp` 起): `argc` → `argv[0..argc]` → `NULL` → `envp[0..]` → `NULL`.
#[unsafe(no_mangle)]
extern "C" fn wasmstub_main(sp: u64) -> ! {
    // SAFETY: 入口栈由内核按上述布局布置, 槽位均在有效区间内.
    let argc = unsafe { word_at(sp, 0) };
    if argc == 0 {
        proc_exit(1);
    }
    let argv_ptr = sp.wrapping_add(8);
    // SAFETY: envp 首地址 = argv 数组 (argc 项) + 结尾 NULL.
    let envp_ptr = argv_ptr.wrapping_add((argc + 1) * 8);
    // SAFETY: argv[0] 为内核复制的模块路径指针.
    let path = unsafe { word_at(sp, 1) };
    if path == 0 {
        proc_exit(1);
    }

    let loaded = wasm_load(
        path as *const u8,
        argv_ptr as *const u64,
        envp_ptr as *const u64,
    );
    if loaded <= 0 {
        proc_exit(1);
    }
    let handle = loaded.cast_unsigned();

    let _ = wasm_run(handle);
    let mut info = EgWasmInfo {
        status: 0,
        exit_code: 1,
        gas_used: 0,
    };
    let info_ret = wasm_get_info(handle, &mut info);
    let _ = wasm_unload(handle);

    let code = if info_ret == 0 && info.status == EG_WASM_STATUS_FINISHED {
        info.exit_code
    } else {
        1
    };
    proc_exit(code);
}
