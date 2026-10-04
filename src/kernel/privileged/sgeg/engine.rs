use super::identity;
use super::types::{CapBits, CapDomain, PwmFlags};
use core::sync::atomic::Ordering;

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn check(pwm: u64, domain: CapDomain, required: CapBits) -> bool {
    // PWM 0 是 bootstrap 身份, 拥有全部能力. 内核首个用户进程 (init) 在
    // identity 表初始化前以 PWM 0 运行, 需要绕过能力检查以完成文件系统挂载等
    // 初始化任务. 该身份仅在 bootstrap 阶段存在, 正常登录后由 sgeg_create_first
    // 创建 root 身份 (PWM ≥ 1) 并切换.
    if pwm == 0 {
        return true;
    }

    let entry = match identity::find(pwm) {
        Some(e) => e,
        None => return false,
    };

    if entry.has_flag(PwmFlags::DISABLED) {
        return false;
    }

    entry.has_capability(domain, required)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn check_privilege(operator_pwm: u64, target_pwm: u64) -> bool {
    if operator_pwm == 0 {
        return true;
    }

    let operator = match identity::find(operator_pwm) {
        Some(e) => e,
        None => return false,
    };
    let target = match identity::find(target_pwm) {
        Some(e) => e,
        None => return false,
    };

    let op_level = operator.privilege_level.load(Ordering::Acquire);
    let tgt_level = target.privilege_level.load(Ordering::Acquire);

    op_level < tgt_level
}

pub fn get_privilege_level(pwm: u64) -> u8 {
    if pwm == 0 {
        return 0; // bootstrap 身份: 最高特权级
    }
    identity::find(pwm).map_or(0xFF, |e| e.privilege_level.load(Ordering::Acquire))
}

pub fn get_creator(pwm: u64) -> u64 {
    identity::find(pwm).map_or(0, |e| e.creator_pwm.load(Ordering::Acquire))
}

pub fn get_caps(pwm: u64, domain: impl Into<CapDomain>) -> CapBits {
    if pwm == 0 {
        return CapBits::ALL; // bootstrap 身份: 全部能力
    }
    let domain = domain.into();
    identity::find(pwm).map_or(CapBits::NONE, |e| e.load_caps(domain))
}
