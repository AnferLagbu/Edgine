#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! USB 核心 — functions 层权威实装 (Phase 2.1.6)
//!
//! USB 子系统核心类型与设备管理: 描述符、URB、`HostController` Trait、`UsbDevice`、`UsbCore`.
//!
//! ## 安全设计
//!
//! - **URB 缓冲区**: `Urb` 不持有裸指针, 以「物理地址 + 长度 + 方向」描述 DMA 缓冲区;
//!   缓冲区由调用方经 privileged DMA 分配并保持存活, 拷贝经 privileged safe wrapper 完成.
//! - **控制器所有权**: `UsbCore` 以 `Box<dyn HostController>` 独占持有控制器;
//!   枚举时 `mem::take` 借用后无条件归还, 无 `*mut` 别名.
//!
//! ## 导出类型
//!
//! - `UsbSpeed` / `DeviceState` / `DeviceClass` — 枚举常量
//! - `DeviceDescriptor` / `ConfigurationDescriptor` / `InterfaceDescriptor` / `EndpointDescriptor` — 描述符
//! - `UsbSetupPacket` / `StandardRequest` — USB 请求
//! - `Urb` / `UrbStatus` — USB 请求块
//! - `HostController` — 主机控制器 Trait
//! - `UsbDevice` — 设备实例

use crate::privileged::driver::infra::{DeviceInfo, DeviceType, Driver, DriverError, Result};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

// ============================================================================
// USB 常量定义
// ============================================================================

/// USB描述符类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DescriptorType {
    /// 设备描述符, 给出 VID/PID、类别与端点 0 包长等整机属性.
    Device = 1,
    /// 配置描述符, 描述一组接口及其供电参数.
    Configuration = 2,
    /// 字符串描述符, 按索引承载厂商/产品/序列号等可读文本.
    String = 3,
    /// 接口描述符, 声明一个功能接口的类别与端点数.
    Interface = 4,
    /// 端点描述符, 声明端点地址、传输类型与包长.
    Endpoint = 5,
    /// 设备限定符描述符, 说明高速设备在另一速度下的能力.
    DeviceQualifier = 6,
    /// 其他速度配置描述符, 说明设备在另一速度下的配置.
    OtherSpeedConfig = 7,
    /// 接口电源描述符, 给出接口级别的省电参数.
    InterfacePower = 8,
    /// HID 描述符, 声明 HID 规范版本与报告描述符长度.
    Hid = 0x21,
    /// HID 报告描述符, 定义输入/输出/特性报告的数据布局.
    HidReport = 0x22,
    /// HID 物理描述符, 把报告项映射到物理量纲.
    HidPhysical = 0x23,
}

/// USB传输类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransferType {
    /// 控制传输, 承载标准/类/厂商请求, 走控制管道.
    Control = 0,
    /// 等时传输, 预留带宽但不重传, 用于音视频流.
    Isochronous = 1,
    /// 批量传输, 保证数据完整、可容忍延迟, 用于存储类.
    Bulk = 2,
    /// 中断传输, 以有界延迟轮询小数据包, 用于 HID 等.
    Interrupt = 3,
}

/// USB方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Direction {
    /// 主机到设备 (写设备).
    Out = 0,
    /// 设备到主机 (读设备); 对应端点地址最高位置 1.
    In = 0x80,
}

/// USB速度
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbSpeed {
    /// 速度未知, 端口尚未完成速度协商.
    Unknown,
    /// 低速 1.5 Mbps (USB 1.0).
    Low,
    /// 全速 12 Mbps (USB 1.1).
    Full,
    /// 高速 480 Mbps (USB 2.0).
    High,
    /// 超速 5 Gbps (USB 3.0).
    Super,
    /// 超速+ 10 Gbps (USB 3.1).
    SuperPlus,
}

impl UsbSpeed {
    /// 返回该速度的理论带宽 (Mbps); 未知速度返回 0.
    pub fn bandwidth_mbps(self) -> u32 {
        match self {
            Self::Low => 1,
            Self::Full => 12,
            Self::High => 480,
            Self::Super => 5000,
            Self::SuperPlus => 10000,
            Self::Unknown => 0,
        }
    }
}

/// USB设备状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 端口无设备.
    NotAttached,
    /// 设备已连接但尚未上电.
    Attached,
    /// 已上电, 等待复位与地址分配.
    Powered,
    /// 已复位并使用默认地址 0, 尚未分配正式地址.
    Default,
    /// 已分配非零地址, 尚未选定配置.
    Addressed,
    /// 已选定配置, 接口与端点可用.
    Configured,
    /// 总线挂起, 设备进入低功耗状态.
    Suspended,
}

// ============================================================================
// USB描述符结构
// ============================================================================

/// USB设备描述符 (18字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct DeviceDescriptor {
    /// 描述符自身长度, 固定为 18 字节.
    pub length: u8,
    /// 描述符类型, 恒为 DescriptorType::Device.
    pub descriptor_type: u8,
    /// USB 规范版本号 (BCD 编码, 如 0x0200 代表 USB 2.0).
    pub usb_version: u16,
    /// 设备级类别代码; 若为 0 则类别由接口指定.
    pub device_class: u8,
    /// 设备级子类代码, 配合类别使用.
    pub device_subclass: u8,
    /// 设备级协议代码, 配合子类使用.
    pub device_protocol: u8,
    /// 端点 0 的最大包长 (8/16/32/64, 由速度决定).
    pub max_packet_size0: u8,
    /// 厂商 ID (VID), 由 USB-IF 分配.
    pub vendor_id: u16,
    /// 产品 ID (PID), 由厂商自行分配.
    pub product_id: u16,
    /// 设备固件版本号 (BCD 编码).
    pub device_version: u16,
    /// 厂商名字符串描述符的索引; 0 表示无.
    pub manufacturer_index: u8,
    /// 产品名字符串描述符的索引; 0 表示无.
    pub product_index: u8,
    /// 序列号字符串描述符的索引; 0 表示无.
    pub serial_number_index: u8,
    /// 该设备支持的配置总数.
    pub num_configurations: u8,
}

impl Default for DeviceDescriptor {
    fn default() -> Self {
        Self {
            length: 18,
            descriptor_type: DescriptorType::Device as u8,
            usb_version: 0x0200,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            max_packet_size0: 64,
            vendor_id: 0,
            product_id: 0,
            device_version: 0x0100,
            manufacturer_index: 0,
            product_index: 0,
            serial_number_index: 0,
            num_configurations: 1,
        }
    }
}

/// USB配置描述符 (9字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct ConfigurationDescriptor {
    /// 描述符自身长度, 固定为 9 字节.
    pub length: u8,
    /// 描述符类型, 恒为 DescriptorType::Configuration.
    pub descriptor_type: u8,
    /// 该配置下全部描述符 (配置 + 接口 + 端点等) 的总字节数.
    pub total_length: u16,
    /// 该配置包含的接口数量.
    pub num_interfaces: u8,
    /// SetConfiguration 请求使用的配置标识值 (从 1 开始).
    pub configuration_value: u8,
    /// 配置名称字符串描述符的索引; 0 表示无.
    pub configuration_index: u8,
    /// 特性位: D7=总线供电, D6=自供电, D5=支持远程唤醒.
    pub attributes: u8,
    /// 总线供电时设备消耗的最大电流, 单位 2 mA.
    pub max_power: u8,
}

/// USB接口描述符 (9字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct InterfaceDescriptor {
    /// 描述符自身长度, 固定为 9 字节.
    pub length: u8,
    /// 描述符类型, 恒为 DescriptorType::Interface.
    pub descriptor_type: u8,
    /// 接口编号 (从 0 开始).
    pub interface_number: u8,
    /// 该接口的备用设置编号; 0 表示默认设置.
    pub alternate_setting: u8,
    /// 该接口使用的端点数 (不含端点 0).
    pub num_endpoints: u8,
    /// 接口类别代码 (HID/大容量存储/音频等).
    pub interface_class: u8,
    /// 接口子类代码, 配合类别使用.
    pub interface_subclass: u8,
    /// 接口协议代码, 配合子类使用.
    pub interface_protocol: u8,
    /// 接口名称字符串描述符的索引; 0 表示无.
    pub interface_index: u8,
}

/// USB端点描述符 (7字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct EndpointDescriptor {
    /// 描述符自身长度, 固定为 7 字节.
    pub length: u8,
    /// 描述符类型, 恒为 DescriptorType::Endpoint.
    pub descriptor_type: u8,
    /// 端点地址: D7=方向 (0=OUT, 1=IN), D3..D0=端点编号.
    pub endpoint_address: u8,
    /// 端点属性: D1..D0 选传输类型 (Control/Isochronous/Bulk/Interrupt).
    pub attributes: u8,
    /// 该端点在非控制传输下的最大数据包长度 (以字节为单位).
    pub max_packet_size: u16,
    /// 中断/等时端点的轮询间隔 (单位为帧/微帧).
    pub interval: u8,
}

impl EndpointDescriptor {
    /// 从端点地址提取传输方向.
    pub fn direction(self) -> Direction {
        if self.endpoint_address & 0x80 != 0 {
            Direction::In
        } else {
            Direction::Out
        }
    }

    /// 从端点地址提取端点编号 (0..15).
    pub fn number(self) -> u8 {
        self.endpoint_address & 0x0F
    }

    /// 从端点属性提取传输类型.
    pub fn transfer_type(self) -> TransferType {
        match self.attributes & 0x03 {
            1 => TransferType::Isochronous,
            2 => TransferType::Bulk,
            3 => TransferType::Interrupt,
            // 未识别传输类型 (含 0 = Control) 回退为 Control
            _ => TransferType::Control,
        }
    }
}

// ============================================================================
// USB设备类代码
// ============================================================================

/// USB设备类代码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceClass {
    /// 未识别或设备未声明类别.
    Unknown = 0x00,
    /// 音频类 (扬声器/麦克风等).
    Audio = 0x01,
    /// 通信类 (调制解调器/网络控制等).
    Communications = 0x02,
    /// 人机接口类 (键盘/鼠标/游戏手柄).
    Hid = 0x03,
    /// 物理设备类 (力反馈等).
    Physical = 0x05,
    /// 图像类 (相机/扫描仪).
    Image = 0x06,
    /// 打印机类.
    Printer = 0x07,
    /// 大容量存储类 (U 盘/移动硬盘).
    MassStorage = 0x08,
    /// 集线器类.
    Hub = 0x09,
    /// 数据类 (通信类的数据接口).
    Data = 0x0A,
    /// 智能卡类.
    SmartCard = 0x0B,
    /// 视频类 (摄像头/视频采集).
    Video = 0x0E,
    /// 无线控制器类 (蓝牙/WiFi 适配器).
    WirelessController = 0xE0,
    /// 杂项类 (复合设备的公共接口).
    Miscellaneous = 0xEF,
    /// 应用特定类, 语义由具体应用规范定义.
    ApplicationSpecific = 0xFE,
    /// 厂商自定义类.
    VendorSpecific = 0xFF,
}

impl From<u8> for DeviceClass {
    fn from(value: u8) -> Self {
        match value {
            0x01 => Self::Audio,
            0x02 => Self::Communications,
            0x03 => Self::Hid,
            0x05 => Self::Physical,
            0x06 => Self::Image,
            0x07 => Self::Printer,
            0x08 => Self::MassStorage,
            0x09 => Self::Hub,
            0x0A => Self::Data,
            0x0B => Self::SmartCard,
            0x0E => Self::Video,
            0xE0 => Self::WirelessController,
            0xEF => Self::Miscellaneous,
            0xFE => Self::ApplicationSpecific,
            0xFF => Self::VendorSpecific,
            _ => Self::Unknown,
        }
    }
}

// ============================================================================
// USB请求块 (URB)
// ============================================================================

/// USB标准请求
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct UsbSetupPacket {
    /// 请求类型字节: D7=方向, D6..D5=类型, D4..D0=接收者.
    pub request_type: u8,
    /// 请求代码; 标准请求见 StandardRequest.
    pub request: u8,
    /// 请求参数 wValue, 含义随请求而定.
    pub value: u16,
    /// 请求参数 wIndex, 通常为接口号或端点号.
    pub index: u16,
    /// 数据阶段期望的字节数 wLength.
    pub length: u16,
}

/// 标准请求代码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StandardRequest {
    /// 查询设备/接口/端点的状态位.
    GetStatus = 0,
    /// 清除指定端点的特性位 (如远程唤醒).
    ClearFeature = 1,
    /// 置位指定端点的特性位.
    SetFeature = 3,
    /// 为设备分配 USB 地址.
    SetAddress = 5,
    /// 读回指定类型的描述符.
    GetDescriptor = 6,
    /// 写入描述符 (仅少数设备支持).
    SetDescriptor = 7,
    /// 查询当前配置值.
    GetConfiguration = 8,
    /// 选定设备配置.
    SetConfiguration = 9,
    /// 查询接口当前的备用设置.
    GetInterface = 10,
    /// 切换接口的备用设置.
    SetInterface = 11,
    /// 读取等时端点的同步帧号.
    SynchFrame = 12,
}

/// URB状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrbStatus {
    /// 已构造但尚未提交给控制器.
    Idle,
    /// 已提交, 等待控制器完成.
    Pending,
    /// 传输已成功完成.
    Completed,
    /// 传输失败 (控制器回报错误).
    Error,
    /// 传输被调用方取消.
    Cancelled,
}

/// USB请求块 (URB)
///
/// 缓冲区以「物理地址 + 长度 + 方向」描述: 调用方经 privileged DMA 分配缓冲区,
/// 并保证其在 `submit_urb` 期间存活; 控制器通过物理地址直接 DMA 读写.
pub struct Urb {
    /// URB 标识, 用于关联提交、完成与取消.
    pub id: u32,
    /// 目标设备的 USB 地址 (0 为默认地址).
    pub device: u8,
    /// 目标端点编号; 传输方向由 `direction` 单独给出.
    pub endpoint: u8,
    /// 控制传输的 8 字节建立包; 非控制传输为 `None`.
    pub setup: Option<UsbSetupPacket>,
    /// DMA 缓冲区物理地址 (0 = 无数据阶段)
    pub buffer_phys: u64,
    /// 缓冲区字节数 (0 = 无数据阶段)
    pub buffer_length: usize,
    /// 传输方向 (决定 DCI 的 IN/OUT 位)
    pub direction: Direction,
    /// 控制器回报的实际传输字节数.
    pub actual_length: usize,
    /// URB 当前所处的状态.
    pub status: UrbStatus,
    /// 完成回调 (中断/软中断上下文调用, 只读 URB)
    pub callback: Option<fn(&Self)>,
}

// ============================================================================
// USB设备结构
// ============================================================================

/// USB设备
pub struct UsbDevice {
    /// 设备ID
    pub id: u32,
    /// USB地址 (1-127)
    pub address: u8,
    /// 设备速度
    pub speed: UsbSpeed,
    /// 设备状态
    pub state: DeviceState,
    /// 设备描述符
    pub descriptor: DeviceDescriptor,
    /// 当前配置
    pub configuration: Option<u8>,
    /// 接口列表
    pub interfaces: Vec<InterfaceDescriptor>,
    /// 端点列表
    pub endpoints: Vec<EndpointDescriptor>,
    /// 设备信息
    pub info: DeviceInfo,
}

impl UsbDevice {
    /// 以给定 ID 创建未连接状态的设备实例, 其余字段取默认值.
    pub fn new(id: u32) -> Self {
        Self {
            id,
            address: 0,
            speed: UsbSpeed::Unknown,
            state: DeviceState::NotAttached,
            descriptor: DeviceDescriptor::default(),
            configuration: None,
            interfaces: Vec::new(),
            endpoints: Vec::new(),
            info: DeviceInfo::new("usb_device", DeviceType::Other),
        }
    }

    /// 由设备描述符的类别字节解析设备类.
    pub fn device_class(&self) -> DeviceClass {
        DeviceClass::from(self.descriptor.device_class)
    }

    /// 返回设备描述符中的厂商 ID.
    pub fn vendor_id(&self) -> u16 {
        self.descriptor.vendor_id
    }

    /// 返回设备描述符中的产品 ID.
    pub fn product_id(&self) -> u16 {
        self.descriptor.product_id
    }
}

// ============================================================================
// USB主机控制器接口
// ============================================================================

/// USB主机控制器 Trait
pub trait HostController: Driver {
    /// 获取控制器支持的USB速度
    fn supported_speeds(&self) -> Vec<UsbSpeed>;

    /// 获取根集线器端口数量
    fn num_ports(&self) -> usize;

    /// 检测端口是否有设备连接
    fn port_has_device(&self, port: usize) -> bool;

    /// 复位端口
    /// # Errors
    /// 端口复位操作失败时返回 Err。
    fn reset_port(&mut self, port: usize) -> Result<()>;

    /// 获取端口设备速度
    fn get_port_speed(&self, port: usize) -> UsbSpeed;

    /// 提交URB
    /// # Errors
    /// URB 提交失败时返回 Err。
    fn submit_urb(&mut self, urb: &Urb) -> Result<()>;

    /// 取消URB
    /// # Errors
    /// 取消操作失败时返回 Err。
    fn cancel_urb(&mut self, urb_id: u32) -> Result<()>;

    /// 分配设备地址
    /// # Errors
    /// 可用地址耗尽时返回 Err。
    fn allocate_address(&mut self) -> Result<u8>;

    /// 释放设备地址
    fn free_address(&mut self, address: u8);
}

// ============================================================================
// USB核心管理器
// ============================================================================

/// 全局设备ID分配器
static NEXT_USB_DEVICE_ID: AtomicU32 = AtomicU32::new(1);

/// USB核心管理器
pub struct UsbCore {
    /// 已连接的设备列表
    devices: Vec<UsbDevice>,
    /// 主机控制器列表 (独占所有权)
    controllers: Vec<Box<dyn HostController>>,
    /// 是否已初始化
    initialized: bool,
}

impl UsbCore {
    /// 创建空的核心管理器: 无已连接设备、无主机控制器、未初始化.
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            controllers: Vec::new(),
            initialized: false,
        }
    }

    /// 注册主机控制器 (接管其所有权)
    pub fn register_controller(&mut self, controller: Box<dyn HostController>) {
        self.controllers.push(controller);
    }

    /// 枚举所有设备
    /// # Errors
    /// 某个端口上的设备枚举失败时返回 Err。
    pub fn enumerate_devices(&mut self) -> Result<()> {
        // 暂时移出控制器集合, 避免与 `&mut self` 的字段借用冲突;
        // 无论成功或失败都无条件归还, 保证控制器不丢失.
        let mut controllers = core::mem::take(&mut self.controllers);
        let mut result = Ok(());
        'outer: for controller in &mut controllers {
            for port in 0..controller.num_ports() {
                if controller.port_has_device(port) {
                    if let Err(e) = self.enumerate_port(controller.as_mut(), port) {
                        result = Err(e);
                        break 'outer;
                    }
                }
            }
        }
        self.controllers = controllers;
        result
    }

    /// 枚举单个端口
    fn enumerate_port(&mut self, controller: &mut dyn HostController, port: usize) -> Result<()> {
        // 复位端口
        controller.reset_port(port)?;

        // 获取设备速度
        let speed = controller.get_port_speed(port);

        // 创建新设备
        let device_id = NEXT_USB_DEVICE_ID.fetch_add(1, Ordering::Relaxed);
        let mut device = UsbDevice::new(device_id);
        device.speed = speed;
        device.state = DeviceState::Powered;

        // 分配地址
        let address = controller.allocate_address()?;
        device.address = address;
        device.state = DeviceState::Addressed;

        // 获取设备描述符
        self.get_device_descriptor(controller, &mut device)?;

        // 配置设备
        self.configure_device(controller, &mut device)?;

        // 添加到设备列表
        self.devices.push(device);

        Ok(())
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 获取设备描述符
    fn get_device_descriptor(
        &self,
        controller: &mut dyn HostController,
        device: &mut UsbDevice,
    ) -> Result<()> {
        let setup = UsbSetupPacket {
            request_type: 0x80,
            request: StandardRequest::GetDescriptor as u8,
            value: (DescriptorType::Device as u16) << 8,
            index: 0,
            length: 18,
        };

        let desc_size = core::mem::size_of::<DeviceDescriptor>();

        // 设备写入目标: 经 privileged DMA 分配 (设备可访问物理地址)
        let mut dma = crate::privileged::driver::storage::nvme_alloc_dma_buffer(desc_size)
            .ok_or(DriverError::BufferTooSmall)?;
        crate::privileged::driver::storage::nvme_zero_dma(
            dma.cpu_addr().as_ptr() as u64,
            dma.size(),
        );

        let urb = Urb {
            id: 0,
            device: device.address,
            endpoint: 0,
            setup: Some(setup),
            buffer_phys: dma.dma_addr().as_u64(),
            buffer_length: desc_size,
            direction: Direction::In,
            actual_length: 0,
            status: UrbStatus::Idle,
            callback: None,
        };

        controller.submit_urb(&urb)?;

        // 设备 DMA 写入后同步回 CPU, 再拷贝到栈上描述符
        let _ = dma.sync_for_cpu();
        let mut descriptor = DeviceDescriptor::default();
        crate::privileged::driver::storage::nvme_copy_from_dma(
            core::ptr::addr_of_mut!(descriptor).cast::<u8>(),
            dma.cpu_addr().as_ptr() as u64,
            desc_size,
        );

        device.descriptor = descriptor;

        Ok(())
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 配置设备
    fn configure_device(
        &self,
        controller: &mut dyn HostController,
        device: &mut UsbDevice,
    ) -> Result<()> {
        let setup = UsbSetupPacket {
            request_type: 0x00,
            request: StandardRequest::SetConfiguration as u8,
            value: 1,
            index: 0,
            length: 0,
        };

        let urb = Urb {
            id: 0,
            device: device.address,
            endpoint: 0,
            setup: Some(setup),
            buffer_phys: 0,
            buffer_length: 0,
            direction: Direction::Out,
            actual_length: 0,
            status: UrbStatus::Idle,
            callback: None,
        };

        controller.submit_urb(&urb)?;

        device.configuration = Some(1);
        device.state = DeviceState::Configured;

        Ok(())
    }

    /// 根据类查找设备
    pub fn find_device_by_class(&self, class: DeviceClass) -> Option<&UsbDevice> {
        self.devices.iter().find(|d| d.device_class() == class)
    }

    /// 根据VID/PID查找设备
    pub fn find_device_by_vid_pid(&self, vid: u16, pid: u16) -> Option<&UsbDevice> {
        self.devices
            .iter()
            .find(|d| d.vendor_id() == vid && d.product_id() == pid)
    }

    /// 获取设备数量
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }
}

impl Driver for UsbCore {
    fn name(&self) -> &'static str {
        "USB Core"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Bus
    }

    fn init(&mut self) -> Result<()> {
        self.enumerate_devices()?;
        self.initialized = true;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        self.devices.clear();
        self.initialized = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.initialized
    }

    fn status(&self) -> &'static str {
        if self.initialized {
            "USB Core ready"
        } else {
            "USB Core not initialized"
        }
    }
}

impl Default for UsbCore {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usb_speed_bandwidth() {
        assert_eq!(UsbSpeed::Low.bandwidth_mbps(), 1);
        assert_eq!(UsbSpeed::Full.bandwidth_mbps(), 12);
        assert_eq!(UsbSpeed::High.bandwidth_mbps(), 480);
        assert_eq!(UsbSpeed::Super.bandwidth_mbps(), 5000);
    }

    #[test]
    fn test_device_descriptor_default() {
        let desc = DeviceDescriptor::default();
        assert_eq!(desc.length, 18);
        assert_eq!(desc.descriptor_type, 1);
        assert_eq!(desc.max_packet_size0, 64);
    }

    #[test]
    fn test_device_class_from_u8() {
        assert_eq!(DeviceClass::from(0x03), DeviceClass::Hid);
        assert_eq!(DeviceClass::from(0x08), DeviceClass::MassStorage);
        assert_eq!(DeviceClass::from(0x09), DeviceClass::Hub);
        assert_eq!(DeviceClass::from(0xFF), DeviceClass::VendorSpecific);
    }

    #[test]
    fn test_usb_device_creation() {
        let device = UsbDevice::new(1);
        assert_eq!(device.id, 1);
        assert_eq!(device.address, 0);
        assert_eq!(device.state, DeviceState::NotAttached);
    }

    #[test]
    fn test_usb_core_creation() {
        let core = UsbCore::new();
        assert_eq!(core.device_count(), 0);
        assert!(!core.is_ready());
    }
}
