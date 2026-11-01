#![deny(unsafe_code)]
#![deny(missing_docs)]
//! 进程文件系统上下文 — cwd / 根前缀 / umask（K-03）
//!
//! ## 口径（D7 / K-03）
//!
//! 每个 TCB（[`Process`](super::process::Process)）持有 [`Arc<FsContext>`]：
//!
//! - `CLONE_FS` 置位时子线程共享父线程的 `Arc`（`chdir` / `chroot` /
//!   `umask` 对全组可见）；
//! - 未置位时 `fork` / `clone` 深拷贝一份快照，互不影响。
//!
//! ## 与全局缺省的关系
//!
//! 无进程上下文的早期启动阶段与内核线程路径仍以 `VfsManager` 的全局
//! cwd / root 为缺省；有进程上下文时以本对象为准。umask 的全局缺省
//! 由 functions 侧保留（见 `functions::fs::mode`）。
//!
//! ## 锁域
//!
//! `cwd` / `root` 的 `IrqSpinLock` 属进程属性级（`ref-lock-order.md`
//! 层级 9）；`umask` 为原子量。路径解析在取 VFS 锁之前读取本对象。
//!
//! [`Arc<FsContext>`]: alloc::sync::Arc

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::privileged::sync::IrqSpinLock;

/// FS 上下文路径缓冲上限（与 `functions::fs::VFS_MAX_PATH` 必须一致）。
pub const FS_MAX_PATH: usize = 128;

/// 默认文件创建掩码（与历史全局缺省一致）。
pub const DEFAULT_UMASK: u32 = 0o022;

/// 根路径常量（NUL 结尾，其余字节为 0）。
const DEFAULT_PATH: [u8; FS_MAX_PATH] = {
    let mut path = [0u8; FS_MAX_PATH];
    path[0] = b'/';
    path
};

/// 进程文件系统上下文（cwd / 根前缀 / umask）。
///
/// 同一 `Arc` 被 `CLONE_FS` 线程共享；`copy_from` 用于 fork / 非共享
/// clone 的深拷贝。
pub struct FsContext {
    /// 当前工作目录（视图路径，NUL 结尾）。
    cwd: IrqSpinLock<[u8; FS_MAX_PATH]>,
    /// 根前缀（真实路径，NUL 结尾）；默认 "/"。
    root: IrqSpinLock<[u8; FS_MAX_PATH]>,
    /// 文件创建掩码（仅低 9 位有效）。
    umask: AtomicU32,
}

impl Default for FsContext {
    fn default() -> Self {
        Self::new_default()
    }
}

impl FsContext {
    /// 构造默认上下文：cwd = "/"、root = "/"、umask = [`DEFAULT_UMASK`]。
    #[must_use]
    pub const fn new_default() -> Self {
        Self {
            cwd: IrqSpinLock::new(DEFAULT_PATH),
            root: IrqSpinLock::new(DEFAULT_PATH),
            umask: AtomicU32::new(DEFAULT_UMASK),
        }
    }

    /// 设置工作目录（视图路径；超长部分截断，同 `VfsManager::set_cwd`）。
    pub fn set_cwd(&self, path: &str) {
        write_path(&mut self.cwd.lock(), path);
    }

    /// 读取工作目录到 `out`，返回有效字节数（不含 NUL）。
    #[must_use]
    pub fn cwd_into(&self, out: &mut [u8; FS_MAX_PATH]) -> usize {
        read_path(&self.cwd.lock(), out)
    }

    /// 读取工作目录为 `String`（非 UTF-8 时回落 "/"）。
    #[must_use]
    pub fn cwd_string(&self) -> String {
        let mut buf = [0u8; FS_MAX_PATH];
        let len = self.cwd_into(&mut buf);
        path_string(&buf[..len])
    }

    /// 设置根前缀（真实路径），并把工作目录重置为视图根 "/"。
    pub fn set_root(&self, real_root: &str) {
        let mut root = self.root.lock();
        write_path(&mut root, real_root);
        if root[0] == 0 {
            root.copy_from_slice(&DEFAULT_PATH);
        }
        drop(root);
        self.set_cwd("/");
    }

    /// 读取根前缀到 `out`，返回有效字节数（不含 NUL）。
    #[must_use]
    pub fn root_into(&self, out: &mut [u8; FS_MAX_PATH]) -> usize {
        read_path(&self.root.lock(), out)
    }

    /// 读取根前缀为 `String`（非 UTF-8 时回落 "/"）。
    #[must_use]
    pub fn root_string(&self) -> String {
        let mut buf = [0u8; FS_MAX_PATH];
        let len = self.root_into(&mut buf);
        path_string(&buf[..len])
    }

    /// 把 `src` 的 cwd / root / umask 深拷贝进本上下文（fork / 非共享 clone）。
    ///
    /// 先快照 `src`（锁作用域内拷贝到栈数组），再写入自身，避免同时持有
    /// 两个实例的锁。`src` 与 `self` 必须是不同实例。
    pub fn copy_from(&self, src: &FsContext) {
        let (src_cwd, src_root) = {
            let cwd = src.cwd.lock();
            let root = src.root.lock();
            (*cwd, *root)
        };
        let src_umask = src.umask.load(Ordering::Acquire);
        *self.cwd.lock() = src_cwd;
        *self.root.lock() = src_root;
        self.umask.store(src_umask, Ordering::Release);
    }

    /// 设置 umask，返回旧值（仅保留低 9 位）。
    #[must_use]
    pub fn set_umask(&self, mask: u32) -> u32 {
        self.umask.swap(mask & 0o777, Ordering::SeqCst)
    }
}

/// 把 `path` 写入 NUL 结尾的定长缓冲（超长截断）。
fn write_path(dst: &mut [u8; FS_MAX_PATH], path: &str) {
    let bytes = path.as_bytes();
    let len = bytes.len().min(FS_MAX_PATH - 1);
    dst[..len].copy_from_slice(&bytes[..len]);
    dst[len] = 0;
}

/// 从 NUL 结尾的定长缓冲读出有效字节。
fn read_path(src: &[u8; FS_MAX_PATH], out: &mut [u8; FS_MAX_PATH]) -> usize {
    let len = src.iter().position(|&b| b == 0).unwrap_or(FS_MAX_PATH);
    out[..len].copy_from_slice(&src[..len]);
    len
}

/// 字节路径转 `String`（非 UTF-8 回落 "/"）。
fn path_string(bytes: &[u8]) -> String {
    String::from(core::str::from_utf8(bytes).unwrap_or("/"))
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_UMASK, FS_MAX_PATH, FsContext};

    #[test]
    fn default_context_starts_at_root() {
        let ctx = FsContext::new_default();
        assert_eq!(ctx.cwd_string(), "/");
        assert_eq!(ctx.root_string(), "/");
        assert_eq!(ctx.set_umask(DEFAULT_UMASK), DEFAULT_UMASK);
    }

    #[test]
    fn copy_from_clones_all_fields() {
        let parent = FsContext::new_default();
        parent.set_cwd("/home/user");
        parent.set_root("/jail");
        assert_eq!(parent.set_umask(0o077), DEFAULT_UMASK);
        // set_root 会把 cwd 重置为 "/"，此处再设一次以验证 cwd 拷贝
        parent.set_cwd("/home/user");

        let child = FsContext::new_default();
        child.copy_from(&parent);

        assert_eq!(child.cwd_string(), "/home/user");
        assert_eq!(child.root_string(), "/jail");
        assert_eq!(child.set_umask(0o007), 0o077, "umask 应随复制继承");
    }

    #[test]
    fn set_root_resets_cwd_and_empty_root_falls_back() {
        let ctx = FsContext::new_default();
        ctx.set_cwd("/tmp");
        ctx.set_root("/jail");
        assert_eq!(ctx.cwd_string(), "/", "切根必须重置 cwd");
        assert_eq!(ctx.root_string(), "/jail");

        ctx.set_root("");
        assert_eq!(ctx.root_string(), "/", "空根必须回落默认根");
    }

    #[test]
    fn path_roundtrip_truncates_to_buffer() {
        let ctx = FsContext::new_default();
        let long = "a".repeat(FS_MAX_PATH + 32);
        ctx.set_cwd(&long);
        let mut out = [0u8; FS_MAX_PATH];
        let n = ctx.cwd_into(&mut out);
        assert_eq!(n, FS_MAX_PATH - 1, "路径必须截断到缓冲上限内");
        assert_eq!(out[n], 0, "截断后必须保留 NUL 结尾");
    }
}
