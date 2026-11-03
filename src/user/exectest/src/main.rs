//! exectest — K-07b 批2 execve 运行时探针载荷
//!
//! 由 init 经 `execve` 拉起并自检三条判据 (任何判据失败即按专属退出码退出,
//! 见下方 `EXIT_*` 常量; panic 为 1):
//! 1. argc/argv: 长度 3, `exectest` / 腿标记 / `K07B` 逐字一致;
//! 2. envp: 恰好两条且逐字为 `K07B_ENV=1` / `K07B_BATCH=2` —— K-07b 批2 前
//!    内核把所有 envp 指针写成 NULL, 本判据必然失败;
//! 3. cwd: `getcwd` 回读与腿标记一致 (`cwd` 腿 `/bin`, `root` 腿 `/`).
//!
//! 通过时打印 `[EXEC] PASS: <腿> argv+envp+cwd`, 供 init 与启动门禁断言.

#![no_std]
#![no_main]

use userlib::sys::{env_getcwd, proc_exit};
use userlib::{print, print_char, print_dec, println};

/// 探针失败退出码 (init 侧按 wstatus 判非 0).
///
/// 逐判据分配独立码: 串口输出按 `write` 分片, 与父进程打印交错时可能丢失
/// 片段; 内核 `exit: pid=N code=X` 是原子日志, 据此可唯一确定失败判据.
const EXIT_PANIC: i32 = 1;
const EXIT_ARGC: i32 = 11;
const EXIT_ARGV_MARKER: i32 = 12;
const EXIT_ARGV_LEG: i32 = 13;
const EXIT_ARGV_SENTINEL: i32 = 14;
const EXIT_ENV_COUNT: i32 = 15;
const EXIT_ENV_VALUE: i32 = 16;
const EXIT_GETCWD: i32 = 17;
const EXIT_CWD: i32 = 18;

/// 入口栈上 envp 首槽下标: argc=3 已断言后, 布局 = argc(1) + argv(3 + NULL),
/// 故首槽恒为 1 + 3 + 1 = 5.
const ENV_SLOT: usize = 5;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    proc_exit(EXIT_PANIC);
}

// 入口需要拿到内核写在用户栈顶的 argc/argv/envp 布局起点 (SP 本身), 普通
// `extern "C" fn _start` 读不到入口 SP, 故用架构汇编把 SP 作为首参转发.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".global _start",
    "_start:",
    "mov rdi, rsp",
    "call exectest_main",
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".global _start",
    "_start:",
    "mov x0, sp",
    "bl exectest_main",
);

/// 用户栈上第 `idx` 个 8 字节槽.
///
/// # Safety
/// `sp` 是内核写入的进程入口栈指针, `idx` 必须落在内核为 argc/argv/envp
/// 布置的区间内.
unsafe fn word_at(sp: u64, idx: usize) -> u64 {
    // SAFETY: 由调用方保证 idx 在入口栈布局内 (本文件只在 argc/argv/envp 槽上使用).
    unsafe { core::ptr::read_volatile((sp as *const u64).add(idx)) }
}

/// 用户字符串 `ptr` 是否逐字节等于 `expect` 且以 NUL 结尾.
///
/// # Safety
/// `ptr` 必须指向内核复制进本进程栈的 NUL 结尾字符串.
unsafe fn cstr_eq(ptr: u64, expect: &[u8]) -> bool {
    for (idx, &want) in expect.iter().enumerate() {
        // SAFETY: ptr 来自入口栈 argv/envp 槽, 由内核写入为合法 C 字符串.
        if unsafe { core::ptr::read_volatile((ptr as *const u8).add(idx)) } != want {
            return false;
        }
    }
    // SAFETY: 同上, 读取字符串终止符.
    unsafe { core::ptr::read_volatile((ptr as *const u8).add(expect.len())) == 0 }
}

/// 失败出口: `tag` 定位判据, `code` 携带实测读数, `exit` 为该判据专属退出码.
fn fail(tag: &'static str, code: i64, exit: i32) -> ! {
    print("[EXEC] FAIL: ");
    print(tag);
    print(" code=");
    print_dec(code);
    print_char(b'\n');
    proc_exit(exit);
}

/// 探针入口 (由汇编 `_start` 转入).
#[unsafe(no_mangle)]
extern "C" fn exectest_main(sp: u64) -> ! {
    // SAFETY: sp 是内核为本进程布置的入口栈顶, 槽位布局写侧见
    // `privileged::proc::user_proc::setup_user_stack_in_table`.
    let argc = unsafe { word_at(sp, 0) };
    if argc != 3 {
        fail("argc", i64::try_from(argc).unwrap_or(i64::MAX), EXIT_ARGC);
    }
    let argv0 = unsafe { word_at(sp, 1) };
    let argv1 = unsafe { word_at(sp, 2) };
    let argv2 = unsafe { word_at(sp, 3) };
    // SAFETY: 三个槽均由内核按 argv 数组写入.
    if !unsafe { cstr_eq(argv0, b"exectest") } || !unsafe { cstr_eq(argv2, b"K07B") } {
        fail("argv-marker", 0, EXIT_ARGV_MARKER);
    }
    // SAFETY: argv[1] 由 init 侧按腿写入.
    let (leg, expect_cwd) = if unsafe { cstr_eq(argv1, b"cwd") } {
        (b"cwd".as_slice(), b"/bin".as_slice())
    } else if unsafe { cstr_eq(argv1, b"root") } {
        (b"root".as_slice(), b"/".as_slice())
    } else {
        fail("argv-leg", 0, EXIT_ARGV_LEG)
    };

    // SAFETY: argv NULL 哨兵与 envp 指针数组均由内核按入口栈布局写入.
    if unsafe { word_at(sp, 4) } != 0 {
        fail("argv-sentinel", 0, EXIT_ARGV_SENTINEL);
    }
    let env0 = unsafe { word_at(sp, ENV_SLOT) };
    let env1 = unsafe { word_at(sp, ENV_SLOT + 1) };
    let env_end = unsafe { word_at(sp, ENV_SLOT + 2) };
    if env0 == 0 || env1 == 0 || env_end != 0 {
        // 条目数不是恰好 2 (含哨兵缺失/多余) 一律失败.
        fail("env-count", 0, EXIT_ENV_COUNT);
    }
    // SAFETY: env0/env1 由内核从调用进程 envp 复制, 均为合法 C 字符串.
    if !unsafe { cstr_eq(env0, b"K07B_ENV=1") } || !unsafe { cstr_eq(env1, b"K07B_BATCH=2") } {
        fail("env-value", 0, EXIT_ENV_VALUE);
    }

    let mut cwd = [0u8; 64];
    let n = env_getcwd(&mut cwd);
    let Ok(n) = usize::try_from(n) else {
        fail("getcwd", i64::from(n), EXIT_GETCWD);
    };
    if cwd.get(..n) != Some(expect_cwd) {
        fail("cwd", i64::try_from(n).unwrap_or(i64::MAX), EXIT_CWD);
    }

    print("[EXEC] PASS: ");
    print(core::str::from_utf8(leg).unwrap_or("?"));
    println(" argv+envp+cwd");
    proc_exit(0);
}
