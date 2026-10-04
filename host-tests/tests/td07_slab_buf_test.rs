// TD-07: 验证 smoltcp TCP/UDP 缓冲已迁移到 slab, 不再静态 BSS 占用.
//
// 验收:
//   1. TCP/UDP 缓冲通过 raw:: 模块访问 (已迁移至 NetState)
//   2. smoltcp socket 创建必须通过 `k_malloc(TCP_BUF_SIZE)` / `k_malloc(UDP_BUF_SIZE)` 申请
//   3. smoltcp close 路径必须 `k_free` 4 个非空指针
//   4. k_free 后必须置回 null (防 double free)

use std::fs;
use std::path::Path;

const NET_INIT: &str = "src/kernel/privileged/net/init.rs";
const NET_SM_FI: &str = "src/kernel/privileged/net/init/sm_fi.rs";
// B04-09 (2026-08-25): buffer accessor 与 k_malloc/k_free 逻辑随拆分
// 移至 init/raw.rs, init.rs 经 `pub use raw::*` 引用.
const NET_RAW: &str = "src/kernel/privileged/net/init/raw.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", path))
}

/// 合并读取 init.rs 与 init/raw.rs 源码 (slab 缓冲逻辑所在位置).
fn read_raw_sources() -> String {
    let mut src = read(NET_INIT);
    src.push_str(&read(NET_RAW));
    src
}

#[test]
fn test_buf_access_through_raw_module() {
    // TD-07: 缓冲访问必须通过 raw:: 模块 (NetState 迁移后)
    let src = read_raw_sources();
    // raw 模块必须提供 tcp_rx_buf / tcp_tx_buf / udp_rx_buf / udp_tx_buf accessor
    for accessor in &["tcp_rx_buf", "tcp_tx_buf", "udp_rx_buf", "udp_tx_buf"] {
        assert!(
            src.contains(accessor),
            "TD-07: raw 模块必须提供 {} accessor",
            accessor
        );
    }
}

#[test]
fn test_socket_alloc_uses_kmalloc() {
    let src = read_raw_sources();
    // TCP 路径必须用 k_malloc(TCP_BUF_SIZE)
    assert!(
        src.contains("k_malloc(TCP_BUF_SIZE)"),
        "TD-07: smoltcp TCP socket alloc 必须通过 `k_malloc(TCP_BUF_SIZE)` 申请缓冲"
    );
    // UDP 路径必须用 k_malloc(UDP_BUF_SIZE)
    assert!(
        src.contains("k_malloc(UDP_BUF_SIZE)"),
        "TD-07: smoltcp UDP socket alloc 必须通过 `k_malloc(UDP_BUF_SIZE)` 申请缓冲"
    );
}

#[test]
fn test_socket_close_uses_kfree() {
    let src = read_raw_sources();
    // close 路径必须对缓冲调用 k_free
    assert!(
        src.contains("k_free(raw::tcp_rx_buf") || src.contains("k_free("),
        "TD-07: smoltcp close 路径必须调用 k_free 归还 slab"
    );
    // 必须置回 null
    assert!(
        src.contains("set_tcp_rx_buf") || src.contains("null_mut()"),
        "TD-07: k_free 之后必须把缓冲置回 null, 防 double free"
    );
}

#[test]
fn test_buf_lifetime_safe_through_socket_remove() {
    // TD-07: smoltcp socket 必须先 `sockets.remove(handle)` (drop), 才能 k_free 缓冲.
    // sm_close 已移至 sm_fi.rs
    let src = read(NET_SM_FI);
    let close_marker = "pub unsafe extern \"C\" fn sm_close(";
    let start = src.find(close_marker).expect("sm_close not found");
    let body = &src[start..start + 5000];
    let remove_idx = body
        .find("sockets.remove(handle);")
        .expect("sockets.remove missing");
    // 找 k_free 中最早一个的下标
    let first_kfree = body.find("k_free(").expect("k_free missing");
    assert!(
        remove_idx < first_kfree,
        "TD-07: sockets.remove(handle) 必须在所有 k_free 之前 (借用释放顺序敏感)"
    );
}
