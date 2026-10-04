//! 安装完成: 标记文件写入 (/mnt) + sync + 重启

use userlib::{println, read_line, delay_loop};
use userlib::sys;
use userlib::fs;

const MARKER_FILE: &[u8] = b"/mnt/.antx_installed\0";

pub fn execute() -> i32 {
    println(""); println("--- Step 6: Finalizing Installation ---"); println("");

    println("Syncing filesystem to disk..."); sys::fs_sync();

    println("Creating installation marker...");
    let fd = fs::file_open(MARKER_FILE, sys::O_CREAT | sys::O_WRONLY);
    if fd < 0 { println("Error: Failed to create installation marker!"); return -1; }
    sys::fs_write(fd, b"installed\n"); sys::fs_close(fd); sys::fs_sync();

    println("");
    println("========================================");
    println("     Installation Complete!");
    println("========================================");
    println(""); println("Edgine has been installed to your disk.");
    println("Please remove the installation media");
    println("and press ENTER to reboot your system.");
    println("");
    let mut buf = [0u8; 16]; read_line(&mut buf);
    println("Rebooting system..."); delay_loop(10_000_000);
    sys::reboot(0);
    0
}

pub fn check_marker() -> bool {
    let fd = fs::file_open(MARKER_FILE, sys::O_RDONLY);
    if fd >= 0 { sys::fs_close(fd); false } else { true }
}
