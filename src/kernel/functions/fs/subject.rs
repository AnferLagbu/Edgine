#![deny(unsafe_code)]
//! 主体解析单点 (L2) — fs 层访问 SGEG 主体的唯一出口.
//!
//! 文件系统只需"知道当前主体的不透明标识"与"把标识映射成 POSIX uid/gid";
//! 它不需要认识 SGEG 的引擎、能力表或会话实现. 换 SGEG v2 时只改本文件
//! (与 [`super::authorize`]) , fs 其余文件零改动.
//!
//! 审计口径: `src/kernel/functions/fs` 子树内允许出现 `privileged::sgeg`
//! 引用的文件只有本文件与 `authorize.rs` (见 host-tests 的门禁锁).

use super::vfs_types::SubjectId;
use crate::privileged::sgeg::{identity, session};

/// 当前主体标识
#[must_use]
pub fn current() -> SubjectId {
    SubjectId::new(session::get_current_pwm())
}

/// 当前主体的原始值 — 仅用于 syscall / ABI 边界 (那里的凭证本就是整数)
#[must_use]
pub fn current_raw() -> u64 {
    session::get_current_pwm()
}

/// 当前主体登记的 uid (未登记为 `0xFFFF_FFFF`)
#[must_use]
pub fn current_uid() -> u32 {
    session::get_current_uid()
}

/// 当前主体有效 uid (未登记为 `0xFFFF_FFFF`)
#[must_use]
pub fn current_euid() -> u32 {
    session::get_euid()
}

/// 以口令登录, 成功返回新主体
#[must_use]
pub fn login(note: &str, password: &str) -> Option<SubjectId> {
    session::login(note, password).ok().map(SubjectId::new)
}

/// 主体对应的 uid (未登记为 `0xFFFF_FFFF`)
#[must_use]
pub fn uid_of(subject: SubjectId) -> u32 {
    identity::get_table().uid_of(subject.raw())
}

/// 主体对应的 gid (未登记为 `0xFFFF_FFFF`)
#[must_use]
pub fn gid_of(subject: SubjectId) -> u32 {
    identity::get_table().gid_of(subject.raw())
}

/// 由 uid 反查主体 (未登记返回 None)
#[must_use]
pub fn subject_of_uid(uid: u32) -> Option<SubjectId> {
    identity::get_table()
        .find_by_uid(uid)
        .map(|entry| SubjectId::new(entry.get_pwm().0))
}

/// 由 gid 反查主体 (未登记返回 None)
#[must_use]
pub fn subject_of_gid(gid: u32) -> Option<SubjectId> {
    // 与原 `file_ops` 语义一致: 身份表按 uid 键查询, gid 走同一查表路径
    identity::get_table()
        .find_by_uid(gid)
        .map(|entry| SubjectId::new(entry.get_pwm().0))
}
