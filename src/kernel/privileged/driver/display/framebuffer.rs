//! Framebuffer 驱动 (Framebuffer Driver)
//!
//! 提供帧缓冲机制层访问：
//! - **多种像素格式**: RGB565、RGB888、ARGB8888等
//! - **像素读写机制**: 像素格式转换 + MMIO 写入
//! - **滚动机制原语**: 帧缓冲内存搬移
//!
//! 绘图算法 (填充/直线/矩形/圆/alpha 混合/反走样) 已按
//! `docs/plan/framekernel-paradigm-enforcement.md` §6.3 下沉
//! `functions::driver::display::framebuffer_ops`, 经
//! [`super::framebuffer_ops::FramebufferOps`] trait 注入调用.

use super::super::infra::{DeviceInfo, DeviceType, Driver, Result};
use super::framebuffer_ops::PixelCanvas;
use crate::privileged::iomem::IoMem;

// ============================================================================
// 像素格式定义
// ============================================================================

/// 像素格式枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// RGB565: 16位色 (5-6-5)
    Rgb565,
    /// RGB888: 24位色 (8-8-8)
    Rgb888,
    /// ARGB8888: 32位色 (8-8-8-8)
    Argb8888,
    /// BGR888: 24位色 (B-G-R)
    Bgr888,
    /// BGRA8888: 32位色 (B-G-R-A)
    Bgra8888,
}

impl PixelFormat {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取每像素字节数
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 => 3,
            Self::Argb8888 => 4,
            Self::Bgr888 => 3,
            Self::Bgra8888 => 4,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取每像素位数
    pub fn bits_per_pixel(&self) -> usize {
        self.bytes_per_pixel() * 8
    }
}

// ============================================================================
// 颜色定义
// ============================================================================

/// RGBA颜色 (32位)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C, packed)]
pub struct Color {
    /// 红色分量
    pub r: u8,
    /// 绿色分量
    pub g: u8,
    /// 蓝色分量
    pub b: u8,
    /// 透明度分量
    pub a: u8,
}

impl Color {
    /// 创建新颜色
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// 创建带透明度的颜色
    pub const fn new_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 转换为RGB565格式
    pub fn to_rgb565(&self) -> u16 {
        let r = (u16::from(self.r) >> 3) & 0x1F;
        let g = (u16::from(self.g) >> 2) & 0x3F;
        let b = (u16::from(self.b) >> 3) & 0x1F;
        (r << 11) | (g << 5) | b
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 转换为RGB888格式 (返回u32方便使用)
    pub fn to_rgb888(&self) -> u32 {
        (u32::from(self.r) << 16) | (u32::from(self.g) << 8) | u32::from(self.b)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 转换为ARGB8888格式
    pub fn to_argb8888(&self) -> u32 {
        (u32::from(self.a) << 24)
            | (u32::from(self.r) << 16)
            | (u32::from(self.g) << 8)
            | u32::from(self.b)
    }

    /// 从RGB565创建颜色
    pub fn from_rgb565(rgb565: u16) -> Self {
        let r = ((rgb565 >> 11) & 0x1F) as u8;
        let g = ((rgb565 >> 5) & 0x3F) as u8;
        let b = (rgb565 & 0x1F) as u8;

        Self {
            r: (r << 3) | (r >> 2),
            g: (g << 2) | (g >> 4),
            b: (b << 3) | (b >> 2),
            a: 255,
        }
    }

    /// 从ARGB8888创建颜色
    pub fn from_argb8888(argb: u32) -> Self {
        Self {
            a: ((argb >> 24) & 0xFF) as u8,
            r: ((argb >> 16) & 0xFF) as u8,
            g: ((argb >> 8) & 0xFF) as u8,
            b: (argb & 0xFF) as u8,
        }
    }

    /// 混合两个颜色 (alpha混合)
    // 有意窄化: 颜色分量/透明度经规范化计算, 值域 [0,255]
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub fn blend(&self, other: &Self) -> Self {
        let alpha = u32::from(self.a);
        let inv_alpha = 255 - alpha;

        Self {
            r: ((u32::from(self.r) * alpha + u32::from(other.r) * inv_alpha) / 255) as u8,
            g: ((u32::from(self.g) * alpha + u32::from(other.g) * inv_alpha) / 255) as u8,
            b: ((u32::from(self.b) * alpha + u32::from(other.b) * inv_alpha) / 255) as u8,
            a: 255,
        }
    }
}

/// 预定义颜色
pub mod colors {
    use super::Color;

    /// 黑色
    pub const BLACK: Color = Color::new(0, 0, 0);
    /// 白色
    pub const WHITE: Color = Color::new(255, 255, 255);
    /// 红色
    pub const RED: Color = Color::new(255, 0, 0);
    /// 绿色
    pub const GREEN: Color = Color::new(0, 255, 0);
    /// 蓝色
    pub const BLUE: Color = Color::new(0, 0, 255);
    /// 黄色
    pub const YELLOW: Color = Color::new(255, 255, 0);
    /// 青色
    pub const CYAN: Color = Color::new(0, 255, 255);
    /// 品红
    pub const MAGENTA: Color = Color::new(255, 0, 255);
    /// 灰色
    pub const GRAY: Color = Color::new(128, 128, 128);
    /// 浅灰色
    pub const LIGHT_GRAY: Color = Color::new(192, 192, 192);
    /// 深灰色
    pub const DARK_GRAY: Color = Color::new(64, 64, 64);
}

// ============================================================================
// 图形原语
// ============================================================================

/// 2D点
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// 水平坐标
    pub x: i32,
    /// 垂直坐标
    pub y: i32,
}

impl Point {
    /// 构造点
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// 2D矩形
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// 左上角水平坐标
    pub x: i32,
    /// 左上角垂直坐标
    pub y: i32,
    /// 宽度
    pub width: u32,
    /// 高度
    pub height: u32,
}

impl Rect {
    /// 构造矩形
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 检查点是否在矩形内
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width as i32
            && point.y >= self.y
            && point.y < self.y + self.height as i32
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 检查是否与另一个矩形相交
    pub fn intersects(&self, other: &Self) -> bool {
        self.x < other.x + other.width as i32
            && self.x + self.width as i32 > other.x
            && self.y < other.y + other.height as i32
            && self.y + self.height as i32 > other.y
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取两个矩形的交集
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        if !self.intersects(other) {
            return None;
        }

        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let x2 = (self.x + self.width as i32).min(other.x + other.width as i32);
        let y2 = (self.y + self.height as i32).min(other.y + other.height as i32);

        Some(Self::new(x, y, (x2 - x) as u32, (y2 - y) as u32))
    }
}

// ============================================================================
// Framebuffer 驱动
// ============================================================================

/// Framebuffer 驱动
pub struct Framebuffer {
    /// 帧缓冲 MMIO 句柄
    iomem: IoMem,
    /// 宽度 (像素)
    width: u32,
    /// 高度 (像素)
    height: u32,
    /// 每行字节数 (可能包含padding)
    pitch: u32,
    /// 像素格式
    format: PixelFormat,
    /// 每像素字节数
    bpp: usize,
    /// 设备信息
    info: DeviceInfo,
    /// 是否已初始化
    initialized: bool,
}

impl Framebuffer {
    /// 创建新的Framebuffer实例
    pub fn new(iomem: IoMem, width: u32, height: u32, pitch: u32, format: PixelFormat) -> Self {
        Self {
            iomem,
            width,
            height,
            pitch,
            format,
            bpp: format.bytes_per_pixel(),
            info: DeviceInfo::new("framebuffer", DeviceType::Other),
            initialized: false,
        }
    }

    /// 获取宽度
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 获取高度
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 获取像素格式
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// 获取设备信息
    pub fn get_info(&self) -> &DeviceInfo {
        &self.info
    }

    /// 获取每行字节数
    #[inline]
    pub fn pitch(&self) -> u32 {
        self.pitch
    }

    /// 获取每像素字节数
    #[inline]
    pub fn bytes_per_pixel(&self) -> usize {
        self.bpp
    }

    /// 计算像素偏移量
    #[inline]
    fn pixel_offset(&self, x: u32, y: u32) -> usize {
        (y as usize * self.pitch as usize) + (x as usize * self.bpp)
    }

    /// 设置像素颜色
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        let offset = self.pixel_offset(x, y);

        match self.format {
            PixelFormat::Rgb565 => {
                let pixel = color.to_rgb565();
                self.iomem.write_u16(offset, pixel);
            }
            PixelFormat::Argb8888 => {
                let pixel = color.to_argb8888();
                self.iomem.write_u32(offset, pixel);
            }
            PixelFormat::Rgb888 => {
                self.iomem.write_u8(offset, color.r);
                self.iomem.write_u8(offset + 1, color.g);
                self.iomem.write_u8(offset + 2, color.b);
            }
            PixelFormat::Bgr888 => {
                self.iomem.write_u8(offset, color.b);
                self.iomem.write_u8(offset + 1, color.g);
                self.iomem.write_u8(offset + 2, color.r);
            }
            PixelFormat::Bgra8888 => {
                let pixel = (u32::from(color.b) << 24)
                    | (u32::from(color.g) << 16)
                    | (u32::from(color.r) << 8)
                    | u32::from(color.a);
                self.iomem.write_u32(offset, pixel);
            }
        }
    }

    /// 获取像素颜色
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }

        let offset = self.pixel_offset(x, y);

        match self.format {
            PixelFormat::Rgb565 => {
                let pixel = self.iomem.read_u16(offset);
                Some(Color::from_rgb565(pixel))
            }
            PixelFormat::Argb8888 => {
                let pixel = self.iomem.read_u32(offset);
                Some(Color::from_argb8888(pixel))
            }
            PixelFormat::Rgb888 => Some(Color::new(
                self.iomem.read_u8(offset),
                self.iomem.read_u8(offset + 1),
                self.iomem.read_u8(offset + 2),
            )),
            PixelFormat::Bgr888 => Some(Color::new(
                self.iomem.read_u8(offset + 2),
                self.iomem.read_u8(offset + 1),
                self.iomem.read_u8(offset),
            )),
            PixelFormat::Bgra8888 => {
                let pixel = self.iomem.read_u32(offset);
                Some(Color::new(
                    ((pixel >> 8) & 0xFF) as u8,
                    ((pixel >> 16) & 0xFF) as u8,
                    ((pixel >> 24) & 0xFF) as u8,
                ))
            }
        }
    }

    /// 向上滚动像素行区间 (机制原语: 帧缓冲内存搬移)
    ///
    /// 将像素行区间 `[region_top, region_bottom)` 整体上移 `lines` 行, 并把区间
    /// 底部腾出的 `lines` 行清零. 用于图形控制台的文本滚动.
    /// 参数越界时按有效高度裁剪; 区间为空或 `lines` 超出区间高度时不执行任何操作.
    pub fn scroll_rows_up(&mut self, region_top: u32, region_bottom: u32, lines: u32) {
        let top = region_top.min(self.height);
        let bottom = region_bottom.min(self.height);
        if lines == 0 || top >= bottom || lines > bottom - top {
            return;
        }

        let pitch = self.pitch as usize;
        let shift_bytes = lines as usize * pitch;
        let copy_bytes = (bottom - top - lines) as usize * pitch;
        let top_off = top as usize * pitch;
        let clear_off = (bottom - lines) as usize * pitch;

        // SAFETY: `iomem` 覆盖整个帧缓冲 `[0, pitch*height)`; `top`/`bottom` 已按
        // height 裁剪, `lines` 已收敛到区间高度以内, 故源区间
        // `[top+lines, bottom)` 与目标区间 `[top, bottom-lines)` 均落在缓冲内;
        // 两者可能重叠, `core::ptr::copy` 按搬移方向自行处理重叠.
        unsafe {
            let base = self.iomem.virt_ptr();
            core::ptr::copy(
                base.add(top_off + shift_bytes),
                base.add(top_off),
                copy_bytes,
            );
            core::ptr::write_bytes(base.add(clear_off), 0u8, shift_bytes);
        }
    }
}

// SAFETY: 像素读写与滚动均经 `Framebuffer` 的安全方法/机制原语,
// `PixelCanvas` 不引入新的别名或裸指针暴露.
impl PixelCanvas for Framebuffer {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        Framebuffer::set_pixel(self, x, y, color);
    }

    fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        Framebuffer::get_pixel(self, x, y)
    }
}

// ============================================================================
// Driver Trait 实现
// ============================================================================

impl Driver for Framebuffer {
    fn name(&self) -> &'static str {
        "Framebuffer"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Other
    }

    fn init(&mut self) -> Result<()> {
        self.initialized = true;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        self.initialized = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.initialized
    }

    fn status(&self) -> &'static str {
        if self.initialized {
            "Framebuffer ready"
        } else {
            "Framebuffer not initialized"
        }
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pixel_format_bytes() {
        assert_eq!(PixelFormat::Rgb565.bytes_per_pixel(), 2);
        assert_eq!(PixelFormat::Rgb888.bytes_per_pixel(), 3);
        assert_eq!(PixelFormat::Argb8888.bytes_per_pixel(), 4);
    }

    #[test]
    fn test_color_rgb565_conversion() {
        let color = Color::new(255, 255, 255);
        let rgb565 = color.to_rgb565();
        let converted = Color::from_rgb565(rgb565);

        // 允许一定的转换误差
        assert!((color.r as i32 - converted.r as i32).abs() <= 8);
        assert!((color.g as i32 - converted.g as i32).abs() <= 4);
        assert!((color.b as i32 - converted.b as i32).abs() <= 8);
    }

    #[test]
    fn test_color_argb_conversion() {
        let color = Color::new(128, 64, 192);
        let argb = color.to_argb8888();
        let converted = Color::from_argb8888(argb);

        assert_eq!(color.r, converted.r);
        assert_eq!(color.g, converted.g);
        assert_eq!(color.b, converted.b);
    }

    #[test]
    fn test_rect_contains() {
        let rect = Rect::new(10, 10, 100, 100);

        assert!(rect.contains(Point::new(50, 50)));
        assert!(rect.contains(Point::new(10, 10)));
        assert!(!rect.contains(Point::new(5, 5)));
        assert!(!rect.contains(Point::new(200, 200)));
    }

    #[test]
    fn test_rect_intersects() {
        let rect1 = Rect::new(0, 0, 100, 100);
        let rect2 = Rect::new(50, 50, 100, 100);
        let rect3 = Rect::new(200, 200, 100, 100);

        assert!(rect1.intersects(&rect2));
        assert!(!rect1.intersects(&rect3));
    }

    #[test]
    fn test_color_blend() {
        let blue = Color::new(0, 0, 255);
        let half_red = Color::new_alpha(255, 0, 0, 128);

        let blended = half_red.blend(&blue);

        // 混合后应该是紫色偏蓝
        assert!(blended.r > 0 && blended.r < 255);
        assert!(blended.b > 0 && blended.b < 255);
    }
}
