// TD-04: 验证 close 路径在释放槽位后唤醒 epoll 等待者, 防止 epoll_wait 睡在已关闭 fd 上
//
// 验收:
//   1. eventfd 关闭走 VFS 通用 `vfs_close_internal`: 先 `OPEN_FILE_TABLE.close`
//      释放槽位, 后 `fd_notify::notify_fd_close` 唤醒等待者
//   2. SFD sys_signalfd_close 在释放 SFD_TABLE 锁之后调用 epoll_pwake
//   3. 已关闭 fd 在 epoll 扫描中按无效 fd 处理, 统一报告 EPOLLERR|EPOLLHUP
//
// 注: 静态契约扫描, 不进内核态.
// fd 对象模型阶段 3 (2026-11-10): eventfd 由 privileged 子系统表迁至
// OpenFileTable 统一挂表, close 路径不再是 `sys_eventfd_close`, 而是 VFS 通用
// `vfs_close_internal`; 槽位回收随 `OpenFile` 引用归零自然完成.

use std::fs;
use std::path::Path;

const HANDLE: &str = "src/kernel/functions/fs/handle.rs";
const SFD: &str = "src/kernel/privileged/syscall/signalfd.rs";
const POLL_POLICY: &str = "src/kernel/functions/fs/vfs_poll_policy.rs";
const EPOLL: &str = "src/kernel/privileged/syscall/epoll.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {path}"))
}

/// 截取函数体, 范围: 从 fn 签名到下一个 #[`no_mangle`] fn / pub fn / 下一个大段注释
fn extract_body(src: &str, sig: &str) -> String {
    let start = src.find(sig).unwrap_or_else(|| panic!("找不到签名: {sig}"));
    let after = &src[start..];
    let candidates = [
        "\n#[no_mangle]\npub fn ",
        "\npub fn ",
        "\nfn ",
        "\n// =============", // 段落分隔
    ];
    let end = candidates
        .iter()
        .filter_map(|c| after.find(c).map(|i| i + 1))
        .min()
        .unwrap_or(after.len());
    after[..end].to_string()
}

#[test]
fn test_efd_close_notifies_after_releasing_slot() {
    // TD-04: eventfd 关闭必须先在 OpenFileTable 释放槽位, 再唤醒 epoll 等待者,
    // 让 waiter 醒来后观察到 fd 已关闭.
    let src = read(HANDLE);
    let body = extract_body(&src, "pub fn vfs_close_internal(");
    let close_idx = body
        .find("OPEN_FILE_TABLE.close(handle_id)")
        .expect("TD-04: vfs_close_internal 必须经 OPEN_FILE_TABLE.close 释放槽位");
    let pwake_idx = body
        .find("fd_notify::notify_fd_close")
        .expect("TD-04: vfs_close_internal 必须调用 fd_notify::notify_fd_close");
    assert!(
        close_idx < pwake_idx,
        "TD-04: 唤醒必须在槽位释放之后, 让 waiter 看到 fd 已关闭"
    );
}

#[test]
fn test_sfd_close_pwake_after_drop_lock() {
    // TD-04: SFD close 必须在 drop(table) 之后 epoll_pwake
    let src = read(SFD);
    let body = extract_body(&src, "pub fn sys_signalfd_close(");
    assert!(
        body.contains("epoll_pwake"),
        "TD-04: SFD sys_signalfd_close 必须调用 epoll_pwake, 防止 epoll_wait 睡在已关闭 fd 上:\n{body}"
    );
    let drop_idx = body
        .find("drop(table);")
        .or_else(|| body.find("drop(table)"))
        .or_else(|| body.find("drop( table )"))
        .expect("TD-04: 必须有 drop(table) 显式释放锁");
    let pwake_idx = body
        .find("epoll_pwake")
        .expect("TD-04: epoll_pwake 必须存在");
    assert!(
        drop_idx < pwake_idx,
        "TD-04: epoll_pwake 必须在 drop(table) 之后, 让 waiter 看到槽位已释放"
    );
}

#[test]
fn test_closed_fd_reported_invalid_with_err_hup() {
    // TD-04: 配合检查 — 被唤醒的 waiter 重新扫描时, 已关闭 fd 必须被判为无效,
    // 由 VfsPollPolicy 统一返回 EPOLLERR|EPOLLHUP (eventfd 迁出后不再有本地 slot.used 检查).
    let policy = read(POLL_POLICY);
    assert!(
        policy.contains("fn events_for_invalid_fd"),
        "TD-04: VfsPollPolicy 必须定义无效 fd 事件策略"
    );
    assert!(
        policy.contains("EPOLLERR | EPOLLHUP"),
        "TD-04: 无效 fd 必须返回 EPOLLERR|EPOLLHUP (与唤醒配合触发 waiter 退出)"
    );
    let epoll = read(EPOLL);
    let body = extract_body(&epoll, "fn check_fd_ready(");
    assert!(
        body.contains("VfsPollContext") && body.contains("valid"),
        "TD-04: epoll 扫描必须把 fd 有效性与 file_type 一起交给 VfsPollPolicy"
    );
}
