//! I/O 子系统
//!
//! 包含异步 I/O 框架 (io_uring) 等组件.

pub mod iouring;

// iouring 公共接口 re-export — 避免跨子系统直接访问 io::iouring 内部
pub use iouring::*;
