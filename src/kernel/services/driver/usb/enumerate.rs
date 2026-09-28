#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! USB 设备枚举 (Device Enumeration) - USB-1.6
//!
//! 实现 USB 设备枚举流程 (USB 2.0 规范 §9.1):
//!
//! 1. **`GET_DESCRIPTOR` (Device)**: 发送 `GET_DESCRIPTOR` Device Descriptor 请求到地址 0
//!    (default address), 解析 18 字节 Device Descriptor (USB 规范 §9.6.1).
//! 2. **`SET_ADDRESS`**: 分配新地址 (通过 `XhciController::allocate_address`), 发送
//!    `SET_ADDRESS` 请求. 之后设备通信使用新地址.
//! 3. **`GET_DESCRIPTOR` (Configuration)**: 读取 Configuration Descriptor + Interface
//!    Descriptor + Endpoint Descriptor (USB 规范 §9.6.3).
//! 4. **`SET_CONFIGURATION`**: 选择 Configuration 1 (或其他有效配置).
//!
//! ## 限制
//!
//! - 当前为**软件骨架**实装, 不通过真实 xHCI 控制器发送 Setup TRBs.
//!   Phase E 集成测试需要真实硬件 / QEMU xHCI 模拟器验证.
//! - 不实现 `GET_HID_DESCRIPTOR` / `GET_REPORT` 等类特定请求 (由 USB-1.7 HID 驱动处理).
//!
//! ## TRACK 跟踪
//!
//! - **TRACK-832FCE**: 设备枚举 (本文件消除).

use crate::framework::driver::framework::{DeviceInfo, DeviceType, DriverError, Result};
use super::usb_core::{
    ConfigurationDescriptor, DeviceDescriptor, DeviceState, EndpointDescriptor,
    InterfaceDescriptor, StandardRequest, UsbDevice, UsbSetupPacket, UsbSpeed,
};
use alloc::vec::Vec;

// ============================================================================
// USB Setup Packet 构造辅助
// ============================================================================

/// 构造 `GET_DESCRIPTOR` 请求.
///
/// # 参数
///
/// - `desc_type`: 描述符类型 (USB 规范 §9.6: 1=DEVICE, 2=CONFIGURATION, ...).
/// - `desc_index`: 描述符索引 (对 Device Descriptor 固定为 0).
/// - `length`: 期望读取的字节数.
fn make_get_descriptor_request(desc_type: u8, desc_index: u8, length: u16) -> UsbSetupPacket {
    UsbSetupPacket {
        request_type: 0x80, // 设备到主机 (Device-to-Host), 标准请求 (Standard)
        request: StandardRequest::GetDescriptor as u8,
        value: (u16::from(desc_type) << 8) | u16::from(desc_index),
        index: 0,
        length,
    }
}

/// 构造 `SET_ADDRESS` 请求.
fn make_set_address_request(address: u8) -> UsbSetupPacket {
    UsbSetupPacket {
        request_type: 0x00, // 主机到设备 (Host-to-Device), 标准请求, 设备 (Device)
        request: StandardRequest::SetAddress as u8,
        value: u16::from(address),
        index: 0,
        length: 0,
    }
}

/// 构造 `SET_CONFIGURATION` 请求.
fn make_set_configuration_request(config_value: u8) -> UsbSetupPacket {
    UsbSetupPacket {
        request_type: 0x00,
        request: StandardRequest::SetConfiguration as u8,
        value: u16::from(config_value),
        index: 0,
        length: 0,
    }
}

// ============================================================================
// 描述符解析 (USB-1.6)
// ============================================================================

/// 解析 Device Descriptor (18 字节, USB 规范 §9.6.1).
///
/// # 参数
///
/// - `data`: 至少 18 字节的原始响应数据.
///
/// # 错误
///
/// - `DriverError::InvalidParameter`: data 长度 < 18 或 bLength 不匹配.
/// # Errors
/// data 长度小于 18 或描述符长度/类型字段不匹配时返回 Err。
pub fn parse_device_descriptor(data: &[u8]) -> Result<DeviceDescriptor> {
    if data.len() < 18 {
        return Err(DriverError::InvalidParameter);
    }
    // length 应为 18
    if data[0] != 18 {
        return Err(DriverError::InvalidParameter);
    }
    // descriptor_type 应为 1 (DEVICE)
    if data[1] != 1 {
        return Err(DriverError::InvalidParameter);
    }

    Ok(DeviceDescriptor {
        length: data[0],
        descriptor_type: data[1],
        usb_version: u16::from_le_bytes([data[2], data[3]]),
        device_class: data[4],
        device_subclass: data[5],
        device_protocol: data[6],
        max_packet_size0: data[7],
        vendor_id: u16::from_le_bytes([data[8], data[9]]),
        product_id: u16::from_le_bytes([data[10], data[11]]),
        device_version: u16::from_le_bytes([data[12], data[13]]),
        manufacturer_index: data[14],
        product_index: data[15],
        serial_number_index: data[16],
        num_configurations: data[17],
    })
}

/// 解析 Configuration Descriptor (9 字节头, USB 规范 §9.6.3).
///
/// 返回 `ConfigurationDescriptor` + 包含的所有 Interface / Endpoint 描述符.
/// 调用方应使用 `total_length` 字段判断 Configuration 块完整长度.
/// # Errors
/// data 长度不足或描述符结构非法时返回 Err。
pub fn parse_configuration_descriptor(
    data: &[u8],
) -> Result<(
    ConfigurationDescriptor,
    Vec<InterfaceDescriptor>,
    Vec<EndpointDescriptor>,
)> {
    if data.len() < 9 {
        return Err(DriverError::InvalidParameter);
    }
    if data[0] != 9 || data[1] != 2 {
        return Err(DriverError::InvalidParameter);
    }

    let config = ConfigurationDescriptor {
        length: data[0],
        descriptor_type: data[1],
        total_length: u16::from_le_bytes([data[2], data[3]]),
        num_interfaces: data[4],
        configuration_value: data[5],
        configuration_index: data[6],
        attributes: data[7],
        max_power: data[8],
    };

    let total_length = config.total_length as usize;
    if data.len() < total_length {
        return Err(DriverError::InvalidParameter);
    }

    // 解析后续 Interface / Endpoint 描述符
    let mut interfaces = Vec::new();
    let mut endpoints = Vec::new();
    let mut offset = 9;
    while offset < total_length && offset + 2 <= data.len() {
        let desc_len = data[offset] as usize;
        let desc_type = data[offset + 1];
        if desc_len == 0 || offset + desc_len > data.len() {
            break;
        }
        match desc_type {
            4 => {
                // INTERFACE
                if desc_len >= 9 {
                    interfaces.push(InterfaceDescriptor {
                        length: data[offset],
                        descriptor_type: data[offset + 1],
                        interface_number: data[offset + 2],
                        alternate_setting: data[offset + 3],
                        num_endpoints: data[offset + 4],
                        interface_class: data[offset + 5],
                        interface_subclass: data[offset + 6],
                        interface_protocol: data[offset + 7],
                        interface_index: data[offset + 8],
                    });
                }
            }
            5 => {
                // ENDPOINT
                if desc_len >= 7 {
                    endpoints.push(EndpointDescriptor {
                        length: data[offset],
                        descriptor_type: data[offset + 1],
                        endpoint_address: data[offset + 2],
                        attributes: data[offset + 3],
                        max_packet_size: u16::from_le_bytes([data[offset + 4], data[offset + 5]]),
                        interval: data[offset + 6],
                    });
                }
            }
            _ => {} // 跳过未知描述符 (HID 等类特定描述符由各驱动单独解析)
        }
        offset += desc_len;
    }

    Ok((config, interfaces, endpoints))
}

// ============================================================================
// 设备枚举 (USB-1.6: TRACK-832FCE 消除)
// ============================================================================

/// 通过 default address (0) 读取 Device Descriptor 的请求数据.
///
/// 真实硬件应通过 Setup Stage TRB + Data Stage TRB + Status Stage TRB 发送
/// `make_get_descriptor_request(DEVICE, 0, 18)` 到 root hub 端口.
/// 当前为**软件骨架**, 返回固定 18 字节 mock 数据 (USB 1.1 HID keyboard).
///
/// 注: 真实硬件集成时, 此函数应被替换为 Setup/Data/Status TRB 序列.
fn mock_get_device_descriptor_response() -> [u8; 18] {
    [
        18, // length
        1,  // descriptor_type = DEVICE
        0x10, 0x01, // usb_version = 0x0110 (USB 1.1)
        0x00, // device_class = 0 (由 Interface 决定)
        0x00, // device_subclass
        0x00, // device_protocol
        0x40, // max_packet_size0 = 64
        0xAB, 0x12, // vendor_id = 0x12AB (mock)
        0xCD, 0x34, // product_id = 0x34CD (mock)
        0x00, 0x01, // device_version = 0x0100
        1,    // manufacturer_index
        2,    // product_index
        0,    // serial_number_index
        1,    // num_configurations = 1
    ]
}

/// 通过已分配地址读取 Configuration Descriptor 的请求数据.
///
/// 真实硬件应通过 Control Transfer 发送
/// `make_get_descriptor_request(CONFIGURATION, 0, total_length)`.
/// 当前返回 mock Configuration Descriptor (9 字节) + 1 Interface (9 字节) +
/// 1 IN Endpoint (7 字节) = 25 字节.
fn mock_get_configuration_descriptor_response() -> Vec<u8> {
    let mut data = Vec::new();
    // Configuration Descriptor (9 字节)
    data.extend_from_slice(&[
        9, // length
        2, // descriptor_type = 配置描述符 (CONFIGURATION)
        25, 0,    // total_length = 25 (9 + 9 + 7)
        1,    // num_interfaces
        1,    // configuration_value
        0,    // configuration_index
        0x80, // attributes (bus-powered)
        50,   // max_power (100 mA)
    ]);
    // Interface Descriptor (9 字节)
    data.extend_from_slice(&[
        9,    // length
        4,    // descriptor_type = INTERFACE
        0,    // interface_number
        0,    // alternate_setting
        1,    // num_endpoints
        0x03, // interface_class = HID
        0x01, // interface_subclass = 启动接口子类 (Boot Interface Subclass)
        0x01, // interface_protocol = 键盘 (Keyboard)
        0,    // interface_index
    ]);
    // Endpoint Descriptor (7 字节, IN interrupt endpoint)
    data.extend_from_slice(&[
        7,    // length
        5,    // descriptor_type = ENDPOINT
        0x81, // endpoint_address (IN, EP1)
        0x03, // attributes (Interrupt)
        0x08, 0x00, // max_packet_size = 8
        0x0A, // interval = 10 ms
    ]);
    data
}

/// 枚举连接在指定端口的新 USB 设备 (USB-1.6: TRACK-832FCE 消除).
///
/// # 参数
///
/// - `port`: USB 端口号 (1-based, 与 xHCI port register 索引一致).
/// - `speed`: 端口速度 (由 xHCI PORTSC 寄存器读取).
/// - `allocate_address`: 闭包, 用于从 `HostController` 分配新地址.
///   真实场景调用 `XhciController::allocate_address`;
///   单测可注入 mock 闭包.
///
/// # 流程
///
/// 1. `GET_DESCRIPTOR` Device → 解析 Device Descriptor
/// 2. 分配地址 (`allocate_address`)
/// 3. `SET_ADDRESS` → 切换设备地址
/// 4. `GET_DESCRIPTOR` Configuration → 解析 Configuration + Interface + Endpoint
/// 5. `SET_CONFIGURATION(1)` → 选择 Configuration 1
/// 6. 返回 `UsbDevice { address, speed, descriptor, interfaces, endpoints, state }`
///
/// # 限制
///
/// - 当前为**软件骨架**: 真实硬件应替换 mock 函数为 Control Transfer (Setup/Data/Status TRB).
/// - 不处理多 Configuration / 多 Interface 设备的复杂枚举.
/// # Errors
/// 描述符解析失败、地址分配失败或分配的地址非法时返回 Err。
pub fn enumerate_new_device<F>(
    port: usize,
    speed: UsbSpeed,
    mut allocate_address: F,
) -> Result<UsbDevice>
where
    F: FnMut() -> Result<u8>,
{
    let _ = port; // 当前未使用 (mock 数据无 port 信息), 真实硬件应通过 port 发送 Setup TRB.

    // 1. 获取描述符 Device (mock 模拟)
    let _req1 = make_get_descriptor_request(1, 0, 18);
    let device_data = mock_get_device_descriptor_response();
    let descriptor = parse_device_descriptor(&device_data)?;

    // 2. 分配地址
    let address = allocate_address()?;
    if address == 0 || address == 255 {
        return Err(DriverError::InvalidParameter);
    }

    // 3. SET_ADDRESS (mock: 仅构造请求, 真实硬件通过 Setup TRB 发送)
    let _req2 = make_set_address_request(address);

    // 4. 获取描述符 Configuration (mock 模拟)
    let _req3 = make_get_descriptor_request(2, 0, 25);
    let config_data = mock_get_configuration_descriptor_response();
    let (config_desc, interfaces, endpoints) = parse_configuration_descriptor(&config_data)?;

    // 5. SET_CONFIGURATION (mock)
    let _req4 = make_set_configuration_request(config_desc.configuration_value);

    // 6. 构造 UsbDevice
    // 注: DeviceInfo.name 是 `&'static str`, 端口号运行时确定; 暂用通用名.
    //     Phase E 集成时可通过 UsbCore 维护静态字符串池.
    //     DeviceType::Other 用于表示通用 USB 设备 (非 Block/Char/Network 等).
    let _ = port;
    let info = DeviceInfo::new("usb-device", DeviceType::Other);

    Ok(UsbDevice {
        id: 0, // 由 UsbCore 分配 (暂占位)
        address,
        speed,
        state: DeviceState::Configured,
        descriptor,
        configuration: Some(config_desc.configuration_value),
        interfaces,
        endpoints,
        info,
    })
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_parse_device_descriptor_valid() {
        let data = [
            18, 1, 0x10, 0x01, 0x00, 0x00, 0x00, 0x40, 0xAB, 0x12, 0xCD, 0x34, 0x00, 0x01, 1, 2, 0,
            1,
        ];
        let desc = parse_device_descriptor(&data).unwrap();
        // 描述符结构为 packed: u16 字段取引用会触发 E0793, 故按值取出再断言.
        assert_eq!({ desc.length }, 18);
        assert_eq!({ desc.descriptor_type }, 1);
        assert_eq!({ desc.usb_version }, 0x0110);
        assert_eq!({ desc.vendor_id }, 0x12AB);
        assert_eq!({ desc.product_id }, 0x34CD);
        assert_eq!({ desc.num_configurations }, 1);
    }

    #[test]
    fn test_parse_device_descriptor_too_short() {
        let data = [0u8; 10];
        assert!(parse_device_descriptor(&data).is_err());
    }

    #[test]
    fn test_parse_device_descriptor_invalid_length() {
        let mut data = [0u8; 18];
        data[0] = 17; // wrong length
        assert!(parse_device_descriptor(&data).is_err());
    }

    #[test]
    fn test_parse_device_descriptor_invalid_type() {
        let mut data = [0u8; 18];
        data[0] = 18;
        data[1] = 2; // wrong descriptor_type
        assert!(parse_device_descriptor(&data).is_err());
    }

    #[test]
    fn test_parse_configuration_descriptor_valid() {
        // 9 (config 配置描述符) + 9 (interface 接口描述符) + 7 (endpoint 端点描述符) = 25
        let mut data = vec![0u8; 25];
        data[0] = 9;
        data[1] = 2;
        data[2] = 25;
        data[3] = 0;
        data[4] = 1; // num_interfaces
        data[5] = 1; // configuration_value
        data[8] = 50; // max_power
        // Interface (标准 USB 接口描述符字段偏移: bNumEndpoints 在 +4, bInterfaceClass 在 +5)
        // (原 fixture 把二者写在 +5/+6, 与 parse_configuration_descriptor 的解析偏移差 1; 2026-09-24 UT-06 修正)
        data[9] = 9;
        data[10] = 4;
        data[11] = 0; // interface_number
        data[13] = 1; // num_endpoints
        data[14] = 0x03; // HID class
        // Endpoint
        data[18] = 7;
        data[19] = 5;
        data[20] = 0x81; // IN EP1
        data[22] = 8; // max_packet_size (u16 LE 低字节)

        let (config, ifaces, eps) = parse_configuration_descriptor(&data).unwrap();
        assert_eq!(config.num_interfaces, 1);
        assert_eq!(config.configuration_value, 1);
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].interface_class, 0x03); // HID
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].endpoint_address, 0x81);
    }

    #[test]
    fn test_parse_configuration_descriptor_too_short() {
        let data = [0u8; 5];
        assert!(parse_configuration_descriptor(&data).is_err());
    }

    #[test]
    fn test_parse_configuration_descriptor_wrong_type() {
        let mut data = vec![0u8; 25];
        data[0] = 9;
        data[1] = 1; // 错误: 应为 2 (配置描述符 CONFIGURATION)
        assert!(parse_configuration_descriptor(&data).is_err());
    }

    #[test]
    fn test_make_get_descriptor_request_device() {
        let req = make_get_descriptor_request(1, 0, 18);
        // 请求结构为 packed: u16 字段取引用会触发 E0793, 故按值取出再断言.
        assert_eq!({ req.request_type }, 0x80);
        assert_eq!({ req.request }, 6); // GetDescriptor
        assert_eq!({ req.value }, 1 << 8); // desc_type=设备描述符 (DEVICE), 位于高字节
        assert_eq!({ req.length }, 18);
    }

    #[test]
    fn test_make_set_address_request() {
        let req = make_set_address_request(42);
        assert_eq!({ req.request_type }, 0x00);
        assert_eq!({ req.request }, 5); // SetAddress
        assert_eq!({ req.value }, 42);
        assert_eq!({ req.length }, 0);
    }

    #[test]
    fn test_make_set_configuration_request() {
        let req = make_set_configuration_request(1);
        assert_eq!({ req.request_type }, 0x00);
        assert_eq!({ req.request }, 9); // SetConfiguration
        assert_eq!({ req.value }, 1);
        assert_eq!({ req.length }, 0);
    }

    #[test]
    fn test_enumerate_new_device_returns_device() {
        let mut next_addr = 5u8;
        let result = enumerate_new_device(1, UsbSpeed::High, || {
            let a = next_addr;
            next_addr += 1;
            Ok(a)
        });
        assert!(result.is_ok());
        let dev = result.unwrap();
        assert_eq!(dev.address, 5);
        assert_eq!(dev.speed, UsbSpeed::High);
        assert_eq!(dev.state, DeviceState::Configured);
        assert_eq!({ dev.descriptor.vendor_id }, 0x12AB);
        assert_eq!({ dev.descriptor.product_id }, 0x34CD);
        assert_eq!(dev.interfaces.len(), 1);
        assert_eq!(dev.interfaces[0].interface_class, 0x03); // HID
        assert_eq!(dev.endpoints.len(), 1);
        assert_eq!(dev.endpoints[0].endpoint_address, 0x81);
        assert_eq!(dev.configuration, Some(1));
    }

    #[test]
    fn test_enumerate_new_device_address_allocation_error() {
        let result = enumerate_new_device(1, UsbSpeed::Full, || Err(DriverError::Busy));
        assert!(matches!(result, Err(DriverError::Busy)));
    }
}
