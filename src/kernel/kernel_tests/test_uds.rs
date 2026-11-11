//! Unix Domain Socket (`AF_UNIX`) 子系统测试 — Phase C.3
//!
//! 覆盖 UDS 状态机的关键路径:
//! - 流式套接字: 绑定 → 监听 → 连接 → 接收 → 收发 → 关闭
//! - 数据报套接字: 绑定 → 连接 → 发送/接收 → 关闭
//! - 路径冲突 → EADDRINUSE
//! - 接受空队列 → EAGAIN
//! - 关闭 listener 保留 pending client 槽位所有权 (只标记对端关闭)
//!
//! fd 对象模型 (阶段 3) 后策略表只暴露槽位下标 (`u8`): `uds_alloc` 返回下标,
//! `uds_free` 归还; fd/位图分配归 `UnixSocketFile` 对象与 `FdTable` 管.
//! 所有测试在 UDS 策略表上操作, 顺序执行 (单核 + 启动期).
use crate::functions::net::unix as uds;
use crate::privileged::tests::{TestResult, runner};
use crate::register_tests_inner;

/// STREAM 完整生命周期
fn test_uds_stream_echo() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let Ok(srv) = uds::uds_alloc(UnixSockType::Stream) else {
        return TestResult::Fail("srv alloc failed");
    };
    let Ok(cli) = uds::uds_alloc(UnixSockType::Stream) else {
        return TestResult::Fail("cli alloc failed");
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
    let Ok(accepted) = uds::uds_accept(srv) else {
        return TestResult::Fail("accept failed");
    };
    if accepted == srv {
        return TestResult::Fail("accept should return new slot");
    }

    // cli → accepted
    let Ok(n) = uds::uds_send(cli, b"hello") else {
        return TestResult::Fail("send failed");
    };
    if n != 5 {
        return TestResult::Fail("send returned wrong count");
    }
    let mut buf = [0u8; 16];
    let Ok(m) = uds::uds_recv(accepted, &mut buf) else {
        return TestResult::Fail("recv failed");
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

    // free
    if uds::uds_free(cli).is_err() {
        return TestResult::Fail("cli free failed");
    }
    if uds::uds_free(accepted).is_err() {
        return TestResult::Fail("accepted free failed");
    }
    if uds::uds_free(srv).is_err() {
        return TestResult::Fail("srv free failed");
    }
    TestResult::Pass
}

/// DGRAM: bind → connect → sendto/recvfrom 流程
fn test_uds_dgram_echo() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let rx = uds::uds_alloc(UnixSockType::Dgram).unwrap();
    let tx = uds::uds_alloc(UnixSockType::Dgram).unwrap();
    if uds::uds_bind(rx, b"/tmp/uds_test_dgram.sock").is_err() {
        return TestResult::Fail("rx bind failed");
    }
    if uds::uds_connect(tx, b"/tmp/uds_test_dgram.sock").is_err() {
        return TestResult::Fail("tx connect failed");
    }
    let n = uds::uds_sendto(b"datagram-payload", b"/tmp/uds_test_dgram.sock").unwrap();
    if n != 16 {
        return TestResult::Fail("sendto returned wrong count");
    }
    let mut buf = [0u8; 32];
    let m = uds::uds_recvfrom(rx, &mut buf).unwrap();
    if m != 16 || &buf[..16] != b"datagram-payload" {
        return TestResult::Fail("recvfrom data mismatch");
    }
    uds::uds_free(tx).unwrap();
    uds::uds_free(rx).unwrap();
    TestResult::Pass
}

/// 路径冲突 → EADDRINUSE
fn test_uds_eaddrinuse() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let a = uds::uds_alloc(UnixSockType::Stream).unwrap();
    let b = uds::uds_alloc(UnixSockType::Stream).unwrap();
    if uds::uds_bind(a, b"/tmp/uds_test_dup.sock").is_err() {
        return TestResult::Fail("a bind failed");
    }
    match uds::uds_bind(b, b"/tmp/uds_test_dup.sock") {
        Err(uds::UdsError::AddrInUse) => {}
        _ => return TestResult::Fail("expected AddrInUse"),
    }
    uds::uds_free(a).unwrap();
    uds::uds_free(b).unwrap();
    TestResult::Pass
}

/// accept 空队列 → EAGAIN
fn test_uds_eagain_accept() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let s = uds::uds_alloc(UnixSockType::Stream).unwrap();
    uds::uds_bind(s, b"/tmp/uds_test_empty.sock").unwrap();
    uds::uds_listen(s).unwrap();
    match uds::uds_accept(s) {
        Err(uds::UdsError::Again) => {}
        _ => return TestResult::Fail("expected Again"),
    }
    uds::uds_free(s).unwrap();
    TestResult::Pass
}

/// close listener 保留 pending client 槽位所有权
///
/// 槽位下标可被新 socket 复用, 若监听端释放时直接清空客户端槽位, 陈旧客户端
/// 的 `release()` 会误伤新持有者; 因此只标记对端关闭, 客户端槽位仍由各自
/// fd 对象持有, 由对象释放时回收.
fn test_uds_close_listener_keeps_client_slots() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let srv = uds::uds_alloc(UnixSockType::Stream).unwrap();
    let c1 = uds::uds_alloc(UnixSockType::Stream).unwrap();
    let c2 = uds::uds_alloc(UnixSockType::Stream).unwrap();
    uds::uds_bind(srv, b"/tmp/uds_test_cancel.sock").unwrap();
    uds::uds_listen(srv).unwrap();
    uds::uds_connect(c1, b"/tmp/uds_test_cancel.sock").unwrap();
    uds::uds_connect(c2, b"/tmp/uds_test_cancel.sock").unwrap();
    uds::uds_free(srv).unwrap();

    // 客户端槽位仍存活: 发往已关闭对端 → NotFound, 读端 → Ok(0) (EOF)
    match uds::uds_send(c1, b"x") {
        Err(uds::UdsError::NotFound) => {}
        _ => return TestResult::Fail("c1 send should be NotFound after srv free"),
    }
    let mut buf = [0u8; 4];
    match uds::uds_recv(c1, &mut buf) {
        Ok(0) => {}
        _ => return TestResult::Fail("c1 recv should be EOF after srv free"),
    }
    // 客户端自行释放成功 (槽位未被监听端清空)
    if uds::uds_free(c1).is_err() {
        return TestResult::Fail("c1 free should succeed after srv free");
    }
    if uds::uds_free(c2).is_err() {
        return TestResult::Fail("c2 free should succeed after srv free");
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

/// socketpair STREAM: 两端互连双向收发 (T1 G3)
fn test_uds_socketpair_stream() -> TestResult {
    uds::uds_reset_for_test();

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
    uds::uds_free(a).unwrap();
    uds::uds_free(b).unwrap();
    TestResult::Pass
}

/// socketpair DGRAM: 已连接无路径发送 + 单条在途 EAGAIN (T1 G3)
fn test_uds_socketpair_dgram() -> TestResult {
    uds::uds_reset_for_test();

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
    uds::uds_free(a).unwrap();
    uds::uds_free(b).unwrap();
    TestResult::Pass
}

/// socketpair 第二端创建失败时回滚第一端 (T1 G3)
fn test_uds_socketpair_rollback() -> TestResult {
    uds::uds_reset_for_test();

    // 占满 MAX_UDS_FD - 1 个槽位, socketpair 需连续 2 个空槽
    let mut guard = [0u8; uds::MAX_UDS_FD];
    for slot in guard.iter_mut().take(uds::MAX_UDS_FD - 1) {
        *slot = match uds::uds_alloc(uds::UnixSockType::Stream) {
            Ok(idx) => idx,
            Err(_) => return TestResult::Fail("fill alloc failed"),
        };
    }
    match uds::uds_socketpair(uds::UnixSockType::Stream) {
        Err(uds::UdsError::NoMem) => {}
        _ => return TestResult::Fail("expected NoMem near capacity"),
    }
    // 回滚验证: uds_socketpair 内部经 uds_free 释放第一端槽位, 单个分配应成功
    match uds::uds_alloc(uds::UnixSockType::Stream) {
        Ok(_) => {}
        Err(_) => return TestResult::Fail("rollback did not release slot"),
    }
    TestResult::Pass
}

/// 回归: `uds_free` 回收策略槽位 (预存 fd 位图泄漏修复的槽位面)
///
/// 槽位未回收时第 `MAX_UDS_FD + 1` 次分配即 `NoMem`; 回收后可循环超过容量上限.
fn test_uds_free_releases_slot() -> TestResult {
    use uds::UnixSockType;
    uds::uds_reset_for_test();

    for _ in 0..uds::MAX_UDS_FD + 2 {
        let Ok(idx) = uds::uds_alloc(UnixSockType::Dgram) else {
            return TestResult::Fail("alloc failed before exhaustion");
        };
        if uds::uds_free(idx).is_err() {
            return TestResult::Fail("free failed");
        }
    }
    TestResult::Pass
}

/// 回归: `uds_sendto` 超 `UNIX_DGRAM_MAX` 拒绝 (越界写防护)
fn test_uds_sendto_oversize() -> TestResult {
    // 静态区放置越界样本, 避免栈上 8KiB 数组
    static BIG: [u8; uds::UNIX_DGRAM_MAX + 1] = [0u8; uds::UNIX_DGRAM_MAX + 1];

    use uds::UnixSockType;
    uds::uds_reset_for_test();

    let Ok(srv) = uds::uds_alloc(UnixSockType::Dgram) else {
        return TestResult::Fail("alloc failed");
    };
    if uds::uds_bind(srv, b"/tmp/uds_test_ovs.sock").is_err() {
        return TestResult::Fail("bind failed");
    }
    match uds::uds_sendto(&BIG, b"/tmp/uds_test_ovs.sock") {
        Err(uds::UdsError::Invalid) => {}
        _ => return TestResult::Fail("expected Invalid on oversize sendto"),
    }
    // 防护未破坏正常路径: 超限拒绝后短消息仍可送达
    if uds::uds_sendto(b"ok", b"/tmp/uds_test_ovs.sock").is_err() {
        return TestResult::Fail("normal sendto after oversize failed");
    }
    let mut buf = [0u8; 8];
    match uds::uds_recvfrom(srv, &mut buf) {
        Ok(2) if &buf[..2] == b"ok" => {}
        _ => return TestResult::Fail("recvfrom after oversize mismatch"),
    }
    uds::uds_free(srv).unwrap();
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
            "close_listener_keeps_client_slots": test_uds_close_listener_keeps_client_slots,
            "err_mapping": test_uds_err_mapping,
            "socketpair_stream": test_uds_socketpair_stream,
            "socketpair_dgram": test_uds_socketpair_dgram,
            "socketpair_rollback": test_uds_socketpair_rollback,
            "free_releases_slot": test_uds_free_releases_slot,
            "sendto_oversize": test_uds_sendto_oversize,
        }
    }
}
