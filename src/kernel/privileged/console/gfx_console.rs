use crate::privileged::driver::{Color, Font, Framebuffer, Rect, colors};
use core::sync::atomic::{AtomicBool, Ordering};

/// 全局紧急/panic 标记 — 当为 true 时，GfxConsole 输出使用 panic 专用配色
pub static PANIC_MODE: AtomicBool = AtomicBool::new(false);

/// 图形控制台 — 在帧缓冲上模拟文本终端
///
/// 替代 VGA 文本模式，使用 PSF 字体在帧缓冲上绘制字符。
/// 支持自动换行、屏幕滚动、以及 panic 紧急模式。
///
/// ## 双模式设计
///
/// - **正常模式**: 白字黑底，用于内核日志输出
/// - **Panic 模式**: 红底白字横幅 + 详细信息，用于崩溃现场展示
///
/// Panic 模式下：
/// 1. 立即清屏并绘制红色顶部横幅
/// 2. 禁用滚动，保留完整崩溃信息
/// 3. 光标定位到横幅下方，准备输出崩溃详情
pub struct GfxConsole {
    fb: *mut Framebuffer,
    font: &'static Font,
    cursor_x: u32,
    cursor_y: u32,
    cols: u32,
    rows: u32,
    fg_color: Color,
    bg_color: Color,
    top_margin: u32,
    panic_banner_height: u32,
}

impl GfxConsole {
    pub fn new(fb: *mut Framebuffer, font: &'static Font) -> Self {
        // SAFETY: `fb` 由调用方保证为有效指针; 只读访问
        let fb_ref = unsafe { &*fb };
        let cols = fb_ref.width() / font.glyph_width;
        let rows = fb_ref.height() / font.glyph_height;
        Self {
            fb,
            font,
            cursor_x: 0,
            cursor_y: 0,
            cols,
            rows,
            fg_color: colors::WHITE,
            bg_color: colors::BLACK,
            top_margin: 0,
            panic_banner_height: 0,
        }
    }

    #[inline]
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe fn fb_mut(&self) -> &mut Framebuffer {
        unsafe { &mut *self.fb }
    }

    pub fn set_margin(&mut self, top: u32) {
        self.top_margin = top.min(self.rows);
    }

    pub fn set_colors(&mut self, fg: Color, bg: Color) {
        self.fg_color = fg;
        self.bg_color = bg;
    }

    pub fn clear(&mut self) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let fb = unsafe { self.fb_mut() };
        fb.fill_rect(Rect::new(0, 0, fb.width(), fb.height()), self.bg_color);
        self.cursor_x = 0;
        self.cursor_y = self.top_margin;
    }

    /// 进入 panic 紧急显示模式
    ///
    /// 在系统崩溃时接管帧缓冲：
    /// 1. 清空整个屏幕为黑色
    /// 2. 在顶部绘制红色横幅 "[ KERNEL PANIC ]"
    /// 3. 显示崩溃消息
    /// 4. 设置光标到横幅下方
    /// 5. 禁用后续滚动，保留崩溃现场
    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    pub fn panic_reclaim(&mut self, msg: &str) {
        PANIC_MODE.store(true, Ordering::Release);
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let fb = unsafe { self.fb_mut() };
        let fb_w = fb.width();
        let fb_h = fb.height();
        let glyph_h = self.font.glyph_height;
        let glyph_w = self.font.glyph_width;

        // 1. 全屏黑色背景
        fb.fill_rect(Rect::new(0, 0, fb_w, fb_h), colors::BLACK);

        // 2. 红色横幅条 (2 行高度)
        let banner_rows: u32 = 2;
        let banner_px = banner_rows * glyph_h;
        let banner_rect = Rect::new(0, 0, fb_w, banner_px);
        fb.fill_rect(banner_rect, Color::new(180, 0, 0));
        self.panic_banner_height = banner_rows;

        // 3. 横幅文字 "[ KERNEL PANIC ]" — 居中放置
        let banner_text = b"[ KERNEL PANIC ]";
        let text_px_w = banner_text.len() as u32 * glyph_w;
        let text_x = if fb_w > text_px_w {
            (fb_w - text_px_w) / 2
        } else {
            0
        };
        let text_y = glyph_h / 2; // 垂直居中在条内
        // SAFETY: `self` 由调用方保证为有效指针; 只读访问
        let fb_ref = unsafe { &mut *self.fb };
        for (i, &ch) in banner_text.iter().enumerate() {
            let px = text_x + i as u32 * glyph_w;
            self.font.render_char(
                fb_ref,
                ch as char,
                px,
                text_y,
                colors::WHITE,
                Color::new(180, 0, 0),
            );
        }

        // 4. 横幅下方显示崩溃消息
        self.cursor_x = glyph_w; // 左缩进一个字符
        self.cursor_y = banner_rows + 1;
        self.fg_color = colors::WHITE;
        self.bg_color = colors::BLACK;
        self.top_margin = banner_rows + 1;

        // 5. 输出消息
        self.write_str(msg);
    }

    /// Panic 模式下写入 — 不滚动，直接追加
    pub fn panic_write(&mut self, s: &str) {
        if !PANIC_MODE.load(Ordering::Acquire) {
            self.write_str(s);
            return;
        }
        // Panic 模式下：不滚动，超出行数则截断
        for ch in s.chars() {
            if self.cursor_y >= self.rows {
                break; // 屏幕已满，停止输出
            }
            self.putchar_no_scroll(ch);
        }
    }

    fn putchar_no_scroll(&mut self, ch: char) {
        match ch {
            '\n' => {
                self.cursor_x = 0;
                self.cursor_y += 1;
            }
            '\r' => self.cursor_x = 0,
            '\t' => self.cursor_x = (self.cursor_x + 4) & !3,
            '\x08' => {
                if self.cursor_x > 0 {
                    self.cursor_x -= 1;
                    let px = self.cursor_x * self.font.glyph_width;
                    let py = self.cursor_y * self.font.glyph_height;
                    let fg = self.fg_color;
                    let bg = self.bg_color;
                    // SAFETY: `self` 由调用方保证为有效指针; 只读访问
                    let fb = unsafe { &mut *self.fb };
                    self.font.render_char(fb, ' ', px, py, fg, bg);
                }
            }
            _ => {
                if (ch.is_ascii_graphic() || ch == ' ') && self.cursor_y < self.rows {
                    let px = self.cursor_x * self.font.glyph_width;
                    let py = self.cursor_y * self.font.glyph_height;
                    let fg = self.fg_color;
                    let bg = self.bg_color;
                    // SAFETY: `self` 由调用方保证为有效指针; 只读访问
                    let fb = unsafe { &mut *self.fb };
                    self.font.render_char(fb, ch, px, py, fg, bg);
                    self.cursor_x += 1;
                    if self.cursor_x >= self.cols {
                        self.cursor_x = 0;
                        self.cursor_y += 1;
                    }
                }
            }
        }
    }

    pub fn putchar(&mut self, ch: char) {
        if PANIC_MODE.load(Ordering::Acquire) {
            return self.putchar_no_scroll(ch);
        }
        match ch {
            '\n' => {
                self.cursor_x = 0;
                self.cursor_y += 1;
            }
            '\r' => self.cursor_x = 0,
            '\t' => self.cursor_x = (self.cursor_x + 4) & !3,
            '\x08' => {
                if self.cursor_x > 0 {
                    self.cursor_x -= 1;
                    let px = self.cursor_x * self.font.glyph_width;
                    let py = self.cursor_y * self.font.glyph_height;
                    let fg = self.fg_color;
                    let bg = self.bg_color;
                    // SAFETY: `self` 由调用方保证为有效指针; 只读访问
                    let fb = unsafe { &mut *self.fb };
                    self.font.render_char(fb, ' ', px, py, fg, bg);
                }
            }
            _ => {
                if ch.is_ascii_graphic() || ch == ' ' {
                    let px = self.cursor_x * self.font.glyph_width;
                    let py = self.cursor_y * self.font.glyph_height;
                    let fg = self.fg_color;
                    let bg = self.bg_color;
                    // SAFETY: `self` 由调用方保证为有效指针; 只读访问
                    let fb = unsafe { &mut *self.fb };
                    self.font.render_char(fb, ch, px, py, fg, bg);
                    self.cursor_x += 1;
                }
            }
        }

        if self.cursor_x >= self.cols {
            self.cursor_x = 0;
            self.cursor_y += 1;
        }
        if self.cursor_y >= self.rows {
            self.scroll_up(1);
        }
    }

    fn scroll_up(&mut self, lines: u32) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let fb = unsafe { self.fb_mut() };
        let glyph_h = self.font.glyph_height;
        let margin_px = self.top_margin * glyph_h;
        let scroll_h = glyph_h * lines;
        let scroll_start = margin_px + scroll_h;
        let scroll_end = fb.height();

        if scroll_start >= scroll_end {
            return;
        }

        let pitch = fb.pitch();

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let src = fb.iomem().virt_ptr().add((scroll_start * pitch) as usize);
            let dst = fb
                .iomem()
                .virt_ptr()
                .add(margin_px as usize * pitch as usize);
            let count = ((scroll_end - scroll_start) * pitch) as usize;
            core::ptr::copy(src, dst, count);

            let clear_start = (scroll_end - scroll_h) * pitch;
            let clear_size = (scroll_h * pitch) as usize;
            core::ptr::write_bytes(
                fb.iomem().virt_ptr().add(clear_start as usize),
                0u8,
                clear_size,
            );
        }

        self.cursor_y -= lines;
        if self.cursor_y < self.top_margin {
            self.cursor_y = self.top_margin;
        }
    }

    pub fn write_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.putchar(ch);
        }
    }
}

impl core::fmt::Write for GfxConsole {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_str(s);
        Ok(())
    }
}
