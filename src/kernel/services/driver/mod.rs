#![deny(unsafe_code)]
//! 设备驱动 — 网卡/存储/显示/输入 (services 层)
//!
//! ## 当前状态: 驱动 safe 层已落 services (Phase 2.1 在途)
//!
//! services 侧安全驱动层 (0 unsafe):
//! - [char/](file:///home/anfer/Code/QueenX/src/kernel/services/driver/char/) — 字符设备 (VGA/serial)
//! - [display/](file:///home/anfer/Code/QueenX/src/kernel/services/driver/display/) — 显示 (HDMI/DP/DDC)
//! - [net/e1000.rs](file:///home/anfer/Code/QueenX/src/kernel/services/driver/net/e1000.rs) — E1000 网卡
//! - [storage/](file:///home/anfer/Code/QueenX/src/kernel/services/driver/storage/) — 存储 (NVMe/AHCI/ATA)
//! - [virtio/](file:///home/anfer/Code/QueenX/src/kernel/services/driver/virtio/) — VirtIO 网卡/块设备
//! - [usb/](file:///home/anfer/Code/QueenX/src/kernel/services/driver/usb/) — USB/XHCI (2-D 整体下沉, services 唯一权威)
//!
//! 注: `framework/driver/` 的 char/storage/virtio/display 仍保留对应实现 (影子双份),
//! 待后续双份合并收口; usb 已随 2-D 整体下沉删除 framework 侧。
//!
//! ## 迁移路径
//!
//! 1. 所有 MMIO 走 `framework::iomem::IoMem`
//! 2. 所有 PIO 走 `framework::ioport::IoPort`
//! 3. 所有 DMA 走 `framework::dma_buf::DmaStream`
//! 4. 中断处理走 `framework::irqline::IrqLine`
//! 5. 在 services/driver/ 暴露纯 safe 驱动 API
//!
//! ## Phase 2.1 进度
//!
//! | 子任务 | 驱动 | 状态 | services 实现 |
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
/// T24: E1000 网卡驱动 (services 层安全逻辑)
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

use crate::framework::idt::irq_trait::{
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

/// `services::driver` 初始化 — 注册策略到 framework
pub fn init() {
    // T-04: 注册驱动层中断处理决策策略
    static POLICY: DriverIrqDecision = DriverIrqDecision;
    let _ = register_irq_decision(&POLICY);
}
