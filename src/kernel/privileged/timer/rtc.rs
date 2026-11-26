#![deny(unsafe_code)]
//! 实时时钟 (RTC) 读取与回写 — 墙钟 epoch 的硬件来源
//!
//! 目标 (D7 T1): 启动时从平台 RTC 取得 Unix epoch 秒, 作为
//! `CLOCK_REALTIME` / `gettimeofday` / `time()` 的基准; `settimeofday`
//! 成功设置系统墙钟后尽力回写 RTC, 使下次启动仍从同一时间线开始.
//!
//! ## 平台
//!
//! - `x86_64`: MC146818 兼容 CMOS RTC (I/O 端口 0x70/0x71), 处理 UIP、
//!   BCD/二进制与 12/24 小时模式
//! - `aarch64`: PL031 RTC (设备树 `arm,pl031`; QEMU virt 默认 0x0901_0000),
//!   寄存器为 32 位 Unix 秒
//!
//! ## 失败语义
//!
//! RTC 不是启动必需条件: 读取失败返回 `None`, 调用方保留原墙钟 (自启动
//! 计数) 并只记录告警; 回写失败返回 `false`, 不回滚已设置的系统时间.
//!
//! SIMPLIFIED: 不处理时区与夏令时 (RTC 按 UTC 使用, 与 QEMU `-rtc base=utc`
//! 及常见 PC 默认一致); 影响面: 本地时区 RTC 会被当作 UTC; 何时需扩展:
//! 引入时区配置并在读/写两侧统一换算.

/// RTC 读取结果 (Unix epoch 秒)
///
/// 返回 `None` 表示硬件不可用、寄存器值非法或读取期间发生更新竞态.
#[must_use]
pub fn read_unix_seconds() -> Option<u64> {
    imp::read_unix_seconds()
}

/// 将 Unix epoch 秒回写到 RTC
///
/// 返回 `true` 表示写入后回读一致 (允许 3 秒内的计数推进); `false` 表示
/// 平台不可用、值超出 RTC 量程或回读不一致.
#[must_use]
pub fn write_unix_seconds(seconds: u64) -> bool {
    imp::write_unix_seconds(seconds)
}

/// 公历日期时间 → Unix epoch 秒 (UTC)
///
/// `year` 支持 1970..=9999; 月/日/时/分/秒做完整范围与闰年校验.
#[must_use]
pub fn civil_to_unix_seconds(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<u64> {
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    // Howard Hinnant 的 days_from_civil: 以 1970-01-01 为 0, 3 月为年首.
    let y = i64::from(year) - i64::from(u8::from(month <= 2));
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    let secs = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600)?
        .checked_add(i64::from(minute) * 60)?
        .checked_add(i64::from(second))?;
    u64::try_from(secs).ok()
}

/// Unix epoch 秒 → 公历日期时间 (UTC)
///
/// 返回 `(year, month, day, hour, minute, second)`; 超出 u32 秒量程的输入
/// 由调用方先行限制 (PL031 为 32 位秒).
#[must_use]
pub fn civil_from_unix_seconds(seconds: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let secs_of_day = seconds % 86_400;
    // days_from_civil 的逆运算.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(u8::from(m <= 2));
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        u32::try_from(m).unwrap_or(12),
        u32::try_from(d).unwrap_or(31),
        u32::try_from(secs_of_day / 3_600).unwrap_or(23),
        u32::try_from((secs_of_day % 3_600) / 60).unwrap_or(59),
        u32::try_from(secs_of_day % 60).unwrap_or(59),
    )
}

/// 指定年月的天数 (含闰年规则)
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// 公历闰年判定
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// x86_64 CMOS RTC 实现
#[cfg(target_arch = "x86_64")]
mod imp {
    #[cfg(not(feature = "host-test"))]
    use super::civil_from_unix_seconds;
    #[cfg(any(not(feature = "host-test"), test))]
    use super::civil_to_unix_seconds;
    #[cfg(not(feature = "host-test"))]
    use crate::privileged::ioport::IoPort;

    /// CMOS 索引端口
    #[cfg(not(feature = "host-test"))]
    const CMOS_INDEX: u16 = 0x70;
    /// 端口数量 (索引 + 数据)
    #[cfg(not(feature = "host-test"))]
    const CMOS_PORTS: u16 = 2;

    /// 寄存器: 秒
    #[cfg(not(feature = "host-test"))]
    const REG_SECOND: u8 = 0x00;
    /// 寄存器: 分
    #[cfg(not(feature = "host-test"))]
    const REG_MINUTE: u8 = 0x02;
    /// 寄存器: 时
    #[cfg(not(feature = "host-test"))]
    const REG_HOUR: u8 = 0x04;
    /// 寄存器: 日
    #[cfg(not(feature = "host-test"))]
    const REG_DAY: u8 = 0x07;
    /// 寄存器: 月
    #[cfg(not(feature = "host-test"))]
    const REG_MONTH: u8 = 0x08;
    /// 寄存器: 年 (两位)
    #[cfg(not(feature = "host-test"))]
    const REG_YEAR: u8 = 0x09;
    /// 寄存器: 状态 A (bit7 = UIP)
    #[cfg(not(feature = "host-test"))]
    const REG_STATUS_A: u8 = 0x0A;
    /// 寄存器: 状态 B (bit2 = 二进制, bit1 = 24h)
    #[cfg(not(feature = "host-test"))]
    const REG_STATUS_B: u8 = 0x0B;
    /// 寄存器: 世纪
    #[cfg(not(feature = "host-test"))]
    const REG_CENTURY: u8 = 0x32;
    /// 状态 A 的更新进行中位
    #[cfg(not(feature = "host-test"))]
    const STATUS_A_UIP: u8 = 0x80;
    /// 状态 B 的二进制模式位
    #[cfg(any(not(feature = "host-test"), test))]
    const STATUS_B_BINARY: u8 = 0x04;
    /// 状态 B 的 24 小时模式位
    #[cfg(any(not(feature = "host-test"), test))]
    const STATUS_B_24H: u8 = 0x02;
    /// UIP 等待上限 (每次寄存器读都重新检查, 足够覆盖一次更新周期)
    #[cfg(not(feature = "host-test"))]
    const UIP_SPIN_LIMIT: usize = 1_000_000;
    /// 读取重试次数 (跨秒回卷时重读)
    #[cfg(not(feature = "host-test"))]
    const READ_RETRIES: usize = 3;

    /// 构造 CMOS 端口句柄
    #[cfg(not(feature = "host-test"))]
    fn open() -> Option<IoPort> {
        IoPort::new_safe(CMOS_INDEX, CMOS_PORTS, "rtc-cmos").ok()
    }

    /// 读 CMOS 寄存器, 保留调用方当前的 NMI 屏蔽位
    #[cfg(not(feature = "host-test"))]
    fn read_reg(port: &IoPort, index: u8) -> u8 {
        let saved = port.read_u8(0);
        let nmi = saved & 0x80;
        port.write_u8(0, nmi | (index & 0x7F));
        port.read_u8(1)
    }

    /// 写 CMOS 寄存器, 保留调用方当前的 NMI 屏蔽位
    #[cfg(not(feature = "host-test"))]
    fn write_reg(port: &IoPort, index: u8, value: u8) {
        let saved = port.read_u8(0);
        let nmi = saved & 0x80;
        port.write_u8(0, nmi | (index & 0x7F));
        port.write_u8(1, value);
    }

    /// 等待 RTC 退出更新周期
    #[cfg(not(feature = "host-test"))]
    fn wait_not_updating(port: &IoPort) -> bool {
        for _ in 0..UIP_SPIN_LIMIT {
            if read_reg(port, REG_STATUS_A) & STATUS_A_UIP == 0 {
                return true;
            }
        }
        false
    }

    /// BCD 字节转二进制 (非法半字节返回 `None`)
    #[cfg(any(not(feature = "host-test"), test))]
    fn decode_bcd(value: u8) -> Option<u8> {
        let high = value >> 4;
        let low = value & 0x0F;
        if high > 9 || low > 9 {
            return None;
        }
        Some(high * 10 + low)
    }

    /// 按状态 B 的编码模式解码一个字段
    #[cfg(any(not(feature = "host-test"), test))]
    fn decode_field(value: u8, binary: bool) -> Option<u8> {
        if binary {
            Some(value)
        } else {
            decode_bcd(value)
        }
    }

    /// 编码一个字段为 CMOS 当前模式 (二进制或 BCD)
    #[cfg(any(not(feature = "host-test"), test))]
    fn encode_field(value: u8, binary: bool) -> u8 {
        if binary {
            value
        } else {
            (value / 10) << 4 | (value % 10)
        }
    }

    /// CMOS 一次完整快照 (寄存器原始值)
    #[cfg(any(not(feature = "host-test"), test))]
    #[derive(Clone, Copy)]
    struct CmosSnapshot {
        second: u8,
        minute: u8,
        hour: u8,
        day: u8,
        month: u8,
        year: u8,
        status_b: u8,
        century: u8,
    }

    /// 用一次完整快照解码 CMOS 时间 (纯函数, 便于单测)
    #[cfg(any(not(feature = "host-test"), test))]
    fn decode_snapshot(snapshot: CmosSnapshot) -> Option<u64> {
        let CmosSnapshot {
            second,
            minute,
            hour,
            day,
            month,
            year,
            status_b,
            century,
        } = snapshot;
        let binary = status_b & STATUS_B_BINARY != 0;
        let is_24h = status_b & STATUS_B_24H != 0;
        let second = decode_field(second, binary)?;
        let minute = decode_field(minute, binary)?;
        // 12 小时制的 PM 位是 bit7, 必须在 BCD 解码前剥离.
        let pm = !is_24h && hour & 0x80 != 0;
        let hour_field = if is_24h { hour } else { hour & 0x7F };
        let raw_hour = decode_field(hour_field, binary)?;
        let day = decode_field(day, binary)?;
        let month = decode_field(month, binary)?;
        let year_2 = decode_field(year, binary)?;

        let hour = if is_24h {
            raw_hour
        } else {
            if !(1..=12).contains(&raw_hour) {
                return None;
            }
            let h24 = if raw_hour == 12 { 0 } else { raw_hour };
            if pm { h24 + 12 } else { h24 }
        };
        if hour > 23 {
            return None;
        }

        let century = decode_field(century, binary)
            .filter(|value| (19..=99).contains(value))
            .unwrap_or(if year_2 < 70 { 20 } else { 19 });
        let year = i32::from(century) * 100 + i32::from(year_2);
        civil_to_unix_seconds(
            year,
            u32::from(month),
            u32::from(day),
            u32::from(hour),
            u32::from(minute),
            u32::from(second),
        )
    }

    /// 一次快照读取; 秒字段前后不一致视为跨秒回卷并重试
    #[cfg(not(feature = "host-test"))]
    fn read_once(port: &IoPort) -> Option<u64> {
        if !wait_not_updating(port) {
            return None;
        }
        let status_b = read_reg(port, REG_STATUS_B);
        let second_first = read_reg(port, REG_SECOND);
        let minute = read_reg(port, REG_MINUTE);
        let hour = read_reg(port, REG_HOUR);
        let day = read_reg(port, REG_DAY);
        let month = read_reg(port, REG_MONTH);
        let year = read_reg(port, REG_YEAR);
        let century = read_reg(port, REG_CENTURY);
        let second_last = read_reg(port, REG_SECOND);
        if second_first != second_last {
            return None;
        }
        decode_snapshot(CmosSnapshot {
            second: second_last,
            minute,
            hour,
            day,
            month,
            year,
            status_b,
            century,
        })
    }

    /// 读取 x86 CMOS RTC
    pub(super) fn read_unix_seconds() -> Option<u64> {
        #[cfg(feature = "host-test")]
        {
            // host-test 运行在宿主 Linux 上, 执行 inb 会触发 SIGSEGV; 硬件路径
            // 不在 host 单测范围内, 由 QEMU 启动腿验收.
            None
        }
        #[cfg(not(feature = "host-test"))]
        {
            read_unix_seconds_hw()
        }
    }

    /// 读取 x86 CMOS RTC 的硬件路径
    #[cfg(not(feature = "host-test"))]
    fn read_unix_seconds_hw() -> Option<u64> {
        let port = open()?;
        for _ in 0..READ_RETRIES {
            if let Some(seconds) = read_once(&port) {
                return Some(seconds);
            }
        }
        None
    }

    /// 回写 x86 CMOS RTC
    pub(super) fn write_unix_seconds(seconds: u64) -> bool {
        #[cfg(feature = "host-test")]
        {
            let _ = seconds;
            false
        }
        #[cfg(not(feature = "host-test"))]
        {
            write_unix_seconds_hw(seconds)
        }
    }

    /// 回写 x86 CMOS RTC 的硬件路径
    #[cfg(not(feature = "host-test"))]
    fn write_unix_seconds_hw(seconds: u64) -> bool {
        let Some(port) = open() else {
            return false;
        };
        let (year, month, day, hour, minute, second) = civil_from_unix_seconds(seconds);
        let Ok(year) = u16::try_from(year) else {
            return false;
        };
        if !(1970..=9999).contains(&year) || !wait_not_updating(&port) {
            return false;
        }

        let Ok(month) = u8::try_from(month) else {
            return false;
        };
        let Ok(day) = u8::try_from(day) else {
            return false;
        };
        let Ok(hour) = u8::try_from(hour) else {
            return false;
        };
        let Ok(minute) = u8::try_from(minute) else {
            return false;
        };
        let Ok(second) = u8::try_from(second) else {
            return false;
        };
        let Ok(year_2) = u8::try_from(year % 100) else {
            return false;
        };
        let Ok(century) = u8::try_from(year / 100) else {
            return false;
        };

        let status_b = read_reg(&port, REG_STATUS_B);
        let binary = status_b & STATUS_B_BINARY != 0;
        let is_24h = status_b & STATUS_B_24H != 0;
        let hour_field = if is_24h {
            encode_field(hour, binary)
        } else {
            let pm = hour >= 12;
            let h12 = match hour % 12 {
                0 => 12,
                value => value,
            };
            let encoded = encode_field(h12, binary);
            if pm { encoded | 0x80 } else { encoded }
        };

        write_reg(&port, REG_YEAR, encode_field(year_2, binary));
        write_reg(&port, REG_MONTH, encode_field(month, binary));
        write_reg(&port, REG_DAY, encode_field(day, binary));
        write_reg(&port, REG_HOUR, hour_field);
        write_reg(&port, REG_MINUTE, encode_field(minute, binary));
        write_reg(&port, REG_SECOND, encode_field(second, binary));
        write_reg(&port, REG_CENTURY, encode_field(century, binary));

        // 回读校验: RTC 计数在写入后仍会推进, 允许 3 秒容差.
        read_unix_seconds().is_some_and(|actual| actual.abs_diff(seconds) <= 3)
    }

    #[cfg(test)]
    mod tests {
        use super::{CmosSnapshot, decode_snapshot, encode_field};

        #[test]
        fn decode_snapshot_bcd_utc() {
            // 2026-11-26 15:27:47 UTC, BCD + 24h.
            let seconds = decode_snapshot(CmosSnapshot {
                second: 0x47,
                minute: 0x27,
                hour: 0x15,
                day: 0x26,
                month: 0x11,
                year: 0x26,
                status_b: 0x02,
                century: 0x20,
            })
            .expect("valid snapshot");
            assert_eq!(seconds, 1_795_706_867);
        }

        #[test]
        fn decode_snapshot_binary_and_century_fallback() {
            // 2026-11-26 15:27:47, binary + 24h, 世纪寄存器不可用.
            let seconds = decode_snapshot(CmosSnapshot {
                second: 47,
                minute: 27,
                hour: 15,
                day: 26,
                month: 11,
                year: 26,
                status_b: 0x06,
                century: 0x00,
            })
            .expect("valid snapshot");
            assert_eq!(seconds, 1_795_706_867);
        }

        #[test]
        fn decode_snapshot_12h_pm() {
            // 2026-11-26 15:27:47 = 12h 模式下 3:27:47 PM.
            let seconds = decode_snapshot(CmosSnapshot {
                second: 0x47,
                minute: 0x27,
                hour: 0x83,
                day: 0x26,
                month: 0x11,
                year: 0x26,
                status_b: 0x00,
                century: 0x20,
            })
            .expect("valid snapshot");
            assert_eq!(seconds, 1_795_706_867);
        }

        #[test]
        fn encode_field_bcd_roundtrip() {
            assert_eq!(encode_field(47, false), 0x47);
            assert_eq!(encode_field(15, false), 0x15);
            assert_eq!(encode_field(15, true), 15);
        }
    }
}

/// aarch64 PL031 RTC 实现
#[cfg(target_arch = "aarch64")]
mod imp {
    use crate::privileged::dtb;
    use crate::privileged::iomem::IoMem;
    use crate::privileged::mm::PhysAddr;
    use crate::privileged::sync::IrqSpinLock;

    /// QEMU virt 的 PL031 默认基址 (设备树缺失时回退)
    const PL031_DEFAULT_BASE: u64 = 0x0901_0000;
    /// PL031 寄存器帧长度 (4 KiB)
    const PL031_LEN: usize = 0x1000;
    /// 寄存器: 数据 (Unix 秒)
    const REG_DATA: usize = 0x00;
    /// 寄存器: 加载值
    const REG_LOAD: usize = 0x04;
    /// 寄存器: 控制
    const REG_CONTROL: usize = 0x08;
    /// 控制寄存器使能位
    const CONTROL_ENABLE: u32 = 1;

    /// 全局 PL031 句柄; 首次访问时按设备树基址建立
    static PL031: IrqSpinLock<Option<IoMem>> = IrqSpinLock::new(None);

    /// 在持锁状态下访问 PL031 寄存器
    fn with_mem<R>(f: impl FnOnce(&IoMem) -> R) -> Option<R> {
        let mut guard = PL031.lock();
        if guard.is_none() {
            let base = dtb::rtc_base().unwrap_or(PL031_DEFAULT_BASE);
            *guard = IoMem::from_platform_device(PhysAddr::new(base), PL031_LEN, "pl031").ok();
            if let Some(mem) = guard.as_ref() {
                // 确保计数器使能; 复位值通常已为 1, 此处幂等.
                if mem.read_u32(REG_CONTROL) & CONTROL_ENABLE == 0 {
                    mem.write_u32(REG_CONTROL, CONTROL_ENABLE);
                }
            }
        }
        guard.as_ref().map(f)
    }

    /// 读取 aarch64 PL031 RTC
    pub(super) fn read_unix_seconds() -> Option<u64> {
        with_mem(|mem| u64::from(mem.read_u32(REG_DATA)))
    }

    /// 回写 aarch64 PL031 RTC
    pub(super) fn write_unix_seconds(seconds: u64) -> bool {
        let Ok(seconds) = u32::try_from(seconds) else {
            return false;
        };
        let written = with_mem(|mem| {
            // 先关计数器 → 写加载值 → 重新使能, 与 Linux PL031 驱动顺序一致.
            mem.write_u32(REG_CONTROL, 0);
            mem.write_u32(REG_LOAD, seconds);
            mem.write_u32(REG_CONTROL, CONTROL_ENABLE);
        });
        if written.is_none() {
            return false;
        }
        read_unix_seconds().is_some_and(|actual| actual.abs_diff(u64::from(seconds)) <= 3)
    }
}

#[cfg(test)]
mod tests {
    use super::{civil_from_unix_seconds, civil_to_unix_seconds};

    #[test]
    fn civil_epoch_is_zero() {
        assert_eq!(civil_to_unix_seconds(1970, 1, 1, 0, 0, 0), Some(0));
    }

    #[test]
    fn civil_known_dates() {
        assert_eq!(
            civil_to_unix_seconds(2000, 1, 1, 0, 0, 0),
            Some(946_684_800)
        );
        assert_eq!(
            civil_to_unix_seconds(2024, 2, 29, 0, 0, 0),
            Some(1_709_164_800)
        );
        assert_eq!(
            civil_to_unix_seconds(2026, 11, 26, 15, 27, 47),
            Some(1_795_706_867)
        );
    }

    #[test]
    fn civil_roundtrip() {
        for seconds in [0, 946_684_800, 1_709_164_800, 1_795_706_867, 2_147_483_647] {
            let (year, month, day, hour, minute, second) = civil_from_unix_seconds(seconds);
            assert_eq!(
                civil_to_unix_seconds(year, month, day, hour, minute, second),
                Some(seconds)
            );
        }
    }

    #[test]
    fn civil_rejects_invalid_fields() {
        assert_eq!(civil_to_unix_seconds(2025, 2, 29, 0, 0, 0), None);
        assert_eq!(civil_to_unix_seconds(2026, 13, 1, 0, 0, 0), None);
        assert_eq!(civil_to_unix_seconds(2026, 1, 1, 24, 0, 0), None);
        assert_eq!(civil_to_unix_seconds(1969, 12, 31, 0, 0, 0), None);
    }
}
