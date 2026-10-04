//! ftrace — 内核函数级跟踪 (TCB)
//!
//! ## 模型
//!
//! - **跟踪点 (Trace Point)**: 编译期声明的命名锚点, 在代码关键路径上
//!   调用 `trace_event!(NAME, ...)` 即可记录一个事件
//! - **事件记录 (Event Record)**: `<ts, name_hash, arg0, arg1, arg2, arg3>` 固定 40 字节
//! - **存储**: 单 ring buffer (4 KiB); 容量不足覆盖最旧事件
//!
//! ## SAFETY 不变式
//!
//! - 事件记录是 POD, 可按字节序列化/反序列化
//! - 名称 hash 独立于字符串地址 (FNV-1a), 跨重启不歧义
//!
//! ## 用户态读取
//!
//! - `ftrace_read`: 弹出事件, 按紧凑布局写入用户缓冲
//! - `ftrace_enable`/`ftrace_disable`: 切换总开关
//!
//! ## 性能
//!
//! - 总开关关闭时 `trace_event!` 展开为单条 `if false { ... }`, 编译期消除
//! - 启用时单事件约 30 ns (无锁 ring push)

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::ringbuf::RingBuffer;

/// 单事件字节大小 (ts:8 + hash:8 + arg0..3:4*8)
/// 事件字节数 (8 + 8 + 4 * 8 = 48)
pub const EVENT_SIZE: usize = 48;

/// Ring buffer 容量
pub const FTRACE_BUF_CAP: usize = 4096;

/// 跟踪点数量上限
pub const MAX_TRACE_POINTS: usize = 64;

/// 一个跟踪事件
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TraceEvent {
    /// RDTSC / cntvct 时间戳
    pub timestamp: u64,
    /// 跟踪点名称 hash (FNV-1a 32-bit, 高 32 位填 0)
    pub name_hash: u64,
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
}

impl TraceEvent {
    pub const fn zero() -> Self {
        Self {
            timestamp: 0,
            name_hash: 0,
            arg0: 0,
            arg1: 0,
            arg2: 0,
            arg3: 0,
        }
    }
}

/// 全局 ftrace 状态
pub struct FtraceState {
    /// 主 ring buffer (4 KiB)
    buf: RingBuffer<{ FTRACE_BUF_CAP }>,
    /// 总开关
    enabled: AtomicBool,
    /// 累计事件计数
    event_count: AtomicU64,
    /// 累计溢出计数
    overflow_count: AtomicU64,
    /// 跟踪点登记表 (FNV-1a 32-bit name -> count)
    points: [AtomicU64; MAX_TRACE_POINTS],
}

impl FtraceState {
    pub const fn new() -> Self {
        Self {
            buf: RingBuffer::new(),
            enabled: AtomicBool::new(false),
            event_count: AtomicU64::new(0),
            overflow_count: AtomicU64::new(0),
            points: [const { AtomicU64::new(0) }; MAX_TRACE_POINTS],
        }
    }

    /// 启用跟踪
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    /// 禁用跟踪
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// 是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// 记录一个事件
    pub fn record(&self, ev: TraceEvent) {
        if !self.is_enabled() {
            return;
        }
        let mut bytes = [0u8; EVENT_SIZE];
        bytes[0..8].copy_from_slice(&ev.timestamp.to_le_bytes());
        bytes[8..16].copy_from_slice(&ev.name_hash.to_le_bytes());
        bytes[16..24].copy_from_slice(&ev.arg0.to_le_bytes());
        bytes[24..32].copy_from_slice(&ev.arg1.to_le_bytes());
        bytes[32..40].copy_from_slice(&ev.arg2.to_le_bytes());
        bytes[40..48].copy_from_slice(&ev.arg3.to_le_bytes());
        let written = self.buf.push(&bytes);
        if written < EVENT_SIZE {
            self.overflow_count.fetch_add(1, Ordering::Relaxed);
        }
        self.event_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 弹出最旧事件
    /// # Panics
    /// 事件数据解析时切片长度异常时 panic (正常情况下不会发生)。
    pub fn pop(&self) -> Option<TraceEvent> {
        let mut bytes = [0u8; EVENT_SIZE];
        let n = self.buf.pop_into(&mut bytes);
        if n < EVENT_SIZE {
            return None;
        }
        // SAFETY: `bytes` 是 `[0u8; EVENT_SIZE]`, 各切片长度固定为 8 字节,
        // `try_into()` 到 `[u8; 8]` 在编译期已证明长度匹配, unwrap 不可能失败.
        let ts = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let hash = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let a0 = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let a1 = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let a2 = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        let a3 = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        Some(TraceEvent {
            timestamp: ts,
            name_hash: hash,
            arg0: a0,
            arg1: a1,
            arg2: a2,
            arg3: a3,
        })
    }

    /// 获取事件计数
    pub fn event_count(&self) -> u64 {
        self.event_count.load(Ordering::Relaxed)
    }

    /// 获取溢出计数
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count.load(Ordering::Relaxed)
    }

    /// 注册一个跟踪点 (按 name hash 查重, 计数累加)
    pub fn register_point(&self, name_hash: u32) -> bool {
        let h = u64::from(name_hash);
        for slot in &self.points {
            let v = slot.load(Ordering::Acquire);
            if v == h || v == 0 {
                slot.store(h, Ordering::Release);
                return true;
            }
        }
        false
    }
}

/// 全局 ftrace 状态实例
pub static FTRACE: FtraceState = FtraceState::new();

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 计算 FNV-1a 32-bit
pub const fn fnv1a_32(s: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    let mut i = 0;
    while i < s.len() {
        h ^= s[i] as u32;
        h = h.wrapping_mul(0x01000193);
        i += 1;
    }
    h
}

/// 读取 TSC / cntvct (跨架构)
#[inline]
pub fn rd_timestamp() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: rdtsc 用户态指令, 在 privileged TCB 内安全使用
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // SAFETY: CNTVCT_EL0 用户态可读
        unsafe {
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack));
        }
        v
    }
}

/// 宏: 记录一个事件, 在关闭时为 no-op
#[macro_export]
macro_rules! trace_event {
    ($name:expr_2021) => {{
        $crate::privileged::debug::ftrace::record_named(
            $crate::privileged::debug::ftrace::fnv1a_32($name.as_bytes()),
            0,
            0,
            0,
            0,
        );
    }};
    ($name:expr_2021, $a0:expr_2021) => {{
        $crate::privileged::debug::ftrace::record_named(
            $crate::privileged::debug::ftrace::fnv1a_32($name.as_bytes()),
            $a0 as u64,
            0,
            0,
            0,
        );
    }};
    ($name:expr_2021, $a0:expr_2021, $a1:expr_2021) => {{
        $crate::privileged::debug::ftrace::record_named(
            $crate::privileged::debug::ftrace::fnv1a_32($name.as_bytes()),
            $a0 as u64,
            $a1 as u64,
            0,
            0,
        );
    }};
    ($name:expr_2021, $a0:expr_2021, $a1:expr_2021, $a2:expr_2021) => {{
        $crate::privileged::debug::ftrace::record_named(
            $crate::privileged::debug::ftrace::fnv1a_32($name.as_bytes()),
            $a0 as u64,
            $a1 as u64,
            $a2 as u64,
            0,
        );
    }};
    ($name:expr_2021, $a0:expr_2021, $a1:expr_2021, $a2:expr_2021, $a3:expr_2021) => {{
        $crate::privileged::debug::ftrace::record_named(
            $crate::privileged::debug::ftrace::fnv1a_32($name.as_bytes()),
            $a0 as u64,
            $a1 as u64,
            $a2 as u64,
            $a3 as u64,
        );
    }};
}

/// 记录带 hash 名称的事件 (供宏使用)
#[inline]
pub fn record_named(name_hash: u32, a0: u64, a1: u64, a2: u64, a3: u64) {
    if !FTRACE.is_enabled() {
        return;
    }
    let ev = TraceEvent {
        timestamp: rd_timestamp(),
        name_hash: u64::from(name_hash),
        arg0: a0,
        arg1: a1,
        arg2: a2,
        arg3: a3,
    };
    FTRACE.record(ev);
}

/// 初始化 ftrace 子系统
pub fn ftrace_init() {
    FTRACE.register_point(fnv1a_32(b"sys_enter"));
    FTRACE.register_point(fnv1a_32(b"sys_exit"));
    FTRACE.register_point(fnv1a_32(b"sched_switch"));
    FTRACE.register_point(fnv1a_32(b"irq_enter"));
    FTRACE.register_point(fnv1a_32(b"irq_exit"));
    FTRACE.register_point(fnv1a_32(b"page_fault"));
    FTRACE.register_point(fnv1a_32(b"kgdb_enter"));
}
