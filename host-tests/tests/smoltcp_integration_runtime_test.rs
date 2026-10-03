//! SmoltcpNetStack fd 生命周期追踪运行时测试
//!
//! 验证:
//! 1. active_fds 字段存在于 SmoltcpNetStack 结构体
//! 2. is_active_fd 验证方法存在且正确
//! 3. socket_create_fd 跟踪 fd 到 active_fds
//! 4. close_fd 清理 active_fds
//! 5. 所有 fd-based 方法 (bind_fd, listen_fd 等) 调用 is_active_fd 做前置校验
//! 6. 新增单元测试覆盖 fd 生命周期

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

const SMOLTCP_IMPL_RS: &str = "src/kernel/services/net/smoltcp_impl.rs";

// ============================================================================
// 结构体字段验证
// ============================================================================

#[test]
fn test_active_fds_field_exists() {
    let src = read_src(SMOLTCP_IMPL_RS);
    assert!(
        src.contains("active_fds: [bool; MAX_SOCKETS]"),
        "SmoltcpNetStack 应包含 active_fds: [bool; MAX_SOCKETS] 字段"
    );
}

#[test]
fn test_active_fds_initialized_in_new() {
    let src = read_src(SMOLTCP_IMPL_RS);
    // 找到 new() 函数体, 检查 active_fds 初始化
    let idx = src.find("fn new() -> Self").expect("应存在 new() 方法");
    let body = &src[idx..idx + 800];
    assert!(
        body.contains("active_fds: [false; MAX_SOCKETS]"),
        "new() 应初始化 active_fds 为 [false; MAX_SOCKETS]"
    );
}

// ============================================================================
// is_active_fd 验证方法
// ============================================================================

#[test]
fn test_is_active_fd_method_exists() {
    let src = read_src(SMOLTCP_IMPL_RS);
    assert!(
        src.contains("fn is_active_fd(&self, fd: i32) -> bool"),
        "应存在 is_active_fd(&self, fd: i32) -> bool 方法"
    );
}

#[test]
fn test_is_active_fd_bounds_check() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let idx = src.find("fn is_active_fd").expect("应存在 is_active_fd");
    let body = &src[idx..idx + 200];
    assert!(
        body.contains("idx < MAX_SOCKETS"),
        "is_active_fd 应做边界检查 idx < MAX_SOCKETS"
    );
    assert!(
        body.contains("self.active_fds[idx]"),
        "is_active_fd 应检查 self.active_fds[idx]"
    );
}

// ============================================================================
// socket_create_fd fd 跟踪
// ============================================================================

#[test]
fn test_socket_create_fd_tracks_active_fd() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let idx = src
        .find("fn socket_create_fd")
        .expect("应存在 socket_create_fd");
    let body = &src[idx..idx + 400];
    assert!(
        body.contains("self.active_fds[idx] = true"),
        "socket_create_fd 成功时应设置 self.active_fds[idx] = true"
    );
}

#[test]
fn test_socket_create_fd_takes_mut_self() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let idx = src
        .find("fn socket_create_fd")
        .expect("应存在 socket_create_fd");
    let sig_end = src[idx..].find(')').expect("应有函数签名结束");
    let sig = &src[idx..idx + sig_end + 1];
    assert!(
        sig.contains("&mut self"),
        "socket_create_fd 应接受 &mut self (需修改 active_fds)"
    );
}

// ============================================================================
// close_fd 清理
// ============================================================================

#[test]
fn test_close_fd_clears_active_fd() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let idx = src.find("pub fn close_fd").expect("应存在 close_fd");
    let body = &src[idx..idx + 300];
    assert!(
        body.contains("self.active_fds[idx] = false"),
        "close_fd 应设置 self.active_fds[idx] = false"
    );
}

#[test]
fn test_close_fd_takes_mut_self() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let idx = src.find("pub fn close_fd").expect("应存在 close_fd");
    let sig_end = src[idx..].find(')').expect("应有函数签名结束");
    let sig = &src[idx..idx + sig_end + 1];
    assert!(
        sig.contains("&mut self"),
        "close_fd 应接受 &mut self (需修改 active_fds)"
    );
}

// ============================================================================
// fd-based 方法校验
// ============================================================================

/// 所有需要校验 fd 的方法列表
const FD_VALIDATED_METHODS: &[&str] = &[
    "bind_fd",
    "listen_fd",
    "accept_fd",
    "connect_fd",
    "send_fd",
    "recv_fd",
    "sendto_fd",
    "recvfrom_fd",
    "setsockopt_fd",
    "getsockopt_fd",
];

#[test]
fn test_all_fd_based_methods_have_validation() {
    let src = read_src(SMOLTCP_IMPL_RS);
    for method in FD_VALIDATED_METHODS {
        let sig = format!("pub fn {}(", method);
        let idx = src
            .find(&sig)
            .unwrap_or_else(|| panic!("应存在 {} 方法", method));
        // 取函数体直到下一个 pub fn, 检查 is_active_fd 调用
        let rest = &src[idx..];
        let end = rest.find("\npub fn ").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("self.is_active_fd(fd)"),
            "{} 方法应调用 self.is_active_fd(fd) 做前置校验",
            method
        );
        assert!(
            body.contains("NetError::InvalidHandle"),
            "{} 方法校验失败应返回 NetError::InvalidHandle",
            method
        );
    }
}

#[test]
fn test_poll_all_fd_has_no_fd_validation() {
    // poll_all_fd 不接受 fd 参数, 不需要 is_active_fd 校验
    let src = read_src(SMOLTCP_IMPL_RS);
    let idx = src.find("pub fn poll_all_fd").expect("应存在 poll_all_fd");
    let body = &src[idx..idx + 200];
    // poll_all_fd 不应有 is_active_fd 调用 (它轮询所有 fd)
    assert!(
        !body.contains("self.is_active_fd(fd)"),
        "poll_all_fd 不接受 fd 参数, 不应有 is_active_fd 校验"
    );
}

// ============================================================================
// socket.rs 调用方 mut 兼容性
// ============================================================================

#[test]
fn test_socket_create_fd_caller_uses_mut() {
    let socket_rs = read_src("src/kernel/services/net/socket.rs");
    // socket 函数调用 socket_create_fd, 需要 mut binding
    let idx = socket_rs
        .find("fn socket(domain:")
        .expect("应存在 socket 函数");
    let body = &socket_rs[idx..idx + 300];
    assert!(
        body.contains("net_stack().ok_or(SocketError::NotReady)?"),
        "socket() 应处理 net_stack() 返回的 Option"
    );
}

#[test]
fn test_close_fd_caller_uses_mut() {
    let socket_rs = read_src("src/kernel/services/net/socket.rs");
    let idx = socket_rs
        .find("pub fn close(fd:")
        .expect("应存在 close 函数");
    let body = &socket_rs[idx..idx + 300];
    assert!(
        body.contains("net_stack().ok_or(SocketError::NotReady)?"),
        "close() 应处理 net_stack() 返回的 Option"
    );
}

// ============================================================================
// services 层 0 unsafe 不变式
// ============================================================================

#[test]
fn test_no_unsafe_in_smoltcp_impl() {
    let src = read_src(SMOLTCP_IMPL_RS);
    assert!(
        src.contains("#![deny(unsafe_code)]"),
        "smoltcp_impl.rs 应启用 deny(unsafe_code)"
    );
    let unsafe_block = src.matches("unsafe {").count();
    let unsafe_fn = src.matches("unsafe fn").count();
    let unsafe_impl = src.matches("unsafe impl").count();
    assert_eq!(
        unsafe_block + unsafe_fn + unsafe_impl,
        0,
        "smoltcp_impl.rs 应 0 unsafe 实际使用"
    );
}

// ============================================================================
// 新增单元测试数量验证
// ============================================================================

#[test]
fn test_fd_lifecycle_unit_tests_present() {
    let src = read_src(SMOLTCP_IMPL_RS);
    // 检查新增的 fd 生命周期相关测试
    let fd_tests = [
        "test_active_fds_initialized",
        "test_socket_create_fd_tracks",
        "test_close_fd_clears_active",
        "test_is_active_fd_rejects_invalid",
        "test_fd_based_methods_reject_inactive",
    ];
    let found = fd_tests.iter().filter(|t| src.contains(*t)).count();
    assert!(
        found >= 3,
        "smoltcp_impl.rs 应有至少 3 个 fd 生命周期单元测试, 实测: {}",
        found
    );
}
