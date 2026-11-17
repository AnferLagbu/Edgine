#![deny(unsafe_code)]
//! 文件对象获取的唯一判定出口 (T-1) — `resolve → authorize → publish` 的中段.
//!
//! 所有能获得文件对象引用的路径 (路径 `open` / `CREAT` / `open_by_handle_at`)
//! 都必须经 [`authorize_open`]. 对应设计册
//! `docs/design/subsystem-decoupling-design.md` 的两条: 判定入口唯一; 判定内容
//! 可替换 (换 SGEG v2 时只换本文件的实现, 不动挂载与 FD 发布面).
//!
//! M1 口径: 句柄打开沿用 B06-03 的 SYSTEM 域 `CAP_SYS_ADMIN` 要求 (与
//! `mount` / `umount2` 先例一致); 路径打开的判定暂仍由各文件系统内部承担,
//! T-3 将把 ramfs 的判定上提至此并接入客体标签查询.

use crate::functions::fs::vfs_types::KernelError;
use crate::privileged::sgeg::api as pwm_api;

/// SGEG 能力域号 — SYSTEM 域 (0)
const SYSTEM_DOMAIN: u16 = 0;
/// SYSTEM 域 `CAP_SYS_ADMIN` 位 (bit0)
const CAP_SYS_ADMIN: u64 = 0x01;

/// 对象获取方式 — 决定采用哪套判据
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenSource {
    /// 按路径打开: 判定暂由文件系统内部承担 (T-3 上提)
    Path,
    /// 按句柄打开: 绕过路径权限, 属特权操作
    Handle,
}

/// 判定当前主体能否获取该文件对象
///
/// # Errors
/// 主体缺少句柄打开所需的 SYSTEM 域 `CAP_SYS_ADMIN` 时返回 `PermissionDenied`.
pub fn authorize_open(subject: u64, source: OpenSource) -> Result<(), KernelError> {
    match source {
        OpenSource::Path => Ok(()),
        OpenSource::Handle => {
            if pwm_api::pwm_has_capability(subject, SYSTEM_DOMAIN, CAP_SYS_ADMIN) {
                Ok(())
            } else {
                Err(KernelError::PermissionDenied)
            }
        }
    }
}
