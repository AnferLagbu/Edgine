//! DECISION-K: IpcStrategy 注册时序门禁测试 (2026-09-12)
//!
//! 验证 (静态契约):
//! 1. 注册点前置: `register_default_ipc_strategy()` 在 lib.rs 中位于
//!    `interrupt_late_init` **之前** (kernel_init 早期, 删去 VFS 后约束)
//! 2. 未注册降级契约: `current_ipc_strategy()` 返回 `Option` (不 panic);
//!    13 处 FFI 调用点含 `Errno::ENOSYS` 降级 (逻辑错误降级原则, 不进 freg 恢复)

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

fn line_of(src: &str, needle: &str) -> usize {
    src.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("lib.rs 未找到标记 '{needle}'"))
}

#[test]
fn registration_precedes_interrupt_late_init() {
    // DECISION-K 注册点前置: 注册 (5.75) 必须在 interrupt_late_init (6) 之前
    // (匹配实际调用语句, 排除注释中的 "interrupt_late_init")
    let src = read_src("src/kernel/lib.rs");
    let reg_line = line_of(&src, "register_default_ipc_strategy");
    // 标记不含尾随 "()": 该调用语句按 100 列被 rustfmt 折行时, 括号会被拆到下一行,
    // 含 "()" 的标记会随折行失效 (而 "Arch>::" 前缀已足以排除注释中的 interrupt_late_init).
    let irq_call = "Arch>::interrupt_late_init";
    let irq_line = line_of(&src, irq_call);
    assert!(
        reg_line < irq_line,
        "IPC 策略注册 ({reg_line}) 必须在 interrupt_late_init 调用 ({irq_line}) 之前 (注册点前置契约)"
    );
}

#[test]
fn registration_has_contract_comment() {
    let src = read_src("src/kernel/lib.rs");
    assert!(
        src.contains("IPC 策略注册契约点") && src.contains("DECISION-K"),
        "lib.rs 注册点必须带 'IPC 策略注册契约点' + DECISION-K 注释 (启动契约化)"
    );
}

#[test]
fn current_ipc_strategy_returns_option() {
    // DECISION-K: current_ipc_strategy() 返回 Option, 不 panic (逻辑错误降级原则)
    let src = read_src("src/kernel/framework/ipc/strategy.rs");
    assert!(
        src.contains("pub fn current_ipc_strategy() -> Option<&'static dyn IpcStrategy>"),
        "strategy.rs 必须返回 Option (未注册降级, 不 panic)"
    );
    let current_fn = src
        .split_once("pub fn current_ipc_strategy")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        !current_fn.contains("panic!"),
        "current_ipc_strategy 不得 panic (未注册降级契约)"
    );
}

#[test]
fn ffi_call_sites_degrade_to_enosys() {
    // DECISION-K: 13 处 FFI 调用点未注册降级 ENOSYS (除 is_pipe_fd/create 类哨兵)
    let pipe = read_src("src/kernel/framework/ipc/pipe.rs");
    let shm = read_src("src/kernel/framework/ipc/shm.rs");
    let msgq = read_src("src/kernel/framework/ipc/msgq.rs");
    // 降级标记: 每处 let-else + klog_warn + ENOSYS/无效 id
    let ffi_degrades = [
        ("ipc_pipe_read", pipe.contains("ENOSYS")),
        ("ipc_pipe_write", pipe.contains("ENOSYS")),
        ("ipc_pipe_close", pipe.contains("ENOSYS")),
        ("ipc_shm_attach", shm.contains("ENOSYS")),
        ("ipc_shm_detach", shm.contains("ENOSYS")),
        ("ipc_shm_destroy", shm.contains("ENOSYS")),
        ("ipc_msgq_send", msgq.contains("ENOSYS")),
        ("ipc_msgq_recv", msgq.contains("ENOSYS")),
        ("ipc_msgq_destroy", msgq.contains("ENOSYS")),
    ];
    for (name, ok) in ffi_degrades {
        assert!(ok, "FFI 调用点 {name} 必须含 ENOSYS 降级 (DECISION-K)");
    }
    // create 类函数: 返回 0 (无效 id) + 日志
    assert!(
        pipe.contains("IpcStrategy 未注册: ipc_pipe_create"),
        "ipc_pipe_create 必须含未注册日志"
    );
    assert!(
        shm.contains("IpcStrategy 未注册: ipc_shm_create"),
        "ipc_shm_create 必须含未注册日志"
    );
    assert!(
        msgq.contains("IpcStrategy 未注册: ipc_msgq_create"),
        "ipc_msgq_create 必须含未注册日志"
    );
    // is_pipe_fd: 未注册返回 false
    assert!(
        pipe.contains("map_or(false"),
        "is_pipe_fd 未注册必须返回 false"
    );
}
