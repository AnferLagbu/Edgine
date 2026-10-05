pub const MAX_RECOVERY_DOMAINS: usize = 32;
pub const MAX_DOMAIN_DEPENDENCIES: usize = 8;
pub const MAX_UNDO_ENTRIES: usize = 256;
pub const DEFAULT_FREG_INTERVAL: u64 = 100;
pub const MAX_CONSECUTIVE_FAILURES: u32 = 5;
pub const BACKOFF_BASE_TICKS: u64 = 100;
pub const MAX_ROLLBACK_LOG: usize = 64;
pub const MAX_FREG_SNAPSHOTS: usize = 8;
pub const MAX_ADDR_RANGES: usize = 16;
pub const DIRECT_MAP_SIZE: usize = 64;

pub const CAP_FS_WRITE: u64 = 1 << 0;
pub const CAP_NET_SEND: u64 = 1 << 1;
pub const CAP_PROC_CREATE: u64 = 1 << 2;

/// 恢复域生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DomainState {
    Active = 0,
    Freezing = 1,
    RollingBack = 2,
    Recovering = 3,
    Degraded = 4,
    Quarantined = 5,
}

impl DomainState {
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

    pub fn from_u32_fallback(v: u32) -> Self {
        Self::from_u32(v).unwrap_or(Self::Quarantined)
    }
}

/// 单条撤销记录: 记录字段原值及其地址, 用于回滚时还原。
#[derive(Clone, Copy)]
pub struct UndoEntry {
    pub generation: u64,
    pub field_ptr: *mut u8,
    pub old_value: [u8; 8],
    pub value_size: u8,
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
    pub generation: u64,
    pub tick: u64,
    pub undo_offset: usize,
}

/// 一次回滚事件记录: 时间刻度/域标识/代数变化/回滚条目数/指纹与结果。
#[derive(Clone, Copy)]
pub struct RollbackEvent {
    pub tick: u64,
    pub domain_id: u64,
    pub generation_from: u64,
    pub generation_to: u64,
    pub entries_rolled_back: usize,
    pub crash_fingerprint: u64,
    pub cascade_depth: usize,
    pub result: i32,
}
