//! egfs: 持久化往返集成测试
//!
//! 追踪: I-05
//! SPDX-License-Identifier: MPL-2.0
//!
//! 验证 EGFS 的内存持久化往返行为:
//! 1. 写入一批文件 → sync
//! 2. 验证可读
//!
//! ## B08-14 迁移 (2026-09-06)
//! 改引内核 `functions::fs::egfs` 真实实现 (host-test feature 暴露), 消除
//! 平行实现依赖. 测试版 `EGFS_DATA` 为 `Mutex<Option<Box>>` 可重置, 内核为
//! `OnceCell` 不可重置 (用户决策: 移除重置用例). 原 Phase 3 (单例重置) /
//! Phase 4 (重新 init 验证文件消失) 删除, 改为验证"已写文件可读 + 重复 init
//! 幂等不破坏数据". pwm 参数使用注册身份 (`identity::get_table().create(..., 0)`,
//! creator=0 得最高特权级).

use edgine::kernel::functions::fs::egfs::egfs_data::get_egfs;
use edgine::kernel::privileged::sgeg::identity;
use std::sync::OnceLock;

/// 注册并缓存一个测试身份 (creator=0 → 最高特权级), 供所有用例作为 pwm 参数.
fn test_pwm() -> u64 {
    static PWM: OnceLock<u64> = OnceLock::new();
    *PWM.get_or_init(|| {
        identity::get_table()
            .create("test-pw", "egfs-persist", 0)
            .expect("注册测试身份失败")
    })
}

#[test]
fn egfs_persistence_roundtrip() {
    println!("\n=== EGFS Persistence Roundtrip (Memory) ===\n");

    let egfs = get_egfs();

    println!("--- Phase 1: Init, write files, sync ---");
    egfs.init();
    assert!(egfs.is_initialized(), "should be initialized");

    let pwm = test_pwm();
    for i in 0..5 {
        let name = format!("/file_{i}");
        let fd = egfs.open(&name, 0x0102, pwm).unwrap();
        let data = format!("data for file {i}");
        assert_eq!(
            egfs.write(
                fd.cast_unsigned(),
                data.as_bytes(),
                u32::try_from(data.len()).expect("测试值必须能无损收窄")
            ),
            i32::try_from(data.len()).expect("测试值必须能无损收窄")
        );
        egfs.close(fd.cast_unsigned());
    }
    egfs.mkdir("/docs", pwm);
    let fd = egfs.open("/docs/readme.txt", 0x0102, pwm).unwrap();
    let c = b"Persist test data";
    assert_eq!(
        egfs.write(
            fd.cast_unsigned(),
            c,
            u32::try_from(c.len()).expect("测试值必须能无损收窄")
        ),
        i32::try_from(c.len()).expect("测试值必须能无损收窄")
    );
    egfs.close(fd.cast_unsigned());

    assert_eq!(egfs.sync(), 0, "sync should succeed");
    println!("  Files written and synced ✓");

    println!("--- Phase 2: Verify pre-reset ---");
    for i in 0..5 {
        let name = format!("/file_{i}");
        let fd = egfs.open(&name, 0x0001, pwm).unwrap().cast_unsigned();
        let mut buf = [0u8; 64];
        let r = egfs.read(fd, &mut buf, 64);
        let s = std::str::from_utf8(&buf[..usize::try_from(r).expect("测试值必须为非负")]).unwrap();
        assert_eq!(s, format!("data for file {i}"));
        egfs.close(fd);
    }
    println!("  All files readable ✓");

    println!("--- Phase 3: Re-init is idempotent (data preserved) ---");
    // J-03 (2026-09-08, G-10 方案 C): init 幂等化 — 重复 init 不再重建 objset,
    // 数据保留. 显式重建仅经 reset() (FREG恢复钩子) 触发, 此处不调用 reset.
    // 原断言 (重复 init 后旧文件不可读) 已随 G-10 修复更新为数据保留语义.
    egfs.init();
    assert!(egfs.is_initialized(), "re-init should keep initialized");
    assert!(
        egfs.open("/file_0", 0x0001, pwm).is_ok(),
        "重复 init 幂等, 数据保留 (旧文件仍可读)"
    );
    println!("  Re-init idempotent, data preserved ✓");

    println!("\n=== Persistence Roundtrip Passed ===\n");
}
