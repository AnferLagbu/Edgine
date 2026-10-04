//! 进程退出清理 — robust futex 遍历与 CLEARTID (机制层, TCB)
//!
//! Linux 退出语义:
//! - `CLONE_CHILD_CLEARTID`: 进程退出时向 `clear_child_tid` 用户地址写 0,
//!   并对该地址执行 futex 唤醒 (父进程 `futex(&tid, FUTEX_WAIT, tid)` 模式)
//! - robust list: 遍历用户态 `struct robust_list_head` 链表, 对本进程仍持有
//!   的 futex 字置 `FUTEX_OWNER_DIED` 位并唤醒等待者, 防止用户态互斥锁
//!   因持有者死亡而永久死锁
//!
//! ## 边界
//!
//! - 本模块属 privileged (TCB): 直接读写用户内存 (`unsafe`)
//! - 调用时机: `process_exit` 切换内核页表之前 (用户地址空间仍有效)
//! - 全路径容错: 任何用户内存访问失败立即中止, 不阻塞退出

use crate::privileged::proc::api;
use core::sync::atomic::Ordering;

/// futex 字持有者 TID 掩码 (低 30 位)
const FUTEX_TID_MASK: u32 = 0x3FFF_FFFF;

/// futex 字持有者已死标志 (bit 30)
const FUTEX_OWNER_DIED: u32 = 0x4000_0000;

/// robust list 遍历上限 (与 Linux `ROBUST_LIST_LIMIT` 一致, 防用户态环形链表死循环)
const ROBUST_LIST_LIMIT: u32 = 2048;

/// `struct robust_list_head` (Linux 用户 ABI, 24 字节)
#[repr(C)]
#[derive(Copy, Clone)]
struct RobustListHead {
    /// 链表首元素指针
    list: u64,
    /// 元素地址到 futex 字地址的偏移 (可为负)
    futex_offset: i64,
    /// pending 操作指针 (持锁中被信号/退出打断时的兜底)
    list_op_pending: u64,
}

/// 进程退出清理入口: CLEARTID 清零 + robust list 遍历
///
/// 必须在进程地址空间销毁前调用 (`process_exit` 切内核页表之前).
pub fn exit_cleanup(pid: u32) {
    // 1. CLONE_CHILD_CLEARTID: 写 0 并唤醒
    let clear_tid =
        api::process_with(pid, |p| p.clear_child_tid.load(Ordering::Acquire)).unwrap_or(0);
    if clear_tid != 0 {
        write_u32_user(clear_tid, 0);
        let _ = crate::privileged::syscall::futex::futex_wake(clear_tid, 1);
        api::process_with(pid, |p| p.clear_child_tid.store(0, Ordering::Release));
    }

    // 2. robust list 遍历 (未登记则跳过)
    let head_addr = api::process_with(pid, |p| p.robust_head.load(Ordering::Acquire)).unwrap_or(0);
    if head_addr != 0 {
        walk_robust_list(pid, head_addr);
    }
}

/// 遍历用户态 robust list, 对本进程持有的 futex 置 owner-died 并唤醒
fn walk_robust_list(pid: u32, head_addr: u64) {
    let mut head = RobustListHead {
        list: 0,
        futex_offset: 0,
        list_op_pending: 0,
    };
    if !crate::privileged::syscall::api::read_struct_from_user(head_addr, &mut head) {
        return;
    }

    let mut count = 0u32;
    let mut entry = head.list;
    while entry != 0 && count < ROBUST_LIST_LIMIT {
        count += 1;
        // 先读下一节点再处理当前 futex 字, 防止链表被并发破坏后失去遍历线索
        let mut next = 0u64;
        if !crate::privileged::syscall::api::read_struct_from_user(entry, &mut next) {
            break;
        }
        let uaddr = entry.wrapping_add(head.futex_offset as u64);
        handle_futex_word(pid, uaddr);
        entry = next;
    }

    // SIMPLIFIED: pending 操作统一按"直接指向 futex 字"处理, 不区分
    // `list_op_pending` 指向链表节点还是 futex 地址两种 pending 语义;
    // 影响面: 持锁进程死亡时 pending 场景可能漏标记节点型 pending;
    // 何时需扩展: 引入完整 robust futex op 编码 (FUTEX_OP_REQUEUED 等) 后细分.
    if head.list_op_pending != 0 {
        handle_futex_word(pid, head.list_op_pending);
    }
}

/// 检查单个 futex 字: 若为本进程持有则置 `FUTEX_OWNER_DIED` 并唤醒等待者
fn handle_futex_word(pid: u32, uaddr: u64) {
    // futex 字为 u32, 未对齐地址直接跳过 (容错)
    if uaddr == 0 || uaddr & 0x3 != 0 {
        return;
    }
    let mut word = 0u32;
    if !crate::privileged::syscall::api::read_struct_from_user(uaddr, &mut word) {
        return;
    }
    // 仅处理本进程持有的 futex; 已标记过的不重复
    if word & FUTEX_TID_MASK != pid || word & FUTEX_OWNER_DIED != 0 {
        return;
    }
    write_u32_user(uaddr, word | FUTEX_OWNER_DIED);
    let _ = crate::privileged::syscall::futex::futex_wake(uaddr, 1);
}

/// 向用户地址写入 u32 (带指针校验, 失败静默)
fn write_u32_user(uaddr: u64, val: u32) {
    if !crate::privileged::syscall::raw::check_user_ptr(uaddr) {
        return;
    }
    // SAFETY: uaddr 已通过 check_user_ptr 验证; 调用方保证当前处于退出路径,
    // 目标进程页表仍处于激活状态 (地址空间尚未销毁)
    unsafe {
        core::ptr::write_volatile(uaddr as *mut u32, val);
    }
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_futex_word_masks() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    let word: u32 = 0x4000_0000 | 0x1234;
    assert_eq_test!(word & FUTEX_TID_MASK, 0x1234, "TID 掩码提取");
    check!(word & FUTEX_OWNER_DIED != 0, "owner-died 位保留");
    assert_eq_test!(
        (word & FUTEX_TID_MASK) | FUTEX_OWNER_DIED,
        0x4000_1234,
        "置位组合"
    );
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_robust_head_layout() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    // 用户 ABI 兼容: struct robust_list_head 必须是 24 字节
    assert_eq_test!(
        core::mem::size_of::<RobustListHead>(),
        24usize,
        "robust_list_head 尺寸"
    );
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
pub fn register_robust_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("robust", "futex_word_masks", test_futex_word_masks);
    r.register("robust", "robust_head_layout", test_robust_head_layout);
}
