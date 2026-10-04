//! VFS 文件类型 POD — privileged 层保留
//!
//! ## 阶段 4b 抽取记录
//!
//! 阶段 4b VFS 实现整体下沉 functions 后, `privileged/fs` 仅保留 2 契约
//! (`ops_trait` + `vfs_poll_trait`) + 本 POD 类型. `VfsFileType` 是
//! privileged 侧残留消费者 (`syscall/epoll.rs` 与 `vfs_poll_trait.rs`)
//! 唯一需要的 VFS 值类型, 故单列于此, 不随 `types.rs` 其余定义下沉.
//!
//! functions 侧经 `crate::functions::fs::vfs_types` re-export 本类型, 保证
//! 既有调用方路径不变.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsFileType {
    File,
    Dir,
    Dev,
    Symlink,
}

impl VfsFileType {
    /// 从 u8 构造 `VfsFileType`
    ///
    /// 返回 `None` 表示非法值 (0-3 为合法值)
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::File),
            1 => Some(Self::Dir),
            2 => Some(Self::Dev),
            3 => Some(Self::Symlink),
            _ => None,
        }
    }

    /// 转 u8 编码 (与 `from_u8` 互逆)
    pub fn as_u8(self) -> u8 {
        match self {
            Self::File => 0,
            Self::Dir => 1,
            Self::Dev => 2,
            Self::Symlink => 3,
        }
    }
}
