#![no_std]
#![no_main]

use userlib::sys::*;
use userlib::*;

const FB_USER_VADDR: u64 = 0x100000000;
const FONT_DATA: &[u8] = include_bytes!("font8x16.raw");
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 16;

#[repr(u8)]
#[derive(Clone, Copy)]
enum PixelFmt {
    RGB565 = 16,
    RGB888 = 24,
    ARGB8888 = 32,
}

/// RGB 颜色分量 (每通道 0-255)
#[derive(Clone, Copy)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

struct Terminal {
    fb: &'static mut [u8],
    width: u32,
    height: u32,
    pitch: u32,
    fmt: PixelFmt,
    cols: u32,
    rows: u32,
    cursor_col: u32,
    cursor_row: u32,
    scroll_top: u32,
    line_buf: [u8; 256],
    line_len: usize,
    prompt: &'static str,
}

impl Terminal {
    fn new(fb: &'static mut [u8], width: u32, height: u32, pitch: u32, bpp: u8) -> Self {
        let fmt = match bpp {
            16 => PixelFmt::RGB565,
            24 => PixelFmt::RGB888,
            _ => PixelFmt::ARGB8888,
        };
        let cols = width / GLYPH_W;
        let rows = height / GLYPH_H;
        Self {
            fb,
            width,
            height,
            pitch,
            fmt,
            cols,
            rows,
            cursor_col: 0,
            cursor_row: 0,
            scroll_top: 0,
            line_buf: [0u8; 256],
            line_len: 0,
            prompt: "$ ",
        }
    }

    fn put_pixel(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let off = (y * self.pitch + x * self.fmt as u32 / 8) as usize;
        let fb = &mut self.fb;
        if off + (self.fmt as u32 / 8) as usize > fb.len() {
            return;
        }
        match self.fmt {
            PixelFmt::RGB565 => {
                let v = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
                fb[off] = v as u8;
                fb[off + 1] = (v >> 8) as u8;
            }
            PixelFmt::RGB888 => {
                fb[off] = b;
                fb[off + 1] = g;
                fb[off + 2] = r;
            }
            PixelFmt::ARGB8888 => {
                fb[off] = b;
                fb[off + 1] = g;
                fb[off + 2] = r;
                fb[off + 3] = 255;
            }
        }
    }

    fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, c: Rgb) {
        let start_y = y;
        let end_y = (y + h).min(self.height);
        let start_x = x;
        let end_x = (x + w).min(self.width);
        for py in start_y..end_y {
            for px in start_x..end_x {
                self.put_pixel(px, py, c.r, c.g, c.b);
            }
        }
    }

    fn draw_glyph(&mut self, ch: u8, col: u32, row: u32, fg: Rgb, bg: Rgb) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let ox = col * GLYPH_W;
        let oy = row * GLYPH_H;
        let glyph_off = ch as usize * GLYPH_H as usize;
        if glyph_off + GLYPH_H as usize > FONT_DATA.len() {
            return;
        }
        for line in 0..GLYPH_H {
            let byte = FONT_DATA[glyph_off + line as usize];
            for bit in 0..GLYPH_W {
                let c = if (byte >> (7 - bit)) & 1 != 0 { fg } else { bg };
                self.put_pixel(ox + bit, oy + line, c.r, c.g, c.b);
            }
        }
    }

    fn clear_screen(&mut self) {
        self.fill_rect(0, 0, self.width, self.height, Rgb::new(20, 20, 28));
    }

    fn clear_line(&mut self, row: u32) {
        if row >= self.rows {
            return;
        }
        let y0 = row * GLYPH_H;
        self.fill_rect(0, y0, self.width, GLYPH_H, Rgb::new(20, 20, 28));
    }

    fn scroll_up_one(&mut self) {
        if self.rows < 2 {
            return;
        }
        let char_height_bytes = (self.pitch * GLYPH_H) as usize;
        for row in 1..self.rows {
            let src = (row * GLYPH_H * self.pitch) as usize;
            let dst = ((row - 1) * GLYPH_H * self.pitch) as usize;
            self.fb.copy_within(src..src + char_height_bytes, dst);
        }
        self.clear_line(self.rows - 1);
    }

    fn newline(&mut self) {
        self.cursor_col = 0;
        self.cursor_row += 1;
        if self.cursor_row >= self.rows {
            self.scroll_up_one();
            self.cursor_row = self.rows - 1;
        }
        self.scroll_top = 0;
    }

    fn putchar(&mut self, ch: u8, fg: Rgb, bg: Rgb) {
        match ch {
            b'\n' => self.newline(),
            b'\r' => self.cursor_col = 0,
            b'\x08' => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
                self.draw_glyph(b' ', self.cursor_col, self.cursor_row, fg, bg);
            }
            _ if ch >= b' ' => {
                if self.cursor_col >= self.cols {
                    self.newline();
                }
                self.draw_glyph(ch, self.cursor_col, self.cursor_row, fg, bg);
                self.cursor_col += 1;
            }
            _ => {}
        }
    }

    fn write_str(&mut self, s: &str, fg: Rgb, bg: Rgb) {
        for &b in s.as_bytes() {
            self.putchar(b, fg, bg);
        }
    }

    fn show_prompt(&mut self) {
        self.write_str(self.prompt, Rgb::new(100, 200, 100), Rgb::new(20, 20, 28));
    }

    fn draw_status_bar(&mut self) {
        let y0 = (self.rows - 1) * GLYPH_H;
        self.fill_rect(0, y0, self.width, GLYPH_H, Rgb::new(50, 50, 70));
        let info = " fbterm v0.2 | Edgine User-Space Terminal ";
        let mut col = 0u32;
        for &b in info.as_bytes() {
            if col < self.cols {
                self.draw_glyph(
                    b,
                    col,
                    self.rows - 1,
                    Rgb::new(180, 180, 200),
                    Rgb::new(50, 50, 70),
                );
                col += 1;
            }
        }
    }

    fn draw_screen(&mut self) {
        self.clear_screen();
        self.draw_status_bar();
        self.cursor_col = 0;
        self.cursor_row = 0;
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    userlib::print("[fbterm] PANIC: ");
    if let Some(loc) = info.location() {
        userlib::print("at ");
        userlib::print(loc.file());
        userlib::print(":");
        print_dec(loc.line() as i64);
    }
    userlib::print("\n");
    proc_exit(1);
}

#[unsafe(no_mangle)]
pub fn _start() -> ! {
    println("[fbterm] Starting user-space terminal...");

    let mut info = FbInfo {
        phys_addr: 0,
        size: 0,
        width: 0,
        height: 0,
        pitch: 0,
        bpp: 0,
        _pad: [0; 3],
    };
    if fb_open(&mut info) < 0 {
        println("[fbterm] ERROR: no framebuffer device");
        proc_exit(1);
    }

    if fb_mmap(FB_USER_VADDR, info.size, 3) < 0 {
        println("[fbterm] ERROR: fb_mmap failed");
        proc_exit(1);
    }

    let fb_slice =
        unsafe { core::slice::from_raw_parts_mut(FB_USER_VADDR as *mut u8, info.size as usize) };

    let mut term = Terminal::new(fb_slice, info.width, info.height, info.pitch, info.bpp);
    term.draw_screen();

    term.cursor_col = 0;
    term.cursor_row = 0;
    term.write_str(
        "*** Edgine fbterm v0.2 ***",
        Rgb::new(255, 255, 100),
        Rgb::new(20, 20, 28),
    );
    term.newline();
    term.write_str(
        "Keyboard-driven user-space terminal.",
        Rgb::new(180, 180, 200),
        Rgb::new(20, 20, 28),
    );
    term.newline();
    term.newline();
    term.show_prompt();

    loop {
        let mut c = 0u8;
        let n = sys::read(0, core::slice::from_mut(&mut c));
        if n <= 0 {
            sys::sched_yield();
            continue;
        }

        match c {
            b'\n' | b'\r' => {
                term.newline();
                if term.line_len > 0 {
                    let mut cmd_buf = [0u8; 256];
                    let cmd_len = term.line_len;
                    cmd_buf[..cmd_len].copy_from_slice(&term.line_buf[..cmd_len]);
                    term.write_str("> ", Rgb::new(180, 180, 200), Rgb::new(20, 20, 28));
                    for &b in &cmd_buf[..cmd_len] {
                        term.putchar(b, Rgb::new(255, 255, 255), Rgb::new(20, 20, 28));
                    }
                    term.newline();

                    if &cmd_buf[..cmd_len] == b"help" {
                        term.write_str(
                            "  help   - Show this message",
                            Rgb::new(200, 200, 220),
                            Rgb::new(20, 20, 28),
                        );
                        term.newline();
                        term.write_str(
                            "  clear  - Clear screen",
                            Rgb::new(200, 200, 220),
                            Rgb::new(20, 20, 28),
                        );
                        term.newline();
                        term.write_str(
                            "  exit   - Quit fbterm",
                            Rgb::new(200, 200, 220),
                            Rgb::new(20, 20, 28),
                        );
                        term.newline();
                        term.write_str(
                            "  colors - Show color palette (WIP)",
                            Rgb::new(200, 200, 220),
                            Rgb::new(20, 20, 28),
                        );
                        term.newline();
                    } else if &cmd_buf[..cmd_len] == b"clear" {
                        term.scroll_top = 0;
                        term.draw_screen();
                        term.cursor_row = 0;
                    } else if &cmd_buf[..cmd_len] == b"exit" {
                        term.write_str("Goodbye.", Rgb::new(255, 200, 100), Rgb::new(20, 20, 28));
                        term.newline();
                        break;
                    } else if cmd_len == 0 {
                    } else {
                        term.write_str(
                            "Unknown command. Type 'help'.",
                            Rgb::new(255, 150, 150),
                            Rgb::new(20, 20, 28),
                        );
                        term.newline();
                    }
                    term.line_len = 0;
                }
                term.show_prompt();
            }
            b'\x7f' | b'\x08' => {
                if term.line_len > 0 {
                    term.line_len -= 1;
                    if term.cursor_col > term.prompt.len() as u32 {
                        term.cursor_col -= 1;
                        term.draw_glyph(
                            b' ',
                            term.cursor_col,
                            term.cursor_row,
                            Rgb::new(255, 255, 255),
                            Rgb::new(20, 20, 28),
                        );
                    }
                }
            }
            ch if ch >= b' ' && term.line_len < 255 => {
                term.line_buf[term.line_len] = ch;
                term.line_len += 1;
                term.putchar(ch, Rgb::new(255, 255, 255), Rgb::new(20, 20, 28));
            }
            _ => {}
        }
    }

    for y in 0..term.height {
        for x in 0..term.width {
            term.put_pixel(x, y, 0, 0, 0);
        }
    }
    let _ = fb_release(FB_USER_VADDR);
    proc_exit(0);
}
