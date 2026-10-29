//! SGEG 引导阶段 — 首次令牌 (First Token) 的生成、使用与一次性授权凭证。

#![deny(missing_docs)]

use super::types::{AuditAction, CapBits, CapDomain, PwmError};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// 首次令牌 (First Token): 初始化阶段向首个身份授予全部权限的一次性凭证。
pub struct FirstToken {
    /// 令牌标识 (由 TSC 派生, 用于审计追溯)。
    pub token_id: u64,
    /// 令牌是否已被使用 (true 表示凭证已消费, 不可重复授权)。
    pub granted: AtomicBool,
    /// 令牌创建时刻 (PWM 时基)。
    pub created_at: u64,
}

impl FirstToken {
    /// 创建新的未使用首次令牌, 记录标识与创建时刻。
    #[must_use]
    pub fn new(token_id: u64, created_at: u64) -> Self {
        Self {
            token_id,
            granted: AtomicBool::new(false),
            created_at,
        }
    }
}

static FIRST_TOKEN_USED: AtomicBool = AtomicBool::new(true);
static FIRST_TOKEN_ID: AtomicU64 = AtomicU64::new(0);
static FIRST_TOKEN_CREATED: AtomicU64 = AtomicU64::new(0);

/// 生成新的首次令牌: 由 TSC 派生 `token_id`, 标记为未使用并记录创建时间。
pub fn generate_first_token() {
    let token_id = {
        let tsc = crate::arch!(timestamp());
        tsc.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32
    };
    FIRST_TOKEN_ID.store(token_id, Ordering::Release);
    FIRST_TOKEN_USED.store(false, Ordering::Release);
    FIRST_TOKEN_CREATED.store(pwm_now(), Ordering::Release);
}

/// 使用首次令牌 (First Token) 向目标 PWM 授予指定域的全部权限, 仅用于系统初始化。
/// # Errors
/// 首次令牌已被使用或目标 PWM 不存在时返回 Err。
pub fn grant_from_first_token(
    target_pwm: u64,
    domain: CapDomain,
    caps: CapBits,
) -> Result<(), PwmError> {
    if FIRST_TOKEN_USED.load(Ordering::Acquire) {
        return Err(PwmError::TokenUsed);
    }

    let target = super::identity::find(target_pwm).ok_or(PwmError::NotFound)?;
    target.fetch_or_caps(domain, caps);

    FIRST_TOKEN_USED.store(true, Ordering::Release);

    super::audit::log(
        0,
        AuditAction::FirstTokenGrant,
        target_pwm,
        u64::from(domain.as_u16()),
        caps.as_u64(),
    );

    Ok(())
}

/// 返回当前 PWM 时间戳 (微秒); 计数器频率未登记时直接返回计数器原值。
#[must_use]
pub fn pwm_now() -> u64 {
    let tsc = crate::arch!(timestamp());
    // 单一时基: 频率取 `timer::calibration` 登记值, 周期→微秒 走与 tick/hrtimer
    // 同一换算入口 (旧形态经 `cpu_get_tsc_frequency` FFI 取 `CpuInfo` 里的厂商
    // 估计值, 且 `tsc / freq * 1_000_000` 的先除后乘在不足 1 秒时恒为 0).
    crate::privileged::timer::calibration::tsc_to_microseconds(tsc).unwrap_or(tsc)
}
