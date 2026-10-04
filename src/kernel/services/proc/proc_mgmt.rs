#![deny(unsafe_code)]
//! 进程管理策略 — proc_list / proc_setpri / sgeg_proc_cputime
//!
//! 从 framework/syscall/mod.rs 迁移的策略代码:
//! - proc_list_syscall: 进程列表查询
//! - proc_setpri_syscall: 设置进程优先级
//! - sgeg_proc_cputime_syscall: 查询进程 CPU 时间
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 通过 framework::proc 和 framework::syscall::api 公开 API 访问
//! - 无 unsafe, 无裸指针

use crate::framework::syscall::Errno;

/// 进程列表条目 (与 framework 定义一致)
#[repr(C)]
#[derive(Copy, Clone)]
struct ProcListEntry {
    pid: u32,
    state: u8,
    _pad: [u8; 3],
    pwm: u64,
    priority: u32,
    _pad2: u32,
    name: [u8; 48],
}

/// `proc_list(buf`, `max_entries`) 策略
pub fn proc_list_syscall(buf_ptr: u64, max_entries: u32) -> i64 {
    if buf_ptr == 0 || !crate::framework::syscall::api::validate_user_ptr(buf_ptr) {
        return Errno::EFAULT.as_ret();
    }

    let entry_size = core::mem::size_of::<ProcListEntry>() as u32;
    let mut count: i32 = 0;

    crate::framework::proc::process_for_each(|proc| {
        if (count as u32) < max_entries {
            let entry = ProcListEntry {
                pid: proc.pid.0,
                state: proc.get_state() as u8,
                _pad: [0u8; 3],
                pwm: proc.get_pwm(),
                priority: proc.get_priority() as u32,
                _pad2: 0,
                name: {
                    let mut arr = [0u8; 48];
                    let name = proc.name.lock();
                    let name_bytes = name.as_bytes();
                    let len = name_bytes.len().min(47);
                    arr[..len].copy_from_slice(&name_bytes[..len]);
                    arr[len] = 0;
                    arr
                },
            };

            let offset = count as u64 * u64::from(entry_size);
            if !crate::framework::syscall::api::write_struct_to_user(buf_ptr + offset, &entry) {
                return false;
            }
            count += 1;
        }
        true
    });

    i64::from(count)
}

/// `proc_setpri(pid`, priority) 策略
pub fn proc_setpri_syscall(pid: u32, priority: u32) -> i64 {
    i64::from(crate::framework::proc::proc_set_priority(pid, priority))
}

/// `sgeg_proc_cputime(pid)` 策略
pub fn sgeg_proc_cputime_syscall(pid: u32) -> i64 {
    let target_pid = if pid == 0 {
        crate::framework::proc::process_get_current_pid()
    } else {
        pid
    };

    if !crate::framework::proc::process_exists(target_pid) {
        return Errno::ESRCH.as_ret();
    }

    let cputime = crate::framework::proc::scheduler_current_cputime();
    cputime as i64
}
