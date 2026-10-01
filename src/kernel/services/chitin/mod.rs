#![deny(unsafe_code)]
//! Chitin 设备驱动框架 — services 层安全代理
//!
//! ## 状态
//!
//! 已完成 5/5 子系统迁移 (chitin 整体), 封装 `kernel::chitin::*` 老 API:
//! - [x] chitin (本文件) — 设备注册表/查找/块设备 IO/字符设备 IO/字符设备统一入口
//! - [x] devtree — 设备树 (Phase 2.4 已迁移)
//! - [x] composite — 复合设备 (Phase 2.4 已迁移)
//! - [x] proto_* — 协议族 (block/net/input 见 [`proto`], MIG-004 已迁移)
//! - [x] user_driver — 用户态驱动 (见 [`user_driver`], MIG-004 已迁移)
//!
//! ## 迁移方法
//!
//! 1. 把 `i32` 错误码 → `Result<_, ChitinError>` 强类型
//! 2. 把设备 ID `u32` → `DeviceId` 新类型
//! 3. 块设备 IO 用 `&mut [u8]`/`&[u8]` 切片替代裸指针
//! 4. 0 unsafe 出现在 services 层

use alloc::vec::Vec;

use crate::framework::chitin;

pub mod composite;
pub mod devtree;
pub mod proto;
pub mod user_driver;
pub use composite::{probe as composite_probe, probe_init as composite_probe_init};
pub use devtree::{
    ChitinNode, DevTreeError, DevTreeNodeId, DevTreeResult, NodeId, Property, PropertyValue,
    add_prop, bind_device, children, clear_user_mapped, clear_user_mapped_by_pid, create_node,
    find_compatible, get_node, get_user_mapped, init as devtree_init, print_tree, properties,
    read_addr, read_irq, root_id, set_compatible, set_user_mapped, walk,
};
pub use proto::{
    BlockDevice, NetDevice, find_net_device, input_has_data, input_read, register_block_device,
    unregister_block,
};
pub use user_driver::{UserDriverError, UserDriverResult};

// ============================================================================
// 错误
// ============================================================================

/// Chitin 操作错误 — TD-20: 收敛到 `KernelError`, 1 字段 chitin 特有 + 1 共享包装.
///
/// 字段说明:
///   - `WrongType`: 设备类型不匹配 (按类型查询时设备类型不符)
///   - `Kernel(KernelError)`: 共享错误 (`NotFound` / `AlreadyExists` / Io→Fault /
///     `InvalidArgument` / NoResources→WouldBlock / `NotReady` / `PermissionDenied` /
///     Other) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChitinError {
    /// 设备类型不匹配
    WrongType,
    /// 共享 `KernelError` 包装
    Kernel(crate::services::error::KernelError),
}

impl ChitinError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::WrongType => E::ENOTTY,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    pub fn from_i32(rc: i32) -> Self {
        use crate::services::error::KernelError as K;
        match rc {
            -2 => Self::Kernel(K::FileNotFound),
            -17 => Self::Kernel(K::AlreadyExists),
            -5 => Self::Kernel(K::Fault),
            -22 => Self::Kernel(K::InvalidArgument),
            -28 => Self::Kernel(K::WouldBlock),
            -19 => Self::Kernel(K::NoDevice),
            -1 => Self::Kernel(K::Other(rc)),
            -25 => Self::WrongType,
            -13 => Self::Kernel(K::PermissionDenied),
            _ => Self::Kernel(K::Other(rc)),
        }
    }
}

/// services 层结果类型别名
pub type ChitinResult<T> = Result<T, ChitinError>;

use crate::framework::syscall::Errno;

// ============================================================================
// 设备 ID
// ============================================================================

/// Chitin 设备 ID (强类型, 替代裸 `u32`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct DeviceId(pub u32);

impl DeviceId {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 原始 ID
    pub fn raw(&self) -> u32 {
        self.0
    }
}

// ============================================================================
// 协议类型
// ============================================================================

/// 设备协议 (与 `kernel::chitin::ChitinProto` 对齐)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Proto {
    Block = 1,
    Char = 2,
    Net = 3,
    Input = 4,
    Bus = 5,
    Other = 255,
}

impl Proto {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Char => "char",
            Self::Net => "net",
            Self::Input => "input",
            Self::Bus => "bus",
            Self::Other => "other",
        }
    }
}

impl From<chitin::ChitinProto> for Proto {
    fn from(p: chitin::ChitinProto) -> Self {
        match p {
            chitin::ChitinProto::Block => Self::Block,
            chitin::ChitinProto::Char => Self::Char,
            chitin::ChitinProto::Net => Self::Net,
            chitin::ChitinProto::Input => Self::Input,
            chitin::ChitinProto::Bus => Self::Bus,
            chitin::ChitinProto::Other => Self::Other,
        }
    }
}

/// 设备状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Uninit,
    Probing,
    Ready,
    Failed,
    Removed,
}

impl From<chitin::DeviceState> for DeviceState {
    fn from(s: chitin::DeviceState) -> Self {
        match s {
            chitin::DeviceState::Uninit => Self::Uninit,
            chitin::DeviceState::Probing => Self::Probing,
            chitin::DeviceState::Ready => Self::Ready,
            chitin::DeviceState::Failed => Self::Failed,
            chitin::DeviceState::Removed => Self::Removed,
        }
    }
}

/// 设备描述符
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: alloc::string::String,
    pub proto: Proto,
    pub state: DeviceState,
}

// ============================================================================
// 注册表 API
// ============================================================================

/// 注册设备 (通用)
///
/// # 注意
/// 由于内核 `chitin_register` 需要 `&'static str`, services 层无法直接传 `&str`。
/// 调用方应在启动期 (`&'static` 上下文) 直接调用 `chitin::chitin_register`,
/// 或使用本函数 (内部泄漏 `Box<str>` 转 `&'static str`, 仅用于一次性注册)。
///
/// # Errors
/// 当设备表已满 (`chitin_register` 返回 `u32::MAX`) 时, 返回
/// `Err(ChitinError::Kernel(KernelError::WouldBlock))`.
pub fn register(name: &str, proto: Proto, driver_data: *mut u8) -> ChitinResult<DeviceId> {
    let chitin_proto = match proto {
        Proto::Block => chitin::ChitinProto::Block,
        Proto::Char => chitin::ChitinProto::Char,
        Proto::Net => chitin::ChitinProto::Net,
        Proto::Input => chitin::ChitinProto::Input,
        Proto::Bus => chitin::ChitinProto::Bus,
        Proto::Other => chitin::ChitinProto::Other,
    };
    // SAFETY leak: 仅启动期一次性使用, 名称永久驻留
    let leaked: &'static str = alloc::string::String::from(name).leak();
    let id = chitin::chitin_register(leaked, chitin_proto, None, None, driver_data);
    if id == u32::MAX {
        Err(ChitinError::Kernel(
            crate::services::error::KernelError::WouldBlock,
        ))
    } else {
        Ok(DeviceId(id))
    }
}

/// 按名称查找
pub fn find_by_name(name: &str) -> Option<usize> {
    chitin::chitin_find_by_name(name)
}

/// 按协议查找第一个
pub fn find_by_proto(proto: Proto) -> Option<usize> {
    let p = match proto {
        Proto::Block => chitin::ChitinProto::Block,
        Proto::Char => chitin::ChitinProto::Char,
        Proto::Net => chitin::ChitinProto::Net,
        Proto::Input => chitin::ChitinProto::Input,
        Proto::Bus => chitin::ChitinProto::Bus,
        Proto::Other => chitin::ChitinProto::Other,
    };
    chitin::chitin_find_by_proto(p)
}

/// 列出所有设备
pub fn list() -> Vec<DeviceInfo> {
    let raw = chitin::chitin_list();
    raw.into_iter()
        .map(|(id, name, proto, state)| DeviceInfo {
            id: DeviceId(id),
            name: alloc::string::String::from(name),
            proto: Proto::from(proto),
            state: DeviceState::from(state),
        })
        .collect()
}

/// 设备总数
pub fn count() -> usize {
    chitin::chitin_count()
}

/// 注销设备
///
/// 返回 `true` 表示注销成功 (返回了原驱动数据指针), `false` 表示设备不存在。
pub fn unregister(id: DeviceId) -> bool {
    chitin::chitin_unregister(id.0).is_some()
}

/// 设置设备状态
pub fn set_state(id: DeviceId, state: DeviceState) {
    let s = match state {
        DeviceState::Uninit => chitin::DeviceState::Uninit,
        DeviceState::Probing => chitin::DeviceState::Probing,
        DeviceState::Ready => chitin::DeviceState::Ready,
        DeviceState::Failed => chitin::DeviceState::Failed,
        DeviceState::Removed => chitin::DeviceState::Removed,
    };
    chitin::chitin_set_state(id.0, s);
}

/// 初始化所有设备
pub fn init_all() {
    chitin::chitin_init_all();
}

/// 关闭所有设备
pub fn shutdown_all() {
    chitin::chitin_shutdown_all();
}

// ============================================================================
// 块设备 IO (按 drive 索引)
// ============================================================================

/// 读块设备
///
/// # 参数
/// - `drive`: 块设备索引
/// - `sector`: 起始扇区
/// - `buf`: 接收缓冲区 (至少 512 字节)
///
/// # Errors
/// 当底层 `chitin_blk_read` 返回非零错误码时, 返回由该错误码转换得到的
/// `Err(ChitinError)`.
pub fn blk_read(drive: u8, sector: u64, buf: &mut [u8]) -> ChitinResult<()> {
    let rc = chitin::chitin_blk_read(drive, sector, buf);
    if rc == 0 {
        Ok(())
    } else {
        Err(ChitinError::from_i32(rc))
    }
}

/// 写块设备
///
/// # Errors
/// 当底层 `chitin_blk_write` 返回非零错误码时, 返回由该错误码转换得到的
/// `Err(ChitinError)`.
pub fn blk_write(drive: u8, sector: u64, buf: &[u8]) -> ChitinResult<()> {
    let rc = chitin::chitin_blk_write(drive, sector, buf);
    if rc == 0 {
        Ok(())
    } else {
        Err(ChitinError::from_i32(rc))
    }
}

/// 块设备是否存在
pub fn blk_is_present(drive: u8) -> bool {
    chitin::chitin_blk_is_present(drive)
}

/// 块设备总扇区数
pub fn blk_total_sectors(drive: u8) -> u64 {
    chitin::chitin_blk_total_sectors(drive)
}

/// 块设备总数
pub fn blk_count() -> usize {
    chitin::chitin_blk_count()
}

// ============================================================================
// 字符设备 IO
// ============================================================================

/// 字符设备写
pub fn char_write(data: &[u8]) {
    chitin::chitin_char_write(data);
}

/// 字符设备读
pub fn char_read(buf: &mut [u8]) -> usize {
    chitin::chitin_char_read(buf)
}
