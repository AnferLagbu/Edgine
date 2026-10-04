//! EGDF字符设备协议 (EGDF Char Protocol)
//!
//! 定义字符设备的统一操作接口:
//! - read: 从设备读取字节
//! - write: 向设备写入字节
//! - ioctl: 设备控制 (预留)
//!
//! 用于 VGA、串口、TTY 等字符设备。

/// 字符设备读操作
pub type CharReadFn = extern "C" fn(driver_data: *mut u8, buf: *mut u8, len: usize) -> usize;

/// 字符设备写操作
pub type CharWriteFn = extern "C" fn(driver_data: *mut u8, buf: *const u8, len: usize) -> usize;

/// 字符设备操作表
pub struct CharOps {
    /// 读取字节, 返回实际读取数
    pub read: CharReadFn,
    /// 写入字节, 返回实际写入数
    pub write: CharWriteFn,
    /// (预留) ioctl
    pub ioctl: Option<extern "C" fn(driver_data: *mut u8, cmd: u32, arg: usize) -> i32>,
}

impl CharOps {
    /// 字符设备读 (Framekernel 安全接口)
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效, `buf` 至少 `buf.len()` 字节。
    pub fn read(&self, driver_data: *mut u8, buf: &mut [u8]) -> usize {
        (self.read)(driver_data, buf.as_mut_ptr(), buf.len())
    }

    /// 字符设备写 (Framekernel 安全接口)
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效, `buf` 在调用期间有效。
    pub fn write(&self, driver_data: *mut u8, buf: &[u8]) -> usize {
        (self.write)(driver_data, buf.as_ptr(), buf.len())
    }
}
