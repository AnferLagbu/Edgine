#![deny(unsafe_code)]
//! brk — functions 层安全代理
//!
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! ## 职责
//!
//! - 参数验证 (addr 范围检查)
//! - 类型安全的结果封装
//! - 委托 privileged 层执行实际 brk 操作
//!
//! ## 注意
//!
//! brk 的核心逻辑 (VMA 扩展/收缩, 页表更新) 在 privileged TCB 中执行.
//! functions 层仅做参数验证和错误码封装.

use crate::privileged::syscall::Errno;

/// 用户空间最大地址 (`x86_64`: `0x7FFF_FFFF_FFFF`)
#[cfg(target_arch = "x86_64")]
const USER_ADDR_MAX: u64 = 0x7FFF_FFFF_FFFF;

#[cfg(target_arch = "aarch64")]
const USER_ADDR_MAX: u64 = 0x0000_FFFF_FFFF_FFFF;

/// brk 系统调用安全代理
///
/// 验证参数后委托 privileged 层执行.
///
/// # Errors
///
/// - `addr` 超过用户空间最大地址 (`USER_ADDR_MAX`) 时返回 [`Errno::ENOMEM`]
/// - privileged 层 `sys_brk` 返回负值 (堆扩展失败或越界) 时返回 [`Errno::ENOMEM`]
pub fn brk_syscall(addr: u64) -> Result<usize, Errno> {
    // addr == 0 是合法查询, 直接委托 (§6.1 下沉 functions/syscall/brk)
    if addr == 0 {
        return Ok(crate::functions::syscall::brk::sys_brk(0) as usize);
    }

    // 参数验证: 地址不能超过用户空间最大值
    if addr > USER_ADDR_MAX {
        return Err(Errno::ENOMEM);
    }

    // 委托 functions 层 (纯策略)
    let ret = crate::functions::syscall::brk::sys_brk(addr);
    if ret < 0 {
        Err(Errno::ENOMEM)
    } else {
        Ok(ret as usize)
    }
}
