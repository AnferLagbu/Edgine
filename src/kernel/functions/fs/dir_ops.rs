#![deny(unsafe_code)]
//! 目录与定位操作策略 — lseek / getdents
//!
//! 从 privileged/syscall/mod.rs 迁移的策略代码:
//! - lseek_syscall: 文件定位
//! - getdents_syscall: 目录项读取
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 通过 privileged::fs 公开 API 访问
//! - 无 unsafe, 无裸指针

/// lseek(fd, offset, whence) 策略
pub fn lseek_syscall(fd: i32, offset: i64, whence: i32) -> i64 {
    i64::from(crate::functions::fs::vfs_seek(
        fd as u32,
        offset as i32,
        whence as u32,
    ))
}

/// getdents(fd, buf, count) 策略
pub fn getdents_syscall(fd: i32, buf_ptr: u64, _count: u64) -> i64 {
    i64::from(crate::functions::fs::vfs_readdir(
        fd as u32,
        buf_ptr as *mut crate::functions::fs::VfsDirEntry,
    ))
}
