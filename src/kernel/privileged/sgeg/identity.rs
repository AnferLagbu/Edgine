use super::audit;
use super::bootstrap;
use super::capability::VIABLE_FLOOR;
use super::csprng;
use super::grant;
use super::sha256;
use super::types::{
    AuditAction, CapBits, CapDomain, GrantRecord, MAX_PWM_ENTRIES, PWM_DIGEST_LEN, PWM_NOTE_LEN,
    PWM_SALT_LEN, PwmEntry, PwmEntrySnapshot, PwmError, PwmFlags, PwmId,
};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

/// 常数时间比较两个字节数组 (B08-22: privileged 权威实现, functions::sgeg::crypto::ct_eq 委托本函数)
///
/// **安全**: 不因数据差异而早返回, 防止计时攻击
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
pub(crate) fn hash_with_salt(password: &str, salt: &[u8; PWM_SALT_LEN]) -> [u8; 32] {
    const STRETCH_ROUNDS: usize = 32768;
    let mut input = [0u8; 256];
    let mut pos = 0usize;
    for byte in salt {
        input[pos] = *byte;
        pos += 1;
    }
    for byte in password.bytes().take(255 - pos) {
        input[pos] = byte;
        pos += 1;
    }
    let full_hash = sha256::sha256(&input[..pos]);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&full_hash[..32]);
    for round in 1..STRETCH_ROUNDS {
        let mut stretch_input = [0u8; 64];
        stretch_input[..32].copy_from_slice(&hash);
        let round_bytes = (round as u64).to_le_bytes();
        stretch_input[32..40].copy_from_slice(&round_bytes);
        stretch_input[40..40 + PWM_SALT_LEN].copy_from_slice(salt);
        let full = sha256::sha256(&stretch_input[..40 + PWM_SALT_LEN]);
        hash.copy_from_slice(&full[..32]);
    }
    hash
}

/// 全局身份表: 固定容量的 `PwmEntry` 集合, 含并发自旋锁与变更标记字段。
pub struct IdentityTable {
    pub entries: alloc::vec::Vec<PwmEntry>,
    pub count: AtomicUsize,
    pub any_identity_exists: AtomicBool,
    pub next_uid: AtomicU32,
    modified: AtomicBool,
    lock: AtomicBool,
}

impl IdentityTable {
    /// 2026-07-02: turn 28 排查 test 86 hang. `[DEFAULT_ENTRY; 256]` (100KB)
    /// 在栈上创建导致栈溢出. 改用 `Vec<PwmEntry>` 直接堆分配.
    pub fn new() -> Self {
        let mut entries = alloc::vec::Vec::with_capacity(MAX_PWM_ENTRIES);
        for _ in 0..MAX_PWM_ENTRIES {
            entries.push(PwmEntry::new());
        }
        Self {
            entries,
            count: AtomicUsize::new(0),
            any_identity_exists: AtomicBool::new(false),
            next_uid: AtomicU32::new(1000),
            modified: AtomicBool::new(false),
            lock: AtomicBool::new(false),
        }
    }

    fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }

    pub fn set_modified(&self) {
        self.modified.store(true, Ordering::Release);
    }

    pub fn is_modified(&self) -> bool {
        self.modified.load(Ordering::Acquire)
    }

    pub fn clear_modified(&self) {
        self.modified.store(false, Ordering::Release);
    }

    pub fn init(&self) {
        self.acquire();
        for entry in &self.entries {
            entry.pwm.store(0, Ordering::Release);
        }
        self.count.store(0, Ordering::Release);
        self.any_identity_exists.store(false, Ordering::Release);
        self.modified.store(false, Ordering::Release);
        self.release();
    }

    /// 当前有效 (已占用) 条目数.
    ///
    /// 供持久化序列化编排使用, 避免 functions 直读 `count` 原子字段.
    pub fn valid_count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// 遍历所有有效条目并回调其 [`PwmEntrySnapshot`].
    ///
    /// 供持久化序列化编排使用, 避免 functions 直穿 `entries` 原子字段.
    pub fn for_each_valid(&self, mut f: impl FnMut(&PwmEntrySnapshot)) {
        for entry in &self.entries {
            if entry.is_valid() {
                let snap = entry.snapshot();
                f(&snap);
            }
        }
    }

    /// 将快照载入首个空闲槽位 (供持久化反序列化编排).
    ///
    /// 成功返回 `true`; 表已满返回 `false`. 内部完成 `import` + `count`
    /// 递增 + `any_identity_exists` 置位, 避免 functions 直穿 TCB 内部字段.
    pub fn load_entry(&self, snap: &PwmEntrySnapshot) -> bool {
        for entry in &self.entries {
            if !entry.is_valid() {
                entry.import(snap);
                self.count.fetch_add(1, Ordering::Relaxed);
                self.any_identity_exists.store(true, Ordering::Release);
                return true;
            }
        }
        false
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn generate_pwm(&self, password: &str, note: &str) -> u64 {
        let mut input = [0u8; 256];
        let mut pos = 0;
        for b in password.bytes().take(128) {
            input[pos] = b;
            pos += 1;
        }
        input[pos] = b':';
        pos += 1;
        for b in note.bytes().take(127) {
            input[pos] = b;
            pos += 1;
        }
        let hash = sha256::sha256(&input[..pos]);
        let mut pwm: u64 = 0;
        for i in 0..8 {
            pwm = (pwm << 8) | u64::from(hash[i]);
        }
        if pwm == 0 {
            pwm = 1;
        }
        pwm
    }

    pub fn find(&self, pwm: u64) -> Option<&PwmEntry> {
        if pwm == 0 {
            return None;
        }
        for entry in &self.entries {
            if entry.pwm.load(Ordering::Acquire) == pwm {
                return Some(entry);
            }
        }
        None
    }

    pub fn find_mut(&mut self, pwm: u64) -> Option<&mut PwmEntry> {
        if pwm == 0 {
            return None;
        }
        for entry in &mut self.entries {
            if entry.pwm.load(Ordering::Acquire) == pwm {
                return Some(entry);
            }
        }
        None
    }

    pub fn find_by_note(&self, note: &str) -> Option<&PwmEntry> {
        for entry in &self.entries {
            if !entry.is_valid() {
                continue;
            }
            if entry.note_equals(note) {
                return Some(entry);
            }
        }
        None
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn verify_password(&self, pwm: u64, password: &str) -> bool {
        let entry = match self.find(pwm) {
            Some(e) => e,
            None => return false,
        };
        // T4-1: 原子读取 password_hash 中的 salt 部分 (PWM_DIGEST_LEN..PWM_HASH_LEN)
        let mut salt = [0u8; PWM_SALT_LEN];
        for i in 0..PWM_SALT_LEN {
            salt[i] = entry.password_hash[PWM_DIGEST_LEN + i].load(Ordering::Acquire);
        }
        let computed = hash_with_salt(password, &salt);
        // T4-1: 原子读取 stored digest 与 computed 比较
        let mut stored_digest = [0u8; PWM_DIGEST_LEN];
        for i in 0..PWM_DIGEST_LEN {
            stored_digest[i] = entry.password_hash[i].load(Ordering::Acquire);
        }
        constant_time_eq(&computed, &stored_digest)
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// T4-1: 全 Atomic 化后, create 改用 &self (替代 &mut self)
    /// 注: 调用方必须保证并发安全 (create 内已有 self.acquire/release 锁)
    /// # Errors
    /// 创建者权限级别溢出、PWM 已存在或身份表已满时返回 Err。
    pub fn create(&self, password: &str, note: &str, creator_pwm: u64) -> Result<u64, PwmError> {
        let privilege_level = if creator_pwm == 0 {
            0u8
        } else {
            let creator = self.find(creator_pwm).ok_or(PwmError::NotFound)?;
            let creator_level = creator.privilege_level.load(Ordering::Acquire);
            if creator_level >= 254 {
                return Err(PwmError::PrivilegeOverflow);
            }
            creator_level + 1
        };

        let pwm = self.generate_pwm(password, note);

        self.acquire();
        for i in 0..MAX_PWM_ENTRIES {
            if self.entries[i].pwm.load(Ordering::Acquire) == pwm {
                self.release();
                return Err(PwmError::AlreadyExists);
            }
        }

        let slot = {
            let mut s = None;
            for i in 0..MAX_PWM_ENTRIES {
                if !self.entries[i].is_valid() {
                    s = Some(i);
                    break;
                }
            }
            s
        };

        let slot = if let Some(s) = slot {
            s
        } else {
            self.release();
            return Err(PwmError::TableFull);
        };

        // T4-1: 通过 &self.entries[slot] + 原子写入
        let entry = &self.entries[slot];
        entry.pwm.store(pwm, Ordering::Release);
        entry.creator_pwm.store(creator_pwm, Ordering::Release);
        entry
            .privilege_level
            .store(privilege_level, Ordering::Release);
        entry.flags.store(0, Ordering::Release);

        let salt = csprng::generate_salt();
        let digest = hash_with_salt(password, &salt);
        // T4-1: 原子写入 password_hash (digest 32 字节 + salt 16 字节)
        for i in 0..PWM_DIGEST_LEN {
            entry.password_hash[i].store(digest[i], Ordering::Release);
        }
        for i in 0..PWM_SALT_LEN {
            entry.password_hash[PWM_DIGEST_LEN + i].store(salt[i], Ordering::Release);
        }

        {
            // T4-1: 原子写入 note
            let note_bytes = note.as_bytes();
            let len = note_bytes.len().min(PWM_NOTE_LEN - 1);
            for i in 0..len {
                entry.note[i].store(note_bytes[i], Ordering::Release);
            }
            entry.note[len].store(0, Ordering::Release);
        }

        for i in 0..16 {
            entry.caps[i].store(VIABLE_FLOOR[i], Ordering::Release);
        }

        let uid = if creator_pwm == 0 {
            0
        } else {
            self.next_uid.fetch_add(1, Ordering::AcqRel)
        };
        entry.set_uid(uid);
        entry.set_gid(uid);

        entry
            .created_time
            .store(bootstrap::pwm_now(), Ordering::Release);
        entry.expires_at.store(0, Ordering::Release);
        entry.lockout_until.store(0, Ordering::Release);
        entry.failed_attempts.store(0, Ordering::Release);
        entry.last_login_time.store(0, Ordering::Release);

        self.count.fetch_add(1, Ordering::AcqRel);
        self.any_identity_exists.store(true, Ordering::Release);
        self.set_modified();
        self.release();

        audit::log(
            creator_pwm,
            AuditAction::Create,
            pwm,
            0,
            u64::from(privilege_level),
        );

        Ok(pwm)
    }

    /// 将指定域上的部分能力从授权方授予被授权方。
    /// # Errors
    /// 授权方或被授权方不存在、授权方能力不足、授权方权限级别不足、被授权方被禁用或授权记录表已满时返回 Err。
    pub fn grant(
        &self,
        grantor_pwm: u64,
        grantee_pwm: u64,
        domain: impl Into<CapDomain>,
        caps: impl Into<CapBits>,
    ) -> Result<(), PwmError> {
        let domain = domain.into();
        let caps = caps.into();

        let grantor = self.find(grantor_pwm).ok_or(PwmError::NotFound)?;
        let grantee = self.find(grantee_pwm).ok_or(PwmError::NotFound)?;

        let grantor_caps = grantor.load_caps(domain);
        if (grantor_caps & caps) != caps {
            return Err(PwmError::PermissionDenied);
        }

        let grantor_level = grantor.privilege_level.load(Ordering::Acquire);
        let grantee_level = grantee.privilege_level.load(Ordering::Acquire);
        if grantor_level > grantee_level {
            return Err(PwmError::InsufficientPrivilege);
        }

        if grantee.has_flag(PwmFlags::DISABLED) {
            return Err(PwmError::Disabled);
        }

        grantee.fetch_or_caps(domain, caps);

        grant::add_record(GrantRecord {
            grantor_pwm: PwmId(grantor_pwm),
            grantee_pwm: PwmId(grantee_pwm),
            domain,
            caps,
            granted_at: bootstrap::pwm_now(),
        })?;

        audit::log(
            grantor_pwm,
            AuditAction::Grant,
            grantee_pwm,
            u64::from(domain.as_u16()),
            caps.as_u64(),
        );

        Ok(())
    }

    /// 从目标 PWM 撤销指定域上的部分能力。
    /// # Errors
    /// 撤销方或目标不存在、撤销方权限级别不足、撤销方既非创建者也非授权者、撤销会破坏能力下限或目标被禁用时返回 Err。
    pub fn revoke(
        &self,
        revoker_pwm: u64,
        target_pwm: u64,
        domain: impl Into<CapDomain>,
        caps: impl Into<CapBits>,
    ) -> Result<(), PwmError> {
        let domain = domain.into();
        let caps = caps.into();

        let revoker = self.find(revoker_pwm).ok_or(PwmError::NotFound)?;
        let target = self.find(target_pwm).ok_or(PwmError::NotFound)?;

        let revoker_level = revoker.privilege_level.load(Ordering::Acquire);
        let target_level = target.privilege_level.load(Ordering::Acquire);
        if revoker_level >= target_level {
            return Err(PwmError::InsufficientPrivilege);
        }

        let creator_pwm = target.creator_pwm.load(Ordering::Acquire);
        let is_creator = revoker_pwm == creator_pwm;

        let is_grantor = grant::is_grantor(revoker_pwm, target_pwm, domain, caps);

        if !is_creator && !is_grantor {
            return Err(PwmError::NotAuthorized);
        }

        let current = target.load_caps(domain);
        let after_revoke = current & !caps;
        if (after_revoke & CapBits(VIABLE_FLOOR[domain.as_usize()]))
            != CapBits(VIABLE_FLOOR[domain.as_usize()])
        {
            return Err(PwmError::WouldBreakFloor);
        }

        target.fetch_and_caps(domain, !caps);

        grant::clear_records(revoker_pwm, target_pwm, domain, caps);

        self.set_modified();

        audit::log(
            revoker_pwm,
            AuditAction::Revoke,
            target_pwm,
            u64::from(domain.as_u16()),
            caps.as_u64(),
        );

        Ok(())
    }

    /// 将目标 PWM 的创建者身份转移给新的创建者。
    /// # Errors
    /// 当前创建者、目标或新创建者不存在、当前创建者并非目标的创建者或权限级别不足时返回 Err。
    pub fn transfer_creator(
        &self,
        current_creator_pwm: u64,
        target_pwm: u64,
        new_creator_pwm: u64,
    ) -> Result<(), PwmError> {
        let current_creator = self.find(current_creator_pwm).ok_or(PwmError::NotFound)?;
        let target = self.find(target_pwm).ok_or(PwmError::NotFound)?;
        let new_creator = self.find(new_creator_pwm).ok_or(PwmError::NotFound)?;

        let creator = target.creator_pwm.load(Ordering::Acquire);
        if creator != current_creator_pwm {
            return Err(PwmError::NotCreator);
        }

        let current_level = current_creator.privilege_level.load(Ordering::Acquire);
        let target_level = target.privilege_level.load(Ordering::Acquire);
        if current_level >= target_level {
            return Err(PwmError::InsufficientPrivilege);
        }

        let new_level = new_creator.privilege_level.load(Ordering::Acquire);
        if new_level >= target_level {
            return Err(PwmError::InsufficientPrivilege);
        }

        target.creator_pwm.store(new_creator_pwm, Ordering::Release);

        self.set_modified();

        audit::log(
            current_creator_pwm,
            AuditAction::TransferCreator,
            target_pwm,
            0,
            new_creator_pwm,
        );

        Ok(())
    }

    /// T4-1: 全 Atomic 化后, bootstrap 改用 &self (create 已为 &self)
    /// # Errors
    /// 创建身份失败或使用首次令牌授权失败时返回 Err。
    pub fn bootstrap(&self, password: &str, note: &str) -> Result<u64, PwmError> {
        bootstrap::generate_first_token();

        let pwm = self.create(password, note, 0)?;

        for i in 0..16u16 {
            bootstrap::grant_from_first_token(pwm, CapDomain(i), CapBits::ALL)?;
        }

        Ok(pwm)
    }

    /// 使用首次令牌恢复一个已有身份的全部权限。
    /// # Errors
    /// 指定 note 对应的 PWM 不存在或密码校验失败时返回 Err。
    pub fn recover_with_first(&self, password: &str, note: &str) -> Result<u64, PwmError> {
        let pwm_entry = self.find_by_note(note).ok_or(PwmError::NotFound)?;
        let pwm = pwm_entry.pwm.load(Ordering::Acquire);

        if !self.verify_password(pwm, password) {
            return Err(PwmError::InvalidPassword);
        }

        bootstrap::generate_first_token();
        for i in 0..16 {
            bootstrap::grant_from_first_token(pwm, CapDomain(i), CapBits::ALL)?;
        }

        Ok(pwm)
    }

    /// 删除指定 PWM 对应的身份条目。
    /// # Errors
    /// 指定 PWM 不存在时返回 Err。
    pub fn delete(&self, pwm: u64) -> Result<(), PwmError> {
        self.acquire();
        for entry in &self.entries {
            if entry.pwm.load(Ordering::Acquire) == pwm {
                entry.pwm.store(0, Ordering::Release);
                self.count.fetch_sub(1, Ordering::AcqRel);
                self.set_modified();
                self.release();
                return Ok(());
            }
        }
        self.release();
        Err(PwmError::NotFound)
    }

    /// 禁用指定 PWM, 禁止其继续使用能力。
    /// # Errors
    /// 指定 PWM 不存在时返回 Err。
    pub fn disable(&self, pwm: u64) -> Result<(), PwmError> {
        let entry = self.find(pwm).ok_or(PwmError::NotFound)?;
        entry.add_flags(PwmFlags::DISABLED);
        self.set_modified();
        Ok(())
    }

    /// 重新启用指定 PWM, 恢复其能力使用权限。
    /// # Errors
    /// 指定 PWM 不存在时返回 Err。
    pub fn enable(&self, pwm: u64) -> Result<(), PwmError> {
        let entry = self.find(pwm).ok_or(PwmError::NotFound)?;
        entry.remove_flags(PwmFlags::DISABLED);
        self.set_modified();
        Ok(())
    }

    /// T4-1: 全 Atomic 化后, `change_password` 改用 &self + 原子字节写入
    /// (替代原 &mut self + `copy_from_slice`, 后者要求 `PwmEntry` 字段非 Atomic)
    /// # Errors
    /// 旧密码校验失败或指定 PWM 不存在时返回 Err。
    pub fn change_password(&self, pwm: u64, old: &str, new: &str) -> Result<(), PwmError> {
        if !self.verify_password(pwm, old) {
            return Err(PwmError::PasswordIncorrect);
        }
        let entry = self.find(pwm).ok_or(PwmError::NotFound)?;
        let salt = csprng::generate_salt();
        let digest = hash_with_salt(new, &salt);
        // T4-1: 原子写入 digest (32 字节) + salt (16 字节)
        for i in 0..PWM_DIGEST_LEN {
            entry.password_hash[i].store(digest[i], Ordering::Release);
        }
        for i in 0..PWM_SALT_LEN {
            entry.password_hash[PWM_DIGEST_LEN + i].store(salt[i], Ordering::Release);
        }
        self.set_modified();
        Ok(())
    }

    pub fn any_identity_exists(&self) -> bool {
        self.any_identity_exists.load(Ordering::Acquire)
    }

    /// uid → `PwmEntry` (POSIX chown/kill 等 syscall 用)
    pub fn find_by_uid(&self, uid: u32) -> Option<&PwmEntry> {
        if uid == 0xFFFF_FFFF {
            return None;
        }
        self.entries
            .iter()
            .find(|e| e.is_valid() && e.get_uid() == uid)
    }

    pub fn find_by_gid(&self, gid: u32) -> Option<&PwmEntry> {
        if gid == 0xFFFF_FFFF {
            return None;
        }
        self.entries
            .iter()
            .find(|e| e.is_valid() && e.get_gid() == gid)
    }

    /// pwm → uid (stat 填充 `st_uid` 用)
    pub fn uid_of(&self, pwm: u64) -> u32 {
        self.find(pwm).map_or(0xFFFF_FFFF, PwmEntry::get_uid)
    }

    /// pwm → gid (stat 填充 `st_gid` 用)
    pub fn gid_of(&self, pwm: u64) -> u32 {
        self.find(pwm).map_or(0xFFFF_FFFF, PwmEntry::get_gid)
    }
}

// ============================================================================
// T4-1 全 Atomic 重构完成 — static mut GLOBAL_TABLE 替换为 OnceLock
// ============================================================================
//
// 2026-07-02: turn 28 排查 test 86 hang. 根因: IdentityTable::new() 创建
// [DEFAULT_ENTRY; 256] (100KB) 在栈上, 超过 KERNEL_STACK_SIZE (64KB),
// 导致栈溢出. 修复: IdentityTable.entries 改用 Vec<PwmEntry> 直接
// 堆分配, IdentityTable 自身仅 ~40 字节, OnceLock 静态可安全容纳.
static GLOBAL_TABLE: crate::privileged::sync::OnceLock<IdentityTable> =
    crate::privileged::sync::OnceLock::new();

/// 获取全局身份表 (T4-1: `OnceLock` 包装, 自动初始化, 0 unsafe)
pub fn get_table() -> &'static IdentityTable {
    GLOBAL_TABLE.get_or_init(|slot| {
        slot.write(IdentityTable::new());
    })
}

///
/// # Safety
///
/// **T4-1 已废弃**: 全 Atomic 化后, 所有变更操作改用 &self + 原子写入,
// T4-1: get_table_mut 已彻底删除.
// 原因: 全 Atomic 化后, 所有变更操作改用 &self + 原子写入, 无需 &mut 全局引用.
// 此前 &mut self 的方法 (create, change_password, bootstrap) 已改为 &self.
// 唯一外部兼容函数 storage::table_mut() 返回 &IdentityTable (非 &mut), 走 get_table().

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 GLOBAL_TABLE 访问
// ============================================================================

// ============================================================================
// T4-1: raw 子模块已废弃
// ============================================================================
//
// 此前使用 `static mut GLOBAL_TABLE` + `addr_of!/addr_of_mut!` 模式,
// 通过 `raw` 子模块集中 unsafe 访问. T4-1 全 Atomic 化后, GLOBAL_TABLE
// 已改为 `OnceLock<IdentityTable>`, 所有变更走 &self + 原子写入,
// 无需 `addr_of!` / `addr_of_mut!`. raw 子模块移除.
//
// 历史保留: 此前 raw 子模块实现见 git log, 此处仅作迁移说明.

pub fn find(pwm: u64) -> Option<&'static PwmEntry> {
    get_table().find(pwm)
}
