#![deny(unsafe_code)]
//! 系统信息策略 — getrusage / sysinfo / getrlimit / setrlimit / gethostname / sethostname / setdomainname / boot_check
//!
//! 从 privileged/syscall/mod.rs 迁移的策略代码:
//! - getrusage_syscall: 资源使用统计
//! - sysinfo_syscall: 系统信息
//! - getrlimit_syscall: 资源限制查询
//! - setrlimit_syscall: 资源限制设置 (T2 批 1 自 privileged 回退层迁移)
//! - gethostname_syscall: 获取主机名
//! - sethostname_syscall: 设置主机名
//! - setdomainname_syscall: 设置域名
//! - boot_check_syscall: 启动检查
//!
//! ## UTS 收敛 (T1 G7)
//!
//! `gethostname` / `sethostname` / `setdomainname` 与 `uname` 的主机名/域名
//! 全部读写当前进程 privileged `UtsNamespace` (单一权威), 不再各自硬编码.
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 通过 privileged::syscall::api 安全写入用户空间
//! - 无 unsafe, 无裸指针

use crate::privileged::syscall::Errno;

/// getrusage(who, rusage) 策略
pub fn getrusage_syscall(who: i32, rusage_ptr: u64) -> i64 {
    let pid = crate::privileged::proc::process_get_current_pid();
    i64::from(crate::privileged::proc::proc_get_rusage(
        pid,
        who,
        rusage_ptr as *mut u8,
        144,
    ))
}

/// sysinfo(info) 策略
pub fn sysinfo_syscall(info_ptr: u64) -> i64 {
    if info_ptr == 0 {
        return Errno::EINVAL.as_ret();
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    struct SysInfo {
        uptime: i64,
        loads: [u64; 3],
        totalram: u64,
        freeram: u64,
        sharedram: u64,
        bufferram: u64,
        totalswap: u64,
        freeswap: u64,
        procs: u16,
        _pad: [u8; 6],
        totalhigh: u64,
        freehigh: u64,
        mem_unit: u32,
    }

    let ticks = crate::privileged::syscall::api::get_ticks();
    let si = SysInfo {
        uptime: (ticks / 1000) as i64,
        loads: [0, 0, 0],
        totalram: 128 * 1024 * 1024,
        freeram: 97 * 1024 * 1024,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 1,
        _pad: [0u8; 6],
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
    };

    if !crate::privileged::syscall::api::write_struct_to_user(info_ptr, &si) {
        return Errno::EFAULT.as_ret();
    }
    0
}

/// setrlimit(resource, rlim) 策略 — 设置资源限制
///
/// T2 批 1 (syscall-followup): 自 privileged 回退层迁移. RlimitTable 机制
/// 字段仍归 privileged (DECISION-J 第十九批判据), functions 仅做策略:
/// 指针校验 + 读取 rlim_cur/rlim_max + 特权判定 + 委托表更新.
pub fn setrlimit_syscall(resource: i32, rlim_ptr: u64) -> i64 {
    use crate::privileged::proc::rlimit::RLIMIT_NLIMITS;

    if rlim_ptr == 0 {
        return Errno::EINVAL.as_ret();
    }
    if !(0..RLIMIT_NLIMITS as i32).contains(&resource) {
        return Errno::EINVAL.as_ret();
    }

    // 从用户空间读取 rlim_cur / rlim_max (16 字节, safe 包装先校验后读)
    let mut vals = [0u64; 2];
    if !crate::privileged::syscall::api::read_struct_from_user(rlim_ptr, &mut vals) {
        return Errno::EFAULT.as_ret();
    }
    let (cur, max) = (vals[0], vals[1]);

    // 特权判定: pid=1 (init) 视为特权进程
    let pid = crate::privileged::proc::process_get_current_pid();
    let is_privileged = pid == 1;

    match crate::privileged::proc::process_with(pid, |proc| {
        let mut rlimit_table = proc.rlimit_table.lock();
        rlimit_table.set(resource as usize, cur, max, is_privileged)
    }) {
        Some(Ok(())) => 0,
        Some(Err(e)) => e.as_ret(),
        None => Errno::ESRCH.as_ret(),
    }
}

/// getrlimit(resource, rlim) 策略
pub fn getrlimit_syscall(_resource: i32, rlim_ptr: u64) -> i64 {
    if rlim_ptr == 0 {
        return Errno::EINVAL.as_ret();
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    struct Rlimit {
        rlim_cur: u64,
        rlim_max: u64,
    }

    let r = Rlimit {
        rlim_cur: u64::MAX,
        rlim_max: u64::MAX,
    };

    if !crate::privileged::syscall::api::write_struct_to_user(rlim_ptr, &r) {
        return Errno::EFAULT.as_ret();
    }
    0
}

/// gethostname(buf, size) 策略 — 读取当前进程 UTS namespace 主机名
///
/// T1 G7: 原实现恒返回字面量 `"localhost"`, 与 `uname` (硬编码
/// `"edgine-node"`) / `sethostname` (不存储) 三处各说各话; 现全部收敛到
/// privileged `UtsNamespace` (单一权威).
pub fn gethostname_syscall(buf_ptr: u64, size: u64) -> i64 {
    if buf_ptr == 0 || size == 0 {
        return Errno::EFAULT.as_ret();
    }
    let Some(uts) = crate::privileged::proc::namespace::uts_current() else {
        return Errno::ESRCH.as_ret();
    };

    let name = uts.get_nodename();
    let bytes = name.as_bytes();
    // 出参缓冲: 主机名 (≤64B) + NUL 终止符
    let mut out = [0u8; 65];
    let len = bytes.len().min(64);
    out[..len].copy_from_slice(&bytes[..len]);
    let n = (len + 1).min(size as usize);
    match crate::privileged::mm::copy_user::copy_to_user(buf_ptr, &out[..n], n) {
        Ok(_) => 0,
        Err(()) => Errno::EFAULT.as_ret(),
    }
}

/// sethostname(name, len) 策略
///
/// T1 G7: 原实现仅校验不存储 (主机名无处可去), 现写入当前进程 UTS namespace.
pub fn sethostname_syscall(name_ptr: u64, len: u64) -> i64 {
    set_uts_name_syscall(name_ptr, len, false)
}

/// setdomainname(name, len) 策略 — 与 `sethostname` 同构 (写入 UTS 域名字段)
pub fn setdomainname_syscall(name_ptr: u64, len: u64) -> i64 {
    set_uts_name_syscall(name_ptr, len, true)
}

/// `sethostname` / `setdomainname` 共用策略: 长度校验 + 特权判定 + 读用户缓冲
/// + 委托 privileged `UtsNamespace` 写入.
fn set_uts_name_syscall(name_ptr: u64, len: u64, domain: bool) -> i64 {
    if name_ptr == 0 || len == 0 || len > 63 {
        return Errno::EINVAL.as_ret();
    }
    // 能力位沿用既有的 SYSTEM 域 UTS 名称设置位 (原 sethostname 字面量 9)
    let pwm = crate::privileged::sgeg::pwm_get_current();
    if !crate::privileged::sgeg::pwm_has_capability(
        pwm,
        crate::privileged::sgeg::CAP_DOMAIN_SYSTEM,
        crate::privileged::sgeg::SYSTEM_CAP_UTS_SETNAME,
    ) {
        return Errno::EACCES.as_ret();
    }
    let Ok(n) = usize::try_from(len) else {
        return Errno::EINVAL.as_ret();
    };
    let mut buf = [0u8; 64];
    if crate::privileged::mm::copy_user::copy_from_user(&mut buf[..n], name_ptr, n).is_err() {
        return Errno::EFAULT.as_ret();
    }

    let Some(uts) = crate::privileged::proc::namespace::uts_current() else {
        return Errno::ESRCH.as_ret();
    };
    if domain {
        uts.set_domainname(&buf[..n]);
    } else {
        uts.set_nodename(&buf[..n]);
    }
    0
}

/// `boot_check(check_type)` 策略
pub fn boot_check_syscall(check_type: i32) -> i64 {
    match check_type {
        0 => i64::from(crate::privileged::sgeg::pwm_any_identity_exists()),
        _ => -1,
    }
}

/// reboot(cmd) 策略
///
/// PWM 权限检查 + 委托 privileged 执行重启机制
pub fn reboot_syscall(cmd: i32) -> i64 {
    let pwm = crate::privileged::sgeg::pwm_get_current();
    if !crate::privileged::sgeg::pwm_has_capability(pwm, 0, 0x01) {
        return Errno::EACCES.as_ret();
    }
    crate::privileged::syscall::api::reboot_mechanism(cmd)
}
