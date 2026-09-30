//! E1000 网卡驱动 (framework 层)
//!
//! 子模块:
//! - `dma_ring`: 描述符结构 + 状态/命令常量 (B04-19 从 services 上移)
//! - `e1000_io`: E1000Io MMIO 安全访问器 + 寄存器偏移常量 (B04-AUDIT-005 #4 修复: 从 services 迁回)
//! - `e1000`: TxRing/RxRing 环机制安全包装 (业务/FFI 面已迁 services::driver::net::e1000)

pub mod dma_ring;
pub mod e1000;
/// E1000Io MMIO 安全访问器 (framekernel 阶段 3: 唯一消费者是 services x86_64 E1000
/// 驱动, 其实现同样受此 cfg 门控; aarch64 与 kernel_test 构建下无使用者, 故不编译
/// 以避免寄存器位掩码常量成为死代码, 见 AGENTS.md §5 F9).
#[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]
pub mod e1000_io;
