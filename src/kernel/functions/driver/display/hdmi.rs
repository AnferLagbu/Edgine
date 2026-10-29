#![deny(unsafe_code)]
//! HDMI (High-Definition Multimedia Interface) 驱动 — functions 层安全实现
//!
//! 提供 HDMI 控制器的 safe 业务逻辑:
//! - EDID 解析 (纯数据, 无硬件访问)
//! - 视频模式管理
//! - 时序参数派生 (DMT lookup + 公式 fallback)
//! - 像素时钟计算
//!
//! 硬件寄存器访问通过 IoMem 安全代理, 无 unsafe.
#![deny(missing_docs)]

use crate::privileged::driver::{DeviceType, Driver, DriverError, DriverResult};
use crate::privileged::iomem::IoMem;

// ============================================================================
// DDC 寄存器偏移 (从 functions::driver::display::ddc 重导出)
// ============================================================================

/// HPD 状态寄存器偏移 (8-bit, bit 0 = connected)
pub const HPD_STATUS_REG_OFFSET: usize = 0x038;
/// HPD 状态 bit (bit 0 = connected)
pub const HPD_STATUS_BIT: u8 = 0x01;

/// `IoMem` 最小大小 (覆盖所有默认寄存器)
pub const REQUIRED_IOMEM_SIZE: usize = 0x07A;

// ============================================================================
// 像素时钟常量
// ============================================================================

/// HDMI 像素时钟参考时钟 (kHz), 默认 27 MHz
const PCLK_BASE_KHZ: u32 = 27_000;

/// 像素时钟乘法寄存器默认偏移 (8-bit)
const PCLK_MUL_REG_OFFSET: usize = 0x060;
/// 像素时钟除法寄存器默认偏移 (8-bit)
const PCLK_DIV_REG_OFFSET: usize = 0x064;
/// PLL 锁定状态寄存器默认偏移 (8-bit, bit 0 = locked)
const PCLK_LOCK_REG_OFFSET: usize = 0x066;
/// PLL 锁定 bit
const PCLK_LOCK_BIT: u8 = 0x01;
/// PLL 锁定轮询超时 (`500_000` iters ≈ 10 ms)
const PLL_LOCK_TIMEOUT_ITERS: usize = 500_000;

// ============================================================================
// 同步极性 + TMDS 寄存器偏移
// ============================================================================

/// 同步极性寄存器偏移 (8-bit)
const SYNC_POL_REG_OFFSET: usize = 0x078;
/// H 同步极性 bit (bit 0)
const SYNC_POL_H_BIT: u8 = 0x01;
/// V 同步极性 bit (bit 1)
const SYNC_POL_V_BIT: u8 = 0x02;

/// TMDS 输出使能寄存器偏移 (8-bit)
const TMDS_ENABLE_REG_OFFSET: usize = 0x079;
/// TMDS 输出使能 bit (bit 0)
const TMDS_ENABLE_BIT: u8 = 0x01;

// ============================================================================
// 时序寄存器偏移 (16-bit, 每项占 2 字节)
// ============================================================================

const H_TOTAL_REG_OFFSET: usize = 0x068;
const H_ACTIVE_REG_OFFSET: usize = 0x06A;
const V_TOTAL_REG_OFFSET: usize = 0x06C;
const V_ACTIVE_REG_OFFSET: usize = 0x06E;
const H_SYNC_OFFSET_REG: usize = 0x070;
const H_SYNC_PW_REG: usize = 0x072;
const V_SYNC_OFFSET_REG: usize = 0x074;
const V_SYNC_PW_REG: usize = 0x076;

// ============================================================================
// 数据类型
// ============================================================================

/// 视频模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    /// 水平像素数.
    pub width: u16,
    /// 垂直像素数.
    pub height: u16,
    /// 刷新率 (Hz).
    pub refresh_rate: u8,
    /// 像素时钟频率 (kHz).
    pub pixel_clock_khz: u32,
    /// 扫描方式与同步极性标志.
    pub flags: VideoModeFlags,
}

/// 视频模式标志 (位域封装: 隔行扫描 / 双扫描 / 水平正极性 / 垂直正极性)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoModeFlags {
    /// 标志位 (bit0 隔行, bit1 双扫描, bit2 水平正极性, bit3 垂直正极性)
    bits: u8,
}

impl VideoModeFlags {
    /// 隔行扫描标志位.
    const INTERLACED: u8 = 1 << 0;
    /// 双扫描标志位.
    const DOUBLE_SCAN: u8 = 1 << 1;
    /// 水平同步正极性标志位.
    const HSYNC_POSITIVE: u8 = 1 << 2;
    /// 垂直同步正极性标志位.
    const VSYNC_POSITIVE: u8 = 1 << 3;

    /// 全部标志为假 (非隔行 / 非双扫描 / 负极性同步).
    pub const NONE: Self = Self { bits: 0 };

    /// 置位「隔行扫描」标志.
    #[must_use]
    pub const fn with_interlaced(self, enabled: bool) -> Self {
        Self {
            bits: self.bits | if enabled { Self::INTERLACED } else { 0 },
        }
    }

    /// 置位「双扫描」标志.
    #[must_use]
    pub const fn with_double_scan(self, enabled: bool) -> Self {
        Self {
            bits: self.bits | if enabled { Self::DOUBLE_SCAN } else { 0 },
        }
    }

    /// 置位「水平同步正极性」标志.
    #[must_use]
    pub const fn with_hsync_positive(self, enabled: bool) -> Self {
        Self {
            bits: self.bits | if enabled { Self::HSYNC_POSITIVE } else { 0 },
        }
    }

    /// 置位「垂直同步正极性」标志.
    #[must_use]
    pub const fn with_vsync_positive(self, enabled: bool) -> Self {
        Self {
            bits: self.bits | if enabled { Self::VSYNC_POSITIVE } else { 0 },
        }
    }

    /// 是否隔行扫描.
    #[must_use]
    pub const fn interlaced(self) -> bool {
        self.bits & Self::INTERLACED != 0
    }

    /// 是否双扫描 (每帧行数减半刷新加倍).
    #[must_use]
    pub const fn double_scan(self) -> bool {
        self.bits & Self::DOUBLE_SCAN != 0
    }

    /// 水平同步是否为正极性.
    #[must_use]
    pub const fn hsync_positive(self) -> bool {
        self.bits & Self::HSYNC_POSITIVE != 0
    }

    /// 垂直同步是否为正极性.
    #[must_use]
    pub const fn vsync_positive(self) -> bool {
        self.bits & Self::VSYNC_POSITIVE != 0
    }
}

/// HDMI 时序参数 (从 `VideoMode` 派生)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTiming {
    /// 水平有效像素数.
    pub h_active: u16,
    /// 水平总像素数 (含消隐区).
    pub h_total: u16,
    /// 水平同步起点偏移 (像素).
    pub h_sync_offset: u16,
    /// 水平同步脉冲宽度 (像素).
    pub h_sync_pulse_width: u16,
    /// 垂直有效行数.
    pub v_active: u16,
    /// 垂直总行数 (含消隐区).
    pub v_total: u16,
    /// 垂直同步起点偏移 (行).
    pub v_sync_offset: u16,
    /// 垂直同步脉冲宽度 (行).
    pub v_sync_pulse_width: u16,
}

/// EDID 基本显示参数
#[derive(Debug, Clone, Copy)]
pub struct EdidBasicDisplay {
    /// 视频输入类型 (数字/模拟及其位深等编码).
    pub video_input_type: u8,
    /// 屏幕水平尺寸 (厘米).
    pub horizontal_image_size: u8,
    /// 屏幕垂直尺寸 (厘米).
    pub vertical_image_size: u8,
    /// 显示传输特性 (伽马编码).
    pub display_transfer_characteristic: u8,
    /// 特性支持位 (电源管理、首选时序等).
    pub feature_support: u8,
}

/// EDID 颜色特性
#[derive(Debug, Clone, Copy)]
pub struct EdidColorCharacteristics {
    /// 红/绿低 2 位色度打包字节.
    pub red_green_low: u8,
    /// 蓝/白低 2 位色度打包字节.
    pub blue_white_low: u8,
    /// 红色度 x 高位.
    pub red_x: u8,
    /// 红色度 y 高位.
    pub red_y: u8,
    /// 绿色度 x 高位.
    pub green_x: u8,
    /// 绿色度 y 高位.
    pub green_y: u8,
    /// 蓝色度 x 高位.
    pub blue_x: u8,
    /// 蓝色度 y 高位.
    pub blue_y: u8,
    /// 白色点色度 x 高位.
    pub white_x: u8,
    /// 白色点色度 y 高位.
    pub white_y: u8,
}

/// EDID 详细时序描述符
#[derive(Debug, Clone, Copy)]
pub struct EdidDetailedTiming {
    /// 像素时钟 (10 kHz 单位).
    pub pixel_clock: u16,
    /// 水平有效像素低 8 位.
    pub horizontal_active: u8,
    /// 水平消隐像素低 8 位.
    pub horizontal_blanking: u8,
    /// 水平有效像素高 4 位 (bit 7..4).
    pub horizontal_active_high: u8,
    /// 水平消隐像素高 4 位 (bit 3..0).
    pub horizontal_blanking_high: u8,
    /// 垂直有效行低 8 位.
    pub vertical_active: u8,
    /// 垂直消隐行低 8 位.
    pub vertical_blanking: u8,
    /// 垂直有效/消隐行高 4 位打包字节.
    pub vertical_active_blanking_high: u8,
    /// 水平同步偏移 (像素).
    pub horizontal_sync_offset: u8,
    /// 水平同步脉冲宽度 (像素).
    pub horizontal_sync_pulse_width: u8,
    /// 垂直同步偏移与脉宽打包字节.
    pub vertical_sync_offset_width: u8,
    /// 同步类型标志 (数字分离/复合、极性等).
    pub sync_type: u8,
    /// 图像尺寸 (毫米, 高/宽各 4 位).
    pub image_size: u8,
    /// 边框像素 (上/下/左/右各 2 位).
    pub border: u8,
    /// 特性标志 (隔行扫描等).
    pub features: u8,
}

impl EdidDetailedTiming {
    /// 获取水平分辨率
    #[must_use]
    pub fn horizontal_resolution(self) -> u16 {
        u16::from(self.horizontal_active) | ((u16::from(self.horizontal_active_high) & 0xF0) << 4)
    }

    /// 获取垂直分辨率
    #[must_use]
    pub fn vertical_resolution(self) -> u16 {
        u16::from(self.vertical_active)
            | ((u16::from(self.vertical_active_blanking_high) & 0xF0) << 4)
    }

    /// 获取刷新率 (近似)
    #[must_use]
    pub fn refresh_rate(self) -> u32 {
        if self.pixel_clock == 0 {
            return 60;
        }
        let h_total = u32::from(self.horizontal_resolution())
            + (u32::from(self.horizontal_blanking)
                | ((u32::from(self.horizontal_blanking_high) & 0x0F) << 8));
        let v_total = u32::from(self.vertical_resolution())
            + (u32::from(self.vertical_blanking)
                | ((u32::from(self.vertical_active_blanking_high) & 0x0F) << 8));
        if h_total == 0 || v_total == 0 {
            return 60;
        }
        let pixel_clock_khz = u32::from(self.pixel_clock) * 10;
        pixel_clock_khz * 1000 / (h_total * v_total)
    }
}

/// EDID 数据最大长度
const EDID_MAX_LENGTH: usize = 256;
/// EDID 块 0 长度
const EDID_BLOCK_SIZE: usize = 128;
/// EDID 头 (固定 8 字节 magic)
const EDID_HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// 完整 EDID 数据结构
#[derive(Debug, Clone)]
pub struct Edid {
    /// 原始 EDID 字节 (含扩展块).
    pub raw: [u8; EDID_MAX_LENGTH],
    /// 厂商 ID (3 字符编码 + 终止符).
    pub manufacturer: [u8; 4],
    /// 产品代码.
    pub product_code: u16,
    /// 序列号.
    pub serial_number: u32,
    /// 生产周 (1..=54).
    pub week: u8,
    /// 生产年份.
    pub year: u16,
    /// EDID 主版本号.
    pub version: u8,
    /// EDID 次版本号.
    pub revision: u8,
    /// 基本显示参数.
    pub basic_display: EdidBasicDisplay,
    /// 颜色特性参数.
    pub color_characteristics: EdidColorCharacteristics,
    /// 最多 4 个详细时序描述符 (空槽为 `None`).
    pub detailed_timings: [Option<EdidDetailedTiming>; 4],
}

impl Edid {
    /// 从原始数据解析 EDID
    ///
    /// # Errors
    ///
    /// - 数据前 8 字节与 EDID 标准头不匹配时返回 [`EdidError::InvalidHeader`]
    /// - 校验和不为 0 时返回 [`EdidError::ChecksumMismatch`]
    pub fn parse(data: &[u8; EDID_MAX_LENGTH]) -> Result<Self, EdidError> {
        if data[0..8] != EDID_HEADER {
            return Err(EdidError::InvalidHeader);
        }
        let checksum: u8 = data[0..EDID_BLOCK_SIZE]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        if checksum != 0 {
            return Err(EdidError::ChecksumMismatch);
        }

        let mut manufacturer = [0u8; 4];
        let man_id = (u16::from(data[8]) << 8) | u16::from(data[9]);
        manufacturer[0] = b'@' + ((man_id >> 10) & 0x1F) as u8;
        manufacturer[1] = b'@' + ((man_id >> 5) & 0x1F) as u8;
        manufacturer[2] = b'@' + (man_id & 0x1F) as u8;
        manufacturer[3] = 0;

        let mut detailed_timings: [Option<EdidDetailedTiming>; 4] = [None, None, None, None];
        for (i, slot) in detailed_timings.iter_mut().enumerate() {
            let offset = 54 + i * 18;
            if data[offset] != 0 || data[offset + 1] != 0 {
                *slot = Some(EdidDetailedTiming {
                    pixel_clock: u16::from(data[offset]) | (u16::from(data[offset + 1]) << 8),
                    horizontal_active: data[offset + 2],
                    horizontal_blanking: data[offset + 3],
                    horizontal_active_high: data[offset + 4],
                    horizontal_blanking_high: data[offset + 5],
                    vertical_active: data[offset + 6],
                    vertical_blanking: data[offset + 7],
                    vertical_active_blanking_high: data[offset + 8],
                    horizontal_sync_offset: data[offset + 9],
                    horizontal_sync_pulse_width: data[offset + 10],
                    vertical_sync_offset_width: data[offset + 11],
                    sync_type: data[offset + 12],
                    image_size: data[offset + 13],
                    border: data[offset + 14],
                    features: data[offset + 15],
                });
            }
        }

        Ok(Self {
            raw: *data,
            manufacturer,
            product_code: u16::from(data[10]) | (u16::from(data[11]) << 8),
            serial_number: u32::from(data[12])
                | (u32::from(data[13]) << 8)
                | (u32::from(data[14]) << 16)
                | (u32::from(data[15]) << 24),
            week: data[16],
            year: u16::from(data[17]) + 1990,
            version: data[18],
            revision: data[19],
            basic_display: EdidBasicDisplay {
                video_input_type: data[20],
                horizontal_image_size: data[21],
                vertical_image_size: data[22],
                display_transfer_characteristic: data[23],
                feature_support: data[24],
            },
            color_characteristics: EdidColorCharacteristics {
                red_green_low: data[25],
                blue_white_low: data[26],
                red_x: data[27],
                red_y: data[28],
                green_x: data[29],
                green_y: data[30],
                blue_x: data[31],
                blue_y: data[32],
                white_x: data[33],
                white_y: data[34],
            },
            detailed_timings,
        })
    }

    /// 获取首选分辨率
    #[must_use]
    pub fn preferred_resolution(&self) -> Option<(u16, u16)> {
        if let Some(t) = self.detailed_timings.iter().flatten().next() {
            return Some((t.horizontal_resolution(), t.vertical_resolution()));
        }
        None
    }
}

/// EDID 解析错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdidError {
    /// EDID 头 8 字节与标准魔数不符.
    InvalidHeader,
    /// 块校验和不为 0.
    ChecksumMismatch,
}

/// 填充 mock EDID 数据 (无硬件 / DDC 失败 fallback)
pub fn fill_mock_edid(edid_data: &mut [u8; EDID_MAX_LENGTH]) {
    edid_data[0..8].copy_from_slice(&EDID_HEADER);
    edid_data[8] = 0x04;
    edid_data[9] = 0x5D;
    edid_data[18] = 1;
    edid_data[19] = 3;
    edid_data[20] = 0x80;
    edid_data[21] = 53;
    edid_data[22] = 30;

    let timing_offset = 54;
    edid_data[timing_offset] = 0x69;
    edid_data[timing_offset + 1] = 0x03;
    edid_data[timing_offset + 2] = 0x80;
    edid_data[timing_offset + 3] = 0x98;
    edid_data[timing_offset + 4] = 0x31;
    edid_data[timing_offset + 5] = 0x02;
    edid_data[timing_offset + 6] = 0x38;
    edid_data[timing_offset + 7] = 0x1D;

    let mut checksum = 0u8;
    for byte in edid_data.iter().take(EDID_BLOCK_SIZE - 1) {
        checksum = checksum.wrapping_add(*byte);
    }
    edid_data[EDID_BLOCK_SIZE - 1] =
        crate::functions::intcast::low_u8_from_usize(256 - checksum as usize);
}

// ============================================================================
// 标准视频模式表
// ============================================================================

/// 默认视频模式标志 (全部 false)
const DEFAULT_FLAGS: VideoModeFlags = VideoModeFlags::NONE;

/// 标准视频模式列表 (10 个常见分辨率)
pub const STANDARD_VIDEO_MODES: &[VideoMode] = &[
    VideoMode {
        width: 640,
        height: 480,
        refresh_rate: 60,
        pixel_clock_khz: 25175,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 800,
        height: 600,
        refresh_rate: 60,
        pixel_clock_khz: 40000,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 1024,
        height: 768,
        refresh_rate: 60,
        pixel_clock_khz: 65000,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 1280,
        height: 720,
        refresh_rate: 60,
        pixel_clock_khz: 74250,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 1280,
        height: 1024,
        refresh_rate: 60,
        pixel_clock_khz: 108_000,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 1920,
        height: 1080,
        refresh_rate: 60,
        pixel_clock_khz: 148_500,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 1920,
        height: 1200,
        refresh_rate: 60,
        pixel_clock_khz: 193_250,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 2560,
        height: 1440,
        refresh_rate: 60,
        pixel_clock_khz: 241_500,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 3840,
        height: 2160,
        refresh_rate: 60,
        pixel_clock_khz: 594_000,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 2560,
        height: 1600,
        refresh_rate: 60,
        pixel_clock_khz: 268_500,
        flags: DEFAULT_FLAGS,
    },
];

// ============================================================================
// DMT 精度表
// ============================================================================

/// DMT lookup table — 覆盖 `STANDARD_VIDEO_MODES` 全部 10 个常见模式
const DMT_TIMINGS: &[(u16, u16, u8, VideoTiming)] = &[
    (
        640,
        480,
        60,
        VideoTiming {
            h_active: 640,
            h_total: 800,
            h_sync_offset: 16,
            h_sync_pulse_width: 96,
            v_active: 480,
            v_total: 525,
            v_sync_offset: 10,
            v_sync_pulse_width: 2,
        },
    ),
    (
        800,
        600,
        60,
        VideoTiming {
            h_active: 800,
            h_total: 1056,
            h_sync_offset: 88,
            h_sync_pulse_width: 128,
            v_active: 600,
            v_total: 628,
            v_sync_offset: 23,
            v_sync_pulse_width: 4,
        },
    ),
    (
        1024,
        768,
        60,
        VideoTiming {
            h_active: 1024,
            h_total: 1344,
            h_sync_offset: 24,
            h_sync_pulse_width: 136,
            v_active: 768,
            v_total: 806,
            v_sync_offset: 3,
            v_sync_pulse_width: 6,
        },
    ),
    (
        1280,
        720,
        60,
        VideoTiming {
            h_active: 1280,
            h_total: 1650,
            h_sync_offset: 110,
            h_sync_pulse_width: 40,
            v_active: 720,
            v_total: 750,
            v_sync_offset: 5,
            v_sync_pulse_width: 5,
        },
    ),
    (
        1280,
        1024,
        60,
        VideoTiming {
            h_active: 1280,
            h_total: 1688,
            h_sync_offset: 48,
            h_sync_pulse_width: 112,
            v_active: 1024,
            v_total: 1066,
            v_sync_offset: 1,
            v_sync_pulse_width: 3,
        },
    ),
    (
        1920,
        1080,
        60,
        VideoTiming {
            h_active: 1920,
            h_total: 2200,
            h_sync_offset: 88,
            h_sync_pulse_width: 44,
            v_active: 1080,
            v_total: 1125,
            v_sync_offset: 4,
            v_sync_pulse_width: 5,
        },
    ),
    (
        1920,
        1200,
        60,
        VideoTiming {
            h_active: 1920,
            h_total: 2592,
            h_sync_offset: 136,
            h_sync_pulse_width: 32,
            v_active: 1200,
            v_total: 1245,
            v_sync_offset: 3,
            v_sync_pulse_width: 6,
        },
    ),
    (
        2560,
        1440,
        60,
        VideoTiming {
            h_active: 2560,
            h_total: 2720,
            h_sync_offset: 48,
            h_sync_pulse_width: 32,
            v_active: 1440,
            v_total: 1481,
            v_sync_offset: 3,
            v_sync_pulse_width: 5,
        },
    ),
    (
        2560,
        1600,
        60,
        VideoTiming {
            h_active: 2560,
            h_total: 2720,
            h_sync_offset: 48,
            h_sync_pulse_width: 32,
            v_active: 1600,
            v_total: 1646,
            v_sync_offset: 3,
            v_sync_pulse_width: 6,
        },
    ),
    (
        3840,
        2160,
        60,
        VideoTiming {
            h_active: 3840,
            h_total: 4400,
            h_sync_offset: 88,
            h_sync_pulse_width: 44,
            v_active: 2160,
            v_total: 2250,
            v_sync_offset: 4,
            v_sync_pulse_width: 5,
        },
    ),
];

/// 在 DMT lookup table 中查找精确时序参数
#[must_use]
pub fn lookup_dmt_timing(mode: VideoMode) -> Option<VideoTiming> {
    for &(w, h, rate, timing) in DMT_TIMINGS {
        if w == mode.width && h == mode.height && rate == mode.refresh_rate {
            return Some(timing);
        }
    }
    None
}

/// 从 `VideoMode` 派生时序参数 (DMT lookup 优先, 公式 fallback)
#[must_use]
pub fn derive_video_timing(mode: VideoMode) -> VideoTiming {
    if let Some(timing) = lookup_dmt_timing(mode) {
        return timing;
    }

    let v_active = mode.height;
    let h_active = mode.width;

    let v_total = if mode.refresh_rate > 0 && mode.pixel_clock_khz > 0 {
        let v_blank = (u32::from(v_active) * 5 / 100).max(1);
        crate::functions::intcast::low_u16_from_u32(u32::from(v_active) + v_blank)
    } else {
        v_active + 50
    };

    let h_total = if mode.refresh_rate > 0 && mode.pixel_clock_khz > 0 {
        let h_total_u32 =
            (mode.pixel_clock_khz * 1000) / (u32::from(v_total) * u32::from(mode.refresh_rate));
        crate::functions::intcast::low_u16_from_u32(h_total_u32.max(u32::from(h_active) + 1))
    } else {
        h_active + 200
    };

    let h_blank = h_total.saturating_sub(h_active);
    let h_sync_offset = h_blank / 4;
    let h_sync_pulse_width = (h_blank / 8).max(1);

    VideoTiming {
        h_active,
        h_total,
        h_sync_offset,
        h_sync_pulse_width,
        v_active,
        v_total,
        v_sync_offset: 1,
        v_sync_pulse_width: 3,
    }
}

// ============================================================================
// 像素时钟算法
// ============================================================================

/// 从目标像素时钟 (kHz) 计算 mul/div 寄存器值
#[must_use]
pub fn compute_pixel_clock_mul_div(target_khz: u32, base_khz: u32) -> (u8, u8) {
    if target_khz == 0 || base_khz == 0 {
        return (1, 1);
    }
    let mut best = (1u8, 1u8);
    let mut best_err: u32 = u32::MAX;
    for div in 1u32..=16 {
        let mul = target_khz.saturating_mul(div).saturating_add(base_khz / 2) / base_khz;
        if mul == 0 || mul > 255 {
            continue;
        }
        let actual = base_khz.saturating_mul(mul) / div;
        let err = actual.abs_diff(target_khz);
        if err < best_err {
            best_err = err;
            best = (
                crate::functions::intcast::low_u8_from_u32(mul),
                crate::functions::intcast::low_u8_from_u32(div),
            );
            if err == 0 {
                break;
            }
        }
    }
    best
}

// ============================================================================
// HDMI 控制器 (safe, 使用 IoMem)
// ============================================================================

/// HDMI 控制器 — functions 层安全实现
///
/// 所有寄存器读写通过 `IoMem` 安全接口, 无 unsafe.
pub struct HdmiController {
    /// MMIO 区域 (Some = 真实硬件, None = fallback 模式)
    iomem: Option<IoMem>,
    /// HPD 状态寄存器偏移
    hpd_reg_offset: usize,
    /// 像素时钟 mul 寄存器偏移
    pclk_mul_reg_offset: usize,
    /// 像素时钟 div 寄存器偏移
    pclk_div_reg_offset: usize,
    /// 已读取的 EDID
    edid: Option<Edid>,
    /// 当前视频模式
    current_mode: Option<VideoMode>,
    /// 是否已连接 (HPD)
    connected: bool,
    /// 是否已初始化
    initialized: bool,
}

impl HdmiController {
    /// 创建 HDMI 控制器实例 (fallback 模式, 无硬件)
    #[must_use]
    pub fn new() -> Self {
        Self {
            iomem: None,
            hpd_reg_offset: HPD_STATUS_REG_OFFSET,
            pclk_mul_reg_offset: PCLK_MUL_REG_OFFSET,
            pclk_div_reg_offset: PCLK_DIV_REG_OFFSET,
            edid: None,
            current_mode: None,
            connected: false,
            initialized: false,
        }
    }

    /// 创建 HDMI 控制器实例 (真实硬件模式)
    ///
    /// 调用方必须保证:
    /// - `iomem` 已映射到有效 HDMI 控制器 MMIO 区域
    /// - `iomem.len() >= REQUIRED_IOMEM_SIZE` (0x07A)
    #[must_use]
    pub fn new_with_iomem(iomem: IoMem, hpd_reg_offset: usize) -> Self {
        debug_assert!(
            iomem.len() >= REQUIRED_IOMEM_SIZE,
            "HdmiController 需要 IoMem >= {} 字节, got {}",
            REQUIRED_IOMEM_SIZE,
            iomem.len()
        );
        Self {
            iomem: Some(iomem),
            hpd_reg_offset,
            pclk_mul_reg_offset: PCLK_MUL_REG_OFFSET,
            pclk_div_reg_offset: PCLK_DIV_REG_OFFSET,
            edid: None,
            current_mode: None,
            connected: false,
            initialized: false,
        }
    }

    /// 创建 HDMI 控制器实例 (真实硬件, 使用默认 HPD 偏移)
    #[must_use]
    pub fn new_with_default_hpd(iomem: IoMem) -> Self {
        Self::new_with_iomem(iomem, HPD_STATUS_REG_OFFSET)
    }

    /// 检测热插拔
    pub fn detect_hot_plug(&mut self) -> bool {
        let hpd = if let Some(iomem) = &self.iomem {
            iomem.read_u8(self.hpd_reg_offset) & HPD_STATUS_BIT != 0
        } else {
            // fallback: 假设已连接
            true
        };
        self.connected = hpd;
        hpd
    }

    /// 读取 EDID (通过 DDC/I2C 或 mock fallback)
    ///
    /// # Errors
    ///
    /// - 设备未连接 (HPD 为低) 时返回 [`HdmiError::NotConnected`]
    /// - EDID 解析失败 (如头无效或校验和不匹配) 时返回 [`HdmiError::EdidParse`]
    ///
    /// # Panics
    ///
    /// 正常情况下不会 panic; 仅当 EDID 刚解析成功并写入 `edid` 字段后又被清空时,
    /// 末尾的 `unwrap()` 才会 panic (逻辑上不可达)。
    pub fn read_edid(&mut self) -> Result<&Edid, HdmiError> {
        if !self.connected {
            return Err(HdmiError::NotConnected);
        }

        let mut edid_data = [0u8; EDID_MAX_LENGTH];

        if let Some(iomem) = &self.iomem {
            match super::ddc::read_edid_block(
                iomem,
                super::ddc::DDC_DEFAULT_CTRL_REG,
                super::ddc::DDC_DEFAULT_STATUS_REG,
                0,
            ) {
                Ok(block) => {
                    edid_data[..128].copy_from_slice(&block);
                }
                Err(_) => {
                    fill_mock_edid(&mut edid_data);
                }
            }
        } else {
            fill_mock_edid(&mut edid_data);
        }

        let parsed = Edid::parse(&edid_data)?;
        self.edid = Some(parsed);
        Ok(self.edid.as_ref().unwrap())
    }

    /// 设置视频模式 (像素时钟 → PLL 锁定 → 时序 → 同步 → TMDS)
    ///
    /// # Errors
    ///
    /// - 设备未连接 (HPD 为低) 时返回 [`HdmiError::NotConnected`]
    /// - PLL 在超时时间内未锁定 (真实硬件路径) 时返回 [`HdmiError::PllLockTimeout`]
    pub fn set_video_mode(&mut self, mode: VideoMode) -> Result<(), HdmiError> {
        if !self.connected {
            return Err(HdmiError::NotConnected);
        }

        // 第 1 步: 配置像素时钟
        if let Some(iomem) = &self.iomem {
            let (mul, div) = compute_pixel_clock_mul_div(mode.pixel_clock_khz, PCLK_BASE_KHZ);
            iomem.write_u8(self.pclk_mul_reg_offset, mul);
            iomem.write_u8(self.pclk_div_reg_offset, div);
        }

        // 第 1.5 步: 等待 PLL 锁定
        if let Some(iomem) = &self.iomem {
            let mut elapsed: usize = 0;
            let mut locked = false;
            while elapsed < PLL_LOCK_TIMEOUT_ITERS {
                let status = iomem.read_u8(PCLK_LOCK_REG_OFFSET);
                if status & PCLK_LOCK_BIT != 0 {
                    locked = true;
                    break;
                }
                for _ in 0..50 {
                    core::hint::spin_loop();
                }
                elapsed += 50;
            }
            if !locked {
                return Err(HdmiError::PllLockTimeout);
            }
        }

        // 第 2 步: 配置时序参数
        let timing = derive_video_timing(mode);
        if let Some(iomem) = &self.iomem {
            write_timing_register(iomem, H_TOTAL_REG_OFFSET, timing.h_total);
            write_timing_register(iomem, H_ACTIVE_REG_OFFSET, timing.h_active);
            write_timing_register(iomem, V_TOTAL_REG_OFFSET, timing.v_total);
            write_timing_register(iomem, V_ACTIVE_REG_OFFSET, timing.v_active);
            write_timing_register(iomem, H_SYNC_OFFSET_REG, timing.h_sync_offset);
            write_timing_register(iomem, H_SYNC_PW_REG, timing.h_sync_pulse_width);
            write_timing_register(iomem, V_SYNC_OFFSET_REG, timing.v_sync_offset);
            write_timing_register(iomem, V_SYNC_PW_REG, timing.v_sync_pulse_width);
        }

        // 第 3 步: 同步极性 + TMDS 输出使能
        if let Some(iomem) = &self.iomem {
            let mut sync_val = 0u8;
            if mode.flags.hsync_positive() {
                sync_val |= SYNC_POL_H_BIT;
            }
            if mode.flags.vsync_positive() {
                sync_val |= SYNC_POL_V_BIT;
            }
            iomem.write_u8(SYNC_POL_REG_OFFSET, sync_val);
            iomem.write_u8(TMDS_ENABLE_REG_OFFSET, TMDS_ENABLE_BIT);
        }

        self.current_mode = Some(mode);
        Ok(())
    }

    /// 获取支持的视频模式列表
    #[must_use]
    pub fn get_supported_modes() -> &'static [VideoMode] {
        STANDARD_VIDEO_MODES
    }

    /// 初始化 HDMI 控制器 (检测 → 读 EDID → 设置首选模式)
    ///
    /// # Errors
    ///
    /// - 热插拔检测失败 (HPD 为低) 时返回 [`HdmiError::NotConnected`]
    /// - 设置首选模式时 PLL 锁定超时等失败时返回相应 [`HdmiError`]
    pub fn init(&mut self) -> Result<(), HdmiError> {
        if !self.detect_hot_plug() {
            return Err(HdmiError::NotConnected);
        }
        if self.connected {
            let _ = self.read_edid();
            if let Some(ref edid) = self.edid
                && let Some((width, height)) = edid.preferred_resolution()
            {
                for mode in STANDARD_VIDEO_MODES {
                    if mode.width == width && mode.height == height {
                        self.set_video_mode(*mode)?;
                        break;
                    }
                }
            }
        }
        self.initialized = true;
        Ok(())
    }

    /// 关闭 HDMI 控制器 (禁用 TMDS 输出)
    pub fn shutdown(&mut self) {
        if let Some(iomem) = &self.iomem {
            iomem.write_u8(TMDS_ENABLE_REG_OFFSET, 0x00);
        }
        self.initialized = false;
    }

    /// 是否已初始化
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

impl Default for HdmiController {
    fn default() -> Self {
        Self::new()
    }
}

/// MIG-008 接线补齐: HDMI 控制器接入 `Driver` 抽象, 供 EGDF 注册 (DECISION-K)
///
/// 委托 inherent 方法 (显式 `Type::method(self)` 路径, 规避同名解析歧义);
/// 错误统一映射为 [`DriverError::HardwareError`].
impl Driver for HdmiController {
    fn name(&self) -> &'static str {
        "HDMI Controller"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Other
    }

    fn init(&mut self) -> DriverResult<()> {
        HdmiController::init(self).map_err(|_| DriverError::HardwareError)
    }

    fn shutdown(&mut self) -> DriverResult<()> {
        HdmiController::shutdown(self);
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.is_initialized()
    }

    fn status(&self) -> &'static str {
        if self.is_initialized() {
            "HDMI Controller ready"
        } else {
            "HDMI Controller not initialized"
        }
    }
}

/// 写入 16-bit 时序寄存器 (低字节 + 高字节)
fn write_timing_register(iomem: &IoMem, reg_offset: usize, value: u16) {
    iomem.write_u8(reg_offset, (value & 0xFF) as u8);
    iomem.write_u8(reg_offset + 1, ((value >> 8) & 0xFF) as u8);
}

/// HDMI 控制器错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdmiError {
    /// 设备未连接 (HPD 为低)
    NotConnected,
    /// PLL 锁定超时
    PllLockTimeout,
    /// EDID 解析失败
    EdidParse(EdidError),
}

impl From<EdidError> for HdmiError {
    fn from(e: EdidError) -> Self {
        Self::EdidParse(e)
    }
}
