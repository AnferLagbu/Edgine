//! reboottest — R3 QEMU 真重启腿的 PID 1 载荷.
//!
//! 由 `scripts/qemu_reboot_test.sh` 打包为 initramfs 的 `/init` 并**真启动**,
//! 作用是把 reboot syscall 走完整链: 用户态 `reboot(0)` → `SYS_SGEG_REBOOT` →
//! `functions::proc::sysinfo::reboot_syscall` (PWM 0 放行) →
//! `privileged::syscall::api::reboot_mechanism` → x86_64 `reboot_via_idt` /
//! aarch64 `psci::system_reset`.
//!
//! 判据 (脚本侧): 启动日志出现 `[REBOOT] PASS:` **恰好两轮以上** —— 只有整机
//! 真的复位并重新引导, 同一份 `/init` 才会第二次打印该行. 若 reboot 路径失效
//! (例如命令语义写反走成停机自旋), 该行只出现一次, 脚本超时判红; 若 reboot
//! 返回 (机制未生效), 本进程打印 `[REBOOT] FAIL:` 并以非零码退出.

#![no_std]
#![no_main]

use userlib::println;
use userlib::sys::{REBOOT_CMD_RESTART, proc_exit, reboot};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    proc_exit(2);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println("[REBOOT] PASS: reboottest requesting restart (cmd=0)");
    let _ = reboot(REBOOT_CMD_RESTART);
    // 走到这里说明 reboot 机制返回了 (未复位) —— 显式失败, 与超时判据互补.
    println("[REBOOT] FAIL: reboot syscall returned");
    proc_exit(1);
}
