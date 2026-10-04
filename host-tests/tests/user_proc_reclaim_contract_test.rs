// SPDX-License-Identifier: MPL-2.0
// 回归测试: 退出进程的 `USER_PROC_MANAGER` 镜像记录回收契约 (ISSUE-RT-006).
//
// 背景:
//   `USER_PROC_MANAGER.processes` 是 `Process` 的 FFI 镜像 (BTreeMap<pid,
//   NonNull<UserProcess>>). 镜像 destroy 负责释放权威 Process 不释放的资源:
//   用户栈物理页、内核栈物理页、BTreeMap 条目、`UserProcess` 分配. 该路径此前
//   在生产路径无调用者 ⇒ 进程退出后镜像记录与栈页永不回收 (每 fork 泄漏一条).
//
// 根因与接线 (本轮):
//   全部权威 `Process` 销毁都收敛到 `ProcessTable::remove_and_free` /
//   `dec_ref_and_maybe_free` 的"引用归零即释放"分支 (scheduler 周期僵尸回收 /
//   exit 孤儿回收 / wait4 / fork 回滚均经此). 回收必须在该分支、`Box::from_raw`
//   **之前** 完成 —— 镜像 destroy 经 cr3 翻译用户栈, 而 `Process::drop` 会销毁该
//   页表 (INV-USER-PROC #2: 镜像须先于权威 Process 被移除).
//
// 验收 (fail-closed, 文本指纹):
//   1. `remove_and_free` 在 `Box::from_raw` 前调用 `USER_PROC_MANAGER.destroy_by_pid`
//   2. `dec_ref_and_maybe_free` (延迟释放分支) 同样回收
//   3. `UserProcManager::destroy` 释放用户栈**先于**销毁页表
//      (否则页表销毁后 cr3 无法翻译用户虚拟地址 ⇒ 静默漏释放)
//   4. `destroy_by_pid` / `destroy_by_pid_no_kstack` 取句柄与 `destroy` 的锁分离
//      (避免 `if let` scrutinee 的 guard 存活到 then 块造成同锁自锁死)

use std::fs;

const PROCESS: &str = "../src/kernel/privileged/proc/process.rs";
const USER_PROC: &str = "../src/kernel/privileged/proc/user_proc.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释 (含 `///` 文档注释), 避免注释文本被误判为代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 从 `start` 起取最多 `len` 字节的窗口, 尾部对齐 UTF-8 字符边界
/// (源码含中文注释, 直接按字节切片会 panic).
fn window(src: &str, start: usize, len: usize) -> &str {
    let mut end = (start + len).min(src.len());
    while end > start && !src.is_char_boundary(end) {
        end -= 1;
    }
    &src[start..end]
}

fn body_of<'a>(src: &'a str, def: &str, len: usize) -> &'a str {
    let at = src.find(def).unwrap_or_else(|| panic!("未找到定义: {def}"));
    window(src, at, len)
}

#[test]
fn test_remove_and_free_reclaims_user_proc_mirror() {
    // 权威销毁的唯一入口分支: 必须在 Process 析构前回收镜像.
    let src = code_only(&read(PROCESS));
    let body = body_of(&src, "pub fn remove_and_free(&self, pid: Pid) {", 1400);
    let reclaim = body
        .find("USER_PROC_MANAGER.destroy_by_pid(pid)")
        .unwrap_or_else(|| panic!("remove_and_free 必须回收 USER_PROC_MANAGER 镜像记录"));
    let drop_box = body
        .find("Box::from_raw")
        .expect("remove_and_free 必须仍以 Box::from_raw 释放 Process");
    assert!(
        reclaim < drop_box,
        "镜像回收必须早于 Process 析构 (Box::from_raw): 镜像 destroy 经 cr3 \
         翻译用户栈, 而 Process::drop 会销毁该页表"
    );
}

#[test]
fn test_dec_ref_and_maybe_free_reclaims_user_proc_mirror() {
    // 引用计数 > 1 时实际释放推迟到本分支, 同样必须回收镜像.
    let src = code_only(&read(PROCESS));
    let body = body_of(
        &src,
        "pub fn dec_ref_and_maybe_free(&self, pid: Pid) {",
        1400,
    );
    let reclaim = body
        .find("USER_PROC_MANAGER.destroy_by_pid(pid)")
        .unwrap_or_else(|| panic!("dec_ref_and_maybe_free 必须回收 USER_PROC_MANAGER 镜像记录"));
    let drop_box = body
        .find("Box::from_raw")
        .expect("dec_ref_and_maybe_free 必须仍以 Box::from_raw 释放 Process");
    assert!(
        reclaim < drop_box,
        "延迟释放分支的镜像回收必须早于 Process 析构 (Box::from_raw)"
    );
}

#[test]
fn test_destroy_frees_user_stack_before_page_table() {
    // 用户栈释放依赖 cr3 翻译; 页表一旦销毁即无法翻译 ⇒ 顺序不可颠倒.
    let src = code_only(&read(USER_PROC));
    let body = body_of(
        &src,
        "fn destroy(&self, proc: NonNull<UserProcess>, keep_kstack: bool) {",
        1800,
    );
    let free_stack = body
        .find("raw::free_phys_page(")
        .unwrap_or_else(|| panic!("destroy 必须释放用户栈物理页"));
    let destroy_table = body
        .find("raw::destroy_user_page_table(")
        .unwrap_or_else(|| panic!("destroy 必须在计数归零时销毁用户页表"));
    assert!(
        free_stack < destroy_table,
        "destroy 必须先释放用户栈 (经 cr3 翻译) 再销毁页表, 否则静默漏释放用户栈"
    );
}

#[test]
fn test_destroy_by_pid_releases_lock_before_destroy() {
    // destroy() 内部会重新获取同一把 IrqSpinLock; 若取句柄与 destroy 同处
    // `if let` 分支 (scrutinee 的 guard 存活到 then 块结束) 将自锁死.
    let src = code_only(&read(USER_PROC));
    for def in [
        "pub fn destroy_by_pid(&self, pid: u32) {",
        "pub fn destroy_by_pid_no_kstack(&self, pid: u32) {",
    ] {
        let body = body_of(&src, def, 900);
        assert!(
            body.contains("let proc = self.processes.lock().get(&pid).copied();"),
            "{def} 必须在独立语句中取句柄 (guard 随语句结束释放)"
        );
        assert!(
            !body.contains("if let Some(proc) = self.processes.lock()"),
            "{def} 不得在 `if let` scrutinee 中持锁取句柄 (会自锁死)"
        );
    }
}
