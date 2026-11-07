//! Priority Inheritance Mutex (PI Mutex) 测试 — P1 #3 / A-7 v3 重设计
//!
//! 覆盖 PI Mutex 状态机的关键路径:
//! - 基本 lock/unlock 与内核上下文 (pid 0) 路径
//! - `try_lock` 失败零副作用
//! - 捐赠 (等待者权重 → 持有者有效权重, 取全部等待者的 max)
//! - 释放/退出时按权重定向移交最高权重等待者
//! - 等待队列容量上界与重复入队刷新
//! - K-05 持有/等待登记生命周期 (退出撤销、退出释放、析构兜底)
use super::{TestResult, runner};
use crate::privileged::sync::pi_mutex as pi;
use crate::register_tests_inner;

/// EEVDF 权重刻度 (nice 0 / -1 / -2 / -3 / -4), 与 `NICE_TO_WEIGHT` 同值.
const W_NICE0: u32 = 1024;
const W_NICE1: u32 = 1277;
const W_NICE2: u32 = 1586;
const W_NICE3: u32 = 1991;
const W_NICE4: u32 = 2501;

/// 在调用方持有的 mutex 上模拟 A 持锁 (pid=100, 权重=nice0)
///
/// 必须在锁对象的最终地址上调用: 退出登记按实例地址记账, 加锁后按值移动
/// 会让登记指向旧地址.
fn setup_a_holds(m: &pi::PiMutex<u32>) -> (u32, u32) {
    let a_pid = 100u32;
    let a_weight = W_NICE0;
    assert!(m.try_lock(a_pid, a_weight));
    assert_eq!(m.holder(), a_pid);
    assert_eq!(m.effective_priority(), a_weight);
    (a_pid, a_weight)
}

fn test_basic_lock_unlock() -> TestResult {
    let m = pi::PiMutex::new(42u32);
    if m.is_locked() {
        return TestResult::Fail("initial should be unlocked");
    }
    let pid = 100u32;
    let g = m.lock(pid, W_NICE0);
    if !m.is_locked() {
        return TestResult::Fail("should be locked after lock()");
    }
    if m.holder() != pid {
        return TestResult::Fail("holder should be the locking pid");
    }
    if *g != 42 {
        return TestResult::Fail("data mismatch");
    }
    drop(g);
    if m.is_locked() {
        return TestResult::Fail("should be unlocked after drop");
    }
    if m.holder() != pi::PID_NONE {
        return TestResult::Fail("holder should be reset");
    }
    if m.effective_priority() != 0 {
        return TestResult::Fail("effective weight should be reset");
    }
    if pi::held_lock_count(pid) != 0 {
        return TestResult::Fail("guard drop should untrack the hold");
    }
    TestResult::Pass
}

fn test_kernel_context_pid0_lock() -> TestResult {
    // pid 0 = 内核上下文: 可持锁, 但不登记退出清理 (没有线程会退出它).
    let m = pi::PiMutex::new(0u32);
    let g = m.lock(pi::PID_NONE, W_NICE0);
    if !m.is_locked() || m.holder() != pi::PID_NONE {
        return TestResult::Fail("pid 0 should acquire an unlocked mutex");
    }
    if pi::held_lock_count(pi::PID_NONE) != 0 {
        return TestResult::Fail("pid 0 must not be registered for exit cleanup");
    }
    drop(g);
    if m.is_locked() {
        return TestResult::Fail("pid 0 guard drop should release");
    }
    TestResult::Pass
}

fn test_try_lock_fails_zero_side_effect() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (a_pid, _) = setup_a_holds(&m);
    let b_pid = 200u32;
    if m.try_lock(b_pid, W_NICE2) {
        return TestResult::Fail("try_lock should fail (held)");
    }
    if m.waiter_count() != 0 {
        return TestResult::Fail("failed try_lock must not enqueue a waiter");
    }
    if m.effective_priority() != W_NICE0 {
        return TestResult::Fail("failed try_lock must not donate");
    }
    if m.holder() != a_pid {
        return TestResult::Fail("holder should stay A");
    }
    if pi::waiting_lock_count(b_pid) != 0 {
        return TestResult::Fail("failed try_lock must not register a wait record");
    }
    if m.update_waiter_priority(b_pid, W_NICE2) {
        return TestResult::Fail("non-waiter update must return false");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_try_lock_guard_paths() -> TestResult {
    let m = pi::PiMutex::new(7u32);
    let Some(g) = m.try_lock_guard(100, W_NICE1) else {
        return TestResult::Fail("try_lock_guard should acquire an unlocked mutex");
    };
    if *g != 7 {
        return TestResult::Fail("guard data mismatch");
    }
    if m.try_lock_guard(200, W_NICE2).is_some() {
        return TestResult::Fail("second try_lock_guard should fail");
    }
    if m.waiter_count() != 0 {
        return TestResult::Fail("failed try_lock_guard must be side-effect free");
    }
    drop(g);
    if m.is_locked() {
        return TestResult::Fail("guard drop should release");
    }
    TestResult::Pass
}

fn test_donation_boosts_effective() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (a_pid, _) = setup_a_holds(&m);
    if !m.register_waiter_for_test(200, W_NICE2) {
        return TestResult::Fail("register waiter should succeed");
    }
    if m.effective_priority() != W_NICE2 {
        return TestResult::Fail("effective weight should be boosted");
    }
    if m.holder() != a_pid {
        return TestResult::Fail("holder should still be A");
    }
    if m.waiter_count() != 1 {
        return TestResult::Fail("waiter count should be 1");
    }
    if pi::waiting_lock_count(200) != 1 {
        return TestResult::Fail("waiter should be registered for exit cleanup");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_donation_max_of_waiters() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    let _ = m.register_waiter_for_test(200, W_NICE2);
    let _ = m.register_waiter_for_test(201, W_NICE1);
    let _ = m.register_waiter_for_test(202, W_NICE3);
    let _ = m.register_waiter_for_test(203, W_NICE4);
    if m.effective_priority() != W_NICE4 {
        return TestResult::Fail("effective should be the max waiter weight");
    }
    if m.waiter_count() != 4 {
        return TestResult::Fail("waiter count should be 4");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_unlock_transfers_to_highest() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    let _ = m.register_waiter_for_test(200, W_NICE2);
    let _ = m.register_waiter_for_test(201, W_NICE1);
    let _ = m.register_waiter_for_test(202, W_NICE3);
    m.force_unlock();
    if m.holder() != 202 {
        return TestResult::Fail("new holder should be the heaviest waiter (202)");
    }
    if m.effective_priority() != W_NICE3 {
        return TestResult::Fail("effective should stay at the new holder's weight");
    }
    if m.waiter_count() != 2 {
        return TestResult::Fail("remaining waiters should be 2");
    }
    if !m.try_lock(202, W_NICE3) {
        return TestResult::Fail("handoff target should accept");
    }
    m.force_unlock();
    if m.holder() != 200 {
        return TestResult::Fail("second handoff should go to 200");
    }
    m.force_unlock();
    if m.holder() != 201 {
        return TestResult::Fail("third handoff should go to 201");
    }
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("final release should unlock");
    }
    TestResult::Pass
}

fn test_handoff_only_for_target() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    let _ = m.register_waiter_for_test(200, W_NICE3);
    let _ = m.register_waiter_for_test(201, W_NICE1);
    m.force_unlock();
    if m.holder() != 200 {
        return TestResult::Fail("handoff should select 200");
    }
    // 移交是定向的: 其他等待者在目标确认接收前不得抢走所有权.
    if m.try_lock(201, W_NICE1) {
        return TestResult::Fail("non-target waiter must not take the handoff");
    }
    if m.holder() != 200 {
        return TestResult::Fail("holder should stay the handoff target");
    }
    if !m.try_lock(200, W_NICE3) {
        return TestResult::Fail("handoff target should accept");
    }
    if m.waiter_count() != 1 {
        return TestResult::Fail("accepted handoff should not disturb other waiters");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_unlock_no_waiters_full_release() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (a_pid, _) = setup_a_holds(&m);
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("should be fully released");
    }
    if m.holder() != pi::PID_NONE {
        return TestResult::Fail("holder should be reset");
    }
    if m.effective_priority() != 0 {
        return TestResult::Fail("effective weight should be 0");
    }
    if pi::held_lock_count(a_pid) != 0 {
        return TestResult::Fail("full release should untrack the hold");
    }
    TestResult::Pass
}

fn test_duplicate_waiter_upsert() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    let _ = m.register_waiter_for_test(200, W_NICE2);
    let _ = m.register_waiter_for_test(200, W_NICE1);
    if m.waiter_count() != 1 {
        return TestResult::Fail("duplicate registration must not stack");
    }
    if m.effective_priority() != W_NICE1 {
        return TestResult::Fail("re-registration should overwrite the weight");
    }
    let _ = m.register_waiter_for_test(200, W_NICE3);
    if m.effective_priority() != W_NICE3 {
        return TestResult::Fail("weight should follow the latest registration");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_waiter_queue_capacity() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    // 队列容量 = 16: 前 16 个等待者入队, 第 17 个被拒 (调用方退化为轮询).
    for idx in 0..16u32 {
        if !m.register_waiter_for_test(200 + idx, W_NICE0 + idx * 10) {
            return TestResult::Fail("first 16 waiters should be accepted");
        }
    }
    if m.waiter_count() != 16 {
        return TestResult::Fail("waiter count should reach capacity");
    }
    if m.register_waiter_for_test(300, W_NICE0) {
        return TestResult::Fail("17th waiter should be rejected");
    }
    if m.waiter_count() != 16 {
        return TestResult::Fail("rejected waiter must not be queued");
    }
    // 已入队 pid 的权重刷新不受容量限制.
    if !m.register_waiter_for_test(200, W_NICE4) {
        return TestResult::Fail("existing waiter weight refresh should succeed");
    }
    if m.effective_priority() != W_NICE4 {
        return TestResult::Fail("refreshed weight should drive effective weight");
    }
    m.force_unlock();
    TestResult::Pass
}

// =============================================================================
// v2.1 — 等待者优先级动态重算 (权重口径)
// =============================================================================

fn test_v2_1_update_boost() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    if !m.register_waiter_for_test(200, W_NICE2) {
        return TestResult::Fail("register waiter should succeed");
    }
    if m.effective_priority() != W_NICE2 {
        return TestResult::Fail("initial effective should be W_NICE2");
    }
    if !m.update_waiter_priority(200, W_NICE4) {
        return TestResult::Fail("update should return true");
    }
    if m.effective_priority() != W_NICE4 {
        return TestResult::Fail("effective should follow the promoted weight");
    }
    if m.holder() != 100 {
        return TestResult::Fail("holder should still be A (100)");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_v2_1_update_drop() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    if !m.register_waiter_for_test(200, W_NICE2) {
        return TestResult::Fail("register waiter should succeed");
    }
    // 降到持有者基础权重之下: 有效权重收敛回基础权重.
    if !m.update_waiter_priority(200, 900) {
        return TestResult::Fail("update should return true");
    }
    if m.effective_priority() != W_NICE0 {
        return TestResult::Fail("effective should fall back to the holder base");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_v2_1_update_no_op() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    if !m.register_waiter_for_test(200, W_NICE2) {
        return TestResult::Fail("register waiter should succeed");
    }
    if m.update_waiter_priority(999, W_NICE4) {
        return TestResult::Fail("update on non-waiter should return false");
    }
    if m.effective_priority() != W_NICE2 {
        return TestResult::Fail("effective should be unchanged");
    }
    if m.update_waiter_priority(200, W_NICE2) {
        return TestResult::Fail("update with same weight should return false");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_v2_1_update_among_many() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    let _ = m.register_waiter_for_test(200, W_NICE2);
    let _ = m.register_waiter_for_test(201, W_NICE0);
    let _ = m.register_waiter_for_test(202, W_NICE1);
    let _ = m.register_waiter_for_test(203, W_NICE3);
    if m.effective_priority() != W_NICE3 {
        return TestResult::Fail("initial max should be W_NICE3");
    }
    // 升级 200 → 新的 max.
    if !m.update_waiter_priority(200, W_NICE4) {
        return TestResult::Fail("update 200 should return true");
    }
    if m.effective_priority() != W_NICE4 {
        return TestResult::Fail("max should now be W_NICE4 (200 promoted)");
    }
    // 降级 203 低于基础权重: 200 仍是 max.
    if !m.update_waiter_priority(203, 900) {
        return TestResult::Fail("update 203 should return true");
    }
    if m.effective_priority() != W_NICE4 {
        return TestResult::Fail("max should still be W_NICE4 (200)");
    }
    // 再降 200: max 回落到 202 (W_NICE1).
    if !m.update_waiter_priority(200, 900) {
        return TestResult::Fail("update 200 again should return true");
    }
    if m.effective_priority() != W_NICE1 {
        return TestResult::Fail("max should be W_NICE1 (202) after 200 dropped");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_v2_1_unlock_uses_recompute() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (_, _) = setup_a_holds(&m);
    let _ = m.register_waiter_for_test(200, W_NICE2);
    let _ = m.register_waiter_for_test(201, W_NICE0);
    let _ = m.register_waiter_for_test(202, W_NICE1);
    m.force_unlock();
    if m.holder() != 200 {
        return TestResult::Fail("new holder should be 200 (heaviest)");
    }
    if m.effective_priority() != W_NICE2 {
        return TestResult::Fail("effective should be the new holder base");
    }
    if m.waiter_count() != 2 {
        return TestResult::Fail("remaining waiters should be 2");
    }
    if !m.try_lock(200, W_NICE2) {
        return TestResult::Fail("handoff target should accept");
    }
    m.force_unlock();
    if m.holder() != 202 {
        return TestResult::Fail("next handoff should go to 202");
    }
    TestResult::Pass
}

// =============================================================================
// K-05 — 持有/等待登记生命周期 (析构兜底 / 移交登记 / 退出释放)
// =============================================================================

fn test_k05_untrack_on_release() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let pid = 9101u32;
    if !m.try_lock(pid, W_NICE0) {
        return TestResult::Fail("try_lock should succeed");
    }
    if pi::held_lock_count(pid) != 1 {
        return TestResult::Fail("hold should be registered once");
    }
    // 完全释放须摘除登记; 重新加锁只产生一条新登记 (不是累加)
    m.force_unlock();
    if pi::held_lock_count(pid) != 0 {
        return TestResult::Fail("hold should be untracked on full release");
    }
    if !m.try_lock(pid, W_NICE0) {
        return TestResult::Fail("re-lock after release should succeed");
    }
    if pi::held_lock_count(pid) != 1 {
        return TestResult::Fail("re-lock should register exactly one entry");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_k05_handoff_registry_and_accept() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let holder = 9201u32;
    let next = 9202u32;
    if !m.try_lock(holder, W_NICE0) {
        return TestResult::Fail("A should acquire");
    }
    if !m.register_waiter_for_test(next, W_NICE3) {
        return TestResult::Fail("B should be enqueued");
    }
    m.force_unlock();

    // 移交后: 持有登记随所有权转移, 等待登记随入队摘除.
    if pi::held_lock_count(holder) != 0 {
        return TestResult::Fail("old holder should be untracked after handoff");
    }
    if pi::held_lock_count(next) != 1 {
        return TestResult::Fail("next holder should be tracked after handoff");
    }
    if pi::waiting_lock_count(next) != 0 {
        return TestResult::Fail("handoff target should not stay in the wait registry");
    }
    if m.holder() != next {
        return TestResult::Fail("holder should be next after handoff");
    }

    // 定向移交必须被 next 的 try_lock 确认接收.
    if !m.try_lock(next, W_NICE3) {
        return TestResult::Fail("next should accept the handoff");
    }
    if m.waiter_count() != 0 {
        return TestResult::Fail("accepted handoff should not stay queued");
    }
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("final release should unlock");
    }
    if pi::held_lock_count(next) != 0 {
        return TestResult::Fail("final release should untrack next");
    }
    TestResult::Pass
}

fn test_k05_exit_releases_pending_handoff() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let holder = 9301u32;
    let next = 9302u32;
    if !m.try_lock(holder, W_NICE0) {
        return TestResult::Fail("A should acquire");
    }
    if !m.register_waiter_for_test(next, W_NICE3) {
        return TestResult::Fail("B should be enqueued");
    }
    m.force_unlock();
    if m.holder() != next {
        return TestResult::Fail("holder should be next before exit");
    }

    // next 在确认接收前退出: 退出路径必须按持有人强制释放待接收的移交.
    pi::pi_mutex_process_exit(next);
    if m.is_locked() {
        return TestResult::Fail("pending handoff should be force-released on exit");
    }
    if m.holder() != pi::PID_NONE {
        return TestResult::Fail("holder should be reset on exit release");
    }
    if pi::held_lock_count(next) != 0 {
        return TestResult::Fail("exit cleanup should remove the registry entry");
    }
    if !m.try_lock(9303, W_NICE0) {
        return TestResult::Fail("lock should be acquirable after exit release");
    }
    m.force_unlock();
    TestResult::Pass
}

fn test_k05_exit_hands_off_to_next() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let holder = 9501u32;
    let high = 9502u32;
    let low = 9503u32;
    if !m.try_lock(holder, W_NICE0) {
        return TestResult::Fail("A should acquire");
    }
    if !m.register_waiter_for_test(high, W_NICE3) {
        return TestResult::Fail("high waiter should be enqueued");
    }
    if !m.register_waiter_for_test(low, W_NICE1) {
        return TestResult::Fail("low waiter should be enqueued");
    }
    // 持有者线程退出: 强制释放仍按等待权重定向移交, 不是整体丢弃.
    pi::pi_mutex_process_exit(holder);
    if m.holder() != high {
        return TestResult::Fail("exit release should hand off to the heaviest waiter");
    }
    if pi::held_lock_count(holder) != 0 {
        return TestResult::Fail("exited holder should be untracked");
    }
    if m.try_lock(low, W_NICE1) {
        return TestResult::Fail("non-target waiter must not take the handoff");
    }
    if !m.try_lock(high, W_NICE3) {
        return TestResult::Fail("handoff target should accept");
    }
    m.force_unlock();
    if m.holder() != low {
        return TestResult::Fail("remaining waiter should become holder");
    }
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("final release should unlock");
    }
    TestResult::Pass
}

// =============================================================================
// A-7 冻结回归锁 — 内核上下文接管未接收的定向移交
// =============================================================================

/// 现场: 桶锁已把所有权移交给目标线程 (`holder == handoff`), 目标被 `unblock`
/// 到释放方的 CPU 运行队列; 该 CPU 上的线程正以 `try_lock_guard(0, 0)` 在退出
/// 路径唤醒 CLEARTID/robust futex. 内核上下文既不能睡眠, 也永远不会成为移交
/// 目标, 拒绝接管就只能永久自旋 (目标等不到 CPU, 占着 CPU 的正是唤醒者).
fn test_a7_kernel_takes_pending_handoff() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let holder = 9601u32;
    let next = 9602u32;
    let low = 9603u32;
    let other = 9604u32;
    if !m.try_lock(holder, W_NICE0) {
        return TestResult::Fail("A should acquire");
    }
    if !m.register_waiter_for_test(next, W_NICE2) {
        return TestResult::Fail("next should be enqueued");
    }
    if !m.register_waiter_for_test(low, W_NICE1) {
        return TestResult::Fail("low should be enqueued");
    }
    m.force_unlock(); // → holder == handoff == next, low 仍排队
    if !m.is_locked() || m.holder() != next {
        return TestResult::Fail("pending handoff state expected");
    }
    // 定向移交语义不收窄: 非目标用户线程仍不得插队.
    if m.try_lock(other, W_NICE0) {
        return TestResult::Fail("user pid must not barge a pending handoff");
    }
    let Some(guard) = m.try_lock_guard(pi::PID_NONE, 0) else {
        return TestResult::Fail("kernel context should take over a pending handoff");
    };
    if m.holder() != pi::PID_NONE || !m.is_locked() {
        return TestResult::Fail("kernel context should own the mutex");
    }
    if pi::held_lock_count(next) != 0 {
        return TestResult::Fail("takeover should untrack the handoff target");
    }
    drop(guard);
    // 接管方释放后按权重移交给剩余等待者, 不丢唤醒.
    if m.holder() != low {
        return TestResult::Fail("kernel release should hand off to the remaining waiter");
    }
    if !m.try_lock(low, W_NICE1) {
        return TestResult::Fail("remaining waiter should accept the handoff");
    }
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("final release should unlock");
    }
    TestResult::Pass
}

fn test_k05_exit_cancels_waiting() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (holder, _) = setup_a_holds(&m);
    if !m.register_waiter_for_test(200, W_NICE2) {
        return TestResult::Fail("waiter should be enqueued");
    }
    if m.effective_priority() != W_NICE2 {
        return TestResult::Fail("waiter should donate before exit");
    }
    // 等待者退出: 撤销等待登记并收回捐赠, 持有关系不受影响.
    pi::pi_mutex_process_exit(200);
    if m.waiter_count() != 0 {
        return TestResult::Fail("exited waiter should be dequeued");
    }
    if pi::waiting_lock_count(200) != 0 {
        return TestResult::Fail("exited waiter should be untracked");
    }
    if m.effective_priority() != W_NICE0 {
        return TestResult::Fail("donation should be revoked after waiter exit");
    }
    if !m.is_locked() || m.holder() != holder {
        return TestResult::Fail("holder should be unaffected by waiter exit");
    }
    pi::pi_mutex_process_exit(holder);
    if m.is_locked() {
        return TestResult::Fail("holder exit should release the lock");
    }
    TestResult::Pass
}

fn test_k05_exit_cleanup_idempotent() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    let (a_pid, _) = setup_a_holds(&m);
    // 无登记的 pid / PID_NONE: 空转不 panic.
    pi::pi_mutex_process_exit(999);
    pi::pi_mutex_process_exit(pi::PID_NONE);
    if !m.is_locked() || m.holder() != a_pid {
        return TestResult::Fail("cleanup of unknown pid must be a no-op");
    }
    pi::pi_mutex_process_exit(a_pid);
    if m.is_locked() {
        return TestResult::Fail("holder exit should release");
    }
    // 二次清理幂等.
    pi::pi_mutex_process_exit(a_pid);
    if m.is_locked() {
        return TestResult::Fail("second cleanup must stay released");
    }
    TestResult::Pass
}

fn test_k05_drop_clears_registry() -> TestResult {
    let pid = 9401u32;
    let waiter = 9402u32;
    {
        let m = pi::PiMutex::new(0u32);
        if !m.try_lock(pid, W_NICE0) {
            return TestResult::Fail("try_lock should succeed");
        }
        if !m.register_waiter_for_test(waiter, W_NICE2) {
            return TestResult::Fail("waiter should be enqueued");
        }
        if pi::held_lock_count(pid) != 1 || pi::waiting_lock_count(waiter) != 1 {
            return TestResult::Fail("hold and wait should be registered");
        }
        // 故意不 unlock 直接析构: Drop 兜底摘除指向本实例的两类条目.
    }
    if pi::held_lock_count(pid) != 0 {
        return TestResult::Fail("drop should clear hold entries");
    }
    if pi::waiting_lock_count(waiter) != 0 {
        return TestResult::Fail("drop should clear wait entries");
    }
    TestResult::Pass
}

fn test_force_unlock_idempotent() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    // 未持有时安全空转.
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("no-op force_unlock must not lock");
    }
    if !m.try_lock(100, W_NICE0) {
        return TestResult::Fail("try_lock should succeed");
    }
    m.force_unlock();
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("repeated force_unlock must stay released");
    }
    TestResult::Pass
}

pub fn register_pi_mutex_tests() {
    let r = runner();
    register_tests_inner! { r:
        "PI_MUTEX": {
            "basic_lock_unlock": test_basic_lock_unlock,
            "kernel_context_pid0_lock": test_kernel_context_pid0_lock,
            "try_lock_fails_zero_side_effect": test_try_lock_fails_zero_side_effect,
            "try_lock_guard_paths": test_try_lock_guard_paths,
            "donation_boosts_effective": test_donation_boosts_effective,
            "donation_max_of_waiters": test_donation_max_of_waiters,
            "unlock_transfers_to_highest": test_unlock_transfers_to_highest,
            "handoff_only_for_target": test_handoff_only_for_target,
            "unlock_no_waiters_full_release": test_unlock_no_waiters_full_release,
            "duplicate_waiter_upsert": test_duplicate_waiter_upsert,
            "waiter_queue_capacity": test_waiter_queue_capacity,
            // v2.1 等待者优先级动态重算
            "v2_1_update_boost": test_v2_1_update_boost,
            "v2_1_update_drop": test_v2_1_update_drop,
            "v2_1_update_no_op": test_v2_1_update_no_op,
            "v2_1_update_among_many": test_v2_1_update_among_many,
            "v2_1_unlock_uses_recompute": test_v2_1_unlock_uses_recompute,
            // K-05 持有/等待登记生命周期
            "k05_untrack_on_release": test_k05_untrack_on_release,
            "k05_handoff_registry_and_accept": test_k05_handoff_registry_and_accept,
            "k05_exit_releases_pending_handoff": test_k05_exit_releases_pending_handoff,
            "k05_exit_hands_off_to_next": test_k05_exit_hands_off_to_next,
            "a7_kernel_takes_pending_handoff": test_a7_kernel_takes_pending_handoff,
            "k05_exit_cancels_waiting": test_k05_exit_cancels_waiting,
            "k05_exit_cleanup_idempotent": test_k05_exit_cleanup_idempotent,
            "k05_drop_clears_registry": test_k05_drop_clears_registry,
            "force_unlock_idempotent": test_force_unlock_idempotent,
        }
    }
}
