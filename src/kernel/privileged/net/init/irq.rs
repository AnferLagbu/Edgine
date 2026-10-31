//! 网络设备中断接线 (ISSUE-SRC-008): 中断 → ISR → 底半部 → poll 收包
//!
//! ## 职责
//!
//! privileged 保留"中断→底半部"机制: 网卡 ISR 注册 (x86_64 MSI-X 向量 /
//! aarch64 GIC SPI) + 设备侧 ack 分发 + `NetRx` 软中断唤醒。报文队列归属
//! functions 驱动内部 (框架不感知包队列), privileged 只做机制转发。
//!
//! ## 数据流
//!
//! ```text
//! 网卡中断 → ISR (本模块) → ops.handle_irq()  设备侧 ack
//!                       └→ raise_softirq(NetRx)
//!                            → do_softirq()  中断退出前
//!                                 → net_rx_softirq_handler → poll_network()
//! ```
//!
//! EOI 与 `do_softirq` 均由各架构中断退出路径统一处理 (x86_64 见
//! `idt::handle_irq`, aarch64 见 `arch::aarch64::exception`), 本模块只负责
//! 设备侧 ack 与底半部置位。

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::privileged::egdf::NetOps;
use crate::privileged::irq::{SoftirqVec, raise_softirq};
use crate::privileged::sync::OnceLock;

/// 探测到的网卡 `NetOps` 指针表 (启动期单次安装)。
static NET_IRQ_OPS: OnceLock<&'static NetOps> = OnceLock::new();

/// 网卡驱动实例裸指针 (`NetDeviceRegistration::driver_data`)。
static NET_IRQ_DATA: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

/// 首个设备中断已投递 (一次置位, 供启动日志与 QEMU 里程碑断言).
static FIRST_IRQ_SEEN: AtomicBool = AtomicBool::new(false);

/// 安装网卡中断接线 (由 `probe::nic_probe_all` 在设备探测成功后调用)。
///
/// 仅记录 ISR 分发所需的 `NetOps` 表与驱动裸指针; 中断源本身的注册由
/// functions 驱动在探测时经 `net_register_msix_isr` / `net_register_intx_isr`
/// 完成 (启动临界区单线程, 各发生一次)。
pub(crate) fn net_irq_install(ops: &'static NetOps, driver_data: *mut c_void) {
    // 启动期单次调用; 重复安装视为无操作 (槽位已被首次安装占用)。
    let _ = NET_IRQ_OPS.set(ops);
    NET_IRQ_DATA.store(driver_data, Ordering::Release);
}

/// 网卡中断统一分发 (中断上下文): 设备侧 ack + 置位 `NetRx` 底半部。
///
/// 未安装 `NetOps` (探测失败) 时仅置位底半部, `poll_network` 会因无设备
/// 直接返回 (fail-quiet)。
fn net_irq_dispatch() {
    if let Some(ops) = NET_IRQ_OPS.get() {
        ops.handle_irq(NET_IRQ_DATA.load(Ordering::Acquire).cast::<u8>());
    }
    if !FIRST_IRQ_SEEN.swap(true, Ordering::Relaxed) {
        crate::klog_info!(
            Net,
            "[net] first device interrupt delivered (IRQ-driven RX active)"
        );
    }
    raise_softirq(SoftirqVec::NetRx);
}

// ============================================================================
// x86_64: MSI-X 中断 (IDT 向量)
// ============================================================================

#[cfg(target_arch = "x86_64")]
/// MSI-X ISR 入口 (IDT 签名) — 转发至统一分发。
///
/// # Safety
///
/// `frame` 由 IDT 中断入口压栈, 指向保存的寄存器; 本 ISR 不读取 frame
/// 内容, 仅作 IDT 签名要求。
extern "C" fn net_msix_irq_handler(_frame: *mut crate::privileged::idt::InterruptFrame) {
    net_irq_dispatch();
}

/// 注册网卡 MSI-X ISR (functions 可调用的 0 unsafe 入口)。
///
/// `msi_vector` 为 `msix_enable` 分配的 LAPIC 向量号, 内部换算 IDT 索引
/// (`irq = vector - IRQ_BASE`) 后注册统一 ISR。
///
/// # Errors
///
/// `IdtManager::register_msi_irq` 失败 (irq 范围错) 时返回错误。
#[cfg(target_arch = "x86_64")]
pub fn net_register_msix_isr(msi_vector: u8) -> Result<(), &'static str> {
    use crate::privileged::idt::IdtManager;
    let irq = msi_vector - crate::privileged::idt::IRQ_BASE;
    let manager = IdtManager::instance();
    manager.register_msi_irq(irq, net_msix_irq_handler, "net-msix")?;
    // 与 NVMe MSI-X 注册一致: IDT 抽象统一要求 enable_irq 以免向量被屏蔽。
    IdtManager::enable_irq(irq);
    Ok(())
}

// ============================================================================
// x86_64: PCI INTx 中断 (8259A 向量段 → IOAPIC 重定向)
// ============================================================================

/// `INTx` ISR 入口 (IDT 签名) — 转发至统一分发。
#[cfg(target_arch = "x86_64")]
extern "C" fn net_intx_irq_handler(_frame: *mut crate::privileged::idt::InterruptFrame) {
    net_irq_dispatch();
}

/// 注册网卡 PCI `INTx` ISR 并接通 IOAPIC 路由 (functions 可调用的 0 unsafe 入口)。
///
/// 仅接受传统线号 `1..=15`: IRQ0 属 PIT/legacy 定时器域 (启动期显式屏蔽),
/// ≥16 属 MSI/LAPIC 向量域。`INTx` 使用向量 `IRQ_BASE + irq` (0x20-0x2F),
/// 与 tick 向量 0x30 不重叠。
///
/// 路由写入 IOAPIC 重定向表并解除屏蔽; 极性/触发模式按
/// `ioapic::route_intx_irq` 的 ISO 优先 + PCI 总线默认策略解析。
///
/// # Errors
///
/// - `irq` 不在 `1..=15`;
/// - `IdtManager::register_irq` 拒绝该线;
/// - IOAPIC 无法映射该 GSI (MADT 无 IOAPIC / 控制器未初始化 / 越界)。
#[cfg(target_arch = "x86_64")]
pub fn net_register_intx_isr(irq: u8) -> Result<(), &'static str> {
    use crate::privileged::idt::{IRQ_BASE, IdtManager};
    if !(1..16).contains(&irq) {
        return Err("net INTx irq must be in 1..=15");
    }
    IdtManager::instance().register_irq(irq, net_intx_irq_handler, "net-intx", 0)?;
    let apic_id = crate::privileged::glue::low_u8_from_u32(crate::privileged::arch::apic::get_id());
    crate::privileged::arch::ioapic::route_intx_irq(irq, IRQ_BASE + irq, apic_id)?;
    Ok(())
}

// ============================================================================
// aarch64: GIC SPI 中断
// ============================================================================

/// 注册网卡 GIC SPI ISR (functions 可调用的 0 unsafe 入口)。
///
/// 委派 `gic::register_device_spi` 登记 handler 并配置/使能该 SPI。
///
/// # Errors
///
/// - `intid` 非 SPI 或超出分发表范围;
/// - 该 SPI 槽位已被占用。
#[cfg(target_arch = "aarch64")]
pub fn net_register_intx_isr(intid: u32) -> Result<(), &'static str> {
    crate::privileged::arch::gic::register_device_spi(intid, net_irq_dispatch)
}
