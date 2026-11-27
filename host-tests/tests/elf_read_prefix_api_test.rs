//! E-1 有界前缀读 API 回归锁.
//!
//! 病灶: `read_file_to_vec` / `read_file_kernel` 在 `size > max_size` 时直接拒绝,
//! 而 ELF 头部 / wasm 魔数只需要文件前若干字节. E-1 新增两个前缀 API:
//! `read_file_prefix_to_vec` (syscall fd 路径) 与 `read_file_prefix_kernel`
//! (启动期无 fd 路径); 超限时只读 `min(size, max_size)`, 不改变整文件 API 的
//! 旧口径.
//!
//! ## 五臂
//! 1. `max_size` 小于文件 ⇒ 只读前缀 (短读截断);
//! 2. `max_size` 等于 / 大于文件 ⇒ 读满实际文件;
//! 3. 空文件 / `max_size == 0` ⇒ `None`;
//! 4. 权限拒绝 (`pwm` 不存在) ⇒ `None`, 但同文件以 `pwm=0` 可读;
//! 5. 路径不存在 ⇒ `None`.
//!
//! 两条路径都走真实 VFS + `RamFS`; `read_file_prefix_to_vec` 另需临时当前进程
//! 提供 fd 表, `read_file_prefix_kernel` 不依赖进程上下文.

use std::sync::Mutex;

use edgine::kernel::functions::fs;
use edgine::kernel::functions::fs::api::{
    read_file_kernel, read_file_prefix_kernel, read_file_prefix_to_vec, read_file_to_vec,
};
use edgine::kernel::functions::fs::ramfs_core;
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};

/// 本测试二进制内共享 VFS / `RamFS` / 进程表, 必须串行.
static GLOBAL: Mutex<()> = Mutex::new(());

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

/// 构造以 NUL 结尾的路径字节串.
fn cpath(path: &str) -> Vec<u8> {
    let mut bytes = path.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// 经内核无 fd 写入路径建文件 (空 `bytes` 也建出 0 字节文件).
fn write_file(name: &str, bytes: &[u8]) {
    let path = format!("/{name}");
    assert!(
        edgine::kernel::functions::fs::api::write_file_kernel(&path, bytes, 0),
        "write_file_kernel {path} 失败"
    );
}

/// 1. `max_size` 小于文件: 两个前缀 API 都只返回前 `max_size` 字节;
///    整文件 API 仍按旧口径拒绝超限.
#[test]
fn prefix_less_than_file_reads_only_prefix() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("elf-prefix-short");
    init_fs_once();
    write_file("prefix_short", b"ABCDEFGH");

    let path = cpath("/prefix_short");
    let got = read_file_prefix_to_vec(path.as_ptr(), 0, 3).expect("syscall 前缀读应成功");
    assert_eq!(got.as_slice(), b"ABC", "只读前 3 字节, 不是整文件 8 字节");
    let got_kernel = read_file_prefix_kernel("/prefix_short", 0, 3).expect("kernel 前缀读应成功");
    assert_eq!(got_kernel.as_slice(), b"ABC");

    assert!(
        read_file_to_vec(path.as_ptr(), 0, 3).is_none(),
        "整文件 API 仍须拒绝 size > max_size"
    );
    assert!(
        read_file_kernel("/prefix_short", 0, 3).is_none(),
        "无 fd 整文件 API 仍须拒绝 size > max_size"
    );
    assert!(
        read_file_prefix_to_vec(path.as_ptr(), 0, 0).is_none(),
        "max_size == 0 不得分配空前缀"
    );
}

/// 2. `max_size` 等于 / 大于文件: 返回实际文件长度, 不补零.
#[test]
fn prefix_equal_or_greater_than_file_reads_full_file() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("elf-prefix-full");
    init_fs_once();
    write_file("prefix_full", b"12345");

    let path = cpath("/prefix_full");
    let exact = read_file_prefix_to_vec(path.as_ptr(), 0, 5).expect("等于文件长度应成功");
    assert_eq!(exact.as_slice(), b"12345");
    let greater = read_file_prefix_to_vec(path.as_ptr(), 0, 64).expect("大于文件长度应成功");
    assert_eq!(greater.as_slice(), b"12345");
    let kernel_exact =
        read_file_prefix_kernel("/prefix_full", 0, 5).expect("kernel 等于文件长度应成功");
    assert_eq!(kernel_exact.as_slice(), b"12345");
    let kernel_greater =
        read_file_prefix_kernel("/prefix_full", 0, 64).expect("kernel 大于文件长度应成功");
    assert_eq!(kernel_greater.as_slice(), b"12345");
}

/// 3. 空文件: 两个前缀 API 都返回 `None` (与整文件 API 的空文件口径一致).
#[test]
fn prefix_empty_file_returns_none() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("elf-prefix-empty");
    init_fs_once();
    write_file("prefix_empty", b"");

    let path = cpath("/prefix_empty");
    assert!(read_file_prefix_to_vec(path.as_ptr(), 0, 4).is_none());
    assert!(read_file_prefix_kernel("/prefix_empty", 0, 4).is_none());
    assert!(read_file_to_vec(path.as_ptr(), 0, 4).is_none());
    assert!(read_file_kernel("/prefix_empty", 0, 4).is_none());
}

/// 4. 权限拒绝: 不存在的 pwm 不得读文件; 同一文件以 bootstrap `pwm=0` 可读.
#[test]
fn prefix_permission_denied_returns_none() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("elf-prefix-denied");
    init_fs_once();
    write_file("prefix_denied", b"secret");

    let path = cpath("/prefix_denied");
    let denied = read_file_prefix_to_vec(path.as_ptr(), u64::MAX, 2);
    assert!(denied.is_none(), "无效 pwm 不得读前缀");
    let denied_kernel = read_file_prefix_kernel("/prefix_denied", u64::MAX, 2);
    assert!(denied_kernel.is_none(), "无效 pwm 的无 fd 前缀读也必须失败");

    let allowed = read_file_prefix_to_vec(path.as_ptr(), 0, 2).expect("bootstrap pwm=0 可读");
    assert_eq!(allowed.as_slice(), b"se");
}

/// 5. 路径不存在: 两个前缀 API 都返回 `None`.
#[test]
fn prefix_missing_path_returns_none() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    let _proc = TestProc::new("elf-prefix-missing");
    init_fs_once();

    let path = cpath("/prefix_missing");
    assert!(read_file_prefix_to_vec(path.as_ptr(), 0, 4).is_none());
    assert!(read_file_prefix_kernel("/prefix_missing", 0, 4).is_none());
}
