//! FREG (Fast Recovery Engine Group) 类型与常量 — 恢复域、快照、回滚事件定义。
//!
//! 定义恢复域状态机、撤销日志条目、快照与回滚事件的公共数据形态,
//! 以及各列表容量上限与能力位常量。

#![deny(missing_docs)]

/// 最大恢复域数量。
pub const MAX_RECOVERY_DOMAINS: usize = 32;
/// 单域最大依赖数量。
pub const MAX_DOMAIN_DEPENDENCIES: usize = 8;
/// 撤销日志最大条目数。
pub const MAX_UNDO_ENTRIES: usize = 256;
/// 默认 FREG 检查间隔 (时间刻度)。
pub const DEFAULT_FREG_INTERVAL: u64 = 100;
/// 允许的连续失败次数上限。
pub const MAX_CONSECUTIVE_FAILURES: u32 = 5;
/// 退避基准时间刻度。
pub const BACKOFF_BASE_TICKS: u64 = 100;
/// 回滚日志最大条目数。
pub const MAX_ROLLBACK_LOG: usize = 64;
/// 最大 FREG 快照数量。
pub const MAX_FREG_SNAPSHOTS: usize = 8;
/// 单域最大地址区间数量。
pub const MAX_ADDR_RANGES: usize = 16;
/// 直接映射区大小。
pub const DIRECT_MAP_SIZE: usize = 64;

/// 文件系统写权限能力位。
pub const CAP_FS_WRITE: u64 = 1 << 0;
/// 网络发送权限能力位。
pub const CAP_NET_SEND: u64 = 1 << 1;
/// 进程创建权限能力位。
pub const CAP_PROC_CREATE: u64 = 1 << 2;

/// 恢复域生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DomainState {
    /// 正常运行。
    Active = 0,
    /// 正在冻结 (停止接受新操作)。
    Freezing = 1,
    /// 正在回滚。
    RollingBack = 2,
    /// 正在恢复。
    Recovering = 3,
    /// 降级运行。
    Degraded = 4,
    /// 隔离 (不再参与恢复)。
    Quarantined = 5,
}

impl DomainState {
    /// 由 u32 编码解析状态; 未知编码返回 `None`。
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Active),
            1 => Some(Self::Freezing),
            2 => Some(Self::RollingBack),
            3 => Some(Self::Recovering),
            4 => Some(Self::Degraded),
            5 => Some(Self::Quarantined),
            _ => None,
        }
    }

    /// 由 u32 编码解析状态, 未知编码回退为 `Quarantined`。
    pub fn from_u32_fallback(v: u32) -> Self {
        Self::from_u32(v).unwrap_or(Self::Quarantined)
    }
}

/// 单条撤销记录: 记录字段原值及其地址, 用于回滚时还原。
#[derive(Clone, Copy)]
pub struct UndoEntry {
    /// 该记录所属的代数。
    pub generation: u64,
    /// 被修改字段的地址。
    pub field_ptr: *mut u8,
    /// 字段原值 (最多 8 字节)。
    pub old_value: [u8; 8],
    /// 原值的有效字节数。
    pub value_size: u8,
    /// 记录校验和。
    pub checksum: u32,
}

// SAFETY: UndoEntry 含裸指针 (field_ptr), 不持有内存所有权,
// 仅记录用于回滚的地址. old_value/value_size/checksum 均为普通 Copy 类型.
// 无内部可变性; 由于指针仅在 UndoLog 锁内解引用, send/share 安全.
unsafe impl Send for UndoEntry {}
unsafe impl Sync for UndoEntry {}

/// FREG 快照: 记录捕获时的代数、时间刻度与撤销日志偏移。
#[derive(Clone, Copy)]
pub struct FREGSnapshot {
    /// 快照捕获时的代数。
    pub generation: u64,
    /// 快照捕获时的时间刻度。
    pub tick: u64,
    /// 对应的撤销日志起始偏移。
    pub undo_offset: usize,
}

/// 一次回滚事件记录: 时间刻度/域标识/代数变化/回滚条目数/指纹与结果。
#[derive(Clone, Copy)]
pub struct RollbackEvent {
    /// 事件发生的时间刻度。
    pub tick: u64,
    /// 发生回滚的恢复域标识。
    pub domain_id: u64,
    /// 回滚前的代数。
    pub generation_from: u64,
    /// 回滚后的代数。
    pub generation_to: u64,
    /// 本次回滚的条目数。
    pub entries_rolled_back: usize,
    /// 崩溃指纹。
    pub crash_fingerprint: u64,
    /// 级联回滚深度。
    pub cascade_depth: usize,
    /// 回滚结果码。
    pub result: i32,
}
