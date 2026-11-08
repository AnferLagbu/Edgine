//! Edgine Init — fork / KPTI 隔离测试 (`print_char`)

#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use userlib::sys::{
    AF_INET, ARCH_GET_FS, ARCH_SET_FS, CLOCK_MONOTONIC, CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID,
    CLONE_FILES, CLONE_FS, CLONE_PARENT_SETTID, CLONE_SETTLS, CLONE_SIGHAND, CLONE_THREAD,
    CLONE_VM, IPPROTO_TCP, InAddr, O_RDONLY, POLLIN, PollFd, SHUT_WR, SO_REUSEADDR, SOCK_STREAM,
    SOL_SOCKET, SockaddrIn, TCP_NODELAY, Timespec, accept, arch_prctl, bind, clock_gettime,
    clone_thread, close_socket, env_chdir, env_chroot, exit_group, fork, fs_close, fs_open,
    fs_read, fs_write, futex_wait, futex_wait_bitset, futex_wake, getpid, getsockopt, gettid, kill,
    listen, nanosleep, pipe_create, poll, proc_exec, proc_exit, recv, rt_sigaction, sched_snapshot,
    sched_yield, send, setsockopt, shutdown, socket, tgkill, wait_pid, wait_pid_status,
    wait_pid_status_nohang,
};
// 仅 x86_64 探针 (ipv6_udp_probe / udp_connect_probe / mcast_probe / udp_echo_probe) 用到的
// UDP 与组播成员管理 ABI; aarch64 无对应 NIC/RA/注帧环境, 故按架构分列以免未用导入告警.
#[cfg(target_arch = "x86_64")]
use userlib::sys::{
    AF_INET6, F_SETFL, IP_ADD_MEMBERSHIP, IP_DROP_MEMBERSHIP, IPPROTO_IP, IPPROTO_IPV6,
    IPPROTO_UDP, IPV6_ADD_MEMBERSHIP, IPV6_DROP_MEMBERSHIP, In6Addr, IpMreq, Ipv6Mreq, O_NONBLOCK,
    SOCK_DGRAM, SockaddrIn6, connect, fcntl, getpeername, getsockname, recvfrom, sendto,
};
use userlib::{print, print_char, print_dec, println};

/// KPTI-09 探针目标: 内核镜像基址的**高半区(高别名)映射**。
///
/// 该地址在任何用户视图下都不得可读 —— 读到值即 KPTI 隔离失效。
/// 基址取自各架构链接脚本把内核镜像放在物理 LMA 起点的约定：
/// - x86_64: `KERNEL_BASE(0xFFFF_8000_0000_0000) + 0x100000`
///   （见 `privileged/link/x86_64.ld` 的 `. = 0x100000`，对应
///   `privileged/mm/mod.rs` 的 `KERNEL_BASE`）。
/// - aarch64: `KERNEL_BASE(0xFFFF_0000_0000_0000) + 0x4008_0000`
///   （见 `privileged/link/aarch64.ld` 的 `. = 0x40080000`，对应
///   `privileged/mm/mod.rs` 的 `KERNEL_BASE`）。
#[cfg(target_arch = "x86_64")]
const KERNEL_IMAGE_ALIAS: u64 = 0xFFFF_8000_0000_0000 + 0x10_0000;
#[cfg(target_arch = "aarch64")]
const KERNEL_IMAGE_ALIAS: u64 = 0xFFFF_0000_0000_0000 + 0x4008_0000;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    proc_exit(1);
}

/// init 入口: 依次跑 IPC / KPTI / 网络探针, 再 fork 常驻 TCP echo 与忙等子进程.
///
/// # Panics
///
/// `fork` 返回的 pid 超出 `i32` 时 panic —— 正常路径下 pid 恒为小正整数.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print_char(b'X');
    print_char(b'\n');
    // IPC 机制探针: 覆盖 `kernel_init` 的 `ipc_init()` 接线 (未接线时首次
    // `pipe()` 会按 0..IPC_MAX_PIPES 索引空 Vec 而越界 panic). 须在 fork 前
    // 同步完成, 避免与 echo 子进程的 fd 分配交错.
    pipe_probe();
    // 第一次 fork：子进程立即退出，父进程 wait4 收割 —— 收割路径 `remove_and_free`
    // 使 `Process::drop` 运行并销毁其地址空间 (走 destroy_page_table)，该批待释放帧
    // 必然滞留 pending 链（远程核尚未追平代），父进程继续执行
    let child1 = fork();
    if child1 == 0 {
        proc_exit(0);
    }
    let _ = wait_pid(i32::try_from(child1).expect("fork 返回的 pid 超出 i32"));
    print_char(b'Y');
    print_char(b'\n');
    // 第二次 fork：子进程退出时再次结算 release_lock，出口排空上一次滞留的 pending 帧，
    // 帧真正归还 PMM，使延迟释放路径被完整执行
    let child2 = fork();
    if child2 == 0 {
        proc_exit(0);
    }
    let _ = wait_pid(i32::try_from(child2).expect("fork 返回的 pid 超出 i32"));
    // KPTI-09: 探针子进程以 EL0 读取内核镜像高半区别名 —— 预期内核
    // (x86_64 #PF / aarch64 同步异常) 终止该子进程, 故该读取**不会返回值**。
    // 父进程以退出码判定: 非 0 ⇒ 隔离生效; 读到值 ⇒ 子进程打印 FAIL 并以 0 退出。
    let probe = fork();
    if probe == 0 {
        // SAFETY: 故意以用户态解引用内核高半区别名。该地址不在用户视图内,
        // 预期触发异常并由内核终止本进程; 此处**不**假设任何可读内容。
        let byte = unsafe { core::ptr::read_volatile(KERNEL_IMAGE_ALIAS as *const u8) };
        // 执行到此 ⇒ 内核高半区可被用户态读取, 隔离失效。
        print("[KPTI] FAIL: kernel high-half readable from EL0, byte=");
        print_dec(i64::from(byte));
        print_char(b'\n');
        proc_exit(0);
    }
    let probe_pid = i32::try_from(probe).expect("fork 返回的 pid 超出 i32");
    if wait_pid(probe_pid) != 0 {
        print("[KPTI] EL0 kernel high-half access denied (pid=");
        print_dec(i64::from(probe_pid));
        println(")");
    } else {
        print("[KPTI] FAIL: EL0 kernel high-half access was NOT denied (pid=");
        print_dec(i64::from(probe_pid));
        println(")");
    }

    // ── K-00: 用户态 TLS 基址随切换恢复 ─────────────────────────────────
    // 必须在 busy 忙等子进程 fork 之前同步完成: 本探针依赖父子进程交替让出
    // CPU 观察切换恢复, 忙等子进程独占次核会降低交替机会.
    tls_probe();

    // ── K-07a: 线程组运行时探针 ────────────────────────────────────────
    // 同样必须在 busy 忙等子进程 fork 之前同步完成: 线程腿依赖"父线程睡下 →
    // 子线程上核 → 子线程睡下 → 父线程上核"的交替推进, 忙等子进程独占一个核
    // 会拖慢该交替直至撞上重试上界.
    thread_probe();

    // ── A-10: 返回用户态信号投递探针 ────────────────────────────────────
    // 必须在 busy 忙等子进程 fork 之前同步完成: IRQ 腿依赖目标线程在次核上
    // 纯用户自旋, 忙等子进程会抢占该核并掩盖定时器中断的投递窗口.
    sig_probe();

    // ── K-07b 批2: execve 运行时探针 ────────────────────────────────────
    // 载荷 `/bin/exectest` 只在 initramfs 载体启动时存在; 默认启动 (无文件
    // 系统载体) 会打印 FAIL, 与网络探针"无宿主服务即 FAIL 并继续"同口径.
    // 必须在 busy 忙等子进程 fork 之前同步完成: 探针靠父进程 wait4 收割.
    exec_probe();

    // ── #108 丙: aarch64 缺页 / COW 恢复路径真链探针 ─────────────────────
    // 必须在 busy 忙等子进程 fork 之前同步完成: 三腿都靠父进程 wait4 收割子进程,
    // 且 `reuse` 腿要求子进程页表**已拆除** (持有计数回到 1) 才成立.
    #[cfg(target_arch = "aarch64")]
    cow_probe();

    // ── APS-05: 双核并发 EL0 验证 ─────────────────────────────────────────
    // fork 一个**不 yield** 的忙等子进程: 父子各自长期占用一核并停留在 EL0
    // (fork 的任务投送路径会把子进程推到空闲次核). 忙等期间只以极低频率发
    // syscall (print_char), 供内核每核有界诊断 `[SMP] EL0 pid=N cpu=M` 锚定
    // "本核确有 EL0 任务在执行" —— syscall 只能由 EL0 任务发起. 内核侧上限
    // 4 行/核, 故此处打印 8 次足以覆盖; 之后静默自增 (仍不 yield, 两核不空闲).
    let busy = fork();
    if busy == 0 {
        busy_wait(b'.');
    }

    // ── K-07b 批3: EEVDF 运行期观测导出 ─────────────────────────────────
    // 先睡 200ms 再醒来, 使本进程在 EEVDF 运行队列留下一次真实的睡眠→唤醒
    // 等待样本 (唤醒入队记时间戳, 被拣出时计入等待时延); busy 子进程同时
    // 占住次核, 使逐核账目与线程组 lag 汇总都有可观测的可运行任务.
    // 快照只读调度账目, 输出走内核串口 (procfs 未接 VFS), 返回值 = 上报
    // 的在线 CPU 数.
    delay_ms(200);
    let snap_cpus = sched_snapshot();
    print("[SCHED-USER] snapshot cpus=");
    print_dec(snap_cpus);
    print_char(b'\n');

    // P6a SLAAC 联调: 用户态 IPv6 UDP 收发探针 (仅 x86_64 — 联调在 x86_64
    // tap + 宿主 dnsmasq RA 下进行; aarch64 无对应 NIC/RA 环境, 不探).
    // 探针占用并释放一个 smoltcp fd; smoltcp FD 位图全局共享, 若与 echo
    // 子进程 accept 并发会扰动其 fd 取值 (FD 回收断言失去确定性), 故须在
    // echo fork **之前**同步完成.
    #[cfg(target_arch = "x86_64")]
    ipv6_udp_probe();
    // P4 D9 端到端验证: UDP 连接态探针 (connect/getpeername/getsockname, 纯本地
    // 状态确定性强). 同 ipv6 探针约束 —— 须在 echo fork 前同步完成并释放 fd.
    #[cfg(target_arch = "x86_64")]
    udp_connect_probe();
    // D11 端到端验证: IPv4 组播成员管理 + 组播报文接收 (仅 x86_64). 依赖宿主
    // 注入器 (scripts/qemu_mcast_test.sh) 持续送帧; 无注入时有界重试后 FAIL 并继续.
    // 同 ipv6 探针约束 —— 须在 echo fork 前同步完成并释放 fd.
    #[cfg(target_arch = "x86_64")]
    mcast_probe();
    // P4 D9 recvfrom 活体对端回填腿: 经已连接 UDP 实际收发 + 校验 src 回填.
    // 依赖宿主回显服务 (qemu_boot_test.sh e2e 阶段起), 无应答者时有界重试后 FAIL
    // 并继续 (与 ipv6_udp_probe 同款容错). 须在 echo fork 前同步完成并释放 fd.
    #[cfg(target_arch = "x86_64")]
    udp_echo_probe();
    // P3 D6 端到端验证: 常驻 TCP echo 服务端 (双架构). 由
    // scripts/qemu_boot_test.sh 的端到端阶段经 hostfwd 入站连接驱动
    // (accept -> recv -> send 回显), 验证 D6 "先建后换" 交接语义.
    // aarch64 腿自 P-32 放行 (数据面已随 P-31 翻绿, 旧门禁的"无 NIC 环境"
    // 前提失效), 由档位2 软观测腿 (hostfwd tcp::8081) 驱动.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let echo = fork();
        if echo == 0 {
            tcp_echo_server();
        }
    }
    busy_wait(b'+');
}

/// IPC 管道端到端探针 (回归锁: 管道已纳入 VFS fd 对象模型).
///
/// 覆盖 `pipe()` 创建 → `write` 写端 → `read` 读端 → `close` 两端全链路
/// (`functions/fs/pipe_fd.rs` + `functions/fs/io.rs` 的 VFS fd 路由).
fn pipe_probe() {
    let mut fds = [-1i32; 2];
    let rc = pipe_create(&mut fds);
    if rc != 0 || fds[0] < 0 || fds[1] < 0 || fds[0] == fds[1] {
        print("[pipe] FAIL: pipe() rc=");
        print_dec(i64::from(rc));
        print(" fds=");
        print_dec(i64::from(fds[0]));
        print_char(b'/');
        print_dec(i64::from(fds[1]));
        print_char(b'\n');
        return;
    }

    let payload = b"edgine-pipe";
    let wrote = fs_write(fds[1], payload);
    let mut buf = [0u8; 16];
    let read = fs_read(fds[0], &mut buf);
    // 先关读端再关写端 (顺序无关; 两端都关后槽位才回收)
    let close_r = fs_close(fds[0]);
    let close_w = fs_close(fds[1]);

    let ok = wrote == i32::try_from(payload.len()).expect("探针载荷长度超出 i32")
        && read == wrote
        && buf[..payload.len()] == *payload
        && close_r == 0
        && close_w == 0;
    if ok {
        print("[pipe] CREATE+RW ok (rfd=");
        print_dec(i64::from(fds[0]));
        print(", wfd=");
        print_dec(i64::from(fds[1]));
        println(")");
    } else {
        print("[pipe] FAIL: write=");
        print_dec(i64::from(wrote));
        print(" read=");
        print_dec(i64::from(read));
        print(" close=");
        print_dec(i64::from(close_r));
        print_char(b'/');
        print_dec(i64::from(close_w));
        print_char(b'\n');
    }
}

// ============================================================
// K-00: 用户态 TLS 基址探针
// ============================================================

/// TLS 探针 magic A (父进程) / B (子进程): 值本身无意义, 只用于区分
/// `FS`/`TPIDR_EL0` 当前指向哪一个静态块.
const TLS_MAGIC_A: u64 = 0x544C_5341_0000_0001;
const TLS_MAGIC_B: u64 = 0x544C_5342_0000_0002;

/// TLS 数据块 A — 父进程基址.
///
/// 用 `AtomicU64` 而非可变静态: 块内容恒定, 只需一段 8 字节对齐的可读用户内存;
/// 原子类型同时避免 `static mut` 的引用未定义行为.
static TLS_BLOCK_A: AtomicU64 = AtomicU64::new(TLS_MAGIC_A);
/// TLS 数据块 B — 子进程基址.
static TLS_BLOCK_B: AtomicU64 = AtomicU64::new(TLS_MAGIC_B);

/// 父子交替让出 CPU 并校验 TLS 的轮数.
const TLS_SWITCH_ROUNDS: u32 = 64;

fn tls_addr(block: &AtomicU64) -> u64 {
    core::ptr::from_ref(block) as u64
}

fn tls_set(addr: u64) -> bool {
    arch_prctl(ARCH_SET_FS, addr) == 0
}

fn tls_get() -> Option<u64> {
    let mut base = 0u64;
    if arch_prctl(ARCH_GET_FS, core::ptr::from_mut(&mut base) as u64) == 0 {
        Some(base)
    } else {
        None
    }
}

/// 经硬件 TLS 基址读取偏移 0 的 8 字节 (x86_64: `fs:[0]`).
#[cfg(target_arch = "x86_64")]
fn tls_read_word() -> u64 {
    let word: u64;
    // SAFETY: FS 基址由 arch_prctl 指向本进程的探针静态块 (8 字节对齐),
    // 读取偏移 0 的 8 字节不越界; 不使用栈.
    unsafe {
        core::arch::asm!("mov {}, qword ptr fs:[0]", out(reg) word, options(nostack, preserves_flags));
    }
    word
}

/// 经硬件 TLS 基址读取偏移 0 的 8 字节 (aarch64: 先取 `TPIDR_EL0` 再解引用).
#[cfg(target_arch = "aarch64")]
fn tls_read_word() -> u64 {
    let base: u64;
    // SAFETY: `mrs` 只读 TPIDR_EL0 (EL0 可读), 不访问内存.
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el0", out(reg) base, options(nomem, nostack));
    }
    // SAFETY: base 由 arch_prctl 指向本进程 8 字节对齐的探针静态块, 只读一次.
    unsafe { core::ptr::read_volatile(base as *const u64) }
}

/// 用户态 TLS 基址随上下文切换恢复探针 (K-00).
///
/// 覆盖三个运行时判据:
/// 1. `arch_prctl(ARCH_SET_FS)` 立即生效 —— 硬件视图 (`fs:[0]` / `TPIDR_EL0`
///    解引用) 与内核字段回读 (`ARCH_GET_FS`) 同步指向新基址;
/// 2. `fork` 子进程继承父进程 TLS 基址;
/// 3. 父子进程各持不同基址并交替 `sched_yield`, 每次重新被调度后读回的仍是
///    自己的 magic —— 切换漏恢复即读到对方的块 (或基址 0).
///
/// `execve` 清零路径不在本探针内: 默认启动环境只加载内嵌 init 一个 ELF
/// (未启用 initramfs), 无法构造成功的 `execve` 目标; 该路径由 host 侧源码
/// 契约测试 (`host-tests/tests/tls_base_restore_test.rs`) 锁定.
fn tls_probe() {
    let addr_a = tls_addr(&TLS_BLOCK_A);
    let addr_b = tls_addr(&TLS_BLOCK_B);
    if !tls_set(addr_a) || tls_read_word() != TLS_MAGIC_A || tls_get() != Some(addr_a) {
        println("[TLS] FAIL: arch_prctl(ARCH_SET_FS) 未立即生效");
        return;
    }
    println("[TLS] SET/GET ok (hardware base effective immediately)");

    let child = fork();
    if child == 0 {
        tls_child_probe(addr_a, addr_b);
    }
    let Ok(child_pid) = i32::try_from(child) else {
        println("[TLS] FAIL: fork 返回值超出 i32");
        return;
    };

    let mut parent_ok = true;
    for _ in 0..TLS_SWITCH_ROUNDS {
        sched_yield();
        if tls_read_word() != TLS_MAGIC_A || tls_get() != Some(addr_a) {
            parent_ok = false;
            break;
        }
    }
    let mut status = 0i32;
    let reaped = wait_pid_status(child_pid, &mut status);
    let child_ok = reaped == i64::from(child_pid) && status == 0;
    if parent_ok && child_ok {
        println("[TLS] PASS: fork inherit + cross-process switch isolation");
    } else {
        print("[TLS] FAIL: parent_ok=");
        print_dec(i64::from(parent_ok));
        print(" reaped=");
        print_dec(reaped);
        print(" status=");
        print_dec(i64::from(status));
        print_char(b'\n');
    }
}

/// 子进程侧: 校验 fork 继承 → 切换到自己的基址 → 与父进程交替后仍稳定.
fn tls_child_probe(addr_a: u64, addr_b: u64) -> ! {
    if tls_read_word() != TLS_MAGIC_A || tls_get() != Some(addr_a) {
        println("[TLS] FAIL: fork 子进程未继承父进程 TLS");
        proc_exit(2);
    }
    if !tls_set(addr_b) || tls_read_word() != TLS_MAGIC_B || tls_get() != Some(addr_b) {
        println("[TLS] FAIL: 子进程切换 TLS 基址未生效");
        proc_exit(3);
    }
    for _ in 0..TLS_SWITCH_ROUNDS {
        sched_yield();
        if tls_read_word() != TLS_MAGIC_B || tls_get() != Some(addr_b) {
            println("[TLS] FAIL: 子进程 TLS 在上下文切换后被覆盖");
            proc_exit(4);
        }
    }
    println("[TLS] child B base stable across context switches");
    proc_exit(0);
}

// ============================================================
// K-07a: 线程组运行时探针 (clone / 每线程 TLS / futex / join / exit_group / tgkill)
// ============================================================

/// 探针创建的线程数.
const THREAD_COUNT: usize = 3;
/// 每线程用户栈字节数 (16 字节对齐满足 `clone` 的对齐校验).
const THREAD_STACK_BYTES: usize = 4096;
/// 线程探针整条腿的等待时间预算 (毫秒).
///
/// 限界按 `CLOCK_MONOTONIC` 墙钟时间计, 不按轮次计: 轮次与真实耗时在宿主超订
/// 下脱钩 (同一计数在空载与满载下跨越的时间差几个数量级), 会把"宿主调度慢"
/// 误判为判据失败. 正常路径的等待是毫秒级, 预算只作兜底, 且必须小于
/// `qemu_boot_test.sh` 的单次启动超时 (25s), 保证失败时 `[THREAD] FAIL` 能落地.
const THREAD_WAIT_BUDGET_MS: u64 = 5_000;
/// `FUTEX_WAIT_BITSET` 到期时内核的返回码 (`-ETIMEDOUT`, Linux errno 110).
const FUTEX_ETIMEDOUT: i64 = -110;
/// 每线程 TLS magic 基址 —— 第 `i` 个线程读到 `BASE + i` 才算 `CLONE_SETTLS` 生效.
const THREAD_TLS_MAGIC_BASE: u64 = 0x7130_0000_0000_0000;
/// `exit_group` 腿的组退出码.
const GROUP_EXIT_CODE: i32 = 7;
/// `wait4` 写回的 `wstatus` (`exit_code << 8`, 无信号位).
const GROUP_EXIT_WSTATUS: i32 = GROUP_EXIT_CODE << 8;

/// 线程自检位: TLS 基址命中自己的块.
const PROOF_TLS: u32 = 1;
/// 线程自检位: 睡在 `THREAD_RELEASE` 上被父线程唤醒.
const PROOF_WAKE: u32 = 2;

/// 线程 `clone` 标志: D4 决策的完整 posix 线程语义组合.
///
/// 内核以 `tls != 0` 判定 `CLONE_SETTLS` (K-00 记录的立场), 故本位与 `tls` 实参
/// 同时给出; `CLONE_PARENT_SETTID` / `CLONE_CHILD_SETTID` / `CLONE_CHILD_CLEARTID`
/// 让 TID 回填与 join 路径都在真机上一次走完.
const THREAD_CLONE_FLAGS: u64 = CLONE_VM
    | CLONE_THREAD
    | CLONE_SIGHAND
    | CLONE_FILES
    | CLONE_FS
    | CLONE_SETTLS
    | CLONE_PARENT_SETTID
    | CLONE_CHILD_SETTID
    | CLONE_CHILD_CLEARTID;

/// 线程用户栈 (整块落 `.bss`, 由 ELF 装载按 `p_memsz` 整页置零).
///
/// 必须带内部可变性: 无 `UnsafeCell` 的不可变 `static` 会被 rustc 归入 `.rodata`,
/// 而 aarch64 的 `.rodata` 段不带 `PF_W` ⇒ 装载成只读页, 子线程首次压栈即触发
/// level-3 权限错误 (x86_64 的链接脚本把 `.rodata` 并入可写 `PT_LOAD`, 故同形代码
/// 在两架构表现不一致 —— 判据以两架构都真跑为准).
#[repr(C, align(16))]
struct ThreadStack(UnsafeCell<[u8; THREAD_STACK_BYTES]>);

// SAFETY: 每个 `ThreadStack` 实例只服务一条线程 (下标即线程号, helper 栈独立成块),
// 内核保证不同时调度两条共用同一栈的线程; `Sync` 仅为让整块可放进 `static`
// —— 共享的只是地址, 写入方始终是该栈所属的那条线程.
unsafe impl Sync for ThreadStack {}

/// 每线程独立用户栈.
static THREAD_STACKS: [ThreadStack; THREAD_COUNT] =
    [const { ThreadStack(UnsafeCell::new([0u8; THREAD_STACK_BYTES])) }; THREAD_COUNT];
/// 每线程 TLS 块 (偏移 0 放 magic), 父线程在 clone 前写入 `BASE + idx`.
static THREAD_TLS: [AtomicU64; THREAD_COUNT] = [const { AtomicU64::new(0) }; THREAD_COUNT];
/// 每线程经硬件 TLS 基址读回的首字 (失败时定位读到了谁的块).
static THREAD_TLS_SEEN: [AtomicU64; THREAD_COUNT] = [const { AtomicU64::new(0) }; THREAD_COUNT];
/// `CLONE_CHILD_SETTID` 回填地址; 线程退出时内核写 0 并 futex 唤醒 (join 依据).
static THREAD_TID: [AtomicU32; THREAD_COUNT] = [const { AtomicU32::new(0) }; THREAD_COUNT];
/// `CLONE_PARENT_SETTID` 回填地址.
static THREAD_PTID: [AtomicI32; THREAD_COUNT] = [const { AtomicI32::new(0) }; THREAD_COUNT];
/// 线程自报 `gettid`.
static THREAD_SELF_TID: [AtomicI64; THREAD_COUNT] = [const { AtomicI64::new(0) }; THREAD_COUNT];
/// 线程自报 `getpid` (应恒等于组长 tgid).
static THREAD_SELF_PID: [AtomicU64; THREAD_COUNT] = [const { AtomicU64::new(0) }; THREAD_COUNT];
/// 线程自检位图 (join 之后由父线程读取).
static THREAD_PROOF: [AtomicU32; THREAD_COUNT] = [const { AtomicU32::new(0) }; THREAD_COUNT];
/// 已成功创建的线程数 (仅父线程写, 失败路径据此放行并 join 已建线程).
static THREAD_SPAWNED: AtomicU32 = AtomicU32::new(0);
/// 报到计数: 线程递增并唤醒父线程 (线程 → 父方向的 futex 往返).
static THREAD_PROGRESS: AtomicU32 = AtomicU32::new(0);
/// 放行标志: 父线程置 1 并唤醒全部线程 (父 → 线程方向).
static THREAD_RELEASE: AtomicU32 = AtomicU32::new(0);

/// `exit_group` 腿的 helper 线程栈 (子进程私有副本).
static GROUP_HELPER_STACK: ThreadStack =
    const { ThreadStack(UnsafeCell::new([0u8; THREAD_STACK_BYTES])) };
/// helper 的 `CLONE_CHILD_SETTID` / `CLONE_PARENT_SETTID` 槽.
static GROUP_HELPER_TID: AtomicU32 = AtomicU32::new(0);
static GROUP_HELPER_PTID: AtomicI32 = AtomicI32::new(0);
/// helper 的上核报到标志.
static GROUP_HELPER_READY: AtomicU32 = AtomicU32::new(0);
/// helper 的沉睡字: 值恒为 0, 只能被组退出路径 (SIGKILL + unblock) 打断.
static GROUP_HOLD: AtomicU32 = AtomicU32::new(0);

/// 线程组运行时探针 (K-07a) 的判据汇总入口.
///
/// 两条腿串成六个运行时判据: 线程腿覆盖 `clone` 建线程与 TID 回填、每线程独立
/// TLS、`gettid`/`getpid` 身份、`tgkill`/`kill` 的 `sig == 0` 存在性探测、futex
/// 双向往返与 `CLONE_CHILD_CLEARTID` join; 组退出腿覆盖 `exit_group` 终止整个
/// 线程组 (含睡在 futex 上的兄弟线程) 后由父进程按 `code << 8` 收割.
///
/// 任一腿失败即打印 `[THREAD] FAIL: <判据>` 并继续启动 (不 `proc_exit`),
/// 使单点失败不掩盖其余探针读数.
fn thread_probe() {
    // 两条腿共用同一个绝对截止时刻: 探针总等待时间有界, 不随判据条数累加.
    let Some(deadline) = wait_deadline() else {
        // 单调时钟不可用时无法建立时间限界: 如实判失败, 不伪装成"等到超时".
        thread_fail("clock", -1);
        return;
    };
    match thread_leg(deadline) {
        Ok(tgid) => match group_exit_leg(deadline) {
            Ok(()) => {
                print("[THREAD] PASS: clone+per-thread TLS+tid/tgid+futex+join+exit_group (tgid=");
                print_dec(tgid);
                print_char(b')');
                print_char(b'\n');
            }
            Err((tag, code)) => thread_fail(tag, code),
        },
        Err((tag, code)) => thread_fail(tag, code),
    }
}

/// 失败出口: `tag` 定位判据, `code` 携带该判据的实测读数 (TID / 位图 / 退出码).
///
/// 整行在栈缓冲内拼好后只发一次 `fs_write`: 控制台写的错误返回值被 `print`
/// 忽略, 分段打印时被瞬时失败吞掉的恰好可能是 tag 那段, 留下
/// `[THREAD] FAIL:  code=0` 这类无从判因的证据; 单次写入使失败行要么完整落地,
/// 要么整行缺失 (后者由启动断言缺 `[THREAD] PASS` 暴露).
fn thread_fail(tag: &'static str, code: i64) {
    let mut buf = [0u8; 96];
    let mut pos = 0usize;
    append_bytes(&mut buf, &mut pos, b"[THREAD] FAIL: ");
    append_bytes(&mut buf, &mut pos, tag.as_bytes());
    append_bytes(&mut buf, &mut pos, b" code=");
    append_dec(&mut buf, &mut pos, code);
    append_bytes(&mut buf, &mut pos, b"\n");
    let _ = fs_write(1, &buf[..pos]);
}

/// 截断式追加字节: 失败行远小于缓冲容量, 正常路径不触发截断.
fn append_bytes(buf: &mut [u8], pos: &mut usize, src: &[u8]) {
    let n = src.len().min(buf.len() - *pos);
    buf[*pos..*pos + n].copy_from_slice(&src[..n]);
    *pos += n;
}

/// 十进制追加 `i64` (负数带 `-` 前缀), 供单次写入的失败行拼装.
fn append_dec(buf: &mut [u8], pos: &mut usize, val: i64) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut v = val.unsigned_abs();
    loop {
        i -= 1;
        digits[i] = b'0' + u8::try_from(v % 10).expect("末位十进制数字必在 u8 范围");
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if val < 0 {
        append_bytes(buf, pos, b"-");
    }
    append_bytes(buf, pos, &digits[i..]);
}

/// 线程腿: 建 N 线程 → 校验身份 / TLS / 信号探测 → 放行 → join.
///
/// 单腿判据:
/// 1. `clone(CLONE_THREAD|...)` 建线程并回填 `CLONE_PARENT_SETTID` /
///    `CLONE_CHILD_SETTID`;
/// 2. 每线程独立 TLS (`CLONE_SETTLS` → `fs:[0]` / `TPIDR_EL0` 读到自己的 magic);
/// 3. 线程身份: `gettid` 逐线程唯一且不等于 tgid, `getpid` 恒等于组长 tgid;
/// 4. `tgkill`/`kill` 的 `sig == 0` 存在性探测 (活 TID 返 0, 越界 TID 返负);
/// 5. futex 双向往返 (线程唤醒父线程 + 父线程唤醒全部线程) 与
///    `CLONE_CHILD_CLEARTID` join (退出写 0 + 唤醒).
///
/// 组退出判据 (`exit_group` 终止整组) 由 [`group_exit_leg`] 承担. 全部等待以
/// 传入的绝对截止时刻为时间上界: 失败只打印 FAIL, 绝不挂死启动.
fn thread_leg(deadline: u64) -> Result<i64, (&'static str, i64)> {
    let tgid = getpid();
    let Ok(tgid_i64) = i64::try_from(tgid) else {
        return Err(("tgid-range", -1));
    };
    // D2: 组长线程的数值 TID 即 tgid.
    let leader_tid = gettid();
    if leader_tid != tgid_i64 || leader_tid <= 0 {
        return Err(("leader-tid-ne-tgid", leader_tid));
    }
    // 单线程期写入每线程 TLS 块内容 (clone 之后无人改写).
    for (idx, blk) in THREAD_TLS.iter().enumerate() {
        blk.store(
            THREAD_TLS_MAGIC_BASE.wrapping_add(idx as u64),
            Ordering::Release,
        );
    }

    let checked = thread_spawn_checks(tgid, tgid_i64, leader_tid, deadline);
    // 已创建的线程睡在 THREAD_RELEASE 上: 不放行会一直阻塞到 init 退出, 掩盖后续
    // 探针的读数, 故无论判据是否通过都执行放行 + join.
    let joined = thread_release_and_join(deadline);
    // join 之后才可读线程自检位图: 线程退出前最后一个动作是落盘 proof.
    // 按位与聚合 —— 任一线程缺任一判据即与期望掩码不符.
    let mut proof_seen = PROOF_TLS | PROOF_WAKE;
    for slot in &THREAD_PROOF {
        proof_seen &= slot.load(Ordering::Acquire);
    }
    checked?;
    joined?;
    if proof_seen != PROOF_TLS | PROOF_WAKE {
        return Err(("thread-proof", i64::from(proof_seen)));
    }
    Ok(tgid_i64)
}

/// 建线程 + 线程侧判据校验 (TID 回填、报到、身份、TLS、信号探测).
///
/// `deadline` 是整条腿共用的绝对截止时刻 (单调时钟毫秒).
fn thread_spawn_checks(
    tgid: u64,
    tgid_i64: i64,
    leader_tid: i64,
    deadline: u64,
) -> Result<(), (&'static str, i64)> {
    let mut tids = [0i64; THREAD_COUNT];
    for (idx, stack) in THREAD_STACKS.iter().enumerate() {
        let ptid = i32_slot_ptr(&THREAD_PTID[idx]);
        let ctid = u32_slot_ptr(&THREAD_TID[idx]);
        let rc = unsafe {
            // SAFETY: 栈顶取自本进程 BSS 的 16 字节对齐静态块 (内核按 16 字节校验);
            // 两个 TID 指针指向本进程私有原子槽 (与 `u32`/`i32` 布局一致);
            // TLS 基址指向本进程 `.data` 的 8 字节块; entry/arg 经被调用者保存
            // 寄存器偷渡, 子分支在汇编块内直接跳入 trampoline.
            clone_thread(
                THREAD_CLONE_FLAGS,
                thread_stack_top(stack),
                ptid,
                ctid,
                core::ptr::from_ref(&THREAD_TLS[idx]) as u64,
                thread_body,
                idx as u64,
            )
        };
        if rc <= 0 {
            return Err(("clone", rc));
        }
        // CLONE_PARENT_SETTID 由内核在父上下文同步回填: clone 返回即必须可见,
        // 这条判据不依赖新线程是否已拿到 CPU. (CLONE_CHILD_SETTID 按子侧语义
        // 在报到握手之后校验, 见下方.)
        let parent_tid = i64::from(THREAD_PTID[idx].load(Ordering::Acquire));
        if parent_tid != rc {
            return Err(("settid", parent_tid));
        }
        tids[idx] = rc;
        let created = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        THREAD_SPAWNED.store(created, Ordering::Release);
    }

    // 全部线程报到 (父线程被子线程的 futex_wake 唤醒 —— 线程 → 父方向往返).
    let want = u32::try_from(THREAD_COUNT).unwrap_or(u32::MAX);
    if !futex_wait_until(&THREAD_PROGRESS, |v| v >= want, deadline) {
        return Err((
            "progress",
            i64::from(THREAD_PROGRESS.load(Ordering::Acquire)),
        ));
    }

    // 线程侧上报: CHILD_SETTID 回填、gettid 与 clone 返回值一致、getpid 恒等于
    // tgid、TLS 读到自己的块.
    //
    // CHILD_SETTID 按 Linux 语义由子侧在返回用户态前落盘, 故判据放在子线程已
    // 上核报到之后 —— 父线程的即时读取不再参与这条判据, 两项 SETTID 各成独立
    // 判据 (任一失败可凭 tag 区分是父侧回填还是子侧回填).
    for (idx, &tid) in tids.iter().enumerate() {
        let child_tid = i64::from(THREAD_TID[idx].load(Ordering::Acquire));
        if child_tid != tid {
            return Err(("child-settid", child_tid));
        }
        if THREAD_SELF_TID[idx].load(Ordering::Acquire) != tid {
            return Err(("gettid-mismatch", tid));
        }
        if THREAD_SELF_PID[idx].load(Ordering::Acquire) != tgid {
            return Err(("getpid-not-tgid", tid));
        }
        let expect = THREAD_TLS_MAGIC_BASE.wrapping_add(idx as u64);
        let seen = THREAD_TLS_SEEN[idx].load(Ordering::Acquire);
        if seen != expect {
            return Err(("tls-cross-thread", i64::try_from(seen).unwrap_or(i64::MAX)));
        }
        // TID 逐线程唯一且不占用组长 tgid.
        if tid <= 0 || tid == tgid_i64 || tid == leader_tid {
            return Err(("tid-is-tgid", tid));
        }
        for &other in &tids[idx + 1..] {
            if other == tid {
                return Err(("tid-duplicated", tid));
            }
        }
    }

    signal_probe(tgid_i64, &tids, leader_tid)
}

/// 放行全部线程并逐个 join (`CLONE_CHILD_CLEARTID` 写 0 + futex 唤醒).
fn thread_release_and_join(deadline: u64) -> Result<(), (&'static str, i64)> {
    let spawned = usize::try_from(THREAD_SPAWNED.load(Ordering::Acquire)).unwrap_or(0);
    THREAD_RELEASE.store(1, Ordering::Release);
    let _ = futex_wake(&THREAD_RELEASE, u32::try_from(spawned).unwrap_or(u32::MAX));
    for slot in THREAD_TID.iter().take(spawned) {
        if !futex_wait_until(slot, |v| v == 0, deadline) {
            return Err(("join-timeout", i64::from(slot.load(Ordering::Acquire))));
        }
    }
    Ok(())
}

/// `sig == 0` 存在性探测: 组内活 TID 全部返 0, 越界 TID 返负, `kill(tgid, 0)` 返 0.
fn signal_probe(tgid_i64: i64, tids: &[i64], leader_tid: i64) -> Result<(), (&'static str, i64)> {
    let Ok(tgid) = i32::try_from(tgid_i64) else {
        return Err(("tgid-range", tgid_i64));
    };
    let Ok(leader_i32) = i32::try_from(leader_tid) else {
        return Err(("tid-range", leader_tid));
    };
    if tgkill(tgid, leader_i32, 0) != 0 {
        return Err(("tgkill-leader", -1));
    }
    for &tid in tids {
        let Ok(tid_i32) = i32::try_from(tid) else {
            return Err(("tid-range", tid));
        };
        if tgkill(tgid, tid_i32, 0) != 0 {
            return Err(("tgkill-thread", tid));
        }
    }
    // 越界 TID: 进程表必不命中, 返 `-ESRCH`.
    if tgkill(tgid, i32::MAX, 0) >= 0 {
        return Err(("tgkill-bogus", i64::from(i32::MAX)));
    }
    // 组定向 `kill(tgid, 0)`: 按 tgid 定位线程组, 存在性探测返 0.
    let rc = i64::from(kill(tgid, 0));
    if rc != 0 {
        return Err(("kill-tgid", rc));
    }
    Ok(())
}

/// 单调时钟毫秒读数; `clock_gettime(CLOCK_MONOTONIC)` 失败时返回 `None`.
fn monotonic_ms() -> Option<u64> {
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if clock_gettime(CLOCK_MONOTONIC, &mut ts) != 0 {
        return None;
    }
    let sec = u64::try_from(ts.tv_sec).ok()?;
    let nsec = u64::try_from(ts.tv_nsec).ok()?;
    Some(sec.saturating_mul(1_000).saturating_add(nsec / 1_000_000))
}

/// 以当前单调时刻为起点建立等待截止时刻; 时钟不可用时返回 `None`.
fn wait_deadline() -> Option<u64> {
    monotonic_ms().map(|now| now.saturating_add(THREAD_WAIT_BUDGET_MS))
}

/// 有界等待原子字满足判据: 每轮先按当前值入睡, 值已变则内核返 `-EAGAIN`
/// 即刻重判 —— 覆盖"唤醒发生在读取与入睡之间"的竞态, 不空转也不挂死.
///
/// 上界是绝对截止时刻而非轮次计数: 轮次与真实耗时在宿主超订下脱钩, 同一计数
/// 在空载与满载下跨越的时间差几个数量级. 截止时刻与宿主调度速度无关 —— 宿主
/// 再慢也只是等待变长, 不会让判据提前失效; 时钟读不到时按失败处理 (无法建立
/// 时间上界, 不伪装成"等到超时").
///
/// 入睡用 `FUTEX_WAIT_BITSET` 把同一截止时刻交给内核: 对端线程若中途消失且
/// 不再唤醒, 内核仍在截止时刻兜底唤醒本线程. 无超时的 `FUTEX_WAIT` 不够 ——
/// 本线程会永久睡在等待字上, 用户态截止检查根本没有机会执行.
fn futex_wait_until(word: &AtomicU32, done: impl Fn(u32) -> bool, deadline: u64) -> bool {
    loop {
        if done(word.load(Ordering::Acquire)) {
            return true;
        }
        if monotonic_ms().is_none_or(|now| now >= deadline) {
            return false;
        }
        let cur = word.load(Ordering::Acquire);
        if done(cur) {
            return true;
        }
        let ts = deadline_timespec(deadline);
        if futex_wait_bitset(word, cur, &ts) == FUTEX_ETIMEDOUT {
            // 内核确认截止时刻已过: 做最后一次值判定后按超时收尾, 不再入睡.
            return done(word.load(Ordering::Acquire));
        }
    }
}

/// 截止毫秒 → `CLOCK_MONOTONIC` 绝对 `Timespec` (与 `monotonic_ms` 同一时基).
fn deadline_timespec(deadline_ms: u64) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(deadline_ms / 1_000).unwrap_or(i64::MAX),
        tv_nsec: i64::try_from((deadline_ms % 1_000).saturating_mul(1_000_000)).unwrap_or(0),
    }
}

/// 第 `idx` 个线程的栈顶 (栈向低地址生长, 顶地址即 16 字节对齐的块尾).
fn thread_stack_top(stack: &ThreadStack) -> u64 {
    let base = core::ptr::from_ref(stack).cast::<u8>() as u64;
    base.wrapping_add(THREAD_STACK_BYTES as u64)
}

/// 取原子 `u32` 槽的裸指针 (`AtomicU32` 与 `u32` 布局一致, 交内核直写).
fn u32_slot_ptr(cell: &AtomicU32) -> *mut u32 {
    core::ptr::from_ref(cell).cast_mut().cast::<u32>()
}

/// 取原子 `i32` 槽的裸指针 (同上).
fn i32_slot_ptr(cell: &AtomicI32) -> *mut i32 {
    core::ptr::from_ref(cell).cast_mut().cast::<i32>()
}

/// 线程体: 自检 TLS → 上报 tid/pid → 报到并唤醒父线程 → 有界等待放行.
///
/// 全程不打印: 线程共享父进程的 fd 表, 并发写会交错, 结论只落原子槽由父线程汇总.
/// 返回即落入 `thread_trampoline` 的 `proc_exit(0)` —— 只结束本线程, 不动整组.
extern "C" fn thread_body(raw_idx: u64) {
    let Some(idx) = usize::try_from(raw_idx).ok().filter(|i| *i < THREAD_COUNT) else {
        return;
    };
    // 1) CLONE_SETTLS 生效: 硬件 TLS 基址指向**本线程**的块.
    let seen = tls_read_word();
    THREAD_TLS_SEEN[idx].store(seen, Ordering::Release);
    let expect = THREAD_TLS_MAGIC_BASE.wrapping_add(raw_idx);
    let mut proof = if seen == expect { PROOF_TLS } else { 0 };
    // 2) 身份上报.
    THREAD_SELF_TID[idx].store(gettid(), Ordering::Release);
    THREAD_SELF_PID[idx].store(getpid(), Ordering::Release);
    // 3) 报到 + 唤醒等待本计数的父线程.
    THREAD_PROGRESS.fetch_add(1, Ordering::AcqRel);
    let _ = futex_wake(&THREAD_PROGRESS, 1);
    // 4) 有界等待父线程放行 (父 → 线程方向的 futex 往返).
    //
    // 子线程自建截止时刻: 与父线程同一个单调时钟, 但起点是本线程拿到 CPU 的
    // 时刻 —— 宿主调度慢只会推迟起点, 不会缩短本线程的等待预算.
    if wait_deadline().is_some_and(|d| futex_wait_until(&THREAD_RELEASE, |v| v != 0, d)) {
        proof |= PROOF_WAKE;
    }
    THREAD_PROOF[idx].store(proof, Ordering::Release);
}

/// `exit_group` 腿: fork 子进程 → 子进程起一个睡死在 futex 上的 helper →
/// 子进程组长 `exit_group(7)` → 父进程按 `7 << 8` 收割整个线程组.
///
/// 收割按 `WNOHANG` + 1ms 睡眠轮询到整条探针的截止时刻为止: 组退出本身是
/// 内核路径, 阻塞式 `wait4` 在组内线程异常滞留时会把启动挂死.
fn group_exit_leg(deadline: u64) -> Result<(), (&'static str, i64)> {
    let child = fork();
    if child == 0 {
        group_child_body(deadline);
    }
    let Ok(child_pid) = i32::try_from(child) else {
        return Err(("fork-range", -1));
    };
    let mut status = 0i32;
    loop {
        let reaped = wait_pid_status_nohang(child_pid, &mut status);
        if reaped == i64::from(child_pid) {
            break;
        }
        if reaped != 0 {
            return Err(("reap", reaped));
        }
        // 时钟读不到 ≠ 截止到期: 分开报因, 否则失败证据只剩 code=0,
        // 无法区分"等待超时"与"时间限界失效".
        let Some(now) = monotonic_ms() else {
            return Err(("clock-lost", -1));
        };
        if now >= deadline {
            return Err(("reap-timeout", 0));
        }
        let pause = Timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000,
        };
        let _ = nanosleep(&pause);
    }
    if status != GROUP_EXIT_WSTATUS {
        return Err(("group-exit-status", i64::from(status)));
    }
    Ok(())
}

/// 组退出腿的子进程侧 (不返回).
fn group_child_body(deadline: u64) -> ! {
    let rc = unsafe {
        // SAFETY: helper 栈是子进程私有 BSS 块 (fork 已复制地址空间), 16 字节对齐;
        // 两个 TID 指针指向本进程私有原子槽; `tls = 0` 表示不设新基址 (内核按
        // `tls != 0` 判定 CLONE_SETTLS, 故本线程继承组长的 TLS 基址).
        clone_thread(
            THREAD_CLONE_FLAGS,
            thread_stack_top(&GROUP_HELPER_STACK),
            i32_slot_ptr(&GROUP_HELPER_PTID),
            u32_slot_ptr(&GROUP_HELPER_TID),
            0,
            group_helper_body,
            0,
        )
    };
    if rc <= 0 {
        proc_exit(2);
    }
    // 等 helper 真正上核报到 (有界), 确保 exit_group 面对的是一个活着的兄弟线程.
    // 报到超时即如实以专属退出码退出: 父进程会在 `group-exit-status` 读到它,
    // 不必伪装成"组退出失败"而掩盖握手超时本身.
    if !futex_wait_until(&GROUP_HELPER_READY, |v| v != 0, deadline) {
        proc_exit(3);
    }
    exit_group(GROUP_EXIT_CODE);
}

/// 组退出腿的 helper 线程体: 报到后睡在 `GROUP_HOLD` 上 —— 该字永不变为 1,
/// 唯一能把本线程搬回运行队列的是 `exit_group` 的组退出路径 (置 SIGKILL + unblock).
extern "C" fn group_helper_body(_arg: u64) {
    GROUP_HELPER_READY.store(1, Ordering::Release);
    let _ = futex_wake(&GROUP_HELPER_READY, 1);
    // 无超时: 本线程唯一合法的唤醒源是组退出路径; 若在此限时返回, 组退出判据
    // 就失去"退出时仍有兄弟线程睡在 futex 上"的前提.
    loop {
        let cur = GROUP_HOLD.load(Ordering::Acquire);
        let _ = futex_wait(&GROUP_HOLD, cur);
    }
}

// ============================================================
// A-10: 返回用户态信号投递探针 (svc 出口 + IRQ 出口)
// ============================================================

/// 探针信号: `SIGTERM` (默认动作 Term, 装 handler 后可控).
const SIG_PROBE_SIGNUM: i32 = 15;
/// IRQ 腿自旋线程的兜底轮数: 信号应在毫秒级到达, 该上界只防回归时挂死.
const SIG_SPIN_LIMIT: u64 = 400_000_000;

/// handler 命中计数 (svc 腿 1 次 + IRQ 腿 1 次, 只增).
static SIG_HITS: AtomicU32 = AtomicU32::new(0);
/// IRQ 腿线程已进入纯用户自旋 (不发起任何 syscall).
static SIG_SPIN_READY: AtomicU32 = AtomicU32::new(0);
/// IRQ 腿线程已观察到 handler 命中并退出自旋.
static SIG_SPIN_DONE: AtomicU32 = AtomicU32::new(0);
/// 自旋进度读数 (每 4096 圈刷新): `spin-done` 超时用它区分「线程没上核」与
/// 「线程在跑但信号没投递」。
static SIG_SPIN_TICKS: AtomicU64 = AtomicU64::new(0);
/// IRQ 腿线程栈 (落 `.bss`, 必须有内部可变性, 理由见 `ThreadStack`).
static SIG_THREAD_STACK: ThreadStack =
    const { ThreadStack(UnsafeCell::new([0u8; THREAD_STACK_BYTES])) };
/// `CLONE_CHILD_SETTID` / `CLONE_CHILD_CLEARTID` 槽 (join 依据).
static SIG_THREAD_TID: AtomicU32 = AtomicU32::new(0);
/// `CLONE_PARENT_SETTID` 槽.
static SIG_THREAD_PTID: AtomicI32 = AtomicI32::new(0);

/// 信号 handler: 只做计数, 返回后经 trampoline (`rt_sigreturn`) 回到原指令.
///
/// 两架构的 handler 入口约定由内核 `UserFrame::enter_handler` 收口:
/// 首参数 = signum (`rdi` / `x0`), 返回地址分别走栈槽 / `x30` (LR).
extern "C" fn sig_probe_handler(_signum: i32) {
    SIG_HITS.fetch_add(1, Ordering::AcqRel);
}

/// A-10 运行时探针: 两腿串成两条返回用户态路径的投递判据.
///
/// - **svc 腿**: 自身定向 `tgkill(SIGTERM)` —— 信号必须在 tgkill 的系统调用
///   返回用户态前投递, handler 返回经 `rt_sigreturn` 恢复, 调用点继续执行;
/// - **IRQ 腿**: 起一条纯用户自旋的兄弟线程 (全程不发起 syscall), 父线程定向
///   `tgkill` 该 TID —— 唯一投递窗口是定时器中断的 EL0 出口.
///
/// 失败打印单行 `[SIG] FAIL: <tag> code=<读数>` 并继续启动; 两腿全过才打印
/// `[SIG] PASS`, 供 QEMU fail-closed 断言.
fn sig_probe() {
    let Some(deadline) = wait_deadline() else {
        sig_fail("clock", -1);
        return;
    };
    match sig_probe_impl(deadline) {
        Ok(()) => println("[SIG] PASS: svc-return + irq-exit handler delivery"),
        Err((tag, code)) => sig_fail(tag, code),
    }
}

/// 探针主体; 两腿共用同一条绝对截止时刻 (毫秒).
fn sig_probe_impl(deadline: u64) -> Result<(), (&'static str, i64)> {
    let tgid = getpid();
    let leader = gettid();
    let (Ok(tgid_i32), Ok(leader_i32)) = (i32::try_from(tgid), i32::try_from(leader)) else {
        return Err(("id-range", -1));
    };
    // 装 handler: 内核 ABI 的 act 是单值 u64 (handler 地址);
    // 经裸指针中转, 避免函数项直接转整数 (clippy `fn_to_numeric_cast`).
    let rc = rt_sigaction(SIG_PROBE_SIGNUM, sig_probe_handler as *const () as u64);
    if rc != 0 {
        return Err(("sigaction", rc));
    }

    // ── 腿1: svc 出口投递 ──
    // 信号在 tgkill 返回用户态前被投递; handler 返回后 `rt_sigreturn` 恢复
    // 现场, 本线程从 tgkill 的返回点继续 —— 返回码仍是 0.
    let rc = tgkill(tgid_i32, leader_i32, SIG_PROBE_SIGNUM);
    if rc != 0 {
        return Err(("tgkill-svc", rc));
    }
    let hits = SIG_HITS.load(Ordering::Acquire);
    if hits != 1 {
        return Err(("svc-hits", i64::from(hits)));
    }

    // ── 腿2: IRQ 出口投递 ──
    let child = unsafe {
        // SAFETY: 栈取自本进程 BSS 的 16 字节对齐静态块; TID 指针指向本进程
        // 私有原子槽; `tls = 0` 表示不设新基址 (内核按 `tls != 0` 判定).
        clone_thread(
            THREAD_CLONE_FLAGS,
            thread_stack_top(&SIG_THREAD_STACK),
            i32_slot_ptr(&SIG_THREAD_PTID),
            u32_slot_ptr(&SIG_THREAD_TID),
            0,
            sig_irq_body,
            0,
        )
    };
    if child <= 0 {
        return Err(("clone", child));
    }
    let Ok(spin_tid) = i32::try_from(child) else {
        return Err(("tid-range", child));
    };
    // 等自旋线程上核: 该线程不发起任何 syscall, 只能由父线程轮询.
    loop {
        if SIG_SPIN_READY.load(Ordering::Acquire) != 0 {
            break;
        }
        if monotonic_ms().is_none_or(|now| now >= deadline) {
            return Err(("spin-ready", -1));
        }
        delay_ms(1);
    }
    let ticks_at_send = SIG_SPIN_TICKS.load(Ordering::Acquire);
    let rc = tgkill(tgid_i32, spin_tid, SIG_PROBE_SIGNUM);
    if rc != 0 {
        return Err(("tgkill-irq", rc));
    }
    // 等目标线程观察到命中并退出自旋 (证明 handler 在中断出口被投递). 用 futex
    // 阻塞而不是轮询: 父线程忙轮询会与自旋目标抢同一颗核, 使「线程没上核」与
    // 「信号没投递」两种失效分不开 (K-07a 的 futex 往返已证明阻塞等待可用).
    if !futex_wait_until(&SIG_SPIN_DONE, |v| v != 0, deadline) {
        // 区分两种「handler 没跑」: 自旋计数在动 ⇒ 线程在核上但信号没投递;
        // 计数冻结 ⇒ 线程根本没再上核 (调度侧饿死), 投递点无从执行.
        let tag = if SIG_SPIN_TICKS.load(Ordering::Acquire) > ticks_at_send {
            "spin-live"
        } else {
            "spin-starved"
        };
        return Err((tag, i64::from(SIG_HITS.load(Ordering::Acquire))));
    }
    let hits = SIG_HITS.load(Ordering::Acquire);
    if hits != 2 {
        return Err(("irq-hits", i64::from(hits)));
    }
    // join: `CLONE_CHILD_CLEARTID` 在退出时清零 TID 槽并唤醒.
    if !futex_wait_until(&SIG_THREAD_TID, |v| v == 0, deadline) {
        return Err((
            "join-timeout",
            i64::from(SIG_THREAD_TID.load(Ordering::Acquire)),
        ));
    }
    Ok(())
}

/// IRQ 腿线程体: 纯用户自旋 (不发起 syscall), handler 命中后退出并落 DONE.
extern "C" fn sig_irq_body(_arg: u64) {
    SIG_SPIN_READY.store(1, Ordering::Release);
    let mut i: u64 = 0;
    while SIG_HITS.load(Ordering::Acquire) < 2 && i < SIG_SPIN_LIMIT {
        i = i.wrapping_add(1);
        if i.is_multiple_of(4_096) {
            SIG_SPIN_TICKS.store(i, Ordering::Release);
        }
        core::hint::spin_loop();
    }
    SIG_SPIN_DONE.store(1, Ordering::Release);
    let _ = futex_wake(&SIG_SPIN_DONE, 1);
}

/// 失败出口: 单次写入整行 (理由同 [`thread_fail`]).
fn sig_fail(tag: &'static str, code: i64) {
    let mut buf = [0u8; 96];
    let mut pos = 0usize;
    append_bytes(&mut buf, &mut pos, b"[SIG] FAIL: ");
    append_bytes(&mut buf, &mut pos, tag.as_bytes());
    append_bytes(&mut buf, &mut pos, b" code=");
    append_dec(&mut buf, &mut pos, code);
    append_bytes(&mut buf, &mut pos, b"\n");
    let _ = fs_write(1, &buf[..pos]);
}

// ============================================================
// K-07b 批2: execve 运行时探针 (envp 传递 + FsContext cwd/root 解析)
// ============================================================

/// `cwd` 腿标记: 子进程先 `chdir("/bin")`, 再以**相对路径** `exectest` 执行.
const EXEC_LEG_CWD: &[u8] = b"cwd";
/// `root` 腿标记: 子进程先 `chroot("/bin")`, 再以**视图绝对路径** `/exectest` 执行.
const EXEC_LEG_ROOT: &[u8] = b"root";

/// execve 运行时探针: 两条腿各自 fork 子进程完成视图切换 + exec, 父进程按
/// `wstatus == 0` 判定. 载荷 `exectest` 在自身进程内校验 argc/argv、envp 与
/// cwd (判据写侧见 `src/user/exectest/src/main.rs`).
///
/// - `cwd` 腿: 只有相对路径按进程 `FsContext.cwd` 解析成 `/bin/exectest` 才成功;
/// - `root` 腿: 只有视图绝对路径按进程 `FsContext.root` 前缀解析成 `/bin/exectest`
///   才成功.
///
/// 任一腿失败打印 `[EXEC] FAIL` 并继续启动 (与网络探针同口径), 两条腿全过才
/// 打印 `[EXEC] PASS`.
///
/// A-14: 两腿的靶子 `/bin/exectest` 只来自 initramfs cpio, 默认启动的根是空
/// ramfs ⇒ 无载体时按设计不可能成功. 故先自检载体, 无载体打印 `[EXEC] SKIP:
/// no carrier` 并跳过两腿 —— 否则三行「预期失败」与真回归在日志里同形.
fn exec_probe() {
    if !carrier_present() {
        println("[EXEC] SKIP: no carrier");
        return;
    }
    let cwd_ok = exec_leg(EXEC_LEG_CWD);
    let root_ok = exec_leg(EXEC_LEG_ROOT);
    if cwd_ok && root_ok {
        println("[EXEC] PASS: cwd+root argv+envp");
    } else {
        print("[EXEC] FAIL: cwd=");
        print_dec(i64::from(cwd_ok));
        print(" root=");
        print_dec(i64::from(root_ok));
        print_char(b'\n');
    }
}

/// 载体自检: 两腿最终解析到的都是真实路径 `/bin/exectest`, 该文件在不在即可
/// 判定载体 (`feature = "initramfs"` 解包) 是否就位. 用 `open` 而非 `stat` ——
/// 用户态没有 stat 包装, 且 `open` 走的正是两腿要验证的那套路径解析.
///
/// SIMPLIFIED: 只判能否打开, 不校验常规文件位与 x 位; 影响面: 载体存在但权限
/// 坏时本函数判「有载体」, 两腿照样打印 `[EXEC] FAIL` (失败信号仍会暴露);
/// 何时需扩展: 需要区分 ENOENT 与 EACCES 两类载体故障时给用户态补 stat 包装.
fn carrier_present() -> bool {
    let fd = fs_open(b"/bin/exectest\0", O_RDONLY, 0);
    if fd < 0 {
        return false;
    }
    let _ = fs_close(fd);
    true
}

/// 单腿: fork → 子进程切换 FS 视图并 exec → 父进程收割按 `wstatus` 判定.
fn exec_leg(leg: &'static [u8]) -> bool {
    let child = fork();
    if child == 0 {
        exec_leg_child(leg);
    }
    let Ok(pid) = i32::try_from(child) else {
        return false;
    };
    let mut status = 0i32;
    let reaped = wait_pid_status(pid, &mut status);
    reaped == i64::from(pid) && status == 0
}

/// 子进程侧: 按腿切换视图 → `execve(exectest, argv, envp)`; exec 失败不返回.
fn exec_leg_child(leg: &'static [u8]) -> ! {
    let view_rc = if leg == EXEC_LEG_CWD {
        env_chdir(b"/bin\0")
    } else {
        env_chroot(b"/bin\0")
    };
    if view_rc != 0 {
        // 失败必须把腿名与返回码带出来: 父进程只看到 wstatus, 静默退出码无法
        // 区分"路径不存在 / 缺 CAP_SYS_ADMIN / 参数非法".
        print("[EXEC] FAIL: view ");
        print(core::str::from_utf8(leg).unwrap_or("?"));
        print(" rc=");
        print_dec(i64::from(view_rc));
        print_char(b'\n');
        proc_exit(2);
    }
    // cwd 腿用相对名 (由 cwd 解析), root 腿用视图绝对路径 (由 root 前缀解析).
    let (leg_cstr, path): (&core::ffi::CStr, &[u8]) = if leg == EXEC_LEG_CWD {
        (c"cwd", b"exectest\0")
    } else {
        (c"root", b"/exectest\0")
    };
    let argv: [*const u8; 4] = [
        c"exectest".as_ptr().cast::<u8>(),
        leg_cstr.as_ptr().cast::<u8>(),
        c"K07B".as_ptr().cast::<u8>(),
        core::ptr::null(),
    ];
    let envp: [*const u8; 3] = [
        c"K07B_ENV=1".as_ptr().cast::<u8>(),
        c"K07B_BATCH=2".as_ptr().cast::<u8>(),
        core::ptr::null(),
    ];
    let exec_rc = proc_exec(path, &argv, &envp);
    // exec 成功不返回; 返回即失败, 带上 errno 便于区分 ENOENT / EACCES.
    print("[EXEC] FAIL: exec ");
    print(core::str::from_utf8(leg).unwrap_or("?"));
    print(" rc=");
    print_dec(exec_rc);
    print_char(b'\n');
    proc_exit(3);
}

// ============================================================
// #108 丙: aarch64 缺页 / COW 恢复路径真链探针
// ============================================================
//
// 只在 aarch64 编译: 本探针验的正是这条链 —— EL0 的写保护/未映射故障经
// `sync_exception_handler` → `handle_user_page_fault` 改正页表与帧持有计数 →
// `eret` 重试**同一条**指令. x86_64 侧同语义由 `idt::handlers` 那条链与既有
// QEMU 断言覆盖, 不在此重复.
//
// 三腿各打一条互不重叠的分支, 任一腿红即可定位缺口:
// - `copy` : fork 把父子两侧 leaf 同时置只读且帧持有计数 = 2 ⇒ 子侧首次写入走
//            **分配 + 复制 + 旧帧递减**那条分支. 子侧写后回读必须读到新值 (证明
//            重试执行的就是那条写指令 —— 返回时若跳过一条指令, 回读仍是旧值);
//            父侧回读必须保持旧值 (证明真分了新帧, 而非父子继续共用).
// - `reuse`: 子进程被收割后其页表已拆除 ⇒ 计数回到 1, 父侧再写走**就地恢复可写**
//            那条分支 (§8.1: 计数 <= 1 不复制). 与 `copy` 互补, 两分支各自钉住.
// - `stack`: 递归下降到装载期预映射的 64KB 栈之外 ⇒ **未映射页的翻译故障** ⇒
//            匿名按需分页 / 栈增长分支. 这条腿与权限故障那条覆盖 `is_demand_pageable`
//            的两个类 (Translation 与 Permission), 缺一类即另一类无从验证.
//
// 失败处置口径: 缺页未恢复时故障进程被内核终止 (EL0 同步异常 → terminate, 退出码
// = pid), 故每腿的风险动作都放进**子进程**, "被杀"落在子进程而非 init, 父进程只看
// 退出码 —— 启动日志因此仍能把后续探针跑完, 红机不掩盖其它判据.

/// 探针靶格的三个取值: 父预写 / 子写 / 子收割后父再写.
#[cfg(target_arch = "aarch64")]
const COW_MAGIC_PARENT: u64 = 0xC0C0_A800_0000_0001;
#[cfg(target_arch = "aarch64")]
const COW_MAGIC_CHILD: u64 = 0xC0C0_A800_0000_0002;
#[cfg(target_arch = "aarch64")]
const COW_MAGIC_LATE: u64 = 0xC0C0_A800_0000_0003;
/// 深栈腿每帧字节数与下降层数: 二者之积必须严格大于装载期预映射的用户栈
/// (`config::memory::USER_STACK_SIZE` = 64KB) 才碰得到未映射页. 取 400 × 512
/// = 200KB. 单帧小于页, 故不引入编译器栈探针 (probestack) 依赖.
#[cfg(target_arch = "aarch64")]
const COW_DESCEND_FRAME_BYTES: usize = 512;
#[cfg(target_arch = "aarch64")]
const COW_DESCEND_STEPS: u32 = 400;
/// 深栈腿写入的标记字节 (任意非零值, 只判写后读回一致).
#[cfg(target_arch = "aarch64")]
const COW_STACK_MARK: u8 = 0x5A;
/// 语义不符时子进程的退出码. 与"被内核终止"的退出码 (pid) 同为非零 ⇒ 判定都是
/// 红, 此处只区分诊断粒度, 不参与通过/失败判据.
#[cfg(target_arch = "aarch64")]
const COW_EXIT_MISMATCH: i32 = 11;
#[cfg(target_arch = "aarch64")]
const COW_EXIT_STACK: i32 = 12;

/// aarch64 缺页/COW 恢复路径探针入口: 三腿全过才打 PASS.
#[cfg(target_arch = "aarch64")]
fn cow_probe() {
    let (copy_ok, reuse_ok) = cow_fault_legs();
    let stack_ok = cow_stack_leg();
    if copy_ok && reuse_ok && stack_ok {
        println("[COW] PASS: cow-copy+cow-reuse+stack-grow");
    } else {
        print("[COW] FAIL: copy=");
        print_dec(i64::from(copy_ok));
        print(" reuse=");
        print_dec(i64::from(reuse_ok));
        print(" stack=");
        print_dec(i64::from(stack_ok));
        print_char(b'\n');
    }
}

/// `copy` + `reuse` 两腿共用一个靶格: 它位于**已映射的可写用户栈页**内, 故 fork
/// 时必然被置只读并与子进程共享 (帧持有计数 2), 两侧的首次写入都只能靠缺页恢复.
///
/// 返回 `(copy_ok, reuse_ok)`.
#[cfg(target_arch = "aarch64")]
fn cow_fault_legs() -> (bool, bool) {
    // 预写: 让该页在 fork 前确定已映射且可写 (不依赖首触语义).
    let mut cell = COW_MAGIC_PARENT;
    let cell_ptr = core::ptr::from_mut(&mut cell);

    let child = fork();
    if child == 0 {
        cow_child_body(cell_ptr);
    }
    let Ok(pid) = i32::try_from(child) else {
        return (false, false);
    };

    // `copy` 腿: 子进程按 wstatus == 0 才算"写生效 + 重试到同一条指令".
    let mut status = 0i32;
    let child_reaped = wait_pid_status(pid, &mut status);
    let child_ok = child_reaped == i64::from(pid) && status == 0;
    // 父侧回读必须仍是预写值: 子侧写入若落在共用帧上 (只读位没置上 / 极性写反),
    // 这里就会读到 COW_MAGIC_CHILD.
    // SAFETY: cell_ptr 指向本帧内的 u64, 父子各自持副本; volatile 读写防止编译器
    // 把"写后读"折叠成常量 —— 折叠后本判据就不再证明那条写指令真的落了页.
    let parent_intact = unsafe { core::ptr::read_volatile(cell_ptr) } == COW_MAGIC_PARENT;
    let copy_ok = child_ok && parent_intact;

    // `reuse` 腿: 子进程页表已随收割拆除 ⇒ 本 leaf 的持有计数回到 1, 父侧再写
    // 走"就地恢复可写"分支 (不复制).
    // SAFETY: 同上, cell_ptr 指向本帧内有效 u64.
    unsafe { core::ptr::write_volatile(cell_ptr, COW_MAGIC_LATE) };
    let reuse_ok = unsafe { core::ptr::read_volatile(cell_ptr) } == COW_MAGIC_LATE;

    (copy_ok, reuse_ok)
}

/// 子进程侧: 读回父预写值 → 写入自己的值 → 立即回读 → 按结果定退出码.
///
/// 回读不等价 ⇒ 恢复后没重试到那条写指令 (或复制来的是错页); 首读不等价 ⇒ fork
/// 没把内容交接到子侧. 两种都记 `[COW] FAIL` 并以非零码退出, 由父侧的收割判据兜住.
#[cfg(target_arch = "aarch64")]
fn cow_child_body(cell_ptr: *mut u64) -> ! {
    // SAFETY: 本进程与父进程共用同一 VA 布局 (fork 语义), cell_ptr 是本帧内 u64
    // 的地址; 用 volatile 读写见 `cow_fault_legs` 的同一理由 (防值折叠).
    let seen = unsafe { core::ptr::read_volatile(cell_ptr) };
    unsafe { core::ptr::write_volatile(cell_ptr, COW_MAGIC_CHILD) };
    // SAFETY: 同上.
    let after = unsafe { core::ptr::read_volatile(cell_ptr) };
    let seen_ok = seen == COW_MAGIC_PARENT;
    let after_ok = after == COW_MAGIC_CHILD;
    if !(seen_ok && after_ok) {
        print("[COW] FAIL: child seen_ok=");
        print_dec(i64::from(seen_ok));
        print(" after_ok=");
        print_dec(i64::from(after_ok));
        print_char(b'\n');
        proc_exit(COW_EXIT_MISMATCH);
    }
    // 成功路径不打印: 本进程退出后父进程才汇总, 避免交叉行.
    proc_exit(0);
}

/// `stack` 腿: 深栈风险动作放进子进程 —— 未恢复的翻译故障会终止本进程.
#[cfg(target_arch = "aarch64")]
fn cow_stack_leg() -> bool {
    let child = fork();
    if child == 0 {
        if !descend_touch(COW_DESCEND_STEPS) {
            print("[COW] FAIL: stack not retained");
            print_char(b'\n');
            proc_exit(COW_EXIT_STACK);
        }
        proc_exit(0);
    }
    let Ok(pid) = i32::try_from(child) else {
        return false;
    };
    let mut status = 0i32;
    wait_pid_status(pid, &mut status) == i64::from(pid) && status == 0
}

/// 递归下降 `depth` 层 (每帧 [`COW_DESCEND_FRAME_BYTES`] 字节), 每层写并回读
/// 本帧末字节.
///
/// 前 [`COW_DESCEND_FRAME_BYTES`] × 若干层落在预映射栈内 (无故障), 越过 64KB 后
/// 每次触及新页都是**未映射页的写**: 恢复路径必须建页并重试那条写指令, 否则本层
/// 回读不等价. 回溯时再读一次, 判"扩展出的映射在深层返回前没有失效".
///
/// `#[inline(never)]` 是判据的一部分: 内联会把帧合流, 下降量不再由层数决定.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
fn descend_touch(depth: u32) -> bool {
    let mut pad = [core::mem::MaybeUninit::<u8>::uninit(); COW_DESCEND_FRAME_BYTES];
    let byte_ptr = core::ptr::from_mut(&mut pad[COW_DESCEND_FRAME_BYTES - 1]).cast::<u8>();
    // SAFETY: pad 是本帧内的栈数组, byte_ptr 指向其界内元素; 该字节由本函数先写
    // 后读, 不存在读取未初始化内容的路径. volatile 保证写真的落页 (否则编译器可
    // 把读折叠成常量), 也保证该页确实被访问 —— 未映射时这正是缺页的来源.
    unsafe { byte_ptr.write_volatile(COW_STACK_MARK) };
    let first_ok = unsafe { byte_ptr.read_volatile() } == COW_STACK_MARK;
    let deep_ok = depth == 0 || descend_touch(depth - 1);
    // SAFETY: 同上, byte_ptr 仍指向本帧 (未返回) 内元素.
    let retain_ok = unsafe { byte_ptr.read_volatile() } == COW_STACK_MARK;
    first_ok && deep_ok && retain_ok
}

/// 用户态 IPv6 UDP 收发探针 (P6a SLAAC 联调).
///
/// 端到端验证用户态 IPv6 socket 路径:
/// 1. `socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP)` 创建双栈 UDP socket;
/// 2. `bind` 到 `[::]:7777` (Linux 布局 `SockaddrIn6`, family 主机序 / 端口网络序);
/// 3. 向宿主回显服务 `fd00::1:9999` `sendto` 载荷, 有界重试至 SLAAC 全局地址就绪;
/// 4. `recv` 回显并校验内容一致.
///
/// 宿主侧回显服务由 `scripts/qemu_slaac_test.sh` 启动; 未就绪时 (如默认
/// slirp 无 RA) 有界重试后打印 FAIL 并继续, 不阻塞启动.
#[cfg(target_arch = "x86_64")]
fn ipv6_udp_probe() {
    // 本地绑定端口 / 宿主回显端口, 与 qemu_slaac_test.sh 约定一致.
    const LOCAL_PORT: u16 = 7777;
    const ECHO_PORT: u16 = 9999;
    // sockaddr_in6 定长布局: 2+2+4+16+4 = 28 字节.
    const SOCKADDR_IN6_LEN: u32 = 28;
    // 最大重试轮数 (每轮 50ms); SLAAC 地址由 RA 异步派生, 需留出等待窗口.
    const MAX_TRIES: u32 = 60;

    let fd = socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP);
    if fd < 0 {
        print("[net6] FAIL: socket()=");
        print_dec(i64::from(fd));
        print_char(b'\n');
        return;
    }
    // 有界重试轮询依赖 recv 立即返回 -EAGAIN: 显式置非阻塞. P5b 起 socket 默认
    // 阻塞, 无应答者时 recv 会永久挂起, 探针必须 O_NONBLOCK 才能收敛到 FAIL 后继续.
    let _ = fcntl(fd, F_SETFL, O_NONBLOCK as u64);

    // 本地 [::]:7777 — sin6_family 主机序 (NE), sin6_port 网络序 (BE).
    let local = SockaddrIn6 {
        sin6_family: u16::try_from(AF_INET6).expect("AF_INET6 超出 u16"),
        sin6_port: LOCAL_PORT.to_be(),
        sin6_flowinfo: 0,
        sin6_addr: In6Addr { s6_addr: [0u8; 16] },
        sin6_scope_id: 0,
    };
    let rc = bind(fd, (&raw const local).cast::<u8>(), SOCKADDR_IN6_LEN);
    if rc < 0 {
        print("[net6] FAIL: bind()=");
        print_dec(i64::from(rc));
        print_char(b'\n');
        let _ = close_socket(fd);
        return;
    }

    // 宿主回显端点 fd00::1:9999 (链路前缀由 RA 下发 fd00::/64).
    let mut remote = SockaddrIn6 {
        sin6_family: u16::try_from(AF_INET6).expect("AF_INET6 超出 u16"),
        sin6_port: ECHO_PORT.to_be(),
        sin6_flowinfo: 0,
        sin6_addr: In6Addr { s6_addr: [0u8; 16] },
        sin6_scope_id: 0,
    };
    remote.sin6_addr.s6_addr[0] = 0xfd;
    remote.sin6_addr.s6_addr[15] = 0x01;

    let payload = b"EDGINE6";
    let mut tries: u32 = 0;
    loop {
        let n = sendto(
            fd,
            payload.as_ptr(),
            payload.len(),
            0,
            (&raw const remote).cast::<u8>(),
            SOCKADDR_IN6_LEN,
        );
        if n == isize::try_from(payload.len()).expect("探针载荷长度超出 isize") {
            break;
        }
        tries += 1;
        if tries >= MAX_TRIES {
            println("[net6] FAIL: sendto() timeout (SLAAC 地址未就绪?)");
            let _ = close_socket(fd);
            return;
        }
        delay_ms(50);
    }
    println("[net6] TX ok: EDGINE6 sent to [fd00::1]:9999");

    // 等待宿主回显 (本探针以 recv() 收取, 不经 recvfrom 回写对端地址; D9 后
    // recvfrom 已回填 src, 其确定性由 udp_connect_probe 与内核契约测试覆盖).
    let mut buf = [0u8; 64];
    let mut recvs: u32 = 0;
    loop {
        let n = recv(fd, buf.as_mut_ptr(), buf.len(), 0);
        if n > 0 {
            let got = usize::try_from(n).expect("已确认 n > 0");
            if got == payload.len() && buf[..got] == payload[..] {
                println("[net6] RX ok: circulated EDGINE6 echo verified");
            } else {
                print("[net6] FAIL: echo mismatch len=");
                print_dec(n as i64);
                print_char(b'\n');
            }
            let _ = close_socket(fd);
            return;
        }
        recvs += 1;
        if recvs >= MAX_TRIES {
            print("[net6] FAIL: recv() timeout (无回显)");
            let _ = close_socket(fd);
            return;
        }
        delay_ms(50);
    }
}

/// 用户态 UDP 连接态探针 (P4 D9 端到端验证, 仅 x86_64).
///
/// 验证 D9 UDP 连接态语义 (纯本地状态, 无需对端应答, 故确定性强):
/// 1. `socket(AF_INET, SOCK_DGRAM)` 建 IPv4 UDP socket;
/// 2. `connect` 到 slirp 网关 `10.0.2.2:53` — 登记 remote 端点 + 自动 bind 临时端口;
/// 3. `getpeername` 回读对端 == 登记的 `10.0.2.2:53` (纯本地 remote 表);
/// 4. `getsockname` 回读本地端口非 0 (connect 触发临时端口分配).
///
/// 须在 echo 子进程 fork **之前**同步完成并释放 fd (smoltcp FD 位图全局共享).
#[cfg(target_arch = "x86_64")]
fn udp_connect_probe() {
    // sockaddr_in 定长布局: 2+2+4+8 = 16 字节.
    const SOCKADDR_IN_LEN: u32 = 16;

    let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if fd < 0 {
        print("[udp] FAIL: socket()=");
        print_dec(i64::from(fd));
        print_char(b'\n');
        return;
    }

    // slirp 网关 10.0.2.2:53 — sin_family 主机序 (NE), sin_port 网络序 (BE).
    let peer = SockaddrIn {
        sin_family: u16::try_from(AF_INET).expect("AF_INET 超出 u16"),
        sin_port: 53u16.to_be(),
        sin_addr: InAddr {
            s_addr: [10, 0, 2, 2],
        },
        sin_zero: [0u8; 8],
    };
    let rc = connect(fd, (&raw const peer).cast::<u8>(), SOCKADDR_IN_LEN);
    if rc != 0 {
        print("[udp] FAIL: connect()=");
        print_dec(i64::from(rc));
        print_char(b'\n');
        let _ = close_socket(fd);
        return;
    }
    println("[udp] CONNECT ok (remote registered)");

    // getpeername 回读对端, 校验与登记一致 (读 remote 端点表, 无网络依赖).
    let mut gp = SockaddrIn {
        sin_family: 0,
        sin_port: 0,
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    };
    let mut glen = SOCKADDR_IN_LEN;
    let rc = getpeername(fd, (&raw mut gp).cast::<u8>(), &raw mut glen);
    if rc == 0 && gp.sin_addr.s_addr == [10, 0, 2, 2] && gp.sin_port == 53u16.to_be() {
        println("[udp] PEERNAME ok (matches connect peer)");
    } else {
        print("[udp] FAIL: getpeername rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
    }

    // getsockname 回读本地临时端口 (connect 自动 bind).
    let mut gs = SockaddrIn {
        sin_family: 0,
        sin_port: 0,
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    };
    let mut slen = SOCKADDR_IN_LEN;
    let rc = getsockname(fd, (&raw mut gs).cast::<u8>(), &raw mut slen);
    if rc == 0 && gs.sin_port != 0 {
        print("[udp] SOCKNAME ok (ephemeral port=");
        print_dec(i64::from(u16::from_be(gs.sin_port)));
        println(")");
    } else {
        print("[udp] FAIL: getsockname rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
    }
    let _ = close_socket(fd);
}

/// 用户态组播成员管理探针 (P6 / D11 端到端验证, 仅 x86_64).
///
/// IPv4 腿一次跑通组播全链路 (per-socket 引用计数 → iface 组表 → IGMP 报告 →
/// L3 RX 过滤 → UDP 端口匹配投递 → 退组):
/// 1. `socket(AF_INET, SOCK_DGRAM)` → `bind` `0.0.0.0:7890` (组播 dst 免地址匹配,
///    只看端口);
/// 2. `setsockopt(IPPROTO_IP, IP_ADD_MEMBERSHIP, ip_mreq{239.255.42.42, ANY})` → 0,
///    内核据此请求 iface 入组并开始发 IGMP 报告;
/// 3. 重复加入同一 slot 同一组 → `-EADDRINUSE(98)` (per-socket 成员位图的 ABI 边界);
/// 4. 有界重试 `recv` 收取宿主注入的组播报文并校验载荷;
/// 5. `IP_DROP_MEMBERSHIP` → 0, 再退一次 → `-EADDRNOTAVAIL(99)` (非成员退组).
///
/// IPv6 腿 (`ff3e::…:42`) 只验 ABI: `IPPROTO_IPV6` level 命中 + level/optname
/// 不成对时返 `-ENOPROTOOPT(92)`; MLD 无宿主注帧路径, 故不收数据.
///
/// 拓扑前提: QEMU 走 `-netdev socket,udp=…` 非 slirp (无 DHCP/RA) → 内核落回
/// `FALLBACK_IPV4` 静态地址; 该路径不参与常规启动的里程碑断言.
///
/// 须在 echo 子进程 fork **之前** 同步完成并释放 fd (smoltcp FD 位图全局共享).
#[cfg(target_arch = "x86_64")]
fn mcast_probe() {
    // 两腿各自独立收口: IPv4 腿 (socket/bind/入组) 失败时不再跑 IPv6 腿,
    // 与拆分前的 `return` 语义一致.
    if mcast_probe_v4() {
        mcast_probe_v6();
    }
}

/// IPv4 组播腿 (总述见 `mcast_probe`): 返回 `false` 表示 socket/bind/入组失败.
#[cfg(target_arch = "x86_64")]
fn mcast_probe_v4() -> bool {
    // 组播接收端口与组地址, 与 scripts/qemu_mcast_test.sh 注入帧逐字一致.
    const MCAST_PORT: u16 = 7890;
    const MCAST_GROUP: [u8; 4] = [239, 255, 42, 42];
    const PAYLOAD: &[u8] = b"EDGEMCAST";
    // sockaddr_in 定长布局: 2+2+4+8 = 16 字节.
    const SOCKADDR_IN_LEN: u32 = 16;
    // Linux errno: 重复加入 EADDRINUSE(98) / 非成员退组 EADDRNOTAVAIL(99).
    const EADDRINUSE: i32 = 98;
    const EADDRNOTAVAIL: i32 = 99;
    // 有界重试轮数 (每轮 50ms, 共 1s 窗口): 注入端 (`scripts/qemu_mcast_test.sh`)
    // 每 250ms 送一帧, 1s 内已有 4 次命中机会, 足够验证入向组播投递.
    //
    // 不可随意放大 (实测 6s 窗口会让 `scripts/qemu_boot_test.sh` 的 TCP 回显 e2e
    // 从 12/12 通过退化为 8/13 失败): 本探针跑在 `tcp_echo_server` fork 之前, 窗口
    // 直接决定监听 socket 就绪时刻与后续入向连接能否被 `accept`. 抖动根因已定论:
    // EEVDF 时基背离 + 运行队列记账泄漏导致的秒级饥饿 (本窗口只是放大器), 详见
    // 详见 docs/plan/net-e2e-tcp-echo-flakiness.md.
    const MAX_TRIES: u32 = 20;

    let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if fd < 0 {
        print("[mcast] FAIL: socket()=");
        print_dec(i64::from(fd));
        print_char(b'\n');
        return false;
    }
    // 阻塞 recv 会挂死有界重试轮询 (P5b 起 socket 默认阻塞), 必须置非阻塞.
    let _ = fcntl(fd, F_SETFL, O_NONBLOCK as u64);

    // bind ANY:7890 — 不绑组地址 (POSIX 允许 ANY 收组播, smoltcp 对组播 dst 豁免地址匹配).
    let local = SockaddrIn {
        sin_family: u16::try_from(AF_INET).expect("AF_INET 超出 u16"),
        sin_port: MCAST_PORT.to_be(),
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    };
    if bind(fd, (&raw const local).cast::<u8>(), SOCKADDR_IN_LEN) < 0 {
        println("[mcast] FAIL: bind()");
        let _ = close_socket(fd);
        return false;
    }

    // ip_mreq: 组地址网络序 (裸 octets 本就是网络序), 接口 0.0.0.0 = 由内核选.
    let mreq = IpMreq {
        imr_multiaddr: InAddr {
            s_addr: MCAST_GROUP,
        },
        imr_interface: InAddr { s_addr: [0u8; 4] },
    };
    let mreq_ptr = (&raw const mreq).cast::<u8>();
    let mreq_len = u32::try_from(core::mem::size_of::<IpMreq>()).expect("结构体尺寸超出 u32");
    let rc = setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, mreq_ptr, mreq_len);
    if rc != 0 {
        print("[mcast] FAIL: ADD_MEMBERSHIP rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
        let _ = close_socket(fd);
        return false;
    }
    println("[mcast] JOIN ok (iface group table entry armed)");

    // 引用计数 ABI 边界: 同一 socket 重复加同一组 → EADDRINUSE, 不二次动 iface.
    let rc = setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, mreq_ptr, mreq_len);
    if rc == -EADDRINUSE {
        println("[mcast] DUP ok (EADDRINUSE)");
    } else {
        print("[mcast] FAIL: duplicate join rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
    }

    // 收取宿主注入的组播报文 (载荷逐字比对).
    let mut buf = [0u8; 64];
    let mut tries: u32 = 0;
    loop {
        let n = recv(fd, buf.as_mut_ptr(), buf.len(), 0);
        if n > 0 {
            let got = usize::try_from(n).expect("已确认 n > 0");
            if got == PAYLOAD.len() && buf[..got] == *PAYLOAD {
                println("[mcast] RX ok (multicast datagram delivered)");
            } else {
                print("[mcast] FAIL: payload mismatch len=");
                print_dec(n as i64);
                print_char(b'\n');
            }
            break;
        }
        tries += 1;
        if tries >= MAX_TRIES {
            println("[mcast] FAIL: recv() timeout (无注入帧?)");
            break;
        }
        delay_ms(50);
    }

    // 退组: 引用归零 → iface 拆组 + IGMP leave 报告.
    let rc = setsockopt(fd, IPPROTO_IP, IP_DROP_MEMBERSHIP, mreq_ptr, mreq_len);
    if rc == 0 {
        println("[mcast] DROP ok");
    } else {
        print("[mcast] FAIL: DROP_MEMBERSHIP rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
    }
    // 非成员再退 → EADDRNOTAVAIL (本 socket 位图已无该组, 不得误伤 iface 表).
    let rc = setsockopt(fd, IPPROTO_IP, IP_DROP_MEMBERSHIP, mreq_ptr, mreq_len);
    if rc == -EADDRNOTAVAIL {
        println("[mcast] NOENT ok (EADDRNOTAVAIL)");
    } else {
        print("[mcast] FAIL: second drop rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
    }
    let _ = close_socket(fd);

    true
}

/// IPv6 成员管理 ABI 腿 (总述见 `mcast_probe`).
///
/// IPv6 组播走 MLD 而非 IGMP, 宿主侧无对应注帧路径, 故本腿**不**验收发,
/// 只验 `IPPROTO_IPV6`(=41, IANA 永久分配) 这一 level 能否真实命中内核
/// 分支 —— 该值是纯 ABI, 取错 (曾误取 10) 会静默落 -ENOPROTOOPT(92),
/// 而 IPv4 腿完全不受影响, 无本腿则漂移无人发现.
#[cfg(target_arch = "x86_64")]
fn mcast_probe_v6() {
    const MCAST6_GROUP: [u8; 16] = [0xff, 0x3e, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x42];
    // Linux errno: 未知选项 ENOPROTOOPT(92).
    const ENOPROTOOPT: i32 = 92;

    let fd6 = socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP);
    if fd6 < 0 {
        print("[mcast] FAIL: socket(AF_INET6)=");
        print_dec(i64::from(fd6));
        print_char(b'\n');
        return;
    }
    let mreq6 = Ipv6Mreq {
        ipv6mr_multiaddr: In6Addr {
            s6_addr: MCAST6_GROUP,
        },
        ipv6mr_interface: 0,
    };
    let mreq6_ptr = (&raw const mreq6).cast::<u8>();
    let mreq6_len = u32::try_from(core::mem::size_of::<Ipv6Mreq>()).expect("结构体尺寸超出 u32");
    let rc = setsockopt(fd6, IPPROTO_IPV6, IPV6_ADD_MEMBERSHIP, mreq6_ptr, mreq6_len);
    if rc != 0 {
        print("[mcast] FAIL: IPV6_ADD_MEMBERSHIP rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
        let _ = close_socket(fd6);
        return;
    }
    println("[mcast] JOIN6 ok (level IPPROTO_IPV6=41 routed)");

    // level 与 optname 必须成对: IPv4 level + IPv6 optname → ENOPROTOOPT,
    // 不得误入 IPv6 分支 (误入会错读 20 字节 mreq 的前 4 字节当 IPv4 地址).
    let rc = setsockopt(fd6, IPPROTO_IP, IPV6_ADD_MEMBERSHIP, mreq6_ptr, mreq6_len);
    if rc == -ENOPROTOOPT {
        println("[mcast] LEVEL ok (ENOPROTOOPT)");
    } else {
        print("[mcast] FAIL: mismatched level rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
    }

    let rc = setsockopt(
        fd6,
        IPPROTO_IPV6,
        IPV6_DROP_MEMBERSHIP,
        mreq6_ptr,
        mreq6_len,
    );
    if rc == 0 {
        println("[mcast] DROP6 ok");
    } else {
        print("[mcast] FAIL: IPV6_DROP_MEMBERSHIP rc=");
        print_dec(i64::from(rc));
        print_char(b'\n');
    }
    let _ = close_socket(fd6);
}

/// 用户态 UDP 活体对端回填探针 (P4 D9 recvfrom 端到端验证, 仅 x86_64).
///
/// 补 `udp_connect_probe` 未覆盖的"活体收发腿": 经已连接 UDP socket 实际发出
/// 数据报并由宿主回显服务应答, 验证 `recvfrom` 回填真实对端地址 (syscall 层 U3
/// 透传 `src_ptr/src_len_ptr` + `sm_recvfrom` UDP 分支 `write_sockaddr`). 拓扑:
/// 1. `socket(AF_INET, SOCK_DGRAM)` → `connect` 到 slirp 网关 `10.0.2.2:9090`;
/// 2. `send` 载荷 (走 D9 登记的 remote 端点, 非 sendto);
/// 3. `recvfrom` 有界重试收取回显, 校验内容一致 + src 回填 == `10.0.2.2:9090`;
/// 4. 里程碑 `[udp] RECVFROM src ok` 仅在宿主回显服务应答时出现; 普通启动无
///    应答者, 有界重试后打印 FAIL 并继续 (与 `ipv6_udp_probe` 同款容错, 不断言 FAIL).
///
/// 须在 echo 子进程 fork **之前**同步完成并释放 fd (smoltcp FD 位图全局共享).
#[cfg(target_arch = "x86_64")]
fn udp_echo_probe() {
    // 宿主回显端口 (slirp 网关侧), 与 scripts/qemu_boot_test.sh e2e UDP 服务约定一致.
    const ECHO_PORT: u16 = 9090;
    // sockaddr_in 定长布局: 2+2+4+8 = 16 字节.
    const SOCKADDR_IN_LEN: u32 = 16;
    // 有界重试轮数 (每轮 50ms, 共 2s 窗口). 宿主服务先于 QEMU 起, 首轮即命中;
    // 无应答者 (普通启动) 时以最小代价收敛到 FAIL.
    const MAX_TRIES: u32 = 40;

    let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if fd < 0 {
        print("[udp] FAIL: echo socket()=");
        print_dec(i64::from(fd));
        print_char(b'\n');
        return;
    }
    // 同 ipv6_udp_probe: recvfrom 有界重试需非阻塞 (P5b 默认阻塞会使无应答时挂死).
    let _ = fcntl(fd, F_SETFL, O_NONBLOCK as u64);

    let peer = SockaddrIn {
        sin_family: u16::try_from(AF_INET).expect("AF_INET 超出 u16"),
        sin_port: ECHO_PORT.to_be(),
        sin_addr: InAddr {
            s_addr: [10, 0, 2, 2],
        },
        sin_zero: [0u8; 8],
    };
    if connect(fd, (&raw const peer).cast::<u8>(), SOCKADDR_IN_LEN) != 0 {
        println("[udp] FAIL: echo connect()");
        let _ = close_socket(fd);
        return;
    }

    // send 走 D9 登记的 remote 端点 (无显式 dest); UDP 非阻塞投递, 有界重试至链路就绪.
    let payload = b"EDGINE-UDP";
    let mut tries = 0u32;
    loop {
        let n = send(fd, payload.as_ptr(), payload.len(), 0);
        if n == isize::try_from(payload.len()).expect("探针载荷长度超出 isize") {
            break;
        }
        tries += 1;
        if tries >= MAX_TRIES {
            println("[udp] FAIL: echo send() timeout");
            let _ = close_socket(fd);
            return;
        }
        delay_ms(50);
    }

    // recvfrom 收取回显 + 回填对端地址 (非阻塞语义, 有界重试让出 CPU 供周期 poll).
    let mut buf = [0u8; 64];
    let mut src = SockaddrIn {
        sin_family: 0,
        sin_port: 0,
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    };
    let mut srclen = SOCKADDR_IN_LEN;
    let mut recvs = 0u32;
    loop {
        let n = recvfrom(
            fd,
            buf.as_mut_ptr(),
            buf.len(),
            0,
            (&raw mut src).cast::<u8>(),
            &raw mut srclen,
        );
        if n > 0 {
            let got = usize::try_from(n).expect("已确认 n > 0");
            if got == payload.len()
                && buf[..got] == payload[..]
                && src.sin_addr.s_addr == [10, 0, 2, 2]
                && src.sin_port == ECHO_PORT.to_be()
            {
                println("[udp] RECVFROM src ok (peer backfilled = 10.0.2.2:9090)");
            } else {
                print("[udp] FAIL: echo mismatch len=");
                print_dec(n as i64);
                print_char(b'\n');
            }
            let _ = close_socket(fd);
            return;
        }
        recvs += 1;
        if recvs >= MAX_TRIES {
            println("[udp] FAIL: echo recvfrom() timeout (no responder)");
            let _ = close_socket(fd);
            return;
        }
        delay_ms(50);
    }
}

/// 单条已建立连接的处理: NODELAY round-trip → poll 就绪位 → 一次性回显 →
/// `shutdown(SHUT_WR)` 半关 → `close` 归还 FD (总述见 `tcp_echo_server`).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn tcp_echo_conn(conn: i32, poll_ms: i64, max_recv_tries: u32) {
    print("[tcp] Connection accepted fd=");
    print_dec(i64::from(conn));
    print_char(b'\n');

    // D8: TCP_NODELAY setsockopt → getsockopt round-trip (关 Nagle → 读回 1).
    let nodelay: i32 = 1;
    let sret = setsockopt(
        conn,
        IPPROTO_TCP,
        TCP_NODELAY,
        (&raw const nodelay).cast::<u8>(),
        u32::try_from(core::mem::size_of::<i32>()).expect("结构体尺寸超出 u32"),
    );
    let mut got: i32 = 0;
    let mut got_len: u32 = u32::try_from(core::mem::size_of::<i32>()).expect("结构体尺寸超出 u32");
    let gret = getsockopt(
        conn,
        IPPROTO_TCP,
        TCP_NODELAY,
        (&raw mut got).cast::<u8>(),
        &raw mut got_len,
    );
    if sret == 0 && gret == 0 && got == 1 {
        println("[tcp] NODELAY roundtrip ok");
    } else {
        print("[tcp] FAIL: NODELAY set=");
        print_dec(i64::from(sret));
        print(" get=");
        print_dec(i64::from(gret));
        print(" val=");
        print_dec(i64::from(got));
        print_char(b'\n');
    }

    // D8b: 用户态 poll 取真实 socket 就绪位 (Smoltcp 路由 → sm_socket_poll).
    // 内核 poll 为单次扫描非阻塞, 故有界轮询直到 POLLIN 置位 (数据到达).
    let mut pfd = [PollFd {
        fd: conn,
        events: POLLIN,
        revents: 0,
    }];
    let mut poll_hits: u32 = 0;
    loop {
        let pr = poll(&mut pfd, 0);
        if pr > 0 && (pfd[0].revents & POLLIN) != 0 {
            break;
        }
        poll_hits += 1;
        if poll_hits >= max_recv_tries {
            break;
        }
        delay_ms(poll_ms);
    }
    print("[tcp] POLLIN revents=");
    print_dec(i64::from(pfd[0].revents));
    print_char(b'\n');

    // 一次性回显: recv 有界重试直至取到数据. `0` 表示"暂无数据"
    // (非对端关闭 —— TCP recv 无数据与 EOF 均返回 0), 让出 CPU 后重试.
    let mut buf = [0u8; 256];
    let mut tries: u32 = 0;
    loop {
        let n = recv(conn, buf.as_mut_ptr(), buf.len(), 0);
        if n > 0 {
            let len = usize::try_from(n).expect("已确认 n > 0");
            let sent = send(conn, buf.as_ptr(), len, 0);
            if sent == n {
                print("[tcp] Echo ok len=");
                print_dec(n as i64);
                print_char(b'\n');
            } else {
                print("[tcp] FAIL: send()=");
                print_dec(sent as i64);
                print_char(b'\n');
            }
            break;
        }
        if n == 0 {
            tries += 1;
            if tries >= max_recv_tries {
                println("[tcp] recv timeout (no data)");
                break;
            }
            delay_ms(poll_ms);
            continue;
        }
        // 其它负值 (如 -E_CONNRESET): 连接已关闭, 结束本连接.
        print("[tcp] FAIL: recv()=");
        print_dec(n as i64);
        print_char(b'\n');
        break;
    }
    // D7: shutdown(SHUT_WR) 半关 — 触发主动关闭 (发 FIN) 但保留 socket/FD,
    // 区别 close 的完全回收; 验证 shutdown ≠ close 语义 (syscall 返 0).
    let shut = shutdown(conn, SHUT_WR);
    if shut == 0 {
        println("[tcp] SHUTDOWN_WR ok");
    } else {
        print("[tcp] FAIL: shutdown()=");
        print_dec(i64::from(shut));
        print_char(b'\n');
    }
    let _ = close_socket(conn);
    println("[tcp] Connection closed");
}

/// 用户态 TCP echo 服务端 (P3 D6 端到端验证, 仅 x86_64).
///
/// 常驻子进程: 监听 `0.0.0.0:80`, 循环 `accept` 已建立连接并原样回显.
/// 用途是端到端验证 D6 "先建后换" 交接语义 ——
/// 1. 连续多次入站连接下**监听槽持续可用** (每次 accept 后监听 fd 仍可再 accept);
/// 2. 连接处理完 `close_socket` 后 FD 归还, 可被下一次 `accept` 复用 (FD 回收).
///
/// 驱动方为 `scripts/qemu_boot_test.sh` 的端到端阶段: x86_64 硬门禁腿以
/// `-netdev user,...,hostfwd=tcp::8080-:80` 启动 QEMU, 宿主客户端连
/// `localhost:8080`; aarch64 软观测腿用 `tcp::8081-:80` (P-32 放行, 接档位2
/// T1 观测). 均由 slirp 转发到 guest `:80` 触发本服务.
///
/// 语义要点: 内核 TCP `recv` 在**无数据**与**对端关闭(EOF)**两种情况都返回
/// `0` (smoltcp `recv_slice` 对已建立但 rx 缓冲为空的连接返回 `Ok(0)`),
/// 二者无法区分; 仅在连接已关闭时返回 `-E_CONNRESET`. 故每连接采取
/// **一次性回显**: `recv` 有界重试 (`0` 视为"暂无数据", 让出 CPU 供定时器
/// IRQ 推进 smoltcp 周期 poll) 直到取到数据 → `send` 原样回显 → `shutdown` +
/// `close` (尾包排空由内核 `sm_close` 的"close → 排空 egress → remove"三步序
/// 负责, 用户态不再预留发送窗口 —— S-4). 本函数永不返回 (常驻), 失败路径以
/// 忙等驻留 (不改变既有 APS-05/KPTI-09 断言).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn tcp_echo_server() -> ! {
    // 监听端口与脚本 hostfwd (`tcp::8080-:80`) 及 Makefile `QEMU_NET` 约定一致.
    const PORT: u16 = 80;
    // sockaddr_in 定长布局: 2+2+4+8 = 16 字节.
    const SOCKADDR_IN_LEN: u32 = 16;
    // accept 无就绪连接时的返回值 (EAGAIN, 非阻塞语义).
    const E_AGAIN: i32 = -11;
    // 重试节流间隔 (ms): 让出 CPU 供定时器 IRQ 推进网络 poll.
    const POLL_MS: i64 = 5;
    // 单连接等待数据的最大重试轮数 (每轮 POLL_MS); 超时则放弃该连接.
    const MAX_RECV_TRIES: u32 = 200;

    let fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        print("[tcp] FAIL: socket()=");
        print_dec(i64::from(fd));
        print_char(b'\n');
        busy_wait(b'!');
    }

    // 允许快速重绑 (TIME_WAIT 残留), 与 httpsrv 一致.
    let opt: i32 = 1;
    let _ = setsockopt(
        fd,
        SOL_SOCKET,
        SO_REUSEADDR,
        (&raw const opt).cast::<u8>(),
        u32::try_from(core::mem::size_of::<i32>()).expect("结构体尺寸超出 u32"),
    );

    // 通配绑定 0.0.0.0:80 — sin_family 主机序 (NE), sin_port 网络序 (BE).
    let local = SockaddrIn {
        sin_family: u16::try_from(AF_INET).expect("AF_INET 超出 u16"),
        sin_port: PORT.to_be(),
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    };
    let rc = bind(fd, (&raw const local).cast::<u8>(), SOCKADDR_IN_LEN);
    if rc < 0 {
        print("[tcp] FAIL: bind()=");
        print_dec(i64::from(rc));
        print_char(b'\n');
        busy_wait(b'!');
    }

    let rc = listen(fd, 5);
    if rc < 0 {
        print("[tcp] FAIL: listen()=");
        print_dec(i64::from(rc));
        print_char(b'\n');
        busy_wait(b'!');
    }
    println("[tcp] Listening on 0.0.0.0:80");

    // 常驻 accept 循环: D6 先建后换保证监听 fd 在每次交接后仍可继续 accept.
    loop {
        let conn = accept(fd, core::ptr::null_mut(), core::ptr::null_mut());
        if conn < 0 {
            if conn == E_AGAIN {
                // 尚无连接完成三次握手: 让出 CPU 后重试 (非阻塞语义).
                delay_ms(POLL_MS);
                continue;
            }
            print("[tcp] FAIL: accept()=");
            print_dec(i64::from(conn));
            print_char(b'\n');
            delay_ms(POLL_MS);
            continue;
        }
        tcp_echo_conn(conn, POLL_MS, MAX_RECV_TRIES);
    }
}

/// 毫秒级休眠 (探针重试节流 / echo 服务 poll 让渡): 以 `nanosleep` 让出 CPU,
/// 使周期网络 poll (timer IRQ) 得以推进 RA 处理与收发.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn delay_ms(ms: i64) {
    let ts = Timespec {
        tv_sec: 0,
        tv_nsec: ms * 1_000_000,
    };
    let _ = nanosleep(&ts);
}

/// 不 yield 的忙等 (APS-05): 仅低频发 syscall (打印 `mark`, 上限 8 次), 之后
/// 静默自增 —— 目的是让本核长期持有可运行用户任务并停留在 EL0.
fn busy_wait(mark: u8) -> ! {
    let mut i: u64 = 0;
    let mut printed: u32 = 0;
    loop {
        i = i.wrapping_add(1);
        if printed < 8 && i.is_multiple_of(4_000_000) {
            print_char(mark);
            printed += 1;
        }
    }
}
