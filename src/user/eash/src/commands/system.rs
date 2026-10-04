/// 系统命令: osinfo, host, ps, reboot, halt
use userlib::{print, println};
use userlib::sys::{gethostname, sethostname, reboot as sys_reboot};
use core::fmt::Write;

use super::{Cmd, as_str};

struct FmtWriter;
impl Write for FmtWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        print(s);
        Ok(())
    }
}

pub fn osinfo(_: &Cmd) {
    println("Edgine Operating System");
    println("Kernel:  Edgine (EG)");
    println("Userland: Rust");
    #[cfg(target_arch = "x86_64")]
    println("Arch:    x86_64");
    #[cfg(target_arch = "aarch64")]
    println("Arch:    aarch64");
}

pub fn host(cmd: &Cmd) {
    if cmd.n == 1 {
        let mut buf = [0u8; 64];
        if gethostname(&mut buf) == 0 {
            println(core::str::from_utf8(&buf).unwrap_or("?").trim_end_matches('\0'));
        } else { println("Error"); }
    } else {
        let name = as_str(cmd.get(1));
        if sethostname(name.as_bytes()) == 0 { print("Host: "); println(name); }
        else { println("Error"); }
    }
}

/// ps: 列出所有进程
pub fn ps(_: &Cmd) {
    let mut buf = [0u8; 4096];
    let count = userlib::sys::proc_list(&mut buf, 64);
    if count <= 0 {
        println("ps: no processes (proc_list returned {})");
        return;
    }

    println("PID   STATE   PWM     PRI  NAME");
    println("----- ------- -------- ---- --------");
    for i in 0..count as usize {
        let entry = &buf[i * 64..(i + 1) * 64];
        let pid = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let state = entry[4];
        let pwm = u64::from_le_bytes([entry[8], entry[9], entry[10], entry[11],
            entry[12], entry[13], entry[14], entry[15]]);
        let pri = u32::from_le_bytes([entry[16], entry[17], entry[18], entry[19]]);

        let state_str = match state {
            0 => "RUN  ", 1 => "READY", 2 => "WAIT ", 3 => "SLEEP",
            _ => "?    ",
        };

        let mut name = [0u8; 48];
        let name_start = 24;
        for j in 0..48 { name[j] = entry[name_start + j]; if entry[name_start + j] == 0 { break; } }
        let name_str = core::str::from_utf8(&name).unwrap_or("?").trim_end_matches('\0');

        let _ = write!(FmtWriter, "{:<5} {:<6} {:08X}  {:>4}  ", pid, state_str, pwm, pri);
        println(name_str);
    }
}

/// reboot: 重启系统
pub fn reboot(_: &Cmd) {
    println("Rebooting...");
    sys_reboot(0); // cmd=0: normal reboot
}

/// halt: 关机
pub fn halt(_: &Cmd) {
    println("Halting...");
    sys_reboot(1); // cmd=1: shutdown/halt
}