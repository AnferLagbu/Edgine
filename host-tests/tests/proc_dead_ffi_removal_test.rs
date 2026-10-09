//! 死 FFI 层删除面回归锁 (`privileged/proc` 的 C ABI 入口清理专项)
//!
//! 背景 (`docs/plan/net-e2e-tcp-echo-flakiness.md` §5 S-7):
//! `proc_ops.rs` / `sched_ops.rs` / `mechanism.rs` / `process.rs` 里沉淀了一批
//! `#[unsafe(no_mangle)] pub extern "C"` 入口 —— 本工程早已改成 Rust 直调，这些
//! C ABI 门面既无 Rust 调用方，也无汇编/C 侧引用（`.asm` / `.S` / `.c` 全库扫描
//! 为空），其中若干还在"替身路径"上偷偷保留了一份平行实现（`proc_sleep_ms` 自
//! 管 jiffies、`proc_alarm`/`proc_*itimer_real` 靠调度器 tick 全表轮询、
//! `process_get_current` 懒初始化一个假的 pid=1）。逐符号定型后全部删除，真实能
//! 力迁到 hrtimer 驱动的 `privileged::proc::itimer` 与 safe Rust 入口。
//!
//! 本测试锁两件事，防"死接口被重新引入"与"迁移成果被回退"：
//! 1. **删除面**：已定型删除的符号名 / `Process` 的 jiffies 记账字段 / 懒初始化
//!    init 进程的私有辅助，在 `src/kernel` 全树的**可执行代码**中不再出现；
//! 2. **替代面**：被删入口对应的真实实现确实就位并被调用（alarm/itimer → hrtimer
//!    槽表、睡眠 → `timer_sleep` 单一入口、当前进程 → `update_current_process_ptr`）。
//!
//! P-9 同族补删也在本锁范围内：`api.rs` 的 4 个 `wait_queue_*` 空桩（真的队列在
//! `privileged::ipc::types::WaitQueue`）与 `cpu_queue.rs` 的 `cpq_init` /
//! `cpq_resched_cpu` 两个死包裹（它们转发的 `init_cpu_queue` / `resched_cpu` 早已
//! 由 SMP 启动链与调度器直接调用，包裹层本身无任何调用方）。
//!
//! 与本工程其它回归锁同范式（源码文本契约）：只锁语义形状，不锁行号。
//!
//! 断言只看"去掉注释后的代码文本"，注释里解释历史时可以坦然提到被删符号。

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 递归收集 `dir` 下的全部 `.rs` 源文件（不依赖外部 crate）。
fn rust_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("无法遍历 {}: {e}", dir.display()))
    {
        let path = entry.expect("目录项可读").path();
        if path.is_dir() {
            rust_files_under(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// 去掉注释，只留可执行代码文本。
///
/// 处理整行注释（含 `//` / `//!`）与行尾注释；行尾判定用“前面未转义引号个数为
/// 偶”来避开字符串字面量里的 `//`。SIMPLIFIED 之类的说明性注释因此不参与断言。
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                return String::new();
            }
            let chars: Vec<char> = line.chars().collect();
            let mut quotes = 0usize;
            let mut i = 0;
            while i + 1 < chars.len() {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => {
                        quotes += 1;
                        i += 1;
                    }
                    '/' if chars[i + 1] == '/' && quotes % 2 == 0 => {
                        return chars[..i].iter().collect();
                    }
                    _ => i += 1,
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 去掉全部空白, 使断言不受换行与缩排影响。
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 在 `hay` 中按标识符边界查 `word`（不命中 `word_suffix` / `xword`）。
fn contains_word(hay: &str, word: &str) -> bool {
    hay.match_indices(word).any(|(at, _)| {
        let before_ok = !hay[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after_ok = !hay[at + word.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        before_ok && after_ok
    })
}

/// 已定型删除的 C ABI 入口与私有辅助（名字本身不得再出现在可执行代码里）。
const DEAD_ENTRIES: &[&str] = &[
    // 进程生命周期门面 (真实实现走 PROCESS_TABLE / syscall 路径)
    "process_get_current",
    "process_create",
    "process_kill",
    "process_find_by_pid",
    "proc_create_internal",
    "proc_wait_child",
    "proc_get_exit_code",
    "proc_get_state",
    "proc_is_initialized",
    "proc_block",
    "proc_unblock",
    "process_init",
    // 睡眠 / alarm / itimer 门面 (迁 hrtimer, 见替代面用例)
    "proc_sleep_ms",
    "proc_alarm",
    "proc_check_alarm",
    "proc_getitimer_real",
    "proc_setitimer_real",
    "proc_check_itimer_real",
    "proc_get_start_jiffies",
    "proc_set_start_jiffies",
    // 调度器门面 (真实实现走 SCHEDULER 的 safe Rust API)
    "scheduler_add",
    "sched_init_internal",
    "sched_add_internal",
    "sched_schedule_internal",
    "sched_should_reschedule",
    "sched_set_current",
    "sched_get_current",
    "scheduler_set_quota",
    "scheduler_remove_quota",
    "scheduler_set_proc_limit",
    "scheduler_add_rt_task",
    "scheduler_set_sched_policy",
    "scheduler_get_rt_count",
    // 线程 / 杂项门面
    "thread_get_current",
    "thread_init",
    "proc_has_runnable",
    "proc_get_current_pid_internal",
    "proc_yield_internal",
    // P-9 同族：等待队列空桩门面与 cpu_queue 死包裹
    "wait_queue_init",
    "wait_queue_add",
    "wait_queue_wake_one",
    "wait_queue_wake_all",
    "cpq_init",
    "cpq_resched_cpu",
    // 随 `process_get_current` 一并删除的懒初始化机制
    "create_init_process",
    "INIT_PROCESS_CREATED",
];

/// `Process` 上被删除的 jiffies 记账字段（时基假设的载体，见 plan §6 P-2）。
const DEAD_PROCESS_FIELDS: &[&str] = &[
    "start_jiffies",
    "alarm_deadline",
    "alarm_prev_remaining",
    "itimer_real_deadline",
    "itimer_real_interval",
    "itimer_real_remaining",
];

const PROC_OPS: &str = "src/kernel/privileged/proc/proc_ops.rs";
const SCHED_OPS: &str = "src/kernel/privileged/proc/sched_ops.rs";
const MECHANISM: &str = "src/kernel/privileged/proc/mechanism.rs";
const PROCESS: &str = "src/kernel/privileged/proc/process.rs";
const ITIMER: &str = "src/kernel/privileged/proc/itimer.rs";
const PROC_MOD: &str = "src/kernel/privileged/proc/mod.rs";
const SLEEP: &str = "src/kernel/privileged/timer/sleep.rs";
const FS_MISC: &str = "src/kernel/functions/fs/misc.rs";

#[test]
fn dead_ffi_entries_are_gone_from_executable_code() {
    let mut files = Vec::new();
    rust_files_under(&repo_root().join("src/kernel"), &mut files);
    assert!(
        files.len() > 300,
        "src/kernel 源文件扫描异常，只有 {}",
        files.len()
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let code = strip_comments(&raw);
        for name in DEAD_ENTRIES {
            if contains_word(&code, name) {
                offenders.push(format!("{}: {name}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "死 FFI 入口重新出现在可执行代码里（违反删除面定型）:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn dead_jiffies_fields_are_gone() {
    // alarm/itimer 迁 hrtimer 后，这些 jiffies 字段与 `start_jiffies` 一并消失；
    // 复现即意味着回到"调度器 tick 全表轮询 + 100Hz/1000Hz 时基假设"。
    let mut files = Vec::new();
    rust_files_under(&repo_root().join("src/kernel"), &mut files);

    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let code = strip_comments(&raw);
        for name in DEAD_PROCESS_FIELDS {
            if contains_word(&code, name) {
                offenders.push(format!("{}: {name}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "jiffies 记账字段复现，说明 alarm/itimer 又被拉回 tick 轮询模型:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn alarm_itimer_lives_on_hrtimer_slots() {
    // 替代面①：每进程一个槽，槽内嵌 HrTimer，登记走 hrtimer 框架而非 tick 轮询。
    let src = strip_comments(&read_src(ITIMER));
    assert!(
        src.contains("struct ItimerSlot"),
        "alarm/itimer 的 per-pid 槽表消失（应取代 Process 的 jiffies 字段）"
    );
    assert!(
        src.contains("timer: HrTimer,"),
        "ItimerSlot 未内嵌 HrTimer —— 截止期没交给 hrtimer 框架"
    );
    assert!(
        src.contains("hrtimer_start(&slot.timer,"),
        "单次触发路径缺 hrtimer_start 登记"
    );
    assert!(
        src.contains("hrtimer_start_periodic(&slot.timer,"),
        "周期模式缺 hrtimer_start_periodic 登记（ITIMER_REAL 的 interval 语义）"
    );
    assert!(
        src.contains("do_signal_send(") && src.contains("SIGALRM"),
        "到期回调未投递 SIGALRM —— alarm(2) 语义直接失效"
    );

    // 替代面②：槽表在 proc 子系统内注册并对外可见。
    let mod_src = strip_comments(&read_src(PROC_MOD));
    assert!(
        mod_src.contains("pub mod itimer;"),
        "itimer 模块未在 proc/mod.rs 注册"
    );

    // 替代面③：syscall 侧（functions 层 0 unsafe）直调 safe API。
    let fs_src = strip_comments(&read_src(FS_MISC));
    for call in [
        "crate::privileged::proc::itimer_real_set(",
        "crate::privileged::proc::itimer_real_get(",
        "crate::privileged::proc::itimer_alarm(",
    ] {
        assert!(
            squash(&fs_src).contains(&squash(call)),
            "setitimer/alarm 系统调用未委托 itimer 的 safe API: {call}"
        );
    }
}

#[test]
fn itimer_is_revoked_on_process_exit() {
    // pid 复用防护：进程终止必须撤防，否则新进程会收到前一进程遗留的 SIGALRM。
    let src = strip_comments(&read_src(ITIMER));
    assert!(
        src.contains("pub fn itimer_real_release_pid(pid: Pid)"),
        "缺 itimer_real_release_pid 撤防入口"
    );
    assert!(
        src.contains("hrtimer_cancel(&slot.timer);"),
        "撤防入口未取消 hrtimer 登记"
    );
    let sched = strip_comments(&read_src("src/kernel/privileged/proc/scheduler.rs"));
    assert!(
        contains_word(&sched, "itimer_real_release_pid"),
        "Scheduler::exit 未调用撤防 —— 槽表生命周期不完整"
    );
}

#[test]
fn sleep_and_current_process_keep_single_rust_path() {
    // 替代面④：`proc_sleep_ms` 被删后，毫秒级可阻塞睡眠只剩 timer_sleep 一条路径。
    let src = strip_comments(&read_src(SLEEP));
    assert!(
        src.contains("proc.sleep_until"),
        "timer_sleep 不再经 sleep_until (tick 时基) 记账唤醒时刻"
    );
    assert!(
        contains_word(&src, "scheduler_block") && contains_word(&src, "scheduler_schedule"),
        "睡眠路径缺 block + schedule 两步序（只 block 不切换 = 带 Blocked 状态继续跑）"
    );

    // 替代面⑤：`process_get_current` 的裸指针门面由这两个入口取代，且不再有懒初始化。
    let proc_ops = strip_comments(&read_src(PROC_OPS));
    assert!(
        proc_ops.contains("pub extern \"C\" fn update_current_process_ptr(ptr: u64)"),
        "上下文切换路径的当前进程指针更新入口丢失"
    );
    assert!(
        proc_ops.contains("pub extern \"C\" fn process_get_current_pid() -> u32"),
        "当前进程 PID 查询入口丢失"
    );
}

#[test]
fn cpu_queue_and_wait_queue_are_reached_via_safe_api() {
    // 被删的 cpq_* / wait_queue_* 不是“未接线的机制”，而是已经被 safe Rust 入口
    // 替代的死包裹 / 空桩 —— 锁真实调用链仍在，防止有人把包裹重新当接口使。
    for rel in [
        "src/kernel/privileged/arch/x86_64/smp_init.rs",
        "src/kernel/privileged/arch/aarch64/smp_init.rs",
    ] {
        let src = strip_comments(&read_src(rel));
        assert!(
            squash(&src).contains("proc::init_cpu_queue("),
            "AP 启动链不再登记 per-CPU 队列 ({}); 队列初始化只能走 safe API",
            rel
        );
    }
    let sched = strip_comments(&read_src("src/kernel/privileged/proc/scheduler.rs"));
    assert!(
        squash(&sched).contains("cpu_queue::resched_cpu(target)"),
        "跨 CPU 抢占不再经 resched_cpu 发 IPI"
    );

    // 等待队列本体存在且由策略层使用（空桩门面删掉后不能连真队列一并丢失）.
    let wq = strip_comments(&read_src("src/kernel/privileged/ipc/types.rs"));
    assert!(
        wq.contains("pub struct WaitQueue"),
        "WaitQueue 本体丢失（原 wait_queue_* 空桩所“代表”的真实机制）"
    );
    let ipc = strip_comments(&read_src(
        "src/kernel/functions/ipc/scheduler_integration.rs",
    ));
    assert!(
        contains_word(&ipc, "WaitQueue"),
        "functions 层的阻塞/唤醒衔接不再使用 WaitQueue"
    );
}

#[test]
fn proc_ffi_files_carry_no_dead_code_hatch() {
    // 硬规则 F9: 删除是靠"实现使用路径"闭环，不是靠豁免压告警。
    for rel in [PROC_OPS, SCHED_OPS, MECHANISM, PROCESS, ITIMER] {
        let src = read_src(rel);
        assert!(
            !src.contains("#[allow(dead_code)]")
                && !src.contains("#[allow(unused")
                && !src.contains("#[expect(\"dead_code\""),
            "{rel} 引入死代码豁免，违反 §5 F9"
        );
    }
}
