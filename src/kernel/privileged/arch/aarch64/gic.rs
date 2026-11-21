//! `GICv3` (Generic Interrupt Controller v3) 初始化
//!
//! QEMU virt 机器使用 GICv3。本模块提供:
//!   - GIC 初始化 (GICD + GICR 基础配置)
//!   - Timer 中断使能
//!   - IRQ ACK/EOI 处理
//!
//! 本文件 cast 多为寄存器地址偏移 (u32 → u64) 与中断号 (SGI < 16 已知).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::arch::gic_logic;
use crate::privileged::racy_cell::RacyCell;

// ============================================================================
// GICv3 寄存器地址 (默认 QEMU virt, 可经设备树覆盖)
// ============================================================================

/// Distributor 基地址 (每个CPU共享)
///
/// 初值为 QEMU virt 的 `TTBR1_EL1` 高半区别名 (`KERNEL_BASE` + `0x0800_0000`),
/// 确保在 `TTBR0_EL1` 切换到用户页表后仍可访问;
/// 引导期若从设备树探测到其他基址, 由 [`set_bases`] 覆盖为对应高半区别名。
static GICD_BASE: AtomicU64 = AtomicU64::new(0xFFFF_0000_0800_0000);
/// Redistributor RD frame 基地址 (每个CPU独立)
static GICR_BASE: AtomicU64 = AtomicU64::new(0xFFFF_0000_080A_0000);
/// Redistributor SGI frame 基地址 (SGI/PPI registers)
static GICR_SGI_BASE: AtomicU64 = AtomicU64::new(0xFFFF_0000_080B_0000);

/// Redistributor RD frame 到 SGI frame 的偏移 (ARM `GICv3`: 相邻 64KiB)
const GICR_SGI_OFFSET: u64 = 0x1_0000;

/// 每个 CPU 的 Redistributor 在 GICR 区域占用的步长
///
/// ARM `GICv3` 规定 GICR 区域内各 CPU 的 Redistributor 按 CPU 序号连续排布,
/// 每个 CPU 占 128 KiB = RD frame (64 KiB) + SGI frame (64 KiB)。
pub const GICR_STRIDE: u64 = 0x2_0000;

/// 以物理地址设置 `GICv3` 基址 (引导期由设备树探测结果调用)
///
/// GIC 始终使用 `TTBR1_EL1` 高半区别名 (VA = `KERNEL_BASE` + PA),
/// 确保在 `TTBR0_EL1` 切换到用户页表后仍可访问。
/// SGI frame 由 RD frame 基址推算 (RD + `GICR_SGI_OFFSET`)。
pub fn set_bases(dist_pa: u64, redist_pa: u64) {
    let base = crate::privileged::mm::KERNEL_BASE;
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
const GICD_ICFGR: u64 = 0x0C00; // 中断配置 (电平/边沿触发)
const GICD_IROUTER: u64 = 0x6000; // 亲和路由 (GICv3, 每中断 64 bit)

/// GICR 寄存器偏移 (SGI + PPI)
///
/// 注: 不定义 `GICR_CTLR` (0x0000) — 其 bit0 为 `EnableLPIs` (LPI 使能), 并非
/// "Redistributor 使能"位; 本项目未使用 LPI, 故不访问该寄存器.
const GICR_TYPER: u64 = 0x0008; // Type (只读: 亲和值 bits[63:32] + Last bit4)
const GICR_WAKER: u64 = 0x0014; // Wake
const GICR_IGROUPR0: u64 = 0x0080; // SGI/PPI 中断分组
pub const GICR_ISENABLER0: u64 = 0x0100; // SGI/PPI 中断使能
const GICR_IPRIORITYR: u64 = 0x0400; // SGI/PPI 中断优先级
const GICR_ICFGR1: u64 = 0x0C04; // Configuration for PPIs

// CPU Interface 寄存器 (系统寄存器, ICC_*)
// 通过 MRS/MSR 访问

// SPIs 范围
const PPI_BASE: u32 = 16;
const SPI_BASE: u32 = 32;

/// 内核使用的 SGI 编号 (Software Generated Interrupt, `GICv3` ID 0-15)。
///
/// SGI/PPI 的使能位属**每核 Redistributor** 私有状态 (`GICR_ISENABLER0`), 故全部
/// 内核 SGI 必须由每核初始化入口 [`init_per_cpu`] 显式使能。编号集中定义于本模块
/// (GIC 资源归属), 供 `exception` (接收路由) 与 `freg` (触发) 复用, 避免编号分散
/// 在多处导致某路 SGI 漏使能 (历史缺陷: SGI 13/14 从未使能, 接收侧永不响应)。
pub const TLB_SHOOTDOWN_SGI: u32 = 0xFD & 0xF;

/// 跨核重新调度 SGI 编号 (对应 x86_64 向量 `0xFE`, 低位 4 bit = 14)。
pub const RESCHEDULE_SGI: u32 = 0xFE & 0xF;

/// FREG恢复专用 SGI 编号 (`0x82` 语义的 aarch64 等价, 取 SGI 7)。
pub const FREG_RECOVERY_SGI: u32 = 7;

// === unsafe 依据单源 (本文件寄存器访问与初始化路径共用) ===
//
// 类别 D「GICD MMIO 寄存器访问」(`gicd_read` / `gicd_write` / `gicd_write64`):
// 地址 = `GICD_BASE.load(Acquire) + offset`, 转 `*const` / `*mut u32` (或 u64) 后走
// `read_volatile` / `write_volatile` —— **不构造引用**, 故无别名与生命周期面;
// volatile 保证编译器不合并/消除这些访问 (MMIO 有侧效应), 前后各一条 `dsb sy`
// 保证屏障两侧的访存次序. 地址有效性 (MMU 已启用 + GICD 帧已映射) 与 offset
// 合法性块内无法证明: `GICD_BASE` 是 `set_bases` 写入的高半区别名
// (`KERNEL_BASE + dist_pa`) 或初值 QEMU virt `0xFFFF_0000_0800_0000`, 映射由引导期
// 页表装配完成, 一律由各 fn `# Safety` 段作为调用方义务传入. 对齐面: GICD 帧基址
// 4 KiB 对齐, 本文件全部 offset 为 4 的倍数 (64 位 IROUTER 为 8 的倍数) ⇒ u32/u64
// 访问自然对齐. 写 GICD 改变中断投递语义 (分组/优先级/使能/路由), 其正确性属
// 调用时序职责, 不是本类别能保证的.
//
// 类别 R「GICR 帧 MMIO 寄存器访问」(`gicr_read_at` / `gicr_write_at` /
// `gicr_read64_at`): 与类别 D 同构, 差别是 base **由调用方传入** (本核 RD 帧或 SGI
// 帧的高半区别名, 由 `redist_frames` 按亲和值定位), 故"base 指向已映射、已唤醒且
// 属本核的帧"这一义务比类别 D 更强 —— 命中他人帧会把本核 SGI/PPI 配置写到别的核
// 上 (ISSUE-RT-002). 帧步长 `GICR_STRIDE` = 128 KiB、SGI 偏移 `GICR_SGI_OFFSET` =
// 64 KiB, 本文件内调用的 offset 均取自 `GICR_*` 常量或按 PPI 计算的 4/8 倍数 ⇒
// 自然对齐; `gicr_sgi_read` 是 pub 面, 其 offset 合法性由调用方契约传入.
//
// 类别 S「ICC_* 系统寄存器 MRS/MSR」: `icc_pmr_el1` / `icc_bpr1_el1` /
// `icc_igrpen0_el1` / `icc_igrpen1_el1` / `icc_ctlr_el1` / `icc_iar1_el1` /
// `icc_eoir1_el1` / `icc_dir_el1` 都是 EL1 特权系统寄存器, EL0 访问会 trap, 故
// "在 EL1+ 执行"由调用方契约传入. asm 只用 `in(reg)` / `out(reg)`, **无指针解引用
// 面**; 未声明 `options(...)` 属保守 (编译器假定可能访存/改标志), 不构成 UB.
// 语义侧: 写 PMR/BPR/IGRPEN/EOImode 改变本核中断屏蔽与优先级行为; 读
// `icc_iar1_el1` **不是纯读** —— 它同时 ACK 中断 (置 active、抬运行优先级), 必须
// 与 `icc_eoir1_el1` 配对, 否则本核运行优先级永不下降.
//
// 类别 C「组合调用, 无新增不变量」: 块内只调上述 D/R/S 类 unsafe fn 与 `gic_logic`
// 纯函数, 不新增指针或指令面, 契约由外层 fn `# Safety` 原样继承; 各处只补该处
// 特有的次序/边界/读改写不变量.

// ============================================================================
// 寄存器读写辅助
// ============================================================================

#[inline]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn gicd_read(offset: u64) -> u32 {
    // SAFETY: 类别 D. 本块只把 `GICD_BASE + offset` 转裸指针后 `read_volatile`
    // 读一个 u32, 不构造引用; 前后 `dsb sy` 夹住该访问, 保证读到的是屏障之后的
    // 设备状态. offset 由本文件调用点取自 `GICD_*` 常量或按 INTID 计算的 4 字节
    // 倍数寄存器索引; 映射与 MMU 前提由外层 fn `# Safety` 传入.
    unsafe {
        core::arch::asm!("dsb sy");
        let val = read_volatile((GICD_BASE.load(Ordering::Acquire) + offset) as *const u32);
        core::arch::asm!("dsb sy");
        val
    }
}

#[inline]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn gicd_write(offset: u64, val: u32) {
    // SAFETY: 类别 D. 与 `gicd_read` 同址同对齐, 差别是 `write_volatile` **写设备
    // 状态**: 写 CTLR/IGROUPR/IPRIORITYR/ISENABLER 会改变中断分组、优先级与投递,
    // 故调用点须自行保证次序 (如 CTLR 写后等 RWP 归零再写 ARE). 块内不构造引用,
    // volatile 写不被优化消除; 映射前提由外层 fn `# Safety` 传入.
    unsafe {
        core::arch::asm!("dsb sy");
        write_volatile(
            (GICD_BASE.load(Ordering::Acquire) + offset) as *mut u32,
            val,
        );
        core::arch::asm!("dsb sy");
    }
}

#[inline]
// SAFETY: 调用方保证 base 指向已映射的 GICR 帧 (RD/SGI), 类型有效。
unsafe fn gicr_read_at(base: u64, offset: u64) -> u32 {
    // SAFETY: 类别 R. base 由调用方传入 (本核 RD 或 SGI 帧高半区别名), 块内无法
    // 证明它已映射/已唤醒/属本核 —— 该义务由外层 fn `# Safety` 传入. 本块只
    // `read_volatile` 一个 u32, 不构造引用; `dsb sy` 前后夹住访问.
    unsafe {
        core::arch::asm!("dsb sy");
        let val = read_volatile((base + offset) as *const u32);
        core::arch::asm!("dsb sy");
        val
    }
}

#[inline]
// SAFETY: 调用方保证 base 指向已映射的 GICR 帧 (RD/SGI), 类型有效。
unsafe fn gicr_write_at(base: u64, offset: u64, val: u32) {
    // SAFETY: 类别 R. 同 `gicr_read_at` 的 base 义务; 本块 `write_volatile` 写
    // SGI/PPI 私有状态 (WAKER 唤醒、IGROUPR0 分组、IPRIORITYR 优先级、ICFGR1
    // 触发模式、ISENABLER0 使能), 只影响 base 所属那一核, 故 base 定位错误即
    // "把本核配置写到他人核上". 块内不构造引用.
    unsafe {
        core::arch::asm!("dsb sy");
        write_volatile((base + offset) as *mut u32, val);
        core::arch::asm!("dsb sy");
    }
}

#[inline]
// SAFETY: 调用方保证 base 指向已映射的 GICR 帧, 且 base+offset 处为 64 位 (8 字节对齐) 寄存器。
unsafe fn gicr_read64_at(base: u64, offset: u64) -> u64 {
    // SAFETY: 类别 R. 唯一用途是读 `GICR_TYPER` (offset 0x0008, 8 字节对齐) 取
    // 亲和值与 Last 位; 64 位 `read_volatile` 要求 base+offset 8 字节对齐, 由
    // GICR 帧 64 KiB 对齐 + 该 offset 保证. base 有效性与本核归属仍由调用方契约
    // 传入, 块内不构造引用.
    unsafe {
        core::arch::asm!("dsb sy");
        let val = read_volatile((base + offset) as *const u64);
        core::arch::asm!("dsb sy");
        val
    }
}

/// 定位当前核的 Redistributor RD / SGI 帧基地址。
///
/// `GICv3` 各核的 Redistributor 帧在 GICR 区域内按**亲和值** (而非物理/逻辑 CPU
/// 序号) 排列; 故不能按逻辑索引线性推算 (`GICR_BASE + stride * cpu_index`),
/// 稀疏或多簇拓扑下会命中他人帧甚至越界, 使本核 SGI/PPI 使能落到错误核上
/// (ISSUE-RT-002 关联缺陷)。
///
/// 本函数以当前核硬件 id (即 [`affinity_pack`](super::affinity_pack) 得到的紧凑
/// 亲和值) 为键, 逐帧回读 `GICR_TYPER[63:32]` 的亲和值匹配; 扫描判定复用架构
/// 中立纯逻辑 [`gic_logic::find_redist_frame`] (host 侧与生产路径同源实证)。
///
/// # Errors
///
/// 扫描至 `GICR_TYPER.Last` 或 [`gic_logic::REDIST_SCAN_MAX_FRAMES`] 上限仍未命中
/// 当前核亲和 ⇒ 返回 `Err` (fail-closed), 而非回退到线性推算。
///
/// # Safety
///
/// 需确保 GICR 区域 (含扫描范围内全部帧) 已映射到内核地址空间。
unsafe fn redist_frames() -> Result<(u64, u64), &'static str> {
    // SAFETY: 类别 C + R. 块内只调 `gicr_read64_at` 与 `gic_logic` 纯函数. 扫描以
    // `REDIST_SCAN_MAX_FRAMES` (1024) 为上界、遇 `GICR_TYPER.Last` 即停, 故最坏会
    // 触到 base + 128 KiB * 1023 的帧 —— 映射面须覆盖**整个扫描范围**而非仅命中
    // 帧, 这一点由本 fn `# Safety` 传入. 未命中即 `Err` (fail-closed), 不回退线性
    // 推算; 返回的 rd/sgi 是同一帧相隔 64 KiB 的两个基址, 后续写入均落在本核
    // 私有状态上.
    unsafe {
        let base = GICR_BASE.load(Ordering::Acquire);
        let target = crate::arch!(cpu_id());
        let frame =
            gic_logic::find_redist_frame(target, gic_logic::REDIST_SCAN_MAX_FRAMES, |idx| {
                gicr_read64_at(base + GICR_STRIDE * u64::from(idx), GICR_TYPER)
            })
            .ok_or("GICR 区域无匹配当前核亲和的 redistributor 帧")?;
        let rd = base + GICR_STRIDE * u64::from(frame);
        Ok((rd, rd + GICR_SGI_OFFSET))
    }
}

#[inline]
/// 读取 `GICv3` Redistributor SGI 帧寄存器 (启动核 CPU0)。
///
/// # Safety
///
/// 调用者需确保 `GICR_SGI_BASE` (`0x080B_0000`) 已映射且 Redistributor 已唤醒。
pub unsafe fn gicr_sgi_read(offset: u64) -> u32 {
    // SAFETY: GICR_SGI_BASE 为引导期探测得到的 SGI 帧高半区别名, 已映射。
    unsafe { gicr_read_at(GICR_SGI_BASE.load(Ordering::Acquire), offset) }
}

// ============================================================================
// GICv3 初始化
// ============================================================================

/// 初始化 `GICv3` Distributor:
/// 1. 读取 GIC 类型和实现者信息
/// 2. 禁用 Distributor 并等待 RWP 清零
/// 3. 显式置亲和路由 (ARE) 并等待 RWP 清零
/// 4. 配置中断分组与优先级
/// 5. 使能 Distributor (Group0 + `Group1NS`) 并等待 RWP 清零
///
/// # Errors
///
/// `GICD_CTLR.RWP` 未在限定自旋内清零 ⇒ 返回 Err (硬件写入未生效), 由 [`init`]
/// 向上传递 fail-fast。
///
/// # Safety
///
/// 调用前需确保 `GICD_BASE` (0x08000000) 已正确映射，MMU 已启用。
pub unsafe fn init_distributor() -> Result<(), &'static str> {
    // SAFETY: 类别 C + D. 块内全是 `gicd_read` / `gicd_write` / `wait_ctlr_rwp`
    // 与纯算术, 无新增指针面. 次序不可颠倒: ARE 位仅在所有 Group 使能位为 0 时
    // 可写, 故先写 CTLR=0 → 等 RWP → 写 ARE → 等 RWP → 最后置 Group0/Group1NS,
    // 每步之间由 `wait_ctlr_rwp` 兜住 (超限即 `Err` 向上传递). IGROUPR 只覆盖
    // INTID 0..63 (2 个 32 位字), 更高 INTID 的分组由
    // `configure_and_enable_device_spi` 按其所在字补写.
    unsafe {
        // 0. 读取 GIC 诊断信息
        let typer = gicd_read(GICD_TYPER);
        let iidr = gicd_read(GICD_IIDR);
        let num_spi = ((typer >> 5) & 0x1F) + 1; // ITLinesNumber: bits [5:0]
        let num_cpus = ((typer >> 8) & 0x07) + 1; // CPUNumber: bits [10:8]
        crate::klog_ffi!(
            klog_ffi_info,
            "[GIC] typer=0x{:08x} iidr=0x{:08x} spi={} cpus={}",
            typer,
            iidr,
            num_spi,
            num_cpus
        );

        // 1. 禁用 Distributor; ARE 仅在所有 Group 使能位为 0 时可写, 故须先禁用。
        gicd_write(GICD_CTLR, 0);
        wait_ctlr_rwp()?;

        // 2. 显式置亲和路由 (ARE_S|ARE_NS)。内核跨核 IPI 经 ICC_SGI1R_EL1 投递、
        //    SPI 经 GICD_IROUTER 分发, 二者均以 ARE=1 为前提。历史上此处仅写 0x3
        //    (未置 ARE), 依赖平台默认值 —— QEMU 强制 ARE=1 掩盖了该缺陷, 而在不
        //    强制 ARE 的真机上 SGI/SPI 失效, 表现为偶发挂起 (ISSUE-RT-002)。
        gicd_write(GICD_CTLR, gic_logic::GICD_CTLR_ARE_MASK);
        wait_ctlr_rwp()?;

        // 3. 设置所有 SPIs 为 Group 1 (Non-secure, IRQ 信号).
        //    Group 0 会触发 FIQ, 但 FIQ handler 仅为 unexpected_exception 桩.
        //    使用 Group 1 使中断走 handle_el1h_irq 正常处理路径.
        for i in 0..2 {
            gicd_write(
                GICD_IGROUPR + (crate::privileged::glue::u64_from_i32(i) * 4),
                0xFFFF_FFFF,
            );
        }

        // 4. 设置中断优先级
        for i in 0..32 {
            gicd_write(
                GICD_IPRIORITYR + (crate::privileged::glue::u64_from_i32(i) * 4),
                0xA0A0_A0A0,
            );
        }

        // 5. 使能 Distributor (Group0 + Group1NS); ARE 已在步骤 2 置位。
        gicd_write(
            GICD_CTLR,
            gic_logic::GICD_CTLR_ARE_MASK
                | gic_logic::GICD_CTLR_ENABLE_GRP0_MASK
                | gic_logic::GICD_CTLR_ENABLE_GRP1_MASK,
        );
        wait_ctlr_rwp()?;
    }
    Ok(())
}

/// 等待 `GICD_CTLR.RWP` (bit31) 清零。
///
/// `GICD_CTLR` 写后须等 RWP 归零方可进行下一次修改 —— 尤其 ARE 位仅在所有 Group
/// 使能位为 0 时可写 (ARM IHI 0069), 故"置 ARE"与"置 Group 使能"两步之间必须等待。
///
/// # Errors
///
/// RWP 未在限定自旋内清零 ⇒ 返回 Err (硬件异常), 由调用方 fail-fast。
///
/// # Safety
///
/// 需确保 GICD MMIO 已映射。
unsafe fn wait_ctlr_rwp() -> Result<(), &'static str> {
    // SAFETY: 类别 C + D. 块内只 `gicd_read(GICD_CTLR)` 自旋 + `spin_loop`, 无新增
    // 指针面. 自旋有界: `ctlr_rwp_timed_out` 超限即返回 `Err`, 不会在硬件不响应时
    // 无限挂住启动路径. 读 CTLR 无侧效应, 不改变 Distributor 状态.
    unsafe {
        let mut wait_count: u32 = 0;
        while gicd_read(GICD_CTLR) & gic_logic::GICD_CTLR_RWP_MASK != 0 {
            wait_count += 1;
            if gic_logic::ctlr_rwp_timed_out(wait_count) {
                return Err("GICD_CTLR.RWP 未在限定自旋内清零");
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}

/// 初始化指定 CPU 的 `GICv3` Redistributor:
/// 1. 唤醒 redistributor
/// 2. 配置 SGI/PPI 分组
/// 3. 配置 PPI 触发模式
/// 4. 为当前核启用 SGIs/PPIs
///
/// `rd` / `sgi` 分别指向目标 CPU 的 RD 帧与 SGI 帧基地址 (由 `redist_frames` 定位)。
///
/// # Errors
///
/// 返回 `Err(原因)` 表示 redistributor 唤醒未在限定自旋内完成
/// (`GICR_WAKER.ChildrenAsleep` 未在 `REDIST_WAKE_SPIN_LIMIT` 内清零),
/// `原因` 为中文诊断串, 由调用方 fail-fast。
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化，`rd` / `sgi` 指向已映射的 GICR 帧。
pub unsafe fn init_redistributor(rd: u64, sgi: u64) -> Result<(), &'static str> {
    // SAFETY: 类别 C + R. rd/sgi 由 `redist_frames` 按本核亲和值定位 (唯一调用点
    // `init_per_cpu`), 块内只调 `gicr_read_at` / `gicr_write_at`. 次序: 清
    // WAKER.ProcessSleep 后必须等 ChildrenAsleep 归零才可访问 SGI 帧寄存器,
    // 否则写落空; 等待有界 (`wake_timed_out` 超限返回 `Err`). ICFGR1 与
    // IPRIORITYR 是**读改写**, 只动 Timer PPI (30) 的位域, 保留同字其他 PPI 配置;
    // 该 RMW 非原子, 但 rd/sgi 是本核私有帧且只在该核自身初始化路径调用,
    // 无并发写者.
    unsafe {
        // 1. 唤醒 redistributor
        let waker = gicr_read_at(rd, GICR_WAKER);

        gicr_write_at(rd, GICR_WAKER, waker & !(1 << 1)); // 清除 ProcessorSleep (bit 1)

        // 等待 ChildrenAsleep == 0; 超限判定唤醒失败, 显式返回错误 (不静默继续).
        // 掩码与超限判据交由架构中立纯逻辑 [`gic_logic`] (host 侧可逐边界实证),
        // 本处仅负责读寄存器与自旋.
        let mut wait_count: u32 = 0;
        while gicr_read_at(rd, GICR_WAKER) & gic_logic::GICR_WAKER_CHILDREN_ASLEEP_MASK != 0 {
            wait_count += 1;
            if gic_logic::wake_timed_out(wait_count) {
                return Err("GICR_WAKER.ChildrenAsleep 未在限定自旋内清零");
            }
            core::hint::spin_loop();
        }

        // 2. 设置 PPI 优先级 (SGI frame)
        gicr_write_at(sgi, GICR_IPRIORITYR, 0xA0A0_A0A0);
        gicr_write_at(sgi, GICR_IPRIORITYR + 4, 0xA0A0_A0A0);
        gicr_write_at(sgi, GICR_IPRIORITYR + 8, 0xA0A0_A0A0);

        // 3. 配置 SGI/PPI 分组: 全部设为 Group 1 (Non-secure, IRQ 信号).
        //    Group 0 会触发 FIQ, 但 FIQ handler 仅为 unexpected_exception 桩.
        //    使用 Group 1 使 Timer PPI (30) 等中断走 handle_el1h_irq 正常路径.
        gicr_write_at(sgi, GICR_IGROUPR0, 0xFFFF_FFFF);

        // 4. 配置 PPI 触发模式: Timer PPI 为 level-triggered
        let icfgr1_val = gicr_read_at(sgi, GICR_ICFGR1);
        // Timer PPI = 30, 在 ICFGR1 中 (PPI 16-31)
        // bit[31:30] 对应 PPI 31, bit[29:28] 对应 PPI 30
        // level-triggered = 0b00
        let ppi30_shift = ((30 - 16) * 2) as u64;
        gicr_write_at(sgi, GICR_ICFGR1, icfgr1_val & !(0x3 << ppi30_shift));

        // 5. Timer PPI 低优先级
        let prio_addr = GICR_IPRIORITYR + ((u64::from(gic_logic::TIMER_PPI) / 4) * 4);
        let prio = gicr_read_at(sgi, prio_addr);
        let shift = u64::from((gic_logic::TIMER_PPI % 4) * 8);
        gicr_write_at(sgi, prio_addr, (prio & !(0xFF << shift)) | (0x40 << shift));

        // 6. GICv3 redistributor 的"使能"由清 ProcessorSleep + ChildrenAsleep
        //    归零完成, 无独立使能位. GICR_CTLR bit0 实为 EnableLPIs (LPI 使能),
        //    本项目未使用 LPI (未配置 GICR_PROPBASER/PENDBASER), 故不写该寄存器.
    }
    Ok(())
}

/// 使能 CPU Interface (ICC_* 系统寄存器)
///
/// # Safety
///
/// 仅在 EL1 或更高特权级调用，需确保 Redistributor 已初始化。
pub unsafe fn init_cpu_interface() {
    // SAFETY: 类别 S. 六条 MRS/MSR 全是 ICC_* EL1 系统寄存器, 只用 in(reg) /
    // out(reg), 无指针解引用面; EL1 前提由本 fn `# Safety` 传入. CTLR 是读改写:
    // 只清 bit1 (EOImode) 保留其余位, EOImode=0 ⇒ `icc_eoir1_el1` 一次完成降
    // 优先级与去激活, 故 IRQ 路径只需 `end_of_interrupt`. 写 IGRPEN0/1 即刻开放
    // 本核中断投递, 调用点须在 Redistributor 唤醒与分组配置完成之后.
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

/// 使能指定 CPU 的 Timer PPI 中断
///
/// `sgi` 为目标 CPU 的 SGI 帧基地址 (由 `redist_frames` 定位)。
///
/// # Safety
///
/// 调用前需确保 CPU Interface 已初始化，`sgi` 指向已映射的 GICR SGI 帧。
pub unsafe fn enable_timer_ppi(sgi: u64) {
    // SAFETY: 类别 C + R. 块内只一次 `gicr_write_at(sgi, GICR_ISENABLER0, bit)`.
    // `TIMER_PPI` = 30 (gic_logic 单源), bit = 1 << (30 % 32) 落在 PPI 段;
    // ISENABLER0 是**写 1 置位**寄存器, 不需读改写, 故不覆盖同字其他使能位.
    // sgi 的本核归属与映射由本 fn `# Safety` 传入.
    unsafe {
        let bit = 1u32 << (gic_logic::TIMER_PPI % 32);
        gicr_write_at(sgi, GICR_ISENABLER0, bit);
    }
}

/// 使能指定 CPU SGI 帧中的一路 SGI。
///
/// `sgi` 为目标 CPU 的 SGI 帧基地址 (由 `redist_frames` 定位); `intid` 为 SGI
/// 编号 (0-15), 取自本模块的 `*_SGI` 常量。
///
/// # Safety
///
/// 调用前需确保 CPU Interface 已初始化，`sgi` 指向已映射的 GICR SGI 帧。
pub unsafe fn enable_sgi(sgi: u64, intid: u32) {
    // SAFETY: 类别 C + R. 同 `enable_timer_ppi` 的写 1 置位形态. `intid % 32`
    // 只保证移位不越 u32 边界 (≤31), **不校验 intid < 16**: 传入 16..31 会落到
    // PPI 使能位而非 SGI. 调用点一律用本模块 `TLB_SHOOTDOWN_SGI` /
    // `RESCHEDULE_SGI` / `FREG_RECOVERY_SGI` 常量 (13/14/7), 该范围义务由本 fn
    // `# Safety` 与常量单源共同兜住.
    unsafe {
        gicr_write_at(sgi, GICR_ISENABLER0, 1u32 << (intid % 32));
    }
}

/// 获取中断 ID (IAR) — 用于 IRQ handler
#[must_use]
pub fn acknowledge() -> u32 {
    let iar: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mrs {}, icc_iar1_el1", out(reg) iar);
    }
    crate::privileged::glue::low_u32_from_u64(iar)
}

/// 中断完成 (EOI)
pub fn end_of_interrupt(intid: u32) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("msr icc_eoir1_el1, {}", in(reg) u64::from(intid));
    }
}

/// 发送 EOI 并解除优先级 (drop priority)
pub fn deactivate(intid: u32) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("msr icc_dir_el1, {}", in(reg) u64::from(intid));
    }
}

/// 初始化当前核的 `GICv3` per-CPU 部分:
/// Redistributor + CPU Interface + Timer PPI + 全部内核 SGI。
///
/// BSP 经 [`init`] 完成启动核初始化 (内含 `init_per_cpu()`);
/// 次核上线后由 SMP 启动路径调用本函数。本函数以**当前核硬件 id**定位自身
/// Redistributor 帧 (见 `redist_frames`), 故 BSP 与 AP 共用同一入口, 无需传入
/// 逻辑索引, 也不受稀疏/多簇拓扑下核序号错位的影响。
///
/// 本函数是**每核中断能力的唯一入口**: 返回 `Ok` 后该核可接收定时器 PPI 与全部
/// 内核 SGI (`TLB_SHOOTDOWN_SGI` / `RESCHEDULE_SGI` / `FREG_RECOVERY_SGI`)。
///
/// # Errors
///
/// 返回 `Err(原因)` 表示本核 redistributor 帧未定位到 (见 `redist_frames`) 或
/// 唤醒失败 (见 [`init_redistributor`]), 由调用方决定 fail-fast 或报错下线该核。
///
/// # Safety
///
/// 仅在目标 CPU 上调用，需确保 MMU 已启用且 GIC MMIO 区域已映射。
pub unsafe fn init_per_cpu() -> Result<(), &'static str> {
    // SAFETY: 类别 C. 块内只调 `redist_frames` / `init_redistributor` /
    // `init_cpu_interface` / `enable_timer_ppi` / `enable_sgi`, 无新增指针面.
    // 次序即依赖链: 先定位本核帧 (失败即 `?` 返回, 不触碰任何寄存器) → 唤醒并
    // 配置 Redistributor → 开放 CPU Interface → 使能 Timer PPI 与三路内核 SGI.
    // 全部帧基址来自同一次 `redist_frames`, 故不存在跨核错配.
    unsafe {
        let (rd, sgi) = redist_frames()?;
        init_redistributor(rd, sgi)?;
        init_cpu_interface();
        enable_timer_ppi(sgi);
        // 每核显式使能全部内核 SGI: SGI/PPI 使能位是 per-CPU Redistributor 私有
        // 状态, 缺任一路则该核永不响应对应 IPI。BSP 与 AP 共用本入口, 避免"仅
        // 启动核使能"的分散缺陷 (历史: SGI 13/14 从未使能 ⇒ 跨核 TLB 失效接收侧
        // 不响应, 延迟释放帧永久滞留)。
        enable_sgi(sgi, TLB_SHOOTDOWN_SGI);
        enable_sgi(sgi, RESCHEDULE_SGI);
        enable_sgi(sgi, FREG_RECOVERY_SGI);
    }
    Ok(())
}

/// 完整 GIC 初始化流程
///
/// 返回 `Ok(())` 表示全部初始化步骤完成且后置条件回读校验通过。
///
/// # Errors
///
/// 任一环节失败返回 `Err(原因)`: per-CPU (redistributor) 初始化失败
/// (见 [`init_per_cpu`]), 或后置条件回读校验未通过
/// (见 [`gic_logic::verify_post_conditions`])。`原因` 为中文诊断串,
/// 由调用方 (boot 入口) fail-fast。
///
/// # Safety
///
/// 仅在启动阶段调用，需确保 MMU 已启用且 GIC MMIO 区域已映射。
pub unsafe fn init() -> Result<(), &'static str> {
    // SAFETY: 类别 C. 块内只 `init_distributor` → `init_per_cpu` →
    // `verify_post_conditions` 三步, 无新增指针面. 次序不可颠倒: Distributor
    // 未使能前 per-CPU 配置不会投递中断; 校验放在全部写之后, 任一后置条件不符
    // 即 `Err` 交 boot 入口 fail-fast, 把静默挂起转成确定性报错.
    unsafe {
        init_distributor()?;
        init_per_cpu()?;
        verify_post_conditions()
    }
}

/// GIC 初始化后置条件校验.
///
/// 逐项回读关键寄存器, 任一不符即返回明确原因, 由 [`init`] 向上传递.
/// 目的是把"静默挂起"转化为"确定性报错", 为 SMP / 真机复现提供诊断锚点.
///
/// # Safety
///
/// 仅在 [`init`] 内部、全部配置写操作完成后调用;
/// 需确保 MMU 已启用且 GIC MMIO 区域已映射。
unsafe fn verify_post_conditions() -> Result<(), &'static str> {
    // SAFETY: 类别 C + D + R + S. 三个 MMIO 回读 (`gicd_read` / `gicr_read_at`)
    // 加一条 `mrs icc_igrpen1_el1`, 全是**只读**且不构造引用; 有 ACK 侧效应的是
    // 读 IAR, 本处不读 IAR. 判定交 `gic_logic::verify_post_conditions` 纯函数
    // (host 侧逐支路实证). 回读用**本核指纹定位的帧** (由 `redist_frames` 按亲和
    // 值扫描得到), 与 `init_per_cpu` 的配置路径同源: 本函数若改读
    // `GICR_BASE` / `GICR_SGI_BASE` 两个启动核别名, 在 BSP 不是 redistributor
    // 第 0 帧的拓扑下会校验他人帧 (多核正确性缺口). 定位失败即 `Err`, 不退回
    // 别名.
    unsafe {
        let (rd, sgi) = redist_frames()?;
        // 逐项回读关键寄存器实测值, 判定交由架构中立纯逻辑 [`gic_logic`] ——
        // 该判据在 host 侧被逐支路单元测试 (ISSUE-RT-002 确定性实证), 与本处同源.
        let gicd_ctlr = gicd_read(GICD_CTLR);
        let gicr_waker = gicr_read_at(rd, GICR_WAKER);
        let gicr_isenabler0 = gicr_read_at(sgi, GICR_ISENABLER0);
        let igrpen1: u64;
        core::arch::asm!("mrs {}, icc_igrpen1_el1", out(reg) igrpen1);
        gic_logic::verify_post_conditions(gicd_ctlr, gicr_waker, gicr_isenabler0, igrpen1)
    }
}

// ============================================================================
// 中断管理 API
// ============================================================================

/// 校验中断号是否为有效的 PPI
#[must_use]
pub fn is_ppi(irq: u32) -> bool {
    (PPI_BASE..SPI_BASE).contains(&irq)
}

/// 校验中断号是否为有效的 SPI
#[must_use]
pub fn is_spi(irq: u32) -> bool {
    irq >= SPI_BASE
}

/// 校验中断号是否在有效范围内
#[must_use]
pub fn is_valid_irq(irq: u32) -> bool {
    irq < SPI_BASE + 960 // GICv3 最多支持 1024 个中断
}

/// 使能 SPI 中断
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn enable_spi(irq: u32) {
    // SAFETY: 类别 C + D. 块内只 `gicd_write` 一次. `is_spi` 只判下界 (>= 32),
    // **不判上界** (`is_valid_irq` 未在此调用), 故 `GICD_ISENABLER + (irq/32)*4`
    // 落在 GICD 帧内这一前提由调用方传入合法 INTID 兜住. ISENABLER 是写 1 置位
    // 寄存器, 不需读改写, 不覆盖同字其他 SPI 的使能位.
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ISENABLER + (u64::from(irq / 32) * 4);
        let bit = 1u32 << (irq % 32);
        gicd_write(reg_offset, bit);
    }
}

/// 禁用 SPI 中断
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn disable_spi(irq: u32) {
    // SAFETY: 类别 C + D. 同 `enable_spi` 的边界口径 (`is_spi` 只判下界). 目标
    // 寄存器是 `GICD_ICENABLER` = ISENABLER 偏移 + 0x80 (0x0180 段), 写 1 **清**
    // 对应使能位; 与 ISENABLER 同为写 1 生效形态, 故不需读改写.
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ISENABLER + (u64::from(irq / 32) * 4);
        let bit = 1u32 << (irq % 32);
        // GICD_ICENABLER 与 ISENABLER 偏移相同, 写 1 禁用
        gicd_write(reg_offset + 0x80, bit);
    }
}

/// 设置 SPI 中断为 pending
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn set_spi_pending(irq: u32) {
    // SAFETY: 类别 C + D. 同 `enable_spi` 的边界口径. 写 `GICD_ISPENDR` 置 pending
    // 会**真的触发一次中断投递** (软件触发 SPI), 属语义侧效应: 调用点须确保该
    // INTID 已配置并注册 handler (见 `register_device_spi`). 块内不构造引用.
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ISPENDR + (u64::from(irq / 32) * 4);
        let bit = 1u32 << (irq % 32);
        gicd_write(reg_offset, bit);
    }
}

/// 配置 SPI 中断为 level-triggered
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn configure_spi_level(irq: u32) {
    // SAFETY: 类别 C + D. `gicd_read` → 位清 → `gicd_write` 的**读改写**, 非原子:
    // 同字 (16 路 SPI) 内若有并发配置者会互相覆盖. 本文件调用点均在启动期单线程
    // 注册路径 (`configure_and_enable_device_spi`), 无并发写者. shift =
    // (irq % 16) * 2 ≤ 30, `3 << shift` 不越 u32 边界; 只清本路 2 位, 保留同字
    // 其他 SPI 的触发模式.
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ICFGR + (u64::from(irq / 16) * 4);
        let shift = u64::from((irq % 16) * 2);
        let val = gicd_read(reg_offset);
        // bit 0 = 0 for level-triggered, bit 1 = 0 for inactive
        gicd_write(reg_offset, val & !(3 << shift));
    }
}

/// 配置 SPI 中断为 edge-triggered
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化。
pub unsafe fn configure_spi_edge(irq: u32) {
    // SAFETY: 类别 C + D. 同 `configure_spi_level` 的读改写口径与并发前提.
    // 差别是 `val | (1 << shift)` 只置本路 bit0 (边沿触发), 不动 bit1; shift ≤ 30
    // 故移位不越 u32 边界. 块内无新增指针面.
    unsafe {
        if !is_spi(irq) {
            return;
        }
        let reg_offset = GICD_ICFGR + (u64::from(irq / 16) * 4);
        let shift = u64::from((irq % 16) * 2);
        let val = gicd_read(reg_offset);
        // bit 0 = 1 for edge-triggered
        gicd_write(reg_offset, val | (1 << shift));
    }
}

/// 读取 SPI 中断状态 (是否 pending)
#[must_use]
pub fn is_spi_pending(irq: u32) -> bool {
    if !is_spi(irq) {
        return false;
    }
    let reg_offset = GICD_ISPENDR + (u64::from(irq / 32) * 4);
    let bit = 1u32 << (irq % 32);
    // SAFETY: 读取 GICD 寄存器, 调用方保证 MMIO 已映射
    unsafe { gicd_read(reg_offset) & bit != 0 }
}

// ============================================================================
// 设备 SPI 分发表 (中断驱动 I/O)
// ============================================================================
//
// 承载 privileged 内部各设备子系统 (网卡等) 的 SPI 中断处理程序:
//   - 启动期由各子系统经 [`register_device_spi`] 单线程注册 (GIC 已初始化);
//   - 运行期由 IRQ 异常路径经 [`dispatch_device_spi`] 在 ACK 之后、EOI 之前分发。

/// 设备 SPI 分发表长度 (覆盖 INTID 32..288)
const DEVICE_IRQ_TABLE_LEN: usize = 256;

/// 设备 SPI handler 分发表类型: 按 INTID 偏移索引的可选 handler 数组.
type DeviceIrqTable = [Option<fn()>; DEVICE_IRQ_TABLE_LEN];

/// 设备 SPI handler 分发表; 索引 = INTID - [`SPI_BASE`].
///
/// 启动期单线程写入 ([`register_device_spi`]), 中断上下文只读
/// ([`dispatch_device_spi`]); 二者天然串行, 无需加锁。
static DEVICE_IRQ_HANDLERS: RacyCell<DeviceIrqTable> = RacyCell::new([None; DEVICE_IRQ_TABLE_LEN]);

/// 写入 64 位 GICD 寄存器 (`GICv3` IROUTER 等)
///
/// # Safety
///
/// 调用前需确保 GICD MMIO 已映射。
#[inline]
unsafe fn gicd_write64(offset: u64, val: u64) {
    // SAFETY: 类别 D. 64 位形态, 唯一调用点 `route_spi_to_cpu0` 传
    // `GICD_IROUTER + irq * 8` ⇒ 8 字节对齐 (u64 volatile 写的对齐前提). 其余同
    // `gicd_write`: 不构造引用, `dsb sy` 前后夹住, 映射由 `# Safety` 传入.
    unsafe {
        core::arch::asm!("dsb sy");
        write_volatile(
            (GICD_BASE.load(Ordering::Acquire) + offset) as *mut u64,
            val,
        );
        core::arch::asm!("dsb sy");
    }
}

/// 将 SPI 路由到 CPU0
///
/// ARE 由 [`init_distributor`] 显式置位、并经 [`verify_post_conditions`] fail-fast
/// 校验, 故 SPI 一律走 64 位 `GICD_IROUTER` (Affinity 全 0 → CPU0)。
///
/// # Safety
///
/// 调用前需确保 Distributor 已初始化且 GICD MMIO 已映射。
unsafe fn route_spi_to_cpu0(irq: u32) {
    // SAFETY: 类别 C + D. 块内只一次 `gicd_write64(GICD_IROUTER + irq*8, 0)`.
    // 写 0 = Aff3:Aff2:Aff1:Aff0 全 0 ⇒ 路由到亲和值 0 的核 (CPU0); 该形态以
    // ARE=1 为前提, 由 `init_distributor` 显式置位并经 `verify_post_conditions`
    // fail-fast 校验. irq 上界同 `enable_spi` 口径, 由调用方传入合法 INTID.
    unsafe {
        // SIMPLIFIED: SPI 亲和路由固定为 CPU0 (硬编码); 影响: 次核上线后 SPI 仍只
        //   投递至 CPU0; 需扩展: 引入多核 SPI 亲和路由时按目标 CPU 计算 Affinity。
        gicd_write64(GICD_IROUTER + u64::from(irq) * 8, 0);
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
    // SAFETY: 类别 C + D. 块内全是 GICD 读改写与 `configure_spi_level` /
    // `route_spi_to_cpu0` / `enable_spi` 组合, 无新增指针面. 次序: 先归 Group 1
    // (否则 INTID >= 64 默认落 Group 0 → FIQ, 而 FIQ handler 只是桩) → 优先级
    // → 触发模式 → 路由 → **最后使能**, 避免配置未完成即投递中断. IGROUPR 与
    // IPRIORITYR 两处读改写只动本路位域, 保留同字其他 SPI 配置; 调用点
    // `register_device_spi` 在启动期单线程执行, 故 RMW 无并发写者.
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
        gicd_write(
            prio_reg,
            (prio & !(0xFFu32 << prio_shift)) | (0xA0u32 << prio_shift),
        );

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
/// 供 privileged 内部设备子系统在启动期为其中断源登记 handler; 注册成功后
/// 该 SPI 被配置为 Group 1 / 优先级 0xA0 / 电平触发 / 路由 CPU0 并使能。
///
/// # Errors
///
/// - `intid` 不在 SPI 范围 (< `SPI_BASE`);
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
    let mut table = DEVICE_IRQ_HANDLERS.get_mut();
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
