#![deny(unsafe_code)]
//! SGEG 密码学原语 — services 层安全代理
//!
//! ## 状态 (v2.17, 2026-06-04)
//!
//! Phase 2.5 sgeg 迁移 2/2 (crypto / storage):
//! - [x] SHA-256 切片 API 替代原始调用
//! - [x] 密码盐生成 (强类型 `[u8; 16]`)
//! - [x] 常数时间比较 (替代 `==` 直接比较)
//! - [x] 持久化错误码翻译
//!
//! ## 迁移方法
//!
//! 1. 内部把 `&[u8]` 切片传给 `sgeg::sha256::sha256`
//! 2. services 层 0 unsafe — 所有 `unsafe` 在 framework `cpuid`/`rdrand` 内部
//! 3. 强类型 `Salt([u8; 16])` / `Hash([u8; 32])` 替代裸 `[u8; N]`
//!
//! 评估日期: 2026-06-04

use crate::framework::sgeg;

// ============================================================================
// 常量 (re-export)
// ============================================================================

/// PWM 密码哈希长度 (包含 32 字节 SHA-256 + 16 字节 salt)
pub const PWM_HASH_LEN: usize = sgeg::types::PWM_HASH_LEN;

/// PWM 盐长度
pub const PWM_SALT_LEN: usize = sgeg::types::PWM_SALT_LEN;

/// SHA-256 输出长度
pub const SHA256_LEN: usize = 32;

/// 盐长度
pub const SALT_LEN: usize = 16;

// ============================================================================
// 强类型
// ============================================================================

/// 盐 (16 字节)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Salt(pub [u8; SALT_LEN]);

impl Salt {
    /// 零盐
    pub const ZERO: Self = Self([0u8; SALT_LEN]);

    /// 从字节数组构造
    #[inline]
    pub const fn from_bytes(bytes: [u8; SALT_LEN]) -> Self {
        Self(bytes)
    }

    /// 生成随机盐 (基于硬件 RDRAND, fallback TSC 派生)
    pub fn generate() -> Self {
        Self(sgeg::csprng::generate_salt())
    }

    /// 字节数组视图
    #[inline]
    pub fn as_bytes(&self) -> &[u8; SALT_LEN] {
        &self.0
    }

    /// 可变字节数组视图
    #[inline]
    pub fn as_bytes_mut(&mut self) -> &mut [u8; SALT_LEN] {
        &mut self.0
    }
}

impl Default for Salt {
    fn default() -> Self {
        Self::ZERO
    }
}

/// SHA-256 哈希 (32 字节)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Sha256Hash(pub [u8; SHA256_LEN]);

impl Sha256Hash {
    /// 零哈希
    pub const ZERO: Self = Self([0u8; SHA256_LEN]);

    /// 从字节数组构造
    #[inline]
    pub const fn from_bytes(bytes: [u8; SHA256_LEN]) -> Self {
        Self(bytes)
    }

    /// 字节数组视图
    #[inline]
    pub fn as_bytes(&self) -> &[u8; SHA256_LEN] {
        &self.0
    }
}

impl Default for Sha256Hash {
    fn default() -> Self {
        Self::ZERO
    }
}

/// 密码哈希 (48 字节 = 32 字节 SHA-256 + 16 字节 salt)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PasswordHash {
    /// 完整 48 字节 (sha256 32 + salt 16)
    pub full: [u8; PWM_HASH_LEN],
}

impl PasswordHash {
    /// 零哈希
    pub const ZERO: Self = Self {
        full: [0u8; PWM_HASH_LEN],
    };

    /// 构造空 (零) 哈希
    #[inline]
    pub const fn new() -> Self {
        Self::ZERO
    }

    /// 提取 SHA-256 部分
    #[inline]
    pub fn digest(&self) -> Sha256Hash {
        let mut d = [0u8; SHA256_LEN];
        d.copy_from_slice(&self.full[..SHA256_LEN]);
        Sha256Hash(d)
    }

    /// 提取盐部分
    #[inline]
    pub fn salt(&self) -> Salt {
        let mut s = [0u8; SALT_LEN];
        s.copy_from_slice(&self.full[SHA256_LEN..SHA256_LEN + SALT_LEN]);
        Salt(s)
    }

    /// 从 sha256 + salt 组合
    pub fn from_parts(digest: Sha256Hash, salt: Salt) -> Self {
        let mut full = [0u8; PWM_HASH_LEN];
        full[..SHA256_LEN].copy_from_slice(&digest.0);
        full[SHA256_LEN..SHA256_LEN + SALT_LEN].copy_from_slice(&salt.0);
        Self { full }
    }
}

impl Default for PasswordHash {
    fn default() -> Self {
        Self::ZERO
    }
}

// ============================================================================
// SHA-256 API
// ============================================================================

/// 计算 SHA-256 哈希
///
/// **参数**:
/// - `data`: 输入数据
///
/// **返回**: 32 字节 SHA-256 哈希
#[inline]
pub fn sha256(data: &[u8]) -> Sha256Hash {
    // B07-07: sgeg::sha256::sha256 返回标准 32 字节, 直接包装.
    Sha256Hash(sgeg::sha256::sha256(data))
}

/// 计算 SHA-256 → 密码哈希 (含盐拉伸)
///
/// 用于 PWM 存储; 输入 = password || salt
pub fn password_hash(password: &[u8], salt: Salt) -> PasswordHash {
    let mut stretched = [0u8; 40 + SALT_LEN];
    let pwd_len = password.len().min(40);
    stretched[..pwd_len].copy_from_slice(&password[..pwd_len]);
    stretched[40..40 + SALT_LEN].copy_from_slice(&salt.0);
    // B07-07: salt 经 from_parts 显式写入 full[32..48].
    // 原实现把 sha256 的 48 字节返回直接当 full, salt 部分恒 0, 加盐失效
    // (相同密码产生相同存储值, 失去防彩虹表能力).
    let digest = sgeg::sha256::sha256(&stretched[..40 + SALT_LEN]);
    PasswordHash::from_parts(Sha256Hash(digest), salt)
}

// ============================================================================
// 常数时间比较 (防侧信道)
// ============================================================================

/// 常数时间比较两个字节数组
///
/// **安全**: 不因数据差异而早返回, 防止计时攻击
// B08-22: 委托 framework::sgeg::constant_time_eq 权威实现, 消除 services 侧重复体
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    sgeg::constant_time_eq(a, b)
}

/// 常数时间比较 SHA-256 哈希
#[inline]
pub fn ct_eq_hash(a: Sha256Hash, b: Sha256Hash) -> bool {
    ct_eq(&a.0, &b.0)
}

/// 常数时间比较盐
#[inline]
pub fn ct_eq_salt(a: Salt, b: Salt) -> bool {
    ct_eq(&a.0, &b.0)
}

/// 常数时间比较密码哈希
#[inline]
pub fn ct_eq_password(a: PasswordHash, b: PasswordHash) -> bool {
    ct_eq(&a.full, &b.full)
}

// ============================================================================
// 持久化错误
// ============================================================================

/// 持久化错误 — TD-20: 收敛到 `KernelError`, 3 字段存储特有 + 1 共享包装.
///
/// 字段说明:
///   - `BadMagic`: 数据库格式不匹配 (POSIX ENOEXEC=8, 但语义不通用)
///   - `UnsupportedVersion`: 数据库版本不兼容 (POSIX EPROTO=71, 罕见)
///   - `ChecksumMismatch`: CRC / 校验和不匹配
///   - `Kernel(KernelError)`: 共享错误 (PathTooLong→NameTooLong / `OpenFailed`
///     / IoFailed→Fault / Truncated / Other) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    /// 数据库格式不匹配 (魔数错误)
    BadMagic,
    /// 数据库版本不兼容
    UnsupportedVersion,
    /// CRC / 校验和不匹配
    ChecksumMismatch,
    /// 共享 `KernelError` 包装
    Kernel(crate::services::error::KernelError),
}

impl StorageError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::BadMagic => E::ENOEXEC,
            Self::UnsupportedVersion => E::EPROTO,
            Self::ChecksumMismatch => E::EBADMSG,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    /// 从内核 `i32` 错误码翻译
    pub fn from_i32(code: i32) -> Self {
        use crate::services::error::KernelError as K;
        match code {
            -1 => Self::Kernel(K::Other(code)),
            -2 => Self::Kernel(K::NameTooLong),
            -3 => Self::Kernel(K::Fault),
            -4 => Self::BadMagic,
            -5 => Self::UnsupportedVersion,
            -6 => Self::Kernel(K::NoSuchProcess),
            -7 => Self::ChecksumMismatch,
            other => Self::Kernel(K::Other(other)),
        }
    }
}

pub type StorageResult<T> = Result<T, StorageError>;

use crate::framework::syscall::Errno;

// ============================================================================
// 持久化 API
// ============================================================================

/// 保存数据库到磁盘 (`/pwm.db`)
///
/// # Errors
/// 当底层 `save_database` 返回非零错误码 (如 IO 失败、校验和不匹配) 时,
/// 返回由该错误码转换得到的 `Err(StorageError)`.
pub fn save_database() -> StorageResult<()> {
    let rc = super::persist::save_database();
    if rc == 0 {
        Ok(())
    } else {
        Err(StorageError::from_i32(rc))
    }
}

/// 从磁盘加载数据库
///
/// # Errors
/// 当底层 `load_database` 返回非零错误码 (如文件不存在、校验和不匹配) 时,
/// 返回由该错误码转换得到的 `Err(StorageError)`.
pub fn load_database() -> StorageResult<()> {
    let rc = super::persist::load_database();
    if rc == 0 {
        Ok(())
    } else {
        Err(StorageError::from_i32(rc))
    }
}

/// 从磁盘删除数据库
///
/// # Errors
/// 当底层 `remove_database` 返回非零错误码 (如删除失败) 时, 返回由该错误码
/// 转换得到的 `Err(StorageError)`.
pub fn remove_database() -> StorageResult<()> {
    let rc = super::persist::remove_database();
    if rc == 0 {
        Ok(())
    } else {
        Err(StorageError::from_i32(rc))
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn sha256_empty() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let h = sha256(b"");
        assert_eq!(h.0[0], 0xe3);
        assert_eq!(h.0[1], 0xb0);
        assert_eq!(h.0[2], 0xc4);
        assert_eq!(h.0[3], 0x42);
    }

    #[test]
    fn sha256_abc() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let h = sha256(b"abc");
        assert_eq!(h.0[0], 0xba);
        assert_eq!(h.0[1], 0x78);
        assert_eq!(h.0[2], 0x16);
        assert_eq!(h.0[3], 0xbf);
    }

    #[test]
    fn sha256_long_input() {
        // 1 字节 ~ 多个 64 字节分块
        let data = vec![0u8; 1000];
        let h = sha256(&data);
        // 不应 panic, 长度应正确
        assert_eq!(h.0.len(), SHA256_LEN);
    }

    #[test]
    fn ct_eq_works() {
        let a = [1u8, 2, 3];
        let b = [1u8, 2, 3];
        let c = [1u8, 2, 4];
        assert!(ct_eq(&a, &b));
        assert!(!ct_eq(&a, &c));
        assert!(!ct_eq(&a, &[1u8, 2]));
    }

    #[test]
    fn ct_eq_hash_works() {
        let a = Sha256Hash([1u8; SHA256_LEN]);
        let b = Sha256Hash([1u8; SHA256_LEN]);
        let c = Sha256Hash([2u8; SHA256_LEN]);
        assert!(ct_eq_hash(a, b));
        assert!(!ct_eq_hash(a, c));
    }

    #[test]
    fn salt_default_zero() {
        let s = Salt::default();
        assert_eq!(s, Salt::ZERO);
        assert_eq!(s.0[0], 0);
    }

    #[test]
    fn password_hash_parts() {
        let salt = Salt::from_bytes([0xAA; SALT_LEN]);
        let digest = Sha256Hash([0xBB; SHA256_LEN]);
        let ph = PasswordHash::from_parts(digest, salt);
        assert_eq!(ph.digest(), digest);
        assert_eq!(ph.salt(), salt);
    }

    #[test]
    fn storage_error_translation() {
        assert_eq!(
            StorageError::from_i32(-1),
            StorageError::Kernel(crate::services::error::KernelError::Other(-1))
        );
        assert_eq!(
            StorageError::from_i32(-3),
            StorageError::Kernel(crate::services::error::KernelError::Fault)
        );
        assert_eq!(StorageError::from_i32(-4), StorageError::BadMagic);
        assert_eq!(
            StorageError::from_i32(0),
            StorageError::Kernel(crate::services::error::KernelError::Other(0))
        );
    }

    #[test]
    fn password_hash_deterministic() {
        let salt = Salt::from_bytes([0x42; SALT_LEN]);
        let pwd = b"hello_world";
        let h1 = password_hash(pwd, salt);
        let h2 = password_hash(pwd, salt);
        assert_eq!(h1, h2);
    }
}
