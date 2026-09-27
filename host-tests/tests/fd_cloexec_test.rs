// FD-CLOEXEC: 验证 close-on-exec 最小链路已接线
// 验收:
//   1. vfs_close_cloexec_fds 先收集索引再逐个关闭 (避免持 fd_table 锁递归自锁死)
//   2. framework::fs 顶层 re-export vfs_close_cloexec_fds (services 可经顶层 API 调用)
//   3. proc_exec_replace 成功路径调用 vfs_close_cloexec_fds (POSIX close-on-exec)
//   4. memfd_create 在 MFD_CLOEXEC 置位时调用 set_fd_cloexec
//
// 注: 静态契约扫描, 不进内核态.

use std::fs;
use std::path::Path;

const VFS_HANDLE: &str = "src/kernel/framework/fs/vfs/handle.rs";
const VFS_MOD: &str = "src/kernel/framework/fs/vfs/mod.rs";
const PROC_OPS: &str = "src/kernel/framework/proc/proc_ops.rs";
const MEMFD: &str = "src/kernel/services/proc/memfd.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", path))
}

#[test]
fn test_vfs_close_cloexec_fds_collects_before_closing() {
    let src = read(VFS_HANDLE);
    let body_start = src
        .find("pub fn vfs_close_cloexec_fds")
        .expect("vfs_close_cloexec_fds 必须存在");
    let after = &src[body_start..];
    // 函数体到下一个顶层 pub 项 (下一个 pub fn / pub extern)
    let next_fn = after[1..]
        .find("\npub ")
        .map(|i| 1 + i)
        .unwrap_or(after.len());
    let body = &after[..next_fn];
    // 必须先收集成 Vec 再关闭 — 若在持有 fd_table 锁的迭代中直接 close,
    // vfs_close_internal 会再次获取同一把锁导致自锁死.
    assert!(
        body.contains("Vec<u32>"),
        "vfs_close_cloexec_fds 必须先收集 fd 索引成 Vec:\n{body}"
    );
    assert!(
        body.contains("vfs_close_internal(fd)"),
        "收集后必须逐个调用 vfs_close_internal"
    );
    // 锁作用域必须在收集处结束 (以语句块形式), 关闭循环在锁外
    let collect_block_end = body.find("};").expect("收集块以 }; 结束");
    let close_idx = body.find("vfs_close_internal(fd)").expect("关闭调用");
    assert!(
        close_idx > collect_block_end,
        "vfs_close_internal 调用必须在 fd_table 锁释放之后 (避免嵌套加锁)"
    );
}

#[test]
fn test_vfs_close_cloexec_fds_re_exported_at_top_level() {
    let src = read(VFS_MOD);
    assert!(
        src.contains("vfs_close_cloexec_fds"),
        "framework::fs 顶层必须 re-export vfs_close_cloexec_fds (F2: services 走顶层 API)"
    );
}

#[test]
fn test_proc_exec_replace_closes_cloexec_fds() {
    let src = read(PROC_OPS);
    let body_start = src
        .find("pub extern \"C\" fn proc_exec_replace")
        .expect("proc_exec_replace 必须存在");
    let body = &src[body_start..];
    assert!(
        body.contains("vfs_close_cloexec_fds()"),
        "proc_exec_replace 成功路径必须调用 vfs_close_cloexec_fds (POSIX close-on-exec)"
    );
}

#[test]
fn test_memfd_create_applies_mfd_cloexec() {
    let src = read(MEMFD);
    assert!(
        src.contains("MFD_CLOEXEC"),
        "memfd 必须定义 MFD_CLOEXEC"
    );
    assert!(
        src.contains("set_fd_cloexec(fd as usize, true)"),
        "memfd_create 在 MFD_CLOEXEC 置位时必须调用 set_fd_cloexec(fd, true)"
    );
}
