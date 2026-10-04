//! 自测 fixture - 故意制造各类违规, 用于验证 audit 脚本能识别
//!
//! services_boundary fixture:
//! - 行 5: 违规 - `use crate::framework::sync::raw` (FORBIDDEN)
//! - 行 7: 违规 - `pub use framework::egdf::composite` (B01-05 pub use 检测)
//! - 行 11: 裸指针解引用 (I2 检测)

#![allow(dead_code, unused)]
use spin::mutex::SpinMutex;

// I2 违规: 裸指针解引用 (在 safe 上下文中)
pub fn i2_violation(ptr: *const u32) -> u32 {
    unsafe {
        *ptr  // 应在 I2 扫描中报告
    }
}

// safe counter example
pub fn safe_counter_inc(counter: &SpinMutex<u32>) {
    let mut val = counter.lock();
    *val += 1;
}
