//! Edgine 内核壳 crate (方案 D: kernel 独立 crate 化)
//!
//! 内核本体在 `kernel` crate (`src/kernel/Cargo.toml`)。本壳仅做 re-export:
//! - `edgine::kernel` → kernel crate (兼容 host-tests 的 `edgine::kernel::` 路径)
//! - `edgine::CpuInfo` / `edgine::LogLevel` → 常用类型直达
//!
//! 用途: host-tests 以 path 依赖本壳 (host-test feature) 引用内核真实源码;
//! 裸机内核构建直接走 kernel crate (Makefile 取 `libkernel.a`), 壳不参与。

#![cfg_attr(not(feature = "host-test"), no_std)]

/// Edgine 内核 (独立 crate, 方案 D)。
///
/// 内核本体全部逻辑 (privileged TCB + functions 业务) 位于 `kernel` crate。
pub use kernel;

// 重新导出常用类型 (兼容 `edgine::CpuInfo` / `edgine::LogLevel` 直达路径)
pub use kernel::privileged::cpu::CpuInfo;
pub use kernel::privileged::klog::LogLevel;
