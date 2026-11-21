// SPDX-License-Identifier: MPL-2.0
//! R5 回归: `swap_in` 的锁序形状锁.
//!
//! 缺陷形态 (安全缺口册 ⑭): `swap_in` 持 `SWAP.lock` (ref-lock-order 层级 6) 期间
//! 调 `pmm::get_pmm().alloc_page()` (内部取 PMM 锁, 层级 1) ⇒ 6→1 降序嵌套.
//!
//! 修复形态: 页分配前移到取锁之前, 锁内只做 initialized / slot 状态复查与数据搬运;
//! 失败路径先释放守卫再归还页. 本测试按源码形状钉住该次序: 把分配移回锁内、或把
//! `free_page` 移回守卫释放之前, 即红.

use std::fs;
use std::path::PathBuf;

const SWAP: &str = "src/kernel/privileged/mm/swap.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免说明文字被当作代码形态.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 截取以 `header` 开头的函数体 (按大括号配对).
fn function_body<'a>(src: &'a str, header: &str) -> &'a str {
    let start = src
        .find(header)
        .unwrap_or_else(|| panic!("未找到函数 {header}"));
    let open = start
        + src[start..]
            .find('{')
            .unwrap_or_else(|| panic!("{header} 缺少函数体"));
    let mut depth = 0usize;
    for (offset, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("{header} 函数体大括号不配对");
}

#[test]
fn swap_in_allocates_page_before_taking_swap_lock() {
    let src = code_only(&read_src(SWAP));
    let body = function_body(&src, "pub fn swap_in(");
    let alloc = body
        .find("alloc_page()")
        .unwrap_or_else(|| panic!("swap_in 未调用 alloc_page: {body}"));
    let lock = body
        .find("SWAP.lock.lock()")
        .unwrap_or_else(|| panic!("swap_in 未取 SWAP.lock: {body}"));
    assert!(
        alloc < lock,
        "页分配必须在取 SWAP.lock 之前 —— 持层级 6 取层级 1 是降序嵌套: {body}"
    );
}

#[test]
fn swap_in_frees_page_only_after_dropping_swap_guard() {
    let src = code_only(&read_src(SWAP));
    let body = function_body(&src, "pub fn swap_in(");
    let drop_guard = body
        .find("drop(guard)")
        .unwrap_or_else(|| panic!("swap_in 失败路径未显式释放 SWAP.lock 守卫: {body}"));
    let free = body
        .find("free_page(")
        .unwrap_or_else(|| panic!("swap_in 未归还失败路径的页: {body}"));
    assert!(
        drop_guard < free,
        "free_page 必须在释放 SWAP.lock 守卫之后 —— 否则仍是 6→1 嵌套: {body}"
    );
}

#[test]
fn swap_in_rechecks_slot_inside_lock() {
    let src = code_only(&read_src(SWAP));
    let body = function_body(&src, "pub fn swap_in(");
    let lock = body
        .find("SWAP.lock.lock()")
        .unwrap_or_else(|| panic!("swap_in 未取 SWAP.lock: {body}"));
    let check = body
        .find("is_slot_used(slot)")
        .unwrap_or_else(|| panic!("swap_in 未在锁内复查 slot 状态: {body}"));
    assert!(lock < check, "slot 复查必须在 SWAP.lock 临界区内: {body}");
}

#[test]
fn swap_deinit_unreserves_only_after_dropping_swap_guard() {
    // 同族锁序: `swap_deinit` 持 SWAP.lock (6) 调 `unreserve_range` (PMM 锁, 层级 1)
    // 同样是 6→1 降序嵌套. 修复形态 = 锁内只摘状态, 守卫释放后再归还 PMM.
    let src = code_only(&read_src(SWAP));
    let body = function_body(&src, "pub fn swap_deinit(");
    let drop_guard = body
        .rfind("drop(guard)")
        .unwrap_or_else(|| panic!("swap_deinit 未显式释放 SWAP.lock 守卫: {body}"));
    let unreserve = body
        .find("unreserve_range(")
        .unwrap_or_else(|| panic!("swap_deinit 未归还存储区: {body}"));
    assert!(
        drop_guard < unreserve,
        "unreserve_range 必须在释放 SWAP.lock 守卫之后 —— 否则仍是 6→1 嵌套: {body}"
    );
}
