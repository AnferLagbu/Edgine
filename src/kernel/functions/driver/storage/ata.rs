#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! ATA/IDE 磁盘驱动 — functions 层真实 PIO 实现 (framekernel 阶段 3)
//!
//! 由 privileged 层回迁的完整 PIO 驱动逻辑: 端口 I/O 全部经 privileged 提供的
//! safe 代理 `IoPort` (in/out 指令封装), functions 侧保持 0 unsafe。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 端口访问只走 `IoPort` safe API
//! - **functions 权威**: 控制器实例存于 `ATA_CONTROLLERS` 注册表 (父模块),
//!   块设备适配器 `AtaBlockDevice` 经注册表查找控制器
//! - **仅注册块设备**: 只向 EGDF 注册 `ata0-3` 块设备, 不注册控制器设备
//!
//! ## 硬件架构
//!
//! ```text
//! ATA 子系统
//! ├── Primary Channel (IO: 0x1F0-0x1F7, Ctrl: 0x3F6)
//! │   ├── Master (Drive 0)
//! │   └── Slave  (Drive 1)
//! └── Secondary Channel (IO: 0x170-0x177, Ctrl: 0x376)
//!     ├── Master (Drive 2)
//!     └── Slave  (Drive 3)
//! ```

use crate::privileged::driver::BlockDevice;
use crate::privileged::ioport::IoPort;

use super::ATA_CONTROLLERS;

// ============================================================================
// ATA 硬件常量定义
// ============================================================================

/// Primary 通道 I/O 基地址
pub const ATA_PRIMARY_IO: u16 = 0x1F0;
/// Primary 通道控制寄存器基址
pub const ATA_PRIMARY_CTRL: u16 = 0x3F6;
/// Secondary 通道 I/O 基地址
pub const ATA_SECONDARY_IO: u16 = 0x170;
/// Secondary 通道控制寄存器基址
pub const ATA_SECONDARY_CTRL: u16 = 0x376;

/// I/O 寄存器偏移量
pub const ATA_DATA: u16 = 0;
pub const ATA_ERROR: u16 = 1;
pub const ATA_SECTOR_COUNT: u16 = 2;
pub const ATA_SECTOR_NUM: u16 = 3;
pub const ATA_CYLINDER_LOW: u16 = 4;
pub const ATA_CYLINDER_HIGH: u16 = 5;
pub const ATA_DRIVE_HEAD: u16 = 6;
pub const ATA_STATUS: u16 = 7;
pub const ATA_COMMAND: u16 = 7;

/// 控制端口块偏移: 替代状态寄存器 (读) / 设备控制寄存器 (写)
const ATA_CTRL_ALT_STATUS: u16 = 0;

/// 状态寄存器标志位
pub const ATA_STATUS_BSY: u8 = 0x80;
pub const ATA_STATUS_DRDY: u8 = 0x40;
pub const ATA_STATUS_DF: u8 = 0x20;
pub const ATA_STATUS_DRQ: u8 = 0x08;
pub const ATA_STATUS_ERR: u8 = 0x01;

/// ATA 命令集
pub const ATA_CMD_IDENTIFY: u8 = 0xEC;
pub const ATA_CMD_READ_SECTORS: u8 = 0x20;
pub const ATA_CMD_WRITE_SECTORS: u8 = 0x30;
pub const ATA_CMD_FLUSH_CACHE: u8 = 0xE7;

/// 扇区大小
pub const SECTOR_SIZE: usize = 512;
/// 每个扇区的字数 (256 × 16bit = 512 bytes)
pub const WORDS_PER_SECTOR: usize = 256;
/// 最大支持设备数量 (2通道 × 2驱动器)
pub const MAX_ATA_DEVICES: usize = 4;

/// 轮询超时值 (循环次数)
const ATA_TIMEOUT: u32 = 100000;

// ============================================================================
// 设备状态结构体
// ============================================================================

/// ATA 驱动器状态
#[derive(Debug, Clone, Copy)]
pub struct AtaDevice {
    /// 是否存在
    pub present: bool,
    /// 是否为主驱动器 (Master)
    pub is_master: bool,
    /// 所属通道 (0=Primary, 1=Secondary)
    pub channel: u8,
    /// 容量 (512 字节扇区数), IDENTIFY 探测结果
    pub total_sectors: u64,
}

impl Default for AtaDevice {
    fn default() -> Self {
        Self {
            present: false,
            is_master: true,
            channel: 0,
            total_sectors: 0,
        }
    }
}

// ============================================================================
// 底层辅助函数 (safe 端口 I/O)
// ============================================================================

/// ATA 延时函数 (读取替代状态寄存器 4 次, 满足 400ns 规范)
fn ata_delay(ctrl: &IoPort) {
    for _ in 0..4 {
        let _ = ctrl.read_u8(ATA_CTRL_ALT_STATUS);
    }
}

/// 读取替代状态寄存器 (读操作不清除中断)
#[inline]
fn read_alt_status(ctrl: &IoPort) -> u8 {
    ctrl.read_u8(ATA_CTRL_ALT_STATUS)
}

/// 等待 BSY 位清除
fn wait_bsy(io: &IoPort, ctrl: &IoPort) -> Result<(), ()> {
    let mut timeout = ATA_TIMEOUT;

    while timeout > 0 {
        if io.read_u8(ATA_STATUS) & ATA_STATUS_BSY == 0 {
            return Ok(());
        }
        ata_delay(ctrl);
        timeout -= 1;
    }

    Err(())
}

/// 等待 DRQ 置位且 BSY 清除
fn wait_drq(io: &IoPort, ctrl: &IoPort) -> Result<(), ()> {
    let mut timeout = ATA_TIMEOUT;

    while timeout > 0 {
        let status = io.read_u8(ATA_STATUS);

        if status & ATA_STATUS_DF != 0 {
            return Err(());
        }

        if status & ATA_STATUS_ERR != 0 {
            let _ = io.read_u8(ATA_ERROR);
            return Err(());
        }

        if status & (ATA_STATUS_DRQ | ATA_STATUS_BSY) == ATA_STATUS_DRQ {
            return Ok(());
        }
        ata_delay(ctrl);
        timeout -= 1;
    }

    Err(())
}

/// 选择驱动器 (Master/Slave)
fn select_drive(io: &IoPort, ctrl: &IoPort, slave: bool) -> Result<(), ()> {
    io.write_u8(ATA_DRIVE_HEAD, 0xA0 | (u8::from(slave) << 4));
    ata_delay(ctrl);

    wait_bsy(io, ctrl)
}

/// 写入 LBA28 地址寄存器组 (1 个扇区)
fn write_lba28(io: &IoPort, ctrl: &IoPort, slave: bool, lba: u32) {
    io.write_u8(ATA_SECTOR_COUNT, 1);
    io.write_u8(ATA_SECTOR_NUM, (lba & 0xFF) as u8);
    io.write_u8(ATA_CYLINDER_LOW, ((lba >> 8) & 0xFF) as u8);
    io.write_u8(ATA_CYLINDER_HIGH, ((lba >> 16) & 0xFF) as u8);
    io.write_u8(
        ATA_DRIVE_HEAD,
        0xE0 | (u8::from(slave) << 4) | ((lba >> 24) & 0x0F) as u8,
    );
    ata_delay(ctrl);
}

/// 从 IDENTIFY DEVICE 返回的 256 字数据解析容量 (512 字节扇区数)
///
/// 优先取 word 100-103 (LBA48, 48 位有效), 为 0 时回落 word 60-61 (LBA28)。
fn identify_capacity(words: &[u16; WORDS_PER_SECTOR]) -> u64 {
    let lba48 = u64::from(words[100])
        | (u64::from(words[101]) << 16)
        | (u64::from(words[102]) << 32)
        | (u64::from(words[103]) << 48);
    if lba48 > 0 {
        return lba48;
    }
    u64::from(words[60]) | (u64::from(words[61]) << 16)
}

/// 探测单个驱动器 (IDENTIFY DEVICE), 返回其状态与容量
fn probe_drive(io: &IoPort, ctrl: &IoPort, slave: bool, channel: u8) -> AtaDevice {
    let mut device = AtaDevice {
        present: false,
        is_master: !slave,
        channel,
        total_sectors: 0,
    };

    if select_drive(io, ctrl, slave).is_err() {
        return device;
    }

    if io.read_u8(ATA_STATUS) & ATA_STATUS_DRDY == 0 {
        return device;
    }

    // 设置参数为 0 (IDENTIFY 前置条件)
    io.write_u8(ATA_SECTOR_COUNT, 0);
    io.write_u8(ATA_SECTOR_NUM, 0);
    io.write_u8(ATA_CYLINDER_LOW, 0);
    io.write_u8(ATA_CYLINDER_HIGH, 0);

    io.write_u8(ATA_COMMAND, ATA_CMD_IDENTIFY);
    ata_delay(ctrl);

    // 状态为 0 说明该位置无设备
    if io.read_u8(ATA_STATUS) == 0 {
        return device;
    }
    if wait_bsy(io, ctrl).is_err() {
        return device;
    }
    if io.read_u8(ATA_STATUS) & ATA_STATUS_ERR != 0 {
        return device;
    }
    if wait_drq(io, ctrl).is_err() {
        return device;
    }

    // 读取 IDENTIFY 数据 (256 字)
    let mut words = [0u16; WORDS_PER_SECTOR];
    for word in &mut words {
        *word = io.read_u16(ATA_DATA);
    }

    device.total_sectors = identify_capacity(&words);
    device.present = true;
    device
}

/// 探测一个通道 (软件复位 + 签名回读), 返回 (通道存在, [Master, Slave])
fn probe_channel(io: &IoPort, ctrl: &IoPort, channel: u8) -> (bool, [AtaDevice; 2]) {
    // Software Reset
    ctrl.write_u8(ATA_CTRL_ALT_STATUS, 0x04);
    ata_delay(ctrl);
    ctrl.write_u8(ATA_CTRL_ALT_STATUS, 0x00);

    // ATA 规范 9.2: 轮询替代状态等待 BSY 清除 (400ns minimum)
    for _ in 0..1000 {
        if read_alt_status(ctrl) & ATA_STATUS_BSY == 0 {
            break;
        }
        core::hint::spin_loop();
    }

    // 写入签名值回读检测通道是否存在
    io.write_u8(ATA_SECTOR_COUNT, 0x55);
    io.write_u8(ATA_SECTOR_NUM, 0xAA);
    let count = io.read_u8(ATA_SECTOR_COUNT);
    let num = io.read_u8(ATA_SECTOR_NUM);
    if count != 0x55 || num != 0xAA {
        return (false, [AtaDevice::default(); 2]);
    }

    (
        true,
        [
            probe_drive(io, ctrl, false, channel),
            probe_drive(io, ctrl, true, channel),
        ],
    )
}

// ============================================================================
// ATA 控制器
// ============================================================================

/// ATA 控制器 (functions 权威)
///
/// 持有 4 个 `IoPort` (Primary/Secondary 的 I/O 与控制端口), 端口 I/O 全部
/// 经 privileged safe 代理, 无 unsafe 块。
pub struct AtaController {
    /// Primary 通道是否存在
    pub primary_present: bool,
    /// Secondary 通道是否存在
    pub secondary_present: bool,
    /// 驱动器列表 [Primary-Master, Primary-Slave, Secondary-Master, Secondary-Slave]
    pub devices: [AtaDevice; MAX_ATA_DEVICES],
    /// 是否已初始化
    initialized: bool,
    /// Primary I/O 端口 (0x1F0-0x1F7)
    primary_io: IoPort,
    /// Primary 控制端口 (0x3F6)
    primary_ctrl: IoPort,
    /// Secondary I/O 端口 (0x170-0x177)
    secondary_io: IoPort,
    /// Secondary 控制端口 (0x376)
    secondary_ctrl: IoPort,
}

impl AtaController {
    /// 创建 ATA 控制器实例 (构造端口句柄, 不做硬件访问)
    ///
    /// 端口范围非法时返回 `None`。
    pub fn new() -> Option<Self> {
        // 端口范围来自 PC 规范 (Primary: 0x1F0/0x3F6, Secondary: 0x170/0x376),
        // 不与其他 IoPort 实例重叠 — 契约由调用方保证 (IoPort::new_safe)。
        Some(Self {
            primary_present: false,
            secondary_present: false,
            devices: [AtaDevice::default(); MAX_ATA_DEVICES],
            initialized: false,
            primary_io: IoPort::new_safe(ATA_PRIMARY_IO, 8, "ata-pio").ok()?,
            primary_ctrl: IoPort::new_safe(ATA_PRIMARY_CTRL, 2, "ata-pctrl").ok()?,
            secondary_io: IoPort::new_safe(ATA_SECONDARY_IO, 8, "ata-sio").ok()?,
            secondary_ctrl: IoPort::new_safe(ATA_SECONDARY_CTRL, 2, "ata-sctrl").ok()?,
        })
    }

    /// 初始化控制器: 双通道软件复位 + 驱动器探测
    ///
    /// 返回是否检测到至少一个驱动器。`false` 同时表示初始化失败
    /// (控制器可能不存在或通道无设备)。
    pub fn init(&mut self) -> bool {
        self.primary_present = false;
        self.secondary_present = false;
        for device in &mut self.devices {
            *device = AtaDevice::default();
        }
        self.initialized = false;

        let (primary_present, primary_devices) =
            probe_channel(&self.primary_io, &self.primary_ctrl, 0);
        self.primary_present = primary_present;
        self.devices[0] = primary_devices[0];
        self.devices[1] = primary_devices[1];

        let (secondary_present, secondary_devices) =
            probe_channel(&self.secondary_io, &self.secondary_ctrl, 1);
        self.secondary_present = secondary_present;
        self.devices[2] = secondary_devices[0];
        self.devices[3] = secondary_devices[1];

        self.initialized = true;
        self.detected_device_count() > 0
    }

    /// 控制器是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// 检查指定驱动器是否存在
    ///
    /// `drive` 超出 0-3 范围时返回 `false`。
    pub fn disk_present(&self, drive: u8) -> bool {
        self.devices
            .get(drive as usize)
            .map_or(false, |device| device.present)
    }

    /// 获取已检测到的驱动器数量
    pub fn detected_device_count(&self) -> usize {
        self.devices.iter().filter(|d| d.present).count()
    }

    /// 获取指定驱动器的容量 (512 字节扇区数), 不存在时返回 0
    pub fn total_sectors(&self, drive: u8) -> u64 {
        self.devices
            .get(drive as usize)
            .map_or(0, |device| device.total_sectors)
    }

    /// 获取指定驱动器的 I/O 和控制端口引用
    fn ports_for_drive(&self, drive: u8) -> (&IoPort, &IoPort) {
        if drive < 2 {
            (&self.primary_io, &self.primary_ctrl)
        } else {
            (&self.secondary_io, &self.secondary_ctrl)
        }
    }

    /// 读取单个扇区 (512 字节, LBA28)
    /// # Errors
    /// 驱动器不存在、缓冲区小于 512 字节、或设备忙/故障/超时时返回 `Err`。
    pub fn read_sector(&self, drive: u8, lba: u32, buffer: &mut [u8]) -> Result<(), ()> {
        if !self.disk_present(drive) {
            return Err(());
        }
        if buffer.len() < SECTOR_SIZE {
            return Err(());
        }

        let (io, ctrl) = self.ports_for_drive(drive);
        let slave = (drive & 0x01) != 0;

        select_drive(io, ctrl, slave)?;
        write_lba28(io, ctrl, slave, lba);
        io.write_u8(ATA_COMMAND, ATA_CMD_READ_SECTORS);
        ata_delay(ctrl);
        wait_bsy(io, ctrl)?;
        wait_drq(io, ctrl)?;

        // 读取 512 字节 = 256 个 16-bit 字 (小端)
        for i in 0..WORDS_PER_SECTOR {
            let word = io.read_u16(ATA_DATA);
            buffer[i * 2] = (word & 0xFF) as u8;
            buffer[i * 2 + 1] = (word >> 8) as u8;
        }

        Ok(())
    }

    /// 写入单个扇区 (512 字节, LBA28)
    /// # Errors
    /// 驱动器不存在、缓冲区小于 512 字节、或设备忙/故障/超时时返回 `Err`。
    pub fn write_sector(&self, drive: u8, lba: u32, buffer: &[u8]) -> Result<(), ()> {
        if !self.disk_present(drive) {
            return Err(());
        }
        if buffer.len() < SECTOR_SIZE {
            return Err(());
        }

        let (io, ctrl) = self.ports_for_drive(drive);
        let slave = (drive & 0x01) != 0;

        select_drive(io, ctrl, slave)?;
        write_lba28(io, ctrl, slave, lba);
        io.write_u8(ATA_COMMAND, ATA_CMD_WRITE_SECTORS);
        ata_delay(ctrl);
        wait_bsy(io, ctrl)?;
        wait_drq(io, ctrl)?;

        for i in 0..WORDS_PER_SECTOR {
            let word = (u16::from(buffer[i * 2 + 1]) << 8) | u16::from(buffer[i * 2]);
            io.write_u16(ATA_DATA, word);
        }

        // 刷新写缓存
        io.write_u8(ATA_COMMAND, ATA_CMD_FLUSH_CACHE);
        ata_delay(ctrl);
        wait_bsy(io, ctrl)
    }
}

// ============================================================================
// BlockDevice 适配器 (仅注册块设备 ata0-3)
// ============================================================================

/// ATA 驱动器的 `BlockDevice` 适配器
///
/// 不直接持有控制器, 经 `drive` 编号在 functions 全局 `ATA_CONTROLLERS`
/// 注册表中查找。仅含 `u8`/`u64` 纯数据字段, `Send + Sync` 自动派生, 0 unsafe。
pub struct AtaBlockDevice {
    /// 驱动器编号 (0-3)
    drive: u8,
    /// 缓存的磁盘容量 (512 字节扇区数)
    total_sectors: u64,
}

impl AtaBlockDevice {
    /// 为指定驱动器创建适配器
    ///
    /// 驱动器不存在或容量探测为 0 时返回 `None`。
    pub fn new(drive: u8) -> Option<Self> {
        let total_sectors = {
            let controllers = ATA_CONTROLLERS.lock();
            let controller = controllers.first()?;
            if !controller.disk_present(drive) {
                return None;
            }
            controller.total_sectors(drive)
        };

        if total_sectors == 0 {
            return None;
        }

        Some(Self {
            drive,
            total_sectors,
        })
    }
}

impl BlockDevice for AtaBlockDevice {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
        if sector > u64::from(u32::MAX) {
            return -1;
        }
        let controllers = ATA_CONTROLLERS.lock();
        let Some(controller) = controllers.first() else {
            return -1;
        };
        match controller.read_sector(self.drive, sector as u32, buf) {
            Ok(()) => 0,
            Err(()) => -1,
        }
    }

    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
        if sector > u64::from(u32::MAX) {
            return -1;
        }
        let controllers = ATA_CONTROLLERS.lock();
        let Some(controller) = controllers.first() else {
            return -1;
        };
        match controller.write_sector(self.drive, sector as u32, buf) {
            Ok(()) => 0,
            Err(()) => -1,
        }
    }

    fn blk_is_present(&self) -> bool {
        let controllers = ATA_CONTROLLERS.lock();
        controllers
            .first()
            .map_or(false, |controller| controller.disk_present(self.drive))
    }

    fn blk_total_sectors(&self) -> u64 {
        self.total_sectors
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(ATA_PRIMARY_IO, 0x1F0);
        assert_eq!(ATA_SECONDARY_IO, 0x170);
        assert_eq!(SECTOR_SIZE, 512);
        assert_eq!(WORDS_PER_SECTOR, 256);
        assert_eq!(MAX_ATA_DEVICES, 4);
    }

    #[test]
    fn test_device_default_state() {
        let device = AtaDevice::default();
        assert!(!device.present);
        assert!(device.is_master);
        assert_eq!(device.channel, 0);
        assert_eq!(device.total_sectors, 0);
    }

    #[test]
    fn test_controller_creation() {
        let controller = AtaController::new().expect("IoPort 构造应成功");
        assert!(!controller.primary_present);
        assert!(!controller.secondary_present);
        assert_eq!(controller.detected_device_count(), 0);
        assert!(!controller.is_initialized());
    }

    #[test]
    fn test_disk_present_bounds() {
        let controller = AtaController::new().expect("IoPort 构造应成功");

        // 未初始化时所有设备都不存在
        assert!(!controller.disk_present(0));
        assert!(!controller.disk_present(3));

        // 超出范围应该返回 false
        assert!(!controller.disk_present(4));
        assert!(!controller.disk_present(255));
        assert_eq!(controller.total_sectors(4), 0);
    }

    #[test]
    fn test_identify_capacity() {
        // LBA48 优先: word 100-103 拼 48 位, word100 为最低 16 位
        let mut words = [0u16; WORDS_PER_SECTOR];
        words[100] = 0x1234;
        words[101] = 0x5678;
        words[102] = 0x0001;
        words[103] = 0x0002;
        assert_eq!(identify_capacity(&words), 0x0002_0001_5678_1234);

        // LBA48 为 0 时回落 LBA28 (word 60-61)
        let mut words = [0u16; WORDS_PER_SECTOR];
        words[60] = 0x0000;
        words[61] = 0x0020;
        assert_eq!(identify_capacity(&words), 0x0020_0000);
    }

    #[test]
    fn test_block_device_absent_controller() {
        // 注册表为空时适配器构造应返回 None (不 panic)
        assert!(AtaBlockDevice::new(0).is_none());
    }
}
