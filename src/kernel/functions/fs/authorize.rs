#![deny(unsafe_code)]
//! 访问判定算法单点 (T-2) — `resolve → authorize → publish` 的 authorize 段.
//!
//! 客体只回答自己的标签 (敏感级 / 属主 / 显式 ACE 判定), 判定组合在本模块完成:
//! 特权级 → MLS 敏感级 → 显式 ACE → FS 能力位 → 属主越权回落.
//! 与原 ramfs `check_permission` 逐分支等价 (搬迁, 非改判).
//!
//! 这也是 SGEG v2 的接口面: 换 v2 只换本文件的判定实现与 [`SubjectId`] 的解释;
//! 文件系统侧只负责给出 [`ObjectLabel`], 不解读主体语义.

use crate::privileged::sgeg::api as pwm_api;

/// 主体标识 (不透明) — 文件系统只透传与持久化, 解释权在判定方
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubjectId(pub u64);

/// 客体标签 — 由对象自己回答
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectLabel {
    /// 敏感级别 (0 = 公开)
    pub sensitivity: u8,
    /// 属主主体
    pub owner: SubjectId,
    /// 显式 ACE 判定: `Some(true)` 允许 / `Some(false)` 拒绝 / `None` 无匹配
    pub acl: Option<bool>,
}

/// 判定结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessDecision {
    /// 允许
    Allow,
    /// 拒绝
    Deny,
}

/// SGEG 能力域号 — FS 域 (与 ramfs 既有 `pwm_has_capability(pwm, 1, cap)` 一致)
const FS_DOMAIN: u16 = 1;
/// 无效 / 未启用主体的特权级哨兵
const LEVEL_DISABLED: u8 = 0xFF;

/// 判定主体能否以 `required` 能力访问该客体
///
/// 分支次序与 ramfs 原实现一致; 任一「拒绝」分支立即返回 `Deny`.
#[must_use]
pub fn decide(subject: SubjectId, label: ObjectLabel, required: u64) -> AccessDecision {
    let level = pwm_api::pwm_get_privilege_level(subject.0);
    if level == LEVEL_DISABLED {
        return AccessDecision::Deny;
    }
    if level > 0 && label.sensitivity > 0 {
        let clearance = match level {
            0 | 1 => 255u8,
            2 => 128u8,
            _ => 64u8,
        };
        if clearance < label.sensitivity {
            return AccessDecision::Deny;
        }
    }
    if let Some(allowed) = label.acl {
        return if allowed {
            AccessDecision::Allow
        } else {
            AccessDecision::Deny
        };
    }
    if pwm_api::pwm_get_fs_capability(subject.0) & required == required {
        return AccessDecision::Allow;
    }
    if label.owner.0 != 0
        && label.owner != subject
        && pwm_api::pwm_has_capability(subject.0, FS_DOMAIN, required)
    {
        return AccessDecision::Allow;
    }
    AccessDecision::Deny
}
