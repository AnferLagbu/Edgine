//! 调度器子系统 — functions 层安全代理
//!
//! C2 CPU 亲和性 (`sched_setaffinity` / `sched_getaffinity`) 透传.
//!
//! ## 设计
//!
//! functions 层仅做参数封装与结果翻译, 所有 unsafe / 硬件操作由 privileged 层处理.
//! 业务逻辑 (cpuset 读写, 调度器 hook) 在 `privileged::proc::scheduler` 中实现.

#![deny(unsafe_code)]

use crate::privileged::proc::PROCESS_TABLE;
use crate::privileged::proc::Scheduler;

/// C2: 设置进程的 CPU 亲和性掩码
///
/// `pid == 0` 表示当前进程. `mask` 是 64-bit CPU 位图 (bit i = 允许 CPU i).
///
/// 返回 0 成功, 负值 errno (Linux 兼容).
pub fn sched_setaffinity(pid: u32, mask: u64) -> i64 {
    let target_pid = if pid == 0 {
        Scheduler::current().unwrap_or(0)
    } else {
        pid
    };

    if target_pid == 0 {
        return -(ESRCH);
    }

    let ok = PROCESS_TABLE
        .with_process(target_pid, |p| {
            p.cpuset_allowed
                .store(mask, core::sync::atomic::Ordering::Release);
        })
        .is_some();

    if !ok {
        return -(ESRCH);
    }
    0
}

/// C2: 读取进程的 CPU 亲和性掩码
///
/// 返回 `Some(mask)` 成功, `None` 表示 pid 不存在.
pub fn sched_getaffinity(pid: u32) -> Option<u64> {
    let target_pid = if pid == 0 {
        Scheduler::current().unwrap_or(0)
    } else {
        pid
    };
    if target_pid == 0 {
        return None;
    }
    PROCESS_TABLE.with_process(target_pid, |p| {
        p.cpuset_allowed.load(core::sync::atomic::Ordering::Acquire)
    })
}

/// C2: 检查 CPU 是否在进程 allowed cpuset 中
#[must_use]
pub fn is_cpu_allowed(pid: u32, cpu_id: u32) -> bool {
    Scheduler::is_cpu_allowed(pid, cpu_id)
}

/// C2: 为进程选择最合适的 CPU
///
/// 策略: 在 allowed cpuset 中选 load 最低的 CPU.
#[must_use]
pub fn select_cpu_for(pid: u32, hint_cpu: u32) -> u32 {
    Scheduler::select_cpu_for(pid, hint_cpu)
}

/// SGEG 私有: 导出 EEVDF 运行期观测快照 (K-07b 批3).
///
/// 逐 CPU 输出账目 / lag / 计数器, 并将每线程组可运行成员的 lag 汇总输出
/// 到内核串口. 返回实际上报的在线 CPU 数; 只读调度账目, 不改变调度状态.
#[must_use]
pub fn sched_snapshot_syscall() -> i64 {
    i64::from(Scheduler::sched_snapshot_dump())
}

// ============================================================================
// errno 常量 (Linux 兼容子集)
// ============================================================================

const ESRCH: i64 = 3; // No such process
