use super::assert_eq_test;
use super::check;
use crate::privileged::proc::types::{ProcessId, ProcessState, ThreadId};
use crate::privileged::tests::{TestResult, runner};
use crate::register_tests_inner;

fn test_process_state_from_u8() -> TestResult {
    assert_eq_test!(
        ProcessState::from_u8(0),
        ProcessState::Created,
        "state 0 should be Created"
    );
    assert_eq_test!(
        ProcessState::from_u8(1),
        ProcessState::Ready,
        "state 1 should be Ready"
    );
    assert_eq_test!(
        ProcessState::from_u8(2),
        ProcessState::Running,
        "state 2 should be Running"
    );
    assert_eq_test!(
        ProcessState::from_u8(3),
        ProcessState::Blocked,
        "state 3 should be Blocked"
    );
    assert_eq_test!(
        ProcessState::from_u8(4),
        ProcessState::Zombie,
        "state 4 should be Zombie"
    );
    assert_eq_test!(
        ProcessState::from_u8(5),
        ProcessState::Terminated,
        "state 5 should be Terminated"
    );
    assert_eq_test!(
        ProcessState::from_u8(6),
        ProcessState::Frozen,
        "state 6 should be Frozen"
    );
    assert_eq_test!(
        ProcessState::from_u8(255),
        ProcessState::Created,
        "invalid state should fallback to Created"
    );
    TestResult::Pass
}

fn test_process_state_from_u32() -> TestResult {
    assert_eq_test!(
        ProcessState::from_u32(0),
        ProcessState::Created,
        "u32 0 should be Created"
    );
    assert_eq_test!(
        ProcessState::from_u32(2),
        ProcessState::Running,
        "u32 2 should be Running"
    );
    assert_eq_test!(
        ProcessState::from_u32(999),
        ProcessState::Created,
        "invalid u32 should fallback"
    );
    TestResult::Pass
}

fn test_process_state_name() -> TestResult {
    check!(
        ProcessState::Created.name() == "Created",
        "Created name mismatch"
    );
    check!(ProcessState::Ready.name() == "Ready", "Ready name mismatch");
    check!(
        ProcessState::Running.name() == "Running",
        "Running name mismatch"
    );
    check!(
        ProcessState::Blocked.name() == "Blocked",
        "Blocked name mismatch"
    );
    check!(
        ProcessState::Zombie.name() == "Zombie",
        "Zombie name mismatch"
    );
    check!(
        ProcessState::Terminated.name() == "Terminated",
        "Terminated name mismatch"
    );
    check!(
        ProcessState::Frozen.name() == "Frozen",
        "Frozen name mismatch"
    );
    TestResult::Pass
}

// 2026-09-11 实测: Ready==Ready 触发 eq_op, 需豁免
#[allow(clippy::eq_op)]
fn test_process_state_equality() -> TestResult {
    check!(
        ProcessState::Ready == ProcessState::Ready,
        "same states should be equal"
    );
    check!(
        ProcessState::Ready != ProcessState::Running,
        "different states should not be equal"
    );
    TestResult::Pass
}

fn test_process_id() -> TestResult {
    let pid1 = ProcessId(1);
    let pid2 = ProcessId(2);
    let pid1_copy = ProcessId(1);
    check!(pid1 == pid1_copy, "same PIDs should be equal");
    check!(pid1 != pid2, "different PIDs should not be equal");
    check!(pid1.0 == 1, "PID value should be 1");
    TestResult::Pass
}

fn test_thread_id() -> TestResult {
    let tid1 = ThreadId(1);
    let tid2 = ThreadId(100);
    check!(tid1.0 == 1, "TID value should be 1");
    check!(tid2.0 == 100, "TID value should be 100");
    TestResult::Pass
}

fn test_process_state_lifecycle() -> TestResult {
    let states = [
        ProcessState::Created,
        ProcessState::Ready,
        ProcessState::Running,
        ProcessState::Blocked,
        ProcessState::Zombie,
        ProcessState::Terminated,
        ProcessState::Frozen,
    ];
    for (i, &state) in states.iter().enumerate() {
        assert_eq_test!(state as u8, i as u8, "state discriminant mismatch");
    }
    TestResult::Pass
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn test_stack_canary() -> TestResult {
    use crate::privileged::proc::KERNEL_STACK_CANARY;
    check!(
        KERNEL_STACK_CANARY == 0xDEADBEEF_CAFEBABE,
        "canary value mismatch"
    );
    TestResult::Pass
}

/// A3: OOMD 牺牲者择优须排除僵尸进程 (丙批审查整改)
///
/// 双向验证:
/// - 僵尸**即便 RSS 最大**也不得选中, 且不得触发页表遍历 (延迟闭包不被调用)
/// - 非僵尸同条件仍可被选中
/// - idle/内核线程 (`pid == 0`) 与无用户页表 (`cr3 == 0`) 一并排除
fn test_oomd_victim_filter() -> TestResult {
    use crate::privileged::proc::oomd::better_oom_victim;
    use core::cell::Cell;

    let walked = Cell::new(false);
    check!(
        !better_oom_victim(42, ProcessState::Zombie, 0x1000, 0, || {
            walked.set(true);
            9999
        }),
        "zombie must not be selected even with the largest RSS"
    );
    check!(!walked.get(), "zombie must not trigger the page table walk");

    check!(
        better_oom_victim(42, ProcessState::Running, 0x1000, 0, || 9999),
        "live process with larger RSS must be selectable"
    );
    check!(
        better_oom_victim(42, ProcessState::Blocked, 0x1000, 5, || 6),
        "blocked live process must be selectable"
    );
    check!(
        !better_oom_victim(42, ProcessState::Ready, 0x1000, 9999, || 9999),
        "equal RSS must not replace the current victim"
    );
    check!(
        !better_oom_victim(0, ProcessState::Running, 0x1000, 0, || 9999),
        "pid 0 (idle/kernel thread) must be excluded"
    );
    check!(
        !better_oom_victim(42, ProcessState::Running, 0, 0, || 9999),
        "process without user page table must be excluded"
    );

    TestResult::Pass
}

/// A4: OOMD `terminated_count` 仅在信号真实送达时累加 (丙批审查整改)
///
/// 两个分支都覆盖, 并验证「这一轮没得杀」时 `stats()` 不涨:
/// - `delivered == false` (无合格候选 / 发送失败) → `terminated_count` 不变
/// - `delivered == true` (信号送达) → `terminated_count` +1
fn test_oomd_terminated_count_only_on_delivery() -> TestResult {
    use crate::privileged::proc::oomd::OomDaemon;

    let daemon = OomDaemon::new();
    check!(
        daemon.stats().1 == 0,
        "fresh daemon must report 0 terminations"
    );

    daemon.record_termination(false);
    check!(
        daemon.stats().1 == 0,
        "no delivery (no victim / send failed) must not increase terminated_count"
    );

    daemon.record_termination(true);
    check!(
        daemon.stats().1 == 1,
        "delivery must increase terminated_count by exactly 1"
    );

    TestResult::Pass
}

// E-04 (2026-09-06) 同源先例: cr3 三用例依赖裸机页表分配
// (`user_proc::raw::create_user_page_table` → `get_vmm()`), host 无 VMM/PMM
// 初始化, 调用即 panic (once_lock.rs "[VMM] accessed before initialization"),
// 且该 panic 发生在不可 unwind 路径 → SIGABRT 打断整个 run_all.
// host-test 下以 Skip 占位, kernel_test (QEMU) 下走下方原实现.
#[cfg(feature = "host-test")]
fn test_cr3_shared_owner_exit_keeps_table() -> TestResult {
    TestResult::Skip("E-04: host 无 VMM/PMM 初始化, 跳过 (依赖裸机页表分配)")
}

#[cfg(feature = "host-test")]
fn test_cr3_single_owner_exit_zeroes_once() -> TestResult {
    TestResult::Skip("E-04: host 无 VMM/PMM 初始化, 跳过 (依赖裸机页表分配)")
}

#[cfg(feature = "host-test")]
fn test_cr3_transfer_source_cleared() -> TestResult {
    TestResult::Skip("E-04: host 无 VMM/PMM 初始化, 跳过 (依赖裸机页表分配)")
}

/// cr3 所有权: `CLONE_VM` 双所有者 —— "共享者仍在运行时所有者退出" 不得销毁页表.
///
/// 与 [clone.rs] 的共享登记同一入口 (`pmm.frame_inc`), 与 [process.rs]
/// `Process::drop` 同一销毁判据 (`pmm.frame_dec` 归零才销毁). 覆盖
/// `docs/plan/cr3-lifetime-ownership.md` §5.1 门槛 7 的共享用例 (G1).
#[cfg(not(feature = "host-test"))]
fn test_cr3_shared_owner_exit_keeps_table() -> TestResult {
    use crate::privileged::mm::PhysAddr;
    use crate::privileged::mm::pmm::get_pmm;
    use crate::privileged::proc::raw;
    use crate::privileged::proc::user_proc::raw::create_user_page_table;
    use core::sync::atomic::Ordering;

    let cr3 = create_user_page_table();
    check!(cr3 != 0, "create_user_page_table must return a valid pml4");
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 1,
        "fresh user page table must start with exactly one holder"
    );

    // 两个 Process 持有同一 cr3: 与 CLONE_VM 的双所有者同形 (均不入进程表)
    let owner = raw::alloc_process(0xFFF0, "cr3-owner", None);
    let sharer = raw::alloc_process(0xFFF1, "cr3-sharer", None);
    raw::process_ref_mut(owner).cr3.store(cr3, Ordering::SeqCst);
    raw::process_ref_mut(sharer)
        .cr3
        .store(cr3, Ordering::SeqCst);
    // CLONE_VM 的共享登记 (clone.rs 同一入口)
    check!(
        get_pmm().frame_inc(PhysAddr(cr3)),
        "shared holder registration must succeed"
    );
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 2,
        "two holders expected after CLONE_VM-style registration"
    );

    // 所有者先退出, 共享者仍在运行 ⇒ 计数 2 -> 1, 页表必须存活
    raw::drop_boxed_process(owner);
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 1,
        "owner exit must not release the still-shared page table"
    );
    // 页表不得被归还 PMM: 连续分配不得再取到该帧
    let mut reissued = false;
    for _ in 0..64 {
        if let Some(p) = get_pmm().alloc_page() {
            if p.0 == cr3 {
                reissued = true;
            }
            get_pmm().free_page(p);
        }
    }
    check!(
        !reissued,
        "shared page table frame must not be reissued to the allocator"
    );

    // 共享者退出 (最后持有者) ⇒ 计数归零 ⇒ 销毁
    raw::drop_boxed_process(sharer);
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 0,
        "last holder exit must zero the count (destroy path taken)"
    );
    TestResult::Pass
}

/// cr3 所有权: 单一所有者退出 ⇒ 计数归零恰好一次, 且归零后不再报告归零 (fail-closed).
#[cfg(not(feature = "host-test"))]
fn test_cr3_single_owner_exit_zeroes_once() -> TestResult {
    use crate::privileged::mm::PhysAddr;
    use crate::privileged::mm::pmm::get_pmm;
    use crate::privileged::proc::raw;
    use crate::privileged::proc::user_proc::raw::create_user_page_table;
    use core::sync::atomic::Ordering;

    let cr3 = create_user_page_table();
    check!(cr3 != 0, "create_user_page_table must return a valid pml4");
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 1,
        "fresh user page table must start with exactly one holder"
    );

    let owner = raw::alloc_process(0xFFF2, "cr3-single", None);
    raw::process_ref_mut(owner).cr3.store(cr3, Ordering::SeqCst);
    raw::drop_boxed_process(owner);
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 0,
        "single owner exit must zero the count"
    );
    // 归零后 (等价未计数态) 再 dec 不得报告归零: 防同一 PML4 被二次销毁
    check!(
        !get_pmm().frame_dec(PhysAddr(cr3)),
        "dec on zeroed frame must be fail-closed"
    );
    TestResult::Pass
}

/// cr3 所有权: execve 转移 —— 源清空后源 drop 不得影响目标持有 (G3).
///
/// 与 [proc_ops.rs] `proc_exec_replace` 阶段 4 同形: 所有权随 cr3 移动 (不重新
/// 计数), 源必须清空, 否则源 `Process::drop` 会把目标在用的页表计数减到归零.
#[cfg(not(feature = "host-test"))]
fn test_cr3_transfer_source_cleared() -> TestResult {
    use crate::privileged::mm::PhysAddr;
    use crate::privileged::mm::pmm::get_pmm;
    use crate::privileged::proc::raw;
    use crate::privileged::proc::user_proc::raw::create_user_page_table;
    use core::sync::atomic::Ordering;

    let cr3 = create_user_page_table();
    check!(cr3 != 0, "create_user_page_table must return a valid pml4");

    let src = raw::alloc_process(0xFFF3, "cr3-temp", None);
    let dst = raw::alloc_process(0xFFF4, "cr3-target", None);
    raw::process_ref_mut(src).cr3.store(cr3, Ordering::SeqCst);
    // 转移: 目标接手, 源清空 (不调用 inc/dec)
    raw::process_ref_mut(dst).cr3.store(cr3, Ordering::SeqCst);
    raw::process_ref_mut(src).cr3.store(0, Ordering::SeqCst);
    raw::drop_boxed_process(src);
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 1,
        "transfer must keep exactly one holder"
    );
    raw::drop_boxed_process(dst);
    check!(
        get_pmm().frame_ref_count(PhysAddr(cr3)) == 0,
        "target exit must zero the count"
    );
    TestResult::Pass
}

/// COW 克隆中途分配失败的回归用例 —— `cr3-lifetime-ownership.md` §5.2 注入 2 的载体.
///
/// **判别力**:
/// - 恢复 `sys_fork` 的 `.unwrap_or(parent_cr3)` 回退 (注入 2) 时, 克隆失败不再回滚
///   ⇒ 子进程静默继承父 cr3 且 `sys_fork` 返回非 0 ⇒ 首项断言 FAIL.
/// - 克隆中途失败不回滚已建子树页表帧 (修复前形态: 4 处 `alloc_page()?` 直接返回)
///   ⇒ 空闲页数不复原 ⇒ 第三项断言 FAIL.
///
/// 注入面 (`PhysicalMemoryManager::arm_alloc_failure`) 只拦 `alloc_page`; 子进程描述符
/// 走 `alloc_pages`/slab, 不消耗注入计数, 故 4 轮迭代逐一命中克隆内部的
/// PML4 → PDPT → PD → PT 四个页表帧分配点.
#[cfg(all(feature = "kernel_test", not(feature = "host-test")))]
fn test_fork_cow_failure_rolls_back() -> TestResult {
    use crate::privileged::mm::PhysAddr;
    use crate::privileged::mm::mechanism::vmm_destroy_page_table;
    use crate::privileged::mm::pmm::get_pmm;
    use crate::privileged::proc::raw;
    use crate::privileged::proc::{PROCESS_TABLE, SCHEDULER, sys_fork};
    use core::sync::atomic::Ordering;

    let pmm = get_pmm();
    let (parent_pml4, data_phys) = match super::test_mm::cow_setup_mapped_page() {
        Ok(v) => v,
        Err(msg) => return TestResult::Fail(msg),
    };

    let Some(pid) = PROCESS_TABLE.allocate_pid() else {
        vmm_destroy_page_table(parent_pml4);
        return TestResult::Fail("no free PID for the rollback case");
    };
    let parent_ptr = raw::alloc_process(pid, "cow-fail-parent", None);
    raw::process_ref_mut(parent_ptr)
        .cr3
        .store(parent_pml4, Ordering::SeqCst);
    if !PROCESS_TABLE.insert(parent_ptr) {
        raw::drop_boxed_process(parent_ptr);
        PROCESS_TABLE.free_pid(pid);
        vmm_destroy_page_table(parent_pml4);
        return TestResult::Fail("parent must be inserted into PROCESS_TABLE");
    }

    let prev_current = SCHEDULER.current();
    SCHEDULER.set_current(pid);

    let result = (|| -> TestResult {
        // 首轮迭代可能触发内核分配器缓存增长 (子进程描述符), 故以首轮后的空闲页数
        // 为稳定基线; 其后每轮克隆失败都必须回到该基线 (已建子树页表帧全部归还).
        let mut free_baseline = 0u64;
        for (round, skip) in (0..4u32).enumerate() {
            pmm.arm_alloc_failure(skip);
            let child = sys_fork();
            pmm.disarm_alloc_failure();

            check!(
                child == 0,
                "克隆失败时 sys_fork 必须返回 0 (不得静默共享父 cr3)"
            );
            check!(
                pmm.frame_ref_count(data_phys) == 1,
                "克隆失败不得为数据帧登记第二持有者"
            );
            check!(
                pmm.frame_ref_count(PhysAddr(parent_pml4)) == 1,
                "克隆失败不得改变父页表帧持有数"
            );

            if round == 0 {
                free_baseline = pmm.get_free_pages();
            } else {
                check!(
                    pmm.get_free_pages() == free_baseline,
                    "克隆失败后已建子树页表帧必须全部归还"
                );
            }
        }
        TestResult::Pass
    })();

    // 拆除 (无条件执行): 恢复调度器当前进程 → 摘表 → 释放描述符
    // (`Process::drop` 在计数归零时销毁 parent_pml4 ⇒ 表格帧与数据帧一并归还)
    SCHEDULER.set_current(prev_current.unwrap_or(0));
    let _ = PROCESS_TABLE.remove(pid);
    raw::drop_boxed_process(parent_ptr);
    PROCESS_TABLE.free_pid(pid);
    check!(
        pmm.frame_ref_count(PhysAddr(parent_pml4)) == 0,
        "parent 拆除后其页表帧应归零"
    );
    check!(
        pmm.frame_ref_count(data_phys) == 0,
        "parent 拆除后数据帧应归零"
    );

    result
}

pub fn register_proc_tests() {
    let r = runner();
    register_tests_inner! { r:
        "Proc": {
            "state_from_u8": test_process_state_from_u8,
            "state_from_u32": test_process_state_from_u32,
            "state_name": test_process_state_name,
            "state_equality": test_process_state_equality,
            "process_id": test_process_id,
            "thread_id": test_thread_id,
            "state_lifecycle": test_process_state_lifecycle,
            "stack_canary": test_stack_canary,
            "oomd_victim_filter": test_oomd_victim_filter,
            "oomd_terminated_count_only_on_delivery": test_oomd_terminated_count_only_on_delivery,
            "cr3_shared_owner_exit_keeps_table": test_cr3_shared_owner_exit_keeps_table,
            "cr3_single_owner_exit_zeroes_once": test_cr3_single_owner_exit_zeroes_once,
            "cr3_transfer_source_cleared": test_cr3_transfer_source_cleared,
        },
    }
    // §5.2 注入 2 的载体: 注入面仅在 kernel_test 配置编译, 注册同样门控
    #[cfg(all(feature = "kernel_test", not(feature = "host-test")))]
    register_tests_inner! { r:
        "Proc": {
            "fork_cow_failure_rolls_back": test_fork_cow_failure_rolls_back,
        },
    }
}
