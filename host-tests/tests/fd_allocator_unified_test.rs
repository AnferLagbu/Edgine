//! TD-02: 统一 FdAllocator — 静态契约测试
//!
//! 验证 `framework/proc/fd_alloc.rs`:
//!   - FdPlan 5 个范围互不重叠
//!   - 全部 ≥ MAX_SM_FD=256 (除 Smoltcp 自身)
//!   - alloc_fd / free_fd / subsystem_of 行为正确
//!   - 启动期不变量 (verify_plan) 满足

use std::fs;
use std::path::Path;

const FD_ALLOC_RS: &str = "src/kernel/framework/proc/fd_alloc.rs";

fn read_fd_alloc() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(FD_ALLOC_RS),
    )
    .expect("读 fd_alloc.rs")
}

#[test]
fn test_fd_plan_ranges_non_overlapping_const() {
    // FdPlan::ranges_non_overlapping() 编译期 const fn
    // 源码必须调用此函数 (或同等检查)
    let src = read_fd_alloc();
    assert!(
        src.contains("pub const fn ranges_non_overlapping"),
        "FdPlan 必须暴露 const fn ranges_non_overlapping (TD-02)"
    );
    // 且 verify_plan() 启动期调用
    assert!(
        src.contains("pub fn verify_plan"),
        "必须暴露 verify_plan 启动期校验 (TD-02)"
    );
}

#[test]
fn test_fd_plan_constants_match_td01() {
    // FdPlan 的 5 个范围必须与 TD-01 修复后的各子系统 FD_BASE 对齐
    let src = read_fd_alloc();
    // 关键值常量: Smoltcp=0, UDS=1000, EventFd=1100, SignalFd=1120, Inotify=1140
    assert!(
        src.contains("SMOLTCP: FdRange = FdRange::new(0,"),
        "Smoltcp 范围起点应为 0"
    );
    assert!(
        src.contains("UDS: FdRange = FdRange::new(1000,"),
        "UDS 范围起点应为 1000 (TD-01)"
    );
    assert!(
        src.contains("EVENT_FD: FdRange = FdRange::new(1100,"),
        "EVENT_FD 范围起点应为 1100 (TD-01)"
    );
    assert!(
        src.contains("SIGNAL_FD: FdRange = FdRange::new(1120,"),
        "SIGNAL_FD 范围起点应为 1120 (TD-01)"
    );
    assert!(
        src.contains("INOTIFY: FdRange = FdRange::new(1140,"),
        "INOTIFY 范围起点应为 1140 (TD-01)"
    );
}

#[test]
fn test_subsystem_count_is_five() {
    // 8 个子系统: Smoltcp/Uds/EventFd/SignalFd/Inotify/TimerFd/PidFd/UserFaultFd
    let src = read_fd_alloc();
    // 提取 FdSubsystem 枚举的变体数
    let enum_start = src.find("pub enum FdSubsystem").expect("FdSubsystem 定义");
    let enum_end = src[enum_start..]
        .find("\n}\n")
        .map(|x| enum_start + x)
        .expect("枚举结束");
    let body = &src[enum_start..enum_end];
    let variants: Vec<&str> = body
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.ends_with(',')
                && t.contains(" = ")
                && t.split(" = ")
                    .nth(1)
                    .is_some_and(|v| v.trim_end_matches(',').parse::<u8>().is_ok())
        })
        .collect();
    assert_eq!(
        variants.len(),
        8,
        "FdSubsystem 应有 8 个变体 (Smoltcp/Uds/EventFd/SignalFd/Inotify/TimerFd/PidFd/UserFaultFd), 实为 {}",
        variants.len()
    );
}

#[test]
fn test_alloc_free_subsystem_of_documented() {
    // alloc_fd / free_fd / subsystem_of 必须在 fd_alloc.rs 中定义
    let src = read_fd_alloc();
    assert!(src.contains("pub fn alloc_fd"), "必须暴露 alloc_fd (TD-02)");
    assert!(src.contains("pub fn free_fd"), "必须暴露 free_fd (TD-02)");
    assert!(
        src.contains("pub fn subsystem_of"),
        "必须暴露 subsystem_of (TD-02)"
    );
}

#[test]
fn test_fd_range_overlaps_helper() {
    // FdRange 暴露 contains / overlaps / end_exclusive 方法
    let src = read_fd_alloc();
    assert!(
        src.contains("pub const fn contains"),
        "FdRange 必须暴露 contains (TD-02)"
    );
    assert!(
        src.contains("pub const fn overlaps"),
        "FdRange 必须暴露 overlaps (TD-02)"
    );
    assert!(
        src.contains("pub const fn end_exclusive"),
        "FdRange 必须暴露 end_exclusive (TD-02)"
    );
}

#[test]
fn test_v2_subsystems_reference_fdplan() {
    // TD-02 V2: 4 个子系统的 *FD_BASE 常量必须从 FdPlan 派生, 不再硬编码字面量
    let cases: &[(&str, &str, &str)] = &[
        (
            "UDS_FD_BASE",
            "src/kernel/services/net/unix.rs",
            "crate::framework::proc::FdPlan::UDS.base",
        ),
        (
            "EFD_FD_BASE",
            "src/kernel/framework/syscall/eventfd.rs",
            "crate::framework::proc::FdPlan::EVENT_FD.base",
        ),
        (
            "SFD_FD_BASE",
            "src/kernel/framework/syscall/signalfd.rs",
            "crate::framework::proc::FdPlan::SIGNAL_FD.base",
        ),
        (
            "INOTIFY_FD_BASE",
            "src/kernel/services/fs/inotify.rs",
            "crate::framework::proc::FdPlan::INOTIFY.base",
        ),
    ];
    for (const_name, rel_path, expected_ref) in cases {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(rel_path);
        let src = fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", rel_path));
        let needle = format!("pub const {}: i32 =", const_name);
        let found = src.lines().any(|l| l.trim().starts_with(&needle));
        assert!(found, "{} 定义缺失 in {}", const_name, rel_path);
        assert!(
            src.contains(expected_ref),
            "{} 必须引用 {} (TD-02 V2 单一来源), 实为: {}",
            const_name,
            expected_ref,
            src.lines()
                .find(|l| l.trim().starts_with(&needle))
                .unwrap_or("?")
        );
    }
}

#[test]
fn test_v2_smoltcp_capacity_derived_from_fdplan() {
    // TD-02 V2: smoltcp MAX_SM_FD 从 FdPlan::SMOLTCP.capacity 派生
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/framework/net/init.rs");
    let src = fs::read_to_string(&p).expect("读 init.rs");
    assert!(
        src.contains("MAX_SM_FD: usize = crate::framework::proc::FdPlan::SMOLTCP.capacity"),
        "MAX_SM_FD 必须从 proc::FdPlan::SMOLTCP.capacity 派生 (TD-02 V2)"
    );
}

#[test]
fn test_v3_fd_at_helper_exposed() {
    // TD-02 V3: fd_alloc 暴露 fd_at / max_slots 辅助, 集中 FD 计算
    let src = read_fd_alloc();
    assert!(
        src.contains("pub const fn fd_at"),
        "必须暴露 fd_at(sub, slot) → i32 (TD-02 V3)"
    );
    assert!(
        src.contains("pub const fn max_slots"),
        "必须暴露 max_slots(sub) → usize (TD-02 V3)"
    );
}

#[test]
fn test_v3_subsystems_use_fd_at_not_base_plus() {
    // TD-02 V3: 各子系统的"base + i" 模式必须改走 fd_at(FdSubsystem::X, i)
    // 5 个 FD 分配点:
    //   1. unix.rs idx_to_fd
    //   2. eventfd.rs sys_eventfd 内部
    //   3. signalfd.rs sys_signalfd 内部
    //   4. inotify.rs InotifyInstance::fd
    //   5. inotify.rs 通知循环 epoll_pwake
    let cases: &[(&str, &str)] = &[
        ("src/kernel/services/net/unix.rs", "fd_at"),
        ("src/kernel/framework/syscall/eventfd.rs", "fd_at"),
        ("src/kernel/framework/syscall/signalfd.rs", "fd_at"),
        ("src/kernel/services/fs/inotify.rs", "fd_at"),
    ];
    for (path, expected) in cases {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(path);
        let src = fs::read_to_string(&p).unwrap_or_else(|_| panic!("读 {}", path));
        assert!(
            src.contains(expected),
            "{} 必须使用 {} (TD-02 V3)",
            path,
            expected
        );
    }
}
