//! P3-I-18: FileSystem trait 增加 fs_sync, vfs_sync 走 trait 分发
//!
//! 验证:
//! 1. FileSystem trait 增加 fs_sync 默认方法 (返回 Ok(()))
//! 2. UNKFS override fs_sync (i32 sync() 包装)
//! 3. RamFS/DevFS 不必 override (继承默认实现)
//! 4. vfs_sync 不再是 unkfs_sync_internal 直调
//! 5. vfs_sync 遍历 VFS_MAX_MOUNTS 挂载点
//! 6. 单元测试覆盖 fs_sync 默认实现语义

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

#[test]
fn trait_has_fs_sync_default() {
    // 阶段 4b: FileSystem trait 已随 VFS 完整下沉迁至 functions/fs/vfs_types.rs
    let src = read_src("src/kernel/functions/fs/vfs_types.rs");
    let required = [
        "fn fs_sync(&self) -> KernelResult<()>",
        "fn fs_sync(&self) -> crate::functions::fs::vfs_types::KernelResult<()>",
    ];
    assert!(
        required.iter().any(|s| src.contains(s)),
        "P3-I-18: FileSystem trait 必须有 `fn fs_sync(&self) -> KernelResult<()>` 默认方法"
    );
    // 必须有 `Ok(())` 默认实现
    let trait_block = src
        .split_once("pub trait FileSystem: Send + Sync")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        trait_block.contains("Ok(())"),
        "P3-I-18: fs_sync 必须默认返回 Ok(())"
    );
}

#[test]
fn unkfs_overrides_fs_sync() {
    // 拆分后 FileSystem impl 在 unkfs_inode.rs (原在 unkfs.rs)
    let src = read_src("src/kernel/functions/fs/unkfs/unkfs_inode.rs");
    let impl_block = src
        .rsplit_once("impl crate::functions::fs::FileSystem for UnkfsData")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        impl_block.contains("fn fs_sync("),
        "P3-I-18: UNKFS impl 必须 override fs_sync"
    );
    assert!(
        impl_block.contains("self.sync()") || impl_block.contains(".sync()"),
        "P3-I-18: UNKFS.fs_sync 必须调用底层 self.sync()"
    );
}

#[test]
fn ramfs_inherits_default() {
    // 阶段 4b: ramfs 实现已下沉 functions/fs/ramfs_core/mod.rs
    let src = read_src("src/kernel/functions/fs/ramfs_core/mod.rs");
    let impl_block = src
        .rsplit_once("impl FileSystem for RamFsFileSystem")
        .map(|(_, b)| b)
        .unwrap_or("");
    // RamFS 不应 override fs_sync (持久化为空)
    assert!(
        !impl_block.contains("fn fs_sync("),
        "P3-I-18: RamFS 不应 override fs_sync (无持久化)"
    );
}

#[test]
fn devfs_inherits_default() {
    // 阶段 4b: devfs 实现已下沉 functions/fs/devfs.rs
    let src = read_src("src/kernel/functions/fs/devfs.rs");
    let impl_block = src
        .rsplit_once("impl FileSystem for DevfsData")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        !impl_block.contains("fn fs_sync("),
        "P3-I-18: DevFS 不应 override fs_sync (无持久化)"
    );
}

#[test]
fn vfs_sync_uses_trait_dispatch() {
    // 阶段 4b: vfs_sync 已随 VFS 完整下沉迁至 functions/fs/vfs_mount.rs
    let src = read_src("src/kernel/functions/fs/vfs_mount.rs");
    let marker = "pub fn vfs_sync() -> i32 {";
    let start = src.find(marker).expect("vfs_sync not found");
    // 找下一个 `pub extern "C" fn` 之前的范围 (4b 后已无 #[no_mangle])
    let next_fn = src[start..]
        .find("\npub extern \"C\" fn ")
        .map(|o| start + o)
        .unwrap_or(src.len());
    let body = &src[start..next_fn];
    // 必须 NOT 调 unkfs_sync_internal(). 排除注释行
    let code_lines: Vec<&str> = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let code_body = code_lines.join("\n");
    assert!(
        !code_body.contains("unkfs_sync_internal()"),
        "P3-I-18: vfs_sync 不应再直调 unkfs_sync_internal (允许注释提及历史)"
    );
    assert!(
        body.contains("VFS_MANAGER.mounts.lock()"),
        "P3-I-18: vfs_sync 必须 mount 表加锁遍历"
    );
    assert!(
        body.contains("VFS_MAX_MOUNTS"),
        "P3-I-18: vfs_sync 必须按 VFS_MAX_MOUNTS 遍历"
    );
    assert!(
        body.contains("fs.fs_sync()"),
        "P3-I-18: vfs_sync 必须通过 trait 的 fs_sync 分发 (E6-4 模式)"
    );
    assert!(
        body.contains("m.get_fs()"),
        "P3-I-18: vfs_sync 必须从 mount 取 trait object"
    );
}

#[test]
fn vfs_sync_continues_on_error() {
    // 阶段 4b: vfs_sync 已随 VFS 完整下沉迁至 functions/fs/vfs_mount.rs
    let src = read_src("src/kernel/functions/fs/vfs_mount.rs");
    let marker = "pub fn vfs_sync() -> i32 {";
    let start = src.find(marker).expect("vfs_sync not found");
    let next_fn = src[start..]
        .find("\npub extern \"C\" fn ")
        .map(|o| start + o)
        .unwrap_or(src.len());
    let body = &src[start..next_fn];
    // 单个 FS 失败不应该 return, 而应继续遍历. 检查 `last_err` 变量存在
    assert!(
        body.contains("last_err"),
        "P3-I-18: vfs_sync 须有 last_err 累积, 单 FS 失败不中断"
    );
}

#[test]
fn no_naked_match_fs_type_in_vfs_sync() {
    // 阶段 4b: vfs_sync 已随 VFS 完整下沉迁至 functions/fs/vfs_mount.rs
    let src = read_src("src/kernel/functions/fs/vfs_mount.rs");
    let marker = "pub fn vfs_sync() -> i32 {";
    let start = src.find(marker).expect("vfs_sync not found");
    let next_fn = src[start..]
        .find("\npub extern \"C\" fn ")
        .map(|o| start + o)
        .unwrap_or(src.len());
    let body = &src[start..next_fn];
    // 验证: 没有 match FsType / match fs_type 这类写死分支
    assert!(
        !body.contains("match fs_type") && !body.contains("match FsType::"),
        "P3-I-18: vfs_sync 内部不应再 match fs_type (应走 trait object)"
    );
}

#[test]
fn trait_object_method_signature() {
    // 阶段 4b: FileSystem trait 已随 VFS 完整下沉迁至 functions/fs/vfs_types.rs
    let src = read_src("src/kernel/functions/fs/vfs_types.rs");
    // 简化版: 验证 trait 块里有 fs_sync + KernelResult<()> 两关键词同时出现
    let trait_block = src
        .split_once("pub trait FileSystem: Send + Sync")
        .map(|(_, b)| b)
        .unwrap_or("");
    let has_full_sig = trait_block.contains("fn fs_sync(&self) -> KernelResult<()>")
        || trait_block
            .contains("fn fs_sync(&self) -> crate::functions::fs::vfs_types::KernelResult<()>");
    assert!(
        has_full_sig,
        "P3-I-18: fs_sync 签名必须符合 (KernelResult<()>)"
    );
}

#[test]
fn unkfs_sync_returns_ioerror_on_nonzero() {
    // 拆分后 FileSystem impl 在 unkfs_inode.rs
    let src = read_src("src/kernel/functions/fs/unkfs/unkfs_inode.rs");
    let impl_block = src
        .rsplit_once("impl crate::functions::fs::FileSystem for UnkfsData")
        .map(|(_, b)| b)
        .unwrap_or("");
    // r == 0 → Ok(()); != 0 → Err(Io)
    let sync_block = impl_block
        .split_once("fn fs_sync(")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        sync_block.contains("KernelError::Io") || sync_block.contains("IoError"),
        "P3-I-18: UNKFS.fs_sync 非零返回必须映射为 KernelError::Io"
    );
}
