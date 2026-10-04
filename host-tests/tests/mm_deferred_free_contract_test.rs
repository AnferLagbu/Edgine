//! TLB shootdown 延迟释放公共层 — 单一实现契约测试
//!
//! 追踪: DECISION-083 (ST-07)
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! 「代发布 + 批次链 + 页帧延迟回收」机制原为 `vmm_x86_64.rs` 私有。DECISION-083
//! 把它抽到架构无关公共层 `privileged/mm/deferred_free.rs`, x86_64 与 aarch64 共用
//! 同一实现 (项目硬约束「内核内部并行实现必须统一为单一规范实现」)。
//!
//! ## 为何用静态契约
//!
//! `deferred_free` 与两个 VMM 后端均为 `privileged` 内部实现, host-tests 无法直接
//! 调用 (需 PMM / SMP 运行期状态)。故本测试以**源码文本分析**固化契约 —— 与同类
//! 测试 `aarch64_smp_contract_test.rs` 同手法, 不引入内核逻辑的平行实现。
//!
//! ## 覆盖契约
//!
//! - 公共层暴露 6 个接口 (`defer_free` / `mark_remote_shootdown` /
//!   `clear_shootdown_flag` / `take_batch` / `release_tail` / `admitted`);
//! - x86_64 后端不再保留机制的本地副本 (静态量 / 链操作 / 结算函数);
//! - 两架构 `release_lock` 出口都接线到公共层收尾;
//! - `mm::release_frame_locked` 已去除架构分叉, 统一走 `defer_free`;
//! - aarch64 页表页/帧归还与映射变更点已接入公共层.

use std::fs;
use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // Edgine workspace root
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

/// 截取 `src` 中 `begin` 起点到其后首个 `end` 之间的片段.
fn slice_between<'a>(src: &'a str, begin: &str, end: &str) -> &'a str {
    let start = src
        .find(begin)
        .unwrap_or_else(|| panic!("未找到起点标记: {}", begin));
    let rest = &src[start..];
    let end_off = rest
        .find(end)
        .unwrap_or_else(|| panic!("未找到终点标记: {}", end));
    &rest[..end_off]
}

const DEFERRED_FREE_RS: &str = "src/kernel/privileged/mm/deferred_free.rs";
const VMM_X86_64_RS: &str = "src/kernel/privileged/mm/vmm_x86_64.rs";
const VMM_AARCH64_RS: &str = "src/kernel/privileged/mm/vmm_aarch64.rs";
const MM_MOD_RS: &str = "src/kernel/privileged/mm/mod.rs";

/// 公共层对外暴露的 6 个接口 (签名逐字固化).
const PUBLIC_API: [&str; 6] = [
    "pub(crate) fn defer_free(frame: u64)",
    "pub(crate) fn mark_remote_shootdown()",
    "pub(crate) fn clear_shootdown_flag() -> bool",
    "pub(crate) fn take_batch() -> u64",
    "pub(crate) fn release_tail(shootdown_needed: bool, batch: u64)",
    "pub(crate) fn admitted() -> u64",
];

/// 机制内部的本地副本符号 —— 只应存在于公共层, 不得在 VMM 后端重复定义.
const FORBIDDEN_LOCAL_COPIES: [&str; 8] = [
    "static SHOOTDOWN_NEEDED",
    "static BATCH_HEAD",
    "static PENDING_HEAD",
    "fn frame_link_next",
    "fn free_chain",
    "fn settle_batch",
    "fn drain_pending",
    "fn frame_link_gen",
];

#[test]
fn deferred_free_public_api_frozen() {
    let src = read(DEFERRED_FREE_RS);
    for sig in PUBLIC_API {
        assert!(
            src.contains(sig),
            "公共层缺少接口 `{}` (x86_64/aarch64 共用契约)",
            sig
        );
    }
}

#[test]
fn x86_64_has_no_local_mechanism_copy() {
    let src = read(VMM_X86_64_RS);
    for sym in FORBIDDEN_LOCAL_COPIES {
        assert!(
            !src.contains(sym),
            "vmm_x86_64.rs 仍保留机制的本地副本 `{}` —— 违反「禁平行实现」",
            sym
        );
    }
}

#[test]
fn both_backends_release_lock_wire_to_common_tail() {
    for (name, path) in [("x86_64", VMM_X86_64_RS), ("aarch64", VMM_AARCH64_RS)] {
        let src = read(path);
        let release = slice_between(&src, "pub fn release_lock", "\n    }");
        assert!(
            release.contains("super::deferred_free::clear_shootdown_flag()"),
            "{} release_lock 必须在持锁期间读-清需远程失效标志",
            name
        );
        assert!(
            release.contains("super::deferred_free::take_batch()"),
            "{} release_lock 必须在持锁期间整条摘走批次链",
            name
        );
        assert!(
            release.contains("super::deferred_free::release_tail(shootdown_needed, batch)"),
            "{} release_lock 必须在释放锁后走公共层收尾",
            name
        );
    }
}

#[test]
fn release_frame_locked_has_no_arch_fork() {
    let src = read(MM_MOD_RS);
    let body = slice_between(&src, "pub(crate) fn release_frame_locked", "\n}");
    assert!(
        body.contains("deferred_free::defer_free(phys.0)"),
        "release_frame_locked 必须统一走公共层 defer_free"
    );
    assert!(
        !body.contains("#[cfg(target_arch"),
        "release_frame_locked 不得再保留 x86_64/aarch64 架构分叉"
    );
    assert!(
        !body.contains("free_page"),
        "release_frame_locked 不得直接立即归还 (须经延迟释放)"
    );
}

#[test]
fn aarch64_wires_deferred_free_and_shootdown() {
    let src = read(VMM_AARCH64_RS);
    // 页表页延迟归还变体必须接入公共层.
    let free_locked = slice_between(&src, "fn free_table_locked", "\n    }");
    assert!(
        free_locked.contains("super::deferred_free::defer_free(paddr)"),
        "aarch64 free_table_locked 必须走公共层 defer_free"
    );
    // 页表修改点必须登记远程失效 (映射变更 / 拆除).
    assert!(
        src.matches("super::deferred_free::mark_remote_shootdown()")
            .count()
            >= 3,
        "aarch64 至少 unmap / destroy / map-替换 三处须登记远程失效"
    );
}

#[test]
fn aarch64_destroy_takes_vmm_lock() {
    // aarch64 destroy_page_table 调用 release_frame_locked / free_table_locked,
    // 二者均要求持 VMM_LOCK —— 故必须在表拆除前 acquire.
    let src = read(VMM_AARCH64_RS);
    let body = slice_between(&src, "pub fn destroy_page_table", "\n    }");
    assert!(
        body.contains("self.acquire_lock()"),
        "aarch64 destroy_page_table 必须持 VMM_LOCK (帧入批次链的单写者前提)"
    );
    assert!(
        body.contains("self.release_lock(&_lock_flags)"),
        "aarch64 destroy_page_table 必须在收尾释放 VMM_LOCK"
    );
}
