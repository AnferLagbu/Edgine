//! 系统配置中心
//!
//! `Edgine` 内核的 **统一配置中心 + 启动自检中心**.
//!
//! ## 职责
//!
//! 1. **集中定义全内核容量/规模常量** — 消除分散与重复
//! 2. **集中定义内存布局常量** — `PAGE_SIZE`、栈/堆尺寸、用户态地址空间
//! 3. **集中定义调度常量** — CFS/调度级别量子、提升间隔
//! 4. **集中定义 slab 配置** — 大小/对象尺寸
//! 5. **启动自检** — 验证常量自洽性、内存布局合法性、中断控制器可用性
//! 6. **跨模块一致性校验** — 防止下游模块因重复定义产生 OOB
//! 7. **特性能力查询** — 编译期 `cfg` 与运行时 `KernelCapabilities`
//! 8. **procfs 接口** — 用户态通过 `/proc/sys/config` 读取
//! 9. **启动期 ASCII 表格输出** — 通过 `print_config_table()`
//!
//! ## 架构
//!
//! 本模块由子模块组成, 每个子模块负责一个领域:
//!
//! ```text
//! config/
//!   capacity.rs   进程/线程/IRQ/文件/会话容量
//!   memory.rs     内存布局常量
//!   sched.rs      调度常量 (SCHED_*)
//!   slab.rs       slab 配置
//!   validate.rs   validate_* 函数 (re-export, 待 trait 注入)
//!   caps.rs       ConfigSummary, KernelCapabilities
//!   boot_image.rs 启动镜像编码
//!   kaslr.rs      KASLR 配置
//!   mod.rs        入口 + re-exports + init
//! ```
//!
//! > DECISION-J (2026-09-12): error (ConfigError) 与 procfs (read_sys_config)
//! > 为 functions 策略项, privileged 侧壳已删除, 由 functions::config 提供。
//!
//! ## 调用方式
//!
//! ```text
//! kernel::config::init();   // 极早期调用, 仅依赖 klog + smp::get_cpu_count
//! ```
//!
//! ## 安全要点
//!
//! - 所有数组分配以本模块常量为 **唯一权威**; 其他模块必须 `pub use` 而非重复 `pub const`
//! - 启动期 `validate_cross_module_consistency` 二次校验, 防止未来回归
//! - debug 构建中, 关键错误会 panic; release 构建降级为日志

// ============================================================================
// 子模块声明
// ============================================================================

mod capacity;
mod caps;
mod error;
mod kaslr;
// I-预存: `privileged::config::memory` 需要从外部测试模块访问, 之前设为私有导致
// `tests::mod` 在 kernel_test build 下编译失败 (E0603). 改 `pub` 暴露给 `privileged` 内的
// 跨模块访问, 外部边界 (functions) 通过 `privileged::config::*` 公共 API 间接使用.
pub mod boot_image;
pub mod memory;
mod sched;
mod slab;
// DECISION-K: validate 壳 (re-export functions validate_*) 改为 ConfigValidateHook trait 注入
mod validate_hook;

// ============================================================================
// 重新导出: 子模块 (便于测试与外部使用)
// ============================================================================

pub use caps::{ConfigSummary, KernelCapabilities, get_config_summary};
pub use error::ConfigError;

// ============================================================================
// 重新导出: 常量 (保持外部 `use crate::privileged::config::XXX` 路径完全不变)
// ============================================================================

pub use capacity::{
    MAX_CPUS, MAX_IRQS, MAX_OPEN_FILES, MAX_PROCESSES, MAX_SESSIONS, MAX_THREADS,
    MAX_THREADS_PER_PROCESS,
};
pub use memory::{
    ASLR_HEAP_BITS, ASLR_MMAP_BITS, ASLR_PIE_BITS, ASLR_STACK_BITS, HUGE_PAGE_1G_SHIFT,
    HUGE_PAGE_1G_SIZE, HUGE_PAGE_2M_SHIFT, HUGE_PAGE_2M_SIZE, KERNEL_STACK_SIZE, PAGE_SHIFT,
    PAGE_SIZE, USER_CODE_BASE, USER_HEAP_BASE, USER_KSTACK_SIZE, USER_MMAP_BASE, USER_PIE_BASE,
    USER_STACK_GUARD, USER_STACK_MAX_SIZE, USER_STACK_SIZE, USER_STACK_TOP, aslr_heap_base,
    aslr_mmap_base, aslr_pie_base, aslr_random_offset, aslr_stack_top,
};
pub use sched::{
    CFS_BOOST_INTERVAL, CFS_DL_MAX_UTILIZATION_PCT, CFS_DL_MIN_PERIOD, CFS_DL_MIN_RUNTIME,
    CFS_MIN_GRANULARITY, CFS_NICE0_WEIGHT, CFS_TARGET_LATENCY, SCHED_BOOST_INTERVAL,
    SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM, SCHED_LEVEL_3_QUANTUM,
    SCHED_RT_WATCHDOG_TICKS,
};
pub use slab::{
    SLAB_DEFAULT_SIZE, SLAB_GENERAL_CACHE_NUM, SLAB_MAX_OBJECT_SIZE, SLAB_MIN_OBJECT_SIZE,
};

// ============================================================================
// 重新导出: 验证策略 (DECISION-K: validate_* 经 ConfigValidateHook trait 注入)
// ============================================================================

pub use validate_hook::{
    ConfigValidateHook, current_config_validate_hook, register_config_validate_hook,
};

// 演进 9: KASLR 配置接入 (validate_kaslr_offset 为 functions 策略校验, 由 functions::config 提供)
pub use kaslr::{
    KASLR_ALIGN, KASLR_BASE_OFFSET, KASLR_DEFAULT_OFFSET, KASLR_ENABLED, KASLR_MAX_OFFSET,
    get_kaslr_offset, is_aligned as is_kaslr_aligned, set_kaslr_offset,
};

// ============================================================================
// ASCII 表格打印 (演进 3)
// ============================================================================

/// 以 ASCII 表格形式输出当前内核配置, 适用于串口启动日志
/// 与 dmesg 抓取。
///
/// 输出宽度固定 56 列, 与内核其他启动表对齐。
pub fn print_config_table() {
    use crate::klog_info;

    let s = get_config_summary();
    let caps = s.capabilities;

    klog_info!(
        Boot,
        "+----------------------------------------------------+"
    );
    klog_info!(
        Boot,
        "|          Edgine Configuration                      |"
    );
    klog_info!(
        Boot,
        "+-------------------------+--------------------------+"
    );
    klog_info!(
        Boot,
        "| max_cpus / actual       | {:>8} / {:<8}    |",
        s.max_cpus,
        s.actual_cpus
    );
    klog_info!(Boot, "| max_irqs                | {:>22}    |", s.max_irqs);
    klog_info!(
        Boot,
        "| max_processes           | {:>22}    |",
        s.max_processes
    );
    klog_info!(
        Boot,
        "| max_threads             | {:>22}    |",
        s.max_threads
    );
    klog_info!(Boot, "| page_size (bytes)       | {:>22}    |", s.page_size);
    klog_info!(
        Boot,
        "+-------------------------+--------------------------+"
    );
    klog_info!(
        Boot,
        "| APIC / IOAPIC           | {:>10} / {:<10}|",
        if s.apic_enabled { "on" } else { "off" },
        if s.ioapic_enabled { "on" } else { "off" }
    );
    klog_info!(
        Boot,
        "+-------------------------+--------------------------+"
    );
    // 演进 9: 运行时 KASLR 偏移
    klog_info!(
        Boot,
        "| KASLR offset (hex)      |                  0x{:>8X} |",
        s.kaslr_offset
    );
    klog_info!(
        Boot,
        "+-------------------------+--------------------------+"
    );
    klog_info!(Boot, "| capabilities                                     |");
    klog_info!(
        Boot,
        "|   smp={} preempt={} kaslr={} kpti={} freg={}      |",
        on_off(caps.smp),
        on_off(caps.preempt),
        on_off(caps.kaslr),
        on_off(caps.kpti),
        on_off(caps.freg)
    );
    klog_info!(
        Boot,
        "+----------------------------------------------------+"
    );
}

#[inline]
fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

// ============================================================================
// 入口
// ============================================================================

/// 初始化配置校验并输出启动摘要.
///
/// 应作为 `kernel_main` 的第一动作调用, 早于任何依赖
/// `MAX_CPUS`/`MAX_PROCESSES`/等常量的子系统.
pub fn init() {
    use crate::klog_info;

    let summary = get_config_summary();
    let caps = summary.capabilities;

    klog_info!(Boot, "==== Edgine Configuration ====");
    klog_info!(
        Boot,
        "  CPUs:    {} / {}",
        summary.actual_cpus,
        summary.max_cpus
    );
    klog_info!(Boot, "  IRQs:    {}", summary.max_irqs);
    klog_info!(Boot, "  Procs:   {}", summary.max_processes);
    klog_info!(
        Boot,
        "  Threads: {} (max {} per proc)",
        summary.max_threads,
        MAX_THREADS_PER_PROCESS
    );
    klog_info!(Boot, "  Page:    {} bytes", summary.page_size);
    klog_info!(
        Boot,
        "  APIC:    {}   IOAPIC: {}",
        on_off(summary.apic_enabled),
        on_off(summary.ioapic_enabled)
    );
    klog_info!(
        Boot,
        "  Caps:    smp={} preempt={} kaslr={} kpti={} freg={}",
        on_off(caps.smp),
        on_off(caps.preempt),
        on_off(caps.kaslr),
        on_off(caps.kpti),
        on_off(caps.freg)
    );

    // DECISION-K: 启动自检经 ConfigValidateHook trait 注入 (Option 可空,
    // 未注册跳过校验 + 日志 — validate 是启动增强, 逻辑错误降级原则)
    // clippy: 单分支匹配改用 if-let (无行为变更)
    let errors = if let Some(h) = current_config_validate_hook() {
        h.validate_system_config()
    } else {
        klog_info!(Boot, "==== ConfigValidateHook 未注册, 跳过启动校验 ====");
        0
    };

    if errors == 0 {
        klog_info!(Boot, "==== Configuration OK ====");
    } else {
        klog_info!(
            Boot,
            "==== Configuration: {} error(s) (see above) ====",
            errors
        );
    }

    // 演进 6: 软校验子系统初始化状态 (PCI/网络/...)
    // 注意: 此校验点位于 kernel_init 极早期, 此时 PCI/网络/驱动尚未初始化。
    // 这里记录为 0 错误是预期行为 — 真正的 driver 配置检查在它们各自的 init() 末尾调用。
    // clippy: 消除冗余闭包 (直接函数引用)
    let driver_errors =
        current_config_validate_hook().map_or(0, ConfigValidateHook::validate_drivers);
    if driver_errors > 0 {
        klog_info!(
            Boot,
            "==== Drivers: {} subsystem(s) uninitialized (deferred to late init) ====",
            driver_errors
        );
    }

    // 演进 3: 启动期同步打印 ASCII 表格 (便于截图与 dmesg 抓取)
    print_config_table();

    // 演进 10: 编码 ConfigSummary → boot_image (崩溃后取证用)
    boot_image::encode_boot_image();
    klog_info!(
        Boot,
        "boot_image: encoded {} bytes for crash dump",
        boot_image::encoded_len()
    );
}
