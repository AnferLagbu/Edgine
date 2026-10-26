#![deny(unsafe_code)]
#![expect(
    clippy::unused_self,
    reason = "unused_self: 策略方法读写全局 tick 状态 (privileged clock_adj_state); 保留子系统方法形态以维持既有调用面"
)]
//! NTP/PTP 时钟同步 — functions 层权威实现 (0 unsafe)
//!
//! 自 privileged 下沉 (见 docs/plan §6.3): NTP/PTP 类型与纯算法、时钟调整策略、
//! `timesync` syscall 分发均为策略/业务, 可在 0 unsafe 下完整实装.
//!
//! 状态面仍留 privileged: [`crate::privileged::timer::time_sync::ClockAdjState`]
//! 由 tick 中断直接消费, 本层经
//! `privileged::timer::time_sync::clock_adj_state()` 的安全原子访问面读写;
//! 时钟读数经 privileged `timer::{get_ticks, ticks_to_ns}` 安全 API.
//!
//! ## 设计
//!
//! 1. **NTP 客户端**: 与 NTP 服务器通信, 计算偏移/延迟
//! 2. **PTP (IEEE 1588)**: 精密时间协议, 亚微秒级同步
//! 3. **时钟调整**: adjtime/adjfreq 渐进调整系统时钟
//! 4. **频率漂移补偿**: 跟踪本地时钟漂移, 自动补偿
//!
//! ### 与 Linux 的差异
//!
//! 1. **无 NTP 硬件时间戳**: 不支持 PPS/GPIO 硬件时间戳
//! 2. **无 PHC (PTP Hardware Clock)**: 仅软件实现
//! 3. **无 NTP API 兼容层**: 使用自定义 syscall
//! 4. **PLL/FLL 简化**: 使用比例控制器代替二阶 PLL

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::privileged::timer::time_sync::{ADJ_RATE_NS, clock_adj_state};

// ============================================================================
// 常量
// ============================================================================

/// NTP 纪元偏移: 1970-01-01 00:00:00 相对于 NTP 纪元 (1900-01-01) 的秒数
pub const NTP_EPOCH_OFFSET: u64 = 2208988800;

/// NTP 端口
pub const NTP_PORT: u16 = 123;

/// PTP 事件端口
pub const PTP_EVENT_PORT: u16 = 319;
/// PTP 普通端口
pub const PTP_GENERAL_PORT: u16 = 320;

/// 最大频率调整 (ppm, 百万分之一)
pub const MAX_FREQ_ADJUST_PPM: i64 = 500000; // ±500 ppm
/// 最大单次偏移调整 (ns)
pub const MAX_OFFSET_NS: i64 = 500_000_000; // 500ms

/// 比例控制器增益 (ppb per ns)
const KP_PPB_PER_NS: i64 = 2;

// ============================================================================
// NTP 时间戳
// ============================================================================

/// NTP 时间戳 (64-bit 秒 + 32-bit 小数)
#[derive(Debug, Clone, Copy, Default)]
pub struct NtpTimestamp {
    /// 秒部分 (自 NTP 纪元 1900-01-01 起)
    pub sec: u32,
    /// 小数部分 (2^-32 秒)
    pub frac: u32,
}

impl NtpTimestamp {
    /// 创建 NTP 时间戳
    pub fn new(sec: u32, frac: u32) -> Self {
        Self { sec, frac }
    }

    /// 从 Unix 时间 (ns) 转换
    pub fn from_unix_ns(ns: u64) -> Self {
        let sec = (ns / 1_000_000_000) as u32;
        let remainder = ns % 1_000_000_000;
        let frac = ((remainder as u64 * u64::MAX / 1_000_000_000) >> 32) as u32;
        Self {
            sec: sec.wrapping_add(NTP_EPOCH_OFFSET as u32),
            frac,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 转换为 Unix 时间 (ns)
    pub fn to_unix_ns(&self) -> u64 {
        let sec = u64::from(self.sec.wrapping_sub(NTP_EPOCH_OFFSET as u32));
        let ns = (u64::from(self.frac) * 1_000_000_000) >> 32;
        sec * 1_000_000_000 + ns
    }
}

// ============================================================================
// NTP 包头
// ============================================================================

/// NTP 包头 (48 字节)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct NtpPacket {
    /// LI(2) + VN(3) + Mode(3)
    pub li_vn_mode: u8,
    /// Stratum
    pub stratum: u8,
    /// Poll 间隔 (log2 秒)
    pub poll: i8,
    /// Precision (log2 秒)
    pub precision: i8,
    /// Root Delay
    pub root_delay: u32,
    /// Root Dispersion
    pub root_dispersion: u32,
    /// Reference ID
    pub ref_id: u32,
    /// Reference Timestamp
    pub ref_ts: NtpTimestamp,
    /// Origin Timestamp
    pub orig_ts: NtpTimestamp,
    /// Receive Timestamp
    pub recv_ts: NtpTimestamp,
    /// Transmit Timestamp
    pub xmit_ts: NtpTimestamp,
}

impl NtpPacket {
    /// 创建客户端请求包
    pub fn client_request() -> Self {
        Self {
            li_vn_mode: 0x23, // LI=0, VN=4, Mode=3 (client)
            stratum: 0,
            poll: 6, // 64s
            precision: -20,
            root_delay: 0,
            root_dispersion: 0,
            ref_id: 0,
            ref_ts: NtpTimestamp::default(),
            orig_ts: NtpTimestamp::default(),
            recv_ts: NtpTimestamp::default(),
            xmit_ts: NtpTimestamp::from_unix_ns(read_clock_ns()),
        }
    }
}

// ============================================================================
// NTP 同步结果
// ============================================================================

/// NTP 同步计算结果
#[derive(Debug, Clone, Copy, Default)]
pub struct NtpResult {
    /// 时钟偏移 (ns): 本地时钟相对 NTP 的偏差
    pub offset_ns: i64,
    /// 往返延迟 (ns)
    pub delay_ns: i64,
    /// 色散 (ns)
    pub dispersion_ns: i64,
    /// Stratum
    pub stratum: u8,
    /// 是否有效
    pub valid: bool,
}

impl NtpResult {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 从 NTP 交换计算偏移和延迟
    ///
    /// offset = ((T2 - T1) + (T3 - T4)) / 2
    /// delay  = (T4 - T1) - (T3 - T2)
    pub fn compute(
        t1: &NtpTimestamp, // 客户端发送时间
        t2: &NtpTimestamp, // 服务器接收时间
        t3: &NtpTimestamp, // 服务器发送时间
        t4: &NtpTimestamp, // 客户端接收时间
    ) -> Self {
        let t1_ns = t1.to_unix_ns() as i64;
        let t2_ns = t2.to_unix_ns() as i64;
        let t3_ns = t3.to_unix_ns() as i64;
        let t4_ns = t4.to_unix_ns() as i64;

        let offset = i64::midpoint(t2_ns - t1_ns, t3_ns - t4_ns);
        let delay = (t4_ns - t1_ns) - (t3_ns - t2_ns);

        Self {
            offset_ns: offset,
            delay_ns: delay.max(0),
            dispersion_ns: 0,
            stratum: 0,
            valid: true,
        }
    }
}

// ============================================================================
// PTP 消息类型
// ============================================================================

/// PTP 消息类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PtpMessageType {
    /// Sync 消息
    Sync = 0x0,
    /// DelayReq 消息
    DelayReq = 0x1,
    /// FollowUp 消息
    FollowUp = 0x8,
    /// DelayResp 消息
    DelayResp = 0x9,
    /// Announce 消息
    Announce = 0xB,
}

// ============================================================================
// 时间同步子系统 (策略面)
// ============================================================================

/// 时间同步子系统
///
/// 本结构仅持策略配置与初始化标志; 时钟调整状态 (tick 中断消费) 在
/// privileged `ClockAdjState`, 经 [`clock_adj_state`] 访问.
pub struct TimeSyncSubsystem {
    /// NTP 服务器地址 (IPv4, 网络字节序)
    ntp_server: AtomicU32,
    /// PTP 域号
    ptp_domain: AtomicU32,
    /// 是否已初始化
    initialized: AtomicBool,
}

impl TimeSyncSubsystem {
    /// 创建时间同步子系统
    pub const fn new() -> Self {
        Self {
            ntp_server: AtomicU32::new(0),
            ptp_domain: AtomicU32::new(0),
            initialized: AtomicBool::new(false),
        }
    }

    /// 初始化
    pub fn init(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        self.initialized.store(true, Ordering::Release);
        crate::privileged::klog_ffi!(
            klog_ffi_info,
            "[TimeSync] initialized: NTP/PTP clock synchronization ready"
        );
    }

    /// 应用 NTP 同步结果
    pub fn apply_ntp_result(&self, result: &NtpResult) -> bool {
        if !result.valid {
            return false;
        }

        let state = clock_adj_state();
        let now_ns = read_clock_ns();

        // 频率调整: 从偏移推导 (简化 PLL)
        // 比例控制器: freq_adj = -Kp * offset
        // Kp 选择使得在 poll 间隔内收敛
        let freq_adj = -result.offset_ns * KP_PPB_PER_NS;
        let clamped_freq = freq_adj.clamp(
            -MAX_FREQ_ADJUST_PPM * 1000, // ppm → ppb
            MAX_FREQ_ADJUST_PPM * 1000,
        );
        state.freq_adj_ppb().store(clamped_freq, Ordering::Release);
        state
            .total_freq_adj()
            .fetch_add(clamped_freq, Ordering::Relaxed);

        // 偏移调整: 渐进调整
        let offset = result.offset_ns.clamp(-MAX_OFFSET_NS, MAX_OFFSET_NS);
        if offset.abs() > ADJ_RATE_NS {
            // 大偏移: 渐进调整
            state.offset_remaining().store(offset, Ordering::Release);
        } else {
            // 小偏移: 直接跳变
            state
                .total_offset_adj()
                .fetch_add(offset, Ordering::Relaxed);
        }

        state.last_sync_time().store(now_ns, Ordering::Release);
        state.sync_count().fetch_add(1, Ordering::Relaxed);
        state.synced().store(true, Ordering::Release);

        crate::privileged::klog_ffi!(
            klog_ffi_info,
            "[TimeSync] NTP sync: offset={}ns delay={}ns freq_adj={}ppb",
            result.offset_ns,
            result.delay_ns,
            clamped_freq
        );
        true
    }

    /// 获取调整后的时间 (ns)
    pub fn get_adjusted_time_ns(&self) -> u64 {
        let raw_ns = read_clock_ns();
        let state = clock_adj_state();
        let offset = state.total_offset_adj().load(Ordering::Acquire);
        let freq = state.freq_adj_ppb().load(Ordering::Acquire);

        // 频率补偿: elapsed * freq_adj / 1e9
        let last_sync = state.last_sync_time().load(Ordering::Acquire);
        let elapsed = raw_ns.saturating_sub(last_sync);
        let freq_compensation = (elapsed as i64 * freq) / 1_000_000_000;

        let total_adj = offset + freq_compensation;
        if total_adj >= 0 {
            raw_ns.saturating_add(total_adj as u64)
        } else {
            raw_ns.saturating_sub((-total_adj) as u64)
        }
    }

    /// 手动设置频率调整 (ppb)
    pub fn adj_freq(&self, ppb: i64) -> bool {
        let state = clock_adj_state();
        let clamped = ppb.clamp(-MAX_FREQ_ADJUST_PPM * 1000, MAX_FREQ_ADJUST_PPM * 1000);
        state.freq_adj_ppb().store(clamped, Ordering::Release);
        state.total_freq_adj().store(clamped, Ordering::Relaxed);
        // 频率补偿以"本次调整时刻"为基准重算 (get_adjusted_time_ns 用
        // last_sync_time 计算 elapsed); 不更新基准会把启动以来的全部 elapsed
        // 都按新频率补偿, 导致读出的墙钟时间持续偏离.
        state
            .last_sync_time()
            .store(read_clock_ns(), Ordering::Release);
        true
    }

    /// 手动设置时间偏移 (ns, 渐进调整)
    pub fn adj_time(&self, offset_ns: i64) -> bool {
        let clamped = offset_ns.clamp(-MAX_OFFSET_NS, MAX_OFFSET_NS);
        clock_adj_state()
            .offset_remaining()
            .store(clamped, Ordering::Release);
        true
    }

    /// 直接设置系统时间 (ns)
    pub fn set_time(&self, time_ns: u64) -> bool {
        let state = clock_adj_state();
        let current = read_clock_ns();
        let diff = if time_ns > current {
            (time_ns - current) as i64
        } else {
            -((current - time_ns) as i64)
        };
        state.total_offset_adj().store(diff, Ordering::Release);
        state.offset_remaining().store(0, Ordering::Release);
        // 跳变后重置频率补偿基准 (同 adj_freq: 避免旧 elapsed 被重复补偿)
        state.last_sync_time().store(current, Ordering::Release);
        state.synced().store(true, Ordering::Release);
        true
    }

    /// 设置 NTP 服务器
    pub fn set_ntp_server(&self, addr: u32) {
        self.ntp_server.store(addr, Ordering::Release);
    }

    /// 获取 NTP 服务器
    pub fn get_ntp_server(&self) -> u32 {
        self.ntp_server.load(Ordering::Acquire)
    }

    /// 设置 PTP 域号
    pub fn set_ptp_domain(&self, domain: u8) {
        self.ptp_domain.store(u32::from(domain), Ordering::Release);
    }

    /// 获取同步状态
    pub fn get_sync_status(&self) -> (bool, u64, i64, i64) {
        let state = clock_adj_state();
        (
            state.synced().load(Ordering::Acquire),
            state.sync_count().load(Ordering::Acquire),
            state.freq_adj_ppb().load(Ordering::Acquire),
            state.total_offset_adj().load(Ordering::Acquire),
        )
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }
}

/// 读取原始时钟 (ns) — 经 privileged 时钟安全 API
fn read_clock_ns() -> u64 {
    crate::privileged::timer::ticks_to_ns(crate::privileged::timer::get_ticks())
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局时间同步子系统
static TIMESYNC: TimeSyncSubsystem = TimeSyncSubsystem::new();

/// 初始化时间同步
pub fn init() {
    TIMESYNC.init();
}

/// 获取全局时间同步子系统
pub fn subsystem() -> &'static TimeSyncSubsystem {
    &TIMESYNC
}

/// 时间同步是否已初始化
pub fn is_initialized() -> bool {
    TIMESYNC.is_initialized()
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_timesync` — 时间同步系统调用
///
/// `a0`: cmd
///   0 = `adj_freq(ppb`: a1 as i64)
///   1 = `adj_time(偏移纳秒数`: a1 as i64)
///   2 = `set_time(时间纳秒数`: a1)
///   3 = `set_ntp_server(addr`: a1 as u32)
///   4 = `get_ntp_server()` → addr
///   5 = `get_sync_status()` → (synced 位于位 48, count 位于位 16, `freq_adj` 低 16 位)
///   6 = `get_adjusted_time()` → ns
///   7 = `set_ptp_domain(domain`: a1 as u8)
///   8 = `apply_ntp_result(offset_ns`: a1, `delay_ns`: a2) — 简化接口
///   9 = `is_initialized()` → bool (是否已初始化)
pub fn timesync_syscall(cmd: u64, a1: u64, a2: u64) -> i64 {
    if !is_initialized() && cmd != 9 {
        return -11; // EAGAIN
    }

    match cmd {
        0 => {
            // adj_freq
            let ppb = a1 as i64;
            if subsystem().adj_freq(ppb) { 0 } else { -22 }
        }
        1 => {
            // adj_time
            let offset_ns = a1 as i64;
            if subsystem().adj_time(offset_ns) {
                0
            } else {
                -22
            }
        }
        2 => {
            // set_time
            if subsystem().set_time(a1) { 0 } else { -22 }
        }
        3 => {
            // set_ntp_server
            subsystem().set_ntp_server(a1 as u32);
            0
        }
        4 => {
            // get_ntp_server
            i64::from(subsystem().get_ntp_server())
        }
        5 => {
            // get_sync_status
            let (synced, count, freq_ppb, offset_ns) = subsystem().get_sync_status();
            let _ = offset_ns;
            let _ = freq_ppb;
            // 高16位: synced, 中32位: count, 低16位: 保留
            (i64::from(synced) << 48) | (count as i64 & 0xFFFF_FFFF_FFFF)
        }
        6 => {
            // get_adjusted_time
            subsystem().get_adjusted_time_ns() as i64
        }
        7 => {
            // set_ptp_domain
            subsystem().set_ptp_domain(a1 as u8);
            0
        }
        8 => {
            // apply_ntp_result (简化: 直接传 offset 和 delay)
            let result = NtpResult {
                offset_ns: a1 as i64,
                delay_ns: a2 as i64,
                dispersion_ns: 0,
                stratum: 1,
                valid: true,
            };
            if subsystem().apply_ntp_result(&result) {
                0
            } else {
                -22
            }
        }
        9 => {
            // is_initialized
            i64::from(is_initialized())
        }
        _ => -38, // ENOSYS
    }
}
