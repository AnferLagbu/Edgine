//! P-24: smoltcp 邻居发现限速本地化守卫
//!
//! 背景: vendored smoltcp 0.14 上游用单一全缓存 `silent_until: Instant` 做邻居发现
//! 限速——任一目的地发出 ARP/NS 后, 整张缓存静音 SILENT_TIME(1s), 他址 `lookup` 返
//! RateLimited (不发请求). 本机 IPv4 ARP(网关) 与 IPv6 ND(SLAAC) 共用一张缓存并发发现,
//! 导致网关 ARP 被 IPv6 流量饿死, 首个出站 UDP 包从未离 NIC (见
//! `docs/plan/net-e2e-tcp-echo-flakiness.md` P-24).
//!
//! 修复: 限速改为 per-destination 有界 silent 环 (`neighbor.rs` 本地化) +
//! `interface/mod.rs` 调用点传入 `dst_addr`.
//!
//! vendored 内部 `#[cfg(test)]` 用例不作为 gate 执行, 故此处用源码结构守卫锁定
//! 本地化不丢失: 若本地化被回退/覆盖为上游全局限速形态, 本测试即失败.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

const NEIGHBOR_RS: &str = "src/kernel/functions/net/smoltcp/src/iface/neighbor.rs";
const INTERFACE_MOD_RS: &str = "src/kernel/functions/net/smoltcp/src/iface/interface/mod.rs";

/// `Cache::silent_until` 必须是 per-destination 的 LinearMap, 而非上游单一 Instant.
#[test]
fn test_silent_until_is_per_destination_map() {
    let src = read_src(NEIGHBOR_RS);
    assert!(
        src.contains("silent_until: LinearMap<IpAddress, Instant, IFACE_NEIGHBOR_CACHE_COUNT>"),
        "P-24: neighbor.rs Cache.silent_until 应为 per-destination LinearMap"
    );
    // 反向守卫: 上游单一全缓存静音字段不应存在.
    assert!(
        !src.contains("silent_until: Instant,"),
        "P-24: neighbor.rs 不应残留上游全缓存 silent_until: Instant (会跨目的地饿死)"
    );
}

/// `limit_rate` 必须接收目的地址参数, 只静音该目的地.
#[test]
fn test_limit_rate_takes_protocol_addr() {
    let src = read_src(NEIGHBOR_RS);
    assert!(
        src.contains("fn limit_rate(&mut self, protocol_addr: IpAddress, timestamp: Instant)"),
        "P-24: neighbor.rs limit_rate 应带 protocol_addr 参数 (per-destination 限速)"
    );
}

/// `lookup` 限速判定必须按目的地址查环, 而非比较单一全局时间戳.
#[test]
fn test_lookup_checks_rate_limit_per_destination() {
    let src = read_src(NEIGHBOR_RS);
    assert!(
        src.contains(".silent_until") && src.contains("get(protocol_addr)"),
        "P-24: neighbor.rs lookup 应按 protocol_addr 查 silent_until 环"
    );
    // 反向守卫: 上游 `timestamp < self.silent_until` 全比较不应存在.
    assert!(
        !src.contains("timestamp < self.silent_until"),
        "P-24: neighbor.rs 不应残留上游全缓存限速比较 (timestamp < self.silent_until)"
    );
}

/// `flush` 必须同时清空 silent_until 环, 防止限速记录跨 flush 残留.
#[test]
fn test_flush_clears_silent_ring() {
    let src = read_src(NEIGHBOR_RS);
    let idx = src
        .find("fn flush(&mut self)")
        .expect("P-24: 应存在 flush() 方法");
    let body = &src[idx..idx + 200];
    assert!(
        body.contains("self.silent_until.clear()"),
        "P-24: flush() 应清空 silent_until 环"
    );
}

/// egress 调用点必须把限速作用到目的地址 (`limit_rate(dst_addr, ...)`), 而非全缓存.
#[test]
fn test_egress_limits_rate_for_destination() {
    let src = read_src(INTERFACE_MOD_RS);
    assert!(
        src.contains("limit_rate(dst_addr, self.now)"),
        "P-24: interface/mod.rs egress 应对 dst_addr 限速 (per-destination)"
    );
    // 反向守卫: 上游无参全缓存调用不应存在.
    assert!(
        !src.contains("limit_rate(self.now)"),
        "P-24: interface/mod.rs 不应残留上游全缓存限速调用 limit_rate(self.now)"
    );
}
