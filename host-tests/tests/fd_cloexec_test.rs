// FD-CLOEXEC: 验证 close-on-exec 最小链路已接线
// 验收:
//   1. vfs_close_cloexec_fds 先收集索引 (owned Vec) 再逐个关闭
//      (B-9.5: 收集经 with_current_fd_table(|t| t.get_cloexec_fds()), 关闭循环在锁外,
//       避免 vfs_close_internal 内重入同锁自锁死)
//   2. functions::fs 顶层 re-export vfs_close_cloexec_fds (调用方经顶层 API 访问)
//   3. proc_exec_replace 成功路径调用 close_cloexec_fds (经 VfsOps 契约, POSIX close-on-exec)
//   4. memfd_create 在 MFD_CLOEXEC 置位时以 cloexec=true 分配 fd
//   5. open 消费 O_CLOEXEC: VfsOpenFlags 定义 CLOEXEC 位, vfs_open_internal 读取
//      该位并在两处 alloc_fd 传入 cloexec (B-8.3: 标记来源 = open O_CLOEXEC)
//   6. fcntl F_GETFD/F_SETFD 接线 FdTable::is_cloexec/set_cloexec (B-8.2/B-8.3)
//
// 注: 静态契约扫描, 不进内核态.

use std::fs;
use std::path::Path;

const VFS_HANDLE: &str = "src/kernel/functions/fs/handle.rs";
const FILE_HANDLE: &str = "src/kernel/functions/fs/file_handle.rs";
const OPEN_GUARD: &str = "src/kernel/functions/fs/open_guard.rs";
const VFS_MOD: &str = "src/kernel/functions/fs/mod.rs";
const VFS_TYPES: &str = "src/kernel/functions/fs/vfs_types.rs";
const SYS_IO: &str = "src/kernel/functions/fs/io.rs";
const PROC_OPS: &str = "src/kernel/privileged/proc/proc_ops.rs";
const MEMFD: &str = "src/kernel/functions/proc/memfd.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {path}"))
}

#[test]
fn test_vfs_close_cloexec_fds_collects_before_closing() {
    let src = read(VFS_HANDLE);
    let body_start = src
        .find("pub fn vfs_close_cloexec_fds")
        .expect("vfs_close_cloexec_fds 必须存在");
    let after = &src[body_start..];
    // 函数体到下一个顶层 pub 项 (下一个 pub fn / pub extern)
    let next_fn = after[1..].find("\npub ").map_or(after.len(), |i| 1 + i);
    let body = &after[..next_fn];
    // 必须先收集成 owned Vec 再关闭 — 若在持有 fd_table 锁的迭代中直接 close,
    // vfs_close_internal 会再次获取同一把锁导致自锁死.
    assert!(
        body.contains("Vec<u32>"),
        "vfs_close_cloexec_fds 必须先收集 fd 索引成 Vec:\n{body}"
    );
    assert!(
        body.contains("vfs_close_internal(fd)"),
        "收集后必须逐个调用 vfs_close_internal"
    );
    // 收集必须以独立语句 (.collect();) 收尾, 关闭循环在其后 (锁已释放)
    let collect_end = body
        .find(".collect();")
        .expect("收集必须以 .collect(); 收尾为独立语句");
    let close_idx = body.find("vfs_close_internal(fd)").expect("关闭调用");
    assert!(
        close_idx > collect_end,
        "vfs_close_internal 调用必须在 fd_table 锁释放之后 (避免嵌套加锁)"
    );
}

#[test]
fn test_vfs_close_cloexec_fds_re_exported_at_top_level() {
    let src = read(VFS_MOD);
    assert!(
        src.contains("vfs_close_cloexec_fds"),
        "functions::fs 顶层必须 re-export vfs_close_cloexec_fds (F2: 走顶层 API)"
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
        body.contains("close_cloexec_fds()"),
        "proc_exec_replace 成功路径必须调用 close_cloexec_fds (经 VfsOps 契约, POSIX close-on-exec)"
    );
}

#[test]
fn test_memfd_create_applies_mfd_cloexec() {
    let src = read(MEMFD);
    assert!(src.contains("MFD_CLOEXEC"), "memfd 必须定义 MFD_CLOEXEC");
    assert!(
        src.contains("alloc_fd(handle_id, flags & MFD_CLOEXEC != 0)"),
        "memfd_create 在 MFD_CLOEXEC 置位时必须 cloexec=true 分配 fd (B-9.5: 经 FdTable::alloc_fd)"
    );
}

#[test]
fn test_open_flags_define_cloexec_bit() {
    let src = read(VFS_TYPES);
    let start = src
        .find("pub struct VfsOpenFlags")
        .expect("VfsOpenFlags 必须存在");
    let after = &src[start..];
    let end = after.find("}\n}").map_or(after.len(), |i| i + 2);
    let body = &after[..end];
    assert!(
        body.contains("CLOEXEC"),
        "VfsOpenFlags 必须定义 CLOEXEC 位 (作为 ABI 旗标所有者):\n{body}"
    );
}

#[test]
fn test_vfs_open_internal_consumes_cloexec() {
    let src = read(VFS_HANDLE);
    let body_start = src
        .find("pub extern \"C\" fn vfs_open_internal")
        .expect("vfs_open_internal 必须存在");
    let after = &src[body_start..];
    let next_fn = after[1..].find("\npub ").map_or(after.len(), |i| 1 + i);
    let body = &after[..next_fn];
    assert!(
        body.contains("VfsOpenFlags::CLOEXEC"),
        "vfs_open_internal 必须读取 VfsOpenFlags::CLOEXEC (O_CLOEXEC 直通 ABI 位):\n{body}"
    );
    // T-1: 两处分支 (fs_open / CREAT) 均须把 cloexec 交给唯一 publish 出口
    // (`publish_opened_inode`), 由后者在 alloc_fd 时传入.
    assert_eq!(
        body.matches("publish_opened_inode(").count(),
        2,
        "vfs_open_internal 两处分支均须经唯一 publish 出口:\n{body}"
    );
    let helper_start = src
        .find("fn publish_opened_inode")
        .expect("publish_opened_inode 必须存在");
    let helper_after = &src[helper_start..];
    let helper_end = helper_after[1..]
        .find("\npub ")
        .map_or(helper_after.len(), |i| 1 + i);
    let helper = &helper_after[..helper_end];
    assert_eq!(
        helper.matches("alloc_fd(handle_id, cloexec)").count(),
        1,
        "唯一 publish 出口分配 fd 时必须传 cloexec:\n{helper}"
    );
    assert!(
        !body.contains("alloc_fd(handle_id, false)"),
        "vfs_open_internal 不得残留 alloc_fd(handle_id, false)"
    );
    assert!(
        !helper.contains("alloc_fd(handle_id, false)"),
        "publish_opened_inode 不得硬编码 alloc_fd(handle_id, false)"
    );
}

// T-1: 对象获取的唯一判定出口 — 能发布文件对象的路径只有 `publish_opened_inode`,
// 判定只有 `authorize_open`. 任一新增绕过 (再写一个 OpenFile::new 或再调一次能力判定)
// 都会让本锁转红.
#[test]
fn test_object_acquisition_has_single_authorize_exit() {
    let handle = read(VFS_HANDLE);
    let file_handle = read(FILE_HANDLE);
    let guard = read(OPEN_GUARD);
    assert_eq!(
        handle.matches("OpenFile::new(").count(),
        1,
        "OpenFile::new 只允许出现在唯一 publish 出口内"
    );
    assert_eq!(
        file_handle.matches("OpenFile::new(").count(),
        0,
        "open_by_handle_at 不得自带发布逻辑 (须经 publish_opened_inode)"
    );
    assert!(
        handle.contains("authorize_open(pwm, source)"),
        "publish_opened_inode 必须先经 authorize_open"
    );
    assert_eq!(
        file_handle.matches("pwm_has_capability").count(),
        0,
        "open_by_handle_at 不得自带能力判定 (判定归 authorize_open)"
    );
    assert!(
        guard.contains("pwm_has_capability"),
        "能力判定实现须落在 open_guard.rs"
    );
}

#[test]
fn test_sys_fcntl_wires_fd_cloexec_flag() {
    let src = read(SYS_IO);
    let body_start = src
        .find("pub fn fcntl_syscall")
        .expect("fcntl_syscall 必须存在");
    let after = &src[body_start..];
    let next_fn = after[1..].find("\npub ").map_or(after.len(), |i| 1 + i);
    let body = &after[..next_fn];
    assert!(
        body.contains("is_cloexec"),
        "sys_fcntl F_GETFD 必须接线 FdTable::is_cloexec:\n{body}"
    );
    assert!(
        body.contains("set_cloexec"),
        "sys_fcntl F_SETFD 必须接线 FdTable::set_cloexec:\n{body}"
    );
}
