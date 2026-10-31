//! P-18 单入口防回归: socket 能力收敛到 `SocketHandle` 域
//!
//! 验证:
//! 1. inherent `*_fd` 平行入口与 `active_fds` 追踪表已退役
//! 2. `handle_map` 登记 `(user_id, privileged_fd)` 二元组
//! 3. `socket_open` 经 `sm_net_socket` 创建并登记句柄
//! 4. `accept` 把 privileged 返回的新 fd 登记为新句柄 (失败即归还 fd)
//! 5. `NetStack` trait 提供 close / setsockopt / getsockopt / `poll_sockets`
//! 6. DHCP 预留槽位用负 fd 哨兵, 数据面操作被 `handle_to_fd` 过滤拒绝
//! 7. `functions::net::socket` 全部操作以 `SocketHandle` 寻址
//! 8. 句柄生命周期单元测试存在

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

const SMOLTCP_IMPL_RS: &str = "src/kernel/functions/net/smoltcp_impl.rs";
const SOCKET_RS: &str = "src/kernel/functions/net/socket.rs";

/// 取 `sig` 起始到下一个 impl 方法 (`\n    fn `) 之间的片段.
fn impl_fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let idx = src.find(sig).unwrap_or_else(|| panic!("应存在 {sig}"));
    let rest = &src[idx..];
    let end = rest[1..].find("\n    fn ").map_or(rest.len(), |e| e + 1);
    &rest[..end]
}

// ============================================================================
// 退役的 inherent fd 入口
// ============================================================================

/// P-18 前的 10 个 fd-based 操作 + 创建/轮询入口.
const RETIRED_FD_METHODS: &[&str] = &[
    "is_active_fd",
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
    "poll_all_fd",
    "socket_create_fd",
    "close_fd",
];

#[test]
fn test_inherent_fd_api_retired() {
    let src = read_src(SMOLTCP_IMPL_RS);
    for method in RETIRED_FD_METHODS {
        let sig = format!("fn {method}(");
        assert!(
            !src.contains(&sig),
            "inherent `{method}` 应已退役 (单入口 = SocketHandle 域)"
        );
    }
    assert!(
        !src.contains("active_fds"),
        "active_fds 追踪表应已删除 (fd 有效性由 handle_map/fd 本身保证)"
    );
}

// ============================================================================
// 句柄表结构
// ============================================================================

#[test]
fn test_handle_map_shape() {
    let src = read_src(SMOLTCP_IMPL_RS);
    assert!(
        src.contains("type HandleSlot = Option<(u32, i32)>;"),
        "HandleSlot 应登记 (user_id, privileged_fd) 二元组"
    );
    assert!(
        src.contains("handle_map: [HandleSlot; MAX_SOCKETS]"),
        "SmoltcpNetStack 应以 handle_map: [HandleSlot; MAX_SOCKETS] 保存句柄"
    );
    assert!(
        src.contains("handle_map: [None; MAX_SOCKETS]"),
        "new() 应初始化 handle_map 为空表"
    );
}

// ============================================================================
// 创建 / 接受路径登记句柄
// ============================================================================

#[test]
fn test_socket_open_single_entry() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let body = impl_fn_body(&src, "fn socket_open(");
    assert!(
        body.contains("fw_net_socket::sm_net_socket(domain, sock_type, 0)"),
        "socket_open 应经唯一的 sm_net_socket 入口创建 privileged fd"
    );
    assert!(
        body.contains("self.handle_map[slot] = Some((user_id, fd))"),
        "socket_open 应将 (user_id, fd) 登记入 handle_map"
    );
    assert!(
        !body.contains("smoltcp_net_stack_socket_open"),
        "socket_open 不得再走 SmoltcpNetStack 专属桥接"
    );
}

#[test]
fn test_accept_registers_returned_handle() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let body = impl_fn_body(&src, "fn accept(");
    assert!(
        body.contains("self.insert_handle(new_fd)"),
        "accept 应把 privileged 返回的 fd 登记为新句柄"
    );
    assert!(
        body.contains("sm_net_close(new_fd)"),
        "登记失败时应归还 privileged fd, 不留无人认领的连接"
    );
    assert!(
        body.contains("Ok(new_handle)"),
        "accept 应返回登记后的新句柄, 而非 privileged fd 数值"
    );
}

// ============================================================================
// trait 能力面
// ============================================================================

#[test]
fn test_trait_impl_has_capability_ops() {
    let src = read_src(SMOLTCP_IMPL_RS);
    for sig in [
        "fn close(",
        "fn setsockopt(",
        "fn getsockopt(",
        "fn poll_sockets(",
    ] {
        assert!(
            src.contains(sig),
            "SmoltcpNetStack 必须实现 {sig} (抽象层作为唯一能力入口)"
        );
    }
}

// ============================================================================
// DHCP 预留槽位
// ============================================================================

#[test]
fn test_dhcp_reserved_slot_uses_sentinel() {
    let src = read_src(SMOLTCP_IMPL_RS);
    assert!(
        src.contains("const DHCP_SENTINEL_FD: i32 = -1;"),
        "DHCP 预留槽位应以 -1 哨兵占位"
    );
    assert!(
        src.contains("self.insert_handle(DHCP_SENTINEL_FD)"),
        "init() 的 DHCP 分支应把哨兵登记为句柄"
    );
    assert!(
        src.contains(".filter(|fd| *fd >= 0)"),
        "handle_to_fd 应过滤负 fd, 使 DHCP 句柄无法触达数据面 FFI"
    );
}

// ============================================================================
// functions::net::socket 的 SocketHandle 域
// ============================================================================

#[test]
fn test_socket_rs_uses_socket_handle_domain() {
    let src = read_src(SOCKET_RS);
    assert!(
        src.contains("pub use crate::privileged::net::iface_trait::SocketHandle;"),
        "socket.rs 应 re-export SocketHandle 作为公开句柄类型"
    );
    let handle_params = src.matches("h: SocketHandle").count();
    assert!(
        handle_params >= 10,
        "socket.rs 全部操作应以 SocketHandle 寻址, 实测 h: SocketHandle 出现 {handle_params} 次"
    );
    assert!(
        src.contains("SocketKind::Tcp") && src.contains("SocketKind::Udp"),
        "socket() 应把 Stream/Dgram 映射为 Tcp/Udp 的 SocketKind"
    );
    for retired in ["fn bind(fd: i32", "fn close(fd: i32", "fn accept(fd: i32"] {
        assert!(
            !src.contains(retired),
            "socket.rs 不得保留数值 fd 形参入口: {retired}"
        );
    }
}

// ============================================================================
// functions 层 0 unsafe 不变式
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
// 句柄生命周期单元测试
// ============================================================================

#[test]
fn test_handle_lifecycle_unit_tests_present() {
    let src = read_src(SMOLTCP_IMPL_RS);
    let tests = [
        "test_handle_map_initialized_empty",
        "test_insert_handle_maps_to_privileged_fd",
        "test_reserved_slot_released_by_close",
        "test_handle_table_exhaustion",
        "test_socket_open_rejects_unsupported_kind",
        "test_socket_ops_reject_unknown_handle",
        "test_dhcp_handle_rejects_data_ops",
    ];
    let found = tests.iter().filter(|t| src.contains(*t)).count();
    assert_eq!(
        found,
        tests.len(),
        "句柄生命周期单元测试应全部存在, 实测 {found}/{}",
        tests.len()
    );
}
