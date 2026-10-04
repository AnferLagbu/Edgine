use super::check;
use crate::privileged::freg::domain::RecoveryDomain;
use crate::privileged::freg::undo_log::UndoLog;
use crate::privileged::tests::{TestResult, runner};
use crate::register_tests_inner;
use alloc::boxed::Box;

fn test_undo_log_basic() -> TestResult {
    let undo = Box::new(UndoLog::new());
    check!(undo.count == 0, "expected empty undo log");
    TestResult::Pass
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
fn test_undo_log_record() -> TestResult {
    let mut undo = Box::new(UndoLog::new());
    undo.current_generation = 1;

    let mut value: u64 = 42;
    let old = value;
    value = 100;
    undo.record(&mut value as *mut u64, old);

    check!(undo.count == 1, "expected 1 entry after record");
    check!(undo.entries[0].generation == 1, "generation mismatch");
    TestResult::Pass
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
fn test_undo_log_dedup() -> TestResult {
    let mut undo = Box::new(UndoLog::new());
    undo.current_generation = 1;

    let mut v: u64 = 1;
    undo.record(&mut v as *mut u64, v);
    v = 2;
    undo.record(&mut v as *mut u64, v);
    v = 3;
    undo.record(&mut v as *mut u64, v);

    check!(undo.count <= 3, "dedup should limit entries");
    TestResult::Pass
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
fn test_undo_log_rollback() -> TestResult {
    let mut undo = Box::new(UndoLog::new());
    undo.current_generation = 1;

    let mut v: u64 = 42;
    undo.record(&mut v as *mut u64, v);
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::ptr::write_volatile(&mut v, 99);
    }

    check!(undo.count == 1, "should have 1 entry");

    let rolled = undo.rollback_to(0);
    check!(rolled == 1, "should have rolled back 1 entry");
    check!(
        undo.count == 0,
        "undo log should be empty after full rollback"
    );
    TestResult::Pass
}

fn test_domain_create() -> TestResult {
    let dom = RecoveryDomain::new(6);
    check!(dom.id == 6, "domain id mismatch");
    TestResult::Pass
}

fn test_domain_freg_push() -> TestResult {
    let dom = RecoveryDomain::new(7);
    dom.freg_generation
        .store(3, core::sync::atomic::Ordering::SeqCst);
    dom.push_freg_snapshot(100);

    let top = dom
        .freg_stack_top
        .load(core::sync::atomic::Ordering::SeqCst) as usize;
    check!(top == 1, "freg stack top should be 1");
    TestResult::Pass
}

fn test_domain_dependency() -> TestResult {
    let dom = RecoveryDomain::new(9);
    check!(dom.add_dependency(2), "add_dependency failed");
    check!(dom.depends_on_id(2), "depends_on_id should return true");
    check!(dom.dependency_count() == 1, "dependency_count should be 1");
    check!(!dom.depends_on_id(99), "should not depend on 99");
    TestResult::Pass
}

pub fn register_freg_tests() {
    let r = runner();
    register_tests_inner! { r:
        "freg::undo_log": {
            "basic": test_undo_log_basic,
            "record": test_undo_log_record,
            "dedup": test_undo_log_dedup,
            "rollback": test_undo_log_rollback,
        },
        "freg::domain": {
            "create": test_domain_create,
            "freg_push": test_domain_freg_push,
            "dependency": test_domain_dependency,
        },
    }
}
