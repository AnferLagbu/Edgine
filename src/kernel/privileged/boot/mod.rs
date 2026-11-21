//! 启动信息模块
//!
//! 解析 Multiboot1 与 Multiboot2 启动信息, 获取内存映射
//! 与其他启动参数.
//!
//! P2.D + F-15 (B02-22): 实际引导路径是 Multiboot2 (boot.asm `.multiboot2`
//! 段 + stage1.asm 16 位 MBR + QEMU `-kernel` 参数).
//! Multiboot1 解析代码仅作为兼容层 (boot.asm `.multiboot1` section 让
//! 老式 GRUB 仍可引导). boot/mod.rs 头注释原"Multiboot1 与 Multiboot2"
//! 描述未与代码现状对齐. 现状代码保留, 仅调整注释表述.
//!
//! # Safety
//! `BOOT_INFO` 的内部可变性通过 `spin::Once` 实现 (启动期写入一次,
//! 之后只读). `MULTIBOOT_INFO_PTR` 使用 `spin::Mutex`, 因为它在 init
//! 之前设置, 在 init 期间读取.

#![deny(missing_docs)]

use crate::privileged::sync::IrqSpinLock;

use crate::privileged::sync::OnceLock;
#[cfg(target_arch = "aarch64")]
pub mod aarch64;
pub mod multiboot2_fb;

/// Multiboot1 引导器魔数 (EAX 寄存器在跳转时携带该值)
pub const MULTIBOOT1_MAGIC: u32 = 0x2BAD_B002;
/// Multiboot2 引导器魔数 (EAX 寄存器在跳转时携带该值)
pub const MULTIBOOT2_MAGIC: u32 = 0x36D7_6289;

/// Multiboot1 信息结构标志位: 基本内存信息 (`mem_lower` / `mem_upper`) 有效
pub const MBOOT1_FLAG_MEM: u32 = 1 << 0;
/// Multiboot1 信息结构标志位: 内存映射 (`mmap_length` / `mmap_addr`) 有效
pub const MBOOT1_FLAG_MMAP: u32 = 1 << 6;
/// Multiboot1 信息结构标志位: 引导模块 (`mods_count` / `mods_addr`) 有效
pub const MBOOT1_FLAG_MODS: u32 = 1 << 3;

/// Multiboot1 引导信息结构 (Multiboot1 spec 定义的机器状态块)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Multiboot1Info {
    /// 有效标志位 (决定后续哪些字段可信)
    pub flags: u32,
    /// 低端内存大小 (KB, 自 0 起)
    pub mem_lower: u32,
    /// 高端内存大小 (KB, 自 1MB 起)
    pub mem_upper: u32,
    /// 引导设备编号 (BIOS 驱动器号)
    pub boot_device: u32,
    /// 内核命令行字符串的物理地址
    pub cmdline: u32,
    /// 引导模块数量
    pub mods_count: u32,
    /// 引导模块数组的物理地址
    pub mods_addr: u32,
    /// a.out 符号表 (`tab_size` / `str_size` / addr / reserved)
    pub syms: [u32; 4],
    /// 内存映射缓冲区字节长度
    pub mmap_length: u32,
    /// 内存映射缓冲区的物理地址
    pub mmap_addr: u32,
    /// 第一个驱动器分区表的字节长度
    pub drives_length: u32,
    /// 第一个驱动器分区表的物理地址
    pub drives_addr: u32,
    /// ROM 配置表的物理地址
    pub config_table: u32,
    /// 引导器名称字符串的物理地址
    pub boot_loader_name: u32,
    /// APM 表的物理地址
    pub apm_table: u32,
    /// VBE 控制信息的物理地址
    pub vbe_control_info: u32,
    /// VBE 模式信息的物理地址
    pub vbe_mode_info: u32,
    /// 当前 VBE 模式号
    pub vbe_mode: u16,
    /// VBE 接口代码段
    pub vbe_interface_seg: u16,
    /// VBE 接口代码偏移
    pub vbe_interface_off: u16,
    /// VBE 接口代码长度
    pub vbe_interface_len: u16,
    /// VBE 控制信息的高 32 位物理地址
    pub vbe_control_info_high: u64,
    /// VBE 模式信息的高 32 位物理地址
    pub vbe_mode_info_high: u64,
}

/// Multiboot1 内存映射条目 (描述一段物理内存的基址 / 长度 / 类型)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryMapEntry {
    /// 本条目字节数 (不含该 size 字段自身的 4 字节)
    pub size: u32,
    /// 基址低 32 位
    pub base_addr_low: u32,
    /// 基址高 32 位
    pub base_addr_high: u32,
    /// 长度低 32 位
    pub length_low: u32,
    /// 长度高 32 位
    pub length_high: u32,
    /// 区域类型 (1 = 可用 RAM)
    pub mtype: u32,
}

impl MemoryMapEntry {
    /// 组合高低 32 位得到 64 位基址
    #[must_use]
    pub fn base_addr(&self) -> u64 {
        u64::from(self.base_addr_high) << 32 | u64::from(self.base_addr_low)
    }

    /// 组合高低 32 位得到 64 位区域长度
    #[must_use]
    pub fn length(&self) -> u64 {
        u64::from(self.length_high) << 32 | u64::from(self.length_low)
    }

    /// 判定该区域是否为可用 RAM (`mtype == 1`)
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.mtype == 1
    }
}

/// Multiboot1 引导模块条目 (16 字节: 起止物理地址 + 命令行串地址 + 保留).
///
/// 引导器把 initrd 等文件作为"模块"装入内存; 本结构是模块数组的单元素,
/// 数组首址见 [`Multiboot1Info::mods_addr`].
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct Mb1Module {
    /// 模块起始物理地址
    mod_start: u32,
    /// 模块结束物理地址 (开区间)
    mod_end: u32,
    /// 模块命令行字符串的物理地址
    string: u32,
    /// 保留字段 (恒 0)
    reserved: u32,
}

/// 引导阶段汇总的启动信息 (可用内存大小 / 内核结束地址 / 内存映射条目数)
#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    /// 可用物理内存大小 (字节)
    pub mem_size: u64,
    /// 内核映像结束的物理地址
    pub kernel_end: u64,
    /// 引导器报告的内存映射条目数
    pub mmap_entries: usize,
    /// 引导器交付的 initrd (引导模块) 起始物理地址; 0 表示无
    pub initrd_base: u64,
    /// initrd 字节长度; 0 表示无
    pub initrd_size: u64,
}

impl Default for BootInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl BootInfo {
    /// 构造全零初值 (尚未解析引导信息时的占位)
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mem_size: 0,
            kernel_end: 0,
            mmap_entries: 0,
            initrd_base: 0,
            initrd_size: 0,
        }
    }
}

/// 一条 firmware 保留区 (引导器内存图里 `type != 1` 的条目).
///
/// MB2 spec §3.6.6 的类型语义: 1 = 可用 RAM, 3 = 承载 ACPI 信息的可回收内存,
/// 4 = 休眠须保留, 5 = 坏 RAM, 其余值一律 reserved. 除 1 之外都**不得**当空闲页
/// 分配出去 —— ACPI 表 (RSDT/XSDT/MADT/FADT) 与 EFI runtime 就住在这些区里.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareRegion {
    /// 起始物理地址.
    pub base: u64,
    /// 字节长度.
    pub size: u64,
}

/// firmware 保留区定长表 (引导期无分配器, 故定长; 容量单源见 `limits`).
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareRegions {
    regions: [FirmwareRegion; crate::privileged::constants::limits::MAX_FIRMWARE_REGIONS],
    count: usize,
}

#[cfg(target_arch = "x86_64")]
impl FirmwareRegions {
    /// 空表 (供 `const` 初始化与解析起点使用).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            regions: [FirmwareRegion { base: 0, size: 0 };
                crate::privileged::constants::limits::MAX_FIRMWARE_REGIONS],
            count: 0,
        }
    }

    /// 登记一条保留区; 零长度区是空集, 直接忽略.
    ///
    /// # Panics
    /// 表满时停机 (fail-closed): 丢一条等于把该 firmware 区当空闲页分配出去.
    pub fn push(&mut self, base: u64, size: u64) {
        if size == 0 {
            return;
        }
        assert!(
            self.count < self.regions.len(),
            "[BOOT] firmware region table full ({}), cannot reserve 0x{:X}+{}",
            self.regions.len(),
            base,
            size
        );
        self.regions[self.count] = FirmwareRegion { base, size };
        self.count += 1;
    }

    /// 已登记条目的只读视图.
    #[must_use]
    pub fn as_slice(&self) -> &[FirmwareRegion] {
        &self.regions[..self.count]
    }
}

#[cfg(target_arch = "x86_64")]
impl Default for FirmwareRegions {
    fn default() -> Self {
        Self::new()
    }
}

struct MultibootPtr(*const u8);
// SAFETY: MultibootPtr 包装一个指向启动信息数据的裸指针, 启动早期写入
// 一次, 之后只读. 访问受 MULTIBOOT_INFO_PTR Mutex 保护.
unsafe impl Send for MultibootPtr {}
// SAFETY: 跨核共享 `&MultibootPtr` 只暴露 `.0` 这个 Copy 裸指针**值**, 不暴露对指向
// 数据的引用, 故 Sync 的风险面全在"指向的数据是否会被并发改写":
// ① 唯一写入点 `boot_set_multiboot_info` 由汇编引导入口在跳入内核前调用一次 (单核,
//    AP 未上电), 此后所有访问都经 `MULTIBOOT_INFO_PTR.lock()` 只读取值;
//    `IrqSpinLock` 持锁期间本核关中断 ⇒ 与中断处理器再入同一锁互斥.
// ② 指向的 MB2 信息块由引导器装入并自此只读, 且经 `multiboot2_info_region()` 进 PMM
//    早期预留 (`lib.rs` 的 `pmm_early_reserve(base, size, "mb2-info")`) ⇒ buddy 不会
//    把该区当空闲页分配出去, 运行期无别名写者 (ACPI 发现面在 PMM 建立之后仍要读它).
// 类型自身不携带有效性/生命周期证明: 未登记时 `.0` 为 null, 消费者须自判 (如
// `multiboot2_info_region` 的 `is_null` 分支). 附注: `IrqSpinLock<T>: Sync` 的界只要求
// `T: Send` (`sync/irq_spinlock.rs`), 故本 Sync 断言不是该 static 的类型必需项, 而是
// 对同一事实的显式声明.
unsafe impl Sync for MultibootPtr {}

static BOOT_INFO: OnceLock<BootInfo> = OnceLock::new();
/// 搬移后的 initrd 载体区 (base, size).
///
/// `BOOT_INFO` 记录的是**引导器交付的事实**, 一经解析不再改写; 而 D4-h 的早期
/// 搬移会把归档挪到与固定堆窗互斥的新址. 两个事实分开存, 消费者经 [`initrd_blob`]
/// 统一取"当前有效"的那一份.
static INITRD_CARRIER: OnceLock<(u64, u64)> = OnceLock::new();
static MULTIBOOT_INFO_PTR: IrqSpinLock<MultibootPtr> =
    IrqSpinLock::new(MultibootPtr(core::ptr::null()));
static MULTIBOOT_MAGIC: IrqSpinLock<u32> = IrqSpinLock::new(0);

/// 引导器内存图里的 firmware 保留区 (x86_64; aarch64 走 DTB, 不经 Multiboot).
#[cfg(target_arch = "x86_64")]
static FIRMWARE_REGIONS: OnceLock<FirmwareRegions> = OnceLock::new();

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
// 符号桩化 (host-test): host 无 _kernel_end 链接脚本符号, `init()` 走常量桩取值,
// 声明一并排除 (符号契约归零); 声明与唯一引用点 (`init` 真机分支) 门控严格同构.
#[cfg(not(feature = "host-test"))]
unsafe extern "C" {
    static _kernel_end: u8;
}

/// 获取全局启动信息结构体。
/// # Panics
/// 启动信息尚未初始化时 panic。
pub fn get_boot_info() -> &'static BootInfo {
    BOOT_INFO
        .get()
        .expect("[BOOT] accessed before initialization")
}

/// 登记搬移后的 initrd 载体区 (写一次; 重复调用保留首值).
///
/// 由 `boot_memory_init` 在 PMM 位图建立后调用 —— 此时早期预留表里的 base 已是
/// 与内核映像 / 固定堆窗 / 元数据六区互斥的最终地址.
pub fn set_initrd_carrier(base: u64, size: u64) {
    let _ = INITRD_CARRIER.set((base, size));
}

/// 取回引导器交付的 initrd (引导模块) 字节切片; 无模块时返回 `None`.
///
/// 该区由引导器装入, 经早期预留表登记 (与固定堆窗相交时已搬移到互斥区), 生命周期
/// 覆盖内核运行; 经高半区直射区映射为只读切片, 供 functions 层解包 (functions 零
/// unsafe, 见 F1).
#[must_use]
pub fn initrd_blob() -> Option<&'static [u8]> {
    let (base, size) = INITRD_CARRIER
        .get()
        .copied()
        .unwrap_or_else(|| (get_boot_info().initrd_base, get_boot_info().initrd_size));
    if size == 0 {
        return None;
    }
    let len = crate::privileged::glue::usize_from_u64(size);
    // SAFETY: initrd 区由引导器装入并经 PMM 早期预留表置位 (搬移后的新址同样置位),
    // 生命周期覆盖内核运行; `phys_to_virt` 经高半区直射区给出唯一内核映射 (x86 直射区
    // 不受低半区恒等窗口约束, aarch64 直射区覆盖 DRAM), 只读借用且无别名写.
    let blob = unsafe {
        let ptr = crate::privileged::mm::phys_to_virt(base) as *const u8;
        core::slice::from_raw_parts(ptr, len)
    };
    Some(blob)
}

/// 取回引导器传入的启动信息块物理地址; 未经引导入口登记时为 0.
///
/// ACPI 发现面 (`arch::x86_64::acpi::find_rsdp`) 由此拿 Multiboot2 信息块 ——
/// tag 14/15 内联携带 RSDP 副本, 是 UEFI 下唯一可用的 RSDP 来源 (OVMF 无 EBDA,
/// BIOS ROM 窗内也没有 RSDP). 硬编码 0 会让该路径整条失效而静默退化成单核.
#[must_use]
pub fn multiboot_info_ptr() -> u64 {
    MULTIBOOT_INFO_PTR.lock().0 as u64
}

/// 取回 Multiboot2 信息块**自身**占用的物理区 `(base, size)`; 非 MB2 引导时 `None`.
///
/// 信息块由引导器装在它自选的地址上, 而 ACPI 发现面在 PMM 建立**之后**还要读它
/// (tag 14/15 内联的 RSDP 副本), 故该区必须与 firmware 保留区一并进早期预留表,
/// 否则 buddy 会把它当空闲页分配出去. MB1 信息块无自描述长度, 不走此面.
#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn multiboot2_info_region() -> Option<(u64, u64)> {
    if *MULTIBOOT_MAGIC.lock() != MULTIBOOT2_MAGIC {
        return None;
    }
    let ptr = MULTIBOOT_INFO_PTR.lock().0;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: 该指针由引导入口 `boot_set_multiboot_info` 登记, 指向引导器装入的
    // MB2 信息块; 首 4 字节按协议是 total_size. 启动期低半区恒等映射覆盖 0-4 GiB,
    // 而 MB2 spec 保证信息块位于 4 GiB 以下, 故物理地址可直读; 只读且无别名写.
    let total_size = u64::from(unsafe { ptr.cast::<u32>().read_unaligned() });
    Some((ptr as u64, total_size))
}

/// 取回引导器内存图里的 firmware 保留区 (MB2 tag 6 / MB1 mmap 中 `type != 1` 项).
///
/// PMM 把 `[0, mem_size)` 建模成平坦 RAM, 若不把这些区登记进早期预留表, ACPI 表
/// (RSDT/XSDT/MADT/FADT) 与 EFI runtime 所在的页会被 buddy 当空闲页分配出去并覆写.
/// 未经引导解析 (aarch64 / 无 Multiboot) 时返回空切片.
#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn firmware_regions() -> &'static [FirmwareRegion] {
    FIRMWARE_REGIONS
        .get()
        .map_or(&[], FirmwareRegions::as_slice)
}

/// 保存引导器传入的魔数与启动信息指针 (由汇编引导入口在跳转前调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_set_multiboot_info(magic: u32, ptr: *const u8) {
    *MULTIBOOT_MAGIC.lock() = magic;
    *MULTIBOOT_INFO_PTR.lock() = MultibootPtr(ptr);
}

#[cfg(target_arch = "x86_64")]
fn parse_multiboot1(ptr: *const u8) -> (u64, usize, (u64, u64), FirmwareRegions) {
    // SAFETY: `ptr` 是 `boot_set_multiboot_info` 保存的引导器 multiboot info 指针
    // (GRUB 置于 4 GiB 以下), 启动期低端物理内存经恒等映射可直读; 此处用
    // `read_unaligned` 按值拷出头部结构, 不构造引用.
    let mbi = unsafe { ptr.cast::<Multiboot1Info>().read_unaligned() };
    let mut mem_size: u64 = 128 * 1024 * 1024;
    let mut mmap_entries: usize = 0;
    let mut firmware = FirmwareRegions::new();

    // 引导模块 (initrd): flags bit3 且 mods_count>0 时取首模块的物理区间.
    let initrd = if mbi.flags & MBOOT1_FLAG_MODS != 0 && mbi.mods_count > 0 && mbi.mods_addr != 0 {
        // SAFETY: `mods_addr` 由引导器保证指向 `mods_count` 个 Mb1Module 的物理数组,
        // 启动期低端物理内存经 boot 恒等映射可直读; 只读借用首元素.
        let first = unsafe { (mbi.mods_addr as *const Mb1Module).read_unaligned() };
        match first.mod_end.checked_sub(first.mod_start) {
            Some(sz) if sz > 0 && first.mod_start != 0 => {
                (u64::from(first.mod_start), u64::from(sz))
            }
            _ => (0, 0),
        }
    } else {
        (0, 0)
    };

    if mbi.flags & MBOOT1_FLAG_MEM != 0 {
        mem_size = (u64::from(mbi.mem_upper) + 1024) * 1024;
    }

    if mbi.flags & MBOOT1_FLAG_MMAP != 0 {
        let mmap_start = mbi.mmap_addr as *const MemoryMapEntry;
        let mmap_end = (mbi.mmap_addr + mbi.mmap_length) as *const MemoryMapEntry;
        let mut max_addr: u64 = 0;

        let mut current = mmap_start;
        while current < mmap_end {
            // SAFETY: `current` 由循环条件约束在 `[mmap_start, mmap_end)` 内, 指向
            // 引导器写入的 MB1 内存图条目起点 (`MemoryMapEntry` 为 `repr(C)` 4 字节
            // 对齐); 启动期只读该条目, 不写.
            let entry = unsafe { &*current };
            let end = entry.base_addr() + entry.length();
            if end > max_addr && entry.is_available() {
                max_addr = end;
            }
            if !entry.is_available() {
                firmware.push(entry.base_addr(), entry.length());
            }
            mmap_entries += 1;
            // SAFETY: 纯指针算术前进到下一 MB1 条目: `entry.size` 是「不含 size 字段
            // 自身 4 字节」的条目长度, 故加 `size + 4`; 输入 `current` 在 mmap 缓冲内,
            // 引导器保证条目毗邻排列; 本行只做地址计算, 不解引用.
            current = unsafe {
                current.cast::<u8>().add(entry.size as usize + 4) as usize as *const MemoryMapEntry
            };
        }

        if max_addr > 0 {
            mem_size = max_addr;
        }
    }

    (mem_size, mmap_entries, initrd, firmware)
}

#[cfg(target_arch = "x86_64")]
fn parse_multiboot2(ptr: *const u8) -> (u64, usize, (u64, u64), FirmwareRegions) {
    // SAFETY: `ptr` 指向 MB2 信息块起始 (引导器置于 4 GiB 以下), 启动期恒等映射
    // 可直读; 此处只读首个 u32 (total_size), 用 `read_unaligned` 不假设对齐.
    let total_size = unsafe { ptr.cast::<u32>().read_unaligned() };
    // D4-g: 内存图 (tag 6) 权威, 基本内存信息 (tag 4) 仅在无图时兜底.
    // 两者必须**与标签出现次序无关**: GRUB 在 SeaBIOS 与 OVMF 下都把 tag 4 排在
    // tag 6 之后, 原先 tag 4 无条件赋值会把已算出的图结果覆盖掉 —— UEFI 上
    // mem_upper 只描述 1 MiB 以上首个连续块 (实测 7168 KB), 于是 512 MiB 的机器
    // 被读成 8 MB, PMM 零空闲页, 启动期分配即 panic. 与 Linux 以 e820/EFI 图为准
    // 同构: 有图用图, 无图才退回 firmware 的粗粒度读数.
    let mut map_mem_size: Option<u64> = None;
    let mut basic_mem_size: Option<u64> = None;
    let mut mmap_entries: usize = 0;
    let mut initrd: Option<(u64, u64)> = None;
    let mut firmware = FirmwareRegions::new();

    let mut offset: usize = 8;
    let end = total_size as usize;

    while offset + 8 <= end {
        // SAFETY: `offset` 由循环条件 `offset + 8 <= end` 约束, `end = total_size`
        // 为 MB2 信息块长度; `ptr` 指向该块起始, 故 `ptr.add(offset)` 落在块内.
        let tag_ptr = unsafe { ptr.add(offset) };
        // SAFETY: `tag_ptr` 在信息块内且循环条件保证了 ≥8 字节余量; 以 `read_unaligned`
        // 读标签头 type 字段, 不构造引用、不假设对齐.
        let tag_type = unsafe { tag_ptr.cast::<u32>().read_unaligned() };
        // SAFETY: 同上一行: 标签头第二字 (size) 位于 +4, 仍在上方保证的 ≥8 字节余量内.
        let tag_size = unsafe { tag_ptr.add(4).cast::<u32>().read_unaligned() };

        if tag_type == 0 || tag_size == 0 {
            break;
        }

        // 完整 tag 边界校验: MB2 标签头固定 8 字节, 故 tag_size < 8 无意义;
        // tag_size 还必须让整个标签落在信息块 [0, end) 内. 二者任一不成立即
        // 停止解析 (fail-closed), 不按 tag_size 去推算越界载荷.
        if tag_size < 8 {
            break;
        }
        let Some(tag_end) = offset.checked_add(tag_size as usize) else {
            break;
        };
        if tag_end > end {
            break;
        }

        match tag_type {
            3 => {
                // tag 3 = 引导模块 (每模块一条); 取首模块 (initrd) 物理区间.
                // 载荷 mod_start@+8 / mod_end@+12 需要 tag_size >= 16.
                if tag_size >= 16 {
                    // SAFETY: `tag_end <= end` 与 `tag_size >= 16` 已校验, 故 +8/+12
                    // (32 位) 落在本标签且在本信息块内; 标签表在启动期只读.
                    let mod_start = unsafe { tag_ptr.add(8).cast::<u32>().read_unaligned() };
                    // SAFETY: MB2 tag 3 (引导模块) 的载荷布局 = mod_start@+8 /
                    // mod_end@+12 (均 32 位物理地址, mod_end 是**开区间上界**), +16
                    // 起为 NUL 结尾的命令行字符串. 本行只读 +12 处的 u32, 经裸指针
                    // `read_unaligned` 取值而不构造引用 ⇒ 无别名面, 也不对载荷作
                    // 对齐假设.
                    // 越界防护: 循环头 + 上方 tag_size>=8 / tag_end<=end 与本节
                    // tag_size>=16 联合证明 +12..+16 在本标签且在本信息块内.
                    // 取值语义: `mod_end.saturating_sub(mod_start)` 使倒置/空区间得 0
                    // 而被跳过, `mod_start != 0` 排除空指针, `initrd.is_none()` 只取
                    // 首模块 (initrd 约定为第一个 boot module). 结果作为物理区间交给
                    // BootInfo; D4-h 早期搬移后消费者须经 `initrd_blob()` 取当前有效区.
                    let mod_end = unsafe { tag_ptr.add(12).cast::<u32>().read_unaligned() };
                    let sz = mod_end.saturating_sub(mod_start);
                    if initrd.is_none() && sz > 0 && mod_start != 0 {
                        initrd = Some((u64::from(mod_start), u64::from(sz)));
                    }
                }
            }
            4 => {
                // 基本内存信息载荷 = mem_lower@+8 / mem_upper@+12, 需要 tag_size >= 16.
                if tag_size >= 16 {
                    // SAFETY: tag_end <= end 与 tag_size >= 16 联合保证 +8..+16 在本标签内.
                    let basic_ptr = unsafe { tag_ptr.add(8) };
                    // SAFETY: `basic_ptr` 指向标签内有效 u32; 只读借用
                    let _mem_lower = unsafe { basic_ptr.cast::<u32>().read_unaligned() };
                    // SAFETY: 同上, 偏移 4 仍在本标签内
                    let mem_upper = unsafe { basic_ptr.add(4).cast::<u32>().read_unaligned() };
                    basic_mem_size = Some((u64::from(mem_upper) + 1024) * 1024);
                }
            }
            6 => {
                // 内存图载荷 = entry_size@+8 / entry_version@+12; 条目自 +16 起.
                // 需要 tag_size >= 16 且 entry_size >= 24 (base 8 + len 8 + type 4 +
                // reserved 4); entry_size < 24 无法容纳一条完整条目, 且 entry_size == 0
                // 会让条目循环永不前进 (挂死), 故整体跳过该标签.
                let tag_len = tag_size as usize;
                if tag_len >= 16 {
                    // SAFETY: tag_size >= 16 且 tag_end <= end, 故 +8/+12 在本标签内.
                    let entry_size = unsafe { tag_ptr.add(8).cast::<u32>().read_unaligned() };
                    // SAFETY: 同上, +12 处为 entry_version.
                    let _entry_version = unsafe { tag_ptr.add(12).cast::<u32>().read_unaligned() };
                    let entry_size = entry_size as usize;
                    if entry_size >= 24 {
                        let mut max_addr: u64 = 0;
                        // 以标签内偏移推进, 不用裸指针比较: 条目边界只依赖 tag_len,
                        // 每次读取前都有 `off + entry_size <= tag_len` 作界.
                        let mut off = 16usize;
                        while off + entry_size <= tag_len {
                            // SAFETY: off + entry_size <= tag_len <= tag_end - offset,
                            // 故 pos 至 pos+23 全在本标签内; 只读且不构造引用.
                            let pos = unsafe { tag_ptr.add(off) };
                            // SAFETY: 条目布局 base@+0..8 / len@+8..16 / type@+16..20,
                            // 均由 entry_size >= 24 覆盖.
                            let (base, len, mtype) = unsafe {
                                let b_lo = pos.cast::<u32>().read_unaligned();
                                let b_hi = pos.add(4).cast::<u32>().read_unaligned();
                                let l_lo = pos.add(8).cast::<u32>().read_unaligned();
                                let l_hi = pos.add(12).cast::<u32>().read_unaligned();
                                let ty = pos.add(16).cast::<u32>().read_unaligned();
                                (
                                    (u64::from(b_hi) << 32) | u64::from(b_lo),
                                    (u64::from(l_hi) << 32) | u64::from(l_lo),
                                    ty,
                                )
                            };

                            if mtype == 1 {
                                let end_addr = base + len;
                                if end_addr > max_addr {
                                    max_addr = end_addr;
                                }
                            } else {
                                // D4-i 乙: 非可用项一律登记为 firmware 保留区. PMM 把
                                // [0, mem_size) 当平坦 RAM, 不登记就会把 ACPI 表 / EFI
                                // runtime / MMIO 洞当空闲页分配出去 (与 Linux 按 e820/EFI
                                // 图逐项 reserve 同构). 区位置由 firmware 钉死, 故只预留不搬移.
                                firmware.push(base, len);
                            }
                            mmap_entries += 1;
                            off += entry_size;
                        }

                        if max_addr > 0 {
                            map_mem_size = Some(max_addr);
                        }
                    }
                }
            }
            8 => {
                // framebuffer 载荷读取到自身偏移 29 (tag 内 +37), 需要 tag_size >= 38.
                if tag_size >= 38 {
                    // SAFETY: tag_end <= end 与 tag_size >= 38 保证载荷 0..=29 在信息块内;
                    // 调用期间标签表不被释放.
                    unsafe {
                        multiboot2_fb::parse_framebuffer_tag(tag_ptr.add(8), tag_size);
                    }
                }
            }
            _ => {}
        }

        offset += tag_size as usize;
        offset = (offset + 7) & !7;
    }

    let mem_size = map_mem_size.or(basic_mem_size).unwrap_or(128 * 1024 * 1024);
    (mem_size, mmap_entries, initrd.unwrap_or((0, 0)), firmware)
}

// 符号桩化 (host-test): kernel_end 取值已分叉, 触发 borrow_as_ptr 的
// `&_kernel_end as *const u8` 仅存在于真机分支, expect 须同步收窄.
/// 初始化启动信息子系统, 解析引导器传入的启动参数并返回汇总的 [`BootInfo`]
///
/// # Panics
/// x86_64 真机构建下, 内核映像末尾越过 `USER_CODE_BASE` (kview 恒等窗口
/// 将与用户映射区重叠, I4 代理面失真) 时以启动期断言停机.
pub fn init() -> BootInfo {
    // 符号桩化 (host-test): host 无 _kernel_end 链接脚本符号且不执行裸机引导,
    // 常量中性取值 0. 真机分支取链接脚本符号地址.
    // 消费侧 (`pmm_init` / `pmm.rs`) 要求本值必须是**物理地址**.
    // aarch64: 内核区链接于高半区 (VMA = KERNEL_BASE + LMA), 故须减去 KERNEL_BASE.
    // x86_64: 低 VMA 链接 + 高别名运行 (位置计数器未跳转), 符号地址即物理地址.
    #[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
    let kernel_end = crate::privileged::mm::virt_to_phys(&raw const _kernel_end as u64);
    #[cfg(all(not(feature = "host-test"), not(target_arch = "aarch64")))]
    let kernel_end = &raw const _kernel_end as u64;
    #[cfg(feature = "host-test")]
    let kernel_end = 0u64;

    // K-08: kview 恒等窗口 (`[0, ceil(kernel_end/2MiB) * 2MiB)`) 不得与用户映射区
    // 相交 —— 用户映射统一下界见 `mm::user_va_lower_bound` (x86_64 上取窗口上界与
    // `USER_CODE_BASE` 的较大者). 内核映像一旦越过 `USER_CODE_BASE` (4 MiB),
    // 恒等窗口就会下探不了而覆盖用户区, 内核态经 kview 的 `copy_*_user` / 缺页
    // 路径会解析到恒等物理页而非用户帧 (I4 代理面失真). 产物越界即启动期
    // fail-closed 停机, 不留到运行期随机踩页. aarch64 无 kview (恒等映射在高半区
    // TTBR1), 不适用此约束.
    #[cfg(all(not(feature = "host-test"), target_arch = "x86_64"))]
    assert!(
        kernel_end <= crate::privileged::config::memory::USER_CODE_BASE,
        "K-08: kernel image end {:#X} exceeds USER_CODE_BASE {:#X}; kview identity window would overlap user mappings",
        kernel_end,
        crate::privileged::config::memory::USER_CODE_BASE
    );

    #[cfg(target_arch = "x86_64")]
    let (mem_size, mmap_entries, initrd) = {
        let magic = *MULTIBOOT_MAGIC.lock();
        let ptr = MULTIBOOT_INFO_PTR.lock().0;
        crate::klog_boot_info!("Multiboot: magic=0x{:X} info=0x{:X}", magic, ptr as usize);

        let mut ms: u64 = 128 * 1024 * 1024;
        let mut me: usize = 0;
        let mut initrd: (u64, u64) = (0, 0);
        let mut firmware = FirmwareRegions::new();

        if !ptr.is_null() {
            match magic {
                MULTIBOOT1_MAGIC => {
                    let (m, e, i, f) = parse_multiboot1(ptr);
                    ms = m;
                    me = e;
                    initrd = i;
                    firmware = f;
                }
                MULTIBOOT2_MAGIC => {
                    let (m, e, i, f) = parse_multiboot2(ptr);
                    ms = m;
                    me = e;
                    initrd = i;
                    firmware = f;
                }
                _ => {}
            }
        }
        crate::klog_boot_info!(
            "Boot firmware reserved regions: {}",
            firmware.as_slice().len()
        );
        FIRMWARE_REGIONS.get_or_init(|slot| {
            slot.write(firmware);
        });
        (ms, me, initrd)
    };

    #[cfg(target_arch = "aarch64")]
    let (mem_size, mmap_entries, initrd) = {
        // aarch64 不使用 Multiboot 协议, 但需读取字段以消除 dead_code 警告
        let _ = MULTIBOOT_INFO_PTR.lock().0;
        // QEMU virt 平台默认 512MB.
        // 可通过 `AARCH64_MEM_SIZE` 环境/构建变量覆盖.
        let ms: u64 = option_env!("AARCH64_MEM_MB")
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(512 * 1024 * 1024, |mb| mb * 1024 * 1024);
        // initrd 位置由 aarch64 entry 的 DTB /chosen 解析后经 dtb::initrd_region 交出.
        let initrd = crate::privileged::dtb::initrd_region().unwrap_or((0, 0));
        (ms, 0, initrd)
    };

    let info = BootInfo {
        mem_size,
        kernel_end,
        mmap_entries,
        initrd_base: initrd.0,
        initrd_size: initrd.1,
    };

    if info.initrd_size > 0 {
        crate::klog_boot_info!(
            "Boot module initrd: base=0x{:X} size={}",
            info.initrd_base,
            info.initrd_size
        );
    }

    BOOT_INFO.get_or_init(|slot| {
        slot.write(info);
    });

    info
}

/// C ABI 查询: 可用物理内存大小 (字节)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_get_mem_size() -> u64 {
    get_boot_info().mem_size
}

/// C ABI 查询: 内核映像结束的物理地址
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_get_kernel_end() -> u64 {
    get_boot_info().kernel_end
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::parse_multiboot2;

    /// 拼装 MB2 标签表: 8 字节头 + 各标签 (逐个补到 8 字节对齐) + 终止标签.
    fn build_mb2(tags: &[Vec<u8>]) -> Vec<u8> {
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

    /// tag 4 = 基本内存信息 (mem_lower / mem_upper, 单位 KB).
    fn tag4(mem_lower: u32, mem_upper: u32) -> Vec<u8> {
        let mut t = 4u32.to_le_bytes().to_vec();
        t.extend_from_slice(&16u32.to_le_bytes());
        t.extend_from_slice(&mem_lower.to_le_bytes());
        t.extend_from_slice(&mem_upper.to_le_bytes());
        t
    }

    /// tag 6 = 内存图 (entry_size=24, version=0; 每项 base/len/type/reserved).
    fn tag6(entries: &[(u64, u64, u32)]) -> Vec<u8> {
        let size = u32::try_from(16 + 24 * entries.len()).expect("内存图标签长度溢出 u32");
        let mut t = 6u32.to_le_bytes().to_vec();
        t.extend_from_slice(&size.to_le_bytes());
        t.extend_from_slice(&24u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        for (base, len, kind) in entries {
            t.extend_from_slice(&base.to_le_bytes());
            t.extend_from_slice(&len.to_le_bytes());
            t.extend_from_slice(&kind.to_le_bytes());
            t.extend_from_slice(&0u32.to_le_bytes());
        }
        t
    }

    /// tag 3 = 引导模块 (mod_start / mod_end + NUL 结尾名字).
    fn tag3(mod_start: u32, mod_end: u32, name: &str) -> Vec<u8> {
        let size = u32::try_from(16 + name.len() + 1).expect("模块标签长度溢出 u32");
        let mut t = 3u32.to_le_bytes().to_vec();
        t.extend_from_slice(&size.to_le_bytes());
        t.extend_from_slice(&mod_start.to_le_bytes());
        t.extend_from_slice(&mod_end.to_le_bytes());
        t.extend_from_slice(name.as_bytes());
        t.push(0);
        t
    }

    /// 实测形态: 512 MiB 机器, UEFI 下 mem_upper 只描述 1 MiB 以上首个连续块
    /// (7168 KB → 8 MB), 内存图才给出全部可用 RAM 的上界.
    const UEFI_BASIC: (u32, u32) = (640, 7168);
    const MAP_512MB: [(u64, u64, u32); 2] = [(0x0, 0x9FC00, 1), (0x10_0000, 0x1FF0_0000, 1)];

    #[test]
    fn mb2_tag4_before_tag6_map_is_authoritative() {
        // GRUB 在 SeaBIOS 与 OVMF 下都把 tag 4 排在 tag 6 之后 —— 这是真机次序.
        let blob = build_mb2(&[tag6(&MAP_512MB), tag4(UEFI_BASIC.0, UEFI_BASIC.1)]);
        let (mem, entries, _, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, 0x2000_0000, "内存图权威: 不得被 tag 4 的 8 MB 覆盖");
        assert_eq!(entries, 2);
    }

    #[test]
    fn mb2_tag6_before_tag4_map_is_authoritative() {
        let blob = build_mb2(&[tag4(UEFI_BASIC.0, UEFI_BASIC.1), tag6(&MAP_512MB)]);
        let (mem, entries, _, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, 0x2000_0000);
        assert_eq!(entries, 2);
    }

    #[test]
    fn mb2_mem_size_independent_of_tag_order() {
        // 核心回归锁: 两种次序必须给出同一读数 (修复前 tag 4 无条件赋值,
        // 次序一交换结果就从 512 MB 掉到 8 MB).
        let a = build_mb2(&[tag6(&MAP_512MB), tag4(UEFI_BASIC.0, UEFI_BASIC.1)]);
        let b = build_mb2(&[tag4(UEFI_BASIC.0, UEFI_BASIC.1), tag6(&MAP_512MB)]);
        assert_eq!(
            parse_multiboot2(a.as_ptr()),
            parse_multiboot2(b.as_ptr()),
            "MB2 内存读数必须与标签出现次序无关"
        );
    }

    #[test]
    fn mb2_tag4_only_falls_back_to_basic() {
        let blob = build_mb2(&[tag4(640, 7168)]);
        let (mem, entries, _, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(
            mem,
            (7168 + 1024) * 1024,
            "无内存图时退回 firmware 粗粒度读数"
        );
        assert_eq!(entries, 0);
    }

    #[test]
    fn mb2_map_without_available_entry_falls_back() {
        // 图里只有 reserved 项 → max_addr=0, 不构成权威读数, 仍退回 tag 4.
        let blob = build_mb2(&[tag6(&[(0x0, 0x1000, 2)]), tag4(640, 7168)]);
        let (mem, entries, _, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, (7168 + 1024) * 1024);
        assert_eq!(entries, 1, "reserved 项也计入图条目数");
    }

    #[test]
    fn mb2_no_mem_tags_uses_default() {
        let blob = build_mb2(&[]);
        let (mem, entries, initrd, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, 128 * 1024 * 1024);
        assert_eq!(entries, 0);
        assert_eq!(initrd, (0, 0));
    }

    #[test]
    fn mb2_module_region_parsed_with_mem_tags() {
        let blob = build_mb2(&[
            tag3(0x6B_2000, 0x6B_2000 + 0x1234, "initramfs"),
            tag6(&MAP_512MB),
            tag4(UEFI_BASIC.0, UEFI_BASIC.1),
        ]);
        let (mem, _, initrd, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, 0x2000_0000);
        assert_eq!(initrd, (0x6B_2000, 0x1234));
    }

    #[test]
    fn mb2_first_module_wins() {
        let blob = build_mb2(&[
            tag3(0x6B_2000, 0x6B_3000, "initramfs"),
            tag3(0x70_0000, 0x70_1000, "second"),
        ]);
        let (_, _, initrd, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(initrd, (0x6B_2000, 0x1000), "取首模块为 initrd 载体");
    }

    /// 实测形态 (OVMF 512 MiB): 可用 RAM 之外还有 ACPI 可回收区 (type 3) /
    /// EFI runtime (type 4) / 缺陷页 (type 5) / 保留洞 (type 2).
    const MAP_MIXED: [(u64, u64, u32); 6] = [
        (0x0, 0x9FC00, 1),
        (0x9FC00, 0x400, 2),
        (0x10_0000, 0x1F00_0000, 1),
        (0x1F10_0000, 0x10_0000, 3),
        (0x1F20_0000, 0x20_0000, 4),
        (0x1F40_0000, 0x1000, 5),
    ];

    #[test]
    fn mb2_non_available_entries_become_firmware_regions() {
        let blob = build_mb2(&[tag6(&MAP_MIXED), tag4(UEFI_BASIC.0, UEFI_BASIC.1)]);
        let (mem, entries, _, firmware) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, 0x1F10_0000, "上界只由 type 1 项决定");
        assert_eq!(entries, 6);
        let regions = firmware.as_slice();
        assert_eq!(regions.len(), 4, "非可用项一律登记, 可用项一律不登记");
        assert_eq!(regions[0].base, 0x9FC00);
        assert_eq!(regions[0].size, 0x400);
        assert_eq!(regions[1].base, 0x1F10_0000);
        assert_eq!(regions[2].base, 0x1F20_0000);
        assert_eq!(regions[3].base, 0x1F40_0000);
        assert_eq!(regions[3].size, 0x1000);
    }

    #[test]
    fn mb2_firmware_regions_independent_of_tag_order() {
        // 与内存读数同一把锁: 保留区表也不得随标签次序漂移 (表非空才有判别力).
        let a = build_mb2(&[tag6(&MAP_MIXED), tag4(UEFI_BASIC.0, UEFI_BASIC.1)]);
        let b = build_mb2(&[tag4(UEFI_BASIC.0, UEFI_BASIC.1), tag6(&MAP_MIXED)]);
        let ra = parse_multiboot2(a.as_ptr());
        let rb = parse_multiboot2(b.as_ptr());
        assert_eq!(ra, rb, "MB2 保留区表必须与标签出现次序无关");
        assert_eq!(ra.3.as_slice().len(), 4);
    }

    #[test]
    fn mb2_zero_length_reserved_entry_is_ignored() {
        // 零长度区是空集: 登记它只会白占表槽, 且 PMM 早期预留对 size=0 直接断言.
        let blob = build_mb2(&[tag6(&[(0x1000, 0, 2), (0x2000, 0x1000, 1)])]);
        let (_, entries, _, firmware) = parse_multiboot2(blob.as_ptr());
        assert_eq!(entries, 2, "图条目数仍按原样计 (零长度项也是一条)");
        assert!(firmware.as_slice().is_empty());
    }

    #[test]
    fn mb2_all_available_map_has_no_firmware_regions() {
        let blob = build_mb2(&[tag6(&MAP_512MB)]);
        let (_, _, _, firmware) = parse_multiboot2(blob.as_ptr());
        assert!(firmware.as_slice().is_empty());
    }

    /// 拼装一个标签头 + 载荷, **声明长度与载荷实际长度可不同** (制造畸形标签).
    fn raw_tag(ty: u32, declared_size: u32, payload: &[u8]) -> Vec<u8> {
        let mut t = ty.to_le_bytes().to_vec();
        t.extend_from_slice(&declared_size.to_le_bytes());
        t.extend_from_slice(payload);
        t
    }

    #[test]
    fn mb2_short_tag3_is_skipped_without_reading_payload() {
        // 声明 size=8 的 tag 3 没有 mod_start/mod_end 载荷; 修复前会读到下一标签
        // 的字节当作 mod_end (把 tag 4 的 mem_lower/mem_upper 误当 initrd 区间).
        let blob = build_mb2(&[raw_tag(3, 8, &[]), tag4(UEFI_BASIC.0, UEFI_BASIC.1)]);
        let (mem, _, initrd, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(initrd, (0, 0), "截断的 tag 3 不得被解析为 initrd");
        assert_eq!(mem, (7168 + 1024) * 1024, "后续 tag 4 仍应正常解析");
    }

    #[test]
    fn mb2_tag_size_beyond_info_block_stops_parsing() {
        // 声明长度越过 total_size: 不得按该长度去推算载荷边界, 直接停止.
        let blob = build_mb2(&[raw_tag(3, 0x1000, &[])]);
        let (mem, entries, initrd, firmware) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, 128 * 1024 * 1024, "越界标签后按默认内存回退");
        assert_eq!(entries, 0);
        assert_eq!(initrd, (0, 0));
        assert!(firmware.as_slice().is_empty());
    }

    #[test]
    fn mb2_tag6_zero_entry_size_is_skipped() {
        // entry_size = 0 会让条目循环永不前进 (挂死); 必须整体跳过该标签.
        // tag 4 放在前面: 畸形标签的声明长度与其物理占位不一致, 放在末尾可避免
        // 后续标签被 desync 读取干扰本用例的判据.
        let malformed = raw_tag(6, 24, &0u32.to_le_bytes());
        let blob = build_mb2(&[tag4(UEFI_BASIC.0, UEFI_BASIC.1), malformed]);
        let (mem, entries, _, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(
            mem,
            (7168 + 1024) * 1024,
            "畸形内存图被跳过, 保留 tag 4 读数"
        );
        assert_eq!(entries, 0);
    }

    #[test]
    fn mb2_tag6_entry_not_fitting_is_ignored() {
        // tag_size=20 容不下一条 24 字节条目: 不得读越标签的半个条目.
        let malformed = raw_tag(6, 20, &[24u32.to_le_bytes(), 0u32.to_le_bytes()].concat());
        let blob = build_mb2(&[tag4(UEFI_BASIC.0, UEFI_BASIC.1), malformed]);
        let (mem, entries, _, _) = parse_multiboot2(blob.as_ptr());
        assert_eq!(mem, (7168 + 1024) * 1024);
        assert_eq!(entries, 0);
    }

    #[test]
    #[should_panic(expected = "firmware region table full")]
    fn firmware_region_table_overflow_is_fail_closed() {
        // 丢一条登记 = 该 firmware 区被当空闲页分配出去 (ACPI 表被覆写), 属静默
        // 腐败, 故满表必须停机而不是截断.
        let mut table = super::FirmwareRegions::new();
        for i in 0..=crate::privileged::constants::limits::MAX_FIRMWARE_REGIONS {
            table.push(0x1000 * (i as u64 + 1), 0x1000);
        }
    }
}
