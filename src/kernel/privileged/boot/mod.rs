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

/// 取回引导器交付的 initrd (引导模块) 字节切片; 无模块时返回 `None`.
///
/// 该区由引导器装入并经 PMM 预留 (见 `boot_memory_init`), 生命周期覆盖内核运行;
/// 经高半区直射区映射为只读切片, 供 functions 层解包 (functions 零 unsafe, 见 F1).
#[cfg(feature = "initramfs")]
pub fn initrd_blob() -> Option<&'static [u8]> {
    let info = get_boot_info();
    if info.initrd_size == 0 {
        return None;
    }
    let size = crate::privileged::glue::usize_from_u64(info.initrd_size);
    // SAFETY: initrd 区由引导器装入并经 PMM 预留, 生命周期覆盖内核运行; `phys_to_virt`
    // 经高半区直射区给出唯一内核映射 (x86 直射区不受低半区恒等窗口约束, aarch64 直射区
    // 覆盖 DRAM), 只读借用且无别名写.
    let blob = unsafe {
        let ptr = crate::privileged::mm::phys_to_virt(info.initrd_base) as *const u8;
        core::slice::from_raw_parts(ptr, size)
    };
    Some(blob)
}

/// 保存引导器传入的魔数与启动信息指针 (由汇编引导入口在跳转前调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn boot_set_multiboot_info(magic: u32, ptr: *const u8) {
    *MULTIBOOT_MAGIC.lock() = magic;
    *MULTIBOOT_INFO_PTR.lock() = MultibootPtr(ptr);
}

#[cfg(target_arch = "x86_64")]
fn parse_multiboot1(ptr: *const u8) -> (u64, usize, (u64, u64)) {
    // SAFETY: `ptr` 由调用方保证指向有效 Multiboot1Info; 只读借用
    let mbi = unsafe { ptr.cast::<Multiboot1Info>().read_unaligned() };
    let mut mem_size: u64 = 128 * 1024 * 1024;
    let mut mmap_entries: usize = 0;

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

    (mem_size, mmap_entries, initrd)
}

#[cfg(target_arch = "x86_64")]
fn parse_multiboot2(ptr: *const u8) -> (u64, usize, (u64, u64)) {
    // SAFETY: `ptr` 由调用方保证指向有效 u32; 只读借用
    let total_size = unsafe { ptr.cast::<u32>().read_unaligned() };
    let mut mem_size: u64 = 128 * 1024 * 1024;
    let mut mmap_entries: usize = 0;
    let mut initrd: Option<(u64, u64)> = None;

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
            3 => {
                // tag 3 = 引导模块 (每模块一条); 取首模块 (initrd) 物理区间.
                // SAFETY: tag_ptr 指向 MB2 标签表内有效 tag; +8/+12 (mod_start/mod_end,
                // 32 位) 为该 tag 载荷, 标签表在启动期只读.
                let mod_start = unsafe { tag_ptr.add(8).cast::<u32>().read_unaligned() };
                let mod_end = unsafe { tag_ptr.add(12).cast::<u32>().read_unaligned() };
                let sz = mod_end.saturating_sub(mod_start);
                if initrd.is_none() && sz > 0 && mod_start != 0 {
                    initrd = Some((u64::from(mod_start), u64::from(sz)));
                }
            }
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

    (mem_size, mmap_entries, initrd.unwrap_or((0, 0)))
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

        if !ptr.is_null() {
            match magic {
                MULTIBOOT1_MAGIC => {
                    let (m, e, i) = parse_multiboot1(ptr);
                    ms = m;
                    me = e;
                    initrd = i;
                }
                MULTIBOOT2_MAGIC => {
                    let (m, e, i) = parse_multiboot2(ptr);
                    ms = m;
                    me = e;
                    initrd = i;
                }
                _ => {}
            }
        }
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
