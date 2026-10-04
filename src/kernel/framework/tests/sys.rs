// UT-07 (2026-09-25): mm::slab 注册副本已删 — 其纯逻辑断言以
// framework/mm/slab.rs 的 #[cfg(test)] 为唯一归属.
use crate::framework::errno::{Errno, errno_from_i64};
use crate::framework::proc::elf::{Elf64Header, Elf64Phdr};
use crate::framework::sgeg::constant_time_eq;
use crate::framework::sgeg::secure_boot::{sha256_extend, sha256_hash};
use crate::framework::sgeg::sha256::sha256;
use crate::framework::tests::{TestResult, assert_eq_test, check, runner};
use crate::register_tests_inner;
// T1 G7: 文件系统组策略 (services 侧)
use crate::services::fs::path::{chroot_syscall, pivot_root_plan, pivot_root_syscall};
use crate::services::proc::exec::execveat_syscall;
use crate::services::proc::sysinfo::setdomainname_syscall;
// T1 G6: 时间组策略 (services 侧) — 与 framework 测试共用 runner 注册
use crate::services::timer::clock::{
    ADJ_FREQUENCY, ADJ_OFFSET, ADJ_SETOFFSET, CLOCK_MONOTONIC, CLOCK_REALTIME, TIMER_ABSTIME,
    adjtimex_syscall, apply_adjtimex, apply_settimeofday, clock_gettime_syscall,
    clock_nanosleep_syscall, clock_nanosleep_wait_ns, settimeofday_syscall,
};

fn syscall_error_conversion() -> TestResult {
    assert_eq_test!(Errno::EPERM.as_ret(), -1, "EPERM");
    assert_eq_test!(Errno::ENOMEM.as_ret(), -12, "ENOMEM");
    assert_eq_test!(Errno::EINVAL.as_ret(), -22, "EINVAL");
    TestResult::Pass
}

fn syscall_error_from_i64() -> TestResult {
    assert_eq_test!(errno_from_i64(-1), Some(Errno::EPERM), "from -1");
    assert_eq_test!(errno_from_i64(-22), Some(Errno::EINVAL), "from -22");
    assert_eq_test!(errno_from_i64(-999), None, "from -999");
    TestResult::Pass
}

fn syscall_error_display() -> TestResult {
    let s = alloc::format!("{}", Errno::EPERM);
    assert_eq_test!(s.as_str(), "Operation not permitted", "EPERM display");
    let s = alloc::format!("{}", Errno::ENOSYS);
    assert_eq_test!(s.as_str(), "Function not implemented", "ENOSYS display");
    TestResult::Pass
}

// UT-07 (2026-09-26): pwm::sha256 注册组 15 例已收敛 —
// 纯算法断言以 framework/sgeg/sha256.rs 的 #[cfg(test)] 为唯一归属;
// 本文件保留 pwm::secure_boot_sha256 与 pwm::ct_eq_authority 两组
// (委托链一致性属 framework 内部协作行为, 非纯算法判据).

// B08-19 专项: secure_boot 侧 sha256_hash 委托规范实现, 输出与已知向量一致
fn secure_boot_sha256_hash_consistency() -> TestResult {
    let expected: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];
    assert_eq_test!(
        sha256_hash(b"abc"),
        expected,
        "secure_boot sha256_hash == 规范向量"
    );
    TestResult::Pass
}

// B08-19 专项: sha256_extend(A, B) == sha256(A || B) 组合语义
fn secure_boot_sha256_extend_combine() -> TestResult {
    let a = *b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let b = *b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    let mut combined = [0u8; 64];
    combined[..32].copy_from_slice(&a);
    combined[32..].copy_from_slice(&b);
    assert_eq_test!(
        sha256_extend(&a, &b),
        sha256(&combined),
        "extend == hash(A||B)"
    );
    TestResult::Pass
}

// B08-22a 专项: framework 权威 constant_time_eq 行为 (services ct_eq 委托链由 crypto.rs 单测覆盖)
fn constant_time_eq_authority() -> TestResult {
    check!(constant_time_eq(b"abc", b"abc"), "equal 相等");
    check!(!constant_time_eq(b"abc", b"abd"), "末尾位不同");
    check!(!constant_time_eq(b"abc", b"ab"), "长度不同");
    check!(constant_time_eq(b"", b""), "空切片相等");
    TestResult::Pass
}

// B08-22b 专项: ELF64 头统一定义布局 (coredump 与 loader 共用 elf/mod.rs)
fn elf64_header_layout() -> TestResult {
    assert_eq_test!(core::mem::size_of::<Elf64Header>(), 64, "ELF64 header 64B");
    assert_eq_test!(core::mem::size_of::<Elf64Phdr>(), 56, "ELF64 phdr 56B");
    TestResult::Pass
}

pub fn register_syscall_ffi_tests() {
    let r = runner();
    register_tests_inner! { r:
        "syscall::ffi": {
            "error_conversion": syscall_error_conversion,
            "error_from_i64": syscall_error_from_i64,
            "error_display": syscall_error_display,
        },
    }
}

pub fn register_sha256_tests() {
    let r = runner();
    register_tests_inner! { r:
        "pwm::secure_boot_sha256": {
            "hash_consistency": secure_boot_sha256_hash_consistency,
            "extend_combine": secure_boot_sha256_extend_combine,
        },
        "pwm::ct_eq_authority": {
            "constant_time_eq": constant_time_eq_authority,
        },
        "proc::elf64_header": {
            "layout": elf64_header_layout,
        },
    }
}

// ============================================================================
// T1 G6: 时间组 (settimeofday / adjtimex / clock_nanosleep)
// ============================================================================

/// 越界用户指针 (`>= USER_ADDR_MAX`): 被指针范围校验拒绝, 不会解引用
const BAD_USER_PTR: u64 = 0x8000_0000_0000_0000;
/// 时间比较容差 (ns): 容忍 tick 粒度 (1kHz → 1ms) 与执行间隙
const CLOCK_TOLERANCE_NS: u64 = 50_000_000;
/// 本内核不支持的时钟 ID (`CLOCK_PROCESS_CPUTIME_ID`)
const CLOCK_PROCESS_CPUTIME_ID: i32 = 2;
/// `settimeofday` 特权路径使用的固定墙钟目标 (2020-09-13)
const TARGET_SEC: i64 = 1_600_000_000;

/// `clock_gettime` 参数校验 (不支持时钟 ID / 指针)
fn clock_gettime_validation() -> TestResult {
    check!(
        clock_gettime_syscall(CLOCK_MONOTONIC, 0) == -22,
        "null tp → EINVAL"
    );
    check!(
        clock_gettime_syscall(CLOCK_PROCESS_CPUTIME_ID, 0x1000) == -22,
        "unsupported clock → EINVAL"
    );
    check!(
        clock_gettime_syscall(-1, 0x1000) == -22,
        "negative clock → EINVAL"
    );
    check!(
        clock_gettime_syscall(CLOCK_MONOTONIC, BAD_USER_PTR) == -14,
        "out-of-range tp → EFAULT"
    );
    TestResult::Pass
}

/// `settimeofday` 参数校验 + 墙钟写入策略
fn settimeofday_apply() -> TestResult {
    let sub = crate::services::timer::time_sync::subsystem();
    let original = sub.get_adjusted_time_ns();

    // tv 为 NULL: Linux 语义为"只设置时区", 本实装无操作
    check!(settimeofday_syscall(0, 0) == Ok(0), "null tv is no-op");
    check!(
        settimeofday_syscall(BAD_USER_PTR, 0) == Err(Errno::EFAULT),
        "bad tv → EFAULT"
    );
    check!(
        settimeofday_syscall(BAD_USER_PTR, BAD_USER_PTR) == Err(Errno::EFAULT),
        "bad tv rejected before tz"
    );

    // 参数越界 (早于特权判定)
    check!(
        apply_settimeofday(-1, 0) == Err(Errno::EINVAL),
        "negative sec → EINVAL"
    );
    check!(
        apply_settimeofday(0, 1_000_000) == Err(Errno::EINVAL),
        "usec out of range → EINVAL"
    );

    if crate::framework::sgeg::get_current_uid() != 0 {
        check!(
            apply_settimeofday(0, 0) == Err(Errno::EPERM),
            "non-root denied"
        );
        return TestResult::Pass;
    }

    // 特权路径: 设置后墙钟立即反映 (2020-09-13 固定时刻 + 500ms)
    check!(
        apply_settimeofday(TARGET_SEC, 500_000) == Ok(0),
        "root can set wall clock"
    );
    let expect = TARGET_SEC as u64 * 1_000_000_000 + 500_000 * 1_000;
    check!(
        sub.get_adjusted_time_ns().abs_diff(expect) < CLOCK_TOLERANCE_NS,
        "wall clock reflects settimeofday"
    );

    check!(sub.set_time(original), "restore wall clock");
    check!(
        sub.get_adjusted_time_ns().abs_diff(original) < CLOCK_TOLERANCE_NS,
        "wall clock restored"
    );
    TestResult::Pass
}

/// `clock_nanosleep` 参数校验 + 等待时长策略 (含 `TIMER_ABSTIME`)
fn clock_nanosleep_policy() -> TestResult {
    check!(
        clock_nanosleep_syscall(CLOCK_REALTIME, 0, 0, 0) == Err(Errno::EFAULT),
        "null req → EFAULT"
    );
    check!(
        clock_nanosleep_syscall(CLOCK_REALTIME, 0x2, 0x1000, 0) == Err(Errno::EINVAL),
        "unknown flags → EINVAL"
    );
    check!(
        clock_nanosleep_syscall(CLOCK_REALTIME, 0, BAD_USER_PTR, 0) == Err(Errno::EFAULT),
        "out-of-range req → EFAULT"
    );

    // 等待时长策略 (不含用户内存访问)
    assert_eq_test!(
        clock_nanosleep_wait_ns(CLOCK_MONOTONIC, 0, 0, 5_000_000),
        Ok(5_000_000),
        "relative duration"
    );
    assert_eq_test!(
        clock_nanosleep_wait_ns(CLOCK_MONOTONIC, TIMER_ABSTIME, 0, 0),
        Ok(0),
        "past absolute deadline"
    );
    check!(
        clock_nanosleep_wait_ns(CLOCK_MONOTONIC, 0x4, 0, 0) == Err(Errno::EINVAL),
        "bad flags → EINVAL"
    );
    check!(
        clock_nanosleep_wait_ns(7, 0, 0, 0) == Err(Errno::EINVAL),
        "unsupported clock → EINVAL"
    );
    check!(
        clock_nanosleep_wait_ns(CLOCK_MONOTONIC, 0, -1, 0) == Err(Errno::EINVAL),
        "negative sec → EINVAL"
    );
    check!(
        clock_nanosleep_wait_ns(CLOCK_MONOTONIC, 0, 0, 1_000_000_000) == Err(Errno::EINVAL),
        "nsec out of range → EINVAL"
    );
    TestResult::Pass
}

/// `adjtimex` 参数校验 + 三个受支持 mode 的策略
fn adjtimex_policy() -> TestResult {
    check!(adjtimex_syscall(0) == -14, "null buf → EFAULT");
    check!(adjtimex_syscall(BAD_USER_PTR) == -14, "bad buf → EFAULT");

    // 不支持的 mode 位 (ADJ_ESTERROR) / ADJ_SETOFFSET 增量越界 → EINVAL
    check!(
        apply_adjtimex(0x0008, 0, 0, 0, 0) == Err(Errno::EINVAL),
        "unsupported mode bit → EINVAL"
    );
    check!(
        apply_adjtimex(ADJ_SETOFFSET, 0, 0, 0, 2_000_000) == Err(Errno::EINVAL),
        "ADJ_SETOFFSET usec out of range"
    );

    if crate::framework::sgeg::get_current_uid() != 0 {
        check!(
            apply_adjtimex(ADJ_FREQUENCY, 0, 0, 0, 0) == Err(Errno::EPERM),
            "non-root denied"
        );
        return TestResult::Pass;
    }

    let sub = crate::services::timer::time_sync::subsystem();
    let original = sub.get_adjusted_time_ns();

    // ADJ_FREQUENCY: freq 单位 2^-16 ppm → 65536 即 1ppm = 1000ppb
    check!(
        apply_adjtimex(ADJ_FREQUENCY, 0, 65_536, 0, 0) == Ok(0),
        "ADJ_FREQUENCY accepted"
    );
    check!(sub.get_sync_status().2 == 1_000, "frequency applied as ppb");
    check!(sub.adj_freq(0), "frequency reset");

    // ADJ_SETOFFSET: 加性跳变 +1s
    let before = sub.get_adjusted_time_ns();
    check!(
        apply_adjtimex(ADJ_SETOFFSET, 0, 0, 1, 0) == Ok(0),
        "ADJ_SETOFFSET accepted"
    );
    check!(
        sub.get_adjusted_time_ns()
            .abs_diff(before.saturating_add(1_000_000_000))
            < CLOCK_TOLERANCE_NS,
        "wall clock stepped by +1s"
    );

    // ADJ_OFFSET: 渐进偏移登记 (由 tick 机制按斜率消耗)
    check!(
        apply_adjtimex(ADJ_OFFSET, 1_000, 0, 0, 0) == Ok(0),
        "ADJ_OFFSET accepted"
    );

    check!(sub.set_time(original), "restore wall clock");
    check!(
        sub.get_adjusted_time_ns().abs_diff(original) < CLOCK_TOLERANCE_NS,
        "wall clock restored"
    );
    TestResult::Pass
}

/// 同步机制: `set_time` 跳变反映 + 频率调整后基准重算 (不因 elapsed 累积偏离)
fn wall_clock_freq_baseline() -> TestResult {
    let sub = crate::services::timer::time_sync::subsystem();
    let original = sub.get_adjusted_time_ns();
    let target = original + 3_600_000_000_000; // +1h

    check!(sub.set_time(target), "set_time accepted");
    check!(
        sub.get_adjusted_time_ns().abs_diff(target) < CLOCK_TOLERANCE_NS,
        "wall clock reflects set_time"
    );

    // 频率调整 (500ppm 上界) 后基准重置: 若沿用启动时刻基准, 补偿量会按
    // 全部 uptime 累积并产生秒级偏离, 超出容差.
    check!(sub.adj_freq(500_000_000), "adj_freq accepted");
    check!(
        sub.get_adjusted_time_ns().abs_diff(target) < CLOCK_TOLERANCE_NS,
        "frequency compensation baseline reset"
    );

    check!(sub.adj_freq(0), "frequency restored");
    check!(sub.set_time(original), "wall clock restored");
    check!(
        sub.get_adjusted_time_ns().abs_diff(original) < CLOCK_TOLERANCE_NS,
        "wall clock restored exactly"
    );
    TestResult::Pass
}

/// 渐进调整: `adj_time` 登记的偏移由 tick 机制 (timesync tick_adjust) 消耗
fn wall_clock_gradual_adjust() -> TestResult {
    let sub = crate::services::timer::time_sync::subsystem();
    let original = sub.get_adjusted_time_ns();

    // 5ms 渐进偏移; ADJ_RATE_NS = 1000ns/tick → 5000 次 tick 消耗完毕
    check!(sub.adj_time(5_000_000), "adj_time accepted");
    for _ in 0..6_000 {
        sub.tick_adjust();
    }
    check!(
        sub.get_adjusted_time_ns() >= original + 5_000_000,
        "gradual offset fully consumed by tick mechanism"
    );

    check!(sub.set_time(original), "wall clock restored");
    check!(
        sub.get_adjusted_time_ns().abs_diff(original) < CLOCK_TOLERANCE_NS,
        "wall clock restored"
    );
    TestResult::Pass
}

pub fn register_time_tests() {
    let r = runner();
    register_tests_inner! { r:
        "time::clock": {
            "gettimeofday_validation": clock_gettime_validation,
            "settimeofday_apply": settimeofday_apply,
            "clock_nanosleep_policy": clock_nanosleep_policy,
            "adjtimex_policy": adjtimex_policy,
            "freq_baseline": wall_clock_freq_baseline,
            "gradual_adjust": wall_clock_gradual_adjust,
        },
    }
}

// ============================================================================
// T1 G7: 文件系统组 (execveat / chroot / pivot_root / setdomainname)
// ============================================================================

/// `AT_FDCWD` (Linux ABI): 相对路径基于当前工作目录
const AT_FDCWD: i32 = -100;
/// `execveat` 标志: 空 `pathname` 表示执行 `dirfd` 指向的文件自身
const AT_EMPTY_PATH: i32 = 0x1000;

/// `execveat` 参数校验 (flags / dirfd / `AT_EMPTY_PATH`)
fn execveat_validation() -> TestResult {
    check!(
        execveat_syscall(AT_FDCWD, BAD_USER_PTR, 0, 0, 0) == Err(Errno::EFAULT),
        "bad pathname → EFAULT"
    );
    check!(
        execveat_syscall(-1, BAD_USER_PTR, 0, 0, 0) == Err(Errno::ENOTSUP),
        "non-AT_FDCWD dirfd → ENOTSUP"
    );
    check!(
        execveat_syscall(AT_FDCWD, BAD_USER_PTR, 0, 0, 0x2000) == Err(Errno::EINVAL),
        "unknown flag → EINVAL"
    );
    check!(
        execveat_syscall(AT_FDCWD, BAD_USER_PTR, 0, 0, AT_EMPTY_PATH) == Err(Errno::EFAULT),
        "AT_EMPTY_PATH with unreadable pathname → EFAULT"
    );
    TestResult::Pass
}

/// `setdomainname` 前置校验链 (指针/长度 → EINVAL, 先于特权判定)
///
/// 策略返回 `i64` (syscall 约定), 故直接比对 `-EINVAL`.
fn setdomainname_validation() -> TestResult {
    check!(setdomainname_syscall(0, 4) == -22, "空指针 → EINVAL");
    check!(
        setdomainname_syscall(BAD_USER_PTR, 0) == -22,
        "len=0 → EINVAL"
    );
    check!(
        setdomainname_syscall(BAD_USER_PTR, 64) == -22,
        "len > 63 超 UTS_DOMAINNAME_LEN → EINVAL"
    );
    TestResult::Pass
}

/// `chroot` 指针校验 (先于特权判定, 保证内核态调用点可确定性验证)
fn chroot_validation() -> TestResult {
    check!(chroot_syscall(0) == Err(Errno::EFAULT), "空指针 → EFAULT");
    check!(
        chroot_syscall(BAD_USER_PTR) == Err(Errno::EFAULT),
        "越界指针 → EFAULT"
    );
    TestResult::Pass
}

/// `pivot_root` 指针校验 + `new_root`/`put_old` 关系校验
fn pivot_root_validation() -> TestResult {
    check!(
        pivot_root_syscall(0, 0) == Err(Errno::EFAULT),
        "空指针 → EFAULT"
    );
    check!(
        pivot_root_syscall(BAD_USER_PTR, BAD_USER_PTR) == Err(Errno::EFAULT),
        "越界指针 → EFAULT"
    );

    // 关系校验: new_root 即当前根 → EBUSY
    check!(
        pivot_root_plan("/", "/old", "/") == Err(Errno::EBUSY),
        "new_root == 当前根 → EBUSY"
    );
    check!(
        pivot_root_plan("/new", "/new/old", "/new") == Err(Errno::EBUSY),
        "new_root == 当前根 (非 /) → EBUSY"
    );
    // put_old 与 new_root 相同 → EINVAL
    check!(
        pivot_root_plan("/new", "/new", "/") == Err(Errno::EINVAL),
        "put_old == new_root → EINVAL"
    );
    // put_old 不在 new_root 之下 → EINVAL
    check!(
        pivot_root_plan("/new", "/other", "/") == Err(Errno::EINVAL),
        "put_old 不在 new_root 之下 → EINVAL"
    );
    // 前缀相同但非路径下级的边界情形 → EINVAL
    check!(
        pivot_root_plan("/newroot", "/new/old", "/") == Err(Errno::EINVAL),
        "前缀相似但非下级 → EINVAL"
    );
    // 合法关系
    check!(
        pivot_root_plan("/new", "/new/old", "/") == Ok(()),
        "合法 new_root/put_old 关系"
    );
    TestResult::Pass
}

pub fn register_fs_tests() {
    let r = runner();
    register_tests_inner! { r:
        "fs::execveat": {
            "arg_validation": execveat_validation,
        },
        "fs::chroot": {
            "chroot_validation": chroot_validation,
            "pivot_root_validation": pivot_root_validation,
        },
        "fs::setdomainname": {
            "setdomainname_validation": setdomainname_validation,
        },
    }
}

pub fn register_tests() {
    register_syscall_ffi_tests();
    register_sha256_tests();
    register_time_tests();
    register_fs_tests();
}
