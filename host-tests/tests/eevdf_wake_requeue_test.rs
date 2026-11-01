// SPDX-License-Identifier: MPL-2.0
//! E-05a 回归锁: Wake / Requeue / Migrate 三分语义与 `vd` 唤醒抢占.
//!
//! 运行维覆盖三条生产路径:
//!
//! - **Wake** (`wake_clamps_sleep_credit_to_one_slice`): 睡眠任务的落点取
//!   `max(vr_raw, V_before - slice_virt)`, 睡眠信用上界恰为一个虚拟切片; 切片与
//!   `vd` 当场重建.
//! - **Wake 抢占门闸** (`wake_with_late_deadline_does_not_request_preemption`):
//!   新任务不合格或 `vd` 不早于 current 时不得请求重调度; 早于则必须请求.
//! - **Requeue** (`prev_requeue_carries_partial_budget` /
//!   `prev_requeue_rebuilds_expired_slice`): prev 收尾按切片是否耗尽二分 ——
//!   未耗尽原样携带, 耗尽则按队列真值重建.
//! - **重复 Wake 降级** (`wake_on_queued_task_keeps_round`): 已在可运行记账中的
//!   任务再次收到 Wake 只做幂等重投, 不得重置切片预算.
//!
//! 运行维在真实 per-CPU rq + `PROCESS_TABLE` 上执行 (`Scheduler::tick` /
//! `Scheduler::schedule` / `Scheduler::unblock` 全为生产路径), 文件内串行化.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use edgine::kernel::privileged::proc::cfs::{TARGET_LATENCY_TICKS, VIRT_ONE};
use edgine::kernel::privileged::proc::cpu_queue::current_cpu_queue;
use edgine::kernel::privileged::proc::process::{PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::{BlockReason, ProcessState, Scheduler};

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

    /// 把 `Created` 进程置为 `Ready`, 使 `unblock` 的唤醒判据成立.
    fn set_ready(&self) {
        self.with(|p| {
            let _ = p.set_state_safe(ProcessState::Ready);
        });
    }

    fn vruntime(&self) -> u64 {
        self.with(|p| p.cfs_vruntime.load(Ordering::Acquire))
    }

    fn slice_real(&self) -> u64 {
        self.with(|p| p.eevdf_slice_real.load(Ordering::Acquire))
    }

    fn slice_left(&self) -> u64 {
        self.with(|p| p.eevdf_slice_left.load(Ordering::Acquire))
    }

    fn slice_virt(&self) -> u64 {
        self.with(|p| p.eevdf_slice_virt.load(Ordering::Acquire))
    }

    fn vdeadline(&self) -> u64 {
        self.with(|p| p.eevdf_vdeadline.load(Ordering::Acquire))
    }

    fn on_rq(&self) -> bool {
        self.with(|p| p.cfs_on_rq.load(Ordering::Acquire))
    }

    fn rq_cpu(&self) -> u32 {
        self.with(|p| p.cfs_rq_cpu.load(Ordering::Acquire))
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

/// 测试收尾: 逐个把进程设为 current 再 `block`, 借生产撤账路径清空 rq 记账.
///
/// 必须在 `TestProc` 之后声明 (Rust 逆序 Drop: 本守卫先执行, PCB 后释放),
/// 否则 `block` 找不到进程, 树/运行账会留下指向已释放 pid 的幽灵节点.
struct RqGuard {
    pids: Vec<u32>,
}

impl RqGuard {
    fn new(pids: &[u32]) -> Self {
        Self {
            pids: pids.to_vec(),
        }
    }
}

impl Drop for RqGuard {
    fn drop(&mut self) {
        for &pid in self.pids.iter().rev() {
            Scheduler::set_current(pid);
            Scheduler::block(BlockReason::Unknown);
        }
        Scheduler::set_current(0);
    }
}

/// Wake: 睡眠信用上界恰为一个虚拟切片, 落点 `V_before - slice_virt`.
///
/// B 的入队落点记为 `base` (floor 跨用例单调累积, 不能假定为 0). 队列构成:
/// B 运行至 `base+20V` (floor=`base+20V`), C 在树上位于 `base+50V`
/// ⇒ `V_before=base+35V`. A 的 raw vr 为 0, 权重 1024, `W_inc=3072`
/// ⇒ `slice_real=20`, `slice_virt=20V` ⇒ 落点 `base+15V` (高于 IC1 下限
/// `floor - 8V = base+12V`, 故观测量就是睡眠信用本身, 不是 floor 钳制).
#[test]
fn wake_clamps_sleep_credit_to_one_slice() {
    let _serial = lock_rq();
    let _ = current_cpu_queue().take_need_reschedule();
    let b = TestProc::new("e05-wake-b");
    let c = TestProc::new("e05-wake-c");
    let a = TestProc::new("e05-wake-a");
    let _guard = RqGuard::new(&[b.pid, c.pid, a.pid]);

    // B 上核并推进 20 tick: 运行账 vr=base+20V, 切片剩 40.
    Scheduler::add_to_run_queue(b.pid);
    // floor 跨用例单调累积 (RqGuard 不清零), 全部期望以队列落点为基准.
    let base = b.vruntime();
    assert_eq!(b.slice_left(), TARGET_LATENCY_TICKS, "Wake 入队即建好切片");
    Scheduler::set_current(b.pid);
    assert_eq!(Scheduler::schedule(), Some(b.pid));
    for _ in 0..20 {
        Scheduler::tick(0);
    }
    assert_eq!(b.vruntime(), base + 20 * VIRT_ONE);
    assert_eq!(b.slice_left(), TARGET_LATENCY_TICKS - 20);

    // C 以 raw=base+50V 入树: 不触发睡眠信用 (负 lag 保留), 但把 V 抬到 base+35V.
    c.with(|p| {
        p.cfs_vruntime
            .store(base + 50 * VIRT_ONE, Ordering::Release);
    });
    Scheduler::add_to_run_queue(c.pid);
    assert_eq!(
        c.vruntime(),
        base + 50 * VIRT_ONE,
        "raw 高于 V 时必须保留欠账"
    );
    assert_eq!(c.vdeadline(), base + 80 * VIRT_ONE);

    // A 睡醒: 信用上界 = 一个虚拟切片.
    a.set_ready();
    Scheduler::unblock(a.pid);
    assert_eq!(
        a.vruntime(),
        base + 15 * VIRT_ONE,
        "Wake 落点必须是 V_before(base+35V) - slice_virt(20V)"
    );
    assert_eq!(a.slice_real(), 20, "W_inc=3072 时默认真实切片应为 20");
    assert_eq!(a.slice_left(), 20, "Wake 必须当场重建整轮切片");
    assert_eq!(a.slice_virt(), 20 * VIRT_ONE);
    assert_eq!(
        a.vdeadline(),
        base + 35 * VIRT_ONE,
        "vd = placed + slice_virt"
    );
    assert!(a.on_rq(), "Wake 成功后必须带记账凭证");
    assert_eq!(a.rq_cpu(), 0);
    assert_eq!(
        Scheduler::current(),
        Some(b.pid),
        "unblock 不直接切换上下文"
    );
    assert!(
        current_cpu_queue().take_need_reschedule(),
        "A 合格且 vd(35V) 早于 current vd(60V) ⇒ 必须请求唤醒抢占"
    );
}

/// Wake 抢占门闸: 晚截止期 / 不合格的新任务不得打扰在跑的 current.
#[test]
fn wake_with_late_deadline_does_not_request_preemption() {
    let _serial = lock_rq();
    let _ = current_cpu_queue().take_need_reschedule();
    let b = TestProc::new("e05-gate-b");
    let a = TestProc::new("e05-gate-a");
    let _guard = RqGuard::new(&[b.pid, a.pid]);

    Scheduler::add_to_run_queue(b.pid);
    let base = b.vruntime();
    Scheduler::set_current(b.pid);
    assert_eq!(Scheduler::schedule(), Some(b.pid));

    // 睡眠欠账远高于 V: Wake 不补偿 (placed = raw), vd 落后于 current.
    a.with(|p| {
        p.cfs_vruntime
            .store(base + 500 * VIRT_ONE, Ordering::Release);
    });
    a.set_ready();
    let _ = current_cpu_queue().take_need_reschedule();
    Scheduler::unblock(a.pid);

    assert_eq!(
        a.vruntime(),
        base + 500 * VIRT_ONE,
        "欠账不得被睡眠时间洗掉"
    );
    assert_eq!(a.slice_real(), 30, "W_inc=2048 时默认真实切片应为 30");
    assert_eq!(a.slice_left(), 30);
    assert_eq!(a.vdeadline(), base + 530 * VIRT_ONE);
    assert!(a.on_rq(), "不抢占不等于不入队");
    assert!(
        !current_cpu_queue().take_need_reschedule(),
        "A 不合格 (vr 远高于 V) 且 vd 晚于 current ⇒ 不得请求重调度"
    );
}

/// Requeue: 已在可运行记账中的任务再次收到 Wake 时降级为原样重投.
#[test]
fn wake_on_queued_task_keeps_round() {
    let _serial = lock_rq();
    let _ = current_cpu_queue().take_need_reschedule();
    let h = TestProc::new("e05-dup");
    let _guard = RqGuard::new(&[h.pid]);

    Scheduler::add_to_run_queue(h.pid);
    // 落点取自队列本身: floor 是跨用例持久的单调下界 (RqGuard 不清零),
    // 硬编码 vr 会被 enqueue 的 IC1 钳制顶高, 与本次要验的重投语义无关.
    let base = h.vruntime();
    let deadline = base + TARGET_LATENCY_TICKS * VIRT_ONE;
    // 人为构造"本轮已消耗一部分"的状态 (Wake 只会由首次入队产生).
    h.with(|p| {
        p.eevdf_slice_left.store(17, Ordering::Release);
        p.eevdf_vdeadline.store(deadline, Ordering::Release);
    });

    Scheduler::add(h.pid);

    assert_eq!(h.vruntime(), base, "重复 Wake 不得改写落点");
    assert_eq!(
        h.slice_real(),
        TARGET_LATENCY_TICKS,
        "重复 Wake 不得重建切片"
    );
    assert_eq!(h.slice_left(), 17, "重复 Wake 不得重置剩余预算");
    assert_eq!(h.slice_virt(), TARGET_LATENCY_TICKS * VIRT_ONE);
    assert_eq!(h.vdeadline(), deadline);
    assert!(h.on_rq());
    assert_eq!(h.rq_cpu(), 0);
}

/// Requeue: prev 收尾必须携带未耗尽的切片 (D8①).
#[test]
fn prev_requeue_carries_partial_budget() {
    let _serial = lock_rq();
    let _ = current_cpu_queue().take_need_reschedule();
    let d = TestProc::new("e05-carry-d");
    let e = TestProc::new("e05-carry-e");
    let _guard = RqGuard::new(&[d.pid, e.pid]);

    // D 上核后消耗 5 tick.
    Scheduler::add_to_run_queue(d.pid);
    // floor 是跨用例持久的单调下界 (RqGuard 不清零), 落点从队列真值取基准,
    // 否则前序用例抬高 floor 后硬编码的 vr 期望会被 IC1 钳制顶偏.
    let base = d.vruntime();
    Scheduler::set_current(d.pid);
    assert_eq!(Scheduler::schedule(), Some(d.pid));
    for _ in 0..5 {
        Scheduler::tick(0);
    }
    assert_eq!(d.vruntime(), base + 5 * VIRT_ONE);
    assert_eq!(d.slice_left(), TARGET_LATENCY_TICKS - 5);

    // E 以请求切片 10 入队: vd 早于 D 的 base+60V ⇒ 唤醒抢占.
    e.with(|p| p.eevdf_requested_slice.store(10, Ordering::Release));
    e.set_ready();
    Scheduler::unblock(e.pid);
    assert_eq!(e.slice_left(), 10, "D8③ 请求切片应直接生效");

    assert_eq!(Scheduler::schedule(), Some(e.pid), "E 应凭更早的 vd 胜出");
    assert_eq!(Scheduler::current(), Some(e.pid));
    assert_eq!(
        d.vruntime(),
        base + 5 * VIRT_ONE,
        "Requeue 不得改写已消耗的 vr"
    );
    assert_eq!(
        d.slice_left(),
        TARGET_LATENCY_TICKS - 5,
        "Requeue 携带未耗尽切片, 不得重置预算"
    );
    assert_eq!(
        d.vdeadline(),
        base + 60 * VIRT_ONE,
        "携带语义下 vd 随 vr 平移"
    );
    assert!(d.on_rq(), "prev 收尾后必须回到树账");
    assert_eq!(d.rq_cpu(), 0);
}

/// Requeue: 切片已耗尽时按队列真值重建 (W 已含本任务, 不得重复加权重).
#[test]
fn prev_requeue_rebuilds_expired_slice() {
    let _serial = lock_rq();
    let _ = current_cpu_queue().take_need_reschedule();
    let f = TestProc::new("e05-rebuild-f");
    let g = TestProc::new("e05-rebuild-g");
    let _guard = RqGuard::new(&[f.pid, g.pid]);

    Scheduler::add_to_run_queue(f.pid);
    // 同 `prev_requeue_carries_partial_budget`: 落点取队列真值, 避免跨用例 floor 影响.
    let base = f.vruntime();
    Scheduler::set_current(f.pid);
    assert_eq!(Scheduler::schedule(), Some(f.pid));
    // 模拟本轮已耗尽 (tick 的到期分支之外, prev 收尾同样要补建).
    f.with(|p| p.eevdf_slice_left.store(0, Ordering::Release));

    g.with(|p| p.eevdf_requested_slice.store(10, Ordering::Release));
    g.set_ready();
    Scheduler::unblock(g.pid);
    assert_eq!(Scheduler::schedule(), Some(g.pid));

    assert_eq!(f.vruntime(), base);
    assert_eq!(
        f.slice_real(),
        30,
        "重建时 W 已含 F 自身 (与 G 合计 2048) ⇒ 默认切片 30, 不得按 1024 算成 60"
    );
    assert_eq!(f.slice_left(), 30);
    assert_eq!(f.slice_virt(), 30 * VIRT_ONE);
    assert_eq!(
        f.vdeadline(),
        base + 30 * VIRT_ONE,
        "重建后 vd = vr + slice_virt"
    );
    assert!(f.on_rq());
}

// ============================================================================
// 源码契约 (与运行维互补: 锁定三分语义的接线形状与门闸位置)
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

fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn has(haystack: &str, needle: &str) -> bool {
    norm(haystack).contains(&norm(needle))
}

fn pos(haystack: &str, needle: &str) -> usize {
    norm(haystack)
        .find(&norm(needle))
        .unwrap_or_else(|| panic!("未找到: {needle}"))
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

/// 三分语义接线: Wake 只在唤醒面, prev / 屏障补投 / RT 降级一律 Requeue.
#[test]
fn placement_kinds_are_wired_at_the_right_entries() {
    let sched = code_only(&read_src(SCHED));

    let unblock = fn_body(&sched, "pub fn unblock");
    assert!(
        has(&unblock, "PlacementKind::Wake"),
        "E-05a: unblock 是唤醒面, 必须按 Wake 入队 (睡眠信用 + 重建切片)"
    );
    let landed = pos(&unblock, "if !outcome.landed");
    let need = pos(&unblock, "if !outcome.need_resched");
    let mark = pos(&unblock, "mark_resched_pending_local()");
    assert!(
        landed < need && need < mark,
        "E-05a: unblock 必须按搬定结果两段早退, 再走统一的重调度登记入口"
    );

    let requeue = fn_body(&sched, "fn requeue_prev");
    assert!(
        has(&requeue, "PlacementKind::Requeue"),
        "E-05a: prev 收尾是原样重投 (Requeue), 不得重发睡眠信用"
    );
    let reap = fn_body(&sched, "fn reap_off_cpu");
    assert!(
        has(&reap, "PlacementKind::Requeue"),
        "E-05a: 屏障补投是原样重投 (Requeue)"
    );
    let pick = fn_body(&sched, "fn pick_next_task");
    assert!(
        has(&pick, "Self::cfs_enqueue(rt_pid, PlacementKind::Requeue)"),
        "E-05a: RT 降级入 CFS 不得被当作唤醒 (重置切片即打断其本轮预算)"
    );

    let place = fn_body(&sched, "fn place_locked");
    let plan = pos(&place, "plan_placement(");
    let enqueue = pos(&place, "let placed = match src");
    assert!(plan < enqueue, "E-05a: 落点/切片计划必须先于唯一入树点");
    assert!(
        has(&place, "kind == PlacementKind::Wake && Self::wake_preempts"),
        "E-05a: 只有 Wake 语义参与唤醒抢占判决"
    );
}

/// 唤醒抢占判据: 合格性用入队后的 W/S, 截止期比较用 current 视图.
#[test]
fn wake_preemption_gate_uses_post_enqueue_eligibility_and_deadline() {
    let sched = code_only(&read_src(SCHED));
    let body = fn_body(&sched, "fn wake_preempts");

    assert!(
        has(
            &body,
            "is_eligible(vruntime, rq.runnable_weight(), rq.sum_w_vr())"
        ),
        "E-05a: 合格性必须用入队后的 W/S (含新任务) 判定"
    );
    assert!(
        has(&body, "eligible && vdeadline < effective_deadline"),
        "E-05a: 抢占判据 = 合格 && vd 更早"
    );
    assert!(
        has(&body, "SchedPolicy::Normal =>"),
        "E-05a: 只有普通 CFS current 参与 vd 比较"
    );
    assert!(
        has(&body, "SchedPolicy::Idle => true"),
        "E-05a: idle 上核必须被唤醒任务接管"
    );
    assert!(
        has(&body, "_ => false"),
        "E-05a: RT / DL current 不因 CFS 唤醒被抢占 (选择顺序本就 DL → RT → CFS)"
    );
}

/// 负控制: 旧形态 (`unblock` 无条件请求重调度 / 旧抢占阈值) 必须能被识别.
#[test]
fn negative_control_predicates() {
    let unconditional = "let outcome = Self::cfs_enqueue_to(pid, cpu, PlacementKind::Wake);\n\
                         crate::privileged::proc::cpu_queue::mark_resched_pending_local();";
    assert!(
        !has(unconditional, "if !outcome.need_resched"),
        "负控制: 缺少门闸的唤醒路径必须能被 `need_resched` 谓词识别"
    );
    let old_threshold = "let should_yield = vr > cfs_rq.floor() + TARGET_LATENCY_TICKS;";
    assert!(
        has(old_threshold, "TARGET_LATENCY_TICKS"),
        "负控制: 旧阈值形态必须能被静态谓词识别"
    );
}
