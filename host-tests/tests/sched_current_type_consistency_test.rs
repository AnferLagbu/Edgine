// SPDX-License-Identifier: MPL-2.0
// KPTI-19 回归测试: `SCHEDULER_EX.current` 类型混用.
//
// 缺陷: 进程级调度器 `Scheduler::schedule` 曾把进程号 `Pid` 写进语义为
// `*mut Thread` 的 `SCHEDULER_EX.current`. 下一次定时器 tick 走
// `tick_accounting` 时把 pid 当指针解引用:
//   - aarch64: 对齐异常陷入 (ESR 0x96000061, DFSC=0x21), 携 FAR = pid*8+0x28
//   - x86_64: 非对齐 `lock incq` 直接写坏低地址 VA (静默内存破坏)
//
// 修复: 删除该跨层写入, 恢复"`SchedulerEx::current` 由 `SchedulerEx` 独占写"
// 的净写者不变式 (`init` 写 idle / `schedule` 写 next 两处).
//
// 验收:
//   - `scheduler.rs` 代码 (去注释) 中不得访问 `SCHEDULER_EX.current`
//   - `scheduler_ex.rs` 中 `self.current.store(` 恰为 2 处 (init / schedule)

use std::fs;

const SCHEDULER: &str = "../src/kernel/framework/proc/scheduler.rs";
const SCHEDULER_EX: &str = "../src/kernel/framework/proc/scheduler_ex.rs";

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
fn test_process_scheduler_does_not_touch_thread_scheduler_current() {
    let src = code_only(&read(SCHEDULER));
    let mut idx = 0;
    while let Some(pos) = src[idx..].find("SCHEDULER_EX") {
        let at = idx + pos;
        let tail = &src[at..(at + 80).min(src.len())];
        assert!(
            !tail.contains(".current"),
            "scheduler.rs 不得访问 SCHEDULER_EX.current (pid 与 *mut Thread 类型混用): {}",
            tail.lines().next().unwrap_or("")
        );
        idx = at + "SCHEDULER_EX".len();
    }
}

#[test]
fn test_thread_scheduler_current_has_single_writer() {
    let src = read(SCHEDULER_EX);
    let writes: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("self.current.store("))
        .collect();
    assert_eq!(
        writes.len(),
        2,
        "SchedulerEx::current 只允许 init(idle) / schedule(next) 两处写入, 实际: {writes:?}"
    );
}
