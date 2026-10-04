// TD-05: 验证 smoltcp 小型热表 (SOCKET_TABLE / FD_TYPES) 已迁移至 NetState 结构体
//
// 注: 原 Align64 包装已在 NetState 统一迁移中移除, 热表现在是 NetState 的字段,
// 由 IrqSpinLock/Mutex 保护. 对齐由结构体布局自动保证.
//
// 注: 静态契约扫描, 不进内核态.

use std::fs;
use std::path::Path;

const INIT: &str = "src/kernel/privileged/net/init.rs";
// B04-09 (2026-08-25): NetState 结构体定义随拆分移至 init/state.rs,
// init.rs 经 `pub use state::*` re-export. 契约扫描需同时覆盖两文件.
const STATE: &str = "src/kernel/privileged/net/init/state.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", path))
}

/// 合并读取 init.rs 与 init/state.rs 源码 (NetState 定义所在位置).
fn read_net_state_sources() -> String {
    let mut src = read(INIT);
    src.push_str(&read(STATE));
    src
}

#[test]
fn test_socket_table_in_net_state() {
    // TD-05: SOCKET_TABLE 必须是 NetState 结构体的字段
    let src = read_net_state_sources();
    assert!(
        src.contains("socket_table:") || src.contains("socket_table: ["),
        "TD-05: SOCKET_TABLE 必须是 NetState 的字段"
    );
    // 不再有独立的 static mut SOCKET_TABLE
    assert!(
        !src.contains("static mut SOCKET_TABLE:"),
        "TD-05: SOCKET_TABLE 不应再是独立的 static mut"
    );
}

#[test]
fn test_fd_types_in_net_state() {
    // TD-05: FD_TYPES 必须是 NetState 结构体的字段
    let src = read_net_state_sources();
    assert!(
        src.contains("fd_types:") || src.contains("fd_types: ["),
        "TD-05: FD_TYPES 必须是 NetState 的字段"
    );
    // 不再有独立的 static mut FD_TYPES
    assert!(
        !src.contains("static mut FD_TYPES:"),
        "TD-05: FD_TYPES 不应再是独立的 static mut"
    );
}

#[test]
fn test_no_raw_static_mut_access() {
    // TD-05: 不应再有直接通过 SOCKET_TABLE[...] 或 FD_TYPES[...] 的访问
    // 所有访问应通过 raw:: 模块
    let src = read(INIT);
    for (idx, line) in src.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        // 检查是否有直接的 SOCKET_TABLE[ 访问 (非 raw:: 前缀)
        if trimmed.contains("SOCKET_TABLE[")
            && !trimmed.contains("raw::")
            && !trimmed.contains("//")
        {
            panic!(
                "TD-05: L{} 仍有直接 SOCKET_TABLE[ 访问, 应通过 raw:: 模块:\n{}",
                idx + 1,
                line
            );
        }
        // 检查是否有直接的 FD_TYPES[ 访问
        if trimmed.contains("FD_TYPES[") && !trimmed.contains("raw::") && !trimmed.contains("//") {
            panic!(
                "TD-05: L{} 仍有直接 FD_TYPES[ 访问, 应通过 raw:: 模块:\n{}",
                idx + 1,
                line
            );
        }
    }
}

#[test]
fn test_net_state_struct_documented() {
    // TD-05: NetState 结构体必须有文档说明
    let src = read_net_state_sources();
    assert!(
        src.contains("struct NetState"),
        "TD-05: 必须定义 NetState 结构体"
    );
}
