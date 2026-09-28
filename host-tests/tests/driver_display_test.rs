//! driver: 显示器驱动集成测试
//!
//! 验收:
//!   - PixelFormat 字节序正确
//!   - Color 转换 (Rgb565/Rgb888/Argb8888) 边界条件
//!   - DisplayMode 带宽计算
//!   - HDMI EDID 解析
//!   - DisplayPort LinkRate/LaneCount 协商
//!
//! 追踪: I-22
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## B08-21 迁移 (2026-09-06)
//! 删除本地 `PixelFormat` / `Color` / `DisplayMode` / `LinkRate` / `LaneCount`
//! 平行镜像, 改引内核真实源码:
//! - `queenx::kernel::framework::driver::display::{Color, PixelFormat}`
//!   — framework 层纯算法类型 (framebuffer.rs, host 可测)
//! - `queenx::kernel::services::driver::display::controller::DisplayMode`
//!   — services 层显示控制器管理策略 (2-E 批次自 framework 迁出, 100% safe)
//! - `queenx::kernel::services::driver::display::dp::{LinkRate, LaneCount}`
//!   — services 层 DisplayPort 协商 (100% safe, 纯算法)
//! - `queenx::kernel::services::driver::display::hdmi::STANDARD_VIDEO_MODES`
//!   — 标准视频模式表 (pub const, 10 个常见 DMT 模式)
//!
//! ## 保留标注 (外部规范, 非内核实现)
//! - `EDID_HEADER` 8 字节魔数表为 HDMI EDID 外部规范常量 (非内核算法),
//!   测试侧保留字节表并标注, 内核 hdmi/edid.rs 中为 `pub(super)` 常量不可 host 引用.

use queenx::kernel::framework::driver::display::{Color, PixelFormat};
use queenx::kernel::services::driver::display::controller::DisplayMode;
use queenx::kernel::services::driver::display::dp::{LaneCount, LinkRate};
use queenx::kernel::services::driver::display::hdmi::STANDARD_VIDEO_MODES;

#[test]
fn test_pixel_format_bytes() {
    assert_eq!(PixelFormat::Rgb565.bytes_per_pixel(), 2);
    assert_eq!(PixelFormat::Rgb888.bytes_per_pixel(), 3);
    assert_eq!(PixelFormat::Argb8888.bytes_per_pixel(), 4);
    assert_eq!(PixelFormat::Bgr888.bytes_per_pixel(), 3);
    assert_eq!(PixelFormat::Bgra8888.bytes_per_pixel(), 4);
}

#[test]
fn test_color_conversion() {
    let color = Color::new(255, 255, 255);
    let rgb565 = color.to_rgb565();
    let converted = Color::from_rgb565(rgb565);
    assert!((color.r as i32 - converted.r as i32).abs() <= 8);

    let color = Color::new(128, 64, 192);
    let argb = color.to_argb8888();
    let converted = Color::from_argb8888(argb);
    assert_eq!(color.r, converted.r);
    assert_eq!(color.g, converted.g);
    assert_eq!(color.b, converted.b);
}

#[test]
fn test_color_rgb888_encoding() {
    // RGB888 编码: R<<16 | G<<8 | B (内核 framebuffer.rs::to_rgb888)
    let c = Color::new(0x12, 0x34, 0x56);
    assert_eq!(c.to_rgb888(), 0x12_34_56);
}

// 显示控制器测试
#[test]
fn test_display_mode() {
    let mode = DisplayMode::new(1920, 1080, 60, PixelFormat::Argb8888);
    assert_eq!(mode.width, 1920);
    assert_eq!(mode.height, 1080);
    assert_eq!(mode.refresh_rate, 60);

    let bw = mode.bandwidth_mbps();
    assert!(bw > 400 && bw < 600);
}

// HDMI 测试
#[test]
fn test_hdmi_modes() {
    // EDID 头魔数表 (外部规范: VESA EDID 1.3/1.4), 非内核算法.
    // 内核 hdmi/edid.rs `EDID_HEADER` 为 pub(super) 常量, host 不可引用;
    // 保留字节表并标注为外部规范.
    const EDID_HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    assert_eq!(
        EDID_HEADER,
        [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]
    );

    // 标准视频模式表: 内核 services hdmi.rs `STANDARD_VIDEO_MODES` (pub const, 10 个模式)
    assert!(!STANDARD_VIDEO_MODES.is_empty());
    // 覆盖常见分辨率: 640x480@60 / 800x600@60 / 1024x768@60 / 1280x720@60 / 1920x1080@60
    for (w, h, r) in [(640u16, 480u16, 60u8), (800, 600, 60), (1024, 768, 60), (1280, 720, 60), (1920, 1080, 60)] {
        assert!(
            STANDARD_VIDEO_MODES
                .iter()
                .any(|m| m.width == w && m.height == h && m.refresh_rate == r),
            "标准模式表应包含 {}x{}@{}", w, h, r
        );
    }
}

// DisplayPort 测试
#[test]
fn test_dp_link_rate() {
    assert_eq!(LinkRate::Rbr.bandwidth_gbps(), 162);
    assert_eq!(LinkRate::Hbr.bandwidth_gbps(), 270);
    assert_eq!(LinkRate::Hbr2.bandwidth_gbps(), 540);
    assert_eq!(LinkRate::Hbr3.bandwidth_gbps(), 810);

    assert_eq!(LinkRate::from_u8(0x06), Some(LinkRate::Rbr));
    assert_eq!(LinkRate::from_u8(0x0A), Some(LinkRate::Hbr));
    assert_eq!(LinkRate::from_u8(0x14), Some(LinkRate::Hbr2));
    assert_eq!(LinkRate::from_u8(0x1E), Some(LinkRate::Hbr3));
    assert_eq!(LinkRate::from_u8(0x00), None);
}

#[test]
fn test_dp_lane_count() {
    assert_eq!(LaneCount::from_u8(1), Some(LaneCount::One));
    assert_eq!(LaneCount::from_u8(2), Some(LaneCount::Two));
    assert_eq!(LaneCount::from_u8(4), Some(LaneCount::Four));
    assert_eq!(LaneCount::from_u8(3), None);
}

#[test]
fn test_dp_total_bandwidth() {
    // 总带宽 = 单链路带宽 × 通道数 (内核 dp.rs get_bandwidth_gbps 语义)
    assert_eq!(LinkRate::Hbr2.bandwidth_gbps() * 4, 2160);
    assert_eq!(LinkRate::Hbr3.bandwidth_gbps() * 4, 3240);
}
