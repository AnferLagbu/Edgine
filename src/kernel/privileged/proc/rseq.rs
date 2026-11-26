//! D7 R1: `rseq` 注册/注销与用户区清理 (机制层, TCB)
//!
//! 仅实现 Linux 原始 32 字节 `struct rseq` ABI 与 legacy 模式:
//! - `cpu_id_start` / `cpu_id` 在注册、返回用户态时写当前逻辑 CPU;
//! - `rseq_cs` 在信号投递、退出、exec 清理时写 0, 避免悬垂临界区;
//! - 注销按 Linux 口径校验指针/长度/签名, 并写回
//!   `cpu_id_start=0` / `cpu_id=RSEQ_CPU_ID_UNINITIALIZED`.
//!
//! 边界:
//! - 本模块属 privileged: 直接读写用户内存; 所有写路径经 `userptr` 完整判据,
//!   坏指针/未映射/只读返回 `false`, 不触发内核页故障.
//! - 注册状态是 per-thread (`Process` 上的原子量); fork/clone 新 TCB 默认
//!   清零, 不继承父线程; exec 在销毁旧地址空间前显式清除.
//! - 接受 `rseq_len >= 32` (glibc 2.44 以 33 字节注册 feature 区),
//!   指针始终按 32 字节对齐; 内核只写 legacy 前 32 字节字段.

use core::sync::atomic::Ordering;

use crate::privileged::errno::Errno;
use crate::privileged::proc::api;
use crate::privileged::proc::{Process, process_get_current_pid};
use crate::privileged::userptr::{validate_user_buf, write_struct_to_user};

/// Linux 原始 `struct rseq` 最小长度.
pub const RSEQ_ORIG_SIZE: u32 = 32;

/// `struct rseq::rseq_cs` 字段偏移.
const RSEQ_CS_OFFSET: u64 = 8;

/// `RSEQ_FLAG_UNREGISTER`.
pub const RSEQ_FLAG_UNREGISTER: i32 = 1;

/// `RSEQ_CPU_ID_UNINITIALIZED`.
const RSEQ_CPU_ID_UNINITIALIZED: u32 = u32::MAX;

/// 注册/注销结果; `Ok(())` 表示内核状态与用户内存均已更新.
///
/// # Errors
///
/// `flags`/长度/指针对齐非法返回 `EINVAL`; 已注册同参返回 `EBUSY`;
/// 注销签名不一致返回 `EPERM`; 用户区未映射或写入失败返回 `EFAULT`;
/// 当前进程不存在返回 `ESRCH`.
pub fn rseq_syscall(rseq: u64, rseq_len: u32, flags: i32, sig: u32) -> Result<(), Errno> {
    if flags & RSEQ_FLAG_UNREGISTER != 0 {
        if flags != RSEQ_FLAG_UNREGISTER {
            return Err(Errno::Einval);
        }
        return unregister(rseq, rseq_len, sig);
    }
    if flags != 0 {
        return Err(Errno::Einval);
    }
    register(rseq, rseq_len, sig)
}

/// 注册当前线程的 rseq 用户区.
fn register(rseq: u64, rseq_len: u32, sig: u32) -> Result<(), Errno> {
    let pid = process_get_current_pid();
    let (old_ptr, old_len, old_sig) = read_state(pid)?;

    // 已注册时 Linux 先比较指针/长度/签名, 再考虑参数合法性.
    if old_ptr != 0 {
        if old_ptr != rseq || old_len != rseq_len {
            return Err(Errno::Einval);
        }
        if old_sig != sig {
            return Err(Errno::Eperm);
        }
        return Err(Errno::Ebusy);
    }

    // 接受原始 32 字节或 32 字节以上的 feature 区 (glibc 2.44 用 33);
    // 指针始终按 32 字节对齐, 内核只写 legacy 前 32 字节.
    if rseq_len < RSEQ_ORIG_SIZE || rseq == 0 || !rseq.is_multiple_of(32) {
        return Err(Errno::Einval);
    }
    if !validate_user_buf(rseq, u64::from(rseq_len)) {
        return Err(Errno::Efault);
    }

    // 先写 CPU ID, 成功后才发布注册状态; 失败不留下半注册.
    let cpu = crate::privileged::smp::get_current_cpu();
    if !write_struct_to_user(rseq, &cpu_pair(cpu)) {
        return Err(Errno::Efault);
    }

    // 长度/签名先于指针发布, 保证信号/退出路径看到非零指针时字段已完整.
    store_state(pid, rseq_len, sig);
    api::process_with(pid, |p| p.rseq_ptr.store(rseq, Ordering::Release));
    Ok(())
}

/// 注销当前线程的 rseq 用户区; 校验必须与注册值完全一致.
fn unregister(rseq: u64, rseq_len: u32, sig: u32) -> Result<(), Errno> {
    let pid = process_get_current_pid();
    let (old_ptr, old_len, old_sig) = read_state(pid)?;
    if old_ptr == 0 {
        return Err(Errno::Einval);
    }
    if old_ptr != rseq || old_len != rseq_len {
        return Err(Errno::Einval);
    }
    if old_sig != sig {
        return Err(Errno::Eperm);
    }

    // Linux 先写回未初始化状态, 再清除内核注册.
    if !write_struct_to_user(
        rseq,
        &(cpu_pair(0) | u64::from(RSEQ_CPU_ID_UNINITIALIZED) << 32),
    ) {
        return Err(Errno::Efault);
    }
    if !write_struct_to_user(rseq + RSEQ_CS_OFFSET, &0u64) {
        return Err(Errno::Efault);
    }

    api::process_with(pid, |p| {
        p.rseq_ptr.store(0, Ordering::Release);
        p.rseq_len.store(0, Ordering::Release);
        p.rseq_sig.store(0, Ordering::Release);
    });
    Ok(())
}

/// 返回用户态前刷新当前线程的 `cpu_id_start` / `cpu_id`.
///
/// 由信号/中断返回共同入口调用; 未注册时只做一次原子读.
pub fn refresh_cpu(proc: &Process) -> bool {
    let ptr = proc.rseq_ptr.load(Ordering::Acquire);
    if ptr == 0 {
        return true;
    }
    let cpu = crate::privileged::smp::get_current_cpu();
    write_struct_to_user(ptr, &cpu_pair(cpu))
}

/// 信号投递前清除 `rseq_cs`, 避免用户 handler 看到悬垂临界区指针.
pub fn clear_rseq_cs(proc: &Process) -> bool {
    let ptr = proc.rseq_ptr.load(Ordering::Acquire);
    if ptr == 0 {
        return true;
    }
    write_struct_to_user(ptr + RSEQ_CS_OFFSET, &0u64)
}

/// 进程退出清理: 清除 `rseq_cs` 并摘除注册状态.
///
/// 必须在 `process_exit` 切换内核页表之前调用 (用户地址空间仍有效).
pub fn exit_cleanup(pid: u32) {
    let ptr = api::process_with(pid, |p| {
        let ptr = p.rseq_ptr.swap(0, Ordering::AcqRel);
        p.rseq_len.store(0, Ordering::Release);
        p.rseq_sig.store(0, Ordering::Release);
        ptr
    })
    .unwrap_or(0);
    if ptr != 0 {
        let _ = write_struct_to_user(ptr + RSEQ_CS_OFFSET, &0u64);
    }
}

/// exec 替换地址空间前清除注册; 不写旧用户区 (旧地址空间即将销毁).
pub fn reset_registration(pid: u32) {
    let _ = api::process_with(pid, |p| {
        p.rseq_ptr.store(0, Ordering::Release);
        p.rseq_len.store(0, Ordering::Release);
        p.rseq_sig.store(0, Ordering::Release);
    });
}

fn read_state(pid: u32) -> Result<(u64, u32, u32), Errno> {
    api::process_with(pid, |p| {
        (
            p.rseq_ptr.load(Ordering::Acquire),
            p.rseq_len.load(Ordering::Acquire),
            p.rseq_sig.load(Ordering::Acquire),
        )
    })
    .ok_or(Errno::Esrch)
}

fn store_state(pid: u32, rseq_len: u32, sig: u32) {
    let _ = api::process_with(pid, |p| {
        p.rseq_len.store(rseq_len, Ordering::Release);
        p.rseq_sig.store(sig, Ordering::Release);
    });
}

/// `cpu_id_start` (低 32 位) 与 `cpu_id` (高 32 位) 合并写入.
const fn cpu_pair(cpu: u32) -> u64 {
    (cpu as u64) | ((cpu as u64) << 32)
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_rseq_cpu_pair() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    assert_eq_test!(cpu_pair(0), 0, "CPU 0 双字段");
    assert_eq_test!(cpu_pair(3), 0x0000_0003_0000_0003, "CPU 3 双字段");
    assert_eq_test!(cpu_pair(u32::MAX), u64::MAX, "最大值不溢出");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_rseq_offsets() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    // 用户 ABI: cpu_id_start@0, cpu_id@4, rseq_cs@8, 总 32 字节.
    assert_eq_test!(RSEQ_ORIG_SIZE, 32, "原始结构大小");
    assert_eq_test!(RSEQ_CS_OFFSET, 8, "rseq_cs 偏移");
    assert_eq_test!(RSEQ_FLAG_UNREGISTER, 1, "注销标志");
    assert_eq_test!(RSEQ_CPU_ID_UNINITIALIZED, u32::MAX, "未初始化 CPU ID");
    TestResult::Pass
}

/// 注册 `rseq` 内核测试用例。
#[cfg(feature = "kernel_test")]
pub fn register_rseq_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("rseq", "cpu_pair", test_rseq_cpu_pair);
    r.register("rseq", "offsets", test_rseq_offsets);
}
