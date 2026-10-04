//! PS/2 键盘驱动 (Rust 安全重写)
//!
//! 提供对 PS/2 兼容键盘的完整支持：
//! - **键盘初始化**: LED 设置、扫描码集检测
//! - **Scancode 转换**: 扫描码到 ASCII 字符映射
//! - **修饰键支持**: Shift, Ctrl, Alt, Caps Lock
//! - **键盘缓冲区**: 环形缓冲区存储按键
//! - **中断处理**: IRQ1 中断服务程序
//!
//! ## 硬件接口
//!
//! ```text
//! PS/2 Controller Ports:
//! ├── 0x60: Data Port (读/写键盘数据)
//! └── 0x64: Status/Command Register
//! ```
//!
//! # Safety
//! 此模块直接操作 PS/2 控制器硬件。

use crate::framework::driver::{DeviceInfo, DeviceType, Driver, DriverError, DriverResult};
use crate::framework::ioport::IoPort;
use crate::framework::sync::IrqSpinLock as Mutex;
use alloc::boxed::Box;
// ============================================================================
// 硬件常量定义
// ============================================================================

/// PS/2 数据端口
const PS2_DATA_PORT: u16 = 0x60;
/// PS/2 状态/命令端口
const PS2_CMD_PORT: u16 = 0x64;

/// 状态寄存器标志位
const PS2_STATUS_OUTPUT_FULL: u8 = 0x01; // 输出缓冲区满
const PS2_STATUS_INPUT_FULL: u8 = 0x02; // 输入缓冲区满
/// 键盘命令
const KB_CMD_SET_LED: u8 = 0xED; // 设置 LED
/// 查询/设置扫描码集 (0xF0 后跟 0=查询, 1=Set 1, 2=Set 2, 3=Set 3)
const KB_CMD_SCANCODE: u8 = 0xF0;

/// LED 标志位
const KB_LED_SCROLL_LOCK: u8 = 0x01;
pub(crate) const KB_LED_NUM_LOCK: u8 = 0x02;
pub(crate) const KB_LED_CAPS_LOCK: u8 = 0x04;

/// 缓冲区大小
const KEYBOARD_BUFFER_SIZE: usize = 128;

// ============================================================================
// Scancode 转换表
// ============================================================================

/// 标准 US QWERTY 键盘 Scancode Set 1 映射表
/// [scancode] -> ASCII 字符 (无修饰键)
pub(crate) const SCANCODE_TABLE: &[u8; 87] = &[
    0x00, 0x1B, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 0x08, 0x09,
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', 0x0D, 0x00, b'a', b's',
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0x00, b'\\', b'z', b'x', b'c',
    b'v', b'b', b'n', b'm', b',', b'.', b'/', 0x00, b'*', 0x00, b' ', 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7F, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Shift 修饰键下的字符映射表
pub(crate) const SHIFT_TABLE: &[u8; 87] = &[
    0x00, 0x1B, b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'_', b'+', 0x08, 0x09,
    b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'{', b'}', 0x0D, 0x00, b'A', b'S',
    b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', b'~', 0x00, b'|', b'Z', b'X', b'C', b'V',
    b'B', b'N', b'M', b'<', b'>', b'?', 0x00, b'*', 0x00, b' ', 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7F, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

// ============================================================================
// 特殊按键定义
// ============================================================================

/// 特殊按键枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialKey {
    /// 无效按键
    None,
    /// Enter 回车
    Enter,
    /// Tab 制表符
    Tab,
    /// Backspace 退格
    Backspace,
    /// Space 空格
    Space,
    /// Escape 退出
    Escape,
    /// Delete 删除
    Delete,
    /// Insert 插入
    Insert,
    /// Home 首行
    Home,
    /// End 尾行
    End,
    /// `PageUp` 上翻页
    PageUp,
    /// `PageDown` 下翻页
    PageDown,
    /// 上箭头
    ArrowUp,
    /// 下箭头
    ArrowDown,
    /// 左箭头
    ArrowLeft,
    /// 右箭头
    ArrowRight,
    /// F1-F12 功能键
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// 特殊按键 scancode 映射
pub(crate) fn get_special_key(scancode: u8) -> SpecialKey {
    match scancode {
        0x0D => SpecialKey::Enter,
        0x0F => SpecialKey::Tab,
        0x0E => SpecialKey::Backspace,
        0x39 => SpecialKey::Space,
        0x01 => SpecialKey::Escape,
        0x53 => SpecialKey::Delete,
        0x52 => SpecialKey::Insert,
        0x47 => SpecialKey::Home,
        0x4F => SpecialKey::End,
        0x49 => SpecialKey::PageUp,
        0x51 => SpecialKey::PageDown,
        0x48 => SpecialKey::ArrowUp,
        0x50 => SpecialKey::ArrowDown,
        0x4B => SpecialKey::ArrowLeft,
        0x4D => SpecialKey::ArrowRight,
        0x3B => SpecialKey::F1,
        0x3C => SpecialKey::F2,
        0x3D => SpecialKey::F3,
        0x3E => SpecialKey::F4,
        0x3F => SpecialKey::F5,
        0x40 => SpecialKey::F6,
        0x41 => SpecialKey::F7,
        0x42 => SpecialKey::F8,
        0x43 => SpecialKey::F9,
        0x44 => SpecialKey::F10,
        0x57 => SpecialKey::F11,
        0x58 => SpecialKey::F12,
        _ => SpecialKey::None,
    }
}

// ============================================================================
// 键盘状态结构体
// ============================================================================

/// 键盘修饰键状态
#[derive(Debug, Clone, Copy)]
pub struct ModifierState {
    pub left_shift: bool,
    pub right_shift: bool,
    pub left_ctrl: bool,
    pub right_ctrl: bool,
    pub left_alt: bool,
    pub right_alt: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub scroll_lock: bool,
}

impl Default for ModifierState {
    fn default() -> Self {
        Self {
            left_shift: false,
            right_shift: false,
            left_ctrl: false,
            right_ctrl: false,
            left_alt: false,
            right_alt: false,
            caps_lock: false,
            num_lock: true, // 默认开启数字锁定
            scroll_lock: false,
        }
    }
}

impl ModifierState {
    /// 检查是否有 Shift 键按下
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn shift_pressed(&self) -> bool {
        self.left_shift || self.right_shift
    }

    /// 检查是否有 Ctrl 键按下
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn ctrl_pressed(&self) -> bool {
        self.left_ctrl || self.right_ctrl
    }

    /// 检查是否有 Alt 键按下
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn alt_pressed(&self) -> bool {
        self.left_alt || self.right_alt
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 计算 LED 状态字节
    pub fn to_led_byte(&self) -> u8 {
        let mut led: u8 = 0;
        if self.scroll_lock {
            led |= KB_LED_SCROLL_LOCK;
        }
        if self.num_lock {
            led |= KB_LED_NUM_LOCK;
        }
        if self.caps_lock {
            led |= KB_LED_CAPS_LOCK;
        }
        led
    }
}

/// 环形键盘缓冲区
pub(crate) struct KeyboardBuffer {
    buffer: [u8; KEYBOARD_BUFFER_SIZE],
    head: usize,
    tail: usize,
    count: usize,
}

impl Default for KeyboardBuffer {
    fn default() -> Self {
        Self {
            buffer: [0u8; KEYBOARD_BUFFER_SIZE],
            head: 0,
            tail: 0,
            count: 0,
        }
    }
}

impl KeyboardBuffer {
    pub(crate) fn push(&mut self, byte: u8) -> DriverResult<()> {
        if self.count >= KEYBOARD_BUFFER_SIZE {
            return Err(DriverError::Busy);
        }

        self.buffer[self.tail] = byte;
        self.tail = (self.tail + 1) % KEYBOARD_BUFFER_SIZE;
        self.count += 1;
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<u8> {
        if self.count == 0 {
            return None;
        }

        let byte = self.buffer[self.head];
        self.head = (self.head + 1) % KEYBOARD_BUFFER_SIZE;
        self.count -= 1;
        Some(byte)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }

    fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.count = 0;
    }
}

/// PS/2 键盘驱动器
pub struct KeyboardDriver {
    /// 修饰键状态
    modifiers: ModifierState,
    /// 键盘缓冲区
    buffer: KeyboardBuffer,
    /// 设备信息
    info: DeviceInfo,
    /// 是否已初始化
    initialized: bool,
    /// PS/2 数据端口 (0x60)
    data_port: IoPort,
    /// PS/2 命令端口 (0x64)
    cmd_port: IoPort,
}

// ============================================================================
// 底层辅助函数
// ============================================================================

/// 等待输入缓冲区为空
fn wait_input_buffer_empty(cmd_port: &IoPort) {
    while cmd_port.read_u8(0) & PS2_STATUS_INPUT_FULL != 0 {
        core::hint::spin_loop();
    }
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 等待输出缓冲区满
fn wait_output_buffer_full(cmd_port: &IoPort) -> bool {
    let mut timeout: u32 = 100000;

    while timeout > 0 {
        if cmd_port.read_u8(0) & PS2_STATUS_OUTPUT_FULL != 0 {
            return true;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }

    false
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 向 PS/2 控制器发送命令
fn ps2_send_command(cmd_port: &IoPort, cmd: u8) -> DriverResult<()> {
    wait_input_buffer_empty(cmd_port);
    cmd_port.write_u8(0, cmd);
    Ok(())
}

/// PS/2 控制器自检
///
/// 发送 0xAA 命令, 期望收到 0x55 表示自检通过。
fn ps2_self_test(cmd_port: &IoPort, data_port: &IoPort) -> DriverResult<()> {
    ps2_send_command(cmd_port, 0xAA)?;
    match keyboard_read_data(cmd_port, data_port) {
        Some(0x55) => Ok(()), // 自检通过
        _ => Err(DriverError::HardwareError),
    }
}

/// 键盘重置
///
/// 发送 0xFF 命令重置键盘, 期望收到 0xFA (ACK)。
fn keyboard_reset(cmd_port: &IoPort, data_port: &IoPort) -> DriverResult<()> {
    keyboard_send_data(cmd_port, data_port, 0xFF)?;
    match keyboard_read_data(cmd_port, data_port) {
        Some(0xFA) => Ok(()), // ACK
        _ => Err(DriverError::HardwareError),
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 向键盘发送数据
fn keyboard_send_data(cmd_port: &IoPort, data_port: &IoPort, data: u8) -> DriverResult<()> {
    wait_input_buffer_empty(cmd_port);
    data_port.write_u8(0, data);
    Ok(())
}

/// 从键盘读取数据
fn keyboard_read_data(cmd_port: &IoPort, data_port: &IoPort) -> Option<u8> {
    if !wait_output_buffer_full(cmd_port) {
        return None;
    }
    Some(data_port.read_u8(0))
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
/// 更新键盘 LED 状态
fn update_leds(cmd_port: &IoPort, data_port: &IoPort, modifiers: &ModifierState) {
    let _ = keyboard_send_data(cmd_port, data_port, KB_CMD_SET_LED);
    // 等待 ACK (0xFA)
    let _ = keyboard_read_data(cmd_port, data_port);
    let _ = keyboard_send_data(cmd_port, data_port, modifiers.to_led_byte());
    // 等待 ACK
    let _ = keyboard_read_data(cmd_port, data_port);
}

/// 查询当前扫描码集
///
/// 发送 0xF0 + 0x00 查询命令, 等待 ACK 后读取回复字节.
/// 回复: 1=Set 1, 2=Set 2, 3=Set 3.
fn query_scancode_set(cmd_port: &IoPort, data_port: &IoPort) -> Option<u8> {
    // 发送 0xF0 0x00 查询
    let _ = keyboard_send_data(cmd_port, data_port, KB_CMD_SCANCODE);
    let _ = keyboard_send_data(cmd_port, data_port, 0x00);
    // 等待 ACK (0xFA)
    let _ = keyboard_read_data(cmd_port, data_port);
    // 读取扫描码集回复
    keyboard_read_data(cmd_port, data_port)
}

/// 切换到指定扫描码集
///
/// 发送 0xF0 + `set_number`, 等待 ACK.
fn switch_scancode_set(cmd_port: &IoPort, data_port: &IoPort, set: u8) -> bool {
    let _ = keyboard_send_data(cmd_port, data_port, KB_CMD_SCANCODE);
    let _ = keyboard_send_data(cmd_port, data_port, set);
    // 等待 ACK (0xFA)
    matches!(keyboard_read_data(cmd_port, data_port), Some(0xFA))
}

/// 协商扫描码集: 查询当前 set, 若非 set 1 则切换
///
/// PS/2 键盘默认使用 Scancode Set 2. QEMU/Bochs 的 PS/2 控制器
/// 自动做 set 2 → set 1 转换, 但某些控制器 (尤其是真实硬件) 不做此转换.
/// 本函数确保键盘使用 Set 1, 若切换失败则打印警告 (由调用方决定是否报错).
fn negotiate_scancode_set(cmd_port: &IoPort, data_port: &IoPort) {
    if let Some(current) = query_scancode_set(cmd_port, data_port) {
        match current {
            1 => {
                // 已是 Set 1, 无需切换
            }
            2 => {
                // Set 2, 尝试切换到 Set 1
                if switch_scancode_set(cmd_port, data_port, 1) {
                    // 切换成功
                } else {
                    crate::klog_warn!(
                        Driver,
                        "keyboard: Set 2→1 切换失败, 使用 Set 1 映射 (可能产生错误字符)"
                    );
                }
            }
            3 => {
                // Set 3, 尝试切换到 Set 1
                let _ = switch_scancode_set(cmd_port, data_port, 1);
                crate::klog_warn!(Driver, "keyboard: Set 3 键盘, 已尝试切换到 Set 1");
            }
            _ => {
                crate::klog_warn!(Driver, "keyboard: 未知扫描码集 {}, 假设 Set 1", current);
            }
        }
    } else {
        // 无法查询, 假设 Set 1 (QEMU 兼容路径)
    }
}

// ============================================================================
// Driver Trait 实现
// ============================================================================

// SAFETY: 单核内核, 键盘操作无并发
unsafe impl Send for KeyboardDriver {}
unsafe impl Sync for KeyboardDriver {}

impl Driver for KeyboardDriver {
    fn name(&self) -> &'static str {
        "PS/2 Keyboard"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Input
    }

    fn init(&mut self) -> DriverResult<()> {
        // 1. 清空输出缓冲区
        let _ = keyboard_read_data(&self.cmd_port, &self.data_port);

        // 2. PS/2 控制器自检
        if ps2_self_test(&self.cmd_port, &self.data_port).is_err() {
            // 自检失败, 尝试重置
            let _ = keyboard_reset(&self.cmd_port, &self.data_port);
            // 重置后再次自检
            if ps2_self_test(&self.cmd_port, &self.data_port).is_err() {
                return Err(DriverError::HardwareError);
            }
        }

        // 3. 协商扫描码集: 查询当前 set, 若非 set 1 则切换
        //    QEMU/Bochs 自动做 set 2→1 转换, 真实硬件可能需要此步
        negotiate_scancode_set(&self.cmd_port, &self.data_port);

        // 4. 发送 SET LED 命令设置初始 LED 状态
        update_leds(&self.cmd_port, &self.data_port, &self.modifiers);

        // 5. 清空缓冲区
        self.buffer.clear();

        self.initialized = true;
        Ok(())
    }

    fn shutdown(&mut self) -> DriverResult<()> {
        self.initialized = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.initialized
    }

    fn status(&self) -> &'static str {
        if self.initialized {
            "Ready"
        } else {
            "Not initialized"
        }
    }
}

// ============================================================================
// 公共 API
// ============================================================================

impl KeyboardDriver {
    /// 创建新的键盘驱动实例
    /// # Panics
    /// PS/2 数据端口或命令端口初始化失败时 panic。
    pub fn new() -> Self {
        // SAFETY: PS/2 数据端口 (0x60) 和命令端口 (0x64) 是标准硬件端口,
        // 由 PC 枚举确定, 不与其他 IoPort 实例重叠.
        let data_port = unsafe { IoPort::new(PS2_DATA_PORT, 1, "ps2-data") }
            .expect("ps2-data port init failed");
        let cmd_port =
            unsafe { IoPort::new(PS2_CMD_PORT, 1, "ps2-cmd") }.expect("ps2-cmd port init failed");
        Self {
            modifiers: ModifierState::default(),
            buffer: KeyboardBuffer::default(),
            info: DeviceInfo::new("ps2_keyboard", DeviceType::Input),
            initialized: false,
            data_port,
            cmd_port,
        }
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 处理 IRQ1 键盘中断
    ///
    /// 从 PS/2 数据端口读取 scancode 并转换后存入缓冲区。
    ///
    /// # Returns
    /// * `Some(u8)` - 成功读取并转换的 ASCII 字符
    /// * `None` - 无有效数据或特殊按键
    pub fn handle_interrupt(&mut self) -> Option<u8> {
        // 读取 scancode
        let scancode = match keyboard_read_data(&self.cmd_port, &self.data_port) {
            Some(s) => s,
            None => return None,
        };

        // 检测释放码 (0xE0 或 0xE1 前缀)
        if scancode == 0xE0 || scancode == 0xE1 {
            return None; // 忽略扩展 scancode 前缀
        }

        // 检测按键释放 (bit 7 = 1 表示释放)
        let pressed = (scancode & 0x80) == 0;
        let key_code = scancode & 0x7F;

        // 处理修饰键
        match key_code {
            0x2A | 0x36 => {
                // 左/右 Shift 键
                if pressed {
                    if key_code == 0x2A {
                        self.modifiers.left_shift = true;
                    } else {
                        self.modifiers.right_shift = true;
                    }
                } else {
                    if key_code == 0x2A {
                        self.modifiers.left_shift = false;
                    } else {
                        self.modifiers.right_shift = false;
                    }
                }
                return None;
            }
            0x1D => {
                // Left Ctrl
                self.modifiers.left_ctrl = pressed;
                return None;
            }
            0x38 => {
                // Left Alt
                self.modifiers.left_alt = pressed;
                return None;
            }
            0x3A => {
                // Caps Lock
                if pressed {
                    self.modifiers.caps_lock = !self.modifiers.caps_lock;
                    update_leds(&self.cmd_port, &self.data_port, &self.modifiers);
                }
                return None;
            }
            0x45 => {
                // Num Lock
                if pressed {
                    self.modifiers.num_lock = !self.modifiers.num_lock;
                    update_leds(&self.cmd_port, &self.data_port, &self.modifiers);
                }
                return None;
            }
            0x46 => {
                // Scroll Lock
                if pressed {
                    self.modifiers.scroll_lock = !self.modifiers.scroll_lock;
                    update_leds(&self.cmd_port, &self.data_port, &self.modifiers);
                }
                return None;
            }
            _ => {}
        }

        // 只处理按键按下事件
        if !pressed {
            return None;
        }

        // 检查是否为特殊按键
        let special = get_special_key(key_code);
        if special != SpecialKey::None {
            // 特殊按键处理 (如 F1-F12, 方向键等)
            // 当前仅返回 None, 可扩展为特殊按键事件
            return None;
        }

        // 转换为 ASCII
        let ascii = if self.modifiers.shift_pressed() ^ self.modifiers.caps_lock {
            SHIFT_TABLE[key_code as usize]
        } else {
            SCANCODE_TABLE[key_code as usize]
        };

        // 存入缓冲区
        if ascii != 0x00 {
            let _ = self.buffer.push(ascii);
            Some(ascii)
        } else {
            None
        }
    }

    /// 读取一个字符 (非阻塞)
    ///
    /// # Returns
    /// * `Some(u8)` - 缓冲区中的字符
    /// * `None` - 缓冲区为空
    pub fn read_char(&mut self) -> Option<u8> {
        self.buffer.pop()
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 读取一行文本 (阻塞直到遇到 Enter)
    ///
    /// # Arguments
    /// * `buffer` - 输出缓冲区
    /// * `max_len` - 最大长度
    ///
    /// # Returns
    /// * `Ok(usize)` - 实际读取的字符数 (不含换行符)
    /// * `Err(DriverError)` - 错误
    /// # Errors
    /// 底层字符读取失败时返回 Err。
    pub fn read_line(&mut self, buffer: &mut [u8], max_len: usize) -> DriverResult<usize> {
        let mut count: usize = 0;

        loop {
            if let Some(ch) = self.read_char() {
                match ch {
                    b'\n' | b'\r' => {
                        break;
                    }
                    0x08 => {
                        // Backspace
                        count = count.saturating_sub(1);
                    }
                    _ if count < max_len => {
                        buffer[count] = ch;
                        count += 1;
                    }
                    _ => {} // 缓冲区满，忽略
                }
            }

            // 让出 CPU (避免忙等待)
            #[cfg(not(feature = "kernel_test"))]
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                // SAFETY: scheduler_yield_ex 由框架调度器提供, 进程上下文安全调用
                unsafe extern "C" {
                    fn scheduler_yield_ex();
                }
                scheduler_yield_ex();
            }
        }

        Ok(count)
    }

    /// 检查缓冲区是否为空
    pub fn is_buffer_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// 获取缓冲区中的字符数
    pub fn buffer_length(&self) -> usize {
        self.buffer.len()
    }

    /// 获取当前修饰键状态
    pub fn get_modifiers(&self) -> &ModifierState {
        &self.modifiers
    }

    /// 清空键盘缓冲区
    pub fn clear_buffer(&mut self) {
        self.buffer.clear();
    }

    /// 获取设备信息
    pub fn get_info(&self) -> &DeviceInfo {
        &self.info
    }
}

// ============================================================================
// FFI 兼容接口
// ============================================================================

/// 全局键盘驱动实例 (无 unsafe, Mutex 保护)
static KEYBOARD_DEVICE: Mutex<Option<Box<KeyboardDriver>>> = Mutex::new(None);

/// 初始化键盘 (C 兼容接口)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
pub extern "C" fn keyboard_init() {
    let mut driver = Box::new(KeyboardDriver::new());
    let _ = driver.init();

    let raw_ptr: *mut KeyboardDriver = &mut *driver;
    let _id = crate::framework::egdf::egdf_register_with_ops(
        "ps2_keyboard",
        crate::framework::egdf::EGDFProto::Input,
        None,
        Some(1),
        raw_ptr as *mut u8,
        crate::framework::egdf::EGDFOps::Input(&PS2_KEYBOARD_INPUT_OPS),
    );

    *KEYBOARD_DEVICE.lock() = Some(driver);
}

/// 处理键盘中断 (C 兼容接口)
/// IRQ 上下文使用 `try_lock` 避免与主代码路径死锁
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn keyboard_irq_handler() {
    if let Some(mut guard) = KEYBOARD_DEVICE.try_lock() {
        if let Some(ref mut driver) = *guard {
            driver.handle_interrupt();
        }
    }
}

/// 读取字符 (C 兼容接口) — 委托到 EGDF 统一输入路径
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn keyboard_read_char() -> i32 {
    crate::framework::egdf::egdf_input_read().map_or(-1, i32::from)
}

/// 检查是否有可读字符 (C 兼容接口) — 委托到 EGDF 统一输入路径
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn keyboard_has_char() -> i32 {
    i32::from(crate::framework::egdf::egdf_input_has_data())
}

/// C 兼容别名: `keyboard_has_data` (旧C代码/FFI调用的名称)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn keyboard_has_data() -> bool {
    keyboard_has_char() != 0
}

/// C 兼容别名: `keyboard_get_char`
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn keyboard_get_char() -> i32 {
    keyboard_read_char()
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scancode_table_basic() {
        assert_eq!(SCANCODE_TABLE[0x02], b'1');
        assert_eq!(SCANCODE_TABLE[0x03], b'2');
        assert_eq!(SCANCODE_TABLE[0x1E], b'a');
        assert_eq!(SCANCODE_TABLE[0x30], b'b');
        assert_eq!(SCANCODE_TABLE[0x39], b' ');
    }

    #[test]
    fn test_shift_table_basic() {
        assert_eq!(SHIFT_TABLE[0x02], b'!');
        assert_eq!(SHIFT_TABLE[0x03], b'@');
        assert_eq!(SHIFT_TABLE[0x1E], b'A');
        assert_eq!(SHIFT_TABLE[0x30], b'B');
    }

    #[test]
    fn test_special_keys() {
        assert_eq!(get_special_key(0x0D), SpecialKey::Enter);
        assert_eq!(get_special_key(0x0E), SpecialKey::Backspace);
        assert_eq!(get_special_key(0x48), SpecialKey::ArrowUp);
        assert_eq!(get_special_key(0x4B), SpecialKey::ArrowLeft);
        assert_eq!(get_special_key(0x57), SpecialKey::F11);

        // 无效 scancode
        assert_eq!(get_special_key(0xFF), SpecialKey::None);
    }

    #[test]
    fn test_modifier_state_default() {
        let mods = ModifierState::default();

        assert!(!mods.shift_pressed());
        assert!(!mods.ctrl_pressed());
        assert!(!mods.alt_pressed());
        assert!(!mods.caps_lock);
        assert!(mods.num_lock); // 默认开启
    }

    #[test]
    fn test_modifier_state_operations() {
        let mut mods = ModifierState::default();

        // 测试 Shift
        mods.left_shift = true;
        assert!(mods.shift_pressed());
        mods.right_shift = true;
        assert!(mods.shift_pressed());
        mods.left_shift = false;
        assert!(mods.shift_pressed()); // 右 Shift 仍按下

        // 测试 Caps Lock
        mods.caps_lock = true;
        assert!(mods.caps_lock);

        // LED 字节计算
        let led = mods.to_led_byte();
        assert!(led & KB_LED_CAPS_LOCK != 0);
        assert!(led & KB_LED_NUM_LOCK != 0);
    }

    #[test]
    fn test_keyboard_buffer() {
        let mut buf = KeyboardBuffer::default();

        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);

        // 写入数据
        assert!(buf.push(b'A').is_ok());
        assert!(buf.push(b'B').is_ok());
        assert!(!buf.is_empty());
        assert_eq!(buf.len(), 2);

        // 读取数据
        assert_eq!(buf.pop(), Some(b'A'));
        assert_eq!(buf.pop(), Some(b'B'));
        assert!(buf.is_empty());
        assert_eq!(buf.pop(), None);
    }

    #[test]
    fn test_driver_trait_meta() {
        // UT-07 (2026-09-26): 注册侧 driver::keyboard::driver_trait 的纯逻辑断言迁入 —
        // 只验证 trait 元数据表面; `init()` 走端口 I/O (host 下 SIGSEGV) 故不调用.
        let driver = KeyboardDriver::new();
        assert_eq!(driver.name(), "PS/2 Keyboard");
        assert_eq!(driver.device_type(), DeviceType::Input);
        assert!(!driver.is_ready());
        assert!(!driver.status().is_empty());
    }

    #[test]
    fn test_error_codes() {
        let err = DriverError::Busy;
        assert_eq!(err.to_string(), "Device busy");
    }
}

// ============================================================================
// InputOps 桥接 — 供 EGDF 统一输入设备 I/O
// ============================================================================

use crate::framework::egdf::InputOps;

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
extern "C" fn kb_input_read(driver_data: *mut u8) -> *const u8 {
    if driver_data.is_null() {
        return core::ptr::null();
    }
    // SAFETY: driver_data 由 EGDF InputOps 契约保证有效。
    let kb = unsafe { &mut *(driver_data as *mut KeyboardDriver) };
    kb.read_char().map_or(core::ptr::null(), |b| {
        // 使用原子槽位存放返回值, 调用方在返回后立即拷贝
        KB_READ_SLOT.store(b, Ordering::Relaxed);
        &KB_READ_SLOT as *const AtomicU8 as *const u8
    })
}

/// 临时存放 `read_char` 返回值的槽位 (原子操作保证线程安全)
use core::sync::atomic::{AtomicU8, Ordering};
static KB_READ_SLOT: AtomicU8 = AtomicU8::new(0);

#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
extern "C" fn kb_input_has(driver_data: *mut u8) -> bool {
    if driver_data.is_null() {
        return false;
    }
    // SAFETY: 同上。
    let kb = unsafe { &*(driver_data as *const KeyboardDriver) };
    !kb.is_buffer_empty()
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
extern "C" fn kb_input_irq(driver_data: *mut u8) {
    if driver_data.is_null() {
        return;
    }
    // SAFETY: driver_data 由 EGDF InputOps 契约保证有效。
    let kb = unsafe { &mut *(driver_data as *mut KeyboardDriver) };
    kb.handle_interrupt();
}

pub static PS2_KEYBOARD_INPUT_OPS: InputOps = InputOps {
    read_char: kb_input_read,
    has_char: kb_input_has,
    handle_irq: kb_input_irq,
};
