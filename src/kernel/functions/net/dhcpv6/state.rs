#![deny(unsafe_code)]
#![deny(missing_docs)]
//! `DHCPv6` SARR / 租约状态机 (RFC 8415, 事件驱动).
//!
//! 有状态形态: `Idle → Soliciting → Selecting → Requesting → Bound`, 租约维护
//! `Bound → Renewing → Rebinding`, 释放 `Bound → Releasing`. 无状态形态:
//! `Idle → InfoRequesting → InfoBound` (仅取 DNS/选项, 不分配地址).
//!
//! 机制与策略分离: 本状态机持协议状态 + 定时器, 重传/续约阈值决策委派
//! [`super::policy`], 报文编解码委派 [`super::wire`]. 输出 [`Output`] 指令
//! (待发包 / 地址落表 / 选项落配置 / 观察态) 交 privileged 执行, 自身不碰
//! smoltcp 句柄 (F1/F2). 全部 0 unsafe, 离线可测 (确定性时钟 + 事务标识).

use alloc::vec;
use alloc::vec::Vec;

use crate::privileged::net::iface_trait::Dhcpv6State;

use super::policy::{self, Dhcpv6Action, Dhcpv6PolicyConfig};
use super::wire::{IaAddr, IaNa, MessageType, OPT_DNS_SERVERS, OPT_INF_REFRESH_TIME, Opt, Repr};

// ============================================================================
// 输出指令
// ============================================================================

/// 待发包目的地 (privileged 据此选 UDP 单播 / 组播).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    /// 组播 `ff02::1:2` (`All_DHCP_Relay_Agents_and_Servers`).
    Multicast,
    /// 单播到指定服务器 IPv6 地址.
    Unicast([u8; 16]),
}

/// 一条待发送的 `DHCPv6` 报文 (字节 + 目的地).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxPacket {
    /// 已编码的 `DHCPv6` 报文字节 (privileged 包 UDP 头发送).
    pub bytes: Vec<u8>,
    /// 发送目的地.
    pub dest: Dest,
}

/// 地址落表指令 (privileged 经 `update_ip_addrs` 执行).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddrApply {
    /// 目标 IPv6 地址.
    pub ipv6: [u8; 16],
    /// 首选生命周期 (秒).
    pub preferred_s: u32,
    /// 有效生命周期 (秒).
    pub valid_s: u32,
    /// true = 移除地址 (释放 / 租约到期), false = 安装 / 刷新.
    pub remove: bool,
}

/// 选项落配置指令 (无状态 DNS / 刷新间隔).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionsApply {
    /// DNS 递归服务器 IPv6 地址列表.
    pub dns_servers: Vec<[u8; 16]>,
    /// 无状态信息刷新间隔 (秒, 若服务器提供).
    pub refresh_s: Option<u32>,
}

/// 状态机一次推进的输出指令集.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// 待发送报文 (可空).
    pub tx_packets: Vec<TxPacket>,
    /// 地址落表指令 (若有变更).
    pub apply_addr: Option<AddrApply>,
    /// 选项落配置指令 (若有变更).
    pub apply_options: Option<OptionsApply>,
    /// 推进后的观察态.
    pub state: Dhcpv6State,
}

// ============================================================================
// 客户端状态机
// ============================================================================

/// `DHCPv6` 客户端 (有状态 `IA_NA` + 无状态 Information-Request).
///
/// 由 privileged poll 循环驱动: [`Self::start_stateful`] / [`Self::start_stateless`]
/// 启动, [`Self::handle_packet`] 喂入收到的报文, [`Self::tick`] 推进定时器,
/// [`Self::release`] 释放租约. 各方法返回 [`Output`] 指令交 privileged 执行.
pub struct Dhcpv6Client {
    state: Dhcpv6State,
    cfg: Dhcpv6PolicyConfig,
    stateless: bool,
    client_duid: Vec<u8>,
    iaid: u32,
    transaction_id: [u8; 3],
    server_duid: Vec<u8>,
    server_addr: Option<[u8; 16]>,
    best_preference: u8,
    ipv6: [u8; 16],
    preferred_s: u32,
    valid_s: u32,
    bound_at_ms: u64,
    next_retry_at_ms: u64,
    retry_count: u32,
    dns_servers: Vec<[u8; 16]>,
    info_refresh_s: Option<u32>,
    info_bound_at_ms: u64,
}

impl Dhcpv6Client {
    /// 构造客户端. `transaction_id` 为初始事务标识种子 (privileged 供给熵).
    #[must_use]
    pub fn new(
        client_duid: Vec<u8>,
        iaid: u32,
        transaction_id: [u8; 3],
        cfg: Dhcpv6PolicyConfig,
    ) -> Self {
        Self {
            state: Dhcpv6State::Idle,
            cfg,
            stateless: false,
            client_duid,
            iaid,
            transaction_id,
            server_duid: Vec::new(),
            server_addr: None,
            best_preference: 0,
            ipv6: [0u8; 16],
            preferred_s: 0,
            valid_s: 0,
            bound_at_ms: 0,
            next_retry_at_ms: 0,
            retry_count: 0,
            dns_servers: Vec::new(),
            info_refresh_s: None,
            info_bound_at_ms: 0,
        }
    }

    /// 当前观察态.
    #[inline]
    #[must_use]
    pub fn state(&self) -> Dhcpv6State {
        self.state
    }

    /// 启动有状态 SARR (RA M=1 触发): 发 `Solicit`, 进入 `Soliciting`.
    pub fn start_stateful(&mut self, now_ms: u64) -> Output {
        self.stateless = false;
        let mut out = self.empty_output();
        self.begin_solicit(now_ms, &mut out);
        out.state = self.state;
        out
    }

    /// 启动无状态 Information-Request (RA O=1 触发): 仅取 DNS/选项.
    pub fn start_stateless(&mut self, now_ms: u64) -> Output {
        self.stateless = true;
        let mut out = self.empty_output();
        self.begin_info_request(now_ms, &mut out);
        out.state = self.state;
        out
    }

    /// 释放租约 (关闭触发): 移除地址并发 `Release`, 进入 `Releasing`.
    pub fn release(&mut self, now_ms: u64) -> Output {
        let mut out = self.empty_output();
        if self.state.is_configured() {
            out.apply_addr = Some(AddrApply {
                ipv6: self.ipv6,
                preferred_s: 0,
                valid_s: 0,
                remove: true,
            });
            self.state = Dhcpv6State::Releasing;
            self.retry_count = 0;
            self.advance_txid();
            Self::push_tx(&self.repr_release(), self.unicast_dest(), &mut out);
            self.next_retry_at_ms = now_ms + self.cfg.rel_timeout_ms;
        }
        out.state = self.state;
        out
    }

    /// 喂入收到的报文 (`src` = IPv6 源地址, privileged 提取; 用于学习服务器地址).
    ///
    /// 事务标识不匹配 / 解析失败 / 非预期消息类型均静默忽略 (不可信输入).
    pub fn handle_packet(&mut self, buf: &[u8], src: Option<[u8; 16]>, now_ms: u64) -> Output {
        let mut out = self.empty_output();
        let Ok(repr) = Repr::parse(buf) else {
            out.state = self.state;
            return out;
        };
        if repr.transaction_id != self.transaction_id {
            out.state = self.state;
            return out;
        }
        match repr.msg_type {
            MessageType::Advertise => self.process_advertise(&repr, src, &mut out),
            MessageType::Reply => self.process_reply(&repr, src, now_ms, &mut out),
            _ => {}
        }
        out.state = self.state;
        out
    }

    /// 推进定时器 (重传 / `T1`/`T2` 续约 / 租约到期 / 无状态刷新).
    pub fn tick(&mut self, now_ms: u64) -> Output {
        let mut out = self.empty_output();
        match self.state {
            Dhcpv6State::Idle | Dhcpv6State::Failed => {}
            Dhcpv6State::Soliciting | Dhcpv6State::Requesting | Dhcpv6State::InfoRequesting => {
                self.tick_discovery(now_ms, &mut out);
            }
            Dhcpv6State::Selecting => self.tick_selecting(now_ms, &mut out),
            Dhcpv6State::Bound { .. } => self.tick_bound(now_ms, &mut out),
            Dhcpv6State::Renewing { .. } => self.tick_renewing(now_ms, &mut out),
            Dhcpv6State::Rebinding { .. } => self.tick_rebinding(now_ms, &mut out),
            Dhcpv6State::Releasing => self.tick_releasing(now_ms, &mut out),
            Dhcpv6State::InfoBound => self.tick_info_bound(now_ms, &mut out),
        }
        out.state = self.state;
        out
    }

    // ------------------------------------------------------------------------
    // 内部: 启动 / 重启
    // ------------------------------------------------------------------------

    fn begin_solicit(&mut self, now_ms: u64, out: &mut Output) {
        self.state = Dhcpv6State::Soliciting;
        self.retry_count = 0;
        self.best_preference = 0;
        self.server_duid.clear();
        self.advance_txid();
        Self::push_tx(&self.repr_solicit(), Dest::Multicast, out);
        self.next_retry_at_ms = now_ms + self.cfg.sol_timeout_ms;
    }

    fn begin_info_request(&mut self, now_ms: u64, out: &mut Output) {
        self.state = Dhcpv6State::InfoRequesting;
        self.retry_count = 0;
        self.advance_txid();
        Self::push_tx(&self.repr_info_request(), Dest::Multicast, out);
        self.next_retry_at_ms = now_ms + self.cfg.inf_timeout_ms;
    }

    fn do_restart(&mut self, now_ms: u64, out: &mut Output) {
        if self.state.is_configured() {
            out.apply_addr = Some(AddrApply {
                ipv6: self.ipv6,
                preferred_s: 0,
                valid_s: 0,
                remove: true,
            });
        }
        if self.stateless {
            self.begin_info_request(now_ms, out);
        } else {
            self.begin_solicit(now_ms, out);
        }
    }

    // ------------------------------------------------------------------------
    // 内部: tick 分支
    // ------------------------------------------------------------------------

    fn tick_discovery(&mut self, now_ms: u64, out: &mut Output) {
        if now_ms < self.next_retry_at_ms {
            return;
        }
        let action = policy::decide(self.state, self.retry_count, 0, 0, self.cfg);
        if action == Dhcpv6Action::Restart {
            self.do_restart(now_ms, out);
            return;
        }
        let (repr, base, max_rt) = match self.state {
            Dhcpv6State::Soliciting => (
                self.repr_solicit(),
                self.cfg.sol_timeout_ms,
                self.cfg.sol_max_rt_ms,
            ),
            Dhcpv6State::Requesting => (
                self.repr_request(),
                self.cfg.req_timeout_ms,
                self.cfg.req_max_rt_ms,
            ),
            _ => (
                self.repr_info_request(),
                self.cfg.inf_timeout_ms,
                self.cfg.inf_max_rt_ms,
            ),
        };
        Self::push_tx(&repr, Dest::Multicast, out);
        self.retry_count += 1;
        let rt = policy::retransmit_timeout_ms(base, max_rt, self.retry_count);
        self.next_retry_at_ms = now_ms + policy::apply_jitter(rt, self.retry_count);
    }

    fn tick_selecting(&mut self, now_ms: u64, out: &mut Output) {
        self.state = Dhcpv6State::Requesting;
        self.retry_count = 0;
        Self::push_tx(&self.repr_request(), Dest::Multicast, out);
        self.next_retry_at_ms = now_ms + self.cfg.req_timeout_ms;
    }

    fn tick_bound(&mut self, now_ms: u64, out: &mut Output) {
        let elapsed = now_ms.saturating_sub(self.bound_at_ms);
        let lease = u64::from(self.valid_s) * 1_000;
        match policy::decide(self.state, 0, elapsed, lease, self.cfg) {
            Dhcpv6Action::Restart => self.do_restart(now_ms, out),
            Dhcpv6Action::Renew => {
                self.state = Dhcpv6State::Renewing { ipv6: self.ipv6 };
                self.retry_count = 0;
                self.advance_txid();
                Self::push_tx(&self.repr_renew(), self.unicast_dest(), out);
                self.next_retry_at_ms = now_ms + self.cfg.ren_timeout_ms;
            }
            Dhcpv6Action::Rebind => {
                self.state = Dhcpv6State::Rebinding { ipv6: self.ipv6 };
                self.retry_count = 0;
                self.advance_txid();
                Self::push_tx(&self.repr_rebind(), Dest::Multicast, out);
                self.next_retry_at_ms = now_ms + self.cfg.reb_timeout_ms;
            }
            _ => {}
        }
    }

    fn tick_renewing(&mut self, now_ms: u64, out: &mut Output) {
        let elapsed = now_ms.saturating_sub(self.bound_at_ms);
        let lease = u64::from(self.valid_s) * 1_000;
        if lease > 0 && elapsed >= lease {
            self.do_restart(now_ms, out);
            return;
        }
        if elapsed >= policy::t2_ms(lease, self.cfg.renew_t2_ratio) {
            self.state = Dhcpv6State::Rebinding { ipv6: self.ipv6 };
            self.retry_count = 0;
            self.advance_txid();
            Self::push_tx(&self.repr_rebind(), Dest::Multicast, out);
            self.next_retry_at_ms = now_ms + self.cfg.reb_timeout_ms;
            return;
        }
        if now_ms >= self.next_retry_at_ms {
            Self::push_tx(&self.repr_renew(), self.unicast_dest(), out);
            let rt = policy::retransmit_timeout_ms(
                self.cfg.ren_timeout_ms,
                self.cfg.ren_max_rt_ms,
                self.retry_count,
            );
            self.retry_count += 1;
            self.next_retry_at_ms = now_ms + policy::apply_jitter(rt, self.retry_count);
        }
    }

    fn tick_rebinding(&mut self, now_ms: u64, out: &mut Output) {
        let elapsed = now_ms.saturating_sub(self.bound_at_ms);
        let lease = u64::from(self.valid_s) * 1_000;
        if lease > 0 && elapsed >= lease {
            self.do_restart(now_ms, out);
            return;
        }
        if now_ms >= self.next_retry_at_ms {
            Self::push_tx(&self.repr_rebind(), Dest::Multicast, out);
            let rt = policy::retransmit_timeout_ms(
                self.cfg.reb_timeout_ms,
                self.cfg.reb_max_rt_ms,
                self.retry_count,
            );
            self.retry_count += 1;
            self.next_retry_at_ms = now_ms + policy::apply_jitter(rt, self.retry_count);
        }
    }

    fn tick_releasing(&mut self, now_ms: u64, out: &mut Output) {
        if now_ms < self.next_retry_at_ms {
            return;
        }
        if self.retry_count >= self.cfg.max_retries {
            self.state = Dhcpv6State::Idle;
            return;
        }
        Self::push_tx(&self.repr_release(), self.unicast_dest(), out);
        self.retry_count += 1;
        let rt = policy::retransmit_timeout_ms(
            self.cfg.rel_timeout_ms,
            self.cfg.rel_timeout_ms,
            self.retry_count,
        );
        self.next_retry_at_ms = now_ms + policy::apply_jitter(rt, self.retry_count);
    }

    fn tick_info_bound(&mut self, now_ms: u64, out: &mut Output) {
        let Some(refresh) = self.info_refresh_s else {
            return;
        };
        let elapsed = now_ms.saturating_sub(self.info_bound_at_ms);
        if elapsed >= u64::from(refresh) * 1_000 {
            self.begin_info_request(now_ms, out);
        }
    }

    // ------------------------------------------------------------------------
    // 内部: 报文处理
    // ------------------------------------------------------------------------

    fn process_advertise(&mut self, repr: &Repr, src: Option<[u8; 16]>, _out: &mut Output) {
        if !matches!(self.state, Dhcpv6State::Soliciting | Dhcpv6State::Selecting) {
            return;
        }
        let Some(server_duid) = find_server_id(repr) else {
            return;
        };
        let preference = find_preference(repr);
        if self.state == Dhcpv6State::Selecting && preference <= self.best_preference {
            return;
        }
        self.server_duid = server_duid.to_vec();
        self.best_preference = preference;
        if let Some(addr) = src {
            self.server_addr = Some(addr);
        }
        self.state = Dhcpv6State::Selecting;
    }

    fn process_reply(&mut self, repr: &Repr, src: Option<[u8; 16]>, now_ms: u64, out: &mut Output) {
        if let Some(addr) = src {
            self.server_addr = Some(addr);
        }
        match self.state {
            Dhcpv6State::Requesting => self.reply_request(repr, now_ms, out),
            Dhcpv6State::Renewing { .. } | Dhcpv6State::Rebinding { .. } => {
                self.reply_renew(repr, now_ms, out);
            }
            Dhcpv6State::InfoRequesting => self.reply_info(repr, now_ms, out),
            Dhcpv6State::Releasing => self.state = Dhcpv6State::Idle,
            _ => {}
        }
    }

    fn reply_request(&mut self, repr: &Repr, now_ms: u64, out: &mut Output) {
        if !self.server_id_matches(repr) {
            return;
        }
        let Some(ia_na) = find_ia_na(repr) else {
            self.do_restart(now_ms, out);
            return;
        };
        if ia_na_status(ia_na) != 0 {
            self.do_restart(now_ms, out);
            return;
        }
        let Some(addr) = ia_na_addr(ia_na) else {
            self.do_restart(now_ms, out);
            return;
        };
        self.bind(repr, addr, now_ms, out);
    }

    fn reply_renew(&mut self, repr: &Repr, now_ms: u64, out: &mut Output) {
        if !self.server_id_matches(repr) {
            return;
        }
        let Some(ia_na) = find_ia_na(repr) else {
            return;
        };
        if ia_na_status(ia_na) != 0 {
            self.do_restart(now_ms, out);
            return;
        }
        let Some(addr) = ia_na_addr(ia_na) else {
            return;
        };
        self.bind(repr, addr, now_ms, out);
    }

    fn reply_info(&mut self, repr: &Repr, now_ms: u64, out: &mut Output) {
        self.dns_servers = find_dns(repr);
        self.info_refresh_s = find_refresh(repr);
        self.info_bound_at_ms = now_ms;
        self.state = Dhcpv6State::InfoBound;
        out.apply_options = Some(OptionsApply {
            dns_servers: self.dns_servers.clone(),
            refresh_s: self.info_refresh_s,
        });
    }

    fn bind(&mut self, repr: &Repr, addr: IaAddr, now_ms: u64, out: &mut Output) {
        self.ipv6 = addr.addr;
        self.preferred_s = addr.preferred_lifetime;
        self.valid_s = addr.valid_lifetime;
        self.bound_at_ms = now_ms;
        self.retry_count = 0;
        self.state = Dhcpv6State::Bound {
            ipv6: addr.addr,
            lease_expires_at: self.lease_expires_at(),
        };
        out.apply_addr = Some(AddrApply {
            ipv6: addr.addr,
            preferred_s: addr.preferred_lifetime,
            valid_s: addr.valid_lifetime,
            remove: false,
        });
        let dns = find_dns(repr);
        if !dns.is_empty() {
            self.dns_servers.clone_from(&dns);
            out.apply_options = Some(OptionsApply {
                dns_servers: dns,
                refresh_s: find_refresh(repr),
            });
        }
    }

    // ------------------------------------------------------------------------
    // 内部: 报文构造 / 工具
    // ------------------------------------------------------------------------

    fn empty_output(&self) -> Output {
        Output {
            tx_packets: Vec::new(),
            apply_addr: None,
            apply_options: None,
            state: self.state,
        }
    }

    fn lease_expires_at(&self) -> u64 {
        self.bound_at_ms + u64::from(self.valid_s) * 1_000
    }

    fn unicast_dest(&self) -> Dest {
        match self.server_addr {
            Some(addr) => Dest::Unicast(addr),
            None => Dest::Multicast,
        }
    }

    fn advance_txid(&mut self) {
        let v = u32::from_be_bytes([
            0,
            self.transaction_id[0],
            self.transaction_id[1],
            self.transaction_id[2],
        ]);
        let next = v.wrapping_add(1) & 0x00FF_FFFF;
        let b = next.to_be_bytes();
        self.transaction_id = [b[1], b[2], b[3]];
    }

    fn server_id_matches(&self, repr: &Repr) -> bool {
        match find_server_id(repr) {
            Some(id) => id == self.server_duid.as_slice(),
            None => false,
        }
    }

    fn push_tx(repr: &Repr, dest: Dest, out: &mut Output) {
        let mut buf = [0u8; 1024];
        if let Ok(n) = repr.emit(&mut buf) {
            out.tx_packets.push(TxPacket {
                bytes: buf[..n].to_vec(),
                dest,
            });
        }
    }

    fn ia_na_request(&self) -> IaNa {
        IaNa {
            iaid: self.iaid,
            t1: 0,
            t2: 0,
            options: Vec::new(),
        }
    }

    fn ia_na_bound(&self) -> IaNa {
        IaNa {
            iaid: self.iaid,
            t1: 0,
            t2: 0,
            options: vec![Opt::IaAddr(IaAddr {
                addr: self.ipv6,
                preferred_lifetime: self.preferred_s,
                valid_lifetime: self.valid_s,
            })],
        }
    }

    fn repr_solicit(&self) -> Repr {
        Repr {
            msg_type: MessageType::Solicit,
            transaction_id: self.transaction_id,
            options: vec![
                Opt::ClientId(self.client_duid.clone()),
                Opt::IaNa(self.ia_na_request()),
                Opt::OptionRequest(vec![OPT_DNS_SERVERS]),
                Opt::ElapsedTime(0),
            ],
        }
    }

    fn repr_request(&self) -> Repr {
        Repr {
            msg_type: MessageType::Request,
            transaction_id: self.transaction_id,
            options: vec![
                Opt::ClientId(self.client_duid.clone()),
                Opt::ServerId(self.server_duid.clone()),
                Opt::IaNa(self.ia_na_request()),
                Opt::OptionRequest(vec![OPT_DNS_SERVERS]),
                Opt::ElapsedTime(0),
            ],
        }
    }

    fn repr_renew(&self) -> Repr {
        Repr {
            msg_type: MessageType::Renew,
            transaction_id: self.transaction_id,
            options: vec![
                Opt::ClientId(self.client_duid.clone()),
                Opt::ServerId(self.server_duid.clone()),
                Opt::IaNa(self.ia_na_bound()),
                Opt::OptionRequest(vec![OPT_DNS_SERVERS]),
                Opt::ElapsedTime(0),
            ],
        }
    }

    fn repr_rebind(&self) -> Repr {
        Repr {
            msg_type: MessageType::Rebind,
            transaction_id: self.transaction_id,
            options: vec![
                Opt::ClientId(self.client_duid.clone()),
                Opt::IaNa(self.ia_na_bound()),
                Opt::OptionRequest(vec![OPT_DNS_SERVERS]),
                Opt::ElapsedTime(0),
            ],
        }
    }

    fn repr_release(&self) -> Repr {
        Repr {
            msg_type: MessageType::Release,
            transaction_id: self.transaction_id,
            options: vec![
                Opt::ClientId(self.client_duid.clone()),
                Opt::ServerId(self.server_duid.clone()),
                Opt::IaNa(self.ia_na_bound()),
                Opt::ElapsedTime(0),
            ],
        }
    }

    fn repr_info_request(&self) -> Repr {
        Repr {
            msg_type: MessageType::InformationRequest,
            transaction_id: self.transaction_id,
            options: vec![
                Opt::ClientId(self.client_duid.clone()),
                Opt::OptionRequest(vec![OPT_DNS_SERVERS, OPT_INF_REFRESH_TIME]),
                Opt::ElapsedTime(0),
            ],
        }
    }
}

// ============================================================================
// 报文选项提取 (纯函数)
// ============================================================================

fn find_server_id(repr: &Repr) -> Option<&[u8]> {
    repr.options.iter().find_map(|o| match o {
        Opt::ServerId(id) => Some(id.as_slice()),
        _ => None,
    })
}

fn find_preference(repr: &Repr) -> u8 {
    repr.options
        .iter()
        .find_map(|o| match o {
            Opt::Preference(p) => Some(*p),
            _ => None,
        })
        .unwrap_or(0)
}

fn find_ia_na(repr: &Repr) -> Option<&IaNa> {
    repr.options.iter().find_map(|o| match o {
        Opt::IaNa(ia) => Some(ia),
        _ => None,
    })
}

fn ia_na_addr(ia_na: &IaNa) -> Option<IaAddr> {
    ia_na.options.iter().find_map(|o| match o {
        Opt::IaAddr(addr) => Some(*addr),
        _ => None,
    })
}

fn ia_na_status(ia_na: &IaNa) -> u16 {
    ia_na
        .options
        .iter()
        .find_map(|o| match o {
            Opt::StatusCode(sc) => Some(sc.code),
            _ => None,
        })
        .unwrap_or(0)
}

fn find_dns(repr: &Repr) -> Vec<[u8; 16]> {
    repr.options
        .iter()
        .find_map(|o| match o {
            Opt::DnsServers(servers) => Some(servers.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn find_refresh(repr: &Repr) -> Option<u32> {
    repr.options.iter().find_map(|o| match o {
        Opt::InformationRefreshTime(t) => Some(*t),
        _ => None,
    })
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::net::dhcpv6::policy::Dhcpv6PolicyConfig;
    use crate::functions::net::dhcpv6::wire::{IaAddr, IaNa, MessageType, Opt, Repr, StatusCode};
    use crate::privileged::net::iface_trait::Dhcpv6State;

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

    /// 完整 SARR: Solicit → Advertise → Request → Reply → Bound.
    #[test]
    fn test_full_sarr_to_bound() {
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
        assert!(out.tx_packets.is_empty());

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

    /// 已绑定客户端夹具 (bound_at=300, valid=7200s, 服务器地址已学习).
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

    /// Advertise 偏好选择: 取最高 preference 的服务器.
    #[test]
    fn test_advertise_preference_selection() {
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
        assert_eq!(sid, high);
    }

    /// T1 触发续约: Bound → Renewing, Renew 单播到服务器.
    #[test]
    fn test_bound_t1_renew_unicast() {
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

    /// Renew 收 Reply 刷新租约回到 Bound.
    #[test]
    fn test_renew_reply_refreshes_bound() {
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

    /// T2 触发重绑: Renewing → Rebinding, Rebind 组播.
    #[test]
    fn test_renewing_t2_rebind_multicast() {
        let mut c = bound_client();
        c.tick(300 + 3_600_000);
        let out = c.tick(300 + 6_300_000);
        assert!(matches!(c.state(), Dhcpv6State::Rebinding { .. }));
        assert_eq!(
            Repr::parse(&out.tx_packets[0].bytes).unwrap().msg_type,
            MessageType::Rebind
        );
        assert_eq!(out.tx_packets[0].dest, Dest::Multicast);
    }

    /// 租约到期: Rebinding → 释放地址并自主重启 SARR.
    #[test]
    fn test_lease_expiry_restart() {
        let mut c = bound_client();
        c.tick(300 + 3_600_000);
        c.tick(300 + 6_300_000);
        let out = c.tick(300 + 7_200_000);
        assert_eq!(c.state(), Dhcpv6State::Soliciting);
        let apply = out.apply_addr.unwrap();
        assert!(apply.remove);
        assert_eq!(apply.ipv6, ADDR);
        assert!(
            out.tx_packets
                .iter()
                .any(|p| Repr::parse(&p.bytes).unwrap().msg_type == MessageType::Solicit)
        );
    }

    /// 释放: Bound → Releasing (移除地址 + Release), Reply → Idle.
    #[test]
    fn test_release_and_ack() {
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

    /// 无状态: Information-Request → Reply (DNS/刷新) → InfoBound, 不落地址.
    #[test]
    fn test_stateless_info_bound() {
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
        assert!(out.apply_addr.is_none());
    }

    /// 无状态刷新间隔到期: InfoBound → InfoRequesting.
    #[test]
    fn test_stateless_refresh() {
        let mut c = client();
        let out = c.start_stateless(0);
        let txid = txid_of(&out);
        let repr = Repr {
            msg_type: MessageType::Reply,
            transaction_id: txid,
            options: vec![Opt::InformationRefreshTime(86400)],
        };
        c.handle_packet(&emit_repr(&repr), Some(SERVER_ADDR), 100);
        assert_eq!(c.state(), Dhcpv6State::InfoBound);
        let out = c.tick(100 + 86_400_000);
        assert_eq!(c.state(), Dhcpv6State::InfoRequesting);
        assert_eq!(
            Repr::parse(&out.tx_packets[0].bytes).unwrap().msg_type,
            MessageType::InformationRequest
        );
    }

    /// 发现期重传: 未到定时器不发, 到点重发 Solicit.
    #[test]
    fn test_discovery_retransmit_timing() {
        let mut c = client();
        c.start_stateful(0);
        let out = c.tick(999);
        assert!(out.tx_packets.is_empty());
        let out = c.tick(1000);
        assert_eq!(out.tx_packets.len(), 1);
        assert_eq!(
            Repr::parse(&out.tx_packets[0].bytes).unwrap().msg_type,
            MessageType::Solicit
        );
        assert_eq!(c.state(), Dhcpv6State::Soliciting);
    }

    /// 发现期重试耗尽: 自主重启 (事务标识前进).
    #[test]
    fn test_discovery_retry_exhaustion_restart() {
        let mut c = client();
        let out = c.start_stateful(0);
        let first_txid = txid_of(&out);
        let mut now = 0u64;
        let mut last_txid = first_txid;
        for _ in 0..9 {
            now += 200_000;
            let out = c.tick(now);
            if let Some(p) = out.tx_packets.first() {
                last_txid = Repr::parse(&p.bytes).unwrap().transaction_id;
            }
        }
        assert_ne!(last_txid, first_txid);
        assert_eq!(c.state(), Dhcpv6State::Soliciting);
    }

    /// 事务标识不匹配的报文被忽略.
    #[test]
    fn test_txid_mismatch_ignored() {
        let mut c = client();
        c.start_stateful(0);
        let out = c.handle_packet(
            &advertise([0xff, 0xff, 0xff], &server_duid(), 10),
            Some(SERVER_ADDR),
            100,
        );
        assert_eq!(c.state(), Dhcpv6State::Soliciting);
        assert!(out.tx_packets.is_empty());
    }

    /// 截断报文被忽略 (不可信输入).
    #[test]
    fn test_malformed_packet_ignored() {
        let mut c = client();
        c.start_stateful(0);
        let out = c.handle_packet(&[0x01, 0x02], Some(SERVER_ADDR), 100);
        assert_eq!(c.state(), Dhcpv6State::Soliciting);
        assert!(out.tx_packets.is_empty());
    }

    /// Reply 的 ServerID 与选定服务器不符时被忽略.
    #[test]
    fn test_reply_server_id_mismatch_ignored() {
        let mut c = client();
        let out = c.start_stateful(0);
        let txid = txid_of(&out);
        c.handle_packet(&advertise(txid, &server_duid(), 10), Some(SERVER_ADDR), 100);
        c.tick(200);
        let out = c.handle_packet(
            &reply_with_addr(txid, &[9, 9, 9, 9], ADDR, 3600, 7200, vec![]),
            Some(SERVER_ADDR),
            300,
        );
        assert_eq!(c.state(), Dhcpv6State::Requesting);
        assert!(out.apply_addr.is_none());
    }

    /// Reply 无 IAAddr (NoAddrsAvail) → 重启.
    #[test]
    fn test_reply_no_addr_restart() {
        let mut c = client();
        let out = c.start_stateful(0);
        let txid = txid_of(&out);
        c.handle_packet(&advertise(txid, &server_duid(), 10), Some(SERVER_ADDR), 100);
        c.tick(200);
        let repr = Repr {
            msg_type: MessageType::Reply,
            transaction_id: txid,
            options: vec![
                Opt::ServerId(server_duid()),
                Opt::IaNa(IaNa {
                    iaid: 7,
                    t1: 0,
                    t2: 0,
                    options: Vec::new(),
                }),
            ],
        };
        let out = c.handle_packet(&emit_repr(&repr), Some(SERVER_ADDR), 300);
        assert_eq!(c.state(), Dhcpv6State::Soliciting);
        assert!(out.apply_addr.is_none());
    }

    /// Reply 携带非零 StatusCode → 重启.
    #[test]
    fn test_reply_status_error_restart() {
        let mut c = client();
        let out = c.start_stateful(0);
        let txid = txid_of(&out);
        c.handle_packet(&advertise(txid, &server_duid(), 10), Some(SERVER_ADDR), 100);
        c.tick(200);
        let repr = Repr {
            msg_type: MessageType::Reply,
            transaction_id: txid,
            options: vec![
                Opt::ServerId(server_duid()),
                Opt::IaNa(IaNa {
                    iaid: 7,
                    t1: 0,
                    t2: 0,
                    options: vec![Opt::StatusCode(StatusCode {
                        code: 2,
                        message: Vec::new(),
                    })],
                }),
            ],
        };
        c.handle_packet(&emit_repr(&repr), Some(SERVER_ADDR), 300);
        assert_eq!(c.state(), Dhcpv6State::Soliciting);
    }

    /// 有状态 Reply 附带 DNS 时一并落选项.
    #[test]
    fn test_bound_reply_with_dns() {
        let mut c = client();
        let out = c.start_stateful(0);
        let txid = txid_of(&out);
        c.handle_packet(&advertise(txid, &server_duid(), 10), Some(SERVER_ADDR), 100);
        c.tick(200);
        let dns = vec![[
            0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x44,
        ]];
        let out = c.handle_packet(
            &reply_with_addr(txid, &server_duid(), ADDR, 3600, 7200, dns.clone()),
            Some(SERVER_ADDR),
            300,
        );
        assert!(matches!(c.state(), Dhcpv6State::Bound { .. }));
        assert_eq!(out.apply_options.unwrap().dns_servers, dns);
    }
}
