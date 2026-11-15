//! DNS 查询电平重扫等待登记 (E2 B2: 免 smoltcp async 的真异步阻塞).
//!
//! DNS 查询不是 socket fd, 无法复用 `wait_queue.rs` 的 per-fd 等待表, 故另设
//! 一张定长登记表 (容量 = smoltcp `dns::Socket` 查询槽数 `DNS_QUERY_SLOTS`).
//! 每槽承载一个阻塞中的进程查询: `pid` + smoltcp `QueryHandle` + 结果 + 死线.
//!
//! ## 为什么结果由 poll 侧消费 (B2 核心约束)
//!
//! smoltcp `get_query_result` 在 Completed/Failed 时**释放查询槽** (无公开
//! 非消费 peek), 故只能由持 `NET_STATE` 锁的 `poll_network` 每 tick 调用一次:
//! 完成即把结果落进本表并采集 pid, 释放锁后 `scheduler_unblock`. 消费者
//! (`dns_query_net`) 醒来后从本表读结果, 不再自行 `get_query_result`.
//!
//! ## 锁序 (F8)
//!
//! 本表唯一互斥域是 `NET_STATE` (层级 12): 登记/读取/排空全部在锁内.
//! `poll_network` 锁内采集 pid → 释放锁 → `scheduler_unblock` (层级 8),
//! 禁 `NET_STATE → SCHEDULER` 嵌套. 丢唤醒由下一 tick 电平重扫自愈:
//! 终态槽在被消费者摘除前每 tick 重复采集 pid (非破坏, 同 `wait_queue`).

use core::cell::UnsafeCell;

use smoltcp::iface::SocketSet;
use smoltcp::socket::dns as smoltcp_dns;
use smoltcp::wire::IpAddress;

use super::dns::DNS_QUERY_SLOTS;
use super::raw;
use crate::privileged::net::iface_trait::{IpAddr, Ipv4Addr, Ipv6Addr};

/// 阻塞查询的绝对超时死线时长 (纳秒). 到点由 `drain` 取消查询并唤醒消费者.
pub(crate) const DNS_QUERY_TIMEOUT_NS: u64 = 5_000_000_000;

/// 查询等待状态机. `Pending` 期间 smoltcp 槽仍占用; 三个终态均已释放或取消.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DnsWaitState {
    /// 已提交, 等待 smoltcp 应答 (poll 侧尚未消费到结果).
    Pending,
    /// 查询成功, `result` 已落表.
    Done,
    /// 查询失败 (smoltcp Failure).
    Failed,
    /// 死线到点, 查询已取消.
    Timeout,
}

/// 单个等待槽. `pid == 0` 表示空槽 (可登记).
#[derive(Clone, Copy)]
struct DnsWaitEntry {
    pid: u32,
    handle: Option<smoltcp_dns::QueryHandle>,
    state: DnsWaitState,
    result: Option<IpAddr>,
    deadline_ns: u64,
}

impl DnsWaitEntry {
    const EMPTY: Self = Self {
        pid: 0,
        handle: None,
        state: DnsWaitState::Pending,
        result: None,
        deadline_ns: 0,
    };
}

/// 定长登记表. 全部访问经 [`registry`] 在 `NET_STATE` 锁内进行.
struct DnsWaitRegistry {
    entries: [DnsWaitEntry; DNS_QUERY_SLOTS],
}

/// `UnsafeCell` 包装: `static` 要求 `Sync`, 而 `UnsafeCell<T>` 非 `Sync`.
struct DnsWaitCell(UnsafeCell<DnsWaitRegistry>);

// SAFETY: 本表唯一访问路径是 registry(), 且全部在 NET_STATE (IrqSpinLock) 锁内
// 串行进行 —— 锁保证跨核互斥, 无锁外共享可变访问, 故共享安全 (同 NetState 契约).
unsafe impl Sync for DnsWaitCell {}

static DNS_WAIT: DnsWaitCell = DnsWaitCell(UnsafeCell::new(DnsWaitRegistry {
    entries: [DnsWaitEntry::EMPTY; DNS_QUERY_SLOTS],
}));

/// 取登记表可变引用.
///
/// # Safety
/// 调用方必须持 `NET_STATE` 锁 —— 本表无独立锁, `NET_STATE` 是其唯一互斥域,
/// 锁内独占访问保证 `&mut` 不产生别名 (同 `socket_set()` 契约).
unsafe fn registry() -> &'static mut DnsWaitRegistry {
    // SAFETY: 调用方持 NET_STATE 锁 (见函数 Safety 契约), 锁内独占.
    unsafe { &mut *DNS_WAIT.0.get() }
}

/// 登记一个阻塞查询, 返回槽索引; 表满返 `None` (调用方应取消查询并回退).
///
/// # Safety
/// 调用方必须持 `NET_STATE` 锁.
pub(crate) unsafe fn register(
    pid: u32,
    handle: smoltcp_dns::QueryHandle,
    deadline_ns: u64,
) -> Option<usize> {
    // SAFETY: 调用方持 NET_STATE 锁 (见函数 Safety 契约).
    let reg = unsafe { registry() };
    let idx = reg.entries.iter().position(|e| e.pid == 0)?;
    reg.entries[idx] = DnsWaitEntry {
        pid,
        handle: Some(handle),
        state: DnsWaitState::Pending,
        result: None,
        deadline_ns,
    };
    Some(idx)
}

/// 读槽状态与结果 (非破坏, 供消费者醒来后重判).
///
/// # Safety
/// 调用方必须持 `NET_STATE` 锁.
pub(crate) unsafe fn status(idx: usize) -> (DnsWaitState, Option<IpAddr>) {
    // SAFETY: 调用方持 NET_STATE 锁.
    let e = unsafe { registry() }.entries[idx];
    (e.state, e.result)
}

/// 读槽承载的 smoltcp 查询句柄 (供消费者超时/信号路径取消查询).
///
/// # Safety
/// 调用方必须持 `NET_STATE` 锁.
pub(crate) unsafe fn handle_of(idx: usize) -> Option<smoltcp_dns::QueryHandle> {
    // SAFETY: 调用方持 NET_STATE 锁.
    unsafe { registry() }.entries[idx].handle
}

/// 摘除槽 (消费者读毕/放弃时自摘, 释放登记位).
///
/// # Safety
/// 调用方必须持 `NET_STATE` 锁.
pub(crate) unsafe fn free(idx: usize) {
    // SAFETY: 调用方持 NET_STATE 锁.
    unsafe { registry() }.entries[idx] = DnsWaitEntry::EMPTY;
}

/// 把 smoltcp 地址列表按地址族映射为 `IpAddr` (`DNS_MAX_RESULT_COUNT=1`, 取首个).
fn map_first(addrs: &[IpAddress]) -> Option<IpAddr> {
    addrs.iter().next().map(|ip| match ip {
        IpAddress::Ipv4(v4) => IpAddr::V4(Ipv4Addr(v4.octets())),
        IpAddress::Ipv6(v6) => IpAddr::V6(Ipv6Addr(v6.octets())),
    })
}

/// 电平重扫排空: 每 tick 由 `poll_network` 在 `NET_STATE` 锁内调用.
///
/// 对每个占用槽: `Pending` 则消费 `get_query_result` 推进状态机 (完成/失败
/// 落终态, 死线到点取消查询置 `Timeout`); 终态槽把 pid 采集进 `to_wake`
/// (非破坏, 消费者摘除前每 tick 重采 ⇒ 丢唤醒自愈). 返回本次采集的 pid 数.
///
/// # Safety
/// 调用方必须持 `NET_STATE` 锁, 且 `sockets` 为该锁域内的 `SocketSet`.
pub(crate) unsafe fn drain(sockets: &mut SocketSet<'_>, now_ns: u64, to_wake: &mut [u32]) -> usize {
    // SAFETY: 调用方持 NET_STATE 锁 (见函数 Safety 契约).
    let reg = unsafe { registry() };
    let Some(dns_handle) = raw::dns_handle() else {
        return 0;
    };
    let mut n = 0usize;
    for entry in &mut reg.entries {
        if entry.pid == 0 {
            continue;
        }
        if entry.state == DnsWaitState::Pending {
            let Some(handle) = entry.handle else {
                continue;
            };
            let socket = sockets.get_mut::<smoltcp_dns::Socket>(dns_handle);
            match socket.get_query_result(handle) {
                Ok(addrs) => {
                    entry.state = DnsWaitState::Done;
                    entry.result = map_first(&addrs);
                }
                Err(smoltcp_dns::GetQueryResultError::Failed) => {
                    entry.state = DnsWaitState::Failed;
                }
                Err(smoltcp_dns::GetQueryResultError::Pending) => {
                    if now_ns >= entry.deadline_ns {
                        socket.cancel_query(handle);
                        entry.state = DnsWaitState::Timeout;
                    }
                }
            }
        }
        // 终态 (含本 tick 刚推进者): 采集 pid, 释放锁后由 poll_network 唤醒.
        if entry.state != DnsWaitState::Pending && n < to_wake.len() {
            to_wake[n] = entry.pid;
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::wire::{Ipv4Address, Ipv6Address};

    /// B2: IPv4 结果按地址族映射为 `IpAddr::V4`, octets 原样保留.
    #[test]
    fn map_first_ipv4() {
        let addrs = [IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 3))];
        assert_eq!(map_first(&addrs), Some(IpAddr::V4(Ipv4Addr([10, 0, 2, 3]))));
    }

    /// B2: IPv6 结果映射为 `IpAddr::V6`, 16 字节原样保留.
    #[test]
    fn map_first_ipv6() {
        let octets = [
            0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
        ];
        let addrs = [IpAddress::Ipv6(Ipv6Address::from_octets(octets))];
        assert_eq!(map_first(&addrs), Some(IpAddr::V6(Ipv6Addr(octets))));
    }

    /// B2: 空列表返 None.
    #[test]
    fn map_first_empty() {
        assert_eq!(map_first(&[]), None);
    }
}
