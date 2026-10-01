#![deny(unsafe_code)]
//! HDMI (High-Definition Multimedia Interface) 驱动 — services 层安全实现
//!
//! 提供 HDMI 控制器的 safe 业务逻辑:
//! - EDID 解析 (纯数据, 无硬件访问)
//! - 视频模式管理
//! - 时序参数派生 (DMT lookup + 公式 fallback)
//! - 像素时钟计算
//!
//! 硬件寄存器访问通过 IoMem 安全代理, 无 unsafe.

use crate::framework::driver::{DeviceType, Driver, DriverError, DriverResult};
use crate::framework::iomem::IoMem;

// ============================================================================
// DDC 寄存器偏移 (从 services::driver::display::ddc 重导出)
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
    pub width: u16,
    pub height: u16,
    pub refresh_rate: u8,
    pub pixel_clock_khz: u32,
    pub flags: VideoModeFlags,
}

/// 视频模式标志
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoModeFlags {
    pub interlaced: bool,
    pub double_scan: bool,
    pub hsync_positive: bool,
    pub vsync_positive: bool,
}

impl Default for VideoModeFlags {
    fn default() -> Self {
        Self {
            interlaced: false,
            double_scan: false,
            hsync_positive: false,
            vsync_positive: false,
        }
    }
}

/// HDMI 时序参数 (从 `VideoMode` 派生)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTiming {
    pub h_active: u16,
    pub h_total: u16,
    pub h_sync_offset: u16,
    pub h_sync_pulse_width: u16,
    pub v_active: u16,
    pub v_total: u16,
    pub v_sync_offset: u16,
    pub v_sync_pulse_width: u16,
}

/// EDID 基本显示参数
#[derive(Debug, Clone, Copy)]
pub struct EdidBasicDisplay {
    pub video_input_type: u8,
    pub horizontal_image_size: u8,
    pub vertical_image_size: u8,
    pub display_transfer_characteristic: u8,
    pub feature_support: u8,
}

/// EDID 颜色特性
#[derive(Debug, Clone, Copy)]
pub struct EdidColorCharacteristics {
    pub red_green_low: u8,
    pub blue_white_low: u8,
    pub red_x: u8,
    pub red_y: u8,
    pub green_x: u8,
    pub green_y: u8,
    pub blue_x: u8,
    pub blue_y: u8,
    pub white_x: u8,
    pub white_y: u8,
}

/// EDID 详细时序描述符
#[derive(Debug, Clone, Copy)]
pub struct EdidDetailedTiming {
    pub pixel_clock: u16,
    pub horizontal_active: u8,
    pub horizontal_blanking: u8,
    pub horizontal_active_high: u8,
    pub horizontal_blanking_high: u8,
    pub vertical_active: u8,
    pub vertical_blanking: u8,
    pub vertical_active_blanking_high: u8,
    pub horizontal_sync_offset: u8,
    pub horizontal_sync_pulse_width: u8,
    pub vertical_sync_offset_width: u8,
    pub sync_type: u8,
    pub image_size: u8,
    pub border: u8,
    pub features: u8,
}

impl EdidDetailedTiming {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取水平分辨率
    pub fn horizontal_resolution(&self) -> u16 {
        u16::from(self.horizontal_active) | ((u16::from(self.horizontal_active_high) & 0xF0) << 4)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取垂直分辨率
    pub fn vertical_resolution(&self) -> u16 {
        u16::from(self.vertical_active)
            | ((u16::from(self.vertical_active_blanking_high) & 0xF0) << 4)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取刷新率 (近似)
    pub fn refresh_rate(&self) -> u32 {
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
    pub raw: [u8; EDID_MAX_LENGTH],
    pub manufacturer: [u8; 4],
    pub product_code: u16,
    pub serial_number: u32,
    pub week: u8,
    pub year: u16,
    pub version: u8,
    pub revision: u8,
    pub basic_display: EdidBasicDisplay,
    pub color_characteristics: EdidColorCharacteristics,
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
        for i in 0..4 {
            let offset = 54 + i * 18;
            if data[offset] != 0 || data[offset + 1] != 0 {
                detailed_timings[i] = Some(EdidDetailedTiming {
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
    pub fn preferred_resolution(&self) -> Option<(u16, u16)> {
        for timing in &self.detailed_timings {
            if let Some(t) = timing {
                return Some((t.horizontal_resolution(), t.vertical_resolution()));
            }
        }
        None
    }
}

/// EDID 解析错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdidError {
    InvalidHeader,
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
    for i in 0..(EDID_BLOCK_SIZE - 1) {
        checksum = checksum.wrapping_add(edid_data[i]);
    }
    edid_data[EDID_BLOCK_SIZE - 1] = (256 - checksum as usize) as u8;
}

// ============================================================================
// 标准视频模式表
// ============================================================================

/// 默认视频模式标志 (全部 false)
const DEFAULT_FLAGS: VideoModeFlags = VideoModeFlags {
    interlaced: false,
    double_scan: false,
    hsync_positive: false,
    vsync_positive: false,
};

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
        pixel_clock_khz: 108000,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 1920,
        height: 1080,
        refresh_rate: 60,
        pixel_clock_khz: 148500,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 1920,
        height: 1200,
        refresh_rate: 60,
        pixel_clock_khz: 193250,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 2560,
        height: 1440,
        refresh_rate: 60,
        pixel_clock_khz: 241500,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 3840,
        height: 2160,
        refresh_rate: 60,
        pixel_clock_khz: 594000,
        flags: DEFAULT_FLAGS,
    },
    VideoMode {
        width: 2560,
        height: 1600,
        refresh_rate: 60,
        pixel_clock_khz: 268500,
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

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
/// 在 DMT lookup table 中查找精确时序参数
pub fn lookup_dmt_timing(mode: &VideoMode) -> Option<VideoTiming> {
    for &(w, h, rate, timing) in DMT_TIMINGS {
        if w == mode.width && h == mode.height && rate == mode.refresh_rate {
            return Some(timing);
        }
    }
    None
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// 从 `VideoMode` 派生时序参数 (DMT lookup 优先, 公式 fallback)
pub fn derive_video_timing(mode: &VideoMode) -> VideoTiming {
    if let Some(timing) = lookup_dmt_timing(mode) {
        return timing;
    }

    let v_active = mode.height;
    let h_active = mode.width;

    let v_total = if mode.refresh_rate > 0 && mode.pixel_clock_khz > 0 {
        let v_blank = (u32::from(v_active) * 5 / 100).max(1);
        (u32::from(v_active) + v_blank) as u16
    } else {
        v_active + 50
    };

    let h_total = if mode.refresh_rate > 0 && mode.pixel_clock_khz > 0 {
        let h_total_u32 =
            (mode.pixel_clock_khz * 1000) / (u32::from(v_total) * u32::from(mode.refresh_rate));
        h_total_u32.max(u32::from(h_active) + 1) as u16
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
            best = (mul as u8, div as u8);
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

/// HDMI 控制器 — services 层安全实现
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
        let timing = derive_video_timing(&mode);
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
            if mode.flags.hsync_positive {
                sync_val |= SYNC_POL_H_BIT;
            }
            if mode.flags.vsync_positive {
                sync_val |= SYNC_POL_V_BIT;
            }
            iomem.write_u8(SYNC_POL_REG_OFFSET, sync_val);
            iomem.write_u8(TMDS_ENABLE_REG_OFFSET, TMDS_ENABLE_BIT);
        }

        self.current_mode = Some(mode);
        Ok(())
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 获取支持的视频模式列表
    pub fn get_supported_modes(&self) -> &[VideoMode] {
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
            if let Some(ref edid) = self.edid {
                if let Some((width, height)) = edid.preferred_resolution() {
                    for mode in STANDARD_VIDEO_MODES {
                        if mode.width == width && mode.height == height {
                            self.set_video_mode(*mode)?;
                            break;
                        }
                    }
                }
            }
        }
        self.initialized = true;
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 关闭 HDMI 控制器 (禁用 TMDS 输出)
    ///
    /// # Errors
    ///
    /// 此函数始终返回 `Ok(())`, 不会返回 `Err`。
    pub fn shutdown(&mut self) -> Result<(), HdmiError> {
        if let Some(iomem) = &self.iomem {
            iomem.write_u8(TMDS_ENABLE_REG_OFFSET, 0x00);
        }
        self.initialized = false;
        Ok(())
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

/// MIG-008 接线补齐: HDMI 控制器接入 `Driver` 抽象, 供 Chitin 注册 (DECISION-K)
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
        HdmiController::shutdown(self).map_err(|_| DriverError::HardwareError)
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
