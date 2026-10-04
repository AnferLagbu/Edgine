#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! # 电源管理 — functions 侧 re-export 兼容层
//!
//! ## DECISION-M 归属反转记录 (2026-09-13)
//!
//! 原策略代码按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/driver/power.rs`：`PM_SUBSYSTEM: PmSubsystem` 全局实例由 privileged
//! 机制持有 (pm_init/pm_idle/pm_suspend/sys_pm 直接操作), governor/C-state 算法
//! 全部操作 PmSubsystem 内部字段, 无法脱离机制状态独立存在; `sys_pm_dispatch`
//! 为 cmd→方法映射 (空转转发), 非独立策略业务 — 与 IpcStrategy 关键区别
//! (IpcStrategy 有真实业务算法需 trait 注入)。裁决: DECISION-M 方案 B — 15 类型
//! + dispatch 迁回 privileged, 本文件改 glob re-export。
//!
//! privileged/driver 的 power 为公开子模块 (`pub mod power` + 顶层 `pub use power::*`),
//! 可直接 glob re-export, 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。
//! 演进预留: 未来 governor 独立化再下沉, 不堵死。

pub use crate::privileged::driver::power::*;
