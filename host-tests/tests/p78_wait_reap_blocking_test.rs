// SPDX-License-Identifier: MPL-2.0
// P-78 回归锁: wait4 阻塞等待必须走 "置 Blocked + 子组退出侧唤醒", 不得回到忙轮询.
//
// 缺陷机制 (x86_64 满载启动静默挂死): `wait_reap` 旧实现用 `scheduler_yield`
// 忙轮询; 等待循环期间本核 IF=0 收不到 tick, EEVDF 合格集随 vruntime 冻结,
// 被等待的子进程 (KPTI 探针, vruntime 带 AP 时间线未钳制) 永不成为合格候选,
// 父进程永久等待.
//
// 本锁钉住三条实现事实:
//   1. 等待路径先 `scheduler_block(BlockReason::WaitingForChild)`, 置 Blocked
//      **之后**复核子组状态并 `scheduler_unblock(current_pid)` 回滚 —— 闭合
//      "读退出码 → 置 Blocked" 之间错过唤醒广播的窗口;
//   2. 真正让出 CPU 用 `scheduler_yield_ex` (唤醒返回处收口 SIGKILL 组退出);
//   3. 子组整体退出时, 退出侧唤醒父组内**全部** WaitingForChild 等待者
//      (wait4 可由非组长线程调用, 只唤醒 `parent_tgid` 会漏).

use std::fs;

const WAIT4: &str = "../src/kernel/functions/proc/wait4.rs";
const SCHEDULER: &str = "../src/kernel/privileged/proc/scheduler.rs";

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

#[test]
fn wait_reap_blocks_then_rechecks_before_yield() {
    let src = code_only(&read(WAIT4));
    let non_blocking_at = src
        .find("if non_blocking {")
        .expect("wait_reap 必须保留 WNOHANG 非阻塞早返回");
    let block_at = src
        .find("scheduler_block(BlockReason::WaitingForChild)")
        .expect("阻塞等待必须经 WaitingForChild 置 Blocked (P-78)");
    let recheck_at = src
        .find("let child_ready = api::process_group_exit_code(child_tgid)")
        .expect("置 Blocked 之后必须复核子组退出码 (闭合丢唤醒窗口)");
    let gone_at = src
        .find("let child_gone =")
        .expect("置 Blocked 之后必须复核子组是否已被并发回收");
    let rollback_at = src
        .find("scheduler_unblock(current_pid)")
        .expect("条件已成立时必须回滚阻塞 (Blocked → 可运行记账)");
    let yield_at = src
        .find("scheduler_yield_ex()")
        .expect("真正让出 CPU 必须走 yield_ex (唤醒返回处收口致命信号)");

    assert!(
        non_blocking_at < block_at,
        "WNOHANG 早返回必须先于阻塞 (非阻塞模式不得置 Blocked)"
    );
    assert!(
        block_at < recheck_at && recheck_at < rollback_at,
        "复核必须在置 Blocked 之后、回滚之前"
    );
    assert!(recheck_at < gone_at, "两个复核分支按退出码/存在性顺序读取");
    assert!(rollback_at < yield_at, "回滚分支先于让出 CPU 的默认分支");
    assert!(
        !src.contains("scheduler_yield()"),
        "wait_reap 不得回到 IF=0 忙轮询 scheduler_yield (P-78 根因)"
    );
}

#[test]
fn child_group_exit_wakes_all_waiting_parent_threads() {
    let src = code_only(&read(SCHEDULER));
    let helper_at = src
        .find("fn wake_child_waiters(parent_tgid: Pid)")
        .expect("退出侧必须有 wake_child_waiters 助手 (P-78)");
    let exit_at = src
        .find("pub fn exit(&self, exit_code: u32)")
        .expect("scheduler.rs 必须保留 Scheduler::exit");
    assert!(helper_at < exit_at, "助手定义应位于退出路径之前");

    let body = &src[helper_at..exit_at];
    assert!(
        body.contains("proc.group().members()"),
        "唤醒面必须是父组全体成员 (非组长线程的 wait4 也必须醒来)"
    );
    assert!(
        body.contains("ProcessState::Blocked"),
        "只能唤醒阻塞中的等待者"
    );
    assert!(
        body.contains("BlockReason::WaitingForChild as u32"),
        "只唤醒 WaitingForChild 原因, 不得误醒其它阻塞原因"
    );
    assert!(
        body.contains("Self::unblock(tid)"),
        "逐成员经调度器入队唤醒 (不得裸改状态)"
    );

    let anchor = "if last_member && parent_tgid != 0 {";
    let at = src.find(anchor).expect("退出路径必须保留组末成员判定");
    let block = &src[at..];
    let end = block.find("\n    }").expect("组末成员分支必须有闭合");
    assert!(
        block[..end].contains("Self::wake_child_waiters(parent_tgid)"),
        "组末成员退出分支必须在组退出后唤醒等待者"
    );
    assert_eq!(
        src.matches("wake_child_waiters(").count(),
        2,
        "助手只允许一个定义 + 一个调用点 (防止静默接线漂移)"
    );
}
