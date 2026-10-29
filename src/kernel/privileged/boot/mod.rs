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

/// 引导阶段汇总的启动信息 (可用内存大小 / 内核结束地址 / 内存映射条目数)
#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    /// 可用物理内存大小 (字节)
    pub mem_size: u64,
    /// 内核映像结束的物理地址
    pub kernel_end: u64,
    /// 引导器报告的内存映射条目数
    pub mmap_entries: usize,
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
        }
    }
}

struct MultibootPtr(*const u8);
// SAFETY: MultibootPtr 包装一个指向启动信息数据的裸指针, 启动早期写入
// 一次, 之后只读. 访问受 MULTIBOOT_INFO_PTR Mutex 保护.
unsafe impl Send for MultibootPtr {}
unsafe impl Sync for MultibootPtr {}

static BOOT_INFO: OnceLock<BootInfo> = OnceLock::new();
static MULTIBOOT_INFO_PTR: IrqSpinLock<MultibootPtr> =
    IrqSpinLock::new(MultibootPtr(core::ptr::null()));
static MULTIBOOT_MAGIC: IrqSpinLock<u32> = IrqSpinLock::new(0);

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

/// 保存引导器传入的魔数与启动信息指针 (由汇编引导入口在跳转前调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_set_multiboot_info(magic: u32, ptr: *const u8) {
    *MULTIBOOT_MAGIC.lock() = magic;
    *MULTIBOOT_INFO_PTR.lock() = MultibootPtr(ptr);
}

#[cfg(target_arch = "x86_64")]
fn parse_multiboot1(ptr: *const u8) -> (u64, usize) {
    // SAFETY: `ptr` 由调用方保证指向有效 Multiboot1Info; 只读借用
    let mbi = unsafe { ptr.cast::<Multiboot1Info>().read_unaligned() };
    let mut mem_size: u64 = 128 * 1024 * 1024;
    let mut mmap_entries: usize = 0;

    if mbi.flags & MBOOT1_FLAG_MEM != 0 {
        mem_size = (u64::from(mbi.mem_upper) + 1024) * 1024;
    }

    if mbi.flags & MBOOT1_FLAG_MMAP != 0 {
        let mmap_start = mbi.mmap_addr as *const MemoryMapEntry;
        let mmap_end = (mbi.mmap_addr + mbi.mmap_length) as *const MemoryMapEntry;
        let mut max_addr: u64 = 0;

        let mut current = mmap_start;
        while current < mmap_end {
            // SAFETY: `current` 由调用方保证为有效指针; 只读访问
            let entry = unsafe { &*current };
            let end = entry.base_addr() + entry.length();
            if end > max_addr && entry.is_available() {
                max_addr = end;
            }
            mmap_entries += 1;
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            current = unsafe {
                current.cast::<u8>().add(entry.size as usize + 4) as usize as *const MemoryMapEntry
            };
        }

        if max_addr > 0 {
            mem_size = max_addr;
        }
    }

    (mem_size, mmap_entries)
}

#[cfg(target_arch = "x86_64")]
fn parse_multiboot2(ptr: *const u8) -> (u64, usize) {
    // SAFETY: `ptr` 由调用方保证指向有效 u32; 只读借用
    let total_size = unsafe { ptr.cast::<u32>().read_unaligned() };
    let mut mem_size: u64 = 128 * 1024 * 1024;
    let mut mmap_entries: usize = 0;

    let mut offset: usize = 8;
    let end = total_size as usize;

    while offset + 8 <= end {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let tag_ptr = unsafe { ptr.add(offset) };
        // SAFETY: `tag_ptr` 由调用方保证指向有效 u32; 只读借用
        let tag_type = unsafe { tag_ptr.cast::<u32>().read_unaligned() };
        // SAFETY: 指针由调用方保证有效, 偏移 1 不越界
        let tag_size = unsafe { tag_ptr.add(4).cast::<u32>().read_unaligned() };

        if tag_type == 0 || tag_size == 0 {
            break;
        }

        match tag_type {
            4 => {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                let basic_ptr = unsafe { tag_ptr.add(8) };
                // SAFETY: `basic_ptr` 由调用方保证指向有效 u32; 只读借用
                let _mem_lower = unsafe { basic_ptr.cast::<u32>().read_unaligned() };
                // SAFETY: 指针由调用方保证有效, 偏移 1 不越界
                let mem_upper = unsafe { basic_ptr.add(4).cast::<u32>().read_unaligned() };
                mem_size = (u64::from(mem_upper) + 1024) * 1024;
            }
            6 => {
                // SAFETY: `const` 由调用方保证为有效指针; 只读访问
                let entry_size = unsafe { tag_ptr.add(8).cast::<u32>().read_unaligned() };
                // SAFETY: `const` 由调用方保证为有效指针; 只读访问
                let _entry_version = unsafe { tag_ptr.add(12).cast::<u32>().read_unaligned() };
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                let entries_start = unsafe { tag_ptr.add(16) };
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                let entries_end = unsafe { tag_ptr.add(tag_size as usize) };
                let mut max_addr: u64 = 0;

                let mut pos = entries_start;
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                while unsafe { pos.add(entry_size as usize) <= entries_end } {
                    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                    let base = unsafe {
                        let lo = pos.cast::<u32>().read_unaligned();
                        let hi = pos.add(4).cast::<u32>().read_unaligned();
                        (u64::from(hi) << 32) | u64::from(lo)
                    };
                    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                    let len = unsafe {
                        let lo = pos.add(8).cast::<u32>().read_unaligned();
                        let hi = pos.add(12).cast::<u32>().read_unaligned();
                        (u64::from(hi) << 32) | u64::from(lo)
                    };
                    // SAFETY: 指针由调用方保证有效, 偏移 4 不越界
                    let mtype = unsafe { pos.add(16).cast::<u32>().read_unaligned() };

                    if mtype == 1 {
                        let end_addr = base + len;
                        if end_addr > max_addr {
                            max_addr = end_addr;
                        }
                    }
                    mmap_entries += 1;
                    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                    pos = unsafe { pos.add(entry_size as usize) };
                }

                if max_addr > 0 {
                    mem_size = max_addr;
                }
            }
            8 => {
                // SAFETY: tag_ptr 指向多引导标签表内有效的 framebuffer 标签
                // (offset 8 起为 tag 载荷), 调用期间标签表不被释放.
                unsafe {
                    multiboot2_fb::parse_framebuffer_tag(tag_ptr.add(8), tag_size);
                }
            }
            _ => {}
        }

        offset += tag_size as usize;
        offset = (offset + 7) & !7;
    }

    (mem_size, mmap_entries)
}

// 符号桩化 (host-test): kernel_end 取值已分叉, 触发 borrow_as_ptr 的
// `&_kernel_end as *const u8` 仅存在于真机分支, expect 须同步收窄.
/// 初始化启动信息子系统, 解析引导器传入的启动参数并返回汇总的 [`BootInfo`]
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

    #[cfg(target_arch = "x86_64")]
    let (mem_size, mmap_entries) = {
        let magic = *MULTIBOOT_MAGIC.lock();
        let ptr = MULTIBOOT_INFO_PTR.lock().0;

        let mut ms: u64 = 128 * 1024 * 1024;
        let mut me: usize = 0;

        if !ptr.is_null() {
            match magic {
                MULTIBOOT1_MAGIC => {
                    let (m, e) = parse_multiboot1(ptr);
                    ms = m;
                    me = e;
                }
                MULTIBOOT2_MAGIC => {
                    let (m, e) = parse_multiboot2(ptr);
                    ms = m;
                    me = e;
                }
                _ => {}
            }
        }
        (ms, me)
    };

    #[cfg(target_arch = "aarch64")]
    let (mem_size, mmap_entries) = {
        // aarch64 不使用 Multiboot 协议, 但需读取字段以消除 dead_code 警告
        let _ = MULTIBOOT_INFO_PTR.lock().0;
        // QEMU virt 平台默认 512MB.
        // 可通过 `AARCH64_MEM_SIZE` 环境/构建变量覆盖.
        let ms: u64 = option_env!("AARCH64_MEM_MB")
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(512 * 1024 * 1024, |mb| mb * 1024 * 1024);
        (ms, 0)
    };

    let info = BootInfo {
        mem_size,
        kernel_end,
        mmap_entries,
    };

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
