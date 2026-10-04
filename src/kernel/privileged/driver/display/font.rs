use super::framebuffer::{Color, Framebuffer};

/// 内嵌的 8x16 位图字体 (256 字形, 每字形 16 字节)
const FONT8X16_DATA: &[u8] = include_bytes!("assets/font8x16.raw");

const GLYPH_WIDTH: u32 = 8;
const GLYPH_HEIGHT: u32 = 16;

pub struct Font {
    data: &'static [u8],
    pub glyph_height: u32,
    pub glyph_width: u32,
    pub glyph_count: u32,
}

impl Font {
    pub fn builtin_8x16() -> Self {
        Self {
            data: FONT8X16_DATA,
            glyph_height: GLYPH_HEIGHT,
            glyph_width: GLYPH_WIDTH,
            glyph_count: 256,
        }
    }

    /// 在帧缓冲上绘制一个字符
    ///
    /// 返回绘制后下一个光标位置 (advance width)。
    pub fn render_char(
        &self,
        fb: &mut Framebuffer,
        ch: char,
        x: u32,
        y: u32,
        fg: Color,
        bg: Color,
    ) -> u32 {
        let idx = ch as u32;
        if idx >= self.glyph_count {
            return self.glyph_width;
        }

        let glyph_offset = idx * self.glyph_height;
        let glyph_data =
            &self.data[glyph_offset as usize..(glyph_offset + self.glyph_height) as usize];

        for row in 0..self.glyph_height {
            let byte = glyph_data[row as usize];
            if byte == 0 {
                continue;
            }
            for col in 0..8u32 {
                let px = x + col;
                let py = y + row;
                if (byte >> (7 - col)) & 1 != 0 {
                    fb.set_pixel(px, py, fg);
                } else {
                    fb.set_pixel(px, py, bg);
                }
            }
        }

        self.glyph_width
    }

    /// 在帧缓冲上绘制一行文本
    ///
    /// 遇到换行符 `\n` 时自动换行并继续绘制。
    /// 超出屏幕宽度的字符会被截断。
    pub fn render_text(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        mut x: u32,
        mut y: u32,
        fg: Color,
        bg: Color,
    ) {
        let start_x = x;

        for ch in text.chars() {
            if ch == '\n' {
                x = start_x;
                y += self.glyph_height;
                continue;
            }
            let advance = self.render_char(fb, ch, x, y, fg, bg);
            x += advance;
        }
    }

    /// 带自动换行的文本绘制
    ///
    /// 当 `x + glyph_width > screen_width` 时自动换到下一行。
    pub fn render_text_wrapped(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        mut x: u32,
        mut y: u32,
        screen_width: u32,
        fg: Color,
        bg: Color,
    ) {
        let start_x = x;

        for ch in text.chars() {
            if ch == '\n' {
                x = start_x;
                y += self.glyph_height;
                continue;
            }

            if x + self.glyph_width > screen_width {
                x = start_x;
                y += self.glyph_height;
            }

            let advance = self.render_char(fb, ch, x, y, fg, bg);
            x += advance;
        }
    }
}

/// 全局默认字体
use crate::privileged::sync::OnceLock;
static DEFAULT_FONT: OnceLock<Font> = OnceLock::new();

pub fn default_font() -> &'static Font {
    DEFAULT_FONT.get_or_init(|slot| {
        slot.write(Font::builtin_8x16());
    })
}
