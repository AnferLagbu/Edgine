//! GICv3 (Generic Interrupt Controller v3) 初始化
//!
//! QEMU virt 机器使用 GICv3。本模块提供:
//!   - GIC 初始化 (GICD + GICR 基础配置)
//!   - Timer 中断使能
//!   - IRQ ACK/EOI 处理
//! 本文件 cast 多为寄存器地址偏移 (u32 → u64) 与中断号 (SGI < 16 已知).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::framework::racy_cell::RacyCell;

// ============================================================================
// GICv3 寄存器地址 (默认 QEMU virt, 可经设备树覆盖)
// ============================================================================

/// Distributor 基地址 (每个CPU共享)
///
/// 初值为 QEMU virt 的 TTBR1_EL1 高半区别名 (KERNEL_BASE + 0x0800_0000),
/// 确保在 TTBR0_EL1 切换到用户页表后仍可访问;
/// 引导期若从设备树探测到其他基址, 由 [`set_bases`] 覆盖为对应高半区别名。
static GICD_BASE: AtomicU64 = AtomicU64::new(0xFFFF_0000_0800_0000);
/// Redistributor RD frame 基地址 (每个CPU独立)
static GICR_BASE: AtomicU64 = AtomicU64::new(0xFFFF_0000_080A_0000);
/// Redistributor SGI frame 基地址 (SGI/PPI registers)
static GICR_SGI_BASE: AtomicU64 = AtomicU64::new(0xFFFF_0000_080B_0000);

/// Redistributor RD frame 到 SGI frame 的偏移 (ARM GICv3: 相邻 64KiB)
const GICR_SGI_OFFSET: u64 = 0x1_0000;

/// 以物理地址设置 GICv3 基址 (引导期由设备树探测结果调用)
///
/// GIC 始终使用 TTBR1_EL1 高半区别名 (VA = KERNEL_BASE + PA),
/// 确保在 TTBR0_EL1 切换到用户页表后仍可访问。
/// SGI frame 由 RD frame 基址推算 (RD + [`GICR_SGI_OFFSET`])。
pub fn set_bases(dist_pa: u64, redist_pa: u64) {
    let base = crate::framework::mm::KERNEL_BASE;
    GICD_BASE.store(base + dist_pa, Ordering::Release);
    GICR_BASE.store(base + redist_pa, Ordering::Release);
    GICR_SGI_BASE.store(base + redist_pa + GICR_SGI_OFFSET, Ordering::Release);
}

/// GICD 寄存器偏移
const GICD_CTLR: u64 = 0x0000; // Distributor Control
const GICD_TYPER: u64 = 0x0008; // Type
const GICD_IIDR: u64 = 0x000C; // Implementer ID
const GICD_IGROUPR: u64 = 0x0080; // Interrupt Group (0-31)
const GICD_ISENABLER: u64 = 0x0100; // Interrupt Set-Enable (0-31)
const GICD_ISPENDR: u64 = 0x0200; // Interrupt Set-Pending
const GICD_IPRIORITYR: u64 = 0x0400; // 中断优先级 (每路 8 bit)
const GICD_ITARGETSR: u64 = 0x0800; // Interrupt Target
const GICD_ICFGR: u64 = 0x0C00; // 中断配置 (电平/边沿触发)
const GICD_IROUTER: u64 = 0x6000; // 亲和路由 (GICv3, 每中断 64 bit)

/// `GICD_CTLR` 亲和路由使能位 (ARE_S bit4 / ARE_NS bit5)
const GICD_CTLR_ARE_MASK: u32 = (1 << 4) | (1 << 5);

/// GICR 寄存器偏移 (SGI + PPI)
const GICR_CTLR: u64 = 0x0000; // Redistributor Control
const GICR_WAKER: u64 = 0x0014; // Wake
const GICR_IGROUPR0: u64 = 0x0080; // SGI/PPI 中断分组
pub const GICR_ISENABLER0: u64 = 0x0100; // SGI/PPI 中断使能
const GICR_IPRIORITYR: u64 = 0x0400; // SGI/PPI 中断优先级
const GICR_ICFGR1: u64 = 0x0C04; // Configuration for PPIs

/// CPU Interface 寄存器 (系统寄存器, ICC_*)
/// 通过 MRS/MSR 访问

// SPIs 范围
const PPI_BASE: u32 = 16;
const SPI_BASE: u32 = 32;

/// ARM 架构定时器 PPI (Non-secure Physical Timer)
const TIMER_PPI: u32 = 30; // CNTPNSIRQ

// ============================================================================
// 寄存器读写辅助
// ============================================================================

#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn gicd_read(offset: u64) -> u32 {
    unsafe {
        core::arch::asm!("dsb sy");
        let val = read_volatile((GICD_BASE.load(Ordering::Acquire) + offset) as *const u32);
        core::arch::asm!("dsb sy");
        val
    }
}

#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn gicd_write(offset: u64, val: u32) {
    unsafe {
        core::arch::asm!("dsb sy");
        write_volatile((GICD_BASE.load(Ordering::Acquire) + offset) as *mut u32, val);
        core::arch::asm!("dsb sy");
    }
}

#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn gicr_read(offset: u64) -> u32 {
    unsafe {
        core::arch::asm!("dsb sy");
        let val = read_volatile((GICR_BASE.load(Ordering::Acquire) + offset) as *const u32);
        core::arch::asm!("dsb sy");
        val
    }
}

#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn gicr_write(offset: u64, val: u32) {
    unsafe {
        core::arch::asm!("dsb sy");
        write_volatile((GICR_BASE.load(Ordering::Acquire) + offset) as *mut u32, val);
        core::arch::asm!("dsb sy");
    }
}

#[inline(always)]
/// 读取 GICv3 Redistributor SGI 帧寄存器。
///
/// # Safety
///
/// 调用者需确保 GICR_SGI_BASE (0x080B_0000) 已映射且 Redistributor 已唤醒。
pub unsafe fn gicr_sgi_read(offset: u64) -> u32 {
    unsafe {
        core::arch::asm!("dsb sy");
        let val = read_volatile((GICR_SGI_BASE.load(Ordering::Acquire) + offset) as *const u32);
        core::arch::asm!("dsb sy");
        val
    }
}

#[inline(always)]
/// 写入 GICv3 Redistributor SGI 帧寄存器。
///
/// # Safety
///
/// 调用者需确保 GICR_SGI_BASE (0x080B_0000) 已映射且 Redistributor 已唤醒。
pub unsafe fn gicr_sgi_write(offset: u64, val: u32) {
    unsafe {
        core::arch::asm!("dsb sy");
        write_volatile((GICR_SGI_BASE.load(Ordering::Acquire) + offset) as *mut u32, val);
        core::arch::asm!("dsb sy");
    }
}

// ============================================================================
// GICv3 初始化
// ============================================================================

/// 初始化 GICv3 Distributor:
/// 1. 读取 GIC 类型和实现者信息
/// 2. 禁用所有中断
/// 3. 配置中断优先级 (全默认 0xA0)
/// 4. 使能 Distributor
/// 5. 使能 CPU Interface
///
/// # Safety
///
/// 调用前需确保 GICD_BASE (0x08000000) 已正确映射，MMU 已启用。
pub unsafe fn init_distributor() {
    unsafe {
        // 0. 读取 GIC 诊断信息
        let typer = gicd_read(GICD_TYPER);
        let iidr = gicd_read(GICD_IIDR);
        let num_spi = ((typer >> 5) & 0x1F) as u32 + 1; // ITLinesNumber: bits [5:0]
        let num_cpus = ((typer >> 8) & 0x07) as u32 + 1; // CPUNumber: bits [10:8]
        crate::klog_ffi!(
            klog_ffi_info,
            "[GIC] typer=0x{:08x} iidr=0x{:08x} spi={} cpus={}",
            typer,
            iidr,
            num_spi,
            num_cpus
        );

        // 1. 禁用 Distributor
        gicd_write(GICD_CTLR, 0);

        // 2. 设置所有 SPIs 为 Group 1 (Non-secure, IRQ 信号).
        //    Group 0 会触发 FIQ, 但 FIQ handler 仅为 unexpected_exception 桩.
        //    使用 Group 1 使中断走 handle_el1h_irq 正常处理路径.
        for i in 0..2 {
            gicd_write(GICD_IGROUPR + (i as u64 * 4), 0xFFFF_FFFF);
        }

        // 3. 设置中断优先级
        for i in 0..32 {
            gicd_write(GICD_IPRIORITYR + (i as u64 * 4), 0xA0A0_A0A0);
        }

        // 4. 使能 Distributor (Group0 + Group1)
        gicd_write(GICD_CTLR, 0x3);

        // 5. 设置 CPU interface target: PPIs to CPU0
        gicd_write(GICD_ITARGETSR, 0x0101_0101);
        gicd_write(GICD_ITARGETSR + 4, 0x0101_0101);
    }
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 初始化 GICv3 Redistributor (当前 CPU):
/// 1. 唤醒 redistributor
/// 2. 配置 SGI/PPI 分组
/// 3. 配置 PPI 触发模式
/// 4. 为当前核启用 SGIs/PPIs
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化，GICR_BASE 已映射。
pub unsafe fn init_redistributor() {
    unsafe {
        // 1. 唤醒 redistributor
        let waker = gicr_read(GICR_WAKER);

        gicr_write(GICR_WAKER, waker & !(1 << 1)); // 清除 ProcessorSleep (bit 1)

        // 等待 ChildrenAsleep == 0
        let mut wait_count = 0;
        while gicr_read(GICR_WAKER) & (1 << 2) != 0 {
            wait_count += 1;
            if wait_count > 1000000 {
                break;
            }
            core::hint::spin_loop();
        }

        // 2. 设置 PPI 优先级 (SGI frame)
        gicr_sgi_write(GICR_IPRIORITYR, 0xA0A0_A0A0);
        gicr_sgi_write(GICR_IPRIORITYR + 4, 0xA0A0_A0A0);
        gicr_sgi_write(GICR_IPRIORITYR + 8, 0xA0A0_A0A0);

        // 3. 配置 SGI/PPI 分组: 全部设为 Group 1 (Non-secure, IRQ 信号).
        //    Group 0 会触发 FIQ, 但 FIQ handler 仅为 unexpected_exception 桩.
        //    使用 Group 1 使 Timer PPI (30) 等中断走 handle_el1h_irq 正常路径.
        gicr_sgi_write(GICR_IGROUPR0, 0xFFFF_FFFF);

        // 4. 配置 PPI 触发模式: Timer PPI 为 level-triggered
        let icfgr1_val = gicr_sgi_read(GICR_ICFGR1);
        // Timer PPI = 30, 在 ICFGR1 中 (PPI 16-31)
        // bit[31:30] 对应 PPI 31, bit[29:28] 对应 PPI 30
        // level-triggered = 0b00
        let ppi30_shift = ((30 - 16) * 2) as u64;
        gicr_sgi_write(GICR_ICFGR1, icfgr1_val & !(0x3 << ppi30_shift));

        // 5. Timer PPI 低优先级
        let prio_addr = GICR_IPRIORITYR + ((TIMER_PPI as u64 / 4) * 4);
        let prio = gicr_sgi_read(prio_addr);
        let shift = ((TIMER_PPI % 4) * 8) as u64;
        gicr_sgi_write(prio_addr, (prio & !(0xFF << shift)) | (0x40 << shift));

        // 6. Enable redistributor
        gicr_write(GICR_CTLR, 0x1); // Enable
    }
}

/// 使能 CPU Interface (ICC_* 系统寄存器)
///
/// # Safety
///
/// 仅在 EL1 或更高特权级调用，需确保 Redistributor 已初始化。
pub unsafe fn init_cpu_interface() {
    unsafe {
        // 设置中断优先级掩码 (PMR): 允许所有优先级
        core::arch::asm!("msr icc_pmr_el1, {}", in(reg) 0xFFu64);

        // 设置 Binary Point (BPR1): 无优先级分组
        core::arch::asm!("msr icc_bpr1_el1, {}", in(reg) 0u64);

        // 启用 Group 0 + Group 1 中断
        core::arch::asm!("msr icc_igrpen0_el1, {}", in(reg) 1u64);
        core::arch::asm!("msr icc_igrpen1_el1, {}", in(reg) 1u64);

        // EOI 模式: 直接降优先级 (ICC_CTLR_EL1.EOImode = 0)
        let ctlr: u64;
        core::arch::asm!("mrs {}, icc_ctlr_el1", out(reg) ctlr);
        core::arch::asm!("msr icc_ctlr_el1, {}", in(reg) ctlr & !(1 << 1));
    }
}

/// 使能 Timer PPI 中断
///
/// # Safety
///
/// 调用前需确保 CPU Interface 已初始化。
pub unsafe fn enable_timer_ppi() {
    unsafe {
        let enable_offset = GICR_ISENABLER0;
        let bit = 1u32 << (TIMER_PPI % 32);
        gicr_sgi_write(enable_offset, bit);
    }
}

/// 获取中断 ID (IAR) — 用于 IRQ handler
pub fn acknowledge() -> u32 {
    let iar: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mrs {}, icc_iar1_el1", out(reg) iar);
    }
    iar as u32
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 中断完成 (EOI)
pub fn end_of_interrupt(intid: u32) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("msr icc_eoir1_el1, {}", in(reg) intid as u64);
    }
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 发送 EOI 并解除优先级 (drop priority)
pub fn deactivate(intid: u32) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("msr icc_dir_el1, {}", in(reg) intid as u64);
    }
}

/// 完整 GIC 初始化流程
///
/// # Safety
///
/// 仅在启动阶段调用，需确保 MMU 已启用且 GIC MMIO 区域已映射。
pub unsafe fn init() {
    unsafe {
        init_distributor();
        init_redistributor();
        init_cpu_interface();
        enable_timer_ppi();
    }
}

// ============================================================================
// 中断管理 API
// ============================================================================

/// 校验中断号是否为有效的 PPI
pub fn is_ppi(irq: u32) -> bool {
    (PPI_BASE..SPI_BASE).contains(&irq)
}

/// 校验中断号是否为有效的 SPI
pub fn is_spi(irq: u32) -> bool {
    irq >= SPI_BASE
}

/// 校验中断号是否在有效范围内
pub fn is_valid_irq(irq: u32) -> bool {
    irq < SPI_BASE + 960 // GICv3 最多支持 1024 个中断
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 使能 SPI 中断
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn enable_spi(irq: u32) {
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ISENABLER + ((irq / 32) as u64 * 4);
        let bit = 1u32 << (irq % 32);
        gicd_write(reg_offset, bit);
    }
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 禁用 SPI 中断
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn disable_spi(irq: u32) {
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ISENABLER + ((irq / 32) as u64 * 4);
        let bit = 1u32 << (irq % 32);
        // GICD_ICENABLER 与 ISENABLER 偏移相同, 写 1 禁用
        gicd_write(reg_offset + 0x80, bit);
    }
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 设置 SPI 中断为 pending
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn set_spi_pending(irq: u32) {
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ISPENDR + ((irq / 32) as u64 * 4);
        let bit = 1u32 << (irq % 32);
        gicd_write(reg_offset, bit);
    }
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 配置 SPI 中断为 level-triggered
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn configure_spi_level(irq: u32) {
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ICFGR + ((irq / 16) as u64 * 4);
        let shift = ((irq % 16) * 2) as u64;
        let val = gicd_read(reg_offset);
        // bit 0 = 0 for level-triggered, bit 1 = 0 for inactive
        gicd_write(reg_offset, val & !(3 << shift));
    }
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 配置 SPI 中断为 edge-triggered
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn configure_spi_edge(irq: u32) {
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ICFGR + ((irq / 16) as u64 * 4);
        let shift = ((irq % 16) * 2) as u64;
        let val = gicd_read(reg_offset);
        // bit 0 = 1 for edge-triggered
        gicd_write(reg_offset, val | (1 << shift));
    }
}

#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 读取 SPI 中断状态 (是否 pending)
pub fn is_spi_pending(irq: u32) -> bool {
    if !is_spi(irq) {
        return false;
    }
    let reg_offset = GICD_ISPENDR + ((irq / 32) as u64 * 4);
    let bit = 1u32 << (irq % 32);
    // SAFETY: 读取 GICD 寄存器, 调用方保证 MMIO 已映射
    unsafe { gicd_read(reg_offset) & bit != 0 }
}

// ============================================================================
// 设备 SPI 分发表 (中断驱动 I/O)
// ============================================================================
//
// 承载 framework 内部各设备子系统 (网卡等) 的 SPI 中断处理程序:
//   - 启动期由各子系统经 [`register_device_spi`] 单线程注册 (GIC 已初始化);
//   - 运行期由 IRQ 异常路径经 [`dispatch_device_spi`] 在 ACK 之后、EOI 之前分发。

/// 设备 SPI 分发表长度 (覆盖 INTID 32..288)
const DEVICE_IRQ_TABLE_LEN: usize = 256;

/// 设备 SPI handler 分发表; 索引 = INTID - [`SPI_BASE`].
///
/// 启动期单线程写入 ([`register_device_spi`]), 中断上下文只读
/// ([`dispatch_device_spi`]); 二者天然串行, 无需加锁。
static DEVICE_IRQ_HANDLERS: RacyCell<[Option<fn()>; DEVICE_IRQ_TABLE_LEN]> =
    RacyCell::new([None; DEVICE_IRQ_TABLE_LEN]);

/// 写入 64 位 GICD 寄存器 (GICv3 IROUTER 等)
///
/// # Safety
///
/// 调用前需确保 GICD MMIO 已映射。
#[inline(always)]
unsafe fn gicd_write64(offset: u64, val: u64) {
    unsafe {
        core::arch::asm!("dsb sy");
        write_volatile((GICD_BASE.load(Ordering::Acquire) + offset) as *mut u64, val);
        core::arch::asm!("dsb sy");
    }
}

/// 将 SPI 路由到 CPU0
///
/// GICv3 的 SPI 路由寄存器取决于亲和路由是否使能:
///   - ARE=1 (GICv3 原生): 64 位 `GICD_IROUTER`, 写 Affinity=0 → CPU0;
///   - ARE=0 (GICv2 兼容): 8 位 `GICD_ITARGETSR`, bit0 → CPU0。
///
/// 读 `GICD_CTLR` 自校正选择, 兼容 QEMU 不同 gic-version 配置。
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化且 GICD MMIO 已映射。
unsafe fn route_spi_to_cpu0(irq: u32) {
    unsafe {
        if gicd_read(GICD_CTLR) & GICD_CTLR_ARE_MASK != 0 {
            // 亲和路由模式: IROUTER[irq] 为 64 位, Affinity 全 0 → CPU0
            gicd_write64(GICD_IROUTER + u64::from(irq) * 8, 0);
        } else {
            // GICv2 兼容模式: ITARGETSR 每 SPI 8 bit, bit0 → CPU0
            let reg = GICD_ITARGETSR + u64::from(irq / 4) * 4;
            let shift = (irq % 4) * 8;
            let val = gicd_read(reg);
            gicd_write(reg, (val & !(0xFFu32 << shift)) | (0x01u32 << shift));
        }
    }
}

/// 配置并使能一个设备 SPI
///
/// 依次完成: Group 1 归组 / 优先级 0xA0 / 电平触发 / 路由 CPU0 / 使能。
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化且 GICD MMIO 已映射。
unsafe fn configure_and_enable_device_spi(irq: u32) {
    unsafe {
        // 1. 分组 Group 1 (Non-secure, IRQ 信号). init_distributor 仅初始化了
        //    int 0..63 的 IGROUPR, INTID >= 64 的 SPI 默认落 Group 0 (FIQ),
        //    故按其所在 32 位字补写。
        let group_reg = GICD_IGROUPR + u64::from(irq / 32) * 4;
        let group_bit = 1u32 << (irq % 32);
        gicd_write(group_reg, gicd_read(group_reg) | group_bit);

        // 2. 优先级 0xA0 (与 init_distributor 全局默认一致)
        let prio_reg = GICD_IPRIORITYR + u64::from(irq / 4) * 4;
        let prio_shift = (irq % 4) * 8;
        let prio = gicd_read(prio_reg);
        gicd_write(prio_reg, (prio & !(0xFFu32 << prio_shift)) | (0xA0u32 << prio_shift));

        // 3. 电平触发 (设备中断常规语义)
        configure_spi_level(irq);

        // 4. 路由至 CPU0
        route_spi_to_cpu0(irq);

        // 5. 使能
        enable_spi(irq);
    }
}

/// 注册设备 SPI 中断处理程序
///
/// 供 framework 内部设备子系统在启动期为其中断源登记 handler; 注册成功后
/// 该 SPI 被配置为 Group 1 / 优先级 0xA0 / 电平触发 / 路由 CPU0 并使能。
///
/// # Errors
///
/// - `intid` 不在 SPI 范围 (< [`SPI_BASE`]);
/// - `intid` 超出分发表范围;
/// - 该 SPI 槽位已被占用。
pub fn register_device_spi(intid: u32, handler: fn()) -> Result<(), &'static str> {
    if !is_spi(intid) {
        return Err("register_device_spi: intid 非 SPI");
    }
    let idx = (intid - SPI_BASE) as usize;
    if idx >= DEVICE_IRQ_TABLE_LEN {
        return Err("register_device_spi: intid 超出分发表范围");
    }
    let table = DEVICE_IRQ_HANDLERS.get_mut();
    if table[idx].is_some() {
        return Err("register_device_spi: SPI 槽位已被占用");
    }
    table[idx] = Some(handler);
    // SAFETY: Distributor 已初始化且 GICD MMIO 已映射 (启动期早于本调用);
    //         启动期为单线程, 无并发注册。
    unsafe {
        configure_and_enable_device_spi(intid);
    }
    Ok(())
}

/// 分发设备 SPI 中断到已注册 handler
///
/// 由 aarch64 IRQ 异常路径在 ACK 之后、EOI 之前调用
/// (电平触发中断须在 EOI 前完成设备侧 ack, 否则 GIC 线路持续拉高)。
/// 返回 `true` 表示已找到并执行 handler。
pub fn dispatch_device_spi(intid: u32) -> bool {
    if !is_spi(intid) {
        return false;
    }
    let idx = (intid - SPI_BASE) as usize;
    if idx >= DEVICE_IRQ_TABLE_LEN {
        return false;
    }
    // 启动期注册完成后分发表不再写入, 中断上下文只读与注册天然串行。
    if let Some(handler) = DEVICE_IRQ_HANDLERS.map(|table| table[idx]) {
        handler();
        true
    } else {
        false
    }
}
