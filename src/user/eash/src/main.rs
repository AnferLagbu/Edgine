//! eash — easy shell (Edgine userland)
//!
//! 模块化 Shell: 主循环 + 提示符在此，命令实现位于 `commands/` 子目录。

#![no_std]
#![no_main]

mod commands;

use core::sync::atomic::{AtomicBool, Ordering};
use userlib::sys::*;
use userlib::*;

/// 全局退出标记 (由 `exit` 命令设置)
pub static MAIN_EXIT: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    userlib::print("[eash] PANIC: ");
    if let Some(loc) = info.location() {
        userlib::print("at ");
        userlib::print(loc.file());
        userlib::print(":");
        print_dec(loc.line() as i64);
    }
    userlib::print("\n");
    proc_exit(1);
}

fn banner() {
    println("");
    println("  ___  _  _ ___");
    println(" / _ \\| || | __|");
    println("| (_) | || |__ \\");
    println(" \\___/|_||_|___/");
    println("");
    println("eash - Edgine Shell  (type 'help')");
    println("");
}

fn prompt() {
    let mut cwd = [0u8; 64];
    let _ = env_getcwd(&mut cwd);
    let cwd_str = core::str::from_utf8(&cwd)
        .unwrap_or("/")
        .trim_end_matches('\0');
    let pwm = proc_get_pwm();
    if pwm != 0 {
        print("[");
        print_hex(pwm);
        print("]");
    }
    print(if cwd_str.is_empty() { "/" } else { cwd_str });
    print("> ");
}

fn shell_main() {
    banner();
    let mut line = [0u8; 256];
    loop {
        if MAIN_EXIT.load(Ordering::SeqCst) {
            break;
        }
        prompt();
        let len = read_line(&mut line);
        if len == 0 {
            continue;
        }

        let input = &line[..len];

        // 管道 / 重定向 — fork+exec 路径
        if commands::is_pipeline(input) {
            commands::execute_pipeline(input);
            continue;
        }

        // 内置命令调度
        let cmd = commands::Cmd::new(input);
        if cmd.n == 0 {
            continue;
        }
        commands::dispatch(&cmd);
    }
}

#[unsafe(no_mangle)]
pub fn _start() -> ! {
    shell_main();
    proc_exit(0);
}
