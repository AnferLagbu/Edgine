//! D5-f: `pivot_root` 切根语义回归锁 (Linux 式 `switch_root` 的内核面).
//!
//! 口径来源: `docs/plan/userspace-decoupling.md` D5-e/D5-f (用户裁定 B: 引导根恒为
//! ramfs/initramfs, EGFS 只挂非根挂载点 `/disk`, 由用户态 init 切根进磁盘根).
//!
//! 改造前 `pivot_root_syscall` 只切根前缀, 既不要求 `new_root` 是挂载点, 也不摘除
//! 旧根挂载 ⇒ 旧根经绝对路径仍可达, "切根"只是视图改名. 本文件以真实 VFS + 临时
//! 进程夹具钉住用户可见行为:
//!
//! - 切根后同一视图路径落到**新根的文件系统**, 旧根挂载点被摘除且不再覆盖任何路径;
//! - `new_root` 只是某挂载下的普通目录时必须 `EINVAL` (最长前缀归并不得冒充挂载点);
//! - 关系非法 (`EBUSY`/`EINVAL`) 与旧根摘除失败都不得留下半完成态.
//!
//! 纯关系校验 (`pivot_root_plan`) 的 6 个用例在 `kernel_tests/sys.rs`, 本文件不重复;
//! 这里锁的是 syscall 面新增的挂载点判据、摘除次序与失败传播.

use std::sync::Mutex;

use edgine::kernel::functions::fs::egfs::entry::get_egfs;
use edgine::kernel::functions::fs::path::pivot_root_syscall;
use edgine::kernel::functions::fs::{self, VFS_MANAGER, ramfs_core};
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};
use edgine::kernel::privileged::syscall::Errno;

/// 全局 FS / 进程表互斥 (本测试二进制内并行用例会互相抹掉挂载与当前进程).
static GLOBAL: Mutex<()> = Mutex::new(());

/// 临时当前进程: 真实 `Process::new` 插入全局表, Drop 时回收.
///
/// 根前缀存于进程 FS 上下文 (`CLONE_FS` 线程共享), 无进程时切根只写全局缺省,
/// 判据会失真; 故每个用例都必须持一个真实进程.
struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new(name: &str) -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, name, None));
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Scheduler::set_current(pid);
        Self { pid }
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
        Scheduler::set_current(0);
    }
}

/// 清挂载表后挂 ramfs 引导根 + EGFS 内存模式 `/disk` (D5-e 拓扑).
fn mount_boot_root_and_disk() {
    fs::init();
    ramfs_core::init();
    VFS_MANAGER.init();
    assert_eq!(fs::vfs_mount_safe("/", "ramfs"), 0, "引导根必须挂上 ramfs");
    get_egfs().init();
    assert_eq!(
        fs::vfs_mount_safe("/disk", "egfs"),
        0,
        "EGFS 必须挂到非根挂载点 /disk"
    );
}

/// 只挂 ramfs 到 `/boot`, 刻意让当前根 "/" 不是挂载点 (摘除失败夹具).
fn mount_without_root() {
    fs::init();
    ramfs_core::init();
    VFS_MANAGER.init();
    assert_eq!(fs::vfs_mount_safe("/boot", "ramfs"), 0);
    get_egfs().init();
    assert_eq!(fs::vfs_mount_safe("/disk", "egfs"), 0);
}

fn mkdir(path: &str) {
    assert_eq!(fs::api::vfs_mkdir_safe(path, 0), 0, "mkdir {path} 应成功");
}

fn write(path: &str, bytes: &[u8]) {
    assert!(
        fs::api::write_file_kernel(path, bytes, 0),
        "write_file_kernel {path} 应成功"
    );
}

fn read(path: &str) -> Vec<u8> {
    fs::api::read_file_kernel(path, 0, 64).unwrap_or_else(|| panic!("read_file_kernel {path} 失败"))
}

/// headline: 切根后视图路径落到新根 FS, 旧根挂载被摘除且不再覆盖任何路径.
#[test]
fn pivot_root_switches_view_and_detaches_old_root() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("pivot-switch");
    mount_boot_root_and_disk();

    // 同名文件在两个 FS 里内容不同 ⇒ 视图归属可由读回内容唯一判定.
    write("/marker", b"ramfs");
    write("/disk/marker", b"egfs");
    mkdir("/disk/oldroot");
    assert_eq!(read("/marker"), b"ramfs".to_vec(), "切根前视图根应是 ramfs");

    assert_eq!(
        pivot_root_syscall(c"/disk".as_ptr() as u64, c"/disk/oldroot".as_ptr() as u64),
        Ok(0),
        "new_root 是挂载点且 put_old 在其下 ⇒ 切根应成功"
    );

    assert_eq!(VFS_MANAGER.current_root_string(), "/disk");
    assert!(
        !VFS_MANAGER.is_mount_point("/"),
        "旧根挂载点必须被摘除 (只切前缀不摘挂载即回归)"
    );
    assert!(VFS_MANAGER.is_mount_point("/disk"));
    assert_eq!(
        read("/marker"),
        b"egfs".to_vec(),
        "切根后同一视图路径必须落到新根的文件系统"
    );
    assert!(
        VFS_MANAGER.resolve_mount_fs("/marker").is_none(),
        "旧根摘除后, 旧根内的真实路径不得再被任何挂载覆盖"
    );
}

/// `new_root` 只是挂载下的普通目录时必须 EINVAL.
///
/// 负控制意义: `find_mount` 会把 `/disk/sub` 归并到覆盖它的 `/disk` 挂载, 用它当
/// 判据即恒真 —— 必须用精确匹配的 `is_mount_point`.
#[test]
fn pivot_root_rejects_non_mount_point_new_root() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("pivot-not-mount");
    mount_boot_root_and_disk();
    mkdir("/disk/sub");
    mkdir("/disk/sub/old");
    assert!(
        VFS_MANAGER.find_mount("/disk/sub").is_some(),
        "夹具前提: /disk/sub 会被最长前缀归并到 /disk (否则本用例证不了精确匹配)"
    );

    assert_eq!(
        pivot_root_syscall(
            c"/disk/sub".as_ptr() as u64,
            c"/disk/sub/old".as_ptr() as u64
        ),
        Err(Errno::Einval),
        "new_root 不是挂载点 ⇒ EINVAL (Linux pivot_root(2))"
    );
    assert_eq!(VFS_MANAGER.current_root_string(), "/", "失败不得切根");
    assert!(
        VFS_MANAGER.is_mount_point("/"),
        "失败不得摘除旧根 (半完成态即回归)"
    );
}

/// 关系非法 (`new_root` 即当前根) 时必须在任何挂载表改动之前拒绝.
#[test]
fn pivot_root_rejects_pivot_to_self_without_mutating_mounts() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("pivot-self");
    mount_boot_root_and_disk();
    mkdir("/oldroot");

    assert_eq!(
        pivot_root_syscall(c"/".as_ptr() as u64, c"/oldroot".as_ptr() as u64),
        Err(Errno::Ebusy),
        "pivot 到自身 ⇒ EBUSY"
    );
    assert_eq!(VFS_MANAGER.current_root_string(), "/");
    assert!(VFS_MANAGER.is_mount_point("/"));
    assert!(VFS_MANAGER.is_mount_point("/disk"));
}

/// 旧根不是挂载点 ⇒ 摘除失败必须整体失败, 不得"根已切但旧根仍在".
#[test]
fn pivot_root_fails_whole_when_old_root_not_mounted() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("pivot-detach-fail");
    mount_without_root();
    mkdir("/disk/oldroot");

    assert_eq!(
        pivot_root_syscall(c"/disk".as_ptr() as u64, c"/disk/oldroot".as_ptr() as u64),
        Err(Errno::Einval),
        "当前根未挂载 ⇒ unmount 失败必须如实上报, 不得静默吞错"
    );
    assert_eq!(
        VFS_MANAGER.current_root_string(),
        "/",
        "摘除失败时根前缀必须保持原值 (先摘后切的次序判据)"
    );
    assert!(
        VFS_MANAGER.is_mount_point("/disk"),
        "摘除失败不得连带丢掉 new_root 挂载"
    );
}

/// 机制锁: syscall 面的四步次序与失败传播形态.
///
/// 行为用例覆盖不到"次序被换回先切后摘"的全部形态 (例如摘除失败被 `let _ =` 吞掉
/// 后仍切根), 故按源码形状钉住: plan → 挂载点判据 → unmount(`?`) → 切前缀.
#[test]
fn pivot_root_orders_plan_mountpoint_unmount_then_switch() {
    let src =
        std::fs::read_to_string("../src/kernel/functions/fs/path.rs").expect("读取 path.rs 失败");
    let body = src
        .split("pub fn pivot_root_syscall(")
        .nth(1)
        .expect("未找到 pivot_root_syscall");
    let flat: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let plan = flat.find("pivot_root_plan(").expect("必须先做关系校验");
    let mountpoint = flat
        .find("VFS_MANAGER.is_mount_point(new_real)")
        .expect("必须精确判定 new_root 是挂载点 (find_mount 归并即恒真)");
    let unmount = flat
        .find("VFS_MANAGER.unmount(&current_root)")
        .expect("必须摘除旧根挂载点");
    let switch = flat
        .find("VFS_MANAGER.set_current_root(new_real)")
        .expect("必须切换根前缀");
    assert!(plan < mountpoint, "关系校验必须先于挂载点判据");
    assert!(mountpoint < unmount, "挂载点判据必须先于摘除旧根");
    assert!(
        unmount < switch,
        "必须先摘旧根再切前缀: 反过来会在摘除失败时留下根已切的半完成态"
    );
    let tail = &flat[unmount..switch];
    assert!(
        tail.contains(".map_err(|_|Errno::Einval)?"),
        "摘除失败必须经 `?` 传播 (let _ = 吞错即回归)"
    );
}
