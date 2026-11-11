#![deny(unsafe_code)]
//! `SocketFile` — smoltcp socket fd 对象的 functions 层实现
//!
//! ## fd 对象模型 (阶段 3 第 3 个迁移子系统)
//!
//! smoltcp socket 不再以"协议栈槽位 + 全局数值区间 `[64, 320)`"的 fd 形态
//! 直接暴露给用户, 而是 [`SocketFile`] 对象挂在全局 `OpenFileTable`, 用户
//! fd 由 per-process `FdTable` 分配 (与 VFS fd 同源). 对象内部只持有 smoltcp
//! 协议栈的**槽位编号** (`slot`), 槽位的生灭由对象 `release()` 驱动:
//! 最后一个 fd 引用关闭 → `release()` → `sm_close(slot)` (撤组 + 三步关闭序
//! + 槽位归还). 由此天然获得:
//!
//! - **close / dup / fork**: 引用计数语义与 VFS fd 完全一致;
//! - **CLOEXEC**: fd 级标志落 per-process fd 表, exec 时统一关闭;
//! - **epoll / poll**: 对象实现 [`File::poll`], 经通用对象分发求值;
//! - **阻塞状态**: 由 `OpenFile::nonblock` 单点持有, syscall 层据此决定
//!   是否向 `sm_*` 传入 `blocking`.
//!
//! 对象**不实现** `Inode` (`as_inode() == None`), 该判据同时用于:
//! sendfile/splice 排除特殊对象、写循环对特殊对象短写即停、`fstat` 合成 stat.
//!
//! ## 语义
//!
//! - read/write: 非阻塞 (见 [`File::read_at`] 的 SIMPLIFIED 注释); 阻塞收发
//!   走 `send`/`recv`/`sendto`/`recvfrom`/`sendmsg`/`recvmsg` 系统调用.
//! - poll: 委托 `sm_socket_poll`, 位值与 Linux `<poll.h>` 同构.
//! - close: `release()` → `sm_close(slot)`.

use alloc::sync::Arc;

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::privileged::net_socket;
use crate::privileged::syscall::Errno;

/// `MSG_DONTWAIT` — 对象内 read/write 强制非阻塞 (`UAPI <sys/socket.h>`, 0x40)
const MSG_DONTWAIT: i32 = 0x40;

/// smoltcp socket fd 对象 — 仅持协议栈槽位编号
pub struct SocketFile {
    /// 协议栈槽位编号 (`sm_socket` / `sm_accept` 返回值)
    slot: i32,
}

impl SocketFile {
    /// 以协议栈槽位编号构造对象 (槽位所有权转移给本对象)
    #[must_use]
    pub fn new(slot: i32) -> Self {
        Self { slot }
    }

    /// 协议栈槽位编号 — syscall 层委托 `privileged::net::syscall` 入口时使用
    #[must_use]
    pub fn slot(&self) -> i32 {
        self.slot
    }

    /// 创建 socket fd: 对象 → `OpenFileTable` → per-process fd 表
    ///
    /// 槽位所有权在本函数入口转移给对象语义: 任一步失败都归还协议栈槽位.
    ///
    /// # Errors
    ///
    /// `OpenFileTable` 或 per-process fd 表耗尽 → `EMFILE`
    pub fn create(slot: i32, nonblock: bool, cloexec: bool) -> Result<usize, Errno> {
        let pwm = crate::privileged::sgeg::pwm_get_current();
        let open_file = OpenFile::from_file(Arc::new(Self::new(slot)), 0, pwm);
        open_file.set_nonblock(nonblock);
        let Some(handle_id) = OPEN_FILE_TABLE.alloc(open_file) else {
            // 对象未挂表 → release() 不会被调用, 槽位须在此显式归还
            let _ = net_socket::sm_net_close(slot);
            return Err(Errno::Emfile);
        };
        let Some(fd_idx) =
            crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, cloexec))
                .flatten()
        else {
            // 挂表成功但 fd 表耗尽: close 触发最后一个引用的 release() → sm_close
            OPEN_FILE_TABLE.close(handle_id);
            return Err(Errno::Emfile);
        };
        Ok(fd_idx)
    }
}

impl File for SocketFile {
    // SIMPLIFIED: read(2)/write(2) 由 VFS 分发层在 `OpenFileTable` 自旋锁内
    // 调用, 不得睡眠 — 故以 MSG_DONTWAIT 强制非阻塞. 影响面 = socket 上的
    // read/write 恒非阻塞 (迁移前 socket fd 不在 VFS 表, read/write 返 EBADF,
    // 无既有用户); 阻塞收发仍走 send/recv 系列 syscall. 何时需扩展: VFS
    // 分发层改为锁外 I/O 后, 按 `OpenFile::nonblock` 传入阻塞语义.
    fn read_at(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        if buf.is_empty() {
            return Err(KernelError::InvalidArgument);
        }
        let rc = net_socket::sm_net_recv(
            self.slot,
            buf.as_mut_ptr() as usize,
            crate::functions::intcast::low_u32_from_usize(buf.len()),
            MSG_DONTWAIT,
        );
        match rc {
            // SIMPLIFIED: TCP 空缓冲+连接存活时 sm_recv 返 0 (非 EOF), UDP 零长
            // 数据报同样返 0 — read(2) 侧统一按 EAGAIN 报告; 影响面 = 零长 UDP
            // 数据报经 read(2) 读不出 (阻塞 recvfrom/recvmsg 路径不受影响).
            // 何时需扩展: sm_recv 能区分"无数据"与"零长数据报"时.
            0 => Err(KernelError::WouldBlock),
            n if n > 0 => Ok(crate::functions::intcast::usize_from_i32(n)),
            n => Err(KernelError::from_i32(-n)),
        }
    }

    fn write_at(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        if buf.is_empty() {
            return Err(KernelError::InvalidArgument);
        }
        let rc = net_socket::sm_net_send(
            self.slot,
            buf.as_ptr() as usize,
            crate::functions::intcast::low_u32_from_usize(buf.len()),
            MSG_DONTWAIT,
        );
        match rc {
            0 => Err(KernelError::WouldBlock),
            n if n > 0 => Ok(crate::functions::intcast::usize_from_i32(n)),
            n => Err(KernelError::from_i32(-n)),
        }
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        PollEvents::from_bits_truncate(net_socket::sm_socket_poll(self.slot, events.bits()))
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn release(&self) {
        // 最后一个 fd 引用关闭: 撤销组成员资格 + 三步关闭序 + 槽位归还
        let _ = net_socket::sm_net_close(self.slot);
    }

    // SIMPLIFIED: `VfsFileType` 无 SOCK 变体 — socket 以 File 呈现 (与管道/
    // eventfd 同构); 影响面 = fstat 的 st_mode 报普通文件; 何时需扩展:
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
        // 槽位编号不触碰协议栈 (read/poll 会访问, 此处仅验证纯对象面)
        let file = SocketFile::new(3);
        assert_eq!(file.slot(), 3, "槽位编号原样保存");
        assert!(file.stream(), "socket 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "socket 非 inode 承载");
        assert_eq!(file.file_type(), VfsFileType::File);
        assert_eq!(
            file.io_control(0x1234_5678, 0),
            Err(KernelError::NotSupported),
            "socket 对象不处理通用 ioctl (回落默认策略)"
        );
    }

    #[test]
    fn object_downcast_round_trip() {
        // syscall 层经 `as_any().downcast_ref::<SocketFile>()` 取槽位
        let file = SocketFile::new(7);
        let erased: &dyn File = &file;
        let recovered = erased
            .as_any()
            .downcast_ref::<SocketFile>()
            .expect("SocketFile 必须可下转型");
        assert_eq!(recovered.slot(), 7);
    }

    #[test]
    fn poll_events_bit_values_match_linux() {
        // 对象 poll 结果按 <poll.h> 位值直接回写用户 (见 file_ops::poll_syscall)
        assert_eq!(PollEvents::IN.bits(), 1);
        assert_eq!(PollEvents::OUT.bits(), 4);
        assert_eq!(PollEvents::ERR.bits(), 8);
        assert_eq!(PollEvents::HUP.bits(), 16);
        assert_eq!(PollEvents::NVAL.bits(), 32);
        assert_eq!(MSG_DONTWAIT, 0x40, "MSG_DONTWAIT 与 UAPI 一致");
    }
}
