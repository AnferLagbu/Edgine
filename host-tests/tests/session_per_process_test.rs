// ============================================================================
// P2-I-30: Session Manager UnsafeCell 全局单例 → per-process 化 host-test
// ============================================================================
//
// 验收契约:
// 1. 源码层 `static GLOBAL_SESSION` 全局单例必须被删除 (框架 sgeg/session.rs
//    不再持有 process 间共享的可变状态).
// 2. 源码层 SessionManager 结构体 (含 UnsafeCell 字段) 必须被删除.
// 3. PwmContext 凭证会话上下文必须绑定到 Process 结构体, 字段命名稳定:
//    - Process::session: Mutex<PwmContext>
//    - Process::session_elev_stack: Mutex<[PwmContext; 8]>
//    - Process::session_elev_depth: AtomicIsize
// 4. Process::new 初始化三个新字段 (而非留未初始化).
// 5. 公开 API 签名保持向后兼容 (login/logout/get_current_pwm/try_setuid 等).
// 6. 跨进程隔离: 进程 A 写入 session, 进程 B 读取不到 (静态源码契约: 每进程
//    持锁路径独立, 不再有进程间共享的 UnsafeCell<PwmContext>).
// ============================================================================

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // host-tests 在 <CARGO_WORKSPACE>/host-tests, 源码在同级的 kernel/
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

#[test]
fn session_rs_no_global_session_static() {
    let src = read_src("src/kernel/privileged/sgeg/session.rs");
    // 仅检查代码行, 排除注释 (注释中允许出现历史名字以解释设计变更)
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        assert!(
            !line.contains("static GLOBAL_SESSION"),
            "P2-I-30: sgeg/session.rs 仍包含 `static GLOBAL_SESSION` 全局单例 (行: {})",
            line
        );
    }
}

#[test]
fn session_rs_no_session_manager_struct() {
    let src = read_src("src/kernel/privileged/sgeg/session.rs");
    assert!(
        !src.contains("struct SessionManager"),
        "P2-I-30: sgeg/session.rs 仍定义 SessionManager 结构体"
    );
    assert!(
        !src.contains("impl SessionManager"),
        "P2-I-30: sgeg/session.rs 仍实现 SessionManager impl 块"
    );
}

#[test]
fn session_rs_no_unsafe_cell() {
    // 检查代码行 (排除注释), per-process 化必须彻底清除 UnsafeCell 使用
    let src = read_src("src/kernel/privileged/sgeg/session.rs");
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        assert!(
            !line.contains("UnsafeCell"),
            "P2-I-30: sgeg/session.rs 代码行仍引用 UnsafeCell (行: {})",
            line
        );
    }
    // unsafe impl Send/Sync for SessionManager 也不应存在
    assert!(
        !src.contains("unsafe impl Send for SessionManager"),
        "P2-I-30: SessionManager 仍显式声明 Send, 表明未删除"
    );
    assert!(
        !src.contains("unsafe impl Sync for SessionManager"),
        "P2-I-30: SessionManager 仍显式声明 Sync, 表明未删除"
    );
}

#[test]
fn process_struct_has_session_fields() {
    let src = read_src("src/kernel/privileged/proc/process.rs");
    assert!(
        src.contains("pub session: Mutex<") && src.contains("PwmContext"),
        "P2-I-30: Process 缺少 `pub session: Mutex<PwmContext>` 字段"
    );
    assert!(
        src.contains("pub session_elev_stack:"),
        "P2-I-30: Process 缺少 `session_elev_stack` 字段"
    );
    assert!(
        src.contains("pub session_elev_depth:"),
        "P2-I-30: Process 缺少 `session_elev_depth` 字段"
    );
    assert!(
        src.contains("AtomicIsize"),
        "P2-I-30: session_elev_depth 必须为 AtomicIsize"
    );
}

#[test]
fn process_new_initializes_session_fields() {
    let src = read_src("src/kernel/privileged/proc/process.rs");
    // 在 Process::new 的初始化块中查找三个字段的赋值
    let new_block_start = src
        .find("pub fn new(pid: Pid")
        .expect("Process::new 不存在");
    let new_block = &src[new_block_start..];
    // 限定在第一个 Self { ... } 闭花括号范围
    let body_end = new_block
        .find("    }\n    }")
        .expect("Process::new 体结构异常");
    let body = &new_block[..body_end];
    assert!(
        body.contains("session: Mutex::new("),
        "P2-I-30: Process::new 未初始化 session 字段"
    );
    assert!(
        body.contains("session_elev_stack: Mutex::new("),
        "P2-I-30: Process::new 未初始化 session_elev_stack 字段"
    );
    assert!(
        body.contains("session_elev_depth: AtomicIsize::new(0)"),
        "P2-I-30: Process::new 未将 session_elev_depth 初始化为 0"
    );
}

#[test]
fn sgeg_mod_does_not_reexport_session_manager() {
    let src = read_src("src/kernel/privileged/sgeg/mod.rs");
    assert!(
        !src.contains("pub use session::SessionManager"),
        "P2-I-30: sgeg/mod.rs 仍在 re-export SessionManager, 应删除"
    );
}

#[test]
fn public_api_signatures_preserved() {
    // 公开 API 函数名/签名必须与 I-30 改造前一致, 所有调用方不需要改
    let src = read_src("src/kernel/privileged/sgeg/session.rs");
    for sig in &[
        "pub fn login(",
        "pub fn logout(",
        "pub fn get_current_pwm(",
        "pub fn get_current_uid(",
        "pub fn get_current_gid(",
        "pub fn get_euid(",
        "pub fn get_egid(",
        "pub fn get_saved_euid(",
        "pub fn get_saved_egid(",
        "pub fn is_logged_in(",
        "pub fn clear_lockout(",
        "pub fn elevate_for_suid(",
        "pub fn drop_elevation(",
        "pub fn has_elevation_authority(",
        "pub fn try_setuid(",
        "pub fn try_setgid(",
        "pub fn try_seteuid(",
        "pub fn try_setegid(",
        "pub fn try_setreuid(",
        "pub fn try_setregid(",
        "pub fn get_current_domain_id(",
    ] {
        assert!(
            src.contains(sig),
            "P2-I-30: 公开 API `{}` 在 sgeg/session.rs 中缺失",
            sig
        );
    }
}

#[test]
fn session_rs_routes_through_process_table() {
    // 内部实现必须走 process_get_current_pid + PROCESS_TABLE 路径
    let src = read_src("src/kernel/privileged/sgeg/session.rs");
    assert!(
        src.contains("process_get_current_pid"),
        "P2-I-30: sgeg/session.rs 内部未调用 process_get_current_pid"
    );
    assert!(
        src.contains("PROCESS_TABLE.with_process"),
        "P2-I-30: sgeg/session.rs 内部未走 PROCESS_TABLE.with_process 查表"
    );
}

#[test]
fn no_legacy_g_session_singleton_in_other_sgeg_modules() {
    // 复查: 其它 sgeg 子模块没有偷偷回退到全局状态
    for rel in &[
        "src/kernel/privileged/sgeg/api.rs",
        "src/kernel/privileged/sgeg/identity.rs",
        "src/kernel/privileged/sgeg/audit.rs",
    ] {
        let src = read_src(rel);
        assert!(
            !src.contains("GLOBAL_SESSION"),
            "P2-I-30: {} 仍引用已删除的 GLOBAL_SESSION",
            rel
        );
    }
}
