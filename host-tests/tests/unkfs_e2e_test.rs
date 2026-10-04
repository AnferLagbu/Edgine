//! I-05: UNKFS 端到端集成测试 (host 端)
//!
//! 验证 maintenance-2026-06-11.md I-05 验收:
//!   1. 格式化 → 创建文件 → 写 → 快照 → 内容验证
//!   2. 写文件 → 崩溃 → 重启 → ZIL 重放
//!   3. 创建 N 个文件 → 扫描延迟 < 1s
//!
//! 注: host 端使用 host-tests/src/unkfs/ 的 mock 数据结构 (与 kernel 内
//! 数据结构同形状, 但 std::sync::Mutex + std::vec), 不依赖真实块设备.
//! 真实端到端 (QEMU) 见 tests/integration/run_integration_tests.py.
//!
//! Mock 语义契约:
//!   - UnkfsDataset::create_file: 分配 objset id 并插入 dir_zap (若 cap 满则静默失败)
//!   - UnkfsZil::add_record: 追加到 records, 分配递增 seq
//!   - UnkfsZil::commit(txg): 移除所有 txg <= txg 的 records
//!   - UnkfsZil::replay(): 返回当前 records (模拟"重启后重放未提交日志")
//!
//! ## B08-14 迁移 (2026-09-06)
//! 改引内核 `functions::fs::unkfs` 真实实现 (host-test feature 暴露), 消除
//! 平行实现依赖. API 与测试版同构 (UnkfsDataset/UnkfsSnapshotManager/UnkfsZil/UnkfsZap/
//! UnkfsDva/UnkfsBlockPointer), 仅 import 路径变化.

use edgine::kernel::functions::fs::unkfs::bp::UnkfsDva;
use edgine::kernel::functions::fs::unkfs::dataset::UnkfsDataset;
use edgine::kernel::functions::fs::unkfs::snapshot::UnkfsSnapshotManager;
use edgine::kernel::functions::fs::unkfs::zap::UnkfsZap;
use edgine::kernel::functions::fs::unkfs::zil::{UnkfsZil, UnkfsZilRecord, UnkfsZilRecordType};
use std::time::Instant;

const ROOT_OWNER: u64 = 0;

fn fresh_zil() -> UnkfsZil {
    let zil = UnkfsZil::new();
    zil.init();
    zil
}

fn fresh_dataset(name: &str) -> UnkfsDataset {
    let ds = UnkfsDataset::new(1, name, ROOT_OWNER);
    ds.init(ROOT_OWNER);
    ds
}

// ========================================================================
// 测试 1: 格式化 → 创建文件 → 写 → 快照 → 内容验证
// ========================================================================

#[test]
fn e2e_format_write_snapshot_restore() {
    // 1) 格式化: 创建根 dataset + ZIL
    let zil = fresh_zil();
    let root = fresh_dataset("edgine-root");

    // 2) 创建文件
    let file_obj = root
        .create_file("hello.txt", ROOT_OWNER)
        .expect("create_file should succeed");
    assert!(file_obj > 0, "obj_id 应当 > 0");

    // 3) dir_zap 可见: create_file 后 lookup 应能找到
    assert_eq!(root.lookup("hello.txt"), Some(file_obj));

    // 4) 写入 (txg 1, 模拟已 fsync 但不 commit 到 ZIL)
    let payload_size: u32 = 16;
    zil.add_record(UnkfsZilRecord::new_write(1, file_obj, 0, payload_size));

    // 5) 创建快照
    let snap_mgr = UnkfsSnapshotManager::new();
    let snap_id = snap_mgr
        .create_snapshot(&root, "before-restore", 1)
        .expect("create_snapshot should succeed");
    assert!(snap_id > 0, "snap_id 应当 > 0");

    // 6) 验证快照存在
    let snaps = snap_mgr.snapshots.lock();
    assert_eq!(snaps.len(), 1);
    let snap = &snaps[0];
    assert!(!snap.is_clone);
    assert_eq!(snap.ds_id, root.ds_id);
    assert_eq!(snap.birth_txg, 1);
    drop(snaps);

    // 7) 验证 ZIL 端到端: 至少 1 条 record, seq 单调递增
    let zil_recs = zil.records.lock();
    assert_eq!(zil_recs.len(), 1, "应有 1 条 ZIL 记录");
    assert_eq!(zil_recs[0].rec_type, UnkfsZilRecordType::Write);
    assert_eq!(zil_recs[0].obj_id, file_obj);
    assert_eq!(zil_recs[0].size, payload_size);
    assert!(zil_recs[0].seq > 0);

    // 8) "恢复" 验证: 销毁快照后, 文件仍存在 (真实场景: 快照是只读视图,
    //    销毁快照只释放引用, 不影响原 dataset 的活跃 objset)
    assert!(snap_mgr.destroy_snapshot(snap_id));
    assert!(root.lookup("hello.txt").is_some());
}

// ========================================================================
// 测试 2: 写文件 → 崩溃 → 重启 → ZIL replay 还原
// ========================================================================

#[test]
fn e2e_crash_zil_replay() {
    // 启动期: 干净的 ZIL + 数据集
    let zil = fresh_zil();
    let root = fresh_dataset("crash-root");

    // 1) 创建文件
    let f1 = root.create_file("a.txt", ROOT_OWNER).unwrap();
    let f2 = root.create_file("b.txt", ROOT_OWNER).unwrap();
    let f3 = root.create_file("c.txt", ROOT_OWNER).unwrap();

    // 2) 提交 txg 1: 3 条 write 写入并 commit (commit 会从 records 中移除它们)
    zil.add_record(UnkfsZilRecord::new_write(1, f1, 0, 100));
    zil.add_record(UnkfsZilRecord::new_write(1, f2, 0, 200));
    zil.add_record(UnkfsZilRecord::new_write(1, f3, 0, 300));
    zil.commit(1);
    assert_eq!(
        zil.records.lock().len(),
        0,
        "txg 1 commit 后 records 应清空"
    );
    assert_eq!(
        zil.committed_seq.load(std::sync::atomic::Ordering::Acquire),
        3
    );

    // 3) 模拟崩溃: txg 2 写入部分 ZIL 记录, 未 commit
    zil.add_record(UnkfsZilRecord::new_write(2, f1, 100, 50));
    zil.add_record(UnkfsZilRecord::new_write(2, f2, 200, 50));
    // (崩溃)

    // 4) "重启": replay() 返回未 commit 的记录
    let replayed = zil.replay();
    assert_eq!(replayed.len(), 2, "replay 应返回未 commit 的 2 条");
    assert!(replayed.iter().all(|r| r.txg == 2));
    assert!(replayed.iter().all(|r| r.seq > 0));

    // 5) replay 后可继续工作: 验证 ZIL 状态机无残留
    let objs: std::collections::HashSet<u64> = replayed.iter().map(|r| r.obj_id).collect();
    assert!(objs.contains(&f1));
    assert!(objs.contains(&f2));
    assert!(
        !objs.contains(&f3),
        "f3 已在 txg 1 commit, 不应出现在 replay 中"
    );

    // 6) replay 之后: 模拟应用 replayed 记录后, commit 到 txg 2
    zil.commit(2);
    assert_eq!(
        zil.records.lock().len(),
        0,
        "txg 2 commit 后 records 应清空"
    );
    let seq_after = zil.committed_seq.load(std::sync::atomic::Ordering::Acquire);
    assert!(
        seq_after >= 3,
        "committed_seq 至少为 3 (来自 txg 2 的 2 条 + txg 1 的 3 条 max)"
    );

    // 7) 继续追加: ZIL 不死锁, seq 继续推进
    let prev = zil.current_seq.load(std::sync::atomic::Ordering::Acquire);
    zil.add_record(UnkfsZilRecord::new_write(3, f1, 0, 10));
    let now = zil.current_seq.load(std::sync::atomic::Ordering::Acquire);
    assert!(now > prev, "ZIL 在 replay/commit 后仍可追加");
}

// ========================================================================
// 测试 3: 创建 N 个文件 → 扫描延迟 < 1s
// ========================================================================

#[test]
fn e2e_thousand_files_scan_latency() {
    let zil = fresh_zil();
    let mut root = fresh_dataset("perf-root");

    // 扩大 dir_zap 容量以容纳 1000 个文件 (默认 UnkfsZap::new() 容量 256)
    root.dir_zap = UnkfsZap::with_capacity(2048);

    // 1) 创建 1000 个文件
    let n = 1000usize;
    let mut obj_ids = Vec::with_capacity(n);
    for i in 0..n {
        let name = format!("file_{:04}.dat", i);
        let obj = root
            .create_file(&name, ROOT_OWNER)
            .unwrap_or_else(|| panic!("create_file {} 失败", i));
        obj_ids.push(obj);
        zil.add_record(UnkfsZilRecord::new_create(1, /* root obj */ 1, &name));
    }
    assert_eq!(obj_ids.len(), n);

    // 2) 扫描 dataset 目录: 1000 个 obj_id 都能在 dir_zap 找到
    let start = Instant::now();
    let mut found = 0usize;
    for (i, expected_obj) in obj_ids.iter().enumerate() {
        let name = format!("file_{:04}.dat", i);
        let looked_up = root.dir_zap.lookup_u64(&name);
        assert_eq!(
            looked_up,
            Some(*expected_obj),
            "file_{:04}.dat dir_zap 查找失败",
            i
        );
        found += 1;
    }
    let elapsed = start.elapsed();

    // 3) 性能预算: 1000 次扫描 < 1s
    assert_eq!(found, n, "所有 {} 个文件都应能被扫描到", n);
    assert!(
        elapsed.as_secs() < 1,
        "扫描 1000 文件耗时 {}s (> 1s, I-05 性能预算)",
        elapsed.as_secs_f64()
    );

    // 4) ZIL create 记录数验证
    let records = zil.records.lock();
    let create_count = records
        .iter()
        .filter(|r| r.rec_type == UnkfsZilRecordType::Create)
        .count();
    assert_eq!(create_count, n, "ZIL create 记录数应为 {}", n);
}

// ========================================================================
// 测试 4 (附加): BP 路径不变性 — 快照保存 root_bp, 验证引用稳定
// ========================================================================

#[test]
fn e2e_snapshot_root_bp_immutable() {
    let zil = fresh_zil();
    let root = fresh_dataset("bp-immutable");
    zil.add_record(UnkfsZilRecord::new_write(1, 1, 0, 64));
    zil.commit(1);

    // 写前 root_bp (默认 null)
    let pre_snap_bp = *root.root_bp.lock();
    assert!(pre_snap_bp.is_null());

    // 快照
    let snap_mgr = UnkfsSnapshotManager::new();
    let snap_id = snap_mgr.create_snapshot(&root, "v1", 1).unwrap();

    // 写入: 模拟 dataset 内部更新 root_bp (此处仅测试契约, 实际生产由 DMU 改)
    // UnkfsBlockPointer 是 repr(C) 公开字段. 真实 DMU 会写入 DVA/出生 txg 等.
    // 这里用 UnkfsDva::new 制造一个非空 DVA 来模拟"已被修改的 BP".
    {
        let mut bp = root.root_bp.lock();
        bp.dva[0] = UnkfsDva::new(
            /* vdev_id */ 1, /* offset */ 0x1000, /* asize */ 0x2000,
        );
        bp.birth_txg = 2;
    }

    // 快照的 root_bp 不应被更新 (它保存的是创建时的副本)
    let snaps = snap_mgr.snapshots.lock();
    let snap = snaps.iter().find(|s| s.snap_id == snap_id).unwrap();
    assert!(
        snap.root_bp.is_null(),
        "快照的 root_bp 必须保留创建时的引用, 后续 dataset 改动不影响快照"
    );
    // 当前 dataset 的 root_bp 已变化
    assert!(!root.root_bp.lock().is_null());
    // 引用关系: snapshot.ds_id == root.ds_id, 验证一致性
    assert_eq!(snap.ds_id, root.ds_id);
}
