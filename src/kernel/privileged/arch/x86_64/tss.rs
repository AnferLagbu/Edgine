//! 任务状态段 (Task State Segment, TSS) - `x86_64` 实现
//!
//! ## 功能概览
//!
//! - **类型安全字段**: 每个寄存器都有明确的类型和文档
//! - **零初始化**: Default trait 保证所有字段为零
//! - **IA-32e 模式**: 仅包含 64-bit 长模式需要的字段
//!
//! ## TSS 在 x86-64 中的角色
//!
//! 在长模式下, TSS 主要用于:
//! 1. **保存 Ring 切换时的栈指针** (RSP0/RSP1/RSP2)
//! 2. **记录中断栈表 (IST) 地址**
//! 3. **I/O 位图** (可选, 用于 I/O 权限检查)
//!
//! **不再使用** (与 32-bit 不同):
//! - 不再保存所有寄存器 (由软件完成)
//! - 不支持硬件任务切换
//! - 无链接字段 (无任务嵌套)

// ============================================================================
// TSS 结构定义 (Intel SDM Vol. 3A Section 7.7)
// ============================================================================

/// 任务状态段 (Task State Segment)
///
/// 在 x86-64 长模式下, TSS 必须是 16-byte 对齐且大小 >= 104 bytes.
///
/// 内存布局 (IA-32e 模式):
/// ```text
/// Offset  Size  Field               Description
/// ------  ----- -------------------- ----------------------------------------
/// 0x00    2     Reserved            保留 (必须为 0)
/// 0x02    2     RSP0               特权级 0 的栈指针 (Ring 0 → Ring 3 时使用)
/// 0x04    2     RSP1               特权级 1 的栈指针 (未使用, 保留)
/// 0x06    2     RSP2               特权级 2 的栈指针 (未使用, 保留)
/// 0x08    2     Reserved            保留 (必须为 0)
/// 0x0A    2     IST1               中断栈表条目 1 (可选)
/// 0x0C    2     IST2               中断栈表条目 2
/// ...     ...    IST3-IST6          中断栈表条目 3-6
/// 0x14    2     IST7               中断栈表条目 7
/// 0x16    2     Reserved            保留
/// 0x18    4     Reserved            保留
/// 0x1C    4     Reserved            保留
/// 0x20    4     IOPB (I/O Permission Bitmap Base)  I/O 位图偏移量
/// ```
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct TaskStateSegment {
    /// 保留字段 (Offset 0x00, 4 bytes)
    /// 必须为 0, 供未来扩展使用
    reserved_0: u32,

    /// 特权级 0 栈指针 (Offset 0x04, 8 bytes)
    ///
    /// 当从 Ring 3 (用户态) 发生中断/异常/调用进入 Ring 0 (内核态) 时,
    /// CPU 自动将 RSP 设置为此值。
    /// **这是最重要的字段!** 必须指向有效的内核栈。
    pub rsp0: u64,

    /// 特权级 1 栈指针 (Offset 0x0C, 8 bytes)
    /// 在 x86-64 中未使用 (仅 0 和 3 有效), 但必须存在。
    pub(crate) rsp1: u64,

    /// 特权级 2 栈指针 (Offset 0x14, 8 bytes)
    /// 同上, 未使用但必须存在。
    pub(crate) rsp2: u64,

    /// 保留字段 (Offset 0x1C, 8 bytes)
    /// 必须为 0
    reserved_1: u64,

    /// 中断栈表 (Interrupt Stack Table, Offset 0x24)
    ///
    /// 7 个条目, 每个 8 字节, 用于特定中断向量的独立栈。
    /// IDT 中的 Gate Descriptor 可以指定使用哪个 IST 条目 (0-6)。
    ///
    /// 典型用法:
    /// - IST0: 双重故障 (#DF) 处理程序专用栈 (防止栈溢出死循环)
    /// - IST1-NMI: NMI (不可屏蔽中断) 专用栈
    /// - 其他: 可选, 用于关键中断
    pub ist: [u64; 7],

    /// 保留字段 (Offset 0x5C, 8 bytes)
    /// 必须为 0
    reserved_2: u64,

    /// 保留字段 (Offset 0x64, 2 bytes)
    /// 必须为 0
    reserved_3: u16,

    /// I/O 权限位图基址 (Offset 0x66, 2 bytes)
    ///
    /// 指向 I/O 位图的偏移量 (相对于 TSS 起始位置)。
    /// - 如果等于 TSS 大小, 表示没有 I/O 位图
    /// - 如果小于 TSS 大小, 则从 TSS + 此偏移处开始是 I/O 位图
    ///
    /// I/O 位图用于 Ring 3 进程的 I/O 端口权限检查。
    /// 通常设置为 `TSS_SIZE` (禁用 I/O 位图) 以简化实现。
    pub iomap_base: u16,
}

// ============================================================================
// 常量定义
// ============================================================================

/// TSS 最小大小 (bytes, Intel 要求 >= 104)
pub const TSS_MINIMUM_SIZE: usize = 104;

/// TSS 实际大小 (我们的结构体大小)
pub const TSS_SIZE: usize = core::mem::size_of::<TaskStateSegment>();

/// IST 条目数量
pub const IST_COUNT: usize = 7;

/// 默认 I/O 位图基址 (设置为 TSS 末尾, 表示无 I/O 位图)
pub const DEFAULT_IOMAP_BASE: u16 = TSS_SIZE as u16;

// ============================================================================
// 方法实现
// ============================================================================

impl TaskStateSegment {
    /// 创建全零 TSS (默认状态)
    ///
    /// 所有栈指针和 IST 都为 0, I/O 位图指向 TSS 末尾。
    #[inline]
    pub const fn zeroed() -> Self {
        Self {
            reserved_0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved_1: 0,
            ist: [0; 7],
            reserved_2: 0,
            reserved_3: 0,
            iomap_base: DEFAULT_IOMAP_BASE,
        }
    }

    /// 设置内核态栈指针 (RSP0)
    ///
    /// **必须在 `gdt_init()` 之后、任何用户进程运行之前调用!**
    ///
    /// # Arguments
    /// * `stack_top` - 内核栈顶地址 (高地址, 因为 x86 栈向下增长)
    ///
    /// # Example
    /// ```ignore
    /// let tss = get_tss_mut();
    /// tss.set_kernel_stack(0xFFFFFFFF_A000_0000); // 示例地址
    /// ```
    #[inline]
    pub fn set_kernel_stack(&mut self, stack_top: u64) {
        self.rsp0 = stack_top;
    }

    /// 获取内核态栈指针
    #[inline]
    pub fn get_kernel_stack(&self) -> u64 {
        self.rsp0
    }

    /// 设置 IST 条目
    ///
    /// # Arguments
    /// * `index` - IST 条目号 (0-6)
    /// * `stack_top` - 该 IST 的栈顶地址
    ///
    /// # Panics
    /// 如果 index >= 7
    #[inline]
    pub fn set_ist(&mut self, index: usize, stack_top: u64) {
        if index < IST_COUNT {
            self.ist[index] = stack_top;
        }
        // else: 静默忽略越界 (生产环境应 panic 或返回 Result)
    }

    /// 获取 IST 条目
    #[inline]
    pub fn get_ist(&self, index: usize) -> Option<u64> {
        if index < IST_COUNT {
            Some(self.ist[index])
        } else {
            None
        }
    }

    /// I-24: 校验关键 IST 条目 (0-3) 已填充非零栈顶.
    /// 启动顺序: GDT/TSS init → `set_ist(0..4)` → IDT init.
    /// IDT init 调用此函数确保 #DF/NMI/#PF/0x82 中断时 IST 栈可用,
    /// 避免 CPU 切换到未初始化的 0 栈顶触发三重故障.
    #[inline]
    pub fn ist_validated(&self) -> bool {
        // IST 0-3: #DF, NMI, #PF, int 0x82
        self.ist[0] != 0 && self.ist[1] != 0 && self.ist[2] != 0 && self.ist[3] != 0
    }

    /// 启用 I/O 权限位图
    ///
    /// 设置 I/O 位图基址到 TSS 内部某处。
    /// 调用者需自行填充位图内容。
    ///
    /// # Arguments
    /// * `offset` - 相对于 TSS 起始的字节偏移
    #[inline]
    pub fn enable_iomap(&mut self, offset: u16) {
        self.iomap_base = offset;
    }

    /// 禁用 I/O 权限位图
    #[inline]
    pub fn disable_iomap(&mut self) {
        self.iomap_base = DEFAULT_IOMAP_BASE;
    }

    /// 检查是否启用了 I/O 位图
    #[inline]
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn has_iomap(&self) -> bool {
        self.iomap_base < TSS_SIZE as u16
    }
}

impl Default for TaskStateSegment {
    fn default() -> Self {
        Self::zeroed()
    }
}

pub fn tss_set_kernel_stack(rsp0: u64) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let tss = super::gdt::get_tss_mut();
        tss.set_kernel_stack(rsp0);
    }
}

// [已删除] `tss_get_kernel_stack`: 原唯一调用者 (VMM `create_user_page_table`
// 的 RSP0 内联映射块) 已随 KPTI-08 迁移到 `kpti::map_rsp0_page`, 该访问器失去
// 全部调用者 (F9 死代码零容忍)。

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tss_zeroed() {
        let tss = TaskStateSegment::zeroed();
        // TaskStateSegment 是 packed 结构: &tss.field 会构造未对齐引用 (E0793),
        // 故先按值取出字段再断言.
        assert_eq!({ tss.rsp0 }, 0);
        assert_eq!({ tss.rsp1 }, 0);
        assert_eq!({ tss.rsp2 }, 0);
        assert_eq!({ tss.iomap_base }, DEFAULT_IOMAP_BASE);

        // UT-07 (2026-09-26): 原读裸字段 ist (packed 规避) 改用公有取值器 get_ist —
        // 与注册侧 arch::tss::zeroed 的 API 级断言对齐 (覆盖不降级).
        for i in 0..7 {
            assert_eq!(tss.get_ist(i), Some(0));
        }
    }

    #[test]
    fn test_tss_set_kernel_stack() {
        let mut tss = TaskStateSegment::zeroed();
        tss.set_kernel_stack(0xDEAD_BEEF_CAFE_BABE);
        assert_eq!(tss.get_kernel_stack(), 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn test_tss_ist_operations() {
        let mut tss = TaskStateSegment::zeroed();

        // 设置 IST
        tss.set_ist(0, 0x1111_2222_3333_4444);
        tss.set_ist(6, 0xAAAA_BBBB_CCCC_DDDD);

        // 读取验证
        assert_eq!(tss.get_ist(0), Some(0x1111_2222_3333_4444));
        assert_eq!(tss.get_ist(6), Some(0xAAAA_BBBB_CCCC_DDDD));

        // 未设置的 IST 应为 0
        assert_eq!(tss.get_ist(3), Some(0));

        // 越界访问返回 None
        assert_eq!(tss.get_ist(7), None);
    }

    #[test]
    fn test_tss_iomap() {
        let mut tss = TaskStateSegment::zeroed();

        // 默认禁用 (iomap_base == TSS_SIZE 是"无位图"哨兵)
        assert!(!tss.has_iomap());

        // 启用: 偏移必须落在 TSS 内部 (iomap_base < TSS_SIZE)
        let offset: u16 = 0;
        tss.enable_iomap(offset);
        assert!(tss.has_iomap());
        assert_eq!({ tss.iomap_base }, offset);

        // 边界: 偏移等于 TSS_SIZE 即回到"无位图"哨兵语义
        // (原用例传入 104 = DEFAULT_IOMAP_BASE 本身, 断言必然不成立; UT-06 实测修正)
        tss.enable_iomap(DEFAULT_IOMAP_BASE);
        assert!(!tss.has_iomap());

        // 禁用
        tss.enable_iomap(0);
        tss.disable_iomap();
        assert!(!tss.has_iomap());
        assert_eq!({ tss.iomap_base }, DEFAULT_IOMAP_BASE);
    }

    #[test]
    fn test_tss_size() {
        // TSS 必须 >= 104 字节 (Intel 64-bit TSS 规范最小值)
        assert!(TSS_SIZE >= TSS_MINIMUM_SIZE);

        // 本结构体为 packed 且恰好等于规范最小值 (104 字节); 104 % 16 == 8,
        // 故不存在"16 字节对齐"性质 (2026-09-24 UT-06 实测原断言不成立).
        assert_eq!(TSS_SIZE, TSS_MINIMUM_SIZE);
    }
}
