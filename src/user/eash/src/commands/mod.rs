//! 命令注册与路由 (Command Registry & Dispatch)

pub mod general;
pub mod fileops;
pub mod system;
pub mod identity;
pub mod pipeline;

pub use pipeline::{is_pipeline, execute_pipeline};

use core::str::from_utf8;

// ── Cmd 解析结构 ──

#[derive(Clone)]
pub struct Cmd {
    pub n: usize,
    args: [u8; 1024],  // 原始参数字节
    offsets: [usize; 32], // 每个参数的起始偏移
}

impl Cmd {
    pub fn new(input: &[u8]) -> Self {
        let mut offsets = [0usize; 32];
        let mut args = [0u8; 1024];
        let mut n = 0usize;
        let len = input.len().min(1023);

        // 复制输入 (去除尾随换行)
        let end = if len > 0 && input[len - 1] == b'\n' { len - 1 } else { len };
        let end = if end > 0 && input[end - 1] == b'\r' { end - 1 } else { end };
        let end = end.min(1023);
        args[..end].copy_from_slice(&input[..end]);
        args[end] = 0;

        // 分词
        let mut i = 0;
        while i < end {
            // 跳过空白
            while i < end && (args[i] == b' ' || args[i] == b'\t') { i += 1; }
            if i >= end { break; }

            // 引号处理
            if args[i] == b'"' {
                i += 1;
                offsets[n] = i;
                while i < end && args[i] != b'"' { i += 1; }
                if i < end { args[i] = 0; i += 1; }
            } else {
                offsets[n] = i;
                while i < end && args[i] != b' ' && args[i] != b'\t' { i += 1; }
                if i < end { args[i] = 0; i += 1; }
            }
            n += 1;
            if n >= 32 { break; }
        }
        Self { n, args, offsets }
    }

    pub fn get(&self, idx: usize) -> &[u8] {
        if idx >= self.n { return b""; }
        let start = self.offsets[idx];
        // I-10: 修复双重计数 bug — 旧代码 `start + len` 又被用于 `start..start+end`
        // 切到 `start..start+len` 避免相对偏移再次相加, 否则 get(1) 跨过自身 NUL
        // 把后续参数也吞进 slice. eash dispatch 之前因此可能读到错位的参数.
        let len = core::ffi::CStr::from_bytes_until_nul(&self.args[start..])
            .map(|c| c.to_bytes().len()).unwrap_or(0);
        &self.args[start..start + len]
    }
}

pub fn as_str(slice: &[u8]) -> &str { from_utf8(slice).unwrap_or("") }

/// 从 cmd 获取 path 参数 (支持引号路径)
pub fn path_arg(cmd: &Cmd) -> Option<[u8; 256]> {
    if cmd.n < 2 { return None; }
    let mut buf = [0u8; 256];
    let raw = cmd.get(1);
    let len = raw.len().min(255);
    buf[..len].copy_from_slice(&raw[..len]);
    buf[len] = 0;
    Some(buf)
}

// ── 命令注册表 & 调度 ──

type CmdFn = fn(&Cmd);

pub(crate) struct Entry {
    pub name: &'static str,
    pub category: &'static str,
    pub help_line: &'static str,
    pub func: CmdFn,
}

pub(crate) static TABLE: &[Entry] = &[
    // Shell 内置
    Entry { name: "help",   category: "shell", help_line: "  help     显示帮助            help [file|sys|id|shell]", func: general::help  },
    Entry { name: "clear",  category: "shell", help_line: "  clear    清屏                clear",                   func: general::clear },
    Entry { name: "echo",   category: "shell", help_line: "  echo     回显文本            echo [text...]",          func: general::echo  },
    Entry { name: "exit",   category: "shell", help_line: "  exit     退出 Shell          exit",                    func: general::exit  },
    // 文件操作
    Entry { name: "dir",    category: "file",  help_line: "  dir      列出目录内容        dir [/path]",             func: fileops::dir   },
    Entry { name: "cd",     category: "file",  help_line: "  cd       切换工作目录        cd <dir>",                func: fileops::cd    },
    Entry { name: "pwd",    category: "file",  help_line: "  pwd      显示当前路径        pwd",                     func: fileops::pwd   },
    Entry { name: "cat",    category: "file",  help_line: "  cat      显示文件内容        cat <file>",              func: fileops::cat   },
    Entry { name: "mkdir",  category: "file",  help_line: "  mkdir    创建目录            mkdir <dir>",             func: fileops::mkdir },
    Entry { name: "touch",  category: "file",  help_line: "  touch    创建空文件          touch <file>",            func: fileops::touch },
    Entry { name: "del",    category: "file",  help_line: "  del      删除文件/目录       del <path>",              func: fileops::del   },
    Entry { name: "cp",     category: "file",  help_line: "  cp       复制文件            cp <src> <dst>",          func: fileops::cp    },
    Entry { name: "mv",     category: "file",  help_line: "  mv       移动/重命名         mv <src> <dst>",          func: fileops::mv    },
    Entry { name: "save",   category: "file",  help_line: "  save     写入文本到文件      save <file> <text>",      func: fileops::save  },
    // 身份
    Entry { name: "login",  category: "id",    help_line: "  login    登录                login <note> <pw>",       func: identity::login  },
    Entry { name: "logout", category: "id",    help_line: "  logout   登出                logout",                  func: identity::logout },
    Entry { name: "who",    category: "id",    help_line: "  who      当前身份            who",                     func: identity::who    },
    Entry { name: "passwd", category: "id",    help_line: "  passwd   修改口令            passwd",                  func: identity::passwd },
    // 系统
    Entry { name: "osinfo", category: "sys",   help_line: "  osinfo   系统版本/架构       osinfo",                  func: system::osinfo },
    Entry { name: "host",   category: "sys",   help_line: "  host     显示/设置主机名     host [name]",             func: system::host   },
    Entry { name: "ps",     category: "sys",   help_line: "  ps       进程列表            ps",                      func: system::ps     },
    Entry { name: "reboot", category: "sys",   help_line: "  reboot   重启系统            reboot",                  func: system::reboot },
    Entry { name: "halt",   category: "sys",   help_line: "  halt     关机                halt",                    func: system::halt   },
];

pub fn dispatch(cmd: &Cmd) {
    let name = as_str(cmd.get(0));
    if name.is_empty() { return; }

    // 搜索精确匹配
    for entry in TABLE {
        if entry.name == name {
            (entry.func)(cmd);
            return;
        }
    }

    // 不是内置命令 — 尝试 fork+exec 启动外部程序
    run_external(cmd);
}

fn run_external(cmd: &Cmd) {
    use userlib::*;

    // 构建路径: 尝试 /usr/bin/<name> 和 ./<name>
    let name = cmd.get(0);
    let mut path_buf = [0u8; 256];
    let mut argv_ptrs: [*const u8; 16] = [core::ptr::null(); 16];

    // 如果名称包含 / 则直接使用, 否则前缀 /usr/bin/
    let path = if name.contains(&b'/') {
        let len = name.len().min(255);
        path_buf[..len].copy_from_slice(name);
        path_buf[len] = 0;
        &path_buf[..len + 1]
    } else {
        let prefix = b"/usr/bin/\0";
        let plen = 9;
        let nlen = name.len().min(245);
        path_buf[..plen].copy_from_slice(&prefix[..plen]);
        path_buf[plen - 1] = b'/';
        path_buf[plen..plen + nlen].copy_from_slice(&name[..nlen]);
        path_buf[plen + nlen] = 0;
        &path_buf[..plen + nlen + 1]
    };

    // 构建 argv
    let mut arg_buf = [0u8; 1024];
    let mut arg_offsets: [usize; 16] = [0; 16];
    let mut argc = 0;
    let mut pos = 0;

    for i in 0..cmd.n {
        let arg = cmd.get(i);
        let alen = arg.len().min(1023 - pos);
        arg_buf[pos..pos + alen].copy_from_slice(&arg[..alen]);
        arg_buf[pos + alen] = 0;
        arg_offsets[argc] = pos;
        argc += 1;
        pos += alen + 1;
        if argc >= 16 || pos >= 1024 { break; }
    }

    for i in 0..argc {
        argv_ptrs[i] = &arg_buf[arg_offsets[i]] as *const u8;
    }
    argv_ptrs[argc] = core::ptr::null();

    let pid = userlib::sys::fork() as i32;
    if pid < 0 {
        println("eash: fork failed");
        return;
    }
    if pid == 0 {
        // 子进程
        userlib::sys::proc_exec(&path[..path.len() - 1], &argv_ptrs[..argc + 1]);
        // exec 失败
        userlib::print("eash: ");
        userlib::print(name_str(name));
        userlib::println(": not found");
        userlib::sys::proc_exit(1);
    } else {
        userlib::sys::wait_pid(pid);
    }
}

fn name_str(name: &[u8]) -> &str {
    core::str::from_utf8(name).unwrap_or("?")
}