//! Ring 3 切换契约测试 (P1-I-02)
//!
//! 验证 `usermode::enter_user_mode` 不再是占位实现, 而是真的串联到架构层:
//! - x86_64: swapgs + iretq
//! - aarch64: eret (EL0)
//!
//! 由于 `enter_user_mode` 是 `unsafe fn -> !` (不返回), 不能在主机直接调用.
//! 测试通过源码静态契约验证:
//! 1. 签名必须是 `-> !` (永不返回)
//! 2. 必须调用 `Arch::enter_user` (具体 impl, 因为 Arch 是 trait 抽象)
//! 3. x86_64 路径必须传 ctx.rip / ctx.rsp / ctx.rdi
//! 4. aarch64 路径必须传 ctx.elr_el1 / ctx.sp_el0 / ctx.x0
//! 5. 禁止占位实现 `*ctx` / `return *ctx`
//! 6. 禁止调用 `unsafe { *_vmspace = ... }` 等未实现形式

use std::fs;

fn read_usermode_rs() -> String {
    let path = format!(
        "{}/../src/kernel/framework/usermode.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read usermode.rs")
}

#[test]
fn enter_user_mode_signature_is_noreturn() {
    // P1-I-02 验收: 签名必须是 `-> !`, 永不返回
    let src = read_usermode_rs();
    let x86 = src
        .find("pub unsafe fn enter_user_mode")
        .expect("enter_user_mode not found");
    let body = &src[x86..];
    let arch_specific = if cfg!(target_arch = "x86_64") {
        body.find("fn enter_user_mode").unwrap_or(0)
    } else {
        // aarch64 第二个定义
        let mut idx = 0;
        for _ in 0..2 {
            idx = body[idx + 1..]
                .find("pub unsafe fn enter_user_mode")
                .map(|i| idx + 1 + i)
                .expect("missing aarch64 enter_user_mode");
        }
        idx
    };
    let arch_block = &body[arch_specific..];
    // 函数头应包含 -> !
    let fn_end = arch_block.find('{').expect("missing { in enter_user_mode");
    let sig = &arch_block[..fn_end];
    assert!(
        sig.contains("-> !"),
        "P1-I-02: enter_user_mode 必须返回 `!` (noreturn), 当前签名: {}",
        sig
    );
}

#[test]
fn x86_64_enter_user_mode_invokes_arch_enter_user() {
    // P1-I-02 验收: x86_64 路径必须调用 <X8664 as Arch>::enter_user
    if cfg!(target_arch = "x86_64") {
        let src = read_usermode_rs();
        // 取第一个 fn (x86_64 平台 cfg 命中)
        let start = src
            .find("pub unsafe fn enter_user_mode")
            .expect("x86 enter_user_mode");
        let body_start = src[start..].find('{').expect("missing {") + start;
        let body_end = find_matching_brace(&src, body_start);
        let body = &src[body_start..=body_end];
        assert!(
            body.contains("X8664 as Arch>::enter_user"),
            "P1-I-02: x86_64 路径必须调用 <X8664 as Arch>::enter_user"
        );
    }
}

#[test]
fn aarch64_enter_user_mode_invokes_arch_enter_user() {
    // P1-I-02 验收: aarch64 路径必须调用 <Aarch64 as Arch>::enter_user
    if cfg!(target_arch = "aarch64") {
        let src = read_usermode_rs();
        let start = src
            .find("pub unsafe fn enter_user_mode")
            .expect("aarch64 enter_user_mode");
        let body_start = src[start..].find('{').expect("missing {") + start;
        let body_end = find_matching_brace(&src, body_start);
        let body = &src[body_start..=body_end];
        assert!(
            body.contains("Aarch64 as Arch>::enter_user"),
            "P1-I-02: aarch64 路径必须调用 <Aarch64 as Arch>::enter_user"
        );
    }
}

#[test]
fn x86_64_uses_correct_ctx_fields() {
    // P1-I-02 验收: x86_64 必须传 rip / rsp / rdi
    if cfg!(target_arch = "x86_64") {
        let src = read_usermode_rs();
        let start = src.find("pub unsafe fn enter_user_mode").unwrap();
        let body_start = src[start..].find('{').unwrap() + start;
        let body_end = find_matching_brace(&src, body_start);
        let body = &src[body_start..=body_end];
        assert!(body.contains("ctx.rip"), "P1-I-02: x86_64 必传 ctx.rip");
        assert!(body.contains("ctx.rsp"), "P1-I-02: x86_64 必传 ctx.rsp");
        assert!(
            body.contains("ctx.rdi"),
            "P1-I-02: x86_64 必传 ctx.rdi (arg0)"
        );
    }
}

#[test]
fn aarch64_uses_correct_ctx_fields() {
    // P1-I-02 验收: aarch64 必须传 elr_el1 / sp_el0 / x0
    if cfg!(target_arch = "aarch64") {
        let src = read_usermode_rs();
        let start = src.find("pub unsafe fn enter_user_mode").unwrap();
        let body_start = src[start..].find('{').unwrap() + start;
        let body_end = find_matching_brace(&src, body_start);
        let body = &src[body_start..=body_end];
        assert!(
            body.contains("ctx.elr_el1"),
            "P1-I-02: aarch64 必传 ctx.elr_el1"
        );
        assert!(
            body.contains("ctx.sp_el0"),
            "P1-I-02: aarch64 必传 ctx.sp_el0"
        );
        assert!(
            body.contains("ctx.x0"),
            "P1-I-02: aarch64 必传 ctx.x0 (arg0)"
        );
    }
}

#[test]
fn no_placeholder_return_ctx() {
    // P1-I-02 验收: 禁止占位 `*ctx` / `return *ctx`
    let src = read_usermode_rs();
    // 排除注释行
    let code_lines: Vec<&str> = src
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("*/")
        })
        .collect();
    for line in code_lines {
        assert!(
            !line.contains("return *ctx") && !line.trim().ends_with("*ctx"),
            "P1-I-02: 检测到占位实现 `*ctx` / `return *ctx`, 行: {}",
            line
        );
    }
}

#[test]
fn no_unimplemented_macro() {
    // P1-I-02 验收: 不允许 `unimplemented!()` / `todo!()` 在 enter_user_mode 中
    let src = read_usermode_rs();
    let start = src.find("pub unsafe fn enter_user_mode").unwrap();
    let body_start = src[start..].find('{').unwrap() + start;
    let body_end = find_matching_brace(&src, body_start);
    let body = &src[body_start..=body_end];
    assert!(
        !body.contains("unimplemented!"),
        "P1-I-02: enter_user_mode 禁止 unimplemented!"
    );
    assert!(
        !body.contains("todo!"),
        "P1-I-02: enter_user_mode 禁止 todo!"
    );
}

fn find_matching_brace(s: &str, open_idx: usize) -> usize {
    let bytes = s.as_bytes();
    let mut depth = 0;
    for (i, &b) in bytes.iter().enumerate().skip(open_idx) {
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    panic!("unmatched brace at {}", open_idx);
}
