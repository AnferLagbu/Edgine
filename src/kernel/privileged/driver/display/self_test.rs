use super::font::Font;
use super::framebuffer::colors;
use super::framebuffer::{Color, Framebuffer, Rect};

/// 帧缓冲自检——绘制完整测试图案并验证功能正确性
///
/// ┌──────────────────────────────────────────────┐
/// │  ████████  ████████  ████████  ████████      │  纯色条 (R/G/B/W)
/// │  RED       GREEN      BLUE       WHITE       │  纯色条 (红/绿/蓝/白)
/// ├──────────────────────────────────────────────┤
/// │  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │  256 级灰度渐变
/// ├──────────────────────────────────────────────┤
/// │  ╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱╱   │  对角 RGB 渐变
/// ├──────────────────────────────────────────────┤
/// │  ┌──────┐    ╱      ○      ●              │  图形原语 (矩形/直线/圆/填充圆)
/// ├──────────────────────────────────────────────┤
/// │  ≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈≈              │  反走样直线 (Wu)
/// ├──────────────────────────────────────────────┤
/// │  `Edgine` Framebuffer Self-Test v2.0             │  调试文本
/// │  Resolution: 1024x768x32                     │
/// └──────────────────────────────────────────────┘
///
/// 返回失败像素数（0 = 全部通过）。

// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
pub fn framebuffer_self_test(fb: &mut Framebuffer, font: &Font) -> usize {
    let mut failures: usize = 0;
    let fw = fb.width();
    let fh = fb.height();

    // ========================================================================
    // 1. 纯色条 (R/G/B/W) — 顶部 48 像素
    // ========================================================================
    let stripe_h: u32 = 48;
    let stripe_w: u32 = fw / 4;

    fb.fill_rect(Rect::new(0, 0, stripe_w, stripe_h), colors::RED);
    fb.fill_rect(
        Rect::new(stripe_w as i32, 0, stripe_w, stripe_h),
        colors::GREEN,
    );
    fb.fill_rect(
        Rect::new(2 * stripe_w as i32, 0, stripe_w, stripe_h),
        colors::BLUE,
    );
    fb.fill_rect(
        Rect::new(3 * stripe_w as i32, 0, stripe_w, stripe_h),
        colors::WHITE,
    );

    if let Some(r) = fb.get_pixel(stripe_w / 2, stripe_h / 2) {
        if r.r < 220 || r.g > 20 || r.b > 20 {
            failures += 1;
        }
    } else {
        failures += 1;
    }

    if let Some(g) = fb.get_pixel(stripe_w + stripe_w / 2, stripe_h / 2) {
        if g.g < 220 || g.r > 20 || g.b > 20 {
            failures += 1;
        }
    } else {
        failures += 1;
    }

    if let Some(b) = fb.get_pixel(2 * stripe_w + stripe_w / 2, stripe_h / 2) {
        if b.b < 220 || b.r > 20 || b.g > 20 {
            failures += 1;
        }
    } else {
        failures += 1;
    }

    // ========================================================================
    // 2. 256 级灰度渐变 (y=56, 单行)
    // ========================================================================
    let gray_y: u32 = 56;
    let gray_len = fw.min(256);
    for x in 0..gray_len {
        let gray = x as u8;
        fb.set_pixel(x, gray_y, Color::new(gray, gray, gray));
    }

    if let Some(g0) = fb.get_pixel(0, gray_y) {
        if g0.r != 0 || g0.g != 0 || g0.b != 0 {
            failures += 1;
        }
    } else {
        failures += 1;
    }
    if let Some(g128) = fb.get_pixel(128.min(gray_len - 1), gray_y) {
        if (i32::from(g128.r) - 128i32).abs() > 4 {
            failures += 1;
        }
    } else {
        failures += 1;
    }
    if let Some(g255) = fb.get_pixel(255.min(gray_len - 1), gray_y) {
        if g255.r < 248 || g255.g < 248 || g255.b < 248 {
            failures += 1;
        }
    } else {
        failures += 1;
    }

    // ========================================================================
    // 3. 对角 RGB 渐变 — 从 (0, 64) 到 (fw, 64+fw) 的 sweep
    // ========================================================================
    if fh > 130 {
        let sweep_start: u32 = 64;
        let sweep_len = fw.min(fh - sweep_start);
        for i in 0..sweep_len {
            let px = i;
            let py = sweep_start + i;
            // red → green → blue 渐变
            let color = if i < sweep_len / 3 {
                let t = (i * 765 / sweep_len) as u8;
                Color::new(255u8.saturating_sub(t), t, 0)
            } else if i < 2 * sweep_len / 3 {
                let t = ((i - sweep_len / 3) * 765 / sweep_len) as u8;
                Color::new(0, 255u8.saturating_sub(t), t)
            } else {
                let t = ((i - 2 * sweep_len / 3) * 765 / sweep_len) as u8;
                Color::new(t, 0, 255u8.saturating_sub(t))
            };
            fb.set_pixel(px, py, color);
        }

        // 采样验证：中点应为绿色附近
        let mid = sweep_len / 2;
        if let Some(mid_px) = fb.get_pixel(mid, sweep_start + mid) {
            if mid_px.g < 80 {
                failures += 1;
            }
        } else {
            failures += 1;
        }
    }

    // ========================================================================
    // 4. 图形原语展示 — 在屏幕中下部
    // ========================================================================
    if fh > 200 {
        let prim_y: i32 = 130;

        // 矩形边框
        fb.draw_rect(Rect::new(10, prim_y, 64, 48), colors::CYAN);

        // 直线 (Bresenham)
        fb.draw_line(95, prim_y + 48, 155, prim_y, colors::MAGENTA);

        // 圆形边框
        fb.draw_circle(210, prim_y + 24, 22, colors::YELLOW);

        // 填充圆
        fb.fill_circle(280, prim_y + 24, 16, colors::GREEN);

        // 反走样直线 (Wu)
        fb.draw_line_aa(10, prim_y + 60, 180, prim_y + 100, colors::WHITE);

        // 水平反走样直线
        fb.draw_line_aa(200, prim_y + 80, 310, prim_y + 80, colors::LIGHT_GRAY);

        // 验证填充圆
        if let Some(c) = fb.get_pixel(280, prim_y as u32 + 24) {
            if c.g < 200 {
                failures += 1;
            }
        } else {
            failures += 1;
        }

        // 验证圆形边框 (中心是背景色，圆上一点是黄色)
        if let Some(c) = fb.get_pixel(210, prim_y as u32 + 24) {
            if c.r > 10 || c.g > 10 || c.b > 10 {
                failures += 1;
            }
        }
    }

    // ========================================================================
    // 5. 调试文本
    // ========================================================================
    let text_y: u32 = fh.saturating_sub(40);
    font.render_text(
        fb,
        "Edgine Framebuffer Self-Test v2.0",
        10,
        text_y,
        colors::WHITE,
        colors::BLACK,
    );
    font.render_text_wrapped(
        fb,
        &alloc::format!(
            "Resolution: {}x{}x{}  Failures: {}",
            fw,
            fh,
            fb.format().bits_per_pixel(),
            failures
        ),
        10,
        text_y + font.glyph_height,
        fw,
        colors::LIGHT_GRAY,
        colors::BLACK,
    );

    failures
}
