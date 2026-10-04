#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 时钟 syscall 策略 — services 层统一时间模块 (B05-26 归位)
//!
//! 集中 `clock_gettime` / `gettimeofday` / `settimeofday` /
//! `clock_nanosleep` / `adjtimex`, 消除此前分散在 fs/file_ops 与
//! proc/info 的职责错位.
//!
//! ## 时间基准
//!
//! - `CLOCK_MONOTONIC`: 自启动起的原始 tick 计数 (只读, 不可设置)
//! - `CLOCK_REALTIME` / `gettimeofday`: 原始 tick 计数 + `framework::timer`
//!   时间同步机制持有的墙钟偏移 (由 `settimeofday` / `adjtimex` 调整)

use crate::framework::syscall::Errno;

/// POSIX 时钟 ID: 墙钟 (可被 `settimeofday` / `adjtimex` 调整)
pub const CLOCK_REALTIME: i32 = 0;
/// POSIX 时钟 ID: 自启动单调时钟 (只读)
pub const CLOCK_MONOTONIC: i32 = 1;

/// `clock_nanosleep` 标志: 请求时间为绝对时刻
/// (与 framework `TFD_TIMER_ABSTIME` 同值, 分属两套 syscall 标志空间)
pub const TIMER_ABSTIME: i32 = 1;

/// 每毫秒的纳秒数 (tick 计数为 ms 精度)
const NS_PER_MS: u64 = 1_000_000;
/// 每秒的纳秒数
const NS_PER_SEC: u64 = 1_000_000_000;
/// 每微秒的纳秒数
const NS_PER_USEC: u64 = 1_000;
/// 每秒的微秒数 (timeval `tv_usec` 上界)
const USEC_PER_SEC: i64 = 1_000_000;

/// `adjtimex` mode 位: 偏移调整 (`offset` 单位微秒, `ADJ_NANO` 下为纳秒)
pub const ADJ_OFFSET: u32 = 0x0001;
/// `adjtimex` mode 位: 频率调整 (`freq` 单位 `2^-16` ppm)
pub const ADJ_FREQUENCY: u32 = 0x0002;
/// `adjtimex` mode 位: 立即加性设置时间 (`time` 字段为增量)
pub const ADJ_SETOFFSET: u32 = 0x0100;
/// `adjtimex` mode 位: `offset` / `time` 字段以纳秒为单位
pub const ADJ_NANO: u32 = 0x2000;
/// `adjtimex` 本实现支持的 mode 位集合
const ADJ_SUPPORTED_MODES: u32 = ADJ_OFFSET | ADJ_FREQUENCY | ADJ_SETOFFSET | ADJ_NANO;
/// `adjtimex` 频率字段标度: `freq` 单位 = `2^-16` ppm
const FREQ_SCALE: i64 = 65_536;
/// `adjtimex` 返回: 时钟状态正常
const TIME_OK: i64 = 0;
/// `adjtimex` status 位: 时钟尚未同步
const STA_UNSYNC: i32 = 0x0040;

/// `struct timeval` (16 字节)
#[repr(C)]
#[derive(Copy, Clone)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}

/// `struct timespec` (16 字节)
#[repr(C)]
#[derive(Copy, Clone)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// `struct timex` — x86_64 Linux ABI 布局 (208 字节, 含尾部保留区)
///
/// 仅 `modes` / `offset` / `freq` / `time` 为输入字段, 其余为状态回填报文;
/// `_pad*` 与尾部保留区维持 ABI 偏移对齐 (`modes`@0, `offset`@8, `freq`@16,
/// `time`@72, 尾部保留 44 字节).
#[repr(C)]
#[derive(Copy, Clone)]
struct Timex {
    modes: u32,
    _pad0: u32,
    offset: i64,
    freq: i64,
    maxerror: i64,
    esterror: i64,
    status: i32,
    _pad1: i32,
    constant: i64,
    precision: i64,
    tolerance: i64,
    time: Timeval,
    tick: i64,
    ppsfreq: i64,
    jitter: i64,
    shift: i32,
    _pad2: i32,
    stabil: i64,
    jitcnt: i64,
    calcnt: i64,
    errcnt: i64,
    stbcnt: i64,
    tai: i32,
    _pad3: [u8; TIMEX_RESERVED_BYTES],
}

/// `struct timex` 尾部保留区大小 (`tai` 之后的 ABI 填充)
const TIMEX_RESERVED_BYTES: usize = 44;

/// 墙钟当前时间 (ns) — 原始 tick 计数 + 时间同步机制持有的偏移
fn wall_clock_ns() -> u64 {
    crate::services::timer::time_sync::subsystem().get_adjusted_time_ns()
}

/// 单调时钟当前时间 (ns) — 原始 tick 计数 (ms 精度)
fn monotonic_ns() -> u64 {
    crate::framework::syscall::api::get_ticks() * NS_PER_MS
}

/// 按时钟 ID 取当前时间 (ns); 不支持的时钟 ID 返回 `None`
fn clock_now_ns(clk_id: i32) -> Option<u64> {
    match clk_id {
        CLOCK_REALTIME => Some(wall_clock_ns()),
        CLOCK_MONOTONIC => Some(monotonic_ns()),
        _ => None,
    }
}

/// 时间设置类 syscall 的特权判定 (等价 Linux `CAP_SYS_TIME`)
fn has_time_privilege() -> bool {
    crate::framework::sgeg::get_current_uid() == 0
}

/// `clock_gettime(clk_id, tp)` 策略
///
/// 仅支持 `CLOCK_REALTIME` 与 `CLOCK_MONOTONIC`; 其余时钟 ID 返回 `EINVAL`.
///
/// # Errors
///
/// - `tp` 为空指针 → `EINVAL`
/// - 时钟 ID 不受支持 → `EINVAL`
/// - 用户缓冲写入失败 → `EFAULT`
pub fn clock_gettime_syscall(clk_id: i32, tp_ptr: u64) -> i64 {
    if tp_ptr == 0 {
        return Errno::EINVAL.as_ret();
    }
    let Some(ns) = clock_now_ns(clk_id) else {
        return Errno::EINVAL.as_ret();
    };

    let t = Timespec {
        tv_sec: (ns / NS_PER_SEC) as i64,
        tv_nsec: (ns % NS_PER_SEC) as i64,
    };

    if !crate::framework::syscall::api::write_struct_to_user(tp_ptr, &t) {
        return Errno::EFAULT.as_ret();
    }
    0
}

/// `gettimeofday(tv)` 策略 — 读墙钟时间
///
/// `tv` 指向 struct timeval (`tv_sec` + `tv_usec`, 16 字节);
/// `tv_usec` 由纳秒余数截断到微秒精度.
///
/// # Errors
///
/// - `tv` 为空指针或写入失败 → `EFAULT`
pub fn gettimeofday_syscall(tv: u64) -> Result<usize, Errno> {
    if tv == 0 {
        return Err(Errno::EFAULT);
    }

    let ns = wall_clock_ns();
    let t = Timeval {
        tv_sec: (ns / NS_PER_SEC) as i64,
        tv_usec: ((ns % NS_PER_SEC) / NS_PER_USEC) as i64,
    };

    if !crate::framework::syscall::api::write_struct_to_user(tv, &t) {
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

/// `settimeofday(tv, tz)` 策略 — 设置墙钟时间
///
/// `tv` 指向 struct timeval (`tv_sec` + `tv_usec`); 设置后 `CLOCK_REALTIME` /
/// `gettimeofday` 立即反映新时间, `CLOCK_MONOTONIC` 不受影响.
/// `tv` 为 NULL 时按 Linux 语义视为"只设置时区" (本实装忽略时区, 无操作).
///
/// # Errors
///
/// - `tv` 读取失败, 或 `tz` 非空但不可读 → `EFAULT`
/// - `tv_sec` 为负或 `tv_usec` 越界 → `EINVAL`
/// - 调用者无特权 (euid != 0) → `EPERM`
///
/// SIMPLIFIED: `tz` 仅做可读性校验后忽略 (Linux 已废弃 timezone 语义,
/// 仅在首次设置时消费); 影响面: 依赖时区偏移反推本地时间的旧程序得不到
/// 时区信息; 何时需扩展: 引入时区表后按 Linux 首次设置语义消费 `tz`.
pub fn settimeofday_syscall(tv: u64, tz: u64) -> Result<usize, Errno> {
    // Linux 语义: tv 为 NULL 表示"只设置时区"; 本实装忽略时区, 故为无操作
    if tv == 0 {
        return Ok(0);
    }

    let mut input = Timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    if !crate::framework::syscall::api::read_struct_from_user(tv, &mut input) {
        return Err(Errno::EFAULT);
    }
    if tz != 0 && !crate::framework::syscall::api::validate_user_buf(tz, 8) {
        return Err(Errno::EFAULT);
    }
    apply_settimeofday(input.tv_sec, input.tv_usec)
}

/// 墙钟设置策略核心 (参数已从用户态读出, 不含用户内存访问)
///
/// # Errors
///
/// - `tv_sec` 为负或 `tv_usec` 越界 → `EINVAL`
/// - 调用者无特权 (euid != 0) → `EPERM`
#[expect(
    clippy::similar_names,
    reason = "tv_sec/tv_usec 是 struct timeval 的 ABI 字段名; 改名会与用户态 ABI 语义脱节"
)]
pub fn apply_settimeofday(tv_sec: i64, tv_usec: i64) -> Result<usize, Errno> {
    if tv_sec < 0 || !(0..USEC_PER_SEC).contains(&tv_usec) {
        return Err(Errno::EINVAL);
    }
    if !has_time_privilege() {
        return Err(Errno::EPERM);
    }

    let ns = tv_sec as u64 * NS_PER_SEC + tv_usec as u64 * NS_PER_USEC;
    if !crate::services::timer::time_sync::subsystem().set_time(ns) {
        return Err(Errno::EINVAL);
    }
    Ok(0)
}

/// `clock_nanosleep(clockid, flags, req, rem)` 策略
///
/// `flags` 仅支持 `TIMER_ABSTIME`: 置位时 `req` 为指定时钟的绝对时刻,
/// 未置位时为相对时长; 绝对时刻已过则立即返回. 睡眠执行统一委托
/// `framework::timer::sleep_ns` (与 `SYS_nanosleep` 同一机制).
///
/// # Errors
///
/// - `req` 为空指针 → `EFAULT`; `req` 读取失败 → `EFAULT`
/// - `flags` 含 `TIMER_ABSTIME` 之外的位 → `EINVAL`
/// - 时钟 ID 不受支持 → `EINVAL`
/// - `tv_sec` / `tv_nsec` 越界 → `EINVAL`
///
/// SIMPLIFIED: 不支持信号中断提前返回 `EINTR`, 与 `SYS_nanosleep` 基线一致;
/// 影响面: 等待期间收到信号不会提前唤醒调用者; 何时需扩展: 睡眠机制接入
/// 可中断等待 (signalfd/信号投递唤醒阻塞队列) 后统一补充.
pub fn clock_nanosleep_syscall(
    clockid: i32,
    flags: i32,
    req: u64,
    rem: u64,
) -> Result<usize, Errno> {
    let _ = rem; // Linux 语义: clock_nanosleep 不回写剩余时间
    if req == 0 {
        return Err(Errno::EFAULT);
    }
    if flags & !TIMER_ABSTIME != 0 {
        return Err(Errno::EINVAL);
    }

    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if !crate::framework::syscall::api::read_struct_from_user(req, &mut ts) {
        return Err(Errno::EFAULT);
    }

    let wait_ns = clock_nanosleep_wait_ns(clockid, flags, ts.tv_sec, ts.tv_nsec)?;
    crate::framework::timer::sleep_ns(wait_ns);
    Ok(0)
}

/// `clock_nanosleep` 等待时长计算 (参数已从用户态读出, 不含用户内存访问)
///
/// 返回本次调用应等待的纳秒数: `TIMER_ABSTIME` 置位时为目标绝对时刻与
/// 当前时钟的差 (目标已过则为 0), 否则为请求的相对时长.
///
/// # Errors
///
/// - `flags` 含 `TIMER_ABSTIME` 之外的位 → `EINVAL`
/// - 时钟 ID 不受支持 → `EINVAL`
/// - `tv_sec` / `tv_nsec` 越界 → `EINVAL`
#[expect(
    clippy::similar_names,
    reason = "tv_sec/tv_nsec 是 struct timespec 的 ABI 字段名; 改名会与用户态 ABI 语义脱节"
)]
pub fn clock_nanosleep_wait_ns(
    clockid: i32,
    flags: i32,
    tv_sec: i64,
    tv_nsec: i64,
) -> Result<u64, Errno> {
    if flags & !TIMER_ABSTIME != 0 {
        return Err(Errno::EINVAL);
    }
    let Some(now_ns) = clock_now_ns(clockid) else {
        return Err(Errno::EINVAL);
    };
    if tv_sec < 0 || tv_nsec < 0 || tv_nsec >= NS_PER_SEC as i64 {
        return Err(Errno::EINVAL);
    }

    let req_ns = tv_sec as u64 * NS_PER_SEC + tv_nsec as u64;
    Ok(if flags & TIMER_ABSTIME != 0 {
        // 绝对时刻: 目标已过则立即返回 (Linux 不报错)
        req_ns.saturating_sub(now_ns)
    } else {
        req_ns
    })
}

/// `adjtimex(buf)` 策略 — 读写时钟调整参数
///
/// `buf` 指向 struct timex (x86_64 为 208 字节). 支持的 mode 位:
/// `ADJ_OFFSET` (渐进偏移调整)、`ADJ_FREQUENCY` (频率调整)、
/// `ADJ_SETOFFSET` (加性跳变)、`ADJ_NANO` (前两者字段单位为纳秒).
/// 无论是否调整, 均回填当前时钟状态快照.
///
/// 返回值: 成功时为时钟状态 `TIME_OK` (0); 失败为负 errno.
///
/// # Errors (返回码形式)
///
/// - `buf` 为空或不可读写 → `-EFAULT`
/// - `modes` 含不支持的位, 或 `ADJ_SETOFFSET` 的 `time` 字段越界 → `-EINVAL`
/// - 调用者无特权 (euid != 0) → `-EPERM`
///
/// SIMPLIFIED: 不支持 `ADJ_MAXERROR` / `ADJ_ESTERROR` / `ADJ_STATUS` /
/// `ADJ_TIMECONST` / `ADJ_TICK` (返回 `-EINVAL`), 误差估计类回填字段恒为 0,
/// `precision` 固定 1us; 影响面: ntpd/chronyd 类调优与误差统计不可用;
/// 何时需扩展: 引入时钟误差估计 (PLL/FLL 二阶环路) 后按 Linux 语义补齐.
pub fn adjtimex_syscall(buf: u64) -> i64 {
    if buf == 0 {
        return Errno::EFAULT.as_ret();
    }

    let mut input = Timex {
        modes: 0,
        _pad0: 0,
        offset: 0,
        freq: 0,
        maxerror: 0,
        esterror: 0,
        status: 0,
        _pad1: 0,
        constant: 0,
        precision: 0,
        tolerance: 0,
        time: Timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        tick: 0,
        ppsfreq: 0,
        jitter: 0,
        shift: 0,
        _pad2: 0,
        stabil: 0,
        jitcnt: 0,
        calcnt: 0,
        errcnt: 0,
        stbcnt: 0,
        tai: 0,
        _pad3: [0; TIMEX_RESERVED_BYTES],
    };
    if !crate::framework::syscall::api::read_struct_from_user(buf, &mut input) {
        return Errno::EFAULT.as_ret();
    }
    let outcome = apply_adjtimex(
        input.modes,
        input.offset,
        input.freq,
        input.time.tv_sec,
        input.time.tv_usec,
    );
    if let Err(e) = outcome {
        return e.as_ret();
    }

    // 回填时钟状态快照
    let subsystem = crate::services::timer::time_sync::subsystem();
    let nano_units = input.modes & ADJ_NANO != 0;
    let (synced, _count, freq_ppb, offset_ns) = subsystem.get_sync_status();
    let now_ns = wall_clock_ns();
    let tick_us = 1_000_000 / i64::from(crate::framework::timer::get_frequency().max(1));
    let payload = Timex {
        modes: 0,
        _pad0: 0,
        offset: if nano_units {
            offset_ns
        } else {
            offset_ns / NS_PER_USEC as i64
        },
        freq: freq_ppb * FREQ_SCALE / 1_000,
        maxerror: 0,
        esterror: 0,
        status: if synced { 0 } else { STA_UNSYNC },
        _pad1: 0,
        constant: 0,
        precision: 1,
        tolerance: 0,
        time: Timeval {
            tv_sec: (now_ns / NS_PER_SEC) as i64,
            tv_usec: ((now_ns % NS_PER_SEC) / NS_PER_USEC) as i64,
        },
        tick: tick_us,
        ppsfreq: 0,
        jitter: 0,
        shift: 0,
        _pad2: 0,
        stabil: 0,
        jitcnt: 0,
        calcnt: 0,
        errcnt: 0,
        stbcnt: 0,
        tai: 0,
        _pad3: [0; TIMEX_RESERVED_BYTES],
    };

    if !crate::framework::syscall::api::write_struct_to_user(buf, &payload) {
        return Errno::EFAULT.as_ret();
    }
    TIME_OK
}

/// `adjtimex` 时钟调整策略核心 (参数已从用户态读出, 不含用户内存访问)
///
/// `time` 参数为 `ADJ_SETOFFSET` 使用的有符号增量 (秒 + 微秒).
///
/// # Errors
///
/// - `modes` 含不支持的位, 或 `ADJ_SETOFFSET` 的 `time` 字段越界 → `EINVAL`
/// - 调用者无特权 (euid != 0) → `EPERM`
#[expect(
    clippy::similar_names,
    reason = "tv_sec/tv_usec 是 struct timeval 的 ABI 字段名; 改名会与用户态 ABI 语义脱节"
)]
pub fn apply_adjtimex(
    modes: u32,
    offset: i64,
    freq: i64,
    tv_sec: i64,
    tv_usec: i64,
) -> Result<i64, Errno> {
    if modes & !ADJ_SUPPORTED_MODES != 0 {
        return Err(Errno::EINVAL);
    }
    if modes & ADJ_SETOFFSET != 0 && !(-USEC_PER_SEC..USEC_PER_SEC).contains(&tv_usec) {
        return Err(Errno::EINVAL);
    }
    if !has_time_privilege() {
        return Err(Errno::EPERM);
    }

    let subsystem = crate::services::timer::time_sync::subsystem();

    // ADJ_SETOFFSET: 在墙钟上叠加增量 (加性跳变)
    if modes & ADJ_SETOFFSET != 0 {
        let delta = Timeval { tv_sec, tv_usec };
        let delta_ns = timeval_to_ns_signed(delta);
        let target = if delta_ns >= 0 {
            wall_clock_ns().saturating_add(delta_ns as u64)
        } else {
            wall_clock_ns().saturating_sub(delta_ns.unsigned_abs())
        };
        subsystem.set_time(target);
    }

    // ADJ_OFFSET: 渐进调整 (由 tick 中断按斜率消耗)
    if modes & ADJ_OFFSET != 0 {
        let offset_ns = if modes & ADJ_NANO != 0 {
            offset
        } else {
            offset * NS_PER_USEC as i64
        };
        subsystem.adj_time(offset_ns);
    }

    // ADJ_FREQUENCY: freq 单位 2^-16 ppm → ppb
    if modes & ADJ_FREQUENCY != 0 {
        subsystem.adj_freq(freq * 1_000 / FREQ_SCALE);
    }

    Ok(TIME_OK)
}

/// 有符号 `timeval` → 纳秒 (adjtimex `ADJ_SETOFFSET` 增量换算)
fn timeval_to_ns_signed(tv: Timeval) -> i64 {
    let sec_ns = i128::from(tv.tv_sec) * i128::from(NS_PER_SEC);
    let usec_ns = i128::from(tv.tv_usec) * i128::from(NS_PER_USEC);
    let total = sec_ns + usec_ns;
    // 越界时饱和到 i64 边界 (调用方已在 mode 校验中排除典型越界)
    total.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}
