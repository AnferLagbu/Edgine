// SPDX-License-Identifier: MPL-2.0
// B07-21/22: 分册 7 网络凭据与 pwm_set 权限回归
//
// 验收 (B07-01/02/03/05):
//   - UDS send/sendto 不得再硬编码 pid=1/uid=0/gid=0 (伪造 root 凭据)
//   - UDS recv 路径须反序列化真实凭据 (不再返回占位全零)
//   - recvmsg 回传凭据须从接收缓冲取真实值 (不再伪造 pid=1)
//   - socket 句柄 alloc_user_id 无 wrapping 回绕复用 (冲突即报错)
//   - pwm_set_syscall 须先校验 SYSTEM 域 SET_PWM 能力位 (防任意提权)
//
// 来源: docs/plan/audit-fix-07-services-net-ipc-credo.md

use std::fs;
use std::path::Path;

const UNIX: &str = "src/kernel/services/net/unix.rs";
const SYS_NET: &str = "src/kernel/services/net/syscall.rs";
const SMOLTCP: &str = "src/kernel/services/net/smoltcp_impl.rs";
const AUTH: &str = "src/kernel/services/sgeg/auth.rs";
// 第二十五批: capability 权威定义反转归位 framework (DECISION-K 项 5 sgeg
// 判据), services/sgeg/capability.rs 仅 re-export 壳 — 源码断言改读权威路径
const CAP: &str = "src/kernel/framework/sgeg/capability.rs";

fn read(p: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

// B07-01: UDS 发送路径不得硬编码伪造 root 凭据
#[test]
fn uds_send_uses_real_credentials() {
    let src = read(UNIX);
    // 硬编码伪造凭据必须消失
    assert!(
        !src.contains("pid: 1,"),
        "unix.rs 不得再硬编码 pid=1 (B07-01)"
    );
    assert!(
        !src.contains("uid: 0,"),
        "unix.rs 不得再硬编码 uid=0 (B07-01)"
    );
    assert!(
        !src.contains("gid: 0,"),
        "unix.rs 不得再硬编码 gid=0 (B07-01)"
    );
    // 必须使用当前进程真实凭据
    assert!(
        src.contains("fn current_scm_credentials"),
        "unix.rs 必须有 current_scm_credentials helper (B07-01)"
    );
    assert!(
        src.contains("process_get_current_pid")
            && src.contains("get_current_uid")
            && src.contains("get_current_gid"),
        "current_scm_credentials 必须取真实 pid/uid/gid (B07-01)"
    );
}

// B07-01: UDS 接收路径必须反序列化真实凭据 (不再返回占位全零)
#[test]
fn uds_recv_deserializes_credentials() {
    let src = read(UNIX);
    assert!(
        src.contains("u32::from_ne_bytes(pid)"),
        "uds_recv 必须反序列化接收缓冲末尾凭据 (B07-01)"
    );
    // 不得再返回占位全零凭据
    assert!(
        !src.contains("Some(ScmCredentials {\n                    pid: 0,"),
        "uds_recv 不得返回占位全零凭据 (B07-01)"
    );
}

// B07-02: syscall 层 sendmsg/recvmsg 不得伪造凭据
#[test]
fn syscall_uses_real_credentials() {
    let src = read(SYS_NET);
    assert!(
        !src.contains("let pid: u64 = 1;"),
        "sendmsg_syscall 不得硬编码 pid=1 (B07-02)"
    );
    assert!(
        !src.contains("1u64 << 32"),
        "recvmsg_syscall 不得伪造 pid=1 (B07-02)"
    );
    assert!(
        src.contains("uds_peer_creds(fd)"),
        "recvmsg 必须从 UDS 接收缓冲取真实凭据 (B07-02)"
    );
}

// B07-03: socket 句柄分配不得 wrapping 回绕复用
#[test]
fn socket_handle_no_wrapping_reuse() {
    let src = read(SMOLTCP);
    assert!(
        !src.contains("wrapping_add(1)"),
        "alloc_user_id 不得使用 wrapping_add 回绕 (B07-03)"
    );
    assert!(
        src.contains("fn alloc_user_id(&mut self) -> Option<u32>"),
        "alloc_user_id 应返回 Option 以表示句柄耗尽 (B07-03)"
    );
    assert!(
        src.contains(".any(|slot| matches!(slot, Some((u, _)) if *u == id))"),
        "alloc_user_id 必须做句柄冲突检测 (B07-03)"
    );
}

// B07-05: pwm_set_syscall 必须校验能力位
#[test]
fn pwm_set_requires_capability() {
    let auth_src = read(AUTH);
    assert!(
        auth_src.contains("SYSTEM_CAP_SET_PWM"),
        "pwm_set_syscall 必须引用 SYSTEM_CAP_SET_PWM (B07-05)"
    );
    assert!(
        auth_src.contains("pwm_has_capability"),
        "pwm_set_syscall 必须校验能力位 (B07-05)"
    );
    assert!(
        auth_src.contains("Errno::EPERM"),
        "pwm_set 无能力应返回 EPERM (B07-05)"
    );

    let cap_src = read(CAP);
    assert!(
        cap_src.contains("pub const SYSTEM_CAP_SET_PWM: u64 = 1 << 1;"),
        "capability.rs 必须定义 SYSTEM 域 bit1 专用能力位 (B07-05, DECISION-078)"
    );
}
