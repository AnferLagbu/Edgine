#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有机制状态访问经 privileged 安全 API.
//! Seccomp / prctl — functions 侧 syscall 策略入口
//!
//! ## 分层 (T2 批 2, syscall-followup)
//!
//! 机制留在 privileged (`privileged/proc/seccomp.rs`):
//!   - `SeccompState` 挂 Process 机制状态 (mode/filters/no_new_privs)
//!   - `seccomp_check` 被 privileged 分发前置消费 (每 syscall 过滤检查)
//!   - `SeccompFilter/SeccompRule/SeccompAction/SeccompMode` 机制类型
//!   - `MAX_FILTERS/DEFAULT_ACTION` 机制常量 (与 seccomp_check 共用权威)
//!
//! 本文件实现 syscall 策略: 参数校验 + 特权判定 + 委托机制状态变更.
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义 (T1-5 于 2026-06-16 自 privileged 迁入) 按统一判据迁回 privileged —
//! SeccompState 是 Process 机制字段。T2 批 2 再将 syscall 策略入口迁回
//! functions, 机制字段仍归 privileged。

pub use crate::privileged::proc::seccomp::{
    DEFAULT_ACTION, MAX_FILTERS, SeccompAction, SeccompFilter, SeccompMode, SeccompRule,
    SeccompState, add_rule, seccomp_check,
};

use crate::privileged::proc::process_get_current_pid;
use crate::privileged::proc::process_with;
use crate::privileged::syscall::Errno;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

/// seccomp(operation, flags, args) 策略 — 设置 seccomp 过滤模式
///
/// T2 批 2 自 privileged 回退层迁移. 机制状态变更 (mode/filters) 经
/// `process_with` + `SeccompState` 公开字段委托, functions 0 unsafe.
pub fn seccomp_syscall(operation: u32, _flags: u32, _args_ptr: u64) -> i64 {
    let pid = process_get_current_pid();

    match operation {
        // SECCOMP_SET_MODE_STRICT — 仅允许白名单 syscall
        0 => match process_with(pid, |p| {
            let mode = p.seccomp.get_mode();
            if mode != SeccompMode::Disabled {
                return Err(Errno::EINVAL);
            }
            p.seccomp
                .mode
                .store(SeccompMode::Strict as u8, Ordering::Release);
            Ok(())
        }) {
            Some(Ok(())) => 0,
            Some(Err(e)) => e.as_ret(),
            None => Errno::ESRCH.as_ret(),
        },
        // SECCOMP_SET_MODE_FILTER — 追加 filter 并进入 Filter 模式
        1 => {
            let has_priv = process_with(pid, |p| p.seccomp.is_no_new_privs()).unwrap_or(false);
            if !has_priv && pid != 1 {
                return Errno::EACCES.as_ret();
            }
            match process_with(pid, |p| {
                let mode = p.seccomp.get_mode();
                if mode == SeccompMode::Strict {
                    return Err(Errno::EINVAL);
                }
                let mut filters = p.seccomp.filters.lock();
                if filters.len() >= MAX_FILTERS {
                    return Err(Errno::ENOMEM);
                }
                filters.push(SeccompFilter::new(Vec::new(), DEFAULT_ACTION));
                p.seccomp
                    .mode
                    .store(SeccompMode::Filter as u8, Ordering::Release);
                Ok(())
            }) {
                Some(Ok(())) => 0,
                Some(Err(e)) => e.as_ret(),
                None => Errno::ESRCH.as_ret(),
            }
        }
        _ => Errno::EINVAL.as_ret(),
    }
}

// prctl option 常量 (Linux ABI)
const PR_SET_SECCOMP: i64 = 22;
const PR_GET_SECCOMP: i64 = 21;
const PR_SET_NO_NEW_PRIVS: i64 = 38;
const PR_GET_NO_NEW_PRIVS: i64 = 39;
const PR_SET_NAME: i64 = 15;
const PR_GET_NAME: i64 = 16;

/// prctl(option, arg2, arg3, arg4, arg5) 策略
///
/// T2 批 2 自 privileged 回退层迁移. 当前实装 seccomp / no_new_privs /
/// 进程名 (PR_SET_NAME/PR_GET_NAME) 六个 option, 其余返回 ENOSYS
/// (与原 privileged 行为一致).
pub fn prctl_syscall(option: i64, arg2: u64, _arg3: u64, _arg4: u64, _arg5: u64) -> i64 {
    let pid = process_get_current_pid();

    match option {
        PR_SET_SECCOMP => match arg2 {
            1 => seccomp_syscall(0, 0, 0),
            2 => seccomp_syscall(1, 0, 0),
            _ => Errno::EINVAL.as_ret(),
        },
        PR_GET_SECCOMP => {
            let mode = process_with(pid, |p| p.seccomp.get_mode()).unwrap_or(SeccompMode::Disabled);
            mode as i64
        }
        PR_SET_NO_NEW_PRIVS => {
            if arg2 != 1 {
                return Errno::EINVAL.as_ret();
            }
            process_with(pid, |p| p.seccomp.set_no_new_privs()).unwrap_or(());
            0
        }
        PR_GET_NO_NEW_PRIVS => {
            process_with(pid, |p| i64::from(p.seccomp.is_no_new_privs())).unwrap_or(0)
        }
        PR_SET_NAME => {
            // 进程名 (comm 语义): 拷贝用户字符串, 截断到 15 字符 + NUL
            let Ok(name) = crate::privileged::mm::copy_user::copy_string_from_user(arg2, 16) else {
                return Errno::EFAULT.as_ret();
            };
            let mut buf = name.into_bytes();
            buf.truncate(15);
            buf.push(0);
            let comm = alloc::string::String::from_utf8_lossy(&buf).into_owned();
            process_with(pid, |p| {
                *p.name.lock() = comm;
            })
            .unwrap_or(());
            0
        }
        PR_GET_NAME => {
            // 写回 16 字节 NUL 结尾缓冲区 (与内核 comm 一致)
            let name = process_with(pid, |p| p.name.lock().clone()).unwrap_or_default();
            let mut bytes = [0u8; 16];
            for (i, b) in name.as_bytes().iter().take(15).enumerate() {
                bytes[i] = *b;
            }
            if !crate::privileged::syscall::api::write_struct_to_user(arg2, &bytes) {
                return Errno::EFAULT.as_ret();
            }
            0
        }
        _ => Errno::ENOSYS.as_ret(),
    }
}
