//! P2-I-44: 网络快照 (net_save / net_restore) host-test
//!
//! 验证:
//! 1. save.rs 模块存在, 暴露 NetSnapshot / save / load / clear / is_valid
//! 2. NetSnapshot 字段完整 (MAC, IP, GW, prefix, FD 表, 状态, 校验)
//! 3. 校验和能检测篡改
//! 4. save → load → clear 流程闭环
//! 5. net_save 不再是空函数体 (静态契约: 必须有 snap::save 调用)
//! 6. net_restore 读取快照并应用 (必须调用 snap::load)
//! 7. net/mod.rs 注册了 save 模块
//! 8. NET_SNAPSHOT_LOCK 不可与 NET_LOCK 死锁 (本测试仅镜像源代码契约)
//! 9. 单元测试数量

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

#[test]
fn save_module_exists() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    assert!(
        src.contains("pub struct NetSnapshot"),
        "P2-I-44: save.rs 必须定义 pub struct NetSnapshot"
    );
    assert!(
        src.contains("pub fn save<")
            && src.contains("pub fn load()")
            && src.contains("pub fn clear()"),
        "P2-I-44: save.rs 必须暴露 save/load/clear 三个入口"
    );
}

#[test]
fn snapshot_fields_complete() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    let required = [
        "magic: u32",
        "version: u32",
        "mac: [u8; 6]",
        "ip: [u8; 4]",
        "prefix_len: u8",
        "gateway: [u8; 4]",
        "dns:",
        "fd_types:",
        "fd_handles:",
        "net_ready: bool",
        "net_configured: bool",
        "sockets_initialized: bool",
        "init_state: u8",
        "checksum: u32",
    ];
    for f in required {
        assert!(src.contains(f), "P2-I-44: NetSnapshot 缺少字段 `{f}`");
    }
}

#[test]
fn snapshot_has_magic_and_version() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    assert!(
        src.contains("NET_SNAPSHOT_MAGIC") && src.contains("NET_SNAPSHOT_VERSION"),
        "P2-I-44: 必须定义魔数与版本常量"
    );
}

#[test]
fn is_valid_distinguishes_sealed_vs_empty() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    assert!(
        src.contains("pub fn is_valid(&self) -> bool"),
        "P2-I-44: NetSnapshot 必须有 is_valid 方法"
    );
    assert!(
        src.contains("pub fn seal(&mut self)"),
        "P2-I-44: NetSnapshot 必须有 seal 入口"
    );
}

#[test]
fn checksum_catches_tampering() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    assert!(
        src.contains("compute_checksum"),
        "P2-I-44: 必须实现 compute_checksum 用于篡改检测"
    );
    let test_block = src
        .rsplit_once("#[cfg(test)]")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        test_block.contains("fn tampering_breaks_checksum"),
        "P2-I-44: 必须有 tampering_breaks_checksum 单元测试"
    );
}

#[test]
fn save_load_roundtrip_unit() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    let test_block = src
        .rsplit_once("#[cfg(test)]")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        test_block.contains("fn save_load_roundtrip"),
        "P2-I-44: 必须有 save_load_roundtrip 单元测试"
    );
}

#[test]
fn save_uses_internal_lock() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    assert!(
        src.contains("NET_SNAPSHOT_LOCK") && src.contains("IrqSpinLock"),
        "P2-I-44: 快照必须用独立 IrqSpinLock 保护 (与 NET_LOCK 死锁矩阵分析见 deadlock_matrix.py)"
    );
}

#[test]
fn net_save_not_empty_anymore() {
    let src = read_src("src/kernel/privileged/net/init.rs");
    let marker = "unsafe fn net_save()";
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("P2-I-44: 找不到 {marker}"));
    // 找下一个 fn (即 net_restore) 之前的范围
    let next_fn = src[start..]
        .find("\nunsafe fn net_restore()")
        .map(|o| start + o)
        .unwrap_or(src.len());
    let body = &src[start..next_fn];
    assert!(
        !body.contains("unsafe fn net_save() {}\n") && !body.contains("unsafe fn net_save(){}"),
        "P2-I-44: net_save 不再是空函数体"
    );
    assert!(
        body.contains("snap::save"),
        "P2-I-44: net_save 必须调用 snap::save 填充快照"
    );
    assert!(
        body.contains("NET_STATE.lock()") || body.contains("NET_STATE.try_lock()"),
        "P2-I-44: net_save 必须持有 NET_STATE (与 smoltcp state 一致性)"
    );
}

#[test]
fn net_restore_reads_snapshot() {
    let src = read_src("src/kernel/privileged/net/init.rs");
    let marker = "unsafe fn net_restore()";
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("P2-I-44: 找不到 {marker}"));
    let next_fn = src[start..]
        .find("\nunsafe fn net_reset()")
        .map(|o| start + o)
        .unwrap_or(src.len());
    let body = &src[start..next_fn];
    assert!(
        body.contains("snap::load()"),
        "P2-I-44: net_restore 必须调用 snap::load() 读取快照"
    );
    assert!(
        body.contains("saved.is_valid()"),
        "P2-I-44: net_restore 必须先检查 saved.is_valid()"
    );
    assert!(
        body.contains("addrs.push(cidr)"),
        "P2-I-44: net_restore 必须把 IP 重新 push 到 iface (而不是依赖 DHCP)"
    );
    assert!(
        body.contains("add_default_ipv4_route"),
        "P2-I-44: net_restore 必须把 GW 重新加入默认路由"
    );
    assert!(
        body.contains("snap::clear()"),
        "P2-I-44: net_restore 末尾必须 snap::clear() 避免脏读"
    );
}

#[test]
fn net_restore_restores_fd_table() {
    let src = read_src("src/kernel/privileged/net/init.rs");
    let marker = "unsafe fn net_restore()";
    let start = src.find(marker).expect("missing net_restore");
    let body = &src[start..start + 5000];
    assert!(
        body.contains("raw::set_fd_type("),
        "P2-I-44: net_restore 必须按 fd 恢复 FD_TYPES"
    );
    assert!(
        body.contains("raw::set_socket_handle("),
        "P2-I-44: net_restore 必须按 fd 恢复 SOCKET_TABLE"
    );
}

#[test]
fn save_module_registered() {
    let src = read_src("src/kernel/privileged/net/mod.rs");
    assert!(
        src.contains("pub mod save"),
        "P2-I-44: net/mod.rs 必须 pub mod save"
    );
}

#[test]
fn unit_tests_count() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    let count = src.matches("#[test]").count();
    assert!(
        count >= 5,
        "P2-I-44: save.rs 内置至少 5 个 #[test] 单元测试, 实测 {count}"
    );
}

#[test]
fn fd_count_matches_max_sm_fd() {
    let src = read_src("src/kernel/privileged/net/save.rs");
    assert!(
        src.contains("SNAPSHOT_FD_COUNT: usize = 16"),
        "P2-I-44: SNAPSHOT_FD_COUNT 必须 = 16 (与 MAX_SM_FD 对齐)"
    );
    assert!(
        src.contains("fd_types: [u8; SNAPSHOT_FD_COUNT]"),
        "P2-I-44: fd_types 数组必须按 SNAPSHOT_FD_COUNT 分配"
    );
    assert!(
        src.contains("fd_handles: [u32; SNAPSHOT_FD_COUNT]"),
        "P2-I-44: fd_handles 数组必须按 SNAPSHOT_FD_COUNT 分配"
    );
}
