//! 调度系统调用 ABI 常量与 `sched_attr` 结构布局（Linux x86_64 `<sched.h>`）.
//!
//! 内核 `privileged::syscall::types` 的编号定义、`privileged::syscall::api`
//! 的结构体拷贝与 `functions::proc::sched_attr` 的策略实现均引用本模块；
//! 两侧数值与结构偏移由结构保证一致，避免"内核实现与用户态常量各写一份".
//!
//! 值域基准：Linux x86_64 通用 syscall 表（`sched_setscheduler` = 144、
//! `sched_getscheduler` = 145、`sched_setattr` = 314、`sched_getattr` = 315）
//! 与 `<linux/sched/types.h>` / `<linux/sched.h>` 的策略值与 `sched_attr` 布局。
//! aarch64 通用表与 x86_64 同值，故双架构一致.

/// `sched_setscheduler` 系统调用号（Linux x86_64 标准编号）.
pub const SYS_SCHED_SETSCHEDULER: u64 = 144;

/// `sched_getscheduler` 系统调用号（Linux x86_64 标准编号）.
pub const SYS_SCHED_GETSCHEDULER: u64 = 145;

/// `sched_setattr` 系统调用号（Linux x86_64 标准编号）.
pub const SYS_SCHED_SETATTR: u64 = 314;

/// `sched_getattr` 系统调用号（Linux x86_64 标准编号）.
pub const SYS_SCHED_GETATTR: u64 = 315;

/// 普通分时调度策略（Linux `SCHED_NORMAL` 的同义名）.
pub const SCHED_OTHER: u32 = 0;

/// 实时先进先出策略.
pub const SCHED_FIFO: u32 = 1;

/// 实时时间片轮转策略.
pub const SCHED_RR: u32 = 2;

/// 批处理策略（Linux 值；Edgine 当前未实现）.
pub const SCHED_BATCH: u32 = 3;

/// 空闲调度策略（Linux ABI 值为 5；与内核内部 `SchedPolicy::Idle = 3` 不同域）.
pub const SCHED_IDLE: u32 = 5;

/// 截止时间调度策略（Linux ABI 值为 6；与内核内部 `SchedPolicy::Deadline = 4` 不同域）.
pub const SCHED_DEADLINE: u32 = 6;

/// `sched_setscheduler` 策略字中的"子进程重置为默认策略"标志位.
///
/// Edgine 当前不支持该语义，置位时由系统调用层明确返回 `EINVAL`，
/// 而不是静默忽略用户请求.
pub const SCHED_RESET_ON_FORK: u32 = 0x4000_0000;

/// `sched_attr.sched_flags`：子进程重置为默认策略（当前不支持，置位即 `EINVAL`）.
pub const SCHED_FLAG_RESET_ON_FORK: u64 = 1;

/// `sched_attr.sched_flags`：回收运行时间（当前不支持，置位即 `EINVAL`）.
pub const SCHED_FLAG_RECLAIM: u64 = 1 << 1;

/// `sched_attr.sched_flags`：截止期超限通知（当前不支持，置位即 `EINVAL`）.
pub const SCHED_FLAG_DL_OVERRUN: u64 = 1 << 2;

/// `sched_attr.sched_flags`：保留策略字段（当前不支持，置位即 `EINVAL`）.
pub const SCHED_FLAG_KEEP_POLICY: u64 = 1 << 3;

/// `sched_attr.sched_flags`：保留参数字段（当前不支持，置位即 `EINVAL`）.
pub const SCHED_FLAG_KEEP_PARAMS: u64 = 1 << 4;

/// `sched_attr.sched_flags`：利用率下限钳制（当前不支持，置位即 `EINVAL`）.
pub const SCHED_FLAG_UTIL_CLAMP_MIN: u64 = 1 << 5;

/// `sched_attr.sched_flags`：利用率上限钳制（当前不支持，置位即 `EINVAL`）.
pub const SCHED_FLAG_UTIL_CLAMP_MAX: u64 = 1 << 6;

/// `struct sched_attr` 的首版长度（字节）.
///
/// `sched_setattr` 只接受与该值完全相等的 `size`：更大尺寸意味着用户携带了
/// 内核尚未识别的字段，按"明确拒绝而非静默忽略"处理.
pub const SCHED_ATTR_SIZE_VER0: u32 = 48;

/// nice 值下界（Linux `MIN_NICE`）.
pub const MIN_NICE: i32 = -20;

/// nice 值上界（Linux `MAX_NICE`）.
pub const MAX_NICE: i32 = 19;

/// Linux `struct sched_param` 布局.
///
/// Edgine 只支持 `SCHED_OTHER`，对应 `sched_priority` 必须为 0；其它策略
/// 一律由系统调用层返回 `EINVAL`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedParam {
    /// 实时调度优先级（`SCHED_OTHER` 下必须为 0）.
    pub sched_priority: i32,
}

/// Linux `struct sched_attr` 首版（VER0）布局，共 48 字节.
///
/// 字段顺序、宽度与偏移与 `<linux/sched/types.h>` 一致；`size` 必须由用户
/// 置为 [`SCHED_ATTR_SIZE_VER0`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedAttr {
    /// 结构体自身长度（必须等于 [`SCHED_ATTR_SIZE_VER0`]）.
    pub size: u32,
    /// 调度策略（当前仅接受 [`SCHED_OTHER`]）.
    pub sched_policy: u32,
    /// 策略标志位（当前要求为 0）.
    pub sched_flags: u64,
    /// nice 值（`MIN_NICE..=MAX_NICE`）.
    pub sched_nice: i32,
    /// 实时优先级（`SCHED_OTHER` 下必须为 0）.
    pub sched_priority: u32,
    /// 请求的真实运行切片（纳秒；0 表示清除请求、回到动态默认切片）.
    pub sched_runtime: u64,
    /// 截止期（当前要求为 0）.
    pub sched_deadline: u64,
    /// 周期（当前要求为 0）.
    pub sched_period: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    /// `SchedAttr` 必须与 Linux VER0 布局逐字段同偏移.
    #[test]
    fn sched_attr_matches_linux_ver0_layout() {
        assert_eq!(size_of::<SchedAttr>(), 48);
        assert_eq!(offset_of!(SchedAttr, size), 0);
        assert_eq!(offset_of!(SchedAttr, sched_policy), 4);
        assert_eq!(offset_of!(SchedAttr, sched_flags), 8);
        assert_eq!(offset_of!(SchedAttr, sched_nice), 16);
        assert_eq!(offset_of!(SchedAttr, sched_priority), 20);
        assert_eq!(offset_of!(SchedAttr, sched_runtime), 24);
        assert_eq!(offset_of!(SchedAttr, sched_deadline), 32);
        assert_eq!(offset_of!(SchedAttr, sched_period), 40);
    }

    /// `SchedParam` 是单个 `int`（4 字节）.
    #[test]
    fn sched_param_is_single_i32() {
        assert_eq!(size_of::<SchedParam>(), 4);
        assert_eq!(offset_of!(SchedParam, sched_priority), 0);
    }

    /// 策略值与 Linux ABI 对齐；内部枚举值不在此处复用.
    #[test]
    fn policy_values_match_linux() {
        assert_eq!(SCHED_OTHER, 0);
        assert_eq!(SCHED_FIFO, 1);
        assert_eq!(SCHED_RR, 2);
        assert_eq!(SCHED_BATCH, 3);
        assert_eq!(SCHED_IDLE, 5);
        assert_eq!(SCHED_DEADLINE, 6);
        assert_eq!(SCHED_RESET_ON_FORK, 0x4000_0000);
    }
}
