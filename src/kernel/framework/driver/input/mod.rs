//! 输入设备驱动子系统 (Input Device Driver Subsystem)
//!
//! 提供输入设备支持：
//! - **键盘**: PS/2键盘驱动
//! - **鼠标**: PS/2鼠标驱动 (未来)
//! - **游戏手柄**: 游戏控制器 (未来)
//!
//! ## 架构
//!
//! ```text
//! Input Device Subsystem
//! ├── keyboard.rs  # 键盘驱动
//! ├── mouse.rs    # 鼠标驱动 (未来)
//! └── joystick.rs # 游戏手柄 (未来)
//! ```

#[cfg(target_arch = "x86_64")]
pub mod keyboard;

#[cfg(target_arch = "x86_64")]
pub use keyboard::KeyboardDriver;

#[cfg(target_arch = "x86_64")]
pub fn input_init() {
    // 键盘注册唯一入口: `keyboard_init()` 内经 `chitin_register_with_ops` 注册
    // `ps2_keyboard` (携带 `InputOps` + IRQ1), 并持有驱动实例供 IRQ 处理使用。
    // 原此处的二次 `chitin_register_driver("ps2_keyboard", ...)` 为历史遗留:
    // Chitin 注册表不按名去重, 重复注册会产生同名设备节点, 且对新建实例再跑一次
    // `init()` 会重复触发 PS/2 自检与扫描码协商 (硬件副作用)。
    keyboard::keyboard_init();
}

/// AArch64 输入初始化 (暂无 PS/2 设备)。
#[cfg(not(target_arch = "x86_64"))]
pub fn input_init() {}
