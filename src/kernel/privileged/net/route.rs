//! 路由表管理 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原路由表 CRUD/CIDR 匹配/syscall 于 T3-3 (2026-06-16) 迁至 `functions::net::route`,
//! 本文件保留 smoltcp 同步逻辑 + re-export。按"机制持有的数据结构/常量归
//! privileged"统一判据反转：`RouteEntry` 被 privileged smoltcp 同步机制直接消费、
//! `sys_route_*` 被 privileged syscall dispatch 直接调用 — 属机制项, 迁回。
//! 依赖闭包全在 privileged 内 (net::iface_trait + sync::IrqSpinLock + syscall::Errno)。
//!
//! functions 侧改 `pub use crate::privileged::net::route::*` 保持 API 兼容。
//! 本文件 0 unsafe (策略逻辑) + privileged smoltcp 同步 (依赖 raw::stack_mut)。

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::privileged::errno::Errno;
use crate::privileged::net::NET_CONFIGURED;
use crate::privileged::net::iface_trait::{IpAddr, Ipv4Addr, Ipv6Addr};
use crate::privileged::sync::IrqSpinLock;

// ============================================================================
// 常量
// ============================================================================

/// 最大路由条目数
pub const MAX_ROUTES: usize = 16;

// ============================================================================
// 路由条目
// ============================================================================

/// 路由条目 — 内核级路由表示 (双栈, DECISION-032)
#[derive(Debug, Clone)]
pub struct RouteEntry {
    /// 目标地址 (V4 或 V6)
    pub dest: IpAddr,
    /// 前缀长度 (V4: 0-32, V6: 0-128)
    pub prefix_len: u8,
    /// 下一跳网关 (V4 或 V6)
    pub gateway: IpAddr,
    /// 接口名 (可选)
    pub iface: Option<String>,
}

impl RouteEntry {
    /// 构造默认路由 (`::/0` via gateway)
    pub fn default_route(gateway: IpAddr) -> Self {
        let dest = match gateway {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        Self {
            dest,
            prefix_len: 0,
            gateway,
            iface: None,
        }
    }

    /// 是否为默认路由
    pub fn is_default(&self) -> bool {
        self.prefix_len == 0
            && match self.dest {
                IpAddr::V4(a) => a.is_unspecified(),
                IpAddr::V6(a) => a.is_unspecified(),
            }
    }
}

/// 路由查询结果
#[derive(Debug, Clone)]
pub struct RouteQueryResult {
    pub gateway: IpAddr,
    pub prefix_len: u8,
}

// ============================================================================
// 内核路由表
// ============================================================================

static KERNEL_ROUTE_TABLE: IrqSpinLock<Vec<RouteEntry>> = IrqSpinLock::new(Vec::new());

// ============================================================================
// 路由操作
// ============================================================================

/// 添加路由条目
///
/// # Errors
///
/// 当路由表已满时返回 `Err(Errno::ENOMEM)`; 当存在完全相同的 (dest, `prefix_len`, gateway)
/// 重复条目时返回 `Err(Errno::EEXIST)`; 同步到 smoltcp 路由表失败时返回对应的内核错误。
pub fn route_add(entry: RouteEntry) -> Result<(), Errno> {
    let mut table = KERNEL_ROUTE_TABLE.lock();

    if table.len() >= MAX_ROUTES {
        return Err(Errno::ENOMEM);
    }

    if table.iter().any(|r| {
        r.dest == entry.dest && r.prefix_len == entry.prefix_len && r.gateway == entry.gateway
    }) {
        return Err(Errno::EEXIST);
    }

    // 同步到 smoltcp 路由表
    if let Err(e) = sync_route_to_smoltcp(&entry) {
        return Err(e);
    }

    table.push(entry);
    Ok(())
}

/// 删除路由条目
///
/// # Errors
///
/// 当路由表中不存在与 (dest, `prefix_len`, gateway) 完全匹配的条目时返回 `Err(Errno::ENOENT)`。
pub fn route_del(dest: IpAddr, prefix_len: u8, gateway: IpAddr) -> Result<(), Errno> {
    let mut table = KERNEL_ROUTE_TABLE.lock();

    let idx = table
        .iter()
        .position(|r| r.dest == dest && r.prefix_len == prefix_len && r.gateway == gateway);

    idx.map_or(Err(Errno::ENOENT), |i| {
        table.remove(i);
        rebuild_smoltcp_routes(&table);
        Ok(())
    })
}

/// 查询路由 (最长前缀匹配, 按 family 分发)
pub fn route_query(dest: IpAddr) -> Option<RouteQueryResult> {
    let table = KERNEL_ROUTE_TABLE.lock();

    table
        .iter()
        .filter(|r| cidr_contains(r, &dest))
        .max_by_key(|r| r.prefix_len)
        .map(|r| RouteQueryResult {
            gateway: r.gateway,
            prefix_len: r.prefix_len,
        })
}

/// 列出所有路由条目
pub fn route_list() -> Vec<RouteEntry> {
    KERNEL_ROUTE_TABLE.lock().clone()
}

// ============================================================================
// CIDR 匹配 (V4: 32 位掩码 / V6: 128 位逐字节掩码)
// ============================================================================

fn cidr_contains(entry: &RouteEntry, dest: &IpAddr) -> bool {
    if entry.prefix_len == 0 {
        return true;
    }
    match (entry.dest, *dest) {
        (IpAddr::V4(net), IpAddr::V4(d)) => {
            let mask = if entry.prefix_len >= 32 {
                0xFF_FF_FF_FFu32
            } else {
                !((1u32 << (32 - entry.prefix_len)) - 1)
            };
            let net_dest = u32::from_be_bytes(d.octets()) & mask;
            let net_entry = u32::from_be_bytes(net.octets()) & mask;
            net_dest == net_entry
        }
        (IpAddr::V6(net), IpAddr::V6(d)) => ipv6_cidr_contains(&net, &d, entry.prefix_len),
        _ => false, // family 不匹配
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
/// IPv6 CIDR 匹配: 按 `prefix_len` (0-128) 逐字节掩码比较.
fn ipv6_cidr_contains(net: &Ipv6Addr, dest: &Ipv6Addr, prefix_len: u8) -> bool {
    let n = prefix_len as usize;
    if n >= 128 {
        return net == dest;
    }
    let full_bytes = n / 8;
    let rem_bits = n % 8;
    let n_octets = net.octets();
    let d_octets = dest.octets();
    if n_octets[..full_bytes] != d_octets[..full_bytes] {
        return false;
    }
    if rem_bits == 0 {
        return true;
    }
    // 剩余 1-7 位: 掩码 = 高 rem_bits 位为 1
    let mask = 0xFFu8 << (8 - rem_bits);
    (n_octets[full_bytes] & mask) == (d_octets[full_bytes] & mask)
}

// ============================================================================
// Syscall 接口 (保持 u32 ABI 兼容, 仅 IPv4; V6 路由走 route_add/route_query)
// ============================================================================

/// `sys_route_add` — 从 u32 ABI 参数添加 IPv4 路由 (前缀长度 > 32 返回 `-EINVAL`)
pub fn sys_route_add(dest_u32: u64, prefix_len: u64, gateway_u32: u64) -> i64 {
    let entry = RouteEntry {
        dest: IpAddr::V4(Ipv4Addr::from_octets((dest_u32 as u32).to_be_bytes())),
        prefix_len: if prefix_len > 32 {
            return -(Errno::EINVAL as i64);
        } else {
            prefix_len as u8
        },
        gateway: IpAddr::V4(Ipv4Addr::from_octets((gateway_u32 as u32).to_be_bytes())),
        iface: None,
    };

    match route_add(entry) {
        Ok(()) => 0,
        Err(e) => -(e as i64),
    }
}

/// `sys_route_del` — 从 u32 ABI 参数删除 IPv4 路由 (前缀长度 > 32 返回 `-EINVAL`)
pub fn sys_route_del(dest_u32: u64, prefix_len: u64, gateway_u32: u64) -> i64 {
    match route_del(
        IpAddr::V4(Ipv4Addr::from_octets((dest_u32 as u32).to_be_bytes())),
        if prefix_len > 32 {
            return -(Errno::EINVAL as i64);
        } else {
            prefix_len as u8
        },
        IpAddr::V4(Ipv4Addr::from_octets((gateway_u32 as u32).to_be_bytes())),
    ) {
        Ok(()) => 0,
        Err(e) => -(e as i64),
    }
}

/// `sys_route_query` — 查询 IPv4 目标的最长前缀匹配网关 (不可达返回 `-ENETUNREACH`)
pub fn sys_route_query(dest_u32: u64) -> i64 {
    route_query(IpAddr::V4(Ipv4Addr::from_octets(
        (dest_u32 as u32).to_be_bytes(),
    )))
    .map_or(-(Errno::ENETUNREACH as i64), |result| {
        match result.gateway {
            IpAddr::V4(v4) => i64::from(u32::from_be_bytes(v4.octets())),
            // V6 路由在 u32 syscall ABI 下不可表达, 视为不可达
            IpAddr::V6(_) => -(Errno::ENETUNREACH as i64),
        }
    })
}

// ============================================================================
// smoltcp 同步 (privileged 机制, 依赖 raw::stack_mut)
// ============================================================================

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
// J-01 (2026-09-08): manual_let_else expect 仅裸机分支 (not kernel_test) 生效 —
// kernel_test 分支为 stub (无 if-let/manual_let_else 模式), expect 在 feature 下 unfulfilled
#[cfg_attr(
    not(feature = "kernel_test"),
    expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )
)]
/// 将单条路由同步到 smoltcp Routes (双栈: V4/V6 按 family 分发)
///
/// # Errors
/// 当前实现不会返回 `Err`: 网络未配置、smoltcp 栈不可用或路由 family 不匹配时
/// 直接返回 `Ok(())` 并忽略该条目.
pub fn sync_route_to_smoltcp(entry: &RouteEntry) -> Result<(), Errno> {
    if !NET_CONFIGURED.load(Ordering::Acquire) {
        return Ok(());
    }

    #[cfg(not(feature = "kernel_test"))]
    {
        use crate::privileged::net::iface_trait::IpAddr;
        use crate::privileged::net::raw;
        use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address, Ipv4Cidr, Ipv6Address, Ipv6Cidr};

        let stack = match raw::stack_mut() {
            Some(s) => s,
            None => return Ok(()),
        };

        let (via_router, cidr) = match (entry.gateway, entry.dest) {
            (IpAddr::V4(gw), IpAddr::V4(dest)) => {
                let gw = Ipv4Address::new(
                    gw.octets()[0],
                    gw.octets()[1],
                    gw.octets()[2],
                    gw.octets()[3],
                );
                let cidr = Ipv4Cidr::new(
                    Ipv4Address::new(
                        dest.octets()[0],
                        dest.octets()[1],
                        dest.octets()[2],
                        dest.octets()[3],
                    ),
                    entry.prefix_len,
                );
                (IpAddress::Ipv4(gw), IpCidr::Ipv4(cidr))
            }
            (IpAddr::V6(gw), IpAddr::V6(dest)) => {
                let gw = Ipv6Address::from_octets(gw.octets());
                let cidr = Ipv6Cidr::new(Ipv6Address::from_octets(dest.octets()), entry.prefix_len);
                (IpAddress::Ipv6(gw), IpCidr::Ipv6(cidr))
            }
            // family 不匹配 (如 V4 目标 + V6 网关) — 忽略该条目
            _ => return Ok(()),
        };

        stack.iface.routes_mut().update(|routes| {
            let route = smoltcp::iface::Route {
                cidr,
                via_router,
                preferred_until: None,
                expires_at: None,
            };
            let _ = routes.push(route);
        });

        Ok(())
    }

    #[cfg(feature = "kernel_test")]
    {
        let _ = entry;
        Ok(())
    }
}

// J-01 (2026-09-08): manual_let_else expect 仅裸机分支 (not kernel_test) 生效 —
// kernel_test 分支为 stub (无 if-let/manual_let_else 模式), expect 在 feature 下 unfulfilled
#[cfg_attr(
    not(feature = "kernel_test"),
    expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )
)]
/// 从内核路由表全量重建 smoltcp Routes (双栈)
pub fn rebuild_smoltcp_routes(table: &[RouteEntry]) {
    #[cfg(not(feature = "kernel_test"))]
    {
        use crate::privileged::net::iface_trait::IpAddr;
        use crate::privileged::net::raw;
        use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address, Ipv4Cidr, Ipv6Address, Ipv6Cidr};

        if !NET_CONFIGURED.load(Ordering::Acquire) {
            return;
        }

        let stack = match raw::stack_mut() {
            Some(s) => s,
            None => return,
        };

        stack.iface.routes_mut().update(|routes| {
            routes.clear();
            for entry in table {
                let (via_router, cidr) = match (entry.gateway, entry.dest) {
                    (IpAddr::V4(gw), IpAddr::V4(dest)) => {
                        let gw = Ipv4Address::new(
                            gw.octets()[0],
                            gw.octets()[1],
                            gw.octets()[2],
                            gw.octets()[3],
                        );
                        let cidr = Ipv4Cidr::new(
                            Ipv4Address::new(
                                dest.octets()[0],
                                dest.octets()[1],
                                dest.octets()[2],
                                dest.octets()[3],
                            ),
                            entry.prefix_len,
                        );
                        (IpAddress::Ipv4(gw), IpCidr::Ipv4(cidr))
                    }
                    (IpAddr::V6(gw), IpAddr::V6(dest)) => {
                        let gw = Ipv6Address::from_octets(gw.octets());
                        let cidr = Ipv6Cidr::new(
                            Ipv6Address::from_octets(dest.octets()),
                            entry.prefix_len,
                        );
                        (IpAddress::Ipv6(gw), IpCidr::Ipv6(cidr))
                    }
                    _ => continue, // family 不匹配, 跳过
                };
                let route = smoltcp::iface::Route {
                    cidr,
                    via_router,
                    preferred_until: None,
                    expires_at: None,
                };
                let _ = routes.push(route);
            }
        });
    }

    #[cfg(feature = "kernel_test")]
    {
        let _ = table;
    }
}
