//! `Edgine` 内核日志系统 (`KLog`)
//!
//! 自举设计 — 零外部依赖:
//!   1. 内建 COM1 串口驱动 (直接 port I/O, 无需 driver 子系统)
//!   2. 128KB 环形缓冲区 (Atomic + Mutex 保护)
//!   3. 级别/分类双重过滤
//!   4. RDTSC 时间戳
//!
//! 所有内核模块通过 klog_* FFI 统一输出。

use core::sync::atomic::{AtomicU8, Ordering};

// ============================================================================
// 格式化宏 — 统一输出格式: [时间戳][级别][分类] 消息
//
// 输出格式: <ts_s>.<ts_us> [LEVEL] [CATEGORY] message\n
//
// 使用方式:
//   klog!(Info, Boot, "Edgine 启动中...");
//   klog_warn!(Kernel, "告警信息");
//   klog_err!(Driver, "驱动错误: {}", code);
// ============================================================================

pub const KLOG_BUF: usize = 256;

pub struct KlogWriter {
    buf: [u8; KLOG_BUF],
    pos: usize,
}
impl KlogWriter {
    pub const fn new() -> Self {
        Self {
            buf: [0; KLOG_BUF],
            pos: 0,
        }
    }
}

impl core::fmt::Write for KlogWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let len = bytes.len().min(self.buf.len() - self.pos);
        self.buf[self.pos..self.pos + len].copy_from_slice(&bytes[..len]);
        self.pos += len;
        Ok(())
    }
}

impl KlogWriter {
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.pos]
    }
}

pub struct CursorWriter<'a> {
    buf: &'a mut [u8],
    cursor: &'a mut usize,
}

impl<'a> CursorWriter<'a> {
    pub fn new(buf: &'a mut [u8], cursor: &'a mut usize) -> Self {
        Self { buf, cursor }
    }
}

impl core::fmt::Write for CursorWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len() - *self.cursor;
        let to_write = bytes.len().min(remaining);
        self.buf[*self.cursor..*self.cursor + to_write].copy_from_slice(&bytes[..to_write]);
        *self.cursor += to_write;
        Ok(())
    }
}

/// 按 C ABI sink 符号写出日志的兼容宏 — 转发至安全入口 [`log`]。
///
/// 历史实现于宏内展开 `unsafe extern "C"` 声明 + `unsafe { ... }` 调用, 当宏在
/// `unsafe fn` 体内 (如 GIC / GDT 初始化) 展开时, 该 unsafe 块会被判为「嵌套冗余」
/// (`unused_unsafe`), 而整体删除又会破坏非嵌套展开点 —— 故改由安全入口统一落地。
/// `klog_ffi_info` / `klog_ffi_warn` 自身仍作为 C ABI 入口保留 (供 FFI 调用方直接使用)。
#[macro_export]
macro_rules! klog_ffi {
    (klog_ffi_info, $($arg:tt)*) => {
        $crate::framework::klog::log(
            $crate::framework::klog::LogLevel::Info,
            $crate::framework::klog::LogCategory::Kernel,
            format_args!($($arg)*),
        )
    };
    (klog_ffi_warn, $($arg:tt)*) => {
        $crate::framework::klog::log(
            $crate::framework::klog::LogLevel::Warn,
            $crate::framework::klog::LogCategory::Kernel,
            format_args!($($arg)*),
        )
    };
    (klog_ffi_error, $($arg:tt)*) => {
        $crate::framework::klog::log(
            $crate::framework::klog::LogLevel::Error,
            $crate::framework::klog::LogCategory::Kernel,
            format_args!($($arg)*),
        )
    };
}

/// 通用格式化日志宏 — 单入口，所有模块共用
#[macro_export]
macro_rules! klog_fmt {
    ($lvl:ident, $cat:ident, $($arg:tt)*) => {
        $crate::framework::klog::log_filtered(
            $crate::framework::klog::LogLevel::$lvl,
            $crate::framework::klog::LogCategory::$cat,
            format_args!($($arg)*),
        )
    };
}

/// 便捷宏: 按级别
#[macro_export]
macro_rules! klog_info  { ($cat:ident, $($arg:tt)*) => { $crate::klog_fmt!(Info,  $cat, $($arg)*) }; }
#[macro_export]
macro_rules! klog_warn  { ($cat:ident, $($arg:tt)*) => { $crate::klog_fmt!(Warn,  $cat, $($arg)*) }; }
#[macro_export]
macro_rules! klog_err   { ($cat:ident, $($arg:tt)*) => { $crate::klog_fmt!(Error, $cat, $($arg)*) }; }
#[macro_export]
macro_rules! klog_debug { ($cat:ident, $($arg:tt)*) => { $crate::klog_fmt!(Debug, $cat, $($arg)*) }; }
#[macro_export]
macro_rules! klog_crit  { ($cat:ident, $($arg:tt)*) => { $crate::klog_fmt!(Crit,  $cat, $($arg)*) }; }

/// 便捷宏: 按类别+级别 (常用组合)
#[macro_export]
macro_rules! klog_boot_info  { ($($arg:tt)*) => { $crate::klog_info!(Boot, $($arg)*) }; }
#[macro_export]
macro_rules! klog_kern_warn { ($($arg:tt)*) => { $crate::klog_warn!(Kernel, $($arg)*) }; }
#[macro_export]
macro_rules! klog_kern_err  { ($($arg:tt)*) => { $crate::klog_err!(Kernel, $($arg)*) }; }
#[macro_export]
macro_rules! klog_drv_warn  { ($($arg:tt)*) => { $crate::klog_warn!(Driver, $($arg)*) }; }
#[macro_export]
macro_rules! klog_drv_err   { ($($arg:tt)*) => { $crate::klog_err!(Driver, $($arg)*) }; }

#[macro_export]
macro_rules! klog_error { ($($arg:tt)*) => { $crate::klog_err!(Kernel, $($arg)*) }; }
#[macro_export]
macro_rules! klog_slab  { ($($arg:tt)*) => { $crate::klog_info!(Memory, $($arg)*) }; }
#[macro_export]
macro_rules! klog_info_simple { ($($arg:tt)*) => { $crate::klog_info!(Kernel, $($arg)*) }; }

// ============================================================================
// 端口 I/O 原语 (无需 driver 框架)
// ============================================================================

#[cfg(target_arch = "x86_64")]
mod serial_impl {
    const COM1: u16 = 0x3F8;

    #[inline(always)]
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    unsafe fn port_outb(port: u16, value: u8) {
        crate::arch!(outb(port, value));
    }

    #[inline(always)]
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe fn port_inb(port: u16) -> u8 {
        crate::arch!(inb(port))
    }

    pub fn serial_init() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            port_outb(COM1 + 1, 0x00);
            port_outb(COM1 + 3, 0x80);
            port_outb(COM1, 0x03);
            port_outb(COM1 + 1, 0x00);
            port_outb(COM1 + 3, 0x03);
            port_outb(COM1 + 2, 0xC7);
            port_outb(COM1 + 4, 0x0B);
        }
    }

    pub fn serial_putc(c: u8) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            while (port_inb(COM1 + 5) & 0x20) == 0 {
                core::hint::spin_loop();
            }
            port_outb(COM1, c);
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod serial_impl {
    use crate::framework::arch::uart;

    pub fn serial_init() {
        // UART 已在 entry.rs 中初始化
    }

    pub fn serial_putc(c: u8) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            uart::putc(c);
        }
    }
}

fn serial_putc_chained(c: u8) {
    serial_impl::serial_putc(c);
}

fn serial_newline() {
    serial_impl::serial_putc(b'\r');
    serial_impl::serial_putc(b'\n');
}

pub fn serial_write_bytes(data: &[u8]) {
    for &byte in data {
        if byte == b'\n' {
            serial_newline();
        } else {
            serial_putc_chained(byte);
        }
    }
}

// ============================================================================
// TD-09: LogSink 抽象 — 支持多个输出后端 (串口/网络/块设备)
// ============================================================================

/// 日志输出后端 trait.
///
/// 所有 sink 必须实现: 名称 (调试用) + 字节写入 + 字符串写入.
/// 字符串写入的默认实现是循环调用 `putc`, 多数后端可以重写以提高效率.
pub trait LogSink: Sync {
    /// 后端名, 用于 `/proc/sys/klog/sinks` 等接口.
    fn name(&self) -> &'static str;
    /// 写入单字节.
    fn putc(&self, c: u8);
    /// 写入字符串 (默认逐字节).
    fn write_str(&self, s: &str) {
        for &b in s.as_bytes() {
            self.putc(b);
        }
    }
    /// 写入字节流 (默认转 str 走 `write_str`; sink 可重写以避免 UTF-8 校验).
    fn write_bytes(&self, b: &[u8]) {
        if let Ok(s) = core::str::from_utf8(b) {
            self.write_str(s);
        }
    }
}

/// 串口 sink — COM1 / aarch64 UART, 框架自带默认 sink.
pub struct SerialSink;

impl LogSink for SerialSink {
    fn name(&self) -> &'static str {
        "serial"
    }
    fn putc(&self, c: u8) {
        serial_putc_chained(c);
    }
    fn write_str(&self, s: &str) {
        serial_write_bytes(s.as_bytes());
    }
    fn write_bytes(&self, b: &[u8]) {
        serial_write_bytes(b);
    }
}

/// TD-09: 全局 sink 注册表 (静态数组, 不依赖分配器).
///
/// 容量固定为 4, 超出时返回 None. 单核模型下无锁 (klog 路径在串口自旋保护).
/// 增删接口在 services 层通过 `/proc/sys/klog/sinks` 暴露.
pub const MAX_LOG_SINKS: usize = 4;

// `*const dyn LogSink` 是宽指针 (含 vtable), 不能用 `core::ptr::null` 在 const 上下文构造.
// 改用 `usize` 薄指针存储, 0 表示未注册, 重建时 union-cast 回去.
#[repr(C)]
#[derive(Copy, Clone)]
union SinkPtr {
    raw: usize,
    fat: *const dyn LogSink,
}
const fn null_sink_ptr() -> SinkPtr {
    SinkPtr { raw: 0 }
}
const fn make_null_sinks() -> [SinkPtr; MAX_LOG_SINKS] {
    [
        null_sink_ptr(),
        null_sink_ptr(),
        null_sink_ptr(),
        null_sink_ptr(),
    ]
}
// SAFETY: SinkPtr 的 raw/fat 共享存储, 注册时写入 fat 有效; 日志路径由调用方序列化.
unsafe impl Send for SinkPtr {}
// SAFETY: 同上, 日志路径由调用方序列化, 跨线程访问安全.
unsafe impl Sync for SinkPtr {}
static LOG_SINKS: crate::framework::sync::IrqSpinLock<[SinkPtr; MAX_LOG_SINKS]> =
    crate::framework::sync::IrqSpinLock::new(make_null_sinks());
static LOG_SINK_COUNT: AtomicU8 = AtomicU8::new(0);

/// 注册 sink, 失败返回 `None` (已满).
///
/// # Safety
/// `sink` 必须是 `'static` (其内部任何状态都不可被释放).
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
pub unsafe fn klog_register_sink(sink: &'static dyn LogSink) -> Option<usize> {
    let idx = LOG_SINK_COUNT.load(Ordering::SeqCst) as usize;
    if idx >= MAX_LOG_SINKS {
        return None;
    }
    let ptr = sink as *const dyn LogSink;
    let mut sinks = LOG_SINKS.lock();
    sinks[idx] = SinkPtr { fat: ptr };
    drop(sinks);
    LOG_SINK_COUNT.store((idx + 1) as u8, Ordering::SeqCst);
    Some(idx)
}

/// 列出已注册 sink 数量.
pub fn klog_sink_count() -> usize {
    LOG_SINK_COUNT.load(Ordering::SeqCst) as usize
}

/// 取出 idx 处的 sink 引用 (idx < count 时必非空).
///
/// # Safety
/// 调用方必须保证 `idx < klog_sink_count()` 且 sink 仍 `'static` 有效.
pub unsafe fn klog_sink_at(idx: usize) -> Option<&'static dyn LogSink> {
    let sinks = LOG_SINKS.lock();
    let entry = sinks[idx];
    drop(sinks);
    // SAFETY: SinkPtr 是 union, raw/fat 共享存储, 注册时写入 fat 有效.
    let fat = unsafe { entry.fat };
    if fat.is_null() {
        None
    } else {
        // SAFETY: 注册时保证非空且 'static.
        Some(unsafe { &*fat })
    }
}

/// 取出 idx 处的 sink 名称. 越界返回 `None`.
///
/// 名称所有权属于 sink 实现 (`'static str`). 名称存放在 sink 自身的 vtable
/// 数据中, 只要 sink 仍注册, 名称引用就一直有效.
///
/// 这是一个**安全** API: 不需要 unsafe 块, 调用方在 services 层可放心使用.
pub fn klog_sink_name_at(idx: usize) -> Option<&'static str> {
    let n = LOG_SINK_COUNT.load(Ordering::SeqCst) as usize;
    if idx >= n {
        return None;
    }
    // SAFETY: idx < n, 满足 klog_sink_at 的契约; 仅取 name() 后立即丢弃 sink 引用.
    let name = unsafe { klog_sink_at(idx) }.map(LogSink::name);
    name
}

/// 广播字符串到所有已注册 sink. 空注册表时直接返回.
pub fn klog_broadcast(s: &str) {
    let n = klog_sink_count();
    for i in 0..n {
        // SAFETY: i < n 即 idx < count, 满足 klog_sink_at 的契约.
        if let Some(sink) = unsafe { klog_sink_at(i) } {
            sink.write_str(s);
        }
    }
}

/// 广播字节流到所有已注册 sink.
pub fn klog_broadcast_bytes(b: &[u8]) {
    let n = klog_sink_count();
    for i in 0..n {
        // SAFETY: i < n 即 idx < count, 满足 klog_sink_at 的契约.
        if let Some(sink) = unsafe { klog_sink_at(i) } {
            sink.write_bytes(b);
        }
    }
}

/// 启动时注册默认 sink (serial). 由 kernel 主入口调用.
pub fn klog_register_defaults() {
    static SERIAL: SerialSink = SerialSink;
    // SAFETY: SERIAL 是 'static.
    let _ = unsafe { klog_register_sink(&SERIAL) };
}

// ============================================================================
// 环形缓冲区
// ============================================================================
//
// B08-14: 环形缓冲区仅裸机日志路径 (klog_output_baremetal) 使用, host-test 下
// klog no-op, 整体 cfg 掉消除 dead_code 警告.

#[cfg(not(feature = "host-test"))]
const RING_SIZE: usize = 128 * 1024;

#[cfg(not(feature = "host-test"))]
struct RingBuf {
    data: [u8; RING_SIZE],
    head: usize,
    tail: usize,
    total: u64,
}

#[cfg(not(feature = "host-test"))]
impl RingBuf {
    #[expect(
        clippy::large_stack_arrays,
        reason = "large_stack_arrays: 大栈数组是性能权衡 (避免堆分配); 当前优先 expect"
    )]
    const fn new() -> Self {
        Self {
            data: [0; RING_SIZE],
            head: 0,
            tail: 0,
            total: 0,
        }
    }

    fn push(&mut self, b: u8) {
        self.data[self.tail] = b;
        self.tail = (self.tail + 1) % RING_SIZE;
        if self.tail == self.head {
            self.head = (self.head + 1) % RING_SIZE;
        }
        self.total = self.total.wrapping_add(1);
    }

    fn push_str(&mut self, s: &[u8]) {
        for &b in s {
            self.push(b);
        }
    }
}

#[cfg(not(feature = "host-test"))]
struct RingLock {
    inner: core::cell::UnsafeCell<RingBuf>,
}
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
#[cfg(not(feature = "host-test"))]
unsafe impl Sync for RingLock {}

#[cfg(not(feature = "host-test"))]
static RING: RingLock = RingLock {
    inner: core::cell::UnsafeCell::new(RingBuf::new()),
};

// ============================================================================
// 全局状态
// ============================================================================

static MIN_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);
pub static KLOG_INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// ============================================================================
// 日志级别 / 分类
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Note = 2,
    Warn = 3,
    Error = 4,
    Crit = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(clippy::upper_case_acronyms)] // IPC 子系统名
pub enum LogCategory {
    Boot = 0,
    Kernel = 1,
    Memory = 2,
    Process = 3,
    FS = 4,
    Net = 5,
    Driver = 6,
    Syscall = 7,
    IPC = 8,
    Security = 9,
    Test = 10,
    Sync = 11,
    Swap = 12,
    Acpi = 13,
    Timer = 14,
}

// B08-14: prefix/name 仅裸机日志路径 (klog_output_baremetal) 使用, host-test 下 cfg 掉.
#[cfg(not(feature = "host-test"))]
impl LogLevel {
    fn prefix(&self) -> &[u8] {
        match self {
            Self::Debug => b"[DBG] ",
            Self::Info => b"[INFO]",
            Self::Note => b"[NOTE]",
            Self::Warn => b"[WARN]",
            Self::Error => b"[ERR] ",
            Self::Crit => b"[CRIT]",
        }
    }
}

#[cfg(not(feature = "host-test"))]
impl LogCategory {
    fn name(&self) -> &[u8] {
        match self {
            Self::Boot => b"BOOT",
            Self::Kernel => b"KERN",
            Self::Memory => b"MEM",
            Self::Process => b"PROC",
            Self::FS => b"FS",
            Self::Net => b"NET",
            Self::Driver => b"DRV",
            Self::Syscall => b"SYSCALL",
            Self::IPC => b"IPC",
            Self::Security => b"SEC",
            Self::Test => b"TEST",
            Self::Sync => b"SYNC",
            Self::Swap => b"SWAP",
            Self::Acpi => b"ACPI",
            Self::Timer => b"TIMER",
        }
    }
}

// ============================================================================
// 辅助
// ============================================================================

// SAFETY: `const` 由调用方保证为有效指针; 只读访问
unsafe fn cstr_slice(ptr: *const u8) -> &'static [u8] {
    unsafe {
        if ptr.is_null() {
            return b"(null)";
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
            if len > 1024 {
                return b"(truncated)";
            }
        }
        core::slice::from_raw_parts(ptr, len)
    }
}

// B08-14: 仅裸机路径使用 (klog_output_baremetal), host-test 下 cfg 掉消除 dead_code 警告.
#[cfg(not(feature = "host-test"))]
fn rdtsc() -> u64 {
    crate::arch!(timestamp())
}

/// 格式化 u64 → 栈上 buffer (最小化依赖)
#[cfg(not(feature = "host-test"))]
fn format_ts(buf: &mut [u8; 32], tsc: u64) -> &[u8] {
    // 假设 ~3GHz TSC, 显示秒.微秒
    let total_us = tsc / 3000;
    let sec = total_us / 1_000_000;
    let us = total_us % 1_000_000;
    let mut pos = 0usize;
    // 写 sec
    let s = sec;
    if s == 0 {
        buf[pos] = b'0';
        pos += 1;
    } else {
        let mut tmp = [0u8; 20];
        let mut n = 0;
        let mut v = s;
        while v > 0 {
            tmp[n] = (v % 10) as u8 + b'0';
            n += 1;
            v /= 10;
        }
        while n > 0 {
            n -= 1;
            buf[pos] = tmp[n];
            pos += 1;
        }
    }
    buf[pos] = b'.';
    pos += 1;
    // 写 us (6 位补零)
    for i in (0..6).rev() {
        let digit = (us / 10u64.pow(i)) % 10;
        buf[pos] = digit as u8 + b'0';
        pos += 1;
    }
    buf[pos] = b' ';
    pos += 1;
    &buf[..pos]
}

// ============================================================================
// 核心输出
// ============================================================================

fn klog_output(level: LogLevel, cat: LogCategory, msg: &[u8]) {
    // B08-14 前置: host-test (std) 下 klog no-op — rdtsc/cli/MMIO sink 为裸机专属
    // (host 用户态执行特权指令 SIGSEGV). 日志功能在 host 测试非被测对象, 丢弃即可.
    #[cfg(feature = "host-test")]
    {
        let _ = (level, cat, msg);
    }
    #[cfg(not(feature = "host-test"))]
    {
        klog_output_baremetal(level, cat, msg);
    }
}

#[cfg(not(feature = "host-test"))]
fn klog_output_baremetal(level: LogLevel, cat: LogCategory, msg: &[u8]) {
    let tsc = rdtsc();
    let mut ts_buf = [0u8; 32];
    let ts = format_ts(&mut ts_buf, tsc);

    let saved_if = crate::arch!(interrupt_disable());

    // TD-09: 走 sink 抽象 (注册时已默认含 serial).
    klog_broadcast_bytes(ts);
    klog_broadcast_bytes(level.prefix());
    klog_broadcast_bytes(b" ");
    klog_broadcast_bytes(b"[");
    klog_broadcast_bytes(cat.name());
    klog_broadcast_bytes(b"] ");
    klog_broadcast_bytes(msg);
    // 换行: serial sink 已实现 \n → \r\n; 其他 sink 若需特殊处理, 可重写 write_bytes.
    klog_broadcast_bytes(b"\n");

    crate::arch!(interrupt_restore(saved_if));

    // SAFETY: `RING` 由调用方保证为有效指针; 只读访问
    let ring = unsafe { &mut *RING.inner.get() };
    ring.push_str(ts);
    ring.push_str(level.prefix());
    ring.push(b' ');
    ring.push(b'[');
    ring.push_str(cat.name());
    ring.push(b']');
    ring.push(b' ');
    ring.push_str(msg);
    ring.push(b'\n');

    crate::framework::console::gfx_console_write(msg);
}

// ============================================================================
// 公共 API
// ============================================================================

/// Safe 日志入口 — 供 services 层调用, 无 unsafe 展开。
///
/// 将 `format_args!` 格式化到栈缓冲区后调用 `klog_output`,
/// 整个调用链对调用方完全 safe。
pub fn log(level: LogLevel, cat: LogCategory, args: core::fmt::Arguments<'_>) {
    let mut w = KlogWriter::new();
    let _ = core::fmt::Write::write_fmt(&mut w, args);
    klog_output(level, cat, w.as_slice());
}

/// 带级别过滤的 Safe 日志入口 — 供 `klog_fmt!` 宏 (即 `klog_info!` 等) 使用。
///
/// 与 [`log`] 的差异是保留 `MIN_LEVEL` 级别过滤 (与 C ABI 入口 `klog_write` 同口径),
/// 使 `klog_debug!` 等低级别宏在默认级别下静默; 过滤发生在格式化之前。
pub fn log_filtered(level: LogLevel, cat: LogCategory, args: core::fmt::Arguments<'_>) {
    if (level as u8) < MIN_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    log(level, cat, args);
}

/// Safe 便捷函数: Info 级别
pub fn log_info(cat: LogCategory, args: core::fmt::Arguments<'_>) {
    log(LogLevel::Info, cat, args);
}

/// Safe 便捷函数: Warn 级别
pub fn log_warn(cat: LogCategory, args: core::fmt::Arguments<'_>) {
    log(LogLevel::Warn, cat, args);
}

/// Safe 便捷函数: Error 级别
pub fn log_err(cat: LogCategory, args: core::fmt::Arguments<'_>) {
    log(LogLevel::Error, cat, args);
}

/// Safe 便捷函数: Debug 级别
pub fn log_debug(cat: LogCategory, args: core::fmt::Arguments<'_>) {
    log(LogLevel::Debug, cat, args);
}

/// Safe 便捷函数: Crit 级别
pub fn log_crit(cat: LogCategory, args: core::fmt::Arguments<'_>) {
    log(LogLevel::Crit, cat, args);
}

pub fn klog_set_level(level: LogLevel) {
    MIN_LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn klog_get_level() -> LogLevel {
    match MIN_LEVEL.load(Ordering::Relaxed) {
        0 => LogLevel::Debug,
        1 => LogLevel::Info,
        2 => LogLevel::Note,
        3 => LogLevel::Warn,
        4 => LogLevel::Error,
        _ => LogLevel::Crit,
    }
}

// ============================================================================
// FFI 桩 → 全部实现在此
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// 在任何日志记录前必须且只能调用一次. 调用方保证串口硬件就绪.
pub unsafe extern "C" fn klog_init() {
    serial_impl::serial_init();
    // 注册默认 serial sink, 否则 klog_broadcast_bytes 无 sink 可写
    klog_register_defaults();
    KLOG_INIT.store(true, Ordering::Release);
    klog_output(LogLevel::Info, LogCategory::Boot, b"KLog initialized");
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_write(
    level: u8,
    cat: u8,
    _file: *const u8,
    _func: *const u8,
    _line: u32,
    fmt: *const u8,
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
) -> i32 {
    unsafe {
        if fmt.is_null() {
            return -1;
        }

        let lvl = match level {
            0 => LogLevel::Debug,
            1 => LogLevel::Info,
            2 => LogLevel::Note,
            3 => LogLevel::Warn,
            4 => LogLevel::Error,
            5 => LogLevel::Crit,
            _ => return -1,
        };

        let category = match cat {
            0 => LogCategory::Boot,
            1 => LogCategory::Kernel,
            2 => LogCategory::Memory,
            3 => LogCategory::Process,
            4 => LogCategory::FS,
            5 => LogCategory::Net,
            6 => LogCategory::Driver,
            7 => LogCategory::Syscall,
            8 => LogCategory::IPC,
            9 => LogCategory::Security,
            10 => LogCategory::Test,
            11 => LogCategory::Sync,
            12 => LogCategory::Swap,
            13 => LogCategory::Acpi,
            14 => LogCategory::Timer,
            _ => LogCategory::Kernel,
        };

        let min = MIN_LEVEL.load(Ordering::Relaxed);
        if i32::from(level) < i32::from(min) {
            return 0;
        }

        let msg = cstr_slice(fmt as *const u8);
        klog_output(lvl, category, msg);
        0
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_ffi_info(msg: *const u8) {
    unsafe {
        if msg.is_null() {
            return;
        }
        let s = cstr_slice(msg);
        klog_output(LogLevel::Info, LogCategory::Kernel, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_ffi_warn(msg: *const u8) {
    unsafe {
        if msg.is_null() {
            return;
        }
        let s = cstr_slice(msg);
        klog_output(LogLevel::Warn, LogCategory::Kernel, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_ffi_error(msg: *const u8) {
    unsafe {
        if msg.is_null() {
            return;
        }
        let s = cstr_slice(msg);
        klog_output(LogLevel::Error, LogCategory::Kernel, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_net(fmt: *const u8) {
    unsafe {
        if fmt.is_null() {
            return;
        }
        let s = cstr_slice(fmt as *const u8);
        klog_output(LogLevel::Info, LogCategory::Net, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_net_err(fmt: *const u8) {
    unsafe {
        if fmt.is_null() {
            return;
        }
        let s = cstr_slice(fmt as *const u8);
        klog_output(LogLevel::Error, LogCategory::Net, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_init_msg(fmt: *const i8) {
    unsafe {
        if fmt.is_null() {
            return;
        }
        let s = cstr_slice(fmt as *const u8);
        klog_output(LogLevel::Info, LogCategory::Boot, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_kern(fmt: *const i8) {
    unsafe {
        if fmt.is_null() {
            return;
        }
        let s = cstr_slice(fmt as *const u8);
        klog_output(LogLevel::Info, LogCategory::Kernel, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_syscall(fmt: *const i8) {
    unsafe {
        if fmt.is_null() {
            return;
        }
        let s = cstr_slice(fmt as *const u8);
        klog_output(LogLevel::Info, LogCategory::Syscall, s);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `msg`/`fmt` 是指向以 NUL 结尾的 C 字符串的有效指针, 所在内存可由内核访问.
pub unsafe extern "C" fn klog_info(fmt: *const i8) {
    unsafe {
        if fmt.is_null() {
            return;
        }
        let s = cstr_slice(fmt as *const u8);
        klog_output(LogLevel::Info, LogCategory::Kernel, s);
    }
}
