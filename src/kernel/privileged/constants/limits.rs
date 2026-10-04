//! TCB 内部容量上限常量
//!
//! 集中 privileged 层自治容量常量, 与 `privileged::config::*` 职责正交:
//!
//! | 模块 | 职责 | 暴露给 functions |
//! |---|---|---|
//! | `privileged::constants` | TCB 内部实现细节 (MMIO 上限, 锁深度, ...)| 否 |
//! | `privileged::config` | functions 公共 API 桥接 (sysctl, 调参)| 是 |
//!
//! 按 framekernel §4.2 资源分类, 敏感资源 (如硬件容量上限) 归 privileged
//! 内部, 不暴露给 functions. 强类型 (usize) const 不可变, 单点定义.

/// MMIO 别名注册表最大容量
///
/// **超限行为**: `iomem::AliasRegistry::register` 返回
/// `Err("MMIO alias registry full")` (iomem.rs:55), 调用方按需处理.
pub const MAX_MMIO_MAPPINGS: usize = 64;

/// Lockdep 锁类最大数量
///
/// **超限行为**: lockdep 满时**静默截断** (放弃检测), 不 panic. 锁
/// 仍正常工作, 仅失去死锁检测覆盖.
pub const MAX_LOCK_CLASSES: usize = 64;

/// 每线程/每 CPU 最大持有锁深度
///
/// **超限行为**: 同 MAX_LOCK_CLASSES, 静默截断.
pub const MAX_HELD_LOCKS: usize = 8;

/// 每 PWM 配额表最大条目数 (调度器定额)
///
/// **超限行为**: 注册新配额超过上限时返回失败, 调用方按需处理
/// (`privileged::proc::scheduler::Scheduler::register_quota`).
pub const MAX_QUOTAS: usize = 32;

/// 每 PWM 进程数限制表最大条目数 (调度器限额)
///
/// **超限行为**: 同 MAX_QUOTAS, 注册新限制超过上限时返回失败.
pub const MAX_LIMITS: usize = 32;

/// fb_mmap 目标虚拟地址上界 (仅 `privileged::syscall::dispatch::sys_fb_mmap` 使用)
///
/// **注意**: 该边界 (`0x7FFFFFFFE000`, 2^39 量级) 是帧缓冲映射专用约束,
/// **不是**用户指针校验边界. 用户指针校验的真实上界见 `USER_ADDR_MAX`.
/// 两处边界语义不同, 不得混用.
///
/// **超限行为**: `sys_fb_mmap` 对 `target_vaddr > 本值 - size` 返回 `EINVAL`.
pub const FB_MMAP_ADDR_MAX: u64 = 0x7FFFFFFFE000;

/// 用户态地址空间上界 (指针校验边界, 2^47 量级)
///
/// **注意**: 与 `FB_MMAP_ADDR_MAX` (2^39) 语义不同, 后者仅限帧缓冲映射.
/// 本值是 `validate_user_ptr` / `validate_user_buf` 的真实上界 (B05-45 集中).
///
/// **超限行为**: 指针 ≥ 本值 (或 ptr+len 越过本值) 的用户访问被拒绝,
/// `validate_user_ptr`/`validate_user_buf` 返回 false.
pub const USER_ADDR_MAX: u64 = 0x0000_7FFF_FFFF_F000;
