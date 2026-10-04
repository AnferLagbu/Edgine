// TD-04: 验证 EFD/SFD close 路径在锁释放后 epoll_pwake, 防止 epoll_wait 睡在已关闭 fd 上
//
// 验收:
//   1. EFD sys_eventfd_close 在释放 EFD_TABLE 锁之后调用 epoll_pwake
//   2. SFD sys_signalfd_close 在释放 SFD_TABLE 锁之后调用 epoll_pwake
//   3. epoll_pwake 调用在 drop(table) 之后, 保证 waiter 唤醒后看到 slot.used=false
//
// 注: 静态契约扫描, 不进内核态.

use std::fs;
use std::path::Path;

const EFD: &str = "src/kernel/privileged/syscall/eventfd.rs";
const SFD: &str = "src/kernel/privileged/syscall/signalfd.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", path))
}

/// 截取函数体, 范围: 从 fn 签名到下一个 #[no_mangle] fn / pub fn / 下一个大段注释
fn extract_body(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("找不到签名: {}", sig));
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
fn test_efd_close_pwake_after_drop_lock() {
    // TD-04: EFD close 必须在 drop(table) 之后 epoll_pwake, 顺序敏感
    let src = read(EFD);
    let body = extract_body(&src, "pub fn sys_eventfd_close(");
    assert!(
        body.contains("epoll_pwake"),
        "TD-04: EFD sys_eventfd_close 必须调用 epoll_pwake, 防止 epoll_wait 睡在已关闭 fd 上:\n{}",
        body
    );
    // drop(table) 必须在 epoll_pwake 之前
    let drop_idx = body
        .find("drop(table);")
        .or_else(|| body.find("drop(table)"))
        .or_else(|| body.find("drop( table )"))
        .expect("TD-04: 必须有 drop(table) 显式释放锁, 顺序敏感");
    let pwake_idx = body
        .find("epoll_pwake")
        .expect("TD-04: epoll_pwake 必须存在");
    assert!(
        drop_idx < pwake_idx,
        "TD-04: epoll_pwake 必须在 drop(table) 之后, 让 waiter 看到 slot.used=false"
    );
}

#[test]
fn test_sfd_close_pwake_after_drop_lock() {
    // TD-04: SFD close 必须在 drop(table) 之后 epoll_pwake
    let src = read(SFD);
    let body = extract_body(&src, "pub fn sys_signalfd_close(");
    assert!(
        body.contains("epoll_pwake"),
        "TD-04: SFD sys_signalfd_close 必须调用 epoll_pwake, 防止 epoll_wait 睡在已关闭 fd 上:\n{}",
        body
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
        "TD-04: epoll_pwake 必须在 drop(table) 之后, 让 waiter 看到 slot.used=false"
    );
}

#[test]
fn test_efd_poll_returns_epollerr_on_freed_slot() {
    // TD-04: 配合检查 — 当 slot.used=false 时, eventfd_poll_events 必须返回 EPOLLERR,
    // 这样被 epoll_pwake 唤醒的 waiter 能识别"fd 已关闭" 状态.
    let src = read(EFD);
    let body = extract_body(&src, "pub fn eventfd_poll_events(");
    assert!(
        body.contains("if !slot.used"),
        "TD-04: eventfd_poll_events 必须检查 slot.used"
    );
    assert!(
        body.contains("EPOLLERR"),
        "TD-04: 关闭态必须返回 EPOLLERR (与 epoll_pwake 配合触发 waiter 退出)"
    );
}
