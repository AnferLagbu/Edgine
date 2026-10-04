//! Edgine host-tests 库
//!
//! ## 职责划分 (按 Rust 官方测试组织标准)
//!
//! 本 crate 承载两类内容:
//!
//! ### 1. 库代码 (供 bin/ 和 tests/ 引用)
//! - `framekernel_bench` — 微基准测试库, 供 `bin/framekernel_bench` 调用
//! - `fsx`              — 文件系统一致性测试工具
//!
//! ### 2. 单元测试 (内联在各源文件 `#[cfg(test)] mod tests`)
//! - `dma_stream`  — DMA 状态机 (改引内核 dma_buf)
//!
//! ## B08-12/B08-14 迁移 (2026-09-06)
//! 原平行实现 unkfs/(19 文件) + unkfs_mock/ 虚拟内核树已删除 — host-tests 经
//! `edgine = { path = "../src/rust", features = ["host-test"] }` 直接引用内核
//! 真实源码 (functions/privileged host-test 暴露面). 详见 docs/plan/
//! eliminate-parallel-implementations.md.
//!
//! ## H-04 迁移 (2026-09-09)
//! `buddy` 平行实现已删除 — buddy 测试移至集成测试 `tests/pmm_buddy_host_test.rs`,
//! 经 `MetaStore` 载体注入 `VecMetaStore` 直接驱动内核真实 `privileged::mm::pmm`.
//!
//! ## 集成测试 (Cargo 自动发现)
//! `tests/` 目录下的每个 `.rs` 文件被 Cargo 视为独立测试二进制, 不在
//! `src/lib.rs` 中声明, 避免双重编译.
//!
//! ## 性能基准 (binary)
//! `src/bin/framekernel_bench.rs` 调用 `framekernel_bench::run_all()` 输出 JSON.
//! `tests/` 中的 `framekernel_bench.rs` (经 [[test]] 删除后已移除) 不再作为
//! 测试入口, 单元测试仅随 lib 编译时运行一次.

#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::module_inception)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::explicit_counter_loop)]

// ── 单元测试载体模块 (内联 #[cfg(test)] mod tests) ──
mod dma_stream;

// ── 公共库代码 (供 tests/ 与 bin/ 引用) ──
pub mod framekernel_bench;
pub mod fsx;
