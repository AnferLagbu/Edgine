#![deny(unsafe_code)]
//! memfd_create 系统调用实现
//!
//! 创建匿名内存文件，可用于 mmap 共享内存。
//! 使用 AnonymousFs 实现真正的匿名文件 (不依赖 tmpfs)。

// privileged::errno 中性 re-export: ('proc','syscall') 不在 ALLOWED_INTER_DEPS,
// 直接走 functions::syscall 会触发跨模块依赖违规 (见 errno.rs 头注释).
use crate::functions::fs::anonymous::ANONYMOUS_FS;
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::OpenFile;
use crate::privileged::errno::Errno;

/// `MFD_CLOEXEC` 标志位
const MFD_CLOEXEC: u32 = 0x0001;
/// `MFD_ALLOW_SEALING` 标志位
const MFD_ALLOW_SEALING: u32 = 0x0002;
/// `MFD_HUGE_16GB` 标志位 (简化: 不支持大页)
const MFD_HUGE_MASK: u32 = 0x3F << 26;

/// `memfd_create` — 创建匿名内存文件
///
/// # Errors
///
/// - flags 含非法位或请求大页 → `EINVAL`
/// - inode 分配或文件表分配失败 → `ENOMEM`
pub fn memfd_create_syscall(_name_ptr: u64, flags: u32) -> Result<usize, Errno> {
    // 检查 flags 有效性
    let supported_flags = MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_HUGE_MASK;
    if flags & !supported_flags != 0 {
        return Err(Errno::EINVAL);
    }

    // 检查是否支持大页 (暂不支持)
    if flags & MFD_HUGE_MASK != 0 {
        return Err(Errno::EINVAL);
    }

    // 在 AnonymousFs 中分配 inode
    let inode_id = ANONYMOUS_FS.alloc_inode().ok_or(Errno::ENOMEM)?;

    // 创建匿名 Inode
    let inode = crate::functions::fs::inode::new_anonymous_inode(inode_id);

    // 创建 OpenFile (匿名文件)
    let open_file = OpenFile::new_anonymous(
        inode,
        0x0003, // O_RDWR
        crate::privileged::sgeg::session::get_current_pwm(),
        0, // File
    );

    // 插入全局 OpenFile 表
    let handle_id = OPEN_FILE_TABLE.alloc(open_file).ok_or(Errno::ENOMEM)?;

    // 在当前进程 fd 表中分配 fd (MFD_CLOEXEC 决定该 fd 是否随 exec 关闭)
    let Some(fd) = crate::privileged::proc::with_current_fd_table(|t| {
        t.alloc_fd(handle_id, flags & MFD_CLOEXEC != 0)
    })
    .flatten() else {
        // fd 表满或当前进程不可用: 回收已分配的 OpenFile handle, 避免泄漏
        OPEN_FILE_TABLE.close(handle_id);
        return Err(Errno::ENOMEM);
    };

    Ok(fd)
}
