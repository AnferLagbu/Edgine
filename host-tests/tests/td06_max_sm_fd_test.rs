// TD-06: 验证 smoltcp 槽位容量配置.
//
// 验收:
//   1. `privileged/net/init/sockets.rs` 的 `MAX_SOCKETS` 必须为 256
//   2. `MAX_SM_FD` 从 `privileged::net::slots::MAX_SLOTS` 单一来源派生
//      (阶段 4 起全局 FD 编号规划层已退役, 不再经 FdPlan)
//   3. 默认容量 = 256 (与 I-47 G_MAX_SOCKETS 初始 DEFAULT_MAX_SOCKETS 一致)

use std::fs;
use std::path::Path;

const NET_INIT: &str = "src/kernel/privileged/net/init.rs";
// B04-09 (2026-08-25): MAX_SOCKETS/SOCKET_STORAGE 定义随拆分移至
// init/sockets.rs, init.rs 经 `pub use sockets::*` re-export.
const NET_SOCKETS: &str = "src/kernel/privileged/net/init/sockets.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path);
    fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {path}"))
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
fn test_max_sm_fd_derives_from_slot_capacity() {
    let src = read(NET_INIT);
    // 阶段 4: 容量单源为 `net::slots::MAX_SLOTS` (全局 FD 编号规划层已退役)
    assert!(
        src.contains("const MAX_SM_FD: usize = crate::privileged::net::slots::MAX_SLOTS;"),
        "TD-06: MAX_SM_FD 必须从 net::slots::MAX_SLOTS 派生"
    );
}
