//! 内存分配器 (TCB) — `FrameAlloc` / `SlabAlloc` trait
//!
//! 策略注入点: functions 层通过 trait 分配内存，
//! 而不直接依赖 Buddy / Slab 具体实现。

pub mod frame_alloc;
pub mod slab_alloc;
