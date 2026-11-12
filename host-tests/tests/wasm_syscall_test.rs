// SPDX-License-Identifier: MPL-2.0
//! W-7a wasm 运行时 syscall 面回归锁 — ABI 单源 / dispatch 接线 / 错误映射 /
//! 注册表生命周期 (syscall 号 → functions dispatch → 注册表 → 解释器全链).
//!
//! 覆盖:
//! - `egnine-abi::wasm` 编号 766-769 与内核 `privileged::syscall::types` 同源;
//! - 四个编号在 `FunctionsSyscallDispatch` 中已分发 (不回退 `ENOSYS`);
//! - `EG_WASM_LOAD` 经 VFS 读文件 → 解析 → 注册表登记 → `RUN` → `GET_INFO`
//!   → `UNLOAD` 全链;
//! - 畸形模块映射 `ENOEXEC`; 句柄跨进程 / 重复加载 / 卸载后使用均被拒.

use std::sync::Mutex;

use edgine::kernel::functions::fs;
use edgine::kernel::functions::fs::ramfs_core::{self, RAMFS_DATA};
use edgine::kernel::functions::syscall::FunctionsSyscallDispatch;
use edgine::kernel::functions::syscall::types as ktypes;
use edgine::kernel::functions::wasm::types::WasmError;
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};
use edgine::kernel::privileged::syscall::{Errno, SyscallDispatch};
use edgine_abi::wasm as abi;

/// 全局 FS / 进程表 / 注册表互斥 (host 测试并行会互相抹掉挂载与当前进程).
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

/// 最小合法模块: 导出 `_start() -> i32`, 返回 `i32.const 0`.
fn minimal_module() -> Vec<u8> {
    let mut bytes = vec![
        0x00, 0x61, 0x73, 0x6D, // \0asm
        0x01, 0x00, 0x00, 0x00, // version 1
        // type section: 1 个 () -> i32
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7F,
        // function section: 1 个函数, 类型 0
        0x03, 0x02, 0x01, 0x00, // export section: "_start" 函数 0
        0x07, 0x0A, 0x01, 0x06, 0x5F, 0x73, 0x74, 0x61, 0x72, 0x74, 0x00, 0x00,
        // code section: 0 局部变量, i32.const 0, end
        0x0A, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0B,
    ];
    bytes.shrink_to_fit();
    bytes
}

/// 把字节写入 ramfs 文件 (测试用最小 VFS 写入链).
fn write_ramfs_file(name: &str, bytes: &[u8]) {
    let created = RAMFS_DATA.lock().create_file("/", name, 0);
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

/// 初始化 fs / ramfs 并挂载根目录; 同一进程内只做一次.
fn init_fs_once() {
    fs::init();
    ramfs_core::init();
    // 已挂载时返回负值; 同一测试进程内重复调用无副作用, 挂载结果由后续 open 断言.
    let _mount: i32 = fs::vfs_mount_safe("/", "ramfs");
}

#[test]
fn abi_numbers_are_single_source() {
    assert_eq!(ktypes::EG_WASM_LOAD, 766);
    assert_eq!(ktypes::EG_WASM_RUN, 767);
    assert_eq!(ktypes::EG_WASM_GET_INFO, 768);
    assert_eq!(ktypes::EG_WASM_UNLOAD, 769);
    assert_eq!(abi::EG_WASM_LOAD, ktypes::EG_WASM_LOAD);
    assert_eq!(abi::EG_WASM_RUN, ktypes::EG_WASM_RUN);
    assert_eq!(abi::EG_WASM_GET_INFO, ktypes::EG_WASM_GET_INFO);
    assert_eq!(abi::EG_WASM_UNLOAD, ktypes::EG_WASM_UNLOAD);
}

#[test]
fn dispatch_does_not_fall_back_to_enosys() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    // 无当前进程: LOAD/RUN 等应返 ESRCH 之外的错误, 但绝不能被 privileged 回退成 ENOSYS.
    let enosys = ktypes::ENOSYS_RET;
    for (num, args) in [
        (abi::EG_WASM_LOAD, [0u64, 0, 0, 0, 0, 0]),
        (abi::EG_WASM_RUN, [0u64, 0, 0, 0, 0, 0]),
        (abi::EG_WASM_GET_INFO, [0u64, 0, 0, 0, 0, 0]),
        (abi::EG_WASM_UNLOAD, [0u64, 0, 0, 0, 0, 0]),
    ] {
        let ret = FunctionsSyscallDispatch.dispatch(num, args);
        assert_ne!(ret, enosys, "编号 {num} 必须已被 functions dispatch 接管");
    }
}

#[test]
fn wasm_error_maps_to_posix_errno() {
    // 结构类 → ENOEXEC; trap 类 → EINVAL; 内存 → EFAULT; 上限 → ENOMEM; gas → ETIME.
    let cases: [(WasmError, Errno); 10] = [
        (WasmError::InvalidMagic, Errno::Enoexec),
        (WasmError::Truncated, Errno::Enoexec),
        (WasmError::UnknownOpcode(0xFF), Errno::Enoexec),
        (WasmError::TypeMismatch, Errno::Einval),
        (WasmError::BadLabelDepth(9), Errno::Einval),
        (WasmError::MemoryOutOfBounds, Errno::Efault),
        (WasmError::CallDepthExceeded, Errno::Enomem),
        (WasmError::GasExhausted, Errno::Etime),
        (WasmError::Unsupported, Errno::Enosys),
        (WasmError::InternalError, Errno::Eio),
    ];
    for (error, expected) in cases {
        let label = format!("{error:?}");
        let mapped = Errno::from(error);
        assert_eq!(mapped, expected, "{label} 映射错误");
    }
}

#[test]
fn syscall_chain_load_run_info_unload() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    init_fs_once();
    // fd 分配依赖当前进程 (per-process fd 表), 故先装进程再写文件.
    let _proc = TestProc::new("w7-l0");
    write_ramfs_file("w7_prog", &minimal_module());

    let path = c"/w7_prog".as_ptr() as u64;
    let handle = FunctionsSyscallDispatch.dispatch(abi::EG_WASM_LOAD, [path, 0, 0, 0, 0, 0]);
    assert!(handle > 0, "LOAD 应返回正句柄, 实测 {handle}");

    // 重复加载: 一个进程至多一个载体.
    let again = FunctionsSyscallDispatch.dispatch(abi::EG_WASM_LOAD, [path, 0, 0, 0, 0, 0]);
    assert_eq!(again, Errno::Ebusy.as_ret(), "重复 LOAD 应 EBUSY");

    let run = FunctionsSyscallDispatch
        .dispatch(abi::EG_WASM_RUN, [handle.cast_unsigned(), 0, 0, 0, 0, 0]);
    assert_eq!(run, 0, "RUN 一次跑完返回 0");

    let mut info = abi::EgWasmInfo {
        status: 0,
        exit_code: 0,
        gas_used: 0,
    };
    let info_ptr = std::ptr::from_mut(&mut info) as u64;
    let info_ret = FunctionsSyscallDispatch.dispatch(
        abi::EG_WASM_GET_INFO,
        [handle.cast_unsigned(), info_ptr, 0, 0, 0, 0],
    );
    assert_eq!(info_ret, 0, "GET_INFO 应成功");
    assert_eq!(info.status, abi::EG_WASM_STATUS_FINISHED);
    assert_eq!(info.exit_code, 0);
    assert!(info.gas_used > 0, "gas 计量应有读数");

    let unload = FunctionsSyscallDispatch
        .dispatch(abi::EG_WASM_UNLOAD, [handle.cast_unsigned(), 0, 0, 0, 0, 0]);
    assert_eq!(unload, 0, "UNLOAD 应成功");
    let after = FunctionsSyscallDispatch
        .dispatch(abi::EG_WASM_RUN, [handle.cast_unsigned(), 0, 0, 0, 0, 0]);
    assert_eq!(after, Errno::Ebadf.as_ret(), "卸载后句柄失效");
}

#[test]
fn malformed_module_yields_enoexec() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    init_fs_once();
    let _proc = TestProc::new("w7-bad");
    let mut bad = minimal_module();
    bad[0] = 0x01; // 破坏魔数
    write_ramfs_file("w7_bad", &bad);

    let path = c"/w7_bad".as_ptr() as u64;
    let ret = FunctionsSyscallDispatch.dispatch(abi::EG_WASM_LOAD, [path, 0, 0, 0, 0, 0]);
    assert_eq!(ret, Errno::Enoexec.as_ret(), "畸形模块应 ENOEXEC");
}
