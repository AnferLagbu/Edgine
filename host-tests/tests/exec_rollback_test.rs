//! P0-I-31: execve transactional 模型语义测试
//!
//! ## B08-20 迁移 (2026-09-06) — 该镜像因内核 proc 层 host 不可测已标注
//!
//! 原镜像平行实现了 `ProcessTable` / `ProcState` (Active/Loading/Destructed) /
//! `exec_replace_transactional` / `exec_replace_legacy` 模拟 "先加载后销毁"
//! 的 transactional 语义. 经评估, 内核真实实现 **host 不可测**, 本地平行实现
//! 已全部删除, 不保留镜像:
//!
//! - 内核权威实现为 `privileged/proc/proc_ops.rs::proc_exec_replace` (FFI, `no_mangle`).
//!   其 transactional 语义依赖全局状态链: `SCHEDULER.current()` (当前进程) →
//!   `api::user_proc_load_elf` (VFS 文件加载 + `USER_PROC_MANAGER` 新进程构造) →
//!   `raw::switch_page_table` (CR3 页表切换) → `USER_PROC_MANAGER.replace_user_space`
//!   (地址空间转移) → `PROCESS_TABLE.remove_and_free`. 完整路径需要真实调度器 /
//!   VFS / 页表上下文, host 上 `SCHEDULER.current()` 返回 0 (无当前进程) 使函数
//!   在入口即返回 -1, 无法验证 transactional 语义.
//!
//! - 原镜像的 `ProcState` (Active/Loading/Destructed) 是**虚构状态机**, 与内核
//!   `functions::proc::types::ProcessState` (Created/Ready/Running/Blocked/Zombie/
//!   Terminated/Frozen) 不符, 无迁移对象. 内核 `ProcessState` 判别值已在
//!   `zombie_signal_boundary_test` 中经真实枚举验证.
//!
//! 迁移后本文件不再含测试用例, 仅保留文档标注.
