// SPDX-License-Identifier: MPL-2.0
//! E-04 tick 记账与切片预算回归锁.
//!
//! 覆盖两条腿:
//!
//! - **运行维** (`tick_builds_advances_and_expires_slice_on_real_rq`): 在真实
//!   per-CPU rq + `PROCESS_TABLE` 上跑生产 `Scheduler::tick`, 验证 `Build →
//!   Advance → Expire` 三分: 切片内只推进运行账 (current 不离核、不进树), 到期
//!   后重建切片并把 current 以 `Requeue` 语义投回树, 再次 `schedule` 可由树侧
//!   拣回运行账.
//! - **源码契约**: tick 热路径的增量必须是 Q16.16 (`calc_vruntime_delta_virt`,
//!   无 `<< VIRT_SHIFT` 桥), 只做运行账 O(1) 增量; 重调度只登记标记, 实际切换
//!   留给中断退出路径; 旧 `cfs_should_preempt` / `floor + TARGET_LATENCY` 判据
//!   不得回归 (D6①).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use edgine::kernel::privileged::proc::cfs::{
    NICE0_WEIGHT, TARGET_LATENCY_TICKS, VIRT_ONE, calc_vruntime_delta_virt, effective_slice_real,
};
use edgine::kernel::privileged::proc::process::{PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::{BlockReason, Scheduler};

const SCHED: &str = "src/kernel/privileged/proc/scheduler.rs";

/// 运行维用例会写全局 per-CPU rq / current, 文件内串行化.
static RQ_LOCK: Mutex<()> = Mutex::new(());

fn lock_rq() -> std::sync::MutexGuard<'static, ()> {
    RQ_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 宿主进程句柄: 真实 `Process::new` + 全局 `PROCESS_TABLE`, Drop 时回收.
struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new(name: &str) -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, name, None));
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Self { pid }
    }

    fn with<R>(&self, f: impl FnOnce(&Process) -> R) -> R {
        PROCESS_TABLE
            .with_process(self.pid, f)
            .expect("测试进程应在表中")
    }

    fn vruntime(&self) -> u64 {
        self.with(|p| p.cfs_vruntime.load(Ordering::Acquire))
    }

    fn sum_exec(&self) -> u64 {
        self.with(|p| p.cfs_sum_exec_runtime.load(Ordering::Acquire))
    }

    fn slice_left(&self) -> u64 {
        self.with(|p| p.eevdf_slice_left.load(Ordering::Acquire))
    }

    fn rq_cpu(&self) -> u32 {
        self.with(|p| p.cfs_rq_cpu.load(Ordering::Acquire))
    }

    fn on_rq(&self) -> bool {
        self.with(|p| p.cfs_on_rq.load(Ordering::Acquire))
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

/// 测试收尾: 先把 current 撤出可运行集 (树 / 运行账两处都覆盖), 再清 current.
struct CurrentGuard {
    pid: u32,
}

impl CurrentGuard {
    fn install(pid: u32) -> Self {
        Scheduler::set_current(pid);
        Self { pid }
    }
}

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        if Scheduler::current() == Some(self.pid) {
            Scheduler::block(BlockReason::Unknown);
        }
        Scheduler::set_current(0);
    }
}

/// 生产 tick 的三分语义: Build 建切片, Advance 只动运行账, Expire 重投入树.
///
/// 选择 `Scheduler::schedule()` 把任务从树拣入运行账: current 与树内任务同一
/// 时刻 ⇒ 函数走到 `next == current_pid` 早返回, 不执行上下文切换 (host 可跑),
/// 但确实完成 `pick_eevdf` 的树→运行账搬移 —— 正是 tick 记账的前置状态.
#[test]
fn tick_builds_advances_and_expires_slice_on_real_rq() {
    let _serial = lock_rq();
    let proc = TestProc::new("e04-tick");
    let pid = proc.pid;
    let weight = NICE0_WEIGHT;
    let delta = calc_vruntime_delta_virt(weight);
    assert_eq!(delta, VIRT_ONE, "nice 0 的 Q16.16 增量应为一个虚拟 tick");
    let slice_real = effective_slice_real(weight, weight, None);
    assert_eq!(
        slice_real, TARGET_LATENCY_TICKS,
        "单任务时默认真实切片应取目标延迟"
    );

    Scheduler::add_to_run_queue(pid);
    assert!(proc.on_rq(), "入队后应带记账凭证");
    assert_eq!(proc.rq_cpu(), 0);
    let _cur = CurrentGuard::install(pid);
    assert_eq!(Scheduler::schedule(), Some(pid), "当前任务应从树拣入运行账");
    assert!(
        !Scheduler::has_runnable(),
        "拣出后运行任务应离开树账 (运行账不是树)"
    );

    // 1) Build: 首个 tick 建立本轮切片, 不把 current 投回树.
    Scheduler::tick(0);
    assert_eq!(
        proc.slice_left(),
        slice_real,
        "Build 后切片预算应为 slice_real"
    );
    assert_eq!(proc.vruntime(), delta, "tick 增量必须落在 Q16.16 域");
    assert_eq!(proc.sum_exec(), 1);
    assert!(
        !Scheduler::has_runnable(),
        "Build 分支不得写树账 (运行任务仍在核上)"
    );

    // 2) Advance: 切片内只做运行账 O(1) 增量, 不请求抢占.
    for consumed in 1..slice_real {
        Scheduler::tick(0);
        assert_eq!(
            proc.slice_left(),
            slice_real - consumed,
            "切片必须逐 tick 递减"
        );
        assert!(
            !Scheduler::has_runnable(),
            "切片未耗尽时 current 不得进树 (否则被重复拣选)"
        );
    }
    assert_eq!(proc.vruntime(), slice_real * delta);
    assert_eq!(proc.sum_exec(), slice_real);

    // 3) Expire: 重建切片 + Requeue 入树, 记账从运行账移到树账.
    Scheduler::tick(0);
    assert_eq!(
        proc.slice_left(),
        slice_real,
        "到期后必须重建下一轮切片预算"
    );
    assert!(
        Scheduler::has_runnable(),
        "切片到期后 current 必须以 Requeue 语义回到树账"
    );
    assert!(proc.on_rq(), "重投入树后记账凭证仍须为真");
    assert_eq!(proc.rq_cpu(), 0, "Requeue 不改变队列归属");
    assert_eq!(proc.vruntime(), (slice_real + 1) * delta);
    assert_eq!(proc.sum_exec(), slice_real + 1);

    // 4) 重投入树的任务可被树侧拣回运行账 (Requeue 语义可消费).
    assert_eq!(
        Scheduler::schedule(),
        Some(pid),
        "重投入树的 current 应能由 pick_eevdf 拣回"
    );
    assert!(!Scheduler::has_runnable());
    for _ in 0..slice_real - 1 {
        Scheduler::tick(0);
    }
    assert!(!Scheduler::has_runnable(), "第二个切片推进期间同样不得写树");
    Scheduler::tick(0);
    assert!(
        Scheduler::has_runnable(),
        "第二个切片到期必须再次重投入树 (切片预算不得只生效一次)"
    );
    assert_eq!(proc.vruntime(), (2 * slice_real + 1) * delta);
    assert_eq!(proc.sum_exec(), 2 * slice_real + 1);
    assert_eq!(proc.rq_cpu(), 0);
}

// ============================================================================
// 源码契约 (与运行维互补: 锁定"热路径只动运行账"与"重调度延后"的形状)
// ============================================================================

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("host-tests 必有父目录")
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {e}", path.display()))
}

/// 去掉整行注释, 避免历史名词被误判为现行代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn has(haystack: &str, needle: &str) -> bool {
    let norm = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    norm(haystack).contains(&norm(needle))
}

/// 取 `sig` 所在函数的体 (含签名行), 按花括号配对截取.
fn fn_body(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let bytes: Vec<char> = src[start..].chars().collect();
    let mut depth = 0usize;
    let mut seen_open = false;
    for (i, c) in bytes.iter().enumerate() {
        match c {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    return bytes[..=i].iter().collect();
                }
            }
            _ => {}
        }
    }
    panic!("函数体花括号不配对: {sig}");
}

/// E-04 记账形状: Q16.16 增量 + 运行账 O(1) + floor 单调 + 到期重建切片.
#[test]
fn tick_cfs_helper_keeps_single_time_base() {
    let sched = code_only(&read_src(SCHED));
    assert!(
        !has(&sched, "<< VIRT_SHIFT"),
        "E-04: 调度器不得再有 `<< VIRT_SHIFT` 时基桥 (cfs_vruntime 即 Q16.16)"
    );

    let tick = fn_body(&sched, "pub fn tick(cpu_id: usize)");
    assert!(
        has(&tick, "Self::tick_current_task(per_cpu, current_pid)"),
        "tick 必须把当前任务记账委托给 tick_current_task"
    );
    assert!(
        has(&tick, "mark_resched_pending_local()"),
        "tick 运行于定时器 ISR: 只能在中断退出路径登记重调度"
    );
    assert!(
        !has(&tick, "Self::schedule()"),
        "tick 不得在定时器 ISR 内直接上下文切换"
    );

    let body = fn_body(&sched, "fn tick_current_cfs(");
    for needle in [
        "calc_vruntime_delta_virt(weight)",
        "cfs_rq.advance_running_vruntime(current_pid, vr)",
        "cfs_rq.advance_floor(vr)",
        "p.eevdf_slice_left.store(left",
        "Self::rebuild_eevdf_round(&cfs_rq, current_pid, weight)",
        "cfs_rq.enqueue(current_pid, vr, weight, true)",
    ] {
        assert!(
            has(&body, needle),
            "tick_current_cfs 缺少 E-04 记账要素: {needle}"
        );
    }
    assert!(
        !has(&body, "need_reschedule"),
        "tick_current_cfs 只回报切片是否到期, 重调度标记由调用方置位"
    );
}

/// 热路径不碰树: 运行账增量与 floor 推进必须早于唯一的到期 `enqueue`,
/// 且 Build / Advance 分支在 `enqueue` 之前早退.
#[test]
fn tick_hot_path_writes_only_running_ledger() {
    let sched = code_only(&read_src(SCHED));
    let body = fn_body(&sched, "fn tick_current_cfs(");
    assert!(
        body.matches(".enqueue(").count() == 1,
        "tick 热路径只允许切片到期这一处入树"
    );
    assert!(
        !body.contains(".dequeue(") && !body.contains("reweight("),
        "tick 热路径不得撤账 / 改权重 (每 tick 一次即账目抖动)"
    );
    assert!(
        !body.contains("pick_next(") && !body.contains("leftmost"),
        "tick 不得做树选择 (选择只在 schedule 路径)"
    );
    let running = body
        .find("advance_running_vruntime")
        .expect("缺少运行账增量");
    let expired_gate = body.find("if !expired").expect("缺少到期门闸");
    let enqueue = body.find(".enqueue(").expect("缺少到期重投");
    assert!(
        running < expired_gate && expired_gate < enqueue,
        "执行次序必须是: 运行账增量 -> 到期判定早退 -> 到期重投入树"
    );
}

/// D6①: 旧抢占判据必须已删除; CFS 分支的重调度只由切片到期驱动.
#[test]
fn slice_expiry_is_the_only_cfs_reschedule_trigger() {
    let sched = code_only(&read_src(SCHED));
    assert!(
        !has(&sched, "cfs_should_preempt"),
        "D6①: 旧 vr 差值抢占判据必须删除, 不得保留兼容阈值"
    );
    assert!(
        !has(&sched, "floor() + TARGET_LATENCY_TICKS"),
        "D6①: `vr > floor + TARGET_LATENCY` 让出判据必须由切片预算取代"
    );
    let body = fn_body(&sched, "fn tick_current_task(");
    assert!(
        has(
            &body,
            "else if Self::tick_current_cfs(per_cpu, current_pid)"
        ),
        "CFS 分支必须只由 tick_current_cfs 的到期回报触发重调度"
    );
    let cfs_branch = body
        .split("else if Self::tick_current_cfs")
        .nth(1)
        .expect("CFS 分支必须包住 tick_current_cfs 的调用");
    assert_eq!(
        cfs_branch.matches("need_reschedule.store(true").count(),
        1,
        "CFS 分支内只允许一处重调度请求 (切片到期)"
    );
}

/// 负控制: 契约谓词必须能识别被禁形态 (防止源码重构后契约静默失效).
#[test]
fn negative_control_predicates() {
    let old_tick = "let should_yield = vr > cfs_rq.floor() + TARGET_LATENCY_TICKS;";
    assert!(
        has(old_tick, "floor() + TARGET_LATENCY_TICKS"),
        "负控制: 旧让出判据必须能被 floor + TARGET_LATENCY 谓词识别"
    );
    let bridge = "let vd = vr << VIRT_SHIFT;";
    assert!(
        has(bridge, "<< VIRT_SHIFT"),
        "负控制: 时基桥形态必须能被 `<< VIRT_SHIFT` 谓词识别"
    );
}
