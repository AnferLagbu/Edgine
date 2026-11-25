//! Linux syscall ABI 的架构边界翻译表。
//!
//! 内部实现统一使用 `x86_64 Linux` 编号（`canonical`）；x86_64 入口恒等，
//! aarch64 入口把 Linux aarch64 公开号翻译成 canonical，用户态门反向翻译。
//! aarch64 没有对应号的 x86_64 遗留 syscall（open/stat/poll 等）走高位私有
//! legacy 段，不占用任何 Linux 公开号。

/// 私有 syscall 基址：所有 EG/SGEG 私有号 = 本基址 + 旧私有编号。
pub const PRIVATE_BASE: u64 = 0x4000_0000;

/// aarch64 无对应 Linux 号的遗留 syscall 段基址。
pub const PRIVATE_LEGACY_BASE: u64 = PRIVATE_BASE + 0x1000;

/// 私有段上界（不含）：覆盖 EG 500–899 与 legacy 段。
pub const PRIVATE_END: u64 = PRIVATE_BASE + 0x2000;

/// 未识别编号哨兵。
pub const UNKNOWN: u64 = u64::MAX;

/// 把 `canonical`（`x86_64 Linux`）编号翻译为 aarch64 公开编号。
///
/// - 私有号原样返回；
/// - aarch64 有对应 Linux 号的返回该号；
/// - 其余 canonical Linux 号落到 [`PRIVATE_LEGACY_BASE`] 段。
#[must_use]
pub const fn canonical_to_aarch64(canonical: u64) -> u64 {
    if canonical >= PRIVATE_BASE {
        return if canonical < PRIVATE_END {
            canonical
        } else {
            UNKNOWN
        };
    }
    let mapped = match canonical {
        0..=99 => canonical_to_aarch64_low(canonical),
        100..=199 => canonical_to_aarch64_mid(canonical),
        200..=299 => canonical_to_aarch64_upper(canonical),
        300..=999 => canonical_to_aarch64_high(canonical),
        _ => UNKNOWN,
    };
    if mapped == UNKNOWN && canonical < 0x1000 {
        PRIVATE_LEGACY_BASE + canonical
    } else {
        mapped
    }
}

const fn canonical_to_aarch64_low(nr: u64) -> u64 {
    match nr {
        0 => 63,   // SYS_READ
        1 => 64,   // SYS_WRITE
        3 => 57,   // SYS_CLOSE
        5 => 80,   // SYS_FSTAT
        8 => 62,   // SYS_LSEEK
        9 => 222,  // SYS_MMAP
        10 => 226, // SYS_MPROTECT
        11 => 215, // SYS_MUNMAP
        12 => 214, // SYS_BRK
        13 => 134, // SYS_RT_SIGACTION
        14 => 135, // SYS_RT_SIGPROCMASK
        15 => 139, // SYS_RT_SIGRETURN
        16 => 29,  // SYS_IOCTL
        17 => 67,  // SYS_PREAD64
        18 => 68,  // SYS_PWRITE64
        19 => 65,  // SYS_READV
        20 => 66,  // SYS_WRITEV
        24 => 124, // SYS_SCHED_YIELD
        25 => 216, // SYS_MREMAP
        27 => 232, // SYS_MINCORE
        28 => 233, // SYS_MADVISE
        32 => 23,  // SYS_DUP
        35 => 101, // SYS_NANOSLEEP
        36 => 102, // SYS_GETITIMER
        38 => 103, // SYS_SETITIMER
        39 => 172, // SYS_GETPID
        40 => 71,  // SYS_SENDFILE
        41 => 198, // SYS_SOCKET
        42 => 203, // SYS_CONNECT
        43 => 202, // SYS_ACCEPT
        44 => 206, // SYS_SENDTO
        45 => 207, // SYS_RECVFROM
        46 => 211, // SYS_SENDMSG
        47 => 212, // SYS_RECVMSG
        48 => 210, // SYS_SHUTDOWN
        49 => 200, // SYS_BIND
        50 => 201, // SYS_LISTEN
        51 => 204, // SYS_GETSOCKNAME
        52 => 205, // SYS_GETPEERNAME
        53 => 199, // SYS_SOCKETPAIR
        54 => 208, // SYS_SETSOCKOPT
        55 => 209, // SYS_GETSOCKOPT
        56 => 220, // SYS_CLONE
        59 => 221, // SYS_EXECVE
        60 => 93,  // SYS_EXIT
        61 => 260, // SYS_WAIT4
        62 => 129, // SYS_KILL
        63 => 160, // SYS_UNAME
        72 => 25,  // SYS_FCNTL
        73 => 32,  // SYS_FLOCK
        74 => 82,  // SYS_FSYNC
        75 => 83,  // SYS_FDATASYNC
        76 => 45,  // SYS_TRUNCATE
        77 => 46,  // SYS_FTRUNCATE
        79 => 17,  // SYS_GETCWD
        80 => 49,  // SYS_CHDIR
        91 => 52,  // SYS_FCHMOD
        93 => 55,  // SYS_FCHOWN
        95 => 166, // SYS_UMASK
        96 => 169, // SYS_GETTIMEOFDAY
        97 => 163, // SYS_GETRLIMIT
        98 => 165, // SYS_GETRUSAGE
        99 => 179, // SYS_SYSINFO
        _ => UNKNOWN,
    }
}

const fn canonical_to_aarch64_mid(nr: u64) -> u64 {
    match nr {
        100 => 153, // SYS_TIMES
        102 => 174, // SYS_GETUID
        104 => 176, // SYS_GETGID
        105 => 146, // SYS_SETUID
        106 => 144, // SYS_SETGID
        107 => 175, // SYS_GETEUID
        108 => 177, // SYS_GETEGID
        109 => 154, // SYS_SETPGID
        110 => 173, // SYS_GETPPID
        112 => 157, // SYS_SETSID
        113 => 145, // SYS_SETREUID
        114 => 143, // SYS_SETREGID
        121 => 155, // SYS_GETPGID
        124 => 156, // SYS_GETSID
        125 => 90,  // SYS_CAPGET
        126 => 91,  // SYS_CAPSET
        131 => 132, // SYS_SIGALTSTACK
        140 => 141, // SYS_GETPRIORITY
        141 => 140, // SYS_SETPRIORITY
        144 => 119, // SYS_SCHED_SETSCHEDULER
        145 => 120, // SYS_SCHED_GETSCHEDULER
        149 => 228, // SYS_MLOCK
        150 => 229, // SYS_MUNLOCK
        151 => 230, // SYS_MLOCKALL
        152 => 231, // SYS_MUNLOCKALL
        155 => 41,  // SYS_PIVOT_ROOT
        157 => 167, // SYS_PRCTL
        159 => 171, // SYS_ADJTIMEX
        160 => 164, // SYS_SETRLIMIT
        161 => 51,  // SYS_CHROOT
        162 => 81,  // SYS_SYNC
        164 => 170, // SYS_SETTIMEOFDAY
        165 => 40,  // SYS_MOUNT
        166 => 39,  // SYS_UMOUNT2
        169 => 142, // SYS_REBOOT
        170 => 161, // SYS_SETHOSTNAME
        171 => 162, // SYS_SETDOMAINNAME
        186 => 178, // SYS_GETTID
        188 => 5,   // SYS_SETXATTR
        191 => 8,   // SYS_GETXATTR
        194 => 11,  // SYS_LISTXATTR
        197 => 14,  // SYS_REMOVEXATTR
        _ => UNKNOWN,
    }
}

const fn canonical_to_aarch64_upper(nr: u64) -> u64 {
    match nr {
        202 => 98,  // SYS_FUTEX
        203 => 122, // SYS_SCHED_SETAFFINITY
        204 => 123, // SYS_SCHED_GETAFFINITY
        218 => 96,  // SYS_SET_TID_ADDRESS
        222 => 107, // SYS_TIMER_CREATE
        223 => 110, // SYS_TIMER_SETTIME
        224 => 108, // SYS_TIMER_GETTIME
        225 => 109, // SYS_TIMER_GETOVERRUN
        226 => 111, // SYS_TIMER_DELETE
        228 => 113, // SYS_CLOCK_GETTIME
        229 => 114, // SYS_CLOCK_GETRES
        230 => 115, // SYS_CLOCK_NANOSLEEP
        231 => 94,  // SYS_EXIT_GROUP
        233 => 21,  // SYS_EPOLL_CTL
        234 => 131, // SYS_TGKILL
        237 => 235, // SYS_MBIND
        238 => 237, // SYS_SET_MEMPOLICY
        239 => 236, // SYS_GET_MEMPOLICY
        246 => 104, // SYS_KEXEC_LOAD
        247 => 95,  // SYS_WAITID
        254 => 27,  // SYS_INOTIFY_ADD_WATCH
        255 => 28,  // SYS_INOTIFY_RM_WATCH
        256 => 238, // SYS_MIGRATE_PAGES
        257 => 56,  // SYS_OPENAT
        260 => 54,  // SYS_FCHOWNAT
        262 => 79,  // SYS_NEWFSTATAT
        263 => 35,  // SYS_UNLINKAT
        264 => 38,  // SYS_RENAMEAT
        265 => 37,  // SYS_LINKAT
        266 => 36,  // SYS_SYMLINKAT
        267 => 78,  // SYS_READLINKAT
        268 => 53,  // SYS_FCHMODAT
        269 => 48,  // SYS_FACCESSAT
        270 => 72,  // SYS_PSELECT6
        271 => 73,  // SYS_PPOLL
        272 => 97,  // SYS_UNSHARE
        273 => 99,  // SYS_SET_ROBUST_LIST
        274 => 100, // SYS_GET_ROBUST_LIST
        275 => 76,  // SYS_SPLICE
        280 => 88,  // SYS_UTIMENSAT
        281 => 22,  // SYS_EPOLL_PWAIT
        283 => 85,  // SYS_TIMERFD_CREATE
        285 => 47,  // SYS_FALLOCATE
        286 => 86,  // SYS_TIMERFD_SETTIME
        287 => 87,  // SYS_TIMERFD_GETTIME
        288 => 242, // SYS_ACCEPT4
        289 => 74,  // SYS_SIGNALFD4
        290 => 19,  // SYS_EVENTFD2
        291 => 20,  // SYS_EPOLL_CREATE1
        292 => 24,  // SYS_DUP3
        293 => 59,  // SYS_PIPE2
        294 => 26,  // SYS_INOTIFY_INIT1
        295 => 69,  // SYS_PREADV
        296 => 70,  // SYS_PWRITEV
        299 => 243, // SYS_RECVMMSG
        _ => UNKNOWN,
    }
}

const fn canonical_to_aarch64_high(nr: u64) -> u64 {
    match nr {
        302 => 261, // SYS_PRLIMIT64
        303 => 264, // SYS_NAME_TO_HANDLE_AT
        304 => 265, // SYS_OPEN_BY_HANDLE_AT
        307 => 269, // SYS_SENDMMSG
        308 => 268, // SYS_SETNS
        309 => 168, // SYS_GETCPU
        310 => 270, // SYS_PROCESS_VM_READV
        311 => 271, // SYS_PROCESS_VM_WRITEV
        314 => 274, // SYS_SCHED_SETATTR
        315 => 275, // SYS_SCHED_GETATTR
        316 => 276, // SYS_RENAMEAT2
        317 => 277, // SYS_SECCOMP
        318 => 278, // SYS_GETRANDOM
        319 => 279, // SYS_MEMFD_CREATE
        321 => 280, // SYS_BPF
        322 => 281, // SYS_EXECVEAT
        323 => 282, // SYS_USERFAULTFD
        326 => 285, // SYS_COPY_FILE_RANGE
        327 => 286, // SYS_PREADV2
        328 => 287, // SYS_PWRITEV2
        332 => 291, // SYS_STATX
        424 => 424, // SYS_PIDFD_SEND_SIGNAL
        425 => 425, // SYS_IO_URING_SETUP
        426 => 426, // SYS_IO_URING_ENTER
        427 => 427, // SYS_IO_URING_REGISTER
        434 => 434, // SYS_PIDFD_OPEN
        435 => 435, // SYS_CLONE3
        436 => 436, // SYS_CLOSE_RANGE
        437 => 437, // SYS_OPENAT2
        438 => 438, // SYS_PIDFD_GETFD
        439 => 439, // SYS_FACCESSAT2
        441 => 441, // SYS_EPOLL_PWAIT2
        452 => 452, // SYS_FCHMODAT2
        _ => UNKNOWN,
    }
}

/// 把 aarch64 公开编号翻译回 `canonical`（`x86_64 Linux`）编号。
///
/// 私有号原样返回；legacy 段按偏移还原；未知 Linux 号返回 [`UNKNOWN`]。
#[must_use]
pub const fn aarch64_to_canonical(aarch64: u64) -> u64 {
    if aarch64 >= PRIVATE_LEGACY_BASE && aarch64 < PRIVATE_LEGACY_BASE + 0x1000 {
        return aarch64 - PRIVATE_LEGACY_BASE;
    }
    if aarch64 >= PRIVATE_BASE {
        return if aarch64 < PRIVATE_END {
            aarch64
        } else {
            UNKNOWN
        };
    }
    match aarch64 {
        0..=99 => aarch64_to_canonical_low(aarch64),
        100..=199 => aarch64_to_canonical_mid(aarch64),
        200..=299 => aarch64_to_canonical_upper(aarch64),
        300..=999 => aarch64_to_canonical_high(aarch64),
        _ => UNKNOWN,
    }
}

const fn aarch64_to_canonical_low(nr: u64) -> u64 {
    match nr {
        63 => 0,   // SYS_READ
        64 => 1,   // SYS_WRITE
        57 => 3,   // SYS_CLOSE
        80 => 5,   // SYS_FSTAT
        62 => 8,   // SYS_LSEEK
        29 => 16,  // SYS_IOCTL
        67 => 17,  // SYS_PREAD64
        68 => 18,  // SYS_PWRITE64
        65 => 19,  // SYS_READV
        66 => 20,  // SYS_WRITEV
        23 => 32,  // SYS_DUP
        71 => 40,  // SYS_SENDFILE
        93 => 60,  // SYS_EXIT
        25 => 72,  // SYS_FCNTL
        32 => 73,  // SYS_FLOCK
        82 => 74,  // SYS_FSYNC
        83 => 75,  // SYS_FDATASYNC
        45 => 76,  // SYS_TRUNCATE
        46 => 77,  // SYS_FTRUNCATE
        17 => 79,  // SYS_GETCWD
        49 => 80,  // SYS_CHDIR
        52 => 91,  // SYS_FCHMOD
        55 => 93,  // SYS_FCHOWN
        90 => 125, // SYS_CAPGET
        91 => 126, // SYS_CAPSET
        41 => 155, // SYS_PIVOT_ROOT
        51 => 161, // SYS_CHROOT
        81 => 162, // SYS_SYNC
        40 => 165, // SYS_MOUNT
        39 => 166, // SYS_UMOUNT2
        5 => 188,  // SYS_SETXATTR
        8 => 191,  // SYS_GETXATTR
        11 => 194, // SYS_LISTXATTR
        14 => 197, // SYS_REMOVEXATTR
        96 => 218, // SYS_SET_TID_ADDRESS
        98 => 202, // SYS_FUTEX
        94 => 231, // SYS_EXIT_GROUP
        21 => 233, // SYS_EPOLL_CTL
        95 => 247, // SYS_WAITID
        27 => 254, // SYS_INOTIFY_ADD_WATCH
        28 => 255, // SYS_INOTIFY_RM_WATCH
        56 => 257, // SYS_OPENAT
        54 => 260, // SYS_FCHOWNAT
        79 => 262, // SYS_NEWFSTATAT
        35 => 263, // SYS_UNLINKAT
        38 => 264, // SYS_RENAMEAT
        37 => 265, // SYS_LINKAT
        36 => 266, // SYS_SYMLINKAT
        78 => 267, // SYS_READLINKAT
        53 => 268, // SYS_FCHMODAT
        48 => 269, // SYS_FACCESSAT
        72 => 270, // SYS_PSELECT6
        73 => 271, // SYS_PPOLL
        97 => 272, // SYS_UNSHARE
        99 => 273, // SYS_SET_ROBUST_LIST
        76 => 275, // SYS_SPLICE
        88 => 280, // SYS_UTIMENSAT
        22 => 281, // SYS_EPOLL_PWAIT
        85 => 283, // SYS_TIMERFD_CREATE
        47 => 285, // SYS_FALLOCATE
        86 => 286, // SYS_TIMERFD_SETTIME
        87 => 287, // SYS_TIMERFD_GETTIME
        74 => 289, // SYS_SIGNALFD4
        19 => 290, // SYS_EVENTFD2
        20 => 291, // SYS_EPOLL_CREATE1
        24 => 292, // SYS_DUP3
        59 => 293, // SYS_PIPE2
        26 => 294, // SYS_INOTIFY_INIT1
        69 => 295, // SYS_PREADV
        70 => 296, // SYS_PWRITEV
        _ => UNKNOWN,
    }
}

const fn aarch64_to_canonical_mid(nr: u64) -> u64 {
    match nr {
        134 => 13,  // SYS_RT_SIGACTION
        135 => 14,  // SYS_RT_SIGPROCMASK
        139 => 15,  // SYS_RT_SIGRETURN
        124 => 24,  // SYS_SCHED_YIELD
        101 => 35,  // SYS_NANOSLEEP
        102 => 36,  // SYS_GETITIMER
        103 => 38,  // SYS_SETITIMER
        172 => 39,  // SYS_GETPID
        198 => 41,  // SYS_SOCKET
        199 => 53,  // SYS_SOCKETPAIR
        129 => 62,  // SYS_KILL
        160 => 63,  // SYS_UNAME
        166 => 95,  // SYS_UMASK
        169 => 96,  // SYS_GETTIMEOFDAY
        163 => 97,  // SYS_GETRLIMIT
        165 => 98,  // SYS_GETRUSAGE
        179 => 99,  // SYS_SYSINFO
        153 => 100, // SYS_TIMES
        174 => 102, // SYS_GETUID
        176 => 104, // SYS_GETGID
        146 => 105, // SYS_SETUID
        144 => 106, // SYS_SETGID
        175 => 107, // SYS_GETEUID
        177 => 108, // SYS_GETEGID
        154 => 109, // SYS_SETPGID
        173 => 110, // SYS_GETPPID
        157 => 112, // SYS_SETSID
        145 => 113, // SYS_SETREUID
        143 => 114, // SYS_SETREGID
        155 => 121, // SYS_GETPGID
        156 => 124, // SYS_GETSID
        132 => 131, // SYS_SIGALTSTACK
        141 => 140, // SYS_GETPRIORITY
        140 => 141, // SYS_SETPRIORITY
        119 => 144, // SYS_SCHED_SETSCHEDULER
        120 => 145, // SYS_SCHED_GETSCHEDULER
        167 => 157, // SYS_PRCTL
        171 => 159, // SYS_ADJTIMEX
        164 => 160, // SYS_SETRLIMIT
        170 => 164, // SYS_SETTIMEOFDAY
        142 => 169, // SYS_REBOOT
        161 => 170, // SYS_SETHOSTNAME
        162 => 171, // SYS_SETDOMAINNAME
        178 => 186, // SYS_GETTID
        122 => 203, // SYS_SCHED_SETAFFINITY
        123 => 204, // SYS_SCHED_GETAFFINITY
        107 => 222, // SYS_TIMER_CREATE
        110 => 223, // SYS_TIMER_SETTIME
        108 => 224, // SYS_TIMER_GETTIME
        109 => 225, // SYS_TIMER_GETOVERRUN
        111 => 226, // SYS_TIMER_DELETE
        113 => 228, // SYS_CLOCK_GETTIME
        114 => 229, // SYS_CLOCK_GETRES
        115 => 230, // SYS_CLOCK_NANOSLEEP
        131 => 234, // SYS_TGKILL
        104 => 246, // SYS_KEXEC_LOAD
        100 => 274, // SYS_GET_ROBUST_LIST
        168 => 309, // SYS_GETCPU
        _ => UNKNOWN,
    }
}

const fn aarch64_to_canonical_upper(nr: u64) -> u64 {
    match nr {
        222 => 9,   // SYS_MMAP
        226 => 10,  // SYS_MPROTECT
        215 => 11,  // SYS_MUNMAP
        214 => 12,  // SYS_BRK
        216 => 25,  // SYS_MREMAP
        232 => 27,  // SYS_MINCORE
        233 => 28,  // SYS_MADVISE
        203 => 42,  // SYS_CONNECT
        202 => 43,  // SYS_ACCEPT
        206 => 44,  // SYS_SENDTO
        207 => 45,  // SYS_RECVFROM
        211 => 46,  // SYS_SENDMSG
        212 => 47,  // SYS_RECVMSG
        210 => 48,  // SYS_SHUTDOWN
        200 => 49,  // SYS_BIND
        201 => 50,  // SYS_LISTEN
        204 => 51,  // SYS_GETSOCKNAME
        205 => 52,  // SYS_GETPEERNAME
        208 => 54,  // SYS_SETSOCKOPT
        209 => 55,  // SYS_GETSOCKOPT
        220 => 56,  // SYS_CLONE
        221 => 59,  // SYS_EXECVE
        260 => 61,  // SYS_WAIT4
        261 => 302, // SYS_PRLIMIT64
        228 => 149, // SYS_MLOCK
        229 => 150, // SYS_MUNLOCK
        230 => 151, // SYS_MLOCKALL
        231 => 152, // SYS_MUNLOCKALL
        235 => 237, // SYS_MBIND
        237 => 238, // SYS_SET_MEMPOLICY
        236 => 239, // SYS_GET_MEMPOLICY
        238 => 256, // SYS_MIGRATE_PAGES
        242 => 288, // SYS_ACCEPT4
        243 => 299, // SYS_RECVMMSG
        264 => 303, // SYS_NAME_TO_HANDLE_AT
        265 => 304, // SYS_OPEN_BY_HANDLE_AT
        269 => 307, // SYS_SENDMMSG
        268 => 308, // SYS_SETNS
        270 => 310, // SYS_PROCESS_VM_READV
        271 => 311, // SYS_PROCESS_VM_WRITEV
        274 => 314, // SYS_SCHED_SETATTR
        275 => 315, // SYS_SCHED_GETATTR
        276 => 316, // SYS_RENAMEAT2
        277 => 317, // SYS_SECCOMP
        278 => 318, // SYS_GETRANDOM
        279 => 319, // SYS_MEMFD_CREATE
        280 => 321, // SYS_BPF
        281 => 322, // SYS_EXECVEAT
        282 => 323, // SYS_USERFAULTFD
        285 => 326, // SYS_COPY_FILE_RANGE
        286 => 327, // SYS_PREADV2
        287 => 328, // SYS_PWRITEV2
        291 => 332, // SYS_STATX
        _ => UNKNOWN,
    }
}

const fn aarch64_to_canonical_high(nr: u64) -> u64 {
    match nr {
        424 => 424, // SYS_PIDFD_SEND_SIGNAL
        425 => 425, // SYS_IO_URING_SETUP
        426 => 426, // SYS_IO_URING_ENTER
        427 => 427, // SYS_IO_URING_REGISTER
        434 => 434, // SYS_PIDFD_OPEN
        435 => 435, // SYS_CLONE3
        436 => 436, // SYS_CLOSE_RANGE
        437 => 437, // SYS_OPENAT2
        438 => 438, // SYS_PIDFD_GETFD
        439 => 439, // SYS_FACCESSAT2
        441 => 441, // SYS_EPOLL_PWAIT2
        452 => 452, // SYS_FCHMODAT2
        _ => UNKNOWN,
    }
}
