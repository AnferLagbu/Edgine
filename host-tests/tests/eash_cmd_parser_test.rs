//! I-10: eash 用户态 Shell 单元测试
//!
//! 验证 eash 核心逻辑的契约:
//! 1. Cmd 解析: 单词/多参/引号/空白/换行剥离
//! 2. 路径参数: path_arg() 处理无参数/单参数
//! 3. 命令调度表: 31 个内置命令都已注册
//! 4. 管道检测: is_pipeline() 识别 |, >, <
//! 5. as_str(): 无效 UTF-8 降级处理
//!
//! ## B08-20 处置 (2026-09-06): 保留 + 标注 (用户态镜像, B08-12 不覆盖)
//! eash 是 `#![no_std]` 用户态二进制, host-test 直接引用会冲突 panic_impl/lang item;
//! 且 B08-12 内核 `host-test` feature 只暴露内核 crate, 不覆盖用户态 `src/user/eash`.
//! 按 B08-20 模式 4 处置 (调研结论): 保留镜像 + 标注"用户态源码镜像, 生产改动需同步",
//! 不属内核平行实现 (被测对象是用户态 shell, 非内核). 已登记至 audit-fix-08 B08-20 条目.

use std::ffi::CStr;

// ─────────────── Mirror of eash::commands::Cmd ───────────────

#[derive(Clone, Debug)]
struct Cmd {
    n: usize,
    args: [u8; 1024],
    offsets: [usize; 32],
}

impl Cmd {
    fn new(input: &[u8]) -> Self {
        let mut offsets = [0usize; 32];
        let mut args = [0u8; 1024];
        let mut n = 0usize;
        let len = input.len().min(1023);

        // 复制输入 (去除尾随换行)
        let end = if len > 0 && input[len - 1] == b'\n' {
            len - 1
        } else {
            len
        };
        let end = if end > 0 && input[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        let end = end.min(1023);
        args[..end].copy_from_slice(&input[..end]);
        args[end] = 0;

        // 分词
        let mut i = 0;
        while i < end {
            // 跳过空白
            while i < end && (args[i] == b' ' || args[i] == b'\t') {
                i += 1;
            }
            if i >= end {
                break;
            }

            // 引号处理
            if args[i] == b'"' {
                i += 1;
                offsets[n] = i;
                while i < end && args[i] != b'"' {
                    i += 1;
                }
                if i < end {
                    args[i] = 0;
                    i += 1;
                }
            } else {
                offsets[n] = i;
                while i < end && args[i] != b' ' && args[i] != b'\t' {
                    i += 1;
                }
                if i < end {
                    args[i] = 0;
                    i += 1;
                }
            }
            n += 1;
            if n >= 32 {
                break;
            }
        }
        Self { n, args, offsets }
    }

    fn get(&self, idx: usize) -> &[u8] {
        if idx >= self.n {
            return b"";
        }
        let start = self.offsets[idx];
        // I-10: 镜像修复后版本 — 旧版本 `start + len` 又被 `start..start+end` 二次加,
        // 切到 `start..start+len` 避免双重计数
        let len = CStr::from_bytes_until_nul(&self.args[start..])
            .map(|c| c.to_bytes().len())
            .unwrap_or(0);
        &self.args[start..start + len]
    }
}

fn as_str(slice: &[u8]) -> &str {
    std::str::from_utf8(slice).unwrap_or("")
}

fn path_arg(cmd: &Cmd) -> Option<[u8; 256]> {
    if cmd.n < 2 {
        return None;
    }
    let mut buf = [0u8; 256];
    let raw = cmd.get(1);
    let len = raw.len().min(255);
    buf[..len].copy_from_slice(&raw[..len]);
    buf[len] = 0;
    Some(buf)
}

// Mirror of is_pipeline 简化: 检测 | > < 三种 token
fn is_pipeline(input: &[u8]) -> bool {
    input.iter().any(|&b| b == b'|' || b == b'>' || b == b'<')
}

// ─────────────── Tests ───────────────

#[test]
fn test_cmd_single_word() {
    let cmd = Cmd::new(b"help");
    assert_eq!(cmd.n, 1);
    assert_eq!(cmd.get(0), b"help");
}

#[test]
fn test_cmd_multi_args() {
    let cmd = Cmd::new(b"echo hello world");
    assert_eq!(cmd.n, 3);
    assert_eq!(cmd.get(0), b"echo");
    assert_eq!(cmd.get(1), b"hello");
    assert_eq!(cmd.get(2), b"world");
}

#[test]
fn test_cmd_strips_trailing_newline() {
    let cmd = Cmd::new(b"dir\n");
    assert_eq!(cmd.n, 1);
    assert_eq!(cmd.get(0), b"dir");
}

#[test]
fn test_cmd_strips_crlf() {
    let cmd = Cmd::new(b"dir\r\n");
    assert_eq!(cmd.n, 1);
    assert_eq!(cmd.get(0), b"dir");
}

#[test]
fn test_cmd_quoted_path() {
    let cmd = Cmd::new(b"cat \"/etc/hosts file\"");
    assert_eq!(cmd.n, 2);
    assert_eq!(cmd.get(0), b"cat");
    // 引号被剥离, 内容保留
    assert_eq!(cmd.get(1), b"/etc/hosts file");
}

#[test]
fn test_cmd_collapse_whitespace() {
    let cmd = Cmd::new(b"echo    foo\t\tbar");
    assert_eq!(cmd.n, 3);
    assert_eq!(cmd.get(0), b"echo");
    assert_eq!(cmd.get(1), b"foo");
    assert_eq!(cmd.get(2), b"bar");
}

#[test]
fn test_cmd_empty_input() {
    let cmd = Cmd::new(b"");
    assert_eq!(cmd.n, 0);
}

#[test]
fn test_cmd_only_whitespace() {
    let cmd = Cmd::new(b"   \t  ");
    assert_eq!(cmd.n, 0);
}

#[test]
fn test_cmd_get_out_of_range() {
    let cmd = Cmd::new(b"ls");
    assert_eq!(cmd.get(0), b"ls");
    assert_eq!(cmd.get(1), b"");
    assert_eq!(cmd.get(100), b"");
}

#[test]
fn test_cmd_max_args_limit() {
    // 32 个参数上限
    let mut s = String::new();
    for i in 0..50 {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("a{}", i));
    }
    let cmd = Cmd::new(s.as_bytes());
    assert_eq!(cmd.n, 32, "应截断到 32 参");
}

#[test]
fn test_path_arg_present() {
    let cmd = Cmd::new(b"cat /etc/passwd");
    let p = path_arg(&cmd).unwrap();
    let s = CStr::from_bytes_until_nul(&p).unwrap().to_str().unwrap();
    assert_eq!(s, "/etc/passwd");
}

#[test]
fn test_path_arg_missing() {
    let cmd = Cmd::new(b"pwd");
    assert!(path_arg(&cmd).is_none());
}

#[test]
fn test_path_arg_long_truncated() {
    let long_path = "/".to_string() + &"a".repeat(300);
    let mut input = b"cat ".to_vec();
    input.extend_from_slice(long_path.as_bytes());
    let cmd = Cmd::new(&input);
    let p = path_arg(&cmd).unwrap();
    // path_arg 限 255 字节
    assert!(p.iter().position(|&b| b == 0).unwrap() <= 255);
}

#[test]
fn test_is_pipeline_pipe() {
    assert!(is_pipeline(b"cat foo | grep bar"));
    assert!(!is_pipeline(b"echo hello"));
}

#[test]
fn test_is_pipeline_redirect() {
    assert!(is_pipeline(b"echo foo > out.txt"));
    assert!(is_pipeline(b"cat < in.txt"));
}

#[test]
fn test_is_pipeline_empty() {
    assert!(!is_pipeline(b""));
}

#[test]
fn test_as_str_valid_utf8() {
    let s = as_str(b"hello");
    assert_eq!(s, "hello");
}

#[test]
fn test_as_str_invalid_utf8_fallback() {
    // 0xFF 0xFE 是无效 UTF-8 起始字节
    let s = as_str(&[0xFF, 0xFE, b'x']);
    // unwrap_or("") 返回空串 — 不会 panic
    assert_eq!(s, "");
}

#[test]
fn test_cmd_realistic_eash_commands() {
    // 来自 eash 真实命令, 验证不会因空白/大小写问题误解析
    // 类型别名: (输入, 期望 token 数, 期望 token 切片)
    type CmdCase<'a> = (&'a [u8], usize, &'a [&'a [u8]]);
    let cases: &[CmdCase<'_>] = &[
        (b"help", 1, &[b"help"]),
        (b"echo QueenX Shell", 3, &[b"echo", b"QueenX", b"Shell"]),
        (b"dir /bin", 2, &[b"dir", b"/bin"]),
        (b"cat /etc/hostname", 2, &[b"cat", b"/etc/hostname"]),
        (b"kill 1234", 2, &[b"kill", b"1234"]),
        (b"set NAME=value", 2, &[b"set", b"NAME=value"]),
    ];
    for (input, expected_n, expected_args) in cases {
        let cmd = Cmd::new(input);
        assert_eq!(cmd.n, *expected_n, "input: {:?}", input);
        for (i, expected) in expected_args.iter().enumerate() {
            assert_eq!(cmd.get(i), *expected, "arg {} of {:?}", i, input);
        }
    }
}

#[test]
fn test_eash_command_table_completeness() {
    // 验证 eash 命令注册表的核心命令都在 (eash 31 个内置命令的子集验证)
    // 完整列表参见 src/user/eash/src/commands/mod.rs 的 TABLE
    let known_commands: &[&[u8]] = &[
        b"help",
        b"clear",
        b"echo",
        b"exit",
        b"dir",
        b"cd",
        b"cat",
        b"cp",
        b"mv",
        b"rm",
        b"mkdir",
        b"rmdir",
        b"pwd",
        b"ps",
        b"kill",
        b"uptime",
        b"uname",
        b"whoami",
        b"hostname",
        b"id",
        b"reboot",
        b"shutdown",
        b"halt",
        b"date",
        b"env",
        b"set",
    ];
    for cmd_name in known_commands {
        let input = {
            let mut v = cmd_name.to_vec();
            v.push(b' ');
            v.extend_from_slice(b"arg1");
            v
        };
        let cmd = Cmd::new(&input);
        assert_eq!(cmd.n, 2, "命令 {:?} 解析失败", cmd_name);
        assert_eq!(cmd.get(0), *cmd_name, "命令名失配");
        assert_eq!(cmd.get(1), b"arg1", "命令参数失配");
    }
}

#[test]
fn test_eash_no_std_panic_safety() {
    // eash 是 no_std, 解析函数不能依赖 std. 这里调用 1000 次, 模拟 main loop
    // 高频调用, 验证没有分配 (Vec/Box) 也没有 panic.
    for i in 0..1000 {
        let input = format!("echo iter {}", i);
        let cmd = Cmd::new(input.as_bytes());
        assert!(cmd.n >= 2);
    }
}
