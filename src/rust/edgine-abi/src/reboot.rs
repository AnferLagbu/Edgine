//! `reboot` 系统调用的命令码常量（重启 / 停机）.
//!
//! 内核 `functions::proc::sysinfo::reboot_syscall` 的权限检查、
//! `privileged::syscall::api::reboot_mechanism` 的机制分派, 与用户态
//! `userlib::sys::reboot` 的调用方 (`eash` 的 `reboot`/`halt`、`install`
//! 完成页) 均引用本模块 —— 两侧语义由单一常量源保证, 不再各写裸数字.
//!
//! 语义约定 (以用户态命令语义为准): **0 = 重启, 1 = 停机**. 历史上内核侧
//! 两者写反 (0 = 停机自旋), 使 `eash reboot` 实际停机、`halt` 实际重启,
//! 由 R3 批修正并把数值上收到本模块.

/// 重启命令码: 重新引导整机 (x86_64 走三重故障复位, aarch64 走 PSCI `SYSTEM_RESET`).
pub const REBOOT_CMD_RESTART: i32 = 0;

/// 停机命令码: 关闭当前 CPU 的调度并自旋 (不重新引导).
pub const REBOOT_CMD_HALT: i32 = 1;
