//! Socket 容量配置测试 (I-47)
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `MAX_SOCKETS` / `DEFAULT_MAX_SOCKETS` 常量与 `configure_max_sockets` /
//! `get_max_sockets` / `set_max_sockets` (参数化 `&AtomicUsize`) 平行实现,
//! 改引内核真实源码 `edgine::kernel::privileged::net::init` (net/init/sockets.rs):
//! - `MAX_SOCKETS` (pub const 256) / `configure_max_sockets` / `get_max_sockets` /
//!   `set_max_sockets` (pub, 操作全局 `G_MAX_SOCKETS` AtomicUsize, host 无硬件依赖)
//! - `DEFAULT_MAX_SOCKETS` (1024) 为内核私有常量, 测试侧不再镜像 (行为经
//!   `configure_max_sockets` 生效值 = MAX_SOCKETS 验证: 1024 > 256 截断).
//!
//! 内核实现操作**全局** `G_MAX_SOCKETS` (非参数化), 测试在并行线程间共享 →
//! 全部调参用例合并为单个顺序测试函数避免互踩.

use edgine::kernel::privileged::net::init::{
    MAX_SOCKETS, configure_max_sockets, get_max_sockets, set_max_sockets,
};

/// 内核配置/调参语义 (顺序执行, 共享全局 G_MAX_SOCKETS)
#[test]
fn max_sockets_config_semantics() {
    // 默认值 1024 > 上限 256 → configure 截断为 MAX_SOCKETS
    configure_max_sockets();
    assert_eq!(
        get_max_sockets(),
        MAX_SOCKETS,
        "DEFAULT(1024) > MAX(256) 应截断为 MAX"
    );

    // set(0) 拒绝, 返回当前值, 状态不变
    let current = get_max_sockets();
    assert_eq!(set_max_sockets(0), current, "0 应被拒绝并返回当前值");
    assert_eq!(get_max_sockets(), current);

    // set(n > MAX) 截断为 MAX
    assert_eq!(set_max_sockets(MAX_SOCKETS * 10), MAX_SOCKETS);
    assert_eq!(get_max_sockets(), MAX_SOCKETS);

    // set 合法区间内生效
    assert_eq!(set_max_sockets(64), 64);
    assert_eq!(get_max_sockets(), 64);
    assert_eq!(set_max_sockets(128), 128);
    assert_eq!(get_max_sockets(), 128);

    // 边界: 1
    assert_eq!(set_max_sockets(1), 1);
    assert_eq!(get_max_sockets(), 1);

    // 边界: 恰好 MAX / MAX+1
    assert_eq!(set_max_sockets(MAX_SOCKETS), MAX_SOCKETS);
    assert_eq!(get_max_sockets(), MAX_SOCKETS);
    assert_eq!(set_max_sockets(MAX_SOCKETS + 1), MAX_SOCKETS);
    assert_eq!(get_max_sockets(), MAX_SOCKETS);
}

/// 内核 MAX_SOCKETS 编译期常量 = 256
#[test]
fn max_sockets_const_is_256() {
    assert_eq!(
        MAX_SOCKETS, 256,
        "内核 net/init/sockets.rs MAX_SOCKETS = 256"
    );
}

#[test]
fn test_active_socket_counting() {
    // 模拟 sm_socket 中的活动 socket 计数
    let fd_types = [0u8, 1, 2, 0, 1, 0, 2, 1];
    let active: usize = fd_types.iter().filter(|&&t| t != 0).count();
    assert_eq!(active, 5);
}

#[test]
#[allow(clippy::assertions_on_constants)] // 编译期回归断言, 故意用常量
fn test_old_hardcoded_limit_was_8() {
    // 文档化回归: 旧硬编码 = 8 (8 个并发 socket).
    // 修复后默认 256 (32x), 编译期可调.
    // 本测试仅作回归记录, 不在运行时检查.
    const OLD_MAX_SOCKETS: usize = 8;
    assert!(MAX_SOCKETS >= OLD_MAX_SOCKETS * 8); // 至少 8 倍
}
