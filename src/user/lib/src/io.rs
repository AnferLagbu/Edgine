/// 控制台 I/O
use crate::sys;

pub fn print(s: &str)                           { sys::fs_write(1, s.as_bytes()); }
pub fn println(s: &str)                         { print(s); print("\n"); }
pub fn print_char(c: u8)                        { sys::fs_write(1, &[c]); }

pub fn print_hex(mut val: u64) {
    let mut buf = [b'0'; 16];
    for i in (0..16).rev() { let d = (val & 0xF) as u8; buf[i] = if d < 10 { b'0'+d } else { b'A'+d-10 }; val >>= 4; }
    print("0x"); sys::fs_write(1, &buf);
}

pub fn print_dec(mut val: i64) {
    let mut buf = [0u8; 21]; let mut i = 20; let neg = val < 0;
    if neg { val = -val; }
    if val == 0 { i-=1; buf[i]=b'0'; }
    else { while val > 0 { i-=1; buf[i]=b'0'+(val%10) as u8; val/=10; } }
    if neg { i-=1; buf[i]=b'-'; }
    // 有效数字区为 buf[i..20] (索引 20 为填充位); 切片上界若取 21 会多输出 1 个 NUL
    sys::fs_write(1, &buf[i..20]);
}

pub fn read_line(buf: &mut [u8]) -> usize {
    let max = buf.len(); let mut i = 0;
    while i < max.saturating_sub(1) {
        let mut c = 0u8; if sys::fs_read(0, core::slice::from_mut(&mut c)) <= 0 { continue; }
        if c == b'\n' { print("\n"); break; }
        else if c == 0x7F || c == 0x08 { if i > 0 { i -= 1; print("\x08 \x08"); } }
        else if (b' '..=b'~').contains(&c) { buf[i] = c; i += 1; print_char(c); }
    }
    if i < max { buf[i] = 0; } i
}
