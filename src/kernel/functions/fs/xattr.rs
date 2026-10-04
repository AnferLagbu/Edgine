#![deny(unsafe_code)]
//! 扩展属性 (xattr) 系统调用处理器
//!
//! 提供 setxattr/getxattr/listxattr/removexattr 系统调用的实现。
//! 调用 privileged 层的 vfs_*_internal 函数处理指针转换。

use crate::functions::syscall::types::Errno;

/// 设置扩展属性
///
/// # Errors
/// 当 `path_ptr` 或 `name_ptr` 为空时返回 `EFAULT`; 其余错误 (如属性名过长、无权限等) 以对应的 `Errno` 返回.
pub fn setxattr_syscall(
    path_ptr: u64,
    name_ptr: u64,
    value_ptr: u64,
    value_len: usize,
    pwm: u64,
) -> Result<usize, Errno> {
    if path_ptr == 0 || name_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    let r = crate::functions::fs::api::vfs_setxattr_internal(
        path_ptr as *const u8,
        name_ptr as *const u8,
        value_ptr as *const u8,
        value_len as u32,
        pwm,
    );

    if r >= 0 {
        Ok(r as usize)
    } else {
        Err(Errno::from_ret(i64::from(r)))
    }
}

/// 获取扩展属性
///
/// # Errors
/// 当 `path_ptr`/`name_ptr`/`buf_ptr` 为空时返回 `EFAULT`; 其余错误 (如属性不存在等) 以对应的 `Errno` 返回.
pub fn getxattr_syscall(
    path_ptr: u64,
    name_ptr: u64,
    buf_ptr: u64,
    buf_len: usize,
    pwm: u64,
) -> Result<usize, Errno> {
    if path_ptr == 0 || name_ptr == 0 || buf_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    let r = crate::functions::fs::api::vfs_getxattr_internal(
        path_ptr as *const u8,
        name_ptr as *const u8,
        buf_ptr as *mut u8,
        buf_len as u32,
        pwm,
    );

    if r >= 0 {
        Ok(r as usize)
    } else {
        Err(Errno::from_ret(i64::from(r)))
    }
}

/// 列出扩展属性
///
/// # Errors
/// 当 `path_ptr` 或 `buf_ptr` 为空时返回 `EFAULT`; 其余错误以对应的 `Errno` 返回.
pub fn listxattr_syscall(
    path_ptr: u64,
    buf_ptr: u64,
    buf_len: usize,
    pwm: u64,
) -> Result<usize, Errno> {
    if path_ptr == 0 || buf_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    let r = crate::functions::fs::api::vfs_listxattr_internal(
        path_ptr as *const u8,
        buf_ptr as *mut u8,
        buf_len as u32,
        pwm,
    );

    if r >= 0 {
        Ok(r as usize)
    } else {
        Err(Errno::from_ret(i64::from(r)))
    }
}

/// 删除扩展属性
///
/// # Errors
/// 当 `path_ptr` 或 `name_ptr` 为空时返回 `EFAULT`; 其余错误 (如属性不存在等) 以对应的 `Errno` 返回.
pub fn removexattr_syscall(path_ptr: u64, name_ptr: u64, pwm: u64) -> Result<usize, Errno> {
    if path_ptr == 0 || name_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    let r = crate::functions::fs::api::vfs_removexattr_internal(
        path_ptr as *const u8,
        name_ptr as *const u8,
        pwm,
    );

    if r >= 0 {
        Ok(r as usize)
    } else {
        Err(Errno::from_ret(i64::from(r)))
    }
}
