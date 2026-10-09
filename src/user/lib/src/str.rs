/// 字符串工具
pub fn cmp(a: &[u8], b: &[u8]) -> i32 {
    let end = if a.len() < b.len() { a.len() } else { b.len() };
    for i in 0..end {
        let d = a[i] as i32 - b[i] as i32;
        if d != 0 {
            return d;
        }
    }
    a.len() as i32 - b.len() as i32
}

struct ParseBuf {
    ptrs: [*const u8; 32],
    buf: [u8; 256],
}
static mut PARSE: ParseBuf = ParseBuf {
    ptrs: [core::ptr::null(); 32],
    buf: [0u8; 256],
};

pub fn parse_args(line: &[u8]) -> (&'static [*const u8], usize) {
    let parse = unsafe { &mut *core::ptr::addr_of_mut!(PARSE) };
    let mut argc = 0;
    let mut in_arg = false;
    let mut in_quote = false;
    let mut out = 0;
    for &byte in line {
        if byte == 0 {
            break;
        }
        if byte == b'"' {
            in_quote = !in_quote;
        } else if byte == b' ' && !in_quote {
            if in_arg {
                parse.buf[out] = 0;
                out += 1;
                in_arg = false;
            }
        } else {
            if !in_arg && argc < 31 {
                parse.ptrs[argc] = parse.buf[out..].as_ptr();
                argc += 1;
                in_arg = true;
            }
            if out < parse.buf.len() {
                parse.buf[out] = byte;
                out += 1;
            }
        }
    }
    if in_arg && out < parse.buf.len() {
        parse.buf[out] = 0;
    }
    parse.ptrs[argc] = core::ptr::null();
    (&parse.ptrs[..argc + 1], argc)
}
