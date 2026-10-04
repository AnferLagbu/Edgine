//! IO 系统调用服务层参数验证测试
//!
//! 覆盖 services/fs/io.rs 的纯标量验证逻辑:
//! - pipe: fds != 0
//! - dup/dup2: oldfd/newfd >= 0
//! - fcntl: fd >= 0

use edgine_tests::*;

// ============================================================================
// pipe
// ============================================================================

#[test]
fn test_pipe_null_rejected() {
    assert_eq!(pipe_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_pipe_valid_buf() {
    // 合法用户空间地址 (8 字节)
    assert_eq!(pipe_validate(0x7fff_ffff_f000), Ok(()));
    assert_eq!(pipe_validate(0x1000), Ok(()));
}

#[test]
fn test_pipe_typical_use_case() {
    // shell pipeline: ls | grep | wc
    assert_eq!(pipe_validate(0x7fff_ffff_e000), Ok(()));
}

// ============================================================================
// dup
// ============================================================================

#[test]
fn test_dup_negative_rejected() {
    assert_eq!(dup_validate(-1), Err(Errno::EBADF));
    assert_eq!(dup_validate(-100), Err(Errno::EBADF));
    assert_eq!(dup_validate(i32::MIN), Err(Errno::EBADF));
}

#[test]
fn test_dup_valid_fd() {
    // 0 (stdin), 1 (stdout), 2 (stderr) 是常用
    assert_eq!(dup_validate(0), Ok(()));
    assert_eq!(dup_validate(1), Ok(()));
    assert_eq!(dup_validate(2), Ok(()));
    assert_eq!(dup_validate(100), Ok(()));
    assert_eq!(dup_validate(i32::MAX), Ok(()));
}

#[test]
fn test_dup_typical_scenarios() {
    // 重定向 stdout 到文件: dup(1)
    assert_eq!(dup_validate(1), Ok(()));
    // 复制 stdin
    assert_eq!(dup_validate(0), Ok(()));
}

// ============================================================================
// dup2
// ============================================================================

#[test]
fn test_dup2_negative_rejected() {
    assert_eq!(dup2_validate(-1, 0), Err(Errno::EBADF));
    assert_eq!(dup2_validate(0, -1), Err(Errno::EBADF));
    assert_eq!(dup2_validate(-1, -1), Err(Errno::EBADF));
    assert_eq!(dup2_validate(i32::MIN, 0), Err(Errno::EBADF));
    assert_eq!(dup2_validate(0, i32::MIN), Err(Errno::EBADF));
}

#[test]
fn test_dup2_valid_fds() {
    // 合法组合
    assert_eq!(dup2_validate(0, 1), Ok(()));
    assert_eq!(dup2_validate(1, 2), Ok(()));
    assert_eq!(dup2_validate(2, 5), Ok(()));
    assert_eq!(dup2_validate(0, 0), Ok(())); // same fd 合法
}

#[test]
fn test_dup2_same_fd() {
    // dup2(old, new) 其中 old == new
    // POSIX 语义: 立即返回 new (不关闭)
    assert_eq!(dup2_validate(5, 5), Ok(()));
    assert_eq!(dup2_validate(0, 0), Ok(()));
}

#[test]
fn test_dup2_typical_scenarios() {
    // shell: command >file  等价于  dup2(fd, 1); close(fd)
    assert_eq!(dup2_validate(3, 1), Ok(()));
    // 复制 stdin: dup2(0, 5)
    assert_eq!(dup2_validate(0, 5), Ok(()));
    // 错误流重定向: dup2(3, 2)
    assert_eq!(dup2_validate(3, 2), Ok(()));
}

// ============================================================================
// fcntl
// ============================================================================

#[test]
fn test_fcntl_negative_fd_rejected() {
    assert_eq!(fcntl_validate(-1, 1, 0), Err(Errno::EBADF));
    assert_eq!(fcntl_validate(-1, 0, 0), Err(Errno::EBADF));
}

#[test]
fn test_fcntl_valid_fd() {
    // F_DUPFD=0, F_GETFD=1, F_SETFD=2, F_GETFL=3, F_SETFL=4
    assert_eq!(fcntl_validate(0, 0, 0), Ok(()));  // F_DUPFD
    assert_eq!(fcntl_validate(1, 1, 0), Ok(()));  // F_GETFD
    assert_eq!(fcntl_validate(2, 2, 1), Ok(()));  // F_SETFD
    assert_eq!(fcntl_validate(3, 3, 0), Ok(()));  // F_GETFL
    assert_eq!(fcntl_validate(4, 4, 0), Ok(()));  // F_SETFL
}

#[test]
fn test_fcntl_arg_value() {
    // F_SETFD 接受 FD_CLOEXEC 标志
    assert_eq!(fcntl_validate(1, 2, 1), Ok(()));
    // F_DUPFD 接受目标 fd
    assert_eq!(fcntl_validate(1, 0, 5), Ok(()));
}

#[test]
fn test_fcntl_invalid_cmd_not_validated_at_services() {
    // cmd 验证由 framework 内部处理
    // services 仅查 fd >= 0
    assert_eq!(fcntl_validate(1, 999, 0), Ok(()));
    assert_eq!(fcntl_validate(1, -1, 0), Ok(()));
}

// ============================================================================
// 集成场景
// ============================================================================

#[test]
fn test_io_shell_redirection() {
    // shell 流程: open(file) -> dup2(fd, 1) -> close(fd)
    assert_eq!(pipe_validate(0x1000), Ok(()));    // 父进程创建管道
    assert_eq!(dup2_validate(3, 1), Ok(()));     // 复制到 stdout
    assert_eq!(fcntl_validate(1, 2, 1), Ok(())); // 设置 CLOEXEC
}

#[test]
fn test_io_pipe_dup_chain() {
    // 多阶段管道
    assert_eq!(pipe_validate(0x1000), Ok(()));    // pipe1
    assert_eq!(pipe_validate(0x1008), Ok(()));    // pipe2
    assert_eq!(dup_validate(0), Ok(()));         // stdin dup
    assert_eq!(dup2_validate(5, 1), Ok(()));     // stdout redir
}

#[test]
fn test_io_safety_no_panic() {
    // 极端边界值不 panic
    assert_eq!(pipe_validate(0), Err(Errno::EFAULT));
    assert_eq!(dup_validate(i32::MIN), Err(Errno::EBADF));
    assert_eq!(dup2_validate(i32::MAX, i32::MAX), Ok(()));
    assert_eq!(fcntl_validate(i32::MAX, 0, 0), Ok(()));
}
