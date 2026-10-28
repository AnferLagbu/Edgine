use super::check;
use crate::privileged::freg::domain::RecoveryDomain;
use crate::privileged::freg::manager::RecoveryManager;
use crate::privileged::freg::recovery::order_from_edges;
use crate::privileged::freg::undo_log::UndoLog;
use crate::privileged::tests::{TestResult, runner};
use crate::register_tests_inner;
use alloc::boxed::Box;

fn test_undo_log_basic() -> TestResult {
    let undo = Box::new(UndoLog::new());
    check!(undo.count == 0, "expected empty undo log");
    TestResult::Pass
}

fn test_undo_log_record() -> TestResult {
    let mut undo = Box::new(UndoLog::new());
    undo.current_generation = 1;

    let mut value: u64 = 42;
    let old = value;
    value = 100;
    undo.record(&raw mut value, old);

    check!(undo.count == 1, "expected 1 entry after record");
    check!(undo.entries[0].generation == 1, "generation mismatch");
    TestResult::Pass
}

fn test_undo_log_dedup() -> TestResult {
    let mut undo = Box::new(UndoLog::new());
    undo.current_generation = 1;

    let mut v: u64 = 1;
    undo.record(&raw mut v, v);
    v = 2;
    undo.record(&raw mut v, v);
    v = 3;
    undo.record(&raw mut v, v);

    check!(undo.count <= 3, "dedup should limit entries");
    TestResult::Pass
}

fn test_undo_log_rollback() -> TestResult {
    let mut undo = Box::new(UndoLog::new());
    undo.current_generation = 1;

    let mut v: u64 = 42;
    undo.record(&raw mut v, v);
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::ptr::write_volatile(&raw mut v, 99);
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
    check!(dom.id() == 6, "domain id mismatch");
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

/// 线性链 A(1)→B(2)→C(3): 恢复序必须依赖优先 (C→B→A).
fn test_order_from_edges_linear() -> TestResult {
    let edges: [(u64, &[u64]); 3] = [(1, &[2]), (2, &[3]), (3, &[])];
    let order = order_from_edges(3, &edges);
    check!(order == alloc::vec![3u64, 2, 1], "linear order mismatch");
    TestResult::Pass
}

/// 菱形 A(1)→{B(2),C(3)}→D(4): D 最先, A 最后, 中间按注册序.
fn test_order_from_edges_diamond() -> TestResult {
    let edges: [(u64, &[u64]); 4] = [(1, &[2, 3]), (2, &[4]), (3, &[4]), (4, &[])];
    let order = order_from_edges(4, &edges);
    check!(
        order == alloc::vec![4u64, 2, 3, 1],
        "diamond order mismatch"
    );
    TestResult::Pass
}

/// root 自身依赖先入序; 依赖 root 的域在 root 之后.
fn test_order_from_edges_root_deps_then_dependents() -> TestResult {
    let edges: [(u64, &[u64]); 3] = [(1, &[2]), (2, &[]), (3, &[1])];
    let order = order_from_edges(1, &edges);
    check!(order == alloc::vec![2u64, 1, 3], "root deps order mismatch");
    TestResult::Pass
}

/// 间接依赖者必须进入恢复序列 (旧实现只收一层直接依赖).
fn test_order_from_edges_transitive_dependent() -> TestResult {
    // A(1)→B(2)→root(3): A 是间接依赖者, 旧实现会漏掉它.
    let edges: [(u64, &[u64]); 3] = [(1, &[2]), (2, &[3]), (3, &[])];
    let order = order_from_edges(3, &edges);
    check!(order.contains(&1), "transitive dependent must be scheduled");
    check!(
        order.len() == 3,
        "transitive dependent order length mismatch"
    );
    TestResult::Pass
}

/// 依赖成环时必须有界终止 (visited 前置位兜底), 且两域都入序.
fn test_order_from_edges_cycle_terminates() -> TestResult {
    let edges: [(u64, &[u64]); 2] = [(1, &[2]), (2, &[1])];
    let order = order_from_edges(1, &edges);
    check!(order == alloc::vec![2u64, 1], "cycle order mismatch");
    TestResult::Pass
}

/// root 未注册时无恢复对象, 返回空序.
fn test_order_from_edges_unregistered_root() -> TestResult {
    let edges: [(u64, &[u64]); 1] = [(1, &[])];
    check!(
        order_from_edges(9, &edges).is_empty(),
        "unregistered root must yield empty order"
    );
    TestResult::Pass
}

/// 同一域重复注册不得重复入表 (否则 tick/快照/回滚对同域执行两次).
fn test_manager_register_dedup() -> TestResult {
    let mut mgr = RecoveryManager::new();
    let dom: &'static RecoveryDomain = Box::leak(Box::new(RecoveryDomain::new(7)));
    check!(mgr.register(dom) == Some(7), "first register id mismatch");
    check!(
        mgr.register(dom) == Some(7),
        "duplicate register id mismatch"
    );
    check!(
        mgr.count.load(core::sync::atomic::Ordering::SeqCst) == 1,
        "duplicate register must not grow table"
    );
    TestResult::Pass
}

/// 注销采用 swap-remove: 计数递减、被移除域不可见、幸存域仍可按 id 查找.
fn test_manager_unregister_swap_remove() -> TestResult {
    let mut mgr = RecoveryManager::new();
    let a: &'static RecoveryDomain = Box::leak(Box::new(RecoveryDomain::new(7)));
    let b: &'static RecoveryDomain = Box::leak(Box::new(RecoveryDomain::new(8)));
    check!(mgr.register(a).is_some(), "register a failed");
    check!(mgr.register(b).is_some(), "register b failed");

    check!(
        mgr.unregister(7).is_some_and(|d| d.id() == 7),
        "unregister must return removed domain"
    );
    check!(
        mgr.count.load(core::sync::atomic::Ordering::SeqCst) == 1,
        "count must shrink after unregister"
    );
    check!(mgr.find(7).is_none(), "removed domain must not be found");
    check!(
        mgr.find(8).is_some_and(|d| d.id() == 8),
        "survivor must remain findable"
    );
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
        "freg::recovery": {
            "linear": test_order_from_edges_linear,
            "diamond": test_order_from_edges_diamond,
            "root_deps": test_order_from_edges_root_deps_then_dependents,
            "transitive": test_order_from_edges_transitive_dependent,
            "cycle": test_order_from_edges_cycle_terminates,
            "unregistered_root": test_order_from_edges_unregistered_root,
        },
        "freg::manager": {
            "register_dedup": test_manager_register_dedup,
            "unregister_swap_remove": test_manager_unregister_swap_remove,
        },
    }
}
