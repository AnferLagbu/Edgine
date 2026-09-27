#![deny(unsafe_code)]
//! process_vm_readv / process_vm_writev — 跨进程用户内存向量读写
//!
//! 在**当前进程** (local) 与**目标进程** (remote, 由 `pid` 指定) 的用户地址空间
//! 之间搬运数据. 两侧 iovec 数组按顺序配对: 本地第 k 段与远端第 k 段逐字节对接,
//! 前一段耗尽后自动切换到下一段, 总搬运量为两侧总长度的较小值.
//!
//! ## 机制归属
//!
//! 目标进程内存不能经当前 CR3 访问, 故远端一侧走 framework 的跨进程安全代理
//! `mm::copy_from_user_in_mm` / `copy_to_user_in_mm` (安全不变式 I4 落地);
//! 本地一侧走同进程原语 `mm::copy_from_user` / `copy_to_user`. 本模块 0 unsafe.
//!
//! ## 校验次序 (本项目约定)
//!
//! 1. `flags != 0` → `EINVAL`;
//! 2. 任一侧 iovec 数超 `IOV_MAX` → `EINVAL`;
//! 3. 任一侧 iovec 数为 0 → `Ok(0)` (空搬运, 不解析指针也不查 pid);
//! 4. `pid <= 0` 或目标进程不存在 → `ESRCH`;
//! 5. 权限不足 → `EPERM`;
//! 6. iovec 数组或数据段地址非法 → `EFAULT` (已搬运字节数 > 0 时返回部分成功).
//!
//! SIMPLIFIED: 不实现 `/proc/pid/mem` 式的 `PTRACE_MODE_ATTACH_FSCREDS` 完整判据,
//! 权限仅取「自身 / pwm 特权 / uid 相等」三档; 影响面为「细粒度 LSM 策略下的远程
//! 内存访问放行范围可能与 Linux 存在差异」; 若后续引入 LSM/能力模型, 应在
//! [`check_access`] 处接入.

use crate::framework::credo::{
    pwm_check_privilege, pwm_get_current, pwm_get_current_uid, pwm_get_uid,
};
use crate::framework::errno::Errno;
use crate::framework::mm::{
    copy_from_user, copy_from_user_in_mm, copy_to_user, copy_to_user_in_mm, is_user_buf,
};
use crate::framework::proc::{
    process_dec_ref, process_get_cr3, process_get_current_pid, process_get_pwm, process_try_inc_ref,
};
use crate::services::fs::io::{IOV_MAX, read_iovecs};

/// 单次跨进程拷贝的分块上限 (内核临时缓冲区大小)
const CHUNK_SIZE: usize = 64 * 1024;

/// 一个 iovec 条目在用户内存中的字节数 (`{iov_base: u64, iov_len: u64}`)
const IOV_ENTRY_SIZE: u64 = 16;

/// 数据搬运方向
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// `process_vm_readv`: 远端进程 → 当前进程
    Read,
    /// `process_vm_writev`: 当前进程 → 远端进程
    Write,
}

/// 目标进程引用 RAII 守卫.
///
/// 持有期间保证目标进程不被释放 (引用计数 > 0), 析构时递减计数.
/// 避免搬运过程中目标进程退出导致 `cr3` / 页表失效.
struct ProcRef(u32);

impl ProcRef {
    /// 为目标 pid 取得引用; 进程不存在或计数已归零时返回 `None`.
    fn acquire(pid: u32) -> Option<Self> {
        if process_try_inc_ref(pid) {
            Some(Self(pid))
        } else {
            None
        }
    }
}

impl Drop for ProcRef {
    fn drop(&mut self) {
        process_dec_ref(self.0);
    }
}

/// 判定当前进程是否有权访问目标进程的用户内存.
///
/// 判据 (按序短路):
/// 1. 目标即当前进程 → 允许;
/// 2. 无有效会话 (`pwm == 0`) → 拒绝;
/// 3. `pwm_check_privilege(current, target)` → 允许 (特权/同主);
/// 4. 两者 uid 相等 → 允许 (`pwm_get_uid` 哨兵 `u32::MAX` 表示查无此 pwm).
fn check_access(target_pid: u32, target_pwm: u64) -> bool {
    if target_pid == process_get_current_pid() {
        return true;
    }
    let current_pwm = pwm_get_current();
    if current_pwm == 0 {
        return false;
    }
    if pwm_check_privilege(current_pwm, target_pwm) {
        return true;
    }
    let owner_uid = pwm_get_uid(target_pwm);
    owner_uid != u32::MAX && owner_uid == pwm_get_current_uid()
}

/// 从目标进程读入 iovec 数组 (每项 16 字节, 按字节重组为两个 u64).
///
/// # Errors
///
/// - `iov_ptr == 0` 或数组任一项不可读 → `EFAULT`
fn read_remote_iovecs(
    target_cr3: u64,
    iov_ptr: u64,
    iovcnt: u64,
) -> Result<alloc::vec::Vec<(u64, u64)>, Errno> {
    if iov_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    let mut iovs = alloc::vec::Vec::with_capacity(iovcnt as usize);
    for i in 0..iovcnt {
        let mut raw = [0u8; 16];
        copy_from_user_in_mm(target_cr3, iov_ptr + i * IOV_ENTRY_SIZE, &mut raw, 16)
            .map_err(|()| Errno::EFAULT)?;
        iovs.push((read_u64(&raw[0..8]), read_u64(&raw[8..16])));
    }
    Ok(iovs)
}

/// 从小端字节切片 (长度 >= 8) 重组出 u64
fn read_u64(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(buf)
}

/// 搬运单对 (local, remote) iovec 交叠区间的 `n` 字节.
///
/// 分块经内核临时缓冲区中转: 读方向 `remote → buf → local`;
/// 写方向 `local → buf → remote`.
///
/// # Errors
///
/// 任一侧地址非法 / 页未映射 / 目标页只读 → `Err(())`.
fn transfer(dir: Direction, target_cr3: u64, l_addr: u64, r_addr: u64, n: usize) -> Result<(), ()> {
    let mut buf = alloc::vec![0u8; core::cmp::min(n, CHUNK_SIZE)];
    let mut done: usize = 0;
    while done < n {
        let chunk = core::cmp::min(n - done, CHUNK_SIZE);
        let off = done as u64;
        let slot = &mut buf[..chunk];
        match dir {
            Direction::Read => {
                copy_from_user_in_mm(target_cr3, r_addr + off, slot, chunk)?;
                copy_to_user(l_addr + off, slot, chunk)?;
            }
            Direction::Write => {
                copy_from_user(slot, l_addr + off, chunk)?;
                copy_to_user_in_mm(target_cr3, r_addr + off, slot, chunk)?;
            }
        }
        done += chunk;
    }
    Ok(())
}

/// 出错收尾: 已有搬运量时返回部分成功, 否则返回错误
fn partial_or(total: usize, err: Errno) -> Result<usize, Errno> {
    if total > 0 { Ok(total) } else { Err(err) }
}

/// 跨进程向量搬运核心 (readv / writev 共用).
///
/// `dir` 决定搬运方向; `local_iov` / `remote_iov` 分别为当前进程与目标进程的
/// iovec 数组用户指针.
///
/// # Errors
///
/// - `flags != 0` 或 iovec 数超 `IOV_MAX` → `EINVAL`
/// - `pid <= 0`、目标进程不存在或 cr3 不可用 → `ESRCH`
/// - 权限不足 → `EPERM`
/// - iovec 数组 / 数据段地址非法 → `EFAULT` (已有搬运量则返回部分成功)
fn process_vm_rw(
    pid: i32,
    local_iov: u64,
    liovcnt: u64,
    remote_iov: u64,
    riovcnt: u64,
    flags: u64,
    dir: Direction,
) -> Result<usize, Errno> {
    if flags != 0 {
        return Err(Errno::EINVAL);
    }
    if liovcnt > IOV_MAX || riovcnt > IOV_MAX {
        return Err(Errno::EINVAL);
    }
    if liovcnt == 0 || riovcnt == 0 {
        return Ok(0);
    }
    if pid <= 0 {
        return Err(Errno::ESRCH);
    }
    let target_pid = pid as u32;
    let _target = ProcRef::acquire(target_pid).ok_or(Errno::ESRCH)?;
    let target_pwm = process_get_pwm(target_pid).ok_or(Errno::ESRCH)?;
    if !check_access(target_pid, target_pwm) {
        return Err(Errno::EPERM);
    }
    let target_cr3 = process_get_cr3(target_pid).ok_or(Errno::ESRCH)?;

    let local_iovs = read_iovecs(local_iov, liovcnt)?;
    let remote_iovs = read_remote_iovecs(target_cr3, remote_iov, riovcnt)?;

    let mut total: usize = 0;
    let mut li: usize = 0;
    let mut l_off: usize = 0;
    let mut ri: usize = 0;
    let mut r_off: usize = 0;
    while li < local_iovs.len() && ri < remote_iovs.len() {
        let (l_base, l_len) = local_iovs[li];
        let (r_base, r_len) = remote_iovs[ri];
        let l_rem = l_len as usize - l_off;
        let r_rem = r_len as usize - r_off;
        let n = core::cmp::min(l_rem, r_rem);
        if n == 0 {
            // 空段: 跳过 (至少一侧前进, 循环必然收敛)
            if l_rem == 0 {
                li += 1;
                l_off = 0;
            }
            if r_rem == 0 {
                ri += 1;
                r_off = 0;
            }
            continue;
        }
        let l_addr = l_base.wrapping_add(l_off as u64);
        let r_addr = r_base.wrapping_add(r_off as u64);
        // 地址范围整体校验: 同时挡住 `base + off` 回绕 (wrapping 后落在用户区之外)
        if !is_user_buf(l_addr, n) || !is_user_buf(r_addr, n) {
            return partial_or(total, Errno::EFAULT);
        }
        if transfer(dir, target_cr3, l_addr, r_addr, n).is_err() {
            return partial_or(total, Errno::EFAULT);
        }
        total += n;
        l_off += n;
        r_off += n;
        if l_off == l_len as usize {
            li += 1;
            l_off = 0;
        }
        if r_off == r_len as usize {
            ri += 1;
            r_off = 0;
        }
    }
    Ok(total)
}

/// `process_vm_readv(pid, local_iov, liovcnt, remote_iov, riovcnt, flags)`
///
/// 从目标进程 `pid` 的用户内存读入当前进程.
///
/// # Errors
///
/// 见 [`process_vm_rw`].
pub fn process_vm_readv_syscall(
    pid: i32,
    local_iov: u64,
    liovcnt: u64,
    remote_iov: u64,
    riovcnt: u64,
    flags: u64,
) -> Result<usize, Errno> {
    process_vm_rw(
        pid,
        local_iov,
        liovcnt,
        remote_iov,
        riovcnt,
        flags,
        Direction::Read,
    )
}

/// `process_vm_writev(pid, local_iov, liovcnt, remote_iov, riovcnt, flags)`
///
/// 把当前进程用户内存写入目标进程 `pid`.
///
/// # Errors
///
/// 见 [`process_vm_rw`].
pub fn process_vm_writev_syscall(
    pid: i32,
    local_iov: u64,
    liovcnt: u64,
    remote_iov: u64,
    riovcnt: u64,
    flags: u64,
) -> Result<usize, Errno> {
    process_vm_rw(
        pid,
        local_iov,
        liovcnt,
        remote_iov,
        riovcnt,
        flags,
        Direction::Write,
    )
}

#[cfg(test)]
mod tests {
    use super::{process_vm_readv_syscall, process_vm_writev_syscall};
    use crate::framework::errno::Errno;

    #[test]
    fn test_flags_nonzero_einval() {
        assert_eq!(
            process_vm_readv_syscall(1, 0x1000, 1, 0x1000, 1, 1),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            process_vm_writev_syscall(1, 0x1000, 1, 0x1000, 1, 1),
            Err(Errno::EINVAL)
        );
    }

    #[test]
    fn test_iovcnt_over_max_einval() {
        let over = 4096;
        assert_eq!(
            process_vm_readv_syscall(1, 0x1000, over, 0x1000, 1, 0),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            process_vm_writev_syscall(1, 0x1000, 1, 0x1000, over, 0),
            Err(Errno::EINVAL)
        );
    }

    #[test]
    fn test_zero_iovcnt_ok() {
        // 任一侧为空向量即空搬运, 不解析指针也不查 pid
        assert_eq!(process_vm_readv_syscall(-1, 0, 0, 0x1000, 1, 0), Ok(0));
        assert_eq!(
            process_vm_writev_syscall(999_999, 0x1000, 1, 0, 0, 0),
            Ok(0)
        );
    }

    #[test]
    fn test_invalid_pid_esrch() {
        assert_eq!(
            process_vm_readv_syscall(0, 0x1000, 1, 0x1000, 1, 0),
            Err(Errno::ESRCH)
        );
        assert_eq!(
            process_vm_writev_syscall(-3, 0x1000, 1, 0x1000, 1, 0),
            Err(Errno::ESRCH)
        );
    }
}
