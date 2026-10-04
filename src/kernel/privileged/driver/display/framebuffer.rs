//! Framebuffer 驱动 (Framebuffer Driver)
//!
//! 提供直接帧缓冲访问和图形绘制功能：
//! - **多种像素格式**: RGB565、RGB888、ARGB8888等
//! - **图形绘制**: 点、线、矩形、圆形等
//! - **字体渲染**: 基本文本输出
//! - **双缓冲**: 支持双缓冲避免撕裂
//!
//! ## 架构
//!
//! ```text
//! Framebuffer
//! ├── 像素格式转换
//! ├── 图形绘制原语
//! ├── 字体渲染
//! └── 双缓冲支持
//! ```

use super::super::infra::{DeviceInfo, DeviceType, Driver, Result};
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
    pub r: u8,
    pub g: u8,
    pub b: u8,
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

    pub const BLACK: Color = Color::new(0, 0, 0);
    pub const WHITE: Color = Color::new(255, 255, 255);
    pub const RED: Color = Color::new(255, 0, 0);
    pub const GREEN: Color = Color::new(0, 255, 0);
    pub const BLUE: Color = Color::new(0, 0, 255);
    pub const YELLOW: Color = Color::new(255, 255, 0);
    pub const CYAN: Color = Color::new(0, 255, 255);
    pub const MAGENTA: Color = Color::new(255, 0, 255);
    pub const GRAY: Color = Color::new(128, 128, 128);
    pub const LIGHT_GRAY: Color = Color::new(192, 192, 192);
    pub const DARK_GRAY: Color = Color::new(64, 64, 64);
}

// ============================================================================
// 图形原语
// ============================================================================

/// 2D点
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// 2D矩形
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
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

    /// 获取帧缓冲 `IoMem` 句柄（仅 crate 内部）
    #[inline]
    pub(crate) fn iomem(&self) -> &IoMem {
        &self.iomem
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

    /// 填充整个屏幕
    pub fn fill(&mut self, color: Color) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.set_pixel(x, y, color);
            }
        }
    }

    /// 填充矩形区域
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let x_start = rect.x.max(0) as u32;
        let y_start = rect.y.max(0) as u32;
        let x_end = (rect.x + rect.width as i32).min(self.width as i32) as u32;
        let y_end = (rect.y + rect.height as i32).min(self.height as i32) as u32;

        for y in y_start..y_end {
            for x in x_start..x_end {
                self.set_pixel(x, y, color);
            }
        }
    }

    /// 绘制水平线
    pub fn draw_hline(&mut self, x: i32, y: i32, length: u32, color: Color) {
        if y < 0 || y >= self.height as i32 {
            return;
        }

        let x_start = x.max(0) as u32;
        let x_end = (x + length as i32).min(self.width as i32) as u32;

        for x in x_start..x_end {
            self.set_pixel(x, y as u32, color);
        }
    }

    /// 绘制垂直线
    pub fn draw_vline(&mut self, x: i32, y: i32, length: u32, color: Color) {
        if x < 0 || x >= self.width as i32 {
            return;
        }

        let y_start = y.max(0) as u32;
        let y_end = (y + length as i32).min(self.height as i32) as u32;

        for y in y_start..y_end {
            self.set_pixel(x as u32, y, color);
        }
    }

    /// 绘制线条 (Bresenham算法)
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx - dy;

        let mut x = x0;
        let mut y = y0;

        loop {
            if x >= 0 && x < self.width as i32 && y >= 0 && y < self.height as i32 {
                self.set_pixel(x as u32, y as u32, color);
            }

            if x == x1 && y == y1 {
                break;
            }

            let e2 = 2 * err;
            if e2 > -dy {
                err -= dy;
                x += sx;
            }
            if e2 < dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// 绘制矩形边框
    pub fn draw_rect(&mut self, rect: Rect, color: Color) {
        self.draw_hline(rect.x, rect.y, rect.width, color);
        self.draw_hline(rect.x, rect.y + rect.height as i32 - 1, rect.width, color);
        self.draw_vline(rect.x, rect.y, rect.height, color);
        self.draw_vline(rect.x + rect.width as i32 - 1, rect.y, rect.height, color);
    }

    /// 绘制圆形 (中点圆算法)
    pub fn draw_circle(&mut self, cx: i32, cy: i32, radius: u32, color: Color) {
        let r = radius as i32;
        let mut x = r;
        let mut y = 0;
        let mut err = 0;

        while x >= y {
            self.set_pixel_if_valid(cx + x, cy + y, color);
            self.set_pixel_if_valid(cx + y, cy + x, color);
            self.set_pixel_if_valid(cx - y, cy + x, color);
            self.set_pixel_if_valid(cx - x, cy + y, color);
            self.set_pixel_if_valid(cx - x, cy - y, color);
            self.set_pixel_if_valid(cx - y, cy - x, color);
            self.set_pixel_if_valid(cx + y, cy - x, color);
            self.set_pixel_if_valid(cx + x, cy - y, color);

            y += 1;
            err += 1 + 2 * y;
            if 2 * (err - x) + 1 > 0 {
                x -= 1;
                err += 1 - 2 * x;
            }
        }
    }

    /// 设置像素（带边界检查）
    #[inline]
    fn set_pixel_if_valid(&mut self, x: i32, y: i32, color: Color) {
        if x >= 0 && x < self.width as i32 && y >= 0 && y < self.height as i32 {
            self.set_pixel(x as u32, y as u32, color);
        }
    }

    /// 绘制填充圆（水平线扫描法）
    pub fn fill_circle(&mut self, cx: i32, cy: i32, radius: u32, color: Color) {
        let r = radius as i32;
        let mut x = r;
        let mut y = 0;
        let mut err = 0;

        while x >= y {
            self.draw_hline(cx - x, cy + y, (2 * x) as u32, color);
            self.draw_hline(cx - x, cy - y, (2 * x) as u32, color);
            self.draw_hline(cx - y, cy + x, (2 * y) as u32, color);
            self.draw_hline(cx - y, cy - x, (2 * y) as u32, color);

            y += 1;
            err += 1 + 2 * y;
            if 2 * (err - x) + 1 > 0 {
                x -= 1;
                err += 1 - 2 * x;
            }
        }
    }

    /// Alpha 混合设置像素（读-改-写）
    ///
    /// 若 `color.a == 0` 则不执行任何操作；若 `color.a == 255` 则退化为 `set_pixel`。
    pub fn blend_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x >= self.width || y >= self.height || color.a == 0 {
            return;
        }
        if color.a == 255 {
            self.set_pixel(x, y, color);
            return;
        }
        if let Some(existing) = self.get_pixel(x, y) {
            let blended = color.blend(&existing);
            self.set_pixel(x, y, blended);
        }
    }

    /// Wu 反走样直线
    // 有意窄化: 颜色分量/透明度经规范化计算, 值域 [0,255]
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    pub fn draw_line_aa(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        let steep = (y1 - y0).abs() > (x1 - x0).abs();
        let (mut x0, mut y0, mut x1, mut y1) = (x0, y0, x1, y1);

        if steep {
            core::mem::swap(&mut x0, &mut y0);
            core::mem::swap(&mut x1, &mut y1);
        }
        if x0 > x1 {
            core::mem::swap(&mut x0, &mut x1);
            core::mem::swap(&mut y0, &mut y1);
        }

        let dx = (x1 - x0) as f32;
        let dy = (y1 - y0) as f32;
        let gradient = if dx == 0.0 { 1.0f32 } else { dy / dx };

        // 有意窄化: 浮点光栅化坐标/透明度取整, 值域有界
        #[expect(clippy::cast_possible_truncation)]
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        fn fpart(x: f32) -> f32 {
            x - (x as i32 as f32)
        }
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        fn rfpart(x: f32) -> f32 {
            1.0f32 - fpart(x)
        }

        let xend_i = ((x0 as f32) + 0.5) as i32;
        let xend_f = xend_i as f32;
        let yend_f = y0 as f32 + gradient * (xend_f - x0 as f32);
        let xgap = rfpart(x0 as f32 + 0.5);
        let xpxl1 = xend_i;
        let ypxl1 = yend_f as i32;

        let a1 = (rfpart(yend_f) * xgap * 255.0) as u8;
        let a2 = (fpart(yend_f) * xgap * 255.0) as u8;
        if steep {
            self.blend_pixel(
                ypxl1 as u32,
                xpxl1 as u32,
                Color::new_alpha(color.r, color.g, color.b, a1),
            );
            self.blend_pixel(
                (ypxl1 + 1) as u32,
                xpxl1 as u32,
                Color::new_alpha(color.r, color.g, color.b, a2),
            );
        } else {
            self.blend_pixel(
                xpxl1 as u32,
                ypxl1 as u32,
                Color::new_alpha(color.r, color.g, color.b, a1),
            );
            self.blend_pixel(
                xpxl1 as u32,
                (ypxl1 + 1) as u32,
                Color::new_alpha(color.r, color.g, color.b, a2),
            );
        }

        let mut intery = yend_f + gradient;

        let xend_i = ((x1 as f32) + 0.5) as i32;
        let xend_f = xend_i as f32;
        let yend_f = y1 as f32 + gradient * (xend_f - x1 as f32);
        let xgap = fpart(x1 as f32 + 0.5);
        let xpxl2 = xend_i;
        let ypxl2 = yend_f as i32;

        let a1 = (rfpart(yend_f) * xgap * 255.0) as u8;
        let a2 = (fpart(yend_f) * xgap * 255.0) as u8;
        if steep {
            self.blend_pixel(
                ypxl2 as u32,
                xpxl2 as u32,
                Color::new_alpha(color.r, color.g, color.b, a1),
            );
            self.blend_pixel(
                (ypxl2 + 1) as u32,
                xpxl2 as u32,
                Color::new_alpha(color.r, color.g, color.b, a2),
            );
        } else {
            self.blend_pixel(
                xpxl2 as u32,
                ypxl2 as u32,
                Color::new_alpha(color.r, color.g, color.b, a1),
            );
            self.blend_pixel(
                xpxl2 as u32,
                (ypxl2 + 1) as u32,
                Color::new_alpha(color.r, color.g, color.b, a2),
            );
        }

        if steep {
            for x in (xpxl1 + 1)..xpxl2 {
                let alpha = (rfpart(intery) * 255.0) as u8;
                self.blend_pixel(
                    intery as u32,
                    x as u32,
                    Color::new_alpha(color.r, color.g, color.b, alpha),
                );
                let alpha = (fpart(intery) * 255.0) as u8;
                self.blend_pixel(
                    (intery + 1.0) as u32,
                    x as u32,
                    Color::new_alpha(color.r, color.g, color.b, alpha),
                );
                intery += gradient;
            }
        } else {
            for x in (xpxl1 + 1)..xpxl2 {
                let alpha = (rfpart(intery) * 255.0) as u8;
                self.blend_pixel(
                    x as u32,
                    intery as u32,
                    Color::new_alpha(color.r, color.g, color.b, alpha),
                );
                let alpha = (fpart(intery) * 255.0) as u8;
                self.blend_pixel(
                    x as u32,
                    (intery + 1.0) as u32,
                    Color::new_alpha(color.r, color.g, color.b, alpha),
                );
                intery += gradient;
            }
        }
    }

    /// 清屏
    pub fn clear(&mut self) {
        self.fill(colors::BLACK);
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
