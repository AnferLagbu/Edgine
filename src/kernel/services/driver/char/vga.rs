#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! VGA 文本模式驱动 — services 层安全代理 (Phase 2.1.5)
//!
//! 封装 VGA 文本模式的 MMIO (0xB8000 显存) + PIO (0x3D4/0x3D5 CRT 控制器) 操作,
//! 通过 `framework::IoMem` + `framework::IoPort` 提供 100% safe API。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 内部 `IoMem` + `IoPort` 由 TCB 抽象, services 层只调用 safe 方法
//! - **类型安全**: 颜色用枚举, 位置用 (x, y) 范围限定
//! - **薄包装**: 仅暴露文本模式常用操作, 不重复 VGA 全部功能 (边框/绘制由 `DisplayOps` 扩展)
//! - **可替代**: 原 `kernel/driver/char/vga.rs` 仍存在, 本文件是迁移目标
//!
//! ## 硬件接口
//!
//! ```text
//! MMIO: 0xB8000 开始的 80*25*2 字节显存
//! PIO:  0x3D4 (CRT Index) + 0x3D5 (CRT Data)
//! ```
//!
//! ## 迁移状态
//!
//! - ✅ 显存读写 (`write_char` / `read_char` / clear / scroll)
//! - ✅ 颜色属性 (Color 枚举 + `TextAttribute`)
//! - ✅ 光标位置 (`set_cursor` / cursor)
//! - ✅ 光标显隐 (`enable_cursor` / `disable_cursor`) — `x86_64` only
//!
//! 评估日期: 2026-06-04
//! Phase 2.1.5 任务: 字符设备 / 显示设备迁移

use crate::framework::iomem::IoMem;
use crate::framework::ioport::IoPort;
use crate::framework::mm::PhysAddr;

// ── 硬件常量 ──

/// VGA 文本模式显存起始物理地址
pub const VGA_BUFFER_ADDR: u64 = 0xB8000;
/// 显存大小 (字节) = 80 * 25 * 2
pub const VGA_BUFFER_SIZE: usize = 80 * 25 * 2;
/// CRT 控制器 Index 端口
pub const VGA_CRT_INDEX: u16 = 0x3D4;
/// CRT 控制器 Data 端口
pub const VGA_CRT_DATA: u16 = 0x3D5;
/// CRT 端口数 (Index + Data)
pub const VGA_CRT_PORT_COUNT: u16 = 2;
/// 光标位置低字节寄存器
pub const VGA_REG_CURSOR_LO: u8 = 0x0F;
/// 光标位置高字节寄存器
pub const VGA_REG_CURSOR_HI: u8 = 0x0E;
/// 光标起始寄存器
pub const VGA_REG_CURSOR_START: u8 = 0x0A;

// ── 屏幕尺寸 ──

pub const SCREEN_WIDTH: usize = 80;
pub const SCREEN_HEIGHT: usize = 25;
pub const SCREEN_CELLS: usize = SCREEN_WIDTH * SCREEN_HEIGHT;

// ============================================================================
// 颜色 / 属性
// ============================================================================

/// VGA 16 色调色板
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGrey = 7,
    DarkGrey = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    LightMagenta = 13,
    Yellow = 14,
    White = 15,
}

impl Color {
    /// 从 u8 安全转换 (无效值返回 None)
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Black),
            1 => Some(Self::Blue),
            2 => Some(Self::Green),
            3 => Some(Self::Cyan),
            4 => Some(Self::Red),
            5 => Some(Self::Magenta),
            6 => Some(Self::Brown),
            7 => Some(Self::LightGrey),
            8 => Some(Self::DarkGrey),
            9 => Some(Self::LightBlue),
            10 => Some(Self::LightGreen),
            11 => Some(Self::LightCyan),
            12 => Some(Self::LightRed),
            13 => Some(Self::LightMagenta),
            14 => Some(Self::Yellow),
            15 => Some(Self::White),
            _ => None,
        }
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::LightGrey
    }
}

/// VGA 文本属性 (前景色 + 背景色 + 闪烁)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextAttribute {
    pub foreground: Color,
    pub background: Color,
    pub blink: bool,
}

impl TextAttribute {
    /// 创建新属性
    pub const fn new(foreground: Color, background: Color) -> Self {
        Self {
            foreground,
            background,
            blink: false,
        }
    }

    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    /// 设置闪烁位
    pub const fn with_blink(mut self) -> Self {
        self.blink = true;
        self
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 编码为 u8 (VGA 显存格式)
    pub const fn as_u8(&self) -> u8 {
        let fg = self.foreground as u8;
        let bg = (self.background as u8) << 4;
        let blink = if self.blink { 0x80 } else { 0 };
        fg | bg | blink
    }
}

impl Default for TextAttribute {
    fn default() -> Self {
        Self::new(Color::LightGrey, Color::Black)
    }
}

/// VGA 文本单元 (字符 + 属性)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VgaCell {
    pub character: u8,
    pub attribute: u8,
}

impl VgaCell {
    /// 创建新单元
    pub const fn new(ch: u8, attr: u8) -> Self {
        Self {
            character: ch,
            attribute: attr,
        }
    }

    /// 从 `TextAttribute` 创建
    pub const fn from_attr(ch: u8, attr: TextAttribute) -> Self {
        Self {
            character: ch,
            attribute: attr.as_u8(),
        }
    }

    /// 编码为 u16 (VGA 显存格式: 高字节属性, 低字节字符)
    pub const fn to_u16(self) -> u16 {
        ((self.attribute as u16) << 8) | (self.character as u16)
    }

    /// 从 u16 解码
    pub const fn from_u16(v: u16) -> Self {
        Self {
            character: (v & 0xFF) as u8,
            attribute: ((v >> 8) & 0xFF) as u8,
        }
    }
}

// ============================================================================
// 光标位置
// ============================================================================

/// 光标坐标 (列, 行)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub x: usize,
    pub y: usize,
}

impl CursorPos {
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    /// 钳制到屏幕范围内
    pub fn clamp(self) -> Self {
        Self {
            x: self.x.min(SCREEN_WIDTH - 1),
            y: self.y.min(SCREEN_HEIGHT - 1),
        }
    }

    /// 转换为线性偏移
    pub const fn offset(self) -> usize {
        self.y * SCREEN_WIDTH + self.x
    }
}

// ============================================================================
// VGA 安全驱动
// ============================================================================

/// VGA 文本模式驱动的 services 层安全代理。
///
/// 内部封装:
/// - `IoMem` 包装 0xB8000 显存 (80*25*2 = 4000 字节)
/// - `IoPort` 包装 0x3D4/0x3D5 CRT 控制器 (2 端口)
///
/// # Safety
///
/// 本结构体的所有方法都是 100% safe Rust。
/// 内部 `IoMem` + `IoPort` 由 TCB 保证:
/// - 别名检测: 同一物理区域不重复映射
/// - 边界检查: 所有 MMIO/PIO 访问做范围校验
/// - volatile 语义: 防止编译器重排 MMIO/PIO
pub struct VgaConsole {
    buffer: IoMem,
    /// CRT 控制器端口 (仅 `x86_64`, aarch64 无 PIO)
    #[cfg(target_arch = "x86_64")]
    crt: IoPort,
}

impl VgaConsole {
    /// 创建 VGA 控制台实例。
    ///
    /// # 返回
    /// - `Some(VgaConsole)`: 初始化成功
    /// - `None`: 端口/内存注册失败 (重名 / 容量满)
    pub fn new() -> Option<Self> {
        let buffer = IoMem::from_pci_bar(
            PhysAddr::new(VGA_BUFFER_ADDR),
            VGA_BUFFER_SIZE,
            "vga-buffer",
        )
        .ok()?;

        // SAFETY: 0x3D4-0x3D5 是 x86 标准 VGA CRT 控制器端口
        #[cfg(target_arch = "x86_64")]
        let crt = IoPort::new_safe(VGA_CRT_INDEX, VGA_CRT_PORT_COUNT, "vga-crt").ok();

        #[cfg(target_arch = "aarch64")]
        let crt: Option<IoPort> = None;

        // aarch64 无 CRT, 抑制 unused 警告
        #[cfg(target_arch = "aarch64")]
        let _ = crt;

        #[cfg(target_arch = "x86_64")]
        {
            Some(Self { buffer, crt: crt? })
        }
        #[cfg(target_arch = "aarch64")]
        {
            Some(Self { buffer })
        }
    }

    // ── 显存操作 ──

    /// 读指定单元 (u16 格式: 高属性低字符)
    #[inline]
    pub fn read_cell_raw(&self, pos: CursorPos) -> u16 {
        let pos = pos.clamp();
        let off = pos.offset() * 2;
        self.buffer.read_u16(off)
    }

    /// 写指定单元 (u16 格式: 高属性低字符)
    #[inline]
    pub fn write_cell_raw(&self, pos: CursorPos, raw: u16) {
        let pos = pos.clamp();
        let off = pos.offset() * 2;
        self.buffer.write_u16(off, raw);
    }

    /// 读指定单元 (结构化)
    pub fn read_cell(&self, pos: CursorPos) -> Option<VgaCell> {
        if pos.x >= SCREEN_WIDTH || pos.y >= SCREEN_HEIGHT {
            return None;
        }
        Some(VgaCell::from_u16(self.read_cell_raw(pos)))
    }

    /// 写指定单元 (结构化)
    pub fn write_cell(&self, pos: CursorPos, cell: VgaCell) {
        if pos.x >= SCREEN_WIDTH || pos.y >= SCREEN_HEIGHT {
            return;
        }
        self.write_cell_raw(pos, cell.to_u16());
    }

    /// 写单个字符 (用指定属性)
    pub fn write_char(&self, pos: CursorPos, ch: u8, attr: TextAttribute) {
        self.write_cell(pos, VgaCell::from_attr(ch, attr));
    }

    /// 清屏 (用指定属性填充空格)
    pub fn clear(&self, attr: TextAttribute) {
        let blank = VgaCell::from_attr(b' ', attr).to_u16();
        for i in 0..SCREEN_CELLS {
            self.buffer.write_u16(i * 2, blank);
        }
    }

    /// 滚动屏幕 (向上滚动一行, 底部填充空行)
    pub fn scroll_up(&self, attr: TextAttribute) {
        let blank = VgaCell::from_attr(b' ', attr).to_u16();
        // 行 1..N 复制到行 0..N-1
        let copy_count = SCREEN_WIDTH * (SCREEN_HEIGHT - 1) * 2;
        for i in 0..copy_count {
            let src = SCREEN_WIDTH * 2 + i;
            let val = self.buffer.read_u16(src);
            self.buffer.write_u16(i, val);
        }
        // 最后一行清空
        let last_row_start = (SCREEN_HEIGHT - 1) * SCREEN_WIDTH * 2;
        for i in 0..SCREEN_WIDTH {
            self.buffer.write_u16(last_row_start + i * 2, blank);
        }
    }

    /// 清除指定行 (用空格 + 指定属性填充)
    pub fn clear_row(&self, y: usize, attr: TextAttribute) {
        if y >= SCREEN_HEIGHT {
            return;
        }
        let blank = VgaCell::from_attr(b' ', attr).to_u16();
        let row_start = y * SCREEN_WIDTH;
        for i in 0..SCREEN_WIDTH {
            self.buffer.write_u16((row_start + i) * 2, blank);
        }
    }

    /// 在指定位置写入字节串 (不越界截断, 不影响光标)
    pub fn write_string_at(&self, pos: CursorPos, s: &[u8], attr: TextAttribute) {
        let pos = pos.clamp();
        for (i, &ch) in s.iter().enumerate() {
            let x = pos.x + i;
            if x >= SCREEN_WIDTH {
                break;
            }
            self.write_char(CursorPos { x, y: pos.y }, ch, attr);
        }
    }

    // ── 光标控制 (x86_64 only) ──

    /// 写硬件光标位置
    #[cfg(target_arch = "x86_64")]
    pub fn set_cursor(&self, pos: CursorPos) {
        let pos = pos.clamp();
        let offset = pos.offset() as u16;
        // 正确访问模式: 先写索引到 CRT Index, 再写数据到 CRT Data
        self.crt.write_u8(0, VGA_REG_CURSOR_LO);
        self.crt.write_u8(1, (offset & 0xFF) as u8);
        self.crt.write_u8(0, VGA_REG_CURSOR_HI);
        self.crt.write_u8(1, ((offset >> 8) & 0xFF) as u8);
    }

    /// 读硬件光标位置
    #[cfg(target_arch = "x86_64")]
    pub fn cursor(&self) -> CursorPos {
        self.crt.write_u8(0, VGA_REG_CURSOR_LO);
        let lo = u16::from(self.crt.read_u8(1));
        self.crt.write_u8(0, VGA_REG_CURSOR_HI);
        let hi = u16::from(self.crt.read_u8(1));
        let raw = ((hi << 8) | lo) as usize;
        CursorPos {
            x: raw % SCREEN_WIDTH,
            y: raw / SCREEN_WIDTH,
        }
    }

    /// 启用硬件光标
    #[cfg(target_arch = "x86_64")]
    pub fn enable_cursor(&self) {
        self.crt.write_u8(0, VGA_REG_CURSOR_START);
        let cur = self.crt.read_u8(1);
        self.crt.write_u8(0, VGA_REG_CURSOR_START);
        self.crt.write_u8(1, cur & 0xC0);
    }

    /// 禁用硬件光标
    #[cfg(target_arch = "x86_64")]
    pub fn disable_cursor(&self) {
        self.crt.write_u8(0, VGA_REG_CURSOR_START);
        self.crt.write_u8(1, 0x20);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 获取屏幕宽度
    pub const fn width(&self) -> usize {
        SCREEN_WIDTH
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 获取屏幕高度
    pub const fn height(&self) -> usize {
        SCREEN_HEIGHT
    }
}

impl Default for VgaConsole {
    fn default() -> Self {
        Self::new().expect("VGA console initialization failed")
    }
}

// ============================================================================
// EGDF Driver trait 实现 (§6.4 直接方案 B: services 权威注册)
// ============================================================================

impl crate::framework::driver::Driver for VgaConsole {
    /// 驱动名 (与 framework 原注册名保持一致: "vga")
    fn name(&self) -> &'static str {
        "vga"
    }

    fn device_type(&self) -> crate::framework::driver::DeviceType {
        crate::framework::driver::DeviceType::Char
    }

    /// `VgaConsole::new` 已映射显存, 此处保持幂等
    fn init(&mut self) -> Result<(), crate::framework::driver::DriverError> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), crate::framework::driver::DriverError> {
        Ok(())
    }
}
