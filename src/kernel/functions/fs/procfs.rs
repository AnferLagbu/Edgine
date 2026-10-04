#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! 进程文件系统 (`ProcFS`) — functions 层安全代理 (Phase 2.2.3)
//!
//! 在 `kernel/fs/procfs` 基础上提供 100% safe 的公共 API,
//! 暴露 /proc 虚拟文件系统 (current/sys/cpu/sys/memory/sys/config/...).
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 内部 `ProcfsData` 已由 P2.2.3 标记为安全 (Send+Sync)
//! - **类型安全**: 条目类型用 `ProcEntryKind` 枚举, 而非裸 `u8`
//! - **薄包装**: 透传 `mount/add_process/remove_process/read/readdir`
//! - **可替代**: 原 `kernel/fs/procfs/procfs.rs` 仍存在, 本文件是迁移目标
//!
//! 评估日期: 2026-06-04
//! Phase 2.2.3 任务: 进程文件系统迁移

use crate::functions::error::KernelError;
use crate::functions::fs::procfs_core::{PROCFS_MAX_NAME, ProcfsData};

// ============================================================================
// 条目类型
// ============================================================================

/// `ProcFS` 条目类型 (强类型枚举)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProcEntryKind {
    /// 普通目录
    Dir = 1,
    /// 当前进程 (self)
    Current = 2,
    /// 进程条目
    Process = 3,
    /// 文件
    File = 4,
}

impl ProcEntryKind {
    /// 从 `u8` 解析
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Dir),
            2 => Some(Self::Current),
            3 => Some(Self::Process),
            4 => Some(Self::File),
            _ => None,
        }
    }
}

/// `ProcFS` 条目 (服务层视图)
#[derive(Debug, Clone)]
pub struct ProcEntry {
    /// 条目名
    pub name: alloc::string::String,
    /// 关联 PID (process 条目用)
    pub pid: u32,
    /// 条目类型
    pub kind: ProcEntryKind,
}

// ============================================================================
// 安全 ProcFS 代理
// ============================================================================

/// `ProcFS` 安全代理 (functions 层)。
pub struct SafeProcFs {
    inner: &'static ProcfsData,
}

impl SafeProcFs {
    /// 创建全局 `ProcFS` 代理
    pub fn new() -> Self {
        Self {
            inner: &crate::functions::fs::procfs_core::PROCFS_DATA,
        }
    }

    /// 挂载 `ProcFS`
    ///
    /// # 返回
    /// 成功: Ok(()); 失败: `KernelError`
    ///
    /// # Errors
    /// 当底层挂载操作失败时返回 `Io`.
    pub fn mount(&self, mount_point: &str) -> Result<(), KernelError> {
        let rc = self.inner.mount(mount_point);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    /// 注册进程
    ///
    /// # Errors
    /// 当进程表已满、无法容纳新进程时返回 `NoSpace`.
    pub fn add_process(&self, pid: u32, name: &str) -> Result<(), KernelError> {
        let rc = self.inner.add_process(pid, name);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::NoSpace)
        }
    }

    /// 注销进程
    ///
    /// # Errors
    /// 当指定 PID 的进程条目不存在时返回 `NoSuchProcess`.
    pub fn remove_process(&self, pid: u32) -> Result<(), KernelError> {
        let rc = self.inner.remove_process(pid);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::NoSuchProcess)
        }
    }

    /// 读条目内容
    ///
    /// # 返回
    /// 成功: 实际写入 `buf` 的字节数
    ///
    /// # Errors
    /// 当条目不存在或底层读取失败时返回 `Io`.
    pub fn read(&self, name: &str, buf: &mut [u8]) -> Result<usize, KernelError> {
        let rc = self.inner.read(name, buf);
        if rc < 0 {
            Err(KernelError::Io)
        } else {
            Ok(rc as usize)
        }
    }

    /// 枚举条目
    pub fn readdir(&self, index: usize) -> Option<ProcEntry> {
        let (raw_name, pid, raw_kind) = self.inner.readdir(index)?;
        let kind = ProcEntryKind::from_u8(raw_kind)?;
        let end = raw_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(raw_name.len());
        let name = alloc::string::String::from_utf8_lossy(&raw_name[..end]).into_owned();
        Some(ProcEntry { name, pid, kind })
    }

    /// 条目总数
    pub fn entry_count(&self) -> u32 {
        self.inner.entry_count()
    }
}

impl Default for SafeProcFs {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 全局实例
// ============================================================================

// I-16: 替换 spin::Once → 项目自研 functions::sync::once::OnceCell
use crate::functions::sync::once::OnceCell;
static GLOBAL_PROCFS: OnceCell<SafeProcFs> = OnceCell::new();

/// 初始化全局 `ProcFS`
pub fn init_global() {
    let _ = GLOBAL_PROCFS.get_or_init(|slot| {
        slot.write(SafeProcFs::new());
    });
}

/// 获取全局 `ProcFS` 引用
///
/// # Safety
/// 调用前需保证 `init_global` 已执行
///
/// # Panics
/// 当 `init_global()` 尚未被调用、全局实例未初始化时发生 panic (内部使用 `expect`).
pub fn global() -> &'static SafeProcFs {
    GLOBAL_PROCFS
        .get()
        .expect("procfs::global() called before init_global()")
}

// ============================================================================
// 便利函数
// ============================================================================

/// 注册进程到全局 `ProcFS`
///
/// # Errors
/// 错误条件与 [`SafeProcFs::add_process`] 相同, 参见其 `# Errors` 段.
pub fn add_process(pid: u32, name: &str) -> Result<(), KernelError> {
    global().add_process(pid, name)
}

/// 注销进程
///
/// # Errors
/// 错误条件与 [`SafeProcFs::remove_process`] 相同, 参见其 `# Errors` 段.
pub fn remove_process(pid: u32) -> Result<(), KernelError> {
    global().remove_process(pid)
}

/// 读全局 `ProcFS` 条目
///
/// # Errors
/// 错误条件与 [`SafeProcFs::read`] 相同, 参见其 `# Errors` 段.
pub fn read(name: &str, buf: &mut [u8]) -> Result<usize, KernelError> {
    global().read(name, buf)
}

/// 条目名最大长度
pub const fn max_name_len() -> usize {
    PROCFS_MAX_NAME
}
