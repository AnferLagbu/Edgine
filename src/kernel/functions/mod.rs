//! Edgine Functions 层 — 100% safe Rust (去特权)
//!
//! **禁止** 包含任何 `unsafe` 代码。
//! 所有硬件交互通过 `kernel::privileged` 的安全 API 进行。
//!
//! ## 架构 (框内核)
//!
//! ```text
//! privileged/ (TCB, unsafe 允许)     ← 唯一 unsafe 位置
//!     ↓ 安全函数调用 (零开销)
//! functions/ (本模块)               ← 100% safe Rust
//!     ↓ 系统调用
//! 用户态                             ← Ring 3
//! ```
//!
//! ## Safe Rust 契约
//!
//! 本模块中的每个文件必须在文件头部声明:
//! ```rust
//! //! @SAFE: 本文件不含 unsafe 代码。
//! //! 所有 unsafe 操作已委托至 privileged API。
//! ```
//!
//! CI 检查: `grep -rn 'unsafe ' src/kernel/functions/` 必须输出为空。
//!
//! ## 检查脚本
//!
//! ```bash
//! tools/check_tcb.sh  # 自动检查 functions/ 中无 unsafe
//! ```

// P-71: functions 已独立为 `kernel-functions` crate (100% safe 业务层).
// host-test (std) 下剥离裸机约束, 与 kernel-privileged 同构.
#![cfg_attr(not(feature = "host-test"), no_std)]
// F1: 本 crate 全树禁止 unsafe (crate 级 deny 覆盖所有子模块).
#![deny(unsafe_code)]
// ── P-71: crate 级 lint 豁免 ──────────────────────────────────────────
// 以下豁免原以 `#![allow(...)]` 写在 glue crate 根 (src/kernel/lib.rs), 单 crate 时覆盖
// 全树; functions 独立成 crate 后须各持一份 (lint 属性不跨 crate 继承). 条目与 lib.rs
// 逐项对应 ⇒ 拆分前后豁免面完全一致, 属**迁移**而非新增抑制 (清零工程仍归 P-64).
// 不用 Cargo `[lints]` 表: 命令行 `-D clippy::pedantic` 优先级高于 manifest 表, 会让
// `[lints]` 的 allow 在 ci/audit.sh 里全部失效 (实测 1623 条误报).
// 详细理由见 lib.rs 各同名条目注释.
// rustc 家族: C 互操作命名 / FFI 类型 / 全局可变静态
#![allow(static_mut_refs)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(improper_ctypes)]
// clippy 家族
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::mut_from_ref)]
#![allow(clippy::manual_c_str_literals)]
#![allow(clippy::empty_line_after_doc_comments)]
#![allow(clippy::result_unit_err)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::module_inception)]
#![allow(clippy::new_without_default)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::single_match)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::transmute_ptr_to_ptr)]
#![allow(clippy::missing_transmute_annotations)]
#![allow(clippy::let_and_return)]
#![allow(clippy::wrong_self_convention)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_find)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::unnecessary_lazy_evaluations)]
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_unwrap_or_default)]
#![allow(clippy::manual_unwrap_or)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::manual_checked_ops)]
#![allow(clippy::question_mark)]
#![allow(clippy::manual_range_patterns)]
#![allow(clippy::manual_flatten)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::empty_loop)]
#![allow(clippy::explicit_counter_loop)]
#![allow(clippy::pointers_in_nomem_asm_block)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::pub_underscore_fields)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::missing_fields_in_debug)]

// ============================================================================
// 子系统声明
// ============================================================================

// ── P-71: crate 根适配 ────────────────────────────────────────────────
// `crate::functions::…` (768 处) 与 `crate::privileged::…` (1078 处) 均零改写:
// 自别名 + 对 kernel_privileged 的别名再导出覆盖了两种旧路径.
extern crate alloc;
pub use crate as functions;
pub use kernel_privileged as privileged;
// privileged `#[macro_export]` 宏的再导出: 本层既有 `crate::slog_*!` /
// `crate::klog_*!` / `crate::arch!` 调用点继续成立 (glue 侧另有自己的再导出清单).
pub use kernel_privileged::{
    arch, klog_boot_info, klog_err, slog_debug, slog_err, slog_info, slog_warn,
};

/// 系统调用 — POSIX + SGEG 分发 (通过 privileged::UserContext)
pub mod syscall;

/// 整数位级转换助手 (functions 层副本; 与 privileged/lib/intcast.rs 同构)
pub mod intcast;

/// TD-08: functions 层统一错误 (单一来源, SocketError/UnixSocketError 共享)
pub mod error;

/// klog 日志子系统 — sink 注册表的安全视图 (TD-09 V2 procfs 运行时管理)
///
/// 启动期注册: `klog::register_defaults()` 调用 privileged::klog 注册默认 sink.
/// 运行时查询: `klog::list_names()` / `klog::count()` / `klog::render_text()` /
///             `klog::render_json()` 为 `/proc/sys/klog/sinks` 提供数据源.
pub mod klog;

/// 进程管理 — 调度 / 进程表 / ELF 加载
pub mod proc;

/// 文件系统 — VFS + ramfs + UNKFS + devfs + procfs
pub mod fs;

/// 网络栈 — smoltcp + 驱动适配
pub mod net;

/// 进程间通信 — 管道 / 共享内存 / 消息队列 / 信号
pub mod ipc;

/// 设备驱动框架 — EGDF 协议族
pub mod egdf;

/// 设备驱动 — 网卡 / 存储 / 显示 / 输入
pub mod driver;

/// 身份与权限 — PWM / 能力矩阵 / 会话
pub mod sgeg;

/// 故障恢复 — FREG恢复
pub mod freg;

/// 同步原语高级封装 — IrqSpinLock / scoped / FREG / Once
///
/// 基础同步原语见 `privileged::sync` (TCB); 本模块提供
/// functions 层的安全抽象 (闭包 API / 一次性初始化等)。
pub mod sync;

/// 定时器子系统 — sleep / POSIX Timer / timerfd / Tickless / NTP-PTP
///
/// 底层实现见 `privileged::timer` (TCB); 本模块提供
/// functions 层的安全代理与参数验证 (0 unsafe)。
pub mod timer;

/// 内存管理 — Page Cache / Swap / mmap safe 代理
///
/// 底层实现见 `privileged::mm` (TCB); 本模块提供
/// functions 层的安全抽象与参数验证。
pub mod mm;

/// init 启动子系统 — PID 1 / initramfs / Ring 3 切换状态查询
///
/// 底层实现见 `privileged::proc::api` (TCB); 本模块提供
/// functions 层的安全状态查询 API, 不暴露 unsafe 入口。
pub mod init;

/// WASM 沙箱
pub mod wasm;

/// 内核调试 / 跟踪 — ftrace / KGDB 安全封装
///
/// 底层实现见 `privileged::debug` (TCB); 系统调用包装见 `functions::syscall::ftrace`;
/// 本模块提供 functions 层的安全抽象与系统调用包装 (0 unsafe)。
pub mod debug;

/// I/O 子系统 (C4: io_uring 异步 I/O).
///
/// 底层实现见 `privileged::io::iouring` (TCB);
/// 本模块提供 functions 层的安全抽象 (0 unsafe)。
pub mod io;

/// T6-9: 内核配置常量 (原 privileged/config/).
/// 纯常量与类型定义, 0 unsafe.
pub mod config;
/// T6-9: 用户态 CPU 寄存器快照 (原 privileged/userctx.rs)
pub mod userctx;

// ============================================================================
// 日志宏
//
// P-71: `slog*` 宏定义随 `privileged::klog::log` 一并归 TCB (kernel-privileged),
// 由 glue crate 根再导出, 本层既有 `crate::slog_*!` 调用点保持不变.
// ============================================================================

// ============================================================================
// CI 自检 (编译时)
// ============================================================================

/// 编译时断言: functions 层禁止 unsafe。
///
/// 实际检查由 `tools/check_tcb.sh` 在 CI 中完成。
/// 此常量仅用于文档目的。
pub const FUNCTIONS_SAFE_RUST: bool = true;
