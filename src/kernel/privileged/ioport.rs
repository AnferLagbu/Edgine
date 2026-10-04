//! `IoPort` — x86 PIO 安全封装 (TCB)
//!
//! 封装 `x86_64` in/out 指令, 提供端口范围校验。
//! aarch64 上无 PIO 语义, 编译为空壳。使用时需 feature gate。
//!
//! ## 适用场景
//!
//! ✅ `x86_64` Legacy 设备 (PCI 配置空间, ATA, VGA, 串口)
//! ❌ aarch64 (使用 MMIO 代替)
//!
//! ## SAFETY 不变量
//!
//! - port 必须是有效的 I/O 端口地址 (0..65536)。
//! - PIO 指令使用 nostack, nomem 选项避免编译器重排。

#[cfg(target_arch = "x86_64")]
use core::arch::asm;

/// x86 I/O 端口句柄。
///
/// 封装一个有效的 I/O 端口范围, 提供类型安全的读写。
pub struct IoPort {
    base: u16,
    /// 端口长度, 用于 `check_offset` 越界校验. 仅 `x86_64` PIO 路径使用.
    #[cfg(target_arch = "x86_64")]
    len: u16,
    name: &'static str,
}

impl IoPort {
    /// 创建 PIO 句柄。
    ///
    /// # SAFETY
    /// - `base..base+len` 必须是有效的 I/O 端口范围。
    /// - 不与任何其他 `IoPort` 实例重叠 (由调用方通过 PCI BAR 信息保证)。
    /// # Errors
    /// 端口范围长度为 0 或端口范围溢出时返回 Err。
    pub unsafe fn new(base: u16, len: u16, name: &'static str) -> Result<Self, &'static str> {
        if len == 0 {
            return Err("IoPort: zero-length port range");
        }
        if base.checked_add(len).is_none() {
            return Err("IoPort: port range overflow");
        }
        Ok(Self {
            base,
            #[cfg(target_arch = "x86_64")]
            len,
            name,
        })
    }

    /// 安全 PIO 构造 (已知 base/len 合法)
    ///
    /// 调用方契约: `base..base+len` 必须是有效 PIO 范围, 不与其他 `IoPort` 重叠。
    /// 委托 unsafe `IoPort::new`, SAFETY 由调用方保证。
    /// # Errors
    /// 端口范围长度为 0 或端口范围溢出时返回 Err。
    pub fn new_safe(base: u16, len: u16, name: &'static str) -> Result<Self, &'static str> {
        // SAFETY: 契约由调用方保证, 同 IoPort::new
        unsafe { Self::new(base, len, name) }
    }

    /// 基端口号
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn base(&self) -> u16 {
        self.base
    }

    /// 端口名称 (调试用)
    #[inline(always)]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 检查偏移量是否在端口范围内 (仅 `x86_64` 使用)
    #[cfg(target_arch = "x86_64")]
    fn check_offset(&self, offset: u16, size: u16) -> Result<u16, &'static str> {
        if offset.saturating_add(size) > self.len {
            return Err("IoPort: access out of bounds");
        }
        Ok(self.base + offset)
    }

    /// 读取 u8
    /// # Panics
    /// 端口偏移越界时 panic。
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn read_u8(&self, offset: u16) -> u8 {
        // 不可恢复: I/O 端口偏移越界是编程错误, 调用方必须保证合法偏移
        let port = self
            .check_offset(offset, 1)
            .expect("IoPort::read_u8: offset+1 越界");
        // SAFETY: `in al, dx` 触发 x86 I/O 端口读; `check_offset` 已验证 `offset + 1`
        // 不超出本 IoPort 持有的端口范围; `port` 是 u16 (与 dx 寄存器同宽), 由
        // `nomem`/`nostack` 告诉编译器此指令无内存/栈副作用, 不会与 Rust 内存模型冲突。
        unsafe {
            let val: u8;
            asm!("in al, dx", in("dx") port, out("al") val, options(nomem, nostack));
            val
        }
    }

    /// 读取 u8 (aarch64 桩)。
    ///
    /// aarch64 无 PIO 概念, 应使用 MMIO 替代。此处返回 0xFF 以保持 API 兼容,
    /// 编译期由 `#[cfg(target_arch = "x86_64")]` 关闭实际定义。
    #[cfg(target_arch = "aarch64")]
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn read_u8(&self, _offset: u16) -> u8 {
        0xFF
    }

    /// 读取 u16
    /// # Panics
    /// 端口偏移越界时 panic。
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn read_u16(&self, offset: u16) -> u16 {
        // 不可恢复: I/O 端口偏移越界是编程错误
        let port = self
            .check_offset(offset, 2)
            .expect("IoPort::read_u16: offset+2 越界");
        // SAFETY: `in ax, dx` 2 字节 PIO 读; `check_offset(offset, 2)` 已验证 2 字节不越界;
        // 2 字节对齐由 x86 PIO 总线自然保证 (端口按字节寻址, 2 字节访问需偶地址端口)。
        unsafe {
            let val: u16;
            asm!("in ax, dx", in("dx") port, out("ax") val, options(nomem, nostack));
            val
        }
    }

    /// 读取 u16 (aarch64 桩)。
    #[cfg(target_arch = "aarch64")]
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn read_u16(&self, _offset: u16) -> u16 {
        0xFFFF
    }

    /// 读取 u32
    /// # Panics
    /// 端口偏移越界时 panic。
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn read_u32(&self, offset: u16) -> u32 {
        // 不可恢复: I/O 端口偏移越界是编程错误
        let port = self
            .check_offset(offset, 4)
            .expect("IoPort::read_u32: offset+4 越界");
        // SAFETY: `in eax, dx` 4 字节 PIO 读; `check_offset(offset, 4)` 已验证 4 字节不越界;
        // 4 字节对齐由 x86 PIO 总线自然保证 (4 字节端口访问需 4 字节对齐端口)。
        unsafe {
            let val: u32;
            asm!("in eax, dx", in("dx") port, out("eax") val, options(nomem, nostack));
            val
        }
    }

    /// 读取 u32 (aarch64 桩)。
    #[cfg(target_arch = "aarch64")]
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn read_u32(&self, _offset: u16) -> u32 {
        0xFFFF_FFFF
    }

    /// 写入 u8
    /// # Panics
    /// 端口偏移越界时 panic。
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn write_u8(&self, offset: u16, val: u8) {
        // 不可恢复: I/O 端口偏移越界是编程错误
        let port = self
            .check_offset(offset, 1)
            .expect("IoPort::write_u8: offset+1 越界");
        // SAFETY: `out dx, al` 触发 x86 I/O 端口写; `check_offset` 已验证 1 字节不越界;
        // `nomem`/`nostack` 正确声明指令无 Rust 可见副作用, 不破坏借用检查。
        unsafe {
            asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
        }
    }

    /// 写入 u8 (aarch64 桩)。
    ///
    /// aarch64 上无 PIO, 编译期由 cfg 关闭实际 PIO 实现。
    /// 调用方应使用 MMIO 寄存器访问 (IoMem) 替代。
    #[cfg(target_arch = "aarch64")]
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn write_u8(&self, _offset: u16, _val: u8) {
        // no-op on aarch64: PIO is x86-only.
    }

    /// 写入 u16
    /// # Panics
    /// 端口偏移越界时 panic。
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn write_u16(&self, offset: u16, val: u16) {
        // 不可恢复: I/O 端口偏移越界是编程错误
        let port = self
            .check_offset(offset, 2)
            .expect("IoPort::write_u16: offset+2 越界");
        // SAFETY: `out dx, ax` 2 字节 PIO 写; `check_offset(offset, 2)` 已验证 2 字节不越界;
        // 2 字节对齐由 x86 PIO 总线自然保证。
        unsafe {
            asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
        }
    }

    /// 写入 u16 (aarch64 桩)。
    #[cfg(target_arch = "aarch64")]
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn write_u16(&self, _offset: u16, _val: u16) {
        // no-op on aarch64.
    }

    /// 写入 u32
    /// # Panics
    /// 端口偏移越界时 panic。
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn write_u32(&self, offset: u16, val: u32) {
        // 不可恢复: I/O 端口偏移越界是编程错误
        let port = self
            .check_offset(offset, 4)
            .expect("IoPort::write_u32: offset+4 越界");
        // SAFETY: `out dx, eax` 4 字节 PIO 写; `check_offset(offset, 4)` 已验证 4 字节不越界;
        // 4 字节对齐由 x86 PIO 总线自然保证。
        unsafe {
            asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
        }
    }

    /// 写入 u32 (aarch64 桩)。
    #[cfg(target_arch = "aarch64")]
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    pub fn write_u32(&self, _offset: u16, _val: u32) {
        // no-op on aarch64.
    }
}

// SAFETY: IoPort 封装了独占的 PIO 端口范围, 内核态独占访问。
unsafe impl Send for IoPort {}
unsafe impl Sync for IoPort {}
