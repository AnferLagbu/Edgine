// SPDX-License-Identifier: MPL-2.0
// P-78 回归锁: 首个用户进程必须在进入用户态**之前**落下 P-21 投送屏障.
//
// # 缺陷机制 (q_int2 第 21 轮 `-d int` 直接取证)
//
// 首个用户进程经 `enter_user_process` 的 iretq 直入用户态, 从不经过
// `schedule()` 的 `next_snap` (屏障唯一置位点). 于是它运行期间
// `eevdf_on_cpu == false` 且 `eevdf_rq_cpu == NO_RQ_CPU`:
//   - `place_barrier` 看不见它 (on_cpu 为假);
//   - `place_locked` 的 `src.is_running` 判据看不见它 (src 为 None);
//   - `on_current` 的源核核查看不见它 (src_cpu == NO_RQ_CPU).
// 子进程退出触发的 `wake_child_waiters → unblock` 于是在 init 的 block→switch
// 窗口内把它投到**另一个核**: 两核以同一 CR3 / 同一用户 SP 运行同一进程
// (实测 int 日志 CPU0 与 CPU1 相继以 CR3=0x7eef000、SP=0x7ffffff9ae10 进异常),
// 共享 16 KiB 内核栈互踩. 次核随后 #UD 于 `lockdep::acquire` 内的调用字节中点
// (RIP=0x210304, 0x210301 处 `call emit_all` 的第 4 字节), 走进 panic → halt;
// 主核停摆在 `LOG_EMIT_LOCK` 的自旋里 (IF=0), 串口永久静默.
//
// # 修复形态 (= 本测试的断言对象)
//
// 1. `enter_user_process` 在 `init_first_entry_context` 之后、进入用户态之前把
//    该进程的 `eevdf_on_cpu` 置 true —— 屏障从首次上核起即覆盖它;
// 2. 置位对象必须是 `USER_PROC_MANAGER.get(pid)` 返回的权威 `Process`
//    (非 C_CURRENT_PROCESS 镜像);
// 3. 清位仍只归 `reap_off_cpu`: 首个 prev 收尾后由下一轮 `schedule()` 入口
//    清位并补投, 不得在入口路径或别处提前清.
//
// 跨核竞态无法在主机侧构造 (无真 SMP 内核上下文), 故窗口形状用源码文本契约
// 锁死 (剥注释 + 去空白; 不受 rustfmt 换行影响). 文本谓词的判别力由负控制
// 用合成回退样本反向验证.

use std::fs;

const API: &str = "../src/kernel/privileged/proc/api.rs";
const SCHED: &str = "../src/kernel/privileged/proc/scheduler.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 剥除注释行: 修复说明本身含被禁形态, 不剥会误判.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && !l.starts_with("/*") && !l.starts_with('*'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 在测试模块声明处截断, 使内核内联用例不污染契约.
fn truncate_test_module(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#[cfg(test)]") || line.starts_with("#[cfg(all(test") {
            let next = lines.get(i + 1).unwrap_or(&"");
            if next.starts_with("mod ") || next.starts_with("pub mod ") {
                return lines[..i].join("\n");
            }
        }
    }
    src.to_string()
}

fn squash(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

fn prod_squashed(path: &str) -> String {
    squash(&truncate_test_module(&strip_comments(&read(path))))
}

/// 取 `sig` 函数体 (花括号配对; 输入须已剔注释并 squash).
fn fn_body<'a>(code: &'a str, sig: &str) -> Option<&'a str> {
    let at = code.find(sig)?;
    let open = code[at..].find('{')? + at;
    let mut depth = 0usize;
    for (i, ch) in code[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&code[open..=(open + i)]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 入口屏障谓词: 置位落在"上下文填充之后、进入用户态之前".
fn boot_entry_barrier(body: &str) -> bool {
    let Some(init) = body.find("init_first_entry_context(proc);") else {
        return false;
    };
    let Some(store) = body.find("(*proc).process().eevdf_on_cpu.store(true,Ordering::Release);")
    else {
        return false;
    };
    let Some(enter) = body.find("user_proc_enter_by_pid(pid);") else {
        return false;
    };
    init < store && store < enter
}

/// 清位单点谓词: 全文件唯一的 `eevdf_on_cpu` 清位落在 `reap_off_cpu` 体内.
fn single_clear_site(sched: &str) -> bool {
    if sched.matches("eevdf_on_cpu.store(false").count() != 1 {
        return false;
    }
    fn_body(sched, "fnreap_off_cpu(").is_some_and(|body| body.contains("eevdf_on_cpu.store(false"))
}

// ============================================================================
// 形状层: 首进程进入用户态前必须自行落下投送屏障
// ============================================================================

#[test]
fn boot_process_marks_itself_on_cpu_before_entering_user_mode() {
    let api = prod_squashed(API);
    let body = fn_body(&api, "fnenter_user_process(").expect("enter_user_process 不存在");
    assert!(
        boot_entry_barrier(body),
        "enter_user_process 必须在 init_first_entry_context 之后、user_proc_enter_by_pid \
         之前置 eevdf_on_cpu: 首进程不经 schedule() 上核, 屏障/在核核查/拣中判据对它全盲, \
         block→switch 窗口内的一次子进程退出唤醒即可让两核交叠同一内核栈 (P-78)"
    );
    assert!(
        body.contains("ifletSome(proc)=USER_PROC_MANAGER.get(pid){"),
        "置位必须挂在 USER_PROC_MANAGER.get(pid) 的 Some 分支内: 对象是权威 Process, \
         且进程缺失时不得留下无人清位的屏障"
    );
    assert_eq!(
        api.matches("eevdf_on_cpu.store(true").count(),
        1,
        "api.rs 只允许一处屏障置位 (首进程入口), 新增置位点须重新核对清位归属"
    );
}

#[test]
fn schedule_keeps_its_own_barrier_setting_and_reap_stays_sole_clear_site() {
    let sched = prod_squashed(SCHED);
    assert_eq!(
        sched.matches("eevdf_on_cpu.store(true").count(),
        1,
        "scheduler.rs 的屏障置位保留在 schedule() 拣中 next 处, 不得新增旁路"
    );
    let schedule_body = fn_body(&sched, "fnschedule(").expect("schedule 不存在");
    assert!(
        schedule_body.contains("eevdf_on_cpu.store(true"),
        "schedule() 拣中 next 时置位屏障 (P-21 唯一置位点的原语义)"
    );
    assert!(
        single_clear_site(&sched),
        "eevdf_on_cpu 的清位必须唯一落在 reap_off_cpu: 那里才确证上一次切换已完成"
    );
    let reap = fn_body(&sched, "fnreap_off_cpu(").expect("reap_off_cpu 不存在");
    let clear_at = reap
        .find("eevdf_on_cpu.store(false")
        .expect("reap 清位缺失");
    let set_at = reap
        .find("eevdf_placed_once.store(true")
        .expect("reap 必须在离核确证时置 placed_once (补投认领失败交 tick 恢复网)");
    let place_at = reap.find("Self::eevdf_place_on(").expect("reap 补投腿缺失");
    assert!(
        clear_at < place_at,
        "reap 必须先清位再补投: 补投经 place_locked 的屏障判据, 清位在后会把 prev 挡在自己队列外"
    );
    assert!(
        clear_at < set_at && set_at < place_at,
        "placed_once 必须在屏障清位之后、补投之前落下: 补投失败时 tick 恢复网才能接手"
    );
    assert_eq!(
        sched.matches("eevdf_placed_once.store(true").count(),
        4,
        "placed_once 置位点只允许四处 (place_locked 入树/拣中 + tick 切片重投 + steal 迁移 + reap 离核确证), 新增点须复核恢复射程"
    );
}

// ============================================================================
// 负控制: 谓词必须在回归真的发生时亮起
// ============================================================================

#[test]
fn boot_entry_barrier_negative_controls() {
    let old_shape = squash(
        "ifletSome(proc)=USER_PROC_MANAGER.get(pid){\
         unsafe{super::user_proc::UserProcManager::init_first_entry_context(proc);}}\
         user_proc_enter_by_pid(pid);",
    );
    let store_after_enter = squash(
        "ifletSome(proc)=USER_PROC_MANAGER.get(pid){\
         unsafe{super::user_proc::UserProcManager::init_first_entry_context(proc);}}\
         user_proc_enter_by_pid(pid);\
         (*proc).process().eevdf_on_cpu.store(true,Ordering::Release);",
    );
    for (name, fixture) in [
        ("旧形态: 入口不置屏障", &old_shape),
        ("补丁式: 置位排在进入用户态之后", &store_after_enter),
    ] {
        assert!(
            !boot_entry_barrier(fixture),
            "负控制失效: 合成回退样本 {name:?} 未被识别 —— 入口屏障门禁已形同虚设"
        );
    }

    let two_clear_sites = squash(
        "fnreap_off_cpu(per_cpu:&PerCpuSched){\
         p.eevdf_on_cpu.store(false,Ordering::Release);}",
    ) + &squash(
        "fnenter_user_process(pid:u32){p.eevdf_on_cpu.store(false,Ordering::Release);}",
    );
    assert!(
        !single_clear_site(&two_clear_sites),
        "负控制失效: 第二处清位未被识别 —— 提前清位即允许两核交叠内核栈"
    );
    let clear_outside_reap = squash(
        "fnreap_off_cpu(per_cpu:&PerCpuSched){Self::eevdf_place_on(prev,0,PlacementKind::Requeue);}",
    );
    assert!(
        !single_clear_site(&clear_outside_reap),
        "负控制失效: 清位不在 reap 体内未被识别"
    );
}
