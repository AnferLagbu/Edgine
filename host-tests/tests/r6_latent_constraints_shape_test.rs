// SPDX-License-Identifier: MPL-2.0
//! R6 回归: latent 约束与别名七项 (⑪ ⑫ ⑬ ㊻ ㊸ ㊶ ㊼) 的源码形状锁.
//!
//! 这七项的共同特征是"约束/生命周期/别名"面, 多数无法在 host 侧以运行时断言
//! 复现 (对齐 UB、伪造 `'static`、`noreturn` 落空都只在特定实例化/执行路径下
//! 成立). 故本文件钉住修复后的源码形状; 把任一项回退成缺陷形态即转红:
//!
//! - ⑪ `RwLock<T>: Sync` 只要求 `T: Send` (缺 `T: Sync`)
//! - ㊻ `RecoverableMutex<T>: Sync` 要求 `T: Sync` (内层锁要求的是 `T: Send`)
//! - ⑫ `slab_dump_cache` 用 `from_utf8_unchecked` 且无长度上限
//! - ⑬ `ensure_pml4_user` / `ensure_path_user` 不持 `VMM_LOCK` 做页表 RMW
//! - ㊸ `cstr_slice` 返回伪造的 `&'static [u8]`
//! - ㊶ `int 0x82` 的 `iretq` 回到 asm 之后, 与 `options(noreturn)` 矛盾
//! - ㊼ `elf_validate` 构造对齐 `&Elf64Header` 并返回伪造 `'static`

use std::fs;
use std::path::PathBuf;

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

fn find_or_panic(hay: &str, needle: &str, ctx: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("{ctx} 未出现 `{needle}`: {hay}"))
}

#[test]
fn rwlock_sync_bound_requires_send_and_sync() {
    let src = code_only(&read_src("src/kernel/privileged/sync/rwlock.rs"));
    assert!(
        src.contains("unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}"),
        "⑪: RwLock 的 Sync impl 必须同时约束 T: Send + Sync"
    );
    assert!(
        !src.contains("unsafe impl<T: ?Sized + Send> Sync for RwLock<T> {}"),
        "⑪: 仅约束 T: Send 的旧形态必须消失"
    );
}

#[test]
fn recoverable_mutex_sync_bound_follows_inner_spinlock() {
    let src = code_only(&read_src("src/kernel/privileged/freg/recoverable.rs"));
    assert!(
        src.contains("unsafe impl<T: Snapshot + Send> Sync for RecoverableMutex<T> {}"),
        "㊻: 外层 Sync 约束须与内层 IrqSpinLock 的 T: Send 同口径"
    );
    assert!(
        !src.contains("unsafe impl<T: Snapshot + Sync> Sync for RecoverableMutex<T> {}"),
        "㊻: 要求 T: Sync 的旧形态必须消失"
    );
    assert!(
        src.contains("unsafe impl<T: Snapshot + Send> Send for RecoverableMutex<T> {}"),
        "㊻: Send impl 仍只要求 T: Send"
    );
}

#[test]
fn slab_dump_cache_uses_checked_utf8_and_bounded_scan() {
    let src = code_only(&read_src("src/kernel/privileged/mm/slab.rs"));
    let body = function_body(&src, "pub unsafe extern \"C\" fn slab_dump_cache");
    assert!(
        body.contains("cstr_into"),
        "⑫: 名字扫描必须走带调用方上限的共享读取器"
    );
    assert!(body.contains("from_utf8"), "⑫: 必须显式校验 UTF-8");
    assert!(
        !body.contains("from_utf8_unchecked"),
        "⑫: 非 UTF-8 即 UB 的 unchecked 形态必须消失"
    );
    assert!(
        src.contains("const MAX_CACHE_NAME_LEN"),
        "⑫: 扫描上限必须由显式常量承载"
    );
}

#[test]
fn vmm_user_flag_rmw_holds_vmm_lock() {
    let src = code_only(&read_src("src/kernel/privileged/mm/vmm_x86_64.rs"));
    for header in [
        "pub fn ensure_pml4_user(virt: u64)",
        "pub fn ensure_path_user(virt: u64)",
    ] {
        let body = function_body(&src, header);
        let acquire = find_or_panic(body, "acquire_lock()", header);
        let rmw = find_or_panic(body, "&mut *", header);
        let release = find_or_panic(body, "release_lock(", header);
        assert!(
            acquire < rmw && rmw < release,
            "⑬: {header} 的页表读-改-写必须整体落在 VMM_LOCK 临界区内"
        );
    }
}

#[test]
fn klog_cstr_slice_borrows_caller_buffer() {
    let src = code_only(&read_src("src/kernel/privileged/klog/mod.rs"));
    assert!(
        src.contains("unsafe fn cstr_slice(ptr: *const u8, out: &mut [u8; CSTR_MAX]) -> &[u8]"),
        "㊸: cstr_slice 必须返回借自调用方缓冲的切片"
    );
    assert!(
        !src.contains("-> &'static [u8]"),
        "㊸: 伪造 `&'static [u8]` 的形态必须消失"
    );
}

#[test]
fn panic_halt_noreturn_is_satisfied_by_0x82_retarget() {
    let lib = code_only(&read_src("src/kernel/lib.rs"));
    let body = function_body(&lib, "fn panic_halt() -> !");
    assert!(
        body.contains("options(noreturn)"),
        "㊶: panic_halt 保持 noreturn 契约"
    );

    let table = code_only(&read_src("src/kernel/privileged/idt/table.rs"));
    assert!(
        table.contains("vector == 0x82"),
        "㊶: IDT 必须专门处理 0x82 恢复向量"
    );
    let recover = function_body(&table, "unsafe fn handle_recovery_vector");
    assert!(
        recover.contains("freg_halt_loop_addr"),
        "㊶: 0x82 处理器必须把返回 RIP 重定向到停机点"
    );
    assert!(
        recover.contains("(*frame).rip"),
        "㊶: 重定向必须写在异常帧的 RIP 字段上"
    );
}

#[test]
fn elf_validate_returns_header_by_value() {
    let src = code_only(&read_src("src/kernel/privileged/proc/elf/mod.rs"));
    assert!(
        src.contains("pub fn elf_validate(elf_data: &[u8]) -> Option<Elf64Header> {"),
        "㊼: elf_validate 必须按值返回文件头"
    );
    assert!(
        !src.contains("Option<&'static Elf64Header>"),
        "㊼: 伪造 `'static` 的返回形态必须消失"
    );
    let body = function_body(
        &src,
        "pub fn elf_validate(elf_data: &[u8]) -> Option<Elf64Header>",
    );
    assert!(
        body.contains("read_unaligned"),
        "㊼: 必须用 read_unaligned 按值读头, 不构造对齐引用"
    );
    assert!(!body.contains("&*"), "㊼: 不得对任意对齐的缓冲构造共享引用");

    let wrapper = code_only(&read_src("src/kernel/privileged/proc_elf.rs"));
    assert!(
        !wrapper.contains("&'static proc::Elf64Header"),
        "㊼: 转发壳的返回类型必须同步去掉 `'static`"
    );
}

#[test]
fn r6_markers_are_cleared() {
    for rel in [
        "src/kernel/privileged/sync/rwlock.rs",
        "src/kernel/privileged/freg/recoverable.rs",
        "src/kernel/privileged/mm/slab.rs",
        "src/kernel/privileged/mm/vmm_x86_64.rs",
        "src/kernel/privileged/klog/mod.rs",
        "src/kernel/lib.rs",
        "src/kernel/privileged/proc/elf/mod.rs",
    ] {
        let raw = read_src(rel);
        assert!(
            !raw.contains("已登记缺口") && !raw.contains("契约缺口") && !raw.contains("并发面缺口"),
            "{rel} 仍留有本批修复前的缺口登记注释"
        );
    }
}
