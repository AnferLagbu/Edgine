#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! madvise / mlock / mincore — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 封装 `privileged::proc::madvise_mlock` 的内存建议与锁定能力
//!
//! ## API 形态
//!
//! 与 Linux `<sys/mman.h>` API 对齐:
//!
//! ```ignore
//! use crate::functions::proc::madvise_mlock as ml;
//!
//! // 内存建议
//! ml::madvise(addr, len, Advice::Sequential)?;
//!
//! // 内存锁定
//! ml::mlock(addr, len)?;
//! ml::munlock(addr, len)?;
//!
//! // 进程级锁定
//! ml::mlockall(MlockAllFlags::CURRENT | MlockAllFlags::FUTURE)?;
//! ml::munlockall()?;
//!
//! // 驻留性查询
//! let mut vec = vec![0u8; pages];
//! ml::mincore(addr, len, &mut vec)?;
//! ```
//!
//! ## 注意事项
//!
//! - mlock 受 `RLIMIT_MEMLOCK` 限制, 超出返回 ENOMEM
//! - mlock 锁定的页不会被 swap/reclaim, 但会参与 `madvise(MADV_PAGEOUT)` 的忽略
//! - 进程退出时由 `privileged::proc::vma::MmStruct::release` 释放所有锁定

use crate::privileged::mm::PAGE_SIZE;
use crate::privileged::proc::madvise_mlock as fw_ml;
use crate::privileged::syscall::Errno;

// ============================================================================
// Advice 枚举
// ============================================================================

/// 内存访问模式建议
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advice {
    /// 无特殊偏好
    Normal = 0,
    /// 随机访问
    Random = 1,
    /// 顺序访问
    Sequential = 2,
    /// 即将访问
    WillNeed = 3,
    /// 不再需要, 立即释放 (匿名页: OOM 路径 / file page: drop)
    DontNeed = 4,
    /// 释放并清零
    Free = 5,
    /// 移除文件映射页 (shmem)
    Remove = 9,
    /// fork 时不继承
    DontFork = 10,
    /// fork 时继承
    DoFork = 11,
    /// KSM 合并
    Mergeable = 12,
    /// KSM 不可合并
    Unmergeable = 13,
    /// THP 优先
    HugePage = 14,
    /// 禁用 THP
    NoHugePage = 15,
    /// core dump 跳过
    DontDump = 16,
    /// core dump 包含
    DoDump = 17,
    /// fork 时清零
    WipeOnFork = 18,
    /// fork 时保留
    KeepOnFork = 19,
    /// 冷数据
    Cold = 20,
    /// 主动换出
    PageOut = 21,
}

impl Advice {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 从 Linux 原始 advice 值构造
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Normal,
            1 => Self::Random,
            2 => Self::Sequential,
            3 => Self::WillNeed,
            4 => Self::DontNeed,
            5 => Self::Free,
            9 => Self::Remove,
            10 => Self::DontFork,
            11 => Self::DoFork,
            12 => Self::Mergeable,
            13 => Self::Unmergeable,
            14 => Self::HugePage,
            15 => Self::NoHugePage,
            16 => Self::DontDump,
            17 => Self::DoDump,
            18 => Self::WipeOnFork,
            19 => Self::KeepOnFork,
            20 => Self::Cold,
            21 => Self::PageOut,
            _ => Self::Normal,
        }
    }

    /// 转换为 privileged 接受的值
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

// ============================================================================
// MlockAllFlags 位集
// ============================================================================

/// 进程级内存锁定标志
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MlockAllFlags(pub u32);

impl MlockAllFlags {
    /// 锁定已存在的映射
    pub const CURRENT: Self = Self(fw_ml::MCL_CURRENT);
    /// 锁定未来映射
    pub const FUTURE: Self = Self(fw_ml::MCL_FUTURE);
    /// 缺页时锁定 (与 FUTURE 联用)
    pub const ON_FAULT: Self = Self(fw_ml::MCL_ONFAULT);
    /// 空
    pub const EMPTY: Self = Self(0);

    #[inline]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for MlockAllFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

// ============================================================================
// 错误
// ============================================================================

/// madvise/mlock 操作错误 — TD-19: 收敛到 `KernelError`, 1 字段 mlock 特有 + 1 共享包装.
///
/// 字段说明:
///   - `NotMapped`: 当前进程无 `MmStruct` (kernel thread 路径), 走 ESRCH
///   - `Kernel(KernelError)`: 共享错误 (InvalidArgument/BadAddress/OutOfMemory/
///     NoResources/PermissionDenied 等) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlockError {
    /// 当前进程无 `MmStruct` (kernel thread 路径)
    NotMapped,
    /// 共享 `KernelError` 包装
    Kernel(crate::functions::error::KernelError),
}

impl MlockError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::NotMapped => E::ESRCH,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    pub fn from_errno(e: Errno) -> Self {
        use crate::functions::error::KernelError as K;
        match e {
            Errno::EINVAL => Self::Kernel(K::InvalidArgument),
            Errno::EFAULT => Self::Kernel(K::Fault),
            Errno::ENOMEM => Self::Kernel(K::NoMemory),
            Errno::ENOSPC => Self::Kernel(K::NoSpace),
            Errno::EAGAIN => Self::Kernel(K::WouldBlock),
            Errno::EPERM => Self::Kernel(K::PermissionDenied),
            Errno::ESRCH => Self::NotMapped,
            other => Self::Kernel(K::Other(other.as_ret() as i32)),
        }
    }
}

pub type MlockResult<T> = Result<T, MlockError>;

// ============================================================================
// API: madvise
// ============================================================================

/// 内存建议 - 用户态包装
///
/// 返回 0 = 成功, `MlockError` 表示失败原因
///
/// # Errors
///
/// 当底层 `madvise` 返回非零时, 将对应 errno 转换为 `MlockError`.
pub fn madvise(addr: usize, len: usize, advice: Advice) -> MlockResult<()> {
    let rc = fw_ml::sys_madvise(addr as u64, len as u64, u64::from(advice.as_u32()));
    if rc == 0 {
        Ok(())
    } else {
        Err(MlockError::from_errno(Errno::from_ret(rc)))
    }
}

// ============================================================================
// API: mlock / munlock
// ============================================================================

/// 锁定 [addr, addr+len) 物理页
///
/// 受 `RLIMIT_MEMLOCK` 限制. 锁定页不会被 swap/reclaim.
///
/// # Errors
///
/// 当超出锁定限额、地址非法或内存不足时, 返回对应的 `MlockError`.
pub fn mlock(addr: usize, len: usize) -> MlockResult<()> {
    let rc = fw_ml::sys_mlock(addr as u64, len as u64);
    if rc == 0 {
        Ok(())
    } else {
        Err(MlockError::from_errno(Errno::from_ret(rc)))
    }
}

/// 解除 [addr, addr+len) 物理页锁定
///
/// # Errors
///
/// 当底层 `munlock` 返回非零时, 返回对应的 `MlockError`.
pub fn munlock(addr: usize, len: usize) -> MlockResult<()> {
    let rc = fw_ml::sys_munlock(addr as u64, len as u64);
    if rc == 0 {
        Ok(())
    } else {
        Err(MlockError::from_errno(Errno::from_ret(rc)))
    }
}

// ============================================================================
// API: mlockall / munlockall
// ============================================================================

/// 进程级锁定 (`MCL_CURRENT` 锁现有 / `MCL_FUTURE` 锁未来)
///
/// # Errors
///
/// 当底层 `mlockall` 返回非零(如超出限额)时, 返回对应的 `MlockError`.
pub fn mlockall(flags: MlockAllFlags) -> MlockResult<()> {
    let rc = fw_ml::sys_mlockall(u64::from(flags.bits()));
    if rc == 0 {
        Ok(())
    } else {
        Err(MlockError::from_errno(Errno::from_ret(rc)))
    }
}

/// 解除进程级所有锁定
///
/// # Errors
///
/// 当底层 `munlockall` 返回非零时, 返回对应的 `MlockError`.
pub fn munlockall() -> MlockResult<()> {
    let rc = fw_ml::sys_munlockall();
    if rc == 0 {
        Ok(())
    } else {
        Err(MlockError::from_errno(Errno::from_ret(rc)))
    }
}

// ============================================================================
// API: mincore
// ============================================================================

/// 查询 [addr, addr+len) 每页驻留性
///
/// `vec` 长度必须 >= len / `page_size`, 每字节 1=驻留 0=未驻留
///
/// # Errors
///
/// 当 `vec` 长度不足以容纳查询结果时返回 `InvalidArgument`; 底层查询
/// 返回非零时转换为对应的 `MlockError`.
pub fn mincore(addr: usize, len: usize, vec: &mut [u8]) -> MlockResult<()> {
    let expected_pages = (len + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
    if vec.len() < expected_pages {
        return Err(MlockError::Kernel(
            crate::functions::error::KernelError::InvalidArgument,
        ));
    }
    let rc = fw_ml::sys_mincore(addr as u64, len as u64, vec.as_mut_ptr() as u64);
    if rc == 0 {
        Ok(())
    } else {
        Err(MlockError::from_errno(Errno::from_ret(rc)))
    }
}
