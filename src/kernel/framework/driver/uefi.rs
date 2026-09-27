//! UEFI 运行时服务 — 固件接口抽象
//!
//! ## 设计
//!
//! UEFI (Unified Extensible Firmware Interface) 替代传统 BIOS, 提供标准化的
//! 固件服务接口. 本模块实现内核对 UEFI 运行时服务的访问:
//!
//! 1. **运行时服务**: GetTime/SetTime, GetVariable/SetVariable, ResetSystem
//! 2. **GOP (Graphics Output Protocol)**: 帧缓冲区信息
//! 3. **变量存储**: UEFI 变量读写 (BootOrder, ConOut 等)
//! 4. **内存映射**: UEFI 内存描述符转换
//!
//! ### 与 Linux 的差异
//!
//! 1. **无 efivarfs**: 不挂载文件系统接口, 使用 syscall
//! 2. **无 EFI_PSTORE**: 不支持 pstore 后端
//! 3. **无 EFI_RNG**: 不使用 UEFI 随机数
//! 4. **运行时服务通过物理地址映射访问**: SetVirtualAddressMap 后
//!
//! ## SAFETY
//!
//! 本模块属于 framework/TCB, 允许 unsafe.
//! UEFI 运行时服务调用涉及物理地址映射和固件调用.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::framework::sync::IrqSpinLock;
use alloc::vec;
use alloc::vec::Vec;

// ============================================================================
// EFI_SYSTEM_TABLE 布局 (x86_64 UEFI 2.x, 物理地址视图)
// ============================================================================

/// EFI_SYSTEM_TABLE 签名 — "IBI SYST"
pub const EFI_SYSTEM_TABLE_SIGNATURE: u64 = 0x5453595320494249;

/// `EFI_TABLE_HEADER.Signature` 偏移 (0x00)
const ST_OFF_SIGNATURE: u64 = 0x00;
/// `FirmwareVendor` (CHAR16*) 偏移 (0x18)
const ST_OFF_FIRMWARE_VENDOR: u64 = 0x18;
/// `FirmwareRevision` (UINT32) 偏移 (0x20)
const ST_OFF_FIRMWARE_REVISION: u64 = 0x20;
/// `RuntimeServices` (EFI_RUNTIME_SERVICES*) 偏移 (0x58)
const ST_OFF_RUNTIME_SERVICES: u64 = 0x58;
/// `BootServices` (EFI_BOOT_SERVICES*) 偏移 (0x60)
const ST_OFF_BOOT_SERVICES: u64 = 0x60;
/// `NumberOfTableEntries` (UINTN) 偏移 (0x68)
const ST_OFF_NUM_TABLE_ENTRIES: u64 = 0x68;
/// `ConfigurationTable` (EFI_CONFIGURATION_TABLE*) 偏移 (0x70)
const ST_OFF_CONFIG_TABLE: u64 = 0x70;

// ============================================================================
// 常量
// ============================================================================

/// UEFI 变量最大名称长度 (字符)
pub const EFI_MAX_VAR_NAME: usize = 1024;
/// UEFI 变量最大数据大小
pub const EFI_MAX_VAR_DATA: usize = 32768;
/// UEFI 变量属性: 非易失性
pub const EFI_VARIABLE_NON_VOLATILE: u32 = 0x00000001;
/// UEFI 变量属性: 引导服务访问
pub const EFI_VARIABLE_BOOTSERVICE_ACCESS: u32 = 0x00000002;
/// UEFI 变量属性: 运行时访问
pub const EFI_VARIABLE_RUNTIME_ACCESS: u32 = 0x00000004;

// ============================================================================
// EFI 时间
// ============================================================================

/// EFI 时间结构
#[derive(Debug, Clone, Copy, Default)]
pub struct EfiTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub nanosecond: u32,
    pub timezone: i16, // 0=UTC, -2047=未指定
    pub daylight: u8,  // bit0=ADJUST (调整), bit1=DST (夏令时)
}

impl EfiTime {
    // 有意窄化: 用户内存代理, 指针/长度上下文保证
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn to_unix_ns(&self) -> u64 {
        // 简化: 转换为秒数
        let days_before_month: [u64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let y = u64::from(self.year);
        let m = u64::from(self.month);
        let d = u64::from(self.day);
        let leap_years = (y - 1) / 4 - (y - 1) / 100 + (y - 1) / 400;
        let is_leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
        let extra = u64::from(is_leap && m > 2);
        let days =
            y * 365 + leap_years + days_before_month.get(m as usize - 1).unwrap_or(&0) + d + extra;
        // 1970-01-01 基准
        let epoch_days = 1970 * 365 + (1970 - 1) / 4 - (1970 - 1) / 100 + (1970 - 1) / 400 + 1;
        let unix_days = days.saturating_sub(epoch_days);
        let unix_secs = unix_days * 86400
            + u64::from(self.hour) * 3600
            + u64::from(self.minute) * 60
            + u64::from(self.second);
        unix_secs * 1_000_000_000 + u64::from(self.nanosecond)
    }

    /// 从 Unix 纳秒构造 `EfiTime` (`to_unix_ns` 的逆运算)。
    ///
    /// 供 `set_time` 系统调用 (ns 语义) 与 `get_time` (epoch 基准换算) 使用。
    /// 年份换算以公历推进, 年份达 `u16::MAX` 时截断 (u64 纳秒量程远超 u16 年份)。
    pub fn from_unix_ns(ns: u64) -> Self {
        let secs = ns / 1_000_000_000;
        let nanosecond = (ns % 1_000_000_000) as u32;
        let days = secs / 86400;
        let time_of_day = secs % 86400;

        let mut year = 1970u16;
        let mut remaining = days;
        loop {
            let days_in_year = if is_leap_year(year) { 366 } else { 365 };
            if remaining < days_in_year || year == u16::MAX {
                break;
            }
            remaining -= days_in_year;
            year += 1;
        }

        let days_in_months = if is_leap_year(year) {
            [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        } else {
            [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        };
        let mut month = 1u8;
        for &dim in &days_in_months {
            if remaining < dim {
                break;
            }
            remaining -= dim;
            month = month.saturating_add(1);
        }

        Self {
            year,
            month,
            day: (remaining + 1) as u8,
            hour: (time_of_day / 3600) as u8,
            minute: ((time_of_day % 3600) / 60) as u8,
            second: (time_of_day % 60) as u8,
            nanosecond,
            timezone: 0,
            daylight: 0,
        }
    }
}

// ============================================================================
// EFI 内存类型
// ============================================================================

/// EFI 内存类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EfiMemoryType {
    Reserved = 0,
    LoaderCode = 1,
    LoaderData = 2,
    BootServicesCode = 3,
    BootServicesData = 4,
    RuntimeServicesCode = 5,
    RuntimeServicesData = 6,
    Conventional = 7,
    Unusable = 8,
    AcpiReclaim = 9,
    AcpiNvs = 10,
    MemoryMappedIo = 11,
    MemoryMappedIoPortSpace = 12,
    PalCode = 13,
    Persistent = 14,
}

impl EfiMemoryType {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::LoaderCode,
            2 => Self::LoaderData,
            3 => Self::BootServicesCode,
            4 => Self::BootServicesData,
            5 => Self::RuntimeServicesCode,
            6 => Self::RuntimeServicesData,
            7 => Self::Conventional,
            8 => Self::Unusable,
            9 => Self::AcpiReclaim,
            10 => Self::AcpiNvs,
            11 => Self::MemoryMappedIo,
            12 => Self::MemoryMappedIoPortSpace,
            13 => Self::PalCode,
            14 => Self::Persistent,
            _ => Self::Reserved,
        }
    }
}

// ============================================================================
// EFI 内存描述符
// ============================================================================

/// EFI 内存描述符
#[derive(Debug, Clone, Copy)]
pub struct EfiMemoryDescriptor {
    pub memory_type: EfiMemoryType,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

// ============================================================================
// GOP 模式信息
// ============================================================================

/// GOP 像素格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EfiPixelFormat {
    RedGreenBlueReserved8BitPerColor = 0,
    BlueGreenRedReserved8BitPerColor = 1,
    BitMask = 2,
    BltOnly = 3,
}

/// GOP 模式信息
#[derive(Debug, Clone, Copy)]
pub struct EfiGopModeInfo {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: EfiPixelFormat,
    pub pixels_per_scan_line: u32,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: u64,
}

// ============================================================================
// UEFI 变量
// ============================================================================

/// UEFI 变量 (软件模拟)
#[derive(Debug, Clone)]
pub struct EfiVariable {
    /// 变量名 (UTF-8)
    pub name: Vec<u8>,
    /// GUID (16 字节)
    pub guid: [u8; 16],
    /// 属性
    pub attributes: u32,
    /// 数据
    pub data: Vec<u8>,
}

// ============================================================================
// UEFI 子系统
// ============================================================================

/// UEFI 子系统
pub struct UefiSubsystem {
    /// 系统表物理地址
    system_table_addr: AtomicU64,
    /// EFI_RUNTIME_SERVICES 指针 (从系统表 0x58 提取)
    runtime_services_addr: AtomicU64,
    /// EFI_BOOT_SERVICES 指针 (从系统表 0x60 提取)
    boot_services_addr: AtomicU64,
    /// EFI_CONFIGURATION_TABLE 指针 (从系统表 0x70 提取)
    config_table_addr: AtomicU64,
    /// 配置表条目数 (从系统表 0x68 提取)
    config_table_entries: AtomicU32,
    /// 固件厂商字符串 (CHAR16*) 指针 (从系统表 0x18 提取)
    firmware_vendor_addr: AtomicU64,
    /// 固件修订版本 (从系统表 0x20 提取)
    firmware_revision: AtomicU32,
    /// 墙上时钟 epoch 偏移 (纳秒) — `set_time` 写入后 `get_time` 反映设定值
    epoch_offset_ns: AtomicU64,
    /// GOP 模式信息
    gop_mode: IrqSpinLock<Option<EfiGopModeInfo>>,
    /// UEFI 变量存储 (软件模拟)
    variables: IrqSpinLock<Vec<EfiVariable>>,
    /// 内存映射
    memory_map: IrqSpinLock<Vec<EfiMemoryDescriptor>>,
    /// 是否已初始化
    initialized: AtomicBool,
    /// 是否有 UEFI 固件
    has_uefi: AtomicBool,
}

impl UefiSubsystem {
    pub const fn new() -> Self {
        Self {
            system_table_addr: AtomicU64::new(0),
            runtime_services_addr: AtomicU64::new(0),
            boot_services_addr: AtomicU64::new(0),
            config_table_addr: AtomicU64::new(0),
            config_table_entries: AtomicU32::new(0),
            firmware_vendor_addr: AtomicU64::new(0),
            firmware_revision: AtomicU32::new(0),
            epoch_offset_ns: AtomicU64::new(0),
            gop_mode: IrqSpinLock::new(None),
            variables: IrqSpinLock::new(Vec::new()),
            memory_map: IrqSpinLock::new(Vec::new()),
            initialized: AtomicBool::new(false),
            has_uefi: AtomicBool::new(false),
        }
    }

    /// 初始化
    pub fn init(&self, system_table_addr: u64) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }

        self.system_table_addr
            .store(system_table_addr, Ordering::Release);
        self.has_uefi
            .store(system_table_addr != 0, Ordering::Release);

        if system_table_addr != 0 {
            // SAFETY: system_table_addr 由引导加载器传入
            // 在实际实现中, 这里会解析 EFI_SYSTEM_TABLE
            self.parse_system_table(system_table_addr);
        }

        // 初始化默认变量
        self.init_default_variables();

        self.initialized.store(true, Ordering::Release);
        crate::klog_ffi!(
            klog_ffi_info,
            "[UEFI] initialized: system_table={:#x}, has_firmware={}, runtime={:#x}, boot={:#x}, cfg_entries={}, fw_rev={:#x}",
            system_table_addr,
            system_table_addr != 0,
            self.runtime_services_addr.load(Ordering::Acquire),
            self.boot_services_addr.load(Ordering::Acquire),
            self.config_table_entries.load(Ordering::Acquire),
            self.firmware_revision.load(Ordering::Acquire),
        );
    }

    /// 解析 EFI_SYSTEM_TABLE — 验证签名 + 提取运行时/引导服务与配置表入口。
    ///
    /// 仅做**只读解析与记录**, 不调用任何 UEFI 运行时服务 (调用需固件仍处
    /// BootServices 生命周期且内存 1:1 映射). 签名不匹配视为非 EFI 系统表,
    /// fail-closed: `has_uefi` 置 false, 提取字段保持 0.
    fn parse_system_table(&self, addr: u64) {
        if addr == 0 {
            return;
        }
        // SAFETY: `addr` 由引导加载器经 `uefi_init` 传入, 指向固件保留的
        // EFI_SYSTEM_TABLE 物理内存 (或 0); 仅读取定长字段, 不触碰指针目标.
        let signature =
            unsafe { core::ptr::read_volatile((addr + ST_OFF_SIGNATURE) as *const u64) };
        if signature != EFI_SYSTEM_TABLE_SIGNATURE {
            self.has_uefi.store(false, Ordering::Release);
            return;
        }
        // SAFETY: 同上 — 已通过签名校验, 系统表结构有效, 读取定长字段.
        let runtime =
            unsafe { core::ptr::read_volatile((addr + ST_OFF_RUNTIME_SERVICES) as *const u64) };
        let boot = unsafe { core::ptr::read_volatile((addr + ST_OFF_BOOT_SERVICES) as *const u64) };
        let entries =
            unsafe { core::ptr::read_volatile((addr + ST_OFF_NUM_TABLE_ENTRIES) as *const u64) };
        let config =
            unsafe { core::ptr::read_volatile((addr + ST_OFF_CONFIG_TABLE) as *const u64) };
        let vendor =
            unsafe { core::ptr::read_volatile((addr + ST_OFF_FIRMWARE_VENDOR) as *const u64) };
        let fw_rev =
            unsafe { core::ptr::read_volatile((addr + ST_OFF_FIRMWARE_REVISION) as *const u32) };

        self.runtime_services_addr.store(runtime, Ordering::Release);
        self.boot_services_addr.store(boot, Ordering::Release);
        self.config_table_addr.store(config, Ordering::Release);
        self.config_table_entries
            .store(entries.min(u64::from(u32::MAX)) as u32, Ordering::Release);
        self.firmware_vendor_addr.store(vendor, Ordering::Release);
        self.firmware_revision.store(fw_rev, Ordering::Release);
    }

    /// 初始化默认变量
    fn init_default_variables(&self) {
        let mut vars = self.variables.lock();

        // BootOrder
        vars.push(EfiVariable {
            name: b"BootOrder".to_vec(),
            guid: [0x84; 16], // 全局变量 GUID (简化)
            attributes: EFI_VARIABLE_NON_VOLATILE
                | EFI_VARIABLE_BOOTSERVICE_ACCESS
                | EFI_VARIABLE_RUNTIME_ACCESS,
            data: vec![0, 0], // Boot0000
        });

        // ConOut (控制台输出)
        vars.push(EfiVariable {
            name: b"ConOut".to_vec(),
            guid: [0x84; 16],
            attributes: EFI_VARIABLE_NON_VOLATILE
                | EFI_VARIABLE_BOOTSERVICE_ACCESS
                | EFI_VARIABLE_RUNTIME_ACCESS,
            data: vec![],
        });

        // SecureBoot
        vars.push(EfiVariable {
            name: b"SecureBoot".to_vec(),
            guid: [0x77; 16], // EFI_GLOBAL_VARIABLE
            attributes: EFI_VARIABLE_BOOTSERVICE_ACCESS | EFI_VARIABLE_RUNTIME_ACCESS,
            data: vec![0], // 0 = disabled
        });
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取 UEFI 变量
    pub fn get_variable(&self, name: &[u8], guid: &[u8; 16]) -> Option<(u32, Vec<u8>)> {
        let vars = self.variables.lock();
        for v in vars.iter() {
            if v.name == name && v.guid == *guid {
                return Some((v.attributes, v.data.clone()));
            }
        }
        None
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 设置 UEFI 变量
    pub fn set_variable(&self, name: &[u8], guid: &[u8; 16], attrs: u32, data: &[u8]) -> bool {
        if name.len() > EFI_MAX_VAR_NAME || data.len() > EFI_MAX_VAR_DATA {
            return false;
        }

        let mut vars = self.variables.lock();
        // 查找已有变量
        for v in vars.iter_mut() {
            if v.name == name && v.guid == *guid {
                v.attributes = attrs;
                v.data = data.to_vec();
                return true;
            }
        }
        // 新变量
        vars.push(EfiVariable {
            name: name.to_vec(),
            guid: *guid,
            attributes: attrs,
            data: data.to_vec(),
        });
        true
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 删除 UEFI 变量
    pub fn delete_variable(&self, name: &[u8], guid: &[u8; 16]) -> bool {
        let mut vars = self.variables.lock();
        let before = vars.len();
        vars.retain(|v| !(v.name == name && v.guid == *guid));
        vars.len() != before
    }

    /// 列出所有变量
    pub fn list_variables(&self) -> Vec<(Vec<u8>, [u8; 16])> {
        let vars = self.variables.lock();
        vars.iter().map(|v| (v.name.clone(), v.guid)).collect()
    }

    /// 获取时间
    pub fn get_time(&self) -> EfiTime {
        // 墙上时钟 = epoch 基准 (set_time 写入) + 自开机起的单调时间
        let ns = crate::framework::timer::ticks_to_ns(crate::framework::timer::get_ticks());
        let wall_ns = self
            .epoch_offset_ns
            .load(Ordering::Acquire)
            .saturating_add(ns);
        EfiTime::from_unix_ns(wall_ns)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 设置时间 — 写入 epoch 基准使 `get_time` 返回设定时刻
    ///
    /// 软件模拟语义: 将 `time` 转 Unix 纳秒后, 减去当前单调时间得到
    /// `epoch_offset_ns`; 之后 `get_time` = 设定值 + 已流逝的单调时间.
    /// 返回 `true` (无固件写入失败路径).
    pub fn set_time(&self, time: &EfiTime) -> bool {
        let now_ns = crate::framework::timer::ticks_to_ns(crate::framework::timer::get_ticks());
        let target_ns = time.to_unix_ns();
        self.epoch_offset_ns
            .store(target_ns.saturating_sub(now_ns), Ordering::Release);
        true
    }

    /// 设置 GOP 模式信息
    pub fn set_gop_mode(&self, mode: EfiGopModeInfo) {
        *self.gop_mode.lock() = Some(mode);
    }

    /// 获取 GOP 模式信息
    pub fn get_gop_mode(&self) -> Option<EfiGopModeInfo> {
        *self.gop_mode.lock()
    }

    /// 设置内存映射
    pub fn set_memory_map(&self, map: Vec<EfiMemoryDescriptor>) {
        *self.memory_map.lock() = map;
    }

    /// 获取内存映射
    pub fn get_memory_map(&self) -> Vec<EfiMemoryDescriptor> {
        self.memory_map.lock().clone()
    }

    /// 是否有 UEFI 固件
    pub fn has_uefi(&self) -> bool {
        self.has_uefi.load(Ordering::Acquire)
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// 获取变量数量
    pub fn variable_count(&self) -> usize {
        self.variables.lock().len()
    }
}

fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 UEFI 子系统
static UEFI: UefiSubsystem = UefiSubsystem::new();

/// 初始化 UEFI
pub fn uefi_init(system_table_addr: u64) {
    UEFI.init(system_table_addr);
}

/// 获取全局 UEFI 子系统
pub fn uefi_subsystem() -> &'static UefiSubsystem {
    &UEFI
}

/// UEFI 是否已初始化
pub fn uefi_is_initialized() -> bool {
    UEFI.is_initialized()
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_uefi` — UEFI 系统调用
///
/// `a0`: cmd
///   0 = `get_variable(名称指针`, GUID 指针) → (属性, 数据指针)
///   1 = `set_variable(名称指针`, GUID 指针, 属性, 数据指针, 数据长度)
///   2 = `delete_variable(名称指针`, GUID 指针)
///   3 = `get_time()` → ns (纳秒)
///   4 = `set_time(ns`: a1)
///   5 = `get_gop_mode()` → `fb_base`  // 帧缓冲基址
///   6 = `list_variables()` → count
///   7 = `has_uefi()` → bool
///   8 = `is_initialized()` → 是否已初始化
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sys_uefi(cmd: u64, a1: u64, a2: u64) -> i64 {
    if !uefi_is_initialized() && cmd != 8 {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // get_variable (简化: 返回是否存在)
            // 实际实现需要 copy_from_user 读取 name/guid
            let _ = (a1, a2);
            0
        }
        1 => {
            // set_variable (简化)
            let _ = (a1, a2);
            0
        }
        2 => {
            // delete_variable (简化)
            let _ = (a1, a2);
            0
        }
        3 => {
            // get_time → ns
            let time = uefi_subsystem().get_time();
            time.to_unix_ns() as i64
        }
        4 => {
            // set_time(ns: a1) — 写入 epoch 基准, 使后续 get_time 反映设定时刻
            i64::from(uefi_subsystem().set_time(&EfiTime::from_unix_ns(a1)))
        }
        5 => {
            // get_gop_mode → fb_base
            uefi_subsystem()
                .get_gop_mode()
                .map_or(0, |mode| mode.frame_buffer_base as i64)
        }
        6 => {
            // list_variables → count
            uefi_subsystem().list_variables().len() as i64
        }
        7 => {
            // has_uefi
            i64::from(uefi_subsystem().has_uefi())
        }
        8 => {
            // is_initialized
            i64::from(uefi_is_initialized())
        }
        _ => -(38i64), // ENOSYS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成有效的 EFI_SYSTEM_TABLE (x86_64 UEFI 2.x 布局)。
    fn valid_table() -> [u8; 0x80] {
        let mut t = [0u8; 0x80];
        t[..8].copy_from_slice(&EFI_SYSTEM_TABLE_SIGNATURE.to_le_bytes());
        t[0x18..0x20].copy_from_slice(&0x1000_3000u64.to_le_bytes()); // FirmwareVendor
        t[0x20..0x24].copy_from_slice(&0x0001_0002u32.to_le_bytes()); // FirmwareRevision
        t[0x58..0x60].copy_from_slice(&0x1000_0000u64.to_le_bytes()); // RuntimeServices
        t[0x60..0x68].copy_from_slice(&0x1000_1000u64.to_le_bytes()); // BootServices
        t[0x68..0x70].copy_from_slice(&2u64.to_le_bytes()); // NumberOfTableEntries
        t[0x70..0x78].copy_from_slice(&0x1000_2000u64.to_le_bytes()); // ConfigurationTable
        t
    }

    /// 有效签名: 解析并提取全部字段, `has_uefi` 置 true。
    #[test]
    fn parse_valid_table_sets_firmware_and_fields() {
        let table = valid_table();
        let sub = UefiSubsystem::new();
        sub.init(table.as_ptr() as u64);
        assert!(sub.has_uefi(), "有效签名应置 has_uefi");
        assert_eq!(
            sub.runtime_services_addr.load(Ordering::Acquire),
            0x1000_0000
        );
        assert_eq!(sub.boot_services_addr.load(Ordering::Acquire), 0x1000_1000);
        assert_eq!(sub.config_table_addr.load(Ordering::Acquire), 0x1000_2000);
        assert_eq!(sub.config_table_entries.load(Ordering::Acquire), 2);
        assert_eq!(
            sub.firmware_vendor_addr.load(Ordering::Acquire),
            0x1000_3000
        );
        assert_eq!(sub.firmware_revision.load(Ordering::Acquire), 0x0001_0002);
    }

    /// 签名不匹配: fail-closed, `has_uefi` 置 false 且提取字段保持 0。
    #[test]
    fn parse_invalid_signature_fail_closed() {
        let mut table = valid_table();
        table[0] ^= 0xFF; // 破坏签名
        let sub = UefiSubsystem::new();
        sub.init(table.as_ptr() as u64);
        assert!(!sub.has_uefi(), "签名不匹配应 fail-closed");
        assert_eq!(sub.runtime_services_addr.load(Ordering::Acquire), 0);
    }

    /// `from_unix_ns`/`to_unix_ns` 往返恒等 (日期换算双向一致)。
    #[test]
    fn efi_time_round_trip() {
        for ns in [
            0u64,
            1_577_836_800_000_000_000, // 2020-01-01T00:00:00Z
            1_609_459_200_000_000_000, // 2021-01-01T00:00:00Z (平年)
            1_757_606_400_000_000_000, // 2025-09-08T00:00:00Z
            4_102_444_800_000_000_000, // 2100-01-01T00:00:00Z (非闰世纪年)
        ] {
            let t = EfiTime::from_unix_ns(ns);
            assert_eq!(t.to_unix_ns(), ns, "往返恒等失败: {ns}");
        }
    }

    /// `set_time` 写入 epoch 基准, `get_time` 反映设定时刻 (host-test 下
    /// 单调 tick 恒 0, 偏差仅来自测试运行间隔)。
    #[test]
    fn set_time_epoch_makes_get_time_reflect() {
        let sub = UefiSubsystem::new();
        let target = EfiTime {
            year: 2020,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            nanosecond: 0,
            timezone: 0,
            daylight: 0,
        };
        let target_ns = target.to_unix_ns();
        assert!(sub.set_time(&target));
        let got_ns = sub.get_time().to_unix_ns();
        assert!(
            got_ns >= target_ns,
            "get_time 应不低于设定时刻: got={got_ns} target={target_ns}"
        );
        assert!(got_ns - target_ns < 2_000_000_000, "偏差应 < 2s");
    }
}
