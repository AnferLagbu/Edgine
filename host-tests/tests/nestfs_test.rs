//! NestFS 综合集成测试 (NestFS Comprehensive Integration Tests)
//!
//! 验证 NestFS 端到端 API 在典型工作负载下的正确性:
//! - 文件 / 目录 CRUD
//! - seek / read / write 边界
//! - 错误码 (NotFound / AlreadyExists / NotDirectory 等)
//! - 高级特性 (symlink / hardlink / xattr / chmod / chown)
//! - Snapshot & Clone
//! - FD 管理 (O_APPEND / 重复 close / 越界读)
//!
//! ## B08-14 迁移 (2026-09-06)
//! 改引内核 `services::fs::nestfs::nestfs_data` 真实实现 (host-test feature 暴露), 消除
//! 平行实现依赖. 主要差异:
//! - pwm 参数必须是已注册身份 (`identity::get_table().create(..., 0)` 返回哈希,
//!   creator=0 得最高特权级), 所有硬编码 pwm=1 替换为 `test_pwm()`.
//! - 错误码走内核 `KernelError::as_i32()` (负 errno): InvalidArgument=-22 /
//!   AlreadyExists=-17 / FileNotFound=-2 / Io=-5, 与测试版 mock 的裸 -1/-4 不同.
//! - 内核 write 推进 fd offset (真实 POSIX 语义); O_APPEND (0x0400) open 后
//!   fd offset 初始化为文件末尾.
//!
//! ## 测试组织
//! 集成测试置于 `tests/` 目录, 由 Cargo 自动发现. 通过
//! `use queenx::kernel::services::fs::nestfs::nestfs_data::get_nestfs` 访问内核
//! 暴露的 NestFS API.

use queenx::kernel::framework::credo::identity;
use queenx::kernel::framework::error::KernelError;
use queenx::kernel::services::fs::nestfs::dataset::NestDataset;
use queenx::kernel::services::fs::nestfs::nestfs_data::get_nestfs;
use std::sync::{Mutex, Once, OnceLock};

// 5 个 #[test] 并行共享 get_nestfs() 全局单例 (fd 表/文件状态), 需串行化
// 消除并行竞争 (偶发 seek/fd EINVAL flaky)。文件内锁, 不影响其他测试文件。
static NESTFS_TEST_LOCK: Mutex<()> = Mutex::new(());

static NESTFS_TEST_INIT: Once = Once::new();

fn ensure_nestfs_init() {
    NESTFS_TEST_INIT.call_once(|| {
        get_nestfs().init();
    });
}

/// 注册并缓存一个测试身份 (creator=0 → 最高特权级), 供所有用例作为 pwm 参数.
fn test_pwm() -> u64 {
    static PWM: OnceLock<u64> = OnceLock::new();
    *PWM.get_or_init(|| {
        identity::get_table()
            .create("test-pw", "nestfs-test", 0)
            .expect("注册测试身份失败")
    })
}

macro_rules! test {
    ($name:ident, $body:block) => {
        print!("  {} ... ", stringify!($name));
        $body
        println!("PASS");
    };
}

macro_rules! assert_eq_nestfs {
    ($left:expr, $right:expr, $msg:expr) => {
        let l = $left;
        let r = $right;
        if l != r {
            panic!("{} FAIL: expected {:?}, got {:?}", $msg, r, l);
        }
    };
}

#[test]
fn nestfs_comprehensive() {
    let _guard = NESTFS_TEST_LOCK.lock().unwrap();
    println!("\n=== NestFS Standalone Test Suite ===\n");

    let nestfs = get_nestfs();

    test!(init, {
        ensure_nestfs_init();
        assert!(nestfs.is_initialized(), "NestFS should be initialized");
    });

    test!(create_and_stat, {
        let fd = nestfs.open("/test.txt", 0x0102, test_pwm()).unwrap();
        assert!(fd >= 0, "open should succeed, got {}", fd);
        nestfs.close(fd as u32);
        let stat = nestfs.stat("/test.txt", test_pwm());
        assert!(stat.is_some(), "stat should find test.txt");
    });

    test!(write_and_read, {
        let fd = nestfs.open("/test.txt", 0x0102, test_pwm()).unwrap();
        let data = b"Hello, QueenX NestFS!";
        let written = nestfs.write(fd as u32, data, data.len() as u32);
        assert_eq_nestfs!(written, data.len() as i32, "write count");
        nestfs.close(fd as u32);

        // read 用新 open 的 0x0001 fd, offset 从 0 开始, 无需 seek
        let fd = nestfs.open("/test.txt", 0x0001, test_pwm()).unwrap();
        let mut buf = [0u8; 64];
        let read = nestfs.read(fd as u32, &mut buf, 64);
        assert!(read > 0, "read should return > 0, got {}", read);
        let read_str = core::str::from_utf8(&buf[..read as usize]).unwrap();
        assert_eq_nestfs!(read_str, "Hello, QueenX NestFS!", "file content");
        nestfs.close(fd as u32);
    });

    test!(mkdir, {
        let result = nestfs.mkdir("/mydir", test_pwm());
        assert!(result >= 0, "mkdir should succeed, got {}", result);
        let stat = nestfs.stat("/mydir", test_pwm());
        assert!(stat.is_some(), "stat should find mydir");
        let obj = stat.unwrap();
        assert!(obj.obj_type as u8 == 2, "mydir should be directory type");
    });

    test!(create_file_in_dir, {
        let fd = nestfs
            .open("/mydir/nested.txt", 0x0102, test_pwm())
            .unwrap();
        let data = b"nested content";
        let w = nestfs.write(fd as u32, data, data.len() as u32);
        assert_eq_nestfs!(w, data.len() as i32, "nested write count");
        nestfs.close(fd as u32);
    });

    test!(read_nested, {
        let fd = nestfs
            .open("/mydir/nested.txt", 0x0001, test_pwm())
            .unwrap();
        let mut buf = [0u8; 64];
        let r = nestfs.read(fd as u32, &mut buf, 64);
        assert!(r > 0, "nested read should return > 0");
        let s = core::str::from_utf8(&buf[..r as usize]).unwrap();
        assert_eq_nestfs!(s, "nested content", "nested file content");
        nestfs.close(fd as u32);
    });

    test!(rename, {
        let r = nestfs.rename("/test.txt", "/renamed.txt", test_pwm());
        assert_eq_nestfs!(r, 0, "rename should succeed");

        let fd = nestfs.open("/renamed.txt", 0x0001, test_pwm()).unwrap();
        let mut buf = [0u8; 64];
        let r = nestfs.read(fd as u32, &mut buf, 64);
        assert!(r > 0, "renamed file should have content");
        nestfs.close(fd as u32);

        match nestfs.open("/test.txt", 0x0001, test_pwm()) {
            Err(_) => {}
            Ok(fd) => {
                nestfs.close(fd as u32);
                panic!("old name should not exist after rename");
            }
        }
    });

    test!(delete, {
        let r = nestfs.unlink("/renamed.txt", test_pwm());
        assert_eq_nestfs!(r, 0, "unlink should succeed");
        match nestfs.open("/renamed.txt", 0x0001, test_pwm()) {
            Err(_) => {}
            Ok(fd) => {
                nestfs.close(fd as u32);
                panic!("deleted file should not be openable");
            }
        }
    });

    test!(large_write, {
        let fd = nestfs.open("/large.bin", 0x0102, test_pwm()).unwrap();
        let pattern: Vec<u8> = (0..1024u16).flat_map(|i| i.to_le_bytes()).collect();
        let written = nestfs.write(fd as u32, &pattern, pattern.len() as u32);
        assert_eq_nestfs!(written, pattern.len() as i32, "large write count");
        nestfs.close(fd as u32);

        let fd = nestfs.open("/large.bin", 0x0001, test_pwm()).unwrap();
        let mut read_buf = vec![0u8; pattern.len()];
        let buf_len = read_buf.len();
        let r = nestfs.read(fd as u32, &mut read_buf, buf_len as u32);
        assert_eq_nestfs!(r, pattern.len() as i32, "large read count");
        assert_eq_nestfs!(&read_buf[..], &pattern[..], "large file content");
        nestfs.close(fd as u32);
    });

    test!(multiple_files, {
        for i in 0..10 {
            let name = format!("/multi_{}", i);
            let fd = nestfs.open(&name, 0x0102, test_pwm()).unwrap();
            let content = format!("file number {}", i);
            let w = nestfs.write(fd as u32, content.as_bytes(), content.len() as u32);
            let msg = format!("write multi_{}", i);
            assert_eq_nestfs!(w, content.len() as i32, msg);
            nestfs.close(fd as u32);
        }
        for i in 0..10 {
            let name = format!("/multi_{}", i);
            let stat = nestfs.stat(&name, test_pwm());
            assert!(stat.is_some(), "should find multi_{}", i);
        }
    });

    test!(overwrite, {
        let fd = nestfs.open("/overwrite.txt", 0x0102, test_pwm()).unwrap();
        let d1 = b"first version";
        nestfs.write(fd as u32, d1, d1.len() as u32);
        nestfs.close(fd as u32);

        let fd = nestfs.open("/overwrite.txt", 0x0102, test_pwm()).unwrap();
        let d2 = b"second version - longer content!";
        nestfs.write(fd as u32, d2, d2.len() as u32);
        nestfs.close(fd as u32);

        let fd = nestfs.open("/overwrite.txt", 0x0001, test_pwm()).unwrap();
        let mut buf = [0u8; 64];
        let r = nestfs.read(fd as u32, &mut buf, 64);
        let s = core::str::from_utf8(&buf[..r as usize]).unwrap();
        assert_eq_nestfs!(s, "second version - longer content!", "overwrite content");
        nestfs.close(fd as u32);
    });

    test!(open_nonexistent, {
        match nestfs.open("/nonexistent", 0x0001, test_pwm()) {
            Err(_) => {}
            Ok(fd) => {
                nestfs.close(fd as u32);
                panic!("open nonexistent should fail");
            }
        }
    });

    test!(stat_nonexistent, {
        let stat = nestfs.stat("/nonexistent", test_pwm());
        assert!(stat.is_none(), "stat nonexistent should return None");
    });

    println!("\n=== All 10 NestFS Tests Passed ===\n");
}

#[test]
fn nestfs_error_paths() {
    let _guard = NESTFS_TEST_LOCK.lock().unwrap();
    println!("\n=== NestFS Error Path Tests ===\n");

    let nestfs = get_nestfs();
    ensure_nestfs_init();

    test!(open_without_create_flag_returns_not_found, {
        match nestfs.open("/no_such_file.txt", 0x0001, test_pwm()) {
            Err(KernelError::FileNotFound) => {}
            Err(e) => panic!("expected FileNotFound, got {:?}", e),
            Ok(fd) => {
                nestfs.close(fd as u32);
                panic!("should fail with FileNotFound");
            }
        }
    });

    test!(open_with_create_flag_succeeds, {
        match nestfs.open("/new_file.txt", 0x0102, test_pwm()) {
            Ok(fd) => {
                nestfs.close(fd as u32);
            }
            Err(e) => panic!("open with O_CREAT should succeed, got {:?}", e),
        }
    });

    test!(close_invalid_fd, {
        // 内核差异: invalid fd 返回 InvalidArgument (-22, EINVAL), 测试版 mock 为 -1
        let r = nestfs.close(9999);
        assert_eq_nestfs!(
            r,
            -22,
            "close invalid fd should return -22 (InvalidArgument)"
        );
    });

    test!(read_invalid_fd, {
        // 内核差异: invalid fd 返回 InvalidArgument (-22, EINVAL), 测试版 mock 为 -1
        let mut buf = [0u8; 64];
        let r = nestfs.read(9999, &mut buf, 64);
        assert_eq_nestfs!(
            r,
            -22,
            "read invalid fd should return -22 (InvalidArgument)"
        );
    });

    test!(write_invalid_fd, {
        // 内核差异: invalid fd 返回 InvalidArgument (-22, EINVAL), 测试版 mock 为 -1
        let data = b"test";
        let r = nestfs.write(9999, data, data.len() as u32);
        assert_eq_nestfs!(
            r,
            -22,
            "write invalid fd should return -22 (InvalidArgument)"
        );
    });

    test!(unlink_nonexistent, {
        let r = nestfs.unlink("/no_such_file", test_pwm());
        assert_eq_nestfs!(r, -2, "unlink nonexistent should return -2");
    });

    test!(stat_nonexistent_returns_none, {
        let s = nestfs.stat("/definitely_not_here", test_pwm());
        assert!(s.is_none(), "stat nonexistent should return None");
    });

    test!(rename_source_not_found, {
        let r = nestfs.rename("/missing_src", "/dst", test_pwm());
        assert_eq_nestfs!(r, -2, "rename missing source should return -2");
    });

    test!(rename_target_exists, {
        let fd1 = nestfs.open("/rename_src.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd1 as u32, b"src", 3);
        nestfs.close(fd1 as u32);
        let fd2 = nestfs.open("/rename_dst.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd2 as u32, b"dst", 3);
        nestfs.close(fd2 as u32);
        // 内核差异: rename 目标已存在返回 AlreadyExists (-17, EEXIST), 测试版 mock 为 -4
        let r = nestfs.rename("/rename_src.txt", "/rename_dst.txt", test_pwm());
        assert_eq_nestfs!(
            r,
            -17,
            "rename to existing target should return -17 (AlreadyExists)"
        );
    });

    test!(seek_invalid_fd, {
        // 内核差异: invalid fd 返回 InvalidArgument (-22, EINVAL), 测试版 mock 为 -1
        let r = nestfs.seek(9999, 0, 0);
        assert_eq_nestfs!(
            r,
            -22,
            "seek invalid fd should return -22 (InvalidArgument)"
        );
    });

    test!(seek_set, {
        let fd = nestfs.open("/seek_test.txt", 0x0102, test_pwm()).unwrap();
        let data = b"0123456789ABCDEF";
        nestfs.write(fd as u32, data, data.len() as u32);
        nestfs.close(fd as u32);

        let fd = nestfs.open("/seek_test.txt", 0x0001, test_pwm()).unwrap();
        let pos = nestfs.seek(fd as u32, 4, 0);
        assert_eq_nestfs!(pos, 4, "seek SET to 4");
        let mut buf = [0u8; 4];
        let r = nestfs.read(fd as u32, &mut buf, 4);
        assert!(r > 0, "read after seek should succeed");
        assert_eq_nestfs!(&buf[..r as usize], b"4567", "read after seek content");
        nestfs.close(fd as u32);
    });

    test!(seek_cur, {
        let fd = nestfs
            .open("/seek_cur_test.txt", 0x0102, test_pwm())
            .unwrap();
        nestfs.write(fd as u32, b"ABCDEFGHIJ", 10);
        nestfs.close(fd as u32);

        let fd = nestfs
            .open("/seek_cur_test.txt", 0x0001, test_pwm())
            .unwrap();
        nestfs.seek(fd as u32, 2, 0);
        let pos = nestfs.seek(fd as u32, 3, 1);
        assert_eq_nestfs!(pos, 5, "seek CUR from 2 + 3 = 5");
        let mut buf = [0u8; 3];
        let r = nestfs.read(fd as u32, &mut buf, 3);
        assert!(r > 0);
        assert_eq_nestfs!(&buf[..r as usize], b"FGH", "read after seek CUR");
        nestfs.close(fd as u32);
    });

    test!(seek_end, {
        let fd = nestfs
            .open("/seek_end_test.txt", 0x0102, test_pwm())
            .unwrap();
        nestfs.write(fd as u32, b"HELLO", 5);
        nestfs.close(fd as u32);

        let fd = nestfs
            .open("/seek_end_test.txt", 0x0001, test_pwm())
            .unwrap();
        let pos = nestfs.seek(fd as u32, -2i64, 2);
        assert_eq_nestfs!(pos, 3, "seek END - 2 = 3");
        let mut buf = [0u8; 4];
        let r = nestfs.read(fd as u32, &mut buf, 4);
        assert!(r > 0);
        assert_eq_nestfs!(&buf[..r as usize], b"LO", "read after seek END");
        nestfs.close(fd as u32);
    });

    println!("\n=== NestFS Error Path Tests Passed ===\n");
}

#[test]
fn nestfs_advanced_features() {
    let _guard = NESTFS_TEST_LOCK.lock().unwrap();
    println!("\n=== NestFS Advanced Feature Tests ===\n");

    let nestfs = get_nestfs();
    ensure_nestfs_init();

    test!(symlink_create_and_readlink, {
        let fd = nestfs.open("/link_target.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"target data", 11);
        nestfs.close(fd as u32);

        let r = nestfs.symlink("/link_target.txt", "/my_symlink", test_pwm());
        assert!(r >= 0, "symlink should succeed, got {}", r);
    });

    test!(hardlink_create, {
        let fd = nestfs
            .open("/hardlink_src.txt", 0x0102, test_pwm())
            .unwrap();
        nestfs.write(fd as u32, b"hardlink data", 13);
        nestfs.close(fd as u32);

        let r = nestfs.link("/hardlink_src.txt", "/hardlink_dst.txt", test_pwm());
        assert_eq_nestfs!(r, 0, "hardlink should succeed");
    });

    test!(hardlink_target_exists, {
        let fd = nestfs
            .open("/hl_exist_src.txt", 0x0102, test_pwm())
            .unwrap();
        nestfs.write(fd as u32, b"src", 3);
        nestfs.close(fd as u32);
        let fd2 = nestfs
            .open("/hl_exist_dst.txt", 0x0102, test_pwm())
            .unwrap();
        nestfs.write(fd2 as u32, b"dst", 3);
        nestfs.close(fd2 as u32);

        // 内核差异: hardlink 目标已存在返回 AlreadyExists (-17, EEXIST), 测试版 mock 为 -4
        let r = nestfs.link("/hl_exist_src.txt", "/hl_exist_dst.txt", test_pwm());
        assert_eq_nestfs!(
            r,
            -17,
            "hardlink to existing target should return -17 (AlreadyExists)"
        );
    });

    test!(hardlink_source_not_found, {
        let r = nestfs.link("/no_such_src", "/hl_dst.txt", test_pwm());
        assert_eq_nestfs!(r, -2, "hardlink missing source should return -2");
    });

    test!(xattr_set_get_remove, {
        let fd = nestfs.open("/xattr_test.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"xattr content", 13);
        nestfs.close(fd as u32);

        let r = nestfs.setxattr(
            "/xattr_test.txt",
            "user.comment",
            b"hello world",
            test_pwm(),
        );
        assert!(r >= 0, "setxattr should succeed, got {}", r);

        let mut buf = [0u8; 64];
        let r = nestfs.getxattr("/xattr_test.txt", "user.comment", &mut buf, test_pwm());
        assert!(r > 0, "getxattr should return data, got {}", r);

        let r = nestfs.removexattr("/xattr_test.txt", "user.comment", test_pwm());
        assert_eq_nestfs!(r, 0, "removexattr should succeed");
    });

    test!(xattr_nonexistent_file, {
        let r = nestfs.setxattr("/no_xattr_file", "user.test", b"val", test_pwm());
        assert_eq_nestfs!(r, -2, "setxattr on nonexistent should return -2");
    });

    test!(xattr_list, {
        let fd = nestfs.open("/xattr_list.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"list test", 9);
        nestfs.close(fd as u32);

        nestfs.setxattr("/xattr_list.txt", "user.attr0", b"v0", test_pwm());
        nestfs.setxattr("/xattr_list.txt", "user.attr1", b"v1", test_pwm());

        let mut buf = [0u8; 256];
        let r = nestfs.listxattr("/xattr_list.txt", &mut buf, test_pwm());
        assert!(r > 0, "listxattr should return data, got {}", r);
    });

    test!(chmod_basic, {
        let fd = nestfs.open("/chmod_test.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"chmod", 5);
        nestfs.close(fd as u32);

        let r = nestfs.chmod("/chmod_test.txt", 0o755, test_pwm());
        assert_eq_nestfs!(r, 0, "chmod should succeed");

        let stat = nestfs.stat("/chmod_test.txt", test_pwm());
        assert!(stat.is_some());
        assert_eq_nestfs!(stat.unwrap().pwm_perm, 0o755u16, "chmod value");
    });

    test!(chmod_nonexistent, {
        // 内核差异: chmod 不存在文件返回 FileNotFound (-2, ENOENT), 测试版 mock 为 -1
        let r = nestfs.chmod("/no_chmod_file", 0o755, test_pwm());
        assert_eq_nestfs!(r, -2, "chmod nonexistent should return -2 (FileNotFound)");
    });

    test!(chown_basic, {
        let fd = nestfs.open("/chown_test.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"chown", 5);
        nestfs.close(fd as u32);

        let r = nestfs.chown("/chown_test.txt", 42, test_pwm());
        assert_eq_nestfs!(r, 0, "chown should succeed");

        let stat = nestfs.stat("/chown_test.txt", test_pwm());
        assert!(stat.is_some());
        assert_eq_nestfs!(stat.unwrap().owner_pwm, 42, "chown value");
    });

    test!(chown_nonexistent, {
        // 内核差异: chown 不存在文件返回 FileNotFound (-2, ENOENT), 测试版 mock 为 -1
        let r = nestfs.chown("/no_chown_file", 42, test_pwm());
        assert_eq_nestfs!(r, -2, "chown nonexistent should return -2 (FileNotFound)");
    });

    test!(sync_operation, {
        let fd = nestfs.open("/sync_test.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"sync data", 9);
        nestfs.close(fd as u32);

        let r = nestfs.sync();
        assert_eq_nestfs!(r, 0, "sync should succeed");
    });

    test!(get_stats, {
        let (allocs, _frees, reads, writes) = nestfs.get_stats();
        assert!(
            allocs > 0 || reads > 0 || writes > 0,
            "stats should reflect activity"
        );
    });

    println!("\n=== NestFS Advanced Feature Tests Passed ===\n");
}

#[test]
fn nestfs_snapshot_clone() {
    let _guard = NESTFS_TEST_LOCK.lock().unwrap();
    println!("\n=== NestFS Snapshot & Clone Tests ===\n");

    let nestfs = get_nestfs();
    ensure_nestfs_init();

    test!(snapshot_create, {
        let fd = nestfs.open("/snap_file.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"snapshot data", 13);
        nestfs.close(fd as u32);

        let r = nestfs.snapshot_create("snap1");
        assert!(r >= 0, "snapshot_create should succeed, got {}", r);
    });

    test!(snapshot_list, {
        let snap_mgr = &nestfs.snap_mgr;
        let count = snap_mgr.snapshot_count();
        assert!(count >= 1, "should have at least 1 snapshot, got {}", count);
    });

    test!(snapshot_get, {
        let snap_mgr = &nestfs.snap_mgr;
        let snaps = snap_mgr.list_snapshots(0);
        assert!(!snaps.is_empty(), "should list snapshots for ds_id=0");
        let snap = snap_mgr.get_snapshot(snaps[0].snap_id);
        assert!(snap.is_some(), "should get snapshot by id");
        let snap = snap.unwrap();
        assert_eq_nestfs!(snap.get_name(), "snap1", "snapshot name");
    });

    test!(snapshot_rollback, {
        let fd = nestfs
            .open("/post_snap_file.txt", 0x0102, test_pwm())
            .unwrap();
        nestfs.write(fd as u32, b"after snapshot", 14);
        nestfs.close(fd as u32);

        let snap_mgr = &nestfs.snap_mgr;
        let snaps = snap_mgr.list_snapshots(0);
        let snap_id = snaps[0].snap_id;

        let r = nestfs.snapshot_rollback(snap_id);
        assert_eq_nestfs!(r, 0, "rollback should succeed");
    });

    test!(snapshot_destroy, {
        let _ = nestfs.snapshot_create("snap_to_destroy");
        let snap_mgr = &nestfs.snap_mgr;
        let snaps = snap_mgr.list_snapshots(0);
        let target = snaps.iter().find(|s| s.get_name() == "snap_to_destroy");
        assert!(target.is_some(), "should find snap_to_destroy");
        let snap_id = target.unwrap().snap_id;

        let r = nestfs.snapshot_destroy(snap_id);
        assert_eq_nestfs!(r, 0, "destroy should succeed");
    });

    test!(snapshot_destroy_nonexistent, {
        // 内核差异: 销毁不存在的快照返回 FileNotFound (-2, ENOENT), 测试版 mock 为 -1
        let r = nestfs.snapshot_destroy(99999);
        assert_eq_nestfs!(r, -2, "destroy nonexistent should return -2 (FileNotFound)");
    });

    test!(snapshot_rollback_wrong_ds, {
        let snap_mgr = &nestfs.snap_mgr;
        let snaps = snap_mgr.list_snapshots(0);
        if !snaps.is_empty() {
            let snap_id = snaps[0].snap_id;
            let fake_ds = NestDataset::new(999, "fake", 0);
            let r = snap_mgr.rollback(snap_id, &fake_ds);
            assert_eq_nestfs!(r, false, "rollback with wrong ds_id should fail");
        }
    });

    test!(clone_from_snapshot, {
        let _ = nestfs.snapshot_create("clone_source");
        let snap_mgr = &nestfs.snap_mgr;
        let snaps = snap_mgr.list_snapshots(0);
        let source = snaps.iter().find(|s| s.get_name() == "clone_source");
        if let Some(snap) = source {
            let r = nestfs.clone_create(snap.snap_id, "cloned_ds");
            assert!(r >= 0, "clone_create should succeed, got {}", r);
        }
    });

    test!(clone_from_nonexistent_snapshot, {
        // 内核差异: clone 不存在的快照返回 Io (-5, EIO), 测试版 mock 为 -1
        let r = nestfs.clone_create(99999, "bad_clone");
        assert_eq_nestfs!(
            r,
            -5,
            "clone from nonexistent snapshot should fail with -5 (Io)"
        );
    });

    println!("\n=== NestFS Snapshot & Clone Tests Passed ===\n");
}

#[test]
fn nestfs_fd_management() {
    let _guard = NESTFS_TEST_LOCK.lock().unwrap();
    println!("\n=== NestFS FD Management Tests ===\n");

    let nestfs = get_nestfs();
    ensure_nestfs_init();

    test!(open_append_flag, {
        let fd = nestfs.open("/append_test.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"first", 5);
        nestfs.close(fd as u32);

        // 内核差异: O_APPEND (0x0400) open 后 fd offset 初始化为文件末尾 (真实 POSIX
        // 语义), 直接 read 返回 0 (offset == size)。seek(0) 后可读全文。
        let fd = nestfs
            .open("/append_test.txt", 0x0100 | 0x0400, test_pwm())
            .unwrap();
        let mut buf = [0u8; 32];
        let r = nestfs.read(fd as u32, &mut buf, 32);
        assert_eq_nestfs!(
            r,
            0,
            "append mode open 后 offset 在文件末尾 (内核 POSIX 语义)"
        );
        let pos = nestfs.seek(fd as u32, 0, 0);
        assert_eq_nestfs!(pos, 0, "seek back to 0");
        let r = nestfs.read(fd as u32, &mut buf, 32);
        assert_eq_nestfs!(r, 5, "seek 后应读到 5 字节");
        assert_eq_nestfs!(&buf[..5], b"first", "append 文件内容");
        nestfs.close(fd as u32);
    });

    test!(open_existing_with_create, {
        let fd = nestfs.open("/existing.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"original", 8);
        nestfs.close(fd as u32);

        let fd2 = nestfs.open("/existing.txt", 0x0102, test_pwm());
        assert!(fd2.is_ok(), "open existing with O_CREAT should succeed");
        nestfs.close(fd2.unwrap() as u32);
    });

    test!(close_twice, {
        let fd = nestfs
            .open("/double_close.txt", 0x0102, test_pwm())
            .unwrap();
        nestfs.close(fd as u32);
        // 内核差异: 重复 close 返回 InvalidArgument (-22, EINVAL), 测试版 mock 为 -1
        let r = nestfs.close(fd as u32);
        assert_eq_nestfs!(
            r,
            -22,
            "closing already-closed fd should return -22 (InvalidArgument)"
        );
    });

    test!(read_write_zero_bytes, {
        let fd = nestfs.open("/zero_rw.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"data", 4);
        nestfs.close(fd as u32);

        let fd = nestfs.open("/zero_rw.txt", 0x0001, test_pwm()).unwrap();
        let mut buf = [0u8; 4];
        let r = nestfs.read(fd as u32, &mut buf, 0);
        assert_eq_nestfs!(r, 0, "read 0 bytes should return 0");
        nestfs.close(fd as u32);

        // 内核差异: write 的权限检查先于 0 字节短路 — 只读 fd 上 0 字节 write 返回
        // PermissionDenied (-1)。在可写 fd 上 0 字节 write 返回 0。
        let fd = nestfs.open("/zero_rw.txt", 0x0102, test_pwm()).unwrap();
        let w = nestfs.write(fd as u32, b"", 0);
        assert_eq_nestfs!(w, 0, "write 0 bytes should return 0");
        nestfs.close(fd as u32);
    });

    test!(read_past_end, {
        let fd = nestfs.open("/short_file.txt", 0x0102, test_pwm()).unwrap();
        nestfs.write(fd as u32, b"hi", 2);
        nestfs.close(fd as u32);

        let fd = nestfs.open("/short_file.txt", 0x0001, test_pwm()).unwrap();
        nestfs.seek(fd as u32, 100, 0);
        let mut buf = [0u8; 10];
        let r = nestfs.read(fd as u32, &mut buf, 10);
        assert_eq_nestfs!(r, 0, "read past end should return 0");
        nestfs.close(fd as u32);
    });

    println!("\n=== NestFS FD Management Tests Passed ===\n");
}

/// C1/T6-④ 时间戳写回: `FileSystem::fs_utimensat` → DMU 对象落库 → `fs_stat` 可观测
///
/// 验收 (utimensat POSIX 语义):
///   1. 显式 atime/mtime → fs_stat 读回同一值 (证明确实写回, 而非只填元数据)
///   2. `u64::MAX` (= UTIME_OMIT) → 该字段保持原值
///   3. 路径不存在 → 返回错误
#[test]
fn nestfs_utimensat_writes_back_times() {
    use queenx::kernel::services::fs::FileSystem;

    let _guard = NESTFS_TEST_LOCK.lock().unwrap();
    ensure_nestfs_init();
    let nestfs = get_nestfs();
    let pwm = test_pwm();

    const PATH: &str = "/times_utimensat.txt";
    const ATIME: u64 = 1_700_000_000;
    const MTIME: u64 = 1_700_000_123;

    let fd = nestfs.open(PATH, 0x0102, pwm).unwrap();
    nestfs.close(fd as u32);

    nestfs
        .fs_utimensat(PATH, ATIME, MTIME, pwm)
        .expect("fs_utimensat on existing file must succeed");

    let st = nestfs.fs_stat(PATH, pwm).expect("fs_stat must find file");
    assert_eq!(st.atime, ATIME, "atime 必须写回");
    assert_eq!(st.mtime, MTIME, "mtime 必须写回");

    // UTIME_OMIT: atime 保持不变, mtime 更新
    nestfs
        .fs_utimensat(PATH, u64::MAX, MTIME + 5, pwm)
        .expect("UTIME_OMIT for atime must succeed");
    let st = nestfs.fs_stat(PATH, pwm).expect("fs_stat must find file");
    assert_eq!(st.atime, ATIME, "UTIME_OMIT 必须保持 atime 原值");
    assert_eq!(st.mtime, MTIME + 5, "mtime 必须更新");

    assert!(
        nestfs
            .fs_utimensat("/no_such_file_utimensat.txt", 1, 1, pwm)
            .is_err(),
        "不存在的路径必须报错"
    );
}
