//! overlayfs 真实行为集成测试
//!
//! 替换早期仅"验证编译通过"的占位用例, 在 host 侧搭建真实测试台驱动内核
//! `services::fs::overlayfs` 实现, 覆盖以下行为:
//! - lowerdir 只读直通 (`OverlayLowerInode`, 写操作显式 `ReadOnlyFilesystem`)
//! - 写意图 open 触发 `copy_up` (下层文件提升到 upperdir, 下层原文不动)
//! - lower-only 文件 unlink 生成 whiteout 遮蔽下层
//! - copy_up 后 unlink 补 whiteout 回归 (修复前会重新暴露下层同名副本)
//! - `fs_utimensat` / `set_times` 时间戳写回 (upper 节点, 含 UTIME_OMIT 与属主判据)
//! - `fs_utimensat` 层级路由 (lower-only 拒改 / whiteout / 不存在 → FileNotFound)
//!
//! ## 测试台搭建
//! 本文件作为独立测试二进制运行, 其全局单例 (`VFS_MANAGER` / `RAMFS_DATA`
//! / `OVERLAY_FS`) 与其他测试文件天然隔离. 初始化链:
//! `services::fs::init()` (注册后端) → `vfs_mount_safe("/lower", "ramfs")`
//! → `vfs_mount_safe("/merged", "overlay")`. 后者经 services 注册表
//! `resolve_fs("overlay")` 解析 trait object, 顺带验证 Option C 注册接线.
//!
//! pwm 统一用 bootstrap 身份 `0` (`engine::check` 直通, ramfs `check_permission`
//! 走 `caps == ALL` 分支), 无需注册身份或授权.

use queenx::kernel::framework::error::KernelError;
use queenx::kernel::framework::fs::{FileSystem, VFS_MANAGER, vfs_mount_safe};
use queenx::kernel::services::fs::init as fs_init;
use queenx::kernel::services::fs::overlayfs::overlay_fs;
use std::sync::{Mutex, Once};

/// bootstrap 身份 — 持全权, 免注册/免授权
const PWM: u64 = 0;

/// Linux `O_RDONLY` — 不命中写意图掩码, 只读直通下层
const O_RDONLY: u32 = 0o0;
/// Linux `O_WRONLY` — 命中写意图掩码, 触发 copy_up
const O_WRONLY: u32 = 0o1;

/// 各 `#[test]` 共享全局单例 (VFS 挂载表 / overlay upper 层), 需串行化.
static OVERLAY_TEST_LOCK: Mutex<()> = Mutex::new(());

/// 测试台一次性初始化守卫
static OVERLAY_TEST_INIT: Once = Once::new();

/// 搭建测试台 (幂等): 注册 fs 后端 → 挂载 lower ramfs → 挂载 overlay merged.
fn ensure_overlay_ready() {
    OVERLAY_TEST_INIT.call_once(|| {
        // 1. 注册 services fs 后端 (ramfs Inode 工厂 + overlay/tmpfs 注册表映射)
        fs_init();
        // 2. lower 层: overlay 的 lower_path 硬编码为 "/lower", 必须挂在此处
        assert_eq!(
            vfs_mount_safe("/lower", "ramfs"),
            0,
            "挂载 lower ramfs 失败"
        );
        // 3. overlay 合并视图: 经 VFS 注册表解析 "overlay" → overlay_fs()
        assert_eq!(
            vfs_mount_safe("/merged", "overlay"),
            0,
            "挂载 overlay 失败"
        );
    });
}

/// 取 lowerdir 挂载的 ramfs FileSystem (用于预置下层数据 / 校验下层残留).
fn lower_ramfs() -> &'static dyn FileSystem {
    let (_, _, fs) = VFS_MANAGER
        .resolve_mount_fs("/lower/probe")
        .expect("lowerdir 未挂载");
    fs.expect("lowerdir 挂载点缺少 FileSystem trait object")
}

/// 在 lowerdir 根下创建文件并写入内容, 模拟镜像预置的下层只读数据.
fn create_lower_file(name: &str, content: &[u8]) {
    let inode = lower_ramfs()
        .fs_create("/", name, PWM)
        .expect("创建下层文件失败");
    let written = inode.write(0, content, PWM).expect("写入下层文件失败");
    assert_eq!(written, content.len(), "下层文件写入长度不符");
}

/// 只读打开仅存在于 lowerdir 的文件: 直通下层, 读得原文, 写/截断被拒.
#[test]
fn overlay_lower_readonly_passthrough() {
    let _guard = OVERLAY_TEST_LOCK.lock().unwrap();
    ensure_overlay_ready();

    let name = "lower_readonly.txt";
    let content = b"lower-original-content";
    create_lower_file(name, content);

    let overlay = overlay_fs();
    let path = format!("/{name}");

    // 只读打开: 应得下层 Inode 代理 (不产生 upper 副本)
    let inode = overlay
        .fs_open(&path, O_RDONLY, PWM)
        .expect("只读打开下层文件失败");

    let mut buf = [0u8; 64];
    let n = inode.read(0, &mut buf, PWM).expect("读取下层文件失败");
    assert_eq!(&buf[..n], content, "只读直通应返回下层原文");

    assert_eq!(
        inode.write(0, b"x", PWM).unwrap_err(),
        KernelError::ReadOnlyFilesystem,
        "只读代理应拒绝写入"
    );
    assert_eq!(
        inode.truncate(0, PWM).unwrap_err(),
        KernelError::ReadOnlyFilesystem,
        "只读代理应拒绝截断"
    );

    // 只读打开不应在 upperdir 产生副本: 重新解析仍命中 lower
    let entry_stat = overlay.fs_stat(&path, PWM).expect("stat 下层文件失败");
    assert_eq!(
        entry_stat.node_id, inode.node_id(),
        "只读打开不得改变 merged 视图来源"
    );
}

/// 写意图 open 触发 copy_up: 内容提升到 upperdir, 下层原文保持不变.
#[test]
fn overlay_write_open_triggers_copy_up() {
    let _guard = OVERLAY_TEST_LOCK.lock().unwrap();
    ensure_overlay_ready();

    let name = "copyup_case.txt";
    let lower = b"lower-version-1";
    let upper = b"UPPER-version-2";
    assert_eq!(lower.len(), upper.len(), "等长内容便于原地覆盖断言");
    create_lower_file(name, lower);

    let overlay = overlay_fs();
    let path = format!("/{name}");

    // 写打开: 触发 copy_up, 返回 upper 层 Inode
    let inode = overlay
        .fs_open(&path, O_WRONLY, PWM)
        .expect("写打开触发 copy_up 失败");
    let written = inode.write(0, upper, PWM).expect("写入 upper 失败");
    assert_eq!(written, upper.len(), "upper 写入长度不符");

    // 重新只读打开: 现由 upper 命中, 读到新内容
    let reread = overlay.fs_open(&path, O_RDONLY, PWM).expect("重开失败");
    let mut buf = [0u8; 32];
    let n = reread.read(0, &mut buf, PWM).expect("读取失败");
    assert_eq!(&buf[..n], upper, "copy_up 后应读到 upper 新内容");

    // 下层原文未被破坏 (copy_up 是复制而非移动)
    let lower_inode = lower_ramfs()
        .fs_open(name, O_RDONLY, PWM)
        .expect("打开下层原文失败");
    let mut lbuf = [0u8; 32];
    let ln = lower_inode.read(0, &mut lbuf, PWM).expect("读取下层原文失败");
    assert_eq!(&lbuf[..ln], lower, "copy_up 不得修改下层原文");
}

/// 仅存在于 lowerdir 的文件 unlink: 生成 whiteout 遮蔽, merged 视图不可见.
#[test]
fn overlay_lower_only_unlink_creates_whiteout() {
    let _guard = OVERLAY_TEST_LOCK.lock().unwrap();
    ensure_overlay_ready();

    let name = "whiteout_case.txt";
    create_lower_file(name, b"to-be-deleted");

    let overlay = overlay_fs();
    let path = format!("/{name}");

    assert!(overlay.fs_stat(&path, PWM).is_ok(), "删除前应可见");
    overlay.fs_unlink(&path, PWM).expect("删除下层文件失败");

    assert_eq!(
        overlay.fs_stat(&path, PWM).unwrap_err(),
        KernelError::FileNotFound,
        "whiteout 后 stat 应不可见"
    );
    assert_eq!(
        overlay
            .fs_open(&path, O_RDONLY, PWM)
            .map(|_| ())
            .unwrap_err(),
        KernelError::FileNotFound,
        "whiteout 后 open 应不可见"
    );

    // whiteout 状态下的重复删除仍报 FileNotFound
    assert_eq!(
        overlay.fs_unlink(&path, PWM).unwrap_err(),
        KernelError::FileNotFound,
        "重复删除应报 FileNotFound"
    );

    // 下层数据本身仍在 (whiteout 只遮蔽合并视图)
    assert!(
        lower_ramfs().fs_stat(name, PWM).is_ok(),
        "whiteout 不得删除下层实体"
    );
}

/// 回归: copy_up 后 unlink 必须补 whiteout, 否则 merged 视图重新暴露下层同名副本.
///
/// 修复前 upper 删除后下层同名文件会再次命中, 本用例断言删除后仍不可见.
#[test]
fn overlay_unlink_after_copy_up_masks_lower() {
    let _guard = OVERLAY_TEST_LOCK.lock().unwrap();
    ensure_overlay_ready();

    let name = "copyup_unlink_regression.txt";
    let lower = b"regression-lower";
    let upper = b"regression-upper";
    assert_eq!(lower.len(), upper.len());
    create_lower_file(name, lower);

    let overlay = overlay_fs();
    let path = format!("/{name}");

    // 写打开触发 copy_up: upper 出现同名副本, lower 仍保留
    let inode = overlay
        .fs_open(&path, O_WRONLY, PWM)
        .expect("copy_up 失败");
    inode.write(0, upper, PWM).expect("写入 upper 失败");

    // 删除: upper 删除后必须补 whiteout 遮蔽 lower 同名副本
    overlay.fs_unlink(&path, PWM).expect("删除失败");

    assert_eq!(
        overlay.fs_stat(&path, PWM).unwrap_err(),
        KernelError::FileNotFound,
        "回归: 删除后 merged 视图不得再命中下层副本"
    );
    assert_eq!(
        overlay
            .fs_open(&path, O_RDONLY, PWM)
            .map(|_| ())
            .unwrap_err(),
        KernelError::FileNotFound,
        "回归: 删除后 open 不得再命中下层副本"
    );

    // 下层实体仍在 (whiteout 仅遮蔽)
    assert!(
        lower_ramfs().fs_stat(name, PWM).is_ok(),
        "whiteout 不得删除下层实体"
    );
}

/// 时间戳写回: `fs_utimensat` / inode `set_times` 更新 upper 节点, `fs_stat` 可观测;
/// 含 UTIME_OMIT (`u64::MAX`) 语义与属主/特权判据 (非属主且无特权 → PermissionDenied).
#[test]
fn overlay_utimensat_writes_times_with_owner_check() {
    let _guard = OVERLAY_TEST_LOCK.lock().unwrap();
    ensure_overlay_ready();

    let name = "utimensat_case.txt";
    create_lower_file(name, b"times-content");

    let overlay = overlay_fs();
    let path = format!("/{name}");

    // 写打开触发 copy_up, 使文件进入 upper (时间戳写回作用于 upper 节点)
    let inode = overlay
        .fs_open(&path, O_WRONLY, PWM)
        .expect("copy_up 失败");
    inode.write(0, b"times-content", PWM).expect("写入失败");

    // 1. 路径级 fs_utimensat: 非 OMIT 字段更新, fs_stat 可观测
    const ATIME: u64 = 1000;
    const MTIME: u64 = 2000;
    overlay
        .fs_utimensat(&path, ATIME, MTIME, PWM)
        .expect("属主 utimensat 应成功");
    let st = overlay.fs_stat(&path, PWM).expect("stat 失败");
    assert_eq!(st.atime, ATIME, "atime 应写回");
    assert_eq!(st.mtime, MTIME, "mtime 应写回");

    // 2. UTIME_OMIT: u64::MAX 字段保持原值, 另一字段照常更新
    overlay
        .fs_utimensat(&path, u64::MAX, MTIME + 5, PWM)
        .expect("OMIT atime 应成功");
    let st = overlay.fs_stat(&path, PWM).expect("stat 失败");
    assert_eq!(st.atime, ATIME, "OMIT atime 应保持原值");
    assert_eq!(st.mtime, MTIME + 5, "mtime 应照常更新");

    // 3. inode 级 set_times (OverlayFsInode) 亦写回 upper 节点
    inode
        .set_times(ATIME + 10, MTIME + 10, PWM)
        .expect("inode 级 set_times 应成功");
    let st = overlay.fs_stat(&path, PWM).expect("stat 失败");
    assert_eq!(st.atime, ATIME + 10, "inode 级 set_times 应写回 atime");
    assert_eq!(st.mtime, MTIME + 10, "inode 级 set_times 应写回 mtime");

    // 4. 属主判据: 未注册非零 pwm (特权级 0xFF, fail-closed) 改他人文件时间戳被拒
    const OTHER_PWM: u64 = 42;
    assert_eq!(
        overlay
            .fs_utimensat(&path, ATIME + 1, MTIME + 1, OTHER_PWM)
            .unwrap_err(),
        KernelError::PermissionDenied,
        "非属主且无特权应被拒"
    );
    // 拒绝后时间戳保持不变
    let st = overlay.fs_stat(&path, PWM).expect("stat 失败");
    assert_eq!(st.atime, ATIME + 10, "被拒后 atime 应不变");
    assert_eq!(st.mtime, MTIME + 10, "被拒后 mtime 应不变");
}

/// `fs_utimensat` 层级路由: lower-only 文件只读拒改, whiteout / 不存在路径报
/// FileNotFound.
#[test]
fn overlay_utimensat_layer_routing() {
    let _guard = OVERLAY_TEST_LOCK.lock().unwrap();
    ensure_overlay_ready();

    let overlay = overlay_fs();

    // 1. lower-only 文件: 只读直通, 时间戳不可改 (metadata copy-up 未实装 → 显式拒绝)
    let name = "lower_only_utimensat.txt";
    create_lower_file(name, b"lower");
    assert_eq!(
        overlay
            .fs_utimensat(&format!("/{name}"), 1, 1, PWM)
            .unwrap_err(),
        KernelError::ReadOnlyFilesystem,
        "lower-only 文件时间戳不可改"
    );

    // 2. 两层皆无: FileNotFound
    assert_eq!(
        overlay
            .fs_utimensat("/no_such_utimensat.txt", 1, 1, PWM)
            .unwrap_err(),
        KernelError::FileNotFound,
        "不存在路径应报 FileNotFound"
    );

    // 3. whiteout 遮蔽路径 (已删除): 视为不存在
    let gone = "whiteout_utimensat.txt";
    create_lower_file(gone, b"gone");
    overlay
        .fs_unlink(&format!("/{gone}"), PWM)
        .expect("unlink 失败");
    assert_eq!(
        overlay
            .fs_utimensat(&format!("/{gone}"), 1, 1, PWM)
            .unwrap_err(),
        KernelError::FileNotFound,
        "whiteout 遮蔽路径应报 FileNotFound"
    );
}
