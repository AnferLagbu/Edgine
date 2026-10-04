//! fs: B06-02/03/07 权限与句柄修复回归测试
//!
//! 验收:
//!   - chown_syscall: UID/GID 未注册返回 EINVAL, 不再回退 root (B06-02)
//!   - open_by_handle_at_syscall: 无 CAP_SYS_ADMIN (SYSTEM 域 0x01) 返回 EPERM (B06-03)
//!   - poll_syscall: fd 有效性由 per-process fd 表判定, 无全局数值上限 (B06-07/B-9.5)
//!
//! ## B08-20 迁移 (2026-09-06)
//! 原镜像三个 syscall 的纯判定逻辑已改引内核真实 API:
//! - chown_syscall 的 UID 判定 → `identity::get_table().find_by_uid` (真实身份表)
//! - open_by_handle_at_syscall 的 CAP_SYS_ADMIN 判定 → `privileged::sgeg::pwm_has_capability`
//! - poll_syscall 的 fd 有效性判定 → `functions::fs::vfs_get_fd_handle` (per-process fd 表)
//!
//! ## 因内核 host 不可测已移除 (syscall 完整路径)
//! 三个 syscall 完整函数 (chown_syscall / open_by_handle_at_syscall / poll_syscall) 依赖
//! 进程凭证上下文 (pwm_get_current / session::get_current_pwm) 与 VFS 全局状态
//! (vfs_chown_ext / per-process fd 表), 且 open_by_handle_at / poll 走
//! copy_from_user / read_struct_from_user (SMAP stac/clac 指令, host 不可用),
//! host 上不可直接调用. 完整路径回归保留在 QEMU 集成测试.
//!
//! 追踪: B06-02 / B06-03 / B06-07
//! SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use edgine::kernel::functions::sgeg::capability::CAP_DOMAIN_SYSTEM;
use edgine::kernel::functions::sgeg::types::{CapBits, CapDomain};
use edgine::kernel::privileged::sgeg::identity;
use edgine::kernel::privileged::sgeg::pwm_has_capability;

/// 注册并缓存测试身份 (creator=0 → 最高特权级, uid=0).
fn test_pwm() -> u64 {
    static PWM: OnceLock<u64> = OnceLock::new();
    *PWM.get_or_init(|| {
        identity::get_table()
            .create("fp-pw", "fs-permissions", 0)
            .expect("注册测试身份失败")
    })
}

/// 独立测试身份, 供"授予 CAP_SYS_ADMIN"用例修改能力而不污染 `test_pwm`.
fn admin_pwm() -> u64 {
    static PWM: OnceLock<u64> = OnceLock::new();
    *PWM.get_or_init(|| {
        identity::get_table()
            .create("fp-admin", "fs-permissions-admin", 0)
            .expect("注册测试身份失败")
    })
}

// ============================================================================
// B06-02: chown_syscall UID/GID 判定 (改引内核 identity::find_by_uid)
// ============================================================================

/// 内核 [functions/fs/file_ops.rs::chown_syscall] 的 uid→pwm 判定 (B06-02 修复后):
///
/// 原实现 `tbl.find_by_uid(uid).map_or(0, ...)` 在 uid 未注册时回退 owner_pwm=0 (root),
/// 存在提权漏洞; 修复后未注册 uid/gid 返回 `EINVAL` (errno=22), 不再默认 root.
/// 本测试直接验证内核 `identity::get_table().find_by_uid` 真实行为:
/// 返回 None = 未注册 → chown_syscall 返回 EINVAL; Some = 已注册 → 取 owner_pwm.
#[test]
fn chown_registered_uid_returns_pwm() {
    // 已注册 uid (creator=0 身份 uid=0) 正常映射到对应 pwm, 不回退 0
    let pwm = test_pwm();
    let tbl = identity::get_table();
    let found = tbl.find_by_uid(0);
    // B08-20: 并行测试共享全局 identity 表, 多个测试文件均可能注册 uid=0 身份,
    // find_by_uid(0) 返回的是"第一个 uid=0 条目"而非必然是 test_pwm().
    // 断言语义: uid=0 已被注册 (Some) 且其 pwm 非 0 (不回退 root 语义), 不锁定具体 pwm.
    assert!(found.is_some(), "已注册身份应可通过 find_by_uid(0) 找到");
    let found_pwm = found.unwrap().get_pwm().0;
    assert_ne!(found_pwm, 0, "uid=0 映射的 pwm 必须非 0 (不回退 root)");
    assert!(
        found_pwm == pwm || identity::find(pwm).is_some(),
        "test_pwm 身份应已注册 (无论 find_by_uid 命中哪个 uid=0 条目)"
    );
}

#[test]
fn chown_unregistered_uid_returns_einval_not_root() {
    // 未注册 uid 必须返回 EINVAL (find_by_uid None), 不得回退 0 (root) — B06-02 核心
    let tbl = identity::get_table();
    // 本测试进程仅注册 uid=0 的身份, 其余 uid 均未注册
    for uid in [42u32, 43, 1000, 2000] {
        assert!(
            tbl.find_by_uid(uid).is_none(),
            "uid {} 未注册应返回 None",
            uid
        );
    }
}

#[test]
fn chown_max_uid_sentinel_rejected() {
    // uid == u32::MAX (Linux "(uid_t)-1" 哨兵) 未注册 → None, 而非回退 root
    let tbl = identity::get_table();
    assert!(tbl.find_by_uid(u32::MAX).is_none());
}

// ============================================================================
// B06-03: open_by_handle_at_syscall CAP 检查 (改引内核 pwm_has_capability)
// ============================================================================

/// 内核 [functions/fs/file_handle.rs::open_by_handle_at_syscall] 的权限检查 (B06-03):
///
/// 采用 SYSTEM 域 (domain=0) + CAP_SYS_ADMIN (0x01), 与 mount/umount2 先例一致;
/// 无能力返回 `EPERM` (errno=1). 本测试直接验证内核
/// `privileged::sgeg::pwm_has_capability(pwm, SYSTEM, 0x01)` 判定.
#[test]
fn open_by_handle_without_cap_returns_eperm() {
    // 注册身份初始 SYSTEM caps = VIABLE_FLOOR[SYSTEM] = 0 → 无 CAP_SYS_ADMIN → EPERM
    let pwm = test_pwm();
    assert!(
        !pwm_has_capability(pwm, CAP_DOMAIN_SYSTEM, 0x01),
        "默认身份不应有 CAP_SYS_ADMIN (对应 open_by_handle_at_syscall → EPERM)"
    );
}

#[test]
fn open_by_handle_bootstrap_has_cap_allowed() {
    // 内核差异: pwm==0 是 bootstrap 全权身份, 拥有 CAP_SYS_ADMIN → 放行
    assert!(pwm_has_capability(0, CAP_DOMAIN_SYSTEM, 0x01));
}

#[test]
fn open_by_handle_grant_sys_admin_allows() {
    // 经内核 PwmEntry::fetch_or_caps 授予 SYSTEM 0x01 后 → 拥有 CAP_SYS_ADMIN → 放行
    let pwm = admin_pwm();
    assert!(!pwm_has_capability(pwm, CAP_DOMAIN_SYSTEM, 0x01));
    identity::find(pwm)
        .expect("测试身份应已注册")
        .fetch_or_caps(CapDomain::SYSTEM, CapBits(0x01));
    assert!(pwm_has_capability(pwm, CAP_DOMAIN_SYSTEM, 0x01));
}

// ============================================================================
// B06-07 / B-9.5: poll_syscall fd 有效性判定 (per-process fd 表)
// ============================================================================

/// 提取 `src` 中列首 `pub fn` 起始的顶层函数体 (至下一个列首 `pub fn` / `fn` 前)
fn pub_fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let rest = &src[start + sig.len()..];
    let mut end = rest.len();
    for pat in ["\npub fn ", "\nfn "] {
        if let Some(i) = rest.find(pat) {
            end = end.min(i + 1);
        }
    }
    &src[start..start + sig.len() + end]
}

/// 内核 [functions/fs/file_ops.rs::poll_syscall] 的 fd 有效性判定 (B06-07 → B-9.5):
///
/// 原实现用硬编码 `< 256` 做上限后直接索引 32 长 fd_table, fd∈[32,255] 越界 panic;
/// B-9.5 per-process fd 表全量下沉后, 全局数值上限 (VFS_MAX_FDS / 256) 随全局 fd 表
/// 一并退役, fd 有效性改由 `vfs_get_fd_handle(fd)` 查 per-process fd 表 →
/// OpenFileTable 句柄映射判定: 映射存在即有效, 不存在则视为不就绪. 无任何定长数组
/// 索引 → 无越界风险. poll_syscall 完整路径依赖 read_struct_from_user (SMAP
/// stac/clac) 与进程上下文, host 不可直接调用; 此处作静态契约扫描.
#[test]
fn poll_syscall_validates_fd_via_per_process_fd_table() {
    let path = Path::new("../src/kernel/functions/fs/file_ops.rs");
    let src = fs::read_to_string(path).expect("读取 functions/fs/file_ops.rs 失败");
    let body = pub_fn_body(&src, "pub fn poll_syscall");
    assert!(
        body.contains("vfs_get_fd_handle(pfd.fd as usize)"),
        "poll_syscall 必须以 per-process fd 表判定 fd 有效性 (B-9.5):\n{body}"
    );
    assert!(
        !body.contains("VFS_MAX_FDS"),
        "poll_syscall 不得再引用已退役的 VFS_MAX_FDS (B-9.5):\n{body}"
    );
    assert!(
        !body.contains("256"),
        "poll_syscall 不得残留硬编码 fd 上限 256 (B06-07):\n{body}"
    );
}

// ============================================================================
// 丙批审查 B1: 时间戳写回属主判据覆盖门槛
// ============================================================================

/// 递归收集 `dir` 下所有 `.rs` 源码路径 (目录不存在时静默跳过, 由调用方断言非空)
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// 提取 `src` 中 `sig` 起始的函数体 (至下一个同级 4 空格缩进的 `fn` 定义前)
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let rest = &src[start..];
    let mut end = rest.len();
    let mut cursor = 0usize;
    for (idx, line) in rest.split('\n').enumerate() {
        if idx > 0
            && (line.starts_with("    fn ")
                || line.starts_with("    pub fn ")
                || line.starts_with("    pub(crate) fn "))
        {
            end = cursor;
            break;
        }
        cursor += line.len() + 1;
    }
    &rest[..end]
}

/// 丙批审查 B1 门槛: 凡真实落盘时间戳的 `set_times` 必须含属主/特权判据.
///
/// 背景: ext2 `set_times` 曾只写回不判权限 (任意 pwm 可改他人文件时间戳), 与
/// unkfs 同层实装分歧. 判据必须与写回同锁域 — 上提到 VFS 层无 owner 模型,
/// 且会把判据与写回拆成两次路径解析 (TOCTOU) — 故一致性由本测试面收口:
/// 扫描 privileged/functions 两侧 fs 源码, 凡 `set_times` 函数体含落盘动作
/// (`save_inode`/`update_obj`) 者, 必须同体出现 `pwm_get_privilege_level` 与
/// `PermissionDenied`. 纯内存 FS 的 `Ok(())` 桩与默认 `NotSupported` 不在门槛内.
///
/// 维护提示: 若判据日后被抽取为共享谓词 (不再内联 `pwm_get_privilege_level`),
/// 请同步更新本测试的谓词断言, 勿直接删除门槛.
#[test]
fn set_times_of_persistent_fs_checks_owner_or_privilege() {
    let roots = [
        Path::new("../src/kernel/functions/fs"),
        Path::new("../src/kernel/privileged/fs"),
    ];
    let mut files = Vec::new();
    for root in roots {
        collect_rs_files(root, &mut files);
    }
    assert!(!files.is_empty(), "fs 源码扫描路径失效, B1 门槛未生效");

    let mut checked = Vec::new();
    for file in &files {
        let src = fs::read_to_string(file).unwrap_or_else(|e| panic!("读取 {file:?} 失败: {e}"));
        if !src.contains("fn set_times(") {
            continue;
        }
        let body = fn_body(&src, "fn set_times(");
        if !body.contains("save_inode") && !body.contains("update_obj") {
            continue;
        }
        checked.push(file.display().to_string());
        assert!(
            body.contains("pwm_get_privilege_level"),
            "{file:?} 的 set_times 落盘前必须判属主/特权级 (丙批 B1)",
        );
        assert!(
            body.contains("PermissionDenied"),
            "{file:?} 的 set_times 无权限必须返回 PermissionDenied (丙批 B1)",
        );
    }
    assert!(
        checked.len() >= 2,
        "B1 门槛应至少覆盖 ext2 与 unkfs 两条落盘路径, 实得 {checked:?}",
    );
}
