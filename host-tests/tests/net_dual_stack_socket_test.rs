//! net: V4/V6 双栈 socket 行为契约验证
//!
//! 验收:
//!   - sm_socket (sm_fi.rs) 接受 domain=2 (AF_INET) 与 domain=10 (AF_INET6)
//!   - privileged/net/socket_types.rs `Domain` 枚举含 `Inet6 = 10`
//!     (DECISION-J 2026-09-13: wire 类型迁回 privileged, functions 侧 re-export)
//!   - functions/net/syscall.rs bind/connect 按 family 分流 (2|10 → fw)
//!   - privileged/net/syscall.rs `raw_read_sockaddr_in6` 28 字节 copy-in
//!   - SmoltcpNetStack (smoltcp_impl.rs) sockaddr 转换支持 V6 (28 字节)
//!
//! 追踪: DECISION-032 (IPv4/IPv6 双栈)
//! SPDX-License-Identifier: MPL-2.0

use std::fs;

const SM_FI_RS: &str = "../src/kernel/privileged/net/init/sm_fi.rs";
const SOCKET_RS: &str = "../src/kernel/privileged/net/socket_types.rs";
const SVC_SYSCALL_RS: &str = "../src/kernel/functions/net/syscall.rs";
const FW_SYSCALL_RS: &str = "../src/kernel/privileged/net/syscall.rs";
const SMOLTCP_IMPL_RS: &str = "../src/kernel/functions/net/smoltcp_impl.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {}", path, e))
}

#[test]
fn test_sm_socket_accepts_af_inet6() {
    let src = read(SM_FI_RS);
    // sm_socket 必须同时接受 AF_INET (2) 与 AF_INET6 (10)
    assert!(
        src.contains("domain == 2 || domain == 10"),
        "sm_socket 未接受 AF_INET6 (domain=10)"
    );
}

#[test]
fn test_domain_enum_has_inet6() {
    let src = read(SOCKET_RS);
    assert!(
        src.contains("Inet6 = 10"),
        "Domain 枚举缺 Inet6 = 10 (AF_INET6)"
    );
    assert!(
        src.contains("10 => Some(Self::Inet6)"),
        "Domain::from_i32 未映射 10 → Inet6"
    );
}

#[test]
fn test_functions_syscall_bind_connect_dual_stack() {
    let src = read(SVC_SYSCALL_RS);
    // functions/net/syscall.rs bind_syscall / connect_syscall 的 family 分流
    let branches = src.matches("2 | 10 =>").count();
    assert!(
        branches >= 2,
        "functions/net/syscall.rs bind/connect 未按 family 分流 (2|10), 当前 {} 处",
        branches
    );
}

#[test]
fn test_fw_raw_read_sockaddr_in6() {
    let src = read(FW_SYSCALL_RS);
    assert!(
        src.contains("pub fn raw_read_sockaddr_in6(ptr: u64) -> Result<[u8; 28], Errno>"),
        "privileged/net/syscall.rs 缺 raw_read_sockaddr_in6 (28 字节)"
    );
    // bind/connect/sendto 按 family 10 分支传 28 字节
    assert!(
        src.contains("10 =>"),
        "privileged/net/syscall.rs 缺 AF_INET6 (10) 分支"
    );
}

#[test]
fn test_smoltcp_impl_sockaddr_v6() {
    let src = read(SMOLTCP_IMPL_RS);
    // endpoint_to_sockaddr 返回 (缓冲区, 长度) 且 V6 分支写 28 字节
    assert!(
        src.contains("fn endpoint_to_sockaddr(ep: NetEndpoint) -> ([u8; 28], u32)"),
        "endpoint_to_sockaddr 未支持 V6 (应返回 28 字节缓冲 + 长度)"
    );
    assert!(
        src.contains("IpAddr::V6(v6)"),
        "endpoint_to_sockaddr 缺 V6 分支"
    );
    // sockaddr_to_endpoint 按 family (2/10) 分支解析
    assert!(
        src.contains("(10, 0) =>"),
        "sockaddr_to_endpoint 缺 AF_INET6 (10) 分支"
    );
}

#[test]
fn test_smoltcp_impl_recv_buffers_28_bytes() {
    let src = read(SMOLTCP_IMPL_RS);
    // recvfrom/accept 的 sockaddr 栈缓冲必须 ≥ 28 字节 (容纳 sockaddr_in6)
    let buf_count = src.matches("[0u8; 28]").count();
    assert!(
        buf_count >= 3,
        "SmoltcpNetStack recvfrom/accept 栈缓冲未扩至 28 字节 (当前 {} 处)",
        buf_count
    );
}
