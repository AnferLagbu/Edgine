/// 字符串工具
///
/// `cmp` 按字典序比较两个字节串, 返回负 / 零 / 正.
///
/// # Panics
///
/// 任一入参长度超出 `i32` 时 panic —— 用户态命令行字符串远小于该上限.
#[must_use]
pub fn cmp(a: &[u8], b: &[u8]) -> i32 {
    let end = if a.len() < b.len() { a.len() } else { b.len() };
    for i in 0..end {
        let d = i32::from(a[i]) - i32::from(b[i]);
        if d != 0 {
            return d;
        }
    }
    i32::try_from(a.len()).expect("字符串长度超出 i32")
        - i32::try_from(b.len()).expect("字符串长度超出 i32")
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
    // SAFETY: PARSE 是本模块唯一的全局解析暂存区, 取 &mut 时不存在其它活引用 ——
    // 前提是本函数不被重入 (返回的 ptrs 指向同一块 buf, 嵌套调用会互相覆盖) 且
    // 不在多线程下并发调用 (无锁保护); 用户态命令行解析是单线程一次性场景.
    // 返回的切片只在下一次 parse_args 调用前有效, 调用方须在该窗口内消费完毕.
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
    (&parse.ptrs[..=argc], argc)
}
