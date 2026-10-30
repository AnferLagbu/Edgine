//! sockopt ABI 契约测试（plan socket-option-constants-unification S2）.
//!
//! 方案 A 单源模型下, 内核 `sm_fi.rs` 与 userlib `sys.rs` 均引用 `edgine-abi`,
//! "两侧不等"风险已由结构消除; 本测试钉住另一头——**值域本身正确**:
//! `edgine-abi::socket` 每个常量必须逐一等于 Linux x86_64 `asm-generic` 预期值.
//! 任一常量被改动偏离预期表, 本测试在 `make test-host` 即失败, 而不会等到
//! 用户程序拿到静默的 `-ENOPROTOOPT` 才暴露（`SO_KEEPALIVE`=8 漂移的教训）.
//!
//! 预期值表来源: `<asm-generic/socket.h>` / `<linux/in.h>` / `<netinet/tcp.h>`
//! (Linux 6.x, x86_64; aarch64 同用 asm-generic, 双架构一致).

use edgine_abi::socket::{
    IP_ADD_MEMBERSHIP, IP_DROP_MEMBERSHIP, IPPROTO_IP, IPPROTO_IPV6, IPPROTO_TCP, IPPROTO_UDP,
    IPV6_ADD_MEMBERSHIP, IPV6_DROP_MEMBERSHIP, SO_BROADCAST, SO_ERROR, SO_KEEPALIVE, SO_PASSCRED,
    SO_RCVTIMEO, SO_REUSEADDR, SO_REUSEPORT, SO_SNDTIMEO, SO_TYPE, SOL_SOCKET, TCP_NODELAY,
};

/// level 常量: `<asm-generic/socket.h>` `SOL_SOCKET` + `<linux/in.h>` IPPROTO_*.
#[test]
fn abi_sockopt_levels_match_asm_generic() {
    assert_eq!(SOL_SOCKET, 1, "SOL_SOCKET 偏离 asm-generic");
    assert_eq!(
        IPPROTO_IP, 0,
        "IPPROTO_IP 兼作 socket() protocol 0, 不得漂移"
    );
    assert_eq!(IPPROTO_TCP, 6, "IPPROTO_TCP 为 IANA 永久分配");
    assert_eq!(IPPROTO_UDP, 17, "IPPROTO_UDP 为 IANA 永久分配");
    // IANA 给 IPv6 协议号的永久分配; 曾在本工程前被误取 10（AF_INET6 值混入）,
    // glibc 程序会完好请求拿到 ENOPROTOOPT —— 本断言是该回归的锁.
    assert_eq!(
        IPPROTO_IPV6, 41,
        "IPPROTO_IPV6 为 IANA 永久分配 (非 AF_INET6=10)"
    );
}

/// `SOL_SOCKET` 层选项: `<asm-generic/socket.h>`.
#[test]
fn abi_sol_socket_options_match_asm_generic() {
    assert_eq!(SO_REUSEADDR, 2);
    assert_eq!(SO_TYPE, 3);
    assert_eq!(SO_ERROR, 4);
    // 曾为 userlib 侧 32（既非内核值也非 asm-generic 值）, 方案 A 单源修正.
    assert_eq!(SO_BROADCAST, 6);
    // 曾为 userlib 侧 8（alpha/mips 系值混入）, 致内核 keep-alive 永不可达,
    // 方案 A 单源修正.
    assert_eq!(SO_KEEPALIVE, 9);
    assert_eq!(SO_REUSEPORT, 15);
    assert_eq!(SO_PASSCRED, 16);
    assert_eq!(SO_RCVTIMEO, 20);
    assert_eq!(SO_SNDTIMEO, 21);
}

/// 传输层/网络层选项: `<netinet/tcp.h>` + `<linux/in.h>` + `<linux/in6.h>`.
#[test]
fn abi_layer_options_match_asm_generic() {
    assert_eq!(TCP_NODELAY, 1);
    assert_eq!(IP_ADD_MEMBERSHIP, 35);
    assert_eq!(IP_DROP_MEMBERSHIP, 36);
    // Linux 口径: glibc 将 BSD 的 IPV6_JOIN_GROUP/LEAVE_GROUP 别名到 20/21.
    assert_eq!(IPV6_ADD_MEMBERSHIP, 20);
    assert_eq!(IPV6_DROP_MEMBERSHIP, 21);
}

/// 值域唯一性: SO_*/TCP_* 同名不同值的历史漂移不可能在单源下复活, 但
/// "`两个不同名常量意外取同值"仍需拦住（SOL_SOCKET=1` 与 `SO_TYPE=3` 各有独立
/// 语义位; 此处显式复核一组历史上易混值）.
#[test]
fn abi_no_accidental_value_collision() {
    // SO_REUSEPORT(15) 与 IP_ADD_MEMBERSHIP(35) 分属不同 level 空间, 但
    // IPV6_ADD_MEMBERSHIP(20) 与 SO_RCVTIMEO(20) 数值同为 20 —— 依赖 level
    // 区分 (SOL_SOCKET vs IPPROTO_IPV6). 若未来把分派从 (level, optname)
    // 退化单按 optname, 本对值就是首个静默串号点, 故在测试内留档提醒.
    assert_eq!(SO_RCVTIMEO, IPV6_ADD_MEMBERSHIP); // 现状: 合法, 由 level 区分
    assert_ne!(SO_KEEPALIVE, SO_BROADCAST, "9 与 6 曾被 userlib 误作 8/32");
}
