#![deny(unsafe_code)]
//! 帧缓冲绘图策略实现 — functions 层 (0 unsafe)
//!
//! 落实 `docs/plan/framekernel-paradigm-enforcement.md` §6.3: 帧缓冲的
//! **绘图算法** (填充/直线/矩形/圆/alpha 混合/Wu 反走样) 自
//! `privileged/driver/display/framebuffer.rs` 下沉至此; privileged 仅保留
//! 像素格式转换与 MMIO 写入 (`set_pixel`/`get_pixel`)、几何参数与滚动机制.
//!
//! 算法对 [`PixelCanvas`] 编程, 不接触任何硬件寄存器; 生产环境由
//! `Framebuffer` 实现画布契约, 单元测试用内存画布 (无需 MMIO).
//!
//! ## 接线时序
//!
//! [`init_render_policy`] 必须在 `privileged::driver::init_all()` **之前** 由
//! crate root 调用: 该链路内 `display_init()` 会执行帧缓冲自检与
//! `GfxConsole` 初始化, 两者都经
//! [`current_framebuffer_ops`](crate::privileged::driver::current_framebuffer_ops)
//! 消费本策略.

use crate::privileged::driver::{Color, FramebufferOps, PixelCanvas, Rect, colors};

/// 标准帧缓冲绘图策略 — Bresenham 直线 / 中点圆 / 水平线扫描 / Wu 反走样
pub struct StandardFramebufferOps;

/// 全局标准绘图策略实例 — [`init_render_policy`] 向 privileged 注册
pub static STANDARD_FRAMEBUFFER_OPS: StandardFramebufferOps = StandardFramebufferOps;

impl FramebufferOps for StandardFramebufferOps {
    fn fill(&self, canvas: &mut dyn PixelCanvas, color: Color) {
        fill_impl(canvas, color);
    }

    fn clear(&self, canvas: &mut dyn PixelCanvas) {
        fill_impl(canvas, colors::BLACK);
    }

    fn fill_rect(&self, canvas: &mut dyn PixelCanvas, rect: Rect, color: Color) {
        fill_rect_impl(canvas, rect, color);
    }

    fn draw_hline(&self, canvas: &mut dyn PixelCanvas, x: i32, y: i32, length: u32, color: Color) {
        draw_hline_impl(canvas, x, y, length, color);
    }

    fn draw_vline(&self, canvas: &mut dyn PixelCanvas, x: i32, y: i32, length: u32, color: Color) {
        draw_vline_impl(canvas, x, y, length, color);
    }

    fn draw_line(
        &self,
        canvas: &mut dyn PixelCanvas,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Color,
    ) {
        draw_line_impl(canvas, x0, y0, x1, y1, color);
    }

    fn draw_rect(&self, canvas: &mut dyn PixelCanvas, rect: Rect, color: Color) {
        draw_rect_impl(canvas, rect, color);
    }

    fn draw_circle(
        &self,
        canvas: &mut dyn PixelCanvas,
        cx: i32,
        cy: i32,
        radius: u32,
        color: Color,
    ) {
        draw_circle_impl(canvas, cx, cy, radius, color);
    }

    fn fill_circle(
        &self,
        canvas: &mut dyn PixelCanvas,
        cx: i32,
        cy: i32,
        radius: u32,
        color: Color,
    ) {
        fill_circle_impl(canvas, cx, cy, radius, color);
    }

    fn blend_pixel(&self, canvas: &mut dyn PixelCanvas, x: u32, y: u32, color: Color) {
        blend_pixel_impl(canvas, x, y, color);
    }

    fn draw_line_aa(
        &self,
        canvas: &mut dyn PixelCanvas,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Color,
    ) {
        draw_line_aa_impl(canvas, x0, y0, x1, y1, color);
    }
}

/// 注册帧缓冲绘图策略到 privileged (crate root 编排调用)
///
/// 时序硬约束: 必须早于 `privileged::driver::init_all()` (见模块文档).
/// 重复注册返回 `Err`, 启动期只应调用一次, 故忽略返回值.
pub fn init_render_policy() {
    let _ = crate::privileged::driver::register_framebuffer_ops(&STANDARD_FRAMEBUFFER_OPS);
}

// ============================================================================
// 绘图算法 (纯策略, 仅经 PixelCanvas 落盘)
// ============================================================================

/// 填充整个画布
fn fill_impl(canvas: &mut dyn PixelCanvas, color: Color) {
    for y in 0..canvas.height() {
        for x in 0..canvas.width() {
            canvas.set_pixel(x, y, color);
        }
    }
}

/// 填充矩形区域 (自动裁剪到画布内)
fn fill_rect_impl(canvas: &mut dyn PixelCanvas, rect: Rect, color: Color) {
    let x_start = rect.x.max(0).cast_unsigned();
    let y_start = rect.y.max(0).cast_unsigned();
    let x_end = (rect.x + rect.width.cast_signed())
        .min(canvas.width().cast_signed())
        .cast_unsigned();
    let y_end = (rect.y + rect.height.cast_signed())
        .min(canvas.height().cast_signed())
        .cast_unsigned();

    for y in y_start..y_end {
        for x in x_start..x_end {
            canvas.set_pixel(x, y, color);
        }
    }
}

/// 绘制水平线
fn draw_hline_impl(canvas: &mut dyn PixelCanvas, x: i32, y: i32, length: u32, color: Color) {
    if y < 0 || y >= canvas.height().cast_signed() {
        return;
    }

    let x_start = x.max(0).cast_unsigned();
    let x_end = (x + length.cast_signed())
        .min(canvas.width().cast_signed())
        .cast_unsigned();

    for x in x_start..x_end {
        canvas.set_pixel(x, y.cast_unsigned(), color);
    }
}

/// 绘制垂直线
fn draw_vline_impl(canvas: &mut dyn PixelCanvas, x: i32, y: i32, length: u32, color: Color) {
    if x < 0 || x >= canvas.width().cast_signed() {
        return;
    }

    let y_start = y.max(0).cast_unsigned();
    let y_end = (y + length.cast_signed())
        .min(canvas.height().cast_signed())
        .cast_unsigned();

    for y in y_start..y_end {
        canvas.set_pixel(x.cast_unsigned(), y, color);
    }
}

/// 绘制直线 (Bresenham 算法)
fn draw_line_impl(canvas: &mut dyn PixelCanvas, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    let dx = (x1 - x0).abs();
    let dy = (y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx - dy;

    let mut x = x0;
    let mut y = y0;

    loop {
        set_pixel_if_valid(canvas, x, y, color);

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
fn draw_rect_impl(canvas: &mut dyn PixelCanvas, rect: Rect, color: Color) {
    draw_hline_impl(canvas, rect.x, rect.y, rect.width, color);
    draw_hline_impl(
        canvas,
        rect.x,
        rect.y + rect.height.cast_signed() - 1,
        rect.width,
        color,
    );
    draw_vline_impl(canvas, rect.x, rect.y, rect.height, color);
    draw_vline_impl(
        canvas,
        rect.x + rect.width.cast_signed() - 1,
        rect.y,
        rect.height,
        color,
    );
}

/// 绘制圆形边框 (中点圆算法)
fn draw_circle_impl(canvas: &mut dyn PixelCanvas, cx: i32, cy: i32, radius: u32, color: Color) {
    let r = radius.cast_signed();
    let mut x = r;
    let mut y = 0;
    let mut err = 0;

    while x >= y {
        set_pixel_if_valid(canvas, cx + x, cy + y, color);
        set_pixel_if_valid(canvas, cx + y, cy + x, color);
        set_pixel_if_valid(canvas, cx - y, cy + x, color);
        set_pixel_if_valid(canvas, cx - x, cy + y, color);
        set_pixel_if_valid(canvas, cx - x, cy - y, color);
        set_pixel_if_valid(canvas, cx - y, cy - x, color);
        set_pixel_if_valid(canvas, cx + y, cy - x, color);
        set_pixel_if_valid(canvas, cx + x, cy - y, color);

        y += 1;
        err += 1 + 2 * y;
        if 2 * (err - x) + 1 > 0 {
            x -= 1;
            err += 1 - 2 * x;
        }
    }
}

/// 设置像素 (带边界检查)
#[inline]
fn set_pixel_if_valid(canvas: &mut dyn PixelCanvas, x: i32, y: i32, color: Color) {
    if x >= 0 && x < canvas.width().cast_signed() && y >= 0 && y < canvas.height().cast_signed() {
        canvas.set_pixel(x.cast_unsigned(), y.cast_unsigned(), color);
    }
}

/// 绘制填充圆 (水平线扫描法)
fn fill_circle_impl(canvas: &mut dyn PixelCanvas, cx: i32, cy: i32, radius: u32, color: Color) {
    let r = radius.cast_signed();
    let mut x = r;
    let mut y = 0;
    let mut err = 0;

    while x >= y {
        draw_hline_impl(canvas, cx - x, cy + y, (2 * x).cast_unsigned(), color);
        draw_hline_impl(canvas, cx - x, cy - y, (2 * x).cast_unsigned(), color);
        draw_hline_impl(canvas, cx - y, cy + x, (2 * y).cast_unsigned(), color);
        draw_hline_impl(canvas, cx - y, cy - x, (2 * y).cast_unsigned(), color);

        y += 1;
        err += 1 + 2 * y;
        if 2 * (err - x) + 1 > 0 {
            x -= 1;
            err += 1 - 2 * x;
        }
    }
}

/// Alpha 混合写入单个像素 (读-改-写)
///
/// `a == 0` 不写入; `a == 255` 退化为直接写入.
fn blend_pixel_impl(canvas: &mut dyn PixelCanvas, x: u32, y: u32, color: Color) {
    if x >= canvas.width() || y >= canvas.height() || color.a == 0 {
        return;
    }
    if color.a == 255 {
        canvas.set_pixel(x, y, color);
        return;
    }
    if let Some(existing) = canvas.get_pixel(x, y) {
        let blended = color.blend(&existing);
        canvas.set_pixel(x, y, blended);
    }
}

/// 取浮点数的小数部分 (Wu 反走样辅助)
fn fpart(x: f32) -> f32 {
    x - crate::functions::intcast::f32_from_i32(crate::functions::intcast::i32_from_f32(x))
}

/// 取浮点数小数部分的补值 (Wu 反走样辅助)
fn rfpart(x: f32) -> f32 {
    1.0f32 - fpart(x)
}

/// 绘制反走样直线 (Wu 算法)
fn draw_line_aa_impl(
    canvas: &mut dyn PixelCanvas,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: Color,
) {
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

    let dx = crate::functions::intcast::f32_from_i32(x1 - x0);
    let dy = crate::functions::intcast::f32_from_i32(y1 - y0);
    let gradient = if dx == 0.0 { 1.0f32 } else { dy / dx };

    let xend_i =
        crate::functions::intcast::i32_from_f32(crate::functions::intcast::f32_from_i32(x0) + 0.5);
    let xend_f = crate::functions::intcast::f32_from_i32(xend_i);
    let yend_f = crate::functions::intcast::f32_from_i32(y0)
        + gradient * (xend_f - crate::functions::intcast::f32_from_i32(x0));
    let xgap = rfpart(crate::functions::intcast::f32_from_i32(x0) + 0.5);
    let xpxl1 = xend_i;
    let ypxl1 = crate::functions::intcast::i32_from_f32(yend_f);

    let a1 = crate::functions::intcast::u8_from_i32(crate::functions::intcast::i32_from_f32(
        rfpart(yend_f) * xgap * 255.0,
    ));
    let a2 = crate::functions::intcast::u8_from_i32(crate::functions::intcast::i32_from_f32(
        fpart(yend_f) * xgap * 255.0,
    ));
    if steep {
        blend_pixel_impl(
            canvas,
            ypxl1.cast_unsigned(),
            xpxl1.cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a1),
        );
        blend_pixel_impl(
            canvas,
            (ypxl1 + 1).cast_unsigned(),
            xpxl1.cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a2),
        );
    } else {
        blend_pixel_impl(
            canvas,
            xpxl1.cast_unsigned(),
            ypxl1.cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a1),
        );
        blend_pixel_impl(
            canvas,
            xpxl1.cast_unsigned(),
            (ypxl1 + 1).cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a2),
        );
    }

    let mut intery = yend_f + gradient;

    let xend_i =
        crate::functions::intcast::i32_from_f32(crate::functions::intcast::f32_from_i32(x1) + 0.5);
    let xend_f = crate::functions::intcast::f32_from_i32(xend_i);
    let yend_f = crate::functions::intcast::f32_from_i32(y1)
        + gradient * (xend_f - crate::functions::intcast::f32_from_i32(x1));
    let xgap = fpart(crate::functions::intcast::f32_from_i32(x1) + 0.5);
    let xpxl2 = xend_i;
    let ypxl2 = crate::functions::intcast::i32_from_f32(yend_f);

    let a1 = crate::functions::intcast::u8_from_i32(crate::functions::intcast::i32_from_f32(
        rfpart(yend_f) * xgap * 255.0,
    ));
    let a2 = crate::functions::intcast::u8_from_i32(crate::functions::intcast::i32_from_f32(
        fpart(yend_f) * xgap * 255.0,
    ));
    if steep {
        blend_pixel_impl(
            canvas,
            ypxl2.cast_unsigned(),
            xpxl2.cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a1),
        );
        blend_pixel_impl(
            canvas,
            (ypxl2 + 1).cast_unsigned(),
            xpxl2.cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a2),
        );
    } else {
        blend_pixel_impl(
            canvas,
            xpxl2.cast_unsigned(),
            ypxl2.cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a1),
        );
        blend_pixel_impl(
            canvas,
            xpxl2.cast_unsigned(),
            (ypxl2 + 1).cast_unsigned(),
            Color::new_alpha(color.r, color.g, color.b, a2),
        );
    }

    if steep {
        for x in (xpxl1 + 1)..xpxl2 {
            let alpha = crate::functions::intcast::u8_from_i32(
                crate::functions::intcast::i32_from_f32(rfpart(intery) * 255.0),
            );
            blend_pixel_impl(
                canvas,
                crate::functions::intcast::i32_from_f32(intery).cast_unsigned(),
                x.cast_unsigned(),
                Color::new_alpha(color.r, color.g, color.b, alpha),
            );
            let alpha = crate::functions::intcast::u8_from_i32(
                crate::functions::intcast::i32_from_f32(fpart(intery) * 255.0),
            );
            blend_pixel_impl(
                canvas,
                crate::functions::intcast::i32_from_f32(intery + 1.0).cast_unsigned(),
                x.cast_unsigned(),
                Color::new_alpha(color.r, color.g, color.b, alpha),
            );
            intery += gradient;
        }
    } else {
        for x in (xpxl1 + 1)..xpxl2 {
            let alpha = crate::functions::intcast::u8_from_i32(
                crate::functions::intcast::i32_from_f32(rfpart(intery) * 255.0),
            );
            blend_pixel_impl(
                canvas,
                x.cast_unsigned(),
                crate::functions::intcast::i32_from_f32(intery).cast_unsigned(),
                Color::new_alpha(color.r, color.g, color.b, alpha),
            );
            let alpha = crate::functions::intcast::u8_from_i32(
                crate::functions::intcast::i32_from_f32(fpart(intery) * 255.0),
            );
            blend_pixel_impl(
                canvas,
                x.cast_unsigned(),
                crate::functions::intcast::i32_from_f32(intery + 1.0).cast_unsigned(),
                Color::new_alpha(color.r, color.g, color.b, alpha),
            );
            intery += gradient;
        }
    }
}

// ============================================================================
// 单元测试 — 内存画布替身, 无需 MMIO
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 内存画布 — 绘图算法单测替身
    struct MockCanvas {
        width: u32,
        height: u32,
        pixels: alloc::vec::Vec<Color>,
    }

    impl MockCanvas {
        fn new(width: u32, height: u32) -> Self {
            let len = (width * height) as usize;
            Self {
                width,
                height,
                pixels: alloc::vec![colors::BLACK; len],
            }
        }

        fn at(&self, x: u32, y: u32) -> Color {
            self.pixels[(y * self.width + x) as usize]
        }
    }

    impl PixelCanvas for MockCanvas {
        fn width(&self) -> u32 {
            self.width
        }

        fn height(&self) -> u32 {
            self.height
        }

        fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
            if x < self.width && y < self.height {
                self.pixels[(y * self.width + x) as usize] = color;
            }
        }

        fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
            if x < self.width && y < self.height {
                Some(self.pixels[(y * self.width + x) as usize])
            } else {
                None
            }
        }
    }

    #[test]
    fn fill_covers_all_pixels() {
        let mut canvas = MockCanvas::new(4, 3);
        StandardFramebufferOps.fill(&mut canvas, colors::GREEN);

        for y in 0..3 {
            for x in 0..4 {
                assert_eq!(canvas.at(x, y), colors::GREEN);
            }
        }
    }

    #[test]
    fn fill_rect_clips_negative_origin() {
        let mut canvas = MockCanvas::new(8, 8);
        StandardFramebufferOps.fill_rect(&mut canvas, Rect::new(-2, -2, 4, 4), colors::RED);

        // 裁剪后仅 (0..2, 0..2) 落色
        assert_eq!(canvas.at(0, 0), colors::RED);
        assert_eq!(canvas.at(1, 1), colors::RED);
        assert_eq!(canvas.at(2, 0), colors::BLACK);
        assert_eq!(canvas.at(0, 2), colors::BLACK);
    }

    #[test]
    fn fill_rect_clips_oversized_extent() {
        let mut canvas = MockCanvas::new(4, 4);
        StandardFramebufferOps.fill_rect(&mut canvas, Rect::new(2, 2, 10, 10), colors::BLUE);

        assert_eq!(canvas.at(3, 3), colors::BLUE);
        assert_eq!(canvas.at(1, 1), colors::BLACK);
    }

    #[test]
    fn hline_and_vline_respect_bounds() {
        let mut canvas = MockCanvas::new(4, 4);
        StandardFramebufferOps.draw_hline(&mut canvas, -2, 1, 10, colors::CYAN);
        StandardFramebufferOps.draw_vline(&mut canvas, 3, -1, 2, colors::MAGENTA);

        // 水平线裁剪到整行
        assert_eq!(canvas.at(0, 1), colors::CYAN);
        assert_eq!(canvas.at(3, 1), colors::CYAN);
        // 垂直线裁剪后仅 (3, 0) 落色; 行 1 已被水平线覆盖为 CYAN
        assert_eq!(canvas.at(3, 0), colors::MAGENTA);
        assert_eq!(canvas.at(3, 2), colors::BLACK);
    }

    #[test]
    fn draw_line_diagonal_hits_diagonal_pixels() {
        let mut canvas = MockCanvas::new(6, 6);
        StandardFramebufferOps.draw_line(&mut canvas, 0, 0, 5, 5, colors::WHITE);

        for i in 0..6 {
            assert_eq!(canvas.at(i, i), colors::WHITE);
        }
        assert_eq!(canvas.at(0, 1), colors::BLACK);
    }

    #[test]
    fn draw_rect_draws_border_only() {
        let mut canvas = MockCanvas::new(8, 8);
        StandardFramebufferOps.draw_rect(&mut canvas, Rect::new(1, 1, 6, 4), colors::YELLOW);

        // 四条边落色
        assert_eq!(canvas.at(1, 1), colors::YELLOW);
        assert_eq!(canvas.at(6, 1), colors::YELLOW);
        assert_eq!(canvas.at(1, 4), colors::YELLOW);
        assert_eq!(canvas.at(6, 4), colors::YELLOW);
        // 内部保持背景
        assert_eq!(canvas.at(3, 3), colors::BLACK);
    }

    #[test]
    fn draw_circle_rim_set_center_empty() {
        let mut canvas = MockCanvas::new(20, 20);
        StandardFramebufferOps.draw_circle(&mut canvas, 10, 10, 5, colors::YELLOW);

        // 中点圆算法首个落点为 (cx + r, cy)
        assert_eq!(canvas.at(15, 10), colors::YELLOW);
        // 圆心不落色
        assert_eq!(canvas.at(10, 10), colors::BLACK);
    }

    #[test]
    fn fill_circle_fills_interior() {
        let mut canvas = MockCanvas::new(20, 20);
        StandardFramebufferOps.fill_circle(&mut canvas, 10, 10, 5, colors::GREEN);

        assert_eq!(canvas.at(10, 10), colors::GREEN);
        assert_eq!(canvas.at(10, 15), colors::GREEN);
        // 圆外保持背景
        assert_eq!(canvas.at(0, 0), colors::BLACK);
    }

    #[test]
    fn blend_pixel_alpha_zero_is_noop() {
        let mut canvas = MockCanvas::new(2, 2);
        StandardFramebufferOps.blend_pixel(&mut canvas, 0, 0, Color::new_alpha(255, 0, 0, 0));
        assert_eq!(canvas.at(0, 0), colors::BLACK);
    }

    #[test]
    fn blend_pixel_alpha_max_replaces() {
        let mut canvas = MockCanvas::new(2, 2);
        StandardFramebufferOps.blend_pixel(&mut canvas, 0, 0, colors::RED);
        assert_eq!(canvas.at(0, 0), colors::RED);
    }

    #[test]
    fn blend_pixel_half_alpha_mixes_channels() {
        let mut canvas = MockCanvas::new(2, 2);
        StandardFramebufferOps.fill(&mut canvas, colors::BLUE);
        StandardFramebufferOps.blend_pixel(&mut canvas, 0, 0, Color::new_alpha(255, 0, 0, 128));

        let blended = canvas.at(0, 0);
        assert!(blended.r > 0 && blended.r < 255);
        assert!(blended.b > 0 && blended.b < 255);
    }

    #[test]
    fn clear_paints_black() {
        let mut canvas = MockCanvas::new(3, 3);
        StandardFramebufferOps.fill(&mut canvas, colors::WHITE);
        StandardFramebufferOps.clear(&mut canvas);

        assert_eq!(canvas.at(0, 0), colors::BLACK);
        assert_eq!(canvas.at(2, 2), colors::BLACK);
    }

    #[test]
    fn draw_line_aa_leaves_pixels_on_canvas() {
        let mut canvas = MockCanvas::new(32, 32);
        StandardFramebufferOps.draw_line_aa(&mut canvas, 0, 0, 31, 20, colors::WHITE);

        let painted = (0..32)
            .flat_map(|y| (0..32).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.at(x, y) != colors::BLACK)
            .count();
        assert!(painted > 0, "反走样直线应至少落一个像素");
    }
}
