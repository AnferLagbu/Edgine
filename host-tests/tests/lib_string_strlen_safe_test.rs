//! 字符串长度契约测试 (B04-23)
//!
//! 镜像内核 `src/kernel/privileged/lib/string.rs` 中 `strlen` 与 `strlen_safe` 的
//! 关键不变量, 在 host 环境验证:
//! 1. `strlen` C FFI 上界 = `cstr::MAX_CSTR_LEN = 4096` (B04-07 / D1), 防御恶意指针无上界读取
//! 2. `strlen(null)` 返回 0
//! 3. `strlen` 在遇到 `\0` 时立即停止 (包括 `MAX_CSTR_LEN` 截断路径)
//! 4. `strlen_safe(&[i8])` 安全版按字节遍历, 遇 `\0` 停止, 不依赖指针上界
//! 5. `MAX_CSTR_LEN` 边界: 长度刚好 `MAX_CSTR_LEN` 与超过 `MAX_CSTR_LEN` 行为差异
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `strlen_impl` / `strlen_safe_impl` 平行实现, 改引内核真实源码
//! `edgine::kernel::privileged::glue::string::{strlen, strlen_safe}`.
//! 内核 `strlen` 为 `pub unsafe extern "C" fn` (带 `#[unsafe(no_mangle)]`),
//! 测试调用需 unsafe 块; 其扫描上限复用 `cstr::MAX_CSTR_LEN` (D1 统一), 测试侧
//! 直接导入该 pub 常量, 消除镜像常量漂移.
//!
//! ## unsafe 依据单源 (本文件全部 `strlen` 调用点共用)
//!
//! 被探针对象 = `privileged/glue/string.rs` 的 `strlen`. 其契约是「`s` 为 null 或
//! 指向以 `\0` 结尾的可读 C 串」, 而**实际读取范围**由实装循环决定:
//! `while *ptr != 0 && len < MAX_CSTR_LEN` —— `&&` 左侧先求值, 故 `len` 已达
//! `MAX_CSTR_LEN` 时**仍会解引用一次**才由右侧退出. 即读取下标闭区间为
//! `[0, min(MAX_CSTR_LEN, 首个 NUL 下标)]`, 缓冲须在该区间内可读 (无 NUL 且缓冲
//! 恰为 `MAX_CSTR_LEN` 字节时会多读 1 字节, 本文件各用例的缓冲均不落在该形态).
//! null 指针走实装首行 `s.is_null()` 早返回, 不解引用.
//!
//! 本文件所有指针均来自**测试自有的本地 `Vec<i8>`** (`to_cstr` 末尾追加 `\0`,
//! 或 `vec!` 定长填充), 借用在调用期间存活、无并发写, 故只需逐处核对
//! 「缓冲长度 > 上述读取下标上界」. 各调用点注释只引本段 + 写该处的长度与 NUL 位置.

use edgine::kernel::privileged::glue::cstr::MAX_CSTR_LEN;
use edgine::kernel::privileged::glue::string::{strlen, strlen_safe};

/// 辅助: 将 Rust 字符串转为以 `\0` 结尾的 C 风格字节数组
fn to_cstr(s: &str) -> Vec<i8> {
    let mut v: Vec<i8> = s.bytes().map(u8::cast_signed).collect();
    v.push(0);
    v
}

// =============================================================================
// strlen C FFI 测试
// =============================================================================

#[test]
fn test_strlen_null_returns_zero() {
    // SAFETY: 显式 null 检查路径
    let len = unsafe { strlen(core::ptr::null()) };
    assert_eq!(len, 0, "strlen(null) must return 0");
}

#[test]
fn test_strlen_empty_string() {
    let s = to_cstr("");
    // SAFETY: 见模块头单源 —— 缓冲 1 字节 (仅 NUL), 读至下标 0 即停
    let len = unsafe { strlen(s.as_ptr()) };
    assert_eq!(len, 0, "strlen of empty string must be 0");
}

#[test]
fn test_strlen_basic_ascii() {
    let s = to_cstr("hello");
    // SAFETY: 见模块头单源 —— 缓冲 6 字节, NUL 在下标 5 ⇒ 读 0..=5
    let len = unsafe { strlen(s.as_ptr()) };
    assert_eq!(len, 5, "strlen(\"hello\") must be 5");
}

#[test]
fn test_strlen_stops_at_terminator() {
    // 字符串 "abc\0garbage" — strlen 必须在第一个 \0 处停止
    let mut buf: Vec<i8> = b"abc".iter().map(|&b| b.cast_signed()).collect();
    buf.push(0);
    buf.extend(b"garbage".iter().map(|&b| b.cast_signed()));
    // SAFETY: 见模块头单源 —— 缓冲 11 字节, NUL 在下标 3 ⇒ 读 0..=3, 尾部 garbage 不被读
    let len = unsafe { strlen(buf.as_ptr()) };
    assert_eq!(
        len, 3,
        "strlen must stop at first NUL, ignore trailing garbage"
    );
}

#[test]
fn test_strlen_under_max_returns_full_length() {
    // 长度 < MAX_CSTR_LEN: 应当返回完整长度 (前提是 buf 实际以 \0 结尾)
    let s = to_cstr(&"x".repeat(MAX_CSTR_LEN - 1));
    // SAFETY: 见模块头单源 —— 缓冲 MAX_CSTR_LEN 字节, NUL 在下标 MAX-1 ⇒ 读 0..=MAX-1,
    // 未触上限故无「多读 1 字节」形态
    let len = unsafe { strlen(s.as_ptr()) };
    assert_eq!(
        len,
        MAX_CSTR_LEN - 1,
        "strlen below MAX must return full length"
    );
}

#[test]
fn test_strlen_at_max_returns_max() {
    // 长度 == MAX_CSTR_LEN: 应当返回 MAX_CSTR_LEN (合法路径, 因遇到 \0 之前已到上限)
    let s = to_cstr(&"y".repeat(MAX_CSTR_LEN));
    // SAFETY: 见模块头单源 —— 缓冲 MAX+1 字节, NUL 恰在下标 MAX ⇒ 读 0..=MAX 仍在界内
    // (本用例正是「上限处多读 1 字节」的临界形态, 靠 to_cstr 追加的 NUL 兜住)
    let len = unsafe { strlen(s.as_ptr()) };
    assert_eq!(len, MAX_CSTR_LEN, "strlen at MAX boundary returns MAX");
}

#[test]
fn test_strlen_over_max_truncated_to_max() {
    // B04-07 关键不变量: 长度 > MAX_CSTR_LEN 必须被截断到 MAX_CSTR_LEN,
    // 不允许无上界读取 (这是 DECISION-060 决策点的核心)
    // 测试场景: buf 长度 > MAX_CSTR_LEN 且全为非 \0 字符
    let buf: Vec<i8> = vec![b'a'.cast_signed(); MAX_CSTR_LEN + 100];
    // 不放 \0, strlen 必须在 MAX_CSTR_LEN 处停止
    // SAFETY: 见模块头单源 —— 缓冲 MAX+100 字节且无 NUL ⇒ 读 0..=MAX (含上限那次多读),
    // 余 99 字节不被读, 故「截断到 MAX」的断言不依赖越界读
    let len = unsafe { strlen(buf.as_ptr()) };
    assert_eq!(
        len, MAX_CSTR_LEN,
        "strlen must truncate at MAX_CSTR_LEN (no unbounded read)"
    );
}

#[test]
fn test_strlen_with_early_terminator_in_max_buffer() {
    // buf 长度 > MAX_CSTR_LEN, 但中间有 \0, 应在 \0 处停止 (而非 MAX_CSTR_LEN)
    let mut buf: Vec<i8> = vec![b'a'.cast_signed(); MAX_CSTR_LEN + 50];
    buf[10] = 0; // 在位置 10 终止
    // SAFETY: 见模块头单源 —— 缓冲 MAX+50 字节, NUL 在下标 10 ⇒ 读 0..=10
    let len = unsafe { strlen(buf.as_ptr()) };
    assert_eq!(len, 10, "strlen stops at NUL even within MAX window");
}

// =============================================================================
// strlen_safe Rust API 测试
// =============================================================================

#[test]
fn test_strlen_safe_empty() {
    let buf: [i8; 1] = [0];
    assert_eq!(strlen_safe(&buf), 0);
}

#[test]
fn test_strlen_safe_basic() {
    let s = to_cstr("hello world");
    assert_eq!(strlen_safe(&s), 11);
}

#[test]
fn test_strlen_safe_no_terminator_counts_all() {
    // 无 \0 终止符: 与 C 版 strlen 不同, 应返回切片长度 (无上界截断,
    // 因为调用方已知切片长度)
    let buf: [i8; 5] = [
        b'a'.cast_signed(),
        b'b'.cast_signed(),
        b'c'.cast_signed(),
        b'd'.cast_signed(),
        b'e'.cast_signed(),
    ];
    assert_eq!(strlen_safe(&buf), 5, "no NUL → full slice length");
}

#[test]
fn test_strlen_safe_stops_at_first_nul() {
    let buf: [i8; 8] = [
        b'a'.cast_signed(),
        b'b'.cast_signed(),
        0,
        b'x'.cast_signed(),
        b'y'.cast_signed(),
        b'z'.cast_signed(),
        b'w'.cast_signed(),
        0,
    ];
    assert_eq!(strlen_safe(&buf), 2);
}

#[test]
fn test_strlen_safe_large_slice_no_nul() {
    // 与 C 版不同: 安全版不依赖上界, 大切片无 \0 时返回完整长度
    let buf: Vec<i8> = vec![b'k'.cast_signed(); MAX_CSTR_LEN * 2];
    assert_eq!(
        strlen_safe(&buf),
        MAX_CSTR_LEN * 2,
        "strlen_safe has no MAX cap (caller controls slice length)"
    );
}

// =============================================================================
// 对比测试: 同一字符串 C 版与安全版结果一致 (前提是切片以 \0 结尾)
// =============================================================================

#[test]
fn test_strlen_c_and_safe_consistent_for_cstr() {
    for sample in &["", "a", "abc", "kernel", "路径测试", "QEMU NVMe MSI-X"] {
        let cstr = to_cstr(sample);
        // SAFETY: cstr 来自 to_cstr, 以 \0 结尾且不含 null 中间
        let c_len = unsafe { strlen(cstr.as_ptr()) };
        let safe_len = strlen_safe(&cstr);
        assert_eq!(
            c_len,
            sample.len(),
            "C strlen of {sample:?} should equal byte length"
        );
        assert_eq!(
            c_len, safe_len,
            "C strlen and strlen_safe should agree on valid C string {sample:?}"
        );
    }
}

// =============================================================================
// MAX_CSTR_LEN 常量不变量测试
// =============================================================================

#[test]
fn test_strlen_max_is_4096() {
    // B04-07 / D1: strlen 扫描上限统一复用 cstr::MAX_CSTR_LEN = 4096. 若此值变更,
    // 需重新评估是否仍覆盖内核内部合法字符串 (路径 ≤ 256, 命令行 ≤ 256).
    assert_eq!(
        MAX_CSTR_LEN, 4096,
        "MAX_CSTR_LEN must remain 4096 per DECISION-060"
    );
}

#[test]
fn test_strlen_max_covers_kernel_paths() {
    // 假设: 内核合法字符串最大长度 ≤ 256 字节 (路径名 ≤ 256 + 命令行 ≤ 256).
    // MAX_CSTR_LEN = 4096 是 256 的 16 倍, 留有充足 buffer.
    const KERNEL_MAX_CSTR: usize = 256;
    const {
        assert!(
            MAX_CSTR_LEN >= KERNEL_MAX_CSTR * 2,
            "MAX_CSTR_LEN must cover at least 2x kernel max cstr length"
        );
    };
}
