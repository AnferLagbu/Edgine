//! `DHCPv6` 运行时策略配置私有 syscall ABI 常量与结构.
//!
//! 值域取 Edgine 私有功能段 `816-817` (紧邻 DNS `812-814` 与 `io_uring` `815`,
//! 落在 cgroup `830` 前的空闲段): Linux 无 `DHCPv6` 客户端配置系统调用 (客户端
//! 在用户态守护进程, 经 rtnetlink / 配置文件管理), 占用 Linux 公开号位既无
//! 兼容收益, 又破坏 "Linux 公开段 == Linux" 的承重不变式, 故按私有段取值 (与
//! `edgine_abi::net` 同构).
//!
//! 内核 `privileged::syscall::types` 的编号定义、`functions::net::syscall`
//! 的配置读写与用户态入口桩 (`userlib::sys`) 均引用本模块, 结构上避免
//! "内核与用户态各写一份常量" 的漂移.
//!
//! 设计立场见 `docs/design/service-placement-design.md`: 内核 `DHCPv6` 客户端
//! 是引导过渡形态, `enabled` 关闭开关是用户态守护接管前的第一个共存钩子.

/// `EG_DHCPV6_SET_POLICY` — 设置 `DHCPv6` 运行时策略配置 + 启用开关.
///
/// 参数: (`policy_ptr`: u64 指向 [`EgDhcpv6Policy`]). 成功返 0; 配置字段非法
/// 返 `-EINVAL`; 用户缓冲非法返 `-EFAULT`. 校验由内核 `dhcpv6::policy` 纯函数
/// 承担 (重传间隔正性 / 上限不小于基值 / 重试次数有界 / `T1`/`T2` 万分比序).
pub const EG_DHCPV6_SET_POLICY: u64 = crate::linux_syscall::PRIVATE_BASE + 816;

/// `EG_DHCPV6_GET_POLICY` — 读取当前 `DHCPv6` 运行时策略配置 + 启用开关.
///
/// 参数: (`policy_ptr`: u64 指向 [`EgDhcpv6Policy`]). 成功返 0 并写出当前配置
/// (客户端未构造时返待生效配置或默认值); 用户缓冲非法返 `-EFAULT`.
pub const EG_DHCPV6_GET_POLICY: u64 = crate::linux_syscall::PRIVATE_BASE + 817;

/// `DHCPv6` 运行时策略配置 (`EG_DHCPV6_SET_POLICY` / `EG_DHCPV6_GET_POLICY`
/// 的读写结构).
///
/// 字段与内核 `dhcpv6::policy::Dhcpv6PolicyConfig` 一一对应 (RFC 8415 §7.6
/// 重传常数 + 续约阈值), 另附 `enabled` 启用开关. 重传间隔单位毫秒; `T1`/`T2`
/// 阈值以万分比表示 (0-10000). `enabled` 取 0 (关闭, 内核客户端惰性) / 1 (启用).
///
/// 布局: 11 个 `u64` 重传间隔在前, 4 个 `u32` (重试次数 / `T1` / `T2` / 开关)
/// 在后, `repr(C)` 无隐式填充.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgDhcpv6Policy {
    /// `Solicit` 初始重传间隔 (ms, RFC `SOL_TIMEOUT`).
    pub sol_timeout_ms: u64,
    /// `Solicit` 重传间隔上限 (ms, RFC `SOL_MAX_RT`).
    pub sol_max_rt_ms: u64,
    /// `Request` 初始重传间隔 (ms, RFC `REQ_TIMEOUT`).
    pub req_timeout_ms: u64,
    /// `Request` 重传间隔上限 (ms, RFC `REQ_MAX_RT`).
    pub req_max_rt_ms: u64,
    /// `Renew` 初始重传间隔 (ms, RFC `REN_TIMEOUT`).
    pub ren_timeout_ms: u64,
    /// `Renew` 重传间隔上限 (ms, RFC `REN_MAX_RT`).
    pub ren_max_rt_ms: u64,
    /// `Rebind` 初始重传间隔 (ms, RFC `REB_TIMEOUT`).
    pub reb_timeout_ms: u64,
    /// `Rebind` 重传间隔上限 (ms, RFC `REB_MAX_RT`).
    pub reb_max_rt_ms: u64,
    /// `Information-Request` 初始重传间隔 (ms, RFC `INF_TIMEOUT`).
    pub inf_timeout_ms: u64,
    /// `Information-Request` 重传间隔上限 (ms, RFC `INF_MAX_RT`).
    pub inf_max_rt_ms: u64,
    /// `Release` 初始重传间隔 (ms, RFC `REL_TIMEOUT`).
    pub rel_timeout_ms: u64,
    /// 发现阶段最大重传次数 (有界, 耗尽转重启).
    pub max_retries: u32,
    /// 续约 `T1` 阈值 (有效租期万分比, 0-10000).
    pub renew_t1_ratio: u32,
    /// 续约 `T2` 阈值 (有效租期万分比, 0-10000, 不小于 `T1`).
    pub renew_t2_ratio: u32,
    /// 启用开关: 0 = 关闭内核客户端 (用户态守护接管), 1 = 启用.
    pub enabled: u32,
}
