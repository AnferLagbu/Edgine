//! Edgine 独立安装程序
//! 可作为独立二进制运行，执行完整 6 步系统安装。

#![no_std]
#![no_main]

use userlib::*;
use userlib::sys::*;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    userlib::print("[install] PANIC: ");
    if let Some(loc) = info.location() {
        userlib::print("at "); userlib::print(loc.file());
        userlib::print(":"); print_dec(loc.line() as i64);
    }
    userlib::print("\n");
    proc_exit(1);
}

#[unsafe(no_mangle)]
pub fn _start() -> ! {
    install::wizard::run();
    proc_exit(0);
}
