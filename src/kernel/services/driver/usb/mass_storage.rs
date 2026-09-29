#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! USB 大容量存储类驱动 (BBB / Bulk-Only Transport 仅批量传输) - USB-1.8
//!
//! 实现 USB Mass Storage Class - Bulk-Only Transport (BBB) 1.0 + SCSI 透明命令集:
//!
//! - **CBW (Command Block Wrapper)**: 31 字节, Host → Device
//! - **CSW (Command Status Wrapper)**: 13 字节, Device → Host
//! - **SCSI 命令**: TEST UNIT READY / INQUIRY / READ CAPACITY (10) / READ (10) / WRITE (10) / REQUEST SENSE
//! - **Bulk-Only 传输**: Bulk-OUT 端点发送 CBW, Bulk-IN 接收数据 + CSW
//!
//! ## 协议流程 (USB MSC BBB §3.1)
//!
//! ```text
//! Host                                  Device
//!   |--- CBW (Bulk-OUT, 31 bytes) ------>|
//!   |<-- Data In (Bulk-IN)  -------------|  (data phase, optional)
//!   |-- Data Out (Bulk-OUT) ------------>|  (data phase, optional)
//!   |<-- CSW (Bulk-IN, 13 bytes) --------|  (status phase)
//! ```
//!
//! ## 限制
//!
//! - 当前为**软件骨架**: 不通过真实 xHCI Bulk 端点发送 CBW
//! - 不实现 CBI (Control/Bulk/Interrupt) 传输 (BBB only)
//! - 不实现多 LUN (单 LUN = 0)
//! - 不实现 WRITE(10) / WRITE(6) (只读)
//!
//! See also:
//! - `usb/enumerate.rs` USB-1.6 (找到 `MassStorage` Interface 后调用本驱动)
//! - `usb/ring.rs` USB-1.5 (Bulk 端点 TRB 提交使用 Command Ring)
//! - `services/driver/storage/ahci.rs` (ATA/SCSI 命令分发, 类似结构)

use super::usb_core::{DeviceClass, UsbDevice};
use crate::framework::driver::framework::{DriverError, Result};

// ============================================================================
// CBW / CSW 结构 (USB MSC BBB §3.1, §3.2)
// ============================================================================

/// CBW 魔术字 ("USBC" little-endian).
pub const CBW_SIGNATURE: u32 = 0x4342_5553;
/// CSW 魔术字 ("USBS" little-endian).
pub const CSW_SIGNATURE: u32 = 0x5342_5553;
/// CBW 总长度 (固定 31 字节).
pub const CBW_LENGTH: usize = 31;
/// CSW 总长度 (固定 13 字节).
pub const CSW_LENGTH: usize = 13;
/// SCSI Command Block 最大长度 (16 字节).
pub const SCSI_CB_MAX_LENGTH: usize = 16;

/// Command Block Wrapper (31 字节, USB MSC BBB §3.1).
///
/// 字节布局 (little-endian):
/// - bytes 0..=3:  dCBWSignature (固定 0x43425553 = "USBC")
/// - bytes 4..=7:  dCBWTag (Host 分配, 必须与 CSW dCSWTag 匹配)
/// - bytes 8..=11: dCBWDataTransferLength (本事务数据阶段字节数)
/// - byte 12:      bmCBWFlags (bit 7: 0=OUT, 1=IN)
/// - byte 13:      bCBWLUN (Logical Unit Number, 通常为 0)
/// - byte 14:      bCBWCBLength (SCSI CB 长度, 1-16)
/// - bytes 15..=30: CBWCB (SCSI 命令块)
#[derive(Debug, Clone, Copy)]
pub struct CommandBlockWrapper {
    /// Host 分配的唯一 tag, 必须与对应 CSW 的 dCSWTag 匹配
    pub tag: u32,
    /// 数据阶段字节数 (0 表示无数据阶段)
    pub data_transfer_length: u32,
    /// CBW flags: bit 7 = direction (0=OUT, 1=IN)
    pub flags: u8,
    /// Logical Unit Number (通常为 0)
    pub lun: u8,
    /// SCSI Command Block (最多 16 字节, 实际长度由 bCBWCBLength 决定)
    pub cb: [u8; SCSI_CB_MAX_LENGTH],
    /// CB 实际长度 (1-16)
    pub cb_length: u8,
}

impl CommandBlockWrapper {
    /// 创建新的 CBW.
    /// # Errors
    /// SCSI 命令块为空或长度超过最大允许值时返回 Err。
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn new(
        tag: u32,
        data_transfer_length: u32,
        direction_in: bool,
        lun: u8,
        cb: &[u8],
    ) -> Result<Self> {
        if cb.is_empty() || cb.len() > SCSI_CB_MAX_LENGTH {
            return Err(DriverError::InvalidParameter);
        }
        let mut cb_arr = [0u8; SCSI_CB_MAX_LENGTH];
        cb_arr[..cb.len()].copy_from_slice(cb);
        Ok(Self {
            tag,
            data_transfer_length,
            flags: if direction_in { 0x80 } else { 0x00 },
            lun,
            cb: cb_arr,
            cb_length: cb.len() as u8,
        })
    }

    /// 序列化为 31 字节 little-endian 缓冲.
    pub fn to_bytes(&self) -> [u8; CBW_LENGTH] {
        let mut buf = [0u8; CBW_LENGTH];
        buf[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        buf[4..8].copy_from_slice(&self.tag.to_le_bytes());
        buf[8..12].copy_from_slice(&self.data_transfer_length.to_le_bytes());
        buf[12] = self.flags;
        buf[13] = self.lun;
        buf[14] = self.cb_length;
        buf[15..15 + SCSI_CB_MAX_LENGTH].copy_from_slice(&self.cb);
        buf
    }
}

/// Command Status Wrapper (13 字节, USB MSC BBB §3.2).
///
/// 字节布局 (little-endian):
/// - bytes 0..=3:  dCSWSignature (固定 0x53425553 = "USBS")
/// - bytes 4..=7:  dCSWTag (必须匹配 CBW dCBWTag)
/// - bytes 8..=11: dCSWDataResidue (数据阶段未传输字节数)
/// - byte 12:      bCSWStatus (0=成功, 1=失败, 2=阶段错误)
#[derive(Debug, Clone, Copy)]
pub struct CommandStatusWrapper {
    /// CSW tag (应匹配对应 CBW 的 tag)
    pub tag: u32,
    /// 数据阶段未传输字节数
    pub data_residue: u32,
    /// 状态: 0=Success, 1=Failure, 2=Phase Error
    pub status: u8,
}

/// CSW 状态码 (USB MSC BBB §3.2).
pub mod csw_status {
    pub const SUCCESS: u8 = 0;
    pub const FAILURE: u8 = 1;
    pub const PHASE_ERROR: u8 = 2;
}

impl CommandStatusWrapper {
    /// 从 13 字节缓冲反序列化.
    /// # Errors
    /// 数据长度不足或 CSW 签名不匹配时返回 Err。
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < CSW_LENGTH {
            return Err(DriverError::InvalidParameter);
        }
        let signature = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if signature != CSW_SIGNATURE {
            return Err(DriverError::InvalidParameter);
        }
        Ok(Self {
            tag: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            data_residue: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            status: data[12],
        })
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 是否成功.
    pub fn is_success(&self) -> bool {
        self.status == csw_status::SUCCESS
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 是否为 Phase Error (协议层错误, 通常需 reset recovery).
    pub fn is_phase_error(&self) -> bool {
        self.status == csw_status::PHASE_ERROR
    }
}

// ============================================================================
// SCSI 命令构造 (SBC-3 / SCSI 标准)
// ============================================================================

/// SCSI 操作码 (常用).
pub mod scsi_op {
    pub const TEST_UNIT_READY: u8 = 0x00;
    pub const REQUEST_SENSE: u8 = 0x03;
    pub const INQUIRY: u8 = 0x12;
    pub const READ_CAPACITY_10: u8 = 0x25;
    pub const READ_10: u8 = 0x28;
    pub const WRITE_10: u8 = 0x2A;
}

/// SCSI READ(10) 命令 (10 字节).
///
/// 字节布局:
/// - byte 0: 0x28 (`READ_10`)
/// - byte 1: bit 4 = DPO, bit 3 = FUA, bit 2 = RDPROTECT, bit 1..=0 = LBA[31..24]
/// - bytes 2..=5: LBA[23..0] (大端字节序)
/// - byte 6: Group Number 分组号
/// - bytes 7..=8: Transfer Length 块数 (blocks, big-endian)
/// - byte 9: Control
pub fn build_read_10_cmd(lba: u32, blocks: u16) -> [u8; 10] {
    [
        scsi_op::READ_10,
        0x00, // byte 1: flags + LBA high (lba < 2^24 时为 0)
        ((lba >> 24) & 0xFF) as u8,
        ((lba >> 16) & 0xFF) as u8,
        ((lba >> 8) & 0xFF) as u8,
        (lba & 0xFF) as u8,
        0x00, // Group Number
        ((blocks >> 8) & 0xFF) as u8,
        (blocks & 0xFF) as u8,
        0x00, // Control
    ]
}

/// SCSI INQUIRY 命令 (6 字节).
///
/// 字节布局:
/// - byte 0: 0x12 (INQUIRY)
/// - byte 1: bit 0 = EVPD
/// - byte 2: Page Code
/// - bytes 3..=4: Allocation Length 分配长度 (大端)
/// - byte 5: Control
pub fn build_inquiry_cmd(allocation_length: u16) -> [u8; 6] {
    [
        scsi_op::INQUIRY,
        0x00, // EVPD = 0 (Standard INQUIRY data)
        0x00, // Page Code
        ((allocation_length >> 8) & 0xFF) as u8,
        (allocation_length & 0xFF) as u8,
        0x00, // Control
    ]
}

/// SCSI READ CAPACITY(10) 命令 (10 字节).
pub fn build_read_capacity_10_cmd() -> [u8; 10] {
    [
        scsi_op::READ_CAPACITY_10,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ]
}

/// SCSI TEST UNIT READY 命令 (6 字节).
pub fn build_test_unit_ready_cmd() -> [u8; 6] {
    [scsi_op::TEST_UNIT_READY, 0x00, 0x00, 0x00, 0x00, 0x00]
}

/// SCSI REQUEST SENSE 命令 (6 字节).
pub fn build_request_sense_cmd(allocation_length: u8) -> [u8; 6] {
    [
        scsi_op::REQUEST_SENSE,
        0x00, // DESC = 0
        0x00,
        0x00,
        allocation_length,
        0x00,
    ]
}

// ============================================================================
// Mass Storage 设备驱动 (USB-1.8)
// ============================================================================

/// Mass Storage 驱动实例 (USB-1.8).
///
/// 当前为**软件骨架**: 记录 Mass Storage 设备的端点信息 + 当前 SCSI tag, 提供 CBW 构造
/// 和 CSW 解析. 真实硬件应通过 Bulk-OUT 发送 CBW, Bulk-IN 接收 Data + CSW.
pub struct MassStorageDriver {
    /// 设备地址 (用于 CBW 数据阶段, Phase E Bulk Transfer 集成时使用)
    device_address: u8,
    /// Interface number (用于 CBW index 字段, Phase E Bulk Transfer 集成时使用)
    interface_number: u8,
    /// Bulk-IN 批量输入端点 (例如 0x81 表示 EP1 IN)
    bulk_in: u8,
    /// Bulk-OUT 批量输出端点 (例如 0x02 表示 EP2 OUT)
    bulk_out: u8,
    /// Bulk 最大包大小 (通常 512)
    bulk_max_packet: u16,
    /// 当前 CBW tag (单调递增)
    next_tag: u32,
}

impl MassStorageDriver {
    /// 从 `UsbDevice` 创建 Mass Storage 驱动实例.
    /// # Errors
    /// 设备或接口不属于 Mass Storage 类、接口索引非法或未找到 Bulk 端点时返回 Err。
    pub fn from_usb_device(device: &UsbDevice, interface_idx: usize) -> Result<Self> {
        if device.descriptor.device_class != DeviceClass::MassStorage as u8 {
            return Err(DriverError::InvalidParameter);
        }
        let iface = device
            .interfaces
            .get(interface_idx)
            .ok_or(DriverError::InvalidParameter)?;
        if iface.interface_class != DeviceClass::MassStorage as u8 {
            return Err(DriverError::InvalidParameter);
        }

        // 查找 Bulk-IN 和 Bulk-OUT endpoints (USB MSC BBB 要求)
        let bulk_in = device
            .endpoints
            .iter()
            .find(|ep| ep.attributes == 0x02 && ep.endpoint_address & 0x80 != 0)
            .ok_or(DriverError::InvalidParameter)?;
        let bulk_out = device
            .endpoints
            .iter()
            .find(|ep| ep.attributes == 0x02 && ep.endpoint_address & 0x80 == 0)
            .ok_or(DriverError::InvalidParameter)?;

        Ok(Self {
            device_address: device.address,
            interface_number: iface.interface_number,
            bulk_in: bulk_in.endpoint_address,
            bulk_out: bulk_out.endpoint_address,
            bulk_max_packet: bulk_in.max_packet_size,
            next_tag: 1,
        })
    }

    /// 获取 Bulk-IN endpoint.
    pub fn bulk_in_endpoint(&self) -> u8 {
        self.bulk_in
    }

    /// 获取 Bulk-OUT endpoint.
    pub fn bulk_out_endpoint(&self) -> u8 {
        self.bulk_out
    }

    /// 获取 Bulk max packet size.
    pub fn bulk_max_packet(&self) -> u16 {
        self.bulk_max_packet
    }

    /// 获取设备地址
    pub fn device_address(&self) -> u8 {
        self.device_address
    }

    /// 获取接口编号
    pub fn interface_number(&self) -> u8 {
        self.interface_number
    }

    /// 分配并构造新的 CBW.
    ///
    /// `next_tag` 单调递增, 调用方在收到 CSW 后验证 tag 匹配.
    /// # Errors
    /// SCSI 命令块为空或长度非法时返回 Err。
    pub fn build_cbw(
        &mut self,
        data_transfer_length: u32,
        direction_in: bool,
        lun: u8,
        scsi_cb: &[u8],
    ) -> Result<CommandBlockWrapper> {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        CommandBlockWrapper::new(tag, data_transfer_length, direction_in, lun, scsi_cb)
    }

    /// 构造 READ(10) CBW.
    ///
    /// # 参数
    ///
    /// - `lba`: Logical Block Address (起始扇区)
    /// - `blocks`: 读取扇区数
    /// - `lun`: 逻辑单元号 LUN
    /// # Errors
    /// 底层 CBW 构造失败时返回 Err。
    pub fn build_read_10_cbw(
        &mut self,
        lba: u32,
        blocks: u16,
        lun: u8,
    ) -> Result<CommandBlockWrapper> {
        let cmd = build_read_10_cmd(lba, blocks);
        let data_len = u32::from(blocks) * 512; // 假设 512 字节/扇区
        self.build_cbw(data_len, true, lun, &cmd)
    }

    /// 构造 INQUIRY CBW.
    /// # Errors
    /// 底层 CBW 构造失败时返回 Err。
    pub fn build_inquiry_cbw(
        &mut self,
        allocation_length: u16,
        lun: u8,
    ) -> Result<CommandBlockWrapper> {
        let cmd = build_inquiry_cmd(allocation_length);
        self.build_cbw(u32::from(allocation_length), true, lun, &cmd)
    }

    /// 构造 READ CAPACITY(10) CBW.
    /// # Errors
    /// 底层 CBW 构造失败时返回 Err。
    pub fn build_read_capacity_10_cbw(&mut self, lun: u8) -> Result<CommandBlockWrapper> {
        let cmd = build_read_capacity_10_cmd();
        self.build_cbw(8, true, lun, &cmd) // READ CAPACITY(10) 返回 8 字节
    }

    /// 构造 TEST UNIT READY CBW.
    /// # Errors
    /// 底层 CBW 构造失败时返回 Err。
    pub fn build_test_unit_ready_cbw(&mut self, lun: u8) -> Result<CommandBlockWrapper> {
        let cmd = build_test_unit_ready_cmd();
        self.build_cbw(0, false, lun, &cmd)
    }

    /// 构造 REQUEST SENSE CBW.
    /// # Errors
    /// 底层 CBW 构造失败时返回 Err。
    pub fn build_request_sense_cbw(
        &mut self,
        allocation_length: u8,
        lun: u8,
    ) -> Result<CommandBlockWrapper> {
        let cmd = build_request_sense_cmd(allocation_length);
        self.build_cbw(u32::from(allocation_length), true, lun, &cmd)
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::driver::usb::enumerate::parse_device_descriptor;
    use crate::services::driver::usb::usb_core::DeviceState;
    use crate::services::driver::usb::usb_core::EndpointDescriptor;
    use crate::services::driver::usb::usb_core::InterfaceDescriptor;
    use crate::services::driver::usb::usb_core::UsbSpeed;
    use alloc::vec;

    // ----------------- CBW Tests -----------------

    #[test]
    fn test_cbw_new_validates_length() {
        // Empty CB invalid
        assert!(CommandBlockWrapper::new(1, 0, false, 0, &[]).is_err());
        // CB too long
        let long_cb = [0u8; 17];
        assert!(CommandBlockWrapper::new(1, 0, false, 0, &long_cb).is_err());
        // Valid CB
        assert!(CommandBlockWrapper::new(1, 0, false, 0, &[0x00]).is_ok());
        assert!(CommandBlockWrapper::new(1, 0, false, 0, &[0u8; 16]).is_ok());
    }

    #[test]
    fn test_cbw_to_bytes_signature_and_header() {
        let cbw = CommandBlockWrapper::new(0x1234_5678, 512, true, 0, &[0x28]).unwrap();
        let buf = cbw.to_bytes();
        // Signature (4 bytes)
        assert_eq!(&buf[0..4], &CBW_SIGNATURE.to_le_bytes());
        // Tag
        assert_eq!(&buf[4..8], &0x1234_5678_u32.to_le_bytes());
        // 数据传输长度
        assert_eq!(&buf[8..12], &512_u32.to_le_bytes());
        // Flags (bit 7 = direction IN)
        assert_eq!(buf[12], 0x80);
        // LUN
        assert_eq!(buf[13], 0);
        // CB length
        assert_eq!(buf[14], 1);
        // CB start
        assert_eq!(buf[15], 0x28);
    }

    #[test]
    fn test_cbw_to_bytes_direction_out() {
        let cbw = CommandBlockWrapper::new(1, 0, false, 0, &[0x00]).unwrap();
        let buf = cbw.to_bytes();
        assert_eq!(buf[12], 0x00); // OUT direction
    }

    // ----------------- CSW Tests -----------------

    #[test]
    fn test_csw_from_bytes_valid() {
        let data = [
            0x53, 0x55, 0x42, 0x53, // Signature "USBS"
            0x78, 0x56, 0x34, 0x12, // Tag = 0x12345678
            0x10, 0x00, 0x00, 0x00, // Data residue = 16
            0x00, // Status = Success
        ];
        let csw = CommandStatusWrapper::from_bytes(&data).unwrap();
        assert_eq!(csw.tag, 0x1234_5678);
        assert_eq!(csw.data_residue, 16);
        assert_eq!(csw.status, csw_status::SUCCESS);
        assert!(csw.is_success());
        assert!(!csw.is_phase_error());
    }

    #[test]
    fn test_csw_from_bytes_invalid_signature() {
        let mut data = [0u8; 13];
        data[0..4].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
        assert!(CommandStatusWrapper::from_bytes(&data).is_err());
    }

    #[test]
    fn test_csw_from_bytes_too_short() {
        assert!(CommandStatusWrapper::from_bytes(&[0u8; 12]).is_err());
    }

    #[test]
    fn test_csw_phase_error_status() {
        let data = [
            0x53, 0x55, 0x42, 0x53, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x02, // PHASE_ERROR
        ];
        let csw = CommandStatusWrapper::from_bytes(&data).unwrap();
        assert!(csw.is_phase_error());
        assert!(!csw.is_success());
    }

    // ----------------- SCSI 命令构造器测试 -----------------

    #[test]
    fn test_build_read_10_cmd_lba_zero() {
        let cmd = build_read_10_cmd(0, 1);
        assert_eq!(cmd[0], scsi_op::READ_10);
        assert_eq!(cmd[1], 0x00);
        assert_eq!(cmd[2], 0); // LBA[31..24]
        assert_eq!(cmd[3], 0); // LBA[23..16]
        assert_eq!(cmd[4], 0); // LBA[15..8]
        assert_eq!(cmd[5], 0); // LBA[7..0]
        assert_eq!(cmd[7], 0); // blocks[15..8]
        assert_eq!(cmd[8], 1); // blocks[7..0]
    }

    #[test]
    fn test_build_read_10_cmd_lba_high() {
        let cmd = build_read_10_cmd(0x1234_5678, 0x10);
        assert_eq!(cmd[0], scsi_op::READ_10);
        assert_eq!(cmd[2], 0x12);
        assert_eq!(cmd[3], 0x34);
        assert_eq!(cmd[4], 0x56);
        assert_eq!(cmd[5], 0x78);
        assert_eq!(cmd[8], 0x10);
    }

    #[test]
    fn test_build_inquiry_cmd() {
        let cmd = build_inquiry_cmd(36);
        assert_eq!(cmd[0], scsi_op::INQUIRY);
        assert_eq!(cmd[3], 0); // 36 >> 8
        assert_eq!(cmd[4], 36);
    }

    #[test]
    fn test_build_read_capacity_10_cmd() {
        let cmd = build_read_capacity_10_cmd();
        assert_eq!(cmd[0], scsi_op::READ_CAPACITY_10);
        // 其他字节应全 0
        assert!(cmd[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_build_test_unit_ready_cmd() {
        let cmd = build_test_unit_ready_cmd();
        assert_eq!(cmd[0], scsi_op::TEST_UNIT_READY);
    }

    #[test]
    fn test_build_request_sense_cmd() {
        let cmd = build_request_sense_cmd(18);
        assert_eq!(cmd[0], scsi_op::REQUEST_SENSE);
        assert_eq!(cmd[4], 18);
    }

    // ----------------- MassStorageDriver Tests -----------------

    fn make_test_msc_device() -> UsbDevice {
        let device_data = [
            18, 1, 0x10, 0x01, 0x08, 0x06, 0x50, 0x40, 0xAB, 0x12, 0xCD, 0x34, 0x00, 0x01, 1, 2, 0,
            1,
        ];
        let descriptor = parse_device_descriptor(&device_data).unwrap();

        UsbDevice {
            id: 0,
            address: 7,
            speed: UsbSpeed::High,
            state: DeviceState::Configured,
            descriptor,
            configuration: Some(1),
            interfaces: vec![InterfaceDescriptor {
                length: 9,
                descriptor_type: 4,
                interface_number: 0,
                alternate_setting: 0,
                num_endpoints: 2,
                interface_class: DeviceClass::MassStorage as u8,
                interface_subclass: 0x06, // SCSI 透明命令集 (Transparent Command Set)
                interface_protocol: 0x50, // BBB
                interface_index: 0,
            }],
            endpoints: vec![
                // Bulk-IN
                EndpointDescriptor {
                    length: 7,
                    descriptor_type: 5,
                    endpoint_address: 0x81, // IN, EP1
                    attributes: 0x02,       // Bulk
                    max_packet_size: 512,
                    interval: 0,
                },
                // Bulk-OUT
                EndpointDescriptor {
                    length: 7,
                    descriptor_type: 5,
                    endpoint_address: 0x02, // OUT, EP2
                    attributes: 0x02,       // Bulk
                    max_packet_size: 512,
                    interval: 0,
                },
            ],
            info: crate::framework::driver::framework::DeviceInfo::new(
                "test-msc",
                crate::framework::driver::framework::DeviceType::Block,
            ),
        }
    }

    #[test]
    fn test_msc_driver_from_usb_device() {
        let device = make_test_msc_device();
        let driver = MassStorageDriver::from_usb_device(&device, 0).unwrap();
        assert_eq!(driver.device_address, 7);
        assert_eq!(driver.interface_number, 0);
        assert_eq!(driver.bulk_in_endpoint(), 0x81);
        assert_eq!(driver.bulk_out_endpoint(), 0x02);
        assert_eq!(driver.bulk_max_packet(), 512);
    }

    #[test]
    fn test_msc_driver_build_read_10_cbw() {
        let device = make_test_msc_device();
        let mut driver = MassStorageDriver::from_usb_device(&device, 0).unwrap();
        let cbw = driver.build_read_10_cbw(2048, 16, 0).unwrap();
        let bytes = cbw.to_bytes();
        // tag should be 1 (first)
        assert_eq!(&bytes[4..8], &1_u32.to_le_bytes());
        // Data transfer length = 16 * 512 = 8192
        assert_eq!(&bytes[8..12], &8192_u32.to_le_bytes());
        // Flags IN
        assert_eq!(bytes[12], 0x80);
        // CB[0] = READ_10
        assert_eq!(bytes[15], scsi_op::READ_10);
    }

    #[test]
    fn test_msc_driver_tag_increments() {
        let device = make_test_msc_device();
        let mut driver = MassStorageDriver::from_usb_device(&device, 0).unwrap();
        let cbw1 = driver.build_test_unit_ready_cbw(0).unwrap();
        let cbw2 = driver.build_test_unit_ready_cbw(0).unwrap();
        let cbw3 = driver.build_test_unit_ready_cbw(0).unwrap();
        assert_eq!(cbw1.tag, 1);
        assert_eq!(cbw2.tag, 2);
        assert_eq!(cbw3.tag, 3);
    }

    #[test]
    fn test_msc_driver_build_inquiry_cbw() {
        let device = make_test_msc_device();
        let mut driver = MassStorageDriver::from_usb_device(&device, 0).unwrap();
        let cbw = driver.build_inquiry_cbw(36, 0).unwrap();
        assert_eq!(cbw.data_transfer_length, 36);
        assert!(cbw.flags & 0x80 != 0); // IN direction
    }

    #[test]
    fn test_msc_driver_rejects_non_msc_device() {
        let mut device = make_test_msc_device();
        // 改写 device_class 为非 MassStorage
        device.descriptor.device_class = 0xFF;
        let result = MassStorageDriver::from_usb_device(&device, 0);
        assert!(matches!(result, Err(DriverError::InvalidParameter)));
    }

    #[test]
    fn test_msc_driver_requires_bulk_endpoints() {
        let mut device = make_test_msc_device();
        // 移除批量端点
        device.endpoints.clear();
        let result = MassStorageDriver::from_usb_device(&device, 0);
        assert!(matches!(result, Err(DriverError::InvalidParameter)));
    }
}
