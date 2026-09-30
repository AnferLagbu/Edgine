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

use crate::framework::driver::hotplug::DeviceLocation;
use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
// 以下仅用于 x86_64 门控代码 (storage_init / 热插拔重扫描 / MSIX-03 自测)
#[cfg(target_arch = "x86_64")]
use crate::framework::driver::BlockDevice;
#[cfg(target_arch = "x86_64")]
use crate::framework::driver::hotplug::{BusType, HotplugEvent};
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

/// PCI 配置空间 Secondary Bus Number 偏移 (端口二级总线号)。
///
/// `PcieHotplugSlot` 只识别 Root Port / Downstream Port, 故热插拔事件里的
/// BDF 是**端口**的 BDF, 而存储控制器挂在该端口的二级总线之下。
#[cfg(target_arch = "x86_64")]
const PCI_CFG_SECONDARY_BUS: u8 = 0x19;

/// 一个已探测的存储控制器的热插拔台账。
#[cfg(target_arch = "x86_64")]
struct ProbedController {
    bus: u8,
    device: u8,
    function: u8,
    /// 控制器在 AHCI/NVMe 注册表中的槽位 (探测失败为 `None`)。
    slot: Option<usize>,
    /// 该控制器下已注册的 Chitin 块设备下标。
    ///
    /// 控制器移除后**保留**该列表: 移除事件要先经重枚举再交监听器,
    /// 监听器需据此解析出要注销的块设备 (见 `drives_for_location`)。
    drives: Vec<u8>,
    /// 当前是否在线 (移除后置 `false`, 重插后复原)。
    installed: bool,
}

/// 已探测存储控制器台账 (启动期全量扫描 + 热插拔增量维护)。
#[cfg(target_arch = "x86_64")]
static PROBED: Mutex<Vec<ProbedController>> = Mutex::new(Vec::new());

/// 将控制器装入注册表并返回其槽位索引。
///
/// `AhciBlockDevice` / `NvmeBlockDevice` 缓存 `controller_index` 并在 I/O 时
/// 经 `registry.get_mut(idx)` 查回控制器, 因此**不可从注册表中间删除**元素
/// (会使其它在线块设备的索引错位)。热插拔重插通过 `slot_hint` 覆盖同槽位
/// 复用, 避免反复插拔导致注册表无限增长与 MMIO 映射泄漏。
#[cfg(target_arch = "x86_64")]
fn install_controller<T>(registry: &mut Vec<T>, slot_hint: Option<usize>, controller: T) -> usize {
    if let Some(slot) = slot_hint {
        if slot < registry.len() {
            registry[slot] = controller;
            return slot;
        }
    }
    registry.push(controller);
    registry.len() - 1
}

/// 单个控制器的探测结果。
#[cfg(target_arch = "x86_64")]
struct ProbeResult {
    slot: Option<usize>,
    drives: Vec<u8>,
}

#[cfg(target_arch = "x86_64")]
impl ProbeResult {
    fn failed() -> Self {
        Self {
            slot: None,
            drives: Vec::new(),
        }
    }
}

/// 探测单个 AHCI 控制器, 初始化并注册其有盘端口的块设备。
///
/// `slot_hint` 为重插场景下复用的注册表槽位 (`None` 表示首次探测)。
#[cfg(target_arch = "x86_64")]
// 有意窄化: Chitin 全局下标当前以 u8 表示块设备编号
#[expect(clippy::cast_possible_truncation)]
fn probe_ahci(dev: &crate::framework::pci::PciDevice, slot_hint: Option<usize>) -> ProbeResult {
    use crate::framework::chitin::register_block_device;
    use crate::framework::mm::PAGE_SIZE;

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
        return ProbeResult::failed();
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

    let Some(mut controller) = ahci::AhciController::new(mmio_base as u64, PAGE_SIZE as usize)
    else {
        slog_warn!(Driver, "AHCI: controller alloc failed (IoMem), skip");
        return ProbeResult::failed();
    };
    if !controller.init_controller() {
        slog_warn!(Driver, "AHCI: init_controller failed, skip");
        return ProbeResult::failed();
    }

    // 枚举有盘端口 (端口索引即 services 控制器 ports 向量下标)
    let active: Vec<usize> = (0..controller.port_count())
        .filter(|&pi| {
            controller
                .get_port(pi)
                .map_or(false, |port| port.device_present)
        })
        .collect();

    let ci = install_controller(&mut AHCI_CONTROLLERS.lock(), slot_hint, controller);

    let mut drives = Vec::new();
    for pi in active {
        if let Some(dev) = ahci::AhciBlockDevice::new(ci, pi) {
            let sectors = dev.blk_total_sectors();
            let dev_name = alloc::format!("ahci{ci}-p{pi}");
            let idx = register_block_device(dev_name.leak(), dev, None);
            drives.push(idx as u8);
            slog_info!(
                Driver,
                "AHCI: ctrl={} port={} registered, {} sectors",
                ci,
                pi,
                sectors
            );
        }
    }

    ProbeResult {
        slot: Some(ci),
        drives,
    }
}

/// 探测单个 NVMe 控制器, 初始化并注册其命名空间的块设备。
///
/// `run_selftest` 仅为启动期 (`storage_init`) 置真: MSIX-03 受控自测内部会
/// 自旋并 `halt()` 等待中断, 不可在软中断 (热插拔重枚举) 上下文执行。
#[cfg(target_arch = "x86_64")]
// 有意窄化: Chitin 全局下标当前以 u8 表示块设备编号
#[expect(clippy::cast_possible_truncation)]
fn probe_nvme(
    dev: &crate::framework::pci::PciDevice,
    slot_hint: Option<usize>,
    run_selftest: bool,
) -> ProbeResult {
    use crate::framework::chitin::register_block_device;
    use crate::framework::mm::PAGE_SIZE;

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
        return ProbeResult::failed();
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
    let Some(mut controller) = nvme::NvmeController::new(mmio_base as u64, 0x2000) else {
        slog_warn!(Driver, "NVMe: controller alloc failed (IoMem), skip");
        return ProbeResult::failed();
    };

    // 分段编排 (时序契约): 基础初始化 + Identify (轮询) → MSI-X
    // 接线 → I/O 队列创建。**I/O CQ 必须在 MSI-X 启用后创建**:
    // QEMU `nvme_init_cq` 仅在 msix_enabled 时调用 `msix_vector_use`,
    // 对未 use 向量 `msix_notify` 静默丢弃 — 先建队列后启用 MSI-X
    // 会导致完成中断永不到达 (MSIX-04 冒烟实证)。
    if !controller.init_controller() || !controller.identify_controller() {
        slog_warn!(Driver, "NVMe: init failed (poll mode), skip");
        return ProbeResult::failed();
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
        return ProbeResult::failed();
    }

    let ns_count = controller.namespace_count();
    let size = controller.namespace_size();
    let ci = install_controller(&mut NVME_CONTROLLERS.lock(), slot_hint, controller);

    let mut drives = Vec::new();
    if size > 0 {
        for nsid in 1..=ns_count {
            if let Some(dev) = nvme::NvmeBlockDevice::new(ci, nsid) {
                let sectors = dev.blk_total_sectors();
                let dev_name = alloc::format!("nvme{ci}-ns{nsid}");
                let idx = register_block_device(dev_name.leak(), dev, None);
                drives.push(idx as u8);
                slog_info!(
                    Driver,
                    "NVMe: ctrl={} nsid={} registered, {} sectors",
                    ci,
                    nsid,
                    sectors
                );
            }
        }
    }

    // MSIX-03 services 受控自测 (注册表锁已释放, 中断窗口验证)
    if run_selftest {
        nvme_msix03_selftest(ci);
    }

    ProbeResult {
        slot: Some(ci),
        drives,
    }
}

/// 判断台账中的控制器是否仍存在于最新设备表中 (按 BDF 匹配)。
///
/// SIMPLIFIED: 只比对 BDF, 不校验类码; 同一 BDF 被非存储设备复用的场景
/// 在现实硬件上不会发生, 待出现多级 PCIe 交换机拓扑时再细化。
#[cfg(target_arch = "x86_64")]
fn is_present(devices: &[crate::framework::pci::PciDevice], bus: u8, device: u8, function: u8) -> bool {
    devices
        .iter()
        .any(|d| d.bus == bus && d.device == device && d.function == function)
}

/// 全量扫描存储控制器: 幂等地探测新增/重插控制器并注册其块设备。
///
/// 返回 (`AHCI` 在线数, `NVMe` 在线数)。已在线的控制器直接跳过, 不重复初始化。
#[cfg(target_arch = "x86_64")]
fn storage_scan_with(devices: &[crate::framework::pci::PciDevice], run_selftest: bool) -> (u32, u32) {
    let mut ahci_found = 0u32;
    let mut nvme_found = 0u32;

    for dev in devices {
        if dev.class_code != PCI_CLASS_STORAGE {
            continue;
        }
        let is_ahci = dev.subclass_code == PCI_SUBCLASS_AHCI;
        if !is_ahci && dev.subclass_code != PCI_SUBCLASS_NVME {
            // 其他存储子类 (IDE, RAID 等) 静默跳过
            continue;
        }

        // 已在线 (已安装) 的控制器幂等跳过; 移除过的记录取出槽位供复用。
        let (online, slot_hint) = {
            let probed = PROBED.lock();
            let mut online = false;
            let mut slot_hint = None;
            for r in probed.iter() {
                if r.bus == dev.bus && r.device == dev.device && r.function == dev.function {
                    if r.installed {
                        online = true;
                    } else {
                        slot_hint = r.slot;
                    }
                }
            }
            (online, slot_hint)
        };
        if online {
            continue;
        }

        let result = if is_ahci {
            probe_ahci(dev, slot_hint)
        } else {
            probe_nvme(dev, slot_hint, run_selftest)
        };
        if is_ahci && result.slot.is_some() {
            ahci_found += 1;
        } else if !is_ahci && result.slot.is_some() {
            nvme_found += 1;
        }

        let mut probed = PROBED.lock();
        if let Some(r) = probed.iter_mut().find(|r| {
            r.bus == dev.bus && r.device == dev.device && r.function == dev.function
        }) {
            r.slot = result.slot;
            r.drives = result.drives;
            r.installed = result.slot.is_some();
        } else {
            probed.push(ProbedController {
                bus: dev.bus,
                device: dev.device,
                function: dev.function,
                slot: result.slot,
                drives: result.drives,
                installed: result.slot.is_some(),
            });
        }
    }

    (ahci_found, nvme_found)
}

/// 将已从总线消失的控制器标记为移除, 并注销其块设备 (Chitin 墓碑)。
///
/// **保留**台账中的 `slot` 与 `drives`: 槽位供重插复用 (见 `install_controller`),
/// 块设备列表供监听器在随后的事件分发中解析出待注销的驱动编号。
#[cfg(target_arch = "x86_64")]
fn remove_stale_controllers(devices: &[crate::framework::pci::PciDevice]) {
    use crate::framework::chitin::chitin_unregister_block;

    let mut probed = PROBED.lock();
    for r in probed.iter_mut() {
        if !r.installed || is_present(devices, r.bus, r.device, r.function) {
            continue;
        }
        for &drive in &r.drives {
            chitin_unregister_block(drive);
        }
        r.installed = false;
        slog_info!(
            Driver,
            "storage: controller {:02X}:{:02X}.{} removed, {} block dev(s) tombstoned",
            r.bus,
            r.device,
            r.function,
            r.drives.len()
        );
    }
}

/// 重扫 PCI 总线并增量维护控制器/块设备 (热插拔重枚举回调)。
#[cfg(target_arch = "x86_64")]
fn storage_rescan() {
    use crate::framework::pci;

    let devices = pci::scan_all_buses();
    remove_stale_controllers(&devices);
    let _ = storage_scan_with(&devices, false);
}

/// framework 热插拔重枚举回调 (经 `register_reenum_hook` 注册)。
///
/// 在监听器处理事件**之前**被调用, 使 `PROBED` 台账与 Chitin 块设备表
/// 与最新总线状态一致。USB 等其他总线的事件不触发 PCI 重扫。
#[cfg(target_arch = "x86_64")]
fn storage_reenum_hook(event: &HotplugEvent) {
    if event_location(event).bus_type != BusType::Pcie {
        return;
    }
    storage_rescan();
}

/// 提取事件中的设备位置。
#[cfg(target_arch = "x86_64")]
fn event_location(event: &HotplugEvent) -> &DeviceLocation {
    match event {
        HotplugEvent::DeviceAdded { location }
        | HotplugEvent::DeviceRemoved { location }
        | HotplugEvent::SurpriseRemoval { location } => location,
    }
}

/// 解析 PCIe 热插拔端口下的存储块设备 (Chitin 下标)。
///
/// `PcieHotplugSlot` 只识别 Root Port / Downstream Port, 故热插拔事件中的
/// BDF 是**端口**的 BDF, 而存储控制器挂在该端口二级总线之下。此处读取端口
/// 配置空间偏移 0x19 的 Secondary Bus Number, 再匹配台账中总线号一致的
/// 控制器 (与 Linux `pciehp` 对端口 `secondary bus` 调 `pci_scan_slot` 同源)。
///
/// 返回空表示该位置下无已注册的存储块设备 (或总线类型非 PCIe)。
#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
pub fn drives_for_location(location: &DeviceLocation) -> Vec<u8> {
    if location.bus_type != BusType::Pcie {
        return Vec::new();
    }
    let secondary = crate::framework::pci::read_config_byte(
        location.bus,
        location.device,
        location.function,
        PCI_CFG_SECONDARY_BUS,
    );
    if secondary == 0 {
        return Vec::new();
    }
    PROBED
        .lock()
        .iter()
        .filter(|r| r.bus == secondary)
        .flat_map(|r| r.drives.iter().copied())
        .collect()
}

/// aarch64 (QEMU virt) 无 PCIe 热插拔槽位, 恒返回空。
#[cfg(not(target_arch = "x86_64"))]
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
pub fn drives_for_location(_location: &DeviceLocation) -> Vec<u8> {
    Vec::new()
}

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
/// 首次全量扫描 PCI (见 `storage_scan_with`) 发现 AHCI/NVMe 控制器 →
/// services 控制器初始化 → 全局注册表 → `_block` 适配器注册 Chitin。
/// NVMe 为 MSI-X 中断驱动 (启用/ISR 注册失败回退轮询), 附 MSIX-03 受控自测。
/// 随后探测传统 ATA PIO 通道 (经由 framework `IoPort` safe 代理) 并注册
/// `ata0-3`。aarch64 (QEMU virt) 无 PCI AHCI/NVMe/ATA, virtio-blk 已由
/// services 编排。
///
/// 同时向 framework 注册热插拔重枚举回调 (`storage_reenum_hook`): 后续的
/// 控制器插入/移除由 `storage_rescan` 增量维护, 无需重启。
///
/// SIMPLIFIED: 错误降级为日志不传播 (char_init 同模式, 逻辑错误降级原则)。
#[cfg(target_arch = "x86_64")]
pub fn storage_init() {
    use crate::framework::driver::hotplug::register_reenum_hook;
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

    // Step 2: 注册热插拔重枚举回调 (DECISION-K 模式)。framework 在分发每个
    // 热插拔事件给监听器之前调用它, 触发 `storage_rescan` 增量重扫总线,
    // 保证监听器看到的 Chitin 块设备表与最新总线状态一致。
    let _ = register_reenum_hook(storage_reenum_hook);

    // Step 3: 全量扫描 PCI 总线寻找存储控制器 (首次扫描附 NVMe MSIX-03 自测)
    let devices = pci::scan_all_buses();
    let (ahci_found, nvme_found) = storage_scan_with(&devices, true);

    // Step 4: 探测传统 ATA PIO 通道 (非热插拔总线, 仅启动期一次)
    let ata_found = probe_ata();

    slog_info!(
        Driver,
        "storage (services): {} AHCI, {} NVMe, {} ATA initialized (NVMe MSI-X if available)",
        ahci_found,
        nvme_found,
        ata_found
    );
}

/// 探测传统 ATA PIO 通道并注册块设备 (framekernel 阶段 3 回迁)。
///
/// ATA PIO 经 framework `IoPort` safe 代理访问端口 (services 0 unsafe)。
/// 无 ATA 控制器 (典型: 仅有 AHCI/NVMe 的机器) 时探测落空, 静默跳过。
/// ATA 通道挂板载/ISA 控制器, 不属 PCIe 热插拔范畴, 故仅启动期执行一次。
///
/// 返回探测到的设备数。
#[cfg(target_arch = "x86_64")]
// 有意窄化: `detected_device_count` 为 usize, 设备数远小于 u32 上限
#[expect(clippy::cast_possible_truncation)]
fn probe_ata() -> u32 {
    use crate::framework::chitin::register_block_device;

    let mut ata_found = 0u32;
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
                    slog_info!(Driver, "ATA: drive={} registered, {} sectors", drive, sectors);
                }
            }
        }
    }
    ata_found
}
