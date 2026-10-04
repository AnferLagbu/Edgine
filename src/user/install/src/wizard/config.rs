//! 系统配置 — 主机名 + 目录树 + fstab (写入 /mnt)

use userlib::{print, println, read_line};
use userlib::sys;
use userlib::fs;

const DEFAULT_HOSTNAME: &[u8] = b"localhost\0";
const HOSTNAME_FILE: &[u8] = b"/mnt/cfg/system/hostname\0";
const FSTAB_FILE:   &[u8] = b"/mnt/cfg/system/fstab\0";

fn mkdir(path: &[u8]) {
    let mut p = [0u8; 128];
    let len = core::cmp::min(path.len(), 127);
    p[..len].copy_from_slice(&path[..len]); p[len] = 0;
    sys::fs_mkdir(&p[..len + 1]);
}

pub fn hostname() {
    println(""); println("--- Step 5: System Configuration ---"); println("");
    print("Enter hostname (default: localhost): ");
    let mut h = [0u8; 64]; let len = read_line(&mut h);
    let name: &[u8] = if len == 0 { DEFAULT_HOSTNAME } else { &h[..len + 1] };
    let name_len = if len == 0 { 9 } else { len };
    let r = sys::sethostname(&name[..name_len]);
    if r == 0 { print("Hostname set to: "); let s = core::str::from_utf8(&name[..name_len]).unwrap_or("?"); println(s); }
    else { println("Warning: Failed to set hostname, using default."); }
    let fd = fs::file_open(HOSTNAME_FILE, sys::O_CREAT | sys::O_WRONLY | sys::O_TRUNC);
    if fd >= 0 { sys::fs_write(fd, &name[..name_len]); sys::fs_close(fd); }
    println(""); println("System configuration complete!");
}

pub fn directory_tree() {
    println("Creating directory structure...");
    for dir in [
        b"/mnt/cfg\0".as_slice(), b"/mnt/cfg/boot\0".as_slice(), b"/mnt/cfg/kernel\0".as_slice(),
        b"/mnt/cfg/system\0".as_slice(), b"/mnt/cfg/gui\0".as_slice(),
        b"/mnt/app\0".as_slice(), b"/mnt/app/bin\0".as_slice(), b"/mnt/app/sys\0".as_slice(),
        b"/mnt/data\0".as_slice(), b"/mnt/data/id\0".as_slice(), b"/mnt/data/share\0".as_slice(),
        b"/mnt/data/var\0".as_slice(), b"/mnt/data/var/log\0".as_slice(), b"/mnt/data/var/run\0".as_slice(),
        b"/mnt/gui\0".as_slice(), b"/mnt/gui/font\0".as_slice(), b"/mnt/gui/theme\0".as_slice(),
        b"/mnt/gui/wallpaper\0".as_slice(), b"/mnt/gui/cursor\0".as_slice(),
        b"/mnt/dev\0".as_slice(), b"/mnt/proc\0".as_slice(), b"/mnt/temp\0".as_slice(), b"/mnt/mnt\0".as_slice(),
    ] { mkdir(dir); }
    println("  [OK] Directory structure created");
}

pub fn fstab() {
    let fd = fs::file_open(FSTAB_FILE, sys::O_CREAT | sys::O_WRONLY | sys::O_TRUNC);
    if fd < 0 { println("  [WARN] Failed to create fstab"); return; }
    let content = b"# Edgine Filesystem Configuration\n# Format: source mountpoint type options\n\nnone    /dev    devfs   defaults\nnone    /proc   procfs  defaults\nnone    /temp   ramfs   defaults,size=64M\n";
    sys::fs_write(fd, content); sys::fs_close(fd);
    println("  [OK] fstab created");
}
