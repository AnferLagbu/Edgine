//! 内核调试与跟踪基础设施 (TCB)
//!
//! 提供两类核心能力:
//!
//! - **ftrace**: 函数级跟踪, ring buffer 事件记录, kprobe-like 动态插桩
//! - **KGDB**: 内核调试器桩, 通过串口与外部 gdb 通信
//!
//! ## 边界
//!
//! 全部位于 privileged 层, 可在 unsafe 上下文使用。functions 层通过
//! `functions::debug` 安全封装暴露给用户态。
//!
//! ## 子模块
//!
//! - [ringbuf](file:///home/anfer/Code/Edgine/src/kernel/privileged/debug/ringbuf.rs) — 单生产者单消费者环形缓冲区
//! - [ftrace](file:///home/anfer/Code/Edgine/src/kernel/privileged/debug/ftrace.rs) — 跟踪点/事件记录
//! - [kgdb](file:///home/anfer/Code/Edgine/src/kernel/privileged/debug/kgdb.rs) — KGDB 桩
//! - [api](file:///home/anfer/Code/Edgine/src/kernel/privileged/debug/api.rs) — 公共 re-export
// ebpf 公共接口 re-export — 避免跨子系统直接访问 debug::ebpf 内部
pub use ebpf::bpf_init;
pub use ebpf::sys_bpf;

pub mod api;
/// D4: eBPF 扩展包过滤器
pub mod ebpf;
pub mod ftrace;
pub mod kgdb;
pub mod ringbuf;

// 子模块公共接口 re-export — 避免跨子系统直接访问 debug 内部子模块
pub use api::*;
pub use ebpf::*;
pub use ftrace::*;
pub use kgdb::*;
