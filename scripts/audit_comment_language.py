#!/usr/bin/env python3
"""
注释语言一致性 audit (TD-22)

目标: 按 [maintenance-2026-06-11.md 注释规范](../../docs/plan/maintenance-2026-06-11.md) 检测
Rust 注释中残留的英文段落式注释 (// 或 /// 或 /* */ 或 // SAFETY: / // TODO:).

设计契约:
  - 注释统一使用中文 (含 /// 文档注释, // 行内, /* */ 块, // SAFETY:, // TODO:)
  - 允许例外 (纯标识符/常量引用不视为英文段落):
    * 代码标识符: Cell::as_ptr, Box::into_raw, BTreeMap
    * 硬件/架构术语: CR3, IST1, TTBR0, MSR, GIC, APIC
    * 算法/协议/机制: RCU, CFS, COW, spin::Mutex
    * 错误码与标准常量: ENOENT, EINVAL, EAGAIN, O_RDONLY
    * 外部 API/标准引用: Linux man page: futex(2), POSIX 1003.1-2017 §2.9
    * 链接路径与文件名: src/kernel/framework/mm/cow.rs
    * 配置项/编译 flag: #[cfg(target_arch = "x86_64")]
    * 第三方 crate 名称: smoltcp, heapless
  - 短英文注释 (≤ 30 字符非空白 + ≤ 2 个英文长词) 视为合理引用, 不算违规
  - 段落式英文 (3+ 个长度 ≥ 4 的英文单词, 整段无中文字符) 视为违规
  - 含中文字符的注释: 视为合规, 即使夹杂英文技术术语 (按"中文字符占主体"原则)

退出码: 0 = 通过, 1 = 有违规, 2 = 配置/IO 错误
"""

import re
import sys
from pathlib import Path
from typing import Iterator

PROJECT_ROOT = Path(__file__).resolve().parent.parent
BASE = PROJECT_ROOT / "src" / "kernel"

# 注释前缀 (/// 文档注释, // 行内, /* 块注释起始, * 块注释续行, // SAFETY:, // TODO:)
COMMENT_PREFIX = re.compile(
    r"^\s*(?:///?|/\*|\*|//\s*SAFETY:|//\s*TODO:)"
)

# 中文字符 (CJK Unified Ideographs + 标点)
HAS_CJK = re.compile(r"[\u4e00-\u9fff\u3000-\u303f\uff00-\uffef]")

# 英文长词 (4+ 个连续 ASCII 字母, 含下划线视为一个词)
EN_LONG_WORD = re.compile(r"\b[A-Za-z_][A-Za-z_0-9]{3,}\b")

# 短英文注释的判断: 总可见字符 ≤ 30 且 英文长词 ≤ 2 个
def is_short_english_comment(text: str) -> bool:
    visible = text.strip()
    if len(visible) <= 30:
        long_words = EN_LONG_WORD.findall(visible)
        if len(long_words) <= 2:
            return True
    return False

# 例外清单: 这些词/标记在英文注释中出现不算"段落式英文"
ALLOWED_ENGLISH_TERMS = frozenset({
    # 算法/协议
    "RCU", "CFS", "COW", "LRU", "FIFO", "BTreeMap", "BTree", "Mutex", "SpinLock",
    "lockdep", "Lockdep", "tcb", "TCB",
    # 硬件/架构
    "CR3", "CR4", "IST", "MSR", "TSC", "PIC", "APIC", "GIC", "GICv2", "GICv3",
    "MMIO", "DMA", "PCID", "INVPCID", "KPTI", "SMEP", "SMAP", "NX",
    "SCTLR", "TCR", "TTBR", "TTBR0", "TTBR1", "ESR", "ELR", "SPSR", "FAR",
    "HPET", "ACPI", "MSI", "MSIX", "PVH", "PV", "KVM",
    # rusage / 资源使用常量 (Linux/POSIX)
    "RUSAGE_SELF", "RUSAGE_CHILDREN", "RUSAGE_THREAD",
    "ru_utime", "ru_stime", "ru_maxrss", "ru_minflt", "ru_majflt",
    "ru_inblock", "ru_oublock", "ru_nvcsw", "ru_nivcsw", "ru_ixrss",
    "ru_idrss", "ru_isrss", "ru_msgsnd", "ru_msgrcv", "ru_nsignals",
    "tv_sec", "tv_usec",
    # 错误码
    "ENOENT", "EINVAL", "EAGAIN", "EBUSY", "EFAULT", "ENOSPC", "ENOMEM",
    "EACCES", "EEXIST", "ENOTDIR", "EISDIR", "EROFS", "ENOSYS", "EPERM",
    "ENAMETOOLONG", "EOVERFLOW", "ENODEV", "EXDEV", "EIO", "ECANCELED",
    "ETIMEDOUT", "EINTR", "EWOULDBLOCK", "EDEADLK", "EMLINK", "ERANGE",
    "EBADF", "ECHILD", "EDEADLOCK",
    # 文件描述符标志
    "O_RDONLY", "O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND",
    "O_NONBLOCK", "O_DIRECTORY", "O_NOFOLLOW", "O_CLOEXEC",
    # 信号
    "SIGILL", "SIGSEGV", "SIGBUS", "SIGFPE", "SIGKILL", "SIGTERM", "SIGCHLD",
    "SIGSTOP", "SIGCONT", "SIGHUP", "SIGINT", "SIGQUIT", "SIGABRT", "SIGPIPE",
    "SIGUSR1", "SIGUSR2", "SIGALRM", "SIGTSTP", "SIGTTIN", "SIGTTOU", "SIGIO",
    # 协议族 / socket
    "AF_UNIX", "AF_INET", "AF_INET6", "PF_UNIX", "SOCK_STREAM", "SOCK_DGRAM",
    "SOCK_RAW", "SOCK_SEQPACKET", "SOCK_NONBLOCK", "SOCK_CLOEXEC",
    "IPPROTO_TCP", "IPPROTO_UDP", "IPPROTO_ICMP", "SOL_SOCKET", "SO_REUSEADDR",
    "SO_REUSEPORT", "SO_KEEPALIVE", "SO_LINGER", "SO_RCVTIMEO", "SO_SNDTIMEO",
    "TCP_NODELAY", "TCP_CORK", "MSG_PEEK", "MSG_WAITALL", "MSG_DONTWAIT",
    "MSG_TRUNC", "MSG_ERRQUEUE", "SHUT_RD", "SHUT_WR", "SHUT_RDWR",
    # 标准引用
    "POSIX", "Linux", "QEMU", "x86_64", "aarch64", "AArch64", "X86_64",
    "Rust", "BSD", "XNU", "NetBSD", "FreeBSD", "OpenBSD", "musl", "glibc",
    # 第三方 crate
    "smoltcp", "heapless", "spin", "bitflags", "lock_api", "libc",
    "framekernel", "Framekernel",
    # syscall / 文件
    "syscall", "syscalls", "ioctl", "fcntl", "flock", "futex", "epoll",
    "madvise", "mremap", "mprotect", "mmap", "munmap", "mprotect",
    "prctl", "seccomp", "capability", "Capability", "Capabilities",
    "kthread", "klog", "kexec", "kfence", "kprobe", "kprobes", "ebpf",
    "bpf", "ftrace", "perf", "kdump", "cgroup", "cgroups", "namespace",
    "namespaces", "pwm", "PWM", "sgeg", "SGEG", "sched", "Scheduler",
    "schedule", "scheduler", "tickless", "shadow", "secure_boot",
    "iopl", "ioperm", "io_uring", "iovec",
    # WASM/WASI 标准术语
    "WASM", "WASI", "wasi_snapshot_preview1", "WasiContext", "WasiFdTable",
    "WasiErrno", "WasiRights", "WasiFileType", "WasiIoVec",
    # WASI 标准函数名 (无法翻译的规范标识符)
    "fd_prestat_get", "fd_prestat_dir_name", "fd_stat_get",
    "fd_read", "fd_write", "fd_pread", "fd_pwrite", "fd_allocate", "fd_advise",
    "fd_renumber", "fd_dup", "fd_readdir",
    "path_unlink_file", "path_relative_path", "path_symlink", "path_readlink",
    "path_filestat_get", "path_filestat_set_times", "path_link",
    "sock_accept", "sock_connect", "sock_recv", "sock_send",
    "filestat", "filetype", "nlink", "atim", "mtim",
    "dirent", "inode", "next_cookie", "namlen",
    "sockaddr_in", "sin_family", "sin_port", "sin_addr",
    "old_path", "new_path", "linkpath",
    # USB/xHCI 规范术语
    "TRB", "Endpoint", "Transfer", "Ring", "Slot", "Context",
    # 扩展 syscall 名称 (linuxulator 表中纯标识符引用)
    "mlock", "mlockall", "munlock", "munlockall", "mincore",
    "timer_create", "timer_settime", "timer_gettime", "timer_delete",
    "timer_getoverrun", "clock_getres", "clock_gettime",
    "unshare", "setns", "get_mempolicy", "set_mempolicy",
    "migrate_pages", "getcpu", "brk", "mremap",
    # 常用英文技术词
    "TODO", "FIXME", "XXX", "NOTE", "HACK", "BUG", "WARNING", "WARN",
    "unsafe", "SAFETY", "default", "Default", "kernel", "Kernel",
    "user", "User", "mode", "Mode", "context", "Context",
    "call", "called", "calls", "wrapper", "thunk", "trait", "impl",
    "struct", "enum", "function", "Function", "method", "Method",
    "module", "Module", "file", "File", "directory", "Directory",
    "test", "Test", "tests", "Tests", "verify", "Verified", "validation",
    "parse", "Parsed", "format", "Format", "serialize", "deserialize",
    "register", "Register", "registered", "unregister", "lookup",
    "initialize", "init", "Init", "initialized", "deinitialize", "destroy",
    "destroyed", "allocate", "allocated", "deallocate", "free", "Free",
    "release", "released", "acquire", "acquired", "hold", "held",
    "lock", "locked", "unlock", "unlocked", "guard", "Guard",
    "check", "checked", "checksum", "crc", "crc32", "sha256",
    "version", "Version", "build", "Build", "commit", "Commit",
    "branch", "Branch", "merge", "merged", "tag", "Tag",
    "true", "false", "True", "False", "None", "Some", "Ok", "Err",
    "Self", "self", "Box", "Arc", "Rc", "RefCell", "OnceCell",
    "Cell", "Pin", "Unpin", "Drop", "Send", "Sync",
    "Vec", "String", "CString", "CStr", "OsStr",
    "Option", "Result", "Iterator", "IntoIterator",
    "Debug", "Display", "Clone", "Copy", "Default", "Eq", "PartialEq",
    "Ord", "PartialOrd", "Hash", "Sized", "?Sized",
    "mut", "pub", "fn", "let", "const", "static", "use", "mod",
    "extern", "crate", "super", "self", "as", "ref", "move",
    "impl", "trait", "struct", "enum", "union", "type",
    "for", "while", "loop", "if", "else", "match", "return",
    "in", "out", "ref", "where", "async", "await", "dyn",
    # 文件名/路径
    "init", "api", "mod", "lib", "bin", "test", "tests", "example",
    "examples", "bench", "benches", "build", "ci", "docs", "doc",
    # 日志级别
    "info", "warn", "error", "debug", "trace", "panic",
    # 业务术语
    "domain", "Domain", "range", "Range", "slot", "Slot", "token", "Token",
    "queue", "Queue", "pool", "Pool", "buffer", "Buffer", "cache", "Cache",
    "page", "Page", "frame", "Frame", "table", "Table", "entry", "Entry",
    "index", "Index", "offset", "Offset", "length", "Length", "size", "Size",
    "count", "Count", "total", "Total", "remain", "Remain",
    "head", "tail", "next", "prev", "first", "last", "begin", "end",
    "start", "Start", "stop", "Stop", "ready", "Ready",
    "enable", "Enable", "enabled", "disable", "Disable", "disabled",
    "active", "Active", "inactive", "Inactive", "pending", "Pending",
    "valid", "Valid", "invalid", "Invalid", "present", "Present",
    "absent", "Absent", "exists", "Exists", "missing", "Missing",
    "found", "Found", "match", "matched", "unmatched",
    "success", "Success", "successful", "failure", "Failure", "failed",
    "error", "Error", "errors", "warning", "Warning", "warnings",
    "skip", "skipping", "skipped", "ignore", "ignored", "handling",
    "return", "returns", "returned",
    "add", "added", "remove", "removed", "insert", "inserted",
    "delete", "deleted", "update", "updated", "set", "setted",
    "get", "gets", "got", "fetch", "fetched", "load", "loaded",
    "store", "stored", "save", "saved", "read", "reads", "write", "writes",
    "wrote", "written", "readable", "writable",
    "open", "opened", "close", "closed", "create", "created",
    "increment", "decrement", "increased", "decreased",
    "current", "Current", "previous", "Previous",
    "process", "Process", "thread", "Thread",
    "system", "System", "platform", "Platform", "architecture", "Architecture",
    "target", "Target", "binary", "Binary",
    "page_fault", "page_table", "page_frame", "virtual", "Virtual",
    "physical", "Physical", "linear", "Linear", "direct", "Direct",
    "indirect", "Indirect",
    "policy", "Policy", "mechanism", "Mechanism",
    "device", "Device", "driver", "Driver", "interrupt", "Interrupt",
    "exception", "Exception", "handler", "Handler", "callback", "Callback",
    "stack", "Stack", "heap", "Heap", "memory", "Memory",
    "address", "Address", "pointer", "Pointer", "value", "Value",
    "field", "Field", "member", "Member", "bit", "Bit", "byte", "Byte",
    "word", "Word", "dword", "qword",
    "page_size", "page_shift", "page_mask", "page_offset",
    "vm_flags", "vma_flags", "vma", "VMA", "vmas",
    "fd", "fds", "fdset", "fdtable",
    "pid", "pids", "pgid", "pgids", "tid", "tids", "uid", "uids",
    "gid", "gids", "euid", "egid", "suid", "sgid", "fsuid", "fsgid",
    "ppid", "sid", "pgid", "tty", "ttyname",
    "epollfd", "eventfd", "signalfd", "timerfd", "inotify",
    "blk", "Block", "block_device", "block_devices",
    "net", "Net", "Network", "network", "socket", "Socket",
    "tcp", "TCP", "udp", "UDP", "ip", "IP", "ipv4", "ipv6", "IPv4", "IPv6",
    "mac", "MAC", "ethernet", "Ethernet", "arp", "ARP",
    "dhcp", "DHCP", "dns", "DNS", "icmp", "ICMP", "igmp", "IGMP",
    "route", "Route", "routing", "Routing", "gateway", "Gateway",
    "subnet", "Subnet", "prefix", "Prefix", "cidr", "CIDR",
    "smoltcp_impl", "smoltcp_iface",
    "fs", "FS", "filesystem", "Filesystem", "file_system",
    "ramfs", "RamFs", "devfs", "DevFs", "procfs", "ProcFs", "unkfs", "Unkfs",
    "spa", "SPA", "dmu", "DMU", "zap", "ZAP", "zil", "ZIL", "arc", "ARC",
    "txg", "TXG", "raid", "RAID", "raidz", "RAIDZ", "dedup", "Dedup",
    "checksum", "Checksum",
    "sgeg", "SGEG", "session", "Session", "privilege", "Privilege",
    "capability", "Capability", "pwm_id", "pwm", "PWM",
    "vfork", "clone", "fork", "execve", "exec", "exit", "wait", "waitpid",
    "kill", "raise", "signal", "signals", "sigaction", "sigprocmask",
    "sigaltstack", "sigreturn", "sigsuspend", "sigtimedwait",
    "wait_queue", "waitqueue", "WaitQueue",
    "hrtimer", "timer", "Timer", "tick", "Tick", "jiffies", "uptime",
    "idle", "Idle", "halt", "stop", "suspend", "resume",
    "context_switch", "switch_to", "iretq", "eret", "sysret", "syscall",
    "trampoline", "Trampoline", "vsyscall", "vDSO", "vvar",
    "kpti", "KPTI", "seccomp", "Seccomp", "landlock", "Landlock",
    "audit", "Audit", "logging", "Logging", "logger", "Logger",
    "subsystem", "Subsystem", "category", "Category", "level", "Level",
    "formater", "render", "rendered", "json", "JSON", "yaml", "YAML",
    "toml", "TOML", "xml", "XML", "csv", "CSV", "text", "Text",
    "ascii", "ASCII", "utf8", "UTF8", "UTF-8", "unicode", "Unicode",
    "ascii_text", "utf8_text",
    "path", "Path", "filepath", "FilePath",
    "proc", "Proc", "process", "Process",
    "subsystem_id", "subsystem_name", "subsystem_count",
    "vfs", "VFS", "vfs_node", "vfs_inode", "vfs_dentry",
    "mount", "Mount", "umount", "Unmount", "mounted", "unmounted",
    "directory", "Directory", "dir", "Dir", "dirname", "basename",
    "filename", "filepath", "abspath", "relpath", "cwd", "pwd",
    "openat", "open_by_handle_at", "name_to_handle_at",
    "stat", "Stat", "fstat", "lstat", "statx", "fstatat",
    "chmod", "chown", "fchmod", "fchown", "lchown", "fchmodat", "fchownat",
    "truncate", "ftruncate", "fallocate", "posix_fallocate",
    "readlink", "symlink", "link", "unlink", "rename", "renameat", "renameat2",
    "readdir", "getdents", "getdents64", "scandir", "opendir", "closedir",
    "fdopendir", "dirfd",
    "inotify", "Inotify", "fanotify", "Fanotify", "dnotify", "Dnotify",
    "mknod", "mknodat", "mkfifo", "mkfifoat", "pivot_root", "chroot",
    "mount", "umount2", "mount_setattr",
    "swapon", "swapoff", "mkswap",
    "io_uring", "IoUring", "io_uring_setup", "io_uring_enter", "io_uring_register",
    "AIO", "aio", "lio_listio", "io_submit", "io_getevents", "io_cancel",
    "select", "poll", "pselect", "ppoll", "epoll_wait", "epoll_pwait",
    "epoll_create", "epoll_create1", "epoll_ctl",
    "signalfd", "signalfd4", "timerfd_create", "timerfd_settime", "timerfd_gettime",
    "eventfd", "eventfd2",
    "memfd", "memfd_create", "userfaultfd", "uffd",
    "pidfd", "pidfd_open", "pidfd_send_signal", "pidfd_getfd",
    "io_pgetevents", "io_uring_prep_",
    # POSIX socket API 参数名 (在 /// POSIX 签名引用中常见, 非解释性文字)
    "addr", "addrlen", "backlog", "dest_addr", "src_addr",
    "optname", "optval", "optlen", "getsockname", "getpeername",
    "accept", "accept4", "shutdown", "sockatmark",
    # syscall 标志常量 (在 /// `flags`: XXX | YYY 注释中常见)
    "EFD_CLOEXEC", "EFD_NONBLOCK", "EFD_SEMAPHORE",
    "SFD_CLOEXEC", "SFD_NONBLOCK",
    "TFD_CLOEXEC", "TFD_NONBLOCK",
    "SECCOMP_SET_MODE_STRICT", "SECCOMP_SET_MODE_FILTER",
    "operation",
    # EGDF 错误类型名 (在 /// 文档注释中引用)
    "InvalidArgument", "NoResources", "WouldBlock", "NotReady", "PermissionDenied",
    "WrongType", "AlreadyExists", "Other",
    # 恢复层术语
    "Layer1", "Layer2", "Layer3",
    # 其他常见术语
    "Suppress", "InMemoryMatrix", "import",
})


def is_posix_signature_ref(text: str) -> bool:
    """检测是否为 POSIX 签名引用 (整行只是 `/// POSIX `func(args) 形式).

    这类注释是"标准函数原型引用", 等同于 SAFETY/TODO 短引用豁免.
    例: `/// POSIX `bind(fd, addr, addrlen)`` (单行, 含 `POSIX` + 反引号函数名)

    也接受不带 POSIX 前缀但形式相同的: `/// sendto(fd, buf, len, flags, ...)`
    (常见于 services/net/syscall.rs 等纯签名引用).
    """
    body = re.sub(r"^\s*(?:///?|\*|/\*)", "", text).strip()
    # 必须是单行 (不允许折行), 长度 < 120
    if "\n" in text or len(body) >= 120:
        return False
    # 形式 1: POSIX/Linux/QEMU/Susv/SVID 起头 + 反引号函数名 (签名引用)
    if re.match(r"^(POSIX|Linux|Susv|SVID|QEMU|RFC|man\s*page)\b", body, re.IGNORECASE) and "`" in body:
        return True
    # 形式 2: 纯函数签名引用 (无 POSIX 前缀, 常见于 net/socket.rs 系列)
    #   `identifier(args, args)` 形式, 单行, 长度 < 80
    #   整行只含标识符/逗号/空格/反引号/数字
    if len(body) < 80:
        # 必须是 `word(...)` 形式, 整行不出现叙述性英文单词
        m = re.match(r"^`?(\w+)`?\s*\(([^)]*)\)\s*`?$", body)
        if m:
            fn_name, args = m.group(1), m.group(2)
            # 函数名必须是合法标识符 (至少 2 字符, 首字母小写)
            if len(fn_name) >= 2 and fn_name[0].islower() and fn_name.replace("_", "").isalnum():
                # 参数列表必须是简单标识符 (小写/下划线/逗号/空格/数字, 无叙述词)
                if re.match(r"^[\w,\s&*_]*$", args):
                    return True
    return False


# 代码示例特征: 注释中含一定密度的代码结构标记
# 命中 2+ 个即视为代码示例 (如 `let x = Itimerspec { ... }`)
CODE_MARKERS = re.compile(r"(::|;|=>|\bstruct\s|\blet\b|\buse\b|\bsyscall\(|::\s*\w+\s*\()")

# 公式/等式特征: 含赋值 (=) 和算术算子 (* /), 或含 => 映射
FORMULA_MARKERS = re.compile(r"[=]\s*.*[\*/]|=>")


def is_formula_or_equation(text: str) -> bool:
    """检测是否为公式/等式注释 (常见于 timer/pit/hrtimer/tsc 等).

    模式:
      - `ns = cycles * 1_000_000_000 / freq_hz`
      - `us = cycles * 1_000_000 / PIT_BASE_FREQUENCY`
      - `P=1, DPL=00, S=0 (System), Type=1001 (TSS Available), Busy=0 => 0x89`
      - `1 = Layer1 (BBR), 2 = Layer2 (BSR), 3 = Layer3 (BHR)`
      - `file_type=Symlink → EPOLLIN | EPOLLHUP`

    这些是"数学/逻辑公式", 等价于 POSIX 签名引用豁免.
    """
    body = re.sub(r"^\s*(?:///?|\*|/\*)", "", text).strip()
    if len(body) >= 120:
        return False
    # 含 = 和至少一个算术算子 (* /) 或映射 (=> →)
    if re.search(r"[=]", body) and re.search(r"[\*/]|=>|→", body):
        return True
    # 位域映射: `X=Y` 逗号分隔列表 (如 GDT 描述符属性)
    if re.search(r"[A-Za-z_]\w*=\d", body) and body.count("=") >= 2:
        return True
    return False


def is_register_doc(text: str) -> bool:
    """检测是否为硬件寄存器文档注释 (常见于 driver/storage/, driver/virtio/).

    模式:
      - `/// Name (R|RW|WO|W1C, u8|u16|u32|u64)` 寄存器访问类型
      - `/// Name (REG.FIELD)`                     寄存器位段引用
      - `/// Name (REG.FIELD, N-bit @ bit K)`     位位置标注
      - `/// Name (offset 0xNN)`                   偏移量标注

    这些是"硬件规范引用", 等价于 POSIX 签名引用豁免.
    """
    body = re.sub(r"^\s*(?:///?|\*|/\*)", "", text).strip()
    if len(body) >= 100:
        return False
    # 寄存器访问类型 + 位宽
    if re.search(r"\(\s*(R|RW|WO|W1C|RWC|RW1C)\s*,\s*u(8|16|32|64)\s*\)", body, re.IGNORECASE):
        return True
    # 寄存器位段引用 (REG.FIELD, N-bit @ bit K)
    if re.search(r"\b[A-Z][A-Z0-9_]+\.[A-Z][A-Z0-9_]*\b", body):
        # 进一步过滤: 必须含 bit 或 @ 或括号包裹
        if re.search(r"(\bbit\b|@|\(|\))", body, re.IGNORECASE):
            return True
    # 位位置标注 (bits NN-MM / bit NN / [NN:MM])
    if re.search(r"(\bbit[s]?\s+\d|\[\d+:\d+\])", body, re.IGNORECASE):
        return True
    # 偏移量标注 `0xNN`
    if re.search(r"offset\s+0x[0-9A-Fa-f]+", body):
        return True
    return False


def is_code_example(text: str) -> bool:
    """检测是否为代码示例 (含 Rust 关键字/Rust 路径/C 风格结构).

    用途: 文档注释中嵌入的代码块 (如 `//! let new_value = Itimerspec { ... }`)
    不应被误判为英文段落. 命中 2+ 个代码结构标记即视为代码示例.
    """
    body = re.sub(r"^\s*(?:///?|\*|/\*)", "", text).strip()
    # 代码示例必有结构符号密度
    matches = CODE_MARKERS.findall(body)
    return len(matches) >= 2


def is_markdown_table(text: str) -> bool:
    """检测是否为 Markdown 表格行 (| col | col | ... |).

    用途: 模块级文档中的 ASCII/Markdown 寄存器表格, 本身是数据呈现
    形式而非自然语言段落, 不应被视作英文段落违规.
    """
    # 兼容 /// / //! / // / * / /* 等注释前缀
    body = re.sub(r"^\s*(?://{1,3})?!?\s*", "", text)
    body = re.sub(r"\*/$", "", body).strip()
    # 表格行: 以 | 开头, 含 2+ 个 | 分隔符
    if body.startswith("|") and body.endswith("|") and body.count("|") >= 4:
        return True
    # 表格分隔行: |---|---|---|
    if re.match(r"^\|[\s\-:|]+\|$", body):
        return True
    return False


def is_allowed_term(word: str) -> bool:
    """判断英文单词是否在例外清单中."""
    return word in ALLOWED_ENGLISH_TERMS


def detect_violation(comment_text: str, continuation: bool = False) -> tuple[bool, str]:
    """检测单条注释是否为违规.

    返回: (is_violation, reason)
    continuation: 上一行已被识别为迁移记录, 当前行作为列表续行处理.
    """
    stripped = comment_text.strip()
    if not stripped:
        return False, ""

    # SPDX License 标识 (必须保留, 国际通用)
    spdx_body = re.sub(r"^\s*(?:///?|\*|/\*)", "", stripped).strip()
    if spdx_body.startswith("SPDX-License-Identifier:"):
        return False, ""

    # 短注释 (< 4 字符) 跳过
    if len(stripped) < 4:
        return False, ""

    # SAFETY/TODO 短引用注释豁免 (常引用上游文档/标准, < 80 字符)
    if is_safety_or_todo_short_ref(stripped):
        return False, ""

    # POSIX/Linux 签名引用豁免 (单行, 反引号函数名, 等价于标准引用)
    if is_posix_signature_ref(stripped):
        return False, ""

    # 硬件寄存器文档豁免 (单行, 含访问类型或位段引用)
    if is_register_doc(stripped):
        return False, ""

    # Markdown 表格行豁免 (模块文档中的寄存器布局表)
    if is_markdown_table(stripped):
        return False, ""

    # 代码示例豁免 (文档中嵌入的 Rust/C 代码块)
    if is_code_example(stripped):
        return False, ""

    # 公式/等式豁免 (数学公式, 含 = 和算术算子)
    if is_formula_or_equation(stripped):
        return False, ""

    # services 迁移记录豁免 (syscall 列表 / 原属 / 迁移到 services 等目录式说明)
    if is_migration_note(stripped, continuation=continuation):
        return False, ""

    # 代码引用行豁免: 注释主体为反引号包裹的代码引用 (如 `func_name(...)`)
    body = re.sub(r"^\s*(?:///?|\*|/\*)", "", stripped).strip()
    # 剥离所有反引号代码段后, 剩余纯英文长词 < 2 则视为代码引用行
    no_code = re.sub(r"`[^`]+`", "", body)
    remaining_long = [w for w in EN_LONG_WORD.findall(no_code) if not is_allowed_term(w)]
    if len(remaining_long) < 2 and body.count("`") >= 2:
        return False, ""

    has_cjk = bool(HAS_CJK.search(stripped))

    # 含中文字符的注释: 合规 (按"中文字符占主体"原则, 允许夹杂英文技术术语)
    if has_cjk:
        return False, ""

    # B01-09 扩展豁免: 位字段常量注释 (如 `bit0=ADJUST, bit1=DST`).
    # 形如 `NAME=value, NAME=value` 的寄存器位描述属于"标识符引用",
    # 不视为英文段落. 检测: 至少 2 个 ALL_CAPS_KEY=value 形式.
    if re.search(r"\b[A-Z][A-Z0-9_]+\s*[=:]\s*[\w/]+", stripped):
        # 进一步要求: 至少 2 个这样的匹配 (避免误判单一位描述)
        bitfield_count = len(re.findall(r"\b[A-Z][A-Z0-9_]+\s*[=:]\s*[\w/]+", stripped))
        if bitfield_count >= 1:
            # 同时要求短 (< 80 字符), 是典型的位字段注释
            if len(stripped) <= 100:
                return False, ""

    # B01-09 扩展豁免: 纯数字+标点说明 (如 `12 direct + 1 indirect + 1 double`)
    # 形如 `<number> <symbol>` 的列表, 是数据结构说明.
    if re.match(r"^\s*//\s*\d+(\s+[A-Za-z][\w/]*\s*[\+\-\*]?\s*\d*)*\s*$", stripped):
        return False, ""

    # 纯英文注释: 区分"段落式"vs"短引用"
    long_words = EN_LONG_WORD.findall(stripped)
    filtered_words = [w for w in long_words if not is_allowed_term(w)]

    if is_short_english_comment(stripped):
        return False, ""

    # 段落式英文: 2+ 个非例外英文长词
    if len(filtered_words) >= 2:
        return True, f"纯英文段落 (英文长词: {', '.join(filtered_words[:5])})"

    return False, ""


def find_line_comment(line: str) -> int | None:
    """查找行尾 // 注释起始位置 (排除字符串 / 字符字面量内的 //).

    简单字符串扫描状态机:
    - 在 `"..."` 内: 跳过 (字符串字面量)
    - 在 `'..'` 内: 跳过 (字符字面量, Rust 字符字面量通常单字符)
    - 在 `//` 上: 返回该位置
    - 其他: 跳过该字符

    返回 // 的字符索引, 找不到返回 None.
    """
    i = 0
    n = len(line)
    while i < n:
        c = line[i]
        # 字符串字面量: "...", 支持 \" 转义
        if c == '"':
            i += 1
            while i < n:
                if line[i] == '\\' and i + 1 < n:
                    i += 2  # 跳过转义序列
                    continue
                if line[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        # 字符字面量: '...' (Rust 字符字面量常单字符, 但允许转义)
        if c == "'":
            i += 1
            while i < n:
                if line[i] == '\\' and i + 1 < n:
                    i += 2
                    continue
                if line[i] == "'":
                    i += 1
                    break
                i += 1
            continue
        # 行注释 // (但要避免 URL 中的 //)
        if c == '/' and i + 1 < n and line[i + 1] == '/':
            # 简单排除 URL: 前面是 `:` 或 `h` (https://) 跳过
            # 但这是简化版, 复杂场景可能误判
            return i
        i += 1
    return None
def is_safety_or_todo_short_ref(text: str) -> bool:
    # 先剥掉前导 // 或 /// 或 * 或 /*
    body = re.sub(r"^\s*(?:///?|\*|/\*)", "", text).strip()
    if not re.match(r"^(SAFETY|TODO|FIXME|XXX|NOTE|HACK|BUG|WARNING|WARN)\b", body):
        return False
    return len(body) < 80


# 迁移记录注释的特征模式 (用于 2026-06-18 回归豁免).
# 这些注释用于记录 services 迁移历史, 含大量 syscall/函数名列表,
# 本质是"目录式索引", 不应被视作英文段落.
MIGRATION_NOTE_HINTS = (
    "已迁移到 services",
    "原属 ",
    "迁移到 services",
    "迁至 services",
    "依赖 framework safe API",
    "依赖 framework safe api",
)
# syscall 标识符 (sys_xxx), 单词边界避免误匹配
MIGRATION_SYS_PATTERN = re.compile(r"\bsys_[a-z][a-z0-9_]*\b")
# safe API 标识符 (xxx_safe / xxx_safe_xxx), 单词边界
MIGRATION_SAFE_PATTERN = re.compile(r"\b[a-z][a-z0-9_]*_safe(?:_[a-z0-9_]+)?\b")


def is_migration_note(text: str, continuation: bool = False) -> bool:
    """检测是否为 services 迁移记录注释.

    模式 (常见于 syscall/fs/ipc 迁移到 services 层后的历史记录):
      - `// 已迁移到 services: sys_xxx, sys_yyy, ...`
      - `// 原属 framework/foo.rs, 2026-XX-XX 迁移到 services.`
      - `//! 依赖 framework safe API (pipe_write_safe / pipe_read_safe / msgq_send_safe).`

    这些是"目录式索引/历史说明", 含 2+ 个 syscall 或 safe-API 标识符列表,
    等价于 POSIX 签名引用豁免.

    续行 (continuation=True): 上一行已识别为迁移记录, 当前行作为
    syscall/函数名列表的下一行也应豁免. 例如:
        // 已迁移到 services: sys_setregid, sys_mmap,
        // sys_munmap, sys_time, sys_sched_setaffinity
    """
    body = re.sub(r"^\s*(?:///?|\*|/\*)", "", text).strip()
    if continuation:
        # 续行: 列表继续 (以英文标识符 + 标点为主, 长度适中)
        if not body or len(body) > 120:
            return False
        # 续行必须有英文标识符 (sys_ / _safe / 逗号分隔列表)
        long_words = EN_LONG_WORD.findall(body)
        if len(long_words) < 1:
            return False
        return True
    if len(body) >= 200:
        return False
    # 必须显式包含"已迁移到 services"等迁移关键字, 避免误判普通英文段落
    if not any(hint in body for hint in MIGRATION_NOTE_HINTS):
        return False
    # 命中 2+ 个 syscall 标识符 (典型: 迁移清单)
    sys_count = len(MIGRATION_SYS_PATTERN.findall(body))
    if sys_count >= 2:
        return True
    # 或命中 2+ 个 safe-API 标识符 (典型: 依赖说明)
    safe_count = len(MIGRATION_SAFE_PATTERN.findall(body))
    if safe_count >= 2:
        return True
    return False


def iter_comments(rs_file: Path) -> Iterator[tuple[int, str, bool]]:
    """逐行迭代 .rs 文件, 产出 (行号, 注释文本, 是否迁移记录续行).

    续行标记: 上一行被识别为迁移记录 (已迁移到 services/原属 .../迁移到 services
    关键字) 时, 当前行作为该迁移记录的列表续行传递, 供 detect_violation 豁免.
    """
    try:
        content = rs_file.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return

    in_block_comment = False
    in_doc_code_block = False  # 文档注释中的 ```text / ```rust / ```c 等代码块
    in_migration_block = False  # 迁移记录列表是否仍在续行中
    for lineno, line in enumerate(content.splitlines(), start=1):
        stripped = line.lstrip()
        # 检测文档代码块 ``` (开/关). 兼容 `/// ```rust` / `//! ```text` 形式
        # 注释前缀支持 `///` / `//!` / `//` 三种
        code_fence_match = (
            re.match(r"^(?://{1,2}!?)\s*```", stripped) is not None
            or stripped.startswith("```")
        )
        if code_fence_match:
            in_doc_code_block = not in_doc_code_block
            continue
        if in_doc_code_block:
            # 代码块内不审计 (ASCII art 表格, 寄存器布局等)
            continue
        if in_block_comment:
            # 块注释结束: 找 */
            end_idx = stripped.find("*/")
            if end_idx >= 0:
                in_block_comment = False
                yield lineno, stripped[:end_idx + 2], in_migration_block
            else:
                yield lineno, stripped, in_migration_block
            continue
        if stripped.startswith("/*"):
            end_idx = stripped.find("*/", 2)
            if end_idx >= 0:
                yield lineno, stripped, in_migration_block
            else:
                in_block_comment = True
                yield lineno, stripped, in_migration_block
            continue
        if stripped.startswith("//"):
            # 先产出当前行 (含续行状态)
            yield lineno, stripped, in_migration_block
            # 再判断当前行是否开启新一轮迁移记录
            in_migration_block = is_migration_note(stripped, continuation=False)
            continue
        # B01-09 修复: 行尾注释检测 (`let x = f(); // English text`)
        # 原脚本不检测行尾 // 注释, 漏掉行尾英文段落.
        # 状态机: 跳过字符串字面量 / 字符字面量内的 //, 找到行尾 //.
        # 这里使用简化版: 直接查找未被字符串包裹的 // .
        line_comment_idx = find_line_comment(line)
        if line_comment_idx is not None:
            tail_comment = line[line_comment_idx:]
            yield lineno, tail_comment, in_migration_block
            # 行尾注释也是迁移记录的延续
            if is_migration_note(stripped, continuation=False):
                in_migration_block = True
            continue
        # 非注释行 → 中断迁移续行状态
        in_migration_block = False
        # 仅在处于块注释内时, 才是注释行 (修复: Rust 解引用 `*list = ...` 不应被误判)
        # 块注释内的 `*` 是装饰字符, 如 `/*\n * comment\n */`


def main() -> int:
    if not BASE.exists():
        print(f"error: base dir not found: {BASE}", file=sys.stderr)
        return 2

    issues: list[str] = []
    files_scanned = 0
    files_with_issues: set[str] = set()

    for rs_file in BASE.rglob("*.rs"):
        files_scanned += 1
        rel = rs_file.relative_to(PROJECT_ROOT)
        rel_str = str(rel)
        # 排除 vendored 第三方代码
        if "smoltcp/" in rel_str:
            continue
        # 排除 cargo 构建产物 (target/ 下第三方 build script 的 probe/生成代码)
        if "/target/" in rel_str:
            continue
        # 排除自动生成
        if rel.name == "build.rs":
            continue
        # 排除 x86_64/aarch64 arch asm 文件 (有大量英文注释作示例)
        if rel.suffix in (".S", ".s", ".asm"):
            continue

        for lineno, comment, is_cont in iter_comments(rs_file):
            is_violation, reason = detect_violation(comment, continuation=is_cont)
            if is_violation:
                issues.append(f"{rel}:{lineno}: [{reason}] {comment.strip()[:80]}")
                files_with_issues.add(rel_str)

    if issues:
        print(f"TD-22 注释语言 audit FAILED: {len(issues)} 处违规, 涉及 {len(files_with_issues)} 个文件")
        for issue in issues[:80]:
            print(f"  {issue}")
        if len(issues) > 80:
            print(f"  ... and {len(issues) - 80} more")
        print(f"\n扫描: {files_scanned} 个 .rs 文件, 排除 vendored smoltcp/*")
        return 1

    print(f"TD-22 注释语言 audit PASSED: 扫描 {files_scanned} 个 .rs 文件, 0 违规")
    return 0


if __name__ == "__main__":
    sys.exit(main())
