// SPDX-License-Identifier: MPL-2.0
//! E-06 调度 syscall 面回归锁 — ABI 单源 / 策略映射 / 属性与请求切片链路.
//!
//! 覆盖:
//! - `edgine-abi::sched` 结构布局与常量, 内核经 privileged 出口消费同一类型;
//! - `sched_getscheduler` 把内部策略映射为 Linux ABI 值 (含 `Idle→5` / `Deadline→6`);
//! - `sched_setscheduler` 只接受 `SCHED_OTHER` + 自身 pid, 其余返回码逐项锁定;
//! - `sched_setattr` 的 nice 经 `Scheduler::set_nice` 生效, 请求切片按 D8③
//!   夹取后在同轮 rq 账目重建;
//! - `sched_getattr` 回读 nice / 请求切片, 容量 >= VER0 时只写 48 字节;
//! - dispatch 表已接线 (命中 functions 分支, 不回退 ENOSYS);
//! - functions 层不复制调度公式.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use edgine::kernel::functions::proc::sched_attr::{
    sched_getattr_syscall, sched_getscheduler_syscall, sched_setattr_syscall,
    sched_setscheduler_syscall,
};
use edgine::kernel::functions::syscall::FunctionsSyscallDispatch;
use edgine::kernel::functions::syscall::types as ktypes;
use edgine::kernel::privileged::proc::cfs::nice_to_weight;
use edgine::kernel::privileged::proc::process::{PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::{BlockReason, SchedPolicy, Scheduler};
use edgine::kernel::privileged::syscall::{Errno, SyscallDispatch};
use edgine_abi::sched as abi;
use edgine_host_tests::eevdf_model::{
    VIRT_SHIFT, clamp_requested_slice, default_slice_real, slice_virt_of,
};

/// 在队路径会写全局 per-CPU rq 与 current, 文件内串行化.
static RQ_LOCK: Mutex<()> = Mutex::new(());

/// 宿主进程句柄: 真实 `Process::new` + 全局 `PROCESS_TABLE`, Drop 时回收.
struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new(name: &str) -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, name, None));
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Self { pid }
    }

    fn with<R>(&self, f: impl FnOnce(&Process) -> R) -> R {
        PROCESS_TABLE
            .with_process(self.pid, f)
            .expect("测试进程应在表中")
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut Process) -> R) -> R {
        PROCESS_TABLE
            .with_process_mut(self.pid, f)
            .expect("测试进程应在表中")
    }

    fn nice(&self) -> i8 {
        self.with(Process::get_nice)
    }

    fn weight(&self) -> u64 {
        self.with(|p| p.cfs_weight.load(Ordering::Acquire))
    }

    fn requested(&self) -> u64 {
        self.with(|p| p.eevdf_requested_slice.load(Ordering::Acquire))
    }

    fn eevdf(&self) -> [u64; 4] {
        self.with(|p| {
            [
                p.eevdf_slice_real.load(Ordering::Acquire),
                p.eevdf_slice_virt.load(Ordering::Acquire),
                p.eevdf_slice_left.load(Ordering::Acquire),
                p.eevdf_vdeadline.load(Ordering::Acquire),
            ]
        })
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

/// 在队测试清理: 把任务撤出全局运行集并复位 current.
struct RqGuard;

impl RqGuard {
    fn enqueue(pid: u32) -> Self {
        Scheduler::add_to_run_queue(pid);
        Scheduler::set_current(pid);
        Self
    }
}

impl Drop for RqGuard {
    fn drop(&mut self) {
        Scheduler::block(BlockReason::Unknown);
        Scheduler::set_current(0);
    }
}

fn lock_rq() -> std::sync::MutexGuard<'static, ()> {
    RQ_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 取一个已释放的 pid, 用于"目标不存在 → ESRCH"断言.
fn free_pid() -> u32 {
    let pid = PROCESS_TABLE
        .allocate_pid()
        .expect("PROCESS_TABLE.allocate_pid 失败");
    PROCESS_TABLE.free_pid(pid);
    pid
}

fn pid_i32(pid: u32) -> i32 {
    i32::try_from(pid).expect("测试 pid 应可由 i32 表示")
}

fn user_ptr<T>(value: &mut T) -> u64 {
    &raw mut *value as u64
}

/// 合法 VER0 属性: `SCHED_OTHER` / nice 0 / 无请求切片.
fn valid_attr() -> ktypes::SchedAttr {
    ktypes::SchedAttr {
        size: ktypes::SCHED_ATTR_SIZE_VER0,
        sched_policy: ktypes::SCHED_OTHER,
        ..ktypes::SchedAttr::default()
    }
}

/// 经 syscall 入口写自身属性 (隐式 pid 0 = 当前进程).
fn set_self(attr: &mut ktypes::SchedAttr, flags: u32) -> i64 {
    sched_setattr_syscall(0, user_ptr(attr), flags)
}

fn get_pid(pid: u32, attr: &mut ktypes::SchedAttr, size: u32) -> i64 {
    sched_getattr_syscall(pid_i32(pid), user_ptr(attr), size, 0)
}

/// 单字段非法: 保留其余合法字段, 断言返回 EINVAL.
fn expect_einval(label: &str, attr: &mut ktypes::SchedAttr) {
    assert_eq!(
        set_self(attr, 0),
        Errno::Einval.as_ret(),
        "{label} 必须返回 EINVAL"
    );
}

/// ABI 布局与常量必须来自 `edgine-abi` 单源, 且内核出口是同一类型.
#[test]
fn abi_layout_and_constants_are_single_source() {
    assert_eq!(std::mem::size_of::<abi::SchedAttr>(), 48);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, size), 0);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, sched_policy), 4);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, sched_flags), 8);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, sched_nice), 16);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, sched_priority), 20);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, sched_runtime), 24);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, sched_deadline), 32);
    assert_eq!(std::mem::offset_of!(abi::SchedAttr, sched_period), 40);
    assert_eq!(std::mem::size_of::<abi::SchedParam>(), 4);
    assert_eq!(std::mem::offset_of!(abi::SchedParam, sched_priority), 0);

    assert_eq!(abi::SCHED_OTHER, 0);
    assert_eq!(abi::SCHED_FIFO, 1);
    assert_eq!(abi::SCHED_RR, 2);
    assert_eq!(abi::SCHED_BATCH, 3);
    assert_eq!(abi::SCHED_IDLE, 5);
    assert_eq!(abi::SCHED_DEADLINE, 6);
    assert_eq!(abi::SCHED_ATTR_SIZE_VER0, 48);
    assert_eq!(abi::MIN_NICE, -20);
    assert_eq!(abi::MAX_NICE, 19);

    // 内核出口就是 edgine-abi 的类型/值, 不是第二份定义.
    let kernel_attr: ktypes::SchedAttr = abi::SchedAttr::default();
    assert_eq!(std::mem::size_of_val(&kernel_attr), 48);
    assert_eq!(ktypes::SCHED_ATTR_SIZE_VER0, abi::SCHED_ATTR_SIZE_VER0);
    assert_eq!(ktypes::SCHED_OTHER, abi::SCHED_OTHER);
    assert_eq!(ktypes::SYS_SCHED_SETSCHEDULER, abi::SYS_SCHED_SETSCHEDULER);
    assert_eq!(ktypes::SYS_SCHED_GETSCHEDULER, abi::SYS_SCHED_GETSCHEDULER);
    assert_eq!(ktypes::SYS_SCHED_SETATTR, abi::SYS_SCHED_SETATTR);
    assert_eq!(ktypes::SYS_SCHED_GETATTR, abi::SYS_SCHED_GETATTR);
    assert_eq!(ktypes::SYS_SCHED_SETSCHEDULER, 144);
    assert_eq!(ktypes::SYS_SCHED_GETSCHEDULER, 145);
    assert_eq!(ktypes::SYS_SCHED_SETATTR, 314);
    assert_eq!(ktypes::SYS_SCHED_GETATTR, 315);
}

/// 内部策略值域与 Linux ABI 不同, 查询面必须显式映射而不是原样返回.
#[test]
fn getscheduler_maps_internal_policy_to_linux_abi() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-getsched");
    let cases = [
        (SchedPolicy::Normal, abi::SCHED_OTHER),
        (SchedPolicy::Fifo, abi::SCHED_FIFO),
        (SchedPolicy::Rr, abi::SCHED_RR),
        (SchedPolicy::Idle, abi::SCHED_IDLE),
        (SchedPolicy::Deadline, abi::SCHED_DEADLINE),
    ];
    for (policy, abi_value) in cases {
        assert!(Scheduler::set_sched_policy(proc.pid, policy, 0));
        assert_eq!(
            sched_getscheduler_syscall(pid_i32(proc.pid)),
            i64::from(abi_value),
            "内部策略 {policy:?} 的 ABI 映射错误"
        );
    }

    assert_eq!(
        sched_getscheduler_syscall(-1),
        Errno::Einval.as_ret(),
        "负 pid 返回 EINVAL"
    );
    assert_eq!(
        sched_getscheduler_syscall(pid_i32(free_pid())),
        Errno::Esrch.as_ret(),
        "不存在的目标返回 ESRCH"
    );

    Scheduler::set_current(proc.pid);
    assert!(Scheduler::set_sched_policy(
        proc.pid,
        SchedPolicy::Normal,
        0
    ));
    assert_eq!(
        sched_getscheduler_syscall(0),
        i64::from(abi::SCHED_OTHER),
        "pid 0 查询当前进程"
    );
    Scheduler::set_current(0);
    assert_eq!(
        sched_getscheduler_syscall(0),
        Errno::Esrch.as_ret(),
        "无当前进程时 pid 0 返回 ESRCH"
    );
}

/// `sched_setscheduler` 对自身 + `SCHED_OTHER` 是幂等成功; 参数错误分别返回 EFAULT/EINVAL.
#[test]
fn setscheduler_accepts_sched_other_for_self() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-setsched-ok");
    Scheduler::set_current(proc.pid);
    let mut param = Box::new(ktypes::SchedParam::default());

    assert_eq!(
        sched_setscheduler_syscall(0, abi::SCHED_OTHER, user_ptr(&mut *param)),
        0,
        "隐式 pid 0 = 当前进程"
    );
    assert_eq!(
        sched_setscheduler_syscall(pid_i32(proc.pid), abi::SCHED_OTHER, user_ptr(&mut *param)),
        0,
        "显式自身 pid"
    );

    let mut nonzero = Box::new(ktypes::SchedParam { sched_priority: 1 });
    assert_eq!(
        sched_setscheduler_syscall(0, abi::SCHED_OTHER, user_ptr(&mut *nonzero)),
        Errno::Einval.as_ret(),
        "SCHED_OTHER 的实时优先级必须为 0"
    );
    assert_eq!(
        sched_setscheduler_syscall(0, abi::SCHED_OTHER, 0),
        Errno::Efault.as_ret(),
        "空参数指针返回 EFAULT"
    );
    Scheduler::set_current(0);
}

/// 非 `SCHED_OTHER` 策略、跨进程修改与目标不存在必须分别返回 EINVAL/EPERM/ESRCH.
#[test]
fn setscheduler_rejects_unsupported_policies_and_foreign_pids() {
    let _serial = lock_rq();
    let self_proc = TestProc::new("e06-setsched-self");
    let foreign = TestProc::new("e06-setsched-foreign");
    Scheduler::set_current(self_proc.pid);
    let mut param = Box::new(ktypes::SchedParam::default());
    let ptr = user_ptr(&mut *param);

    for policy in [
        abi::SCHED_FIFO,
        abi::SCHED_RR,
        abi::SCHED_BATCH,
        abi::SCHED_IDLE,
        abi::SCHED_DEADLINE,
        abi::SCHED_OTHER | abi::SCHED_RESET_ON_FORK,
    ] {
        assert_eq!(
            sched_setscheduler_syscall(0, policy, ptr),
            Errno::Einval.as_ret(),
            "policy {policy:#x} 必须返回 EINVAL 而非静默忽略"
        );
    }
    assert_eq!(
        sched_setscheduler_syscall(pid_i32(foreign.pid), abi::SCHED_OTHER, ptr),
        Errno::Eperm.as_ret(),
        "修改其它进程需要 EPERM (当前无 CAP_SYS_NICE 入口)"
    );
    assert_eq!(
        sched_setscheduler_syscall(-2, abi::SCHED_OTHER, ptr),
        Errno::Einval.as_ret(),
        "负 pid 返回 EINVAL"
    );
    assert_eq!(
        sched_setscheduler_syscall(pid_i32(free_pid()), abi::SCHED_OTHER, ptr),
        Errno::Esrch.as_ret(),
        "目标不存在返回 ESRCH"
    );
    assert_eq!(
        sched_setscheduler_syscall(0, abi::SCHED_FIFO, 0),
        Errno::Einval.as_ret(),
        "策略检查先于参数指针读取"
    );

    assert!(Scheduler::set_sched_policy(
        self_proc.pid,
        SchedPolicy::Fifo,
        50
    ));
    assert_eq!(
        sched_setscheduler_syscall(0, abi::SCHED_OTHER, ptr),
        Errno::Einval.as_ret(),
        "内部 RT 策略没有用户态迁移路径"
    );
    Scheduler::set_current(0);
}

/// 在队任务: nice 与请求切片同时生效, 切片/vd 在同一 rq 锁内按新权重重建.
#[test]
fn setattr_applies_nice_and_clamps_requested_slice() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-setattr-rq");
    let vr = 100u64 << VIRT_SHIFT;
    proc.with_mut(|p| p.cfs_vruntime.store(vr, Ordering::Release));
    let _rq = RqGuard::enqueue(proc.pid);

    let mut attr = Box::new(valid_attr());
    attr.sched_nice = 10;
    attr.sched_runtime = 10_000_000; // 10 ms = 10 tick
    assert_eq!(set_self(&mut attr, 0), 0);

    let weight = nice_to_weight(10);
    let slice_real = 10;
    let slice_virt = slice_virt_of(slice_real, weight);
    assert_eq!(proc.nice(), 10);
    assert_eq!(proc.weight(), weight);
    assert_eq!(proc.requested(), 10);
    assert_eq!(
        proc.eevdf(),
        [slice_real, slice_virt, slice_real, vr + slice_virt],
        "setattr 必须在同一 rq 锁内重建 slice_real/slice_virt/slice_left/vd"
    );

    // D8③ 下界: 1 tick 请求夹取到 MIN_GRANULARITY.
    attr.sched_runtime = 1_000_000;
    assert_eq!(set_self(&mut attr, 0), 0);
    let lower = clamp_requested_slice(1);
    assert_eq!(proc.requested(), lower);
    assert_eq!(proc.eevdf()[0], lower);
    assert_eq!(proc.eevdf()[2], lower);

    // D8③ 上界: 100 tick 请求夹取到 TARGET_LATENCY.
    attr.sched_runtime = 100_000_000;
    assert_eq!(set_self(&mut attr, 0), 0);
    let upper = clamp_requested_slice(100);
    assert_eq!(proc.requested(), upper);
    assert_eq!(proc.eevdf()[0], upper);
    assert_eq!(proc.eevdf()[2], upper);

    // 0 = 清除请求, 回到动态默认切片.
    attr.sched_runtime = 0;
    assert_eq!(set_self(&mut attr, 0), 0);
    let default = default_slice_real(weight, weight);
    assert_eq!(proc.requested(), 0);
    assert_eq!(proc.eevdf()[0], default);
    assert_eq!(proc.eevdf()[2], default);
    assert_eq!(proc.eevdf()[3], vr + slice_virt_of(default, weight));
}

/// 不在可运行集: 只记录属性, 不建本轮账目, 归属保持 `NO_RQ_CPU`.
#[test]
fn setattr_off_rq_only_records_attributes() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-setattr-offrq");
    Scheduler::set_current(proc.pid);
    let mut attr = Box::new(valid_attr());
    attr.sched_nice = -5;
    attr.sched_runtime = 30_000_000;
    assert_eq!(set_self(&mut attr, 0), 0);

    assert_eq!(proc.nice(), -5);
    assert_eq!(proc.weight(), nice_to_weight(-5));
    assert_eq!(proc.requested(), 30);
    assert_eq!(proc.eevdf(), [0, 0, 0, 0], "未入队不得凭空建账");
    Scheduler::set_current(0);
}

/// `sched_setattr` 未支持字段必须显式 EINVAL, 不静默忽略.
#[test]
fn setattr_rejects_unsupported_fields() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-setattr-fields");
    Scheduler::set_current(proc.pid);

    let baseline = valid_attr();

    let mut attr = baseline;
    attr.size = 47;
    expect_einval("size=47", &mut attr);
    let mut attr = baseline;
    attr.size = 49;
    expect_einval("size=49", &mut attr);
    let mut attr = baseline;
    attr.sched_policy = abi::SCHED_FIFO;
    expect_einval("policy=FIFO", &mut attr);
    let mut attr = baseline;
    attr.sched_policy = abi::SCHED_DEADLINE;
    expect_einval("policy=DEADLINE", &mut attr);
    let mut attr = baseline;
    attr.sched_flags = abi::SCHED_FLAG_RESET_ON_FORK;
    expect_einval("flags=RESET_ON_FORK", &mut attr);
    let mut attr = baseline;
    attr.sched_flags = abi::SCHED_FLAG_UTIL_CLAMP_MAX;
    expect_einval("flags=UTIL_CLAMP_MAX", &mut attr);
    let mut attr = baseline;
    attr.sched_priority = 1;
    expect_einval("priority=1", &mut attr);
    let mut attr = baseline;
    attr.sched_deadline = 1;
    expect_einval("deadline=1", &mut attr);
    let mut attr = baseline;
    attr.sched_period = 1;
    expect_einval("period=1", &mut attr);
    let mut attr = baseline;
    attr.sched_nice = -21;
    expect_einval("nice=-21", &mut attr);
    let mut attr = baseline;
    attr.sched_nice = 20;
    expect_einval("nice=20", &mut attr);

    let mut valid = baseline;
    assert_eq!(
        set_self(&mut valid, 1),
        Errno::Einval.as_ret(),
        "syscall flags 参数非 0 返回 EINVAL"
    );
    Scheduler::set_current(0);
}

/// 目标解析与权限顺序: EFAULT (指针) / EINVAL (负 pid) / ESRCH (不存在) / EPERM (跨进程).
#[test]
fn setattr_rejects_target_errors() {
    let _serial = lock_rq();
    let self_proc = TestProc::new("e06-setattr-self");
    let foreign = TestProc::new("e06-setattr-foreign");
    Scheduler::set_current(self_proc.pid);
    let mut attr = Box::new(valid_attr());

    assert_eq!(
        sched_setattr_syscall(0, 0, 0),
        Errno::Efault.as_ret(),
        "空属性指针返回 EFAULT"
    );
    assert_eq!(
        sched_setattr_syscall(-3, user_ptr(&mut *attr), 0),
        Errno::Einval.as_ret(),
        "负 pid 返回 EINVAL"
    );
    assert_eq!(
        sched_setattr_syscall(pid_i32(free_pid()), user_ptr(&mut *attr), 0),
        Errno::Esrch.as_ret(),
        "目标不存在返回 ESRCH"
    );
    assert_eq!(
        sched_setattr_syscall(pid_i32(foreign.pid), user_ptr(&mut *attr), 0),
        Errno::Eperm.as_ret(),
        "修改其它进程返回 EPERM"
    );
    Scheduler::set_current(0);
}

/// 回读: nice / 请求切片往返一致; 结构容量 >= VER0 时只回写 48 字节.
#[test]
fn getattr_round_trips_nice_and_requested_slice() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-getattr");
    Scheduler::set_current(proc.pid);

    let mut attr = Box::new(ktypes::SchedAttr::default());
    assert_eq!(get_pid(proc.pid, &mut attr, 48), 0);
    assert_eq!(attr.size, 48);
    assert_eq!(attr.sched_policy, abi::SCHED_OTHER);
    assert_eq!(attr.sched_nice, 0);
    assert_eq!(attr.sched_priority, 0);
    assert_eq!(attr.sched_runtime, 0);

    let mut set = Box::new(valid_attr());
    set.sched_nice = -5;
    set.sched_runtime = 20_000_000;
    assert_eq!(set_self(&mut set, 0), 0);

    *attr = ktypes::SchedAttr::default();
    assert_eq!(get_pid(proc.pid, &mut attr, 64), 0, "更大容量应被接受");
    assert_eq!(attr.size, 48, "只回写 VER0 尺寸");
    assert_eq!(attr.sched_nice, -5);
    assert_eq!(attr.sched_runtime, 20_000_000);
    assert_eq!(attr.sched_flags, 0);
    assert_eq!(attr.sched_deadline, 0);
    assert_eq!(attr.sched_period, 0);

    let mut current = Box::new(ktypes::SchedAttr::default());
    assert_eq!(sched_getattr_syscall(0, user_ptr(&mut *current), 48, 0), 0);
    assert_eq!(current.sched_nice, -5, "pid 0 回读当前进程");
    Scheduler::set_current(0);
}

/// `sched_getattr` 参数与目标错误; 查询其它进程不设 self-only 限制.
#[test]
fn getattr_validation_and_cross_process_query() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-getattr-errors");
    let foreign = TestProc::new("e06-getattr-foreign");
    assert!(Scheduler::set_nice(foreign.pid, 3));
    let mut attr = Box::new(ktypes::SchedAttr::default());

    assert_eq!(
        sched_getattr_syscall(pid_i32(proc.pid), user_ptr(&mut *attr), 47, 0),
        Errno::Einval.as_ret(),
        "容量 < VER0"
    );
    assert_eq!(
        sched_getattr_syscall(pid_i32(proc.pid), user_ptr(&mut *attr), 48, 1),
        Errno::Einval.as_ret(),
        "flags 非 0"
    );
    assert_eq!(
        sched_getattr_syscall(-1, user_ptr(&mut *attr), 48, 0),
        Errno::Einval.as_ret(),
        "负 pid"
    );
    assert_eq!(
        sched_getattr_syscall(pid_i32(free_pid()), user_ptr(&mut *attr), 48, 0),
        Errno::Esrch.as_ret(),
        "目标不存在"
    );
    assert_eq!(
        sched_getattr_syscall(pid_i32(proc.pid), 0, 48, 0),
        Errno::Efault.as_ret(),
        "空指针"
    );
    assert_eq!(
        sched_getattr_syscall(0, user_ptr(&mut *attr), 48, 0),
        Errno::Esrch.as_ret(),
        "无当前进程时 pid 0 返回 ESRCH"
    );

    assert_eq!(get_pid(foreign.pid, &mut attr, 48), 0);
    assert_eq!(attr.sched_nice, 3, "查询其它进程不受 self-only 限制");
}

/// dispatch 表接线: 命中 functions 分支, 不回退 ENOSYS.
#[test]
fn dispatch_routes_sched_syscalls_to_functions_layer() {
    let _serial = lock_rq();
    let proc = TestProc::new("e06-dispatch");
    Scheduler::set_current(proc.pid);

    assert_eq!(
        FunctionsSyscallDispatch.dispatch(
            ktypes::SYS_SCHED_GETSCHEDULER,
            [u64::from(proc.pid), 0, 0, 0, 0, 0],
        ),
        i64::from(abi::SCHED_OTHER)
    );
    let mut param = Box::new(ktypes::SchedParam::default());
    assert_eq!(
        FunctionsSyscallDispatch.dispatch(
            ktypes::SYS_SCHED_SETSCHEDULER,
            [
                0,
                u64::from(abi::SCHED_OTHER),
                user_ptr(&mut *param),
                0,
                0,
                0
            ],
        ),
        0
    );
    let mut attr = Box::new(ktypes::SchedAttr::default());
    assert_eq!(
        FunctionsSyscallDispatch.dispatch(
            ktypes::SYS_SCHED_GETATTR,
            [0, user_ptr(&mut *attr), 48, 0, 0, 0],
        ),
        0
    );
    Scheduler::set_current(0);
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("host-tests 必有父目录")
        .to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {e}", path.display()))
}

/// 源码契约: functions 层只编排 privileged 入口, 不复制权重/切片/虚拟时间公式.
#[test]
fn source_functions_layer_has_no_scheduler_formula() {
    let src = read_src("src/kernel/functions/proc/sched_attr.rs");
    for needle in [
        "nice_to_weight(",
        "calc_vruntime_delta_virt(",
        "weighted_avg_vruntime(",
        "slice_virt_of(",
        "VIRT_ONE",
    ] {
        assert!(
            !src.contains(needle),
            "functions 层不得复制调度公式: {needle}"
        );
    }
    assert!(
        src.contains("setpriority_for_pid"),
        "sched_setattr 的 nice 必须复用 setpriority 单一写路径"
    );
    assert!(
        src.contains("Scheduler::set_requested_slice"),
        "请求切片必须经 E-03 privileged 入口按 D8③ 夹取"
    );
}
