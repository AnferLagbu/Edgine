#![deny(unsafe_code)]
//! 设备驱动 — 网卡/存储/显示/输入 (functions 层)
//!
//! ## 当前状态: 驱动业务层落 functions (0 unsafe); privileged 保留机制原语
//!
//! functions 侧安全驱动层 (0 unsafe):
//! - [char/](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/char/) — 字符设备 (VGA/serial, x86_64)
//! - [display/](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/display/) — 显示 (HDMI/DP/DDC)
//! - [net/e1000.rs](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/net/e1000.rs) — E1000 网卡
//! - [storage/](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/storage/) — 存储 (NVMe/AHCI/ATA)
//! - [virtio/](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/virtio/) — VirtIO 网卡/块设备
//! - [usb/](file:///home/anfer/Code/Edgine/src/kernel/functions/driver/usb/) — USB/XHCI (2-D 整体下沉, functions 唯一权威)
//!
//! 注: `privileged/driver/` 对应模块**保留机制原语** (业务下沉后已非影子双份):
//! MMIO 安全代理 (`net/e1000_io.rs`)、DMA 环机制 (`net/e1000.rs` + `net/dma_ring.rs`)、
//! 存储 wire 类型与队列 safe wrapper (`storage/`)、VirtIO MMIO 传输 (`virtio/`)、
//! aarch64 PL011 (`char/pl011.rs`); 业务/FFI 面均在 functions。B04 审计
//! (2026-08-24/25) 曾将 E1000 机制反向回迁 privileged, 业务面随后再正式落 functions
//! (A 形态 + privileged 机制)。usb 已随 2-D 整体下沉, privileged 侧无 usb 模块。
//!
//! ## 迁移路径
//!
//! 1. 所有 MMIO 走 `privileged::iomem::IoMem`
//! 2. 所有 PIO 走 `privileged::ioport::IoPort`
//! 3. 所有 DMA 走 `privileged::dma_buf::DmaStream`
//! 4. 中断处理走 `privileged::irqline::IrqLine`
//! 5. 在 functions/driver/ 暴露纯 safe 驱动 API
//!
//! ## Phase 2.1 进度
//!
//! | 子任务 | 驱动 | 状态 | functions 实现 |
//! |--------|------|------|--------------|
//! | 2.1.1  | E1000 网卡 | ✅ | `net/e1000.rs` |
//! | 2.1.2  | VirtIO-Net | ✅ | `virtio/net.rs` |
//! | 2.1.3  | NVMe 存储 | ✅ | `storage/nvme.rs` |
//! | 2.1.4  | AHCI/ATA  | ✅ | `storage/ahci.rs` |
//! | 2.1.5  | VGA/串口/Framebuffer | ✅ | `char/{vga,serial}.rs` + `display/` |
//! | 2.1.6  | USB/XHCI | ✅ | `usb/xhci.rs` (2-D 整体下沉, 唯一权威) |

pub mod acpi;
pub mod char;
pub mod firmware;
/// T24: E1000 网卡驱动 (functions 层安全逻辑)
pub mod net;
/// D5: 电源管理安全封装
pub mod power;
pub mod storage;
pub mod usb;
pub mod virtio;

/// 显示子系统 (DDC + HDMI) 安全封装
pub mod display;
/// 帧缓冲 syscall 安全代理 (fb_open/fb_mmap/fb_release, T2 批 5)
pub mod fb;
/// D10: kexec 安全封装
pub mod kexec;
/// D11: UEFI 安全封装
pub mod uefi;

// ============================================================================
// T-04: 中断处理决策策略
// ============================================================================

use crate::privileged::idt::irq_trait::{
    IrqContext, IrqDecision, SoftirqContext, register_irq_decision,
};

/// 驱动层中断处理决策策略
///
/// - 共享 IRQ: 按注册顺序选择 handler (先注册先服务)
/// - Softirq: 按固定优先级 (High > Timer > `NetRx` > `NetTx` > Block > Tasklet > Sched > Kswapd)
/// - ksoftirqd: 超过 10 次循环后唤醒
pub struct DriverIrqDecision;

impl IrqDecision for DriverIrqDecision {
    fn select_handler_index(&self, _ctx: IrqContext) -> usize {
        // 先注册先服务
        0
    }

    fn softirq_priority_mask(&self, ctx: SoftirqContext) -> u64 {
        // 返回最高优先级位
        if ctx.pending_mask == 0 {
            0
        } else {
            1u64 << ctx.pending_mask.ilog2()
        }
    }

    fn should_wake_ksoftirqd(&self, loop_count: u32) -> bool {
        loop_count > 10
    }
}

/// `functions::driver` 初始化 — 注册策略到 privileged
pub fn init() {
    // T-04: 注册驱动层中断处理决策策略
    static POLICY: DriverIrqDecision = DriverIrqDecision;
    let _ = register_irq_decision(&POLICY);
}
