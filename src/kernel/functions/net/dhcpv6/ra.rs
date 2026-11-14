#![deny(unsafe_code)]
#![deny(missing_docs)]
//! RA M/O 触发解析 (P6b Phase 5): 纯字节, 0 smoltcp.
//!
//! 从 privileged raw `ICMPv6` socket 捕获的**完整 IPv6 报文** (40 字节固定头 +
//! `ICMPv6` 载荷) 中提取路由通告 (Router Advertisement, RFC 4861 §4.2) 的
//! M (Managed) / O (Other) 标志, 据此触发 `DHCPv6` 有状态 / 无状态启动.
//!
//! 与 `wire` / `state` / `policy` 同范式: 仅用 `&[u8]` / 整数, 不依赖 smoltcp,
//! 可离线单测. 校验口径仿 smoltcp 内部 RA 接收判定 (`iface::interface::ipv6`
//! 的 `process_ndisc`): 源 link-local + 目的 (全节点组播或 link-local) +
//! hop limit = 255, 再过滤 `ICMPv6` type = 134 / code = 0.

/// RA 触发的 `DHCPv6` 启动判定 (RFC 4861 §4.2 M/O 标志).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaTrigger {
    /// M (Managed) 标志: 置位表示经有状态 `DHCPv6` 分配地址 (SARR).
    pub managed: bool,
    /// O (Other) 标志: 置位表示经无状态 `DHCPv6` 取其他配置 (Information-Request).
    pub other: bool,
}

/// IPv6 固定头长度 (字节).
const IPV6_HDR_LEN: usize = 40;
/// IPv6 头 next header 字段偏移.
const IPV6_NEXT_HEADER: usize = 6;
/// IPv6 头 hop limit 字段偏移.
const IPV6_HOP_LIMIT: usize = 7;
/// `ICMPv6` 协议号 (IPv6 next header).
const ICMPV6_PROTO: u8 = 58;
/// `ICMPv6` RA 固定头长度 (type/code/checksum/hop/flags/lifetime/reachable/retrans).
const ICMPV6_RA_HDR_LEN: usize = 16;
/// `ICMPv6` type 字段偏移 (相对 `ICMPv6` 起点).
const ICMPV6_TYPE: usize = 0;
/// `ICMPv6` code 字段偏移 (相对 `ICMPv6` 起点).
const ICMPV6_CODE: usize = 1;
/// `ICMPv6` RA flags 字段偏移 (相对 `ICMPv6` 起点; 全报文偏移 45).
const ICMPV6_RA_FLAGS: usize = 5;
/// `ICMPv6` Router Advertisement 类型号 (RFC 4861 §4.2).
const RA_TYPE: u8 = 134;
/// RA flags 的 M (Managed) 位.
const RA_FLAG_M: u8 = 0x80;
/// RA flags 的 O (Other) 位.
const RA_FLAG_O: u8 = 0x40;
/// 全节点链路本地组播地址 `ff02::1` (RA 的常见目的).
const ALL_NODES: [u8; 16] = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01];

/// 判定地址是否链路本地 (`fe80::/10`: 首字节 `0xfe` 且次字节高 2 位 `10`).
fn is_link_local(addr: &[u8]) -> bool {
    addr[0] == 0xfe && (addr[1] & 0xc0) == 0x80
}

/// 从完整 IPv6 报文解析 RA 的 M/O 触发标志.
///
/// 依次校验: 长度 ≥ 56 (IPv6 头 + RA 固定头) / next header = 58 / hop limit = 255 /
/// 源 link-local / 目的 (`ff02::1` 或 link-local) / `ICMPv6` type = 134 / code = 0.
/// 任一不符 (含非 RA 的 `ICMPv6` 报文) 返回 `None`; 合法 RA 返回 [`RaTrigger`]
/// (M/O 均未置位时两字段皆 `false`, 表示不触发 `DHCPv6`).
///
/// 入参为 raw socket 重序列化后的完整 IPv6 报文 (头字段保留接收原值).
#[must_use]
pub fn parse(packet: &[u8]) -> Option<RaTrigger> {
    // SIMPLIFIED: 不校验 ICMPv6 checksum; 影响面: raw 捕获仅触发 DHCPv6 发现 (低危,
    //   地址分配仍需合法 Reply + ServerID 匹配), 结构性校验 (hop limit=255 + 源/目的
    //   link-local + type/code) 已防 off-link 伪造, smoltcp 内部 SLAAC 路径独立校验
    //   checksum; 何时扩展: 若 RA 触发面扩到直接落地址/路由等高权操作时补伪头 checksum.
    if packet.len() < IPV6_HDR_LEN + ICMPV6_RA_HDR_LEN {
        return None;
    }
    if packet[IPV6_NEXT_HEADER] != ICMPV6_PROTO || packet[IPV6_HOP_LIMIT] != 255 {
        return None;
    }
    if !is_link_local(&packet[8..24]) {
        return None;
    }
    let dst = &packet[24..IPV6_HDR_LEN];
    if dst != ALL_NODES && !is_link_local(dst) {
        return None;
    }
    let icmp = &packet[IPV6_HDR_LEN..];
    if icmp[ICMPV6_TYPE] != RA_TYPE || icmp[ICMPV6_CODE] != 0 {
        return None;
    }
    let flags = icmp[ICMPV6_RA_FLAGS];
    Some(RaTrigger {
        managed: (flags & RA_FLAG_M) != 0,
        other: (flags & RA_FLAG_O) != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造最小合法 RA 全报文 (56 字节): 源 `fe80::1`, 目的 `ff02::1`,
    /// hop limit 255, type 134 / code 0, flags 由参数给定.
    fn ra_packet(flags: u8) -> [u8; 56] {
        let mut p = [0u8; 56];
        p[0] = 0x60; // IPv6 版本.
        p[4..6].copy_from_slice(&16u16.to_be_bytes()); // 载荷长 = RA 固定头.
        p[IPV6_NEXT_HEADER] = ICMPV6_PROTO;
        p[IPV6_HOP_LIMIT] = 255;
        p[8] = 0xfe; // 源 fe80::1.
        p[9] = 0x80;
        p[23] = 0x01;
        p[24..40].copy_from_slice(&ALL_NODES); // 目的 ff02::1.
        p[40] = RA_TYPE;
        p[44] = 64; // cur hop limit.
        p[45] = flags;
        p[46..48].copy_from_slice(&1800u16.to_be_bytes()); // router lifetime.
        p
    }

    #[test]
    fn managed_only() {
        let t = parse(&ra_packet(RA_FLAG_M)).unwrap();
        assert!(t.managed);
        assert!(!t.other);
    }

    #[test]
    fn other_only() {
        let t = parse(&ra_packet(RA_FLAG_O)).unwrap();
        assert!(!t.managed);
        assert!(t.other);
    }

    #[test]
    fn both_flags() {
        let t = parse(&ra_packet(RA_FLAG_M | RA_FLAG_O)).unwrap();
        assert!(t.managed && t.other);
    }

    #[test]
    fn neither_flag_is_valid_ra() {
        let t = parse(&ra_packet(0)).unwrap();
        assert!(!t.managed && !t.other);
    }

    #[test]
    fn unicast_link_local_dst_accepted() {
        let mut p = ra_packet(RA_FLAG_M);
        p[24..40].fill(0);
        p[24] = 0xfe; // 目的 fe80::2 (link-local 单播).
        p[25] = 0x80;
        p[39] = 0x02;
        assert!(parse(&p).unwrap().managed);
    }

    #[test]
    fn too_short_rejected() {
        let p = ra_packet(RA_FLAG_M);
        assert!(parse(&p[..55]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn wrong_next_header_rejected() {
        let mut p = ra_packet(RA_FLAG_M);
        p[IPV6_NEXT_HEADER] = 17; // UDP.
        assert!(parse(&p).is_none());
    }

    #[test]
    fn wrong_hop_limit_rejected() {
        let mut p = ra_packet(RA_FLAG_M);
        p[IPV6_HOP_LIMIT] = 64;
        assert!(parse(&p).is_none());
    }

    #[test]
    fn non_link_local_src_rejected() {
        let mut p = ra_packet(RA_FLAG_M);
        p[8] = 0x20; // 源 2001::1 (全局单播).
        p[9] = 0x01;
        assert!(parse(&p).is_none());
    }

    #[test]
    fn global_unicast_dst_rejected() {
        let mut p = ra_packet(RA_FLAG_M);
        p[24..40].fill(0);
        p[24] = 0x20; // 目的 2001::2 (非全节点 / 非 link-local).
        p[25] = 0x01;
        p[39] = 0x02;
        assert!(parse(&p).is_none());
    }

    #[test]
    fn wrong_icmp_type_rejected() {
        let mut p = ra_packet(RA_FLAG_M);
        p[40] = 135; // Neighbor Solicitation.
        assert!(parse(&p).is_none());
    }

    #[test]
    fn wrong_icmp_code_rejected() {
        let mut p = ra_packet(RA_FLAG_M);
        p[41] = 1;
        assert!(parse(&p).is_none());
    }
}
