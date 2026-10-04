//! 全局描述符表 (Global Descriptor Table, GDT) - `x86_64` 实现
//!
//! ## 功能概览
//!
//! - **类型安全**: 使用枚举和常量替代魔法数字
//! - **编译时检查**: 常量泛型确保描述符格式正确
//! - **零成本**: 所有检查都在编译期完成, 运行时无开销
//!
//! ## GDT 结构 (x86-64 长模式)
//!
//! ```text
//! Index | Selector | Description
//! ------|----------|------------------------------------------
//! 0     | 0x00     | Null Descriptor (必须)
//! 1     | 0x08     | Kernel Code Segment (64-bit)
//! 2     | 0x10     | Kernel Data Segment
//! 3     | 0x18     | User Data Segment (Ring 3) — 与 User Code 互换以适配 SYSRET 规则
//! 4     | 0x20     | User Code Segment (Ring 3)
//! 5     | 0x28     | TSS Descriptor (64-bit, 占用2个槽位)
//! 6     | 0x30     | TSS High (自动生成)
//! ```
//!
//! ## 对比 C 版本 (gdt.c, 71行)
//!
//! **功能复刻 + 增强**:
//! ✅ Descriptor 结构体 (替代 raw struct)
//! ✅ Access Byte 枚举 (编译时验证)
//! ✅ Granularity 标志 (类型安全位操作)
//! ✅ TSS 描述符 (自动处理高32位)
//! ✅ lgdt 汇编封装 (safe wrapper)

use core::sync::atomic::{AtomicPtr, Ordering};

use crate::privileged::racy_cell::RacyCell;

// ============================================================================
// 常量定义
// ============================================================================

/// GDT 最大条目数 (x86-64 通常需要 7 个)
pub const GDT_MAX_ENTRIES: usize = 7;

/// Per-CPU IST 栈大小 (16KB)
const PER_CPU_IST_SIZE: usize = 16384;

/// Per-CPU KPTI 用户态出口 trampoline 栈大小 (4KB, 单页)
///
/// KPTI-08 收窄后用户页表不再复制内核高半区别名, `process_switch_asm` 的用户态
/// 出口**不能**再在 prev 内核栈上构建 iretq 帧 (该栈不在 next 用户页表中).
/// 改用本栈: 其栈顶页由 `kpti::map_kernel_pages_in_user_pml4` 显式映射进每份
/// 用户页表, 出口 stub 在切 CR3 后仍能 pop/iretq.
const PER_CPU_TRAMPOLINE_SIZE: usize = 4096;

/// 页对齐栈容器 (KPTI-08)
///
/// IST 栈与 KPTI trampoline 栈必须 4KB 对齐: `TSS.ist[]` 与 `trampoline_top`
/// 取的都是"栈顶"地址, 而 CPU (中断交付) / 出口 stub 会在切换 CR3 **之前**
/// 用该地址压入帧; 栈顶页须能以单一页面精确映射 (公式 `top - PAGE_SIZE`).
/// 栈顶若非页对齐, 帧会跨页 ⇒ 必须映射 2 页, 映射面与公式均不再确定.
#[repr(C, align(4096))]
struct AlignedStack<const N: usize>([u8; N]);

/// 空选择子值 (必须为 0)
pub const SELECTOR_NULL: u16 = 0x00;

/// 内核代码段选择子
pub const SELECTOR_KERNEL_CODE: u16 = 0x08;

/// 内核数据段选择子
pub const SELECTOR_KERNEL_DATA: u16 = 0x10;

/// 用户数据段选择子 (Ring 3)
/// 注: 与用户代码段互换位置以适配 SYSRET 指令的段选择规则
pub const SELECTOR_USER_DATA: u16 = 0x18;

/// 用户代码段选择子 (Ring 3)
pub const SELECTOR_USER_CODE: u16 = 0x20;

/// TSS 选择子 (低32位)
pub const SELECTOR_TSS: u16 = 0x28;

// P2.B + F-13 (DECISION-051): GDT 选择子单一来源策略.
// 双重单一来源: Rust 端 pub const 是 Rust 代码单一来源; x86_64.ld
// ABSOLUTE 符号是汇编代码单一来源. 二者数值必须一致. 未来 GDT 重排
// 需同时修改 gdt.rs 与 x86_64.ld (DECISION-051 已登记).
// 不在 Rust 端用 #[link_name] 导出: 链接器符号名仍受 Rust mangling
// 影响, 实践上不稳定; 直接用链接脚本 ABSOLUTE 更简洁可靠.

// ============================================================================
// 位域定义 (Access Byte + Granularity)
// ============================================================================

/// 访问权限字节 (Access Byte) 标志
#[derive(Debug, Clone, Copy)]
pub struct AccessByte(pub(crate) u8);

impl AccessByte {
    /// 已访问 (Accessed) - CPU 设置此位
    pub const ACCESSED: u8 = 1 << 0;
    /// 可读/写 (对于代码段=可读, 数据段=可写)
    pub const READABLE_WRITABLE: u8 = 1 << 1;
    /// 方向/符合 (Direction/Conforming)
    pub const DIRECTION_CONFORMING: u8 = 1 << 2;
    /// 可执行 (Executable)
    /// - 0: 数据段 (Data Segment)
    /// - 1: 代码段 (Code Segment)
    pub const EXECUTABLE: u8 = 1 << 3;
    /// 描述符类型 (Descriptor Type)
    /// 必须为 1 (System=0 在 LDT 中使用)
    pub const TYPE_SYSTEM: u8 = 1 << 4;
    /// 特权级 (DPL, Descriptor Privilege Level) - bits 6:5
    /// - 00: Ring 0 (内核)
    /// - 11: Ring 3 (用户)
    pub const DPL_MASK: u8 = 0b0110_0000;
    /// 存在位 (Present) - 必须为 1 才有效
    pub const PRESENT: u8 = 1 << 7;

    /// 创建内核代码段访问字节
    #[inline]
    pub const fn kernel_code() -> Self {
        // P=1, DPL=00, S=1, E=1, RW=1, A=0 => 0x9A
        Self(Self::PRESENT | Self::TYPE_SYSTEM | Self::EXECUTABLE | Self::READABLE_WRITABLE)
    }

    /// 创建内核数据段访问字节
    #[inline]
    pub const fn kernel_data() -> Self {
        // P=1, DPL=00, S=1, E=0, RW=1, A=0 => 0x92
        Self(Self::PRESENT | Self::TYPE_SYSTEM | Self::READABLE_WRITABLE)
    }

    /// 创建用户代码段访问字节 (Ring 3)
    #[inline]
    pub const fn user_code() -> Self {
        // P=1, DPL=11, S=1, E=1, RW=1, A=0 => 0xFA
        Self(
            Self::PRESENT
                | Self::DPL_MASK
                | Self::TYPE_SYSTEM
                | Self::EXECUTABLE
                | Self::READABLE_WRITABLE,
        )
    }

    /// 创建用户数据段访问字节 (Ring 3)
    #[inline]
    pub const fn user_data() -> Self {
        // P=1, DPL=11, S=1, E=0, RW=1, A=0 => 0xF2
        Self(Self::PRESENT | Self::DPL_MASK | Self::TYPE_SYSTEM | Self::READABLE_WRITABLE)
    }

    /// 创建 TSS 访问字节
    #[inline]
    pub const fn tss() -> Self {
        // P=1, DPL=00, S=0 (System), Type=1001 (TSS Available), Busy=0 => 0x89
        Self(0x89)
    }
}

/// 粒度标志 (Granularity Byte) 标志
#[derive(Debug, Clone, Copy)]
pub struct Granularity(pub(crate) u8);

impl Granularity {
    /// 段限制单位 (Limit granularity)
    /// - 0: 1 字节粒度
    /// - 1: 4KB 页粒度
    pub const PAGE_GRANULARITY: u8 = 1 << 7;
    /// 默认操作数大小 (Default Operation Size)
    /// - 0: 16-bit 保护模式
    /// - 1: 32-bit 保护模式 (在 64-bit 长模式下忽略)
    pub const SIZE_32BIT: u8 = 1 << 6;
    /// 64-bit 代码段标志 (Long Mode)
    /// 仅对代码段有效, 设置后启用 64-bit 模式
    pub const LONG_MODE: u8 = 1 << 5;

    /// 创建 64-bit 代码段粒度 (4KB 粒度, Long Mode)
    #[inline]
    pub const fn code_64bit() -> Self {
        Self(Self::PAGE_GRANULARITY | Self::LONG_MODE)
    }

    /// 创建数据段粒度 (4KB 粒度, 32-bit)
    #[inline]
    pub const fn data_32bit() -> Self {
        Self(Self::PAGE_GRANULARITY | Self::SIZE_32BIT)
    }

    /// 创建 TSS 粒度 (字节粒度, 64-bit)
    #[inline]
    pub const fn tss_64bit() -> Self {
        Self(Self::LONG_MODE)
    }
}

// ============================================================================
// 数据结构定义
// ============================================================================

/// GDT 条目 (64位, 8字节)
///
/// 内存布局:
/// ```text
/// Bits   Field
/// ----   ---------------------------
/// 0-15   Limit (bits 15:0)
/// 16-31  Base (bits 15:0)
/// 32-39  Base (bits 23:16)
/// 40-43  Type (4 bits)
/// 44-45  S (System, 1 bit)
/// 46-47  DPL (2 bits)
/// 48     P (Present, 1 bit)
/// 49-51  Limit (bits 19:16)
/// 52     AVL (Available, 1 bit)
/// 53     L (Long mode, 1 bit)
/// 54     D/B (Size, 1 bit)
/// 55     G (Granularity, 1 bit)
/// 56-63  Base (bits 31:24)
/// ```
#[derive(Debug, Clone, Copy, Default)]
#[repr(C, packed)]
pub struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_middle: u8,
    access: u8,
    granularity_limit_high: u8,
    base_high: u8,
}

impl GdtEntry {
    /// 创建空描述符 (Null Descriptor)
    #[inline]
    pub const fn null() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_middle: 0,
            access: 0,
            granularity_limit_high: 0,
            base_high: 0,
        }
    }

    /// 创建标准段描述符 (代码或数据)
    ///
    /// # Arguments
    /// * `base` - 段基地址 (在长模式下通常为 0)
    /// * `limit` - 段限制 (最大 4GB 如果使用页粒度)
    /// * `access` - 访问权限字节
    /// * `gran` - 粒度标志字节
    #[inline]
    pub const fn new_segment(base: u32, limit: u32, access: AccessByte, gran: Granularity) -> Self {
        Self {
            limit_low: (limit & 0xFFFF) as u16,
            base_low: (base & 0xFFFF) as u16,
            base_middle: ((base >> 16) & 0xFF) as u8,
            access: access.0,
            granularity_limit_high: (((limit >> 16) & 0x0F) as u8) | (gran.0 & 0xF0),
            base_high: ((base >> 24) & 0xFF) as u8,
        }
    }

    /// 创建 TSS 描述符 (低64位)
    ///
    /// TSS 描述符占用两个 GDT 槽位:
    /// - 第一个槽位: bits 63:0 of base + limit
    /// - 第二个槽位: bits 127:64 of base
    ///
    /// # Arguments
    /// * `tss_addr` - TSS 结构体的 64 位地址
    /// * `tss_size` - TSS 结构体大小 (bytes)
    #[inline]
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub const fn tss_low(tss_addr: u64, tss_size: u16) -> Self {
        let base_low = tss_addr as u32;

        Self {
            limit_low: tss_size,
            base_low: (base_low & 0xFFFF) as u16,
            base_middle: ((base_low >> 16) & 0xFF) as u8,
            access: AccessByte::tss().0,
            granularity_limit_high: 0x00, // TSS 不使用粒度标志
            base_high: ((base_low >> 24) & 0xFF) as u8,
        }
    }

    /// 创建 TSS 描述符的高64位 (base`[63:32]`)
    #[inline]
    pub const fn tss_high(tss_addr: u64) -> Self {
        let base_high32 = (tss_addr >> 32) as u32;

        Self {
            limit_low: (base_high32 & 0xFFFF) as u16,
            base_low: ((base_high32 >> 16) & 0xFFFF) as u16,
            base_middle: 0,
            access: 0,
            granularity_limit_high: 0,
            base_high: 0,
        }
    }
}

/// GDTR 寄存器结构 (用于 lgdt 指令)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct GdtPtr {
    /// GDT 大小限制 (bytes - 1)
    pub limit: u16,
    /// GDT 基地址 (64位指针)
    pub base: u64,
}

// ============================================================================
// Per-CPU GDT 数据结构
// ============================================================================

/// Syscall 入口 per-CPU 数据 (通过 swapgs + GS 段访问)
///
/// 布局: 汇编代码通过 `[gs:OFFSET]` 访问各字段,
/// 因此字段顺序与偏移必须与 isr.asm 中的常量一致。
///
/// | 偏移 | 字段         | 汇编常量          |
/// |------|-------------|-------------------|
/// | 0    | kernel_rsp  | KERNEL_RSP_OFF    |
/// | 8    | kernel_pml4 | KERNEL_PML4_OFF   |
/// | 16   | user_pml4   | USER_PML4_OFF     |
/// | 24   | user_rsp    | USER_RSP_OFF      |
/// | 32   | trampoline_top | TRAMPOLINE_TOP_OFF |
///
/// `user_rsp` (TRACK-INIT-RING3-SYSCALL-RET): syscall 入口保存当前用户 RSP,
/// 供 iretq 帧的 RSP 槽位使用. 与 `kernel_rsp` 分离, 避免覆盖 (原实现复用
/// [gs:0], 导致首次 syscall 后 kernel_rsp 丢失, 后续 syscall 用错内核栈).
///
/// `trampoline_top` (KPTI-08): 本 CPU KPTI trampoline 栈顶 VA (高半区).
/// `process_switch_asm` 的用户态出口在 `swapgs` 前读入该值, 切换到 trampoline
/// 栈后再构建 iretq 帧 (该栈页已映射进 next 的用户页表).
#[repr(C)]
pub struct SyscallPerCpu {
    pub kernel_rsp: u64,
    pub kernel_pml4: u64,
    pub user_pml4: u64,
    pub user_rsp: u64,
    pub trampoline_top: u64,
}

/// 每个 CPU 独立的 syscall 内核栈大小 (64KB, syscall 入口与调度切换共用).
/// 与内核栈容量惯例对齐 (KERNEL_STACK_SIZE / AP_STACK_SIZE 均为 64KB).
/// 实证: 8KB 回归测试中 fork 路径三重故障 (脚本 -no-reboot, 日志止于 X),
/// 64KB 下 fork 连续回归稳定 — d8330ef9 增大栈容量为 fork 必要条件而非临时诊断.
const PER_CPU_SYSCALL_STACK_SIZE: usize = 65536;

/// 每个 CPU 独立的 GDT + TSS + IST 栈 + syscall 数据
///
/// 所有 CPU 共享相同的段描述符 (flat model)，
/// 但 TSS 描述符必须指向各自的 TSS 实例。
///
/// # Safety: repr(C) 强制
///
/// `#[repr(C)]` 保证字段按声明顺序排列且偏移可预测。
/// 去掉 repr(C) 时 Rust 编译器可能将大数组 (`syscall_stack`) 重排到
/// entries 之前, 导致 `kernel_rsp` == GDT base, 栈 push 覆盖 GDT
/// entries → CPU 取指到 RIP=0x3 → #UD.
#[repr(C)]
struct PerCpuGdt {
    entries: [GdtEntry; GDT_MAX_ENTRIES],
    ptr: GdtPtr,
    tss: super::tss::TaskStateSegment,
    syscall: SyscallPerCpu,
    syscall_stack: [u8; PER_CPU_SYSCALL_STACK_SIZE],
    ist0: AlignedStack<PER_CPU_IST_SIZE>,
    ist1: AlignedStack<PER_CPU_IST_SIZE>,
    ist2: AlignedStack<PER_CPU_IST_SIZE>,
    ist3: AlignedStack<PER_CPU_IST_SIZE>,
    trampoline_stack: AlignedStack<PER_CPU_TRAMPOLINE_SIZE>,
}

impl PerCpuGdt {
    #[expect(
        clippy::large_stack_arrays,
        reason = "PerCpuGdt 的 syscall/IST 栈数组布局于 BSP_GDT / PMM 分配的 AP GDT, 非栈上分配"
    )]
    const fn new() -> Self {
        Self {
            entries: [GdtEntry::null(); GDT_MAX_ENTRIES],
            ptr: GdtPtr { limit: 0, base: 0 },
            tss: super::tss::TaskStateSegment::zeroed(),
            syscall: SyscallPerCpu {
                kernel_rsp: 0,
                kernel_pml4: 0,
                user_pml4: 0,
                user_rsp: 0,
                trampoline_top: 0,
            },
            syscall_stack: [0u8; PER_CPU_SYSCALL_STACK_SIZE],
            ist0: AlignedStack([0u8; PER_CPU_IST_SIZE]),
            ist1: AlignedStack([0u8; PER_CPU_IST_SIZE]),
            ist2: AlignedStack([0u8; PER_CPU_IST_SIZE]),
            ist3: AlignedStack([0u8; PER_CPU_IST_SIZE]),
            trampoline_stack: AlignedStack([0u8; PER_CPU_TRAMPOLINE_SIZE]),
        }
    }
}

// ============================================================================
// 全局状态 (Per-CPU)
// ============================================================================

/// BSP 的静态 GDT 存储.
///
/// BSP 的 GDT 在启动早期 (页分配器就绪之前) 就必须可用, 故固定为静态实例.
/// `PerCpuGdt::new()` 全零, 该实例落入 `.bss` (不占镜像 `.data`).
///
/// # Safety 不变量
///
/// - **写入时机**: 仅 `gdt_init` (BSP, 单线程早期) 写入
/// - **运行时**: 各 CPU 只读访问; BSP 独占本槽位
static BSP_GDT: RacyCell<PerCpuGdt> = RacyCell::new(PerCpuGdt::new());

/// AP 的 GDT 物理地址表 (按 `cpu_index` 索引, 槽位 0 保留给 BSP).
///
/// AP 的 GDT 由 `alloc_ap_gdt` 在 SIPI 之前按需从 PMM 分配, 以**低半区物理地址**
/// 存放 —— boot 建立的恒等映射 (phys 0..4GB) 与 `KERNEL_PML4` 保留的低半区使该
/// 地址在 AP 运行期始终可访问, 且 `&gdt.syscall as u64 == phys` 与 KPTI 用户页表
/// 的 "LMA 恒等" 映射自洽 (见 `per_cpu_gdt_head_range`).
///
/// 槽位为 `null` 表示尚未分配 (对应 AP 未上线).
static AP_GDT: [AtomicPtr<PerCpuGdt>; crate::privileged::config::MAX_CPUS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; crate::privileged::config::MAX_CPUS];

// ============================================================================
// 内部辅助函数
// ============================================================================

/// 将 CPU 编号映射到 GDT 槽位下标.
///
/// 槽位容量与 `MAX_CPUS` 一致; 取模仅为防御越界 (越界时退化为槽位别名而非 UB).
#[inline]
fn gdt_slot(cpu: u32) -> usize {
    (cpu as usize) % crate::privileged::config::MAX_CPUS
}

/// 获取指定 CPU 的 GDT 不可变引用.
///
/// # 不变量
///
/// 调用方保证目标槽位已分配 (启动时序: `alloc_ap_gdt` 早于 SIPI, `register_cpu`
/// 晚于 `gdt_init_ap`). 若 AP 槽位为 `null` (例如真机 LAPIC ID 与顺序 `cpu_index`
/// 不一致导致查错槽位), 回退 BSP GDT 以保证内存安全 —— 这是已知的预存问题
/// (见 `smp::get_current_cpu` 返回 LAPIC ID 的语义).
#[inline]
fn per_cpu_gdt(cpu: u32) -> &'static PerCpuGdt {
    let idx = gdt_slot(cpu);
    if idx == 0 {
        // SAFETY: BSP_GDT 为静态实例; 启动期由 gdt_init 独占写入, 运行期只读.
        return unsafe { BSP_GDT.get() };
    }
    let ptr = AP_GDT[idx].load(Ordering::Acquire);
    if ptr.is_null() {
        // SAFETY: 回退 BSP GDT 仅用于避免 null 解引用 (见 doc 的预存问题).
        return unsafe { BSP_GDT.get() };
    }
    // SAFETY: 指针由 alloc_ap_gdt 以 Release 发布, 指向 PMM 分配且已清零的
    // PerCpuGdt; 低半区恒等映射使其运行期始终可访问; 只读访问.
    unsafe { &*ptr }
}

/// 获取指定 CPU 的 GDT 可变引用.
///
/// # 不变量
///
/// 同 `per_cpu_gdt`: 调用方保证对目标槽位独占访问 (启动期单线程 / 运行期本核).
#[inline]
fn per_cpu_gdt_mut(cpu: u32) -> &'static mut PerCpuGdt {
    let idx = gdt_slot(cpu);
    if idx == 0 {
        // SAFETY: BSP_GDT 为静态实例; 调用方保证该槽位独占访问.
        return BSP_GDT.get_mut();
    }
    let ptr = AP_GDT[idx].load(Ordering::Acquire);
    if ptr.is_null() {
        // SAFETY: 回退 BSP GDT 仅用于避免 null 解引用 (见 per_cpu_gdt 的预存问题).
        return BSP_GDT.get_mut();
    }
    // SAFETY: 指针由 alloc_ap_gdt 以 Release 发布; 调用方保证该槽位独占访问.
    unsafe { &mut *ptr }
}

/// 获取当前 CPU 的 GDT 不可变引用
#[inline]
fn current_per_cpu_gdt() -> &'static PerCpuGdt {
    let cpu = crate::privileged::smp::get_current_cpu();
    per_cpu_gdt(cpu)
}

/// 获取当前 CPU 的 GDT 可变引用
#[inline]
fn current_per_cpu_gdt_mut() -> &'static mut PerCpuGdt {
    let cpu = crate::privileged::smp::get_current_cpu();
    per_cpu_gdt_mut(cpu)
}

/// 初始化 per-CPU GDT 的段描述符 (所有 CPU 共享相同段描述符)
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn init_gdt_entries(entries: &mut [GdtEntry; GDT_MAX_ENTRIES]) {
    entries[0] = GdtEntry::null();

    entries[1] = GdtEntry::new_segment(
        0,
        0xFFFF_FFFF,
        AccessByte::kernel_code(),
        Granularity::code_64bit(),
    );

    entries[2] = GdtEntry::new_segment(
        0,
        0xFFFF_FFFF,
        AccessByte::kernel_data(),
        Granularity::data_32bit(),
    );

    entries[3] = GdtEntry::new_segment(
        0,
        0xFFFF_FFFF,
        AccessByte::user_data(),
        Granularity::data_32bit(),
    );

    entries[4] = GdtEntry::new_segment(
        0,
        0xFFFF_FFFF,
        AccessByte::user_code(),
        Granularity::code_64bit(),
    );
}

/// 初始化本槽位的 IST 栈顶、KPTI trampoline 栈顶 (均为高半区 VA).
///
/// 二者必须取高半区 VA: 用户态异常交付时 CPU 在 `isr_common` 切内核页表**之前**
/// 就用 `TSS.ist[N-1]` 压入异常帧; `process_switch_asm` 的用户态出口同样在切 CR3
/// 之前 (以及之后) 使用 `trampoline_top`. 因此这些栈顶页必须能在**用户页表**中
/// 按同一 VA 访问 —— 由 `kpti::map_kernel_pages_in_user_pml4` 逐 CPU 显式映射
/// 栈顶页 (KPTI-08: 用户页表不再复制内核高半区别名).
///
/// 若改用低半区恒等 VA, 就必须把内核栈页恒等映射进用户页表低半区, 那份映射会与
/// 用户 ELF 装载区 (0x400000) 争用同一 VA — 内核静态布局一旦漂移到该地址, ELF
/// 代码段便无法映射 → Ring 3 取指 #PF. 故统一走高半区 VA.
fn init_stack_tops(gdt: &mut PerCpuGdt) {
    let tops = ist_tops_of(gdt);
    for (i, top) in tops.iter().enumerate() {
        gdt.tss.set_ist(i, *top);
    }
    gdt.syscall.trampoline_top = trampoline_top_of(gdt);
}

/// 由静态布局推导指定槽位的 IST0..3 栈顶 VA (高半区, 页对齐).
///
/// 与 `init_stack_tops` 共用同一公式: 该函数写入 `TSS.ist[]`, 本函数供 KPTI 在
/// **`gdt_init` 之前** (见 `kpti_init` 的调用时序) 计算待映射的栈顶页 —— 那时
/// `TSS.ist[]` 仍为 0, 不能读运行时字段.
#[inline]
fn ist_tops_of(gdt: &PerCpuGdt) -> [u64; 4] {
    let bias = crate::privileged::mm::KERNEL_BASE;
    let top = |s: &AlignedStack<PER_CPU_IST_SIZE>| bias + s.0.as_ptr() as u64 + s.0.len() as u64;
    [
        top(&gdt.ist0),
        top(&gdt.ist1),
        top(&gdt.ist2),
        top(&gdt.ist3),
    ]
}

/// 由静态布局推导指定槽位的 KPTI trampoline 栈顶 VA (高半区, 页对齐).
///
/// 同 `ist_tops_of`: 公式与 `init_stack_tops` 一致, 可在 `gdt_init` 之前调用.
#[inline]
fn trampoline_top_of(gdt: &PerCpuGdt) -> u64 {
    let s = &gdt.trampoline_stack.0;
    crate::privileged::mm::KERNEL_BASE + s.as_ptr() as u64 + s.len() as u64
}

// ============================================================================
// 公共 API
// ============================================================================

/// 初始化 BSP 的 GDT 和 TSS
///
/// **必须在内核启动早期调用**, 在 IDT 初始化之前。
///
/// # 功能
/// 1. 设置标准段描述符 (Null, Kernel CS/DS, User CS/DS)
/// 2. 初始化 per-CPU TSS 结构体
/// 3. 设置 TSS 描述符 (占用2个槽位)
/// 4. 加载 GDTR (lgdt 指令)
/// 5. 加载 TR (ltr 指令, 任务寄存器)
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub fn gdt_init() -> i32 {
    use crate::privileged::klog::{LogCategory, LogLevel, klog_write};

    static INIT_MSG: &[u8] = b"Initializing GDT and TSS (BSP)...\0";
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        klog_write(
            LogLevel::Info as u8,
            LogCategory::Boot as u8,
            core::ptr::null(),
            core::ptr::null(),
            0,
            INIT_MSG.as_ptr() as *const u8,
        );
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let gdt = per_cpu_gdt_mut(0);

        gdt.ptr.limit = (core::mem::size_of::<[GdtEntry; GDT_MAX_ENTRIES]>() - 1) as u16;
        gdt.ptr.base = gdt.entries.as_ptr() as u64;

        init_gdt_entries(&mut gdt.entries);

        gdt.tss = super::tss::TaskStateSegment::zeroed();

        // IST 栈顶 / KPTI trampoline 栈顶统一取高半区 VA (理由见 init_stack_tops).
        init_stack_tops(gdt);

        gdt.tss.iomap_base = core::mem::size_of::<super::tss::TaskStateSegment>() as u16;

        let tss_addr = &gdt.tss as *const _ as u64;
        let tss_size = (core::mem::size_of::<super::tss::TaskStateSegment>() - 1) as u16;

        gdt.entries[5] = GdtEntry::tss_low(tss_addr, tss_size);
        gdt.entries[6] = GdtEntry::tss_high(tss_addr);

        gdt_flush(&gdt.ptr);
        tss_flush(SELECTOR_TSS);

        // 统一内核栈契约 (D5): `kernel_rsp` 恒为**高半区 VA**.
        // 运行期由 `gdt_set_kernel_rsp` (经 `cpu::arch::set_kernel_stack`) 按当前
        // 任务内核栈更新, 与 TSS.RSP0 同值; 此处初值指向本 CPU `syscall_stack`
        // 的高半区别名, 仅覆盖"首个任务被调度之前"这一窗口 —— 该窗口内不可能
        // 出现用户态 syscall (用户态首次进入由 `enter_user` / 调度器完成).
        gdt.syscall.kernel_rsp = crate::privileged::mm::KERNEL_BASE
            + gdt.syscall_stack.as_ptr() as u64
            + gdt.syscall_stack.len() as u64;

        // 读取当前 CR3 作为 PML4 初始值
        // KPTI 激活后, kernel_pml4/user_pml4 已由 kpti_init 通过
        // gdt_set_kpti_pml4 正确设置 (含 PCID 编码), 不应覆盖.
        // 仅在 KPTI 未激活时用当前 CR3 初始化.
        if !crate::privileged::mm::kpti::kpti_is_active() {
            let current_cr3: u64;
            core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack));
            gdt.syscall.kernel_pml4 = current_cr3;
            gdt.syscall.user_pml4 = current_cr3;
        }

        // IA32_GS_BASE — 内核态 GS 基地址 (swapgs 前)
        // IA32_KERNEL_GS_BASE — 用户态 GS 基地址 (swapgs 后)
        //
        // 本内核 GS 约定:
        //   内核态: IA32_GS_BASE = per_cpu_addr, IA32_KERNEL_GS_BASE = 0
        //   用户态: IA32_GS_BASE = 0, IA32_KERNEL_GS_BASE = per_cpu_addr
        //
        // 进入 enter_user_asm 时在内核态, IA32_GS_BASE = per_cpu_addr.
        // 执行 swapgs 后, IA32_GS_BASE = 0 (用户态), IA32_KERNEL_GS_BASE = per_cpu_addr.
        // 用户态执行 syscall 时, syscall_entry 的 swapgs 将其换回 per_cpu_addr.
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const IA32_GS_BASE: u32 = 0xC0000101;
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const IA32_KERNEL_GS_BASE: u32 = 0xC0000102;
        crate::privileged::cpu::msr::write_msr(IA32_GS_BASE, &gdt.syscall as *const _ as u64);
        crate::privileged::cpu::msr::write_msr(IA32_KERNEL_GS_BASE, 0);

        // 诊断: 验证 write_msr 后 IA32_GS_BASE 的实际值
        let gs_base_readback = crate::privileged::cpu::msr::read_msr(IA32_GS_BASE);
        let kernel_gs_base_readback = crate::privileged::cpu::msr::read_msr(IA32_KERNEL_GS_BASE);
        crate::klog_boot_info!(
            "[GDT] GS MSR verify: IA32_GS_BASE={:#x} (expect {:#x}), IA32_KERNEL_GS_BASE={:#x} (expect 0)",
            gs_base_readback,
            &gdt.syscall as *const _ as u64,
            kernel_gs_base_readback
        );

        crate::klog_boot_info!(
            "[GDT] syscall base={:#x}, kernel_rsp={:#x}, kernel_pml4={:#x}, user_pml4={:#x}",
            &gdt.syscall as *const _ as u64,
            gdt.syscall.kernel_rsp,
            gdt.syscall.kernel_pml4,
            gdt.syscall.user_pml4,
        );
    }

    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    static OK_MSG: &[u8] = b"GDT and TSS initialized successfully (BSP)\0";
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        klog_write(
            LogLevel::Info as u8,
            LogCategory::Boot as u8,
            core::ptr::null(),
            core::ptr::null(),
            0,
            OK_MSG.as_ptr() as *const u8,
        );
    }

    0
}

/// 为指定 AP 分配独立的 `PerCpuGdt` (SIPI 之前调用).
///
/// 从 PMM 分配连续物理页并以**低半区物理地址**登记到 `AP_GDT[cpu_index]`:
/// boot 阶段的恒等映射 (phys 0..4GB) 与 `KERNEL_PML4` 保留的低半区使其在 AP
/// 运行期始终可访问, 无需 `map_page` (内核高半区映射对已恒等的低地址是冗余的).
/// 分配到的页清零, 与 `PerCpuGdt::new()` 的等价格式 (全零) 一致.
///
/// 返回 `false` 表示 PMM 分配失败 (无可用页), 调用方应放弃启动该 AP.
///
/// 取代原先 `static mut PER_CPU_GDT: [MaybeUninit<PerCpuGdt>; 256]` 的编译期
/// 预置 (34MiB `.data`), 改为按在线核数按需分配.
pub fn alloc_ap_gdt(cpu_index: u32) -> bool {
    let idx = gdt_slot(cpu_index);
    if idx == 0 {
        // 槽位 0 保留给 BSP 静态实例.
        return false;
    }
    if !AP_GDT[idx].load(Ordering::Acquire).is_null() {
        return true;
    }

    let page = crate::privileged::mm::PAGE_SIZE as usize;
    let pages = core::mem::size_of::<PerCpuGdt>().div_ceil(page);
    let Some(phys) = crate::privileged::mm::pmm_alloc_pages_phys(pages) else {
        return false;
    };
    let ptr = phys.0 as *mut PerCpuGdt;

    // SAFETY: phys 是 PMM 新分配的连续页起点 (页对齐, 满足 PerCpuGdt 的 4096
    // 对齐要求); 低半区恒等映射 (phys 0..4GB, boot 建立且 KERNEL_PML4 保留)
    // 使其可直接写; 此路径在 AP 启动期单线程执行.
    unsafe {
        core::ptr::write_bytes(ptr.cast::<u8>(), 0, pages * page);
    }

    AP_GDT[idx].store(ptr, Ordering::Release);
    true
}

/// 初始化 AP 的 per-CPU GDT 和独立 TSS
///
/// Trampoline 已通过 `lgdt [gdt_ptr]` 加载了 BSP 的 GDT 作为过渡，
/// 本函数在 AP 进入长模式后调用，为目标 CPU 初始化独立的 GDT + TSS。
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub fn gdt_init_ap(cpu_index: u32) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let ap = per_cpu_gdt_mut(cpu_index);

        ap.ptr.limit = (core::mem::size_of::<[GdtEntry; GDT_MAX_ENTRIES]>() - 1) as u16;
        ap.ptr.base = ap.entries.as_ptr() as u64;

        init_gdt_entries(&mut ap.entries);

        ap.tss = super::tss::TaskStateSegment::zeroed();

        // IST 栈顶 / KPTI trampoline 栈顶与 BSP 路径 (gdt_init) 一致, 理由见 init_stack_tops.
        init_stack_tops(ap);

        ap.tss.iomap_base = core::mem::size_of::<super::tss::TaskStateSegment>() as u16;

        let tss_addr = &ap.tss as *const _ as u64;
        let tss_size = (core::mem::size_of::<super::tss::TaskStateSegment>() - 1) as u16;

        ap.entries[5] = GdtEntry::tss_low(tss_addr, tss_size);
        ap.entries[6] = GdtEntry::tss_high(tss_addr);

        gdt_flush(&ap.ptr);
        tss_flush(SELECTOR_TSS);

        // AP 由 trampoline 以 CS=0x18 进入长模式, 而内核 per-CPU GDT 的
        // entry[3] (0x18) 是 user_data 数据段. lgdt 只换 GDTR 不重载 CS,
        // CPU 仍缓存 trampoline GDT 的 0x18 代码段描述符. 若不显式重载
        // CS=0x08, 首个中断返回路径的 iretq 会以陈旧 CS=0x18 在当前 GDT
        // 中重载 → #GP(0x18) → #DF → triple fault.
        reload_cs();

        // 统一内核栈契约 (D5): 与 BSP 路径 (gdt_init) 一致, `kernel_rsp` 取高半区 VA,
        // 理由见该处注释.
        ap.syscall.kernel_rsp = crate::privileged::mm::KERNEL_BASE
            + ap.syscall_stack.as_ptr() as u64
            + ap.syscall_stack.len() as u64;

        // KPTI 激活后, 全局 kernel_pml4/user_pml4 (含 PCID 编码) 由 kpti_init 写入
        // BSP GDT; AP 的动态 GDT 在 SIPI 之前才分配 (清零), 故从 BSP GDT 复制,
        // 不能依赖 kpti_init 的广播 (其只覆盖启动期已存在的 BSP 实例).
        // KPTI 未激活时用当前 CR3 初始化.
        if crate::privileged::mm::kpti::kpti_is_active() {
            let bsp = per_cpu_gdt(0);
            ap.syscall.kernel_pml4 = bsp.syscall.kernel_pml4;
            ap.syscall.user_pml4 = bsp.syscall.user_pml4;
        } else {
            let current_cr3: u64;
            core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack));
            ap.syscall.kernel_pml4 = current_cr3;
            ap.syscall.user_pml4 = current_cr3;
        }

        // 内核态 GS 基址约定与 BSP `gdt_init` 完全一致 (见该处注释):
        //   内核态: IA32_GS_BASE = per_cpu_addr, IA32_KERNEL_GS_BASE = 0
        //   用户态: IA32_GS_BASE = 0,            IA32_KERNEL_GS_BASE = per_cpu_addr
        // 此前本函数只写 IA32_KERNEL_GS_BASE, 使 AP 内核态 IA32_GS_BASE 保持
        // `mov gs` 遗留的 0 ⇒ `process_switch_asm` 的 `[gs:TRAMPOLINE_TOP_OFF]`
        // 读到线性地址 0x20 的垃圾值, 用户态出口栈错乱 (首次上下文切换即
        // #PF → #DF); 且用户态 syscall 的 swapgs 也拿不到 per-CPU 基址。
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const IA32_GS_BASE: u32 = 0xC0000101;
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const IA32_KERNEL_GS_BASE: u32 = 0xC0000102;
        crate::privileged::cpu::msr::write_msr(IA32_GS_BASE, &ap.syscall as *const _ as u64);
        crate::privileged::cpu::msr::write_msr(IA32_KERNEL_GS_BASE, 0);
    }
}

/// 获取指定 CPU 的 `PerCpuGdt` **头区** [start, end) 低半区线性地址 (LMA)。
///
/// 头区 = `entries` + `ptr` + `tss` + `syscall`, 是 KPTI 用户页表在中断交付
/// 路径上必须可达的全部 GDT 侧对象: CPU 读 IDT 门目标 CS 的描述符 (GDT entries)、
/// RSP0/IST 栈指针 (TSS), swapgs 后读内核 PML4 (`SyscallPerCpu`)。
///
/// 这里**不**返回单个"基址页": `SyscallPerCpu` 起始于结构体偏移 0x620 处, 若
/// `PerCpuGdt` 基址非页对齐, 头区会跨两页 —— 故返回区间, 由调用方按页逐页映射。
///
/// 调用方须同时映射 `KERNEL_BASE + 区间` (VMA) 与区间本身 (LMA 恒等): `GS_BASE`
/// 直接取自 `&gdt.syscall` 的 LMA (见 `gdt_init` 的 `IA32_GS_BASE` 写入), 而
/// 高半区访问走 VMA。
pub fn per_cpu_gdt_head_range(cpu: u32) -> (u64, u64) {
    let g = per_cpu_gdt(cpu);
    let start = core::ptr::from_ref(g) as u64;
    let end = core::ptr::from_ref(&g.syscall) as u64 + core::mem::size_of::<SyscallPerCpu>() as u64;
    (start, end)
}

/// 获取指定 CPU 的 IST0..3 栈顶 VA (高半区, 页对齐)。
///
/// 供 KPTI 逐页映射其**栈顶页**: 用户态异常交付时 CPU 在切内核页表之前就用
/// `TSS.ist[N-1]` 压入异常帧。由**静态布局推导** (而非读 `TSS.ist[]`), 使
/// `kpti_init` 能在 `gdt_init` 之前调用本函数 —— 那时 `TSS.ist[]` 仍为 0。
/// 公式与写入 `TSS.ist[]` 的 `init_stack_tops` 同源 (见 `ist_tops_of`)。
pub fn ist_tops_virt(cpu: u32) -> [u64; 4] {
    ist_tops_of(per_cpu_gdt(cpu))
}

/// 获取指定 CPU 的 KPTI trampoline 栈顶 VA (高半区, 页对齐)。
///
/// 汇编出口 (`process_switch_asm`) 在 swapgs 前经 `[gs:TRAMPOLINE_TOP_OFF]` 读取
/// `SyscallPerCpu.trampoline_top`, 而本函数由静态布局推导同一值 (见
/// `trampoline_top_of`), 故"映射的页"与"汇编使用的栈"同源, 且可在 `gdt_init`
/// 之前调用。
pub fn trampoline_top_virt(cpu: u32) -> u64 {
    trampoline_top_of(per_cpu_gdt(cpu))
}

/// 获取 GDT 表的引用 (调试用途)
///
/// # Safety
/// 返回的引用在整个程序生命周期有效。
#[inline]
pub unsafe fn get_gdt_table() -> &'static [GdtEntry; GDT_MAX_ENTRIES] {
    &current_per_cpu_gdt().entries
}

/// 获取 BSP 的 GDT 指针 (用于 AP trampoline 过渡阶段)
///
/// Trampoline 在 16→64 过渡时使用此 GDT 加载段寄存器，
/// 进入长模式后 AP 会切换到自己的 per-CPU GDT。
pub fn get_gdt_ptr() -> &'static GdtPtr {
    &per_cpu_gdt(0).ptr
}

/// 获取当前 CPU 的 TSS 可变引用 (用于设置栈指针等)
///
/// # Safety
/// 应该在初始化后调用, 且注意并发安全。
#[inline]
pub unsafe fn get_tss_mut() -> &'static mut super::tss::TaskStateSegment {
    &mut current_per_cpu_gdt_mut().tss
}

/// 更新指定 CPU 的 KPTI PML4 字段
///
/// KPTI init 完成后调用, 设置 `kernel_pml4` 和 `user_pml4`.
/// PCID 启用时, 值为 `PML4_PHYS` | PCID; 未启用时为纯 PML4 物理地址.
/// KPTI 未激活时 `user_pml4 == kernel_pml4`, 汇编中 `mov cr3` 无实际切换效果.
///
/// # Safety
///
/// 调用方保证: `cpu_index` 合法; 仅在 boot 阶段或所有 CPU 已停止调度时调用.
pub unsafe fn gdt_set_kpti_pml4(cpu_index: u32, kernel_pml4: u64, user_pml4: u64) {
    let gdt = per_cpu_gdt_mut(cpu_index);
    gdt.syscall.kernel_pml4 = kernel_pml4;
    gdt.syscall.user_pml4 = user_pml4;
}

/// 更新当前 CPU 的 per-CPU 用户页表 CR3 值 (`gs:USER_PML4_OFF`).
///
/// 每个用户进程有独立的用户页表, syscall/中断返回用户态时
/// 从 `gs:USER_PML4_OFF` 读取 CR3, 因此必须在进入用户态前
/// 将当前进程的用户页表物理地址写入该字段.
///
/// # Safety
///
/// 调用方保证: `user_cr3` 是有效的用户页表 PML4 物理地址;
/// 仅在当前 CPU 独占访问时调用 (中断上下文或调度器持锁).
#[inline]
pub unsafe fn gdt_set_user_cr3(user_cr3: u64) {
    let gdt = current_per_cpu_gdt_mut();
    gdt.syscall.user_pml4 = user_cr3;
}

/// 更新当前 CPU 的 syscall 入口内核栈顶 (`gs:KERNEL_RSP_OFF`).
///
/// 该字段是 `syscall` 指令入口读取内核栈顶的唯一来源 (见 `boot/isr.asm`)。
/// 契约 (D5 统一内核栈契约): 恒为**当前任务内核栈的高半区栈顶 VA**
/// (`Process::allocate_kernel_stack` 写入的 `phys + KERNEL_BASE + KERNEL_STACK_SIZE`),
/// 与 `TSS.RSP0` 同值 —— 二者由 `cpu::arch::set_kernel_stack` 一并更新。
///
/// 若只更新 `TSS.RSP0` 而漏掉本字段, syscall 会落到每 CPU 共享的
/// `syscall_stack`: 任务在 syscall 中被让出后, 其内核栈帧会被同一核上后续
/// 任务的 syscall 覆盖, 恢复时局部量与返回地址失真.
#[inline]
pub fn gdt_set_kernel_rsp(rsp: u64) {
    current_per_cpu_gdt_mut().syscall.kernel_rsp = rsp;
}

// ============================================================================
// 内联汇编封装 (硬件指令)
// ============================================================================

/// 执行 LGDT 指令 (Load Global Descriptor Table Register)
///
/// # Arguments
/// * `gdt_ptr` - 指向 `GdtPtr` 结构体的引用
///
/// # Safety
/// 此函数修改 GDTR 寄存器, 会立即影响内存分段行为。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
unsafe fn gdt_flush(gdt_ptr: &GdtPtr) {
    unsafe {
        core::arch::asm!(
            "lgdt [{0}]",
            in(reg) gdt_ptr,
            options(nostack, preserves_flags),
        );
    }
}

/// 执行 LTR 指令 (Load Task Register)
///
/// # Arguments
/// * `selector` - TSS 选择子 (如 0x28)
///
/// # Safety
/// 此函数加载新的 TSS 到任务寄存器, 会标记 TSS 为 busy。
#[inline(always)]
unsafe fn tss_flush(selector: u16) {
    unsafe {
        core::arch::asm!(
            "ltr {0:x}",
            in(reg) selector,
            options(nostack, preserves_flags),
        );
    }
}

/// 将 CS 重载为内核代码段选择子 (0x08)
///
/// lgdt 仅更新 GDTR, 不重载 CS; CPU 会继续使用 CS 缓存的旧描述符, 直到
/// 下一次 CS 装载才按新 GDT 校验. AP 经 trampoline 进入长模式时 CS=0x18,
/// 该值在 trampoline GDT 中是 64-bit 代码段, 但在内核 per-CPU GDT 中
/// entry[3] (0x18) 是 user_data 数据段, 故装载内核 GDT 后必须重载 CS.
///
/// 实现: far return 重载 CS — 依次压入选择子与返回 RIP, `retfq` 先弹 RIP
/// 再弹 CS. 返回 RIP 用 `lea [rip + 2f]` 取运行时地址 (AP 经 trampoline
/// 运行时 RIP 位于低地址别名, 写死 link-time 地址会跳错).
///
/// # Safety
/// 调用方保证当前 GDTR 已加载, 且 `SELECTOR_KERNEL_CODE` 在其中是合法的
/// 64-bit 代码段; 本函数用 far return 改写 CS, 不改变栈深度.
unsafe fn reload_cs() {
    // SAFETY: 压入的 0x08 在当前 GDTR 中为合法 64-bit 代码段, 返回地址由
    // `lea [rip + 2f]` 取运行时地址并指向本汇编块内紧随其后的标签 `2`;
    // 栈上净效果为 push/push + retfq 弹两次, 深度不变.
    unsafe {
        core::arch::asm!(
            "push {sel}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) u64::from(SELECTOR_KERNEL_CODE),
            tmp = lateout(reg) _,
        );
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gdt_entry_null() {
        let null_desc = GdtEntry::null();
        // SAFETY: `const` 由调用方保证为有效指针; 只读访问
        let bytes = unsafe { core::ptr::read_volatile(&null_desc as *const _ as *const u64) };
        assert_eq!(bytes, 0, "Null descriptor should be all zeros");
    }

    #[test]
    fn test_access_byte_constants() {
        assert_eq!(AccessByte::kernel_code().0, 0x9A);
        assert_eq!(AccessByte::kernel_data().0, 0x92);
        assert_eq!(AccessByte::user_code().0, 0xFA);
        assert_eq!(AccessByte::user_data().0, 0xF2);
        assert_eq!(AccessByte::tss().0, 0x89);
    }

    #[test]
    fn test_granularity_constants() {
        let code_gran = Granularity::code_64bit();
        assert!(code_gran.0 & Granularity::PAGE_GRANULARITY != 0);
        assert!(code_gran.0 & Granularity::LONG_MODE != 0);

        let data_gran = Granularity::data_32bit();
        assert!(data_gran.0 & Granularity::PAGE_GRANULARITY != 0);
        assert!(data_gran.0 & Granularity::SIZE_32BIT != 0);
    }

    #[test]
    fn test_selector_values() {
        assert_eq!(SELECTOR_NULL, 0x00);
        assert_eq!(SELECTOR_KERNEL_CODE, 0x08);
        assert_eq!(SELECTOR_KERNEL_DATA, 0x10);
        assert_eq!(SELECTOR_USER_DATA, 0x18);
        assert_eq!(SELECTOR_USER_CODE, 0x20);
        assert_eq!(SELECTOR_TSS, 0x28);
    }
}
