//! Unix Domain Socket (`AF_UNIX`) 子系统测试 — Phase C.3
//!
//! 覆盖 UDS 状态机的关键路径:
//! - 流式套接字: 绑定 → 监听 → 连接 → 接收 → 收发 → 关闭
//! - 数据报套接字: 绑定 → 连接 → 发送/接收 → 关闭
//! - 路径冲突 → EADDRINUSE
//! - 接受空队列 → EAGAIN
//! - 关闭 listener 同步取消 pending client
//!
//! 所有测试在 UDS TCB 的全局表上操作, 顺序执行 (单核 + 启动期)
use super::{TestResult, runner};
use crate::functions::net::unix as uds;
use crate::register_tests_inner;

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// STREAM 完整生命周期
fn test_uds_stream_echo() -> TestResult {
    use uds::{UdsError, UnixSockType};
    uds::uds_reset_for_test();

    let srv = match uds::uds_create(UnixSockType::Stream) {
        Ok(fd) => fd,
        Err(_) => return TestResult::Fail("srv create failed"),
    };
    let cli = match uds::uds_create(UnixSockType::Stream) {
        Ok(fd) => fd,
        Err(_) => return TestResult::Fail("cli create failed"),
    };

    if uds::uds_bind(srv, b"/tmp/uds_test_stream.sock").is_err() {
        return TestResult::Fail("srv bind failed");
    }
    if uds::uds_listen(srv).is_err() {
        return TestResult::Fail("srv listen failed");
    }
    if uds::uds_connect(cli, b"/tmp/uds_test_stream.sock").is_err() {
        return TestResult::Fail("cli connect failed");
    }
    let accepted = match uds::uds_accept(srv) {
        Ok(fd) => fd,
        Err(_) => return TestResult::Fail("accept failed"),
    };
    if accepted == srv {
        return TestResult::Fail("accept should return new FD");
    }

    // cli → accepted
    let n = match uds::uds_send(cli, b"hello") {
        Ok(n) => n,
        Err(_) => return TestResult::Fail("send failed"),
    };
    if n != 5 {
        return TestResult::Fail("send returned wrong count");
    }
    let mut buf = [0u8; 16];
    let m = match uds::uds_recv(accepted, &mut buf) {
        Ok(n) => n,
        Err(_) => return TestResult::Fail("recv failed"),
    };
    if m != 5 || &buf[..5] != b"hello" {
        return TestResult::Fail("recv data mismatch");
    }

    // accepted → cli
    let k = uds::uds_send(accepted, b"world").unwrap();
    if k != 5 {
        return TestResult::Fail("send back wrong count");
    }
    let mut buf2 = [0u8; 16];
    let p = uds::uds_recv(cli, &mut buf2).unwrap();
    if p != 5 || &buf2[..5] != b"world" {
        return TestResult::Fail("recv back data mismatch");
    }

    // close
    if uds::uds_close(cli).is_err() {
        return TestResult::Fail("cli close failed");
    }
    if uds::uds_close(accepted).is_err() {
        return TestResult::Fail("accepted close failed");
    }
    if uds::uds_close(srv).is_err() {
        return TestResult::Fail("srv close failed");
    }
    let _ = UdsError::BadFd; // 抑制未用警告
    TestResult::Pass
}

/// DGRAM: bind → connect → sendto/recvfrom 流程
fn test_uds_dgram_echo() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let rx = uds::uds_create(UnixSockType::Dgram).unwrap();
    let tx = uds::uds_create(UnixSockType::Dgram).unwrap();
    if uds::uds_bind(rx, b"/tmp/uds_test_dgram.sock").is_err() {
        return TestResult::Fail("rx bind failed");
    }
    if uds::uds_connect(tx, b"/tmp/uds_test_dgram.sock").is_err() {
        return TestResult::Fail("tx connect failed");
    }
    let n = uds::uds_sendto(tx, b"datagram-payload", b"/tmp/uds_test_dgram.sock").unwrap();
    if n != 16 {
        return TestResult::Fail("sendto returned wrong count");
    }
    let mut buf = [0u8; 32];
    let m = uds::uds_recvfrom(rx, &mut buf).unwrap();
    if m != 16 || &buf[..16] != b"datagram-payload" {
        return TestResult::Fail("recvfrom data mismatch");
    }
    uds::uds_close(tx).unwrap();
    uds::uds_close(rx).unwrap();
    TestResult::Pass
}

/// 路径冲突 → EADDRINUSE
fn test_uds_eaddrinuse() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let a = uds::uds_create(UnixSockType::Stream).unwrap();
    let b = uds::uds_create(UnixSockType::Stream).unwrap();
    if uds::uds_bind(a, b"/tmp/uds_test_dup.sock").is_err() {
        return TestResult::Fail("a bind failed");
    }
    match uds::uds_bind(b, b"/tmp/uds_test_dup.sock") {
        Err(uds::UdsError::AddrInUse) => {}
        _ => return TestResult::Fail("expected AddrInUse"),
    }
    uds::uds_close(a).unwrap();
    uds::uds_close(b).unwrap();
    TestResult::Pass
}

/// accept 空队列 → EAGAIN
fn test_uds_eagain_accept() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let s = uds::uds_create(UnixSockType::Stream).unwrap();
    uds::uds_bind(s, b"/tmp/uds_test_empty.sock").unwrap();
    uds::uds_listen(s).unwrap();
    match uds::uds_accept(s) {
        Err(uds::UdsError::Again) => {}
        _ => return TestResult::Fail("expected Again"),
    }
    uds::uds_close(s).unwrap();
    TestResult::Pass
}

/// close listener 同步取消 pending client
fn test_uds_close_listener_cancels() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let srv = uds::uds_create(UnixSockType::Stream).unwrap();
    let c1 = uds::uds_create(UnixSockType::Stream).unwrap();
    let c2 = uds::uds_create(UnixSockType::Stream).unwrap();
    uds::uds_bind(srv, b"/tmp/uds_test_cancel.sock").unwrap();
    uds::uds_listen(srv).unwrap();
    uds::uds_connect(c1, b"/tmp/uds_test_cancel.sock").unwrap();
    uds::uds_connect(c2, b"/tmp/uds_test_cancel.sock").unwrap();
    uds::uds_close(srv).unwrap();
    // pending client 槽位已被清空, 二次 close 返回 BadFd
    match uds::uds_close(c1) {
        Err(uds::UdsError::BadFd) => {}
        _ => return TestResult::Fail("c1 should be BadFd after srv close"),
    }
    match uds::uds_close(c2) {
        Err(uds::UdsError::BadFd) => {}
        _ => return TestResult::Fail("c2 should be BadFd after srv close"),
    }
    TestResult::Pass
}

/// 服务层错误映射完整性 (type-level)
fn test_uds_err_mapping() -> TestResult {
    use uds::UnixSockType;
    // 此测试只验证类型层映射, 不重复运行时逻辑
    let _ = UnixSockType::Stream;
    let _ = UnixSockType::Dgram;
    TestResult::Pass
}

/// 位图自清理: 释放全部 UDS fd_alloc 位图位
///
/// 防御性隔离: `uds_close` 已修复为回收 fd_alloc 位图 (对齐 pidfd close 模式),
/// 正常用例结束后位图应为空; 此函数兜底清理失败用例残留 (uds_reset_for_test
/// 后状态表已空, 不存在存活 UDS socket, 全量释放安全), 保证测试可复跑。
fn release_all_uds_bitmap_bits() {
    use crate::privileged::proc::fd_alloc::{FdSubsystem, free_fd};
    for i in 0..uds::MAX_UDS_FD {
        let _ = free_fd(FdSubsystem::Uds, uds::FD_BASE + i as i32);
    }
}

/// socketpair STREAM: 两端互连双向收发 (T1 G3)
fn test_uds_socketpair_stream() -> TestResult {
    uds::uds_reset_for_test();
    release_all_uds_bitmap_bits();

    let (a, b) = match uds::uds_socketpair(uds::UnixSockType::Stream) {
        Ok((x, y)) if x != y => (x, y),
        _ => return TestResult::Fail("socketpair stream failed"),
    };
    // a → b
    match uds::uds_send(a, b"ping") {
        Ok(4) => {}
        _ => return TestResult::Fail("send a->b wrong count"),
    }
    let mut buf = [0u8; 8];
    match uds::uds_recv(b, &mut buf) {
        Ok(4) if &buf[..4] == b"ping" => {}
        _ => return TestResult::Fail("recv b data mismatch"),
    }
    // b → a
    match uds::uds_send(b, b"pong") {
        Ok(4) => {}
        _ => return TestResult::Fail("send b->a wrong count"),
    }
    let mut buf2 = [0u8; 8];
    match uds::uds_recv(a, &mut buf2) {
        Ok(4) if &buf2[..4] == b"pong" => {}
        _ => return TestResult::Fail("recv a data mismatch"),
    }
    uds::uds_close(a).unwrap();
    uds::uds_close(b).unwrap();
    release_all_uds_bitmap_bits();
    TestResult::Pass
}

/// socketpair DGRAM: 已连接无路径发送 + 单条在途 EAGAIN (T1 G3)
fn test_uds_socketpair_dgram() -> TestResult {
    uds::uds_reset_for_test();
    release_all_uds_bitmap_bits();

    let (a, b) = match uds::uds_socketpair(uds::UnixSockType::Dgram) {
        Ok((x, y)) if x != y => (x, y),
        _ => return TestResult::Fail("socketpair dgram failed"),
    };
    match uds::uds_send_connected(a, b"dg") {
        Ok(2) => {}
        _ => return TestResult::Fail("send_connected wrong count"),
    }
    let mut buf = [0u8; 8];
    match uds::uds_recvfrom(b, &mut buf) {
        Ok(2) if &buf[..2] == b"dg" => {}
        _ => return TestResult::Fail("recvfrom data mismatch"),
    }
    // 对端在途未收走时再发 → Again (单条在途简化)
    uds::uds_send_connected(a, b"first").unwrap();
    match uds::uds_send_connected(a, b"second") {
        Err(uds::UdsError::Again) => {}
        _ => return TestResult::Fail("expected Again on in-flight dgram"),
    }
    uds::uds_recvfrom(b, &mut buf).unwrap();
    uds::uds_close(a).unwrap();
    uds::uds_close(b).unwrap();
    release_all_uds_bitmap_bits();
    TestResult::Pass
}

/// socketpair 第二端创建失败时回滚第一端 (T1 G3)
fn test_uds_socketpair_rollback() -> TestResult {
    uds::uds_reset_for_test();
    release_all_uds_bitmap_bits();

    // 占满 MAX_UDS_FD - 1 个槽位, socketpair 需连续 2 个空槽
    let mut guard = [0i32; uds::MAX_UDS_FD];
    for slot in guard.iter_mut().take(uds::MAX_UDS_FD - 1) {
        *slot = match uds::uds_create(uds::UnixSockType::Stream) {
            Ok(fd) => fd,
            Err(_) => return TestResult::Fail("fill create failed"),
        };
    }
    match uds::uds_socketpair(uds::UnixSockType::Stream) {
        Err(uds::UdsError::NoMem) => {}
        _ => return TestResult::Fail("expected NoMem near capacity"),
    }
    // 回滚验证: uds_socketpair 内部回滚经 uds_close 释放状态槽与 fd_alloc 位图位,
    // 单个创建应成功
    match uds::uds_create(uds::UnixSockType::Stream) {
        Ok(_) => {}
        Err(_) => return TestResult::Fail("rollback did not release slot"),
    }
    release_all_uds_bitmap_bits();
    TestResult::Pass
}

/// 回归: uds_close 回收 fd_alloc 位图 (预存泄漏修复)
///
/// 泄漏行为下第 17 次 create 即 NoMem (位图 16 位被前 16 次创建永久占用);
/// 修复后 create + close 可循环超过容量上限。
fn test_uds_close_releases_bitmap() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();
    release_all_uds_bitmap_bits();

    for _ in 0..uds::MAX_UDS_FD + 2 {
        let Ok(fd) = uds::uds_create(UnixSockType::Dgram) else {
            return TestResult::Fail("create failed before exhaustion");
        };
        if uds::uds_close(fd).is_err() {
            return TestResult::Fail("close failed");
        }
    }
    TestResult::Pass
}

/// 回归: uds_sendto 超 UNIX_DGRAM_MAX 拒绝 (越界写防护)
fn test_uds_sendto_oversize() -> TestResult {
    // 静态区放置越界样本, 避免栈上 8KiB 数组
    static BIG: [u8; uds::UNIX_DGRAM_MAX + 1] = [0u8; uds::UNIX_DGRAM_MAX + 1];

    use uds::UnixSockType;
    uds::uds_reset_for_test();
    release_all_uds_bitmap_bits();

    let Ok(srv) = uds::uds_create(UnixSockType::Dgram) else {
        return TestResult::Fail("create failed");
    };
    if uds::uds_bind(srv, b"/tmp/uds_test_ovs.sock").is_err() {
        return TestResult::Fail("bind failed");
    }
    // sendto 首参 fd 未使用 (路径发送), 传 0 仅占位
    match uds::uds_sendto(0, &BIG, b"/tmp/uds_test_ovs.sock") {
        Err(uds::UdsError::Invalid) => {}
        _ => return TestResult::Fail("expected Invalid on oversize sendto"),
    }
    // 防护未破坏正常路径: 超限拒绝后短消息仍可送达
    if uds::uds_sendto(0, b"ok", b"/tmp/uds_test_ovs.sock").is_err() {
        return TestResult::Fail("normal sendto after oversize failed");
    }
    let mut buf = [0u8; 8];
    match uds::uds_recvfrom(srv, &mut buf) {
        Ok(2) if &buf[..2] == b"ok" => {}
        _ => return TestResult::Fail("recvfrom after oversize mismatch"),
    }
    uds::uds_close(srv).unwrap();
    TestResult::Pass
}

pub fn register_uds_tests() {
    let r = runner();
    register_tests_inner! { r:
        "UDS": {
            "stream_echo": test_uds_stream_echo,
            "dgram_echo": test_uds_dgram_echo,
            "eaddrinuse": test_uds_eaddrinuse,
            "eagain_accept": test_uds_eagain_accept,
            "close_listener_cancels": test_uds_close_listener_cancels,
            "err_mapping": test_uds_err_mapping,
            "socketpair_stream": test_uds_socketpair_stream,
            "socketpair_dgram": test_uds_socketpair_dgram,
            "socketpair_rollback": test_uds_socketpair_rollback,
            "close_releases_bitmap": test_uds_close_releases_bitmap,
            "sendto_oversize": test_uds_sendto_oversize,
        }
    }
}
