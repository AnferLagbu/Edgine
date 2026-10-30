//! P6 / D11: 组播成员管理 (per-socket 引用计数) host-test 接线契约
//!
//! 内核内联单测 (`privileged/net/mcast.rs` 的 `#[cfg(test)]`) 已覆盖引用计数的
//! 全部算法分支; 本文件锁的是**接线与配置契约** —— 那些"编译得过但语义已坏"的
//! 回归点:
//!
//! 1. `mcast` 模块存在且导出编排所需的全部 API
//! 2. 组表容量与 `Cargo.toml` 的 smoltcp feature 一致 (容量漂移会静默丢组)
//! 3. smoltcp `multicast` feature 已启用 (整模块受该 feature 门控, 未启用时
//!    `join/leave_multicast_group` 根本不存在 —— D11 曾长期误判为已实装)
//! 4. sockopt 常量为 Linux/IANA 值域 (edgine-abi 单源, 锁位在 `abi_socket_constants_test`)
//! 5. `sm_setsockopt` 已把四个组播选项路由到 `mc_membership`
//! 6. `mc_membership` 的 iface/簿记次序铁律 (join 失败回滚 / leave 先簿记后拆)
//! 7. `sm_close` 拆组的"先取址后 `force_leave"顺序` (倒转会静默丢订阅)
//! 8. syscall 层 copy-in 容量足以承载 `struct ipv6_mreq` (20 字节)

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

const MCAST: &str = "src/kernel/privileged/net/mcast.rs";
const SM_FI: &str = "src/kernel/privileged/net/init/sm_fi.rs";
const RAW: &str = "src/kernel/privileged/net/init/raw.rs";
// P-71: smoltcp 依赖 (及其 feature 组合) 已随 functions 层独立成 crate 而迁到
// `functions/Cargo.toml`; glue 只做同名再导出, 自身不再声明 smoltcp. 本契约锁的是
// "实际消费 smoltcp 的那份 manifest", 故指向 functions.
const CARGO: &str = "src/kernel/functions/Cargo.toml";
const SYSCALL: &str = "src/kernel/privileged/net/syscall.rs";
const USERLIB: &str = "src/user/lib/src/sys.rs";

#[test]
fn mcast_module_exposes_required_api() {
    let src = read_src(MCAST);
    let required = [
        "pub const MC_MAX_GROUPS",
        "pub enum McError",
        "pub struct McastRegistry",
        "pub const fn new()",
        "pub fn allocate(&mut self, slots: usize)",
        "pub fn join(&mut self, slot: usize, addr: IpAddress) -> Result<bool, McError>",
        "pub fn unjoin(&mut self, slot: usize, addr: IpAddress)",
        "pub fn leave(&mut self, slot: usize, addr: IpAddress) -> Result<bool, McError>",
        "pub fn slot_bits(&self, slot: usize) -> u8",
        "pub fn addr_at(&self, idx: usize) -> Option<IpAddress>",
        "pub fn force_leave(&mut self, slot: usize, idx: usize) -> bool",
    ];
    for sig in required {
        assert!(src.contains(sig), "D11: mcast.rs 缺少 API `{sig}`");
    }
}

#[test]
fn group_table_capacity_matches_iface_feature() {
    let mcast = read_src(MCAST);
    let cargo = read_src(CARGO);
    // 登记表容量必须等于 iface 组表容量; 大于 → iface 先满 (谎报可加入), 小于 →
    // 本表先满 (提前拒绝合法请求). 两者都不报错, 只会静默减少可用组数.
    assert!(
        mcast.contains("pub const MC_MAX_GROUPS: usize = 8;"),
        "D11: MC_MAX_GROUPS 必须为 8, 与 Cargo.toml 的 iface-max-multicast-group-count-8 对齐"
    );
    assert!(
        cargo.contains("iface-max-multicast-group-count-8"),
        "D11: Cargo.toml 必须启用 iface-max-multicast-group-count-8"
    );
    // per-slot 成员位图以 u8 承载, 容量超过 8 会静默丢高位 —— 必须有编译期断言.
    assert!(
        mcast.contains("assert!(MC_MAX_GROUPS <= u8::BITS as usize)"),
        "D11: 缺少 MC_MAX_GROUPS <= u8::BITS 的编译期容量断言"
    );
}

#[test]
fn smoltcp_multicast_feature_is_enabled() {
    // 硬前提: smoltcp 的 `iface/interface/multicast.rs` 整体受 `multicast` feature
    // 门控 (含 `MulticastError` 的 re-export). 未启用时组播 API 不存在, 任何
    // "已实装"的记录都是错的.
    let cargo = read_src(CARGO);
    assert!(
        cargo.contains("\"multicast\""),
        "D11: smoltcp `multicast` feature 未启用 → 组播整模块不参与编译"
    );
}

#[test]
fn sockopt_constants_use_linux_abi() {
    // 方案 A 单源 (plan socket-option-constants-unification, 2026-10-19 裁定):
    // sockopt 常量值域由 edgine-abi 提供, 锁位断言在 abi_socket_constants_test;
    // 本处只验内核侧确实引用单源, 未再就地私定选项常量.
    let src = read_src(SM_FI);
    assert!(
        src.contains("use edgine_abi::socket"),
        "D11: sm_fi 应引用 edgine-abi 单源获取 sockopt 常量 (回归为就地 const 即漂移起点)"
    );
    // 内核内部件 (mreq 参数长度, 非跨层选项值域) 仍就地定义.
    for c in [
        "const IP_MREQ_LEN: u32 = 8;",
        "const IPV6_MREQ_LEN: u32 = 20;",
    ] {
        assert!(src.contains(c), "D11: 内核内部 mreq 长度常量偏离: `{c}`");
    }
    // 旧就地定义已移除 (否则两套同时存在 = 双写漂移复活).
    for gone in [
        "const IPPROTO_IPV6: i32 =",
        "const IP_ADD_MEMBERSHIP: i32 =",
    ] {
        assert!(
            !src.contains(gone),
            "D11: sm_fi 仍存在就地选项常量定义 `{gone}` (应迁 edgine-abi)"
        );
    }
}

#[test]
fn userlib_membership_constants_mirror_kernel() {
    // 方案 A 单源: userlib 的选项常量来自 edgine-abi re-export (与内核同源,
    // 天然镜像); 本处验引用关系 + mreq 结构体仍可构造.
    let user = read_src(USERLIB);
    assert!(
        user.contains("pub use edgine_abi::socket"),
        "D11: userlib 应 re-export edgine-abi 单源常量 (就地重定义 = 双写漂移复活)"
    );
    // mreq 结构体必须是 repr(C) 且尺寸与内核期望的 optlen 下限一致.
    assert!(
        user.contains("pub struct IpMreq") && user.contains("pub struct Ipv6Mreq"),
        "D11: userlib 缺少 mreq 结构体, 用户态无法发起成员管理"
    );
}

#[test]
fn setsockopt_routes_all_four_membership_options() {
    let src = read_src(SM_FI);
    // 四个选项必须都进入同一编排入口, 且先做 mreq 长度校验再取组地址.
    for opt in [
        "(IPPROTO_IP, IP_ADD_MEMBERSHIP) => Some((false, true))",
        "(IPPROTO_IP, IP_DROP_MEMBERSHIP) => Some((false, false))",
        "(IPPROTO_IPV6, IPV6_ADD_MEMBERSHIP) => Some((true, true))",
        "(IPPROTO_IPV6, IPV6_DROP_MEMBERSHIP) => Some((true, false))",
    ] {
        assert!(src.contains(opt), "D11: setsockopt 未路由组播选项: `{opt}`");
    }
    assert!(
        src.contains("return mc_membership(slot, addr, join);"),
        "D11: setsockopt 未经 mc_membership 入口 (绕开引用计数 = 语义破坏)"
    );
}

#[test]
fn membership_bridge_keeps_iface_bookkeeping_order() {
    let src = read_src(SM_FI);
    let start = src
        .find("fn mc_membership(")
        .expect("D11: mc_membership 入口不存在");
    let body = &src[start
        ..src
            .find("\n/// POSIX `setsockopt`")
            .expect("D11: 函数体边界异常")];

    // join: 簿记在前, iface 在后 (反序会让 iface 失败时留下无从回收的表项).
    let join_book = body
        .find("raw::mc_join")
        .expect("D11: mc_membership 未做 join 簿记");
    let join_iface = body
        .find("join_multicast_group")
        .expect("D11: mc_membership 未请求 iface join");
    assert!(
        join_book < join_iface,
        "D11: join 必须「先簿记后 iface」, 否则引用计数与组表会脱钩"
    );
    // iface join 失败必须回滚簿记 —— 谎报"已入组"会让后续 join 永久跳过 iface.
    assert!(
        body.contains("raw::mc_unjoin(slot, addr)"),
        "D11: iface join 失败路径缺少 mc_unjoin 回滚"
    );
    // leave: 仅引用归零 (dropped) 才触 iface, 否则会把其他 socket 的组拆掉.
    assert!(
        body.contains("if dropped && let Some(stack) = raw::stack_mut()"),
        "D11: leave 未做「引用归零才拆组」判定"
    );
    // 非组播地址必须先拒, 不让 iface 的 Unaddressable 参与回滚.
    assert!(
        body.contains("if !addr.is_multicast()"),
        "D11: 缺少 is_multicast 前置校验"
    );
}

#[test]
fn close_releases_membership_before_mutating_table() {
    let src = read_src(SM_FI);
    let start = src.find("fn sm_close(").expect("D11: sm_close 不存在");
    let body = &src[start..start + 3_000];

    assert!(
        body.contains("raw::mc_slot_bits(slot)"),
        "D11: sm_close 未拆离组成员资格 (socket 死后仍占组表 → 永久泄漏)"
    );
    // 顺序铁律: force_leave 使引用归零时会清空 groups[idx], 地址必须先取.
    let addr_pos = body
        .find("raw::mc_addr_at(idx)")
        .expect("D11: sm_close 未取组地址");
    let leave_pos = body
        .find("raw::mc_force_leave(slot, idx)")
        .expect("D11: sm_close 未做 force_leave");
    assert!(
        addr_pos < leave_pos,
        "D11: sm_close 必须先 mc_addr_at 再 mc_force_leave, 倒转会静默丢订阅"
    );
    // 仅最后一个成员退出才请求 iface 拆组.
    assert!(
        body.contains("leave_multicast_group"),
        "D11: sm_close 未向 iface 请求拆组"
    );
}

#[test]
fn raw_accessors_are_safe_wrappers_over_net_state() {
    let src = read_src(RAW);
    for sig in [
        "pub fn mc_join(slot: usize, addr: IpAddress) -> Result<bool, McError>",
        "pub fn mc_unjoin(slot: usize, addr: IpAddress)",
        "pub fn mc_leave(slot: usize, addr: IpAddress) -> Result<bool, McError>",
        "pub fn mc_slot_bits(slot: usize) -> u8",
        "pub fn mc_addr_at(idx: usize) -> Option<IpAddress>",
        "pub fn mc_force_leave(slot: usize, idx: usize) -> bool",
    ] {
        assert!(src.contains(sig), "D11: raw 缺少 safe accessor `{sig}`");
    }
    // 边界立场: raw 只做簿记, iface 编排在 sm_fi (functions 侧只见 safe API).
    // 只审计 mc_* accessor 的**非注释代码** (doc 注释必须提到 iface 才能说清次序).
    let block_start = src.find("pub fn mc_join").expect("D11: mc_join 不存在");
    let block = &src[block_start
        ..src
            .find("pub fn socket_handle")
            .expect("D11: accessor 块边界异常")];
    let code: String = block
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("stack_mut") && !code.contains("multicast_group"),
        "D11: iface 编排不应下沉到 raw accessor (errno 映射归 sm_fi)"
    );
}

#[test]
fn syscall_copyin_covers_ipv6_mreq() {
    let src = read_src(SYSCALL);
    let start = src
        .find("fn setsockopt_syscall(")
        .expect("D11: setsockopt_syscall 不存在");
    let body = &src[start..start + 2_000];
    assert!(
        body.contains("let mut buf = [0u8; 20];"),
        "D11: setsockopt copy-in 缓冲须容纳 struct ipv6_mreq (20 字节)"
    );
    assert!(
        body.contains("(valen as usize).min(20)"),
        "D11: setsockopt copy-in 长度上限须为 20, 否则 ipv6_mreq 被截断"
    );
}

#[test]
fn mcast_inline_tests_present() {
    let src = read_src(MCAST);
    let n = src.matches("#[test]").count();
    assert!(
        n >= 10,
        "D11: mcast.rs 内联单测不足 (当前 {n}, 要求 >= 10): 引用计数分支必须全覆盖"
    );
}
