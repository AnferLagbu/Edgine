// SPDX-License-Identifier: MPL-2.0
//! M7 施工批 (A-15 丙) 回归锁: tick 扫描由「逐槽加锁」改为「一次表锁内的有界快照」。
//!
//! 口径来源: `docs/plan/multithreading-project.md` §8.1 A-15 裁定行。本文件锁三层:
//!
//! 1. **行为面** (真实 `PROCESS_TABLE` + 真实 `ProcessTable::scan_into` + 真实
//!    `Scheduler::sleep_wake_due`): 到期命中、未到期、`sleep_until == 0` 哨兵、非
//!    Sleeping 阻塞原因、非 Blocked 状态、跳过本核当前任务、上限截断、空槽不计、
//!    槽位升序。
//! 2. **形状面**: 两个 tick 扫描点各只剩一次 `scan_into` 取锁, 且"要再取表锁或取
//!    rq 锁"的动作全部排在快照之后 (锁外)。
//! 3. **契约面**: `scan_into` 的谓词四律 (不回调表 / 不分配 / 不打印 / 不取 rq 锁)
//!    写在它自己的文档注释里, 文档失效即红; 迭代面取锁点唯一性同时钉住, 防止第二
//!    个手写全表扫描长出来。
//!
//! 本文件不改任何 QEMU/CI 判据 (裁定④「只加源码形状锁」)。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, MutexGuard};

use edgine::kernel::privileged::proc::process::{PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::{BlockReason, ProcessState, Scheduler};

const SCHED_SRC: &str = "src/kernel/privileged/proc/scheduler.rs";
const PROCESS_SRC: &str = "src/kernel/privileged/proc/process.rs";

/// 全局进程表是进程内共享态, 文件内测试串行执行。
static TABLE_LOCK: StdMutex<()> = StdMutex::new(());

fn lock_table() -> MutexGuard<'static, ()> {
    TABLE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 已登记进全局进程表的测试 TCB; Drop 时回收 (幂等)。
struct TableProc {
    pid: u32,
}

impl TableProc {
    /// 以显式状态 / 阻塞原因 / 到期时刻登记一个 TCB 进全局表。
    fn armed(pid: u32, state: ProcessState, reason: BlockReason, until: u64) -> Self {
        let proc = Process::new(pid, "m7-scan-test", None);
        proc.state.store(state as u32, Ordering::SeqCst);
        proc.block_reason.store(reason as u32, Ordering::Relaxed);
        proc.sleep_until.store(until, Ordering::SeqCst);
        assert!(
            PROCESS_TABLE.insert(Box::into_raw(Box::new(proc))),
            "测试 TCB 登记失败 (pid={pid})"
        );
        Self { pid }
    }

    /// 到期睡眠任务 (入选形态)。
    fn sleeping_expired(pid: u32) -> Self {
        Self::armed(pid, ProcessState::Blocked, BlockReason::Sleeping, 10)
    }

    /// 不进表、只构造 TCB 的形态 (供纯判据用例使用, 不污染全局表)。
    fn standalone(pid: u32) -> Process {
        Process::new(pid, "m7-scan-standalone", None)
    }
}

impl Drop for TableProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

/// 分配 n 个互不冲突的 pid (位图环形分配, 顺序不保证递增)。
fn alloc_pids(n: usize) -> Vec<u32> {
    (0..n)
        .map(|_| {
            PROCESS_TABLE
                .allocate_pid()
                .expect("测试 pid 分配失败 (位图已满?)")
        })
        .collect()
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {e}", path.display()))
}

/// 去掉整行注释, 避免注释里的历史名词被误判为代码。
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 源码片段匹配统一忽略空白 (rustfmt 折行不影响契约)。
fn has(haystack: &str, needle: &str) -> bool {
    let norm = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    norm(haystack).contains(&norm(needle))
}

/// 取 `sig` 所在函数的体 (含签名行), 按花括号配对截取。
fn fn_body(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let chars: Vec<char> = src[start..].chars().collect();
    let mut depth = 0usize;
    let mut seen_open = false;
    for (idx, ch) in chars.iter().enumerate() {
        match ch {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    return chars[..=idx].iter().collect();
                }
            }
            _ => {}
        }
    }
    panic!("函数体花括号不配对: {sig}");
}

/// 以函数体内的 `PROCESS_TABLE.scan_into(...)` 调用收口处把函数切成
/// "快照相" (调用 + 谓词) 与 "锁外相" (其后的全部动作)。
///
/// 取括号配对而非固定文本: 谓词写成块闭包还是表达式闭包由 rustfmt 决定, 形状锁
/// 不能依赖其中一种排版。
fn split_phases(body: &str) -> (String, String) {
    let start = body
        .find("PROCESS_TABLE.scan_into(")
        .expect("扫描点应有一处 PROCESS_TABLE.scan_into 调用");
    let chars: Vec<char> = body[start..].chars().collect();
    let mut depth = 0usize;
    for (idx, ch) in chars.iter().enumerate() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let cut = start + idx + 1;
                    // 连同其后的 `;` 归入快照相
                    let cut = if body[cut..].starts_with(';') {
                        cut + 1
                    } else {
                        cut
                    };
                    return (body[..cut].to_string(), body[cut..].to_string());
                }
            }
            _ => {}
        }
    }
    panic!("scan_into 调用括号不配对");
}

// ============================================================================
// 1. 行为面: tick 睡眠入选判据 (纯判据, 不碰全局表)
// ============================================================================

#[test]
fn sleep_wake_due_selects_only_expired_sleeping_blocked() {
    let due = TableProc::standalone(101);
    due.state
        .store(ProcessState::Blocked as u32, Ordering::SeqCst);
    due.block_reason
        .store(BlockReason::Sleeping as u32, Ordering::Relaxed);
    due.sleep_until.store(500, Ordering::SeqCst);

    assert_eq!(
        Scheduler::sleep_wake_due(&due, 0, 500),
        Some(101),
        "current_ticks == sleep_until 必须到期 (判据是 >= 不是 >)"
    );
    assert_eq!(
        Scheduler::sleep_wake_due(&due, 0, 499),
        None,
        "current_ticks < sleep_until 不得唤醒 (提前唤醒即丢睡眠时长)"
    );
    // until == 0 是「未挂睡眠」哨兵, 不能被 `current_ticks >= 0` 收走
    due.sleep_until.store(0, Ordering::SeqCst);
    assert_eq!(
        Scheduler::sleep_wake_due(&due, 0, u64::MAX),
        None,
        "sleep_until == 0 一律不入选"
    );
}

#[test]
fn sleep_wake_due_skips_current_task_and_non_sleeping_blocked() {
    let blocked = TableProc::standalone(102);
    blocked
        .state
        .store(ProcessState::Blocked as u32, Ordering::SeqCst);
    blocked
        .block_reason
        .store(BlockReason::Sleeping as u32, Ordering::Relaxed);
    blocked.sleep_until.store(10, Ordering::SeqCst);

    // 本核当前任务不参与扫描 (旧循环的 `pid == Self::current()` 分支)
    assert_eq!(
        Scheduler::sleep_wake_due(&blocked, 102, 20),
        None,
        "本核在跑的任务不参与唤醒扫描"
    );
    assert_eq!(
        Scheduler::sleep_wake_due(&blocked, 0, 20),
        Some(102),
        "current_pid 取哨兵 0 时正常入选"
    );

    // 非 Sleeping 的阻塞原因不入选 (futex / IO 唤醒归各自路径)
    blocked
        .block_reason
        .store(BlockReason::FutexWait as u32, Ordering::Relaxed);
    assert_eq!(
        Scheduler::sleep_wake_due(&blocked, 0, 20),
        None,
        "阻塞原因不是 Sleeping 时不得由 tick 唤醒"
    );

    // 非 Blocked 状态不入选
    blocked
        .block_reason
        .store(BlockReason::Sleeping as u32, Ordering::Relaxed);
    blocked
        .state
        .store(ProcessState::Ready as u32, Ordering::SeqCst);
    assert_eq!(
        Scheduler::sleep_wake_due(&blocked, 0, 20),
        None,
        "Ready 状态不参与睡眠扫描"
    );
}

// ============================================================================
// 2. 行为面: 真实表上的有界快照
// ============================================================================

#[test]
fn scan_into_visits_only_registered_slots_and_selects_expired() {
    let _serial = lock_table();
    let pids = alloc_pids(4);
    let (a_pid, b_pid, c_pid, d_pid) = (pids[0], pids[1], pids[2], pids[3]);
    let _a = TableProc::sleeping_expired(a_pid);
    let _b = TableProc::armed(b_pid, ProcessState::Blocked, BlockReason::Sleeping, 9_999);
    let _c = TableProc::armed(c_pid, ProcessState::Running, BlockReason::Sleeping, 10);
    let _d = TableProc::armed(d_pid, ProcessState::Blocked, BlockReason::Sleeping, 0);

    let visited = AtomicUsize::new(0);
    let mut picks: [u32; 8] = [0; 8];
    let hits = PROCESS_TABLE.scan_into(&mut picks, |proc| {
        if proc.get_state() != ProcessState::Zombie {
            visited.fetch_add(1, Ordering::SeqCst);
        }
        Scheduler::sleep_wake_due(proc, 0, 40)
    });

    assert_eq!(
        hits, 1,
        "四种形态里只有「Blocked + Sleeping + 已到期」入选 (未到期 / until=0 / 非 Blocked 都要落空)"
    );
    assert_eq!(picks[0], a_pid, "入选的必须是那个到期睡眠任务");
    assert_eq!(
        visited.load(Ordering::SeqCst),
        4,
        "空槽不得计一次谓词调用: 表内只有 4 个在册 TCB"
    );
}

#[test]
fn scan_into_returns_candidates_in_slot_ascending_order() {
    let _serial = lock_table();
    let pids = alloc_pids(2);
    let _first = TableProc::sleeping_expired(pids[0]);
    let _second = TableProc::sleeping_expired(pids[1]);

    let mut picks: [u32; 8] = [0; 8];
    let hits = PROCESS_TABLE.scan_into(&mut picks, |proc| Scheduler::sleep_wake_due(proc, 0, 40));

    let mut expected = pids.clone();
    expected.sort_unstable();
    assert_eq!(hits, 2, "两个到期睡眠任务都应入选");
    assert_eq!(
        picks[..hits],
        expected,
        "快照次序必须是槽位升序 (与旧逐槽循环 1..=255 逐位等价)"
    );
}

#[test]
fn scan_into_stops_calling_predicate_once_buffer_is_full() {
    let _serial = lock_table();
    let pids = alloc_pids(3);
    let _armed: Vec<TableProc> = pids
        .iter()
        .map(|&pid| TableProc::armed(pid, ProcessState::Blocked, BlockReason::Sleeping, 10))
        .collect();

    let called = AtomicUsize::new(0);
    let mut picked: [u32; 2] = [0; 2];
    let hits = PROCESS_TABLE.scan_into(&mut picked, |proc| {
        if pids.contains(&proc.pid.0) {
            called.fetch_add(1, Ordering::SeqCst);
            Some(proc.pid.0)
        } else {
            None
        }
    });

    assert_eq!(hits, 2, "缓冲长度即单轮上限");
    assert_eq!(
        called.load(Ordering::SeqCst),
        2,
        "缓冲写满后谓词不得再被调用 (tick_reap_zombies 的同长去重表依赖这一性质)"
    );
    let mut expected = pids.clone();
    expected.sort_unstable();
    expected.truncate(2);
    assert_eq!(
        picked,
        [expected[0], expected[1]],
        "截断必须取槽位升序的前 2 个"
    );
}

// ============================================================================
// 3. 形状面: 两个扫描点各只剩一次取锁, 重动作全在锁外
// ============================================================================

#[test]
fn tick_sleep_wake_scans_with_one_table_lock() {
    let sched = code_only(&read_src(SCHED_SRC));
    let body = fn_body(&sched, "fn tick_sleep_wake(current_ticks: u64)");
    let (snapshot, out_of_lock) = split_phases(&body);

    assert!(
        has(&snapshot, "PROCESS_TABLE.scan_into(&mut to_wake"),
        "tick 睡眠扫描未改用一次表锁内的批量快照"
    );
    assert_eq!(
        snapshot.matches("PROCESS_TABLE.").count(),
        1,
        "睡眠扫描的表访问必须只有 scan_into 一处"
    );
    assert!(
        !body.contains("with_process("),
        "睡眠扫描重新出现逐槽 with_process (每 tick 每核 255 次有名锁获取的回归形态)"
    );
    assert!(!body.contains("for pid in"), "睡眠扫描重新出现逐槽循环");
    assert!(
        !snapshot.contains("Self::unblock("),
        "unblock 被挪进快照闭包内 (持表锁取 eevdf_rq 锁 = 反向锁序)"
    );
    assert!(
        has(&out_of_lock, "Self::unblock(pid)"),
        "唤醒动作必须在快照之后 (锁外) 执行"
    );
}

#[test]
fn tick_reap_zombies_snapshots_then_queries_outside_the_lock() {
    let sched = code_only(&read_src(SCHED_SRC));
    let body = fn_body(&sched, "fn tick_reap_zombies(new_tick: u64)");
    let (snapshot, out_of_lock) = split_phases(&body);

    assert!(
        has(&snapshot, "PROCESS_TABLE.scan_into(&mut candidates"),
        "僵尸回收未改用一次表锁内的批量快照"
    );
    assert!(
        !body.contains("with_process(pid"),
        "僵尸回收重新出现逐槽 with_process 扫描"
    );
    assert!(!body.contains("for pid in"), "僵尸回收重新出现逐槽循环");
    assert!(
        has(
            &body,
            "(p.get_state() == ProcessState::Zombie)
                 .then(|| (p.thread_group.tgid(), p.group().parent()))"
        ),
        "K-04: 僵尸候选读数必须仍读线程组 (tgid + 父 tgid)"
    );

    // 快照闭包 (含调用本身) 内只允许一次 PROCESS_TABLE 引用, 且不得出现分配/打印/取 rq 锁.
    assert_eq!(
        snapshot.matches("PROCESS_TABLE.").count(),
        1,
        "僵尸候选快照内出现第二处表访问 (不可重入锁同核自死锁)"
    );
    for forbidden in [
        "group_zombie_exit_code(",
        "reap_zombie_group(",
        "Self::unblock(",
        "Vec::new",
        "Arc::clone",
        "slog_",
        "klog_",
        "println!",
    ] {
        assert!(
            !snapshot.contains(forbidden),
            "僵尸候选快照内出现 {forbidden}: 违反 scan_into 谓词契约 (锁内不回调表 / 不分配 / 不打印 / 不取 rq 锁)"
        );
    }
    for outside in [
        "PROCESS_TABLE.with_process(parent",
        "PROCESS_TABLE.group_zombie_exit_code(tgid)",
        "PROCESS_TABLE.reap_zombie_group(tgid)",
    ] {
        assert!(has(&out_of_lock, outside), "锁外查询缺失: {outside}");
    }
}

#[test]
fn reap_candidate_window_is_a_named_bound() {
    let sched = code_only(&read_src(SCHED_SRC));
    assert!(
        sched.contains("const REAP_CANDIDATE_WINDOW: usize = 32;"),
        "候选窗口未提为常量 (SIMPLIFIED 上限必须单点可读)"
    );
    let body = fn_body(&sched, "fn tick_reap_zombies(new_tick: u64)");
    assert!(
        body.contains("REAP_CANDIDATE_WINDOW"),
        "扫描点未引用候选窗口常量"
    );
    assert!(
        !body.contains("32"),
        "扫描点重新出现硬编码窗口字面量 (与 REAP_CANDIDATE_WINDOW 双写即漂移)"
    );
}

// ============================================================================
// 4. 契约面: 迭代面取锁点唯一 + 谓词四律写在文档里
// ============================================================================

#[test]
fn table_iteration_has_a_single_lock_acquire_point() {
    let src = code_only(&read_src(PROCESS_SRC));
    assert_eq!(
        src.matches("table.iter().flatten()").count(),
        1,
        "迭代面出现第二处手写全表扫描: 表锁契约与锁序重核将只覆盖其中一处"
    );
    assert!(
        has(
            &src,
            "pub fn for_each<F: FnMut(&Process) -> bool>(&self, f: F) { self.walk_locked(f); }"
        ),
        "for_each 未收敛到 walk_locked (迭代面取锁点须唯一)"
    );
    assert!(
        has(
            &src,
            "fn walk_locked<F: FnMut(&Process) -> bool>(&self, mut f: F)"
        ),
        "缺 walk_locked: 迭代面的唯一取锁点被拆散"
    );
}

#[test]
fn scan_into_predicate_contract_is_documented() {
    let src = read_src(PROCESS_SRC);
    let start = src
        .find("pub fn scan_into<T, F>(&self, buf: &mut [T]")
        .expect("缺 ProcessTable::scan_into");
    let head = &src[..start];
    let doc_start = head
        .rfind("/// 一次表锁内的**有界快照**")
        .expect("缺 scan_into 的文档段 (契约只能写在 API 上)");
    // 行内 markdown (强调星号、代码反引号) 只关乎排版, 与契约措辞无关: 归一化掉再比对,
    // 免得给某个词加粗就误报"契约丢失".
    let doc: String = head[doc_start..]
        .chars()
        .filter(|c| *c != '*' && *c != '`')
        .collect();
    for clause in [
        "不得回调",
        "不可重入",
        "不得分配",
        "不得打印",
        "运行队列",
        "ref-lock-order",
    ] {
        assert!(
            doc.contains(clause),
            "scan_into 的谓词契约文档缺「{clause}」: 契约退回靠调用点自觉, 正是本批要关闭的缺口"
        );
    }
}
