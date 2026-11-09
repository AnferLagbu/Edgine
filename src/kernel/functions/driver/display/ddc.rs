#![deny(unsafe_code)]
//! DDC (Display Data Channel) I2C bitbang 协议 — functions 层安全实现
//!
//! 通过 IoMem 安全代理访问 HDMI 控制器 MMIO 寄存器, 实现 I2C bitbang 协议.
//! 所有寄存器读写均通过 IoMem 的安全接口 (bounds-checked), 无 unsafe.
//!
//! ## 协议层
//!
//! - **100 kHz 标准模式**: 半周期 ~5 µs, spin_loop 延时
//! - **MSB first**: 字节传输高位先行
//! - **ACK 采样**: 主机发送 8 bit 后释放 SDA, 读 1 bit 作为从机应答
//!
//! ## 事务超时
//!
//! - 事务墙钟预算: 100 ms (覆盖完整 EDID 读: 2 START + 3 写字节 + 128 读字节 + STOP)
//! - 时基未建立 (无硬件计数器) 时回退为 50_000 轮迭代上限
//! - 单字节传输: 8 bit + ACK = 10 次延时调用

use crate::privileged::iomem::IoMem;

// ============================================================================
// DDC 寄存器偏移
// ============================================================================

/// DDC 控制寄存器默认偏移 (8-bit)
///
/// bit 0: SDA 输出 (1=高, 0=低)
/// bit 1: SCL 输出 (1=高, 0=低)
pub const DDC_DEFAULT_CTRL_REG: usize = 0x050;

/// DDC 状态寄存器默认偏移 (8-bit)
///
/// bit 0: SDA 输入 (从机驱动)
pub const DDC_DEFAULT_STATUS_REG: usize = 0x054;

/// DDC SDA 输出 bit (bit 0)
const SDA_OUT_BIT: u8 = 0x01;
/// DDC SCL 输出 bit (bit 1)
const SCL_OUT_BIT: u8 = 0x02;
/// DDC SDA 输入 bit (bit 0, status 寄存器)
const SDA_IN_BIT: u8 = 0x01;

/// EDID I2C 从机地址 (写模式, 0xA0 = 0x50 << 1)
const EDID_ADDR_WRITE: u8 = 0xA0;
/// EDID I2C 从机地址 (读模式, 0xA1 = 0x50 << 1 | 1)
const EDID_ADDR_READ: u8 = 0xA1;

/// DDC I2C 时序延时 (spin loop 次数, 适配 ~100 kHz 标准模式)
const DDC_DELAY_ITERS: usize = 50;

/// DDC I2C 事务墙钟预算 (完整 EDID 读 ≈ 1.3k 次时序延时)
const DDC_TRANSACTION_TIMEOUT_MS: u64 = 100;

/// 事务预算的回退轮数上限 (时基未建立时使用; 一轮 = 一次时序延时,
/// 完整 EDID 读约 2.6k 轮)
const DDC_TRANSACTION_TIMEOUT_ROUNDS: u32 = 4_000;

/// DDC 读取错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdcError {
    /// I2C 从机无响应 (NACK)
    Nack,
    /// 事务超时 (总线挂起或从机无响应)
    Timeout,
}

// ============================================================================
// DDC 协议原子
// ============================================================================

/// 单比特时序延时 (预算耗尽返回 `Err`, 回退模式按此粒度计轮)
#[inline]
fn ddc_delay(deadline: &mut crate::privileged::timer::Deadline) -> Result<(), DdcError> {
    if deadline.expired() {
        return Err(DdcError::Timeout);
    }
    for _ in 0..DDC_DELAY_ITERS {
        core::hint::spin_loop();
    }
    Ok(())
}

/// 同时设置 SDA 与 SCL 输出电平
#[inline]
fn set_sda_scl(iomem: &IoMem, ctrl_reg: usize, sda_high: bool, scl_high: bool) {
    let mut val = 0u8;
    if sda_high {
        val |= SDA_OUT_BIT;
    }
    if scl_high {
        val |= SCL_OUT_BIT;
    }
    iomem.write_u8(ctrl_reg, val);
}

/// I2C START 条件: SDA 在 SCL 高电平时由高变低
fn i2c_start(
    iomem: &IoMem,
    ctrl_reg: usize,
    deadline: &mut crate::privileged::timer::Deadline,
) -> Result<(), DdcError> {
    if deadline.expired() {
        return Err(DdcError::Timeout);
    }
    set_sda_scl(iomem, ctrl_reg, true, true);
    ddc_delay(deadline)?;
    set_sda_scl(iomem, ctrl_reg, false, true);
    ddc_delay(deadline)?;
    Ok(())
}

/// I2C STOP 条件: SDA 在 SCL 高电平时由低变高
fn i2c_stop(
    iomem: &IoMem,
    ctrl_reg: usize,
    deadline: &mut crate::privileged::timer::Deadline,
) -> Result<(), DdcError> {
    if deadline.expired() {
        return Err(DdcError::Timeout);
    }
    set_sda_scl(iomem, ctrl_reg, false, true);
    ddc_delay(deadline)?;
    set_sda_scl(iomem, ctrl_reg, true, true);
    ddc_delay(deadline)?;
    Ok(())
}

/// I2C 写 1 字节 (MSB first) 并采样 ACK
///
/// 返回 `Ok(true)` = ACK, `Ok(false)` = NACK
fn i2c_write_byte(
    iomem: &IoMem,
    ctrl_reg: usize,
    status_reg: usize,
    byte: u8,
    deadline: &mut crate::privileged::timer::Deadline,
) -> Result<bool, DdcError> {
    if deadline.expired() {
        return Err(DdcError::Timeout);
    }
    for i in 0..8u8 {
        let bit = (byte >> (7 - i)) & 1 != 0;
        set_sda_scl(iomem, ctrl_reg, bit, false);
        ddc_delay(deadline)?;
        set_sda_scl(iomem, ctrl_reg, bit, true);
        ddc_delay(deadline)?;
    }
    // 释放 SDA 让从机 ACK
    set_sda_scl(iomem, ctrl_reg, true, false);
    ddc_delay(deadline)?;
    set_sda_scl(iomem, ctrl_reg, true, true);
    ddc_delay(deadline)?;
    // 采样 SDA: 0 = ACK, 1 = NACK
    let sda = iomem.read_u8(status_reg) & SDA_IN_BIT;
    set_sda_scl(iomem, ctrl_reg, true, false);
    Ok(sda == 0)
}

/// I2C 读 1 字节 (MSB first), 由主机发送 ACK/NACK
///
/// `send_ack = true`: 读完后主机 ACK (从机继续发送)
/// `send_ack = false`: NACK (从机停止发送, 用于读最后 1 字节)
fn i2c_read_byte(
    iomem: &IoMem,
    ctrl_reg: usize,
    status_reg: usize,
    send_ack: bool,
    deadline: &mut crate::privileged::timer::Deadline,
) -> Result<u8, DdcError> {
    if deadline.expired() {
        return Err(DdcError::Timeout);
    }
    let mut byte = 0u8;
    // 释放 SDA 让从机驱动
    set_sda_scl(iomem, ctrl_reg, true, false);
    ddc_delay(deadline)?;

    for i in 0..8u8 {
        set_sda_scl(iomem, ctrl_reg, true, true);
        ddc_delay(deadline)?;
        let bit = iomem.read_u8(status_reg) & SDA_IN_BIT;
        byte |= bit << (7 - i);
        set_sda_scl(iomem, ctrl_reg, true, false);
        ddc_delay(deadline)?;
    }

    // 主机发送 ACK/NACK
    set_sda_scl(iomem, ctrl_reg, !send_ack, false);
    ddc_delay(deadline)?;
    set_sda_scl(iomem, ctrl_reg, !send_ack, true);
    ddc_delay(deadline)?;
    set_sda_scl(iomem, ctrl_reg, !send_ack, false);

    Ok(byte)
}

// ============================================================================
// 公共 API
// ============================================================================

/// 通过 DDC 总线读取 EDID 块 (128 字节)
///
/// I2C 事务序列:
/// ```text
/// START -> [0xA0] -> [offset] -> REPEATED_START -> [0xA1] -> [128 字节] -> STOP
/// ```
///
/// # Errors
///
/// - 从机无响应 (NACK) 时返回 [`DdcError::Nack`]
/// - I2C 事务超时 (总线挂起或从机无响应) 时返回 [`DdcError::Timeout`]
pub fn read_edid_block(
    iomem: &IoMem,
    ctrl_reg: usize,
    status_reg: usize,
    block: u8,
) -> Result<[u8; 128], DdcError> {
    let mut data = [0u8; 128];
    let mut deadline = crate::privileged::timer::Deadline::after_ms(
        DDC_TRANSACTION_TIMEOUT_MS,
        DDC_TRANSACTION_TIMEOUT_ROUNDS,
    );

    i2c_start(iomem, ctrl_reg, &mut deadline)?;
    match i2c_write_byte(iomem, ctrl_reg, status_reg, EDID_ADDR_WRITE, &mut deadline) {
        Ok(true) => {}
        Ok(false) => {
            let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
            return Err(DdcError::Nack);
        }
        Err(e) => {
            let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
            return Err(e);
        }
    }

    let offset = block.wrapping_mul(128);
    match i2c_write_byte(iomem, ctrl_reg, status_reg, offset, &mut deadline) {
        Ok(true) => {}
        Ok(false) => {
            let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
            return Err(DdcError::Nack);
        }
        Err(e) => {
            let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
            return Err(e);
        }
    }

    // REPEATED START 切换到读模式
    i2c_start(iomem, ctrl_reg, &mut deadline)?;
    match i2c_write_byte(iomem, ctrl_reg, status_reg, EDID_ADDR_READ, &mut deadline) {
        Ok(true) => {}
        Ok(false) => {
            let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
            return Err(DdcError::Nack);
        }
        Err(e) => {
            let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
            return Err(e);
        }
    }

    // 读 128 字节: 前 127 字节 ACK, 最后 1 字节 NACK
    for byte in data.iter_mut().take(127) {
        match i2c_read_byte(iomem, ctrl_reg, status_reg, true, &mut deadline) {
            Ok(b) => *byte = b,
            Err(e) => {
                let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
                return Err(e);
            }
        }
    }
    match i2c_read_byte(iomem, ctrl_reg, status_reg, false, &mut deadline) {
        Ok(b) => data[127] = b,
        Err(e) => {
            let _ = i2c_stop(iomem, ctrl_reg, &mut deadline);
            return Err(e);
        }
    }

    i2c_stop(iomem, ctrl_reg, &mut deadline)?;
    Ok(data)
}
