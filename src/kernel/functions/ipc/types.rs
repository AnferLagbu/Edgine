#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! IPC 数据类型 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原类型定义 (IpcNamespace/Pipe/MsgQueue/ShmSegment/Semaphore/Message/
//! WaitQueue/WaitQueueItem + IPC_MAX_* 常量) 按统一判据"机制持有的数据结构
//! 归 privileged、功能实现归 functions"迁回 `privileged/ipc/types.rs`
//! (被 privileged `IPC_NAMESPACE` 全局机制直接持有)。
//!
//! 本文件保留 re-export 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。
//! functions/ipc/{pipe,shm,msgq,sem,signal}.rs 的策略实现 (T6 迁移权威) 不受影响。

pub use crate::privileged::ipc::types::*;
