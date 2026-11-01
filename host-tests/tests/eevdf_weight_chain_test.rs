// SPDX-License-Identifier: MPL-2.0
//! E-02 权重链路回归锁: nice → weight → Δvr → rq 账目 → 切片/vd.
//!
//! 覆盖:
//! - 生产 `nice_to_weight` / `calc_vruntime_delta_virt` 与 E-01 模型全 40 档对拍;
//! - `setpriority` / `getpriority` 端到端 (显式 pid): 真实 nice、派生权重、
//!   粗粒度 `ProcessPriority` 三者同步; 负数 nice 不再被误判为错误;
//! - 在队任务改权重经真实 per-CPU rq 验证: `reweight` 后切片与 `vd` 按新权重
//!   重建, 归属不变 (单测进程内串行, 测试结束经 `block` 撤出运行集);
//! - 源码契约: `set_nice` 在同一 rq 锁内先改账再重建切片, functions 层走
//!   `Scheduler::set_nice` 单一入口.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use edgine::kernel::functions::proc::priority::{
    apply_nice, getpriority_syscall, setpriority_syscall,
};
use edgine::kernel::privileged::proc::cfs::{VIRT_SHIFT, calc_vruntime_delta_virt, nice_to_weight};
use edgine::kernel::privileged::proc::process::{NO_RQ_CPU, PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::{BlockReason, ProcessPriority, Scheduler};
use edgine::kernel::privileged::syscall::Errno;
use edgine_host_tests::eevdf_model::{
    VIRT_ONE, clamp_requested_slice as model_clamp_requested_slice,
    default_slice_real as model_default_slice_real, delta_vruntime as model_delta_vruntime,
    nice_to_weight as model_nice_to_weight, slice_virt_of as model_slice_virt_of,
};

const PRIO_PROCESS: i32 = 0;

/// 在队路径会写全局 per-CPU rq 与 current, 文件内串行化.
static RQ_LOCK: Mutex<()> = Mutex::new(());

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

    fn with_mut<R>(&self, f: impl FnOnce(&mut Process) -> R) -> R {
        PROCESS_TABLE
            .with_process_mut(self.pid, f)
            .expect("测试进程应在表中")
    }

    fn nice(&self) -> i8 {
        self.with(Process::get_nice)
    }

    fn weight(&self) -> u64 {
        self.with(|p| p.cfs_weight.load(Ordering::Acquire))
    }

    fn rq_cpu(&self) -> u32 {
        self.with(|p| p.cfs_rq_cpu.load(Ordering::Acquire))
    }

    fn on_rq(&self) -> bool {
        self.with(|p| p.cfs_on_rq.load(Ordering::Acquire))
    }

    fn eevdf(&self) -> [u64; 4] {
        self.with(|p| {
            [
                p.eevdf_slice_real.load(Ordering::Acquire),
                p.eevdf_slice_virt.load(Ordering::Acquire),
                p.eevdf_slice_left.load(Ordering::Acquire),
                p.eevdf_vdeadline.load(Ordering::Acquire),
            ]
        })
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

/// 在队任务清理: 恢复 current=0 并把任务撤出全局运行集.
struct RqGuard;

impl RqGuard {
    fn enqueue(pid: u32) -> Self {
        Scheduler::add_to_run_queue(pid);
        Scheduler::set_current(pid);
        Self
    }
}

impl Drop for RqGuard {
    fn drop(&mut self) {
        Scheduler::block(BlockReason::Unknown);
        Scheduler::set_current(0);
    }
}

fn lock_rq() -> std::sync::MutexGuard<'static, ()> {
    RQ_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 生产权重链全 40 档: 与 E-01 模型逐项一致, 且 Δvr 随 nice 严格递增.
#[test]
fn weight_chain_all_40_levels_match_model() {
    let mut previous = 0u64;
    for nice in -20i8..=19 {
        let weight = nice_to_weight(nice);
        assert_eq!(
            weight,
            model_nice_to_weight(nice),
            "nice {nice} 的权重与模型不一致"
        );
        let delta = calc_vruntime_delta_virt(weight);
        assert_eq!(
            delta,
            model_delta_vruntime(weight),
            "nice {nice} (w={weight}) 的 Q16.16 增量与模型不一致"
        );
        assert!(
            delta > previous,
            "nice {nice} 的 Δvr={delta} 未随 nice 严格递增 (上一档 {previous})"
        );
        previous = delta;
    }
    assert_eq!(calc_vruntime_delta_virt(nice_to_weight(-20)), 756);
    assert_eq!(calc_vruntime_delta_virt(nice_to_weight(0)), VIRT_ONE);
    assert_eq!(calc_vruntime_delta_virt(nice_to_weight(19)), 4_473_924);
}

/// 负控制: w=0 退化、越界 nice 收敛到端点, 不得出现 0 增量.
#[test]
fn weight_chain_boundaries_degenerate_like_model() {
    assert_eq!(calc_vruntime_delta_virt(0), VIRT_ONE, "w=0 按 NICE0 退化");
    assert_eq!(nice_to_weight(-21), nice_to_weight(-20));
    assert_eq!(nice_to_weight(20), nice_to_weight(19));
    assert_eq!(nice_to_weight(-20), 88_761);
}

/// 生产 setpriority/getpriority: 真实 nice、权重与粗粒度优先级同步.
#[test]
fn setpriority_updates_nice_weight_and_coarse_priority() {
    let proc = TestProc::new("e02-priority");

    assert_eq!(setpriority_syscall(PRIO_PROCESS, proc.pid, -15), 0);
    assert_eq!(getpriority_syscall(PRIO_PROCESS, proc.pid), -15);
    assert_eq!(proc.nice(), -15);
    assert_eq!(proc.weight(), nice_to_weight(-15));
    assert_eq!(proc.with(Process::get_priority), ProcessPriority::RealTime);

    assert_eq!(setpriority_syscall(PRIO_PROCESS, proc.pid, 5), 0);
    assert_eq!(getpriority_syscall(PRIO_PROCESS, proc.pid), 5);
    assert_eq!(proc.nice(), 5);
    assert_eq!(proc.weight(), nice_to_weight(5));
    assert_eq!(proc.with(Process::get_priority), ProcessPriority::Normal);

    assert_eq!(setpriority_syscall(PRIO_PROCESS, proc.pid, 19), 0);
    assert_eq!(getpriority_syscall(PRIO_PROCESS, proc.pid), 19);
    assert_eq!(proc.nice(), 19);
    assert_eq!(proc.weight(), nice_to_weight(19));
    assert_eq!(proc.with(Process::get_priority), ProcessPriority::Idle);
}

/// 负数 nice 是合法值: 增量路径不得把它当成 errno (旧 `nice_syscall` 缺陷).
#[test]
fn negative_nice_is_not_an_error() {
    let proc = TestProc::new("e02-negative-nice");
    assert_eq!(setpriority_syscall(PRIO_PROCESS, proc.pid, -10), 0);

    assert_eq!(apply_nice(proc.pid, -5), -15);
    assert_eq!(proc.nice(), -15);
    assert_eq!(apply_nice(proc.pid, 2), -13);
    assert_eq!(proc.nice(), -13);
    assert_eq!(apply_nice(proc.pid, i32::MAX), 19, "增量溢出饱和后夹取");
    assert_eq!(proc.nice(), 19);
}

/// 参数边界与负控制: 越界夹取、非法 which、不存在的 pid.
#[test]
fn priority_syscall_negative_controls() {
    let proc = TestProc::new("e02-negative-controls");

    assert_eq!(setpriority_syscall(PRIO_PROCESS, proc.pid, -100), 0);
    assert_eq!(proc.nice(), -20);
    assert_eq!(setpriority_syscall(PRIO_PROCESS, proc.pid, 100), 0);
    assert_eq!(proc.nice(), 19);

    let invalid_which = 1;
    assert_eq!(
        setpriority_syscall(invalid_which, proc.pid, 0),
        Errno::Einval.as_ret()
    );
    assert_eq!(
        getpriority_syscall(invalid_which, proc.pid),
        Errno::Einval.as_ret()
    );

    let missing = u32::MAX;
    assert_eq!(
        setpriority_syscall(PRIO_PROCESS, missing, 0),
        Errno::Esrch.as_ret()
    );
    assert_eq!(
        getpriority_syscall(PRIO_PROCESS, missing),
        Errno::Esrch.as_ret()
    );
    assert_eq!(apply_nice(missing, 0), Errno::Esrch.as_ret());
}

/// 不在可运行集: 只写 nice/weight, 不建切片, 归属保持 `NO_RQ_CPU`.
#[test]
fn off_rq_set_nice_leaves_no_eevdf_accounting() {
    let proc = TestProc::new("e02-off-rq");
    assert_eq!(proc.rq_cpu(), NO_RQ_CPU);
    assert!(Scheduler::set_nice(proc.pid, 7));

    assert_eq!(proc.nice(), 7);
    assert_eq!(proc.weight(), nice_to_weight(7));
    assert_eq!(proc.rq_cpu(), NO_RQ_CPU, "认领凭证必须回滚归属");
    assert!(!proc.on_rq());
    assert_eq!(proc.eevdf(), [0, 0, 0, 0]);
}

/// 在队路径: 同一 rq 锁内改账并重建切片/vd; 请求切片按 D8③ 夹取.
#[test]
fn on_rq_set_nice_reweights_and_rebuilds_slice() {
    let _serial = lock_rq();
    let proc = TestProc::new("e02-on-rq");
    // 非零 vr 让 `vd` 同时验证 E-04 之前的 Q16.16 时基桥.
    proc.with_mut(|p| p.cfs_vruntime.store(100, Ordering::Release));

    let _rq = RqGuard::enqueue(proc.pid);
    assert!(proc.on_rq(), "任务应已进入全局 CFS 运行记账");
    assert_eq!(proc.rq_cpu(), 0);

    assert!(Scheduler::set_nice(proc.pid, -20));
    let weight = nice_to_weight(-20);
    let slice_real = model_default_slice_real(weight, weight);
    let slice_virt = model_slice_virt_of(slice_real, weight);
    let vr_virt = 100u64 << VIRT_SHIFT;
    assert_eq!(proc.weight(), weight);
    assert_eq!(
        proc.eevdf(),
        [slice_real, slice_virt, slice_real, vr_virt + slice_virt],
        "权重变更必须在同一 rq 锁内重建 slice_real/slice_virt/slice_left/vd"
    );
    assert_eq!(proc.rq_cpu(), 0, "在队改权重不得改变归属");
    assert!(proc.on_rq());

    // D8③: 请求切片 30 生效并按新权重换算虚拟切片.
    proc.with_mut(|p| p.eevdf_requested_slice.store(30, Ordering::Release));
    assert!(Scheduler::set_nice(proc.pid, 0));
    let weight = nice_to_weight(0);
    let slice_real = model_clamp_requested_slice(30);
    let slice_virt = model_slice_virt_of(slice_real, weight);
    assert_eq!(
        proc.eevdf(),
        [slice_real, slice_virt, slice_real, vr_virt + slice_virt]
    );
    assert_eq!(proc.eevdf()[0], 30, "请求切片 30 应在夹取区间内原样生效");

    // 下界夹取: 请求 1 tick 收敛到 MIN_GRANULARITY.
    proc.with_mut(|p| p.eevdf_requested_slice.store(1, Ordering::Release));
    assert!(Scheduler::set_nice(proc.pid, 0));
    assert_eq!(proc.eevdf()[0], 8);
    assert_eq!(proc.eevdf()[2], 8);

    // 上界夹取: 请求 1000 tick 收敛到 TARGET_LATENCY.
    proc.with_mut(|p| p.eevdf_requested_slice.store(1000, Ordering::Release));
    assert!(Scheduler::set_nice(proc.pid, 19));
    let weight = nice_to_weight(19);
    let slice_virt = model_slice_virt_of(60, weight);
    assert_eq!(proc.eevdf()[0], 60);
    assert_eq!(proc.eevdf()[2], 60);
    assert_eq!(
        proc.eevdf()[3],
        vr_virt + slice_virt,
        "vd 必须用新权重的 Δvr 与虚拟切片重建"
    );
}

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

fn fn_body<'a>(src: &'a str, needle: &str) -> &'a str {
    let start = src
        .find(needle)
        .unwrap_or_else(|| panic!("源码中找不到 {needle}"));
    let open = start + src[start..].find('{').expect("函数体缺少左花括号");
    let mut depth = 0usize;
    for (offset, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[start..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("{needle} 的函数体未闭合");
}

/// 源码契约: `set_nice` 在同一 rq 锁内先改账 (`reweight`) 再重建切片.
#[test]
fn source_set_nice_reweights_inside_rq_lock() {
    let src = read_src("src/kernel/privileged/proc/scheduler.rs");
    let body = fn_body(&src, "pub fn set_nice(pid: Pid, nice: i8) -> bool");
    assert!(
        body.contains("claim_rq_blocking(pid)"),
        "set_nice 必须经等待式认领入口串行化迁移"
    );
    let lock = body
        .find(".cfs_rq.lock()")
        .expect("set_nice 在队分支必须持有 rq 锁");
    let tail = &body[lock..];
    assert!(
        tail.find("PROCESS_TABLE").is_some(),
        "改权重的进程表访问必须在 rq 锁之后 (锁序 cfs_rq -> PROCESS_TABLE)"
    );
    assert!(
        tail.find("rq.reweight(pid, new_weight)").is_some(),
        "在队任务必须经 E-03 的 reweight 入口修正账目"
    );
    assert!(
        tail.find("rebuild_eevdf_round(&rq, pid, new_weight)")
            .is_some(),
        "改权重后必须在同一 rq 锁内重建本轮切片/vd"
    );
    assert!(
        !body.contains("SIMPLIFIED"),
        "旧权重账目偏差的简化标记必须随修复删除"
    );

    let rebuild = fn_body(
        &src,
        "fn rebuild_eevdf_round(rq: &CfsRunQueue, pid: Pid, weight: u64)",
    );
    for needle in [
        "effective_slice_real(",
        "slice_virt_of(",
        "virtual_deadline(",
        "eevdf_slice_left",
        "eevdf_vdeadline",
    ] {
        assert!(
            rebuild.contains(needle),
            "rebuild_eevdf_round 缺少 {needle}"
        );
    }
}

/// 源码契约: functions 层只经 `Scheduler::set_nice` 单一入口写权重/nice.
#[test]
fn source_priority_uses_scheduler_entry() {
    let src = read_src("src/kernel/functions/proc/priority.rs");
    assert!(
        src.contains("Scheduler::set_nice(pid"),
        "setpriority 必须经 Scheduler::set_nice 更新 nice/权重"
    );
    assert!(
        src.contains("Process::get_nice"),
        "getpriority/nice 必须读取真实 nice"
    );
    assert!(
        !src.contains("fn priority_to_nice"),
        "粗粒度反查 (priority_to_nice) 已无调用, 按 F9 删除"
    );
    assert!(
        !src.contains("current_nice < 0 && current_nice != -38"),
        "负数 nice 误判为错误的旧分支必须删除"
    );
}
