//! 管道与重定向执行器
//!
//! 解析用户输入中的 | < > >> 并执行多段管道命令。

use core::ffi::CStr;
use core::str::from_utf8;
use userlib::sys::*;
use userlib::*;

const MAX_PIPELINE: usize = 8;
const STDOUT: i32 = 1;
const STDIN: i32 = 0;

#[derive(Clone, Copy, PartialEq)]
enum RedirKind {
    None,
    StdoutNew,    // >
    StdoutAppend, // >>
    StdinFile,    // <
}

struct Segment<'a> {
    cmd_buf: [u8; 256],
    cmd_len: usize,
    argv: [*const u8; 16],
    argc: usize,
    redir_in: Option<&'a [u8]>,
    redir_out: Option<&'a [u8]>,
    redir_kind: RedirKind,
}

pub fn is_pipeline(input: &[u8]) -> bool {
    for &b in input {
        if b == b'|' || b == b'>' || b == b'<' {
            return true;
        }
    }
    false
}

fn find_byte(slice: &[u8], byte: u8, start: usize) -> Option<usize> {
    for i in start..slice.len() {
        if slice[i] == byte {
            // > 后面不能是数字 (避免重定向到 fd)
            if byte == b'>' && i + 1 < slice.len() && slice[i + 1].is_ascii_digit() {
                continue;
            }
            // >> 整体跳过
            if byte == b'>' && i + 1 < slice.len() && slice[i + 1] == b'>' {
                return Some(i);
            }
            // << 非重定向
            if byte == b'<' && i + 1 < slice.len() && slice[i + 1] == b'<' {
                return Some(i);
            }
            return Some(i);
        }
    }
    None
}

fn parse_single_segment<'a>(raw: &'a [u8]) -> Segment<'a> {
    let mut seg = Segment {
        cmd_buf: [0u8; 256],
        cmd_len: 0,
        argv: [core::ptr::null(); 16],
        argc: 0,
        redir_in: None,
        redir_out: None,
        redir_kind: RedirKind::None,
    };

    let _len = raw.len().min(255);

    // 复制到 cmd_buf
    let copy_len = _len.min(255);
    seg.cmd_buf[..copy_len].copy_from_slice(&raw[..copy_len]);
    seg.cmd_buf[copy_len] = 0;
    seg.cmd_len = copy_len;

    // 扫描 < 和 >
    let mut j = 0;
    while j < copy_len {
        let b = seg.cmd_buf[j];

        // 跳过引号内的内容
        if b == b'"' {
            j += 1;
            while j < copy_len && seg.cmd_buf[j] != b'"' {
                j += 1;
            }
            if j < copy_len {
                j += 1;
            }
            continue;
        }

        // >>  追加重定向
        if b == b'>' && j + 1 < copy_len && seg.cmd_buf[j + 1] == b'>' {
            // 提取后面的文件名
            let mut k = j + 2;
            while k < copy_len && (seg.cmd_buf[k] == b' ' || seg.cmd_buf[k] == b'\t') {
                k += 1;
            }
            let name_start = k;
            while k < copy_len && seg.cmd_buf[k] != b' ' && seg.cmd_buf[k] != b'\t' {
                k += 1;
            }
            seg.redir_kind = RedirKind::StdoutAppend;
            seg.redir_out = Some(&raw[name_start..k]);
            // 移除重定向部分从命令中
            seg.cmd_buf[j] = 0;
            seg.cmd_len = j;
            break;
        }

        // >   覆盖重定向
        if b == b'>' {
            let mut k = j + 1;
            while k < copy_len && (seg.cmd_buf[k] == b' ' || seg.cmd_buf[k] == b'\t') {
                k += 1;
            }
            let name_start = k;
            while k < copy_len && seg.cmd_buf[k] != b' ' && seg.cmd_buf[k] != b'\t' {
                k += 1;
            }
            seg.redir_kind = RedirKind::StdoutNew;
            seg.redir_out = Some(&raw[name_start..k]);
            seg.cmd_buf[j] = 0;
            seg.cmd_len = j;
            break;
        }

        // <   输入重定向
        if b == b'<' {
            let mut k = j + 1;
            while k < copy_len && (seg.cmd_buf[k] == b' ' || seg.cmd_buf[k] == b'\t') {
                k += 1;
            }
            let name_start = k;
            while k < copy_len && seg.cmd_buf[k] != b' ' && seg.cmd_buf[k] != b'\t' {
                k += 1;
            }
            seg.redir_in = Some(&raw[name_start..k]);
            seg.redir_kind = RedirKind::StdinFile;
            seg.cmd_buf[j] = 0;
            seg.cmd_len = j;
            break;
        }

        j += 1;
    }

    // 解析 args
    let mut pos = 0;
    while pos < seg.cmd_len {
        while pos < seg.cmd_len && (seg.cmd_buf[pos] == b' ' || seg.cmd_buf[pos] == b'\t') {
            pos += 1;
        }
        if pos >= seg.cmd_len {
            break;
        }

        seg.argv[seg.argc] = &seg.cmd_buf[pos] as *const u8;
        seg.argc += 1;
        if seg.argc >= 16 {
            break;
        }

        while pos < seg.cmd_len && seg.cmd_buf[pos] != b' ' && seg.cmd_buf[pos] != b'\t' {
            pos += 1;
        }
        if pos < seg.cmd_len {
            seg.cmd_buf[pos] = 0;
            pos += 1;
        }
    }
    seg.argv[seg.argc] = core::ptr::null();
    seg
}

use core::mem::MaybeUninit;

pub fn execute_pipeline(input: &[u8]) {
    let len = input.len();
    if len == 0 {
        return;
    }
    let len = if len > 0 && input[len - 1] == b'\n' {
        len - 1
    } else {
        len
    };
    let len = if len > 0 && input[len - 1] == b'\r' {
        len - 1
    } else {
        len
    };

    // 按 | 切分段
    let mut segments: [MaybeUninit<Segment>; MAX_PIPELINE] =
        unsafe { MaybeUninit::uninit().assume_init() };
    let mut seg_count = 0;

    let mut start = 0;
    while start < len && seg_count < MAX_PIPELINE {
        // 跳过前导空白
        while start < len && (input[start] == b' ' || input[start] == b'\t') {
            start += 1;
        }
        if start >= len {
            break;
        }

        let end = find_byte(&input[..len], b'|', start).unwrap_or(len);
        segments[seg_count].write(parse_single_segment(&input[start..end]));
        seg_count += 1;
        start = end + 1;
    }

    if seg_count == 0 {
        return;
    }

    let segments: &[Segment] =
        unsafe { core::slice::from_raw_parts(segments.as_ptr() as *const Segment, seg_count) };

    // 执行管道
    let mut prev_pipe: [i32; 2] = [-1, -1];
    let mut pids: [i32; MAX_PIPELINE] = [-1; MAX_PIPELINE];

    for i in 0..seg_count {
        let mut cur_pipe: [i32; 2] = [-1, -1];
        let is_last = i == seg_count - 1;

        if !is_last && pipe_create(&mut cur_pipe) < 0 {
            println("eash: pipe() failed");
            return;
        }

        let pid = fork() as i32;
        if pid < 0 {
            println("eash: fork() failed");
            // 清理所有管道
            for fd in &prev_pipe {
                if *fd >= 0 {
                    fs_close(*fd);
                }
            }
            for fd in &cur_pipe {
                if *fd >= 0 {
                    fs_close(*fd);
                }
            }
            return;
        }

        if pid == 0 {
            // ──────── 子进程 ────────

            // 输入: 来自上一个管道
            if prev_pipe[0] >= 0 {
                dup2_fd(prev_pipe[0], STDIN);
                fs_close(prev_pipe[0]);
                fs_close(prev_pipe[1]);
            }

            // 输出: 到下一个管道
            if cur_pipe[1] >= 0 {
                dup2_fd(cur_pipe[1], STDOUT);
                fs_close(cur_pipe[0]);
                fs_close(cur_pipe[1]);
            }

            // 输入重定向 <
            if let Some(file) = segments[i].redir_in {
                let fd = fs_open(file, 0, 0);
                if fd >= 0 {
                    dup2_fd(fd, STDIN);
                    fs_close(fd);
                } else {
                    print("eash: cannot open ");
                    println(from_utf8(file).unwrap_or("?"));
                    proc_exit(1);
                }
            }

            // 输出重定向 >
            if let Some(file) = segments[i].redir_out {
                let flags = match segments[i].redir_kind {
                    RedirKind::StdoutAppend => 0x401, // O_WRONLY | O_CREAT | O_APPEND
                    _ => 0x201,                       // O_WRONLY | O_CREAT | O_TRUNC
                };
                let fd = fs_open(file, flags, 0o644);
                if fd >= 0 {
                    dup2_fd(fd, STDOUT);
                    fs_close(fd);
                } else {
                    print("eash: cannot create ");
                    println(from_utf8(file).unwrap_or("?"));
                    proc_exit(1);
                }
            }

            let argv = &segments[i].argv[..(segments[i].argc + 1)];
            let path = segments[i].argv[0];
            if path.is_null() {
                proc_exit(1);
            }

            // null 终止路径方便 exec
            let path_slice = unsafe { CStr::from_ptr(path as *const core::ffi::c_char) }.to_bytes();
            proc_exec(path_slice, argv);
            // exec 失败
            print("eash: ");
            print(from_utf8(path_slice).unwrap_or("?"));
            println(": exec failed");
            proc_exit(1);
        } else {
            // ──────── 父进程 ────────
            pids[i] = pid;

            // 关闭不用的管道 fd
            if prev_pipe[0] >= 0 {
                fs_close(prev_pipe[0]);
                fs_close(prev_pipe[1]);
            }

            // 当前管道的读端传给下一段
            prev_pipe = cur_pipe;
        }
    }

    // 关闭最后的管道 fd
    if prev_pipe[0] >= 0 {
        fs_close(prev_pipe[0]);
        fs_close(prev_pipe[1]);
    }

    // 等待所有子进程
    for &pid in &pids[..seg_count] {
        if pid >= 0 {
            wait_pid(pid);
        }
    }
}
