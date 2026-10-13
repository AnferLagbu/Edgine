// SPDX-License-Identifier: MPL-2.0
//! P-21 回归: 离核投送屏障 (`Process::cfs_on_cpu`) 的接线契约.
//!
//! 缺陷 (取证结论见 `docs/report/net-e2e-offcore-dequeue-window.md`):
//! 任务 `block()` 后仍是本核 `current` —— 归属已被清成 `NO_RQ_CPU`, 于是别核的
//! 唤醒投送 (`place_locked`) 与偷取 (`steal_locked`) 的"在核判据"都看不见它,
//! 把它合法投进树并置 `Ready`; 紧接着本核 `schedule()` 的 prev 收尾按快照
//! (`prev_state_code`) 把它**摘出树**而不改状态 —— 该任务就此 `Ready` 却不在
//! 任何树上、不属任何核, 而唤醒面的电平重扫以 `state == Blocked` 为选集条件,
//! 它再没有自愈入口 (实测软挂轮 `摘走Ready=1`, 健康轮恒 0). 反序分叉即两棵树 →
//! 两核同时上核运行同一任务 → 同一块进程内核栈被交叠使用 → GPF.
//!
//! 修复形态: 显式屏障字段承载"占用某个核"这一语义 (与承载"在可运行记账里"的
//! `cfs_on_rq` 正交), 屏障期到达的唤醒只做延迟投送 (搬状态不入树), 落地由占用者
//! 核接手; 清屏障后与 tick 各有一条补投腿.
//!
//! 本测试锁定该形态的**接线契约** (屏障的运行时效果由 QEMU e2e 多轮覆盖;
//! host 侧无多核时序仿真能力, 见 `docs/plan/net-e2e-tcp-echo-flakiness.md` P-21
//! 的验证项): 屏障的单点置位/单点清除、两处搬动闸门的拒投、prev 落地的幂等入树、
//! 清屏障后的补投两条腿 (二者均只面对已被派发过的任务).

use std::fs;
use std::path::PathBuf;

const PROC: &str = "src/kernel/privileged/proc/process.rs";
const CFS: &str = "src/kernel/privileged/proc/cfs.rs";
const SCHED: &str = "src/kernel/privileged/proc/scheduler.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免注释里的历史名词被误判为代码
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 源码片段匹配统一忽略空白: rustfmt 会把长链式调用折行, 契约关心的是调用序列
/// 本身, 不是排版.
fn norm(s: &str) -> String {
    // 空白不敏感 + 尾随逗号不敏感: rustfmt 会把长参数列表折行并在 `)` 前补 `,`.
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .replace(",)", ")")
}

fn has(hay: &str, needle: &str) -> bool {
    norm(hay).contains(&norm(needle))
}

/// 空白忽略后的出现次数 (屏障的"单点性"契约用它, 而不是 `contains`).
fn occurrences(hay: &str, needle: &str) -> usize {
    let hay = norm(hay);
    let needle = norm(needle);
    assert!(!needle.is_empty(), "探针不得为空串");
    let mut count = 0usize;
    let mut pos = 0usize;
    while let Some(i) = hay[pos..].find(&needle) {
        count += 1;
        pos += i + needle.len();
    }
    count
}

/// 空白忽略后的先后次序判据: 返回 `true` ⇔ `first` 的末次出现早于 `second` 的
/// 首次出现. 用于钉住"必须先置屏障再写 current"这类顺序契约.
fn before(hay: &str, first: &str, second: &str) -> bool {
    let hay = norm(hay);
    let Some(f) = hay.find(&norm(first)) else {
        return false;
    };
    let Some(s) = hay.find(&norm(second)) else {
        return false;
    };
    f + norm(first).len() <= s
}

/// 取 `sig` 所在函数的体 (含签名行), 按花括号配对截取
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

/// 屏障字段必须存在且与记账凭证正交 (两条语义各一字段, 不得复用).
#[test]
fn barrier_field_is_declared_orthogonal_to_accounting() {
    let proc_src = code_only(&read_src(PROC));
    assert!(
        has(&proc_src, "pub cfs_on_cpu: AtomicBool"),
        "P-21: 投送屏障必须是显式字段 —— 旧模型把'在核上'与'在记账里'挤进 \
         `cfs_on_rq`/`cfs_rq_cpu` 两个语义, 正是离核窗口不可见的根因"
    );
    assert!(
        has(&proc_src, "cfs_on_rq: AtomicBool"),
        "P-21: 记账凭证必须独立存在, 屏障不得靠复用它实现"
    );
    // 新建任务不占用任何核 —— 初值必须为 false, 否则该任务永不可被投送.
    let new_body = code_only(&fn_body(&read_src(PROC), "pub fn new"));
    assert!(
        has(&new_body, "cfs_on_cpu: AtomicBool::new(false)"),
        "P-21: Process::new 必须把屏障初始化为未占用"
    );
}

/// 屏障的唯一置位点: `schedule()` 拣中 next 时, 且必须早于 `per_cpu.current` 写入.
#[test]
fn barrier_is_set_at_dispatch_before_current_store() {
    let sched = code_only(&read_src(SCHED));
    let schedule_body = code_only(&fn_body(&sched, "pub fn schedule"));
    assert_eq!(
        occurrences(&schedule_body, "cfs_on_cpu.store(true"),
        1,
        "P-21: 屏障必须只有 `schedule()` 一个置位点 —— 多点置位等于多点漏置位"
    );
    // 负控制: `schedule()` 之外不得有第二处置位点 (整文件计数 = 函数体计数).
    assert_eq!(
        occurrences(&sched, "cfs_on_cpu.store(true"),
        occurrences(&schedule_body, "cfs_on_cpu.store(true"),
        "P-21: `schedule()` 之外不得有第二处置位点"
    );
    assert!(
        before(
            &schedule_body,
            "cfs_on_cpu.store(true",
            "per_cpu.current.store(next",
        ),
        "P-21: 屏障必须早于 `current` 写入 —— 从写入 current 起到真正切走之间, \
         本核已经可以被别核观察到该任务'还是 current'; 晚置即留下无人设障的窗口"
    );
    // 负控制: 不得在 `context_switch` 之后才置屏障 (那时别核早已投送成功).
    assert!(
        before(&schedule_body, "cfs_on_cpu.store(true", "context_switch("),
        "P-21: 屏障晚于上下文切换即形同虚设"
    );
}

/// 屏障的唯一清位点: `reap_off_cpu` (语义同 Linux `finish_task_switch`).
#[test]
fn barrier_is_cleared_only_in_reap_off_cpu() {
    let sched = code_only(&read_src(SCHED));
    assert_eq!(
        occurrences(&sched, "cfs_on_cpu.store(false"),
        1,
        "P-21: 清位必须集中在 `reap_off_cpu` 一处 —— 提前清位等于允许别核在上下文\
         未保存完时把它投进树, 两核即交叠同一块进程内核栈"
    );
    let reap = code_only(&fn_body(&sched, "fn reap_off_cpu"));
    assert!(
        has(&reap, "cfs_on_cpu.store(false"),
        "P-21: 清位点必须就是 `reap_off_cpu` (它运行在新任务上下文, 是'prev 已离开 \
         CPU'被确证的唯一时刻)"
    );
    // 负控制: `block()` 不得清屏障 —— 阻塞时任务仍是本核 current.
    let block = code_only(&fn_body(&sched, "pub fn block"));
    assert!(
        !block.contains("cfs_on_cpu"),
        "P-21: `block()` 清屏障即把保护窗口整个抹掉 (prev 收尾与唤醒方在此重逢)"
    );
}

/// 搬动闸门一: 唤醒投送见屏障只搬状态、不入树, 且必须回报"已承接".
#[test]
fn placement_delivers_state_but_not_tree_under_barrier() {
    let sched = code_only(&read_src(SCHED));
    let place = code_only(&fn_body(&sched, "fn place_locked"));
    assert!(
        has(&place, "p.cfs_on_cpu.load(Ordering::Acquire)"),
        "P-21: 快照必须读屏障 —— 只读 `cfs_rq_cpu` 的在核判据对 `NO_RQ_CPU` 的离核\
         任务失明 (实测的单一成因窗口)"
    );
    assert!(
        before(
            &place,
            "if on_cpu && !owner_landing {",
            "let placed = match src"
        ),
        "P-21: 拒投分支必须在真正入树的搬移之前"
    );
    // 屏障分支的函数体 (到 `return true;` 为止) 内不得出现入树/撤账动作.
    let gate = place
        .find("if on_cpu && !owner_landing {")
        .expect("place_locked 缺屏障拒投分支");
    let barrier_seg = &place[gate..place.find("return true;").unwrap()];
    assert!(
        !has(barrier_seg, "enqueue") && !has(barrier_seg, "dequeue"),
        "P-21: 屏障期不得动任何一棵树 (进树即'树上与核上各一份', 被别核拣走即双派)"
    );
    assert!(
        has(barrier_seg, "ProcessState::Ready"),
        "P-21: 屏障期到达的唤醒必须仍把状态搬成 `Ready` 并回报承接 —— 丢弃唤醒\
         (edge-triggered 事件源) 就是丢任务"
    );
}

/// P-26 回归: 屏障延迟投送必须原子 —— 复查 `on_cpu` 与置 `Ready` 在同一临界区.
///
/// 缺陷 (取证见 `docs/report/net-e2e-tcp-echo-flakiness.md` P-26): `place_locked` 上方
/// 快照读 `cfs_on_cpu` 与延迟支的 `set_state(Ready)` 分属两次 `PROCESS_TABLE` 加锁,
/// 占用者核的 `reap_off_cpu` 可夹在两者之间清屏障 + 读到仍是 `Blocked` 而放弃补投腿,
/// 本支随后才翻 `Ready` ⇒ prev 收尾腿与 reap 补投腿同时落空, init 永久停在
/// "Ready 且不在任何树上" (`rq_cpu=NO_RQ_CPU`; x86_64 双核 e2e ~6% 就绪超时挂死).
///
/// 契约: 延迟支必须在持锁期内**重新读取** `cfs_on_cpu` 再据此翻 `Ready` —— 承接决策
/// 不能只依赖快照的陈旧屏障值; 屏障若已在快照后被清除, 延迟支必须 fall-through 到
/// 完整搬定路径亲自入树, 而不是回报"已承接"后无人接手.
#[test]
fn barrier_defer_rechecks_on_cpu_atomically() {
    let sched = code_only(&read_src(SCHED));
    let place = code_only(&fn_body(&sched, "fn place_locked"));
    let gate = place
        .find("if on_cpu && !owner_landing {")
        .expect("place_locked 缺屏障拒投分支");
    // 延迟支体 = gate 到其内第一处 `return true;` (屏障在位时的承接返回).
    let defer_end = place[gate..]
        .find("return true;")
        .map(|i| gate + i)
        .expect("屏障延迟支缺 return true;");
    let barrier_seg = &place[gate..defer_end];
    // 延迟支必须重新读取屏障 (不能只用上方快照的 on_cpu) —— 复查即 TOCTOU 的闭合点.
    assert!(
        has(barrier_seg, "cfs_on_cpu.load(Ordering::Acquire)"),
        "P-26: 屏障延迟支必须在临界区内复查 cfs_on_cpu —— 靠快照的陈旧 on_cpu 判延迟, \
         会让 reap_off_cpu 夹在快照与置 Ready 之间清屏障, 两条腿同时落空"
    );
    // 复查必须先于置 Ready (同一 with_process 闭包: 先 load 屏障再条件翻 Ready).
    assert!(
        before(
            barrier_seg,
            "cfs_on_cpu.load",
            "set_state_safe(ProcessState::Ready",
        ),
        "P-26: 延迟投送必须在复查到屏障在位后才翻 Ready —— 顺序倒置即回到 TOCTOU"
    );
    // 延迟支仍不得动任何一棵树 (落地交占用者核 reap, 或 fall-through 完整搬定路径).
    assert!(
        !has(barrier_seg, "enqueue") && !has(barrier_seg, "dequeue"),
        "P-26: 屏障延迟支复查后仍不得入树 —— 进树即'树上与核上各一份', 被别核拣走即双派"
    );
}

/// 搬动闸门二: 跨核偷取拒搬占用中的任务, 且不撤它的账 (账属源核).
#[test]
fn steal_refuses_barred_candidate_without_unaccounting() {
    let sched = code_only(&read_src(SCHED));
    let steal = code_only(&fn_body(&sched, "fn steal_locked"));
    assert!(
        has(&steal, "p.cfs_on_cpu.load(Ordering::Acquire)"),
        "P-21: 偷取候选同样要过屏障判据"
    );
    assert!(
        before(&steal, "if on_cpu {", "let schedulable ="),
        "P-21: 拒搬必须早于'不可调度即撤账摘除'分支 —— 屏障期任务只是暂时不可搬, \
         走摘除分支会把源核的账扣穿"
    );
}

/// prev 落地: 走搬定唯一入口的 `owner_landing` 形态 (凭证 + 去重 + 记账一次闭合).
#[test]
fn prev_landing_goes_through_the_single_placement_entry() {
    let sched = code_only(&read_src(SCHED));
    let schedule_body = code_only(&fn_body(&sched, "pub fn schedule"));
    assert_eq!(
        occurrences(&schedule_body, "cfs_place_on_inner("),
        1,
        "P-21: prev 落地必须走搬定入口 (认领凭证 + 锁序 + 去重 + 落点回写不在之外另起炉灶)"
    );
    // 唯一一处 `owner_landing = true`: 只有占用者本核的 prev 收尾可以放行屏障.
    assert_eq!(
        occurrences(&sched, "get_current_cpu(), true)"),
        1,
        "P-21: 屏障放行不得出现在 prev 收尾之外的调用点 —— 多点放行等于无屏障"
    );
    assert!(
        has(
            &fn_body(&sched, "fn cfs_place_on"),
            "self.cfs_place_on_inner(pid, dst_cpu, false)"
        ),
        "P-21: 公有入口必须把 `owner_landing` 固定为 false"
    );
    // 负控制: 旧的裸 `requeue` 与"只持本核锁就投节点"的编程面都不得复活.
    assert!(
        !has(&schedule_body, ".requeue(") && !has(&sched, "cfs_rq.lock().enqueue("),
        "P-21: 裸 insert / 绕过认领的入队会重新打开双树与漏记账窗口"
    );
    let cfs = code_only(&read_src(CFS));
    assert!(
        !has(&cfs, "fn requeue"),
        "P-21: `CfsRunQueue::requeue` 必须由类型 API 层面移除 (§5 F9: 不留死代码)"
    );
}

/// 第二条腿: 清屏障后立即补投仍不可见的 `Ready` 任务.
#[test]
fn reap_redelivers_ready_task_left_off_tree() {
    let sched = code_only(&read_src(SCHED));
    let reap = code_only(&fn_body(&sched, "fn reap_off_cpu"));
    assert!(
        before(&reap, "cfs_on_cpu.store(false", "cfs_place_on(prev"),
        "P-21: 补投必须在清屏障之后 —— 屏障未清时 `place_locked` 只会再次延迟投送, \
         任务永远落不了地"
    );
    assert!(
        has(&reap, "state == ProcessState::Ready") && has(&reap, "!on_rq"),
        "P-21: 补投判据必须是 `Ready 且不在可运行记账` (正是被摘走后的隐形形态)"
    );
    assert!(
        has(&reap, "SchedPolicy::Normal"),
        "P-21: 补投只针对 CFS —— RT/DL 的 `cfs_on_rq` 恒为 false, 不加策略限定会把\
         实时任务投进 CFS 树"
    );
    assert!(
        has(&reap, "per_cpu.idle.load"),
        "P-21: idle 任务不属任何运行队列, 补投必须排除它 (否则本核 CFS 会被 idle \
         占据最小键位而饥饿)"
    );
}

/// 电平重扫不得充当第三条腿: 新创建但尚未搬定的任务与孤儿形状完全相同.
///
/// 实测回归 (本轮施工): 在 `tick` 的睡眠扫描里加“`Ready` 且不在队不占核 => 重投”后,
/// boot 在 1.24s (`[ELF] self.create OK` 后的首次 AP tick) 崩溃: 新建进程初始
/// 状态就是 `Ready` (`user_proc.rs` 创建即置), 其 `ProcessContext` 尚未装配 ⇒
/// AP 拣中它并切过去, 恢复出一全零上下文 (CR3=0) => 取指 #PF => 二重/三重故障.
/// 因此 P-21 只允许**占用者核**的两条腿收尾 (它们天然只面对已被派发过的任务).
#[test]
fn tick_scan_still_selects_only_blocked_tasks() {
    let sched = code_only(&read_src(SCHED));
    let tick = code_only(&fn_body(&sched, "pub fn tick"));
    assert!(
        !has(&tick, "cfs_on_rq") && !has(&tick, "cfs_on_cpu"),
        "P-21: tick 的电平重扫不得拿 `Ready 且不在队` 当选题判据 —— 该形状与\n         '刚创建还未装配上下文的进程' 不可区分 (扇中将切到全零上下文 => 三重故障)"
    );
    assert!(
        !has(&tick, "ProcessState::Ready"),
        "P-21: 屏障窗口的落地只由占用者核收尾 (prev 落地 / `reap_off_cpu` 补投),\n         不依赖全局重扫"
    );
}
