use super::check;
use crate::framework::fs::vfs::types::{FsType, VFS_MAX_PATH};
use crate::framework::fs::vfs::vfs::VfsManager;
use crate::framework::tests::{TestResult, runner};
use crate::register_tests_inner;

// UT-07 (2026-09-26): vfs::types 注册子块已收敛 —
// 纯类型编码/映射断言以 framework/fs/vfs/types.rs 的 #[cfg(test)] 为唯一归属.

fn test_vfs_mount_unmount() -> TestResult {
    let mgr = VfsManager::new();
    let result = mgr.mount("/", "ramfs");
    check!(result.is_ok(), "mount / should succeed");

    let dup = mgr.mount("/", "ramfs");
    check!(dup.is_err(), "duplicate mount should fail");

    let found = mgr.find_mount("/");
    check!(found.is_some(), "should find / mount");

    let unmount_result = mgr.unmount("/");
    check!(unmount_result.is_ok(), "unmount / should succeed");

    let not_found = mgr.find_mount("/");
    check!(not_found.is_none(), "should not find / after unmount");
    TestResult::Pass
}

fn test_vfs_resolve_mount() -> TestResult {
    let mgr = VfsManager::new();
    let _ = mgr.mount("/", "ramfs");
    let _ = mgr.mount("/home", "nestfs");

    let root = mgr.resolve_mount("/");
    check!(root.is_some(), "should resolve /");
    let (_idx, fs_type) = root.unwrap();
    check!(fs_type == FsType::RamFs, "/ should be RamFs");

    let home = mgr.resolve_mount("/home/user/file.txt");
    check!(home.is_some(), "should resolve /home/user/file.txt");
    let (_, home_fs) = home.unwrap();
    check!(home_fs == FsType::NestFs, "/home should be NestFs");

    let rel = mgr.get_relative_path("/home/user/file.txt", home.unwrap().0);
    check!(rel == "user/file.txt", "relative path mismatch");
    TestResult::Pass
}

/// per-process fd 表 (B-9.5): first-fit 分配 + close 后槽位复用 + dup 共享 handle
fn test_fd_table_alloc_close() -> TestResult {
    use crate::framework::proc::fd_table::FdTable;

    let table = FdTable::new();
    let fd1 = table.alloc_fd(7, false);
    check!(fd1.is_some(), "first alloc should succeed");
    let fd2 = table.alloc_fd(8, false);
    check!(fd2.is_some(), "second alloc should succeed");
    check!(fd1.unwrap() != fd2.unwrap(), "fds should be different");
    check!(
        table.get_handle_id(fd1.unwrap()) == Some(7)
            && table.get_handle_id(fd2.unwrap()) == Some(8),
        "本地 fd 应映射到各自 handle_id"
    );

    // close 后槽位空闲, 下一次分配复用最小空闲槽位 (first-fit)
    check!(
        table.close_fd(fd1.unwrap()) == Some(7),
        "close 应返回被关闭的 handle"
    );
    check!(
        table.get_handle_id(fd1.unwrap()).is_none(),
        "已关闭 fd 应无映射"
    );
    let fd3 = table.alloc_fd(9, false);
    check!(fd3 == fd1, "first-fit 应复用刚释放的槽位");

    // dup 语义: 两个本地 fd 共享同一 handle (offset 由 OpenFile 承载)
    let dup_fd = table.alloc_fd(9, false);
    check!(dup_fd.is_some(), "dup slot alloc 应成功");
    check!(
        table.get_handle_id(dup_fd.unwrap()) == table.get_handle_id(fd3.unwrap()),
        "dup 出的两个 fd 应共享同一 handle"
    );
    TestResult::Pass
}

fn test_vfs_cwd() -> TestResult {
    let mgr = VfsManager::new();
    mgr.set_cwd("/home/user");
    let cwd = mgr.get_cwd();
    check!(cwd == "/home/user", "cwd mismatch");
    TestResult::Pass
}

fn test_vfs_snapshot_restore() -> TestResult {
    let mgr = VfsManager::new();
    let _ = mgr.mount("/", "ramfs");
    mgr.capture_snapshot();

    let _ = mgr.unmount("/");
    check!(
        mgr.find_mount("/").is_none(),
        "mount should be gone after unmount"
    );

    mgr.restore_from_snapshot();
    let found = mgr.find_mount("/");
    check!(
        found.is_some(),
        "mount should be restored after snapshot restore"
    );
    TestResult::Pass
}

// ============================================================================
// DECISION-K 项 6 回归测试 (第二十四批): services::fs::init 注册激活
//
// 回归背景: services::fs::init 此前全库无调用者, 第二十三批 ramfs 回迁引入
// 的 make_ramfs_inode 钩子恒命中 FallbackFsBackend → Err(NotInitialized),
// ramfs open/create 生产路径被回退策略拦截.
// ============================================================================

fn test_fs_backend_registered_make_inode() -> TestResult {
    // 激活注册 (幂等: 重复注册 Err 被忽略)
    crate::services::fs::init();
    // 钩子必须返回真实 Inode — FallbackFsBackend 恒 Err, 本断言锁定回归
    let result =
        crate::framework::fs::vfs::backend_trait::current_fs_backend().make_ramfs_inode(0, 0, 0);
    check!(
        result.is_ok(),
        "make_ramfs_inode 命中回退策略 — services::fs::init 未生效"
    );
    TestResult::Pass
}

fn test_ramfs_fs_open_via_backend_hook() -> TestResult {
    use crate::framework::fs::FileSystem;
    use crate::framework::fs::ramfs::{RAMFS_DATA, RamFsData};

    crate::services::fs::init();
    // 建根目录 (幂等): RAMFS_DATA 初始为空, resolve_path("/") 需先 mount
    crate::framework::fs::ramfs::init();

    // 在 RamFS 根目录建文件 (锁内操作, 作用域结束释放锁)
    let created = {
        let mut ramfs = RAMFS_DATA.lock();
        ramfs.create_file("/", "backend_reg_t", 0)
    };
    check!(created.is_some(), "create_file 应成功");
    let Some(_node_id) = created else {
        return TestResult::Fail("create_file 失败");
    };

    // SAFETY: 全局 static RAMFS_DATA 拥有 RamFsData, 裸指针提升后生命周期为
    // 'static (mount.rs 同款手法); fs_open 内部自行加锁, 此处不持锁调用, 无死锁.
    // 注意: 守卫必须收窄到块内 — rustc 1.98 nightly (RFC 3606 临时生命周期
    // 延长) 下 `let p = &raw const *RAMFS_DATA.lock()` 会使守卫存活至绑定
    // 作用域结束, fs_open 内部重入 lock() 将同线程自旋死锁 (cast 形式无此
    // 延长, 块作用域强制语句末释放).
    let ramfs_ptr: *const RamFsData = {
        let guard = RAMFS_DATA.lock();
        &raw const *guard
    };
    let fs: &'static RamFsData = unsafe { &*ramfs_ptr };

    // fs_open → make_inode 钩子 → services RamFsInode (回归路径本体)
    let opened = fs.fs_open("/backend_reg_t", 0, 0);
    check!(
        opened.is_ok(),
        "fs_open 应经 backend 钩子返回 Inode (命中 Fallback 即回归)"
    );
    TestResult::Pass
}

/// B-9.5: fd 分配已下沉到 per-process `FdTable` (`with_current_fd_table`),
/// 依赖当前进程上下文。host 侧共享测试集无当前进程 ⇒ open 恒失败。
///
/// 本辅助临时安装一个当前进程 (含独立 FdTable), 执行闭包后无条件拆除现场
/// (恢复调度器当前进程 → 摘表 → 释放描述符 → 回收 pid)。
/// 安装/拆除模式参照 [test_proc.rs] 的 `test_fork_cow_failure_rolls_back`。
fn with_temp_process<F: FnOnce() -> TestResult>(name: &str, f: F) -> TestResult {
    use crate::framework::proc::raw;
    use crate::framework::proc::{PROCESS_TABLE, SCHEDULER};

    let Some(pid) = PROCESS_TABLE.allocate_pid() else {
        return TestResult::Fail("临时进程: 无空闲 pid");
    };
    let proc_ptr = raw::alloc_process(pid, name, None);
    if !PROCESS_TABLE.insert(proc_ptr) {
        raw::drop_boxed_process(proc_ptr);
        PROCESS_TABLE.free_pid(pid);
        return TestResult::Fail("临时进程: 插入进程表失败");
    }

    let prev_current = SCHEDULER.current();
    SCHEDULER.set_current(pid);

    let result = f();

    SCHEDULER.set_current(prev_current.unwrap_or(0));
    let _ = PROCESS_TABLE.remove(pid);
    raw::drop_boxed_process(proc_ptr);
    PROCESS_TABLE.free_pid(pid);
    result
}

/// T5 甲批 C-1 接线证据: 真实 open 路径 (`open_syscall` → `vfs_open` →
/// `vfs_open_internal`) 必须把 fd 表元数据写全, 否则
/// (flock 的 ino / mmap-by-fd 的 `fd_to_inode_id`) 恒得 0,
/// 挂载点反查失败。
///
/// B-9.5: 元数据源已由全局 `VfsManager.fd_table` 改源为 per-process fd 表
/// (fd → `OpenFileTable` handle) + `OpenFile` 自身元数据。
fn test_open_populates_fd_metadata() -> TestResult {
    use crate::framework::fs::ramfs::{RAMFS_DATA, init as ramfs_init};
    use crate::framework::fs::{OPEN_FILE_TABLE, api, vfs_get_fd_handle};

    crate::services::fs::init();
    ramfs_init();
    // 必须走真实挂载入口 (挂 trait object): `VFS_MANAGER.mount` 只登记 fs_type,
    // `resolve_mount_fs` 的 `fs` 仍为 None, vfs_open_internal 直接返回 NotSupported.
    // boot / host 均已挂载 "/" 时返回负值, 忽略即可.
    let _ = api::vfs_mount_safe("/", "ramfs");

    with_temp_process("fd-meta-t", || {
        let created = {
            let mut ramfs = RAMFS_DATA.lock();
            ramfs.create_file("/", "fd_meta_t", 0)
        };
        let Some(node_id) = created else {
            return TestResult::Fail("create_file 失败");
        };
        check!(node_id != 0, "inode 编号不应为 0 (0 是未填充哨兵)");

        let fd = api::vfs_open_safe("/fd_meta_t", 0, 0);
        check!(fd >= 0, "open /fd_meta_t 应成功");

        let Some(handle_id) = vfs_get_fd_handle(fd as usize) else {
            return TestResult::Fail("per-process fd 表应含该 fd 条目");
        };
        let Some((fd_node_id, fd_mount_idx)) =
            OPEN_FILE_TABLE.with_file(handle_id, |of| (of.inode_id(), of.mount_idx()))
        else {
            return TestResult::Fail("OpenFile 表应含该 handle");
        };
        check!(
            fd_node_id == node_id,
            "fd 元数据 node_id 应为真实 inode (元数据未接线时恒为 0)"
        );
        check!(
            usize::try_from(fd_mount_idx).is_ok(),
            "fd 元数据应携带挂载点索引, 可反查 (mmap-by-fd 依赖)"
        );

        check!(api::vfs_close_safe(fd as u32) == 0, "close 应成功");
        TestResult::Pass
    })
}

/// T5 乙批（C-1 证据补齐）: `set_fd` 接线的**下游链路**验证 —— 证明修的是下游
/// 行为, 而非只把元数据填进了表。
///
/// 下游消费者: `services::mm::mmap::fd_to_inode_id` 是 `mmap_syscall` 文件映射
/// 的**唯一** inode 来源, 取 0 时直接返回 `EBADF` (mmap.rs:134-137);
/// `fd_to_mount_idx` 为其挂载点来源。二者均经 per-process fd 表 (fd →
/// `OpenFileTable` handle) 读 `OpenFile` 元数据。
fn test_fd_to_inode_id_downstream() -> TestResult {
    use crate::framework::fs::ramfs::{RAMFS_DATA, init as ramfs_init};
    use crate::framework::fs::{api, vfs_get_fd_handle};
    use crate::services::mm::mmap::{fd_to_inode_id, fd_to_mount_idx};

    crate::services::fs::init();
    ramfs_init();
    // 真实挂载入口 (挂 trait object); 已挂载时返回负值, 忽略.
    let _ = api::vfs_mount_safe("/", "ramfs");

    with_temp_process("fd-down-t", || {
        let created = {
            let mut ramfs = RAMFS_DATA.lock();
            ramfs.create_file("/", "fd_down_t", 0)
        };
        let Some(node_id) = created else {
            return TestResult::Fail("create_file 失败");
        };
        check!(node_id != 0, "inode 编号不应为 0");

        let fd = api::vfs_open_safe("/fd_down_t", 0, 0);
        check!(fd >= 0, "open /fd_down_t 应成功");

        // 下游消费者 1: mmap 文件映射的 inode 来源 — 未接线时恒 0 ⇒ mmap 恒 EBADF
        check!(
            fd_to_inode_id(fd) == node_id,
            "fd_to_inode_id 应为真实 inode (未接线时恒 0 ⇒ mmap 文件映射恒 EBADF)"
        );
        // 下游消费者 2: mmap 的挂载点反查 — 未接线时 path 为空 ⇒ None
        check!(
            fd_to_mount_idx(fd).is_some(),
            "fd_to_mount_idx 应可反查挂载点 (未接线时为 None)"
        );
        check!(
            vfs_get_fd_handle(fd as usize).is_some(),
            "per-process fd 表条目应存在"
        );

        check!(api::vfs_close_safe(fd as u32) == 0, "close 应成功");
        TestResult::Pass
    })
}

fn test_nestfs_fs_registered() -> TestResult {
    crate::services::fs::init();
    let Some(fs) = crate::framework::fs::vfs::backend_trait::nestfs_fs() else {
        return TestResult::Fail("nestfs_fs() 未注册 — services::fs::init 未生效");
    };
    check!(fs.name() == "nestfs", "nestfs name mismatch");
    // 注: fs_format 行为不在单测覆盖 (内存模式调 format_drive 有底层 IO 副作用),
    // 语义等价性由 fsformat 路径代码搬移保证, QEMU boot 覆盖挂载分发链路
    TestResult::Pass
}

// ============================================================================
// T1 G5: 多路复用 (inotify_init / ppoll / epoll_pwait)
// ============================================================================

/// 越界用户指针 (`>= USER_ADDR_MAX`): 被 `check_user_buf` 拒绝而非解引用
const BAD_USER_PTR: u64 = 0x8000_0000_0000_0000;

/// `inotify_init` 遗留接口等价 `inotify_init1(0)`
fn test_inotify_init_legacy() -> TestResult {
    use crate::framework::fs::vfs::inotify::{
        IN_NONBLOCK, inotify_release, is_inotify_fd, sys_inotify_init1,
    };

    let fd = sys_inotify_init1(0);
    check!(fd > 0, "inotify_init1(0) 应返回有效 fd");
    check!(is_inotify_fd(fd as i32), "fd 应为 inotify fd");
    inotify_release(fd);

    // flags 保留位非零 → EINVAL
    check!(
        sys_inotify_init1(IN_NONBLOCK | 0x10) == -22,
        "非法 flags 应返回 EINVAL"
    );
    // IN_NONBLOCK 单独合法
    let fd2 = sys_inotify_init1(IN_NONBLOCK);
    check!(fd2 > 0, "IN_NONBLOCK 应为合法 flags");
    inotify_release(fd2);
    TestResult::Pass
}

/// 临时信号屏蔽字替换/恢复 (`ppoll` / `epoll_pwait` 共用策略)
fn test_temporary_sigmask_swap() -> TestResult {
    use crate::framework::proc::{
        get_blocked_mask, process_get_current_pid, sanitize_blocked_mask, set_blocked_mask,
    };
    use crate::services::proc::signal::with_temporary_sigmask;

    // sigmask == NULL: 直接执行, sigsetsize 不参与校验
    check!(
        with_temporary_sigmask(0, 99, || Ok(7)) == Ok(7),
        "sigmask=NULL 应透传闭包结果"
    );

    // 不可屏蔽信号位被剔除
    check!(
        sanitize_blocked_mask(u64::MAX) == !((1u64 << 9) | (1u64 << 19)),
        "SIGKILL/SIGSTOP 位应被剔除"
    );

    // 有当前进程时: 替换 → 闭包内可见新掩码 → 返回后恢复
    let pid = process_get_current_pid();
    if pid != 0 {
        let original = get_blocked_mask(pid);
        // sigmask 指针越界 → EFAULT (此处仅验证错误路径不污染原掩码)
        let err = with_temporary_sigmask(BAD_USER_PTR, 8, || Ok(9));
        check!(err.is_err(), "非法 sigmask 指针应返回错误");
        check!(get_blocked_mask(pid) == original, "错误路径不应改变屏蔽字");
    }
    // sigsetsize != 8 → EINVAL (sigmask 非 NULL 时校验)
    match with_temporary_sigmask(BAD_USER_PTR, 4, || Ok(9)) {
        Err(crate::framework::syscall::Errno::EINVAL) => {}
        _ => return TestResult::Fail("sigsetsize != 8 应返回 EINVAL"),
    }
    // 恢复现场 (测试自身不留残余屏蔽字)
    if pid != 0 {
        set_blocked_mask(pid, 0);
    }
    TestResult::Pass
}

/// `ppoll` 参数校验 (nfds == 0 短路 / sigsetsize 校验)
fn test_ppoll_arg_validation() -> TestResult {
    use crate::services::fs::file_ops::ppoll_syscall;

    // nfds == 0 → 0 (无 fd 可扫)
    check!(ppoll_syscall(0, 0, 0, 0, 0) == 0, "nfds=0 应返回 0");
    // 非法 sigsetsize → EINVAL
    check!(
        ppoll_syscall(0, 0, 0, BAD_USER_PTR, 4) == -22,
        "sigsetsize != 8 应返回 EINVAL"
    );
    // 越界 timespec 指针 → EFAULT
    check!(
        ppoll_syscall(0, 0, BAD_USER_PTR, 0, 0) == -14,
        "非法 timespec 指针应返回 EFAULT"
    );
    TestResult::Pass
}

/// `epoll_pwait` 参数校验与 `epoll_wait` 委托 (sigmask == NULL)
fn test_epoll_pwait_validation() -> TestResult {
    use crate::services::sync::epoll::epoll_pwait_syscall;

    // maxevents <= 0 → EINVAL (委托 epoll_wait 校验)
    match epoll_pwait_syscall(-1, 0x1000, 0, 0, 0, 0) {
        Err(crate::framework::syscall::Errno::EINVAL) => {}
        _ => return TestResult::Fail("maxevents=0 应返回 EINVAL"),
    }
    // maxevents 合法 + epfd 非法 → EINVAL (framework 层 epfd <= 0)
    match epoll_pwait_syscall(-1, 0x1000, 1, 0, 0, 0) {
        Err(crate::framework::syscall::Errno::EINVAL) => {}
        _ => return TestResult::Fail("epfd<0 应返回 EINVAL"),
    }
    // sigmask 越界指针 → 错误先于等待返回
    check!(
        epoll_pwait_syscall(-1, 0x1000, 1, 0, BAD_USER_PTR, 8).is_err(),
        "非法 sigmask 指针应返回错误"
    );
    TestResult::Pass
}

// ============================================================================
// T1 G7: VFS 根前缀归一化 (chroot / pivot_root 机制)
// ============================================================================

/// 局部实例归一化比对 (栈上缓冲, 零分配)
fn resolve_is(mgr: &VfsManager, path: &str, expected: &str) -> bool {
    let mut buf = [0u8; VFS_MAX_PATH];
    mgr.resolve_user_path(path, &mut buf) == Some(expected)
}

/// 默认根: 绝对路径逐字节等价 (无点组件时与改造前一致)
fn test_resolve_default_root() -> TestResult {
    let mgr = VfsManager::new();
    check!(mgr.get_root() == "/", "默认根应为 /");
    check!(
        resolve_is(&mgr, "/home/user/file.txt", "/home/user/file.txt"),
        "默认根下绝对路径应原样返回"
    );
    TestResult::Pass
}

/// `.` / `..` 组件归一化 + 视图根内钳制 (逃逸防护)
fn test_resolve_dot_components() -> TestResult {
    let mgr = VfsManager::new();
    check!(resolve_is(&mgr, "/a/b/../c", "/a/c"), ".. 应上溯一级");
    check!(
        resolve_is(&mgr, "/a/./b//c/", "/a/b/c"),
        ". 与空组件应被忽略, 尾随 / 应去除"
    );
    check!(
        resolve_is(&mgr, "/../../x", "/x"),
        ".. 应钳制在视图根内 (不可逃逸)"
    );
    check!(resolve_is(&mgr, "/..", "/"), "根之上仍为根");
    check!(resolve_is(&mgr, "/", "/"), "根路径应归一化为 /");
    TestResult::Pass
}

/// 相对路径以视图 cwd 为基准
fn test_resolve_relative_to_cwd() -> TestResult {
    let mgr = VfsManager::new();
    mgr.set_cwd("/home/user");
    check!(
        resolve_is(&mgr, "file.txt", "/home/user/file.txt"),
        "相对路径应拼接 cwd"
    );
    check!(
        resolve_is(&mgr, "../other", "/home/other"),
        "相对路径 .. 应上溯 cwd 一级"
    );

    // chdir 经 resolve_view_path 存储 (视图路径, 无根前缀)
    let mut buf = [0u8; VFS_MAX_PATH];
    let view = mgr.resolve_view_path("/opt/./srv/../app", &mut buf);
    check!(
        view == Some("/opt/app"),
        "resolve_view_path 应归一化视图路径"
    );
    TestResult::Pass
}

/// 根前缀: 视图路径 → 真实路径拼接 + 切根后 cwd 重置
fn test_resolve_with_root_prefix() -> TestResult {
    let mgr = VfsManager::new();
    check!(resolve_is(&mgr, "/tmp", "/tmp"), "前置: 默认根下路径不变");

    mgr.set_root("/jail");
    check!(mgr.get_root() == "/jail", "切根后 root 应为 /jail");
    check!(mgr.get_cwd() == "/", "切根后 cwd 应重置为 /");
    check!(
        resolve_is(&mgr, "/etc/passwd", "/jail/etc/passwd"),
        "视图路径应拼接根前缀"
    );
    check!(
        resolve_is(&mgr, "/../..", "/jail"),
        "根前缀之外的 .. 不应逃逸 (钳制在视图根)"
    );
    check!(resolve_is(&mgr, "/", "/jail"), "视图根应映射为根前缀自身");

    // 相对路径基于视图 cwd (cwd 为视图路径, 与根前缀无关)
    mgr.set_cwd("/sub");
    check!(
        resolve_is(&mgr, "f", "/jail/sub/f"),
        "切根后相对路径应为 根前缀 + 视图路径"
    );

    // 快照往返携带 root (barrier 回滚语义)
    mgr.capture_snapshot();
    mgr.set_root("/other");
    check!(mgr.get_root() == "/other", "第二次切根应生效");
    mgr.restore_from_snapshot();
    check!(mgr.get_root() == "/jail", "快照恢复应还原根前缀");
    TestResult::Pass
}

pub fn register_vfs_tests() {
    let r = runner();
    register_tests_inner! { r:
        "vfs::mgr": {
            "mount_unmount": test_vfs_mount_unmount,
            "resolve_mount": test_vfs_resolve_mount,
            "cwd": test_vfs_cwd,
            "snapshot_restore": test_vfs_snapshot_restore,
            "resolve_default_root": test_resolve_default_root,
            "resolve_dot_components": test_resolve_dot_components,
            "resolve_relative_to_cwd": test_resolve_relative_to_cwd,
            "resolve_with_root_prefix": test_resolve_with_root_prefix,
            "fd_table_alloc_close": test_fd_table_alloc_close,
        },
        "vfs::backend": {
            "fs_backend_registered_make_inode": test_fs_backend_registered_make_inode,
            "ramfs_fs_open_via_backend_hook": test_ramfs_fs_open_via_backend_hook,
            "open_populates_fd_metadata": test_open_populates_fd_metadata,
            "fd_to_inode_id_downstream": test_fd_to_inode_id_downstream,
            "nestfs_fs_registered": test_nestfs_fs_registered,
        },
        "fs::multiplex": {
            "inotify_init_legacy": test_inotify_init_legacy,
            "temporary_sigmask_swap": test_temporary_sigmask_swap,
            "ppoll_arg_validation": test_ppoll_arg_validation,
            "epoll_pwait_validation": test_epoll_pwait_validation,
        },
    }
}
