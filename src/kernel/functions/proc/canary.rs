#![deny(unsafe_code)]
//! 用户态 Stack Canary 服务层安全封装 (P1 #14)
//!
//! functions 层禁止 `unsafe`, 全部通过 privileged 提供的 safe API 包装.

use crate::privileged::proc::canary;

/// 触发 `Edgine` 原生 `getrandom` syscall
///
/// ## 入参
///
/// - `buf`: 用户 buffer 虚拟地址
/// - `len`: 字节数
/// - `flags`: 兼容位, 当前忽略
///
/// ## 返回
///
/// 写入字节数; 失败 (用户指针非法 / 长度 0) 返回 -1.
pub fn getrandom(buf: u64, len: usize, flags: u32) -> i64 {
    let _ = flags; // 兼容位
    let written = canary::get_random_bytes(buf, len);
    if written == 0 && len > 0 {
        -1
    } else {
        written as i64
    }
}

/// 读取当前进程 stack canary, 写入用户 buffer
///
/// ## 入参
///
/// - `buf`: 用户 buffer 虚拟地址 (必须可写, 至少 8 字节)
/// - `len`: buffer 长度
///
/// ## 返回
///
/// 0 (成功) / -1 (失败: 长度不足 / 指针非法)
pub fn get_canary(buf: u64, len: usize) -> i64 {
    canary::write_canary_to_user(buf, len)
}

/// 返回当前进程 8 字节 canary (u64, 不会截断)
///
/// 用户态启动序列参考:
/// ```c
/// uint64_t c = functions_get_canary_u64();
/// ```
pub fn get_canary_u64() -> u64 {
    canary::process_get_current_canary()
}
