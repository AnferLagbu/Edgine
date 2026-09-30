#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! 存储设备驱动 — services 层 (Phase 2.1.3 + 2.1.4)
//!
//! 包含块设备控制器的 100% safe API,
//! 为内核块设备栈 (`BlockDevice` trait) 提供安全抽象。
//!
//! ## 模块结构
//!
//! - [nvme]  — `NVMe` 控制器 (Phase 2.1.3), 0 unsafe, 完整驱动逻辑
//! - [ahci]  — AHCI SATA 控制器 (Phase 2.1.4), 0 unsafe, 完整驱动逻辑
//! - [ata]   — 传统 ATA PIO 驱动 (framekernel 阶段 3 回迁), 0 unsafe, 真实驱动
//!
//! ## 架构
//!
//! - 所有 MMIO 通过 `framework::IoMem` 安全代理
//! - 所有 DMA 通过 framework safe wrapper (`nvme_alloc`_* / `ahci_alloc`_*)
//! - 命令构造在 services 层 (safe), 提交通过 framework safe function
//! - 零 unsafe: services 层严格遵守 `#![deny(unsafe_code)]`
//!
//! 评估日期: 2026-06-04

pub mod ahci;
/// 传统 ATA PIO 驱动 (framekernel 阶段 3: framework PIO 驱动回迁 services)
pub mod ata;
pub mod nvme;

use alloc::vec::Vec;

use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
// 日志仅用于 x86_64 门控代码 (storage_init / MSIX-03 自测)
#[cfg(target_arch = "x86_64")]
use crate::slog_info;
#[cfg(target_arch = "x86_64")]
use crate::slog_warn;

/// services 层 `NVMe` 控制器注册表 (DECISION-H storage 专项 1 号子步)
pub static NVME_CONTROLLERS: Mutex<Vec<nvme::NvmeController>> = Mutex::new(Vec::new());

/// services 层 AHCI 控制器注册表
pub static AHCI_CONTROLLERS: Mutex<Vec<ahci::AhciController>> = Mutex::new(Vec::new());

/// services 层 ATA 控制器注册表 (framekernel 阶段 3: ATA PIO 驱动回迁)
pub static ATA_CONTROLLERS: Mutex<Vec<ata::AtaController>> = Mutex::new(Vec::new());

// ============================================================================
// 存储子系统初始化 (DECISION-H storage 专项 1 号子步: services 注册路径)
// ============================================================================

/// PCI 存储控制器类码
#[cfg(target_arch = "x86_64")]
const PCI_CLASS_STORAGE: u8 = 0x01;
/// PCI AHCI 子类码
#[cfg(target_arch = "x86_64")]
const PCI_SUBCLASS_AHCI: u8 = 0x06;
/// PCI NVMe 子类码
#[cfg(target_arch = "x86_64")]
const PCI_SUBCLASS_NVME: u8 = 0x08;

// ============================================================================
// NVMe MSI-X 中断路径 (DECISION-H storage 专项 2 号子步)
// ============================================================================

/// NVMe MSI-X ISR services 分发回调 (注册契约: framework handler 转发调用)
///
/// ISR 上下文约束: 仅持 IrqSpinLock (中断安全), 不分配/不睡眠 —
/// 与 framework 版 `nvme_msix_irq_handler` 的注册表分发同构。
#[cfg(target_arch = "x86_64")]
fn nvme_msix_dispatch() {
    let mut controllers = NVME_CONTROLLERS.lock();
    for ctrl in controllers.iter_mut() {
        if ctrl.irq_vector().is_some() {
            ctrl.handle_interrupt();
        }
    }
}

/// MSIX-03: services 版受控 MSI-X 中断投递验证 (与 framework hook 等值)
///
/// 锁内提交 (IF=0 无中断窗口) → 释放锁 → 开 IF 窗口等 ISR 计数变化:
/// 验证 handle_irq MSI 分支 → LAPIC EOI → 注册契约分发 → services
/// `handle_interrupt` 端到端链路。结果仅记日志 (与 framework hook 一致)。
#[cfg(target_arch = "x86_64")]
fn nvme_msix03_selftest(ci: usize) {
    use crate::framework::driver::storage::nvme as fw_nvme;
    use crate::framework::driver::storage::{nvme_alloc_dma_buffer, nvme_with_interrupts_enabled};
    use crate::framework::mm::PAGE_SIZE;

    let has_irq = NVME_CONTROLLERS
        .lock()
        .get(ci)
        .map_or(false, |c| c.irq_vector().is_some());
    if !has_irq {
        return;
    }

    slog_info!(
        Driver,
        "[MSIX-03][services] pre-test hook entered (ctrl {ci})"
    );

    // RAII 句柄: 作用域结束 (含提前 return) 即归还 DMA 缓冲
    let Some(buf) = nvme_alloc_dma_buffer(PAGE_SIZE as usize) else {
        slog_warn!(Driver, "[MSIX-03][services] DMA alloc failed, skip");
        return;
    };
    let buf_phys = buf.dma_addr().as_u64();

    // 锁内提交: IF=0 无中断窗口, 计数捕获与提交原子 (无丢失唤醒)
    let cmd = fw_nvme::NvmeCommand::read(1, 0, 1, buf_phys);
    let submitted = {
        let mut controllers = NVME_CONTROLLERS.lock();
        controllers
            .get_mut(ci)
            .map_or(Err(()), |c| c.io_submit_isr(cmd))
    };
    let Ok(before) = submitted else {
        slog_warn!(Driver, "[MSIX-03][services] io submit failed, skip");
        return;
    };

    // IF 窗口内等待 ISR 处理计数变化 (hlt 让出流水线, MSI-X 投递唤醒)
    let handled = nvme_with_interrupts_enabled(|| {
        let mut timeout = 5_000_000u64;
        loop {
            let now = {
                NVME_CONTROLLERS
                    .lock()
                    .get(ci)
                    .map_or(before, nvme::NvmeController::io_isr_processed)
            };
            if now != before {
                return true;
            }
            timeout -= 1;
            if timeout == 0 {
                return false;
            }
            crate::framework::cpu::arch::halt();
        }
    });

    slog_info!(
        Driver,
        "[MSIX-03][services] ISR-driven io read (ctrl {ci}): {}",
        if handled { "Ok" } else { "Err(timeout)" }
    );
}

/// 初始化存储子系统并注册块设备到 Chitin (services 权威)
///
/// 扫描 PCI 发现 AHCI/NVMe 控制器 → services 控制器初始化 → 全局注册表 →
/// `_block` 适配器注册 Chitin。NVMe 为 MSI-X 中断驱动 (启用/ISR 注册失败
/// 回退轮询), 附 MSIX-03 受控自测。随后探测传统 ATA PIO 通道 (经由
/// framework `IoPort` safe 代理) 并注册 `ata0-3`。aarch64 (QEMU virt)
/// 无 PCI AHCI/NVMe/ATA, virtio-blk 已由 services 编排。
///
/// SIMPLIFIED: 错误降级为日志不传播 (char_init 同模式, 逻辑错误降级原则)。
#[cfg(target_arch = "x86_64")]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::too_many_lines,
    reason = "PCI 分支流程内联展开 (AHCI/NVMe 两分支); 拆分需传跨分支上下文增加间接层, 当前优先 expect 兜底"
)]
pub fn storage_init() {
    use crate::framework::chitin::register_block_device;
    use crate::framework::driver::BlockDevice;
    use crate::framework::mm::PAGE_SIZE;
    use crate::framework::pci;

    // Step 1: 确保 PCI 子系统已初始化 (幂等)
    let pci_count = pci::init();
    if pci_count == 0 {
        slog_warn!(Driver, "storage_init: no PCI devices found");
    }

    // MSI-X 分发契约注册 (DECISION-K 模式): framework ISR handler 排空
    // framework 注册表后转发 services 注册表。先于任何 enable_msix 调用,
    // 保证中断投递时分发回调已就位 (OnceLock set-once, 重复注册 fail-quiet)。
    let _ =
        crate::framework::driver::storage::nvme_register_services_msix_dispatch(nvme_msix_dispatch);

    // Step 2: 扫描 PCI 总线寻找存储控制器
    let devices = pci::scan_all_buses();

    let mut ahci_found = 0u32;
    let mut nvme_found = 0u32;
    let mut ata_found = 0u32;
    // 待注册端口/命名空间: (控制器索引, 端口索引) / (控制器索引, 命名空间 ID)
    let mut ahci_ports: Vec<(usize, usize)> = Vec::new();
    let mut nvme_ns: Vec<(usize, u32)> = Vec::new();

    for dev in &devices {
        if dev.class_code != PCI_CLASS_STORAGE {
            continue;
        }

        match dev.subclass_code {
            PCI_SUBCLASS_AHCI => {
                // AHCI 控制器 - 使用 BAR5 (偏移 0x24)
                let bar = dev.bars[5].base_addr;
                if bar == 0 || bar == 0xFFFF_FFFF {
                    slog_warn!(
                        Driver,
                        "AHCI: device {:02X}:{:02X}.{} has no valid BAR5",
                        dev.bus,
                        dev.device,
                        dev.function
                    );
                    continue;
                }

                let mmio_base = (bar as usize) & !(PAGE_SIZE as usize - 1);
                slog_info!(
                    Driver,
                    "AHCI: found at {:02X}:{:02X}.{}, BAR5=0x{:X}",
                    dev.bus,
                    dev.device,
                    dev.function,
                    mmio_base
                );

                if let Some(mut controller) =
                    ahci::AhciController::new(mmio_base as u64, PAGE_SIZE as usize)
                {
                    if controller.init_controller() {
                        // 枚举有盘端口 (端口索引即 services 控制器 ports 向量下标)
                        let active: Vec<usize> = (0..controller.port_count())
                            .filter(|&pi| {
                                controller
                                    .get_port(pi)
                                    .map_or(false, |port| port.device_present)
                            })
                            .collect();

                        let mut registry = AHCI_CONTROLLERS.lock();
                        let ci = registry.len();
                        for pi in active {
                            ahci_ports.push((ci, pi));
                        }
                        registry.push(controller);
                        ahci_found += 1;
                    } else {
                        slog_warn!(Driver, "AHCI: init_controller failed, skip");
                    }
                } else {
                    slog_warn!(Driver, "AHCI: controller alloc failed (IoMem), skip");
                }
            }

            PCI_SUBCLASS_NVME => {
                // NVMe 控制器 - 使用 BAR0
                let bar = dev.bars[0].base_addr;
                if bar == 0 || bar == 0xFFFF_FFFF {
                    slog_warn!(
                        Driver,
                        "NVMe: device {:02X}:{:02X}.{} has no valid BAR0",
                        dev.bus,
                        dev.device,
                        dev.function
                    );
                    continue;
                }

                let mmio_base = (bar as usize) & !(PAGE_SIZE as usize - 1);
                slog_info!(
                    Driver,
                    "NVMe: found at {:02X}:{:02X}.{}, BAR0=0x{:X}",
                    dev.bus,
                    dev.device,
                    dev.function,
                    mmio_base
                );

                // BAR0 MMIO 区域 0x2000 (HBA 寄存器 0x1000 + I/O 队列门铃区)
                if let Some(mut controller) = nvme::NvmeController::new(mmio_base as u64, 0x2000) {
                    // 分段编排 (时序契约): 基础初始化 + Identify (轮询) → MSI-X
                    // 接线 → I/O 队列创建。**I/O CQ 必须在 MSI-X 启用后创建**:
                    // QEMU `nvme_init_cq` 仅在 msix_enabled 时调用
                    // `msix_vector_use`, 对未 use 向量 `msix_notify` 静默丢弃 —
                    // 先建队列后启用 MSI-X 会导致完成中断永不到达 (MSIX-04 冒烟实证)。
                    if !controller.init_controller() || !controller.identify_controller() {
                        slog_warn!(Driver, "NVMe: init failed (poll mode), skip");
                        continue;
                    }
                    if controller.namespace_count() > 0 {
                        controller.identify_namespace(1);
                    }

                    // MSI-X 接线 (DECISION-H 2 号子步): 启用 + 注册 ISR,
                    // 任一失败保持轮询 (irq_vector = None)
                    if let Some(vector) = nvme::NvmeController::enable_msix(dev) {
                        match crate::framework::driver::storage::nvme_register_msix_isr(vector) {
                            Ok(()) => {
                                controller.set_irq_vector(vector);
                                slog_info!(Driver, "NVMe: MSI-X enabled, vector={vector}");
                            }
                            Err(e) => {
                                slog_warn!(
                                    Driver,
                                    "NVMe: MSI-X ISR register failed on vector {vector}: {e}, falling back to poll"
                                );
                            }
                        }
                    } else {
                        slog_info!(Driver, "NVMe: MSI-X unavailable, using poll mode");
                    }

                    // I/O 队列创建 (CQ 中断向量字段恒为 Table entry 0)
                    if !controller.create_io_queue() {
                        slog_warn!(Driver, "NVMe: io queue creation failed, skip");
                        continue;
                    }

                    let ci;
                    {
                        let ns_count = controller.namespace_count();
                        let size = controller.namespace_size();

                        let mut registry = NVME_CONTROLLERS.lock();
                        ci = registry.len();
                        for nsid in 1..=ns_count {
                            if size > 0 {
                                nvme_ns.push((ci, nsid));
                            }
                        }
                        registry.push(controller);
                    }
                    nvme_found += 1;

                    // MSIX-03 services 受控自测 (注册表锁已释放, 中断窗口验证)
                    nvme_msix03_selftest(ci);
                } else {
                    slog_warn!(Driver, "NVMe: controller alloc failed (IoMem), skip");
                }
            }

            _ => {
                // 其他存储子类 (IDE, RAID 等) 静默跳过
            }
        }
    }

    // Step 3: 将 AHCI 端口注册到 Chitin (唯一注册入口)
    for (ci, pi) in ahci_ports {
        if let Some(dev) = ahci::AhciBlockDevice::new(ci, pi) {
            let sectors = dev.blk_total_sectors();
            let dev_name = alloc::format!("ahci{ci}-p{pi}");
            let name_leaked: &'static str = dev_name.leak();
            register_block_device(name_leaked, dev, None);
            slog_info!(
                Driver,
                "AHCI: ctrl={} port={} registered, {} sectors",
                ci,
                pi,
                sectors
            );
        }
    }

    // Step 4: 将 NVMe 命名空间注册到 Chitin (唯一注册入口)
    for (ci, nsid) in nvme_ns {
        if let Some(dev) = nvme::NvmeBlockDevice::new(ci, nsid) {
            let sectors = dev.blk_total_sectors();
            let dev_name = alloc::format!("nvme{ci}-ns{nsid}");
            let name_leaked: &'static str = dev_name.leak();
            register_block_device(name_leaked, dev, None);
            slog_info!(
                Driver,
                "NVMe: ctrl={} nsid={} registered, {} sectors",
                ci,
                nsid,
                sectors
            );
        }
    }

    // Step 5: 探测传统 ATA PIO 通道并注册块设备 (framekernel 阶段 3 回迁)
    //
    // ATA PIO 经 framework `IoPort` safe 代理访问端口 (services 0 unsafe)。
    // 无 ATA 控制器 (典型: 仅有 AHCI/NVMe 的机器) 时探测落空, 静默跳过。
    if let Some(mut controller) = ata::AtaController::new() {
        if controller.init() {
            ata_found = controller.detected_device_count() as u32;
            // 先入册 (AtaBlockDevice 经注册表查找控制器), 再注册块设备
            ATA_CONTROLLERS.lock().push(controller);
            for drive in 0..ata::MAX_ATA_DEVICES as u8 {
                if let Some(dev) = ata::AtaBlockDevice::new(drive) {
                    let sectors = dev.blk_total_sectors();
                    let dev_name = alloc::format!("ata{drive}");
                    let name_leaked: &'static str = dev_name.leak();
                    register_block_device(name_leaked, dev, None);
                    slog_info!(
                        Driver,
                        "ATA: drive={} registered, {} sectors",
                        drive,
                        sectors
                    );
                }
            }
        }
    }

    slog_info!(
        Driver,
        "storage (services): {} AHCI, {} NVMe, {} ATA initialized (NVMe MSI-X if available)",
        ahci_found,
        nvme_found,
        ata_found
    );
}
