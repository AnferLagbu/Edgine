//! `arch_prctl` 系统调用常量（用户态 TLS 基址读写）.
//!
//! 内核 `privileged::syscall` 的 FFI 分派与用户态 `userlib::sys` 的 wrapper
//! 均引用本模块，两侧数值由结构保证一致.
//!
//! 值域基准: Linux x86_64 `<asm/prctl.h>`。aarch64 侧复用同一 syscall 号与
//! 子码承载 `TPIDR_EL0`（Edgine 双架构统一接口，非 Linux aarch64 语义）.

/// `arch_prctl` 系统调用号（Linux x86_64 标准编号）.
pub const SYS_ARCH_PRCTL: u64 = 158;

/// `ARCH_SET_FS`: 设置用户态 TLS 基址.
pub const ARCH_SET_FS: u64 = 0x1002;

/// `ARCH_GET_FS`: 查询用户态 TLS 基址.
pub const ARCH_GET_FS: u64 = 0x1003;
