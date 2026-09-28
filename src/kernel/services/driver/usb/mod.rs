#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! USB 驱动 — services 层 (Phase 2.1.6)
//!
//! 100% safe USB 子系统, 0 unsafe.
//! 所有 MMIO 操作通过 `framework::IoMem` 安全代理.
//!
//! ## 模块结构
//!
//! ```text
//! services::driver::usb
//! ├── xhci.rs         — xHCI 主机控制器 (USB 3.0) 安全代理
//! ├── usb_core.rs     — USB 核心类型 (Descriptors, URB, HostController Trait)
//! ├── enumerate.rs    — USB 设备枚举 (描述符解析 + 枚举流程)
//! ├── ring.rs         — xHCI 环形缓冲区 (Command Ring + Event Ring)
//! ├── hid.rs          — HID 类驱动 (键盘/鼠标 Boot Protocol)
//! └── mass_storage.rs — 大容量存储类驱动 (BBB + SCSI)
//! ```
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 所有类型定义 + 逻辑均不含 unsafe
//! - **MMIO 代理**: `xhci.rs` 通过 `IoMem` 安全访问寄存器
//! - **迁移中**: `enumerate` / `ring` / `hid` / `mass_storage` 已实装于 services (0 unsafe);
//!   `usb_core` 暂重导出 framework 类型, 待封装 2 处 unsafe 后实装
//!
//! ## 后续添加
//!
//! - `ehci.rs`  — EHCI (USB 2.0) (Phase 2.1.6 后续)
//! - `uhci.rs`  — UHCI (USB 1.1) (Phase 2.1.6 后续)
//! - `ohci.rs`  — OHCI (USB 1.1) (Phase 2.1.6 后续)
//!
//! 评估日期: 2026-07-04
//! Phase 2.1.6 任务: USB/XHCI 驱动迁移

pub mod enumerate;
pub mod hid;
pub mod mass_storage;
pub mod ring;
pub mod usb_core;
pub mod xhci;
