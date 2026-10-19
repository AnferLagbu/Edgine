//! Edgine 内核态/用户态共享 ABI 常量.
//!
//! 单一权威值域源: 内核（`privileged` FFI 分派）与用户态（userlib）两侧的
//! syscall ABI 常量均引用本 crate, 结构上消除"两处各写各的、改一处忘另一处"
//! 的漂移（见 `docs/plan/socket-option-constants-unification.md` 裁定记录）.
//!
//! 值域基准: Linux x86_64 `asm-generic` 头文件（`<linux/socket.h>` /
//! `<netinet/in.h>` / `<netinet/tcp.h>`）. 按本项目主目标架构 x86_64 取值;
//! aarch64 与 x86_64 同用 asm-generic 值域, 故双架构一致.
//!
//! 本 crate 是纯常量定义, 无 `unsafe`、无 `alloc`、无依赖; 仅承载跨层 ABI
//! 数值, 不承载任何策略或机制逻辑.

#![no_std]
#![deny(missing_docs)]

pub mod socket;
