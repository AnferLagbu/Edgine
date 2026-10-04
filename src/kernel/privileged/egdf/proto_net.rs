//! EGDF网络设备协议 (EGDF Net Protocol)
//!
//! 定义网络设备的统一操作接口:
//! - send: 发送网络包
//! - `try_receive`: 尝试接收 (返回长度, 0=无数据)
//! - `get_mac`: 读取 MAC 地址
//! - `handle_irq`: 中断处理
//!
//! 用于 E1000、virtio-net 等网卡驱动。

/// 网络设备发送函数 (dst buffer owned by caller, len bytes to send)
pub type NetSendFn = extern "C" fn(driver_data: *mut u8, data: *const u8, len: u32) -> i32;

/// 网络设备接收函数 (receive buffer owned by caller)
/// 返回接收到的字节数, 0 表示无数据, <0 表示错误
pub type NetRecvFn = extern "C" fn(driver_data: *mut u8, buf: *mut u8, buf_len: u32) -> i32;

/// 获取 MAC 地址
pub type NetGetMacFn = extern "C" fn(driver_data: *mut u8, mac: *mut [u8; 6]);

/// 中断处理
pub type NetIrqFn = extern "C" fn(driver_data: *mut u8);

/// 网络设备操作表
pub struct NetOps {
    /// 发送网络包, 返回 0 成功 / -1 失败
    pub send: NetSendFn,
    /// 接收网络包, 返回长度 (0=空), `buf_len` 最大缓冲区大小
    pub try_receive: NetRecvFn,
    /// 获取 MAC 地址
    pub get_mac: NetGetMacFn,
    /// 中断处理
    pub handle_irq: Option<NetIrqFn>,
}

impl NetOps {
    /// 发送网络包 (Framekernel 安全接口, 调用方无需 unsafe)
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效, `data` 在调用期间有效, len 字节。
    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    pub fn send(&self, driver_data: *mut u8, data: &[u8]) -> i32 {
        (self.send)(driver_data, data.as_ptr(), data.len() as u32)
    }

    /// 接收网络包 (Framekernel 安全接口)
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效, `buf` 至少 `buf.capacity()` 字节。
    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    pub fn try_receive(&self, driver_data: *mut u8, buf: &mut [u8]) -> i32 {
        (self.try_receive)(driver_data, buf.as_mut_ptr(), buf.len() as u32)
    }

    /// 获取 MAC 地址 (Framekernel 安全接口)
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效。
    pub fn get_mac(&self, driver_data: *mut u8, mac: &mut [u8; 6]) {
        (self.get_mac)(driver_data, mac);
    }

    /// 中断处理 (Framekernel 安全接口)
    ///
    /// # Safety (调用方)
    /// - `driver_data` 必须有效, 在中断上下文中调用。
    pub fn handle_irq(&self, driver_data: *mut u8) {
        if let Some(f) = self.handle_irq {
            f(driver_data);
        }
    }
}
