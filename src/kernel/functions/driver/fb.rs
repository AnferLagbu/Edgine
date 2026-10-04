#![deny(unsafe_code)]
//! 帧缓冲 syscall — functions 层安全代理 (0 unsafe)
//!
//! ## 分层 (T2 批 5, syscall-followup)
//!
//! 机制留在 privileged (`privileged/syscall/dispatch.rs`):
//!   - `sys_fb_open` — FB 驱动信息读取 + 用户结构写入 (unsafe)
//!   - `sys_fb_mmap` — 页表映射建立 + 映射记录 (unsafe)
//!   - `sys_fb_release` — 页表映射解除 (unsafe, 自空 stub 实装)
//!
//! 本文件实现 syscall 策略入口: 参数透传 + 委托机制函数.
//! 指针/页表有效性校验全部由机制层负责.

/// fb_open(info_ptr, flags) 策略 — 查询帧缓冲信息
///
/// T2 批 5 自 privileged 回退层迁移. 委托 privileged 机制 `sys_fb_open`
/// (FB 驱动读取 + 用户 `FbInfo` 结构写入, 指针校验在机制层).
pub fn fb_open_syscall(info_ptr: u64, flags: u64) -> i64 {
    crate::privileged::syscall::sys_fb_open(info_ptr, flags)
}

/// fb_mmap(target_vaddr, size, prot) 策略 — 建立帧缓冲页映射
///
/// T2 批 5 自 privileged 回退层迁移. 委托 privileged 机制 `sys_fb_mmap`
/// (user_entry_cr3 页表映射, 记录区间供 release 解除).
pub fn fb_mmap_syscall(target_vaddr: u64, size: u64, prot: u64) -> i64 {
    crate::privileged::syscall::sys_fb_mmap(target_vaddr, size, prot)
}

/// fb_release(vaddr) 策略 — 解除帧缓冲页映射
///
/// T2 批 5 自 privileged 回退层迁移. 委托 privileged 机制 `sys_fb_release`
/// (依映射记录 unmap 页表并清记录, T2 实装空 stub).
pub fn fb_release_syscall(vaddr: u64) -> i64 {
    crate::privileged::syscall::sys_fb_release(vaddr)
}
