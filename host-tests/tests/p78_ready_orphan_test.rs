// SPDX-License-Identifier: MPL-2.0
// P-78 回归锁: tick 必须兜底恢复「Ready 但既不在队也不在核上」的无主任务.
//
// 缺陷机制 (x86_64 满载启动静默挂死的第二形态, 与 wait4 忙轮询独立):
// 唤醒方在 `place_barrier` 延迟投送支只把状态翻成 Ready 并把落地交给占用者核的
// `reap_off_cpu` 补投; 若补投腿与并发唤醒都因队列归属认领 (`MIGRATE_PENDING`
// 持有方被宿主调度器抢占) 超时落空, 任务永久停在 Ready + rq_cpu=NO_RQ_CPU,
// 两核各自空转, 心跳持续而用户态永久静默.
//
// 本锁钉住五条实现事实:
//   1. 判据 `ready_orphan_due` 只收 Normal + Ready + 不在队 + 不在核的任务
//      (idle 策略为 Idle 不入队, 必须被策略面排除; 屏障在位时 on_cpu=true 也不收,
//      否则会把"上下文尚未保存完"的任务塞进树);
//   2. 从未入过队 (`eevdf_placed_once == false`) 的 Ready 任务不属恢复对象:
//      首个用户进程按 `enter_user_process` 契约**必须**留在队外, 创建中/待激活
//      的任务也处于该形态 —— 恢复网若补投它们, 首进程会被两核交叠运行 (P-78
//      实测三重故障);
//   3. 恢复走 `PlacementKind::Requeue` —— 任务本就 Ready, 不得按 Wake 重发睡眠信用;
//   4. 扫描仍是一次表锁内的有界快照, 补投动作留在表锁外 (锁序 8 → 7);
//   5. `Scheduler::tick` 每 tick 调用恢复入口 (最迟一个 tick 内被重新看见).

use std::fs;
use std::sync::atomic::Ordering;
use std::sync::{Mutex as StdMutex, MutexGuard};

use edgine::kernel::privileged::proc::process::{PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::{ProcessState, SchedPolicy, Scheduler};

const SCHEDULER: &str = "../src/kernel/privileged/proc/scheduler.rs";

/// 全局进程表是进程内共享态, 文件内测试串行执行。
static TABLE_LOCK: StdMutex<()> = StdMutex::new(());

fn lock_table() -> MutexGuard<'static, ()> {
    TABLE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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

fn standalone(pid: u32) -> Process {
    Process::new(pid, "p78-orphan-test", None)
}

fn ready(proc: &Process) {
    proc.state
        .store(ProcessState::Ready as u32, Ordering::SeqCst);
}

#[test]
fn ready_orphan_due_selects_only_unqueued_offcore_normal_ready() {
    let proc = standalone(101);
    ready(&proc);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        None,
        "从未入过队的 Ready 任务不属恢复对象 (首个用户进程/fork 待激活形态)"
    );
    proc.eevdf_placed_once.store(true, Ordering::SeqCst);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        Some(101),
        "Ready + 曾在队 + 不在队 + 不在核 + Normal 必须入选 (无主可运行任务)"
    );

    // 已在运行队列记账: 不是无主任务
    proc.eevdf_on_rq.store(true, Ordering::SeqCst);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        None,
        "在树的 Ready 任务不得重复补投"
    );
    proc.eevdf_on_rq.store(false, Ordering::SeqCst);

    // 投送屏障在位 (上下文尚未保存完): 进树即两核交叠
    proc.eevdf_on_cpu.store(true, Ordering::SeqCst);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        None,
        "on_cpu 屏障在位的任务不得进树 (P-21 语义)"
    );
    proc.eevdf_on_cpu.store(false, Ordering::SeqCst);

    // 本核 current 不参与扫描
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 101),
        None,
        "本核在跑的任务不参与恢复扫描"
    );

    // 非 Ready 状态 (Blocked / Running / Zombie) 不入选
    proc.state
        .store(ProcessState::Blocked as u32, Ordering::SeqCst);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        None,
        "Blocked 任务归睡眠/唤醒路径, 不得由恢复扫描入队"
    );
    proc.state
        .store(ProcessState::Zombie as u32, Ordering::SeqCst);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        None,
        "Zombie 不得入队"
    );
}

#[test]
fn ready_orphan_due_excludes_non_normal_policies() {
    // idle 任务同为 Ready 且永不入队 (策略 Idle), 必须被策略面排除.
    let idle = standalone(102);
    ready(&idle);
    idle.eevdf_placed_once.store(true, Ordering::SeqCst);
    idle.set_sched_policy(SchedPolicy::Idle);
    assert_eq!(
        Scheduler::ready_orphan_due(&idle, 0),
        None,
        "idle 任务不得被恢复扫描塞进 EEVDF 树"
    );

    let rt = standalone(103);
    ready(&rt);
    rt.eevdf_placed_once.store(true, Ordering::SeqCst);
    rt.set_sched_policy(SchedPolicy::Fifo);
    assert_eq!(
        Scheduler::ready_orphan_due(&rt, 0),
        None,
        "实时策略任务不归 EEVDF 树, 恢复扫描只覆盖 Normal"
    );
}

/// 已登记进全局进程表的测试 TCB; Drop 时回收 (幂等)。
struct TableProc {
    pid: u32,
}

impl TableProc {
    fn armed(pid: u32, state: ProcessState, on_rq: bool, on_cpu: bool) -> Self {
        let proc = Process::new(pid, "p78-orphan-table", None);
        proc.state.store(state as u32, Ordering::SeqCst);
        proc.eevdf_on_rq.store(on_rq, Ordering::SeqCst);
        proc.eevdf_on_cpu.store(on_cpu, Ordering::SeqCst);
        // 默认「曾在队」: 所有既有形态都是入过队后掉队/在册的任务.
        proc.eevdf_placed_once.store(true, Ordering::SeqCst);
        assert!(
            PROCESS_TABLE.insert(Box::into_raw(Box::new(proc))),
            "测试 TCB 登记失败 (pid={pid})"
        );
        Self { pid }
    }

    /// 从未入过队的 Ready 任务 (首个用户进程 / fork 待激活形态).
    fn never_placed(pid: u32) -> Self {
        let proc = Process::new(pid, "p78-orphan-never", None);
        proc.state
            .store(ProcessState::Ready as u32, Ordering::SeqCst);
        assert!(
            PROCESS_TABLE.insert(Box::into_raw(Box::new(proc))),
            "测试 TCB 登记失败 (pid={pid})"
        );
        Self { pid }
    }
}

impl Drop for TableProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

#[test]
fn scan_into_selects_only_the_real_orphan() {
    let _serial = lock_table();
    let pids: Vec<u32> = (0..5)
        .map(|_| PROCESS_TABLE.allocate_pid().expect("pid 分配失败"))
        .collect();

    let _orphan = TableProc::armed(pids[0], ProcessState::Ready, false, false);
    let _queued = TableProc::armed(pids[1], ProcessState::Ready, true, false);
    let _running = TableProc::armed(pids[2], ProcessState::Ready, false, true);
    let _blocked = TableProc::armed(pids[3], ProcessState::Blocked, false, false);
    let _never = TableProc::never_placed(pids[4]);

    let mut picks: [u32; 8] = [0; 8];
    let hits = PROCESS_TABLE.scan_into(&mut picks, |proc| Scheduler::ready_orphan_due(proc, 0));

    assert_eq!(hits, 1, "五种形态里只有「曾在队且掉队」的 Ready 任务入选");
    assert_eq!(picks[0], pids[0], "入选的必须是无主 Ready 任务");
    assert!(
        !picks[..hits].contains(&pids[4]),
        "从未入过队的 Ready 任务不得被恢复网补投 (首个用户进程会长驻该形态)"
    );
}

#[test]
fn tick_runs_orphan_recovery_and_requeues_outside_the_scan_lock() {
    let src = code_only(&read(SCHEDULER));

    // 判据面: 三个排除维度必须留在判据函数体内
    let due_at = src
        .find("pub fn ready_orphan_due(process: &Process, current_pid: Pid) -> Option<Pid>")
        .expect("缺 ready_orphan_due (P-78 无主任务判据)");
    let due_tail = &src[due_at..];
    let due_end = due_tail.find("\n    }\n").expect("ready_orphan_due 未闭合");
    let due_body = &due_tail[..due_end];
    for needle in [
        "ProcessState::Ready",
        "SchedPolicy::Normal",
        "eevdf_on_rq",
        "eevdf_on_cpu",
        "eevdf_placed_once",
    ] {
        assert!(
            due_body.contains(needle),
            "ready_orphan_due 缺判据 {needle}: 判据面收窄/放宽都会改变恢复射程"
        );
    }

    // 入口面: tick 每拍调用恢复扫描
    let tick_at = src
        .find("pub fn tick(cpu_id: usize)")
        .expect("缺 Scheduler::tick");
    let tick_end = src[tick_at..].find("\n    }\n").expect("tick 未闭合") + tick_at;
    let tick_body = &src[tick_at..tick_end];
    let sleep_at = tick_body
        .find("Self::tick_sleep_wake(new_tick);")
        .expect("tick 缺睡眠唤醒扫描");
    let recover_at = tick_body
        .find("Self::tick_recover_orphans();")
        .expect("tick 缺无主任务恢复扫描 (P-78 兜底腿)");
    assert!(
        sleep_at < recover_at,
        "恢复扫描排在睡眠唤醒之后 (先处理有主唤醒, 再兜底无主任务)"
    );

    // 锁序面: 快照一次表锁, 补投在锁外, 且必须用 Requeue 语义
    let rec_at = src
        .find("fn tick_recover_orphans()")
        .expect("缺 tick_recover_orphans");
    let rec_tail = &src[rec_at..];
    let rec_end = rec_tail
        .find("\n    }\n")
        .expect("tick_recover_orphans 未闭合");
    let rec_body = &rec_tail[..rec_end];
    assert_eq!(
        rec_body.matches("PROCESS_TABLE.").count(),
        1,
        "恢复扫描的表访问必须只有 scan_into 一处 (不得逐槽 with_process)"
    );
    assert!(
        rec_body.contains("scan_into(&mut orphans"),
        "恢复扫描必须走一次表锁内的有界快照"
    );
    assert!(
        rec_body.contains("PlacementKind::Requeue"),
        "补投必须用 Requeue: 任务本就 Ready, 不得按 Wake 重发睡眠信用"
    );
    assert!(
        !rec_body.contains("PlacementKind::Wake"),
        "恢复路径不得出现 Wake 语义"
    );
    let scan_at = rec_body
        .find("scan_into(&mut orphans")
        .expect("恢复扫描缺 scan_into");
    let place_at = rec_body
        .find("eevdf_place_on(pid")
        .expect("恢复扫描缺补投动作");
    assert!(
        scan_at < place_at,
        "补投必须排在快照之后 (持表锁取 rq 锁即反向锁序)"
    );
    assert!(
        !rec_body.contains("with_process("),
        "恢复路径不得逐槽取表锁"
    );
}
