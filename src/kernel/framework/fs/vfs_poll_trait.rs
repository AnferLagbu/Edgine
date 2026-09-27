//! `VfsPollable` trait 抽象 — REVAL-6.1
//!
//! 解耦 epoll 机制 (framework/syscall/epoll.rs) 与 VFS 文件类型 → 事件位
//! 的策略 (`services/fs/vfs_poll_policy.rs`).
//!
//! ## 架构
//!
//! ```text
//! epoll_wait → check_fd_ready (机制) → VfsPollPolicy::events_for_file_type (策略)
//!                                            ↓
//!                                     StandardVfsPollPolicy (services/fs)
//! ```
//!
//! ## TCB 减负
//!
//! 原 `epoll.rs::check_fd_ready` 第 432-450 行硬编码 match `VfsFileType` → events.
//! 提取后:
//! - framework 仅持有 trait 接口 (机制)
//! - services 持有具体策略 (策略)
//! - 添加新事件策略 (如 poll 设备节点) 只需新增 impl, 无需改 framework
//!
//! ## 与 LEGACY-4 (`BlockDevice`) 范式一致
//!
//! - 0 unsafe (策略层)
//! - 0 thunk (trait dispatch 走 vtable)
//! - 编译期类型安全 (驱动方必须 impl `VfsPollPolicy`)

use super::VfsFileType;
use crate::framework::sync::IrqSpinLock as Mutex;

// ============================================================================
// epoll 事件位常量 (从 epoll.rs 提取)
// ============================================================================
//
// epoll 与 VFS poll 共享事件位 (Linux 兼容).
// 集中定义便于 services 策略层复用, 避免与 epoll.rs 重复.

/// EPOLLIN: 可读事件
pub const EPOLLIN: u32 = 0x001;
/// EPOLLOUT: 可写事件
pub const EPOLLOUT: u32 = 0x004;
/// EPOLLERR: 错误事件
pub const EPOLLERR: u32 = 0x008;
/// EPOLLHUP: 挂断事件
pub const EPOLLHUP: u32 = 0x010;

// ============================================================================
// VfsPollable 上下文
// ============================================================================

/// `VfsPollable` 决策上下文 — 传递给策略
///
/// 不持有 fd 引用, 仅描述当前 fd 的 VFS 状态.
#[derive(Debug, Clone, Copy)]
pub struct VfsPollContext {
    /// fd 是否有效 (当前进程 per-process fd 表中有映射)
    pub valid: bool,
    /// VFS 文件类型 (File/Dir/Dev/Symlink)
    pub file_type: VfsFileType,
}

// ============================================================================
// VfsPollable trait — framework 调用, services 实现
// ============================================================================

/// VFS 轮询策略 — 决定给定 VFS 文件类型应报告哪些 epoll 事件位
///
/// 纯函数, 0 unsafe, 0 锁. 所有方法仅依赖输入参数.
///
/// # 实现示例
///
/// ```ignore
/// pub struct StandardVfsPollPolicy;
/// impl VfsPollPolicy for StandardVfsPollPolicy {
///     fn events_for_file_type(&self, ft: VfsFileType) -> u32 {
///         match ft {
///             VfsFileType::File => EPOLLIN | EPOLLOUT,
///             VfsFileType::Dir => EPOLLIN,
///             VfsFileType::Dev => EPOLLHUP,
///             VfsFileType::Symlink => EPOLLIN | EPOLLHUP,
///         }
///     }
///     fn events_for_invalid_fd(&self) -> u32 { EPOLLERR | EPOLLHUP }
/// }
/// ```
pub trait VfsPollPolicy: Send + Sync {
    /// 决定给定 VFS 文件类型应报告的 epoll 事件位
    ///
    /// framework/syscall/epoll.rs 在 `check_fd_ready` 中调用.
    /// 策略可自由实现 (例如: Dev 类型可向驱动层查询真实就绪状态).
    fn events_for_file_type(&self, file_type: VfsFileType) -> u32;

    /// 决定无效 fd 应报告的事件 (`fd_table` 越界或未 used)
    fn events_for_invalid_fd(&self) -> u32;
}

// ============================================================================
// 全局策略注册
// ============================================================================

/// 当前注册的 `VfsPollPolicy` (静态 `OnceLock` 风格)
static CURRENT_POLICY: Mutex<Option<&'static dyn VfsPollPolicy>> = Mutex::new(None);

/// 注册 `VfsPollPolicy` (只允许注册一次, 后续注册返回 false)
pub fn register_vfs_poll_policy(policy: &'static dyn VfsPollPolicy) -> bool {
    let mut slot = CURRENT_POLICY.lock();
    if slot.is_some() {
        return false;
    }
    *slot = Some(policy);
    true
}

/// 获取当前注册的 `VfsPollPolicy`
///
/// 若未注册, 返回 fallback (硬编码 match, 与原 epoll.rs 行为一致).
/// 这保证 framework 在未注册策略时仍可工作 (向后兼容).
pub fn current_vfs_poll_policy() -> VfsPollPolicyRef<'static> {
    let slot = CURRENT_POLICY.lock();
    slot.map_or(VfsPollPolicyRef::Fallback, VfsPollPolicyRef::Registered)
}

/// `VfsPollPolicy` 引用 — 可能是注册的或 fallback
pub enum VfsPollPolicyRef<'a> {
    Registered(&'a dyn VfsPollPolicy),
    Fallback,
}

impl VfsPollPolicyRef<'_> {
    /// 决策事件位
    pub fn events_for(&self, ctx: VfsPollContext) -> u32 {
        if !ctx.valid {
            return match self {
                VfsPollPolicyRef::Registered(p) => p.events_for_invalid_fd(),
                VfsPollPolicyRef::Fallback => EPOLLERR | EPOLLHUP,
            };
        }
        match self {
            VfsPollPolicyRef::Registered(p) => p.events_for_file_type(ctx.file_type),
            VfsPollPolicyRef::Fallback => fallback_events(ctx.file_type),
        }
    }
}

/// Fallback 策略 (未注册时使用) — 与原 `epoll.rs::check_fd_ready` 行为一致
fn fallback_events(file_type: VfsFileType) -> u32 {
    match file_type {
        VfsFileType::File => EPOLLIN | EPOLLOUT,
        VfsFileType::Dir => EPOLLIN,
        VfsFileType::Dev => EPOLLHUP,
        VfsFileType::Symlink => EPOLLIN | EPOLLHUP,
    }
}
