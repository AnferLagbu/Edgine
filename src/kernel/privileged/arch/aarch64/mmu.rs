//! `AArch64` MMU / 页表管理
//!
//! 双地址空间:
//!   - `TTBR0_EL1`: 用户空间 (`0x0000_0000_0000_0000` - `0x0000_FFFF_FFFF_FFFF`)
//!   - `TTBR1_EL1`: 内核空间 (`0xFFFF_0000_0000_0000` - `0xFFFF_FFFF_FFFF_FFFF`)
//!
//! 使用 4KB 页粒度, 48-bit VA, 4-level 页表 (T0SZ=T1SZ=16 ⇒ 硬件从 level 0 起遍历)。
//!
//! QEMU virt 内存布局:
//!   - DRAM: 0x40000000 - ?
//!   - UART: 0x09000000
//!   - GIC:  0x08000000

use core::ptr;

use crate::privileged::mm::virt_to_phys;

// ============================================================================
// 页表常量 (ARMv8-A 4KB granule)
// ============================================================================

/// 页表条目常量
const PT_TYPE_TABLE: u64 = 0x3; // valid + table descriptor
const PT_TYPE_BLOCK: u64 = 0x1; // 有效位 + 块描述符 (block descriptor)
const PT_AF: u64 = 1 << 10; // Access Flag
const PT_ATTR_NORMAL: u64 = (0b0100 << 2) | (0b0100 << 8); // 普通内存, 内/外 WBWA 缓存策略
const PT_ATTR_DEVICE: u64 = 0; // Device-nGnRnE memory (AttrIndx=0, MAIR[0]=0x44)
const PT_AP_EL1_RW: u64 = 0 << 6; // EL1 read/write
const PT_AP_ALL_RW: u64 = 1 << 6; // EL1+EL0 read/write

const L2_BLOCK_SIZE: u64 = 0x20_0000; // 2MB (4KB 粒度下的 L2 块)

// ============================================================================
// 页表 (static, BSS)
// ============================================================================

/// 4KB 对齐的页表数组 (ARM 要求页表 4KB 对齐)
#[repr(align(4096))]
pub struct AlignedPageTable([u64; 512]);

/// L0 页表 (512 entries × 8 bytes = 4KB)
///
/// # Safety 不变量
///
/// - **写入时机**: 仅在 `init()` 函数中写入 (启动最早期, MMU 启用前)
/// - **运行时**: MMU 启用后硬件直接使用, 软件不可写入
/// - **并发**: 写入时系统单线程 (AP 未启动), 无竞争
/// - **对齐**: 4KB 对齐 (ARM MMU 硬件要求)
static mut L0_TABLE: AlignedPageTable = AlignedPageTable([0; 512]);

/// L1 页表 (512 entries × 8 bytes = 4KB), 覆盖 0-2GB (L1 块描述符, 每项 1GB)
///
/// # Safety 不变量
///
/// - **写入时机**: 仅在 `init()` 函数中写入 (启动最早期, MMU 启用前)
/// - **运行时**: MMU 启用后硬件直接使用, 软件不可写入
/// - **并发**: 写入时系统单线程 (AP 未启动), 无竞争
/// - **对齐**: 4KB 对齐 (ARM MMU 硬件要求)
static mut L1_IDMAP: AlignedPageTable = AlignedPageTable([0; 512]);

/// L2 页表 (512 entries × 8 bytes = 4KB), 覆盖 0-1GB (L2 块描述符, 每项 2MB)
/// 用于以 Device memory 属性映射 GIC/UART 等 MMIO 区域
///
/// # Safety 不变量
///
/// - **写入时机**: 仅在 `init()` 函数中写入 (启动最早期, MMU 启用前)
/// - **运行时**: MMU 启用后硬件直接使用, 软件不可写入
/// - **并发**: 写入时系统单线程 (AP 未启动), 无竞争
/// - **对齐**: 4KB 对齐 (ARM MMU 硬件要求)
static mut L2_DEVICE: AlignedPageTable = AlignedPageTable([0; 512]);

// === unsafe 依据单源 (本文件页表写入与翻译控制寄存器位点共用) ===
//
// 类别 T「静态引导页表写入」: `L0_TABLE` / `L1_IDMAP` / `L2_DEVICE` 是 BSS 中的
// `static mut AlignedPageTable`, `#[repr(align(4096))]` 满足硬件对页表 4KB 对齐的要求;
// 字段 `.0` 位于偏移 0 ⇒ `[u64; 512]` 恰 4096 字节, 故 `write_bytes(..., 0, 512)` 与
// `iter_mut()` 都落在该数组内, 无越界面. 唯一写者是 `init()`: 它在 MMU 启用前、单核
// 阶段执行 (次核要到 `interrupt_late_init` / `secondary_cpu_bringup` 才由 `smp_init::init`
// 拉起), 因此既无并发写, 也无硬件 walker 与软件写同时消费同一张表; `init_kernel_ttbr1`
// 与 `alloc_user_page_table` 只经 `&raw const` 读. 取址一律 `&raw mut` / `&raw const`
// 而非 `&` / `&mut`, 避免对 `static mut` 构造引用 (`static_mut_refs`). 表内条目存的是
// **下一级表的物理地址** (硬件按 PA 遍历), 而三张表链接于高半区 (VMA = PA + KERNEL_BASE,
// aarch64 的 KERNEL_BASE = 0xFFFF_0000_0000_0000), 故每个表地址都必须过 `mm::virt_to_phys`
// —— 它是纯 `virt - KERNEL_BASE` 减法, 不查页表, 因而能在 MMU 尚未启用时使用, 但只对
// 真正链接在高半区的地址成立.
//
// 类别 R「EL1 翻译控制寄存器 MSR + ISB」: `msr ttbr0_el1 / ttbr1_el1 / tcr_el1 /
// mair_el1 / sctlr_el1, Xt` 都是 EL1 系统寄存器 (EL0 访问会陷入), 操作数只有 `in(reg)` /
// `out(reg)` ⇒ 无指针解引用面; 未声明 `options(...)` 属保守 (编译器假定可能访存), 不构成
// UB. 每次写后必须 `isb`: 翻译控制类寄存器的写入需要上下文同步事件才对后续取指/访存生效,
// 否则流水线可能继续按旧翻译体制执行. 值的合法性由调用方承担 —— TTBR 必须是 4KB 对齐的
// 表物理地址且该表已填好, TCR/MAIR 的字段编码必须与表的粒度/层级一致.
//
// 类别 B「屏障 + TLBI 序列」: 前置 `dsb sy` 等待此前所有显式访存 (含页表条目写入) 完成,
// `tlbi vmalle1` 失效当前 VMID 下 EL1&0 的全部 stage-1 TLB 条目, 后置 `dsb sy` 等待失效
// 完成, `isb` 同步上下文使后续访问重新走表. 这些指令本身无内存操作数, 但会改变翻译缓存,
// 故次序不可省: 缺前置 DSB 会让 walker 读到陈旧表项, 缺后置 DSB/ISB 会让失效尚未生效.

// ============================================================================
// MMU 初始化
// ============================================================================

/// 初始化 identity mapping (覆盖 0-2GB) 并启用 MMU。
///
/// 同时设置 `TTBR1_EL1` 覆盖内核高地址空间 (2GB)。
///
/// 页表层级 (4KB 粒度, 48-bit VA, 4-level):
///   L0`[0]` → `L1_IDMAP`
///   
/// L1`[0]` → `L2_DEVICE` (2MB 粒度, 全部 Device memory — QEMU virt 低 1GB 无 DRAM)
/// L1`[1]` → 1GB 块: PA 0x40000000 (DRAM, kernel @ 0x40080000, Normal 内存)
///
/// 所有映射均为 EL1 RW。
///
/// # Safety
///
/// 调用前需确保：
/// - 运行在 EL1 或更高特权级
/// - 页表所在物理内存 (BSS) 可访问
/// - 在设置 TTBR0/TTBR1 前已确保旧映射一致性
pub unsafe fn init() {
    // SAFETY: 类别 T + R + B. 本块是引导页表的唯一写入点, 由 `boot/aarch64/entry.rs` 的
    // `entry` 第 2 步调用 (在 UART 之前 —— UART 走高半区别名, MMU 未启用时该地址不可达),
    // 此时单核、MMU 关闭、三张表尚无硬件消费者. 映射形状: L0[0] → L1_IDMAP; L1[0] →
    // L2_DEVICE 的 512 个 2MB **Device** 块覆盖 [0, 0x4000_0000) —— QEMU virt 低 1GB 无
    // DRAM (GIC@0x0800_0000 / UART@0x0900_0000), 以 Normal cacheable 属性访问 MMIO 会
    // 数据异常/挂死; L1[1] 是 PA 0x4000_0000 起的 1GB Normal 块, 覆盖 DRAM (内核
    // @0x4008_0000). 全部条目 `PT_AP_EL1_RW` ⇒ EL0 经本表不可访问. TCR: T0SZ=T1SZ=16 ⇒
    // 两半区都是 48-bit VA, 硬件**从 level 0 起**遍历 (故 TTBR1 也必须是 4 级根, 见
    // `init_kernel_ttbr1`); TG1=0b10 为 4KB 粒度, TG0=00 是复位值故不出现在或运算中.
    // MAIR: Attr0=0x44 (Device-nGnRnE) 对应 `PT_ATTR_DEVICE` 的 AttrIndx=0; Attr4=0xFF
    // (Normal WBWA) 对应 `PT_ATTR_NORMAL` = (0b0100<<2)|(0b0100<<8) 的 AttrIndx=4;
    // Attr1=0xFF 由 KPTI trampoline 表用于 `.vectors` 与全局量页. 次序: 表写完 →
    // dsb/isb → tlbi → dsb/isb → `enable_mmu` → `init_kernel_ttbr1`.
    unsafe {
        // 清零页表 (经 &raw mut 取裸指针, 避免对 static mut 取引用触发 static_mut_refs)
        ptr::write_bytes((&raw mut L0_TABLE.0).cast::<u64>(), 0, 512);
        ptr::write_bytes((&raw mut L1_IDMAP.0).cast::<u64>(), 0, 512);
        ptr::write_bytes((&raw mut L2_DEVICE.0).cast::<u64>(), 0, 512);

        // 表内条目存的是**下一级表的物理地址** (硬件按 PA 遍历), 而三张 static
        // 表链接于高半区 (VMA = PA + KERNEL_BASE), 故必须经 virt_to_phys 转换。
        // L0[0] → L1_IDMAP (L0 每项覆盖 512GB, 这里只需一个)
        L0_TABLE.0[0] = virt_to_phys((&raw const L1_IDMAP.0).cast::<u64>() as u64) | PT_TYPE_TABLE;

        // L1[0] → L2_DEVICE: 0-1GB 用 L2 2MB 块映射, Device memory 属性
        //   因为 QEMU virt 低 1GB 无 DRAM (只有 GIC@0x08000000, UART@0x09000000),
        //   以 Normal cacheable 属性访问 MMIO 会导致数据异常/挂死。
        L1_IDMAP.0[0] = virt_to_phys((&raw const L2_DEVICE.0).cast::<u64>() as u64) | PT_TYPE_TABLE;

        // L2_DEVICE: 512 个 2MB Device 块, 覆盖 0x00000000 - 0x40000000
        // (经 &raw mut 取裸指针后解引用, 避免对 static mut 取引用触发 static_mut_refs)
        let l2_device = &raw mut L2_DEVICE.0;
        for (i, entry) in (*l2_device).iter_mut().enumerate() {
            let pa = (i as u64) * L2_BLOCK_SIZE;
            *entry = pa | PT_TYPE_BLOCK | PT_AF | PT_ATTR_DEVICE | PT_AP_EL1_RW;
        }

        // L1[1]: VA 1-2GB → PA 1-2GB (DRAM, kernel @ 0x40080000, Normal 内存)
        L1_IDMAP.0[1] = 0x4000_0000 | PT_TYPE_BLOCK | PT_AF | PT_ATTR_NORMAL | PT_AP_EL1_RW;

        // 设置 TTBR0_EL1 (TTBR 承载表物理地址)
        set_ttbr0(virt_to_phys((&raw const L0_TABLE.0).cast::<u64>() as u64));

        // 设置 TCR_EL1 (Translation Control Register)
        // T0SZ=16 (TTBR0 用 48-bit IPA), T1SZ=16 (TTBR1 用 48-bit IPA)
        // 4KB 颗粒 (TG0=00, TG1=10, TG0 为零值故不出现在或运算中), 内部共享, Normal 可缓存
        let tcr: u64 = 16u64 // T0SZ: 64 - 48 = 16
                  | (16u64 << 16)   // T1SZ: 64 - 48 = 16
                  | (0b10 << 30)    // TG1: 4KB
                  | (0b11 << 12)    // SH0: Inner Shareable
                  | (0b11 << 28)    // SH1: Inner Shareable
                  | (0b01 << 10)    // ORGN0: Normal, WB, RA, WA
                  | (0b01 << 26)    // ORGN1: Normal, WB, RA, WA
                  | (0b01 << 8)     // IRGN0: Normal, WB, RA, WA
                  | (0b01 << 24); // IRGN1: Normal, WB, RA, WA
        set_tcr(tcr);

        // 设置 MAIR_EL1 (Memory Attribute Indirection Register)
        // 定义: PT_ATTR_NORMAL = (0b0100<<2)|(0b0100<<8) → AttrIndx=4
        // 定义: PT_ATTR_DEVICE = (0b0000<<2)|(0b0000<<8) → AttrIndx=0
        // MAIR[0] = 0x44 (Device-nGnRnE, 对应 PT_ATTR_DEVICE)
        // MAIR[1] = 0xFF (Normal IWBWA OWBWA, 对应 vmm_aarch64::MAIR_NORMAL_WBWA):
        //   KPTI trampoline 表以该索引映射 `.vectors` (需可取指) 与 KPTI 全局量页,
        //   未定义索引 (0x00) 会被硬件按 Device-nGnRnE 解释.
        // MAIR[4] = 0xFF (Normal IWBWA OWBWA, 对应 PT_ATTR_NORMAL)
        let mair: u64 = 0x44                     // Attr0: Device
                   | (0xFFu64 << 8)          // Attr1: Normal
                   | (0xFFu64 << 32); // Attr4: Normal
        set_mair(mair);

        // 使之前的所有页表写入和系统寄存器设置可见
        core::arch::asm!("dsb sy");
        core::arch::asm!("isb");

        // 清空 TLB (确保旧映射不影响新表)
        core::arch::asm!("tlbi vmalle1");
        core::arch::asm!("dsb sy");
        core::arch::asm!("isb");

        // 启用 MMU
        enable_mmu();

        // 设置 TTBR1 内核页表 (映射高地址 → 物理地址)
        init_kernel_ttbr1();
    }
}

/// 初始化 `TTBR1_EL1` 内核页表。
///
/// TTBR1 覆盖高半区 VA `0xFFFF_0000_0000_0000 - 0xFFFF_FFFF_FFFF_FFFF`。
/// TCR 中 T1SZ=16 + TG1=4KB ⇒ 高半区 VA 宽 48-bit ⇒ 硬件**从 level 0 开始**遍历,
/// 因而 `TTBR1_EL1` 必须指向 4 级根表。
///
/// 本函数复用 TTBR0 使用的同一 4 级根 [`L0_TABLE`], 依据是:
/// 高半区别名 `VA = 0xFFFF_0000_0000_0000 + PA` 把 PA 放在 VA`[38:0]`,
/// VA`[47:39]` 恒为 0 ⇒ L0 索引恒为 0, 而 L1/L2/L3 索引与恒等映射 `VA = PA`
/// 完全相同。故别名与恒等走同一条遍历路径, 得到:
///   - `0xFFFF_0000_0000_0000 - 0xFFFF_0000_4000_0000` → 低 1GB MMIO (Device, 2MB 粒度)
///   - `0xFFFF_0000_4000_0000 - 0xFFFF_0000_8000_0000` → DRAM 1GB 块 (Normal;
///     含内核代码/数据/BSS/内核栈/异常向量表)
///
/// 历史: 旧实现基于「T1SZ=16 时硬件从 level 1 开始遍历」的错误认知, 令 `TTBR1_EL1`
/// 直接指向一张 L1 形态表 (仅填 `[0]`/`[1]`)。实测 (`AT S1E1R` + 根表转储) 表明
/// 该表被硬件当作 L0 消费: 高半区 DRAM 别名的物理基址被截断为 0
/// (`0xFFFF_0000_401B_F000` 解析到 PA `0x1BF000` 而非 `0x401BF000`), 导致
/// `VBAR_EL1` 指向的异常向量表不可取指 —— 内核从未进入过任何异常/中断处理
/// (串口日志中 `TIMER IRQ` 恒为 0 次)。归因与验证见
/// docs/plan/archive/kpti-complete-project.md。
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn init_kernel_ttbr1() {
    // SAFETY: 类别 T (只读 `&raw const`) + R + B. 与 TTBR0 共用同一张 4 级根: 高半区
    // 别名 VA = KERNEL_BASE + PA 把 PA 放在 VA[38:0], VA[47:39] 恒 0 ⇒ L0 索引恒为 0,
    // L1/L2/L3 索引与恒等映射完全相同, 故别名与恒等走同一条遍历路径. 本块的 TLBI 不与
    // `init()` 末尾那次重复: 从 `enable_mmu` 到本次 MSR 之间, CPU 可能以 TTBR1_EL1 旧值
    // 0 投机翻译高半区地址并缓存失败条目, 不失效就会在后续高半区访问命中过期条目而
    // Translation fault. `set_ttbr1` 自带 isb, 此处再补 dsb/tlbi/dsb/isb 覆盖该窗口.
    unsafe {
        // 与 TTBR0 共用 4 级根: 高半区别名与恒等映射的各级索引一致, 无需独立表。
        // TTBR 承载表物理地址, 故经 virt_to_phys 转换 (表链接于高半区)。
        set_ttbr1(virt_to_phys((&raw const L0_TABLE.0).cast::<u64>() as u64));

        // 刷新 TLB: MMU 启用后到 TTBR1_EL1 设置前的窗口期,
        // CPU 可能投机翻译 TTBR1 地址 (TTBR1_EL1 旧值为 0),
        // 缓存翻译失败条目。后续访问 TTBR1 地址会命中这些过期条目
        // 导致 Translation fault。
        // DSB 在 TLBI 前确保页表写入对 MMU walker 可见.
        core::arch::asm!("dsb sy", "tlbi vmalle1", "dsb sy", "isb");
    }
}

/// 分配用户空间页表 (返回 TTBR0 值)。
///
/// 为简单实现, 使用 BSS 静态页表。
/// Phase 6: 每进程独立页表。
#[must_use]
pub fn alloc_user_page_table() -> u64 {
    // 返回当前 identity mapping 的 TTBR0
    // Phase 6+: 实现每进程独立页表分配
    // 返回值写入 TTBR0_EL1, 必须是表物理地址 (表链接于高半区, 故经 virt_to_phys 转换)。
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe { virt_to_phys((&raw const L0_TABLE.0).cast::<u64>() as u64) }
}

// ============================================================================
// 系统寄存器读写 (inline asm)
// ============================================================================

#[inline]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn set_ttbr0(val: u64) {
    // SAFETY: 类别 R. 写低半区根表基址; 唯一调用方 `init()` 传入
    // `virt_to_phys(&raw const L0_TABLE.0)` —— 4KB 对齐的 4 级表物理地址, 且该表在本块
    // 之前已填好并经过 dsb/tlbi. 写入值高 16 位为 0 (TCR_EL1.A1 未置位 ⇒ ASID 取自
    // TTBR0_EL1[63:48], 此处即 ASID 0). 运行期每进程根表切换不经本 setter: KPTI
    // trampoline 与 `MmuArch::write_page_table_base` 各有自己的 asm.
    unsafe {
        core::arch::asm!("msr ttbr0_el1, {}", in(reg) val);
        core::arch::asm!("isb");
    }
}

#[inline]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn set_ttbr1(val: u64) {
    // SAFETY: 类别 R. 写高半区根表基址; 唯一调用方 `init_kernel_ttbr1()` 传入与 TTBR0
    // 相同的 `L0_TABLE` 物理地址 (共用依据见该函数). T1SZ=16 ⇒ 硬件从 level 0 起遍历,
    // 故这里必须是 4 级根而非 L1 形态表 (历史缺陷见该函数文档段).
    unsafe {
        core::arch::asm!("msr ttbr1_el1, {}", in(reg) val);
        core::arch::asm!("isb");
    }
}

#[inline]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn set_tcr(val: u64) {
    // SAFETY: 类别 R. 写翻译控制寄存器; 唯一调用方 `init()` 传入的 tcr 与本文件页表形状
    // 绑定: T0SZ/T1SZ=16 (48-bit VA, 4 级) + TG1=4KB + SH0/SH1=Inner Shareable +
    // ORGN0/1 与 IRGN0/1=Normal WBWA. 改这些字段而不改表结构会让 walker 按错误层级/粒度
    // 消费表; 本 setter 不校验 val, 一致性由调用方保证.
    unsafe {
        core::arch::asm!("msr tcr_el1, {}", in(reg) val);
        core::arch::asm!("isb");
    }
}

#[inline]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn set_mair(val: u64) {
    // SAFETY: 类别 R. 写内存属性间接寄存器; 唯一调用方 `init()` 传入的 mair 定义了页表
    // 条目 AttrIndx 所索引的属性槽 (0=Device 0x44, 1=Normal 0xFF 供 KPTI trampoline,
    // 4=Normal 0xFF). 未写入的槽保持 0x00, 硬件按 Device-nGnRnE 解释, 故新增 AttrIndx
    // 必须同批扩 mair, 否则该索引的映射属性静默变成 Device.
    unsafe {
        core::arch::asm!("msr mair_el1, {}", in(reg) val);
        core::arch::asm!("isb");
    }
}

/// 启用 MMU: 设置 `SCTLR_EL1.M` (bit 0) + C (bit 2) + I (bit 12)
/// P3.B + F-11: 同时启用 D-cache 与 I-cache. ARM ARM 强烈建议启用 MMU 时
/// 同时启用 C/I cache 以避免 speculative 访问绕过 MMU (CVE 防御).
/// 历史版本注释 "暂不启用缓存, 后续单独处理" — 本次落地一并启用.
#[inline(never)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn enable_mmu() {
    // SAFETY: 类别 R + B. 读-改-写 SCTLR_EL1 的 M(bit0) / C(bit2) / I(bit12) 三位:
    // 前置 dsb 保证页表与 TCR/MAIR/TTBR 写入对 walker 可见, 后置 isb 使新翻译体制对后续
    // 取指生效 (ARM ARM D5.10.2). 三位一并置起是 P3.B/F-11 的要求 —— 按 ARM ARM 建议,
    // 启用 MMU 时同时启用 C/I 以避免投机访问绕过 MMU. `out("x0") _` 显式声明 x0 被本 asm
    // 破坏, 编译器因此不会把活跃值分配进 x0; 1 / 4 / (1<<12) 分三步 orr 是因为 LLVM 内联
    // 汇编不接受复杂立即数表达式. 唯一调用方是 `init()`, 在表与 TCR/MAIR 就位之后.
    unsafe {
        // ARM ARM D5.10.2: 启用 MMU 前需要 DSB 确保所有之前操作可见,
        // 启用后需要 ISB 使 MMU 对后续指令生效.
        // bit 0 = M (MMU enable), bit 2 = C (D-cache), bit 12 = I (I-cache).
        // 立即数值 1 | 4 | 4096 = 4101; LLVM inline asm 不支持复杂表达式,
        // 故展开为 3 步 orr (MOV + ORR 链). 修复 F-11.
        core::arch::asm!(
            "dsb sy",
            "mrs x0, sctlr_el1",
            "orr x0, x0, #1",          // M bit
            "orr x0, x0, #4",          // C bit
            "orr x0, x0, #(1 << 12)",  // I bit (4096)
            "msr sctlr_el1, x0",
            "isb",
            out("x0") _,
        );
    }
}

// ============================================================================
// Arch trait 辅助函数 (供 Arch impl 调用)
// ============================================================================

/// 读取 `TTBR0_EL1` (低半区页表基址寄存器)。
#[inline]
#[must_use]
pub fn read_ttbr0() -> u64 {
    let val: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mrs {}, ttbr0_el1", out(reg) val);
    }
    val
}

/// 读取 `TTBR1_EL1` (高半区页表基址寄存器)
#[inline]
#[must_use]
pub fn read_ttbr1() -> u64 {
    let val: u64;
    // SAFETY: mrs ttbr1_el1 是只读系统寄存器读取, 无副作用.
    unsafe {
        core::arch::asm!("mrs {}, ttbr1_el1", out(reg) val);
    }
    val
}

/// 写入 `TTBR0_EL1` 并执行 ISB 使改动生效。
#[inline]
pub fn write_ttbr0(val: u64) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("msr ttbr0_el1, {}", in(reg) val);
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("isb", options(nomem, nostack));
    }
}

/// 按虚拟地址刷新 EL1 的 TLB 条目 (tlbi vaae1)。
#[inline]
pub fn tlbi_vaae1(vaddr: u64) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("dsb ishst", options(nomem, nostack));
        core::arch::asm!("tlbi vaae1, {}", in(reg) (vaddr >> 12));
        core::arch::asm!("dsb ish", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }
}

/// 刷新 EL1 的整个 TLB (tlbi vmalle1)。
#[inline]
pub fn tlbi_vmalle1() {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("dsb ishst", options(nomem, nostack));
        core::arch::asm!("tlbi vmalle1", options(nomem, nostack));
        core::arch::asm!("dsb ish", options(nomem, nostack));
        core::arch::asm!("isb", options(nomem, nostack));
    }
}

/// 读取 `FAR_EL1` (触发页错误的故障地址寄存器)。
#[inline]
#[must_use]
pub fn read_far() -> u64 {
    let val: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mrs {}, far_el1", out(reg) val);
    }
    val
}

/// 读取 `MAIR_EL1` (内存属性间接寄存器)
#[inline]
#[must_use]
pub fn read_mair() -> u64 {
    let val: u64;
    // SAFETY: mrs mair_el1 是只读系统寄存器读取, 无副作用.
    unsafe {
        core::arch::asm!("mrs {}, mair_el1", out(reg) val);
    }
    val
}

/// 读取 `TCR_EL1` (地址翻译控制寄存器)
#[inline]
#[must_use]
pub fn read_tcr() -> u64 {
    let val: u64;
    // SAFETY: mrs tcr_el1 是只读系统寄存器读取, 无副作用.
    unsafe {
        core::arch::asm!("mrs {}, tcr_el1", out(reg) val);
    }
    val
}

/// 读取 `SCTLR_EL1` (系统控制寄存器, 含 MMU/Cache 使能位)
#[inline]
#[must_use]
pub fn read_sctlr() -> u64 {
    let val: u64;
    // SAFETY: mrs sctlr_el1 是只读系统寄存器读取, 无副作用.
    unsafe {
        core::arch::asm!("mrs {}, sctlr_el1", out(reg) val);
    }
    val
}

// ============================================================================
// 页表权限查询 API
// ============================================================================

/// 检查页表条目是否允许 EL0 (用户态) 访问
#[must_use]
pub fn allows_el0_access(descriptor: u64) -> bool {
    let ap = (descriptor >> 6) & 0x3;
    // AP[1] = 1 表示 EL0 也可访问
    ap & 0x2 != 0
}

/// 获取页表条目的访问权限描述
#[must_use]
pub fn describe_ap(descriptor: u64) -> &'static str {
    let ap = (descriptor >> 6) & 0x3;
    match ap {
        0b00 => "EL1 RW, EL0 none",
        0b01 => "EL1 RW, EL0 RW",
        0b10 => "EL1 RO, EL0 none",
        0b11 => "EL1 RO, EL0 RO",
        _ => "unknown",
    }
}

/// 创建用户态可读写的页表条目 (使用 `PT_AP_ALL_RW`)
#[must_use]
pub fn make_user_rw_entry(phys_addr: u64, attr: u64) -> u64 {
    phys_addr | PT_TYPE_BLOCK | PT_AF | attr | PT_AP_ALL_RW
}

/// 创建内核态可读写的页表条目 (使用 `PT_AP_EL1_RW`)
#[must_use]
pub fn make_kernel_rw_entry(phys_addr: u64, attr: u64) -> u64 {
    phys_addr | PT_TYPE_BLOCK | PT_AF | attr | PT_AP_EL1_RW
}

/// 创建用户态只读的页表条目
#[must_use]
pub fn make_user_ro_entry(phys_addr: u64, attr: u64) -> u64 {
    phys_addr | PT_TYPE_BLOCK | PT_AF | attr | 0x3 << 6 // AP_EL1_RO | AP_EL0_RO
}

/// 诊断页表条目权限
pub fn diagnose_permission(descriptor: u64, label: &str) {
    let ap = (descriptor >> 6) & 0x3;
    let af = (descriptor >> 10) & 0x1;
    let xn = (descriptor >> 54) & 0x1;

    crate::klog_ffi!(
        klog_ffi_info,
        "[MMU] {} desc=0x{:016x}: ap={} af={} xn={} ({})",
        label,
        descriptor,
        ap,
        af,
        xn,
        describe_ap(descriptor)
    );
}
