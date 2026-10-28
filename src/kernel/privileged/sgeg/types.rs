#![deny(unsafe_code)]
#![deny(missing_docs)]
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

/// PWM 表的最大条目数 (固定容量静态表).
pub const MAX_PWM_ENTRIES: usize = 256;
/// 条目备注字段的字节容量 (含结尾 NUL).
pub const PWM_NOTE_LEN: usize = 64;
/// 条目密码哈希字段的字节容量.
pub const PWM_HASH_LEN: usize = 48;
/// 密码派生所用盐值的字节长度.
pub const PWM_SALT_LEN: usize = 16;
/// 密码派生输出的摘要字节长度.
pub const PWM_DIGEST_LEN: usize = 32;
/// 授权记录表的最大条目数.
pub const MAX_GRANT_RECORDS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// PWM 唯一标识; 0 表示无效/未分配。
#[repr(transparent)]
pub struct PwmId(pub u64);

impl PwmId {
    /// 无效/未分配标识的哨兵值.
    pub const ZERO: Self = Self(0);

    /// 判断该标识是否为已分配 (非零).
    pub fn is_valid(self) -> bool {
        self.0 != 0
    }

    /// 取出内部 u64 原始值.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl Default for PwmId {
    fn default() -> Self {
        Self::ZERO
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// 域身份 ID (DID); 由 uid 派生, 0 表示无活动域。
#[repr(transparent)]
pub struct DomainId(pub u64);

impl DomainId {
    /// 无活动域的哨兵值.
    pub const ZERO: Self = Self(0);
    /// 内核自身域的保留编号 (uid 1).
    pub const KERNEL: Self = Self(1);
    /// 根用户域的保留编号 (uid 1000).
    pub const ROOT: Self = Self(1000);
    /// 无特权用户域的保留编号 (uid 65534).
    pub const NOBODY: Self = Self(65534);

    /// 判断是否关联了活动域 (非零).
    pub fn is_valid(self) -> bool {
        self.0 != 0
    }

    /// 取出内部 u64 原始值.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// 由 POSIX uid 派生域身份.
    pub fn from_uid(uid: u32) -> Self {
        Self(u64::from(uid))
    }

    /// 转换为对应的 POSIX uid (截断为 u32).
    pub fn to_uid(self) -> u32 {
        crate::privileged::lib::low_u32_from_u64(self.0)
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
    /// 域级行为门控标志 (按进程生效, 在 syscall 咽喉点裁决).
    pub struct DomainFlags: u32 {
        /// 无门控 (默认值, 不改变任何调用行为).
        const NONE       = 0;
        /// 拒绝 fork/clone/clone3.
        const NO_FORK    = 1 << 0;
        /// 拒绝 execve/execveat.
        const NO_EXEC    = 1 << 1;
        /// 拒绝网络类系统调用.
        const NO_NET     = 1 << 2;
        /// 拒绝设备类调用 (ioctl/mount/umount2).
        const NO_DEVICE  = 1 << 3;
        /// 复合位: 严格白名单, 仅放行 read/write/exit/exit_group/rt_sigreturn.
        const SANDBOX    = 1 << 4;
        /// 拒绝写类调用及带写意图的 open/openat.
        const READONLY   = 1 << 5;
        /// 元数据位: 生命周期标记, 不参与门控.
        const TEMP       = 1 << 6;
        /// 元数据位: 特权标记, 不参与门控.
        const SYSTEM     = 1 << 7;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// 能力域编号 (0-15), 标识一类受控资源。
#[repr(transparent)]
pub struct CapDomain(pub u16);

impl CapDomain {
    /// 系统管理与全局策略域.
    pub const SYSTEM: Self = Self(0);
    /// 文件系统域.
    pub const FS: Self = Self(1);
    /// 网络栈域.
    pub const NET: Self = Self(2);
    /// 进程管理域.
    pub const PROC: Self = Self(3);
    /// 设备访问域.
    pub const DEVICE: Self = Self(4);
    /// 用户与身份管理域.
    pub const USER_MGMT: Self = Self(5);
    /// 进程间通信域.
    pub const IPC: Self = Self(6);
    /// 内存管理域.
    pub const MEM: Self = Self(7);
    /// 时钟与定时器域.
    pub const TIME: Self = Self(8);
    /// 文件记录 (FREG) 域.
    pub const FREG: Self = Self(9);
    /// 信号投递域.
    pub const SIGNAL: Self = Self(10);
    /// 共享内存域.
    pub const SHM: Self = Self(11);
    /// 信号量域.
    pub const SEM: Self = Self(12);
    /// 消息队列域.
    pub const MSGQ: Self = Self(13);
    /// DMA 缓冲区域.
    pub const DMA: Self = Self(14);
    /// 预留域 (未分配语义).
    pub const RESERVED: Self = Self(15);

    /// 转为能力矩阵下标 (对 16 取模, 保证不越界).
    pub fn as_usize(self) -> usize {
        (self.0 as usize) % 16
    }

    /// 取出内部 u16 原始值.
    pub fn as_u16(self) -> u16 {
        self.0
    }
}

impl From<u16> for CapDomain {
    fn from(v: u16) -> Self {
        Self(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// 单个能力域内的能力位图 (u64)。
#[repr(transparent)]
pub struct CapBits(pub u64);

impl CapBits {
    /// 空能力集 (无任何位).
    pub const NONE: Self = Self(0);
    /// 全集: 所有能力位均置位.
    pub const ALL: Self = Self(0xFFFF_FFFF_FFFF_FFFF);

    /// 判断当前集合是否完整包含另一集合的全部位.
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// 取出内部 u64 位图.
    pub fn as_u64(self) -> u64 {
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
    /// PWM 条目的生命周期与状态标志.
    pub struct PwmFlags: u16 {
        /// 无标志 (默认值).
        const NONE       = 0;
        /// 条目已被禁用, 身份校验直接拒绝.
        const DISABLED   = 1 << 0;
        /// 条目已变更 (待持久化的差异标记).
        const MODIFIED   = 1 << 3;
        /// 连续登录失败达上限后被临时锁定.
        const LOCKED     = 1 << 4;
    }
}

impl Default for PwmFlags {
    fn default() -> Self {
        Self::NONE
    }
}

/// PWM 表条目: 身份的标识、凭证、标志、能力与登录状态的原子化字段集合。
#[repr(C)]
pub struct PwmEntry {
    /// 该条目的 PWM 标识; 0 表示空槽.
    pub pwm: AtomicU64,
    /// 映射的 POSIX 用户 ID.
    pub posix_uid: AtomicU32,
    /// 映射的 POSIX 组 ID.
    pub posix_gid: AtomicU32,
    /// 创建该条目的 PWM 标识 (权限授予链的源头).
    pub creator_pwm: AtomicU64,
    /// 特权级别; 默认 0xFF 表示未设置.
    pub privilege_level: AtomicU8,
    /// 标志位原始值 (见 [`PwmFlags`]).
    pub flags: AtomicU16,
    /// 16 个能力域的能力位图 (每域一个 u64).
    pub caps: [AtomicU64; 16],
    /// 备注 (T4-1 全 Atomic 化: u8 数组 → `AtomicU8` 数组, 支持 &self 写入)
    pub note: [AtomicU8; PWM_NOTE_LEN],
    /// 密码哈希 (T4-1 全 Atomic 化: u8 数组 → `AtomicU8` 数组, 支持 &self 写入)
    pub password_hash: [AtomicU8; PWM_HASH_LEN],
    /// 条目创建时间戳.
    pub created_time: AtomicU64,
    /// 凭证过期时间戳; 0 表示永不过期.
    pub expires_at: AtomicU64,
    /// 登录失败锁定的截止时间戳.
    pub lockout_until: AtomicU64,
    /// 连续登录失败次数.
    pub failed_attempts: AtomicU32,
    /// 最近一次成功登录时间戳.
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
    /// 创建全默认 (未分配) 的条目.
    pub fn new() -> Self {
        Self::default()
    }

    /// 判断条目是否已分配 (PWM 标识非零).
    pub fn is_valid(&self) -> bool {
        self.pwm.load(core::sync::atomic::Ordering::Acquire) != 0
    }

    /// 读取条目自身的 PWM 标识.
    pub fn get_pwm(&self) -> PwmId {
        PwmId(self.pwm.load(core::sync::atomic::Ordering::Acquire))
    }

    /// 读取创建者 PWM 标识.
    pub fn get_creator_pwm(&self) -> PwmId {
        PwmId(self.creator_pwm.load(core::sync::atomic::Ordering::Acquire))
    }

    /// 读取标志位并转为 [`PwmFlags`] (未知位被丢弃).
    pub fn get_flags(&self) -> PwmFlags {
        PwmFlags::from_bits_truncate(self.flags.load(core::sync::atomic::Ordering::Acquire))
    }

    /// 整体覆盖标志位.
    pub fn set_flags(&self, flags: PwmFlags) {
        self.flags
            .store(flags.bits(), core::sync::atomic::Ordering::Release);
    }

    /// 按位或加入指定标志.
    pub fn add_flags(&self, flags: PwmFlags) {
        let current = self.get_flags();
        self.set_flags(current | flags);
    }

    /// 按位清除指定标志.
    pub fn remove_flags(&self, flags: PwmFlags) {
        let current = self.get_flags();
        self.set_flags(current & !flags);
    }

    /// 判断是否置位了全部给定标志.
    pub fn has_flag(&self, flag: PwmFlags) -> bool {
        self.get_flags().contains(flag)
    }

    /// T4-1: 全 Atomic 化后此 API 行为变化.
    /// 原因: [`AtomicU8`; N] 不能直接借用为 &[u8], 返回 owned &str 需要内部静态缓冲.
    /// 当前实现: 返回静态空串占位. 推荐使用 `note_bytes()` 复制 + 自行转换.
    /// 兼容保留: 签名不变, 行为退化为"返回空串 (新值前为 None)". 调用方应迁移.
    pub fn get_note_str() -> &'static str {
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

    /// 读取指定能力域的能力位图.
    pub fn load_caps(&self, domain: CapDomain) -> CapBits {
        let idx = domain.as_usize();
        CapBits(self.caps[idx].load(core::sync::atomic::Ordering::Acquire))
    }

    /// 整体覆盖指定能力域的能力位图.
    pub fn store_caps(&self, domain: CapDomain, caps: CapBits) {
        let idx = domain.as_usize();
        self.caps[idx].store(caps.0, core::sync::atomic::Ordering::Release);
    }

    /// 对指定能力域原子按位或 (用于授予能力).
    pub fn fetch_or_caps(&self, domain: CapDomain, caps: CapBits) {
        let idx = domain.as_usize();
        self.caps[idx].fetch_or(caps.0, core::sync::atomic::Ordering::AcqRel);
    }

    /// 对指定能力域原子按位与 (用于回收能力).
    pub fn fetch_and_caps(&self, domain: CapDomain, caps: CapBits) {
        let idx = domain.as_usize();
        self.caps[idx].fetch_and(caps.0, core::sync::atomic::Ordering::AcqRel);
    }

    /// 判断指定能力域是否已具备所需能力位 (子集判定).
    pub fn has_capability(&self, domain: CapDomain, required: CapBits) -> bool {
        let current = self.load_caps(domain);
        current.contains(required)
    }

    /// 读取映射的 POSIX 用户 ID.
    pub fn get_uid(&self) -> u32 {
        self.posix_uid.load(core::sync::atomic::Ordering::Acquire)
    }

    /// 读取映射的 POSIX 组 ID.
    pub fn get_gid(&self) -> u32 {
        self.posix_gid.load(core::sync::atomic::Ordering::Acquire)
    }

    /// 写入映射的 POSIX 用户 ID.
    pub fn set_uid(&self, uid: u32) {
        self.posix_uid
            .store(uid, core::sync::atomic::Ordering::Release);
    }

    /// 写入映射的 POSIX 组 ID.
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

/// 进程级会话上下文: 登录身份、缓存的 uid/gid、有效与保存的 uid/gid、活动域与提权授权。
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct PwmContext {
    /// 指向该会话当前生效的 PWM 条目 (只读引用).
    pub current_entry: *const PwmEntry,
    /// 本次登录会话绑定的 PWM 标识.
    pub session_pwm: PwmId,
    /// 缓存的 POSIX 用户 ID (热路径免重复解析).
    pub cached_uid: u32,
    /// 缓存的 POSIX 组 ID (热路径免重复解析).
    pub cached_gid: u32,
    /// 有效用户 ID (euid).
    pub euid: u32,
    /// 有效组 ID (egid).
    pub egid: u32,
    /// 保存的用户 ID (供临时提权后恢复).
    pub saved_euid: u32,
    /// 保存的组 ID (供临时提权后恢复).
    pub saved_egid: u32,
    /// 当前活动域身份.
    pub active_domain_id: DomainId,
    /// 提权所依据的授权方 PWM 标识.
    pub elevation_granted_pwm: PwmId,
}

/// 授权记录: 记录授权方对被授权方在指定能力域上授予的能力。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct GrantRecord {
    /// 授权方 PWM 标识.
    pub grantor_pwm: PwmId,
    /// 被授权方 PWM 标识.
    pub grantee_pwm: PwmId,
    /// 授予的能力域.
    pub domain: CapDomain,
    /// 授予的能力位集合.
    pub caps: CapBits,
    /// 授权生效的时间戳.
    pub granted_at: u64,
}

impl GrantRecord {
    /// 空记录: 表示未占用的授权槽位.
    pub const EMPTY: Self = Self {
        grantor_pwm: PwmId::ZERO,
        grantee_pwm: PwmId::ZERO,
        domain: CapDomain(0),
        caps: CapBits::NONE,
        granted_at: 0,
    };

    /// 判断记录是否为未占用槽位 (授权方为空).
    pub fn is_empty(&self) -> bool {
        self.grantor_pwm == PwmId::ZERO
    }
}

/// SGEG 操作错误码; 0 为成功, 负值编码对应各类失败原因。
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwmError {
    /// 操作成功.
    Ok = 0,
    /// 目标 PWM 条目不存在.
    NotFound = -1,
    /// 目标条目已被禁用.
    Disabled = -2,
    /// 密码校验失败.
    PasswordIncorrect = -3,
    /// 调用方权限不足 (门控拒绝).
    PermissionDenied = -4,
    /// PWM 表已满, 无法创建新条目.
    TableFull = -5,
    /// 同标识的条目已存在.
    AlreadyExists = -6,
    /// 目标特权级别高于允许授予的上限.
    InsufficientPrivilege = -7,
    /// 缺少完成该操作所需的授权.
    NotAuthorized = -8,
    /// 调用方不是该条目的创建者.
    NotCreator = -9,
    /// 操作会突破特权下界约束.
    WouldBreakFloor = -10,
    /// 特权级别或能力位溢出上限.
    PrivilegeOverflow = -11,
    /// 首次令牌已被使用过.
    TokenUsed = -12,
    /// 尚未签发首次令牌.
    NoFirstToken = -13,
    /// 密码格式或内容不合法.
    InvalidPassword = -14,
}

impl PwmError {
    /// 取出错误码的 i32 表示 (0 成功, 负值失败).
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// 审计动作类型 (登录/登出/创建/删除/授权等)。
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default)]
pub enum AuditAction {
    /// 登录.
    #[default]
    Login = 1,
    /// 登出.
    Logout = 2,
    /// 创建身份条目.
    Create = 3,
    /// 删除身份条目.
    Delete = 4,
    /// 修改身份条目.
    Modify = 5,
    /// 修改密码.
    PasswordChange = 8,
    /// 授予能力.
    Grant = 10,
    /// 回收能力.
    Revoke = 11,
    /// 转移创建者.
    TransferCreator = 12,
    /// 签发首次令牌.
    FirstTokenGrant = 13,
}

impl AuditAction {
    /// 取出动作的 u32 判别式 (与磁盘审计格式一致).
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// 审计结果 (成功/失败/拒绝)。
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default)]
pub enum AuditResult {
    /// 操作成功.
    #[default]
    Success = 0,
    /// 操作失败.
    Failure = 1,
    /// 被门控拒绝.
    Denied = 2,
}

impl AuditResult {
    /// 取出结果的 u32 判别式 (与磁盘审计格式一致).
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// 单条审计记录 (时间戳、主体 PWM、动作、结果、目标 PWM 与细节)。
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct AuditEntry {
    /// 事件发生的时间戳.
    pub timestamp: u64,
    /// 执行动作的主体 PWM 标识.
    pub pwm: PwmId,
    /// 审计动作类型.
    pub action: AuditAction,
    /// 动作执行结果.
    pub result: AuditResult,
    /// 动作作用的目标 PWM 标识 (无目标时为 0).
    pub target_pwm: PwmId,
    /// 动作相关的附加细节 (语义由动作类型决定).
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
