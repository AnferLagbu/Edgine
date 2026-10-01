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

use crate::framework::sync::{IrqSpinLock, OnceLock};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub use crate::framework::config::MAX_CPUS;

/// MADT 条目类型
const MADT_TYPE_LAPIC: u8 = 0x00;
const MADT_TYPE_IOAPIC: u8 = 0x01;
const _MADT_TYPE_ISO: u8 = 0x02;
const _MADT_TYPE_NMI: u8 = 0x04;

static MADT_BASE: AtomicU64 = AtomicU64::new(0);
static MADT_FOUND: AtomicBool = AtomicBool::new(false);

/// 发现的 AP LAPIC 信息
#[derive(Debug, Clone, Copy)]
pub struct ApInfo {
    pub lapic_id: u32,
    pub apic_id: u32,
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

pub fn find_rsdp(multiboot2_info_ptr: u64) -> Option<u64> {
    // 1. 尝试从 Multiboot2 info 中获取 RSDP
    if multiboot2_info_ptr != 0 {
        if let Some(rsdp) = find_rsdp_from_mb2(multiboot2_info_ptr) {
            return Some(rsdp);
        }
    }

    // 2. 扫描 EBDA (Extended BIOS Data Area, 通常在 0x80000-0x9FFFF)
    if let Some(rsdp) = scan_ebda() {
        return Some(rsdp);
    }

    // 3. 扫描 BIOS ROM 区域 (0xE0000-0xFFFFF)
    scan_bios_rom()
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
fn find_rsdp_from_mb2(mb2_ptr: u64) -> Option<u64> {
    let ptr = mb2_ptr as *const u8;
    // SAFETY: `ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(u32)); 只读访问
    let total_size = unsafe { *(ptr as *const u32) };

    let mut offset: usize = 8;
    while offset + 8 <= total_size as usize {
        // SAFETY: 指针指向有效的 ACPI/Multiboot2 表 (长度已校验 ≥ offset / 4+sizeof(u32)); 只读访问
        let tag_type = unsafe { *((ptr as *const u32).add(offset / 4)) };
        // SAFETY: 指针指向有效的 ACPI/Multiboot2 表 (长度已校验 ≥ offset / 4 + 1+sizeof(u32)); 只读访问
        let tag_size = unsafe { *((ptr as *const u32).add(offset / 4 + 1)) };

        if tag_type == 0 || tag_size == 0 {
            break;
        }

        // Multiboot2 tag 14 = 旧版 ACPI RSDP, tag 15 = 新版 RSDP
        if tag_type == 14 || tag_type == 15 {
            // SAFETY: 指针指向有效的 ACPI/Multiboot2 表 (长度已校验 ≥ offset / 8 + 1+sizeof(u64)); 只读访问
            let rsdp_ptr = unsafe { *((ptr as *const u64).add(offset / 8 + 1)) };
            if is_valid_rsdp(rsdp_ptr) {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    crate::framework::klog::klog_info(c"[ACPI] RSDP found via Multiboot2".as_ptr());
                }
                return Some(rsdp_ptr);
            }
        }

        offset += tag_size as usize;
        // 对齐到 8 字节边界
        offset = (offset + 7) & !7;
    }

    None
}

fn scan_ebda() -> Option<u64> {
    // EBDA 基址存于 BDA (BIOS Data Area) 偏移 0x40E
    let bda_ptr = 0x400 as *const u16;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
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
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                crate::framework::klog::klog_info(c"[ACPI] RSDP found via BIOS scan".as_ptr());
            }
            return Some(addr);
        }
        addr += 16;
    }
    None
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
fn is_valid_rsdp(addr: u64) -> bool {
    let ptr = addr as *const u8;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        // "RSD PTR " 签名字符串
        let sig: &[u8; 8] = &*(ptr as *const [u8; 8]);
        if sig != b"RSD PTR " {
            return false;
        }

        // 校验和: 前 20 个字节的校验和必须为 0
        let mut sum: u8 = 0;
        for i in 0..20 {
            sum = sum.wrapping_add(ptr.add(i).read_volatile());
        }
        if sum != 0 {
            return false;
        }

        true
    }
}

// ============================================================================
// SDT 解析
// ============================================================================

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
fn get_rsdt(rsdp: u64) -> Option<&'static SdtHeader> {
    let ptr = rsdp as *const u8;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let revision = ptr.add(15).read_volatile();
        if revision >= 2 {
            // XSDT (64 位指针, 仅在 revision >= 2 中出现)
            let xsdt_addr = *(ptr.add(24) as *const u64);
            if xsdt_addr != 0 {
                return Some(&*(xsdt_addr as *const SdtHeader));
            }
        }

        // RSDT (32-bit pointers)
        let rsdt_addr = u64::from(*(ptr.add(16) as *const u32));
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

// ============================================================================
// MADT 解析 — 核心公共接口
// ============================================================================

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn parse_madt(multiboot2_info_ptr: u64) -> bool {
    let rsdp = if let Some(addr) = find_rsdp(multiboot2_info_ptr) {
        addr
    } else {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            crate::framework::klog::klog_info(c"[ACPI] RSDP not found".as_ptr());
        }
        return false;
    };

    let rsdt_or_xsdt = if let Some(sdt) = get_rsdt(rsdp) {
        sdt
    } else {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            crate::framework::klog::klog_info(c"[ACPI] RSDT/XSDT not found".as_ptr());
        }
        return false;
    };

    let rsdp_ptr = rsdp as *const u8;
    // SAFETY: 指针指向已通过 BIOS/ACPI 探测验证的物理地址; volatile 访问保证不被编译器重排
    let revision = unsafe { rsdp_ptr.add(15).read_volatile() };
    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let uses_xsdt = revision >= 2 && unsafe { *(rsdp_ptr.add(24) as *const u64) } != 0;

    let table_count = if uses_xsdt {
        (rsdt_or_xsdt.length - 12) / 8
    } else {
        (rsdt_or_xsdt.length - 12) / 4
    } as usize;

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let entries_ptr = (rsdt_or_xsdt as *const SdtHeader).add(1) as *const u8;

        for i in 0..table_count {
            let madt_ptr: u64 = if uses_xsdt {
                *(entries_ptr.add(i * 8) as *const u64)
            } else {
                u64::from(*(entries_ptr.add(i * 4) as *const u32))
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

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        crate::framework::klog::klog_info(c"[ACPI] MADT not found in RSDT/XSDT".as_ptr());
    }
    false
}

// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
fn parse_madt_entries(madt_ptr: u64) {
    // SAFETY: `madt_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(MadtHeader)); 只读访问
    let madt = unsafe { &*(madt_ptr as *const MadtHeader) };
    let lapic_base = u64::from(madt.local_apic_addr);

    let entries_start = madt_ptr as usize + core::mem::size_of::<MadtHeader>();
    let entries_end = madt_ptr as usize + madt.header.length as usize;

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
                    AP_COUNT.store((idx + 1) as u32, Ordering::Release);
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
                    IOAPIC_COUNT.store((idx + 1) as u32, Ordering::Release);
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

pub fn get_ap_count() -> u32 {
    AP_COUNT.load(Ordering::Acquire)
}

pub fn get_ap_list() -> Box<[Option<ApInfo>]> {
    ap_list().lock().clone()
}

pub fn get_ap(index: usize) -> Option<ApInfo> {
    ap_list().lock()[index]
}

pub fn has_madt() -> bool {
    MADT_FOUND.load(Ordering::Acquire)
}

pub fn get_ioapic_addr() -> u64 {
    let ioapics = IOAPICS.lock();
    ioapics.iter().flatten().next().map_or(0, |i| i.base_addr)
}

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
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn gsi_to_ioapic(gsi: u32) -> Option<(usize, u8)> {
    let ioapics = IOAPICS.lock();
    for (i, ioapic) in ioapics.iter().enumerate() {
        if let Some(info) = ioapic {
            if gsi >= info.gsi_base && gsi < info.gsi_base + u32::from(info.max_irq) {
                return Some((i, (gsi - info.gsi_base) as u8));
            }
        }
    }
    None
}

pub fn get_lapic_base() -> u64 {
    if !MADT_FOUND.load(Ordering::Acquire) {
        return 0;
    }
    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let madt = unsafe { &*(MADT_BASE.load(Ordering::Acquire) as *const MadtHeader) };
    u64::from(madt.local_apic_addr)
}

#[unsafe(no_mangle)]
pub extern "C" fn acpi_parse_madt(mb2_ptr: u64) -> bool {
    parse_madt(mb2_ptr)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
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

static FADT_ADDR: AtomicU64 = AtomicU64::new(0);
static FADT_FOUND: AtomicBool = AtomicBool::new(false);

/// 是否已解析 FADT (B04-21)
/// services 层委托此 API 替代硬编码 `true`.
#[inline]
pub fn has_fadt() -> bool {
    FADT_FOUND.load(core::sync::atomic::Ordering::Acquire)
}

/// FADT 解析
fn parse_fadt(fadt_ptr: u64) {
    // SAFETY: `fadt_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(Fadt)); 只读访问
    let fadt = unsafe { &*(fadt_ptr as *const Fadt) };
    FADT_ADDR.store(fadt_ptr, Ordering::Release);
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
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn acpi_shutdown() -> ! {
    if !FADT_FOUND.load(Ordering::Acquire) {
        crate::klog_warn!(Acpi, "[ACPI] FADT not found, cannot ACPI shutdown");
        // 回退: 通过 QEMU debug exit
        #[cfg(target_arch = "x86_64")]

        crate::arch!(outl(0x604u16, 0x2000u32));

        loop {}
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
            crate::arch!(outl(pm1a_cnt as u16, u32::from(value)));
        } else if pm1_cnt_len == 4 {
            // 32-bit I/O 端口
            crate::arch!(outl(pm1a_cnt as u16, u32::from(value)));
        }
    }

    // 如果关机失败, 回退到 QEMU debug exit
    #[cfg(target_arch = "x86_64")]

    crate::arch!(outl(0x604u16, 0x2000u32));

    loop {}
}

/// ACPI 重启
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn acpi_reboot() -> ! {
    if FADT_FOUND.load(Ordering::Acquire) {
        // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
        let fadt = unsafe { &*(FADT_ADDR.load(Ordering::Acquire) as *const Fadt) };

        // 方式1: 通过 Reset Register (ACPI 2.0+)
        let reset_reg_space = fadt.reset_reg[0];
        let reset_reg_addr = u32::from_le_bytes([
            fadt.reset_reg[4],
            fadt.reset_reg[5],
            fadt.reset_reg[6],
            fadt.reset_reg[7],
        ]);
        let reset_val = fadt.reset_value;

        if reset_reg_space == 1 {
            // I/O 端口空间

            crate::arch!(outb(reset_reg_addr as u16, reset_val));
        }
        // 方式2: MMIO 空间 (reset_reg_space == 0)
        else if reset_reg_space == 0 && reset_reg_addr != 0 {
            unsafe {
                core::ptr::write_volatile(reset_reg_addr as *mut u8, reset_val);
            }
        }
    }

    // 回退: 键盘控制器重启
    #[cfg(target_arch = "x86_64")]

    // 通过键盘控制器脉冲 reset 线路 (端口 0x64, 命令 0xFE)
    crate::arch!(outb(0x64u16, 0xFEu8));

    loop {}
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
    _comparator_count: u8,
    _counter_size: u8,
    _reserved1: u8,
    _pci_vendor_id: u16,
    address: [u8; 12], // 通用地址结构 (Generic Address Structure)
    hpet_number: u8,
    _min_tick: u16,
    _page_protection: u8,
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

#[expect(
    clippy::used_underscore_binding,
    reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
)]
fn parse_hpet(hpet_ptr: u64) {
    // SAFETY: `hpet_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(HpetTable)); 只读访问
    let hpet = unsafe { &*(hpet_ptr as *const HpetTable) };

    // 通用地址结构: [0]=space_id, [4..8]=address
    let base_addr = u64::from_le_bytes([
        hpet.address[4],
        hpet.address[5],
        hpet.address[6],
        hpet.address[7],
        0,
        0,
        0,
        0,
    ]);

    let info = HpetInfo {
        base_addr,
        hpet_number: hpet.hpet_number,
        comparator_count: hpet._comparator_count,
        counter_size: hpet._counter_size,
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

// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
fn parse_dmar(dmar_ptr: u64) {
    // SAFETY: `dmar_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(DmarTable)); 只读访问
    let dmar = unsafe { &*(dmar_ptr as *const DmarTable) };
    *DMAR_HOST_ADDR_WIDTH.lock() = dmar.host_addr_width;

    let entries_start = dmar_ptr as usize + core::mem::size_of::<DmarTable>();
    let entries_end = dmar_ptr as usize + dmar.header.length as usize;

    let mut offset = entries_start;
    while offset + 4 <= entries_end {
        // SAFETY: 指针指向已校验的 u16 表项; 只读访问
        let entry_type = unsafe { *((offset) as *const u16) };
        // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
        let entry_len = unsafe { *((offset + 2) as *const u16) };

        if entry_len == 0 {
            break;
        }

        if entry_type == DMAR_TYPE_DRHD {
            if offset + core::mem::size_of::<DrhdEntry>() <= entries_end {
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

// ============================================================================
// 统一 SDT 遍历 — 发现所有表
// ============================================================================

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 解析所有 ACPI 表 (MADT + FADT + HPET + DMAR)
///
/// 在内核启动时调用, 替代仅解析 MADT 的 `parse_madt`.
pub fn parse_all_tables(multiboot2_info_ptr: u64) -> bool {
    let rsdp = if let Some(addr) = find_rsdp(multiboot2_info_ptr) {
        addr
    } else {
        crate::klog_warn!(Acpi, "[ACPI] RSDP not found");
        return false;
    };

    let rsdt_or_xsdt = if let Some(sdt) = get_rsdt(rsdp) {
        sdt
    } else {
        crate::klog_warn!(Acpi, "[ACPI] RSDT/XSDT not found");
        return false;
    };

    let rsdp_ptr = rsdp as *const u8;
    // SAFETY: 指针指向已通过 BIOS/ACPI 探测验证的物理地址; volatile 访问保证不被编译器重排
    let revision = unsafe { rsdp_ptr.add(15).read_volatile() };
    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let uses_xsdt = revision >= 2 && unsafe { *(rsdp_ptr.add(24) as *const u64) } != 0;

    let table_count = if uses_xsdt {
        (rsdt_or_xsdt.length - 12) / 8
    } else {
        (rsdt_or_xsdt.length - 12) / 4
    } as usize;

    // SAFETY: `const` 指向 ACPI/BIOS 探测过的物理地址; 只读访问
    let entries_ptr = unsafe { (rsdt_or_xsdt as *const SdtHeader).add(1) as *const u8 };

    for i in 0..table_count {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let table_ptr: u64 = unsafe {
            if uses_xsdt {
                *(entries_ptr.add(i * 8) as *const u64)
            } else {
                u64::from(*(entries_ptr.add(i * 4) as *const u32))
            }
        };

        if table_ptr == 0 {
            continue;
        }

        // SAFETY: `table_ptr` 指向已验证有效的 ACPI/BIOS 表头 (长度 ≥ sizeof(SdtHeader)); 只读访问
        let header = unsafe { &*(table_ptr as *const SdtHeader) };

        if header.signature == *b"APIC" {
            parse_madt_entries(table_ptr);
            MADT_BASE.store(table_ptr, Ordering::Release);
            MADT_FOUND.store(true, Ordering::Release);
        } else if header.signature == *b"FACP" {
            parse_fadt(table_ptr);
        } else if header.signature == *b"HPET" {
            parse_hpet(table_ptr);
        } else if header.signature == *b"DMAR" {
            parse_dmar(table_ptr);
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
#[unsafe(no_mangle)]
pub extern "C" fn acpi_parse_all(mb2_ptr: u64) -> bool {
    parse_all_tables(mb2_ptr)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn acpi_shutdown_system() -> ! {
    acpi_shutdown()
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn acpi_reboot_system() -> ! {
    acpi_reboot()
}
