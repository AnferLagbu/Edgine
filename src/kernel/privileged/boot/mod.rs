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

use crate::privileged::sync::IrqSpinLock;

use crate::privileged::sync::OnceLock;
#[cfg(target_arch = "aarch64")]
pub mod aarch64;
pub mod multiboot2_fb;

pub const MULTIBOOT1_MAGIC: u32 = 0x2BADB002;
pub const MULTIBOOT2_MAGIC: u32 = 0x36D76289;

pub const MBOOT1_FLAG_MEM: u32 = 1 << 0;
pub const MBOOT1_FLAG_MMAP: u32 = 1 << 6;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Multiboot1Info {
    pub flags: u32,
    pub mem_lower: u32,
    pub mem_upper: u32,
    pub boot_device: u32,
    pub cmdline: u32,
    pub mods_count: u32,
    pub mods_addr: u32,
    pub syms: [u32; 4],
    pub mmap_length: u32,
    pub mmap_addr: u32,
    pub drives_length: u32,
    pub drives_addr: u32,
    pub config_table: u32,
    pub boot_loader_name: u32,
    pub apm_table: u32,
    pub vbe_control_info: u32,
    pub vbe_mode_info: u32,
    pub vbe_mode: u16,
    pub vbe_interface_seg: u16,
    pub vbe_interface_off: u16,
    pub vbe_interface_len: u16,
    pub vbe_control_info_high: u64,
    pub vbe_mode_info_high: u64,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryMapEntry {
    pub size: u32,
    pub base_addr_low: u32,
    pub base_addr_high: u32,
    pub length_low: u32,
    pub length_high: u32,
    pub mtype: u32,
}

impl MemoryMapEntry {
    pub fn base_addr(&self) -> u64 {
        u64::from(self.base_addr_high) << 32 | u64::from(self.base_addr_low)
    }

    pub fn length(&self) -> u64 {
        u64::from(self.length_high) << 32 | u64::from(self.length_low)
    }

    pub fn is_available(&self) -> bool {
        self.mtype == 1
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    pub mem_size: u64,
    pub kernel_end: u64,
    pub mmap_entries: usize,
}

impl BootInfo {
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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_set_multiboot_info(magic: u32, ptr: *const u8) {
    *MULTIBOOT_MAGIC.lock() = magic;
    *MULTIBOOT_INFO_PTR.lock() = MultibootPtr(ptr);
}

#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
fn parse_multiboot1(ptr: *const u8) -> (u64, usize) {
    // SAFETY: `ptr` 由调用方保证指向有效 Multiboot1Info; 只读借用
    let mbi = unsafe { &*(ptr as *const Multiboot1Info) };
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
                (current as *const u8).add(entry.size as usize + 4) as *const MemoryMapEntry
            };
        }

        if max_addr > 0 {
            mem_size = max_addr;
        }
    }

    (mem_size, mmap_entries)
}

#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
fn parse_multiboot2(ptr: *const u8) -> (u64, usize) {
    // SAFETY: `ptr` 由调用方保证指向有效 u32; 只读借用
    let total_size = unsafe { *(ptr as *const u32) };
    let mut mem_size: u64 = 128 * 1024 * 1024;
    let mut mmap_entries: usize = 0;

    let mut offset: usize = 8;
    let end = total_size as usize;

    while offset + 8 <= end {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let tag_ptr = unsafe { ptr.add(offset) };
        // SAFETY: `tag_ptr` 由调用方保证指向有效 u32; 只读借用
        let tag_type = unsafe { *(tag_ptr as *const u32) };
        // SAFETY: 指针由调用方保证有效, 偏移 1 不越界
        let tag_size = unsafe { *((tag_ptr as *const u32).add(1)) };

        if tag_type == 0 || tag_size == 0 {
            break;
        }

        match tag_type {
            4 => {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                let basic_ptr = unsafe { tag_ptr.add(8) };
                // SAFETY: `basic_ptr` 由调用方保证指向有效 u32; 只读借用
                let _mem_lower = unsafe { *(basic_ptr as *const u32) };
                // SAFETY: 指针由调用方保证有效, 偏移 1 不越界
                let mem_upper = unsafe { *((basic_ptr as *const u32).add(1)) };
                mem_size = (u64::from(mem_upper) + 1024) * 1024;
            }
            6 => {
                // SAFETY: `const` 由调用方保证为有效指针; 只读访问
                let entry_size = unsafe { *(tag_ptr.add(8) as *const u32) };
                // SAFETY: `const` 由调用方保证为有效指针; 只读访问
                let _entry_version = unsafe { *((tag_ptr.add(8) as *const u32).add(1)) };
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
                        let lo = *(pos as *const u32);
                        let hi = *((pos as *const u32).add(1));
                        (u64::from(hi) << 32) | u64::from(lo)
                    };
                    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                    let len = unsafe {
                        let lo = *((pos as *const u32).add(2));
                        let hi = *((pos as *const u32).add(3));
                        (u64::from(hi) << 32) | u64::from(lo)
                    };
                    // SAFETY: 指针由调用方保证有效, 偏移 4 不越界
                    let mtype = unsafe { *((pos as *const u32).add(4)) };

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
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                multiboot2_fb::parse_framebuffer_tag(unsafe { tag_ptr.add(8) }, tag_size);
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
#[cfg_attr(
    not(feature = "host-test"),
    expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )
)]
pub fn init() -> BootInfo {
    // 符号桩化 (host-test): host 无 _kernel_end 链接脚本符号且不执行裸机引导,
    // 常量中性取值 0. 真机分支取链接脚本符号地址.
    // 消费侧 (`pmm_init` / `pmm.rs`) 要求本值必须是**物理地址**.
    // aarch64: 内核区链接于高半区 (VMA = KERNEL_BASE + LMA), 故须减去 KERNEL_BASE.
    // x86_64: 低 VMA 链接 + 高别名运行 (位置计数器未跳转), 符号地址即物理地址.
    #[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
    let kernel_end = {
        // SAFETY: `_kernel_end` 为链接脚本定义的符号, 只读访问.
        unsafe { crate::privileged::mm::virt_to_phys(&_kernel_end as *const u8 as u64) }
    };
    #[cfg(all(not(feature = "host-test"), not(target_arch = "aarch64")))]
    let kernel_end = {
        // SAFETY: `_kernel_end` 为链接脚本定义的符号, 只读访问.
        unsafe { &_kernel_end as *const u8 as u64 }
    };
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
    #[expect(
        clippy::map_unwrap_or,
        reason = "option_env → parse → 默认值链式调用可读性较好; map_or 反而需要反向参数"
    )]
    let (mem_size, mmap_entries) = {
        // aarch64 不使用 Multiboot 协议, 但需读取字段以消除 dead_code 警告
        let _ = MULTIBOOT_INFO_PTR.lock().0;
        // QEMU virt 平台默认 512MB.
        // 可通过 `AARCH64_MEM_SIZE` 环境/构建变量覆盖.
        let ms: u64 = option_env!("AARCH64_MEM_MB")
            .and_then(|s| s.parse::<u64>().ok())
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(512 * 1024 * 1024);
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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_get_mem_size() -> u64 {
    get_boot_info().mem_size
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_get_kernel_end() -> u64 {
    get_boot_info().kernel_end
}
