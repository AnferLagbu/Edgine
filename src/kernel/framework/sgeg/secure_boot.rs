//! 安全启动 + TPM 2.0 信任链
//!
//! ## 设计
//!
//! 1. **Secure Boot**: 基于公钥签名的引导链验证
//!    - 根密钥 (Platform Key) → 签名密钥 (Key Exchange Key) → 内核镜像签名
//!    - 验证流程: 加载镜像 → 计算哈希 → 验证签名 → 通过/拒绝
//!
//! 2. **TPM 2.0**: 可信平台模块抽象
//!    - 度量 (Extend): 将启动组件哈希扩展到 PCR
//!    - 密封 (Seal): 绑定数据到 PCR 状态
//!    - 报价 (Quote): 远程证明 PCR 值
//!    - 当前为软件模拟实现 (无硬件 TPM 时回退)
//!
//! ### 与 Linux 的差异
//!
//! 1. **签名算法**: 仅支持 Ed25519 (不实现 RSA/ECDSA)
//! 2. **PCR 数量**: 8 个 (Linux TPM 有 24 个)
//! 3. **固件验证**: 不实现 UEFI Secure Boot 变量, 仅内核级验证
//! 4. **TPM**: 软件模拟, 后续可对接硬件 TIS/CRB 接口
//!
//! ## SAFETY
//!
//! 本模块属于 framework/TCB, 允许 unsafe.
//! 签名验证失败将拒绝加载, 不可绕过.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::framework::sync::IrqSpinLock;
use alloc::vec::Vec;

// ============================================================================
// 常量
// ============================================================================

/// SHA-256 哈希长度
pub const SHA256_LEN: usize = 32;
/// Ed25519 公钥长度
pub const ED25519_PUBKEY_LEN: usize = 32;
/// Ed25519 签名长度
pub const ED25519_SIG_LEN: usize = 64;
/// PCR 数量
pub const PCR_COUNT: usize = 8;
/// 最大信任链深度
pub const MAX_TRUST_CHAIN_DEPTH: usize = 4;

// ============================================================================
// SHA-256 (B08-19: 委托 services::sgeg::sha256 规范实现,
// 删除本地重复 K 常量表/填充/轮函数 — 原注释"后者输出 48 字节"已被 B07-07 证伪)
// ============================================================================

use crate::framework::sgeg::sha256::sha256;

/// 计算 SHA-256 哈希 (标准 32 字节输出, 委托 services 规范实现)
pub fn sha256_hash(data: &[u8]) -> [u8; SHA256_LEN] {
    sha256(data)
}

/// SHA-256 扩展 (hash1 || hash2 → hash)
pub fn sha256_extend(a: &[u8; SHA256_LEN], b: &[u8; SHA256_LEN]) -> [u8; SHA256_LEN] {
    let mut combined = [0u8; SHA256_LEN * 2];
    combined[..SHA256_LEN].copy_from_slice(a);
    combined[SHA256_LEN..].copy_from_slice(b);
    sha256_hash(&combined)
}

// ============================================================================
// Ed25519 签名验证 (简化实现)
// ============================================================================

/// Ed25519 公钥
#[derive(Debug, Clone)]
pub struct Ed25519PubKey {
    pub key: [u8; ED25519_PUBKEY_LEN],
}

impl Ed25519PubKey {
    pub fn new(key: [u8; ED25519_PUBKEY_LEN]) -> Self {
        Self { key }
    }

    /// 验证 Ed25519 签名 (RFC 8032)
    ///
    /// B07-06 (DECISION-078): 使用 ed25519-dalek 真实验证, 替换原占位实现
    /// (签名非全零即视为有效, 攻击者可伪造任意签名通过 Secure Boot).
    /// 采用 `verify_strict` 拒绝非规范 (malleable) 签名, 与 RFC 8032 一致.
    pub fn verify(&self, message: &[u8], signature: &[u8; ED25519_SIG_LEN]) -> bool {
        let Ok(pk) = ed25519_dalek::VerifyingKey::from_bytes(&self.key) else {
            return false;
        };
        let sig = ed25519_dalek::Signature::from_bytes(signature);
        pk.verify_strict(message, &sig).is_ok()
    }
}

// ============================================================================
// Secure Boot — 信任链验证
// ============================================================================

/// 信任链角色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TrustRole {
    /// 平台密钥 (PK) — 信任链根
    Platform = 0,
    /// 密钥交换密钥 (KEK) — 签名其他密钥
    KeyExchange = 1,
    /// 镜像签名密钥 (DB) — 签名内核/模块
    ImageSigning = 2,
}

/// 信任链条目
#[derive(Debug, Clone)]
pub struct TrustEntry {
    /// 角色
    pub role: TrustRole,
    /// 公钥
    pub pubkey: Ed25519PubKey,
    /// 签名 (由上级密钥签名)
    pub signature: [u8; ED25519_SIG_LEN],
    /// 签名者公钥索引 (在信任链中的位置)
    pub signer_idx: Option<usize>,
}

/// 验证结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    /// 验证通过
    Ok,
    /// 签名无效
    InvalidSignature,
    /// 信任链断裂
    ChainBroken,
    /// 未知密钥
    UnknownKey,
    /// Secure Boot 未启用
    NotEnabled,
}

/// 安全启动子系统
pub struct SecureBootSubsystem {
    /// 信任链 (从 PK → KEK → DB)
    trust_chain: IrqSpinLock<Vec<TrustEntry>>,
    /// 是否启用
    enabled: AtomicBool,
    /// 是否已锁定 (启动后锁定, 不可再添加密钥)
    locked: AtomicBool,
    /// 验证失败次数
    verify_fail_count: AtomicU32,
    /// 验证通过次数
    verify_ok_count: AtomicU32,
    /// 是否已初始化
    initialized: AtomicBool,
}

impl SecureBootSubsystem {
    pub const fn new() -> Self {
        Self {
            trust_chain: IrqSpinLock::new(Vec::new()),
            enabled: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            verify_fail_count: AtomicU32::new(0),
            verify_ok_count: AtomicU32::new(0),
            initialized: AtomicBool::new(false),
        }
    }

    /// 初始化安全启动子系统
    pub fn init(&self, platform_key: Ed25519PubKey) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        // 插入平台密钥 (自签名, 信任链根)
        let pk_entry = TrustEntry {
            role: TrustRole::Platform,
            pubkey: platform_key,
            signature: [0u8; ED25519_SIG_LEN],
            signer_idx: None, // PK 自签
        };
        self.trust_chain.lock().push(pk_entry);
        self.enabled.store(true, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
        crate::klog_ffi!(klog_ffi_info, "[SecureBoot] initialized with Platform Key");
    }

    /// 添加信任链条目 (KEK/DB)
    pub fn add_trust_entry(&self, entry: TrustEntry) -> VerifyResult {
        if self.locked.load(Ordering::Acquire) {
            return VerifyResult::ChainBroken;
        }
        // 验证签名者存在
        if let Some(signer_idx) = entry.signer_idx {
            let chain = self.trust_chain.lock();
            if signer_idx >= chain.len() {
                return VerifyResult::UnknownKey;
            }
            let signer = &chain[signer_idx];
            // 验证签名
            let pubkey_bytes = &entry.pubkey.key;
            if !signer.pubkey.verify(pubkey_bytes, &entry.signature) {
                return VerifyResult::InvalidSignature;
            }
        } else if entry.role != TrustRole::Platform {
            // 只有 PK 可以自签
            return VerifyResult::ChainBroken;
        }
        self.trust_chain.lock().push(entry);
        VerifyResult::Ok
    }

    /// 验证镜像签名
    pub fn verify_image(&self, image: &[u8], signature: &[u8; ED25519_SIG_LEN]) -> VerifyResult {
        if !self.enabled.load(Ordering::Acquire) {
            return VerifyResult::NotEnabled;
        }
        let chain = self.trust_chain.lock();
        // 查找 DB 角色的密钥
        let db_keys: Vec<&TrustEntry> = chain
            .iter()
            .filter(|e| e.role == TrustRole::ImageSigning)
            .collect();
        if db_keys.is_empty() {
            // 回退: 用 KEK 验证
            let kek_keys: Vec<&TrustEntry> = chain
                .iter()
                .filter(|e| e.role == TrustRole::KeyExchange)
                .collect();
            if kek_keys.is_empty() {
                // 最后回退: 用 PK 验证
                let pk_keys: Vec<&TrustEntry> = chain
                    .iter()
                    .filter(|e| e.role == TrustRole::Platform)
                    .collect();
                for pk in pk_keys {
                    if pk.pubkey.verify(image, signature) {
                        self.verify_ok_count.fetch_add(1, Ordering::Relaxed);
                        return VerifyResult::Ok;
                    }
                }
            } else {
                for kek in kek_keys {
                    if kek.pubkey.verify(image, signature) {
                        self.verify_ok_count.fetch_add(1, Ordering::Relaxed);
                        return VerifyResult::Ok;
                    }
                }
            }
        } else {
            for db in db_keys {
                if db.pubkey.verify(image, signature) {
                    self.verify_ok_count.fetch_add(1, Ordering::Relaxed);
                    return VerifyResult::Ok;
                }
            }
        }
        self.verify_fail_count.fetch_add(1, Ordering::Relaxed);
        VerifyResult::InvalidSignature
    }

    /// 锁定信任链 (启动完成后调用)
    pub fn lock(&self) {
        self.locked.store(true, Ordering::Release);
    }

    /// 是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// 是否锁定
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Acquire)
    }

    /// 获取验证统计
    pub fn stats(&self) -> (u32, u32) {
        (
            self.verify_ok_count.load(Ordering::Acquire),
            self.verify_fail_count.load(Ordering::Acquire),
        )
    }
}

// ============================================================================
// TPM 2.0 — 可信平台模块 (软件模拟)
// ============================================================================

/// PCR 索引
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PcrIndex {
    /// PCR0: 引导固件度量
    Firmware = 0,
    /// PCR1: 引导配置度量
    BootConfig = 1,
    /// PCR2: 内核镜像度量
    KernelImage = 2,
    /// PCR3: 内核命令行度量
    KernelCmdline = 3,
    /// PCR4: 模块度量
    Modules = 4,
    /// PCR5: 文件系统度量
    FileSystem = 5,
    /// PCR6: 安全策略度量
    SecurityPolicy = 6,
    /// PCR7: 应用自定义度量
    Application = 7,
}

impl PcrIndex {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Firmware),
            1 => Some(Self::BootConfig),
            2 => Some(Self::KernelImage),
            3 => Some(Self::KernelCmdline),
            4 => Some(Self::Modules),
            5 => Some(Self::FileSystem),
            6 => Some(Self::SecurityPolicy),
            7 => Some(Self::Application),
            _ => None,
        }
    }
}

/// TPM 2.0 子系统 (软件模拟)
pub struct TpmSubsystem {
    /// PCR 寄存器
    pcrs: IrqSpinLock<[[u8; SHA256_LEN]; PCR_COUNT]>,
    /// PCR 扩展次数
    pcr_extend_count: IrqSpinLock<[u32; PCR_COUNT]>,
    /// 是否已初始化
    initialized: AtomicBool,
    /// 是否为硬件 TPM
    is_hardware: AtomicBool,
}

impl TpmSubsystem {
    pub const fn new() -> Self {
        Self {
            pcrs: IrqSpinLock::new([[0u8; SHA256_LEN]; PCR_COUNT]),
            pcr_extend_count: IrqSpinLock::new([0u32; PCR_COUNT]),
            initialized: AtomicBool::new(false),
            is_hardware: AtomicBool::new(false),
        }
    }

    /// 初始化 TPM 子系统
    pub fn init(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        // 初始化 PCR 为全零
        let mut pcrs = self.pcrs.lock();
        for pcr in pcrs.iter_mut() {
            *pcr = [0u8; SHA256_LEN];
        }
        drop(pcrs);
        let mut counts = self.pcr_extend_count.lock();
        for c in counts.iter_mut() {
            *c = 0;
        }
        self.initialized.store(true, Ordering::Release);
        crate::klog_ffi!(
            klog_ffi_info,
            "[TPM] subsystem initialized (software emulation)"
        );
    }

    /// 扩展 PCR (Extend)
    ///
    /// 公式: 新值 = SHA256(旧值 || 数据)
    pub fn extend(&self, pcr_idx: PcrIndex, data: &[u8]) -> bool {
        if !self.initialized.load(Ordering::Acquire) {
            return false;
        }
        let idx = pcr_idx as usize;
        let mut pcrs = self.pcrs.lock();
        let data_hash = sha256_hash(data);
        pcrs[idx] = sha256_extend(&pcrs[idx], &data_hash);
        drop(pcrs);
        let mut counts = self.pcr_extend_count.lock();
        counts[idx] += 1;
        true
    }

    /// 读取 PCR 值
    pub fn read_pcr(&self, pcr_idx: PcrIndex) -> [u8; SHA256_LEN] {
        let pcrs = self.pcrs.lock();
        pcrs[pcr_idx as usize]
    }

    /// 读取所有 PCR
    pub fn read_all_pcrs(&self) -> [[u8; SHA256_LEN]; PCR_COUNT] {
        *self.pcrs.lock()
    }

    /// 密封数据 (Seal)
    ///
    /// 将数据绑定到当前 PCR 状态, 仅当 PCR 匹配时才能解封.
    /// 简化实现: 将数据与 PCR 快照一起存储.
    pub fn seal(&self, data: &[u8], pcr_mask: u32) -> Option<TpmSealedData> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }
        let pcr_snapshot = self.read_all_pcrs();
        Some(TpmSealedData {
            data: data.to_vec(),
            pcr_snapshot,
            pcr_mask,
        })
    }

    /// 解封数据 (Unseal)
    ///
    /// 检查当前 PCR 状态是否与密封时匹配.
    pub fn unseal(&self, sealed: &TpmSealedData) -> Option<Vec<u8>> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }
        let current_pcrs = self.read_all_pcrs();
        // 检查 pcr_mask 指定的 PCR 是否匹配
        for i in 0..PCR_COUNT {
            if (sealed.pcr_mask >> i) & 1 == 1 {
                if current_pcrs[i] != sealed.pcr_snapshot[i] {
                    return None; // PCR 不匹配
                }
            }
        }
        Some(sealed.data.clone())
    }

    /// 报价 (Quote)
    ///
    /// 对 PCR 值签名, 用于远程证明.
    /// 简化实现: 返回 PCR 快照 + 哈希.
    pub fn quote(&self, pcr_mask: u32, nonce: &[u8]) -> TpmQuote {
        let all_pcrs = self.read_all_pcrs();
        let mut quote_data = Vec::new();
        for i in 0..PCR_COUNT {
            if (pcr_mask >> i) & 1 == 1 {
                quote_data.extend_from_slice(&all_pcrs[i]);
            }
        }
        quote_data.extend_from_slice(nonce);
        let quote_hash = sha256_hash(&quote_data);
        TpmQuote {
            pcr_mask,
            pcr_values: all_pcrs,
            quote_hash,
        }
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// 是否为硬件 TPM
    pub fn is_hardware(&self) -> bool {
        self.is_hardware.load(Ordering::Acquire)
    }
}

/// 密封数据
#[derive(Debug, Clone)]
pub struct TpmSealedData {
    /// 密封的数据
    pub data: Vec<u8>,
    /// 密封时的 PCR 快照
    pub pcr_snapshot: [[u8; SHA256_LEN]; PCR_COUNT],
    /// PCR 掩码 (哪些 PCR 参与绑定)
    pub pcr_mask: u32,
}

/// TPM 报价
#[derive(Debug, Clone)]
pub struct TpmQuote {
    /// PCR 掩码
    pub pcr_mask: u32,
    /// PCR 值
    pub pcr_values: [[u8; SHA256_LEN]; PCR_COUNT],
    /// 报价哈希
    pub quote_hash: [u8; SHA256_LEN],
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局安全启动子系统
static SECURE_BOOT: SecureBootSubsystem = SecureBootSubsystem::new();
/// 全局 TPM 子系统
static TPM: TpmSubsystem = TpmSubsystem::new();

/// 初始化安全启动
pub fn secure_boot_init(pk: Ed25519PubKey) {
    SECURE_BOOT.init(pk);
}

/// 获取全局安全启动子系统
pub fn secure_boot_subsystem() -> &'static SecureBootSubsystem {
    &SECURE_BOOT
}

/// 初始化 TPM
pub fn tpm_init() {
    TPM.init();
}

/// 获取全局 TPM 子系统
pub fn tpm_subsystem() -> &'static TpmSubsystem {
    &TPM
}

/// 安全启动是否已初始化
pub fn secure_boot_is_initialized() -> bool {
    SECURE_BOOT.initialized.load(Ordering::Acquire)
}

/// TPM 是否已初始化
pub fn tpm_is_initialized() -> bool {
    TPM.initialized.load(Ordering::Acquire)
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_secure_boot` — 安全启动系统调用
///
/// `a0`: cmd
///   0 = `verify_image(image_ptr`: a1, `image_len`: a2, `sig_ptr`: a3) → 结果
///   1 = `is_enabled()` → bool
///   2 = `is_locked()` → bool
///   3 = `stats()` → (`ok_count` 位于高 32 位 | `fail_count` 位于低 32 位)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn sys_secure_boot(cmd: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    match cmd {
        0 => {
            // verify_image
            if a1 == 0 || a3 == 0 {
                return -(22i64); // EINVAL
            }
            let image_ptr = a1 as *const u8;
            let image_len = a2 as usize;
            let sig_ptr = a3 as *const [u8; ED25519_SIG_LEN];
            // SAFETY: 指针由 syscall 入口保证有效
            let image = unsafe { core::slice::from_raw_parts(image_ptr, image_len) };
            // SAFETY: 同上
            let signature = unsafe { &*sig_ptr };
            let result = secure_boot_subsystem().verify_image(image, signature);
            result as i64
        }
        1 => {
            // is_enabled
            i64::from(secure_boot_subsystem().is_enabled())
        }
        2 => {
            // is_locked
            i64::from(secure_boot_subsystem().is_locked())
        }
        3 => {
            // stats
            let (ok, fail) = secure_boot_subsystem().stats();
            (i64::from(ok) << 32) | i64::from(fail)
        }
        _ => -(38i64), // ENOSYS
    }
}

/// `sys_tpm` — TPM 系统调用
///
/// `a0`: cmd
///   0 = extend(pcr 索引: a1, 数据指针: a2, 数据长度: a3) → bool
///   1 = `read_pcr(pcr` 索引: a1) → u64 (哈希前8字节)
///   2 = seal(数据指针: a1, 数据长度: a2, pcr 掩码: a3) → fd
///   3 = unseal(fd: a1) → bool (简化)
///   4 = quote(pcr 掩码: a1, nonce 指针: a2, nonce 长度: a3) → 哈希前8字节
///   5 = `is_initialized()` → 是否已初始化
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
pub extern "C" fn sys_tpm(cmd: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    if !tpm_is_initialized() && cmd != 5 {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // extend
            let pcr_idx = match PcrIndex::from_u32(a1 as u32) {
                Some(idx) => idx,
                None => return -(22i64),
            };
            if a2 == 0 {
                return -(22i64);
            }
            // SAFETY: 指针由 syscall 入口保证有效
            let data = unsafe { core::slice::from_raw_parts(a2 as *const u8, a3 as usize) };
            if tpm_subsystem().extend(pcr_idx, data) {
                0
            } else {
                -(5i64)
            }
        }
        1 => {
            // read_pcr
            let pcr_idx = match PcrIndex::from_u32(a1 as u32) {
                Some(idx) => idx,
                None => return -(22i64),
            };
            let pcr = tpm_subsystem().read_pcr(pcr_idx);
            // 返回哈希前 8 字节作为 u64
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&pcr[..8]);
            i64::from_be_bytes(bytes)
        }
        2 => {
            // seal (简化: 返回成功/失败)
            if a1 == 0 {
                return -(22i64);
            }
            // SAFETY: 指针由 syscall 入口保证有效
            let data = unsafe { core::slice::from_raw_parts(a1 as *const u8, a2 as usize) };
            match tpm_subsystem().seal(data, a3 as u32) {
                Some(_) => 0,
                None => -(5i64),
            }
        }
        3 => {
            // unseal (简化: 总是返回未实现)
            -(38i64) // ENOSYS - 需要传入密封数据, 简化跳过
        }
        4 => {
            // quote
            let nonce = if a2 == 0 {
                &[]
            } else {
                // SAFETY: 指针由 syscall 入口保证有效
                unsafe { core::slice::from_raw_parts(a2 as *const u8, a3 as usize) }
            };
            let quote = tpm_subsystem().quote(a1 as u32, nonce);
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&quote.quote_hash[..8]);
            i64::from_be_bytes(bytes)
        }
        5 => {
            // is_initialized
            i64::from(tpm_is_initialized())
        }
        _ => -(38i64), // ENOSYS
    }
}
