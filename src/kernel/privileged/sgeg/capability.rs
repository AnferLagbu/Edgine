//! PWM v5 能力定义 — privileged 层权威定义
//!
//! ## 归属记录
//!
//! 纯常量定义 (16 域能力位 + viable floor), 0 unsafe, 0 外部依赖.
//! T6-8 曾于 2026-06-16 提取到 functions; 第二十五批 (DECISION-K 项 5 sgeg
//! 判据: 持有类型/安全原语归 privileged) 反转归位本文件.
//! functions/sgeg/capability.rs 为 re-export 壳保持路径兼容.

pub const SYS_CAP_ALL: u64 = 0xFFFFFFFFFFFFFFFF;

pub const CAP_DOMAIN_SYSTEM: u16 = 0;
pub const CAP_DOMAIN_FS: u16 = 1;
pub const CAP_DOMAIN_NET: u16 = 2;
pub const CAP_DOMAIN_PROC: u16 = 3;
pub const CAP_DOMAIN_DEVICE: u16 = 4;
pub const CAP_DOMAIN_USER_MGMT: u16 = 5;
pub const CAP_DOMAIN_IPC: u16 = 6;
pub const CAP_DOMAIN_MEM: u16 = 7;
pub const CAP_DOMAIN_TIME: u16 = 8;
pub const CAP_DOMAIN_FREG: u16 = 9;
pub const CAP_DOMAIN_SIGNAL: u16 = 10;
pub const CAP_DOMAIN_SHM: u16 = 11;
pub const CAP_DOMAIN_SEM: u16 = 12;
pub const CAP_DOMAIN_MSGQ: u16 = 13;
pub const CAP_DOMAIN_DMA: u16 = 14;
pub const CAP_DOMAIN_RESERVED: u16 = 15;

/// SYSTEM 域 bit1 — 设置进程 PWM 身份 (B07-05, DECISION-078 新增专用位)
///
/// `pwm_set` 会改变进程身份, 无既有能力位可精确表达, 故新增专用位而非复用
/// SYSTEM 域 bit0 (mount/setns 等复用的 CAP_SYS_ADMIN 语义). 任意进程不可
/// 自行设置自身 PWM (防任意提权).
pub const SYSTEM_CAP_SET_PWM: u64 = 1 << 1;

/// SYSTEM 域 bit2 — 设置本进程域级行为门控标志 (`domain_flags_set`)
///
/// 分册 9 批次 4: 域标志可放宽既有 `SANDBOX`/`READONLY` 约束 (清位即解除限制),
/// 属身份安全关键操作, 仿 `SYSTEM_CAP_SET_PWM` (B07-05) 新增专用位。
/// 无此位 (且非 bootstrap PWM 0) 的身份不可修改自身标志, 防止被监管进程自行
/// 解除域限制。
pub const SYSTEM_CAP_SET_DOMAIN_FLAGS: u64 = 1 << 2;

/// SYSTEM 域 bit9 — 设置 UTS 主机名/域名 (`sethostname` / `setdomainname`)
///
/// T1 G7: 补命名常量以消除 `sethostname_syscall` 中的字面量 `9`, 并供同级
/// 的 `setdomainname_syscall` 复用 (语义与既有 sethostname 判定完全一致).
pub const SYSTEM_CAP_UTS_SETNAME: u64 = 1 << 9;

pub const FS_CAP_READ: u64 = 1 << 0;
pub const FS_CAP_WRITE: u64 = 1 << 1;
pub const FS_CAP_EXECUTE: u64 = 1 << 2;
pub const FS_CAP_CREATE: u64 = 1 << 3;
pub const FS_CAP_DELETE: u64 = 1 << 4;
pub const FS_CAP_CHOWN: u64 = 1 << 5;
pub const FS_CAP_CHMOD: u64 = 1 << 6;

pub const NET_CAP_SEND: u64 = 1 << 0;
pub const NET_CAP_RECV: u64 = 1 << 1;
pub const NET_CAP_CONNECT: u64 = 1 << 2;
pub const NET_CAP_LISTEN: u64 = 1 << 3;
pub const NET_CAP_BIND: u64 = 1 << 4;

pub const PROC_CAP_FORK: u64 = 1 << 0;
pub const PROC_CAP_EXEC: u64 = 1 << 1;
pub const PROC_CAP_KILL: u64 = 1 << 2;
pub const PROC_CAP_WAIT: u64 = 1 << 3;
pub const PROC_CAP_CREATE: u64 = 1 << 4;

pub const USER_MGMT_CAP_LIST: u64 = 1 << 0;
pub const USER_MGMT_CAP_CREATE: u64 = 1 << 1;
pub const USER_MGMT_CAP_DELETE: u64 = 1 << 2;
pub const USER_MGMT_CAP_MODIFY: u64 = 1 << 3;

pub const DEVICE_CAP_MMIO: u64 = 1 << 0;
pub const DEVICE_CAP_IRQ: u64 = 1 << 1;
pub const DEVICE_CAP_DMA: u64 = 1 << 2;
pub const DEVICE_CAP_BIND: u64 = 1 << 3;

pub const VIABLE_FLOOR: [u64; 16] = {
    let mut f = [0u64; 16];
    f[CAP_DOMAIN_FS as usize] = FS_CAP_READ | FS_CAP_EXECUTE;
    f[CAP_DOMAIN_PROC as usize] = PROC_CAP_FORK | PROC_CAP_EXEC;
    f[CAP_DOMAIN_DEVICE as usize] = 0;
    f
};
