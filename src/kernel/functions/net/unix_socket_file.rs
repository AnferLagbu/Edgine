#![deny(unsafe_code)]
//! `UnixSocketFile` — UDS socket fd 对象的 functions 层实现
//!
//! ## fd 对象模型 (阶段 3 第 4 个迁移子系统)
//!
//! UDS socket 不再以"全局编号区间 `[1000, 1016)` + 全局位图"的 fd 形态直接
//! 暴露给用户, 而是 [`UnixSocketFile`] 对象挂在全局 `OpenFileTable`, 用户 fd
//! 由 per-process `FdTable` 分配 (与 VFS / Smoltcp socket fd 同源). 对象内部
//! 只持有 `functions::net::unix` 策略表的**槽位下标** (`idx`), 槽位的生灭由
//! 对象 `release()` 驱动: 最后一个 fd 引用关闭 → `release()` → `uds_free(idx)`.
//! 由此与其它 fd 对象同构地获得:
//!
//! - **close / dup / fork**: 引用计数语义与 VFS fd 一致;
//! - **CLOEXEC**: fd 级标志落 per-process fd 表;
//! - **epoll / poll**: 对象实现 [`File::poll`], 经通用对象分发求值;
//! - **阻塞状态**: 由 `OpenFile::nonblock` 单点持有.
//!
//! 对象**不实现** `Inode` (`as_inode() == None`), 不参与 pcache / inotify
//! 命名空间, 也不可作 mmap 后端.

use alloc::sync::Arc;

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::functions::net::unix as uds;
use crate::privileged::syscall::Errno;

/// UDS socket fd 对象 — 仅持策略表槽位下标
pub struct UnixSocketFile {
    /// `functions::net::unix` 策略表槽位下标
    idx: u8,
}

impl UnixSocketFile {
    /// 以策略表槽位下标构造对象 (槽位所有权转移给本对象)
    #[must_use]
    pub fn new(idx: u8) -> Self {
        Self { idx }
    }

    /// 策略表槽位下标 — syscall 层委托 UDS 策略入口时使用
    #[must_use]
    pub fn idx(&self) -> u8 {
        self.idx
    }

    /// 策略错误 → errno (`UnixSocketError` 为 UDS 错误映射的单一来源)
    fn errno(e: uds::UdsError) -> Errno {
        uds::UnixSocketError::from(e).to_errno()
    }

    /// 对象 → `OpenFileTable` → per-process fd 表; 任一步失败归还槽位
    fn attach(idx: u8, nonblock: bool, cloexec: bool) -> Result<usize, Errno> {
        let pwm = crate::privileged::sgeg::pwm_get_current();
        let open_file = OpenFile::from_file(Arc::new(Self::new(idx)), 0, pwm);
        open_file.set_nonblock(nonblock);
        let Some(handle_id) = OPEN_FILE_TABLE.alloc(open_file) else {
            // 对象未挂表 → release() 不会被调用, 槽位须在此显式归还
            let _ = uds::uds_free(idx);
            return Err(Errno::Emfile);
        };
        let Some(fd_idx) =
            crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, cloexec))
                .flatten()
        else {
            // 挂表成功但 fd 表耗尽: close 触发最后一个引用的 release() → uds_free
            OPEN_FILE_TABLE.close(handle_id);
            return Err(Errno::Emfile);
        };
        Ok(fd_idx)
    }

    /// 创建 UDS socket fd (`socket(2)` 的 `AF_UNIX` 分支)
    ///
    /// # Errors
    ///
    /// 策略表槽位耗尽 → `ENOMEM`; `OpenFileTable` 或 per-process fd 表耗尽 → `EMFILE`.
    pub fn create(
        sock_type: uds::UnixSockType,
        nonblock: bool,
        cloexec: bool,
    ) -> Result<usize, Errno> {
        let idx = uds::uds_alloc(sock_type).map_err(Self::errno)?;
        Self::attach(idx, nonblock, cloexec)
    }

    /// 接受连接并创建新 fd (`accept(2)` 的 `AF_UNIX` 分支)
    ///
    /// 新 fd 不继承监听端的非阻塞位与 CLOEXEC (POSIX accept 语义).
    ///
    /// # Errors
    ///
    /// 监听槽位无效 / 非流式 / 非 Listening → 对应 errno; 待连接队列为空 →
    /// `EAGAIN`; 策略表槽位或 fd 表耗尽 → `ENOMEM` / `EMFILE`.
    pub fn create_accepted(listen_idx: u8, nonblock: bool, cloexec: bool) -> Result<usize, Errno> {
        let idx = uds::uds_accept(listen_idx).map_err(Self::errno)?;
        Self::attach(idx, nonblock, cloexec)
    }

    /// 创建一对互相连接的 UDS socket fd (`socketpair(2)`)
    ///
    /// 第二端挂表失败时经 VFS 共享关闭链回收第一端 (release → `uds_free`).
    ///
    /// # Errors
    ///
    /// 策略表槽位耗尽 → `ENOMEM`; `OpenFileTable` 或 per-process fd 表耗尽 → `EMFILE`.
    pub fn create_pair(
        sock_type: uds::UnixSockType,
        nonblock: bool,
        cloexec: bool,
    ) -> Result<(usize, usize), Errno> {
        let (idx0, idx1) = uds::uds_socketpair(sock_type).map_err(Self::errno)?;
        let fd0 = match Self::attach(idx0, nonblock, cloexec) {
            Ok(fd) => fd,
            Err(e) => {
                let _ = uds::uds_free(idx1);
                return Err(e);
            }
        };
        match Self::attach(idx1, nonblock, cloexec) {
            Ok(fd1) => Ok((fd0, fd1)),
            Err(e) => {
                // 第一端已用户可见: 必须走共享关闭链回收, 不得绕过引用计数
                let _ = crate::functions::fs::api::vfs_close_safe(
                    crate::functions::intcast::low_u32_from_usize(fd0),
                );
                Err(e)
            }
        }
    }
}

impl File for UnixSocketFile {
    // SIMPLIFIED: read(2)/write(2) 由 VFS 分发层在 `OpenFileTable` 自旋锁内
    // 调用, 不得睡眠 — UDS 策略本身非阻塞 (空缓冲返 EAGAIN), 故无需额外
    // MSG_DONTWAIT 合成. 影响面 = UDS fd 上的 read/write 恒非阻塞 (迁移前
    // UDS fd 不在 VFS 表, read/write 返 EBADF, 无既有用户). 何时需扩展:
    // UDS 引入阻塞等待队列后, 按 `OpenFile::nonblock` 分流阻塞/非阻塞.
    fn read_at(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        if buf.is_empty() {
            return Err(KernelError::InvalidArgument);
        }
        let sock_type =
            uds::uds_sock_type(self.idx).map_err(|e| KernelError::from_i32(e as i32))?;
        let rc = match sock_type {
            uds::UnixSockType::Stream => uds::uds_recv(self.idx, buf),
            uds::UnixSockType::Dgram => uds::uds_recvfrom(self.idx, buf),
        };
        rc.map_err(|e| KernelError::from_i32(e as i32))
    }

    fn write_at(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        if buf.is_empty() {
            return Err(KernelError::InvalidArgument);
        }
        // Stream 委托 uds_send; Dgram 走已连接发送 (无路径写语义)
        uds::uds_send_connected(self.idx, buf).map_err(|e| KernelError::from_i32(e as i32))
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        uds::uds_poll(self.idx, events)
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn release(&self) {
        // 最后一个 fd 引用关闭: 解绑路径 / 通知对端 / 归还策略表槽位
        let _ = uds::uds_free(self.idx);
    }

    // SIMPLIFIED: `VfsFileType` 无 SOCK 变体 — UDS socket 以 File 呈现 (与
    // Smoltcp socket 同构); 影响面 = fstat 的 st_mode 报普通文件; 何时需扩展:
    // VfsFileType 增加 Sock 变体并贯通 stat 编码时.
    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn stream(&self) -> bool {
        // POSIX: socket 不可定位 (lseek → ESPIPE)
        true
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ============================================================================
// host 侧单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_contract_sentinels() {
        // 槽位下标不触碰 UDS 策略表 (read/poll 会访问, 此处仅验证纯对象面)
        let file = UnixSocketFile::new(3);
        assert_eq!(file.idx(), 3, "槽位下标原样保存");
        assert!(file.stream(), "socket 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "socket 非 inode 承载");
        assert!(file.mmap_backing().is_none(), "socket 不可作 mmap 后端");
        assert!(
            file.inode_namespace_id().is_none(),
            "socket 不属于 inode 命名空间"
        );
        assert_eq!(file.file_type(), VfsFileType::File);
        assert_eq!(
            file.io_control(0x1234_5678, 0),
            Err(KernelError::NotSupported),
            "socket 对象不处理通用 ioctl (回落默认策略)"
        );
    }

    #[test]
    fn object_downcast_round_trip() {
        // syscall 层经 `as_any().downcast_ref::<UnixSocketFile>()` 取槽位
        let file = UnixSocketFile::new(7);
        let erased: &dyn File = &file;
        let recovered = erased
            .as_any()
            .downcast_ref::<UnixSocketFile>()
            .expect("UnixSocketFile 必须可下转型");
        assert_eq!(recovered.idx(), 7);
    }
}
