#![deny(unsafe_code)]
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

// ============================================================================
// 子系统声明
// ============================================================================

/// 系统调用 — POSIX + SGEG 分发 (通过 privileged::UserContext)
pub mod syscall;

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
// Functions 层日志宏 — safe 封装, 无 unsafe 展开
//
// 用法与 privileged 层 klog_info! 等一致, 但展开后只调用
// privileged::klog::log_info 等 safe 函数, 不含任何 unsafe 块。
//
// 示例:
//   slog_info!(FS, "UNKFS 已初始化: pool={}", name);
//   slog_warn!(Kernel, "内存不足: 剩余 {} 页", free);
//   slog_err!(Driver, "未找到磁盘 {}", id);
// ============================================================================

/// Functions 层通用日志宏 — 指定级别与分类
#[macro_export]
macro_rules! slog {
    ($lvl:ident, $cat:ident, $($arg:tt)*) => {
        $crate::privileged::klog::log(
            $crate::privileged::klog::LogLevel::$lvl,
            $crate::privileged::klog::LogCategory::$cat,
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! slog_info  { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Info,  $cat, $($arg)*) }; }
#[macro_export]
macro_rules! slog_warn  { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Warn,  $cat, $($arg)*) }; }
#[macro_export]
macro_rules! slog_err   { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Error, $cat, $($arg)*) }; }
#[macro_export]
macro_rules! slog_debug { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Debug, $cat, $($arg)*) }; }
#[macro_export]
macro_rules! slog_crit  { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Crit,  $cat, $($arg)*) }; }

// ============================================================================
// CI 自检 (编译时)
// ============================================================================

/// 编译时断言: functions 层禁止 unsafe。
///
/// 实际检查由 `tools/check_tcb.sh` 在 CI 中完成。
/// 此常量仅用于文档目的。
pub const FUNCTIONS_SAFE_RUST: bool = true;
