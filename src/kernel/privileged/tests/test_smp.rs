use crate::privileged::tests::{TestResult, assert_eq_test, check, runner};
use crate::register_tests_inner;
// E-04 (2026-09-06): Ordering 仅被 per_cpu_sched::init 原实现 (not(host-test) 分支) 使用,
// host-test 下该测试为 Skip 占位, 门控 import 避免 unused warning.
#[cfg(not(feature = "host-test"))]
use core::sync::atomic::Ordering;

// ============================================================
// SMP — Per-CPU Count & Online
// ============================================================

fn test_smp_cpu_count_positive() -> TestResult {
    let count = crate::privileged::smp::get_cpu_count();
    check!(count >= 1, "cpu_count >= 1");
    check!(count <= 64, "cpu_count <= 64 (sane upper bound)");
    TestResult::Pass
}

fn test_smp_current_cpu_valid() -> TestResult {
    let cpu = crate::privileged::smp::get_current_cpu();
    let count = crate::privileged::smp::get_cpu_count();
    check!(cpu < count, "current CPU index within range");
    TestResult::Pass
}

// E-04 (2026-09-06): 测试运行器双端适配 — host-test 下 SMP 子系统未初始化,
// is_cpu_online(0) 恒 false (BSP online 标记由裸机启动路径设置), 必然 FAIL
// → 直接 Skip. kernel_test (QEMU) 下走下方原实现, 行为不变.
#[cfg(feature = "host-test")]
fn test_smp_cpu_online() -> TestResult {
    TestResult::Skip("E-04: host 无 SMP 初始化, 跳过 (BSP online 标记由裸机启动设置)")
}

#[cfg(not(feature = "host-test"))]
fn test_smp_cpu_online() -> TestResult {
    let cpu = crate::privileged::smp::get_current_cpu();
    let online = crate::privileged::smp::is_cpu_online(cpu);
    check!(online, "BSP (cpu 0) must be online");
    TestResult::Pass
}

// ============================================================
// Per-CPU Scheduler — Initialization
// ============================================================

// E-04 (2026-09-06): 测试运行器双端适配 — host-test 下调度器未初始化
// (SCHEDULER_READY 由裸机启动路径 scheduler::init() 置位), 必然 FAIL
// → 直接 Skip. kernel_test (QEMU) 下走下方原实现, 行为不变.
#[cfg(feature = "host-test")]
fn test_per_cpu_sched_init() -> TestResult {
    TestResult::Skip("E-04: host 无调度器初始化, 跳过 (SCHEDULER_READY 由裸机启动置位)")
}

#[cfg(not(feature = "host-test"))]
fn test_per_cpu_sched_init() -> TestResult {
    use crate::privileged::proc::SCHEDULER_READY;
    check!(
        SCHEDULER_READY.load(Ordering::Acquire),
        "scheduler initialized"
    );
    TestResult::Pass
}

fn test_per_cpu_current_valid() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    let _current = SCHEDULER.current();
    TestResult::Pass
}

fn test_per_cpu_has_runnable() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    let _runnable = SCHEDULER.has_any_runnable();
    TestResult::Pass
}

fn test_per_cpu_rt_count() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    let count = SCHEDULER.get_rt_count();
    check!(count <= 256, "RT task count bounded");
    TestResult::Pass
}

// ============================================================
// MLFQ — 算法不变量
// ============================================================

fn test_sched_policy_from_u32() -> TestResult {
    use crate::privileged::proc::SchedPolicy;

    assert_eq_test!(SchedPolicy::from_u32(0), SchedPolicy::Normal, "0 → Normal");
    assert_eq_test!(SchedPolicy::from_u32(1), SchedPolicy::Fifo, "1 → Fifo");
    assert_eq_test!(SchedPolicy::from_u32(2), SchedPolicy::Rr, "2 → Rr");
    assert_eq_test!(SchedPolicy::from_u32(3), SchedPolicy::Idle, "3 → Idle");
    assert_eq_test!(
        SchedPolicy::from_u32(255),
        SchedPolicy::Normal,
        "255 → Normal fallback"
    );
    TestResult::Pass
}

fn test_sched_policy_discriminant() -> TestResult {
    use crate::privileged::proc::SchedPolicy;
    check!(SchedPolicy::Normal as u32 == 0, "Normal=0");
    check!(SchedPolicy::Fifo as u32 == 1, "Fifo=1");
    check!(SchedPolicy::Rr as u32 == 2, "Rr=2");
    check!(SchedPolicy::Idle as u32 == 3, "Idle=3");
    TestResult::Pass
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn test_sched_quota_operations() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    let test_pwm: u64 = 0xDEAD0000;
    SCHEDULER.set_quota(test_pwm, 100_000_000, 1_000_000_000);
    SCHEDULER.remove_quota(test_pwm);
    TestResult::Pass
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn test_sched_limit_init() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    SCHEDULER.set_limit(0x100001, 5);
    SCHEDULER.remove_quota(0x100001);
    TestResult::Pass
}

// ============================================================
// RT 调度 — 策略切换
// ============================================================

fn test_rt_policy_switching_self() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    use crate::privileged::proc::SchedPolicy;

    let pid = SCHEDULER.current().unwrap_or(0);
    if pid == 0 {
        return TestResult::Skip("no current process to switch policy");
    }

    let result = SCHEDULER.set_sched_policy(pid, SchedPolicy::Fifo, 50);
    check!(result, "set_sched_policy on current should succeed");

    let result = SCHEDULER.set_sched_policy(pid, SchedPolicy::Rr, 30);
    check!(result, "switch Fifo → Rr should succeed");

    let result = SCHEDULER.set_sched_policy(pid, SchedPolicy::Normal, 0);
    check!(result, "switch Rr → Normal should succeed");
    TestResult::Pass
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn test_rt_invalid_pid() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    use crate::privileged::proc::SchedPolicy;

    let result = SCHEDULER.set_sched_policy(0xFFFFFFFF, SchedPolicy::Fifo, 50);
    check!(!result, "invalid PID must fail");
    TestResult::Pass
}

// ============================================================
// 负载均衡 — 基础
// ============================================================

fn test_load_balance_no_panic() -> TestResult {
    use crate::privileged::proc::SCHEDULER;
    SCHEDULER.load_balance();
    TestResult::Pass
}

// ============================================================
// 进程退出 — CR3 安全性 (回归测试)
// ============================================================

// E-04 (2026-09-06): 测试运行器双端适配 — host-test 下 VMM 未初始化,
// get_kernel_pml4() 恒 0, 必然 FAIL → 直接 Skip.
// kernel_test (QEMU) 下走下方原实现, 行为不变.
#[cfg(feature = "host-test")]
fn test_kernel_pml4_exists() -> TestResult {
    TestResult::Skip("E-04: host 无 VMM 初始化, 跳过 (内核页表由裸机 VMM 初始化)")
}

#[cfg(not(feature = "host-test"))]
fn test_kernel_pml4_exists() -> TestResult {
    let kpml4 = crate::privileged::mm::vmm::get_kernel_pml4();
    check!(kpml4 != 0, "kernel PML4 is non-zero");
    TestResult::Pass
}

fn test_kernel_pml4_stable() -> TestResult {
    let k1 = crate::privileged::mm::vmm::get_kernel_pml4();
    let k2 = crate::privileged::mm::vmm::get_kernel_pml4();
    check!(k1 == k2, "kernel PML4 is stable across calls");
    TestResult::Pass
}

fn test_user_proc_manager_destroy_no_kstack() -> TestResult {
    use crate::privileged::proc::user_proc::USER_PROC_MANAGER;

    // 2026-07-02 分析: 本测试验证 USER_PROC_MANAGER.destroy_by_pid_no_kstack() 的
    // 基本契约 — 对不存在的 PID 调用应无副作用 (不 panic).
    //
    // 无法测试真实用户进程销毁路径 (需要完整调度器 + 页表 + 内核栈上下文),
    // 该路径由 host-tests 集成测试覆盖. kernel_test 模式下仅验证 API 可安全调用.
    //
    // 使用 PID = 99999 (确保不存在), 验证函数正常返回 (不 panic).
    USER_PROC_MANAGER.destroy_by_pid_no_kstack(99999);
    TestResult::Pass
}

// ============================================================
// Softirq — 注册与向量 (Registration & Vector)
// ============================================================

fn test_softirq_vec_enum_values() -> TestResult {
    use crate::privileged::irq::SoftirqVec;
    check!(SoftirqVec::High.to_idx() == 0, "High=0");
    check!(SoftirqVec::Timer.to_idx() == 1, "Timer=1");
    check!(SoftirqVec::NetRx.to_idx() == 2, "NetRx=2");
    check!(SoftirqVec::NetTx.to_idx() == 3, "NetTx=3");
    check!(SoftirqVec::Block.to_idx() == 4, "Block=4");
    check!(SoftirqVec::Tasklet.to_idx() == 5, "Tasklet=5");
    check!(SoftirqVec::Sched.to_idx() == 6, "Sched=6");
    TestResult::Pass
}

fn test_softirq_from_u8() -> TestResult {
    use crate::privileged::irq::SoftirqVec;
    check!(SoftirqVec::from_u8(0) == Some(SoftirqVec::High), "0→High");
    check!(SoftirqVec::from_u8(1) == Some(SoftirqVec::Timer), "1→Timer");
    check!(
        SoftirqVec::from_u8(5) == Some(SoftirqVec::Tasklet),
        "5→Tasklet"
    );
    check!(SoftirqVec::from_u8(255).is_none(), "255→None");
    TestResult::Pass
}

fn test_softirq_not_initially_in() -> TestResult {
    let in_softirq = crate::privileged::irq::in_softirq();
    check!(!in_softirq, "not in softirq context at test start");
    TestResult::Pass
}

fn test_softirq_pending_initially_zero() -> TestResult {
    let pending = crate::privileged::irq::pending_softirq();
    check!(!pending, "no pending softirqs at test start");
    TestResult::Pass
}

fn test_softirq_raise_then_check() -> TestResult {
    use crate::privileged::irq::SoftirqVec;

    crate::privileged::irq::open_softirq(SoftirqVec::Tasklet, || {});
    crate::privileged::irq::raise_softirq(SoftirqVec::Tasklet);

    let pending = crate::privileged::irq::pending_softirq();
    check!(pending, "softirq should be pending after raise");

    crate::privileged::irq::do_softirq();
    TestResult::Pass
}

fn test_softirq_mask_raise() -> TestResult {
    let mask: u64 = (1u64 << crate::privileged::irq::SoftirqVec::Timer.to_idx())
        | (1u64 << crate::privileged::irq::SoftirqVec::NetRx.to_idx());

    crate::privileged::irq::raise_softirq_mask(mask);

    let pending = crate::privileged::irq::pending_softirq();
    check!(pending, "mask-raised softirqs should be pending");

    crate::privileged::irq::do_softirq();
    TestResult::Pass
}

// ============================================================
// Test Registration
// ============================================================

pub fn register_smp_tests() {
    let r = runner();
    register_tests_inner! { r:
        "smp": {
            "cpu_count_positive": test_smp_cpu_count_positive,
            "current_cpu_valid": test_smp_current_cpu_valid,
            "cpu_online": test_smp_cpu_online,
        },
        "per_cpu_sched": {
            "init": test_per_cpu_sched_init,
            "current_valid": test_per_cpu_current_valid,
            "has_runnable": test_per_cpu_has_runnable,
            "rt_count_init": test_per_cpu_rt_count,
        },
        "sched_policy": {
            "from_u32": test_sched_policy_from_u32,
            "discriminant": test_sched_policy_discriminant,
        },
        "sched_quota": {
            "set_operations": test_sched_quota_operations,
        },
        "sched_limit": {
            "set_init": test_sched_limit_init,
        },
        "rt_sched": {
            "policy_switching_self": test_rt_policy_switching_self,
            "invalid_pid": test_rt_invalid_pid,
        },
        "load_balance": {
            "no_panic": test_load_balance_no_panic,
        },
        "proc_exit": {
            "kernel_pml4_exists": test_kernel_pml4_exists,
            "kernel_pml4_stable": test_kernel_pml4_stable,
            "destroy_no_kstack": test_user_proc_manager_destroy_no_kstack,
        },
        "softirq": {
            "vec_enum_values": test_softirq_vec_enum_values,
            "from_u8": test_softirq_from_u8,
            "not_initially_in": test_softirq_not_initially_in,
            "pending_initially_zero": test_softirq_pending_initially_zero,
            "raise_then_check": test_softirq_raise_then_check,
            "mask_raise": test_softirq_mask_raise,
        },
    }
}
