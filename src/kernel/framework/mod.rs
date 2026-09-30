//! QueenX Framekernel — 特权 OS Framework (TCB)
//!
//! 这是整个内核中**唯一**允许包含 `unsafe` Rust 代码的模块。
//! 所有低层硬件交互 (MMU/DMA/中断/上下文切换) 在此封装为
//! 安全 API, 供 `services/` 层在纯 safe Rust 中调用。
//!
//! ## 架构 (框内核 / Asterinas OSTD 范式)
//!
//! ```text
//! framework/ (TCB, unsafe 允许)
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
//!   ├── credo/            身份/密码学硬件
//!   ├── chitin/           设备框架底层
//!   ├── barrier/          弹性恢复底层 (故障注入/snapshot)
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
//!   ├── credo_pwm.rs      PWM 身份 FFI 安全代理
//!   ├── proc_elf.rs       ELF 加载器 FFI 安全代理
//!   ├── syscall_init.rs   syscall 初始化 FFI 安全代理
//!   └── prelude.rs        公共导入
//!
//! services/ (去特权, 100% safe Rust, 禁止 unsafe)
//!   ├── driver/  fs/  net/  ipc/  chitin/
//!   ├── proc/  sync/  syscall/  barrier/
//!   ├── credo/  wasm/  config/  console/  klog/
//!   └── ...
//! ```
//!
//! ## SAFETY 规范
//!
//! 本模块中每个 `unsafe` 块必须有 `// SAFETY:` 注释, 说明:
//! 1. 前提条件: 哪些不变量在本块内被假设成立
//! 2. 调用方保证: 哪些条件由调用上下文的类型/生命周期保证
//! 3. 硬件契约: 对 CPU/MMU/DMA 行为的假设

pub mod alloc;
pub mod arch;
pub mod barrier;
pub mod boot;
pub mod chitin;
pub mod config;
pub mod console;
/// TCB 内部容量常量 (与 framework::config 职责正交, 见 constants/mod.rs)
pub mod constants;
pub mod cpu;
pub mod credo;
pub mod debug;
pub mod dma;
pub mod driver;
/// 扁平设备树 (FDT/DTB) 最小解析器 — aarch64 SoC 移植运行时硬件探测
pub mod dtb;
pub mod fs;
pub mod idt;
pub mod iobuf;
pub mod ipc;
pub mod irq;
pub mod klog;
pub mod lib;
pub mod mm;
pub mod net;
pub mod pci;
pub mod proc;
pub mod sched;
pub mod smp;
pub mod sync;
pub mod syscall;
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

pub mod credo_pwm;

pub mod proc_elf;

pub mod syscall_init;

pub mod prelude;

pub mod racy_cell;

/// POSIX Errno 统一入口 (消除子系统对 syscall 的 Errno 依赖)
pub mod errno;

/// 全内核统一错误类型 (KernelError, B09-12 P0-2 迁回 framework)
pub mod error;

/// fd 关闭通知接口 (消除 fs 对 syscall::epoll 的直接依赖)
pub mod fd_notify;

/// 资源限制查询接口 (消除 mm 对 proc::rlimit 的直接依赖)
pub mod rlimit_query;

/// 全局 tick 查询接口 (消除 barrier 对 proc::scheduler 的直接依赖)
pub mod tick_query;

/// 进程退出清理回调接口 (消除 proc 对 chitin::user_driver 的直接依赖)
pub mod process_cleanup;
