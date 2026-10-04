#![deny(unsafe_code)]
//! eBPF 安全代理 — functions 层 (0 unsafe)
//!
//! 封装 `privileged::debug::ebpf` 的安全 API.

// 重导出强类型
pub use crate::privileged::debug::{
    BPF_MAX_INSNS, BPF_MAX_MAPS, BPF_MAX_PROGS, BPF_REG_NUM, BPF_STACK_SIZE, BpfCtx, BpfHelper,
    BpfInsn, BpfInterpreter, BpfMap, BpfMapDef, BpfMapType, BpfProg, BpfProgType, BpfSubsystem,
    BpfVerifier,
};

use crate::privileged::debug::{bpf_init, bpf_is_initialized, bpf_subsystem, sys_bpf};

/// 初始化 eBPF 子系统 + 注册标准验证器 (Safe Policy Injection, T4-3)
///
/// ## 注册契约 (DECISION-K 统一模式: 机制 init 后立即注册策略)
///
/// 本函数是 functions 侧 eBPF 注册契约点: `bpf_init()` (幂等) + `set_verifier`
/// (重复注册覆盖 + 警告日志). 由 lib.rs kernel_init 在 scheduler init 之后
/// 调用. 此前生产路径中 scheduler_init FFI (含 verifier 注册) 无调用者,
/// verifier 从未注册 — 本契约同时修复该预存欠账. 未注册窗口内 prog_load
/// 走 fail-closed (verifier() 返回 None 拒绝加载), 启动早期无用户态进程,
/// 窗口安全.
pub fn init() {
    bpf_init();
    bpf_subsystem().set_verifier(&super::ebpf_verifier::STANDARD_VERIFIER);
}

/// eBPF 是否已初始化
pub fn is_initialized() -> bool {
    bpf_is_initialized()
}

/// 获取全局 BPF 子系统
pub fn subsystem() -> &'static BpfSubsystem {
    bpf_subsystem()
}

/// BPF 系统调用 (安全封装)
pub fn bpf_syscall(cmd: u64, attr: u64, size: u64) -> i64 {
    sys_bpf(cmd, attr, size)
}
