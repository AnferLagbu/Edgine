// SPDX-License-Identifier: MPL-2.0
//! R8 回归: 冻结面 (52) 的文档一致性 + asm 契约 (56) 的形状锁与行为覆盖.
//!
//! 52 属 SGEG 现版冻结面, 本批只裁定不施工: 形状锁钉住「模块文档与实装相符」的形态,
//! 并确认数据结构未被顺手改动.
//!
//! 56 的两处 asm 缺口是编译器优化面 UB (`stp` 误标 `readonly` / `rep stosb` 未声明
//! rdi、rcx 被改写), 旧形态在 host 上通常也能跑对, 行为观测无法可靠复现, 故以**源码
//! 形状锁**作负控制判据. 比较族全仓零调用方由本文件的语义矩阵给出真实使用路径,
//! 不使用任何 `#[allow(dead_code)]` (F9).
//!
//! 覆盖:
//! - 形状: aarch64 `stp` 不带 `readonly`/`nomem`; x86_64 `rep stosb` 用 `inout` 声明 rdi/rcx
//! - 形状: 两文件不再留 `已登记缺口`; `AuditLog` 模块文档声明自身不加锁且现版冻结
//! - 行为: `memset` / `memset_optimized` (前缀 / 尾字节 / 守卫字节 / 零长 / 空指针 / 截断)
//! - 行为: `strcmp` / `strncmp` / `memcmp` / `memchr` / `strchr` / `strrchr` / `strstr` 语义矩阵

#[cfg(target_arch = "x86_64")]
use edgine::kernel::privileged::glue::string::memset_optimized;
use edgine::kernel::privileged::glue::string::{
    memchr, memcmp, memset, strchr, strcmp, strncmp, strrchr, strstr,
};
use std::fs;
use std::path::PathBuf;

const STRING_SRC: &str = "src/kernel/privileged/glue/string.rs";
const AUDIT_SRC: &str = "src/kernel/privileged/sgeg/audit.rs";

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

/// 从 `first_insn` 起截到该宏调用的 `);`.
fn asm_block<'a>(body: &'a str, first_insn: &str) -> &'a str {
    let start = body
        .find(first_insn)
        .unwrap_or_else(|| panic!("未找到 asm 指令 {first_insn}"));
    let tail = &body[start..];
    let end = tail
        .find(");")
        .unwrap_or_else(|| panic!("{first_insn} 缺少 asm 宏结尾"))
        + 2;
    &tail[..end]
}

fn to_cstr(s: &str) -> Vec<i8> {
    let mut v: Vec<i8> = s.bytes().map(u8::cast_signed).collect();
    v.push(0);
    v
}

// =============================================================================
// 形状锁: 56 asm 契约
// =============================================================================

#[test]
fn aarch64_stp_does_not_claim_readonly() {
    let src = code_only(&read_src(STRING_SRC));
    let body = function_body(&src, "pub unsafe extern \"C\" fn memset(");
    assert!(
        body.contains("stp {val}, {val}, [{dst}], #16"),
        "56: memset 的 16 字节批量写形态必须仍存在"
    );
    let asm = asm_block(body, "stp {val}");
    assert!(
        asm.contains("options(nostack)"),
        "56: stp asm 须保留 nostack"
    );
    assert!(
        !asm.contains("readonly"),
        "56: stp 写内存, 不得声明 readonly (LLVM 会据此重排/消除后续读)"
    );
    assert!(!asm.contains("nomem"), "56: stp 写内存, 不得声明 nomem");
}

#[test]
fn x8664_rep_stosb_declares_register_clobbers() {
    let src = code_only(&read_src(STRING_SRC));
    let body = function_body(&src, "pub unsafe extern \"C\" fn memset_optimized(");
    let asm = asm_block(body, "\"rep stosb\"");
    assert!(
        asm.contains("inout(\"rdi\") dest => _"),
        "56: rep stosb 递增 rdi, 必须以 inout 声明"
    );
    assert!(
        asm.contains("inout(\"rcx\") n => _"),
        "56: rep stosb 递减 rcx 至 0, 必须以 inout 声明"
    );
    assert!(
        !asm.contains("in(\"rdi\")") && !asm.contains("in(\"rcx\")"),
        "56: rdi/rcx 的只读 in(...) 旧形态必须消失"
    );
    assert!(
        asm.contains("options(nostack)"),
        "56: rep stosb asm 须保留 nostack"
    );
    assert!(
        !asm.contains("readonly"),
        "56: rep stosb 写内存, 不得声明 readonly"
    );
}

// =============================================================================
// 形状锁: 52 冻结面文档一致性 + 缺口登记清零
// =============================================================================

#[test]
fn audit_log_module_doc_matches_implementation() {
    let raw = read_src(AUDIT_SRC);
    let module_doc = raw
        .lines()
        .take_while(|l| l.trim_start().starts_with("//!"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        module_doc.contains("自身**不加锁**"),
        "52: 模块文档须声明 AuditLog 自身不加锁"
    );
    assert!(
        module_doc.contains("现版冻结"),
        "52: 模块文档须写明现版冻结裁定"
    );
    assert!(
        !module_doc.contains("写入经 `IrqSpinLock` 保护"),
        "52: 「写入经 IrqSpinLock 保护」的假称必须消失"
    );
    // 只裁定不施工: 数据结构保持冻结形态
    assert!(
        raw.contains("entries: [AuditEntry; AUDIT_CAPACITY]") && raw.contains("count: AtomicUsize"),
        "52: 冻结面数据结构不得被本批改动"
    );
}

#[test]
fn r8_markers_are_cleared() {
    for rel in [STRING_SRC, AUDIT_SRC] {
        let raw = read_src(rel);
        assert!(
            !raw.contains("已登记缺口") && !raw.contains("批6 待裁"),
            "{rel} 仍留有本批修复前的缺口登记注释"
        );
    }
}

// =============================================================================
// 行为: memset / memset_optimized
// =============================================================================

#[test]
fn memset_writes_exact_prefix_and_returns_dest() {
    let mut buf = [0xAAu8; 32];
    // SAFETY: buf 至少 32 字节可写, 传入 n=10 不越界; c 低 8 位 = 0x5A.
    let ret = unsafe { memset(buf.as_mut_ptr().cast(), 0x5A, 10) };
    assert_eq!(
        ret,
        buf.as_mut_ptr().cast::<core::ffi::c_void>(),
        "memset 须返回入参指针"
    );
    assert!(buf[..10].iter().all(|&b| b == 0x5A), "前 n 字节须被填充");
    assert!(
        buf[10..].iter().all(|&b| b == 0xAA),
        "memset 不得越过 n 字节"
    );
}

#[test]
fn memset_truncates_c_to_low_byte() {
    let mut buf = [0u8; 4];
    // SAFETY: buf 4 字节可写, n=4 恰为容量; 0x1_00FF 的低 8 位是 0xFF.
    unsafe { memset(buf.as_mut_ptr().cast(), 0x1_00FF, 4) };
    assert_eq!(buf, [0xFF; 4], "memset 的填充值须截断为低 8 位");
}

#[test]
fn memset_zero_len_and_null_are_noops() {
    let mut buf = [0x11u8; 4];
    // SAFETY: n=0, 实现首行提前返回, 不解引用/不写入.
    let ret = unsafe { memset(buf.as_mut_ptr().cast(), 0xFF, 0) };
    assert_eq!(ret, buf.as_mut_ptr().cast::<core::ffi::c_void>());
    assert_eq!(buf, [0x11; 4], "n=0 不得写入");
    // SAFETY: 显式 nullptr 路径, 实装首行判空后返回.
    let ret = unsafe { memset(core::ptr::null_mut(), 0xFF, 4) };
    assert!(ret.is_null(), "memset(null) 须返回 null");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn memset_optimized_fills_all_lengths_without_overrun() {
    for n in [1usize, 15, 16, 17, 31, 32, 33, 4096] {
        let mut buf = vec![0xAAu8; n + 4];
        // 起点故意错位 1 字节, 证明 rep stosb 不依赖对齐.
        // SAFETY: buf 有 n+4 字节, 下标 1 在分配内.
        let start = unsafe { buf.as_mut_ptr().add(1) };
        // SAFETY: 从 start 起写 n 字节, 末尾仍有 3 字节守卫.
        let ret = unsafe { memset_optimized(start, 0x5A, n) };
        assert_eq!(ret, start, "memset_optimized 须返回入参指针 (n={n})");
        assert!(
            buf[1..=n].iter().all(|&b| b == 0x5A),
            "n={n}: 前缀须被完整填充"
        );
        assert!(
            buf[n + 1..].iter().all(|&b| b == 0xAA),
            "n={n}: 不得越过 n 字节写守卫区"
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn memset_optimized_truncates_and_handles_noops() {
    let mut buf = [0u8; 8];
    // SAFETY: buf 8 字节可写; 0x2_00AB 低 8 位 = 0xAB.
    unsafe { memset_optimized(buf.as_mut_ptr(), 0x2_00AB, 8) };
    assert_eq!(buf, [0xAB; 8], "填充值须截断为低 8 位");

    let mut buf = [0x22u8; 4];
    // SAFETY: n=0 走实装提前返回路径.
    let ret = unsafe { memset_optimized(buf.as_mut_ptr(), 0xFF, 0) };
    assert_eq!(ret, buf.as_mut_ptr());
    assert_eq!(buf, [0x22; 4], "n=0 不得写入");
    // SAFETY: 显式 nullptr 路径, 实装首行判空后返回.
    let ret = unsafe { memset_optimized(core::ptr::null_mut(), 0xFF, 4) };
    assert!(ret.is_null(), "memset_optimized(null) 须返回 null");
}

// =============================================================================
// 行为: 比较族语义矩阵 (同时构成其真实使用路径)
// =============================================================================

#[test]
fn strcmp_orders_lexicographically() {
    let (a, b) = (to_cstr("abc"), to_cstr("abc"));
    // SAFETY: 两缓冲均 NUL 终止且调用期间存活/不被并发修改.
    assert_eq!(unsafe { strcmp(a.as_ptr(), b.as_ptr()) }, 0);

    let (a, b) = (to_cstr("abc"), to_cstr("abd"));
    // SAFETY: 同上.
    assert!(unsafe { strcmp(a.as_ptr(), b.as_ptr()) } < 0, "abc < abd");

    let (a, b) = (to_cstr("abd"), to_cstr("abc"));
    // SAFETY: 同上.
    assert!(unsafe { strcmp(a.as_ptr(), b.as_ptr()) } > 0, "abd > abc");

    let (a, b) = (to_cstr("abc"), to_cstr("abcd"));
    // SAFETY: 同上; 短串的 NUL 与 'd' 比较 ⇒ 负值.
    assert!(unsafe { strcmp(a.as_ptr(), b.as_ptr()) } < 0, "abc < abcd");

    // SAFETY: 显式 nullptr 组合路径, 实装判空后返回.
    unsafe {
        assert_eq!(strcmp(core::ptr::null(), core::ptr::null()), 0);
        assert!(strcmp(core::ptr::null(), b.as_ptr()) < 0);
        assert!(strcmp(a.as_ptr(), core::ptr::null()) > 0);
    }
}

#[test]
fn strncmp_respects_n_limit() {
    let (a, b) = (to_cstr("abcdef"), to_cstr("abcxyz"));
    // SAFETY: 两缓冲均 NUL 终止; n=3 只读前 3 字节, 在缓冲内.
    assert_eq!(unsafe { strncmp(a.as_ptr(), b.as_ptr(), 3) }, 0);
    // SAFETY: n=4 读到第 4 字节, 仍在缓冲内.
    assert!(unsafe { strncmp(a.as_ptr(), b.as_ptr(), 4) } < 0);
    // SAFETY: n=0 实装首行提前返回, 不解引用.
    let zero = unsafe { strncmp(core::ptr::null(), core::ptr::null(), 0) };
    assert_eq!(zero, 0);
    // SAFETY: 单 null + n>0, 实装判空后返回.
    assert!(unsafe { strncmp(core::ptr::null(), b.as_ptr(), 3) } < 0);
}

#[test]
fn memcmp_compares_exact_n_bytes() {
    let a = [1u8, 2, 3, 4];
    let b = [1u8, 2, 9, 9];
    // SAFETY: 两缓冲各 4 字节, n=2 只读前缀.
    let prefix = unsafe { memcmp(a.as_ptr().cast(), b.as_ptr().cast(), 2) };
    assert_eq!(prefix, 0, "前 n 字节相等即返回 0");
    // SAFETY: n=3 仍在两缓冲内, 第 3 字节不同.
    assert!(unsafe { memcmp(a.as_ptr().cast(), b.as_ptr().cast(), 3) } < 0);
    // SAFETY: n=0 实装首行提前返回, 不解引用.
    unsafe {
        assert_eq!(memcmp(core::ptr::null(), core::ptr::null(), 0), 0);
    }
    // SAFETY: 数组长度相等, n=4 恰好读完.
    assert!(unsafe { memcmp(b.as_ptr().cast(), a.as_ptr().cast(), 4) } > 0);
}

#[test]
fn memchr_finds_within_bound() {
    let buf = [9u8, 0, 7, 5];
    // SAFETY: buf 4 字节可读, n=4 恰为长度; 命中下标 1 的 0.
    let hit = unsafe { memchr(buf.as_ptr(), 0, 4) };
    assert_eq!(hit as usize - buf.as_ptr() as usize, 1);

    // SAFETY: n=3 只扫前 3 字节, 目标 5 不在范围内.
    let outside = unsafe { memchr(buf.as_ptr(), 5, 3) };
    assert!(outside.is_null(), "n 界外命中必须不算");
    // SAFETY: 目标 5 在下标 3, n=4 覆盖.
    let hit = unsafe { memchr(buf.as_ptr(), 5, 4) };
    assert_eq!(hit as usize - buf.as_ptr() as usize, 3);

    // SAFETY: null 与 n=0 均走实装提前返回.
    unsafe {
        assert!(memchr(core::ptr::null(), 5, 4).is_null());
        assert!(memchr(buf.as_ptr(), 5, 0).is_null());
    }
}

#[test]
fn strchr_and_strrchr_locate_first_and_last() {
    let s = to_cstr("abcabc");
    // SAFETY: s NUL 终止且调用期间存活.
    let first = unsafe { strchr(s.as_ptr(), i32::from(b'b')) };
    assert_eq!(first as usize - s.as_ptr() as usize, 1);

    // SAFETY: 同上; 'b' 最后出现在下标 4.
    let last = unsafe { strrchr(s.as_ptr(), i32::from(b'b')) };
    assert_eq!(last as usize - s.as_ptr() as usize, 4);

    // SAFETY: 同上; 'z' 未命中.
    unsafe {
        assert!(strchr(s.as_ptr(), i32::from(b'z')).is_null());
        assert!(strrchr(s.as_ptr(), i32::from(b'z')).is_null());
    }

    // SAFETY: 同上; 搜 NUL 按 C 语义返回终止符位置 (下标 6).
    let term = unsafe { strrchr(s.as_ptr(), 0) };
    assert_eq!(term as usize - s.as_ptr() as usize, 6);
    // SAFETY: 显式 nullptr 路径.
    unsafe {
        assert!(strchr(core::ptr::null(), i32::from(b'a')).is_null());
        assert!(strrchr(core::ptr::null(), i32::from(b'a')).is_null());
    }
}

#[test]
fn strstr_finds_substring_and_handles_empty_needle() {
    let hay = to_cstr("hello world");
    let needle = to_cstr("world");
    // SAFETY: 两缓冲均 NUL 终止且调用期间存活.
    let hit = unsafe { strstr(hay.as_ptr(), needle.as_ptr()) };
    assert_eq!(hit as usize - hay.as_ptr() as usize, 6);

    let missing = to_cstr("mars");
    // SAFETY: 同上; "mars" 不在 haystack.
    assert!(unsafe { strstr(hay.as_ptr(), missing.as_ptr()) }.is_null());

    let empty = to_cstr("");
    // SAFETY: 空 needle 按 C 语义返回 haystack 本身.
    let hit = unsafe { strstr(hay.as_ptr(), empty.as_ptr()) };
    assert_eq!(hit as usize - hay.as_ptr() as usize, 0);

    let longer = to_cstr("hello world!!!");
    // SAFETY: needle 长于 haystack, 实装只读到 haystack 终止符.
    assert!(unsafe { strstr(hay.as_ptr(), longer.as_ptr()) }.is_null());
    // SAFETY: 显式 nullptr 路径: haystack=null 返回 null; needle=null 按实装契约
    // 等同于空串, 返回 haystack 本身.
    unsafe {
        assert!(strstr(core::ptr::null(), needle.as_ptr()).is_null());
        let hit = strstr(hay.as_ptr(), core::ptr::null());
        assert_eq!(hit as usize - hay.as_ptr() as usize, 0);
    }
}
