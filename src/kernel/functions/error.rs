// SPDX-License-Identifier: MPL-2.0
// TD-08: functions 层统一错误类型 `KernelError` (Single Source of Truth).
//
// 验收:
//   - 字段数为子集枚举 (不变量: 跨服务共享字段 = 1 份)
//   - `functions::net::socket::SocketError` 改为 `pub type SocketError = KernelError;` 零包装
//   - `functions::net::unix::UnixSocketError` 仅保留子系统特有字段 (PathNotFound) + `Kernel(KernelError)` 包装
//   - `From<fw::UdsError>` / `From<i32>` / `to_errno` 单一来源

#![deny(unsafe_code)]

// B09-12/DECISION-H13 P0-2: KernelError 定义已迁回 privileged (privileged::error),
// 本处 re-export 保持调用方兼容 (functions→privileged 单向依赖).
pub use crate::privileged::error::KernelError;

/// 策略/Hook 重复注册错误
///
/// functions 层各 `register_*` 注册函数 (pmm/slab/swap/pressure/ipc/config/
/// coredump/signal/syscall) 的唯一失败原因: 对应的 privileged 槽位已被占用.
/// 取代此前的 `Result<(), ()>` 裸单元错误, 使调用方可按类型区分失败来源.
///
/// # 与「本域 ZST」的分工 (双形态口径)
///
/// functions 层的 `Result<_, E>` 存在两种错误载体形态, 判据是**语义是否跨模块复用**:
/// - **跨模块复用的同义失败 ⇒ 共享单例**: 本类型代表「对应 privileged 槽位已被占用」
///   这一个语义, 由本模块单点定义, 各策略注册方共用;
/// - **某驱动域内部的失败 ⇒ 本域 ZST**: `driver/char/serial.rs::BufferFull`、
///   `driver/storage/{ata,nvme,ahci}.rs` 的 `AtaError` / `NvmeError` / `AhciError`、
///   `driver/virtio/{net,blk}.rs` 的 `VirtioNetError` / `VirtioBlkError` —— 失败原因
///   由各自硬件/协议状态派生, 语义不可迁移, 故留在驱动模块内部.
///
/// 双形态并存是**有意分层**, 不是待统一的残留: clippy `result_unit_err` 只约束
/// `Result<(), ()>` 裸单元形, 不对 ZST 载体提出统一要求. 新增错误类型时按上述
/// 判据选择载体形态, 不再引入第三种写法.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyRegistered;
