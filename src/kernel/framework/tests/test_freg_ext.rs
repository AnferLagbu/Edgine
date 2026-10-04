use super::check;
use crate::framework::freg::domain::RecoveryDomain;
use crate::framework::freg::manager::RecoveryManager;
use crate::framework::freg::types::{DomainState, MAX_CONSECUTIVE_FAILURES};
use crate::framework::tests::{TestResult, runner};
use crate::register_tests_inner;

fn test_domain_state_semantic() -> TestResult {
    let dom = RecoveryDomain::new(1);
    check!(
        dom.get_state() == DomainState::Active,
        "new domain should be Active"
    );

    dom.set_state(DomainState::Freezing, core::sync::atomic::Ordering::SeqCst);
    check!(
        dom.get_state() == DomainState::Freezing,
        "should be Freezing"
    );

    dom.set_state(DomainState::Degraded, core::sync::atomic::Ordering::SeqCst);
    check!(
        dom.get_state() == DomainState::Degraded,
        "should be Degraded"
    );
    check!(dom.is_active(), "Degraded should be active");

    dom.set_state(
        DomainState::Quarantined,
        core::sync::atomic::Ordering::SeqCst,
    );
    check!(
        dom.get_state() == DomainState::Quarantined,
        "should be Quarantined"
    );
    check!(!dom.is_active(), "Quarantined should not be active");
    TestResult::Pass
}

fn test_domain_from_u32_safe() -> TestResult {
    let valid = DomainState::from_u32(3);
    check!(
        valid == Some(DomainState::Recovering),
        "3 should be Recovering"
    );

    let invalid = DomainState::from_u32(99);
    check!(invalid.is_none(), "99 should be None");

    let fallback = DomainState::from_u32_fallback(99);
    check!(
        fallback == DomainState::Quarantined,
        "invalid should fallback to Quarantined"
    );

    let fallback_active = DomainState::from_u32_fallback(0);
    check!(
        fallback_active == DomainState::Active,
        "0 should fallback to Active"
    );
    TestResult::Pass
}

fn test_domain_degradation() -> TestResult {
    let dom = RecoveryDomain::new(2);
    dom.original_cap_mask
        .store(u64::MAX, core::sync::atomic::Ordering::SeqCst);
    dom.dom_cap_mask
        .store(u64::MAX, core::sync::atomic::Ordering::SeqCst);

    let rolled = dom.try_rollback(100, 0xAA);
    check!(rolled, "first rollback should succeed");
    let failures = dom
        .consecutive_failures
        .load(core::sync::atomic::Ordering::SeqCst);
    check!(failures >= 1, "should have at least 1 failure");

    for i in 0..4u64 {
        dom.try_rollback(200 + i * 1000, 0xBB + i);
    }
    let state = dom.get_state();
    check!(
        state == DomainState::Degraded || state == DomainState::Quarantined,
        "after many failures should be Degraded or Quarantined"
    );
    TestResult::Pass
}

fn test_domain_quarantine() -> TestResult {
    let dom = RecoveryDomain::new(3);
    dom.original_cap_mask
        .store(u64::MAX, core::sync::atomic::Ordering::SeqCst);
    dom.dom_cap_mask
        .store(u64::MAX, core::sync::atomic::Ordering::SeqCst);
    dom.consecutive_failures.store(
        MAX_CONSECUTIVE_FAILURES,
        core::sync::atomic::Ordering::SeqCst,
    );

    let rolled = dom.try_rollback(100, 0xCC);
    check!(!rolled, "quarantined domain should not rollback");
    check!(
        dom.get_state() == DomainState::Quarantined,
        "should be Quarantined"
    );
    TestResult::Pass
}

fn test_domain_backoff() -> TestResult {
    let dom = RecoveryDomain::new(4);
    dom.original_cap_mask
        .store(u64::MAX, core::sync::atomic::Ordering::SeqCst);
    dom.dom_cap_mask
        .store(u64::MAX, core::sync::atomic::Ordering::SeqCst);

    let first = dom.try_rollback(100, 0x11);
    check!(first, "first rollback should succeed");

    let backoff_until = dom.backoff_until.load(core::sync::atomic::Ordering::SeqCst);
    check!(backoff_until > 100, "backoff should be in the future");
    TestResult::Pass
}

fn test_domain_addr_range() -> TestResult {
    let dom = RecoveryDomain::new(5);
    check!(
        dom.add_addr_range(0x1000, 0x2000),
        "add_addr_range should succeed"
    );
    check!(dom.contains_addr(0x1500), "0x1500 should be in range");
    check!(!dom.contains_addr(0x0FFF), "0x0FFF should be below range");
    check!(
        !dom.contains_addr(0x2000),
        "0x2000 should be at range end (exclusive)"
    );
    TestResult::Pass
}

fn test_manager_register_find() -> TestResult {
    let mut mgr = RecoveryManager::new();
    let dom: &'static RecoveryDomain = {
        let bx = alloc::boxed::Box::new(RecoveryDomain::new(10));
        alloc::boxed::Box::leak(bx)
    };
    let result = mgr.register(dom);
    check!(result.is_some(), "register should succeed");

    let found = mgr.find(10);
    check!(found.is_some(), "find should succeed");
    check!(found.unwrap().id == 10, "found domain id mismatch");

    let not_found = mgr.find(999);
    check!(not_found.is_none(), "find 999 should be None");
    TestResult::Pass
}

fn test_manager_panic_msg_locate() -> TestResult {
    let mut mgr = RecoveryManager::new();
    let dom: &'static RecoveryDomain = {
        let bx = alloc::boxed::Box::new(RecoveryDomain::new(3));
        alloc::boxed::Box::leak(bx)
    };
    mgr.register(dom);

    {
        let mut msg = super::super::freg::PANIC_MSG.lock();
        let prefix = b"PMM out of memory";
        msg[..prefix.len()].copy_from_slice(prefix);
        msg[prefix.len()] = 0;
    }

    let located = mgr.locate_domain_by_panic_msg();
    check!(located.is_some(), "should locate PMM domain");
    check!(located.unwrap() == 3, "should locate domain 3 for PMM");
    TestResult::Pass
}

fn test_domain_mark_recovered() -> TestResult {
    let dom = RecoveryDomain::new(6);
    dom.original_cap_mask
        .store(u64::MAX, core::sync::atomic::Ordering::SeqCst);
    dom.dom_cap_mask
        .store(0, core::sync::atomic::Ordering::SeqCst);
    dom.consecutive_failures
        .store(3, core::sync::atomic::Ordering::SeqCst);

    dom.mark_recovered();
    let failures = dom
        .consecutive_failures
        .load(core::sync::atomic::Ordering::SeqCst);
    check!(failures == 0, "failures should be 0 after recovery");
    let caps = dom.dom_cap_mask.load(core::sync::atomic::Ordering::SeqCst);
    check!(caps == u64::MAX, "caps should be restored after recovery");
    TestResult::Pass
}

pub fn register_freg_ext_tests() {
    let r = runner();
    register_tests_inner! { r:
        "freg::domain": {
            "state_semantic": test_domain_state_semantic,
            "from_u32_safe": test_domain_from_u32_safe,
            "degradation": test_domain_degradation,
            "quarantine": test_domain_quarantine,
            "backoff": test_domain_backoff,
            "addr_range": test_domain_addr_range,
            "mark_recovered": test_domain_mark_recovered,
        },
        "freg::manager": {
            "register_find": test_manager_register_find,
            "panic_msg_locate": test_manager_panic_msg_locate,
        },
    }
}
