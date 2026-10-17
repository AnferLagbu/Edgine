use crate::privileged::proc::{
    SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM, SCHED_LEVEL_3_QUANTUM,
};
use crate::privileged::tests::{TestResult, assert_eq_test, runner};
use crate::register_tests_inner;

fn quantum_constants() -> TestResult {
    assert_eq_test!(SCHED_LEVEL_0_QUANTUM, 80, "level 0 quantum");
    assert_eq_test!(SCHED_LEVEL_1_QUANTUM, 60, "level 1 quantum");
    assert_eq_test!(SCHED_LEVEL_2_QUANTUM, 40, "level 2 quantum");
    assert_eq_test!(SCHED_LEVEL_3_QUANTUM, 20, "level 3 quantum");
    TestResult::Pass
}

pub fn register_tests() {
    let r = runner();
    register_tests_inner! { r:
        "sched::quantum": {
            "quantum_constants": quantum_constants,
        },
    }
}
