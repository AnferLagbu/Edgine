//! DNS 双栈 (docs/plan/dns-dual-stack.md) — 跨 crate 公共面契约锁.
//!
//! 核心合并排序算法 (`merge_dns_servers`) 的行为单测在 privileged 侧
//! `init/dns.rs` 内联 (IPv6 优先 / 跳过零槽 / 回退 / 满容量); `DHCPv6` 状态机
//! 产出 `apply_options.dns_servers` 的行为锁在 `dhcpv6_policy_test.rs` 与
//! `state.rs` 内联. 本文件补齐两者之间的 **接线与公共面契约**: 锁定 D1 破坏性
//! 返回面 (`dns_resolve -> Option<IpAddr>`)、D4 观测面 (`get_dns6_servers`)、
//! D3 存储/短路/喂入接线, 防止未来回退把双栈面悄悄改回 IPv4-only.
//!
//! 真实 AAAA 解析 (`DHCPv6` DNS 服务器 → 双栈查询) 需真网络, 归 QEMU e2e 腿.

use std::fs;
use std::path::Path;

const NET_INIT: &str = "../src/kernel/privileged/net/init";
const NET: &str = "../src/kernel/privileged/net";
const DHCPV6_FN: &str = "../src/kernel/functions/net/dhcpv6";

fn read(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {} 失败: {}", Path::new(path).display(), e))
}

/// D1: 解析返回面双栈 —— `init::dns` 与 api 包装均返 `Option<IpAddr>`,
/// 不得回退到 `Option<[u8; 4]>` (IPv4-only).
#[test]
fn contract_dns_resolve_dual_stack_return_face() {
    let dns = read(&format!("{NET_INIT}/dns.rs"));
    assert!(
        dns.contains("pub fn dns_resolve(name: &str) -> Option<IpAddr>"),
        "init::dns::dns_resolve 应返 Option<IpAddr> (D1 双栈返回面)"
    );
    let api = read(&format!("{NET}/api.rs"));
    assert!(
        api.contains("pub fn dns_resolve(name: &str) -> Option<IpAddr>"),
        "api::dns_resolve 应返 Option<IpAddr> (D1 双栈返回面)"
    );
    // IPv6 优先: 先 AAAA 后 A (D2).
    assert!(
        dns.contains("dns_query_net(name, DnsQueryType::Aaaa)"),
        "dns_resolve 应先发 AAAA 查询 (D2 IPv6 优先)"
    );
    assert!(
        dns.contains("dns_query_net(name, DnsQueryType::A)"),
        "dns_resolve 应回退 A 查询 (D2 双栈)"
    );
}

/// D4: IPv6 DNS 服务器观测面 —— query 实现与 api 包装均暴露
/// `get_dns6_servers() -> [Option<[u8; 16]>; 3]`, 与 v4 面并行.
#[test]
fn contract_dns6_observation_face() {
    let query = read(&format!("{NET_INIT}/query.rs"));
    assert!(
        query.contains("pub fn get_dns6_servers() -> [Option<[u8; 16]>; 3]"),
        "query::get_dns6_servers 应暴露 IPv6 服务器观测面 (D4)"
    );
    let api = read(&format!("{NET}/api.rs"));
    assert!(
        api.contains("pub fn get_dns6_servers() -> [Option<[u8; 16]>; 3]"),
        "api::get_dns6_servers 包装应存在 (D4)"
    );
}

/// D3: IPv6 服务器存储 + lock-free 短路观测位.
#[test]
fn contract_dns6_storage_and_short_circuit() {
    let state = read(&format!("{NET_INIT}/state.rs"));
    assert!(
        state.contains("pub(crate) dns6_servers: [Option<[u8; 16]>; 3]"),
        "NetState 应有 dns6_servers 三槽存储 (D3)"
    );
    assert!(
        state.contains("pub static G_DNS6_PRESENT: AtomicBool"),
        "应有 G_DNS6_PRESENT lock-free 观测位 (D3)"
    );
    let dns = read(&format!("{NET_INIT}/dns.rs"));
    assert!(
        dns.contains("G_DNS6_PRESENT.load(Ordering::Acquire)"),
        "dns_query_net 短路判应消费 G_DNS6_PRESENT (IPv6-only server 亦可查询)"
    );
}

/// D3: `DHCPv6` 学到的 IPv6 DNS 服务器经 functions driver → privileged ctx 喂入.
#[test]
fn contract_dhcpv6_dns6_wiring() {
    let driver = read(&format!("{DHCPV6_FN}/driver.rs"));
    assert!(
        driver.contains("ctx.apply_dns6(&opts.dns_servers);"),
        "driver::execute 应消费 apply_options.dns_servers 调 ctx.apply_dns6 (D3 接线)"
    );
    let ctx = read(&format!("{NET_INIT}/dhcpv6.rs"));
    assert!(
        ctx.contains("pub fn apply_dns6(&mut self, servers: &[[u8; 16]])"),
        "Dhcpv6PollCtx::apply_dns6 应只接受裸字节 (F2 类型擦除)"
    );
    assert!(
        ctx.contains("super::dns::apply_dns6_servers(self.sockets, raw::dns_handle(), servers);"),
        "apply_dns6 应委托 dns::apply_dns6_servers 合并喂入"
    );
}

/// D3: 合并排序 IPv6 优先 —— `merge_dns_servers` 中 IPv6 循环必须先于 IPv4
/// 循环 (smoltcp `DNS_MAX_SERVER_COUNT=1` 截断到首个, 排序即优先级).
#[test]
fn contract_ipv6_first_merge_order() {
    let dns = read(&format!("{NET_INIT}/dns.rs"));
    assert!(
        dns.contains("pub(crate) fn merge_dns_servers("),
        "应有纯函数 merge_dns_servers (可单测的合并排序)"
    );
    let i6 = dns
        .find("for slot in dns6")
        .expect("merge_dns_servers 应含 IPv6 合并循环");
    let i4 = dns
        .find("for ip in leases")
        .expect("merge_dns_servers 应含 IPv4 合并循环");
    assert!(i6 < i4, "IPv6 循环必须先于 IPv4 循环 (D3 IPv6 优先)");
}
