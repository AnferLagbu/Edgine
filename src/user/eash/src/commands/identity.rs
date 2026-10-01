/// 身份认证命令: login, logout, who, passwd
use userlib::*;
use userlib::sys::*;

use super::{Cmd, as_str};

pub fn login(cmd: &Cmd) {
    if cmd.n < 3 { println("login: usage: login <note> <password>"); return; }
    let note = as_str(cmd.get(1)); let pw = as_str(cmd.get(2));
    let mut nb = [0u8; 128]; let nbl = core::cmp::min(note.len(), 127);
    nb[..nbl].copy_from_slice(&note.as_bytes()[..nbl]); nb[nbl] = 0;
    let mut pb = [0u8; 128]; let pbl = core::cmp::min(pw.len(), 127);
    pb[..pbl].copy_from_slice(&pw.as_bytes()[..pbl]); pb[pbl] = 0;
    let result = auth_login(&nb[..nbl + 1], &pb[..pbl + 1]);
    if result > 0 { print("Logged in: "); print_hex(proc_get_pwm()); print("\n"); }
    else if result == -104 { println("login: wrong password"); }
    else if result == -101 { println("login: not found"); }
    else { println("login: failed"); }
}

pub fn logout(_: &Cmd) { auth_logout(); println("Logged out"); }

pub fn who(_: &Cmd) {
    let pwm = proc_get_pwm();
    if pwm == 0 { println("Not logged in"); } else { print("PWM: "); print_hex(pwm); print("\n"); }
}

pub fn passwd(_: &Cmd) {
    if proc_get_pwm() == 0 { println("Not logged in"); return; }
    print("Current password: "); let mut old = [0u8; 64]; read_line(&mut old);
    print("New password: "); let mut new = [0u8; 64]; read_line(&mut new);
    print("Confirm: "); let mut confirm = [0u8; 64]; let cl = read_line(&mut confirm);
    if cmp(&new[..cl], &confirm[..cl]) != 0 { println("Mismatch"); return; }
    let r = auth_change_password(&old, &new);
    if r == 0 { println("Password changed"); }
    else if r == -104 { println("Wrong current password"); }
    else { println("Failed"); }
}