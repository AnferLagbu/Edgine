#![deny(unsafe_code)]
//! 内核调试 / 跟踪 — functions 层安全代理
//!
//! 将 `privileged::debug` (TCB) 的 unsafe 原语封装为 100% safe Rust API,
//! 供用户态 / 业务模块使用。
//!
//! ## 子能力
//!
//! - **ftrace**: 内核函数级跟踪, ring buffer 事件记录
//!   - 开关控制 (启用/禁用)
//!   - 事件读取 (弹出一条事件到用户态)
//!   - 状态查询 (累计计数 / 溢出计数)
//!   - 跟踪点注册
//! - **KGDB**: 内核调试器桩, 通过串口与外部 gdb 通信
//!   - 主动进入 (用户态触发)
//!   - 状态查询
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! use crate::functions::debug;
//!
//! // 启用 ftrace
//! debug::ftrace_enable();
//!
//! // 注册跟踪点
//! debug::ftrace_register("sched_switch");
//!
//! // 弹出一条事件
//! if let Some(ev) = debug::ftrace_read_event() {
//!     klog::printk("ev ts={} hash={:x}\n", ev.timestamp, ev.name_hash);
//! }
//!
//! // 主动进入 KGDB (要求串口已注册)
//! if debug::kgdb_enter() == 0 {
//!     klog::printk("KGDB 返回\n");
//! }
//! ```
//!
//! ## 安全契约
//!
//! - 本模块零 unsafe, 所有 unsafe 操作在 `privileged::debug` 中完成
//! - `kgdb_enter` 在用户态串口未注册时返回 false (而非阻塞)

// Re-export 关键类型
pub use crate::privileged::debug::fnv1a_32;
pub use crate::privileged::debug::{EVENT_SIZE, FTRACE_BUF_CAP, TraceEvent};
pub use crate::privileged::debug::{KgdbRegs, KgdbSerial};

/// D4: eBPF 安全封装
pub mod ebpf;

/// T4-3: eBPF 验证器策略 (Safe Policy Injection)
///
/// 实现 `privileged::debug::BpfVerifier` trait, 提供标准验证策略.
/// 本模块 0 unsafe, 全部策略逻辑由 functions 拥有.
pub mod ebpf_verifier;

// ============================================================================
// ftrace 接口
// ============================================================================

/// 启用 ftrace 全局开关
pub fn ftrace_enable() {
    crate::privileged::debug::ftrace_enable();
}

/// 禁用 ftrace 全局开关
pub fn ftrace_disable() {
    crate::privileged::debug::ftrace_disable();
}

/// 查询 ftrace 启用状态
pub fn ftrace_is_enabled() -> bool {
    crate::privileged::debug::ftrace_is_enabled()
}

/// 累计事件计数
pub fn ftrace_event_count() -> u64 {
    crate::privileged::debug::ftrace_event_count()
}

/// 累计溢出计数
pub fn ftrace_overflow_count() -> u64 {
    crate::privileged::debug::ftrace_overflow_count()
}

/// 弹出一条事件 (None = 空)
pub fn ftrace_read_event() -> Option<TraceEvent> {
    crate::privileged::debug::ftrace_pop_event()
}

/// 注册一个跟踪点 (按名称字符串)
pub fn ftrace_register(name: &'static str) -> bool {
    crate::privileged::debug::ftrace_register_point(fnv1a_32(name.as_bytes()))
}

// ============================================================================
// KGDB 接口
// ============================================================================

/// 主动进入 KGDB 主循环
///
/// - 串口未注册时: 返回 false, 不阻塞
/// - 串口已注册时: 阻塞与外部 gdb 通信, 返回 true 表示 KGDB 已返回
pub fn kgdb_enter() -> bool {
    if !crate::privileged::debug::kgdb_serial_ready() {
        return false;
    }
    let mut regs = KgdbRegs::default();
    crate::privileged::debug::kgdb_breakpoint(&mut regs);
    true
}

/// 当前是否在 KGDB 主循环中
pub fn kgdb_is_active() -> bool {
    crate::privileged::debug::kgdb_active()
}

/// 串口是否已注册到 KGDB
pub fn kgdb_serial_ready() -> bool {
    crate::privileged::debug::kgdb_serial_ready()
}

// ============================================================================
// 初始化
// ============================================================================

/// 初始化 debug 子系统
pub fn debug_init() {
    crate::privileged::debug::debug_init();
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_basic() {
        // FNV-1a ("") = 0x811c9dc5
        assert_eq!(fnv1a_32(b""), 0x811c9dc5);
        // FNV-1a ("a") = 0xe40c292c
        assert_eq!(fnv1a_32(b"a"), 0xe40c292c);
    }

    #[test]
    fn trace_event_size() {
        // 8 (ts) + 8 (hash) + 4 * 8 (args) = 48 字节
        assert_eq!(EVENT_SIZE, 48);
        assert_eq!(core::mem::size_of::<TraceEvent>(), 48);
    }
}
