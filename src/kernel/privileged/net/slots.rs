//! Socket 槽位分配器 — smoltcp 内部资源池 (fd 对象模型阶段 3)
//!
//! ## 定位
//!
//! smoltcp socket 的**内部槽位**不再是用户态可见的 fd 编号: 用户 fd 由
//! per-process `FdTable` 分配, 指向挂在 `OpenFileTable` 上的 `SocketFile`
//! 对象; 对象私有状态只保存本模块分配的槽位号. 槽位号不跨进程、不暴露给
//! 用户态, 仅作为 `NET_STATE` 内各 per-slot 表的索引.
//!
//! 本模块是槽位空间的唯一分配者, 只依赖 `core` 原子与本模块自有容量常量,
//! 不引用 `net::init` — 供 `init` / `wait_queue` / `save` 共用而不形成模块环.
//!
//! ## 容量单源
//!
//! `MAX_SLOTS` 是 socket 槽位容量的唯一真源 (fd 对象模型阶段 4 起不再经
//! 全局 FD 编号规划派生); `init.rs` 的 `MAX_SM_FD`、`wait_queue.rs` 与
//! `save.rs` 均引用本常量.
//!
//! ## 并发
//!
//! 分配/释放全部发生在 `NET_STATE` 临界区内 (`sm_socket` / `sm_accept` /
//! `sm_close` 均持锁), 原子位图主要用于 `free_all` 复位与 host 测试的
//! 无锁可读性; 位图本身不提供跨锁事务语义.

#![deny(missing_docs)]

use core::sync::atomic::{AtomicU64, Ordering};

/// Socket 槽位上限 — 容量唯一真源.
///
/// 与 `net/init/sockets.rs` 的 `MAX_SOCKETS` 恒等 (256); 调整本值须同步
/// `MAX_SOCKETS` 与 `SOCKET_STORAGE` / TCP/UDP 缓冲静态表尺寸.
pub const MAX_SLOTS: usize = 256;

/// 位图字数 (`MAX_SLOTS <= WORDS * 64`).
const WORDS: usize = MAX_SLOTS.div_ceil(64);

/// 槽位占用位图 (1 = 已分配).
static SLOT_BITMAP: [AtomicU64; WORDS] = [const { AtomicU64::new(0) }; WORDS];

/// 分配一个空闲槽位; 槽位耗尽返回 `None`.
///
/// 返回值为 `[0, MAX_SLOTS)` 内的槽位索引. 调用方须在释放前持续持有该
/// 槽位对应的 per-slot 资源 (`raw::fd_type != 0` 表示已就绪).
pub fn alloc() -> Option<usize> {
    for (wi, word) in SLOT_BITMAP.iter().enumerate() {
        for bit in 0..64 {
            let idx = wi * 64 + bit;
            if idx >= MAX_SLOTS {
                return None;
            }
            let mask = 1u64 << bit;
            loop {
                let cur = word.load(Ordering::Acquire);
                if cur & mask != 0 {
                    break;
                }
                if word
                    .compare_exchange(cur, cur | mask, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return Some(idx);
                }
            }
        }
    }
    None
}

/// 释放槽位; 越界或未分配的槽位为 no-op (close 幂等).
pub fn free(slot: usize) {
    if slot >= MAX_SLOTS {
        return;
    }
    SLOT_BITMAP[slot / 64].fetch_and(!(1u64 << (slot % 64)), Ordering::Release);
}

/// 查询槽位是否已分配; 越界返回 `false`.
#[must_use]
pub fn is_allocated(slot: usize) -> bool {
    if slot >= MAX_SLOTS {
        return false;
    }
    SLOT_BITMAP[slot / 64].load(Ordering::Acquire) & (1u64 << (slot % 64)) != 0
}

/// 清空全部槽位登记 — 网络 (重) 初始化路径与 per-slot 表复位同步调用.
pub fn free_all() {
    for word in &SLOT_BITMAP {
        word.store(0, Ordering::Release);
    }
}
