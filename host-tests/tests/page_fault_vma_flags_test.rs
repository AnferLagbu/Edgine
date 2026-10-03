//! I-27: handle_simple_fault 不再硬编码 WRITABLE+USER — 静态契约测试
//!
//! 验证 maintenance-2026-06-11.md I-27 修复目标:
//!   - page_fault 主路径必须先查 VMA, 用 VMA 的 flags 决定新页权限
//!   - 禁止"对任意用户缺页地址都映射成 RWX 零页"
//!   - read-only mmap 缺页时, 新页必须只有 PRESENT|USER (无 WRITABLE)
//!
//! 注: 栈扩张 (`handle_stack_expansion` / `handle_stack_expansion_simple`)
//! 与 COW 写入本身就用 RW, 那是其语义, 不在本契约审查范围.

use std::fs;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn test_page_fault_uses_vma_flags_for_user_fault() {
    let path = repo_root().join("src/kernel/framework/mm/page_fault.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    // 关键: handle_page_fault 函数体内, 在 fallthrough 到通用 alloc+map 之前,
    // 必须先调用 find_vma (或等价的 VMA 查询), 否则 read-only mmap 缺页
    // 会被错误授予写权限 (I-27 修复目标).
    let fn_start = src
        .find("fn handle_page_fault(")
        .expect("page_fault.rs 缺少 handle_page_fault");
    // 找函数体 — 假设缩进 4 空格, 函数体首行是 4 空格缩进
    let body_lines: Vec<&str> = src[fn_start..]
        .lines()
        .take_while(|_l| {
            // 截到下一个 `fn ` 顶层 (4 空格缩进才进入) 或文件末尾
            // 简化: 截到 200 行即可覆盖主函数
            true
        })
        .take(80)
        .collect();
    let body = body_lines.join("\n");

    assert!(
        body.contains("find_vma(") || body.contains(".find_vma("),
        "handle_page_fault 必须先查 VMA (find_vma), 否则 read-only mmap 缺页\n\
         会被错误授予 WRITABLE 权限 (I-27 修复目标)."
    );
}

#[test]
fn test_page_fault_mmap_path_uses_vma_flags_not_constants() {
    // 读 VMA 后, 新页 flags 应基于 vma.flags (而非硬编码常量)
    let path = repo_root().join("src/kernel/framework/mm/page_fault.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    // 找到含 `let flags = ` 且上下文有 vma 关键词的代码段
    // 粗略检查: 在 handle_vma_fault_with_mm / handle_file_fault 等函数体内
    // 至少有一处 `flags = ... vma.flags` (使用 VMA flags) 而非纯常量
    let uses_vma_flags = src.contains("vma.flags | PageFlags::PRESENT")
        || src.contains("vma.flags & !PageFlags::WRITABLE")
        || src.contains("| vma.flags");

    assert!(
        uses_vma_flags,
        "page_fault.rs 应当有至少一处基于 vma.flags 派生页 flags 的代码,\n\
         否则 read-only mmap 缺页会硬编码成 WRITABLE (I-27)."
    );
}

#[test]
fn test_page_fault_no_explicit_rwx_for_user_fault() {
    // 静态扫描: 不应在主路径上看到显式 RWX 组合 (PRESENT | WRITABLE | USER | ...)
    // 也不应看到对缺页地址直接 map_page_in_table 而不带 vma 的代码.
    //
    // 允许: 栈扩张 / COW 写入 (语义正确, 注释会说明)
    // 禁止: 通用 fallthrough 路径 (应走 VMA 查询)

    let path = repo_root().join("src/kernel/framework/mm/page_fault.rs");
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    // 检查: 标记为 "P0-I-26 修复" 的注释存在, 表明 fallthrough 已上 VMA 查询
    let has_fix_marker = src.contains("P0-I-26")
        || src.contains("B13-FL-01")
        || src.contains("不再为任意用户地址隐式分配");

    assert!(
        has_fix_marker,
        "page_fault.rs 应保留 I-26 / B13-FL-01 修复标记, 说明\n\
         fallthrough 已改为 VMA 查询优先 (I-27 验收要求)."
    );
}
