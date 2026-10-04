#![deny(unsafe_code)]
//! 协议族 (Protocol Family) — services 层安全代理
//!
//! 封装 framework `egdf::proto_*` 协议族入口, 向 services 提供强类型 safe API:
//! - block — 块设备注册 (`register_block_device`) + 墓碑注销 (`unregister_block`)
//! - net — 网络设备句柄 (`NetDevice`, 去裸指针元组)
//! - input — 输入设备统一入口 (`input_read` / `input_has_data`)
//!
//! ## 覆盖说明
//!
//! `char` 协议族的统一入口已在同层 [`crate::services::egdf`] 顶层封装
//! (B1 保留项), 故不在本模块重复。
//!
//! ## 迁移方法
//!
//! 1. framework 元组返回 (`NetOps`, `driver_data`, mac) → `NetDevice` 句柄
//! 2. 裸指针不跨 services API 边界暴露 (句柄内部持有, 调用方只传切片)
//! 3. 0 unsafe 出现在 services 层

use crate::framework::egdf;

// ============================================================================
// 块设备 (proto_block)
// ============================================================================

/// 块设备统一接口 (framework 定义, 经 services 顶层 re-export)
pub use crate::framework::egdf::BlockDevice;
/// 注册块设备 (framework 安全入口, 经 services 顶层 re-export)
///
/// # 注意
/// framework 侧定义即接收 `impl BlockDevice` + 可选 MMIO 基址, 无裸指针参数,
/// 故在本层直接 re-export, 统一 `services::egdf` 出口。返回值为块设备在
/// `EGDF_DEVICES` 中的全局下标 (`drive`)。
pub use crate::framework::egdf::register_block_device;

/// 注销 (墓碑化) 块设备
///
/// `drive` 为块设备在注册表中的全局下标。热插拔移除路径专用: 不物理删除条目,
/// 而是标记为 `Removed` 并释放驱动引用, 保持后续设备下标稳定。
///
/// 返回 `true` 表示该下标处存在块设备且已成功标记移除; 下标越界 / 非块设备 /
/// 已移除时返回 `false`。
pub fn unregister_block(drive: u8) -> bool {
    egdf::egdf_unregister_block(drive)
}

// ============================================================================
// 网络设备 (proto_net)
// ============================================================================

/// 网络设备句柄 — 强类型封装 framework 的 (`NetOps`, `driver_data`, mac) 三元组
///
/// 内部持有驱动数据裸指针 (`driver_data`), 但**不跨 services API 边界暴露**:
/// 外部只经 [`NetDevice::send`] / [`NetDevice::try_receive`] /
/// [`NetDevice::handle_irq`] 传切片交互, 由 framework `NetOps` 安全方法完成
/// 指针跨界。
#[derive(Clone, Copy)]
pub struct NetDevice {
    /// 协议操作表 (framework 静态引用)
    ops: &'static egdf::NetOps,
    /// 驱动私有数据指针 (由 framework 注册时提供, 生命周期由注册表保证)
    driver_data: *mut u8,
    /// 设备 MAC 地址
    mac: [u8; 6],
}

impl NetDevice {
    /// 设备 MAC 地址
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// 发送网络包, 返回 0 成功 / 负值失败
    pub fn send(&self, data: &[u8]) -> i32 {
        self.ops.send(self.driver_data, data)
    }

    /// 尝试接收网络包, 返回接收字节数 (0=无数据, <0=错误)
    pub fn try_receive(&self, buf: &mut [u8]) -> i32 {
        self.ops.try_receive(self.driver_data, buf)
    }

    /// 网络设备中断处理
    pub fn handle_irq(&self) {
        self.ops.handle_irq(self.driver_data);
    }
}

/// 查找第一个就绪网络设备
///
/// 无就绪网络设备时返回 `None`。
pub fn find_net_device() -> Option<NetDevice> {
    let (ops, driver_data, mac) = egdf::egdf_find_net_device()?;
    Some(NetDevice {
        ops,
        driver_data,
        mac,
    })
}

// ============================================================================
// 输入设备 (proto_input)
// ============================================================================

/// 从第一个就绪输入设备读取一个字符
///
/// 无就绪设备或无数据时返回 `None`。
pub fn input_read() -> Option<u8> {
    egdf::egdf_input_read()
}

/// 检查第一个就绪输入设备是否有可读数据
pub fn input_has_data() -> bool {
    egdf::egdf_input_has_data()
}
