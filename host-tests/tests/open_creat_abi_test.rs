// VfsOpenFlags ABI 错位根治 — 用户态 open 边界翻译的行为级回归锁.
//
// 病灶: `open_syscall` 曾把 Linux 原始 `O_*` flags 直传 VFS, 而 VFS 内部层用
// 另一套 `VfsOpenFlags` 编码 (accmode 独立位 1/2/4、CREAT=0x0100)。Linux
// `O_CREAT`=0o100 恒不命中 VFS CREAT=0x0100 ⇒ 创建不存在文件恒 ENOENT;
// accmode 亦被误读 (O_WRONLY→RDONLY、O_RDWR→WRONLY)。
//
// 修复: `fs::open::linux_o_flags_to_vfs` 在用户态边界翻译 (open/openat/creat
// 三入口共用 open_syscall chokepoint)。本文件以真实 VFS + 临时进程夹具钉住
// 用户可见行为: O_CREAT 真创建、无 O_CREAT 仍 ENOENT、accmode 经 F_GETFL 可观测。
//
// 翻译矩阵的逐位断言另见 `fs/open.rs` 内联 `mod tests` (纯函数, 无需 VFS)。

use std::sync::Mutex;

use edgine::kernel::functions::fs;
use edgine::kernel::functions::fs::io::fcntl_syscall;
use edgine::kernel::functions::fs::open::{
    O_CREAT, O_RDONLY, O_RDWR, O_WRONLY, close_syscall, open_syscall,
};
use edgine::kernel::functions::fs::ramfs_core;
use edgine::kernel::functions::fs::vfs_types::VfsOpenFlags;
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};
use edgine::kernel::privileged::syscall::Errno;

/// 全局 FS / 进程表互斥 (本测试二进制内并行用例会互相抹掉挂载与当前进程).
static GLOBAL: Mutex<()> = Mutex::new(());

/// `fcntl` 命令 `F_GETFL` (与 `fs/io.rs` 私有常量同值).
const F_GETFL: i32 = 3;

/// 临时当前进程: 真实 `Process::new` 插入全局表, Drop 时回收.
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

/// 初始化 fs / ramfs 并挂载根目录; 重复调用无副作用.
fn init_fs_once() {
    fs::init();
    ramfs_core::init();
    let _mount: i32 = fs::vfs_mount_safe("/", "ramfs");
}

/// 把字节写入 ramfs 文件 (构造"已存在文件"夹具).
fn write_ramfs_file(name: &str, bytes: &[u8]) {
    let created = ramfs_core::RAMFS_DATA.lock().create_file("/", name, 0);
    assert!(created.is_some(), "create_file /{name} 失败");
    let path = format!("/{name}");
    let fd = fs::vfs_open_safe(&path, 1, 0);
    assert!(fd >= 0, "open {path} 应成功");
    let written = fs::vfs_write_safe(fd.cast_unsigned(), bytes);
    assert_eq!(
        usize::try_from(written).expect("写入字节数"),
        bytes.len(),
        "写入 {path} 应完整"
    );
    assert_eq!(fs::vfs_close_safe(fd.cast_unsigned()), 0);
}

/// headline 回归: Linux `O_CREAT|O_WRONLY` 打开不存在文件必须真创建.
/// 修复前首开即 `Err(Enoent)`; 修复后创建成功且可再次只读打开.
#[test]
fn o_creat_creates_missing_file() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("ocreat-new");
    init_fs_once();

    let path = b"/ocreat_new_a\0";
    let fd = open_syscall(path.as_ptr() as u64, O_CREAT | O_WRONLY, 0o644)
        .expect("O_CREAT|O_WRONLY 打开不存在文件应创建成功 (修复前恒 ENOENT)");
    assert_eq!(
        close_syscall(i32::try_from(fd).expect("fd 应可由 i32 表示")),
        Ok(0)
    );

    // 文件既已创建, 不带 O_CREAT 的只读打开也应成功.
    let rd = open_syscall(path.as_ptr() as u64, O_RDONLY, 0).expect("创建后只读打开应成功");
    assert_eq!(
        close_syscall(i32::try_from(rd).expect("fd 应可由 i32 表示")),
        Ok(0)
    );
}

/// 负控制: 不带 `O_CREAT` 打开不存在文件仍须 `ENOENT`
/// (证明修复不是"一律创建", 未放宽存在性判据).
#[test]
fn no_creat_missing_file_is_enoent() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("ocreat-nocreat");
    init_fs_once();

    let path = b"/ocreat_missing_b\0";
    let ret = open_syscall(path.as_ptr() as u64, O_WRONLY, 0);
    assert_eq!(
        ret,
        Err(Errno::Enoent),
        "无 O_CREAT 打开不存在文件必须 ENOENT"
    );
}

/// `O_CREAT|O_RDWR` 同样须真创建 (accmode 与 CREAT 组合不退化).
#[test]
fn o_creat_rdwr_creates() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("ocreat-rdwr");
    init_fs_once();

    let path = b"/ocreat_new_c\0";
    let fd = open_syscall(path.as_ptr() as u64, O_CREAT | O_RDWR, 0o644)
        .expect("O_CREAT|O_RDWR 打开不存在文件应创建成功");
    assert_eq!(
        close_syscall(i32::try_from(fd).expect("fd 应可由 i32 表示")),
        Ok(0)
    );
}

/// accmode 可观测回归: Linux `O_WRONLY` 经翻译后, `F_GETFL` 回读的 VFS 状态
/// 标志须置 WRONLY 位、不得置 RDONLY 位 (修复前 `O_WRONLY=1` 被误读成 VFS RDONLY).
#[test]
fn accmode_wronly_translated_not_rdonly() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("ocreat-accmode-w");
    init_fs_once();
    write_ramfs_file("ocreat_existing_w", b"x");

    let path = b"/ocreat_existing_w\0";
    let fd =
        open_syscall(path.as_ptr() as u64, O_WRONLY, 0).expect("O_WRONLY 打开已存在文件应成功");
    let flags = fcntl_syscall(i32::try_from(fd).expect("fd"), F_GETFL, 0).expect("F_GETFL 应成功");
    let flags = u32::try_from(flags).expect("flags 应可由 u32 表示");
    assert_ne!(
        flags & VfsOpenFlags::WRONLY.bits(),
        0,
        "O_WRONLY 应译为 VFS WRONLY"
    );
    assert_eq!(
        flags & VfsOpenFlags::RDONLY.bits(),
        0,
        "O_WRONLY 不得被误读为 RDONLY"
    );
    assert_eq!(close_syscall(i32::try_from(fd).expect("fd")), Ok(0));
}

/// accmode 可观测回归: Linux `O_RDWR` 经翻译后 `F_GETFL` 须置 RDWR 位,
/// 不得退化为 WRONLY (修复前 `O_RDWR=2` 被误读成 VFS WRONLY).
#[test]
fn accmode_rdwr_translated_not_wronly() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("ocreat-accmode-rw");
    init_fs_once();
    write_ramfs_file("ocreat_existing_rw", b"x");

    let path = b"/ocreat_existing_rw\0";
    let fd = open_syscall(path.as_ptr() as u64, O_RDWR, 0).expect("O_RDWR 打开已存在文件应成功");
    let flags = fcntl_syscall(i32::try_from(fd).expect("fd"), F_GETFL, 0).expect("F_GETFL 应成功");
    let flags = u32::try_from(flags).expect("flags 应可由 u32 表示");
    assert_ne!(
        flags & VfsOpenFlags::RDWR.bits(),
        0,
        "O_RDWR 应译为 VFS RDWR"
    );
    assert_eq!(
        flags & VfsOpenFlags::WRONLY.bits(),
        0,
        "O_RDWR 不得退化为 WRONLY"
    );
    assert_eq!(
        flags & VfsOpenFlags::RDONLY.bits(),
        0,
        "O_RDWR 不得含 RDONLY 位"
    );
    assert_eq!(close_syscall(i32::try_from(fd).expect("fd")), Ok(0));
}
