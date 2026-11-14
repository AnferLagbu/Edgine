#![deny(unsafe_code)]
#![deny(missing_docs)]
//! `DHCPv6` 客户端机制 (RFC 8415) — functions 层自持.
//!
//! 与 `DHCPv4` (smoltcp 内置 `dhcpv4::Socket` 机制 + Edgine 策略) 不同,
//! smoltcp 0.14 未提供 `DHCPv6` socket, 故本子模块自持**机制**:
//! 报文编解码 (`wire`) + SARR/租约状态机 (`state`, Phase 3) + 重传退避
//! 策略 (`policy`, Phase 3). privileged 层只提供 UDP 字节管道与 RA 捕获,
//! 不暴露任何 smoltcp 类型 (F2 类型擦除).
//!
//! 全部 0 unsafe, 仅用 `&[u8]` / 整数 / `alloc::vec::Vec`, 不依赖 smoltcp.

pub mod policy;
/// P6b Phase 5: RA M/O 触发解析 (纯字节, 0 smoltcp, 离线可测).
pub mod ra;
pub mod state;
pub mod wire;

/// P6b Phase 4: 客户端驱动 (IRQ 安全静态 + privileged 反转钩子).
///
/// 依赖 privileged `init::dhcpv6` 的 [`Dhcpv6PollCtx`](crate::privileged::net::init::dhcpv6::Dhcpv6PollCtx),
/// 该模块在 `kernel_test` 下被 cfg-out, 故本模块同 gate.
#[cfg(not(feature = "kernel_test"))]
pub mod driver;
