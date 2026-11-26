#![deny(unsafe_code)]
//! 系统信息策略 — getrusage / sysinfo / getrlimit / setrlimit / prlimit64 / gethostname / sethostname / setdomainname / boot_check
//!
//! 从 privileged/syscall/mod.rs 迁移的策略代码:
//! - getrusage_syscall: 资源使用统计
//! - sysinfo_syscall: 系统信息
//! - getrlimit_syscall: 资源限制查询
//! - setrlimit_syscall: 资源限制设置 (T2 批 1 自 privileged 回退层迁移)
//! - prlimit64_syscall: 跨进程资源限制查询/设置 (D7 P1 能力面收口)
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

use crate::privileged::proc::rlimit::{RLIMIT_NLIMITS, Rlimit};
use crate::privileged::syscall::Errno;

/// getrusage(who, rusage) 策略
#[must_use]
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
#[must_use]
pub fn sysinfo_syscall(info_ptr: u64) -> i64 {
    #[repr(C)]
    #[derive(Copy, Clone)]
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
    if info_ptr == 0 {
        return Errno::Einval.as_ret();
    }

    let ticks = crate::privileged::syscall::api::get_ticks();
    let si = SysInfo {
        uptime: (ticks / 1000).cast_signed(),
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
        return Errno::Efault.as_ret();
    }
    0
}

/// 当前会话是否持有修改 rlimit 的 SYSTEM 域能力.
///
/// 与 `prlimit64`/`setrlimit` 的 `is_privileged` 判据单源: 修改其他 pid 的
/// rlimit、或提高 hard limit, 都需要 `SYSTEM_CAP_SET_RLIMIT`; bootstrap
/// PWM 0 由 `pwm_has_capability` 恒真. 不再用 `pid == 1` 冒充特权.
fn current_has_rlimit_capability() -> bool {
    crate::privileged::sgeg::pwm_has_capability(
        crate::privileged::sgeg::pwm_get_current(),
        crate::privileged::sgeg::CAP_DOMAIN_SYSTEM,
        crate::privileged::sgeg::SYSTEM_CAP_SET_RLIMIT,
    )
}

/// setrlimit(resource, rlim) 策略 — 设置资源限制
///
/// T2 批 1 (syscall-followup): 自 privileged 回退层迁移. `RlimitTable` 机制
/// 字段仍归 privileged (DECISION-J 第十九批判据), functions 仅做策略:
/// 指针校验 + 读取 `rlim_cur/rlim_max` + 特权判定 + 委托表更新.
#[must_use]
pub fn setrlimit_syscall(resource: i32, rlim_ptr: u64) -> i64 {
    use crate::privileged::proc::rlimit::RLIMIT_NLIMITS;

    if rlim_ptr == 0 {
        return Errno::Einval.as_ret();
    }
    if !(0..crate::functions::intcast::low_i32_from_usize(RLIMIT_NLIMITS)).contains(&resource) {
        return Errno::Einval.as_ret();
    }

    // 从用户空间读取 rlim_cur / rlim_max (16 字节, safe 包装先校验后读)
    let mut vals = [0u64; 2];
    if !crate::privileged::syscall::api::read_struct_from_user(rlim_ptr, &mut vals) {
        return Errno::Efault.as_ret();
    }
    let (cur, max) = (vals[0], vals[1]);

    // 提高 hard limit 需要 rlimit 专属能力; 降低限制不受此门控.
    let pid = crate::privileged::proc::process_get_current_pid();
    let is_privileged = current_has_rlimit_capability();

    match crate::privileged::proc::process_with(pid, |proc| {
        let mut rlimit_table = proc.rlimit_table.lock();
        rlimit_table.set(
            crate::functions::intcast::usize_from_i32(resource),
            cur,
            max,
            is_privileged,
        )
    }) {
        Some(Ok(())) => 0,
        Some(Err(e)) => e.as_ret(),
        None => Errno::Esrch.as_ret(),
    }
}

/// 读取指定进程的 rlimit 条目.
///
/// `getrlimit` 与 `prlimit64` 的唯一表访问入口: 两入口必须读同一张
/// `Process::rlimit_table`, 不得各自返回默认值. `pid == 0` 或目标进程不在表内
/// 返回 `ESRCH`; 资源号越界返回 `EINVAL`.
fn rlimit_for_process(pid: u32, resource: usize) -> Result<Rlimit, Errno> {
    if resource >= RLIMIT_NLIMITS {
        return Err(Errno::Einval);
    }
    if pid == 0 {
        return Err(Errno::Esrch);
    }
    match crate::privileged::proc::process_with(pid, |p| p.rlimit_table.lock().get(resource)) {
        Some(Some(limit)) => Ok(limit),
        Some(None) => Err(Errno::Einval),
        None => Err(Errno::Esrch),
    }
}

/// getrlimit(resource, rlim) 策略
///
/// 读取当前进程 `rlimit_table` 的 `{cur,max}`, 与 `prlimit64(pid=0, ...)` 同源;
/// 不再恒返回 `RLIM_INFINITY`. 保持既有入口错误口径: `rlim_ptr == 0` 返回
/// `EINVAL`, 非法用户指针返回 `EFAULT`.
#[must_use]
pub fn getrlimit_syscall(resource: i32, rlim_ptr: u64) -> i64 {
    if rlim_ptr == 0 {
        return Errno::Einval.as_ret();
    }
    if !crate::privileged::syscall::api::validate_user_buf(rlim_ptr, 16) {
        return Errno::Efault.as_ret();
    }
    let Ok(index) = usize::try_from(resource) else {
        return Errno::Einval.as_ret();
    };
    let pid = crate::privileged::proc::process_get_current_pid();
    let limit = match rlimit_for_process(pid, index) {
        Ok(limit) => limit,
        Err(errno) => return errno.as_ret(),
    };
    let values = [limit.cur, limit.max];
    if !crate::privileged::syscall::api::write_struct_to_user(rlim_ptr, &values) {
        return Errno::Efault.as_ret();
    }
    0
}

/// `prlimit64(pid, resource, new_limit, old_limit)` 策略
///
/// 支持 `pid = 0`（当前进程）与显式当前 PID；修改其他进程的 rlimit 需要
/// SYSTEM 域 `SYSTEM_CAP_SET_RLIMIT`（bootstrap PWM 0 恒有），避免绕过
/// SGEG 权限面。`new_limit` 和 `old_limit` 都可为空，语义与 Linux 的
/// `prlimit64(2)` 一致：空 new 只查询，空 old 只设置。
///
/// # Errors
///
/// 资源号越界或 pid 非法返回 `EINVAL`；目标进程不存在返回 `ESRCH`；无
/// rlimit 能力改其他进程返回 `EPERM`；用户缓冲不可访问返回 `EFAULT`；设置
/// 被 rlimit 表拒绝时透传 `EPERM`/`EINVAL`。
#[must_use]
pub fn prlimit64_syscall(pid: i64, resource: i32, new_ptr: u64, old_ptr: u64) -> i64 {
    use crate::privileged::proc::rlimit::RLIMIT_NLIMITS;

    let current = crate::privileged::proc::process_get_current_pid();
    let privileged = current_has_rlimit_capability();
    let target = match pid.cmp(&0) {
        core::cmp::Ordering::Equal => current,
        core::cmp::Ordering::Greater => {
            let Ok(target) = u32::try_from(pid) else {
                return Errno::Esrch.as_ret();
            };
            if target != current && !privileged {
                return Errno::Eperm.as_ret();
            }
            target
        }
        core::cmp::Ordering::Less => return Errno::Einval.as_ret(),
    };
    if target == 0 {
        return Errno::Esrch.as_ret();
    }
    let Ok(index) = usize::try_from(resource) else {
        return Errno::Einval.as_ret();
    };
    if index >= RLIMIT_NLIMITS {
        return Errno::Einval.as_ret();
    }
    if (new_ptr != 0 && !crate::privileged::syscall::api::validate_user_buf(new_ptr, 16))
        || (old_ptr != 0 && !crate::privileged::syscall::api::validate_user_buf(old_ptr, 16))
    {
        return Errno::Efault.as_ret();
    }

    let old = match rlimit_for_process(target, index) {
        Ok(limit) => limit,
        Err(errno) => return errno.as_ret(),
    };

    if new_ptr != 0 {
        let mut values = [0u64; 2];
        if !crate::privileged::syscall::api::read_struct_from_user(new_ptr, &mut values) {
            return Errno::Efault.as_ret();
        }
        let is_privileged = privileged;
        match crate::privileged::proc::process_with_mut(target, |p| {
            p.rlimit_table
                .lock()
                .set(index, values[0], values[1], is_privileged)
        }) {
            Some(Ok(())) => {}
            Some(Err(error)) => return error.as_ret(),
            None => return Errno::Esrch.as_ret(),
        }
    }

    if old_ptr != 0
        && !crate::privileged::syscall::api::write_struct_to_user(old_ptr, &[old.cur, old.max])
    {
        return Errno::Efault.as_ret();
    }
    0
}

/// gethostname(buf, size) 策略 — 读取当前进程 UTS namespace 主机名
///
/// T1 G7: 原实现恒返回字面量 `"localhost"`, 与 `uname` (硬编码
/// `"edgine-node"`) / `sethostname` (不存储) 三处各说各话; 现全部收敛到
/// privileged `UtsNamespace` (单一权威).
#[must_use]
pub fn gethostname_syscall(buf_ptr: u64, size: u64) -> i64 {
    if buf_ptr == 0 || size == 0 {
        return Errno::Efault.as_ret();
    }
    let Some(uts) = crate::privileged::proc::namespace::uts_current() else {
        return Errno::Esrch.as_ret();
    };

    let name = uts.get_nodename();
    let bytes = name.as_bytes();
    // 出参缓冲: 主机名 (≤64B) + NUL 终止符
    let mut out = [0u8; 65];
    let len = bytes.len().min(64);
    out[..len].copy_from_slice(&bytes[..len]);
    let n = (len + 1).min(crate::functions::intcast::usize_from_u64(size));
    match crate::privileged::mm::copy_user::copy_to_user(buf_ptr, &out[..n], n) {
        Ok(_) => 0,
        Err(_) => Errno::Efault.as_ret(),
    }
}

/// sethostname(name, len) 策略
///
/// T1 G7: 原实现仅校验不存储 (主机名无处可去), 现写入当前进程 UTS namespace.
#[must_use]
pub fn sethostname_syscall(name_ptr: u64, len: u64) -> i64 {
    set_uts_name_syscall(name_ptr, len, false)
}

/// setdomainname(name, len) 策略 — 与 `sethostname` 同构 (写入 UTS 域名字段)
#[must_use]
pub fn setdomainname_syscall(name_ptr: u64, len: u64) -> i64 {
    set_uts_name_syscall(name_ptr, len, true)
}

/// `sethostname` / `setdomainname` 共用策略: 长度校验 + 特权判定 + 读用户缓冲
/// + 委托 privileged `UtsNamespace` 写入.
fn set_uts_name_syscall(name_ptr: u64, len: u64, domain: bool) -> i64 {
    if name_ptr == 0 || len == 0 || len > 63 {
        return Errno::Einval.as_ret();
    }
    // 能力位沿用既有的 SYSTEM 域 UTS 名称设置位 (原 sethostname 字面量 9)
    let pwm = crate::privileged::sgeg::pwm_get_current();
    if !crate::privileged::sgeg::pwm_has_capability(
        pwm,
        crate::privileged::sgeg::CAP_DOMAIN_SYSTEM,
        crate::privileged::sgeg::SYSTEM_CAP_UTS_SETNAME,
    ) {
        return Errno::Eacces.as_ret();
    }
    let Ok(n) = usize::try_from(len) else {
        return Errno::Einval.as_ret();
    };
    let mut buf = [0u8; 64];
    if crate::privileged::mm::copy_user::copy_from_user(&mut buf[..n], name_ptr, n).is_err() {
        return Errno::Efault.as_ret();
    }

    let Some(uts) = crate::privileged::proc::namespace::uts_current() else {
        return Errno::Esrch.as_ret();
    };
    if domain {
        uts.set_domainname(&buf[..n]);
    } else {
        uts.set_nodename(&buf[..n]);
    }
    0
}

/// `boot_check(check_type)` 策略
#[must_use]
pub fn boot_check_syscall(check_type: i32) -> i64 {
    match check_type {
        0 => i64::from(crate::privileged::sgeg::pwm_any_identity_exists()),
        _ => -1,
    }
}

/// reboot(cmd) 策略
///
/// PWM 权限检查 + 委托 privileged 执行重启机制
#[must_use]
pub fn reboot_syscall(cmd: i32) -> i64 {
    let pwm = crate::privileged::sgeg::pwm_get_current();
    if !crate::privileged::sgeg::pwm_has_capability(pwm, 0, 0x01) {
        return Errno::Eacces.as_ret();
    }
    crate::privileged::syscall::api::reboot_mechanism(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::config::MAX_OPEN_FILES;
    use crate::privileged::constants::limits::USER_ADDR_MAX;
    use crate::privileged::proc::rlimit::{RLIM_INFINITY, RLIMIT_NLIMITS, RLIMIT_NOFILE};
    use crate::privileged::proc::scheduler::Scheduler;
    use crate::privileged::proc::{PROCESS_TABLE, Process};
    use alloc::boxed::Box;

    /// 当前调度进程的恢复守卫: 断言失败时也把 `Scheduler` 恢复到测试前状态.
    struct CurrentGuard(Option<u32>);

    impl CurrentGuard {
        fn set(pid: u32) -> Self {
            let prev = Scheduler::current();
            Scheduler::set_current(pid);
            Self(prev)
        }
    }

    impl Drop for CurrentGuard {
        fn drop(&mut self) {
            Scheduler::set_current(self.0.unwrap_or(0));
        }
    }

    /// 上界本身不是合法用户指针 (`validate_user_ptr` 要求 `ptr < USER_ADDR_MAX`),
    /// 用它触发指针校验分支, 同时不会真的解引用 host 地址空间.
    const BAD_USER_PTR: u64 = USER_ADDR_MAX;

    /// 测试进程句柄: 真实 `Process::new` 插入全局表, Drop 时回收.
    struct TestProc(u32);

    impl TestProc {
        fn new(name: &str) -> Self {
            let pid = PROCESS_TABLE
                .allocate_pid()
                .expect("PROCESS_TABLE.allocate_pid 失败");
            let proc = Box::new(Process::new(pid, name, None));
            assert!(
                PROCESS_TABLE.insert(Box::into_raw(proc)),
                "测试进程插入失败"
            );
            Self(pid)
        }
    }

    impl Drop for TestProc {
        fn drop(&mut self) {
            PROCESS_TABLE.remove_and_free(self.0);
        }
    }

    #[test]
    fn test_rlimit_for_process_reads_table() {
        let proc = TestProc::new("rlimit-read-test");

        let nofile = rlimit_for_process(proc.0, RLIMIT_NOFILE).expect("NOFILE 条目必须存在");
        assert_eq!(nofile.cur, MAX_OPEN_FILES as u64, "NOFILE soft 取默认表");
        assert_eq!(nofile.max, RLIM_INFINITY, "NOFILE hard 取默认表");

        let changed = PROCESS_TABLE
            .with_process_mut(proc.0, |p| {
                p.rlimit_table
                    .lock()
                    .set(RLIMIT_NOFILE, 40, 64, true)
                    .expect("设置 rlimit 必须成功");
                p.rlimit_table.lock().get(RLIMIT_NOFILE)
            })
            .expect("测试进程应在表中")
            .expect("NOFILE 条目必须存在");
        assert_eq!((changed.cur, changed.max), (40, 64), "表更新必须可读回");

        let reread = rlimit_for_process(proc.0, RLIMIT_NOFILE).expect("NOFILE 重读必须成功");
        assert_eq!(
            (reread.cur, reread.max),
            (40, 64),
            "getrlimit 数据源是表而非常量"
        );
        assert!(
            matches!(
                rlimit_for_process(proc.0, RLIMIT_NLIMITS),
                Err(Errno::Einval)
            ),
            "resource == RLIMIT_NLIMITS 必须 EINVAL"
        );
        assert!(
            matches!(rlimit_for_process(0, RLIMIT_NOFILE), Err(Errno::Esrch)),
            "pid == 0 必须 ESRCH"
        );
    }

    #[test]
    fn test_prlimit64_cross_process_requires_rlimit_capability() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        let current = TestProc::new("rlimit-cap-current");
        let target = TestProc::new("rlimit-cap-target");
        let _guard = CurrentGuard::set(current.0);

        // 创建一个非 bootstrap 身份并登录; 新身份仅有 VIABLE_FLOOR, 不含
        // SYSTEM_CAP_SET_RLIMIT, 因此改他 pid 必须 EPERM.
        let note = "p1-rlimit-cap";
        let password = "p1-rlimit-password";
        let table = crate::privileged::sgeg::identity::get_table();
        let pwm = table.create(password, note, 0).expect("测试身份创建应成功");
        crate::privileged::sgeg::session::login(note, password).expect("测试身份登录应成功");
        assert_eq!(
            crate::privileged::sgeg::session::get_current_pwm(),
            pwm,
            "登录后当前会话应绑定测试身份"
        );
        assert_eq!(
            prlimit64_syscall(i64::from(target.0), RLIMIT_NOFILE as i32, 0, 0),
            Errno::Eperm.as_ret(),
            "无 rlimit 能力改他 pid 必须 EPERM"
        );

        // 同一 pid / pid=0 只是查询, 不要求跨进程能力.
        assert_eq!(
            prlimit64_syscall(i64::from(current.0), RLIMIT_NOFILE as i32, 0, 0),
            0,
            "查询自身 rlimit 不应要求能力"
        );
        assert_eq!(
            prlimit64_syscall(0, RLIMIT_NOFILE as i32, 0, 0),
            0,
            "pid=0 查询当前进程不应要求能力"
        );

        // 登出回到 bootstrap PWM 0 (恒有全部能力), 可查询其他 pid.
        crate::privileged::sgeg::session::logout();
        assert_eq!(
            prlimit64_syscall(i64::from(target.0), RLIMIT_NOFILE as i32, 0, 0),
            0,
            "bootstrap PWM 0 必须具备跨进程 rlimit 能力"
        );
        table.delete(pwm).expect("测试身份清理应成功");
    }

    #[test]
    fn test_setrlimit_rejects_null_and_bad_resource() {
        assert_eq!(
            setrlimit_syscall(0, 0),
            Errno::Einval.as_ret(),
            "rlim_ptr == NULL 必须 EINVAL"
        );
        assert_eq!(
            setrlimit_syscall(-1, BAD_USER_PTR),
            Errno::Einval.as_ret(),
            "负 resource 必须 EINVAL"
        );
        assert_eq!(
            setrlimit_syscall(
                crate::functions::intcast::low_i32_from_usize(RLIMIT_NLIMITS),
                BAD_USER_PTR
            ),
            Errno::Einval.as_ret(),
            "resource == RLIMIT_NLIMITS 越界必须 EINVAL"
        );
    }

    #[test]
    fn test_setrlimit_bad_user_ptr_is_efault() {
        assert_eq!(
            setrlimit_syscall(0, BAD_USER_PTR),
            Errno::Efault.as_ret(),
            "合法 resource + 非法用户指针必须 EFAULT (不是 EINVAL, 也不得解引用)"
        );
    }

    #[test]
    fn test_getrlimit_rejects_null_and_bad_ptr() {
        assert_eq!(
            getrlimit_syscall(0, 0),
            Errno::Einval.as_ret(),
            "rlim_ptr == NULL 必须 EINVAL"
        );
        assert_eq!(
            getrlimit_syscall(0, BAD_USER_PTR),
            Errno::Efault.as_ret(),
            "非法用户指针必须 EFAULT"
        );
        let mut out = [0u64; 2];
        assert_eq!(
            getrlimit_syscall(-1, out.as_mut_ptr() as u64),
            Errno::Einval.as_ret(),
            "负 resource 必须 EINVAL"
        );
        assert_eq!(
            getrlimit_syscall(
                crate::functions::intcast::low_i32_from_usize(RLIMIT_NLIMITS),
                out.as_mut_ptr() as u64
            ),
            Errno::Einval.as_ret(),
            "resource == RLIMIT_NLIMITS 必须 EINVAL"
        );
    }
}
