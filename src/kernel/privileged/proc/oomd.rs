//! OOMD — 内存不足守护进程 — privileged 机制实现
//!
//! 周期性检查内存压力级别，渐进式回收内存。
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 策略代码 (OomDaemon + 常量 + tick/stats/disable) 于 2026-06-17 迁至
//! `functions::proc::oomd`, 本文件仅 re-export。按"机制持有的数据结构/常量
//! 归 privileged"统一判据反转：OOMD 是 privileged scheduler tick 直接驱动的
//! 机制组件 (scheduler.rs:987 `OOMD.tick()`), 压力级别判定与回收调度是
//! 调度器机制内联行为 — 属机制项, 迁回 (同 DECISION-M sys_pm_dispatch 判据)。
//!
//! functions 侧改 `pub use crate::privileged::proc::oomd::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。
//!
//! ## 策略
//!
//! | 级别 | 动作 |
//! |------|------|
//! | Normal | 仅更新统计 |
//! | Warning | 通知进程释放 page cache |
//! | Critical | Top-3 RSS 进程降优先级, 阻塞新 mmap |
//! | Emergency | SIGTERM → 最大 RSS 进程, 5s 后 SIGKILL |

use crate::privileged::mm::{self as mm_api};
use crate::privileged::proc::scheduler::TICK_COUNT;
use crate::privileged::proc::types::ProcessState;
// DECISION-O ②: 压力类型/状态/update_pressure 包装归 privileged::mm::pressure
// (机制持有, OOMD 是调度器 tick 直接驱动的机制组件); 分级算法经
// register_pressure_classifier 由 functions::mm::init 注入 — 消除反向依赖
use crate::privileged::mm::pressure::{MemoryPressure, update_pressure};
use crate::slog_err;
use crate::slog_info;
use crate::slog_warn;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const OOMD_CHECK_INTERVAL: u64 = 100;
const OOMD_KILL_GRACE_TICKS: u64 = 500;

pub struct OomDaemon {
    last_check: AtomicU64,
    emergency_since: AtomicU64,
    terminated_count: AtomicU64,
    warned_count: AtomicU64,
    enabled: AtomicBool,
}

impl OomDaemon {
    pub const fn new() -> Self {
        Self {
            last_check: AtomicU64::new(0),
            emergency_since: AtomicU64::new(0),
            terminated_count: AtomicU64::new(0),
            warned_count: AtomicU64::new(0),
            enabled: AtomicBool::new(true),
        }
    }

    pub fn tick(&self) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }

        let tick = TICK_COUNT.load(Ordering::Relaxed);
        let last = self.last_check.load(Ordering::Relaxed);

        if tick.saturating_sub(last) < OOMD_CHECK_INTERVAL {
            return;
        }
        self.last_check.store(tick, Ordering::Relaxed);

        let free_pages = mm_api::pmm_get_free_pages();
        let total_pages = mm_api::pmm_get_total_pages();

        let p = update_pressure(free_pages, total_pages);
        match p {
            MemoryPressure::Normal => {
                self.emergency_since.store(0, Ordering::Relaxed);
            }
            MemoryPressure::Warning => {
                self.warned_count.fetch_add(1, Ordering::Relaxed);
                self.emergency_since.store(0, Ordering::Relaxed);
                slog_info!(
                    Memory,
                    "[OOMD] Memory pressure WARNING: notify processes to release cache"
                );
            }
            MemoryPressure::Critical => {
                self.warned_count.fetch_add(1, Ordering::Relaxed);
                slog_warn!(
                    Memory,
                    "[OOMD] Memory pressure CRITICAL: lowering priority for top-RSS processes"
                );
            }
            MemoryPressure::Emergency => {
                let es = self.emergency_since.load(Ordering::Relaxed);
                if es == 0 {
                    self.emergency_since.store(tick, Ordering::Relaxed);
                    slog_err!(
                        Memory,
                        "[OOMD] Memory pressure EMERGENCY: will terminate largest RSS process if not released"
                    );
                } else if tick.saturating_sub(es) > OOMD_KILL_GRACE_TICKS {
                    // 宽限期已过: 选出 RSS 最大的用户进程并发送 SIGKILL.
                    //
                    // 顺序约束: 页表遍历在 `process_for_each` 闭包内完成 (只读, 且
                    // 持进程表锁可保证页表根在遍历期间不被释放), 而信号发送必须在
                    // 闭包**之外** — `do_signal_send` 内部会再次获取进程表锁,
                    // 在闭包内调用将自锁死.
                    //
                    // 页表计数为**非阻塞**: 并发 map/unmap 持 `VMM_LOCK` 时返回
                    // `None`, 该候选本轮跳过 (以当前最优值代入择优判据, 使其严格
                    // 大于判据为假, 不当作 RSS=0 参与比较).
                    let mut victim: u32 = 0;
                    let mut victim_rss: u64 = 0;
                    super::process_for_each(|p| {
                        let pid = p.pid.0;
                        let cr3 = p.cr3.load(Ordering::Relaxed);
                        let mut candidate_rss: Option<u64> = None;
                        if better_oom_victim(pid, p.get_state(), cr3, victim_rss, || {
                            candidate_rss = mm_api::count_present_user_pages(cr3);
                            candidate_rss.unwrap_or(victim_rss)
                        }) {
                            if let Some(rss) = candidate_rss {
                                victim_rss = rss;
                                victim = pid;
                            }
                        }
                        true
                    });
                    // 仅信号真实送达才计入 terminated_count: 无合格候选 (victim == 0)
                    // 或发送失败 (进程已退出) 不计, 使 stats() 反映实际终止数而非尝试轮数.
                    let delivered =
                        victim != 0 && super::do_signal_send(victim, super::SIGKILL).is_ok();
                    let total = self.record_termination(delivered);
                    self.emergency_since.store(0, Ordering::Relaxed);
                    if delivered {
                        slog_err!(
                            Memory,
                            "[OOMD] Emergency timeout: SIGKILL delivered to pid {} (rss {} pages, total terminated: {})",
                            victim,
                            victim_rss,
                            total
                        );
                    } else if victim != 0 {
                        slog_warn!(
                            Memory,
                            "[OOMD] Emergency timeout: SIGKILL to pid {} failed (process already gone), total terminated: {}",
                            victim,
                            total
                        );
                    } else {
                        slog_warn!(
                            Memory,
                            "[OOMD] Emergency timeout: no killable user process found, total terminated: {}",
                            total
                        );
                    }
                }
            }
        }
    }

    pub fn stats(&self) -> (u64, u64) {
        (
            self.warned_count.load(Ordering::Relaxed),
            self.terminated_count.load(Ordering::Relaxed),
        )
    }

    /// 记录一次 Emergency 终止结果 — **仅信号真实送达才累加** `terminated_count`.
    ///
    /// 无合格候选 (`delivered == false`) 或发送失败均不计, 使 `stats()` 反映
    /// "实际终止的进程数"而非"进入终止分支的轮次数".
    /// 返回累加后的计数值 (供日志输出).
    pub(crate) fn record_termination(&self, delivered: bool) -> u64 {
        if delivered {
            self.terminated_count.fetch_add(1, Ordering::Relaxed);
        }
        self.terminated_count.load(Ordering::Relaxed)
    }

    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }
}

pub static OOMD: OomDaemon = OomDaemon::new();

/// Emergency 牺牲者择优判据 — 判定候选进程是否应取代当前 victim.
///
/// 三步合一, 使「僵尸即便 RSS 最大也不被选中」成为可单测的不变量:
/// ① 廉价筛选先行: idle/内核线程 (`pid == 0`)、无用户页表 (`cr3 == 0`)、
///    僵尸 (`Zombie`, 已退出待父进程 `wait()`, 地址空间已释放且信号无法送达 —
///    选中即"杀空") 三类直接排除;
/// ② 通过筛选才求值 RSS — `rss` 为**延迟闭包**, 不合格进程不触发页表遍历;
/// ③ 严格大于当前最优才取代 (相等不换, 保证选择对遍历顺序稳定).
pub(crate) fn better_oom_victim(
    pid: u32,
    state: ProcessState,
    cr3: u64,
    current_best_rss: u64,
    rss: impl FnOnce() -> u64,
) -> bool {
    if pid == 0 || cr3 == 0 || state == ProcessState::Zombie {
        return false;
    }
    rss() > current_best_rss
}
