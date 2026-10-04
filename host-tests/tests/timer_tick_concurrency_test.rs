//! 定时器 tick 并发健壮性 host 测试 (B03-LEGACY-003)
//!
//! 背景: `privileged/timer/tick.rs` 的 `TICK_COUNT` 为单一全局原子计数器,
//! `on_timer_interrupt()` 经 `TICK_COUNT.fetch_add(1, Ordering::AcqRel)` 递增
//! (原为 `Relaxed`, B03-12 修正为 `AcqRel`)。本项目当前无 aarch64 SMP
//! (PSCI `CPU_ON` 未实装), 且 x86_64 的 AP 不接收 timer tick, 因此无法在
//! 真机上构造多核并发 tick 场景。
//!
//! 本测试在 host 真多核上, 直接驱动**内核真实** `on_timer_interrupt()` 路径
//! (host 下该函数退化为无副作用: `pit_on_interrupt` 纯原子 / `hrtimer_run_queues`
//! 与 `tick_adjust` 在未初始化时早返回 / `read_tsc` 走 rdtsc), 以 N 个写线程
//! 并发递增 + M 个读线程并发校验, 断言:
//! 1. 读线程观测到的 tick 单调不回退 (读-读一致性);
//! 2. 终值精确等于 `初始 + 写线程数 × 每线程次数` (无丢失更新)。
//!
//! # 判别力边界 (重要)
//! 本用例是**并发健壮性 + 回归防护**: 可捕获 `fetch_add` 被误改为
//! 非原子 load/store RMW (导致丢失更新)、意外重置计数器、撕裂写等回归。
//! 但 **不声称** 证明 `AcqRel` 相对 `Relaxed` 的必要性——x86_64 的 TSO 模型下,
//! `lock` 前缀 RMW 本身即全屏障, `Relaxed` 与 `AcqRel` 在运行时不可区分。
//! 真正对弱序 (Release/Acquire) 语义的判别需待 aarch64 SMP 落地后再补。

use edgine::kernel::privileged::timer::{get_ticks, on_timer_interrupt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 并发写线程数 (模拟多核并发 IRQ 递增)
const WRITERS: usize = 4;
/// 每写线程递增次数
const PER_WRITER: u64 = 50_000;
/// 并发读线程数 (校验单调性)
const READERS: usize = 2;

#[test]
fn concurrent_tick_increments_are_atomic_and_monotonic() {
    let start = get_ticks();
    let done = Arc::new(AtomicBool::new(false));

    // 写线程: 各自并发调用真实中断处理路径
    let mut writers = Vec::with_capacity(WRITERS);
    for _ in 0..WRITERS {
        writers.push(std::thread::spawn(|| {
            for _ in 0..PER_WRITER {
                on_timer_interrupt();
            }
        }));
    }

    // 读线程: 在写并发进行期间反复读取, 断言单调不回退
    let mut readers = Vec::with_capacity(READERS);
    for _ in 0..READERS {
        let done = Arc::clone(&done);
        readers.push(std::thread::spawn(move || {
            let mut last = get_ticks();
            while !done.load(Ordering::Acquire) {
                let cur = get_ticks();
                assert!(cur >= last, "tick 回退: {cur} < {last}");
                last = cur;
            }
        }));
    }

    for h in writers {
        h.join().expect("writer thread panicked");
    }
    done.store(true, Ordering::Release);
    for h in readers {
        h.join().expect("reader thread panicked");
    }

    let total = get_ticks() - start;
    assert_eq!(
        total,
        (WRITERS as u64) * PER_WRITER,
        "tick 丢失更新: 期望 {}, 实际 {total}",
        (WRITERS as u64) * PER_WRITER
    );
}
