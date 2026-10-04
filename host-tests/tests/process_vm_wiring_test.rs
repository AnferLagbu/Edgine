// process_vm: 验证 process_vm_readv / process_vm_writev 已接线到 privileged 安全代理
// 验收:
//   1. privileged::mm 顶层 re-export 跨进程拷贝原语 (F2: functions 只走顶层 API)
//   2. cross_process 提供读/写两个 safe 入口, 写入口校验叶子项写位 (I4)
//   3. functions 侧 process_vm 使用 privileged 代理且 0 unsafe
//   4. dispatch.rs 把 SYS_process_vm_readv / SYS_process_vm_writev 分发到 functions
//   5. io.rs 的 read_iovecs / IOV_MAX 对 crate 内可见 (供 process_vm 复用)
//
// 注: 静态契约扫描, 不进内核态.

use std::fs;
use std::path::Path;

const MM_MOD: &str = "src/kernel/privileged/mm/mod.rs";
const CROSS_PROCESS: &str = "src/kernel/privileged/mm/cross_process.rs";
const PROCESS_VM: &str = "src/kernel/functions/proc/process_vm.rs";
const PROC_MOD: &str = "src/kernel/functions/proc/mod.rs";
const DISPATCH: &str = "src/kernel/functions/syscall/dispatch.rs";
const FS_IO: &str = "src/kernel/functions/fs/io.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", path))
}

#[test]
fn test_mm_re_exports_cross_process_primitives() {
    let src = read(MM_MOD);
    assert!(
        src.contains("copy_from_user_in_mm") && src.contains("copy_to_user_in_mm"),
        "privileged::mm 顶层必须 re-export 跨进程拷贝原语 (F2: functions 走顶层 API)"
    );
}

#[test]
fn test_cross_process_exposes_read_write_and_checks_writable() {
    let src = read(CROSS_PROCESS);
    assert!(
        src.contains("pub fn copy_from_user_in_mm"),
        "cross_process 必须提供读入口 copy_from_user_in_mm"
    );
    assert!(
        src.contains("pub fn copy_to_user_in_mm"),
        "cross_process 必须提供写入口 copy_to_user_in_mm"
    );
    // 写方向必须校验目标页可写 (只读页拒绝), 否则内核会绕过用户态写保护
    assert!(
        src.contains("write_to_target && !t.writable"),
        "写方向必须校验叶子表项写位 (只读页拒绝写入)"
    );
    assert!(
        src.contains("// SAFETY:"),
        "privileged unsafe 块必须配 SAFETY 注释 (F4)"
    );
}

#[test]
fn test_functions_process_vm_uses_safe_proxy() {
    let src = read(PROCESS_VM);
    assert!(
        src.contains("#![deny(unsafe_code)]"),
        "functions process_vm 必须 deny(unsafe_code) (F1)"
    );
    assert!(
        src.contains("copy_from_user_in_mm") && src.contains("copy_to_user_in_mm"),
        "远端一侧必须经 privileged 跨进程代理访问 (I4)"
    );
    assert!(
        src.contains("copy_from_user") && src.contains("copy_to_user"),
        "本地一侧必须经 privileged 同进程代理访问"
    );
}

#[test]
fn test_process_vm_module_registered() {
    let src = read(PROC_MOD);
    assert!(
        src.contains("pub mod process_vm;"),
        "functions::proc 必须注册 process_vm 模块"
    );
}

#[test]
fn test_dispatch_wires_process_vm_syscalls() {
    let src = read(DISPATCH);
    assert!(
        src.contains("SYS_process_vm_readv")
            && src.contains("process_vm_readv_syscall")
            && src.contains("SYS_process_vm_writev")
            && src.contains("process_vm_writev_syscall"),
        "dispatch 必须把 SYS_process_vm_readv/writev 分发到 functions::proc::process_vm"
    );
}

#[test]
fn test_iovec_helpers_visible_to_crate() {
    let src = read(FS_IO);
    assert!(
        src.contains("pub(crate) const IOV_MAX"),
        "IOV_MAX 必须对 crate 内可见 (process_vm 复用)"
    );
    assert!(
        src.contains("pub(crate) fn read_iovecs"),
        "read_iovecs 必须对 crate 内可见 (process_vm 复用于本地 iovec)"
    );
}
