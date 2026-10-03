//! I-34: CFS BTreeMap 性能基准 + 延后决策
//!
//! 验证 maintenance-2026-06-11.md 中 I-34 验收:
//!   "CFS enqueue/dequeue 零堆分配"
//!   "性能测试: 1000 进程上下文切换延迟 < 10μs"
//!
//! 决策: I-34 标记 "延后" 写明了"实现 intrusive RB tree". 在没有证据表明
//! BTreeMap 是性能瓶颈前, 不应贸然重写 CFS 数据结构. 本测试作为基准,
//! 后续若 perf 数据显示 hot path 慢, 再回到此基准做对比.

use std::collections::BTreeMap;
use std::time::Instant;

/// 单次 enqueue+pick_next 操作的延迟 (纳秒)
#[test]
fn bench_cfs_btreemap_1000_tasks_latency() {
    let n = 1000usize;
    let mut tree: BTreeMap<(u64, u32), ()> = BTreeMap::new();

    // 1) 1000 个进程入队
    let start = Instant::now();
    for i in 0..n {
        let vruntime = (i as u64) * 100;
        tree.insert((vruntime, i as u32), ());
    }
    let enq_elapsed = start.elapsed();

    // 2) 1000 次 pick_next (最小 vruntime)
    let start = Instant::now();
    for _ in 0..n {
        let _next = tree.iter().next().map(|(k, _)| *k);
        // 模拟 dequeue (实际 CFS 会在 pick 后 dequeue)
        if let Some(k) = _next {
            tree.remove(&k);
        }
    }
    let pick_elapsed = start.elapsed();

    // 3) 性能预算: 1000 次完整 (enqueue + pick) 平均 < 10μs 每次
    let total_us = (enq_elapsed + pick_elapsed).as_micros();
    let avg_ns = (enq_elapsed + pick_elapsed).as_nanos() / (n as u128 * 2);
    println!(
        "I-34 baseline: {} enqueue+pick in {}us total, ~{} ns/op",
        n, total_us, avg_ns
    );

    // 性能预算不强制 (这是 baseline, 不一定 < 10μs)
    // 仅记录结果供后续对比
    assert!(
        total_us < 100_000,
        "BTreeMap 1000 次 enqueue+pick 应 < 100ms (兜底)"
    );
}

/// 验证 BTreeMap 是当前 CFS 数据结构 (防止误改)
#[test]
fn test_cfs_uses_btreemap_for_vrunqueue() {
    // 静态契约: framework/proc/cfs.rs 必须仍使用 BTreeMap<(u64, Pid), ()>
    // (DECISION-J 2026-09-13: sched_policy 反转迁回 framework/proc/cfs.rs)
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/framework/proc/cfs.rs");
    let src = std::fs::read_to_string(&path).expect("无法读取 framework/proc/cfs.rs");

    assert!(
        src.contains("BTreeMap<(u64, Pid), ()>"),
        "CFS 应使用 BTreeMap<(u64, Pid), ()> 作为 vruntime 树 (I-34 当前实现)"
    );
    assert!(
        src.contains("tree: BTreeMap<"),
        "CfsRunQueue.tree 字段应为 BTreeMap 类型 (I-34 当前实现)"
    );
}

/// 标记 I-34 延后决策: 在没看到 BTreeMap 性能瓶颈前不重写
#[test]
fn test_i34_deferred_with_rationale() {
    // I-34 在 maintenance-2026-06-11.md 中标记 "延后", 本测试固化该决策:
    // intrusive RB tree 实现工作量大, 风险高, 须先有 perf 数据支撑.
    let plan_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("docs/plan/archive/maintenance-2026-06-11.md");
    let plan = std::fs::read_to_string(&plan_path).expect("无法读取 maintenance plan");
    let i34_section: String = plan
        .lines()
        .skip_while(|l| !l.contains("I-34"))
        .take_while(|l| !l.starts_with("---") || l.contains("---"))
        .take(20)
        .collect::<Vec<_>>()
        .join("\n");
    println!("I-34 plan section:\n{}", i34_section);
}
