//! VFS 对外 API (syscall 边界)
//!
//! ## 调用方契约
//! - `syscall::sys_read/write/open/close` —— 用户态文件操作
//! - `proc::exec::load_elf` —— 加载 ELF 时通过 VFS 读文件
//! - `sgeg::persist` —— 持久化身份数据
//! - `host-tests` —— host 端单元测试
//!
//! ## 内部接口
//! - `vfs_*_internal` 是核心实现,**不对外**;`vfs_*`
//!   函数将指针参数 (来自用户态/asm) 转为 `&str` 后委托给内部实现。
//!
//! ## 安全约束
//! - 所有 `*_internal` 函数在调用前已验证指针非空与字符串 UTF-8
//!   (委托给 [`CStrExt::as_kstr`])。
//! - `&[u8]` buffer 长度由调用方提供,实现按需截断,不会越界写。
//!
//! ## 性能特征
//! - 静态分发,无 vtable 开销
//! - 字符串路径解析纯栈上,无堆分配 (除路径 split 时 `alloc::string`)
//!
//! ## 模块拆分
//! - 挂载/生命周期/同步/格式化见 [`vfs_mount`] (本模块 `pub use vfs_mount::*`)
//! - 路径/目录/链接/元数据/cwd 见 [`vfs_path`](super::vfs_path) (本模块 `pub use vfs_path::*`)
//! - fd 句柄操作 (open/close/read/write/seek/...) 见 [`handle`](super::handle) (本模块
//!   `pub use handle::*`)
use super::vfs_manager::VFS_MANAGER;
use super::vfs_types::VFS_MAX_PATH;
use crate::privileged::glue::CStrExt;
use crate::privileged::mm::PAGE_SIZE;

/// B2: 4KB 对齐 read 时的 pcache 命中快路径上限 (16 页 = 64KB)
pub(crate) const PCACHE_FAST_MAX_BYTES: usize = 64 * 1024;
/// B2: 4KB 对齐 read 时的 pcache 命中快路径下限 (1 页 = 4KB)
pub(crate) const PCACHE_FAST_MIN_BYTES: usize =
    crate::functions::intcast::usize_from_u64(PAGE_SIZE);

// ============================================================================
// 对外契约: VFS 通过 `VfsOps` 契约注册 (见 privileged::fs::vfs::ops_trait)
// ============================================================================

/// 兼容旧 `ptr_to_str(ptr)` 调用语义:
/// - 空指针 → `""`
/// - 非 UTF-8 → `""`(降级)
/// - 超过 `VFS_MAX_PATH` 长度 → 截断到该上限
///
/// 委托给统一抽象 [`CStrExt::as_kstr`],行为完全一致。
///
/// 可见性: `pub(crate)` 供拆分后的兄弟子模块 `mount` / `path` / `handle` 复用。
pub(crate) fn ptr_to_str<'a>(ptr: *const u8) -> &'a str {
    ptr.as_kstr()
}

/// 拆分父路径与文件名, 供兄弟子模块 `mount` / `path` / `handle` 复用。
pub(crate) fn split_parent_name(rel_path: &str) -> (&str, &str) {
    rel_path.rfind('/').map_or(("/", rel_path), |pos| {
        if pos == 0 {
            ("/", &rel_path[1..])
        } else {
            (&rel_path[..pos], &rel_path[pos + 1..])
        }
    })
}

// ============================================================================
// 公共 VFS API
// ============================================================================

/// 将 Rust &str 转换为 null 终止的 C 字符串并调用 VFS 函数
///
/// # Safety
/// 本函数内部处理 unsafe 指针操作，调用方无需 unsafe。
pub fn with_cstr<F, R>(path: &str, f: F) -> R
where
    F: FnOnce(*const u8) -> R,
{
    let mut buf = alloc::vec::Vec::with_capacity(path.len() + 1);
    buf.extend_from_slice(path.as_bytes());
    buf.push(0);
    f(buf.as_ptr())
}

/// 读取整个文件为 `Vec<u8>` (syscall 路径, 需进程上下文).
///
/// `path` 为 null 终止的 C 字符串首地址; `max_size` 为可读上限, 超过则返回
/// `None` (防止异常 stat 值导致超大分配). stat/open/read 任一环节失败均返回
/// `None`. `pwm` 用于可访问性判定 (与 syscall 边界一致).
///
/// 注意: 本函数经 `vfs_open` 分配本地 fd, 而 fd 挂在**当前进程**的 fd 表上
/// (见 [`vfs_open_internal`]), 故首个用户进程建立之前不可用; 启动期载体读取
/// 请用 [`read_file_kernel`].
#[must_use]
pub fn read_file_to_vec(path: *const u8, pwm: u64, max_size: usize) -> Option<alloc::vec::Vec<u8>> {
    let st = vfs_stat_safe(path, pwm)?;
    let size = usize::try_from(st.size).ok()?;
    if size == 0 || size > max_size {
        return None;
    }

    let Ok(fd) = u32::try_from(vfs_open_safe(path.as_kstr(), 0, pwm)) else {
        return None;
    };

    let mut buf = alloc::vec![0u8; size];
    let n = vfs_read_safe(fd, &mut buf);
    let _ = vfs_close_safe(fd);

    let Ok(n) = usize::try_from(n) else {
        return None;
    };
    if n == 0 {
        return None;
    }
    buf.truncate(n);
    Some(buf)
}

/// 内核态**无 fd** 读整个文件 (启动期载体用).
///
/// 与 [`read_file_to_vec`] 的差别只在"不经过 fd": 后者要先 `vfs_open` 把句柄挂到
/// 当前进程的 fd 表, 而内核早期启动阶段 (`launch_first_user_process`) 尚无进程
/// 上下文, `with_current_fd_table` 返回 `None` ⇒ open 必失败. 本函数直接在
/// `FileSystem` / `Inode` trait 上按显式 offset 读, 不分配 fd, 因此无进程上下文
/// 也可用; 这也是 initramfs 载体让 PID 1 "经文件系统取镜像" 的最小可行链路.
///
/// `path` 为内核侧 Rust 字符串 (非 C 指针); 按当前进程视图归一 (无上下文时为
/// 根视图). `max_size` 为可读上限, 超限或任一环节失败返回 `None`.
#[must_use]
pub fn read_file_kernel(path: &str, pwm: u64, max_size: usize) -> Option<alloc::vec::Vec<u8>> {
    let mut pbuf = [0u8; VFS_MAX_PATH];
    let resolved = VFS_MANAGER.resolve_user_path(path, &mut pbuf)?;
    let Some((mount_idx, _fs_type, Some(fs))) = VFS_MANAGER.resolve_mount_fs(resolved) else {
        return None;
    };
    let rel_path = VFS_MANAGER.get_relative_path(resolved, mount_idx);

    let stat = fs.fs_stat(rel_path, pwm).ok()?;
    let size = usize::try_from(stat.size).ok()?;
    if size == 0 || size > max_size {
        return None;
    }

    let inode = fs.fs_open(rel_path, 0, pwm).ok()?;
    let mut buf = alloc::vec![0u8; size];
    // 单次读满: RamFS `read_at_offset` 内部按块循环, 与 [`read_file_to_vec`]
    // 走 `vfs_read_safe(fd, &mut buf)` 的调用形状一致.
    let Ok(n) = inode.read(0, &mut buf, pwm) else {
        return None;
    };
    if n == 0 {
        return None;
    }
    buf.truncate(n);
    Some(buf)
}

/// 内核态**无 fd** 建文件并一次性写入 (initramfs 落盘用).
///
/// 存在的理由同 [`read_file_kernel`]: `vfs_open(O_CREAT)` + `vfs_write` 需要 per-process
/// fd 表, 启动期不可用. 本函数走 `fs_create` 拿 `Inode` 再按 offset 0 写, 不碰 fd 表.
///
/// 目标已存在时不覆盖 (`RamFS` `fs_create` 返回 `EEXIST`) —— initramfs 解到刚挂载的
/// 空 ramfs 上, 重名只可能是归档自身的问题, 静默覆盖会掩盖它.
/// 成功返回 `true`; 挂载点不可解析、创建失败或短写返回 `false`.
#[must_use]
pub fn write_file_kernel(path: &str, data: &[u8], pwm: u64) -> bool {
    let mut pbuf = [0u8; VFS_MAX_PATH];
    let Some(resolved) = VFS_MANAGER.resolve_user_path(path, &mut pbuf) else {
        return false;
    };
    let Some((mount_idx, _fs_type, Some(fs))) = VFS_MANAGER.resolve_mount_fs(resolved) else {
        return false;
    };
    let rel_path = VFS_MANAGER.get_relative_path(resolved, mount_idx);
    let (parent_path, name) = split_parent_name(rel_path);

    let Ok(inode) = fs.fs_create(parent_path, name, pwm) else {
        return false;
    };
    if data.is_empty() {
        return true;
    }
    matches!(inode.write(0, data, pwm), Ok(n) if n == data.len())
}

// ============================================================================
// 拆分后的子模块 re-export — 保持对外符号名与调用路径不变
// ============================================================================
pub use super::handle::*;
pub use super::vfs_mount::*;
pub use super::vfs_path::*;
