//! I-29 补充验收: 权限矩阵 16 domain 全覆盖
//!
//! 原镜像内核 [src/kernel/framework/credo/capability.rs] 的 16 domain 矩阵契约,
//! 现改引内核 `services::credo::policy` 真实实现 (host-test feature 暴露).
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 CapBits / CapabilityMatrix / VIABLE_FLOOR 平行实现, 改引内核
//! `services::credo::policy::{CapBits, CapDomain, CapMatrix, InMemoryMatrix, VIABLE_FLOOR}`
//! 与 `services::credo::capability` 能力位常量; 原 `#![allow(dead_code)]` (F9 违规)
//! 随常量表删除而消失.
//!
//! ## 因内核 host 不可测已移除 (current_pwm 部分)
//! 原镜像 `services/fs/mount.rs::current_pwm` (pwm==0 → EACCES) 与
//! `framework/credo/api.rs::pwm_has_capability` 简化判定已移除:
//! - 内核 `current_pwm()` 为私有函数 (`fn`), 依赖 `credo::api::pwm_get_current()`
//!   (读取当前进程凭证, host 上无进程上下文), 无法直接调用;
//! - 且真实内核语义与镜像相反: `engine::check(0, ...) == true` (pwm==0 是
//!   bootstrap 全权身份), 不存在"pwm==0 → EACCES"的拦截逻辑 (见 framework/credo/engine.rs).
//! - 内核 `services/fs/mount.rs::mount_syscall` 完整路径依赖 VFS 全局状态, host 不可测.
//!
//! ## 覆盖
//! 1. 16 个 domain 全部参与 (grant/revoke/has/superset 路径全覆盖)
//! 2. viable floor 的 FS_READ|FS_EXECUTE + PROC_FORK|PROC_EXEC 不变
//!    (内核 policy::VIABLE_FLOOR 追加 USER_MGMT::LIST, 与 capability::VIABLE_FLOOR 不同)
//! 3. 越界 domain 静默失败

use queenx::kernel::services::credo::capability::{
    DEVICE_CAP_IRQ, DEVICE_CAP_MMIO, FS_CAP_CREATE, FS_CAP_EXECUTE, FS_CAP_READ, FS_CAP_WRITE,
    NET_CAP_RECV, NET_CAP_SEND, PROC_CAP_EXEC, PROC_CAP_FORK, PROC_CAP_KILL, SYS_CAP_ALL,
    USER_MGMT_CAP_CREATE, USER_MGMT_CAP_LIST,
};
use queenx::kernel::services::credo::policy::{
    CAP_DOMAINS, CapBits, CapDomain, CapMatrix, CapabilityMatrix, InMemoryMatrix, VIABLE_FLOOR,
};

// =====================================================================
// 16 domain 权限矩阵
// =====================================================================

#[test]
fn matrix_has_16_domains() {
    // 编译期: 16 domain 常量 (最后一个 RESERVED = 15)
    assert_eq!(CapDomain::RESERVED.0, 15);
    // 运行时: viable floor 长度 = 16
    assert_eq!(VIABLE_FLOOR.len(), CAP_DOMAINS);
    assert_eq!(CAP_DOMAINS, 16);
}

#[test]
fn matrix_viable_floor_is_minimal() {
    let cm = CapMatrix::from_bits(VIABLE_FLOOR);
    // FS domain
    assert!(cm.get(CapDomain::FS).contains(CapBits(FS_CAP_READ)));
    assert!(cm.get(CapDomain::FS).contains(CapBits(FS_CAP_EXECUTE)));
    assert!(!cm.get(CapDomain::FS).contains(CapBits(FS_CAP_WRITE)));
    assert!(!cm.get(CapDomain::FS).contains(CapBits(FS_CAP_CREATE)));
    // PROC domain
    assert!(cm.get(CapDomain::PROC).contains(CapBits(PROC_CAP_FORK)));
    assert!(cm.get(CapDomain::PROC).contains(CapBits(PROC_CAP_EXEC)));
    assert!(!cm.get(CapDomain::PROC).contains(CapBits(PROC_CAP_KILL)));
    // 内核差异: policy::VIABLE_FLOOR 追加 USER_MGMT::LIST (identity::create 初始化用
    // capability::VIABLE_FLOOR 无此位; 此处以 policy 权威实现为准)
    assert!(
        cm.get(CapDomain::USER_MGMT)
            .contains(CapBits(USER_MGMT_CAP_LIST))
    );
    // 其余 13 domain 全部为 0
    for d in [
        CapDomain::SYSTEM,
        CapDomain::NET,
        CapDomain::DEVICE,
        CapDomain::IPC,
        CapDomain::MEM,
        CapDomain::TIME,
        CapDomain::BARRIER,
        CapDomain::SIGNAL,
        CapDomain::SHM,
        CapDomain::SEM,
        CapDomain::MSGQ,
        CapDomain::DMA,
        CapDomain::RESERVED,
    ] {
        assert!(
            cm.get(d).is_empty(),
            "domain {:?} 在 viable floor 必须为 0",
            d
        );
    }
}

#[test]
fn matrix_all_grants_every_domain() {
    let cm = CapMatrix::all();
    for d in 0..16u8 {
        assert!(
            cm.get(CapDomain(d)).contains(CapBits(SYS_CAP_ALL)),
            "domain {} 应有全权",
            d
        );
    }
}

#[test]
fn matrix_grant_revoke_isolates_domains() {
    let cm = InMemoryMatrix::new();
    // 内核 InMemoryMatrix::set 为覆盖写, grant/revoke 通过 现值 | bits / 现值 & !bits 表达
    let grant = |cm: &InMemoryMatrix, d: CapDomain, bits: u64| {
        cm.set(d, cm.get(d).unwrap() | CapBits(bits)).unwrap();
    };
    let revoke = |cm: &InMemoryMatrix, d: CapDomain, bits: u64| {
        cm.set(d, cm.get(d).unwrap().diff(CapBits(bits))).unwrap();
    };

    grant(&cm, CapDomain::FS, FS_CAP_READ);
    grant(&cm, CapDomain::NET, NET_CAP_SEND);
    grant(&cm, CapDomain::PROC, PROC_CAP_FORK);
    grant(&cm, CapDomain::DEVICE, DEVICE_CAP_MMIO);
    grant(&cm, CapDomain::USER_MGMT, USER_MGMT_CAP_LIST);

    // FS
    assert!(
        cm.get(CapDomain::FS)
            .unwrap()
            .contains(CapBits(FS_CAP_READ))
    );
    assert!(
        !cm.get(CapDomain::FS)
            .unwrap()
            .contains(CapBits(FS_CAP_WRITE))
    );
    // NET
    assert!(
        cm.get(CapDomain::NET)
            .unwrap()
            .contains(CapBits(NET_CAP_SEND))
    );
    assert!(
        !cm.get(CapDomain::NET)
            .unwrap()
            .contains(CapBits(NET_CAP_RECV))
    );
    // PROC
    assert!(
        cm.get(CapDomain::PROC)
            .unwrap()
            .contains(CapBits(PROC_CAP_FORK))
    );
    assert!(
        !cm.get(CapDomain::PROC)
            .unwrap()
            .contains(CapBits(PROC_CAP_EXEC))
    );
    // DEVICE
    assert!(
        cm.get(CapDomain::DEVICE)
            .unwrap()
            .contains(CapBits(DEVICE_CAP_MMIO))
    );
    assert!(
        !cm.get(CapDomain::DEVICE)
            .unwrap()
            .contains(CapBits(DEVICE_CAP_IRQ))
    );
    // USER_MGMT
    assert!(
        cm.get(CapDomain::USER_MGMT)
            .unwrap()
            .contains(CapBits(USER_MGMT_CAP_LIST))
    );
    assert!(
        !cm.get(CapDomain::USER_MGMT)
            .unwrap()
            .contains(CapBits(USER_MGMT_CAP_CREATE))
    );

    // 撤销验证
    revoke(&cm, CapDomain::FS, FS_CAP_READ);
    assert!(
        !cm.get(CapDomain::FS)
            .unwrap()
            .contains(CapBits(FS_CAP_READ))
    );
    revoke(&cm, CapDomain::NET, NET_CAP_SEND);
    assert!(
        !cm.get(CapDomain::NET)
            .unwrap()
            .contains(CapBits(NET_CAP_SEND))
    );
}

#[test]
fn matrix_out_of_range_is_silent() {
    // 内核差异: InMemoryMatrix::set 对越界 domain 返回 Err (不静默忽略), 矩阵内容不变
    let cm = InMemoryMatrix::new();
    assert!(cm.set(CapDomain(16), CapBits(0xFF)).is_err());
    assert!(cm.set(CapDomain(255), CapBits(0xFF)).is_err());
    assert_eq!(cm.get(CapDomain(16)), None);
    assert_eq!(cm.get(CapDomain(255)), None);
    // 不影响现有 domain
    for d in 0..16u8 {
        assert_eq!(cm.get(CapDomain(d)), Some(CapBits::NONE));
    }
}

#[test]
fn matrix_superset_transitivity() {
    let root = CapMatrix::all();
    let admin = InMemoryMatrix::new();
    // admin: FS READ|WRITE|EXECUTE + PROC FORK|EXEC|KILL + USER_MGMT LIST
    // (USER_MGMT LIST 是内核 policy VIABLE_FLOOR 下界, admin 必须含之才能是全矩阵 superset)
    admin
        .set(
            CapDomain::FS,
            CapBits(FS_CAP_READ | FS_CAP_WRITE | FS_CAP_EXECUTE),
        )
        .unwrap();
    admin
        .set(
            CapDomain::PROC,
            CapBits(PROC_CAP_FORK | PROC_CAP_EXEC | PROC_CAP_KILL),
        )
        .unwrap();
    admin
        .set(CapDomain::USER_MGMT, CapBits(USER_MGMT_CAP_LIST))
        .unwrap();
    let user = CapMatrix::from_bits(VIABLE_FLOOR);

    let superset = |a: &dyn Fn(CapDomain) -> CapBits, b: &dyn Fn(CapDomain) -> CapBits| {
        (0..16u8).all(|d| a(CapDomain(d)).contains(b(CapDomain(d))))
    };
    let root_get = |d: CapDomain| root.get(d);
    let admin_get = |d: CapDomain| admin.get(d).unwrap();
    let user_get = |d: CapDomain| user.get(d);

    assert!(superset(&root_get, &admin_get));
    assert!(superset(&root_get, &user_get));
    assert!(superset(&admin_get, &user_get));
    assert!(!superset(&user_get, &admin_get));
    assert!(!superset(&user_get, &root_get));
}

#[test]
fn matrix_cap_bits_isolated() {
    let mut cb = CapBits(FS_CAP_READ | FS_CAP_WRITE);
    cb = cb | CapBits(FS_CAP_EXECUTE);
    assert!(cb.contains(CapBits(FS_CAP_READ)));
    assert!(cb.contains(CapBits(FS_CAP_WRITE)));
    assert!(cb.contains(CapBits(FS_CAP_EXECUTE)));
    cb = cb.diff(CapBits(FS_CAP_WRITE));
    assert!(cb.contains(CapBits(FS_CAP_READ)));
    assert!(!cb.contains(CapBits(FS_CAP_WRITE)));
    assert!(cb.contains(CapBits(FS_CAP_EXECUTE)));
}

#[test]
fn matrix_grant_idempotent_and_revoke_noop() {
    let mut cb = CapBits(FS_CAP_READ);
    cb = cb | CapBits(FS_CAP_READ);
    assert_eq!(cb.0, FS_CAP_READ);
    cb = cb.diff(CapBits(FS_CAP_WRITE));
    assert_eq!(cb.0, FS_CAP_READ);
}

#[test]
fn matrix_cap_bit_superset_reflexive() {
    let a = CapBits(FS_CAP_READ | FS_CAP_WRITE);
    let b = CapBits(FS_CAP_READ);
    assert!(a.contains(b));
    assert!(a.contains(a));
    assert!(!b.contains(a));
}

#[test]
fn all_16_domains_covered_by_viable_or_zero() {
    // 不变量: viable floor 中每个 domain 都有定义位 (可以为 0)
    // 这是 16 domain 全覆盖的硬性契约
    let cm = CapMatrix::from_bits(VIABLE_FLOOR);
    let mut nonzero_domains = 0;
    for d in 0..16u8 {
        // 任何 domain 都必须能 get 查询, 0 位也算覆盖
        let _ = cm.get(CapDomain(d));
        if !cm.get(CapDomain(d)).is_empty() {
            nonzero_domains += 1;
        }
    }
    // 内核 policy VIABLE_FLOOR 非空域: FS / PROC / USER_MGMT
    assert!(
        nonzero_domains >= 3,
        "viable floor 应至少有 FS/PROC/USER_MGMT 非空, 实际 = {}",
        nonzero_domains
    );
    // 16 个 domain 全部 0..=15 可寻址 (set/get 不应越界)
    for d in 0..16u8 {
        let cm2 = InMemoryMatrix::new();
        cm2.set(CapDomain(d), CapBits(0x1)).unwrap();
        assert_eq!(cm2.get(CapDomain(d)), Some(CapBits(0x1)));
    }
}
