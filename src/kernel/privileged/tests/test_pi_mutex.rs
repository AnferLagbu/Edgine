//! Priority Inheritance Mutex (PI Mutex) 测试 — P1 #3
//!
//! 覆盖 PI Mutex 状态机的关键路径:
//! - 基本 lock/unlock
//! - `try_lock` 失败路径
//! - 直接捐赠 (高优等待 → 低优持有者被提升)
//! - 多等待者取 max 优先级
//! - 释放时移交最高优先级等待者
//! - 无等待者时完全释放
//! - 重复 lock 同一线程不重复 push
//! - 回调注册
use super::{TestResult, runner};
use crate::privileged::sync::pi_mutex as pi;
use crate::register_tests_inner;

/// 模拟 A 持锁 + B/C/D 注册为等待者的辅助函数
///
/// A = PID 100, `base_prio=1`
/// 返回 (mutex, `a_pid`, `a_prio`)
fn setup_a_holds() -> (pi::PiMutex<u32>, u32, u32) {
    let m = pi::PiMutex::new(0u32);
    let a_pid = 100u32;
    let a_prio = 1u32;
    assert!(m.try_lock(a_pid, a_prio));
    assert_eq!(m.holder(), a_pid);
    assert_eq!(m.effective_priority(), a_prio);
    (m, a_pid, a_prio)
}

fn test_basic_lock_unlock() -> TestResult {
    let m = pi::PiMutex::new(42u32);
    if m.is_locked() {
        return TestResult::Fail("initial should be unlocked");
    }
    // 环境 pid: kernel_test 环境无调度器, current_pid() 返回 0 (PID_NONE)。
    // 必须用环境 pid 持锁, 否则 RAII drop 走 unlock_internal (校验 holder==current_pid)
    // 会因硬编码 pid 与环境不符而提前返回, 无法覆盖 RAII 释放路径。
    let my_pid = crate::privileged::sync::raw::current_pid();
    let g = m.lock(my_pid, 5);
    if !m.is_locked() {
        return TestResult::Fail("should be locked after lock()");
    }
    if m.holder() != my_pid {
        return TestResult::Fail("holder should be current pid");
    }
    if *g != 42 {
        return TestResult::Fail("data mismatch");
    }
    drop(g);
    if m.is_locked() {
        return TestResult::Fail("should be unlocked after drop");
    }
    TestResult::Pass
}

fn test_try_lock_fails_when_held() -> TestResult {
    let m = pi::PiMutex::new(0u32);
    if !m.try_lock(1, 5) {
        return TestResult::Fail("first try_lock should succeed");
    }
    if m.try_lock(2, 5) {
        return TestResult::Fail("second try_lock should fail");
    }
    if m.holder() != 1 {
        return TestResult::Fail("holder should be 1");
    }
    TestResult::Pass
}

fn test_donation_boosts_effective() -> TestResult {
    let (m, a_pid, a_prio) = setup_a_holds();
    // 模拟 B (prio=10) 失败注册
    if m.try_lock(200, 10) {
        return TestResult::Fail("try_lock should fail (held)");
    }
    if m.effective_priority() != 10 {
        return TestResult::Fail("effective_priority should be boosted to 10");
    }
    if m.holder() != a_pid {
        return TestResult::Fail("holder should still be A");
    }
    if m.waiter_count() != 1 {
        return TestResult::Fail("waiter count should be 1");
    }
    let _ = a_prio;
    TestResult::Pass
}

fn test_donation_max_of_waiters() -> TestResult {
    let (m, _, _) = setup_a_holds();
    let _ = m.try_lock(200, 10);
    let _ = m.try_lock(201, 5);
    let _ = m.try_lock(202, 8);
    let _ = m.try_lock(203, 12);
    if m.effective_priority() != 12 {
        return TestResult::Fail("max should be 12");
    }
    if m.waiter_count() != 4 {
        return TestResult::Fail("waiter count should be 4");
    }
    TestResult::Pass
}

fn test_unlock_transfers_to_highest() -> TestResult {
    let (m, _a_pid, _a_prio) = setup_a_holds();
    let _ = m.try_lock(200, 10);
    let _ = m.try_lock(201, 5);
    let _ = m.try_lock(202, 8);
    // v2.1 修复: 改用 force_unlock 跳过 current_pid 检查 (测试环境不返回 100)
    m.force_unlock();
    // 新持有者应是最高优先级等待者 (200, prio=10)
    if m.holder() != 200 {
        return TestResult::Fail("new holder should be 200");
    }
    if m.effective_priority() != 10 {
        return TestResult::Fail("effective should be 10");
    }
    if m.waiter_count() != 2 {
        return TestResult::Fail("remaining waiters should be 2");
    }
    TestResult::Pass
}

fn test_unlock_no_waiters_full_release() -> TestResult {
    let (m, _a_pid, _a_prio) = setup_a_holds();
    // v2.1 修复: 同上, force_unlock 跳过 holder 检查
    m.force_unlock();
    if m.is_locked() {
        return TestResult::Fail("should be fully released");
    }
    if m.holder() != 0 {
        return TestResult::Fail("holder should be 0");
    }
    if m.effective_priority() != 0 {
        return TestResult::Fail("effective should be 0");
    }
    TestResult::Pass
}

fn test_duplicate_lock_no_double_register() -> TestResult {
    let (m, _a_pid, _a_prio) = setup_a_holds();
    let _ = m.try_lock(200, 10);
    // 同一 PID 再次 try_lock (失败但不应再 push)
    let _ = m.try_lock(200, 10);
    if m.waiter_count() != 1 {
        return TestResult::Fail("duplicate lock should not double register");
    }
    TestResult::Pass
}

fn test_callback_install() -> TestResult {
    fn dummy_donate(_h: u32, _p: u32) {}
    // SAFETY: 测试作用域内有效, 不会被释放
    unsafe { pi::set_donation_callback(dummy_donate) };
    unsafe { pi::set_revoke_callback(dummy_donate) };
    // 简单验证: 调用不 panic
    let m = pi::PiMutex::new(0u32);
    let _ = m.try_lock(1, 5);
    let _ = m.try_lock(2, 10);
    // v2.1 修复: 用 force_unlock 跳过 holder 检查
    m.force_unlock();
    TestResult::Pass
}

// =============================================================================
// v2.1 — 等待者优先级动态重算 (DECISION-012)
// =============================================================================

fn test_v2_1_update_boost() -> TestResult {
    let (m, _a_pid, _a_prio) = setup_a_holds();
    // B 注册为 prio=10
    if m.try_lock(200, 10) {
        return TestResult::Fail("try_lock should fail (held)");
    }
    if m.effective_priority() != 10 {
        return TestResult::Fail("initial effective should be 10");
    }
    // B 提升到 prio=20
    if !m.update_waiter_priority(200, 20) {
        return TestResult::Fail("update should return true");
    }
    if m.effective_priority() != 20 {
        return TestResult::Fail("effective should be boosted to 20");
    }
    if m.holder() != 100 {
        return TestResult::Fail("holder should still be A (100)");
    }
    TestResult::Pass
}

fn test_v2_1_update_drop() -> TestResult {
    let (m, _a_pid, _a_prio) = setup_a_holds();
    if m.try_lock(200, 10) {
        return TestResult::Fail("try_lock should fail");
    }
    // B 降级到 prio=2 (A.base=1, max_waiter=2)
    if !m.update_waiter_priority(200, 2) {
        return TestResult::Fail("update should return true");
    }
    if m.effective_priority() != 2 {
        return TestResult::Fail("effective should drop to 2");
    }
    TestResult::Pass
}

fn test_v2_1_update_no_op() -> TestResult {
    let (m, _a_pid, _a_prio) = setup_a_holds();
    if m.try_lock(200, 10) {
        return TestResult::Fail("try_lock should fail");
    }
    // 非等待者 PID → 返回 false, effective 不变
    if m.update_waiter_priority(999, 100) {
        return TestResult::Fail("update on non-waiter should return false");
    }
    if m.effective_priority() != 10 {
        return TestResult::Fail("effective should be unchanged");
    }
    // 同优先级 → 返回 false
    if m.update_waiter_priority(200, 10) {
        return TestResult::Fail("update with same priority should return false");
    }
    TestResult::Pass
}

fn test_v2_1_update_among_many() -> TestResult {
    let (m, _a_pid, _a_prio) = setup_a_holds();
    // 4 个等待者, max=12 (203)
    let _ = m.try_lock(200, 10);
    let _ = m.try_lock(201, 5);
    let _ = m.try_lock(202, 8);
    let _ = m.try_lock(203, 12);
    if m.effective_priority() != 12 {
        return TestResult::Fail("initial max should be 12");
    }
    // 升级 200: 10 → 20 → 200 现在是 max → effective = max(holder_base=1, 20) = 20
    if !m.update_waiter_priority(200, 20) {
        return TestResult::Fail("update 200 should return true");
    }
    if m.effective_priority() != 20 {
        return TestResult::Fail("max should now be 20 (200 promoted)");
    }
    // 降级 203: 12 → 3, 但 200=20 仍是 max → effective 仍 20
    if !m.update_waiter_priority(203, 3) {
        return TestResult::Fail("update 203 should return true");
    }
    if m.effective_priority() != 20 {
        return TestResult::Fail("max should still be 20 (200 still highest)");
    }
    // 再次降级 200: 20 → 2 → max 变为 8 (202) → effective = 8
    if !m.update_waiter_priority(200, 2) {
        return TestResult::Fail("update 200 again should return true");
    }
    if m.effective_priority() != 8 {
        return TestResult::Fail("max should be 8 (202) after 200 dropped");
    }
    TestResult::Pass
}

fn test_v2_1_unlock_uses_recompute() -> TestResult {
    // 验证 unlock 路径走 recompute_and_notify 助手后行为正确
    // (v2.1 修复: 用 force_unlock 跳过 current_pid 检查, 测试环境无法返回 100)
    let (m, _a_pid, _a_prio) = setup_a_holds();
    let _ = m.try_lock(200, 10);
    let _ = m.try_lock(201, 5);
    let _ = m.try_lock(202, 8);
    m.force_unlock();
    if m.holder() != 200 {
        return TestResult::Fail("new holder should be 200 (prio=10)");
    }
    if m.effective_priority() != 10 {
        return TestResult::Fail("effective should be 10 after handoff");
    }
    if m.waiter_count() != 2 {
        return TestResult::Fail("remaining waiters should be 2");
    }
    TestResult::Pass
}

pub fn register_pi_mutex_tests() {
    let r = runner();
    register_tests_inner! { r:
        "PI_MUTEX": {
            "basic_lock_unlock": test_basic_lock_unlock,
            "try_lock_fails_when_held": test_try_lock_fails_when_held,
            "donation_boosts_effective": test_donation_boosts_effective,
            "donation_max_of_waiters": test_donation_max_of_waiters,
            "unlock_transfers_to_highest": test_unlock_transfers_to_highest,
            "unlock_no_waiters_full_release": test_unlock_no_waiters_full_release,
            "duplicate_lock_no_double_register": test_duplicate_lock_no_double_register,
            "callback_install": test_callback_install,
            // v2.1 等待者优先级动态重算
            "v2_1_update_boost": test_v2_1_update_boost,
            "v2_1_update_drop": test_v2_1_update_drop,
            "v2_1_update_no_op": test_v2_1_update_no_op,
            "v2_1_update_among_many": test_v2_1_update_among_many,
            "v2_1_unlock_uses_recompute": test_v2_1_unlock_uses_recompute,
        }
    }
}
