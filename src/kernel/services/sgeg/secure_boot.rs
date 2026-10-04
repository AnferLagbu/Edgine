#![deny(unsafe_code)]
//! 安全启动 + TPM 安全代理 — services 层 (0 unsafe)
//!
//! 封装 `framework::sgeg::secure_boot` 的安全 API.

// 重导出强类型
pub use crate::framework::sgeg::{
    ED25519_PUBKEY_LEN, ED25519_SIG_LEN, Ed25519PubKey, MAX_TRUST_CHAIN_DEPTH, PCR_COUNT, PcrIndex,
    SHA256_LEN, SecureBootSubsystem, TpmQuote, TpmSealedData, TpmSubsystem, TrustEntry, TrustRole,
    VerifyResult,
};

use crate::framework::sgeg::{
    secure_boot_init, secure_boot_is_initialized, secure_boot_subsystem, sha256_extend,
    sha256_hash, sys_secure_boot, sys_tpm, tpm_init, tpm_is_initialized, tpm_subsystem,
};

/// 初始化安全启动
pub fn init_secure_boot(pk: Ed25519PubKey) {
    secure_boot_init(pk);
}

/// 安全启动是否已初始化
pub fn is_secure_boot_initialized() -> bool {
    secure_boot_is_initialized()
}

/// 获取全局安全启动子系统
pub fn secure_boot() -> &'static SecureBootSubsystem {
    secure_boot_subsystem()
}

/// 初始化 TPM
pub fn init_tpm() {
    tpm_init();
}

/// TPM 是否已初始化
pub fn is_tpm_initialized() -> bool {
    tpm_is_initialized()
}

/// 获取全局 TPM 子系统
pub fn tpm() -> &'static TpmSubsystem {
    tpm_subsystem()
}

/// 安全启动系统调用 (安全封装)
pub fn secure_boot_syscall(cmd: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    sys_secure_boot(cmd, a1, a2, a3)
}

/// TPM 系统调用 (安全封装)
pub fn tpm_syscall(cmd: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    sys_tpm(cmd, a1, a2, a3)
}

/// SHA-256 哈希 (安全封装)
pub fn hash_sha256(data: &[u8]) -> [u8; SHA256_LEN] {
    sha256_hash(data)
}

/// SHA-256 扩展 (安全封装)
pub fn hash_sha256_extend(a: &[u8; SHA256_LEN], b: &[u8; SHA256_LEN]) -> [u8; SHA256_LEN] {
    sha256_extend(a, b)
}
