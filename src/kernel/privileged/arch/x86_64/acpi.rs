//! ACPI / RSDP / MADT / FADT / HPET / DMAR — 多核检测与硬件发现
//!
//! 通过 RSDP → RSDT/XSDT → 各 SDT 路径发现硬件信息:
//!
//! | 表    | 签名 | 用途 |
//! |-------|------|------|
//! | MADT  | APIC | LAPIC/IOAPIC 拓扑, AP 启动 |
//! | FADT  | FACP | 电源管理寄存器 (PM1a/b), 关机/重启 |
//! | HPET  | HPET | 高精度事件定时器基址与频率 |
//! | DMAR  | DMAR | IOMMU (VT-d) DRHD 单元, DMA 重映射 |
//!
//! ## 架构
//!
//! ```text
//! RSDP (Root System Description Pointer)
//!   ├─ RSDT (Root SDT, 32-bit pointers)
//!   │    └─ MADT (Multiple APIC Description Table)
//!   └─ XSDT (Extended SDT, 64-bit pointers)
//!        └─ MADT
//!
//! MADT 条目:
//!   0x00 — Processor Local APIC (lapic_id, apic_id, flags)
//!   0x01 — I/O APIC
//!   0x02 — Interrupt Source Override
//!   0x04 — Local APIC NMI
//!   0x05 — Local APIC Address Override
//!   0x09 — x2APIC
//! ```

#![deny(missing_docs)]

use crate::privileged::sync::{IrqSpinLock, OnceLock};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub use crate::privileged::config::MAX_CPUS;

/// MADT 条目类型
const MADT_TYPE_LAPIC: u8 = 0x00;
const MADT_TYPE_IOAPIC: u8 = 0x01;
const MADT_TYPE_ISO: u8 = 0x02;
const _MADT_TYPE_NMI: u8 = 0x04;

static MADT_BASE: AtomicU64 = AtomicU64::new(0);
static MADT_FOUND: AtomicBool = AtomicBool::new(false);

/// 发现的 AP LAPIC 信息
#[derive(Debug, Clone, Copy)]
pub struct ApInfo {
    /// 本核 LAPIC id
    pub lapic_id: u32,
    /// ACPI 报告的 APIC id
    pub apic_id: u32,
    /// 该 AP 是否可用
    pub enabled: bool,
}

/// 单个 IOAPIC 控制器的描述信息
#[derive(Debug, Clone, Copy)]
pub struct IoApicInfo {
    /// IOAPIC 硬件 ID (MADT 中的 `io_apic_id`)
    pub id: u8,
    /// MMIO 基址 (从 MADT 解析)
    pub base_addr: u64,
    /// 全局系统中断基址 (GSI base)
    pub gsi_base: u32,
    /// 最大 IRQ 数 (从 IOAPICVER 寄存器读取, 初始化时填充)
    pub max_irq: u8,
}

/// 所有 IOAPIC 控制器 (启动期由 ACPI MADT 填充)
static IOAPICS: IrqSpinLock<[Option<IoApicInfo>; MAX_IOAPICS]> =
    IrqSpinLock::new([None; MAX_IOAPICS]);
static IOAPIC_COUNT: AtomicU32 = AtomicU32::new(0);
const MAX_IOAPICS: usize = 8;

/// MADT 中断源覆盖项容量 (固件实际条目数通常 ≤ 4).
const MAX_IRQ_OVERRIDES: usize = 16;

/// MADT ISO 极性编码 (flags bit`[1:0]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoPolarity {
    /// 遵循总线默认 (`00`; 保留编码 `10` 同样按未指定处理).
    Conforms,
    /// 高电平有效 (`01`).
    ActiveHigh,
    /// 低电平有效 (`11`).
    ActiveLow,
}

/// MADT ISO 触发模式编码 (flags bit`[3:2]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoTrigger {
    /// 遵循总线默认 (`00`; 保留编码 `10` 同样按未指定处理).
    Conforms,
    /// 边沿触发 (`01`).
    Edge,
    /// 电平触发 (`11`).
    Level,
}

/// 解析后的 MADT ISO 标志位.
///
/// ACPI 对 bit`[1:0]` (极性) 与 bit`[3:2]` (触发模式) 的编码一致:
/// `00` = 遵循总线默认, `01` = 高电平/边沿, `10` = 保留, `11` = 低电平/电平.
/// `Conforms` 时由消费方按总线语义补齐 (PCI `INTx` = 低电平有效 + 电平触发).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoFlags {
    /// 极性.
    pub polarity: IsoPolarity,
    /// 触发模式.
    pub trigger: IsoTrigger,
}

/// 一条 MADT 中断源覆盖 (Interrupt Source Override) 记录.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqOverride {
    /// 被覆盖的源 IRQ 线号 (MADT `source` 字段).
    pub irq: u8,
    /// 覆盖后的全局系统中断号 (MADT `global_sys_int` 字段).
    pub gsi: u32,
    /// 极性与触发模式.
    pub flags: IsoFlags,
}

/// MADT 中断源覆盖表 (启动期由 `parse_madt_entries` 填充).
static IRQ_OVERRIDES: IrqSpinLock<[Option<IrqOverride>; MAX_IRQ_OVERRIDES]> =
    IrqSpinLock::new([None; MAX_IRQ_OVERRIDES]);
static IRQ_OVERRIDE_COUNT: AtomicU32 = AtomicU32::new(0);

/// AP 列表 — 惰性堆置, 避免 `[Option<ApInfo>; MAX_CPUS]` (约 12 KB) 常驻 BSS。
///
/// `OnceLock` 内联仅数十字节; 首次访问 (MADT 解析前或查询时) 在 kmalloc 堆上
/// 逐元素构造 `Box<[Option<ApInfo>]>`。首次访问恒晚于 kmalloc 初始化。
static AP_LIST: OnceLock<IrqSpinLock<Box<[Option<ApInfo>]>>> = OnceLock::new();
static AP_COUNT: AtomicU32 = AtomicU32::new(0);

/// 获取 AP 列表 (首次调用时惰性分配)。
fn ap_list() -> &'static IrqSpinLock<Box<[Option<ApInfo>]>> {
    AP_LIST.get_or_init(|slot| {
        slot.write(IrqSpinLock::new(
            (0..MAX_CPUS).map(|_| None).collect::<Box<[_]>>(),
        ));
    })
}

// ============================================================================
// RSDP 搜索
// ============================================================================

/// 搜索 ACPI RSDP 表: 依次尝试 Multiboot2 信息、EBDA 与 BIOS ROM。
#[must_use]
pub fn find_rsdp(multiboot2_info_ptr: u64) -> Option<u64> {
    // 1. 尝试从 Multiboot2 info 中获取 RSDP
    if multiboot2_info_ptr != 0
        && let Some(rsdp) = find_rsdp_from_mb2(multiboot2_info_ptr)
    {
        return Some(rsdp);
    }

    // 2. 扫描 EBDA (Extended BIOS Data Area, 通常在 0x80000-0x9FFFF)
    if let Some(rsdp) = scan_ebda() {
        return Some(rsdp);
    }

    // 3. 扫描 BIOS ROM 区域 (0xE0000-0xFFFFF)
    scan_bios_rom()
}

/// 从小端字节镜像取一个 `u32`; 越界返回 `None` (不 panic, 不截断).
fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// 校验一段 RSDP 字节副本.
///
/// 签名 `"RSD PTR "` + 前 20 字节 (v1 面) 校验和为 0; revision ≥ 2 时另按 `length`
/// 字段 (@20) 校验 v2 扩展面整段和. 长度不足以容纳所声明的面即判否 —— fail-closed,
/// 不按短面静默放行.
fn rsdp_bytes_valid(bytes: &[u8]) -> bool {
    if bytes.len() < 20 || bytes[..8] != *b"RSD PTR " {
        return false;
    }
    let sum = |slice: &[u8]| slice.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    if sum(&bytes[..20]) != 0 {
        return false;
    }
    if bytes[15] >= 2 {
        let Some(declared) = le_u32(bytes, 20).map(|v| v as usize) else {
            return false;
        };
        if declared < 36 || declared > bytes.len() {
            return false;
        }
        return sum(&bytes[..declared]) == 0;
    }
    true
}

/// 在 MB2 标签表字节镜像里定位 ACPI RSDP, 返回其**块内字节偏移**.
///
/// MB2 spec §3.6.16/17: tag 14 (旧版) / tag 15 (新版) 的载荷是 RSDP 的**内联副本**
/// (分别 20 / 36 字节), **不是**指向 RSDP 的指针. 原实现把载荷按 `u64` 读成地址,
/// 读到的是签名 `"RSD PTR "` 的小端整数 `0x2052_5450_2044_5352` —— 非规范地址,
/// 解引用即 #GP. 故本函数返回偏移, 由调用方加回块基址.
///
/// 两标签并存时优先 tag 15 (v2, 带 XSDT), 且**与标签出现次序无关**: GRUB 在
/// `SeaBIOS` 与 OVMF 下都同时给出两条, 次序不构成优先级.
fn rsdp_offset_in_mb2(blob: &[u8]) -> Option<usize> {
    let end = (le_u32(blob, 0)? as usize).min(blob.len());
    let mut v1: Option<usize> = None;
    let mut offset: usize = 8;
    while offset + 8 <= end {
        let tag_type = le_u32(blob, offset)?;
        let tag_size = le_u32(blob, offset + 4)? as usize;
        if tag_type == 0 || tag_size < 8 {
            break;
        }
        let payload = offset + 8;
        let payload_end = (offset + tag_size).min(end);
        if (tag_type == 14 || tag_type == 15)
            && payload < payload_end
            && rsdp_bytes_valid(&blob[payload..payload_end])
        {
            if tag_type == 15 {
                return Some(payload);
            }
            v1 = Some(payload);
        }
        offset = (offset + tag_size + 7) & !7;
    }
    v1
}

fn find_rsdp_from_mb2(mb2_ptr: u64) -> Option<u64> {
    let ptr = mb2_ptr as *const u8;
    // SAFETY: MB2 信息块由引导器装入, 经 PMM 早期预留 (label "mb2-info") 后生命周期
    // 覆盖内核运行; 首 4 字节为 total_size. 启动期低半区恒等映射覆盖 0-4 GiB, 而
    // MB2 spec 保证引导器把信息块放在 4 GiB 以下, 故物理地址可直读.
    let total_size = unsafe { ptr.cast::<u32>().read_unaligned() } as usize;
    // SAFETY: 同上; total_size 由引导器写入并描述整块长度, 只读借用且无别名写.
    let blob = unsafe { core::slice::from_raw_parts(ptr, total_size) };
    let offset = rsdp_offset_in_mb2(blob)?;
    // SAFETY: `klog_info` 要求入参为 NUL 结尾的有效 C 串; 实参是 `c"[ACPI] RSDP found
    // via Multiboot2"` 静态字面量, 地址在 .rodata 内、进程生命周期有效, 函数同步消费.
    unsafe {
        crate::privileged::klog::klog_info(c"[ACPI] RSDP found via Multiboot2".as_ptr());
    }
    Some(mb2_ptr + offset as u64)
}

fn scan_ebda() -> Option<u64> {
    // EBDA 基址存于 BDA (BIOS Data Area) 偏移 0x40E
    let bda_ptr = 0x400 as *const u16;
    // SAFETY: 读 BIOS 数据区 (BDA) 偏移 0x40E 的 u16 (EBDA 段基址): 0x400..0x4FF
    // 属启动恒等映射覆盖的低 1MB 固件区, `0x40E` 是 2 的倍数故 u16 访问自然对齐;
    // `read_volatile` 只读, 不构造引用.
    unsafe {
        let ebda_seg = bda_ptr.add(0x40E / 2).read_volatile();
        if ebda_seg == 0 {
            return None;
        }
        let ebda_base = u64::from(ebda_seg) << 4;
        scan_memory_range(ebda_base, 0x1000)
    }
}

fn scan_bios_rom() -> Option<u64> {
    scan_memory_range(0xE0000, 0x20000)
}

fn scan_memory_range(start: u64, len: u64) -> Option<u64> {
    let end = start + len;
    let mut addr = start;
    while addr + 36 <= end {
        if is_valid_rsdp(addr) {
            // SAFETY: `klog_info` 要求 NUL 结尾有效 C 串; 实参为静态字面量
            // `c"[ACPI] RSDP found via BIOS scan"`, 位于 .rodata, 同步消费.
            unsafe {
                crate::privileged::klog::klog_info(c"[ACPI] RSDP found via BIOS scan".as_ptr());
            }
            return Some(addr);
        }
        addr += 16;
    }
    None
}

/// 校验物理地址 `addr` 处的 RSDP 签名与 v1 面校验和 (BIOS/EBDA 扫描面).
///
/// 扫描面逐 16 字节试候选, 故先只读 20 字节 (v1 面) 以压低 ROM 扫描开销;
/// MB2 内联副本面另按 [`rsdp_bytes_valid`] 校验 (含 v2 扩展面).
fn is_valid_rsdp(addr: u64) -> bool {
    let ptr = addr as *const u8;
    // SAFETY: `addr` 只来自 `scan_memory_range` (EBDA 与 BIOS ROM 两个扫描窗都走它),
    // 其循环以 `addr + 36 <= end` 保证窗内余量; 该区在启动恒等映射内.
    // `[u8; 8]` 对齐为 1, 随后 20 次单字节 `read_volatile` 均不越界; 全程只读.
    unsafe {
        // "RSD PTR " 签名字符串
        let sig: &[u8; 8] = &*ptr.cast::<[u8; 8]>();
        if sig != b"RSD PTR " {
            return false;
        }

        // 校验和: 前 20 个字节的校验和必须为 0
        let mut sum: u8 = 0;
        for i in 0..20 {
            sum = sum.wrapping_add(ptr.add(i).read_volatile());
        }
        sum == 0
    }
}

// ============================================================================
// SDT 解析
// ============================================================================

fn get_rsdt(rsdp: u64) -> Option<&'static SdtHeader> {
    let ptr = rsdp as *const u8;
    // SAFETY: `rsdp` 是 `find_rsdp` 返回且经 `is_valid_rsdp` 校验签名与 v1 校验和的
    // 候选地址; 此处只读 +15 (revision) 与 +24/+16 (XSDT/RSDT 表地址), 表地址字段用
    // `read_unaligned` 避开 packed 布局的对齐要求. 表地址指向固件 ACPI 表, 位于启动
    // 恒等映射的低区窗口; 后续只读表头 `SdtHeader` (36 字节).
    unsafe {
        let revision = ptr.add(15).read_volatile();
        if revision >= 2 {
            // XSDT (64 位指针, 仅在 revision >= 2 中出现)
            let xsdt_addr = ptr.add(24).cast::<u64>().read_unaligned();
            if xsdt_addr != 0 {
                return Some(&*(xsdt_addr as *const SdtHeader));
            }
        }

        // RSDT (32-bit pointers)
        let rsdt_addr = u64::from(ptr.add(16).cast::<u32>().read_unaligned());
        if rsdt_addr != 0 {
            Some(&*(rsdt_addr as *const SdtHeader))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    _revision: u8,
    _checksum: u8,
    _oem_id: [u8; 6],
    _oem_table_id: [u8; 8],
    _oem_revision: u32,
    _creator_id: u32,
    _creator_revision: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct MadtHeader {
    header: SdtHeader,
    local_apic_addr: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct MadtEntry {
    entry_type: u8,
    length: u8,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct MadtLapic {
    entry_type: u8,
    length: u8,
    acpi_proc_id: u8,
    apic_id: u8,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct MadtIoApic {
    entry_type: u8,
    length: u8,
    io_apic_id: u8,
    _reserved: u8,
    io_apic_addr: u32,
    global_sys_int_base: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct MadtIso {
    entry_type: u8,
    length: u8,
    _bus: u8,
    source: u8,
    global_sys_int: u32,
    flags: u16,
}

// ============================================================================
// MADT 解析 — 核心公共接口
// ============================================================================

/// 解析 ACPI MADT 表, 填充 AP 与 IOAPIC 信息; 成功返回 true。
///
/// 启动路径改用 [`parse_all_tables`] 单次遍历 (内含 MADT); 本函数保留为
/// 独立 MADT 入口与 C ABI [`acpi_parse_madt`] 的转发目标。
pub fn parse_madt(multiboot2_info_ptr: u64) -> bool {
    let Some(rsdp) = find_rsdp(multiboot2_info_ptr) else {
        // SAFETY: `klog_info` 要求 NUL 结尾有效 C 串; 实参为静态字面量
        // `c"[ACPI] RSDP not found"`, 位于 .rodata, 同步消费.
        unsafe {
            crate::privileged::klog::klog_info(c"[ACPI] RSDP not found".as_ptr());
        }
        return false;
    };

    let Some(rsdt_or_xsdt) = get_rsdt(rsdp) else {
        // SAFETY: `klog_info` 要求 NUL 结尾有效 C 串; 实参为静态字面量
        // `c"[ACPI] RSDT/XSDT not found"`, 位于 .rodata, 同步消费.
        unsafe {
            crate::privileged::klog::klog_info(c"[ACPI] RSDT/XSDT not found".as_ptr());
        }
        return false;
    };

    let rsdp_ptr = rsdp as *const u8;
    // SAFETY: 指针指向已通过 BIOS/ACPI 探测验证的物理地址; volatile 访问保证不被编译器重排
    let revision = unsafe { rsdp_ptr.add(15).read_volatile() };
    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let uses_xsdt =
        revision >= 2 && unsafe { rsdp_ptr.add(24).cast::<u64>().read_unaligned() } != 0;

    let table_count = if uses_xsdt {
        (rsdt_or_xsdt.length - 12) / 8
    } else {
        (rsdt_or_xsdt.length - 12) / 4
    } as usize;

    // SAFETY: 本块从 `entries_ptr` (RSDT/XSDT 表头之后) 按 `table_count` 逐项读取
    // 4/8 字节表项, 读取宽度与 `uses_xsdt` 一致, 表项用 `read_unaligned` 取; 表项指向
    // 固件 ACPI 表, 只读表头 `signature` 后转交 `parse_madt_entries`. 表项数与边界
    // 取自固件 Length 字段 (公式见上方 `table_count`).
    unsafe {
        let entries_ptr = core::ptr::from_ref::<SdtHeader>(rsdt_or_xsdt)
            .add(1)
            .cast::<u8>();

        for i in 0..table_count {
            let madt_ptr: u64 = if uses_xsdt {
                entries_ptr.add(i * 8).cast::<u64>().read_unaligned()
            } else {
                u64::from(entries_ptr.add(i * 4).cast::<u32>().read_unaligned())
            };

            if madt_ptr == 0 {
                continue;
            }

            let header = &*(madt_ptr as *const SdtHeader);
            if header.signature == *b"APIC" {
                parse_madt_entries(madt_ptr);
                MADT_BASE.store(madt_ptr, Ordering::Release);
                MADT_FOUND.store(true, Ordering::Release);
                return true;
            }
        }
    }

    // SAFETY: `klog_info` 要求 NUL 结尾有效 C 串; 实参为静态字面量
    // `c"[ACPI] MADT not found in RSDT/XSDT"`, 位于 .rodata, 同步消费.
    unsafe {
        crate::privileged::klog::klog_info(c"[ACPI] MADT not found in RSDT/XSDT".as_ptr());
    }
    false
}

fn parse_madt_entries(madt_ptr: u64) {
    // SAFETY: `madt_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(MadtHeader)); 只读访问
    let madt = unsafe { &*(madt_ptr as *const MadtHeader) };
    let lapic_base = u64::from(madt.local_apic_addr);

    let entries_start =
        crate::privileged::glue::usize_from_u64(madt_ptr) + core::mem::size_of::<MadtHeader>();
    let entries_end =
        crate::privileged::glue::usize_from_u64(madt_ptr) + madt.header.length as usize;

    let mut offset = entries_start;
    while offset + 2 <= entries_end {
        // SAFETY: `offset` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(MadtEntry)); 只读访问
        let entry = unsafe { &*(offset as *const MadtEntry) };

        match entry.entry_type {
            MADT_TYPE_LAPIC => {
                if offset + core::mem::size_of::<MadtLapic>() > entries_end {
                    break;
                }
                // SAFETY: `offset` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(MadtLapic)); 只读访问
                let lapic = unsafe { &*(offset as *const MadtLapic) };
                let idx = AP_COUNT.load(Ordering::Acquire) as usize;
                if idx < MAX_CPUS {
                    let enabled = (lapic.flags & 0x1) != 0;
                    ap_list().lock()[idx] = Some(ApInfo {
                        lapic_id: u32::from(lapic.apic_id),
                        apic_id: u32::from(lapic.acpi_proc_id),
                        enabled,
                    });
                    AP_COUNT.store(
                        crate::privileged::glue::low_u32_from_usize(idx + 1),
                        Ordering::Release,
                    );
                }
            }
            MADT_TYPE_IOAPIC => {
                if offset + core::mem::size_of::<MadtIoApic>() > entries_end {
                    break;
                }
                // SAFETY: `offset` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(MadtIoApic)); 只读访问
                let ioapic = unsafe { &*(offset as *const MadtIoApic) };
                let idx = IOAPIC_COUNT.load(Ordering::Acquire) as usize;
                if idx < MAX_IOAPICS {
                    let info = IoApicInfo {
                        id: ioapic.io_apic_id,
                        base_addr: u64::from(ioapic.io_apic_addr),
                        gsi_base: ioapic.global_sys_int_base,
                        max_irq: 0, // 初始化时填充
                    };
                    IOAPICS.lock()[idx] = Some(info);
                    IOAPIC_COUNT.store(
                        crate::privileged::glue::low_u32_from_usize(idx + 1),
                        Ordering::Release,
                    );
                }
            }
            MADT_TYPE_ISO => {
                if offset + core::mem::size_of::<MadtIso>() > entries_end {
                    break;
                }
                // SAFETY: `offset` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(MadtIso)); 只读访问
                let iso = unsafe { &*(offset as *const MadtIso) };
                // packed 结构字段先按值取出, 避免在格式化/函数调用处构造未对齐引用
                let irq = iso.source;
                let gsi = iso.global_sys_int;
                let raw_flags = iso.flags;
                let idx = IRQ_OVERRIDE_COUNT.load(Ordering::Acquire) as usize;
                if idx < MAX_IRQ_OVERRIDES {
                    let flags = decode_iso_flags(raw_flags);
                    IRQ_OVERRIDES.lock()[idx] = Some(IrqOverride { irq, gsi, flags });
                    IRQ_OVERRIDE_COUNT.store(
                        crate::privileged::glue::low_u32_from_usize(idx + 1),
                        Ordering::Release,
                    );
                    crate::klog_info!(
                        Acpi,
                        "[ACPI] MADT ISO: irq={} -> gsi={} flags={:#06x} polarity={:?} trigger={:?}",
                        irq,
                        gsi,
                        raw_flags,
                        flags.polarity,
                        flags.trigger
                    );
                }
            }
            _ => {}
        }

        if entry.length == 0 {
            break;
        }
        offset += entry.length as usize;
    }

    // 输出真实 LAPIC base 与 AP 计数 (原为占位串, 日志失真)
    crate::klog_info!(
        Acpi,
        "[ACPI] MADT: LAPIC base=0x{:X}, AP count={}",
        lapic_base,
        AP_COUNT.load(Ordering::Acquire)
    );
}

// ============================================================================
// 公共查询 API
// ============================================================================

/// 获取已解析的 AP (应用处理器) 数量。
pub fn get_ap_count() -> u32 {
    AP_COUNT.load(Ordering::Acquire)
}

/// 获取全部 AP 信息列表的克隆快照。
#[must_use]
pub fn get_ap_list() -> Box<[Option<ApInfo>]> {
    ap_list().lock().clone()
}

/// 按下标获取单个 AP 信息。
#[must_use]
pub fn get_ap(index: usize) -> Option<ApInfo> {
    ap_list().lock()[index]
}

/// 查询是否已成功解析到 MADT 表。
pub fn has_madt() -> bool {
    MADT_FOUND.load(Ordering::Acquire)
}

/// 获取首个 IOAPIC 的 MMIO 基址 (无则返回 0)。
pub fn get_ioapic_addr() -> u64 {
    let ioapics = IOAPICS.lock();
    ioapics.iter().flatten().next().map_or(0, |i| i.base_addr)
}

/// 获取首个 IOAPIC 的全局系统中断基址 (GSI base, 无则返回 0)。
pub fn get_ioapic_gsib() -> u32 {
    let ioapics = IOAPICS.lock();
    ioapics.iter().flatten().next().map_or(0, |i| i.gsi_base)
}

/// 获取所有 IOAPIC 信息
pub fn get_ioapics() -> [Option<IoApicInfo>; MAX_IOAPICS] {
    *IOAPICS.lock()
}

/// 获取 IOAPIC 数量
pub fn get_ioapic_count() -> u32 {
    IOAPIC_COUNT.load(Ordering::Acquire)
}

/// GSI → IOAPIC 路由: 返回 (`ioapic_index`, `local_irq`)
pub fn gsi_to_ioapic(gsi: u32) -> Option<(usize, u8)> {
    let ioapics = IOAPICS.lock();
    for (i, ioapic) in ioapics.iter().enumerate() {
        if let Some(info) = ioapic
            && gsi >= info.gsi_base
            && gsi < info.gsi_base + u32::from(info.max_irq)
        {
            return Some((
                i,
                crate::privileged::glue::low_u8_from_u32(gsi - info.gsi_base),
            ));
        }
    }
    None
}

/// 回填指定 IOAPIC 的实际重定向表条目数 (IOAPICVER 读数).
///
/// `parse_madt_entries` 只拿到 MADT 声明的基址, 条目数必须读硬件;
/// 由 `ioapic::init_from_acpi` 在硬件初始化后回填, 否则 [`gsi_to_ioapic`]
/// 会因 `max_irq == 0` 恒判区间为空.
pub fn set_ioapic_max_irq(index: usize, max_irq: u8) {
    let mut ioapics = IOAPICS.lock();
    if let Some(info) = ioapics.get_mut(index).and_then(Option::as_mut) {
        info.max_irq = max_irq;
    }
}

/// 解码 MADT ISO 标志位 (编码见 [`IsoFlags`]).
#[must_use]
pub fn decode_iso_flags(raw: u16) -> IsoFlags {
    let polarity = match (raw & 0b11) as u8 {
        0b01 => IsoPolarity::ActiveHigh,
        0b11 => IsoPolarity::ActiveLow,
        // 0b00 = 遵循总线默认; 0b10 = 保留编码, 同样按未指定处理.
        _ => IsoPolarity::Conforms,
    };
    let trigger = match ((raw >> 2) & 0b11) as u8 {
        0b01 => IsoTrigger::Edge,
        0b11 => IsoTrigger::Level,
        _ => IsoTrigger::Conforms,
    };
    IsoFlags { polarity, trigger }
}

/// 在覆盖表中查找源 IRQ (首个匹配项判定, 与固件表内唯一性假设一致).
fn lookup_irq_override(table: &[Option<IrqOverride>], irq: u8) -> Option<IrqOverride> {
    table.iter().flatten().find(|o| o.irq == irq).copied()
}

/// 查询源 IRQ 的 MADT 覆盖项 (无覆盖返回 `None`).
#[must_use]
pub fn irq_override(irq: u8) -> Option<IrqOverride> {
    let table = *IRQ_OVERRIDES.lock();
    lookup_irq_override(&table, irq)
}

/// 源 IRQ → GSI (有 MADT ISO 覆盖时取覆盖值, 否则 GSI = IRQ).
#[must_use]
pub fn irq_to_gsi(irq: u8) -> u32 {
    irq_override(irq).map_or(u32::from(irq), |ov| ov.gsi)
}

/// 获取 MADT 记录的本地 APIC MMIO 基址 (无 MADT 则返回 0)。
pub fn get_lapic_base() -> u64 {
    if !MADT_FOUND.load(Ordering::Acquire) {
        return 0;
    }
    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let madt = unsafe { &*(MADT_BASE.load(Ordering::Acquire) as *const MadtHeader) };
    u64::from(madt.local_apic_addr)
}

/// MADT 解析 C ABI 入口 (转调 `parse_madt`)
#[unsafe(no_mangle)]
pub extern "C" fn acpi_parse_madt(mb2_ptr: u64) -> bool {
    parse_madt(mb2_ptr)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 取已发现 AP 数量 (C ABI)
#[unsafe(no_mangle)]
pub extern "C" fn acpi_get_ap_count() -> u32 {
    get_ap_count()
}

// ============================================================================
// FADT (Fixed ACPI Description Table) — 电源管理
// ============================================================================

/// FADT 结构 (ACPI 2.0+, 关键字段)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct Fadt {
    header: SdtHeader,
    _firmware_ctrl: u32,
    dsdt: u32,
    _reserved1: u8,
    _preferred_pm_profile: u8,
    _sci_int: u16,
    _smi_cmd: u32,
    _acpi_enable: u8,
    _acpi_disable: u8,
    _s4bios_req: u8,
    _pstate_cnt: u8,
    pm1a_evt_blk: u32,
    pm1b_evt_blk: u32,
    pm1a_cnt_blk: u32,
    pm1b_cnt_blk: u32,
    pm2_cnt_blk: u32,
    pm_tmr_blk: u32,
    gpe0_blk: u32,
    gpe1_blk: u32,
    pm1_evt_len: u8,
    pm1_cnt_len: u8,
    pm2_cnt_len: u8,
    pm_tmr_len: u8,
    gpe0_blk_len: u8,
    gpe1_blk_len: u8,
    gpe1_base: u8,
    _cst_cnt: u8,
    _p_lvl2_lat: u16,
    _p_lvl3_lat: u16,
    _flush_size: u16,
    _flush_stride: u16,
    _duty_offset: u8,
    _duty_width: u8,
    _day_alrm: u8,
    _mon_alrm: u8,
    _century: u8,
    _iapc_boot_arch: u16,
    _reserved2: u8,
    flags: u32,
    // ACPI 2.0+ 扩展字段
    reset_reg: [u8; 12], // 通用地址结构 (Generic Address Structure)
    reset_value: u8,
    _reserved3: [u8; 3],
    x_firmware_ctrl: u64,
    x_dsdt: u64,
    x_pm1a_evt_blk: [u8; 12],
    x_pm1b_evt_blk: [u8; 12],
    x_pm1a_cnt_blk: [u8; 12],
    x_pm1b_cnt_blk: [u8; 12],
}

/// ACPI 1.0 FADT 最小长度: 覆盖到 `flags` 末尾 (`reset_reg` 起始处).
///
/// QEMU i440fx 发布 116 字节的 ACPI 1.0 FADT; 用 `size_of::<Fadt>()` 作闸门
/// 会把合法短表整张拒掉, 电源管理发现面又会退回空值.
const FADT_MIN_LEN: usize = core::mem::offset_of!(Fadt, reset_reg);

/// 读取 ACPI 2.0+ 复位寄存器所需最小长度: 覆盖 `reset_reg` + `reset_value`.
const FADT_RESET_MIN_LEN: usize = core::mem::offset_of!(Fadt, reset_value) + 1;

static FADT_ADDR: AtomicU64 = AtomicU64::new(0);
static FADT_LEN: AtomicU32 = AtomicU32::new(0);
static FADT_FOUND: AtomicBool = AtomicBool::new(false);

/// 是否已解析 FADT (B04-21)
/// functions 层委托此 API 替代硬编码 `true`.
#[inline]
pub fn has_fadt() -> bool {
    FADT_FOUND.load(core::sync::atomic::Ordering::Acquire)
}

/// FADT 解析
///
/// 只读取 `FADT_MIN_LEN` 覆盖的前缀字段, 故 ACPI 1.0 短表 (116 字节) 亦合法.
fn parse_fadt(fadt_ptr: u64, table_len: usize) {
    debug_assert!(table_len >= FADT_MIN_LEN);
    // SAFETY: `fadt_ptr` 指向长度 ≥ FADT_MIN_LEN 的 ACPI 表; 此处只读前缀字段
    let fadt = unsafe { &*(fadt_ptr as *const Fadt) };
    FADT_ADDR.store(fadt_ptr, Ordering::Release);
    FADT_LEN.store(fadt.header.length, Ordering::Release);
    FADT_FOUND.store(true, Ordering::Release);

    let pm1a = fadt.pm1a_evt_blk;
    let pm1a_cnt = fadt.pm1a_cnt_blk;
    let flags = fadt.flags;
    crate::klog_info!(
        Acpi,
        "[ACPI] FADT: PM1a_EVT=0x{:X} PM1a_CNT=0x{:X} flags=0x{:X}",
        pm1a,
        pm1a_cnt,
        flags
    );
}

/// ACPI 关机 (S5 状态)
///
/// 通过 FADT 的 `PM1a_CNT` 寄存器写入 `SLP_TYP` + `SLP_EN` 实现关机.
/// QEMU 和大多数硬件支持此方式.
pub fn acpi_shutdown() -> ! {
    if !FADT_FOUND.load(Ordering::Acquire) {
        crate::klog_warn!(Acpi, "[ACPI] FADT not found, cannot ACPI shutdown");
        // 回退: 通过 QEMU debug exit
        #[cfg(target_arch = "x86_64")]

        crate::arch!(outl(0x604u16, 0x2000u32));

        loop {
            core::hint::spin_loop();
        }
    }

    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let fadt = unsafe { &*(FADT_ADDR.load(Ordering::Acquire) as *const Fadt) };
    let pm1a_cnt = fadt.pm1a_cnt_blk;
    let pm1_cnt_len = fadt.pm1_cnt_len;

    // S5 睡眠类型值: 从 DSDT 的 \_S5 对象解析
    // 大多数 QEMU/硬件: SLP_TYP = 0 (PM1a_CNT 写入 0x1C00 或 0x3400)
    // 通用做法: 读取 PM1a_CNT, 设置 SLP_TYP (bits 10-12) + SLP_EN (bit 13)
    // QEMU 默认: S5 SLP_TYP = 0, 所以写入 SLP_EN = 1<<13 = 0x2000
    let slp_typ_s5: u16 = 0; // QEMU 默认, 真实硬件需从 DSDT 解析
    let slp_en: u16 = 1 << 13;
    let value = slp_typ_s5 | slp_en;

    if pm1a_cnt != 0 {
        if pm1_cnt_len == 2 {
            // 16-bit I/O 端口 — 使用 outl 写入 32 位 (低 16 位有效)
            crate::arch!(outl(
                crate::privileged::glue::low_u16_from_u32(pm1a_cnt),
                u32::from(value)
            ));
        } else if pm1_cnt_len == 4 {
            // 32-bit I/O 端口
            crate::arch!(outl(
                crate::privileged::glue::low_u16_from_u32(pm1a_cnt),
                u32::from(value)
            ));
        }
    }

    // 如果关机失败, 回退到 QEMU debug exit
    #[cfg(target_arch = "x86_64")]

    crate::arch!(outl(0x604u16, 0x2000u32));

    loop {
        core::hint::spin_loop();
    }
}

/// FADT 复位寄存器的执行目标 (由 GAS 解码得到).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetTarget {
    /// I/O 端口空间 (`AddressSpace == 1`), 端口号取 GAS 地址的低 16 位.
    IoPort {
        /// 16 位 I/O 端口号.
        port: u16,
        /// 写入值 (FADT `RESET_VALUE`, 单字节).
        value: u8,
    },
    /// 系统内存空间 (`AddressSpace == 0`), 地址与访问宽度均取自 GAS.
    Mmio {
        /// 复位寄存器物理地址 (完整 64 位).
        addr: u64,
        /// 访问宽度 (1/2/4/8 字节).
        width: u8,
        /// 写入值 (FADT `RESET_VALUE`, 按目标宽度零扩展).
        value: u8,
    },
    /// 该 GAS 不可安全执行; 携带显式原因供调用方记录并回退.
    Unsupported(&'static str),
}

/// 解码 FADT 复位寄存器 GAS.
///
/// GAS 布局: `AddressSpace`@0, `RegisterBitWidth`@1, `RegisterBitOffset`@2,
/// `AccessSize`@3, `Address`@4..12 (小端 64 位).
///
/// 系统内存分支的判据 (任一不符即 `Unsupported`, 不产生任何写):
/// - 地址非 0 且在直射区 `[0, DIRECT_MAP_LIMIT)` 内 (窗外未映射, 写了即缺页);
/// - `RegisterBitOffset == 0` (复位寄存器不支持位域);
/// - `AccessSize ∈ {1,2,3,4}` (未定义值拒绝);
/// - `RegisterBitWidth` 为 0 (未指定) 或等于 `AccessSize * 8`;
/// - 地址按访问宽度自然对齐.
///
/// I/O 端口分支要求地址能完整放进 16 位端口空间 (否则截断会写到错误端口);
/// 写宽度沿用 `outb` 的单字节语义.
#[must_use]
fn decode_reset_register(gas: [u8; 12], value: u8) -> ResetTarget {
    let space = gas[0];
    let bit_width = gas[1];
    let bit_offset = gas[2];
    let access_size = gas[3];
    let addr = u64::from_le_bytes([
        gas[4], gas[5], gas[6], gas[7], gas[8], gas[9], gas[10], gas[11],
    ]);

    match space {
        1 => match u16::try_from(addr) {
            Ok(port) => ResetTarget::IoPort { port, value },
            Err(_) => ResetTarget::Unsupported("I/O 端口地址超出 16 位"),
        },
        0 => {
            if addr == 0 {
                return ResetTarget::Unsupported("复位寄存器地址为 0");
            }
            if addr >= crate::privileged::mm::DIRECT_MAP_LIMIT {
                return ResetTarget::Unsupported("复位寄存器地址在直射区外");
            }
            if bit_offset != 0 {
                return ResetTarget::Unsupported("复位寄存器位偏移非 0");
            }
            let width = match access_size {
                1 => 1u8,
                2 => 2,
                3 => 4,
                4 => 8,
                _ => return ResetTarget::Unsupported("复位寄存器 AccessSize 未定义"),
            };
            if bit_width != 0 && u16::from(bit_width) != u16::from(width) * 8 {
                return ResetTarget::Unsupported(
                    "复位寄存器 RegisterBitWidth 与 AccessSize 不一致",
                );
            }
            if !addr.is_multiple_of(u64::from(width)) {
                return ResetTarget::Unsupported("复位寄存器地址未按访问宽度对齐");
            }
            ResetTarget::Mmio { addr, width, value }
        }
        _ => ResetTarget::Unsupported("复位寄存器地址空间非 I/O 或内存"),
    }
}

/// ACPI 重启
pub fn acpi_reboot() -> ! {
    if FADT_FOUND.load(Ordering::Acquire) {
        let fadt_ptr = FADT_ADDR.load(Ordering::Acquire);
        let fadt_len = FADT_LEN.load(Ordering::Acquire) as usize;

        // 方式1: 通过 Reset Register (ACPI 2.0+; ACPI 1.0 短表无该字段)
        if fadt_len >= FADT_RESET_MIN_LEN {
            // SAFETY: `fadt_ptr` 指向长度 ≥ FADT_RESET_MIN_LEN 的 ACPI 表; 只读访问
            let fadt = unsafe { &*(fadt_ptr as *const Fadt) };

            // `Fadt` 是 `repr(C, packed)`, 逐字段按值拷出 (不构造打包字段引用).
            let reset_reg = fadt.reset_reg;
            let reset_val = fadt.reset_value;

            match decode_reset_register(reset_reg, reset_val) {
                ResetTarget::IoPort { port, value } => {
                    crate::arch!(outb(port, value));
                }
                ResetTarget::Mmio { addr, width, value } => {
                    // 直射区地址: `phys_to_virt` 给唯一内核别名, 与当前 CR3
                    // (KERNEL_PML4 / kview) 无关, 不依赖低半区恒等窗是否被用户映射
                    // 覆盖. `decode_reset_register` 已保证 addr 非 0、在
                    // `[0, DIRECT_MAP_LIMIT)` 内且按 width 自然对齐.
                    let va = crate::privileged::mm::phys_to_virt(addr);
                    // SAFETY: 类别「MMIO 复位寄存器写」. 地址取自 FADT GAS 的完整
                    // 64 位字段并经直射区/对齐/宽度校验 (见 decode_reset_register);
                    // 写是设备侧复位触发, 必须 volatile 且不得重排. 无读-改-写,
                    // 不构造引用. `RESET_VALUE` 是单字节, 按目标宽度零扩展写入
                    // (ACPI 只定义值, 未定义高位语义).
                    unsafe {
                        match width {
                            1 => core::ptr::write_volatile(va as *mut u8, value),
                            2 => core::ptr::write_volatile(va as *mut u16, u16::from(value)),
                            4 => core::ptr::write_volatile(va as *mut u32, u32::from(value)),
                            _ => core::ptr::write_volatile(va as *mut u64, u64::from(value)),
                        }
                    }
                }
                ResetTarget::Unsupported(reason) => {
                    crate::klog_kern_warn!("[ACPI] reset register unsupported: {}", reason);
                }
            }
        }
    }

    // 回退: 键盘控制器重启
    #[cfg(target_arch = "x86_64")]

    // 通过键盘控制器脉冲 reset 线路 (端口 0x64, 命令 0xFE)
    crate::arch!(outb(0x64u16, 0xFEu8));

    loop {
        core::hint::spin_loop();
    }
}

// ============================================================================
// HPET (高精度事件定时器)
// ============================================================================

/// HPET 结构
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct HpetTable {
    header: SdtHeader,
    _hardware_rev_id: u8,
    /// 位域字节: bit[4:0] = 比较器数量-1, bit5 = 计数器位宽, bit7 = legacy 替换能力.
    caps: u8,
    _pci_vendor_id: u16,
    address: [u8; 12], // 通用地址结构 (Generic Address Structure)
    hpet_number: u8,
    _min_tick: u16,
    _page_protection: u8,
}

// ACPI 6.x HPET 表固定 56 字节; 位域字节与 GAS 偏移一旦漂移, 解析值即失真.
const _: () = {
    assert!(core::mem::offset_of!(HpetTable, caps) == 37);
    assert!(core::mem::offset_of!(HpetTable, address) == 40);
    assert!(core::mem::offset_of!(HpetTable, hpet_number) == 52);
    assert!(core::mem::size_of::<HpetTable>() == 56);
};

/// 解码 HPET 位域中的比较器数量 (`bit[4:0]` 编码值为数量-1).
#[inline]
fn hpet_comparator_count(caps: u8) -> u8 {
    (caps & 0x1F) + 1
}

/// 解码 HPET 位域中的计数器位宽 (`bit5`: 0 = 32 位, 1 = 64 位).
#[inline]
fn hpet_counter_size(caps: u8) -> u8 {
    (caps >> 5) & 0x01
}

/// HPET 信息
#[derive(Debug, Clone, Copy)]
pub struct HpetInfo {
    /// MMIO 基址
    pub base_addr: u64,
    /// HPET 编号
    pub hpet_number: u8,
    /// 比较器数量
    pub comparator_count: u8,
    /// 计数器位宽 (32 或 64)
    pub counter_size: u8,
}

static HPET_INFO: IrqSpinLock<Option<HpetInfo>> = IrqSpinLock::new(None);

fn parse_hpet(hpet_ptr: u64) {
    // SAFETY: `hpet_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(HpetTable)); 只读访问
    let hpet = unsafe { &*(hpet_ptr as *const HpetTable) };

    // 通用地址结构: [0]=space_id, [4..12]=64 位基址 (低 32 位非零的机器亦正确)
    let base_addr = u64::from_le_bytes([
        hpet.address[4],
        hpet.address[5],
        hpet.address[6],
        hpet.address[7],
        hpet.address[8],
        hpet.address[9],
        hpet.address[10],
        hpet.address[11],
    ]);

    let info = HpetInfo {
        base_addr,
        hpet_number: hpet.hpet_number,
        comparator_count: hpet_comparator_count(hpet.caps),
        counter_size: hpet_counter_size(hpet.caps),
    };

    crate::klog_info!(
        Acpi,
        "[ACPI] HPET: base=0x{:X} comparators={} counter_size={}",
        base_addr,
        info.comparator_count,
        info.counter_size
    );

    *HPET_INFO.lock() = Some(info);
}

/// 获取 HPET 信息
pub fn get_hpet_info() -> Option<HpetInfo> {
    *HPET_INFO.lock()
}

// ============================================================================
// DMAR (DMA 重映射) — IOMMU VT-d
// ============================================================================

/// DMAR 表头
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct DmarTable {
    header: SdtHeader,
    host_addr_width: u8,
    flags: u8,
    _reserved: [u8; 10],
}

/// DMAR Remapping Structure 类型
const DMAR_TYPE_DRHD: u16 = 0x0000;

/// DRHD (DMA 重映射硬件单元定义)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct DrhdEntry {
    r#type: u16,
    length: u16,
    flags: u8,
    _reserved: u8,
    segment_number: u16,
    register_base: u64,
}

/// DMAR DRHD 单元信息
#[derive(Debug, Clone, Copy)]
pub struct DmarDrhdInfo {
    /// MMIO 寄存器基址
    pub register_base: u64,
    /// 段号
    pub segment_number: u16,
    /// 是否包含所有 PCI 设备 (`INCLUDE_ALL`)
    pub include_all: bool,
}

static DMAR_DRHD_LIST: IrqSpinLock<Vec<DmarDrhdInfo>> = IrqSpinLock::new(Vec::new());
static DMAR_HOST_ADDR_WIDTH: IrqSpinLock<u8> = IrqSpinLock::new(0);
static DMAR_FOUND: AtomicBool = AtomicBool::new(false);

fn parse_dmar(dmar_ptr: u64) {
    // SAFETY: `dmar_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(DmarTable)); 只读访问
    let dmar = unsafe { &*(dmar_ptr as *const DmarTable) };
    DMAR_FOUND.store(true, Ordering::Release);
    *DMAR_HOST_ADDR_WIDTH.lock() = dmar.host_addr_width;

    let entries_start =
        crate::privileged::glue::usize_from_u64(dmar_ptr) + core::mem::size_of::<DmarTable>();
    let entries_end =
        crate::privileged::glue::usize_from_u64(dmar_ptr) + dmar.header.length as usize;

    let mut offset = entries_start;
    while offset + 4 <= entries_end {
        // SAFETY: 指针指向已校验的 u16 表项; 只读访问
        let entry_type = unsafe { *((offset) as *const u16) };
        // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
        let entry_len = unsafe { *((offset + 2) as *const u16) };

        if entry_len == 0 {
            break;
        }

        if entry_type == DMAR_TYPE_DRHD && offset + core::mem::size_of::<DrhdEntry>() <= entries_end
        {
            // SAFETY: `offset` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(DrhdEntry)); 只读访问
            let drhd = unsafe { &*(offset as *const DrhdEntry) };
            let info = DmarDrhdInfo {
                register_base: drhd.register_base,
                segment_number: drhd.segment_number,
                include_all: (drhd.flags & 0x01) != 0,
            };
            crate::klog_info!(
                Acpi,
                "[ACPI] DMAR DRHD: reg_base=0x{:X} seg={} include_all={}",
                info.register_base,
                info.segment_number,
                info.include_all
            );
            DMAR_DRHD_LIST.lock().push(info);
        }

        offset += entry_len as usize;
    }

    crate::klog_info!(
        Acpi,
        "[ACPI] DMAR: host_addr_width={} DRHD_count={}",
        dmar.host_addr_width,
        DMAR_DRHD_LIST.lock().len()
    );
}

/// 获取 DMAR DRHD 列表
pub fn get_dmar_drhd_list() -> Vec<DmarDrhdInfo> {
    DMAR_DRHD_LIST.lock().clone()
}

/// 获取主机物理地址宽度
pub fn get_dmar_host_addr_width() -> u8 {
    *DMAR_HOST_ADDR_WIDTH.lock()
}

/// 是否已发现并解析 DMAR 表 (表存在即真, 与 DRHD 条目数无关).
#[inline]
#[must_use]
pub fn has_dmar() -> bool {
    DMAR_FOUND.load(Ordering::Acquire)
}

// ============================================================================
// 统一 SDT 遍历 — 发现所有表
// ============================================================================

/// 解析所有 ACPI 表 (MADT + FADT + HPET + DMAR)
///
/// 启动期由 `smp_init::init` 调用: 单次 RSDT/XSDT 遍历完成全部四类表的发现与解析,
/// 避免先 `parse_madt` 再补扫造成的重复遍历与列表重复登记.
pub fn parse_all_tables(multiboot2_info_ptr: u64) -> bool {
    let Some(rsdp) = find_rsdp(multiboot2_info_ptr) else {
        crate::klog_warn!(Acpi, "[ACPI] RSDP not found");
        return false;
    };

    let Some(rsdt_or_xsdt) = get_rsdt(rsdp) else {
        crate::klog_warn!(Acpi, "[ACPI] RSDT/XSDT not found");
        return false;
    };

    let rsdp_ptr = rsdp as *const u8;
    // SAFETY: 指针指向已通过 BIOS/ACPI 探测验证的物理地址; volatile 访问保证不被编译器重排
    let revision = unsafe { rsdp_ptr.add(15).read_volatile() };
    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let uses_xsdt =
        revision >= 2 && unsafe { rsdp_ptr.add(24).cast::<u64>().read_unaligned() } != 0;

    let table_count = if uses_xsdt {
        (rsdt_or_xsdt.length - 12) / 8
    } else {
        (rsdt_or_xsdt.length - 12) / 4
    } as usize;

    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let entries_ptr = unsafe {
        core::ptr::from_ref::<SdtHeader>(rsdt_or_xsdt)
            .add(1)
            .cast::<u8>()
    };

    for i in 0..table_count {
        // SAFETY: 表项位于 `entries_ptr` 起的 RSDT/XSDT 表体内, 以 `table_count`
        // (由固件 Length 计算) 为界; 4/8 字节读取按 `uses_xsdt` 选择宽度, 且用
        // `read_unaligned` 避开 packed 表体的对齐要求; 只读.
        let table_ptr: u64 = unsafe {
            if uses_xsdt {
                entries_ptr.add(i * 8).cast::<u64>().read_unaligned()
            } else {
                u64::from(entries_ptr.add(i * 4).cast::<u32>().read_unaligned())
            }
        };

        if table_ptr == 0 {
            continue;
        }

        // SAFETY: `table_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(SdtHeader)); 只读访问
        let header = unsafe { &*(table_ptr as *const SdtHeader) };
        let table_len = header.length as usize;

        if header.signature == *b"APIC" {
            parse_madt_entries(table_ptr);
            MADT_BASE.store(table_ptr, Ordering::Release);
            MADT_FOUND.store(true, Ordering::Release);
        } else if header.signature == *b"FACP" {
            if table_len < FADT_MIN_LEN {
                crate::klog_warn!(
                    Acpi,
                    "[ACPI] FADT too short ({} < {}), skipped",
                    table_len,
                    FADT_MIN_LEN
                );
            } else {
                parse_fadt(table_ptr, table_len);
            }
        } else if header.signature == *b"HPET" {
            if table_len < core::mem::size_of::<HpetTable>() {
                crate::klog_warn!(
                    Acpi,
                    "[ACPI] HPET too short ({} < {}), skipped",
                    table_len,
                    core::mem::size_of::<HpetTable>()
                );
            } else {
                parse_hpet(table_ptr);
            }
        } else if header.signature == *b"DMAR" {
            if table_len < core::mem::size_of::<DmarTable>() {
                crate::klog_warn!(
                    Acpi,
                    "[ACPI] DMAR too short ({} < {}), skipped",
                    table_len,
                    core::mem::size_of::<DmarTable>()
                );
            } else {
                parse_dmar(table_ptr);
            }
        }
    }

    crate::klog_info!(
        Acpi,
        "[ACPI] Table scan complete: MADT={} FADT={} HPET={} DMAR_DRHD={}",
        MADT_FOUND.load(Ordering::Acquire),
        FADT_FOUND.load(Ordering::Acquire),
        HPET_INFO.lock().is_some(),
        DMAR_DRHD_LIST.lock().len()
    );

    true
}

// ============================================================================
// C-ABI 兼容接口
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 全部 ACPI 表解析 C ABI 入口 (转调 `parse_all_tables`)
#[unsafe(no_mangle)]
pub extern "C" fn acpi_parse_all(mb2_ptr: u64) -> bool {
    parse_all_tables(mb2_ptr)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 系统关机 C ABI 入口 (不返回)
#[unsafe(no_mangle)]
pub extern "C" fn acpi_shutdown_system() -> ! {
    acpi_shutdown()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 系统重启 C ABI 入口 (不返回)
#[unsafe(no_mangle)]
pub extern "C" fn acpi_reboot_system() -> ! {
    acpi_reboot()
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn flags_of(raw: u16) -> IsoFlags {
        decode_iso_flags(raw)
    }

    #[test]
    fn hpet_caps_decode_matches_acpi_bitfields() {
        // QEMU i440fx: 3 comparators (编码 2) + 64 位计数器 (bit5) + legacy 能力 (bit7)
        let caps = 0xA2;
        assert_eq!(hpet_comparator_count(caps), 3);
        assert_eq!(hpet_counter_size(caps), 1);
        // 1 comparator (编码 0) + 32 位计数器
        assert_eq!(hpet_comparator_count(0x00), 1);
        assert_eq!(hpet_counter_size(0x00), 0);
    }

    #[test]
    fn fadt_length_tiers_match_acpi_1_0_and_2_0_offsets() {
        // ACPI 1.0 FADT 到 flags 末尾为 116 字节; 复位字段在其后的 ACPI 2.0+ 扩展区
        assert_eq!(FADT_MIN_LEN, 116);
        assert_eq!(FADT_RESET_MIN_LEN, 129);
        assert!(FADT_RESET_MIN_LEN <= core::mem::size_of::<Fadt>());
    }

    /// 拼装 12 字节 GAS.
    fn reset_gas(space: u8, bit_width: u8, bit_offset: u8, access_size: u8, addr: u64) -> [u8; 12] {
        let mut gas = [0u8; 12];
        gas[0] = space;
        gas[1] = bit_width;
        gas[2] = bit_offset;
        gas[3] = access_size;
        gas[4..12].copy_from_slice(&addr.to_le_bytes());
        gas
    }

    #[test]
    fn reset_gas_mmio_byte_is_decoded_with_full_address() {
        let gas = reset_gas(0, 8, 0, 1, 0xFED0_0000);
        assert_eq!(
            decode_reset_register(gas, 0x06),
            ResetTarget::Mmio {
                addr: 0xFED0_0000,
                width: 1,
                value: 0x06
            }
        );
    }

    #[test]
    fn reset_gas_uses_full_64_bit_address_not_low_32() {
        // 旧实现只取 GAS 地址的低 32 位, 会把 0x1_0000_1234 写成 0x1234.
        // 现须按完整 64 位判直射区并显式拒绝.
        let gas = reset_gas(0, 8, 0, 1, 0x1_0000_1234);
        assert_eq!(
            decode_reset_register(gas, 0x06),
            ResetTarget::Unsupported("复位寄存器地址在直射区外")
        );
    }

    #[test]
    fn reset_gas_direct_map_boundary_is_exclusive() {
        let at_limit = reset_gas(0, 8, 0, 1, crate::privileged::mm::DIRECT_MAP_LIMIT);
        assert_eq!(
            decode_reset_register(at_limit, 0x06),
            ResetTarget::Unsupported("复位寄存器地址在直射区外")
        );
        let last = crate::privileged::mm::DIRECT_MAP_LIMIT - 1;
        let inside = reset_gas(0, 8, 0, 1, last);
        assert_eq!(
            decode_reset_register(inside, 0x06),
            ResetTarget::Mmio {
                addr: last,
                width: 1,
                value: 0x06
            }
        );
    }

    #[test]
    fn reset_gas_honours_access_size() {
        // AccessSize=3(DWord)/4(QWord) 映射到 4/8 字节写; BitWidth=0 视为未指定.
        assert_eq!(
            decode_reset_register(reset_gas(0, 0, 0, 3, 0x1000), 1),
            ResetTarget::Mmio {
                addr: 0x1000,
                width: 4,
                value: 1
            }
        );
        assert_eq!(
            decode_reset_register(reset_gas(0, 64, 0, 4, 0x2000), 1),
            ResetTarget::Mmio {
                addr: 0x2000,
                width: 8,
                value: 1
            }
        );
    }

    #[test]
    fn reset_gas_rejects_inconsistent_or_unsafe_mmio() {
        // BitWidth 与 AccessSize 不一致
        assert_eq!(
            decode_reset_register(reset_gas(0, 8, 0, 3, 0x1000), 1),
            ResetTarget::Unsupported("复位寄存器 RegisterBitWidth 与 AccessSize 不一致")
        );
        // 位偏移非 0
        assert_eq!(
            decode_reset_register(reset_gas(0, 8, 4, 1, 0x1000), 1),
            ResetTarget::Unsupported("复位寄存器位偏移非 0")
        );
        // AccessSize 未定义
        assert_eq!(
            decode_reset_register(reset_gas(0, 0, 0, 0, 0x1000), 1),
            ResetTarget::Unsupported("复位寄存器 AccessSize 未定义")
        );
        // 地址 0
        assert_eq!(
            decode_reset_register(reset_gas(0, 8, 0, 1, 0), 1),
            ResetTarget::Unsupported("复位寄存器地址为 0")
        );
        // 未按访问宽度对齐 (Word 写到奇地址)
        assert_eq!(
            decode_reset_register(reset_gas(0, 16, 0, 2, 0x1001), 1),
            ResetTarget::Unsupported("复位寄存器地址未按访问宽度对齐")
        );
        // 非 I/O 非内存地址空间
        assert_eq!(
            decode_reset_register(reset_gas(2, 8, 0, 1, 0x1000), 1),
            ResetTarget::Unsupported("复位寄存器地址空间非 I/O 或内存")
        );
    }

    #[test]
    fn reset_gas_io_port_requires_16_bit_address() {
        assert_eq!(
            decode_reset_register(reset_gas(1, 8, 0, 1, 0xCF9), 0x06),
            ResetTarget::IoPort {
                port: 0xCF9,
                value: 0x06
            }
        );
        // 高 32 位非零的地址不能被截断成另一个端口.
        assert_eq!(
            decode_reset_register(reset_gas(1, 8, 0, 1, 0x1_0000_CF9), 0x06),
            ResetTarget::Unsupported("I/O 端口地址超出 16 位")
        );
    }

    #[test]
    fn iso_flags_conform_to_bus_is_unspecified() {
        let flags = flags_of(0x0000);
        assert_eq!(flags.polarity, IsoPolarity::Conforms);
        assert_eq!(flags.trigger, IsoTrigger::Conforms);
    }

    #[test]
    fn iso_flags_explicit_level_low() {
        // 0x000F = 低电平有效 (bit[1:0]=11) + 电平触发 (bit[3:2]=11)
        let flags = flags_of(0x000F);
        assert_eq!(flags.polarity, IsoPolarity::ActiveLow);
        assert_eq!(flags.trigger, IsoTrigger::Level);
    }

    #[test]
    fn iso_flags_explicit_level_high() {
        // 0x000D = 高电平有效 (bit[1:0]=01) + 电平触发 (bit[3:2]=11)
        let flags = flags_of(0x000D);
        assert_eq!(flags.polarity, IsoPolarity::ActiveHigh);
        assert_eq!(flags.trigger, IsoTrigger::Level);
    }

    #[test]
    fn iso_flags_explicit_edge_high() {
        // 0x0005 = 高电平有效 (01) + 边沿触发 (01)
        let flags = flags_of(0x0005);
        assert_eq!(flags.polarity, IsoPolarity::ActiveHigh);
        assert_eq!(flags.trigger, IsoTrigger::Edge);
    }

    #[test]
    fn iso_flags_reserved_encoding_is_unspecified() {
        // 保留编码 10 不得被当作已指定
        let flags = flags_of(0x000A);
        assert_eq!(flags.polarity, IsoPolarity::Conforms);
        assert_eq!(flags.trigger, IsoTrigger::Conforms);
    }

    #[test]
    fn iso_override_lookup_returns_first_match() {
        let over = |irq: u8, gsi: u32| {
            Some(IrqOverride {
                irq,
                gsi,
                flags: decode_iso_flags(0x000D),
            })
        };
        let table = [over(0, 2), None, over(9, 9)];
        assert_eq!(lookup_irq_override(&table, 0).map(|o| o.gsi), Some(2));
        assert_eq!(lookup_irq_override(&table, 9).map(|o| o.gsi), Some(9));
        assert!(lookup_irq_override(&table, 11).is_none());
    }

    // ---- MB2 内联 RSDP 副本解析 (D4-i 甲) ----

    /// 补齐 RSDP 的两面校验和 (v1 面 @8 覆盖前 20 B; v2 扩展面 @32 覆盖整段).
    fn fix_rsdp_checksums(b: &mut [u8]) {
        b[8] = 0;
        let s1 = b[..20].iter().fold(0u8, |a, x| a.wrapping_add(*x));
        b[8] = 0u8.wrapping_sub(s1);
        if b.len() >= 36 {
            b[32] = 0;
            let s2 = b.iter().fold(0u8, |a, x| a.wrapping_add(*x));
            b[32] = 0u8.wrapping_sub(s2);
        }
    }

    /// RSDP v1 副本 (20 B, revision 0, 只带 RSDT 地址).
    fn rsdp_v1(rsdt: u32) -> Vec<u8> {
        let mut b = b"RSD PTR ".to_vec();
        b.push(0); // checksum @8
        b.extend_from_slice(b"EDGINE"); // OEMID @9..15
        b.push(0); // revision @15
        b.extend_from_slice(&rsdt.to_le_bytes()); // rsdt_address @16
        fix_rsdp_checksums(&mut b);
        b
    }

    /// RSDP v2 副本 (36 B, revision 2, 带 length / XSDT 地址 / 扩展校验和).
    fn rsdp_v2(xsdt: u64) -> Vec<u8> {
        let mut b = rsdp_v1(0);
        b[15] = 2;
        b.extend_from_slice(&36u32.to_le_bytes()); // length @20
        b.extend_from_slice(&xsdt.to_le_bytes()); // xsdt_address @24
        b.push(0); // extended_checksum @32
        b.extend_from_slice(&[0u8; 3]); // reserved @33..36
        fix_rsdp_checksums(&mut b);
        b
    }

    /// tag 14 / 15: 8 字节头 + RSDP **内联副本** (实测 size 分别 28 / 44).
    fn acpi_tag(kind: u32, rsdp: &[u8]) -> Vec<u8> {
        let size = u32::try_from(8 + rsdp.len()).expect("ACPI 标签长度溢出 u32");
        let mut t = kind.to_le_bytes().to_vec();
        t.extend_from_slice(&size.to_le_bytes());
        t.extend_from_slice(rsdp);
        t
    }

    /// 拼装 MB2 标签表 (8 字节头 + 逐标签补到 8 字节对齐 + 终止标签).
    fn mb2_blob(tags: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for tag in tags {
            body.extend_from_slice(tag);
            while body.len() % 8 != 0 {
                body.push(0);
            }
        }
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&8u32.to_le_bytes());
        let total = u32::try_from(8 + body.len()).expect("标签表长度溢出 u32");
        let mut out = total.to_le_bytes().to_vec();
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn mb2_rsdp_payload_is_inline_copy_not_pointer() {
        // 核心回归锁: 载荷首 8 字节就是签名本身. 原实现把它按 u64 读成"RSDP 地址",
        // 得到 0x2052_5450_2044_5352 —— 非规范地址, 解引用即 #GP.
        let blob = mb2_blob(&[acpi_tag(15, &rsdp_v2(0xFED0_0000))]);
        let offset = rsdp_offset_in_mb2(&blob).expect("tag 15 内联副本应被识别");
        assert_eq!(
            offset, 16,
            "8 块头 + 8 标签头 → 副本在 16, 不是载荷里存的地址"
        );
        let as_pointer = u64::from_le_bytes(blob[offset..offset + 8].try_into().unwrap());
        assert_eq!(as_pointer, 0x2052_5450_2044_5352, "签名的小端整数即误读值");
        assert_eq!(&blob[offset..offset + 8], b"RSD PTR ");
    }

    #[test]
    fn mb2_rsdp_prefers_v2_regardless_of_tag_order() {
        let v1 = acpi_tag(14, &rsdp_v1(0x1F00_0000));
        let v2 = acpi_tag(15, &rsdp_v2(0x1F10_0000));
        // GRUB 实测次序是 14 在前 15 在后; 两种次序都必须取 v2 (带 XSDT).
        for tags in [[v1.clone(), v2.clone()], [v2, v1]] {
            let blob = mb2_blob(&tags);
            let offset = rsdp_offset_in_mb2(&blob).expect("两标签并存应识别");
            assert_eq!(blob[offset + 15], 2, "必须取 revision 2 的那份");
        }
    }

    #[test]
    fn mb2_rsdp_v1_only_uses_tag14() {
        let blob = mb2_blob(&[acpi_tag(14, &rsdp_v1(0x1F00_0000))]);
        let offset = rsdp_offset_in_mb2(&blob).expect("只有旧版标签时取 tag 14");
        assert_eq!(offset, 16);
        assert_eq!(blob[offset + 15], 0);
    }

    #[test]
    fn mb2_rsdp_offset_accounts_for_tag_padding() {
        // tag 1 (cmdline) 长度非 8 倍数, 其后标签须按 8 字节对齐后才开始.
        let mut cmdline = 1u32.to_le_bytes().to_vec();
        cmdline.extend_from_slice(&9u32.to_le_bytes());
        cmdline.push(b'x');
        let blob = mb2_blob(&[cmdline, acpi_tag(15, &rsdp_v2(0x1F10_0000))]);
        let offset = rsdp_offset_in_mb2(&blob).expect("补齐后仍应定位到副本");
        assert_eq!(
            offset, 32,
            "8 块头 + 9 字节 tag1 补到 16 + 8 标签头 → 副本在 32"
        );
        assert_eq!(&blob[offset..offset + 8], b"RSD PTR ");
    }

    #[test]
    fn mb2_rsdp_bad_v2_checksum_falls_back_to_v1() {
        let mut bad = rsdp_v2(0x1F10_0000);
        bad[32] = bad[32].wrapping_add(1); // 破坏扩展校验和
        let blob = mb2_blob(&[acpi_tag(15, &bad), acpi_tag(14, &rsdp_v1(0x1F00_0000))]);
        let offset = rsdp_offset_in_mb2(&blob).expect("v2 校验失败应退回 v1");
        assert_eq!(blob[offset + 15], 0, "取的是 tag 14 的 v1 副本");
        // 只有坏 v2 时无可用副本 —— 不静默放行.
        assert!(rsdp_offset_in_mb2(&mb2_blob(&[acpi_tag(15, &bad)])).is_none());
    }

    #[test]
    fn mb2_rsdp_without_acpi_tags_returns_none() {
        let blob = mb2_blob(&[acpi_tag(21, &[0u8; 4])]);
        assert!(rsdp_offset_in_mb2(&blob).is_none());
        assert!(rsdp_offset_in_mb2(&mb2_blob(&[])).is_none());
    }

    #[test]
    fn rsdp_bytes_valid_rejects_truncated_v2_face() {
        // revision 声称 2 但只给 20 字节: v2 扩展面无从校验, 判否而非按 v1 放行.
        let mut short = rsdp_v1(0x1F00_0000);
        short[15] = 2;
        fix_rsdp_checksums(&mut short);
        assert!(!rsdp_bytes_valid(&short));
        assert!(rsdp_bytes_valid(&rsdp_v1(0x1F00_0000)));
        assert!(rsdp_bytes_valid(&rsdp_v2(0x1F10_0000)));
        assert!(!rsdp_bytes_valid(b"RSD PTR "));
        assert!(!rsdp_bytes_valid(&[]));
    }
}
