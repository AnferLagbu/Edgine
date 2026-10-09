//! Edgine Init — fork / KPTI 隔离测试 (print_char)

#![no_std]
#![no_main]

use userlib::sys::*;
use userlib::*;

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

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print_char(b'X');
    print_char(b'\n');
    // 第一次 fork：子进程立即退出，父进程 wait4 收割 —— 收割路径 `remove_and_free`
    // 使 `Process::drop` 运行并销毁其地址空间 (走 destroy_page_table)，该批待释放帧
    // 必然滞留 pending 链（远程核尚未追平代），父进程继续执行
    let child1 = fork();
    if child1 == 0 {
        proc_exit(0);
    }
    wait_pid(child1 as i32);
    print_char(b'Y');
    print_char(b'\n');
    // 第二次 fork：子进程退出时再次结算 release_lock，出口排空上一次滞留的 pending 帧，
    // 帧真正归还 PMM，使延迟释放路径被完整执行
    let child2 = fork();
    if child2 == 0 {
        proc_exit(0);
    }
    wait_pid(child2 as i32);
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
    if wait_pid(probe as i32) != 0 {
        print("[KPTI] EL0 kernel high-half access denied (pid=");
        print_dec(probe as i64);
        println(")");
    } else {
        print("[KPTI] FAIL: EL0 kernel high-half access was NOT denied (pid=");
        print_dec(probe as i64);
        println(")");
    }

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
    // P3 D6 端到端验证: 常驻 TCP echo 服务端 (仅 x86_64). 由
    // scripts/qemu_boot_test.sh 的端到端阶段经 hostfwd 入站连接驱动
    // (accept -> recv -> send 回显), 验证 D6 "先建后换" 交接语义.
    #[cfg(target_arch = "x86_64")]
    {
        let echo = fork();
        if echo == 0 {
            tcp_echo_server();
        }
    }
    busy_wait(b'+');
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
        print_dec(fd as i64);
        print_char(b'\n');
        return;
    }
    // 有界重试轮询依赖 recv 立即返回 -EAGAIN: 显式置非阻塞. P5b 起 socket 默认
    // 阻塞, 无应答者时 recv 会永久挂起, 探针必须 O_NONBLOCK 才能收敛到 FAIL 后继续.
    let _ = fcntl(fd, F_SETFL, O_NONBLOCK as u64);

    // 本地 [::]:7777 — sin6_family 主机序 (NE), sin6_port 网络序 (BE).
    let local = SockaddrIn6 {
        sin6_family: AF_INET6 as u16,
        sin6_port: LOCAL_PORT.to_be(),
        sin6_flowinfo: 0,
        sin6_addr: In6Addr { s6_addr: [0u8; 16] },
        sin6_scope_id: 0,
    };
    let rc = bind(
        fd,
        &local as *const SockaddrIn6 as *const u8,
        SOCKADDR_IN6_LEN,
    );
    if rc < 0 {
        print("[net6] FAIL: bind()=");
        print_dec(rc as i64);
        print_char(b'\n');
        close_socket(fd);
        return;
    }

    // 宿主回显端点 fd00::1:9999 (链路前缀由 RA 下发 fd00::/64).
    let mut remote = SockaddrIn6 {
        sin6_family: AF_INET6 as u16,
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
            &remote as *const SockaddrIn6 as *const u8,
            SOCKADDR_IN6_LEN,
        );
        if n == payload.len() as isize {
            break;
        }
        tries += 1;
        if tries >= MAX_TRIES {
            println("[net6] FAIL: sendto() timeout (SLAAC 地址未就绪?)");
            close_socket(fd);
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
            let got = n as usize;
            if got == payload.len() && &buf[..got] == &payload[..] {
                println("[net6] RX ok: circulated EDGINE6 echo verified");
            } else {
                print("[net6] FAIL: echo mismatch len=");
                print_dec(n as i64);
                print_char(b'\n');
            }
            close_socket(fd);
            return;
        }
        recvs += 1;
        if recvs >= MAX_TRIES {
            print("[net6] FAIL: recv() timeout (无回显)");
            close_socket(fd);
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
        print_dec(fd as i64);
        print_char(b'\n');
        return;
    }

    // slirp 网关 10.0.2.2:53 — sin_family 主机序 (NE), sin_port 网络序 (BE).
    let peer = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: 53u16.to_be(),
        sin_addr: InAddr {
            s_addr: [10, 0, 2, 2],
        },
        sin_zero: [0u8; 8],
    };
    let rc = connect(fd, &peer as *const SockaddrIn as *const u8, SOCKADDR_IN_LEN);
    if rc != 0 {
        print("[udp] FAIL: connect()=");
        print_dec(rc as i64);
        print_char(b'\n');
        close_socket(fd);
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
    let rc = getpeername(fd, &mut gp as *mut SockaddrIn as *mut u8, &mut glen);
    if rc == 0 && gp.sin_addr.s_addr == [10, 0, 2, 2] && gp.sin_port == 53u16.to_be() {
        println("[udp] PEERNAME ok (matches connect peer)");
    } else {
        print("[udp] FAIL: getpeername rc=");
        print_dec(rc as i64);
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
    let rc = getsockname(fd, &mut gs as *mut SockaddrIn as *mut u8, &mut slen);
    if rc == 0 && gs.sin_port != 0 {
        print("[udp] SOCKNAME ok (ephemeral port=");
        print_dec(i64::from(u16::from_be(gs.sin_port)));
        println(")");
    } else {
        print("[udp] FAIL: getsockname rc=");
        print_dec(rc as i64);
        print_char(b'\n');
    }
    close_socket(fd);
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
    // 直接决定监听 socket 就绪时刻与后续入向连接能否被 `accept`. 根因未定, 详见
    // docs/plan/net-e2e-tcp-inbound-flakiness.md.
    const MAX_TRIES: u32 = 20;

    let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if fd < 0 {
        print("[mcast] FAIL: socket()=");
        print_dec(fd as i64);
        print_char(b'\n');
        return;
    }
    // 阻塞 recv 会挂死有界重试轮询 (P5b 起 socket 默认阻塞), 必须置非阻塞.
    let _ = fcntl(fd, F_SETFL, O_NONBLOCK as u64);

    // bind ANY:7890 — 不绑组地址 (POSIX 允许 ANY 收组播, smoltcp 对组播 dst 豁免地址匹配).
    let local = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: MCAST_PORT.to_be(),
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    };
    if bind(
        fd,
        &local as *const SockaddrIn as *const u8,
        SOCKADDR_IN_LEN,
    ) < 0
    {
        println("[mcast] FAIL: bind()");
        close_socket(fd);
        return;
    }

    // ip_mreq: 组地址网络序 (裸 octets 本就是网络序), 接口 0.0.0.0 = 由内核选.
    let mreq = IpMreq {
        imr_multiaddr: InAddr {
            s_addr: MCAST_GROUP,
        },
        imr_interface: InAddr { s_addr: [0u8; 4] },
    };
    let mreq_ptr = &mreq as *const IpMreq as *const u8;
    let mreq_len = core::mem::size_of::<IpMreq>() as u32;
    let rc = setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, mreq_ptr, mreq_len);
    if rc != 0 {
        print("[mcast] FAIL: ADD_MEMBERSHIP rc=");
        print_dec(rc as i64);
        print_char(b'\n');
        close_socket(fd);
        return;
    }
    println("[mcast] JOIN ok (iface group table entry armed)");

    // 引用计数 ABI 边界: 同一 socket 重复加同一组 → EADDRINUSE, 不二次动 iface.
    let rc = setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, mreq_ptr, mreq_len);
    if rc == -EADDRINUSE {
        println("[mcast] DUP ok (EADDRINUSE)");
    } else {
        print("[mcast] FAIL: duplicate join rc=");
        print_dec(rc as i64);
        print_char(b'\n');
    }

    // 收取宿主注入的组播报文 (载荷逐字比对).
    let mut buf = [0u8; 64];
    let mut tries: u32 = 0;
    loop {
        let n = recv(fd, buf.as_mut_ptr(), buf.len(), 0);
        if n > 0 {
            let got = n as usize;
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
        print_dec(rc as i64);
        print_char(b'\n');
    }
    // 非成员再退 → EADDRNOTAVAIL (本 socket 位图已无该组, 不得误伤 iface 表).
    let rc = setsockopt(fd, IPPROTO_IP, IP_DROP_MEMBERSHIP, mreq_ptr, mreq_len);
    if rc == -EADDRNOTAVAIL {
        println("[mcast] NOENT ok (EADDRNOTAVAIL)");
    } else {
        print("[mcast] FAIL: second drop rc=");
        print_dec(rc as i64);
        print_char(b'\n');
    }
    close_socket(fd);

    // ── IPv6 成员管理 ABI 腿 ───────────────────────────────────────────
    // IPv6 组播走 MLD 而非 IGMP, 宿主侧无对应注帧路径, 故本腿**不**验收发,
    // 只验 `IPPROTO_IPV6`(=41, IANA 永久分配) 这一 level 能否真实命中内核
    // 分支 —— 该值是纯 ABI, 取错 (曾误取 10) 会静默落 -ENOPROTOOPT(92),
    // 而 IPv4 腿完全不受影响, 无本腿则漂移无人发现.
    const MCAST6_GROUP: [u8; 16] = [0xff, 0x3e, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x42];
    // Linux errno: 未知选项 ENOPROTOOPT(92).
    const ENOPROTOOPT: i32 = 92;

    let fd6 = socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP);
    if fd6 < 0 {
        print("[mcast] FAIL: socket(AF_INET6)=");
        print_dec(fd6 as i64);
        print_char(b'\n');
        return;
    }
    let mreq6 = Ipv6Mreq {
        ipv6mr_multiaddr: In6Addr {
            s6_addr: MCAST6_GROUP,
        },
        ipv6mr_interface: 0,
    };
    let mreq6_ptr = &mreq6 as *const Ipv6Mreq as *const u8;
    let mreq6_len = core::mem::size_of::<Ipv6Mreq>() as u32;
    let rc = setsockopt(fd6, IPPROTO_IPV6, IPV6_ADD_MEMBERSHIP, mreq6_ptr, mreq6_len);
    if rc != 0 {
        print("[mcast] FAIL: IPV6_ADD_MEMBERSHIP rc=");
        print_dec(rc as i64);
        print_char(b'\n');
        close_socket(fd6);
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
        print_dec(rc as i64);
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
        print_dec(rc as i64);
        print_char(b'\n');
    }
    close_socket(fd6);
}

/// 用户态 UDP 活体对端回填探针 (P4 D9 recvfrom 端到端验证, 仅 x86_64).
///
/// 补 `udp_connect_probe` 未覆盖的"活体收发腿": 经已连接 UDP socket 实际发出
/// 数据报并由宿主回显服务应答, 验证 `recvfrom` 回填真实对端地址 (syscall 层 U3
/// 透传 src_ptr/src_len_ptr + `sm_recvfrom` UDP 分支 write_sockaddr). 拓扑:
/// 1. `socket(AF_INET, SOCK_DGRAM)` → `connect` 到 slirp 网关 `10.0.2.2:9090`;
/// 2. `send` 载荷 (走 D9 登记的 remote 端点, 非 sendto);
/// 3. `recvfrom` 有界重试收取回显, 校验内容一致 + src 回填 == `10.0.2.2:9090`;
/// 4. 里程碑 `[udp] RECVFROM src ok` 仅在宿主回显服务应答时出现; 普通启动无
///    应答者, 有界重试后打印 FAIL 并继续 (与 ipv6_udp_probe 同款容错, 不断言 FAIL).
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
        print_dec(fd as i64);
        print_char(b'\n');
        return;
    }
    // 同 ipv6_udp_probe: recvfrom 有界重试需非阻塞 (P5b 默认阻塞会使无应答时挂死).
    let _ = fcntl(fd, F_SETFL, O_NONBLOCK as u64);

    let peer = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: ECHO_PORT.to_be(),
        sin_addr: InAddr {
            s_addr: [10, 0, 2, 2],
        },
        sin_zero: [0u8; 8],
    };
    if connect(fd, &peer as *const SockaddrIn as *const u8, SOCKADDR_IN_LEN) != 0 {
        println("[udp] FAIL: echo connect()");
        close_socket(fd);
        return;
    }

    // send 走 D9 登记的 remote 端点 (无显式 dest); UDP 非阻塞投递, 有界重试至链路就绪.
    let payload = b"EDGINE-UDP";
    let mut tries = 0u32;
    loop {
        let n = send(fd, payload.as_ptr(), payload.len(), 0);
        if n == payload.len() as isize {
            break;
        }
        tries += 1;
        if tries >= MAX_TRIES {
            println("[udp] FAIL: echo send() timeout");
            close_socket(fd);
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
            &mut src as *mut SockaddrIn as *mut u8,
            &mut srclen as *mut u32,
        );
        if n > 0 {
            let got = n as usize;
            if got == payload.len()
                && &buf[..got] == &payload[..]
                && src.sin_addr.s_addr == [10, 0, 2, 2]
                && src.sin_port == ECHO_PORT.to_be()
            {
                println("[udp] RECVFROM src ok (peer backfilled = 10.0.2.2:9090)");
            } else {
                print("[udp] FAIL: echo mismatch len=");
                print_dec(n as i64);
                print_char(b'\n');
            }
            close_socket(fd);
            return;
        }
        recvs += 1;
        if recvs >= MAX_TRIES {
            println("[udp] FAIL: echo recvfrom() timeout (no responder)");
            close_socket(fd);
            return;
        }
        delay_ms(50);
    }
}

/// 用户态 TCP echo 服务端 (P3 D6 端到端验证, 仅 x86_64).
///
/// 常驻子进程: 监听 `0.0.0.0:80`, 循环 `accept` 已建立连接并原样回显.
/// 用途是端到端验证 D6 "先建后换" 交接语义 ——
/// 1. 连续多次入站连接下**监听槽持续可用** (每次 accept 后监听 fd 仍可再 accept);
/// 2. 连接处理完 `close_socket` 后 FD 归还, 可被下一次 `accept` 复用 (FD 回收).
///
/// 驱动方为 `scripts/qemu_boot_test.sh` 的端到端阶段: 该阶段以
/// `-netdev user,...,hostfwd=tcp::8080-:80` 启动 QEMU, 宿主客户端连
/// `localhost:8080`, 由 slirp 转发到 guest `:80` 触发本服务.
///
/// 语义要点: 内核 TCP `recv` 在**无数据**与**对端关闭(EOF)**两种情况都返回
/// `0` (smoltcp `recv_slice` 对已建立但 rx 缓冲为空的连接返回 `Ok(0)`),
/// 二者无法区分; 仅在连接已关闭时返回 `-E_CONNRESET`. 故每连接采取
/// **一次性回显**: `recv` 有界重试 (`0` 视为"暂无数据", 让出 CPU 供定时器
/// IRQ 推进 smoltcp 周期 poll) 直到取到数据 → `send` 原样回显 → 留出发送
/// 窗口后 `close`. 本函数永不返回 (常驻), 失败路径以忙等驻留 (不改变既有
/// APS-05/KPTI-09 断言).
#[cfg(target_arch = "x86_64")]
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
    // 回显 `send` 后留出的窗口 (ms): 待定时器 IRQ poll 把 tx 缓冲排空,
    // 再 close —— sm_close 中 `sockets.remove` 会丢弃未发出的数据.
    const FLUSH_MS: i64 = 50;

    let fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        print("[tcp] FAIL: socket()=");
        print_dec(fd as i64);
        print_char(b'\n');
        busy_wait(b'!');
    }

    // 允许快速重绑 (TIME_WAIT 残留), 与 httpsrv 一致.
    let opt: i32 = 1;
    let _ = setsockopt(
        fd,
        SOL_SOCKET,
        SO_REUSEADDR,
        &opt as *const i32 as *const u8,
        core::mem::size_of::<i32>() as u32,
    );

    // 通配绑定 0.0.0.0:80 — sin_family 主机序 (NE), sin_port 网络序 (BE).
    let local = SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: PORT.to_be(),
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    };
    let rc = bind(
        fd,
        &local as *const SockaddrIn as *const u8,
        SOCKADDR_IN_LEN,
    );
    if rc < 0 {
        print("[tcp] FAIL: bind()=");
        print_dec(rc as i64);
        print_char(b'\n');
        busy_wait(b'!');
    }

    let rc = listen(fd, 5);
    if rc < 0 {
        print("[tcp] FAIL: listen()=");
        print_dec(rc as i64);
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
            print_dec(conn as i64);
            print_char(b'\n');
            delay_ms(POLL_MS);
            continue;
        }
        print("[tcp] Connection accepted fd=");
        print_dec(conn as i64);
        print_char(b'\n');

        // D8: TCP_NODELAY setsockopt → getsockopt round-trip (关 Nagle → 读回 1).
        let nodelay: i32 = 1;
        let sret = setsockopt(
            conn,
            IPPROTO_TCP,
            TCP_NODELAY,
            &nodelay as *const i32 as *const u8,
            core::mem::size_of::<i32>() as u32,
        );
        let mut got: i32 = 0;
        let mut got_len: u32 = core::mem::size_of::<i32>() as u32;
        let gret = getsockopt(
            conn,
            IPPROTO_TCP,
            TCP_NODELAY,
            &mut got as *mut i32 as *mut u8,
            &mut got_len,
        );
        if sret == 0 && gret == 0 && got == 1 {
            println("[tcp] NODELAY roundtrip ok");
        } else {
            print("[tcp] FAIL: NODELAY set=");
            print_dec(sret as i64);
            print(" get=");
            print_dec(gret as i64);
            print(" val=");
            print_dec(got as i64);
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
            if poll_hits >= MAX_RECV_TRIES {
                break;
            }
            delay_ms(POLL_MS);
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
                let len = n as usize;
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
                // 留出窗口让定时器 IRQ poll 把回显排空 (close 会丢弃未发数据).
                delay_ms(FLUSH_MS);
                break;
            }
            if n == 0 {
                tries += 1;
                if tries >= MAX_RECV_TRIES {
                    println("[tcp] recv timeout (no data)");
                    break;
                }
                delay_ms(POLL_MS);
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
            print_dec(shut as i64);
            print_char(b'\n');
        }
        close_socket(conn);
        println("[tcp] Connection closed");
    }
}

/// 毫秒级休眠 (IPv6 探针重试节流): 以 `nanosleep` 让出 CPU, 使周期网络
/// poll (timer IRQ) 得以推进 RA 处理与收发.
#[cfg(target_arch = "x86_64")]
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
