// SPDX-License-Identifier: MPL-2.0
// H2 回归锁 (裁定 A, 入睡侧检查): 阻塞收尾必须在入睡前查致命组信号, 命中即退出.
//
// # 缺陷机制 (aarch64 `[THREAD] FAIL: reap-timeout` 的 H2 根因链)
//
// `process_exit_group` 对兄弟线程投递一次 `SIGKILL` pending + `unblock`:
//   1. 目标恰在"置 pending 之后、入 block 之前"仍 Running 时, `Scheduler::unblock`
//      按 Running 判据丢弃唤醒且不投送 (无 sticky wake);
//   2. 目标随后入睡 (futex GROUP_HOLD 永不变 1), 此后再无人唤醒它;
//   3. tick 只救 `Sleeping + sleep_until>0`, `ready_orphan_due` 只救 Ready,
//      `wait4` 不重发 SIGKILL ⇒ 该兄弟永久 Blocked, 组收不齐 Zombie;
//   4. 父进程 `wait4` 5 s 预算耗尽 ⇒ `reap-timeout`.
// SMP=4 扰动批 4/80 命中 (SMP=2 基线 0/80), 直证见 R-6.
//
// # 修复形态 (= 本测试的断言对象)
//
// 裁定 A (入睡侧检查, Linux `signal_pending_state` 范式): `process_block_finish`
// 在 `Scheduler::schedule()` **之前**查 `SIGKILL` pending + 组退出 / de_thread 标记,
// 命中即 `process_exit` 不入睡. 闭窗: `process_block_prepare` 已置 Blocked 先于本
// 检查; 唤醒方先置 SIGKILL 再 unblock, 故本检查未看到 SIGKILL ⇒ Blocked 已 store
// ⇒ 唤醒方 unblock 必见 Blocked 而投送, 由醒后检查兜底. 醒后检查保留不变.
//
// 裁定 B (唤醒侧轮询重发循环) 曾施工但经 A/B 负控制坐实严重回归 (SMP=2 pre-fix
// 0/60 vs post-fix 19/60 失败, 组长自旋 livelock), 已弃; `process_exit_group` 维持
// 单次投递, 其形状由 thread_group_k06_test 锁定, 不在本测试重复.
//
// # 断言方式
//
// 检查本体依赖进程表与在核上下文, 主机侧无法构造, 故用源码文本契约锁死 (剥注释 +
// 去空白), 判别力由合成回退样本反向验证: 仅醒后检查 (pre-fix) 必须判红.

use std::fs;

const PROC_OPS: &str = "../src/kernel/privileged/proc/proc_ops.rs";
const SIG: &str = "pubfnprocess_block_finish(pid:u32)";

// ============================================================================
// 文本读取与规范化 (与 reap_terminal_state_test 同口径)
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 剥除注释行: 修复说明里会引用被禁形态, 不剥会误判.
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

/// 入睡侧致命检查判据: `Scheduler::schedule()` 之前必须存在一处查 SIGKILL pending
/// (带组退出标记) 并 `process_exit` 的收口. 仅醒后检查 (pre-fix) 不满足.
fn pre_sleep_kill_check(body: &str) -> bool {
    let Some(sched) = body.find("Scheduler::schedule()") else {
        return false;
    };
    let before = &body[..sched];
    before.contains("signal_pending_get()")
        && before.contains("SIGKILL")
        && before.contains("exit_requested()")
        && before.contains("process_exit(")
}

// ============================================================================
// 正向: 真源码与合成修复样本都必须满足入睡侧检查
// ============================================================================

#[test]
fn real_source_checks_kill_before_sleep() {
    let code = prod_squashed(PROC_OPS);
    let body = fn_body(&code, SIG).expect("proc_ops.rs 必须含 process_block_finish");
    assert!(
        pre_sleep_kill_check(body),
        "H2 (裁定 A): process_block_finish 必须在 schedule() 前查致命组信号并退出, \
         否则 Running 窗口被丢弃的唤醒无人兜底, 兄弟永睡 futex ⇒ reap-timeout"
    );
}

#[test]
fn synthetic_fixed_shape_passes() {
    let fixed = squash(
        "{ if pid == 0 { return; } \
          let k = PROCESS_TABLE.with_process(pid, |p| { \
              p.signal_pending_get() & (1u64 << super::signal::SIGKILL) != 0 \
                  && p.group().exit_requested() }).unwrap_or(false); \
          if k { process_exit(0x97f); } \
          Scheduler::schedule(); \
          if k { process_exit(0x97f); } }",
    );
    assert!(
        pre_sleep_kill_check(&fixed),
        "合成修复样本 (schedule 前有检查) 必须被判绿, 证明判据非恒真"
    );
}

// ============================================================================
// 负控制: 合成回退样本必须判红 (判别力反向验证)
// ============================================================================

#[test]
fn negative_pre_fix_post_only_fails() {
    // pre-fix 形态: 检查只在 schedule() 之后 (醒后收口), 入睡侧无检查.
    let pre_fix = squash(
        "{ if pid == 0 { return; } \
          Scheduler::schedule(); \
          let k = PROCESS_TABLE.with_process(pid, |p| { \
              p.signal_pending_get() & (1u64 << super::signal::SIGKILL) != 0 \
                  && p.group().exit_requested() }).unwrap_or(false); \
          if k { process_exit(0x97f); } }",
    );
    assert!(
        !pre_sleep_kill_check(&pre_fix),
        "仅醒后检查 (pre-fix) 必须判红: 它不覆盖永不醒来的 Running 丢唤醒窗口"
    );
}

#[test]
fn negative_no_kill_check_fails() {
    let bare = squash("{ if pid == 0 { return; } Scheduler::schedule(); }");
    assert!(
        !pre_sleep_kill_check(&bare),
        "无任何致命检查的阻塞收尾必须判红"
    );
}

#[test]
fn negative_check_without_exit_fails() {
    // 有检查但不退出 (只 schedule), 不满足"命中即退出"形态.
    let no_exit = squash(
        "{ let k = PROCESS_TABLE.with_process(pid, |p| { \
              p.signal_pending_get() & (1u64 << super::signal::SIGKILL) != 0 \
                  && p.group().exit_requested() }).unwrap_or(false); \
          let _ = k; Scheduler::schedule(); }",
    );
    assert!(
        !pre_sleep_kill_check(&no_exit),
        "schedule 前仅有检查而无 process_exit 必须判红: 检查不退出等于没检查"
    );
}
