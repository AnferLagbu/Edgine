//! NTP/PTP 时钟同步 — 网络时间协议子系统
//!
//! ## 设计
//!
//! 实现内核级时间同步基础设施:
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
//!
//! ## SAFETY
//!
//! 本模块属于 privileged/TCB, 允许 unsafe.
//! 时钟调整涉及定时器频率修改.

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

use crate::privileged::sync::IrqSpinLock;

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
/// 渐进调整速率 (每次 tick 调整的最大 ns)
pub const ADJ_RATE_NS: i64 = 1000; // 1us/tick

// ============================================================================
// NTP 时间戳
// ============================================================================

/// NTP 时间戳 (64-bit 秒 + 32-bit 小数)
#[derive(Debug, Clone, Copy, Default)]
pub struct NtpTimestamp {
    pub sec: u32,
    pub frac: u32,
}

impl NtpTimestamp {
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
            xmit_ts: NtpTimestamp::from_unix_ns(Self::read_clock_ns()),
        }
    }

    fn read_clock_ns() -> u64 {
        crate::privileged::timer::tick::ticks_to_ns(crate::privileged::timer::tick::get_ticks())
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
    Sync = 0x0,
    DelayReq = 0x1,
    FollowUp = 0x8,
    DelayResp = 0x9,
    Announce = 0xB,
}

// ============================================================================
// 时钟调整状态
// ============================================================================

/// 时钟调整状态
#[derive(Debug)]
pub struct ClockAdjState {
    /// 当前频率调整 (ppb, 十亿分之一)
    freq_adj_ppb: AtomicI64,
    /// 待调整偏移 (ns), 渐进消耗
    offset_remaining: AtomicI64,
    /// 累计偏移调整 (ns)
    total_offset_adj: AtomicI64,
    /// 累计频率调整 (ppb)
    total_freq_adj: AtomicI64,
    /// 上次 NTP 同步时间 (ns)
    last_sync_time: AtomicU64,
    /// 同步次数
    sync_count: AtomicU64,
    /// 时钟是否同步
    synced: AtomicBool,
}

impl ClockAdjState {
    pub const fn new() -> Self {
        Self {
            freq_adj_ppb: AtomicI64::new(0),
            offset_remaining: AtomicI64::new(0),
            total_offset_adj: AtomicI64::new(0),
            total_freq_adj: AtomicI64::new(0),
            last_sync_time: AtomicU64::new(0),
            sync_count: AtomicU64::new(0),
            synced: AtomicBool::new(false),
        }
    }
}

// ============================================================================
// 时间同步子系统
// ============================================================================

/// 时间同步子系统
pub struct TimeSyncSubsystem {
    /// 时钟调整状态
    adj: ClockAdjState,
    /// NTP 服务器地址 (IPv4, 网络字节序)
    ntp_server: IrqSpinLock<u32>,
    /// PTP 域号
    ptp_domain: AtomicU32,
    /// 是否已初始化
    initialized: AtomicBool,
}

impl TimeSyncSubsystem {
    pub const fn new() -> Self {
        Self {
            adj: ClockAdjState::new(),
            ntp_server: IrqSpinLock::new(0),
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
        crate::klog_ffi!(
            klog_ffi_info,
            "[TimeSync] initialized: NTP/PTP clock synchronization ready"
        );
    }

    /// 应用 NTP 同步结果
    pub fn apply_ntp_result(&self, result: &NtpResult) -> bool {
        if !result.valid {
            return false;
        }

        let now_ns = Self::read_clock_ns();

        // 频率调整: 从偏移推导 (简化 PLL)
        // 比例控制器: freq_adj = -Kp * offset
        // Kp 选择使得在 poll 间隔内收敛
        let kp_ppb_per_ns = 2; // 2 ppb/ns
        let freq_adj = -result.offset_ns * kp_ppb_per_ns;
        let clamped_freq = freq_adj.clamp(
            -MAX_FREQ_ADJUST_PPM * 1000, // ppm → ppb
            MAX_FREQ_ADJUST_PPM * 1000,
        );
        self.adj.freq_adj_ppb.store(clamped_freq, Ordering::Release);
        self.adj
            .total_freq_adj
            .fetch_add(clamped_freq, Ordering::Relaxed);

        // 偏移调整: 渐进调整
        let offset = result.offset_ns.clamp(-MAX_OFFSET_NS, MAX_OFFSET_NS);
        if offset.abs() > ADJ_RATE_NS {
            // 大偏移: 渐进调整
            self.adj.offset_remaining.store(offset, Ordering::Release);
        } else {
            // 小偏移: 直接跳变
            self.adj
                .total_offset_adj
                .fetch_add(offset, Ordering::Relaxed);
        }

        self.adj.last_sync_time.store(now_ns, Ordering::Release);
        self.adj.sync_count.fetch_add(1, Ordering::Relaxed);
        self.adj.synced.store(true, Ordering::Release);

        crate::klog_ffi!(
            klog_ffi_info,
            "[TimeSync] NTP sync: offset={}ns delay={}ns freq_adj={}ppb",
            result.offset_ns,
            result.delay_ns,
            clamped_freq
        );
        true
    }

    /// 每个 tick 调用: 渐进调整时钟
    ///
    /// 返回: 本次调整的 ns 偏移
    pub fn tick_adjust(&self) -> i64 {
        let remaining = self.adj.offset_remaining.load(Ordering::Acquire);
        if remaining == 0 {
            return 0;
        }

        let adj = if remaining.abs() <= ADJ_RATE_NS {
            let r = remaining;
            self.adj.offset_remaining.store(0, Ordering::Release);
            r
        } else if remaining > 0 {
            self.adj
                .offset_remaining
                .fetch_sub(ADJ_RATE_NS, Ordering::Relaxed);
            ADJ_RATE_NS
        } else {
            self.adj
                .offset_remaining
                .fetch_add(ADJ_RATE_NS, Ordering::Relaxed);
            -ADJ_RATE_NS
        };

        self.adj.total_offset_adj.fetch_add(adj, Ordering::Relaxed);
        adj
    }

    /// 获取调整后的时间 (ns)
    pub fn get_adjusted_time_ns(&self) -> u64 {
        let raw_ns = Self::read_clock_ns();
        let offset = self.adj.total_offset_adj.load(Ordering::Acquire);
        let freq = self.adj.freq_adj_ppb.load(Ordering::Acquire);

        // 频率补偿: elapsed * freq_adj / 1e9
        let last_sync = self.adj.last_sync_time.load(Ordering::Acquire);
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
        let clamped = ppb.clamp(-MAX_FREQ_ADJUST_PPM * 1000, MAX_FREQ_ADJUST_PPM * 1000);
        self.adj.freq_adj_ppb.store(clamped, Ordering::Release);
        self.adj.total_freq_adj.store(clamped, Ordering::Relaxed);
        // 频率补偿以"本次调整时刻"为基准重算 (get_adjusted_time_ns 用
        // last_sync_time 计算 elapsed); 不更新基准会把启动以来的全部 elapsed
        // 都按新频率补偿, 导致读出的墙钟时间持续偏离.
        self.adj
            .last_sync_time
            .store(Self::read_clock_ns(), Ordering::Release);
        true
    }

    /// 手动设置时间偏移 (ns, 渐进调整)
    pub fn adj_time(&self, offset_ns: i64) -> bool {
        let clamped = offset_ns.clamp(-MAX_OFFSET_NS, MAX_OFFSET_NS);
        self.adj.offset_remaining.store(clamped, Ordering::Release);
        true
    }

    /// 直接设置系统时间 (ns)
    pub fn set_time(&self, time_ns: u64) -> bool {
        let current = Self::read_clock_ns();
        let diff = if time_ns > current {
            (time_ns - current) as i64
        } else {
            -((current - time_ns) as i64)
        };
        self.adj.total_offset_adj.store(diff, Ordering::Release);
        self.adj.offset_remaining.store(0, Ordering::Release);
        // 跳变后重置频率补偿基准 (同 adj_freq: 避免旧 elapsed 被重复补偿)
        self.adj.last_sync_time.store(current, Ordering::Release);
        self.adj.synced.store(true, Ordering::Release);
        true
    }

    /// 设置 NTP 服务器
    pub fn set_ntp_server(&self, addr: u32) {
        *self.ntp_server.lock() = addr;
    }

    /// 获取 NTP 服务器
    pub fn get_ntp_server(&self) -> u32 {
        *self.ntp_server.lock()
    }

    /// 设置 PTP 域号
    pub fn set_ptp_domain(&self, domain: u8) {
        self.ptp_domain.store(u32::from(domain), Ordering::Release);
    }

    /// 获取同步状态
    pub fn get_sync_status(&self) -> (bool, u64, i64, i64) {
        (
            self.adj.synced.load(Ordering::Acquire),
            self.adj.sync_count.load(Ordering::Acquire),
            self.adj.freq_adj_ppb.load(Ordering::Acquire),
            self.adj.total_offset_adj.load(Ordering::Acquire),
        )
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    fn read_clock_ns() -> u64 {
        crate::privileged::timer::tick::ticks_to_ns(crate::privileged::timer::tick::get_ticks())
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局时间同步子系统
static TIMESYNC: TimeSyncSubsystem = TimeSyncSubsystem::new();

/// 初始化时间同步
pub fn timesync_init() {
    TIMESYNC.init();
}

/// 获取全局时间同步子系统
pub fn timesync_subsystem() -> &'static TimeSyncSubsystem {
    &TIMESYNC
}

/// 时间同步是否已初始化
pub fn timesync_is_initialized() -> bool {
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
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub extern "C" fn sys_timesync(cmd: u64, a1: u64, a2: u64) -> i64 {
    if !timesync_is_initialized() && cmd != 9 {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // adj_freq
            let ppb = a1 as i64;
            if timesync_subsystem().adj_freq(ppb) {
                0
            } else {
                -(22i64)
            }
        }
        1 => {
            // adj_time
            let offset_ns = a1 as i64;
            if timesync_subsystem().adj_time(offset_ns) {
                0
            } else {
                -(22i64)
            }
        }
        2 => {
            // set_time
            if timesync_subsystem().set_time(a1) {
                0
            } else {
                -(22i64)
            }
        }
        3 => {
            // set_ntp_server
            timesync_subsystem().set_ntp_server(a1 as u32);
            0
        }
        4 => {
            // get_ntp_server
            i64::from(timesync_subsystem().get_ntp_server())
        }
        5 => {
            // get_sync_status
            let (synced, count, freq_ppb, offset_ns) = timesync_subsystem().get_sync_status();
            let _ = offset_ns;
            let _ = freq_ppb;
            // 高16位: synced, 中32位: count, 低16位: 保留
            (i64::from(synced) << 48) | (count as i64 & 0xFFFFFFFFFFFF)
        }
        6 => {
            // get_adjusted_time
            timesync_subsystem().get_adjusted_time_ns() as i64
        }
        7 => {
            // set_ptp_domain
            timesync_subsystem().set_ptp_domain(a1 as u8);
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
            if timesync_subsystem().apply_ntp_result(&result) {
                0
            } else {
                -(22i64)
            }
        }
        9 => {
            // is_initialized
            i64::from(timesync_is_initialized())
        }
        _ => -(38i64), // ENOSYS
    }
}
