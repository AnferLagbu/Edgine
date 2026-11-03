/// 控制台 I/O
use crate::sys;

/// 整行单次写出的正文上限 (字节).
///
/// 与内核侧 `klog::CONSOLE_EMIT_MAX` (256) 对齐: 正文 + `\n` 不超过一次内核控制台
/// 写出分块, 才能落在同一个持锁段里.
const PRINT_LINE_MAX: usize = 255;

pub fn print(s: &str) {
    let _ = sys::fs_write(1, s.as_bytes());
}
pub fn println(s: &str) {
    // 整行单次 write: 正文与换行分两次 write 会在段间给别的写者留缝 (内核控制台写出
    // 已收为行原子单点, 见 `privileged::klog::console_write_bytes`).
    let bytes = s.as_bytes();
    if bytes.len() <= PRINT_LINE_MAX {
        let mut buf = [0u8; PRINT_LINE_MAX + 1];
        buf[..bytes.len()].copy_from_slice(bytes);
        buf[bytes.len()] = b'\n';
        let _ = sys::fs_write(1, &buf[..=bytes.len()]);
        return;
    }
    // SIMPLIFIED: 超上限的行退回两次 write; 影响面 = 这类极长行仍可能在切段处被插入;
    // 需要任意长度整段原子时改走内核侧行缓冲.
    print(s);
    print("\n");
}
pub fn print_char(c: u8) {
    let _ = sys::fs_write(1, &[c]);
}

pub fn print_hex(mut val: u64) {
    let mut buf = [b'0'; 16];
    for i in (0..16).rev() {
        let d = (val & 0xF) as u8;
        buf[i] = if d < 10 { b'0' + d } else { b'A' + d - 10 };
        val >>= 4;
    }
    print("0x");
    let _ = sys::fs_write(1, &buf);
}

/// 十进制打印 `i64` (负数带 `-` 前缀).
///
/// # Panics
///
/// 取模结果超出 `u8` 时 panic —— `val` 已在本函数内取绝对值, 余数恒在 0..=9.
pub fn print_dec(mut val: i64) {
    let mut buf = [0u8; 21];
    let mut i = 20;
    let neg = val < 0;
    if neg {
        val = -val;
    }
    if val == 0 {
        i -= 1;
        buf[i] = b'0';
    } else {
        while val > 0 {
            i -= 1;
            buf[i] = b'0' + u8::try_from(val % 10).expect("末位十进制数字必在 u8 范围");
            val /= 10;
        }
    }
    if neg {
        i -= 1;
        buf[i] = b'-';
    }
    // 有效数字区为 buf[i..20] (索引 20 为填充位); 切片上界若取 21 会多输出 1 个 NUL
    let _ = sys::fs_write(1, &buf[i..20]);
}

pub fn read_line(buf: &mut [u8]) -> usize {
    let max = buf.len();
    let mut i = 0;
    while i < max.saturating_sub(1) {
        let mut c = 0u8;
        if sys::fs_read(0, core::slice::from_mut(&mut c)) <= 0 {
            continue;
        }
        if c == b'\n' {
            print("\n");
            break;
        } else if c == 0x7F || c == 0x08 {
            if i > 0 {
                i -= 1;
                print("\x08 \x08");
            }
        } else if (b' '..=b'~').contains(&c) {
            buf[i] = c;
            i += 1;
            print_char(c);
        }
    }
    if i < max {
        buf[i] = 0;
    }
    i
}
