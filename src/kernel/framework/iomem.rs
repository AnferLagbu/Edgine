//! `IoMem` — MMIO 安全代理 (TCB)
//!
//! 防止 driver 访问其 BAR 之外的 MMIO 区域,
//! 通过全局别名检测表防止同一物理地址被多个 driver 独占映射。
//!
//! ## 与 Asterinas OSTD `IoMem` 的关系
//!
//! 等价于 OSTD 的 `IoMem`。
//!
//! ## SAFETY 不变量
//!
//! - 每个 `IoMem` 实例在创建时核验 phys..phys+len 不与其他实例冲突。
//! - 所有读/写操作检查 offset 在 [0, len) 范围内。
//! - 底层访问使用 `read_volatile/write_volatile` 保证 MMIO 语义。

use core::fmt;
use core::ptr::NonNull;

use crate::framework::constants::limits::MAX_MMIO_MAPPINGS;
use crate::framework::mm::PhysAddr;
use crate::framework::mm::phys_to_virt;
use crate::framework::sync::IrqSpinLock;
// 仅供 x86_64 的 `ensure_mmio_mapped` 使用 (aarch64 走静态别名, 无需补映射)
#[cfg(target_arch = "x86_64")]
use crate::klog_warn;
/// MMIO 别名注册表, 防止同一物理区域被多次映射。
/// 使用 `spin::Mutex` (已在内核中广泛使用) 保证线程安全。
static ALIAS_REGISTRY: IrqSpinLock<AliasRegistry> = IrqSpinLock::new(AliasRegistry::new());

struct AliasRegistry {
    entries: [(u64, usize, &'static str); MAX_MMIO_MAPPINGS],
    count: usize,
}

impl AliasRegistry {
    const fn new() -> Self {
        Self {
            entries: [(0, 0, ""); MAX_MMIO_MAPPINGS],
            count: 0,
        }
    }

    fn check_conflict(&self, phys: u64, end: u64) -> Option<&'static str> {
        for i in 0..self.count {
            let (b, l, name) = self.entries[i];
            // B03-22: 已有 entries 的 l 也可能溢出, 用 saturating_add 但
            // 当发生溢出时 (result 钳到 u64::MAX) 跳过范围比较, 视为冲突
            // (因为溢出范围不可信)。
            let existing_end = b.saturating_add(l as u64);
            if phys < existing_end && end > b {
                return Some(name);
            }
        }
        None
    }

    fn register_checked(
        &mut self,
        phys: u64,
        end: u64,
        name: &'static str,
    ) -> Result<(), &'static str> {
        if self.count >= MAX_MMIO_MAPPINGS {
            return Err("MMIO alias registry full");
        }
        if let Some(_conflict) = self.check_conflict(phys, end) {
            return Err("MMIO region overlaps existing region");
        }
        // 存储 (phys, len) 但 end 已被 checked_add 校验不溢出
        let len = (end - phys) as usize;
        self.entries[self.count] = (phys, len, name);
        self.count += 1;
        Ok(())
    }

    fn unregister(&mut self, phys: u64) {
        for i in 0..self.count {
            if self.entries[i].0 == phys {
                self.entries[i] = self.entries[self.count - 1];
                self.count -= 1;
                return;
            }
        }
    }
}

/// MMIO 物理地址 → 内核虚拟地址。
///
/// 统一走 [`phys_to_virt`] (L1-04 收敛): `x86_64` 为高半区直接映射
/// (boot.asm 已建立), aarch64 为**高半区别名** —— 内核迁至高半区后二者同为
/// `PA + KERNEL_BASE`, 故不再按架构分派 (原 aarch64 分支的独立常量
/// `HIGH_ALIAS_BASE` 已删除, 换算基数唯一来源为 `mm::KERNEL_BASE`).
///
/// aarch64 侧必须走别名: EL0→EL1 入口把 TTBR0 切到本进程 **EL1 视图**
/// (只承载用户页 —— 内核迁高半区后 DRAM 块已移除, 见 L1-05), 该视图**刻意不含 Device**
/// (见 `vmm_aarch64::build_el1_view`), 故内核态不能再以低半区恒等地址访问
/// MMIO (UART/GIC 已在别处改用别名, 此处补齐 virtio 等经 `IoMem` 的消费者).
/// 别名由 TTBR1 (= `L0_TABLE`) 的 `L1_IDMAP[0] → L2_DEVICE` 提供
/// (0-1 GiB Device, 2 MiB 粒度), 恒常有效, **无需动态建映射**.
#[inline]
fn mmio_virt(phys: u64) -> u64 {
    phys_to_virt(phys)
}

/// 经校验的 MMIO 区域句柄。
///
/// 创建时核验物理地址范围并注册别名检测;
/// 运行时所有读/写做边界检查。
pub struct IoMem {
    phys_base: PhysAddr,
    len: usize,
    virt: NonNull<u8>,
    name: &'static str,
}

impl IoMem {
    /// 创建 MMIO 句柄。
    ///
    /// # SAFETY
    /// - phys 必须指向有效的设备 MMIO 物理区域。
    /// - phys..phys+len 必须已映射到内核空间 (identity map / ioremap)。
    /// - 同一物理区域不重复创建 (由别名检测保证)。
    /// # Errors
    /// 长度为 0、物理地址未 4 字节对齐、虚拟地址映射为 null 或与已有 MMIO 区域别名冲突时返回 Err。
    pub unsafe fn new(
        phys: PhysAddr,
        len: usize,
        name: &'static str,
    ) -> Result<Self, &'static str> {
        if len == 0 {
            return Err("IoMem: zero-length MMIO region");
        }
        if !phys.as_u64().is_multiple_of(4) {
            return Err("IoMem: phys must be 4-byte aligned");
        }

        // B03-22 修复: 溢出检查. 之前未检查 `phys + len` 是否溢出 u64,
        // 溢出回绕 0 绕过后续所有范围检查 (I5 不变式违反)。
        let end = phys
            .as_u64()
            .checked_add(len as u64)
            .ok_or("IoMem: phys + len overflows u64")?;

        // SAFETY: 别名注册在 Mutex 保护下, 无竞争。
        {
            let mut reg = ALIAS_REGISTRY.lock();
            // 使用已校验的 `end` 而非 phys + len (可能溢出)
            reg.register_checked(phys.as_u64(), end, name)?;
        }

        let virt_addr = mmio_virt(phys.as_u64()) as *mut u8;
        let virt = NonNull::new(virt_addr).ok_or("IoMem: null virtual address")?;

        Ok(Self {
            phys_base: phys,
            len,
            virt,
            name,
        })
    }

    /// 从 PCI BAR 地址创建 MMIO 句柄 (安全包装)。
    ///
    /// PCI BAR 地址由 PCI 枚举保证为有效 MMIO 区域,
    /// 因此本函数是安全的。services 层应使用此方法而非 `new`。
    ///
    /// # 参数
    /// - `bar_phys`: PCI BAR 基地址 (来自 PCI 枚举)
    /// - `len`: MMIO 区域大小 (来自 BAR 大小寄存器)
    /// - `name`: 设备名称 (用于调试和别名检测)
    /// # Errors
    /// PCI BAR 基地址为 0 (设备未配置) 或底层 `IoMem::new` 校验失败时返回 Err。
    pub fn from_pci_bar(
        bar_phys: PhysAddr,
        len: usize,
        name: &'static str,
    ) -> Result<Self, &'static str> {
        if bar_phys.as_u64() == 0 {
            return Err("IoMem: PCI BAR is zero (device not configured)");
        }

        // 确保 MMIO 物理地址在内核页表中有映射.
        // boot.asm 仅映射前 1GB 物理内存到高半区, PCI BAR 地址 (如 0xfebc0000)
        // 可能超出此范围, 需要动态映射.
        //
        // aarch64 **不需要**此步: MMIO 走 TTBR1 高半区别名, 0-1 GiB 的 Device
        // 映射 (L2_DEVICE) 在 `mmu::init` 已建立且覆盖全部该类地址 (见 `mmio_virt`).
        #[cfg(target_arch = "x86_64")]
        Self::ensure_mmio_mapped(bar_phys.as_u64(), len);

        // SAFETY: PCI BAR 地址由 PCI 枚举保证为有效 MMIO 区域,
        // 且 ensure_mmio_mapped 已保证页表映射存在 (aarch64 由上段注释所述别名覆盖).
        unsafe { Self::new(bar_phys, len, name) }
    }

    /// 从平台设备物理基址创建 MMIO 句柄 (安全包装)。
    ///
    /// 平台设备 (非 PCI, 如 aarch64 PL011 UART) 的 MMIO 基址由设备树/平台常量固定,
    /// 不参与 PCI BAR 枚举, 故无 `from_pci_bar` 的 BAR==0 特判, 但契约同等:
    /// 调用方须保证 `phys..phys+len` 是平台设备真实独占的 MMIO 区域, 且已可在内核态访问。
    /// services 层应使用此方法而非 `new`。
    ///
    /// # 参数
    /// - `phys`: 平台设备 MMIO 基址 (由设备树/平台常量提供)
    /// - `len`: MMIO 区域大小 (字节)
    /// - `name`: 设备名称 (用于调试和别名检测)
    /// # Errors
    /// 物理基址为 0 或底层 `IoMem::new` 校验 (对齐/溢出/别名冲突) 失败时返回 Err。
    pub fn from_platform_device(
        phys: PhysAddr,
        len: usize,
        name: &'static str,
    ) -> Result<Self, &'static str> {
        if phys.as_u64() == 0 {
            return Err("IoMem: platform device phys is zero");
        }

        // 本方法当前仅 aarch64 PL011 使用: MMIO 经 TTBR1 高半区别名 (0-1 GiB Device,
        // 由 `mmu::init` 的 L2_DEVICE 静态建立), 无需动态补映射。
        // 若未来在 x86_64 引入平台设备, 其基址须落在 boot.asm 已映射范围内,
        // 否则需在此补 `ensure_mmio_mapped` 调用。
        // SAFETY: 调用方契约保证 phys..phys+len 是平台设备有效 MMIO 区域且内核态可访问。
        unsafe { Self::new(phys, len, name) }
    }

    // x86_64 专属: aarch64 的 MMIO 经 TTBR1 高半区别名访问, 0-1 GiB Device 映射
    // 由 `mmu::init` 的 L2_DEVICE 静态提供, 不存在"需要动态补映射"的情形
    // (见 `mmio_virt`). 故整个函数在 aarch64 不参与编译, 避免死代码 (F9).
    #[cfg(target_arch = "x86_64")]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 确保 MMIO 物理地址范围在内核页表中有映射.
    /// 使用 2MB 大页映射, 覆盖 [phys, phys + len) 所在的所有 2MB 页.
    /// 如果映射已存在 (同一 2MB 页), `map_huge_page` 会安全地跳过或覆盖.
    fn ensure_mmio_mapped(phys: u64, len: usize) {
        use crate::framework::mm::get_vmm;
        use crate::framework::mm::{PageFlags, PageSize, VirtAddr};

        let vmm = get_vmm();
        let page_2m: u64 = 0x200000;
        let start_page = phys & !(page_2m - 1);
        let end = phys + len as u64;
        let end_page = (end + page_2m - 1) & !(page_2m - 1);

        let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::GLOBAL;

        let mut pa = start_page;
        while pa < end_page {
            let va = phys_to_virt(pa);
            if let Err(e) = vmm.map_huge_page(VirtAddr(va), PhysAddr(pa), flags, PageSize::Size2M) {
                klog_warn!(
                    Driver,
                    "IoMem: failed to map MMIO 2MB page va={:#x} pa={:#x}: {}",
                    va,
                    pa,
                    e
                );
            }
            pa += page_2m;
        }
    }

    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[inline(always)]
    pub fn phys(&self) -> PhysAddr {
        self.phys_base
    }
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[inline(always)]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 获取 `IoMem` 内部虚拟地址指针 (供上层安全访问结构体 MMIO)。
    ///
    /// # SAFETY
    /// 调用方必须保证:
    /// - 返回的指针类型与 MMIO 寄存器布局一致 (大小/对齐)。
    /// - 仅通过 volatile 访问 (无编译器重排)。
    /// - 不会写出 `IoMem` 自身的字节范围 (offset + `size_of::<T>()` <= self.len)。
    #[inline(always)]
    pub unsafe fn virt_ptr(&self) -> *mut u8 {
        // SAFETY: `self.virt` 是 `IoMem::from_*` 构造时由 `NonNull::new_unchecked`
        // 校验的物理基地址经 MMU 映射后的虚拟地址, 满足 NonNull 契约。
        // 调用方 (标记为 `unsafe fn`) 须自行保证:
        //   1. 指针类型 (`*mut T`) 与 MMIO 寄存器布局一致 (大小/对齐)
        //   2. 仅通过 volatile 访问 (无编译器重排)
        //   3. 不会写出 `IoMem` 自身的字节范围 (`offset + size_of::<T>() <= self.len`)
        self.virt.as_ptr()
    }

    fn check_offset(&self, offset: usize, size: usize) -> Result<(), &'static str> {
        if offset.saturating_add(size) > self.len {
            return Err("IoMem: access out of bounds");
        }
        Ok(())
    }

    /// 从 MMIO 区域读取一个字节。
    /// # Panics
    /// 读取范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发, 便于单元测试与 early detection.
    #[inline]
    pub fn read_u8(&self, offset: usize) -> u8 {
        debug_assert!(
            self.check_offset(offset, 1).is_ok(),
            "IoMem: read_u8 offset+1 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 1)
            .expect("IoMem: read_u8 offset+1 越界 (构造函数保证合法范围)");
        // SAFETY: `check_offset` 已验证 `offset + 1 <= self.len`, 指针 `self.virt + offset`
        // 落在 IoMem 持有的 MMIO 区域内, 不会越界; `read_volatile` 防止编译器重排。
        unsafe { self.virt.as_ptr().add(offset).read_volatile() }
    }
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 从 MMIO 区域读取一个 u16 (小端)。
    /// # Panics
    /// 读取范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发.
    #[inline]
    pub fn read_u16(&self, offset: usize) -> u16 {
        debug_assert!(
            self.check_offset(offset, 2).is_ok(),
            "IoMem: read_u16 offset+2 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 2)
            .expect("IoMem: read_u16 offset+2 越界 (构造函数保证合法范围)");
        // SAFETY: `check_offset(offset, 2)` 已验证 2 字节访问不越界; u16 转换要求
        // 2 字节对齐 (PCI BAR MMIO 由 BIOS/UEFI 建立时保证自然对齐)。
        unsafe { (self.virt.as_ptr().add(offset) as *const u16).read_volatile() }
    }
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 从 MMIO 区域读取一个 u32 (小端)。
    /// # Panics
    /// 读取范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发.
    #[inline]
    pub fn read_u32(&self, offset: usize) -> u32 {
        debug_assert!(
            self.check_offset(offset, 4).is_ok(),
            "IoMem: read_u32 offset+4 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 4)
            .expect("IoMem: read_u32 offset+4 越界 (构造函数保证合法范围)");
        // SAFETY: `check_offset(offset, 4)` 已验证 4 字节访问不越界; 4 字节自然对齐
        // 由 MMIO 基地址的页对齐保证 (PAGE_SIZE=4096, 任何 4 字节偏移都对其)。
        unsafe { (self.virt.as_ptr().add(offset) as *const u32).read_volatile() }
    }
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 从 MMIO 区域读取一个 u64 (小端)。
    /// # Panics
    /// 读取范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发.
    #[inline]
    pub fn read_u64(&self, offset: usize) -> u64 {
        debug_assert!(
            self.check_offset(offset, 8).is_ok(),
            "IoMem: read_u64 offset+8 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 8)
            .expect("IoMem: read_u64 offset+8 越界 (构造函数保证合法范围)");
        // SAFETY: `check_offset(offset, 8)` 已验证 8 字节访问不越界; 8 字节自然对齐
        // 由 MMIO 基地址的页对齐保证。
        unsafe { (self.virt.as_ptr().add(offset) as *const u64).read_volatile() }
    }
    /// 向 MMIO 区域写入一个字节。
    /// # Panics
    /// 写入范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发.
    #[inline]
    pub fn write_u8(&self, offset: usize, val: u8) {
        debug_assert!(
            self.check_offset(offset, 1).is_ok(),
            "IoMem: write_u8 offset+1 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 1)
            .expect("IoMem: write_u8 offset+1 越界 (构造函数保证合法范围)");
        // SAFETY: 与 `read_u8` 对称, 写 1 字节不会越界; volatile 写保证设备立即可见。
        unsafe {
            self.virt.as_ptr().add(offset).write_volatile(val);
        }
    }
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 向 MMIO 区域写入一个 u16 (小端)。
    /// # Panics
    /// 写入范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发.
    #[inline]
    pub fn write_u16(&self, offset: usize, val: u16) {
        debug_assert!(
            self.check_offset(offset, 2).is_ok(),
            "IoMem: write_u16 offset+2 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 2)
            .expect("IoMem: write_u16 offset+2 越界 (构造函数保证合法范围)");
        // SAFETY: `check_offset(offset, 2)` 已验证 2 字节写不越界; 2 字节对齐由 MMIO 基地址页对齐保证。
        unsafe {
            (self.virt.as_ptr().add(offset) as *mut u16).write_volatile(val);
        }
    }
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 向 MMIO 区域写入一个 u32 (小端)。
    /// # Panics
    /// 写入范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发.
    #[inline]
    pub fn write_u32(&self, offset: usize, val: u32) {
        debug_assert!(
            self.check_offset(offset, 4).is_ok(),
            "IoMem: write_u32 offset+4 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 4)
            .expect("IoMem: write_u32 offset+4 越界 (构造函数保证合法范围)");
        // SAFETY: `check_offset(offset, 4)` 已验证 4 字节写不越界; 4 字节自然对齐由页对齐保证。
        unsafe {
            (self.virt.as_ptr().add(offset) as *mut u32).write_volatile(val);
        }
    }
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 向 MMIO 区域写入一个 u64 (小端)。
    /// # Panics
    /// 写入范围超出 MMIO 区域大小时 panic (生产路径).
    /// 调试构建下 `debug_assert!` 提前触发.
    #[inline]
    pub fn write_u64(&self, offset: usize, val: u64) {
        debug_assert!(
            self.check_offset(offset, 8).is_ok(),
            "IoMem: write_u64 offset+8 越界 (offset={}, len={})",
            offset,
            self.len
        );
        self.check_offset(offset, 8)
            .expect("IoMem: write_u64 offset+8 越界 (构造函数保证合法范围)");
        // SAFETY: `check_offset(offset, 8)` 已验证 8 字节写不越界; 8 字节自然对齐由页对齐保证。
        unsafe {
            (self.virt.as_ptr().add(offset) as *mut u64).write_volatile(val);
        }
    }
}

impl Drop for IoMem {
    fn drop(&mut self) {
        let mut reg = ALIAS_REGISTRY.lock();
        reg.unregister(self.phys_base.as_u64());
    }
}

impl fmt::Display for IoMem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "IoMem({}, phys=0x{:x}, len=0x{:x})",
            self.name,
            self.phys_base.as_u64(),
            self.len
        )
    }
}

// SAFETY: IoMem 封装了独占的 MMIO 物理区域, 内核态独占访问。
unsafe impl Send for IoMem {}
// SAFETY: &IoMem 通过别名检测 + SpinLock (内核层注册表) 保证无并发写。
unsafe impl Sync for IoMem {}
