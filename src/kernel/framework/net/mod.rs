//! 网络子系统
//!
//! ## 依赖声明
//!
//! framework 内部依赖: sync, syscall, driver
//! services 依赖: `services::net` (安全代理)

pub mod driver;
/// REVAL-W: 网络协议栈抽象 — Framekernel Safe API (W1 子任务, 2026-06-24)
/// 设计见 [docs/plan/smoltcp-framekernel-wrapper.md]
pub mod iface_trait;
#[cfg(not(feature = "kernel_test"))]
pub mod init;
/// NetOps 安全桥 (批次 Z ④): services 网络设备经 trait 接入 smoltcp。
pub mod net_device_ops;
/// C5: Netfilter 包过滤框架
pub mod netfilter;
/// C5: 路由表管理
pub mod route;
/// P2-I-44: 网络快照 (`net_save` / `net_restore` 完整实现)
pub mod save;
#[cfg(not(feature = "kernel_test"))]
pub mod smoltcp_impl;
/// P2-I-41: Socket WaitQueue 基础设施
pub mod wait_queue;

/// 网络子系统 Rust 模块
///
/// 基于 smoltcp 协议栈的完整网络实现：
/// - 网络初始化状态机 (init)
/// - 网卡驱动抽象 (NetNic enum)
/// - smoltcp 网络栈集成 (smoltcp_impl)
///
/// ## 架构概览
///
/// ```text
/// src/net/
/// |-- mod.rs          # 模块导出
/// |-- types.rs        # 公共状态 (NET_READY / NET_CONFIGURED)
/// |-- smoltcp_impl.rs # Device trait 实现 + ChitinNetDevice
/// |-- init.rs         # 初始化状态机 + DHCP + Socket API
/// +-- driver/         # 网卡驱动重新导出 → kernel::driver::net
/// +-- smoltcp/        # smoltcp 协议栈源码
/// ```
/// - **状态机**: 初始化过程使用有限状态机，防止重复初始化
/// - **RAII**: 资源自动清理，防止内存泄漏
/// - **边界检查**: 数组访问和指针操作都有安全保证
/// - **错误处理**: Result<T, E> 替代 int 错误码
// ============================================================================
// 核心模块
// ============================================================================
pub mod types; // 公共状态 (NET_READY / NET_CONFIGURED)

// ============================================================================
// API 层
// ============================================================================

pub mod api;
pub mod socket_types; // 协议 wire 类型 (DECISION-J: 机制安全导出面)
pub mod syscall;

// ============================================================================
// 公共 API 导出
// ============================================================================

pub use types::*;
pub use wait_queue::*;
// init/smoltcp_impl 模块在 `#[cfg(not(feature = "kernel_test"))]` 守卫下;
// 此处 re-export 必须同步 gate, 否则 kernel_test build 失败 (P0-1 修复).
#[cfg(not(feature = "kernel_test"))]
pub use init::poll_network;
#[cfg(not(feature = "kernel_test"))]
pub(crate) use init::raw;
// ISSUE-SRC-008: 网卡中断接线 — services 驱动探测时注册其 ISR (0 unsafe 入口)。
#[cfg(all(not(feature = "kernel_test"), target_arch = "aarch64"))]
pub use init::irq::net_register_intx_isr;
#[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
pub use init::irq::net_register_msix_isr;
#[cfg(not(feature = "kernel_test"))]
pub use smoltcp_impl::{ChitinNetDevice, NetworkStack, init_stack, poll_stack};
// NetOps 安全桥 (批次 Z ④): trait + 注册入口 + DECISION-K 单向拉取槽。
pub use net_device_ops::{
    NetDeviceOps, NetDeviceRegistration, net_register_services_driver, register_net_device,
};
