//! 设备驱动基础设施 (Driver Infrastructure)
//!
//! 提供统一的设备驱动接口和底层操作:
//! - **Driver Trait**: 所有驱动的统一接口契约
//! - **IO Port 抽象**: 安全的端口/MMIO操作
//! - **DriverError**: 统一的错误码体系
//!
//! 设备注册/发现/管理统一由 [EGDF 框架](super::egdf) 提供。
//!
//! ## 架构
//!
//! ```text
//! Driver Trait (接口契约, 本模块)
//!   ├── init() / shutdown()  ── 运行时行为
//!   ├── name() / device_type() ── 元信息
//!   └── is_ready() / reset() ── 状态查询
//!
//! EGDF 框架 (管理层, egdf 模块)
//!   ├── egdf_register_driver() ── 注册驱动到全局表
//!   ├── egdf_init_all() ── 批量初始化
//!   └── egdf_find_by_*() ── 设备发现
//! ```

// ============================================================================
// IO 端口操作安全封装
// ============================================================================

/// 向指定端口写入字节 (架构无关: `x86_64` → out, `AArch64` → MMIO)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
///
/// # Safety
///
/// `port` 必须是当前特权级 (Ring 0) 可访问的有效 I/O 端口地址.
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: 调用方保证 `port` 是内核态可访问的有效 I/O 端口; value 任意 u8。
    // arch!() 委托给 <CurrentArch as Arch>::outb, 内部汇编执行特权指令;
    // 外层 unsafe 函数已声明 SAFETY 契约 (见 docstring)。
    crate::arch!(outb(port, value));
}

/// 从指定端口读入字节 (架构无关: `x86_64` → in, `AArch64` → MMIO)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
///
/// # Safety
///
/// `port` 必须是当前特权级 (Ring 0) 可访问的有效 I/O 端口地址.
pub unsafe fn inb(port: u16) -> u8 {
    // SAFETY: 调用方保证 `port` 是内核态可访问的有效 I/O 端口;
    // 读操作返回 CPU 端口值到 u8 (截断高 8 位), 无其他副作用。
    crate::arch!(inb(port))
}

/// 向指定端口写入字 (`x86_64` 特有, 无 Arch trait 等价方法)
#[inline(always)]
#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
///
/// # Safety
///
/// `port` 必须是当前特权级 (Ring 0) 可访问的有效 I/O 端口地址.
pub unsafe fn outw(port: u16, value: u16) {
    unsafe {
        // SAFETY: 调用方保证 `port` 是内核态可访问的有效 I/O 端口 (字宽 16-bit);
        // options(nomem, nostack, preserves_flags) 正确声明 out 指令的副作用范围。
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// 从指定端口读入字 (`x86_64` 特有, 无 Arch trait 等价方法)
#[inline(always)]
#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
///
/// # Safety
///
/// `port` 必须是当前特权级 (Ring 0) 可访问的有效 I/O 端口地址.
pub unsafe fn inw(port: u16) -> u16 {
    unsafe {
        // SAFETY: 调用方保证 `port` 是内核态可访问的有效 I/O 端口 (字宽 16-bit);
        // in 指令仅返回端口值到指定寄存器, 无其他副作用。
        let value: u16;
        core::arch::asm!(
            "in ax, dx",
            out("ax") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags),
        );
        value
    }
}

/// 向指定端口写入双字 (架构无关: `x86_64` → out, `AArch64` → MMIO)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
///
/// # Safety
///
/// `port` 必须是当前特权级 (Ring 0) 可访问的有效 I/O 端口地址.
pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: 调用方保证 `port` 是内核态可访问的有效 I/O 端口 (双字 32-bit);
    // arch!() 委托给 <CurrentArch as Arch>::outl, value 任意 u32。
    crate::arch!(outl(port, value));
}

/// 从指定端口读入双字 (架构无关: `x86_64` → in, `AArch64` → MMIO)
#[inline(always)]
///
/// # Safety
///
/// `port` 必须是当前特权级 (Ring 0) 可访问的有效 I/O 端口地址.
pub unsafe fn inl(port: u16) -> u32 {
    // SAFETY: 调用方保证 `port` 是内核态可访问的有效 I/O 端口 (双字 32-bit);
    // 读操作返回 CPU 端口值, 无其他副作用。
    crate::arch!(inl(port))
}

// ============================================================================
// 错误码定义
// ============================================================================

/// 驱动通用错误码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    /// 参数无效
    InvalidParameter,
    /// 设备不存在
    DeviceNotFound,
    /// 超时
    Timeout,
    /// 硬件错误
    HardwareError,
    /// 缓冲区不足
    BufferTooSmall,
    /// 不支持的操作
    UnsupportedOperation,
    /// 忙碌
    Busy,
    /// 未初始化
    NotInitialized,
}

impl core::fmt::Display for DriverError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidParameter => write!(f, "Invalid parameter"),
            Self::DeviceNotFound => write!(f, "Device not found"),
            Self::Timeout => write!(f, "Operation timeout"),
            Self::HardwareError => write!(f, "Hardware error"),
            Self::BufferTooSmall => write!(f, "Buffer too small"),
            Self::UnsupportedOperation => write!(f, "Unsupported operation"),
            Self::Busy => write!(f, "Device busy"),
            Self::NotInitialized => write!(f, "Not initialized"),
        }
    }
}

/// 操作结果类型别名
pub type Result<T> = core::result::Result<T, DriverError>;

// ============================================================================
// 设备类型枚举
// ============================================================================

/// 设备类型分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    /// 块设备 (磁盘)
    Block,
    /// 字符设备 (终端、键盘)
    Char,
    /// 网络设备
    Network,
    /// 总线控制器 (PCI, USB)
    Bus,
    /// 输入设备 (鼠标、触摸板)
    Input,
    /// 其他
    Other,
}

impl core::fmt::Display for DeviceType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Block => write!(f, "Block"),
            Self::Char => write!(f, "Char"),
            Self::Network => write!(f, "Network"),
            Self::Bus => write!(f, "Bus"),
            Self::Input => write!(f, "Input"),
            Self::Other => write!(f, "Other"),
        }
    }
}

// ============================================================================
// 设备描述信息 (纯数据结构, 注册管理由 EGDF 负责)
// ============================================================================

/// 设备描述信息 (元数据容器, 不含注册逻辑)
///
/// 注册/发现/管理统一由 EGDF 框架处理。
/// 此结构体保留用于驱动内部的元数据存储和向后兼容。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: u32,
    pub name: &'static str,
    pub device_type: DeviceType,
    pub initialized: bool,
    pub io_base: Option<u16>,
    pub irq: Option<u8>,
}

impl DeviceInfo {
    pub fn new(name: &'static str, device_type: DeviceType) -> Self {
        static NEXT_INFO_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);
        Self {
            id: NEXT_INFO_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            name,
            device_type,
            initialized: false,
            io_base: None,
            irq: None,
        }
    }

    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub fn with_io_base(mut self, base: u16) -> Self {
        self.io_base = Some(base);
        self
    }

    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub fn with_irq(mut self, irq: u8) -> Self {
        self.irq = Some(irq);
        self
    }
}

// ============================================================================
// 统一驱动 Trait
// ============================================================================

/// 设备驱动统一接口
///
/// 所有硬件驱动都必须实现此 trait，提供标准化的初始化、
/// 控制和查询接口。
///
/// # Example
/// ```rust,ignore
/// struct MyDriver;
///
/// impl Driver for MyDriver {
///     fn name(&self) -> &'static str { "my_driver" }
///     fn device_type(&self) -> DeviceType { DeviceType::Other }
///     
///     fn init(&mut self) -> Result<()> { Ok(()) }
///     fn shutdown(&mut self) -> Result<()> { Ok(()) }
/// }
/// ```
pub trait Driver {
    /// 获取驱动名称
    fn name(&self) -> &'static str;

    /// 获取设备类型
    fn device_type(&self) -> DeviceType;

    /// 初始化驱动和硬件
    ///
    /// # Returns
    /// - `Ok(())` - 初始化成功
    /// - `Err(DriverError)` - 初始化失败
    /// # Errors
    /// 硬件初始化失败或驱动初始化不完整时返回 Err。
    fn init(&mut self) -> Result<()>;

    /// 关闭驱动并释放资源
    /// # Errors
    /// 关闭操作失败时返回 Err。
    fn shutdown(&mut self) -> Result<()>;

    /// 检查设备是否就绪
    #[inline]
    fn is_ready(&self) -> bool {
        true
    }

    /// 重置设备
    /// # Errors
    /// 设备不支持重置操作时返回 Err。
    #[inline]
    fn reset(&mut self) -> Result<()> {
        Err(DriverError::UnsupportedOperation)
    }

    /// 获取设备状态信息
    ///
    /// 返回人类可读的状态字符串
    fn status(&self) -> &'static str {
        "Ready"
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn test_error_codes() {
        assert_eq!(
            DriverError::InvalidParameter.to_string(),
            "Invalid parameter"
        );
        assert_eq!(DriverError::Timeout.to_string(), "Operation timeout");
        assert_ne!(DriverError::Busy, DriverError::NotInitialized);
    }

    #[test]
    fn test_device_types() {
        assert_eq!(DeviceType::Block.to_string(), "Block");
        assert_eq!(DeviceType::Char.to_string(), "Char");
        assert_eq!(DeviceType::Network.to_string(), "Network");

        let block_str = format!("{}", DeviceType::Block);
        assert_eq!(block_str, "Block");
    }

    #[test]
    fn test_device_info_creation() {
        // UT-07 (2026-09-26): 注册侧 driver::infra::device_info_creation 迁入.
        let info = DeviceInfo::new("test_device", DeviceType::Other);
        assert!(info.id > 0);
        assert_eq!(info.name, "test_device");
        assert!(!info.initialized);
        assert!(info.io_base.is_none());
        assert!(info.irq.is_none());
    }

    #[test]
    fn test_device_info_builder() {
        // UT-07 (2026-09-26): 注册侧 driver::infra::device_info_builder 迁入.
        let info = DeviceInfo::new("serial0", DeviceType::Char)
            .with_io_base(0x3F8)
            .with_irq(4);
        assert_eq!(info.io_base, Some(0x3F8));
        assert_eq!(info.irq, Some(4));
    }

    #[test]
    fn test_result_type_alias() {
        fn returns_ok() -> Result<u32> {
            Ok(42)
        }

        fn returns_err() -> Result<u32> {
            Err(DriverError::DeviceNotFound)
        }

        assert!(returns_ok().is_ok());
        assert!(returns_err().is_err());
        assert_eq!(returns_ok().unwrap(), 42);
    }
}
