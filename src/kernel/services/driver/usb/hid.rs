#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! USB HID (Human Interface Device 人机接口设备) 类驱动 - USB-1.7
//!
//! 实现 USB HID 1.11 规范的最小可用骨架:
//!
//! - **HID Descriptor 解析**: `GET_DESCRIPTOR(HID)` 响应解析
//! - **Boot Protocol 支持**: 键盘/鼠标 boot 报告格式 (USB HID 1.11 §4.2, §4.4)
//! - **`SET_PROTOCOL` / `GET_PROTOCOL`**: 协议切换 (USB HID 1.11 §4.5)
//! - **Boot Keyboard Report**: 8 字节 (modifiers, reserved, 6 keycodes)
//! - **Boot Mouse Report**: 3 字节 (buttons, X, Y)
//!
//! ## 限制
//!
//! - 当前为**软件骨架**: 不通过真实 xHCI 控制器发送 HID 请求
//! - 不实现 HID Report Descriptor 解析 (类特定描述符, 由应用层处理)
//! - 不实现 Feature Report 处理
//! - 不实现 Report ID 多路复用
//!
//! See also:
//! - `usb/enumerate.rs` USB-1.6 设备枚举 (找到 HID Interface 后调用本驱动初始化)
//! - `xhci.rs` USB-1.3 URB 提交 (本驱动使用 `submit_urb` 发送中断 IN 报告)

use crate::framework::driver::framework::{DriverError, Result};
use super::usb_core::{DeviceClass, UsbDevice, UsbSetupPacket};
use alloc::vec::Vec;

// ============================================================================
// HID 描述符 (USB HID 1.11 §4.2.1)
// ============================================================================

/// HID Descriptor (6 字节 + 物理层特定扩展).
///
/// 注: 标准 HID Descriptor 前 6 字节是定长的, 后续字段 (`DescriptorLength`, `CountryCode`)
///     属于 Report Descriptor 引用. 当前骨架仅解析前 6 字节.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidDescriptor {
    /// 描述符长度 (字节)
    pub length: u8,
    /// 描述符类型 (固定为 0x21 = HID)
    pub descriptor_type: u8,
    /// HID 规范版本 (BCD, 0x0110 = HID 1.11)
    pub hid_version: u16,
    /// 国家代码 (USB HID 1.11 §4.2.1: 0=Not Supported, 33=US)
    pub country_code: u8,
    /// Class-Specific 描述符数量 (Report Descriptor 等)
    pub num_descriptors: u8,
    /// Class-Specific 描述符类型 (固定为 0x22 = Report)
    pub report_descriptor_type: u8,
    /// Report Descriptor 总长度
    pub report_descriptor_length: u16,
}

/// 解析 HID Descriptor (至少 6 字节, USB HID 1.11 §4.2.1).
///
/// # 参数
///
/// - `data`: 至少 6 字节的 `GET_DESCRIPTOR(HID)` 响应数据.
///
/// # 错误
///
/// - `DriverError::InvalidParameter`: data 长度不足或 `descriptor_type` 不为 0x21.
/// # Errors
/// data 长度不足 6 字节或描述符类型不为 0x21 时返回 Err。
pub fn parse_hid_descriptor(data: &[u8]) -> Result<HidDescriptor> {
    if data.len() < 6 {
        return Err(DriverError::InvalidParameter);
    }
    if data[0] != 6 {
        return Err(DriverError::InvalidParameter);
    }
    if data[1] != 0x21 {
        // HID descriptor type
        return Err(DriverError::InvalidParameter);
    }

    Ok(HidDescriptor {
        length: data[0],
        descriptor_type: data[1],
        hid_version: u16::from_le_bytes([data[2], data[3]]),
        country_code: data[4],
        num_descriptors: data[5],
        report_descriptor_type: if data.len() > 6 { data[6] } else { 0 },
        report_descriptor_length: if data.len() > 7 {
            u16::from_le_bytes([data[7], data[8]])
        } else {
            0
        },
    })
}

// ============================================================================
// HID 协议常量 (USB HID 1.11 §4.5)
// ============================================================================

/// HID 协议类型 (USB HID 1.11 §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HidProtocol {
    /// Boot Protocol (键盘/鼠标的简化协议, BIOS 兼容)
    Boot = 0,
    /// Report Protocol (完整 HID Report Descriptor)
    Report = 1,
}

/// `SET_PROTOCOL` / `GET_PROTOCOL` Setup Packet 构造.
fn make_set_protocol_request(protocol: HidProtocol) -> UsbSetupPacket {
    UsbSetupPacket {
        request_type: 0x21, // 主机到设备 (Host-to-Device), 类请求 (Class), 接口 (Interface)
        request: 0x0B,      // SET_PROTOCOL
        value: protocol as u16,
        index: 0, // 由调用方填入 Interface number
        length: 0,
    }
}

// ============================================================================
// Boot Report 格式 (USB HID 1.11 §4.2, §4.4)
// ============================================================================

/// Boot Keyboard Report (8 字节, USB HID 1.11 §4.2).
///
/// 字节布局:
/// - byte 0: Modifier keys (bit 0=LCTRL, bit 1=LSHIFT, ..., bit 7=LGUI)
/// - byte 1: Reserved (固定为 0)
/// - byte 2..=7: 最多 6 个同时按下的 keycode (USB HID Usage Table)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BootKeyboardReport {
    /// Modifier byte: bit 0=LCTRL, 1=LSHIFT, 2=LALT, 3=LGUI,
    ///                bit 4=RCTRL, 5=RSHIFT, 6=RALT, 7=RGUI
    pub modifier: u8,
    /// Reserved byte (必须为 0)
    pub reserved: u8,
    /// 6 个 keycode (USB HID Usage Page 7 / Keyboard)
    pub keycodes: [u8; 6],
}

impl BootKeyboardReport {
    /// 解析 8 字节 Boot Keyboard Report 数据.
    /// # Errors
    /// data 长度不足 8 字节时返回 Err。
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 8 {
            return Err(DriverError::InvalidParameter);
        }
        Ok(Self {
            modifier: data[0],
            reserved: data[1],
            keycodes: [data[2], data[3], data[4], data[5], data[6], data[7]],
        })
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 检查 Ctrl 是否按下.
    pub fn ctrl_pressed(&self) -> bool {
        self.modifier & 0x01 != 0 || self.modifier & 0x10 != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 检查 Shift 是否按下.
    pub fn shift_pressed(&self) -> bool {
        self.modifier & 0x02 != 0 || self.modifier & 0x20 != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 检查 Alt 是否按下.
    pub fn alt_pressed(&self) -> bool {
        self.modifier & 0x04 != 0 || self.modifier & 0x40 != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 返回当前按下的所有 keycode 列表 (去重).
    pub fn pressed_keys(&self) -> Vec<u8> {
        let mut keys = Vec::new();
        for &kc in &self.keycodes {
            if kc != 0 && !keys.contains(&kc) {
                keys.push(kc);
            }
        }
        keys
    }
}

/// Modifier 键常量 (USB HID Usage Table / Keyboard §10).
pub mod modifier {
    pub const LCTRL: u8 = 1 << 0;
    pub const LSHIFT: u8 = 1 << 1;
    pub const LALT: u8 = 1 << 2;
    pub const LGUI: u8 = 1 << 3;
    pub const RCTRL: u8 = 1 << 4;
    pub const RSHIFT: u8 = 1 << 5;
    pub const RALT: u8 = 1 << 6;
    pub const RGUI: u8 = 1 << 7;
}

/// Boot Mouse Report (3 字节最小, USB HID 1.11 §4.4).
///
/// 字节布局:
/// - byte 0: Buttons (bit 0=Left, bit 1=Right, bit 2=Middle)
/// - byte 1: X displacement (signed 8-bit, 相对位移)
/// - byte 2: Y displacement (signed 8-bit, 相对位移)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BootMouseReport {
    /// Buttons (bit 0=Left, bit 1=Right, bit 2=Middle)
    pub buttons: u8,
    /// X 位移 (有符号 8-bit)
    pub x: i8,
    /// Y 位移 (有符号 8-bit)
    pub y: i8,
}

impl BootMouseReport {
    /// 解析 3 字节 Boot Mouse Report 数据.
    /// # Errors
    /// data 长度不足 3 字节时返回 Err。
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 3 {
            return Err(DriverError::InvalidParameter);
        }
        Ok(Self {
            buttons: data[0],
            x: data[1] as i8,
            y: data[2] as i8,
        })
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// Left button 是否按下.
    pub fn left_pressed(&self) -> bool {
        self.buttons & 0x01 != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// Right button 是否按下.
    pub fn right_pressed(&self) -> bool {
        self.buttons & 0x02 != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// Middle button 是否按下.
    pub fn middle_pressed(&self) -> bool {
        self.buttons & 0x04 != 0
    }
}

/// Mouse buttons 键常量.
pub mod mouse_button {
    pub const LEFT: u8 = 1 << 0;
    pub const RIGHT: u8 = 1 << 1;
    pub const MIDDLE: u8 = 1 << 2;
}

// ============================================================================
// HID Device Driver (USB-1.7)
// ============================================================================

/// HID 设备类型 (USB HID 1.11 §4.1 Subclass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidDeviceType {
    /// 无 Subclass (1.0 兼容)
    None,
    /// Boot 接口子类 (1.1+)
    Boot,
    /// 其他 Subclass
    Other(u8),
}

impl HidDeviceType {
    /// 从 Interface Subclass 字节构造.
    pub fn from_subclass(subclass: u8) -> Self {
        match subclass {
            0 => Self::None,
            1 => Self::Boot,
            other => Self::Other(other),
        }
    }
}

/// HID 协议类型 (USB HID 1.11 §4.3 Protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidProtocolType {
    /// None
    None,
    /// Keyboard
    Keyboard,
    /// Mouse
    Mouse,
    /// 其他
    Other(u8),
}

impl HidProtocolType {
    /// 从 Interface Protocol 字节构造.
    pub fn from_protocol(protocol: u8) -> Self {
        match protocol {
            0 => Self::None,
            1 => Self::Keyboard,
            2 => Self::Mouse,
            other => Self::Other(other),
        }
    }
}

/// HID 设备驱动实例 (USB-1.7).
///
/// 当前为**软件骨架**: 记录 HID 设备的协议 / 类型信息, 提供 `SET_PROTOCOL` Setup Packet
/// 构造. 真实硬件应通过 Control Transfer (`request_type=0x21`, request=0x0B) 切换协议.
pub struct HidDriver {
    /// Interface number
    interface_number: u8,
    /// Interrupt IN endpoint (e.g. 0x81 for EP1 IN)
    interrupt_in_endpoint: u8,
    /// Interrupt IN max packet size
    interrupt_in_max_packet: u16,
    /// 当前协议
    protocol: HidProtocol,
    /// HID 设备类型
    device_type: HidDeviceType,
    /// HID 协议类型
    protocol_type: HidProtocolType,
}

impl HidDriver {
    /// 从 `UsbDevice` 创建 HID 驱动实例.
    ///
    /// # 限制
    ///
    /// - 当前骨架假设设备只有一个 Interface 且 class=HID
    /// - 不解析 HID Descriptor 字段 (由调用方提供 endpoint 信息)
    /// # Errors
    /// 设备或接口不属于 HID 类、接口索引非法或未找到 Interrupt IN 端点时返回 Err。
    pub fn from_usb_device(device: &UsbDevice, interface_idx: usize) -> Result<Self> {
        if device.descriptor.device_class != DeviceClass::Hid as u8 {
            return Err(DriverError::InvalidParameter);
        }
        let iface = device
            .interfaces
            .get(interface_idx)
            .ok_or(DriverError::InvalidParameter)?;
        if iface.interface_class != DeviceClass::Hid as u8 {
            return Err(DriverError::InvalidParameter);
        }

        // 查找 Interrupt IN endpoint (USB HID 1.11 要求)
        let interrupt_in = device
            .endpoints
            .iter()
            .find(|ep| {
                ep.attributes == 0x03 // Interrupt
                    && ep.endpoint_address & 0x80 != 0 // IN direction
            })
            .ok_or(DriverError::InvalidParameter)?;

        Ok(Self {
            interface_number: iface.interface_number,
            interrupt_in_endpoint: interrupt_in.endpoint_address,
            interrupt_in_max_packet: interrupt_in.max_packet_size,
            protocol: HidProtocol::Report,
            device_type: HidDeviceType::from_subclass(iface.interface_subclass),
            protocol_type: HidProtocolType::from_protocol(iface.interface_protocol),
        })
    }

    /// 构造 `SET_PROTOCOL` Boot Protocol 请求 Setup Packet.
    ///
    /// 真实硬件应通过 Control Transfer (`request_type=0x21`, request=0x0B) 发送.
    pub fn set_protocol_setup(&self, protocol: HidProtocol) -> UsbSetupPacket {
        let mut req = make_set_protocol_request(protocol);
        req.index = u16::from(self.interface_number);
        req
    }

    /// 获取 Interrupt IN endpoint 信息.
    pub fn interrupt_in_endpoint(&self) -> u8 {
        self.interrupt_in_endpoint
    }

    /// 获取 Interrupt IN max packet size.
    pub fn interrupt_in_max_packet(&self) -> u16 {
        self.interrupt_in_max_packet
    }

    /// 获取接口编号
    pub fn interface_number(&self) -> u8 {
        self.interface_number
    }

    /// 获取当前协议.
    pub fn protocol(&self) -> HidProtocol {
        self.protocol
    }

    /// 切换协议.
    pub fn switch_protocol(&mut self, protocol: HidProtocol) {
        self.protocol = protocol;
    }

    /// 获取 HID 设备类型.
    pub fn device_type(&self) -> HidDeviceType {
        self.device_type
    }

    /// 获取 HID 协议类型.
    pub fn protocol_type(&self) -> HidProtocolType {
        self.protocol_type
    }

    /// 检查是否为 Boot Keyboard.
    pub fn is_boot_keyboard(&self) -> bool {
        self.device_type == HidDeviceType::Boot && self.protocol_type == HidProtocolType::Keyboard
    }

    /// 检查是否为 Boot Mouse.
    pub fn is_boot_mouse(&self) -> bool {
        self.device_type == HidDeviceType::Boot && self.protocol_type == HidProtocolType::Mouse
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::driver::usb::usb_core::EndpointDescriptor;
    use crate::services::driver::usb::usb_core::InterfaceDescriptor;
    use crate::services::driver::usb::usb_core::UsbSpeed;
    use alloc::vec;

    // ----------------- HID 描述符解析器测试 -----------------

    #[test]
    fn test_parse_hid_descriptor_valid() {
        let data = [
            6,    // bLength
            0x21, // bDescriptorType = HID
            0x10, 0x01, // bcdHID = 0x0110 (HID 1.1)
            0x21, // bCountryCode = US
            1,    // bNumDescriptors = 1
            0x22, // bDescriptorType = Report
            0x3F, 0x00, // wDescriptorLength = 63
        ];
        let desc = parse_hid_descriptor(&data).unwrap();
        assert_eq!(desc.length, 6);
        assert_eq!(desc.descriptor_type, 0x21);
        assert_eq!(desc.hid_version, 0x0110);
        assert_eq!(desc.country_code, 0x21);
        assert_eq!(desc.num_descriptors, 1);
        assert_eq!(desc.report_descriptor_type, 0x22);
        assert_eq!(desc.report_descriptor_length, 63);
    }

    #[test]
    fn test_parse_hid_descriptor_too_short() {
        assert!(parse_hid_descriptor(&[0u8; 5]).is_err());
    }

    #[test]
    fn test_parse_hid_descriptor_wrong_type() {
        let mut data = [0u8; 6];
        data[0] = 6;
        data[1] = 0x22; // wrong type
        assert!(parse_hid_descriptor(&data).is_err());
    }

    // ----------------- Boot 键盘报告测试 -----------------

    #[test]
    fn test_boot_keyboard_report_parse_no_keys() {
        let data = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let report = BootKeyboardReport::parse(&data).unwrap();
        assert_eq!(report.modifier, 0);
        assert_eq!(report.keycodes, [0; 6]);
        assert!(report.pressed_keys().is_empty());
    }

    #[test]
    fn test_boot_keyboard_report_parse_with_keys() {
        // 'a' (0x04) + 'b' (0x05) + 'c' (0x06) + LCTRL
        let data = [modifier::LCTRL, 0x00, 0x04, 0x05, 0x06, 0x00, 0x00, 0x00];
        let report = BootKeyboardReport::parse(&data).unwrap();
        assert!(report.ctrl_pressed());
        assert!(!report.shift_pressed());
        let keys = report.pressed_keys();
        assert_eq!(keys, vec![0x04, 0x05, 0x06]);
    }

    #[test]
    fn test_boot_keyboard_report_parse_too_short() {
        assert!(BootKeyboardReport::parse(&[0u8; 7]).is_err());
    }

    #[test]
    fn test_boot_keyboard_modifier_helpers() {
        let lshift = BootKeyboardReport {
            modifier: modifier::LSHIFT,
            reserved: 0,
            keycodes: [0; 6],
        };
        let rctrl = BootKeyboardReport {
            modifier: modifier::RCTRL,
            reserved: 0,
            keycodes: [0; 6],
        };
        assert!(lshift.shift_pressed());
        assert!(rctrl.ctrl_pressed());
        assert!(!lshift.alt_pressed());
    }

    // ----------------- Boot 鼠标报告测试 -----------------

    #[test]
    fn test_boot_mouse_report_parse_no_movement() {
        let data = [0x00, 0x00, 0x00];
        let report = BootMouseReport::parse(&data).unwrap();
        assert_eq!(report.buttons, 0);
        assert_eq!(report.x, 0);
        assert_eq!(report.y, 0);
        assert!(!report.left_pressed());
        assert!(!report.right_pressed());
        assert!(!report.middle_pressed());
    }

    #[test]
    fn test_boot_mouse_report_parse_with_movement_and_buttons() {
        // 左键按下, X 移动 +10, Y 移动 -5
        let data = [mouse_button::LEFT, 10, (-5i8) as u8];
        let report = BootMouseReport::parse(&data).unwrap();
        assert!(report.left_pressed());
        assert_eq!(report.x, 10);
        assert_eq!(report.y, -5);
    }

    #[test]
    fn test_boot_mouse_report_negative_displacement() {
        // 测试有符号转换: 0xFF → -1
        let data = [0x00, 0xFF, 0xFF];
        let report = BootMouseReport::parse(&data).unwrap();
        assert_eq!(report.x, -1);
        assert_eq!(report.y, -1);
    }

    #[test]
    fn test_boot_mouse_report_parse_too_short() {
        assert!(BootMouseReport::parse(&[0u8; 2]).is_err());
    }

    // ----------------- HidDeviceType / HidProtocolType 类型测试 -----------------

    #[test]
    fn test_hid_device_type_from_subclass() {
        assert_eq!(HidDeviceType::from_subclass(0), HidDeviceType::None);
        assert_eq!(HidDeviceType::from_subclass(1), HidDeviceType::Boot);
        assert_eq!(HidDeviceType::from_subclass(2), HidDeviceType::Other(2));
    }

    #[test]
    fn test_hid_protocol_type_from_protocol() {
        assert_eq!(HidProtocolType::from_protocol(0), HidProtocolType::None);
        assert_eq!(HidProtocolType::from_protocol(1), HidProtocolType::Keyboard);
        assert_eq!(HidProtocolType::from_protocol(2), HidProtocolType::Mouse);
        assert_eq!(HidProtocolType::from_protocol(3), HidProtocolType::Other(3));
    }

    // ----------------- HidDriver Tests -----------------

    fn make_test_hid_device() -> UsbDevice {
        use crate::services::driver::usb::enumerate::parse_device_descriptor;

        // data[4] = bDeviceClass: HidDriver::from_usb_device 要求其为 HID (0x03),
        // 原值为 0 导致 3 例构造测试全部被拒 (2026-09-24 UT-06 修正).
        let device_data = [
            18, 1, 0x10, 0x01, 0x03, 0x00, 0x00, 0x40, 0xAB, 0x12, 0xCD, 0x34, 0x00, 0x01, 1, 2, 0,
            1,
        ];
        let descriptor = parse_device_descriptor(&device_data).unwrap();

        UsbDevice {
            id: 0,
            address: 5,
            speed: UsbSpeed::High,
            state: crate::services::driver::usb::usb_core::DeviceState::Configured,
            descriptor,
            configuration: Some(1),
            interfaces: vec![InterfaceDescriptor {
                length: 9,
                descriptor_type: 4,
                interface_number: 0,
                alternate_setting: 0,
                num_endpoints: 1,
                interface_class: DeviceClass::Hid as u8,
                interface_subclass: 1, // 启动接口子类 (Boot Interface Subclass)
                interface_protocol: 1, // Keyboard
                interface_index: 0,
            }],
            endpoints: vec![EndpointDescriptor {
                length: 7,
                descriptor_type: 5,
                endpoint_address: 0x81, // IN, EP1
                attributes: 0x03,       // Interrupt
                max_packet_size: 8,
                interval: 10,
            }],
            info: crate::framework::driver::framework::DeviceInfo::new(
                "test-hid",
                crate::framework::driver::framework::DeviceType::Other,
            ),
        }
    }

    #[test]
    fn test_hid_driver_from_usb_device_keyboard() {
        let device = make_test_hid_device();
        let driver = HidDriver::from_usb_device(&device, 0).unwrap();
        // HidDriver 不再保存 device_address 字段, 该断言随字段移除而删除.
        assert_eq!(driver.interface_number, 0);
        assert_eq!(driver.interrupt_in_endpoint(), 0x81);
        assert_eq!(driver.interrupt_in_max_packet(), 8);
        assert!(driver.is_boot_keyboard());
        assert!(!driver.is_boot_mouse());
    }

    #[test]
    fn test_hid_driver_set_protocol_setup_packet() {
        let device = make_test_hid_device();
        let driver = HidDriver::from_usb_device(&device, 0).unwrap();
        let req = driver.set_protocol_setup(HidProtocol::Boot);
        // 请求结构为 packed: u16 字段取引用会触发 E0793, 故按值取出再断言.
        assert_eq!({ req.request_type }, 0x21); // 类请求 (Class), 接口 (Interface), 主机到设备
        assert_eq!({ req.request }, 0x0B); // SET_PROTOCOL
        assert_eq!({ req.value }, 0); // Boot
        assert_eq!({ req.index }, 0); // interface 0
    }

    #[test]
    fn test_hid_driver_switch_protocol() {
        let device = make_test_hid_device();
        let mut driver = HidDriver::from_usb_device(&device, 0).unwrap();
        assert_eq!(driver.protocol(), HidProtocol::Report);
        driver.switch_protocol(HidProtocol::Boot);
        assert_eq!(driver.protocol(), HidProtocol::Boot);
    }

    #[test]
    fn test_hid_driver_rejects_non_hid_device() {
        let mut device = make_test_hid_device();
        // 改写 device_class 为非 HID
        device.descriptor.device_class = 0xFF; // not HID
        let result = HidDriver::from_usb_device(&device, 0);
        assert!(matches!(result, Err(DriverError::InvalidParameter)));
    }

    #[test]
    fn test_hid_driver_requires_interrupt_in_endpoint() {
        let mut device = make_test_hid_device();
        // Remove all endpoints
        device.endpoints.clear();
        let result = HidDriver::from_usb_device(&device, 0);
        assert!(matches!(result, Err(DriverError::InvalidParameter)));
    }
}
