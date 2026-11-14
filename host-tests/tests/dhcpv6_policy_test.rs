//! P6b Phase 6a: `DHCPv6` 客户端 — 跨 crate 公共 API 契约 + 行为端到端.
//!
//! 经 `edgine` 壳导入 functions 侧纯逻辑 (`wire` / `state` / `ra` / `policy`)
//! 与 privileged 边界类型 (`Dhcpv6State`), 验证:
//! 1. 契约锁: 5 个源文件 0 unsafe + 模块存在 + 关键 API 签名 + RFC 8415 默认常数.
//! 2. 行为: 完整 SARR → Bound、无状态 Information-Request → `InfoBound`、
//!    RA M/O 触发解析、T1 续约单播、释放移除地址、txid 不匹配 / 畸形报文忽略、
//!    Advertise 偏好选择、wire 往返保真.
//!
//! 与 `state.rs` 内联单测同源夹具, 但走跨 crate 公共面 (壳 re-export 路径),
//! 锁定 functions → privileged 依赖方向与公共 API 稳定性.

use std::fs;
use std::path::Path;

use edgine::kernel::functions::net::dhcpv6::policy::Dhcpv6PolicyConfig;
use edgine::kernel::functions::net::dhcpv6::ra;
use edgine::kernel::functions::net::dhcpv6::state::{Dest, Dhcpv6Client, Output};
use edgine::kernel::functions::net::dhcpv6::wire::{IaAddr, IaNa, MessageType, Opt, Repr};
use edgine::kernel::privileged::net::iface_trait::Dhcpv6State;

const DHCPV6_DIR: &str = "../src/kernel/functions/net/dhcpv6";

// ============================================================================
// 契约锁 (文本扫描)
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {} 失败: {}", Path::new(path).display(), e))
}

#[test]
fn contract_dhcpv6_modules_exist() {
    for f in [
        "mod.rs",
        "wire.rs",
        "state.rs",
        "policy.rs",
        "ra.rs",
        "driver.rs",
    ] {
        let content = read(&format!("{DHCPV6_DIR}/{f}"));
        assert!(
            content.contains("#![deny(unsafe_code)]"),
            "{f} 应启用 deny(unsafe_code) (functions 层铁律 F1)"
        );
    }
}

#[test]
fn contract_zero_unsafe_across_dhcpv6() {
    // functions 层 0 unsafe 实际使用形态 (deny 属性与注释不计).
    for f in [
        "mod.rs",
        "wire.rs",
        "state.rs",
        "policy.rs",
        "ra.rs",
        "driver.rs",
    ] {
        let content = read(&format!("{DHCPV6_DIR}/{f}"));
        let total = content.matches("unsafe {").count()
            + content.matches("unsafe fn").count()
            + content.matches("unsafe impl").count()
            + content.matches("unsafe trait").count();
        assert_eq!(total, 0, "{f} 应 0 unsafe 实际使用, 实测: {total}");
    }
}

#[test]
fn contract_state_public_api_signatures() {
    let content = read(&format!("{DHCPV6_DIR}/state.rs"));
    for sig in [
        "pub fn new(",
        "pub fn state(&self) -> Dhcpv6State",
        "pub fn start_stateful(&mut self, now_ms: u64) -> Output",
        "pub fn start_stateless(&mut self, now_ms: u64) -> Output",
        "pub fn release(&mut self, now_ms: u64) -> Output",
        "pub fn handle_packet(",
        "pub fn tick(&mut self, now_ms: u64) -> Output",
    ] {
        assert!(content.contains(sig), "state.rs 应含公共 API: {sig}");
    }
}

#[test]
fn contract_wire_public_api_signatures() {
    let content = read(&format!("{DHCPV6_DIR}/wire.rs"));
    for sig in [
        "pub fn parse(buf: &[u8]) -> Result<Self, ParseError>",
        "pub fn emit(&self, buf: &mut [u8]) -> Result<usize, ParseError>",
        "pub const fn from_u8(v: u8) -> Self",
        "pub const fn to_u8(self) -> u8",
    ] {
        assert!(content.contains(sig), "wire.rs 应含公共 API: {sig}");
    }
}

#[test]
fn contract_policy_rfc8415_defaults() {
    let content = read(&format!("{DHCPV6_DIR}/policy.rs"));
    // RFC 8415 §15 重传常数 + T1=50% / T2=87.5%.
    for needle in [
        "sol_timeout_ms: 1_000",
        "sol_max_rt_ms: 120_000",
        "max_retries: 8",
        "renew_t1_ratio: 5_000",
        "renew_t2_ratio: 8_750",
    ] {
        assert!(
            content.contains(needle),
            "policy.rs 应含 RFC 默认: {needle}"
        );
    }
}

#[test]
fn contract_ra_pure_byte_parser() {
    let content = read(&format!("{DHCPV6_DIR}/ra.rs"));
    assert!(
        content.contains("pub fn parse(packet: &[u8]) -> Option<RaTrigger>"),
        "ra.rs 应暴露纯字节 RA 解析公共 API"
    );
    assert!(
        content.contains("pub struct RaTrigger"),
        "ra.rs 应定义 RaTrigger (M/O 标志)"
    );
}

// ============================================================================
// 行为夹具 (与 state.rs 内联单测同源)
// ============================================================================

const ADDR: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const SERVER_ADDR: [u8; 16] = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

fn server_duid() -> Vec<u8> {
    vec![0, 3, 0, 1, 1, 2, 3, 4, 5, 6]
}

fn client() -> Dhcpv6Client {
    Dhcpv6Client::new(
        vec![0, 3, 0, 1, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        7,
        [0, 0, 0],
        Dhcpv6PolicyConfig::default(),
    )
}

fn emit_repr(repr: &Repr) -> Vec<u8> {
    let mut buf = [0u8; 1024];
    let n = repr.emit(&mut buf).unwrap();
    buf[..n].to_vec()
}

fn txid_of(out: &Output) -> [u8; 3] {
    Repr::parse(&out.tx_packets[0].bytes)
        .unwrap()
        .transaction_id
}

fn advertise(txid: [u8; 3], server: &[u8], preference: u8) -> Vec<u8> {
    emit_repr(&Repr {
        msg_type: MessageType::Advertise,
        transaction_id: txid,
        options: vec![
            Opt::ServerId(server.to_vec()),
            Opt::Preference(preference),
            Opt::IaNa(IaNa {
                iaid: 7,
                t1: 0,
                t2: 0,
                options: Vec::new(),
            }),
        ],
    })
}

fn reply_with_addr(
    txid: [u8; 3],
    server: &[u8],
    addr: [u8; 16],
    preferred: u32,
    valid: u32,
    dns: Vec<[u8; 16]>,
) -> Vec<u8> {
    let mut options = vec![
        Opt::ServerId(server.to_vec()),
        Opt::IaNa(IaNa {
            iaid: 7,
            t1: 0,
            t2: 0,
            options: vec![Opt::IaAddr(IaAddr {
                addr,
                preferred_lifetime: preferred,
                valid_lifetime: valid,
            })],
        }),
    ];
    if !dns.is_empty() {
        options.push(Opt::DnsServers(dns));
    }
    emit_repr(&Repr {
        msg_type: MessageType::Reply,
        transaction_id: txid,
        options,
    })
}

fn reply_ack(txid: [u8; 3], server: &[u8]) -> Vec<u8> {
    emit_repr(&Repr {
        msg_type: MessageType::Reply,
        transaction_id: txid,
        options: vec![Opt::ServerId(server.to_vec())],
    })
}

/// 已绑定客户端夹具 (绑定时刻 300ms, 有效生命周期 7200s, 服务器地址已学习).
fn bound_client() -> Dhcpv6Client {
    let mut c = client();
    let out = c.start_stateful(0);
    let txid = txid_of(&out);
    c.handle_packet(&advertise(txid, &server_duid(), 10), Some(SERVER_ADDR), 100);
    c.tick(200);
    c.handle_packet(
        &reply_with_addr(txid, &server_duid(), ADDR, 3600, 7200, vec![]),
        Some(SERVER_ADDR),
        300,
    );
    c
}

// ============================================================================
// 行为: 有状态 SARR
// ============================================================================

#[test]
fn behavior_full_sarr_to_bound() {
    let mut c = client();
    let out = c.start_stateful(0);
    assert_eq!(c.state(), Dhcpv6State::Soliciting);
    assert_eq!(out.tx_packets.len(), 1);
    assert_eq!(out.tx_packets[0].dest, Dest::Multicast);
    let solicit = Repr::parse(&out.tx_packets[0].bytes).unwrap();
    assert_eq!(solicit.msg_type, MessageType::Solicit);
    let txid = solicit.transaction_id;

    let out = c.handle_packet(&advertise(txid, &server_duid(), 10), Some(SERVER_ADDR), 100);
    assert_eq!(c.state(), Dhcpv6State::Selecting);
    assert_eq!(out.tx_packets.len(), 0);

    let out = c.tick(200);
    assert_eq!(c.state(), Dhcpv6State::Requesting);
    assert_eq!(
        Repr::parse(&out.tx_packets[0].bytes).unwrap().msg_type,
        MessageType::Request
    );

    let out = c.handle_packet(
        &reply_with_addr(txid, &server_duid(), ADDR, 3600, 7200, vec![]),
        Some(SERVER_ADDR),
        300,
    );
    assert!(matches!(c.state(), Dhcpv6State::Bound { ipv6, .. } if ipv6 == ADDR));
    let apply = out.apply_addr.unwrap();
    assert_eq!(apply.ipv6, ADDR);
    assert_eq!(apply.preferred_s, 3600);
    assert_eq!(apply.valid_s, 7200);
    assert!(!apply.remove);
}

#[test]
fn behavior_advertise_preference_selection() {
    let mut c = client();
    let out = c.start_stateful(0);
    let txid = txid_of(&out);
    let low = vec![1, 1, 1, 1];
    let high = vec![2, 2, 2, 2];
    c.handle_packet(&advertise(txid, &low, 5), Some(SERVER_ADDR), 100);
    c.handle_packet(&advertise(txid, &high, 20), Some(SERVER_ADDR), 110);
    let out = c.tick(200);
    let req = Repr::parse(&out.tx_packets[0].bytes).unwrap();
    let sid = req
        .options
        .iter()
        .find_map(|o| match o {
            Opt::ServerId(id) => Some(id.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(sid, high, "应选最高 preference 的服务器");
}

#[test]
fn behavior_t1_renew_unicast() {
    let mut c = bound_client();
    let out = c.tick(300 + 3_600_000);
    assert!(matches!(c.state(), Dhcpv6State::Renewing { .. }));
    assert_eq!(out.tx_packets.len(), 1);
    assert_eq!(
        Repr::parse(&out.tx_packets[0].bytes).unwrap().msg_type,
        MessageType::Renew
    );
    assert_eq!(out.tx_packets[0].dest, Dest::Unicast(SERVER_ADDR));
}

#[test]
fn behavior_renew_reply_refreshes_bound() {
    let mut c = bound_client();
    let out = c.tick(300 + 3_600_000);
    let txid = txid_of(&out);
    let out = c.handle_packet(
        &reply_with_addr(txid, &server_duid(), ADDR, 3600, 7200, vec![]),
        Some(SERVER_ADDR),
        300 + 3_600_100,
    );
    assert!(matches!(c.state(), Dhcpv6State::Bound { .. }));
    assert!(!out.apply_addr.unwrap().remove);
}

#[test]
fn behavior_release_removes_addr_then_idle() {
    let mut c = bound_client();
    let out = c.release(1000);
    assert_eq!(c.state(), Dhcpv6State::Releasing);
    assert!(out.apply_addr.unwrap().remove);
    let rel = Repr::parse(&out.tx_packets[0].bytes).unwrap();
    assert_eq!(rel.msg_type, MessageType::Release);
    c.handle_packet(
        &reply_ack(rel.transaction_id, &server_duid()),
        Some(SERVER_ADDR),
        1100,
    );
    assert_eq!(c.state(), Dhcpv6State::Idle);
}

#[test]
fn behavior_txid_mismatch_ignored() {
    let mut c = client();
    c.start_stateful(0);
    let out = c.handle_packet(
        &advertise([0xff, 0xff, 0xff], &server_duid(), 10),
        Some(SERVER_ADDR),
        100,
    );
    assert_eq!(c.state(), Dhcpv6State::Soliciting);
    assert_eq!(out.tx_packets.len(), 0);
}

#[test]
fn behavior_malformed_packet_ignored() {
    let mut c = client();
    c.start_stateful(0);
    let out = c.handle_packet(&[0x01, 0x02], Some(SERVER_ADDR), 100);
    assert_eq!(c.state(), Dhcpv6State::Soliciting);
    assert_eq!(out.tx_packets.len(), 0);
}

// ============================================================================
// 行为: 无状态 Information-Request
// ============================================================================

#[test]
fn behavior_stateless_info_bound() {
    let mut c = client();
    let out = c.start_stateless(0);
    assert_eq!(c.state(), Dhcpv6State::InfoRequesting);
    let info = Repr::parse(&out.tx_packets[0].bytes).unwrap();
    assert_eq!(info.msg_type, MessageType::InformationRequest);
    let dns = vec![[
        0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
    ]];
    let repr = Repr {
        msg_type: MessageType::Reply,
        transaction_id: info.transaction_id,
        options: vec![
            Opt::DnsServers(dns.clone()),
            Opt::InformationRefreshTime(86400),
        ],
    };
    let out = c.handle_packet(&emit_repr(&repr), Some(SERVER_ADDR), 100);
    assert_eq!(c.state(), Dhcpv6State::InfoBound);
    let opts = out.apply_options.unwrap();
    assert_eq!(opts.dns_servers, dns);
    assert_eq!(opts.refresh_s, Some(86400));
    assert!(out.apply_addr.is_none(), "无状态不落地址");
}

// ============================================================================
// 行为: RA M/O 触发解析 (纯字节)
// ============================================================================

/// 构造最小合法 RA 全报文 (56 字节): 源 `fe80::1`, 目的 `ff02::1`,
/// hop limit 255, type 134 / code 0, flags 由参数给定.
fn ra_packet(flags: u8) -> [u8; 56] {
    let mut p = [0u8; 56];
    p[0] = 0x60;
    p[4..6].copy_from_slice(&16u16.to_be_bytes());
    p[6] = 58; // 下一报头 = ICMPv6 (58).
    p[7] = 255; // 跳数限制 = 255.
    p[8] = 0xfe; // 源 fe80::1.
    p[9] = 0x80;
    p[23] = 0x01;
    p[24] = 0xff; // 目的 ff02::1 (全节点).
    p[25] = 0x02;
    p[39] = 0x01;
    p[40] = 134; // RA 类型 = 134.
    p[44] = 64; // 当前跳数限制 = 64.
    p[45] = flags;
    p[46..48].copy_from_slice(&1800u16.to_be_bytes());
    p
}

#[test]
fn behavior_ra_managed_only() {
    let t = ra::parse(&ra_packet(0x80)).unwrap();
    assert!(t.managed);
    assert!(!t.other);
}

#[test]
fn behavior_ra_other_only() {
    let t = ra::parse(&ra_packet(0x40)).unwrap();
    assert!(!t.managed);
    assert!(t.other);
}

#[test]
fn behavior_ra_both_flags() {
    let t = ra::parse(&ra_packet(0xc0)).unwrap();
    assert!(t.managed && t.other);
}

#[test]
fn behavior_ra_neither_flag_valid() {
    let t = ra::parse(&ra_packet(0)).unwrap();
    assert!(!t.managed && !t.other);
}

#[test]
fn behavior_ra_rejects_bad_hop_limit() {
    let mut p = ra_packet(0x80);
    p[7] = 64;
    assert!(ra::parse(&p).is_none());
}

#[test]
fn behavior_ra_rejects_non_link_local_src() {
    let mut p = ra_packet(0x80);
    p[8] = 0x20; // 源 2001::1 (全局单播).
    p[9] = 0x01;
    assert!(ra::parse(&p).is_none());
}

#[test]
fn behavior_ra_rejects_wrong_type() {
    let mut p = ra_packet(0x80);
    p[40] = 135; // Neighbor Solicitation.
    assert!(ra::parse(&p).is_none());
}

// ============================================================================
// 行为: wire 往返保真
// ============================================================================

#[test]
fn behavior_wire_roundtrip_preserves_options() {
    let dns = vec![ADDR];
    let repr = Repr {
        msg_type: MessageType::Reply,
        transaction_id: [0x12, 0x34, 0x56],
        options: vec![
            Opt::ServerId(server_duid()),
            Opt::IaNa(IaNa {
                iaid: 7,
                t1: 100,
                t2: 200,
                options: vec![Opt::IaAddr(IaAddr {
                    addr: ADDR,
                    preferred_lifetime: 3600,
                    valid_lifetime: 7200,
                })],
            }),
            Opt::DnsServers(dns),
            Opt::InformationRefreshTime(86400),
        ],
    };
    let bytes = emit_repr(&repr);
    let parsed = Repr::parse(&bytes).unwrap();
    assert_eq!(
        parsed, repr,
        "wire 往返应保真 (含嵌套 IA_NA / DNS / 刷新间隔)"
    );
}

#[test]
fn behavior_wire_message_type_codes() {
    // RFC 8415 §7.3 客户端相关码往返.
    for (mt, code) in [
        (MessageType::Solicit, 1u8),
        (MessageType::Advertise, 2),
        (MessageType::Request, 3),
        (MessageType::Renew, 5),
        (MessageType::Rebind, 6),
        (MessageType::Reply, 7),
        (MessageType::Release, 8),
        (MessageType::InformationRequest, 11),
    ] {
        assert_eq!(mt.to_u8(), code);
        assert_eq!(MessageType::from_u8(code), mt);
    }
    assert_eq!(MessageType::from_u8(200), MessageType::Unknown(200));
}
