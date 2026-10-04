//! KGDB — 内核调试器桩 (TCB)
//!
//! ## 协议
//!
//! 通过串口 (默认 COM1 / pl011) 与外部 gdb 通信, 实现精简版 GDB
//! Remote Serial Protocol (RSP) 子集:
//!
//! - `$ packet # checksum` 行格式
//! - `+`/`-` 确认
//! - 支持的 packet: `?` (停止原因), `g`/`G` (读/写寄存器), `m`/`M` (读/写内存),
//!   `c`/`s` (继续/单步), `Z0`/`z0` (插桩/取消软件断点), `k` (kill)
//!
//! ## 入口
//!
//! - [`kgdb_breakpoint`][]: 主动断点
//! - [`kgdb_handle_exception`][]: 异常处理钩子
//!
//! ## 当前限制
//!
//! - 串口驱动由 caller 提供 trait 实现, 通过 [`kgdb_set_serial`] 注入
//! - 不支持多线程同步, 进入 KGDB 后其他 CPU 自旋等待
//!
//! ## SAFETY 不变式
//!
//! - 进入 KGDB 前必须关闭中断 (cli / msr daifset)
//! - 串口 read/write 在轮询模式, 不允许睡眠

use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use super::ftrace;

/// 串口接口 (caller 实现)
pub trait KgdbSerial: Sync {
    /// 非阻塞读一个字节
    fn try_getchar(&self) -> Option<u8>;
    /// 阻塞写一个字节
    fn putchar(&self, c: u8);
}

// 因为 trait 对象在 no_std 静态初始化受限, 改用全局 *const () + 读写函数指针。
// 读写函数使用 generic fn (monomorphization 在调用点), 调用方需把函数指针
// 转型后存入 AtomicPtr。

// SAFETY: 函数指针由 kgdb_set_serial 在 caller 端通过 monomorphization 注入,
// 仅当 T: KgdbSerial + 'static 时调用, 不存在悬垂指针
type TryGetcFn = unsafe fn(*const ()) -> Option<u8>;
// SAFETY: 函数指针由 kgdb_set_serial 在 caller 端通过 monomorphization 注入,
// 仅当 T: KgdbSerial + 'static 时调用, 不存在悬垂指针
type PutcFn = unsafe fn(*const (), u8);

static SERIAL: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static TRY_GETC: AtomicPtr<TryGetcFn> = AtomicPtr::new(core::ptr::null_mut());
static PUTC: AtomicPtr<PutcFn> = AtomicPtr::new(core::ptr::null_mut());

#[inline]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
fn read_dispatch<T: KgdbSerial>(p: *const ()) -> Option<u8> {
    // SAFETY: SERIAL 由 kgdb_set_serial 注入, T 满足 trait 约束
    unsafe { (*(p as *const T)).try_getchar() }
}

#[inline]
fn write_dispatch<T: KgdbSerial>(p: *const (), c: u8) {
    // SAFETY: 同上
    unsafe { (*(p as *const T)).putchar(c) }
}

/// 注册串口
///
/// # Safety
///
/// 调用方必须保证 `serial` 指向的对象生命周期为 'static, 且 trait 实现
/// 满足轮询读写语义。
// SAFETY: 见上 # Safety 段. 调用方必须保证 serial 指向 'static 对象且 trait 满足轮询读写语义.
pub unsafe fn kgdb_set_serial<T: KgdbSerial>(serial: &'static T) {
    let data = core::ptr::from_ref::<T>(serial).cast::<()>().cast_mut();
    SERIAL.store(data, Ordering::Release);
    let try_getc: TryGetcFn = read_dispatch::<T>;
    let putc: PutcFn = write_dispatch::<T>;
    TRY_GETC.store(try_getc as *mut _, Ordering::Release);
    PUTC.store(putc as *mut _, Ordering::Release);
}

/// 是否已注册串口
pub fn kgdb_serial_ready() -> bool {
    !SERIAL.load(Ordering::Acquire).is_null()
}

/// 阻塞写一字节
pub fn kgdb_putc(c: u8) {
    let data = SERIAL.load(Ordering::Acquire);
    let f = PUTC.load(Ordering::Acquire);
    if data.is_null() || f.is_null() {
        return;
    }
    // SAFETY: serial 对象生命周期 'static, f 来自 kgdb_set_serial 注册
    unsafe {
        (*f)(data, c);
    }
}

/// 非阻塞读一字节
pub fn kgdb_try_getc() -> Option<u8> {
    let data = SERIAL.load(Ordering::Acquire);
    let f = TRY_GETC.load(Ordering::Acquire);
    if data.is_null() || f.is_null() {
        return None;
    }
    // SAFETY: 同 kgdb_putc
    unsafe { (*f)(data) }
}

/// 写一段字符串
pub fn kgdb_write_str(s: &str) {
    for &b in s.as_bytes() {
        kgdb_putc(b);
    }
}

/// 计算 checksum
fn pkt_checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// 发送一个 packet (含 ACK 等待)
pub fn kgdb_send_packet(payload: &[u8]) {
    kgdb_putc(b'$');
    for &b in payload {
        kgdb_putc(b);
    }
    kgdb_putc(b'#');
    let c = pkt_checksum(payload);
    let hi = b"0123456789abcdef"[(c >> 4) as usize];
    let lo = b"0123456789abcdef"[(c & 0xf) as usize];
    kgdb_putc(hi);
    kgdb_putc(lo);
    // 等待 + / - 应答 (简化: 阻塞读)
    for _ in 0..1000 {
        if let Some(c) = kgdb_try_getc() {
            if c == b'+' {
                return;
            }
            if c == b'-' {
                // 重发
                kgdb_putc(b'$');
                for &b in payload {
                    kgdb_putc(b);
                }
                kgdb_putc(b'#');
                kgdb_putc(hi);
                kgdb_putc(lo);
                continue;
            }
        }
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 接收一个 packet, payload 写入 out, 返回有效字节数
pub fn kgdb_recv_packet(out: &mut [u8]) -> usize {
    let mut state = 0u8;
    let mut body_len = 0usize;
    let mut checksum_hi = 0u8;
    let mut attempts = 0u32;
    while attempts < 1_000_000 {
        attempts += 1;
        let c = match kgdb_try_getc() {
            Some(c) => c,
            None => continue,
        };
        match state {
            0 => {
                if c == b'$' {
                    state = 1;
                }
            }
            1 => {
                if c == b'#' {
                    state = 2;
                } else if body_len < out.len() {
                    out[body_len] = c;
                    body_len += 1;
                }
            }
            2 => {
                checksum_hi = c;
                state = 3;
            }
            3 => {
                let checksum_lo = c;
                let csum = pkt_checksum(&out[..body_len]);
                let expect_hi = b"0123456789abcdef"[(csum >> 4) as usize];
                let expect_lo = b"0123456789abcdef"[(csum & 0xf) as usize];
                if expect_hi == checksum_hi && expect_lo == checksum_lo {
                    kgdb_putc(b'+');
                    return body_len;
                }
                kgdb_putc(b'-');
                return 0;
            }
            _ => break,
        }
    }
    0
}

/// 处理器状态 (`x86_64`)
#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KgdbRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub eflags: u64,
}

/// 处理器状态 (aarch64)
#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KgdbRegs {
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,
    pub x29: u64,
    pub x30: u64,
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

/// KGDB 是否进入调试循环
static IN_KGDB: AtomicBool = AtomicBool::new(false);

/// 是否在 KGDB 中
pub fn kgdb_active() -> bool {
    IN_KGDB.load(Ordering::Acquire)
}

fn format_stop(out: &mut [u8]) -> usize {
    let s = b"T05thread:0;";
    let n = s.len().min(out.len());
    out[..n].copy_from_slice(&s[..n]);
    n
}

fn format_registers(out: &mut [u8], r: &KgdbRegs) -> usize {
    use core::fmt::Write;
    struct W<'a>(Option<&'a mut [u8]>);
    impl Write for W<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            // take 暂时取走 self.0 以避免重借用与 outlive 推断冲突
            let slice = self.0.take().expect("W invariant: 1 slice at a time");
            let bytes = s.as_bytes();
            let n = bytes.len().min(slice.len());
            let (head, rest) = slice.split_at_mut(n);
            head.copy_from_slice(&bytes[..n]);
            self.0 = Some(rest);
            Ok(())
        }
    }
    let mut w = W(Some(out));
    let orig_len = w.0.as_ref().map_or(0, |s| s.len());
    #[cfg(target_arch = "x86_64")]
    let _ = write!(
        w,
        "{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}",
        r.rax,
        r.rbx,
        r.rcx,
        r.rdx,
        r.rsi,
        r.rdi,
        r.rbp,
        r.rsp,
        r.r8,
        r.r9,
        r.r10,
        r.r11,
        r.r12,
        r.r13,
        r.r14,
        r.r15,
        r.rip,
        r.eflags
    );
    #[cfg(target_arch = "aarch64")]
    let _ = write!(
        w,
        "{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}{:016x}",
        r.x0,
        r.x1,
        r.x2,
        r.x3,
        r.x4,
        r.x5,
        r.x6,
        r.x7,
        r.x8,
        r.x9,
        r.x10,
        r.x11,
        r.x12,
        r.x13,
        r.x14,
        r.x15,
        r.x16,
        r.x17,
        r.x18,
        r.x19,
        r.x20,
        r.x21,
        r.x22,
        r.x23,
        r.x24,
        r.x25,
        r.x26,
        r.x27,
        r.x28,
        r.x29,
        r.x30,
        r.sp,
        r.pc
    );
    orig_len - w.0.expect("W invariant: 1 slice at a time").len()
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
fn parse_registers(hex: &[u8], r: &mut KgdbRegs) -> bool {
    #[cfg(target_arch = "x86_64")]
    const N: usize = 18;
    #[cfg(target_arch = "aarch64")]
    const N: usize = 33;
    if hex.len() < N * 16 {
        return false;
    }
    for i in 0..N {
        let mut v: u64 = 0;
        if !parse_hex(&hex[i * 16..(i + 1) * 16], &mut v) {
            return false;
        }
        // SAFETY: KgdbRegs 是 POD, 按 [u64; N] 视图读写
        let fields = unsafe { core::slice::from_raw_parts_mut(r as *mut _ as *mut u64, N) };
        fields[i] = v;
    }
    true
}

fn parse_hex(s: &[u8], out: &mut u64) -> bool {
    if s.len() != 16 {
        return false;
    }
    let mut v: u64 = 0;
    for &b in s {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return false,
        };
        v = (v << 4) | u64::from(d);
    }
    *out = v;
    true
}

// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
fn handle_mem_read(arg: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut comma = 0;
    while comma < arg.len() && arg[comma] != b',' {
        comma += 1;
    }
    if comma >= arg.len() {
        return None;
    }
    let mut addr: u64 = 0;
    if !parse_hex(&arg[..comma], &mut addr) {
        return None;
    }
    let mut len: u64 = 0;
    if !parse_hex(&arg[comma + 1..], &mut len) {
        return None;
    }
    if len > 128 || out.len() < (len as usize) * 2 {
        return None;
    }
    let mut idx = 0;
    for i in 0..len {
        // SAFETY: 调用方须保证 `addr + i` 指向可读物理/虚拟内存
        let byte = unsafe { core::ptr::read_volatile((addr + i) as *const u8) };
        out[idx] = b"0123456789abcdef"[(byte >> 4) as usize];
        out[idx + 1] = b"0123456789abcdef"[(byte & 0xf) as usize];
        idx += 2;
    }
    Some(idx)
}

// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
fn handle_mem_write(arg: &[u8]) -> bool {
    let mut colon = 0;
    while colon < arg.len() && arg[colon] != b':' {
        colon += 1;
    }
    if colon >= arg.len() {
        return false;
    }
    let head = &arg[..colon];
    let data = &arg[colon + 1..];
    let mut comma = 0;
    while comma < head.len() && head[comma] != b',' {
        comma += 1;
    }
    if comma >= head.len() {
        return false;
    }
    let mut addr: u64 = 0;
    if !parse_hex(&head[..comma], &mut addr) {
        return false;
    }
    let mut len: u64 = 0;
    if !parse_hex(&head[comma + 1..], &mut len) {
        return false;
    }
    if data.len() < (len as usize) * 2 {
        return false;
    }
    for i in 0..len {
        let hi = match data[(i * 2) as usize] {
            b'0'..=b'9' => data[(i * 2) as usize] - b'0',
            b'a'..=b'f' => data[(i * 2) as usize] - b'a' + 10,
            b'A'..=b'F' => data[(i * 2) as usize] - b'A' + 10,
            _ => return false,
        };
        let lo = match data[(i * 2 + 1) as usize] {
            b'0'..=b'9' => data[(i * 2 + 1) as usize] - b'0',
            b'a'..=b'f' => data[(i * 2 + 1) as usize] - b'a' + 10,
            b'A'..=b'F' => data[(i * 2 + 1) as usize] - b'A' + 10,
            _ => return false,
        };
        let byte = (hi << 4) | lo;
        // SAFETY: 调用方须保证 `addr` 指向 len 字节的可写物理/虚拟内存
        unsafe { core::ptr::write_volatile((addr + i) as *mut u8, byte) };
    }
    true
}

/// 进入 KGDB 主循环
pub fn kgdb_loop(regs: &mut KgdbRegs) {
    if IN_KGDB.swap(true, Ordering::AcqRel) {
        // 已在 KGDB 中 (NMI 重入), 自旋等待
        loop {
            core::hint::spin_loop();
        }
    }

    ftrace::record_named(ftrace::fnv1a_32(b"kgdb_enter"), 0, 0, 0, 0);

    let mut stop = [0u8; 64];
    let n = format_stop(&mut stop);
    kgdb_send_packet(&stop[..n]);

    let mut pkt = [0u8; 256];
    let mut reply = [0u8; 512];
    loop {
        let n = kgdb_recv_packet(&mut pkt);
        if n == 0 {
            continue;
        }
        match pkt[0] {
            b'?' => {
                let m = format_stop(&mut stop);
                kgdb_send_packet(&stop[..m]);
            }
            b'g' => {
                let m = format_registers(&mut reply, regs);
                kgdb_send_packet(&reply[..m]);
            }
            b'G' => {
                if parse_registers(&pkt[1..n], regs) {
                    kgdb_send_packet(b"OK");
                } else {
                    kgdb_send_packet(b"E00");
                }
            }
            b'm' => {
                if let Some(m) = handle_mem_read(&pkt[1..n], &mut reply) {
                    kgdb_send_packet(&reply[..m]);
                } else {
                    kgdb_send_packet(b"E00");
                }
            }
            b'M' => {
                if handle_mem_write(&pkt[1..n]) {
                    kgdb_send_packet(b"OK");
                } else {
                    kgdb_send_packet(b"E00");
                }
            }
            b'c' | b's' => {
                kgdb_send_packet(b"S05");
                IN_KGDB.store(false, Ordering::Release);
                return;
            }
            b'k' => {
                IN_KGDB.store(false, Ordering::Release);
                return;
            }
            b'Z' | b'z' => {
                kgdb_send_packet(b"OK");
            }
            _ => {
                kgdb_send_packet(b"");
            }
        }
    }
}

/// 主动断点入口 (在 panic 路径调用)
pub fn kgdb_breakpoint(regs: &mut KgdbRegs) {
    kgdb_loop(regs);
}

/// 异常处理钩子 (IDT exception handler 调用)
pub fn kgdb_handle_exception(regs: &mut KgdbRegs) {
    kgdb_loop(regs);
}
