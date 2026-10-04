#![deny(unsafe_code)]
//! 共享内存策略 — T6-1 从 privileged/ipc/shm.rs 提取
//!
//! 纯策略逻辑: 参数校验、槽位查找、附加/分离管理、引用计数.
//! 物理页分配/释放通过 `privileged::mm` 机制 API 完成.

use crate::privileged::ipc::types::{IpcId, IpcNamespace, SHM_MAX_SIZE, ShmSegment};
use crate::privileged::mm::PAGE_SIZE;

/// 查找空闲共享内存段槽位
pub fn shm_find_free(namespace: &mut IpcNamespace) -> Option<&mut ShmSegment> {
    namespace.shm_segs.iter_mut().find(|s| s.id == 0)
}

/// 根据 ID 查找共享内存段
pub fn shm_find_by_id(namespace: &mut IpcNamespace, id: IpcId) -> Option<&mut ShmSegment> {
    namespace.shm_segs.iter_mut().find(|s| s.id == id)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 创建共享内存段 (策略: 参数校验 + 槽位分配 + 物理页分配 + 初始化)
///
/// # Errors
/// 当 `size` 为 0 或超过 `SHM_MAX_SIZE` 时返回 `Err(-1)`;
/// 当共享内存段表已满时返回 `Err(-2)`; 当物理页分配失败时返回 `Err(-3)`.
pub fn shm_create_safe(
    namespace: &mut IpcNamespace,
    next_id: &mut IpcId,
    size: u64,
    perm: i32,
    current_pid: u32,
) -> Result<IpcId, i32> {
    // 参数校验
    if size == 0 || size > SHM_MAX_SIZE {
        return Err(-1);
    }

    // 查找空闲槽位
    let shm = match shm_find_free(namespace) {
        Some(s) => s,
        None => return Err(-2),
    };

    // 计算需要的页数并分配物理内存 (委托 privileged 机制)
    let pages = size.div_ceil(PAGE_SIZE);
    let phys = crate::privileged::mm::pmm_alloc_pages(pages as usize);
    if phys.is_null() {
        return Err(-3);
    }

    // 初始化共享内存段
    shm.id = *next_id;
    *next_id += 1;

    shm.phys_addr = phys as u64;
    shm.size = size;
    shm.creator = current_pid;
    shm.ref_count = 0;
    shm.attach_count = 0;
    shm.flags = 0;
    shm.perm = perm;
    shm.attached_pids = [0u32; 16];

    Ok(shm.id)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 将共享内存段附加到当前进程地址空间 (策略: 重复附加检查 + 限制检查 + 引用计数)
///
/// # Errors
/// 当共享内存段不存在时返回 `Err(-1)`; 当附加进程数已达上限 (16) 时返回 `Err(-2)`.
pub fn shm_attach_safe(
    namespace: &mut IpcNamespace,
    id: IpcId,
    current_pid: u32,
) -> Result<u64, i32> {
    let shm = match shm_find_by_id(namespace, id) {
        Some(s) => s,
        None => return Err(-1),
    };

    // 检查是否已经附加过
    for i in 0..shm.attach_count as usize {
        if shm.attached_pids[i] == current_pid {
            return Ok(shm.phys_addr);
        }
    }

    // 检查是否超过最大附加进程数
    if shm.attach_count >= 16 {
        return Err(-2);
    }

    // 记录附加信息
    shm.attached_pids[shm.attach_count as usize] = current_pid;
    shm.attach_count += 1;
    shm.ref_count += 1;

    Ok(shm.phys_addr)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 从当前进程分离共享内存段 (策略: 附加记录查找 + 引用计数递减)
///
/// # Errors
/// 当共享内存段不存在时返回 `Err(-1)`; 当当前进程未附加该段时返回 `Err(-2)`.
pub fn shm_detach_safe(
    namespace: &mut IpcNamespace,
    id: IpcId,
    current_pid: u32,
) -> Result<(), i32> {
    let shm = match shm_find_by_id(namespace, id) {
        Some(s) => s,
        None => return Err(-1),
    };

    // 查找并移除当前进程的附加记录
    for i in 0..shm.attach_count as usize {
        if shm.attached_pids[i] == current_pid {
            shm.attached_pids[i] = shm.attached_pids[shm.attach_count as usize - 1];
            shm.attached_pids[shm.attach_count as usize - 1] = 0;
            shm.attach_count -= 1;
            shm.ref_count -= 1;
            return Ok(());
        }
    }

    Err(-2)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 销毁共享内存段 (策略: 引用计数检查 + 物理页释放 + 结构体清理)
///
/// # Errors
/// 当共享内存段不存在时返回 `Err(-1)`; 当仍有进程附加时返回 `Err(-2)`.
pub fn shm_destroy_safe(namespace: &mut IpcNamespace, id: IpcId) -> Result<(), i32> {
    let shm = match shm_find_by_id(namespace, id) {
        Some(s) => s,
        None => return Err(-1),
    };

    // 检查是否有进程仍附加
    if shm.ref_count > 0 {
        return Err(-2);
    }

    // 释放物理页 (委托 privileged 机制)
    let pages = shm.size.div_ceil(PAGE_SIZE);
    crate::privileged::mm::pmm_free_pages(shm.phys_addr as *mut u8, pages as usize);

    // 清理结构体
    shm.id = 0;
    shm.phys_addr = 0;
    shm.size = 0;

    Ok(())
}
