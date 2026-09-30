//! 存储设备驱动子系统 — framework 机制层 (Storage Mechanism Layer)
//!
//! DECISION-H storage 专项 3 号子步 (storage_init 退位) 后的职责边界:
//! - **机制保留**: NVMe wire 类型 / 队列 DMA 分配 / 提交与排空 safe wrapper /
//!   AHCI DMA fill 原语 / xHCI TRB 原语 / NVMe MSI-X ISR 编排 (IDT 注册 +
//!   services 分发契约槽)
//! - **业务退位**: PCI AHCI/NVMe 控制器探测、初始化、块设备注册已迁
//!   `services::driver::storage::storage_init` (services 权威, crate root 编排)
//! - **ATA 回迁**: 传统 ATA PIO 驱动已迁 `services::driver::storage::ata`
//!   (经 framework `IoPort` safe 代理做端口 I/O)
//!
//! ## 初始化流程 (退位后)
//!
//! ```text
//! storage_init()  [services, x86_64]
//!   ├── PCI::scan_all_buses()
//!   ├── for each AHCI device  → services AhciController + AhciBlockDevice 注册
//!   ├── for each NVMe device  → services NvmeController (MSI-X) + NvmeBlockDevice 注册
//!   └── ATA PIO 探测 → services AtaController + AtaBlockDevice 注册 (ata0-3)
//! ```

pub mod ahci;
pub mod nvme;

// 为 driver/mod.rs 方便而重导出关键类型 (机制 wire 类型; 控制器业务已迁 services)
pub use ahci::H2dFis;
pub use nvme::{NvmeCommand, NvmeCompletion};

#[cfg(target_arch = "x86_64")]
use crate::framework::arch::InterruptArch;
use crate::framework::dma_buf::{DmaDirection, DmaStream};
use crate::framework::iomem::IoMem;

#[cfg(target_arch = "x86_64")]
/// B07 MSI-X 完整接入: NVMe 中断路径 (端到端).
///
/// `register_nvme_msix_isr(irq, msi_vector)` 通过 `IdtManager::register_msi_irq`
/// 注册 NVMe 中断处理, ISR 经注册契约分发 services `handle_interrupt`
/// 处理完成队列 (框架仅持 IDT/MSI-X 编排机制, DECISION-H 3 号子步).
///
/// # Safety
///
/// `frame` 由 IDT 中断入口压栈, 指向保存的寄存器. NVMe ISR 不读取 frame
/// 内容, 仅作 IDT 签名要求.
extern "C" fn nvme_msix_irq_handler(_frame: *mut crate::framework::idt::InterruptFrame) {
    // B07: handle_irq 已自动 EOI (LAPIC 路径), 这里只需 dispatch 给 controller.
    // MSIX-03: 中断触发计数 — 首次 + 每 1000 次打印, 验证真实 MSI-X 投递路径.
    let count = NVME_MSIX_IRQ_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    if count <= 5 || count.is_multiple_of(1000) {
        crate::klog_info!(Driver, "[NVMe] MSI-X IRQ {} fired (total {})", count, count);
    }
    // DECISION-H storage 专项 3 号子步 (storage_init 退位): framework 控制器业务
    // 已删, ISR 编排仅转发 services 分发契约 (控制器状态机在 services 注册表).
    if let Some(dispatch) = NVME_SERVICES_DISPATCH.get() {
        dispatch();
    }
}

/// services 层 NVMe MSI-X 分发回调槽 (DECISION-H storage 专项 2 号子步)
///
/// 注册契约 (DECISION-K 模式, 同 register_pressure_classifier): framework 持有
/// IDT/MSI-X 编排机制 (机制留 framework), services 注册业务分发函数
/// (无捕获函数指针). 未注册时 handler 跳过 services 分发 (fail-quiet).
#[cfg(target_arch = "x86_64")]
static NVME_SERVICES_DISPATCH: crate::framework::sync::OnceLock<fn()> =
    crate::framework::sync::OnceLock::new();

/// 注册 services 层 NVMe MSI-X 分发回调 (services 可调用的 0 unsafe 入口)
///
/// # Errors
///
/// 回调槽已被占用 (重复注册) 时返回 `Err(已注册回调)`.
#[cfg(target_arch = "x86_64")]
pub fn nvme_register_services_msix_dispatch(dispatch: fn()) -> Result<(), fn()> {
    NVME_SERVICES_DISPATCH.set(dispatch)
}

/// 在开启中断的窗口内执行闭包 (MSIX-03 services 自测路径专用机制原语)
///
/// boot 上下文 `storage_init` 运行于 IF=0 (框架约定), 而 MSI-X 中断路径验证
/// 需要 IF=1 让 LAPIC 投递完成中断。本原语进入时开 IF, 返回前恢复关 IF —
/// 与 framework 版 MSIX-03 hook 的手写 enable/disable 序列等值, 收敛为
/// 单一安全入口供 services 复用 (0 unsafe)。
///
/// # 契约
///
/// 仅限 boot 单线程存储初始化上下文调用 (与 framework hook 同约束);
/// `f` 内不得调用可能依赖 IF=0 语义的低层机制。
#[cfg(target_arch = "x86_64")]
pub fn nvme_with_interrupts_enabled<R>(f: impl FnOnce() -> R) -> R {
    crate::framework::arch::CurrentArch::interrupt_enable();
    let r = f();
    crate::framework::arch::CurrentArch::interrupt_disable();
    r
}

/// NVMe MSI-X ISR 触发计数 (MSIX-03 验证)
///
/// 仅 x86_64 (MSI-X 路径专属, 与 `nvme_msix_irq_handler` 同 cfg).
#[cfg(target_arch = "x86_64")]
static NVME_MSIX_IRQ_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// 注册 NVMe MSI-X ISR (services 可调用的 0 unsafe 入口, DECISION-H 2 号子步)
///
/// framework 持有 IDT/MSI-X 编排机制: `msi_vector` 是 `msix_enable` 分配的
/// LAPIC 向量号, 内部换算 IDT 索引 (`irq = vector - IRQ_BASE`) 后注册统一
/// `nvme_msix_irq_handler` (双注册表分发, 见 handler 注释).
///
/// # Errors
///
/// `IdtManager::register_msi_irq` 失败 (irq 范围错) 时返回错误.
#[cfg(target_arch = "x86_64")]
pub fn nvme_register_msix_isr(msi_vector: u8) -> Result<(), &'static str> {
    use crate::framework::idt::IdtManager;
    let irq = msi_vector - crate::framework::idt::IRQ_BASE;
    let manager = IdtManager::instance();
    manager.register_msi_irq(irq, nvme_msix_irq_handler, "nvme-msix")?;
    // enable_irq 用于 PIC IRQ 路径; MSI vector 不需 enable (LAPIC 已 mask).
    // 但 IDT 抽象统一要求 enable_irq (否则 vector 被屏蔽). 调用以保持一致性.
    manager.enable_irq(irq);
    crate::klog_info!(
        Driver,
        "NVMe MSI-X ISR registered: vector={}, irq={}",
        msi_vector,
        irq
    );
    Ok(())
}

// ============================================================================
// Safe queue wrapper API (services 层可调用的 0 unsafe 入口)
//
// 将 framework 中 unsafe 的队列操作封装为 safe 函数,
// 使 services 层可执行完整的 NVMe/AHCI 驱动逻辑而不引入 unsafe。
// ============================================================================

// ── 本地常量 (来自 framework nvme.rs/ahci.rs 的私有常量副本) ──

/// Admin/I/O 队列深度
const NVME_QD: u32 = 64;
/// SQ 条目大小
const NVME_SQ_ENTRY: usize = 64;
/// CQ 条目大小
const NVME_CQ_ENTRY: usize = 16;
/// `NVMe` Doorbell 基址
const NVME_DB_BASE: usize = 0x1000;
/// AHCI 命令槽数量
const AHCI_CMD_SLOTS: usize = 32;
/// AHCI 命令头大小
const AHCI_CMD_HDR_SIZE: usize = 32;
/// AHCI 命令表大小
const AHCI_CMD_TBL_SIZE: usize = 256;

/// 分配 `NVMe` Admin 队列 (SQ + CQ DMA 内存)
///
/// 返回 `((sq_virt, sq_phys), (cq_virt, cq_phys))` — 虚拟地址供 CPU 侧队列
/// 访问 (direct-map, `virt = phys + KERNEL_BASE`), 物理地址供设备侧寄存器/
/// 命令写入。失败返回 None (DMA 分配不足)。
pub fn nvme_alloc_admin_queues() -> Option<((u64, u64), (u64, u64))> {
    use crate::framework::dma::get_dma;

    let dma = get_dma();
    if !dma.is_initialized() {
        return None;
    }

    let sq_size = NVME_QD as usize * NVME_SQ_ENTRY;
    let cq_size = NVME_QD as usize * NVME_CQ_ENTRY;

    let (sq_virt, sq_phys) = dma.alloc_coherent(sq_size)?;
    let (cq_virt, cq_phys) = dma.alloc_coherent(cq_size)?;

    Some(((sq_virt.0, sq_phys.0), (cq_virt.0, cq_phys.0)))
}

/// 分配 `NVMe` I/O 队列 (SQ + CQ DMA 内存)
///
/// 返回 `((sq_virt, sq_phys), (cq_virt, cq_phys))` — 虚拟地址供 CPU 侧队列
/// 访问, 物理地址供 Create CQ/SQ Admin 命令。
pub fn nvme_alloc_io_queues() -> Option<((u64, u64), (u64, u64))> {
    use crate::framework::dma::get_dma;

    let dma = get_dma();
    if !dma.is_initialized() {
        return None;
    }

    let sq_size = NVME_QD as usize * NVME_SQ_ENTRY;
    let cq_size = NVME_QD as usize * NVME_CQ_ENTRY;

    let (sq_virt, sq_phys) = dma.alloc_coherent(sq_size)?;
    let (cq_virt, cq_phys) = dma.alloc_coherent(cq_size)?;

    Some(((sq_virt.0, sq_phys.0), (cq_virt.0, cq_phys.0)))
}

/// 分配 DMA 缓冲区 (RAII 句柄) — NVMe / AHCI / xHCI 共用入口。
///
/// 物理页由 `BuddyFrameAlloc` 分配并清零, 句柄析构即按持有计数归还物理帧
/// (取代原 `alloc_coherent` + `free_coherent` 双键反查路径)。
fn alloc_dma_buffer(size: usize) -> Option<DmaStream> {
    use crate::framework::alloc::frame_alloc::{BuddyFrameAlloc, FrameAlloc};
    use crate::framework::dma::get_dma;
    use crate::framework::mm::PAGE_SIZE;

    if size == 0 {
        return None;
    }
    let pages = size.div_ceil(PAGE_SIZE as usize);
    let frame = BuddyFrameAlloc.alloc_pages(pages)?;
    // 清零: 设备读到的初值必须是已定义数据
    frame.zero();
    let stream = DmaStream::from_frame(frame, DmaDirection::Bidirectional).ok()?;
    // 保持既有 alloc_coherent 的"清零对设备可见"语义
    // (x86_64: CLFLUSH 循环; aarch64: DC CVAU + dsb ish)
    get_dma().cache_flush(stream.dma_addr().to_virt(), stream.size());
    Some(stream)
}

/// 分配 DMA 缓冲区, 返回持有该缓冲区的 RAII 句柄 —
/// 实际分配大小按页向上取整 (`DmaStream::size()` 可能大于 `size`)。
pub fn nvme_alloc_dma_buffer(size: usize) -> Option<DmaStream> {
    alloc_dma_buffer(size)
}

/// 向 `NVMe` Admin SQ 提交命令并等待完成
///
/// `cmd_ptr` — SQ DMA 区域虚拟地址
/// `cq_ptr` — CQ DMA 区域虚拟地址
/// `cmd` — 要提交的命令 (cid 会被覆盖)
/// `tail` — 当前 SQ tail, 提交后更新为 (tail+1) % depth
/// `cq_head` — 当前 CQ head, 完成后更新
/// `phase` — CQ phase bit, 完成后可能翻转
/// `depth` — 队列深度
/// `db_stride` — 门铃步长
/// `iomem` — `NVMe` BAR0 `IoMem` 句柄
///
/// 返回: `Ok(status_code)` 或 `Err(())` (超时/错误)
/// # Errors
/// 命令超时或设备返回非零状态码时返回 Err。
pub fn nvme_submit_admin_cmd(
    cmd_ptr: u64,
    cq_ptr: u64,
    cmd: nvme::NvmeCommand,
    tail: &mut u32,
    cq_head: &mut u32,
    phase: &mut u16,
    depth: u32,
    _db_stride: u32,
    iomem: &IoMem,
    cid: u16,
) -> Result<u16, ()> {
    // SAFETY: cmd_ptr/cq_ptr 由 DMA 分配保证有效; IoMem 确保 MMIO 安全
    unsafe {
        // 写入 SQ entry
        let sq = cmd_ptr as *mut nvme::NvmeCommand;
        let mut entry = cmd;
        entry.cid = cid;
        core::ptr::write_volatile(sq.add(*tail as usize), entry);

        // 更新 tail 并敲门铃
        let new_tail = (*tail + 1) % depth;
        *tail = new_tail;

        let db_offset = NVME_DB_BASE;
        iomem.write_u32(db_offset, new_tail);

        // 等待 CQ 完成
        let cq = cq_ptr as *const nvme::NvmeCompletion;
        let mut timeout = 5_000_000u64;
        loop {
            let entry = core::ptr::read_volatile(cq.add(*cq_head as usize));
            if (entry.status & 0x01) == *phase {
                // 更新 head
                let new_head = (*cq_head + 1) % depth;
                *cq_head = new_head;
                if new_head == 0 {
                    *phase ^= 1;
                }
                // 敲 CQ 门铃
                iomem.write_u32(db_offset + 4, new_head);

                let sc = (entry.status >> 1) & 0x7FF;
                if sc == 0 {
                    return Ok(sc);
                }
                crate::klog_warn!(
                    Driver,
                    "nvme_submit_admin_cmd: device error cid={cid} sc={sc:#X}",
                );
                return Err(());
            }
            timeout -= 1;
            if timeout == 0 {
                crate::klog_warn!(
                    Driver,
                    "nvme_submit_admin_cmd: timeout cid={cid} cq_head={cq_head} phase={}",
                    *phase
                );
                return Err(());
            }
            core::hint::spin_loop();
        }
    }
}

/// 向 `NVMe` I/O SQ 提交命令并等待完成
/// # Errors
/// 命令超时或设备返回非零状态码时返回 Err。
pub fn nvme_submit_io_cmd(
    cmd_ptr: u64,
    cq_ptr: u64,
    cmd: nvme::NvmeCommand,
    tail: &mut u32,
    cq_head: &mut u32,
    phase: &mut u16,
    depth: u32,
    _db_stride: u32,
    iomem: &IoMem,
    cid: u16,
    io_queue_db_offset: usize,
) -> Result<(), ()> {
    // SAFETY: cmd_ptr/cq_ptr 由 DMA 分配保证有效; IoMem 确保 MMIO 安全
    unsafe {
        let sq = cmd_ptr as *mut nvme::NvmeCommand;
        let mut entry = cmd;
        entry.cid = cid;
        core::ptr::write_volatile(sq.add(*tail as usize), entry);

        let new_tail = (*tail + 1) % depth;
        *tail = new_tail;

        // I/O SQ doorbell
        iomem.write_u32(io_queue_db_offset, new_tail);

        // 等待 CQ 完成
        let cq = cq_ptr as *const nvme::NvmeCompletion;
        let mut timeout = 5_000_000u64;
        loop {
            let entry = core::ptr::read_volatile(cq.add(*cq_head as usize));
            if (entry.status & 0x01) == *phase {
                let new_head = (*cq_head + 1) % depth;
                *cq_head = new_head;
                if new_head == 0 {
                    *phase ^= 1;
                }
                // I/O CQ doorbell
                iomem.write_u32(io_queue_db_offset + 4, new_head);

                let sc = (entry.status >> 1) & 0x7FF;
                if sc == 0 {
                    return Ok(());
                }
                return Err(());
            }
            timeout -= 1;
            if timeout == 0 {
                return Err(());
            }
            core::hint::spin_loop();
        }
    }
}

/// 向 `NVMe` I/O SQ 提交命令但不等待完成 (MSI-X 中断路径, DECISION-H 2 号子步)
///
/// 仅写 SQ entry + 敲 SQ 门铃, 完成处理由 ISR 侧 [`nvme_drain_completions`]
/// 执行。`sq_virt` 为 SQ DMA 区域**虚拟地址** (CPU 侧访问)。
///
/// # Errors
///
/// SQ entry 写入无返回值, 当前恒返 `Ok(())` (签名保留以统一提交路径错误面)。
#[expect(
    clippy::unnecessary_wraps,
    reason = "签名保留以统一提交路径错误面 (admin/I-O 提交链一致使用 ? 运算符)"
)]
pub fn nvme_submit_io_cmd_noblock(
    sq_virt: u64,
    cmd: nvme::NvmeCommand,
    tail: &mut u32,
    depth: u32,
    iomem: &IoMem,
    cid: u16,
    io_sq_db_offset: usize,
) -> Result<(), ()> {
    // SAFETY: sq_virt 由 DMA 分配保证有效; IoMem 确保 MMIO 安全
    unsafe {
        let sq = sq_virt as *mut nvme::NvmeCommand;
        let mut entry = cmd;
        entry.cid = cid;
        core::ptr::write_volatile(sq.add(*tail as usize), entry);

        let new_tail = (*tail + 1) % depth;
        *tail = new_tail;

        // I/O SQ doorbell
        iomem.write_u32(io_sq_db_offset, new_tail);
    }
    Ok(())
}

/// 从 `NVMe` CQ 排空已完成条目 (ISR 侧 0 unsafe 入口, DECISION-H 2 号子步)
///
/// 语义与 framework `NvmeController::handle_interrupt` 等值: 按 phase bit 循环
/// 收割完成条目, 推进 head (回绕翻转 phase), 排空后敲一次 CQ 门铃。
/// `cq_virt` 为 CQ DMA 区域**虚拟地址** (CPU 侧访问)。
///
/// 返回 `(排空条目数, 最后完成条目 status 原始字段; 无条目时为 0)`。
pub fn nvme_drain_completions(
    cq_virt: u64,
    cq_head: &mut u32,
    phase: &mut u16,
    depth: u32,
    iomem: &IoMem,
    cq_db_offset: usize,
) -> (usize, u16) {
    // SAFETY: cq_virt 由 DMA 分配保证有效; head/phase 由调用方在锁内独占推进
    unsafe {
        let cq = cq_virt as *const nvme::NvmeCompletion;
        let mut drained: usize = 0;
        let mut last_status: u16 = 0;
        loop {
            let entry = core::ptr::read_volatile(cq.add(*cq_head as usize));
            if (entry.status & 0x01) != *phase {
                break;
            }
            last_status = entry.status;
            let new_head = (*cq_head + 1) % depth;
            *cq_head = new_head;
            if new_head == 0 {
                *phase ^= 1;
            }
            drained += 1;
        }
        if drained > 0 {
            // 敲 CQ 门铃, 通知控制器已完成条目被处理
            iomem.write_u32(cq_db_offset, *cq_head);
        }
        (drained, last_status)
    }
}

/// 复制数据到 DMA 缓冲区 (write 路径)
pub fn nvme_copy_to_dma(dst_vaddr: u64, src: *const u8, len: usize) {
    // SAFETY: dst_vaddr 由 DMA 分配保证有效; 调用方保证 src 有效且 len 匹配
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst_vaddr as *mut u8, len);
    }
}

/// 从 DMA 缓冲区复制数据 (read 路径)
pub fn nvme_copy_from_dma(dst: *mut u8, src_vaddr: u64, len: usize) {
    // SAFETY: src_vaddr 由 DMA 分配保证有效; 调用方保证 dst 有效且 len 匹配
    unsafe {
        core::ptr::copy_nonoverlapping(src_vaddr as *const u8, dst, len);
    }
}

/// 清零 DMA 缓冲区
pub fn nvme_zero_dma(vaddr: u64, len: usize) {
    // SAFETY: vaddr 由 DMA 分配保证有效
    unsafe {
        core::ptr::write_bytes(vaddr as *mut u8, 0, len);
    }
}

/// AHCI 命令列表句柄 — services 层持有的 safe 句柄
pub struct AhciCmdListHandle {
    /// 命令列表 DMA 虚拟地址
    pub cmd_list_virt: u64,
    /// 命令列表 DMA 物理地址
    pub cmd_list_phys: u64,
    /// FIS 接收缓冲区 DMA 虚拟地址
    pub fis_virt: u64,
    /// FIS 接收缓冲区 DMA 物理地址
    pub fis_phys: u64,
    /// 命令表 DMA 虚拟地址
    pub cmd_table_virt: u64,
    /// 命令表 DMA 物理地址
    pub cmd_table_phys: u64,
}

impl AhciCmdListHandle {
    /// 命令列表物理地址 (64-bit)
    pub fn cmd_list_phys(&self) -> u64 {
        self.cmd_list_phys
    }

    /// FIS 缓冲区物理地址 (64-bit)
    pub fn fis_phys(&self) -> u64 {
        self.fis_phys
    }
}

/// 分配 AHCI 端口 DMA 资源 (命令列表 + FIS 缓冲区 + 命令表)
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
pub fn ahci_alloc_port_dma() -> Option<AhciCmdListHandle> {
    use crate::framework::dma::get_dma;
    use crate::framework::mm::PAGE_SIZE;

    let dma = get_dma();
    if !dma.is_initialized() {
        return None;
    }

    let cmd_list_size = AHCI_CMD_SLOTS * AHCI_CMD_HDR_SIZE;
    let fis_size = PAGE_SIZE as usize;
    let cmd_table_size = AHCI_CMD_TBL_SIZE;

    let (cmd_list_v, cmd_list_phys) = dma.alloc_coherent(cmd_list_size)?;
    let (fis_v, fis_phys) = dma.alloc_coherent(fis_size)?;
    let (cmd_table_v, cmd_table_phys) = dma.alloc_coherent(cmd_table_size)?;

    Some(AhciCmdListHandle {
        // 虚拟地址必须真实保留: services 经 cmd_list_virt 填充命令头,
        // 置 0 会把命令头写到虚拟地址 0 (页 0 野写), 设备侧读到全零
        // 命令头 (CFL=0/CTBA=0) 静默丢弃命令 (首次带盘冒烟实证)
        cmd_list_virt: cmd_list_v.0,
        cmd_list_phys: cmd_list_phys.0,
        fis_virt: fis_v.0,
        fis_phys: fis_phys.0,
        cmd_table_virt: cmd_table_v.0,
        cmd_table_phys: cmd_table_phys.0,
    })
}

/// AHCI DMA buffer 分配 (用于读写数据传输), 返回 RAII 句柄
pub fn ahci_alloc_dma_buffer(size: usize) -> Option<DmaStream> {
    alloc_dma_buffer(size)
}

/// 复制数据到 AHCI DMA 缓冲区
pub fn ahci_copy_to_dma(dst_vaddr: u64, src: *const u8, len: usize) {
    // SAFETY: dst_vaddr 由 DMA 分配保证有效; 调用方保证 src 有效且 len 匹配
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst_vaddr as *mut u8, len);
    }
}

/// 从 AHCI DMA 缓冲区复制数据
pub fn ahci_copy_from_dma(dst: *mut u8, src_vaddr: u64, len: usize) {
    // SAFETY: src_vaddr 由 DMA 分配保证有效; 调用方保证 dst 有效且 len 匹配
    unsafe {
        core::ptr::copy_nonoverlapping(src_vaddr as *const u8, dst, len);
    }
}

/// 填充 AHCI Command Header (slot 0)
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub fn ahci_fill_cmd_header(
    cmd_list_virt: u64,
    slot: u32,
    fis_len_dwords: u32,
    is_write: bool,
    prdt_len: u16,
    cmd_table_phys: u64,
) {
    use core::ptr::addr_of_mut;
    // SAFETY: cmd_list_virt 由 DMA 分配保证有效; slot < CMD_SLOTS
    unsafe {
        let hdr = (cmd_list_virt as *mut ahci::AhciCommandHeader).add(slot as usize);
        // DW0 位域 (AHCI 1.3.1 §4.2.2): 位 0-4 = CFL (FIS 长度, DW 计),
        // 位 6 = W (写方向), 位 12-15 = PMP, 位 16-31 = PRDTL (表项数)
        let flags: u32 = fis_len_dwords | (if is_write { 1 << 6 } else { 0 });
        let dw0_val = flags | (u32::from(prdt_len) << 16);
        addr_of_mut!((*hdr).dw0).write_volatile(dw0_val);
        // DW1 = PRDBC (已传字节, 硬件维护) 软件清零;
        // DW2/DW3 = 命令表基址低/高 32 位
        addr_of_mut!((*hdr).prdbc).write_volatile(0u32);
        addr_of_mut!((*hdr).ctba).write_volatile(cmd_table_phys as u32);
        addr_of_mut!((*hdr).ctbau).write_volatile((cmd_table_phys >> 32) as u32);
    }
}

/// 填充 AHCI H2D FIS 到命令表 CFIS 区域
///
/// 使用字节拷贝避免 packed struct 对齐问题。
/// `fis_src` — FIS 源指针, `fis_size` — 字节大小
pub fn ahci_fill_h2d_fis(cmd_table_virt: u64, fis_src: usize, fis_size: usize) {
    // SAFETY: cmd_table_virt 由 DMA 分配保证有效; fis_src 由调用方保证指向有效 FIS 内存
    unsafe {
        let cfis = cmd_table_virt as *mut u8;
        let src = fis_src as *const u8;
        core::ptr::copy_nonoverlapping(src, cfis, fis_size);
    }
}

/// 填充 AHCI PRDT entry (数据缓冲区物理地址 + 字节数)
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub fn ahci_fill_prdt(
    cmd_table_virt: u64,
    entry_index: usize,
    data_phys: u64,
    byte_count: u32,
    ioc: bool,
) {
    // SAFETY: cmd_table_virt 由 DMA 分配保证有效; entry_index < 8
    unsafe {
        let table = cmd_table_virt as *mut ahci::AhciCommandTable;
        (*table).prdt[entry_index] = ahci::PhysicalRegionDescriptor {
            dba: data_phys as u32,
            dbau: (data_phys >> 32) as u32,
            rsvd: 0,
            dbc: (byte_count - 1) | (if ioc { 1u32 << 31 } else { 0 }),
        };
    }
}

// ============================================================================
// xHCI Transfer Ring safe wrapper API (services 层可调用的 0 unsafe 入口)
//
// 将 framework 中 unsafe 的 Transfer Ring 操作封装为 safe 函数,
// 使 services 层可执行完整的 USB 传输逻辑而不引入 unsafe。
// ============================================================================

/// 向 Transfer Ring 写入一个 TRB (16 字节).
///
/// `ring_vaddr` — Transfer Ring DMA 虚拟地址
/// `index` — TRB 索引 (0-based)
/// `trb_src` — 源 TRB 字节指针 (调用方保证指向有效 16 字节内存)
pub fn xhci_write_trb(ring_vaddr: u64, index: u32, trb_src: *const u8) {
    // SAFETY: ring_vaddr 由 DMA 分配保证有效; trb_src 由调用方保证指向有效 16 字节内存
    unsafe {
        let dst = (ring_vaddr as *mut u8).add((index as usize) * 16);
        core::ptr::copy_nonoverlapping(trb_src, dst, 16);
    }
}

/// 从 Transfer Ring 读取一个 TRB (16 字节) 到缓冲区.
///
/// `ring_vaddr` — Transfer Ring DMA 虚拟地址
/// `index` — TRB 索引 (0-based)
/// `dst` — 目标缓冲区 (至少 16 字节)
pub fn xhci_read_trb(ring_vaddr: u64, index: u32, dst: *mut u8) {
    // SAFETY: ring_vaddr 由 DMA 分配保证有效; dst 由调用方保证有效
    unsafe {
        let src = (ring_vaddr as *const u8).add((index as usize) * 16);
        core::ptr::copy_nonoverlapping(src, dst, 16);
    }
}
