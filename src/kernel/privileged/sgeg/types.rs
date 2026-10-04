#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯类型定义和常量。
//! SGEG v1 类型定义 — privileged 层权威定义
//!
//! ## 归属记录
//!
//! 纯数据定义 (PWM 类型/能力矩阵/身份条目/审计类型), 0 unsafe.
//! T6-7 曾于 2026-06-16 提取到 functions; 第二十五批 (DECISION-K 项 5 sgeg
//! 判据: 持有类型归 privileged — privileged/proc/process.rs 进程表机制持有
//! PwmContext) 反转归位本文件. functions/sgeg/types.rs 为 re-export 壳.
//!
//! SGEG 权限模型的核心数据结构.
//! 域身份 (DID) + 能力矩阵 + 身份条目 + 审计类型.
//! SGEG: 密码决定身份 | 无预设特权 | 能力来自授予

use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64};

pub const MAX_PWM_ENTRIES: usize = 256;
pub const PWM_NOTE_LEN: usize = 64;
pub const PWM_HASH_LEN: usize = 48;
pub const PWM_SALT_LEN: usize = 16;
pub const PWM_DIGEST_LEN: usize = 32;
pub const MAX_GRANT_RECORDS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PwmId(pub u64);

impl PwmId {
    pub const ZERO: Self = Self(0);

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl Default for PwmId {
    fn default() -> Self {
        Self::ZERO
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct DomainId(pub u64);

impl DomainId {
    pub const ZERO: Self = Self(0);
    pub const KERNEL: Self = Self(1);
    pub const ROOT: Self = Self(1000);
    pub const NOBODY: Self = Self(65534);

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn from_uid(uid: u32) -> Self {
        Self(u64::from(uid))
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn to_uid(&self) -> u32 {
        self.0 as u32
    }
}

impl Default for DomainId {
    fn default() -> Self {
        Self::ZERO
    }
}

// 域级行为门控标志 — 按进程生效, 在 syscall 咽喉点裁决
//
// ## 归属记录
//
// 分册 9 批次 4 实装: 原为零引用纯声明, 现由 `privileged/proc/domain.rs`
// 消费 (门控判定 + 状态读写), `functions/sgeg/domain.rs` 提供用户态入口.
//
// ## 位语义表
//
// | 位 | 名称 | 门控语义 |
// |---|---|---|
// | bit0 | `NO_FORK` | 拒 `clone`/`fork`/`clone3` |
// | bit1 | `NO_EXEC` | 拒 `execve`/`execveat` |
// | bit2 | `NO_NET` | 拒 `socket`/`socketpair`/`connect`/`bind`/`listen`/`accept`/`accept4`/`sendto`/`recvfrom`/`sendmsg`/`recvmsg` |
// | bit3 | `NO_DEVICE` | 拒 `ioctl`/`mount`/`umount2` |
// | bit4 | `SANDBOX` | 复合位: 严格白名单, 仅放行 `read`/`write`/`exit`/`exit_group`/`rt_sigreturn` |
// | bit5 | `READONLY` | 拒写类调用, 及带写意图 (`O_WRONLY`/`O_RDWR`/`O_CREAT`/`O_TRUNC`/`O_APPEND`) 的 `open`/`openat` |
// | bit6 | `TEMP` | 元数据位: 不参与门控 (审计/生命周期标记) |
// | bit7 | `SYSTEM` | 元数据位: 不参与门控 (特权标记) |
//
// 拒绝以 `EPERM` 返回. 默认值 `NONE` (无门控, 不改变任何调用行为).
// fork 继承全部标志; `execve` 保留. 修改标志需 SYSTEM 域能力位
// `SYSTEM_CAP_SET_DOMAIN_FLAGS`.
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DomainFlags: u32 {
        const NONE       = 0;
        const NO_FORK    = 1 << 0;
        const NO_EXEC    = 1 << 1;
        const NO_NET     = 1 << 2;
        const NO_DEVICE  = 1 << 3;
        const SANDBOX    = 1 << 4;
        const READONLY   = 1 << 5;
        const TEMP       = 1 << 6;
        const SYSTEM     = 1 << 7;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CapDomain(pub u16);

impl CapDomain {
    pub const SYSTEM: Self = Self(0);
    pub const FS: Self = Self(1);
    pub const NET: Self = Self(2);
    pub const PROC: Self = Self(3);
    pub const DEVICE: Self = Self(4);
    pub const USER_MGMT: Self = Self(5);
    pub const IPC: Self = Self(6);
    pub const MEM: Self = Self(7);
    pub const TIME: Self = Self(8);
    pub const FREG: Self = Self(9);
    pub const SIGNAL: Self = Self(10);
    pub const SHM: Self = Self(11);
    pub const SEM: Self = Self(12);
    pub const MSGQ: Self = Self(13);
    pub const DMA: Self = Self(14);
    pub const RESERVED: Self = Self(15);

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_usize(&self) -> usize {
        (self.0 as usize) % 16
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_u16(&self) -> u16 {
        self.0
    }
}

impl From<u16> for CapDomain {
    fn from(v: u16) -> Self {
        Self(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CapBits(pub u64);

impl CapBits {
    pub const NONE: Self = Self(0);
    pub const ALL: Self = Self(0xFFFFFFFFFFFFFFFF);

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl core::ops::BitOr for CapBits {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for CapBits {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl core::ops::BitOrAssign for CapBits {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::ops::BitAndAssign for CapBits {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl core::ops::Not for CapBits {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    pub struct PwmFlags: u16 {
        const NONE       = 0;
        const DISABLED   = 1 << 0;
        const MODIFIED   = 1 << 3;
        const LOCKED     = 1 << 4;
    }
}

impl Default for PwmFlags {
    fn default() -> Self {
        Self::NONE
    }
}

#[repr(C)]
pub struct PwmEntry {
    pub pwm: AtomicU64,
    pub posix_uid: AtomicU32,
    pub posix_gid: AtomicU32,
    pub creator_pwm: AtomicU64,
    pub privilege_level: AtomicU8,
    pub flags: AtomicU16,
    pub caps: [AtomicU64; 16],
    /// 备注 (T4-1 全 Atomic 化: u8 数组 → `AtomicU8` 数组, 支持 &self 写入)
    pub note: [AtomicU8; PWM_NOTE_LEN],
    /// 密码哈希 (T4-1 全 Atomic 化: u8 数组 → `AtomicU8` 数组, 支持 &self 写入)
    pub password_hash: [AtomicU8; PWM_HASH_LEN],
    pub created_time: AtomicU64,
    pub expires_at: AtomicU64,
    pub lockout_until: AtomicU64,
    pub failed_attempts: AtomicU32,
    pub last_login_time: AtomicU64,
}

impl Default for PwmEntry {
    fn default() -> Self {
        Self {
            pwm: AtomicU64::new(0),
            posix_uid: AtomicU32::new(0),
            posix_gid: AtomicU32::new(0),
            creator_pwm: AtomicU64::new(0),
            privilege_level: AtomicU8::new(0xFF),
            flags: AtomicU16::new(0),
            caps: [0; 16].map(AtomicU64::new),
            note: [const { AtomicU8::new(0) }; PWM_NOTE_LEN],
            password_hash: [const { AtomicU8::new(0) }; PWM_HASH_LEN],
            created_time: AtomicU64::new(0),
            expires_at: AtomicU64::new(0),
            lockout_until: AtomicU64::new(0),
            failed_attempts: AtomicU32::new(0),
            last_login_time: AtomicU64::new(0),
        }
    }
}

/// PWM 表条目的持久化快照 (纯 POD, 无原子字段).
///
/// 用于跨 TCB 边界传递序列化/反序列化所需的条目字段, 使 functions 层无需
/// 直读 [`PwmEntry`] 的原子字段. 字段集合与磁盘 v5 条目布局一致 (见
/// `functions::sgeg::persist` 的 `ENTRY_SZ`).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct PwmEntrySnapshot {
    /// PWM 标识 (0 表示空槽)
    pub pwm: u64,
    /// 创建者 PWM
    pub creator_pwm: u64,
    /// 特权级别
    pub privilege_level: u8,
    /// 标志位
    pub flags: u16,
    /// 能力矩阵 (16 个域)
    pub caps: [u64; 16],
    /// 备注
    pub note: [u8; PWM_NOTE_LEN],
    /// 密码哈希
    pub password_hash: [u8; PWM_HASH_LEN],
    /// 创建时间
    pub created_time: u64,
    /// 过期时间
    pub expires_at: u64,
}

impl Default for PwmEntrySnapshot {
    fn default() -> Self {
        Self {
            pwm: 0,
            creator_pwm: 0,
            privilege_level: 0xFF,
            flags: 0,
            caps: [0; 16],
            note: [0; PWM_NOTE_LEN],
            password_hash: [0; PWM_HASH_LEN],
            created_time: 0,
            expires_at: 0,
        }
    }
}

impl PwmEntry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_valid(&self) -> bool {
        self.pwm.load(core::sync::atomic::Ordering::Acquire) != 0
    }

    pub fn get_pwm(&self) -> PwmId {
        PwmId(self.pwm.load(core::sync::atomic::Ordering::Acquire))
    }

    pub fn get_creator_pwm(&self) -> PwmId {
        PwmId(self.creator_pwm.load(core::sync::atomic::Ordering::Acquire))
    }

    pub fn get_flags(&self) -> PwmFlags {
        PwmFlags::from_bits_truncate(self.flags.load(core::sync::atomic::Ordering::Acquire))
    }

    pub fn set_flags(&self, flags: PwmFlags) {
        self.flags
            .store(flags.bits(), core::sync::atomic::Ordering::Release);
    }

    pub fn add_flags(&self, flags: PwmFlags) {
        let current = self.get_flags();
        self.set_flags(current | flags);
    }

    pub fn remove_flags(&self, flags: PwmFlags) {
        let current = self.get_flags();
        self.set_flags(current & !flags);
    }

    pub fn has_flag(&self, flag: PwmFlags) -> bool {
        self.get_flags().contains(flag)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// T4-1: 全 Atomic 化后此 API 行为变化.
    /// 原因: [`AtomicU8`; N] 不能直接借用为 &[u8], 返回 owned &str 需要内部静态缓冲.
    /// 当前实现: 返回静态空串占位. 推荐使用 `note_bytes()` 复制 + 自行转换.
    /// 兼容保留: 签名不变, 行为退化为"返回空串 (新值前为 None)". 调用方应迁移.
    pub fn get_note_str(&self) -> &'static str {
        // T4-1: 静态生命周期 &str, 仅占位 (此 API 已废弃, 推荐 note_bytes/note_equals)
        ""
    }

    /// T4-1: 复制 note 到 owned 数组 (替代 `get_note_str` 的 &str 返回)
    pub fn note_bytes(&self) -> [u8; PWM_NOTE_LEN] {
        let mut buf = [0u8; PWM_NOTE_LEN];
        for (i, slot) in self.note.iter().enumerate() {
            buf[i] = slot.load(core::sync::atomic::Ordering::Acquire);
        }
        buf
    }

    /// T4-1: 全 Atomic 化后, `set_note` 改用原子字节写入, 接受 &self.
    pub fn set_note(&self, note: &str) {
        let bytes = note.as_bytes();
        let len = bytes.len().min(PWM_NOTE_LEN - 1);
        for i in 0..len {
            self.note[i].store(bytes[i], core::sync::atomic::Ordering::Release);
        }
        self.note[len].store(0, core::sync::atomic::Ordering::Release);
    }

    /// T4-1: 比较备注是否相等 (避免 &str 生命周期问题)
    pub fn note_equals(&self, other: &str) -> bool {
        let buf = self.note_bytes();
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        raw::bytes_to_str(&buf[..len]) == other
    }

    pub fn load_caps(&self, domain: CapDomain) -> CapBits {
        let idx = domain.as_usize();
        CapBits(self.caps[idx].load(core::sync::atomic::Ordering::Acquire))
    }

    pub fn store_caps(&self, domain: CapDomain, caps: CapBits) {
        let idx = domain.as_usize();
        self.caps[idx].store(caps.0, core::sync::atomic::Ordering::Release);
    }

    pub fn fetch_or_caps(&self, domain: CapDomain, caps: CapBits) {
        let idx = domain.as_usize();
        self.caps[idx].fetch_or(caps.0, core::sync::atomic::Ordering::AcqRel);
    }

    pub fn fetch_and_caps(&self, domain: CapDomain, caps: CapBits) {
        let idx = domain.as_usize();
        self.caps[idx].fetch_and(caps.0, core::sync::atomic::Ordering::AcqRel);
    }

    pub fn has_capability(&self, domain: CapDomain, required: CapBits) -> bool {
        let current = self.load_caps(domain);
        current.contains(required)
    }

    pub fn get_uid(&self) -> u32 {
        self.posix_uid.load(core::sync::atomic::Ordering::Acquire)
    }

    pub fn get_gid(&self) -> u32 {
        self.posix_gid.load(core::sync::atomic::Ordering::Acquire)
    }

    pub fn set_uid(&self, uid: u32) {
        self.posix_uid
            .store(uid, core::sync::atomic::Ordering::Release);
    }

    pub fn set_gid(&self, gid: u32) {
        self.posix_gid
            .store(gid, core::sync::atomic::Ordering::Release);
    }

    /// 采集条目快照 (全字段 `Acquire` 读, 供持久化序列化).
    pub fn snapshot(&self) -> PwmEntrySnapshot {
        let mut caps = [0u64; 16];
        for (i, c) in self.caps.iter().enumerate() {
            caps[i] = c.load(core::sync::atomic::Ordering::Acquire);
        }
        let mut note = [0u8; PWM_NOTE_LEN];
        for (i, b) in self.note.iter().enumerate() {
            note[i] = b.load(core::sync::atomic::Ordering::Acquire);
        }
        let mut password_hash = [0u8; PWM_HASH_LEN];
        for (i, b) in self.password_hash.iter().enumerate() {
            password_hash[i] = b.load(core::sync::atomic::Ordering::Acquire);
        }
        PwmEntrySnapshot {
            pwm: self.pwm.load(core::sync::atomic::Ordering::Acquire),
            creator_pwm: self.creator_pwm.load(core::sync::atomic::Ordering::Acquire),
            privilege_level: self
                .privilege_level
                .load(core::sync::atomic::Ordering::Acquire),
            flags: self.flags.load(core::sync::atomic::Ordering::Acquire),
            caps,
            note,
            password_hash,
            created_time: self
                .created_time
                .load(core::sync::atomic::Ordering::Acquire),
            expires_at: self.expires_at.load(core::sync::atomic::Ordering::Acquire),
        }
    }

    /// 从快照恢复条目 (全字段 `Release` 写).
    ///
    /// `pwm` 最后写: `is_valid()` 依据 `pwm != 0`, 保证其余字段先就绪.
    pub fn import(&self, snap: &PwmEntrySnapshot) {
        self.creator_pwm
            .store(snap.creator_pwm, core::sync::atomic::Ordering::Release);
        self.privilege_level
            .store(snap.privilege_level, core::sync::atomic::Ordering::Release);
        self.flags
            .store(snap.flags, core::sync::atomic::Ordering::Release);
        for (i, c) in self.caps.iter().enumerate() {
            c.store(snap.caps[i], core::sync::atomic::Ordering::Release);
        }
        for (i, b) in self.note.iter().enumerate() {
            b.store(snap.note[i], core::sync::atomic::Ordering::Release);
        }
        for (i, b) in self.password_hash.iter().enumerate() {
            b.store(snap.password_hash[i], core::sync::atomic::Ordering::Release);
        }
        self.created_time
            .store(snap.created_time, core::sync::atomic::Ordering::Release);
        self.expires_at
            .store(snap.expires_at, core::sync::atomic::Ordering::Release);
        self.pwm
            .store(snap.pwm, core::sync::atomic::Ordering::Release);
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct PwmContext {
    pub current_entry: *const PwmEntry,
    pub session_pwm: PwmId,
    pub cached_uid: u32,
    pub cached_gid: u32,
    pub euid: u32,
    pub egid: u32,
    pub saved_euid: u32,
    pub saved_egid: u32,
    pub active_domain_id: DomainId,
    pub elevation_granted_pwm: PwmId,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct GrantRecord {
    pub grantor_pwm: PwmId,
    pub grantee_pwm: PwmId,
    pub domain: CapDomain,
    pub caps: CapBits,
    pub granted_at: u64,
}

impl GrantRecord {
    pub const EMPTY: Self = Self {
        grantor_pwm: PwmId::ZERO,
        grantee_pwm: PwmId::ZERO,
        domain: CapDomain(0),
        caps: CapBits::NONE,
        granted_at: 0,
    };

    pub fn is_empty(&self) -> bool {
        self.grantor_pwm == PwmId::ZERO
    }
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwmError {
    Ok = 0,
    NotFound = -1,
    Disabled = -2,
    PasswordIncorrect = -3,
    PermissionDenied = -4,
    TableFull = -5,
    AlreadyExists = -6,
    InsufficientPrivilege = -7,
    NotAuthorized = -8,
    NotCreator = -9,
    WouldBreakFloor = -10,
    PrivilegeOverflow = -11,
    TokenUsed = -12,
    NoFirstToken = -13,
    InvalidPassword = -14,
}

impl PwmError {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default)]
pub enum AuditAction {
    #[default]
    Login = 1,
    Logout = 2,
    Create = 3,
    Delete = 4,
    Modify = 5,
    PasswordChange = 8,
    Grant = 10,
    Revoke = 11,
    TransferCreator = 12,
    FirstTokenGrant = 13,
}

impl AuditAction {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default)]
pub enum AuditResult {
    #[default]
    Success = 0,
    Failure = 1,
    Denied = 2,
}

impl AuditResult {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct AuditEntry {
    pub timestamp: u64,
    pub pwm: PwmId,
    pub action: AuditAction,
    pub result: AuditResult,
    pub target_pwm: PwmId,
    pub details: u64,
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中不安全转换
// ============================================================================

pub(crate) mod raw {
    /// 字节切片 → &str (safe 版本)
    /// 合法来源: `set_note()` 写入的字符串已经是合法 UTF-8。
    /// T6-7: 从 `from_utf8_unchecked` 改为 `from_utf8`, 消除唯一 unsafe,
    /// 使 types.rs 可迁移到 functions 层.
    pub fn bytes_to_str(bytes: &[u8]) -> &str {
        core::str::from_utf8(bytes).unwrap_or("")
    }
}

// ============================================================================
// 单元测试 — REVAL-5 T4-2: CapabilityMatrix 路径契约
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::sgeg::capability;
    use core::sync::atomic::Ordering;

    #[test]
    fn pwm_entry_default_is_invalid() {
        let e = PwmEntry::default();
        // pwm == 0 表示未分配, is_valid() 应返 false
        assert!(!e.is_valid());
        assert_eq!(e.get_pwm(), PwmId(0));
    }

    #[test]
    fn pwm_entry_valid_after_pwm_set() {
        let e = PwmEntry::default();
        e.pwm.store(0xABCD, Ordering::Release);
        assert!(e.is_valid());
        assert_eq!(e.get_pwm(), PwmId(0xABCD));
    }

    #[test]
    fn pwm_entry_caps_load_store() {
        let e = PwmEntry::default();
        // 初始 0
        assert_eq!(e.load_caps(CapDomain(0)), CapBits::NONE);
        // 写入域 0
        e.store_caps(CapDomain(0), CapBits(0xFF));
        assert_eq!(e.load_caps(CapDomain(0)), CapBits(0xFF));
        // 其他域仍为 0 (隔离性)
        assert_eq!(e.load_caps(CapDomain(1)), CapBits::NONE);
    }

    #[test]
    fn pwm_entry_caps_fetch_or() {
        let e = PwmEntry::default();
        e.store_caps(CapDomain(0), CapBits(0b1100));
        e.fetch_or_caps(CapDomain(0), CapBits(0b0011));
        // OR 合并: 0b1100 | 0b0011 = 0b1111
        assert_eq!(e.load_caps(CapDomain(0)), CapBits(0b1111));
    }

    #[test]
    fn pwm_entry_caps_fetch_and() {
        let e = PwmEntry::default();
        e.store_caps(CapDomain(0), CapBits(0b1111));
        e.fetch_and_caps(CapDomain(0), CapBits(0b1010));
        // AND 屏蔽: 0b1111 & 0b1010 = 0b1010
        assert_eq!(e.load_caps(CapDomain(0)), CapBits(0b1010));
    }

    #[test]
    fn pwm_entry_has_capability_subset() {
        let e = PwmEntry::default();
        e.store_caps(CapDomain(0), CapBits(0b1111));
        // 子集检查: 拥有的位 ⊇ required
        assert!(e.has_capability(CapDomain(0), CapBits(0b1000)));
        assert!(e.has_capability(CapDomain(0), CapBits(0b0100)));
        assert!(e.has_capability(CapDomain(0), CapBits(0b1100)));
        // 超出拥有的位 → false
        assert!(!e.has_capability(CapDomain(0), CapBits(0b10000)));
    }

    #[test]
    fn pwm_entry_uid_gid_round_trip() {
        let e = PwmEntry::default();
        assert_eq!(e.get_uid(), 0);
        assert_eq!(e.get_gid(), 0);
        e.set_uid(1000);
        e.set_gid(100);
        assert_eq!(e.get_uid(), 1000);
        assert_eq!(e.get_gid(), 100);
    }

    #[test]
    fn pwm_entry_flags_lifecycle() {
        let e = PwmEntry::default();
        assert!(!e.has_flag(PwmFlags::DISABLED));
        e.add_flags(PwmFlags::DISABLED);
        assert!(e.has_flag(PwmFlags::DISABLED));
        e.remove_flags(PwmFlags::DISABLED);
        assert!(!e.has_flag(PwmFlags::DISABLED));
    }

    #[test]
    fn pwm_entry_set_note_round_trip() {
        let e = PwmEntry::default();
        e.set_note("admin");
        // T4-1: 用 note_equals 比较, 避免 &str 生命周期问题
        assert!(e.note_equals("admin"));
        assert!(!e.note_equals("root"));
    }

    #[test]
    fn pwm_context_default_fields() {
        let c = PwmContext::default();
        assert!(c.current_entry.is_null());
        assert_eq!(c.cached_uid, 0);
        assert_eq!(c.active_domain_id, DomainId(0));
    }

    // UT-07 (2026-09-26): 注册侧 pwm::types 的 PwmId/CapDomain/CapBits newtype 用例迁入
    // (源侧原仅覆盖 PwmEntry/PwmContext).

    #[test]
    fn pwm_id_newtype() {
        let id = PwmId(42);
        assert!(id.is_valid());
        assert_eq!(id.as_u64(), 42);

        // 历史上此处用 PwmId::TEST 验证 is_valid, 该常量已移除 (P0-I-29d:
        // 硬编码"魔法值"权限字会绕过访问控制); 改用任意非零 PwmId 验证语义.
        let zero = PwmId::ZERO;
        assert!(!zero.is_valid());
        assert_eq!(zero.as_u64(), 0);

        let arbitrary = PwmId(0xDEAD_BEEF_CAFE_F00D);
        assert!(arbitrary.is_valid());
        assert_eq!(arbitrary.as_u64(), 0xDEAD_BEEF_CAFE_F00D);
    }

    #[test]
    fn cap_domain_newtype() {
        assert_eq!(CapDomain::FS.as_u16(), 1);
        assert_eq!(CapDomain::FS.as_usize(), 1);

        let from_raw: CapDomain = 2u16.into();
        assert_eq!(from_raw, CapDomain::NET);

        assert_eq!(CapDomain::SYSTEM.as_usize(), 0);
    }

    #[test]
    fn cap_bits_newtype() {
        assert_eq!(CapBits::NONE.as_u64(), 0);
        assert_eq!(CapBits::ALL.as_u64(), u64::MAX);

        let read = CapBits(capability::FS_CAP_READ);
        let write = CapBits(capability::FS_CAP_WRITE);
        let rw = read | write;
        assert!(rw.contains(read));
        assert!(rw.contains(write));
        assert!(!read.contains(write));

        let mut caps = CapBits::NONE;
        caps |= read;
        assert!(caps.contains(read));
        caps &= !read;
        assert!(!caps.contains(read));
    }

    #[test]
    fn audit_entry_newtype_fields() {
        // UT-07 (2026-09-26): 注册侧 pwm::audit::entry 三条例行迁入 —
        // AuditAction/AuditResult 判别式取值 (Create=3 / Success=0) 与 pwm newtype 取值.
        // (判据对象为 privileged::sgeg::types 内的类型, 故归本模块而非 functions/sgeg/audit.rs.)
        let entry = AuditEntry {
            timestamp: 1000,
            pwm: PwmId(42),
            action: AuditAction::Create,
            result: AuditResult::Success,
            target_pwm: PwmId(0),
            details: 0,
        };
        assert_eq!(entry.pwm.as_u64(), 42);
        assert_eq!(entry.action.as_u32(), 3);
        assert_eq!(entry.result.as_u32(), 0);
    }
}
