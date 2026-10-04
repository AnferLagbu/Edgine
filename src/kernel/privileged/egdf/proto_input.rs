//! EGDF输入设备协议 (EGDF Input Protocol)
//!
//! 定义输入设备的统一操作接口:
//! - `read_char`: 读取一个字符 (非阻塞)
//! - `has_char`: 检查缓冲区是否有数据
//! - `handle_irq`: 中断处理
//!
//! 用于键盘、鼠标等输入设备。

/// 输入设备读字符 (返回非空指针=成功, null=无数据)
pub type InputReadFn = extern "C" fn(driver_data: *mut u8) -> *const u8;

/// 输入设备检查可读
pub type InputHasDataFn = extern "C" fn(driver_data: *mut u8) -> bool;

/// 输入设备中断处理
pub type InputIrqFn = extern "C" fn(driver_data: *mut u8);

/// 输入设备操作表
pub struct InputOps {
    /// 读取一个字符, None 表示无数据
    pub read_char: InputReadFn,
    /// 检查是否有可读数据
    pub has_char: InputHasDataFn,
    /// 中断处理 (键盘 IRQ1)
    pub handle_irq: InputIrqFn,
}

impl InputOps {
    /// 读取一个字符 (Framekernel 安全接口)
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效。
    pub fn read_char(&self, driver_data: *mut u8) -> Option<u8> {
        let p = (self.read_char)(driver_data);
        if p.is_null() {
            None
        } else {
            // SAFETY: 非空指针由驱动函数保证指向有效 u8。
            Some(unsafe { *p })
        }
    }

    /// 检查是否有可读数据
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效。
    pub fn has_char(&self, driver_data: *mut u8) -> bool {
        (self.has_char)(driver_data)
    }

    /// 输入设备中断处理
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效, 在中断上下文中调用。
    pub fn handle_irq(&self, driver_data: *mut u8) {
        (self.handle_irq)(driver_data);
    }
}
