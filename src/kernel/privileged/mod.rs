#![cfg_attr(not(feature = "host-test"), no_std)]
// P-71: 模块 `lib` 与 library target 根同名, 触发 special_module_name (原单 crate
// 下嵌套于 `privileged::lib`, 无此特例). 重命名会改动 privileged::lib 公共路径,
// 故在 crate 级豁免该命名特例 lint.
#![allow(special_module_name)]
// ── P-71: crate 级 lint 豁免 ──────────────────────────────────────────
// 以下豁免原以 `#![allow(...)]` 写在 glue crate 根 (src/kernel/lib.rs), 单 crate 时覆盖
// 全树; privileged 独立成 crate 后须各持一份 (lint 属性不跨 crate 继承). 条目与 lib.rs
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
//! Edgine Framekernel — 特权 OS Framework (TCB)
//!
//! 这是整个内核中**唯一**允许包含 `unsafe` Rust 代码的模块。
//! 所有低层硬件交互 (MMU/DMA/中断/上下文切换) 在此封装为
//! 安全 API, 供 `functions/` 层在纯 safe Rust 中调用。
//!
//! ## 架构 (框内核 / Asterinas OSTD 范式)
//!
//! ```text
//! privileged/ (TCB, unsafe 允许)
//!   ├── arch/             架构特定 (GDT/IDT/APIC/MMU/GIC)
//!   ├── boot/             引导协议 (Multiboot2/UEFI/...)
//!   ├── cpu/              CPU 探测 (CPUID/MSR/TSC/缓存/拓扑)
//!   ├── mm/               物理/虚拟内存 (PMM/VMM/Slab/Kmalloc)
//!   ├── irq/              中断控制器底层
//!   ├── idt/              中断描述符表
//!   ├── dma/              DMA 引擎
//!   ├── driver/           原生硬件驱动 (寄存器/时序)
//!   ├── net/              网络硬件 + 协议栈
//!   ├── fs/               文件系统底层 (VFS 抽象)
//!   ├── ipc/              IPC 底层 (内核态通道)
//!   ├── sgeg/            身份/密码学硬件
//!   ├── egdf/           设备框架底层
//!   ├── freg/          弹性恢复底层 (故障注入/snapshot)
//!   ├── console/          串口/终端硬件初始化
//!   ├── klog/             日志硬件输出
//!   ├── config/           硬件相关配置
//!   ├── smp/              多核支持
//!   ├── lib/              底层工具
//!   ├── link/             链接脚本
//!   ├── alloc/            全局分配器
//!   ├── sync/             同步原语
//!   ├── sched/            调度器特质
//!   ├── frame.rs          Frame 物理页抽象
//!   ├── vmspace.rs        VmSpace 用户地址空间句柄
//!   ├── usermode.rs       UserMode 进入 Ring 3 / EL0 句柄
//!   ├── userctx.rs        UserContext 用户态寄存器
//!   ├── userptr.rs        用户指针
//!   ├── iomem.rs          IoMem MMIO 安全代理
//!   ├── ioport.rs         IoPort x86 PIO 封装
//!   ├── irqline.rs        IrqLine 中断线注册
//!   ├── dma_buf.rs        DmaStream 安全 DMA
//!   ├── page_table.rs     PageTableChecker
//!   ├── cpu_local.rs      CpuLocal Per-CPU 变量
//!   ├── racy_cell.rs      裸 Cell
//!   ├── net_socket.rs     网络 FFI 安全代理
//!   ├── sgeg_pwm.rs      PWM 身份 FFI 安全代理
//!   ├── proc_elf.rs       ELF 加载器 FFI 安全代理
//!   ├── syscall_init.rs   syscall 初始化 FFI 安全代理
//!   └── prelude.rs        公共导入
//!
//! functions/ (去特权, 100% safe Rust, 禁止 unsafe)
//!   ├── driver/  fs/  net/  ipc/  egdf/
//!   ├── proc/  sync/  syscall/  freg/
//!   ├── sgeg/  wasm/  config/  console/  klog/
//!   └── ...
//! ```
//!
//! ## SAFETY 规范
//!
//! 本模块中每个 `unsafe` 块必须有 `// SAFETY:` 注释, 说明:
//! 1. 前提条件: 哪些不变量在本块内被假设成立
//! 2. 调用方保证: 哪些条件由调用上下文的类型/生命周期保证
//! 3. 硬件契约: 对 CPU/MMU/DMA 行为的假设

// ── P-71: crate 根适配 ────────────────────────────────────────────────
// 既有代码里 1736 处 `crate::privileged::…` 自我引用不作改写: 自别名把本 crate
// 根重新导出成 `privileged` 模块; glue 侧再 `pub use kernel_privileged as privileged`.
// host-test 下启用 std (与 glue 的门控同语义), 裸机目标保持 no_std.
extern crate alloc;
pub use crate as privileged;

pub mod arch;
pub mod boot;
pub mod config;
/// 图形/文本控制台输出子系统。
pub mod console;
/// TCB 内部容量常量 (与 privileged::config 职责正交, 见 constants/mod.rs)
pub mod constants;
pub mod cpu;
pub mod debug;
pub mod dma;
pub mod driver;
/// 扁平设备树 (FDT/DTB) 最小解析器 — aarch64 SoC 移植运行时硬件探测
pub mod dtb;
pub mod egdf;
pub mod freg;
pub mod fs;
pub mod idt;
pub mod iobuf;
pub mod ipc;
pub mod irq;
pub mod klog;
/// 内存分配器 TCB — `FrameAlloc` / `SlabAlloc` trait 注入点.
///
/// P-71: 本模块原名 `alloc`. 独立成 crate 后, 根级 `alloc` 会与 `extern crate alloc`
/// 的 crate 绑定同名冲突 (E0260), 故改名 `mem_alloc`, 避免迫使 202 处 `alloc::…` 路径改写.
pub mod mem_alloc;
// P-71: 模块名 `lib` 与本 crate 的 library target 根同名, 触发 special_module_name
// (原单 crate 下嵌套于 `privileged::lib`, 无此特例). 路径保持不变以免影响调用方.
/// crate 内部的 glue/兼容层模块 (模块名与 library target 根同名)。
pub mod lib;
pub mod mm;
pub mod net;
pub mod pci;
pub mod proc;
pub mod sched;
pub mod sgeg;
pub mod smp;
pub mod sync;
/// syscall 分发入口与各系统调用的 privileged 侧机制实现。
pub mod syscall;
// B4 (内存过配治理): 内核测试框架 (TEST_RUNNER 等) 仅在 kernel_test / host-test
// 维度编译, 绝不进入 release 及 host 生产构建. 生产路径所需的 qemu_exit 已迁往
// privileged::debug; 所有 register_*_tests 载体亦由 kernel_test 门控.
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod tests;
pub mod timer;

pub mod cpu_local;
pub mod frame;
/// C4: I/O 子系统 (io_uring)
pub mod io;
pub mod userctx;
pub mod usermode;
pub mod userptr;
pub mod vmspace;

// Phase 1.3 — 设备访问抽象
pub mod dma_buf;
pub mod iomem;
pub mod ioport;
pub mod irqline;
pub mod page_table;

pub mod net_socket;

pub mod sgeg_pwm;

pub mod proc_elf;

pub mod syscall_init;

pub mod prelude;

pub mod racy_cell;

/// POSIX Errno 统一入口 (消除子系统对 syscall 的 Errno 依赖)
pub mod errno;

/// 全内核统一错误类型 (KernelError, B09-12 P0-2 迁回 privileged)
pub mod error;

/// fd 关闭通知接口 (消除 fs 对 syscall::epoll 的直接依赖)
pub mod fd_notify;

/// 资源限制查询接口 (消除 mm 对 proc::rlimit 的直接依赖)
pub mod rlimit_query;

/// 全局 tick 查询接口 (消除 freg 对 proc::scheduler 的直接依赖)
pub mod tick_query;

/// 进程退出清理回调接口 (消除 proc 对 egdf::user_driver 的直接依赖)
pub mod process_cleanup;

// ============================================================================
// P-71: 日志宏归 TCB (原 functions/mod.rs 定义)
//
// 展开后只调用本 crate 的 safe 函数 `klog::log`, 不含任何 unsafe 块.
// 定义随 TCB 下沉后, `privileged` 自身与 glue crate 的 functions 层共用同一份宏
// (`$crate::privileged::klog::…` 经 crate 内自别名解析).
// ============================================================================

/// 通用日志宏 — 指定级别与分类.
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

/// `Info` 级日志宏.
#[macro_export]
macro_rules! slog_info  { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Info,  $cat, $($arg)*) }; }
/// `Warn` 级日志宏.
#[macro_export]
macro_rules! slog_warn  { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Warn,  $cat, $($arg)*) }; }
/// `Error` 级日志宏.
#[macro_export]
macro_rules! slog_err   { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Error, $cat, $($arg)*) }; }
/// `Debug` 级日志宏.
#[macro_export]
macro_rules! slog_debug { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Debug, $cat, $($arg)*) }; }
/// `Crit` 级日志宏.
#[macro_export]
macro_rules! slog_crit  { ($cat:ident, $($arg:tt)*) => { $crate::slog!(Crit,  $cat, $($arg)*) }; }
