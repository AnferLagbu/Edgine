// TD-06: 验证 smoltcp FD 容量配置.
//
// 验收:
//   1. `framework/net/init.rs` 的 `MAX_SOCKETS` 必须为 256
//   2. `MAX_SM_FD` 与 `FdPlan::SMOLTCP.capacity` 仍为单一来源 (MAX_SOCKETS == MAX_SM_FD 隐含)
//   3. 默认容量 = 256 (与 I-47 G_MAX_SOCKETS 初始 DEFAULT_MAX_SOCKETS 一致)

use std::fs;
use std::path::Path;

const NET_INIT: &str = "src/kernel/framework/net/init.rs";
// B04-09 (2026-08-25): MAX_SOCKETS/SOCKET_STORAGE 定义随拆分移至
// init/sockets.rs, init.rs 经 `pub use sockets::*` re-export.
const NET_SOCKETS: &str = "src/kernel/framework/net/init/sockets.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", path))
}

/// 合并读取 init.rs 与 init/sockets.rs 源码 (socket 容量配置所在位置).
fn read_socket_sources() -> String {
    let mut src = read(NET_INIT);
    src.push_str(&read(NET_SOCKETS));
    src
}

#[test]
fn test_max_sockets_is_256() {
    let src = read_socket_sources();
    // 验: MAX_SOCKETS 必须为 256
    assert!(
        src.contains("const MAX_SOCKETS: usize = 256;"),
        "TD-06: MAX_SOCKETS 必须为 256"
    );
    // 验: 注释必须提示用户改本值后须同步相关尺寸
    assert!(
        src.contains("SOCKET_STORAGE"),
        "TD-06: 注释必须提示改 MAX_SOCKETS 后须同步 SOCKET_STORAGE 尺寸"
    );
}

#[test]
fn test_max_sm_fd_derives_from_fdplan() {
    let src = read(NET_INIT);
    // MAX_SM_FD 必须仍从 FdPlan::SMOLTCP.capacity 派生 (TD-02 V3)
    assert!(src.contains("const MAX_SM_FD: usize = crate::framework::proc::fd_alloc::FdPlan::SMOLTCP.capacity as usize")
            || src.contains("const MAX_SM_FD: usize = crate::framework::proc::FdPlan::SMOLTCP.capacity as usize")
            || src.contains("const MAX_SM_FD: usize = crate::services::proc::FdPlan::SMOLTCP.capacity as usize"),
        "TD-06: MAX_SM_FD 必须仍从 FdPlan::SMOLTCP.capacity 派生 (TD-02 V3 一致性)");
}
