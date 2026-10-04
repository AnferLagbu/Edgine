#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export + syscall 策略入口.
//! `io_uring` 异步 I/O 框架 — services 侧 re-export 兼容层
//!
//! ## 分层 (T2 批 3, syscall-followup)
//!
//! 机制留在 framework (`framework/io/iouring.rs`):
//!   - `IoUring` 实例 + `RingBuffer` + `Sqe/Cqe` 数据结构
//!   - 全局实例管理 (`io_uring_setup/enter/destroy/submit/reap`)
//!   - `sys_io_uring_submit_sqe` (EG_IO_URING_SUBMIT 机制独有, 留在回退层)
//!
//! 本文件实现 syscall 策略入口: 参数转换 + errno 转换 + 委托机制函数.
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原策略代码按统一判据"机制持有的数据结构/常量归 framework"迁回
//! `framework/io/iouring.rs`。framework/io 顶层 re-export iouring (`pub use
//! iouring::*`), services 经合法方向消费。

pub use crate::framework::io::iouring::*;

/// io_uring_setup(entries) 策略 — 创建 io_uring 实例
///
/// T2 批 3 自 framework 回退层迁移. 委托 framework 机制 `io_uring_setup`
/// (全局实例表 + ID 分配), 本层仅参数转换 + 错误码映射.
pub fn io_uring_setup_syscall(entries: u64) -> i64 {
    let pid = crate::framework::proc::process_get_current_pid();
    match crate::framework::io::io_uring_setup(entries as u32, pid as u32) {
        Ok(id) => i64::from(id),
        Err(e) => e.as_ret(),
    }
}

/// io_uring_enter(fd, to_submit, min_complete) 策略 — 提交并处理请求
///
/// T2 批 3 自 framework 回退层迁移. 委托 framework 机制 `io_uring_enter`
/// (处理 SQE → 生成 CQE), 本层仅参数转换 + 错误码映射.
pub fn io_uring_enter_syscall(id: u64, to_submit: u64, min_complete: u64) -> i64 {
    match crate::framework::io::io_uring_enter(id as u32, to_submit as u32, min_complete as u32) {
        Ok(n) => i64::from(n),
        Err(e) => e.as_ret(),
    }
}
