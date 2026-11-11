#![deny(unsafe_code)]
#![deny(missing_docs)]
//! 统一 fd 对象接口 (`File` trait) + 文件系统薄适配器 (`RegularFile`)
//!
//! ## 设计立场
//!
//! fd 对象模型的单一 trait: 一切 fd 指向的对象 (常规文件 / 管道 / socket /
//! 事件对象) 都以 `Arc<dyn File>` 形态挂在 `OpenFileTable`. 与 `Inode` 的
//! 关系是**叠加适配, 不做继承** — 文件系统对象经 [`RegularFile`] 薄适配器
//! 桥接, `Inode` 保持文件系统中心语义 (readdir/truncate/chmod 等目录与
//! 元数据操作经 `File::as_inode` 回到 Inode).
//!
//! 阶段 2 范围: 抽象落地 (VFS 单分发点). 8 个非 VFS 子系统迁移归阶段 3,
//! `FdPlan`/`subsystem_of`/`sm_slot` 退役归阶段 4. 详见
//! `docs/design/fd-object-model-design.md`.

use alloc::sync::Arc;

use super::inode::Inode;
use super::vfs_types::{KernelError, KernelResult, VfsFileType};

bitflags::bitflags! {
    /// poll 就绪事件位 — 数值与 Linux `<poll.h>` 对齐
    ///
    /// 用户态 ABI 兼容优先: 真实用户程序直接按这些位值使用, 内核内部不再
    /// 另造编码, 避免 syscall 边界翻译.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PollEvents: i16 {
        /// 可读 (`POLLIN`)
        const IN = 1;
        /// 优先数据可读 (`POLLPRI`)
        const PRI = 2;
        /// 可写 (`POLLOUT`)
        const OUT = 4;
        /// 错误 (`POLLERR`)
        const ERR = 8;
        /// 挂断 (`POLLHUP`)
        const HUP = 16;
        /// fd 无效 (`POLLNVAL`)
        const NVAL = 32;
    }
}

/// 统一 fd 对象接口 — 与文件系统解耦.
///
/// 契约:
/// - `poll` 非阻塞、可重复调用, 返回值是请求位的子集 (`ERR`/`HUP` 可附加);
/// - `read_at`/`write_at` 不修改对象级偏移, 偏移由 `OpenFile` 管理;
/// - `release` 在最后一个引用关闭时调用, 且不持 `OpenFileTable` 锁.
pub trait File: Send + Sync + 'static {
    /// 从对象读取 (offset 由分发层提供).
    ///
    /// # Errors
    /// 偏移越界 / 权限不足 / 底层 I/O 失败时返回 `KernelError` (由实现者决定).
    fn read_at(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize>;

    /// 向对象写入 (offset 由分发层提供).
    ///
    /// # Errors
    /// 只读对象 / 容量不足 / 底层 I/O 失败时返回 `KernelError` (由实现者决定).
    fn write_at(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize>;

    /// 就绪查询: 输入请求位, 返回就绪位 (非阻塞, 可重复调用).
    fn poll(&self, events: PollEvents) -> PollEvents;

    /// 对象控制 (ioctl / socket option 统一入口).
    ///
    /// # Errors
    /// 命令号不支持或参数非法时返回 `KernelError`; `NotSupported` 表示对象
    /// 不处理该命令, 由调用方回落到默认策略.
    fn io_control(&self, cmd: u32, arg: usize) -> KernelResult<usize>;

    /// 最后一个引用关闭时回收对象资源 (在 `OpenFileTable` 锁外调用).
    fn release(&self) {}

    /// 对象类型 (`fstat` 的 `st_mode` 派生 + `/proc` 展示).
    fn file_type(&self) -> VfsFileType;

    /// 名字/描述 (默认无名; socket 等对象可给出 `socket:[ino]` 形态).
    fn name(&self) -> Option<&str> {
        None
    }

    /// 前进型读取 (管道 / uart 类一次性流); 默认 `None` = 走 [`File::read_at`].
    fn read_forward(&self, _buf: &mut [u8]) -> Option<KernelResult<usize>> {
        None
    }

    /// 非 seekable 判据: 为 `true` 时 `lseek` 返回 ESPIPE.
    fn stream(&self) -> bool {
        false
    }

    /// 桥接回文件系统 Inode (目录 / 元数据操作经此); 非文件对象返回 `None`.
    fn as_inode(&self) -> Option<&dyn Inode> {
        None
    }

    /// 文件映射后端凭据 (`mmap` 文件映射的合法性判据); 默认不可映射.
    fn mmap_backing(&self) -> Option<MmapBacking> {
        None
    }

    /// 对象在 VFS inode 命名空间 (`pcache` / `inotify` 键空间) 中的编号.
    ///
    /// 默认 `None`: 伪 inode 对象 (管道端点等) 的编号是合成值, 不是命名空间
    /// 成员, 不得作为 `pcache` / `inotify` 的键. 与 [`File::mmap_backing`] 的
    /// 「可映射」判据不同 — 目录 / 设备是真实命名空间成员 (可作 inotify 事件
    /// 源), 但不可作文件映射.
    fn inode_namespace_id(&self) -> Option<u32> {
        None
    }

    /// 类型擦除入口 — 需要对象特有方法 (socket 协议操作) 时按具体类型下转型.
    ///
    /// 通用 `read_at` / `write_at` / `io_control` 只承载无需多参数的请求;
    /// `socket(2)` 系 syscall 的入参组合超出 `io_control` 的 `(u32, usize)` 面,
    /// 由 syscall 层 `downcast_ref` 到对象后走对象方法, 避免在 functions 层
    /// 恢复裸指针.
    fn as_any(&self) -> &dyn core::any::Any;
}

/// 文件映射后端凭据 — 可作为页缓存后端的对象提供的命名空间键.
///
/// `mmap` 只接受**常规文件内容的页缓存后端**: 管道 / 事件对象等虽同样报告
/// `VfsFileType::File`, 但其 inode 编号是伪命名空间 (或根本没有), 绝不能落进
/// `pcache` 的 inode 键空间. 是否存在本凭据由对象自行声明, 不使用类型或
/// `inode_id` 数值猜测.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmapBacking {
    /// 页缓存命名空间键 (对象真实 inode 编号)
    pub inode_id: u32,
    /// 缺页回填时的文件系统挂载点 (`None` = 无挂载点, 如 memfd)
    pub mount_idx: Option<usize>,
}

/// 文件系统对象 → [`File`] 的薄适配器.
///
/// `read_at`/`write_at` 直转 `Inode`; `poll` 对有效对象恒报请求的 `IN|OUT`
/// (内容常驻内存); 目录 / 设备 / 符号链接的差异化就绪位由 epoll 的
/// `VfsPollPolicy` 承担, 本适配器不改写该路径.
pub struct RegularFile {
    /// 文件系统 Inode
    inode: Arc<dyn Inode>,
    /// 对象类型 — 构造时由 `stat` 派生 (失败按普通文件)
    file_type: VfsFileType,
}

impl RegularFile {
    /// 包装文件系统 Inode, 对象类型由 `stat` 派生.
    #[must_use]
    pub fn new(inode: Arc<dyn Inode>, pwm: u64) -> Self {
        let file_type = inode
            .stat(pwm)
            .ok()
            .and_then(|stat| VfsFileType::from_u8(stat.file_type))
            .unwrap_or(VfsFileType::File);
        Self { inode, file_type }
    }
}

impl File for RegularFile {
    fn read_at(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        self.inode.read(offset, buf, pwm)
    }

    fn write_at(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        self.inode.write(offset, buf, pwm)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        // VFS 常规对象内容常驻内存: 有效对象恒可读, 请求写即可写.
        events & (PollEvents::IN | PollEvents::OUT)
    }

    fn io_control(&self, cmd: u32, arg: usize) -> KernelResult<usize> {
        // SIMPLIFIED: 常规文件不实现设备类 ioctl, 统一返回 NotSupported,
        // 由 syscall 层回落终端/默认策略; 影响面 = 文件类 ioctl 保持既有
        // ENOTTY 语义; 何时需扩展: FS_IOC_* 子集接入时.
        let _ = (cmd, arg);
        Err(KernelError::NotSupported)
    }

    fn file_type(&self) -> VfsFileType {
        self.file_type
    }

    fn as_inode(&self) -> Option<&dyn Inode> {
        Some(&*self.inode)
    }

    fn inode_namespace_id(&self) -> Option<u32> {
        Some(self.inode.node_id())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn mmap_backing(&self) -> Option<MmapBacking> {
        // 仅常规文件内容有页缓存后端; 目录 / 设备 / 符号链接不可作文件映射.
        if self.file_type != VfsFileType::File {
            return None;
        }
        let mount = self.inode.mount_idx();
        Some(MmapBacking {
            inode_id: self.inode.node_id(),
            // `Inode::mount_idx` 的 u32::MAX 是「无挂载」哨兵, 在此收口为 None;
            // 直接转 usize 会得到 4294967295 这种越界挂载索引.
            mount_idx: if mount == u32::MAX {
                None
            } else {
                usize::try_from(mount).ok()
            },
        })
    }
}
