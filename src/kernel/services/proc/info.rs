#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 信息查询系统调用 — services 层实现 (从 framework/syscall/info.rs 下沉, §6.2)
//!
//! ## 安全边界
//!
//! - services 层: 0 unsafe; 用户态缓冲写经 framework
//!   `syscall::api::write_struct_to_user` (内部先 `check_user_buf` 校验再写入)
//! - 进程/线程 ID 查询经 framework `proc::api` 只读访问器
//! - 主机名/域名数据源为 framework `proc::namespace::uts_current` (单一权威, 与
//!   sethostname/gethostname/setdomainname 同源)
//!
//! ## 范围
//!
//! - getpid / gettid / getppid: 进程/线程 ID 查询
//! - getpgid: 进程组 ID
//! - uname: 系统信息
//!
//! gettimeofday / clock_gettime 属 services::timer::clock (B05-26 时间归位).

use alloc::string::String;

use crate::framework::proc::api;
use crate::framework::syscall::Errno;

/// `struct utsname` (POSIX): 6 个 65 字节字符串字段, 共 390 字节, 对齐 1
#[repr(C)]
#[derive(Copy, Clone)]
struct Utsname {
    sysname: [u8; 65],
    nodename: [u8; 65],
    release: [u8; 65],
    version: [u8; 65],
    machine: [u8; 65],
    domainname: [u8; 65],
}

// ============================================================================
// 进程/线程 ID
// ============================================================================

/// getpid — 返回当前进程 PID (恒成功)
pub fn getpid_syscall() -> usize {
    api::process_get_current_pid() as usize
}

/// gettid — 返回当前线程 TID (线程与进程 ID 共享, 等同 getpid)
pub fn gettid_syscall() -> usize {
    api::process_get_current_pid() as usize
}

/// getppid — 返回父进程 PID (恒成功)
pub fn getppid_syscall() -> usize {
    let pid = api::process_get_current_pid();
    api::proc_get_ppid(pid) as usize
}

/// getpgid — 返回进程组 ID
///
/// pid 范围: 0 (当前进程) 或 > 0
///
/// # Errors
///
/// 当 `pid < 0` 时返回 `EINVAL`; 底层查询失败时返回对应的 `Errno`.
pub fn getpgid_syscall(pid: i32) -> Result<usize, Errno> {
    if pid < 0 {
        return Err(Errno::EINVAL);
    }
    let ret = crate::framework::proc::proc_getpgid(pid);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

// ============================================================================
// uname
// ============================================================================

/// uname — 系统信息
///
/// `buf` 指向 struct utsname (6 × 65 字节字段)
///
/// # Errors
///
/// 当 `buf == 0` 或指向非法用户空间时返回 `EFAULT`.
pub fn uname_syscall(buf: u64) -> Result<usize, Errno> {
    let uts_ns = crate::framework::proc::namespace::uts_current();
    let nodename = uts_ns
        .as_ref()
        .map_or_else(default_nodename, |n| n.get_nodename());
    let domainname = uts_ns
        .as_ref()
        .map_or_else(String::new, |n| n.get_domainname());

    let mut uts = Utsname {
        sysname: [0; 65],
        nodename: [0; 65],
        release: [0; 65],
        version: [0; 65],
        machine: [0; 65],
        domainname: [0; 65],
    };
    copy_str(&mut uts.sysname, b"QueenX");
    copy_str(&mut uts.nodename, nodename.as_bytes());
    copy_str(&mut uts.release, b"0.1.0");
    copy_str(&mut uts.version, b"QueenX 0.1.0 (queenx)");
    #[cfg(target_arch = "x86_64")]
    copy_str(&mut uts.machine, b"x86_64");
    #[cfg(target_arch = "aarch64")]
    copy_str(&mut uts.machine, b"aarch64");
    copy_str(&mut uts.domainname, domainname.as_bytes());

    // framework 安全写入: 内部先 check_user_buf(buf, 390) 再 write_volatile
    if crate::framework::syscall::api::write_struct_to_user(buf, &uts) {
        Ok(0)
    } else {
        Err(Errno::EFAULT)
    }
}

/// 无进程上下文时 `uname` 使用的主机名 (与 `UtsNamespace::new` 初值同源)
fn default_nodename() -> String {
    String::from_utf8_lossy(crate::framework::proc::namespace::UTS_DEFAULT_NODENAME).into_owned()
}

/// 复制字符串到固定长度数组 (NUL 终止)
fn copy_str(dst: &mut [u8], src: &[u8]) {
    let len = src.len().min(dst.len() - 1);
    dst[..len].copy_from_slice(&src[..len]);
    dst[len] = 0;
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utsname_size_matches_abi() {
        // POSIX struct utsname: 6 × 65 = 390 字节, 对齐 1
        assert_eq!(core::mem::size_of::<Utsname>(), 390);
        assert_eq!(core::mem::align_of::<Utsname>(), 1);
    }

    #[test]
    fn copy_str_truncates_and_nulls() {
        // 仅写入 src 字节 + 单一 NUL, 后续字节不动
        // (调用方以零初始化数组保证剩余为 0, 见 uname_syscall)
        let mut dst = [0xAAu8; 8];
        copy_str(&mut dst, b"abc");
        assert_eq!(&dst[..4], b"abc\0");
        assert_eq!(&dst[4..], &[0xAA; 4]);

        // 源超长时截断至 len-1 并补 NUL
        let mut dst2 = [0xAAu8; 4];
        copy_str(&mut dst2, b"abcdef");
        assert_eq!(&dst2, b"abc\0");
    }

    #[test]
    fn default_nodename_matches_const() {
        assert_eq!(default_nodename(), "QueenX");
    }
}
