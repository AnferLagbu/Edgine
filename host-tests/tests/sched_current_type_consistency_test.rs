// SPDX-License-Identifier: MPL-2.0
// KPTI-19 / P-29 route C 回归测试: 线程级并行调度器不得重现.
//
// 历史缺陷 (KPTI-19): 进程级调度器 `Scheduler::schedule` 曾把进程号 `Pid` 写进
// 语义为 `*mut Thread` 的 `SCHEDULER_EX.current`. 下一次定时器 tick 走
// `tick_accounting` 时把 pid 当指针解引用 → aarch64 对齐异常 / x86_64 静默内存破坏.
// 当时以"净写者不变式"约束该跨层字段.
//
// P-29 route C: 全局单例无锁 `SCHEDULER_EX` 被每核 timer tick 并发驱动 → SMP
// livelock, 已将整套线程级并行实现 (scheduler_ex.rs / thread.rs) 从代码库移除,
// 调度统一到 per-CPU 进程级 `Scheduler`. 本回归守卫该"移除"这一根治事实:
//   - 进程级 `scheduler.rs` (去注释) 中不得再出现 `SCHEDULER_EX` / `scheduler_ex`
//   - `scheduler_ex.rs` 文件本身不得重新存在

use std::fs;
use std::path::Path;

const SCHEDULER: &str = "../src/kernel/privileged/proc/scheduler.rs";
const SCHEDULER_EX: &str = "../src/kernel/privileged/proc/scheduler_ex.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释, 避免注释文本被误判为代码
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_process_scheduler_has_no_thread_scheduler_reference() {
    let src = code_only(&read(SCHEDULER));
    assert!(
        !src.contains("SCHEDULER_EX") && !src.contains("scheduler_ex"),
        "scheduler.rs 不得再引用已移除的线程级并行调度器 SCHEDULER_EX (P-29 route C)"
    );
}

#[test]
fn test_thread_scheduler_impl_stays_removed() {
    assert!(
        !Path::new(SCHEDULER_EX).exists(),
        "线程级并行调度器 scheduler_ex.rs 必须保持删除状态 (P-29 route C)"
    );
}
