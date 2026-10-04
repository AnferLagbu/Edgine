use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering;

use crate::framework::sync::irq_spinlock::IrqSpinLock;

use crate::framework::sync::once_lock::OnceLock;
// E-03 (2026-09-06): feature 语义拆分 — 纯逻辑测试模块在 host-test 下同样编译
// (同源双编译, 供 host-tests 引用内核真实源码). 硬件路径门控保持 kernel_test.
// 语义: any(kernel_test, host-test) = 纯逻辑测试辅助; kernel_test = 硬件路径切换.
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod arch;
// E-03 (2026-09-06): host 不可编译（依赖 idt/types.rs::InterruptFrame::new_test_frame 与
// idt/statistics.rs::DetailedStatistics::reset, 二者 cfg(any(test, kernel_test))
// 门控在 host-test 下关闭），保持 kernel_test
#[cfg(feature = "kernel_test")]
pub mod idt;
// UT-07 (2026-09-26): net 由 kernel_test 专属改归 any(kernel_test, host-test) —
// e1000 硬件路径注册组删除后本模块仅剩纯逻辑用例 (net::utils), 按 E-03 双端约定.
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod net;
// UT-07 (2026-09-26): reset 由 kernel_test 专属改归 any(kernel_test, host-test) —
// freg::reset::{audit,bbr,bsr,parallel}::tests 已由 cfg(feature = "kernel_test")
// 的 `pub mod tests { pub fn .. -> bool }` 改写为源侧 `#[cfg(test)] #[test]`,
// reset.rs 对 kernel_test 门控的依赖随之解除 (config::tests 本就是 any 双端).
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod reset;
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod sched;
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod sys;
pub mod test_config;
pub mod test_freg;
pub mod test_freg_ext;
// UT-07 (2026-09-26): test_sgeg 注册载体已删 — services::sgeg::policy
// (CapBits/CapMatrix/InMemoryMatrix) 纯逻辑断言以该文件源侧 #[cfg(test)] 为唯一归属.
pub mod test_ipc;
pub mod test_mm;
pub mod test_new_features;
pub mod test_pi_mutex;
pub mod test_proc;
pub mod test_pwm;
pub mod test_smp;
pub mod test_uds;

pub type TestFn = fn() -> TestResult;

pub enum TestResult {
    Pass,
    Fail(&'static str),
    Skip(&'static str),
}

#[derive(Copy, Clone)]
pub struct TestCase {
    pub module: &'static str,
    pub name: &'static str,
    pub func: TestFn,
}

// E-06 (2026-09-07): 256→512 扩容 — host-tests 侧 sha256/checksum/capability
// 载体用例去重合入本套件 (新增 ~60 纯逻辑用例), 256 容量已满会导致注册静默丢弃.
// pcache 帧持有计数工程 (2026-09-21): 512 已再次占满, 新增 mm::pcache 四条用例
// 会把注册表尾部的 timerfd/syscall 用例挤出 (静默丢弃) ⇒ 扩容至 640 留余量.
const MAX_TESTS: usize = 640;

fn noop_test() -> TestResult {
    TestResult::Pass
}

const NOOP_CASE: TestCase = TestCase {
    module: "",
    name: "",
    func: noop_test,
};

struct TestRegistry {
    count: usize,
    cases: [TestCase; MAX_TESTS],
}

impl TestRegistry {
    // E-06 (2026-09-07): MAX_TESTS 256→512 后数组 40B×512=20KiB, clippy
    // large_stack_arrays 误报 — 本数组经 OnceLock 存放于 static (.bss) 非栈.
    #[expect(
        clippy::large_stack_arrays,
        reason = "large_stack_arrays: 测试注册表数组存放于 static OnceLock (.bss) 非栈分配; clippy 对 const fn 内字面量误报, 当前优先 expect"
    )]
    const fn new() -> Self {
        Self {
            count: 0,
            cases: [NOOP_CASE; MAX_TESTS],
        }
    }

    fn register(&mut self, module: &'static str, name: &'static str, func: TestFn) {
        // 满容量 fail-closed: 静默丢弃会让用例"看起来绿"而实际未运行
        // (2026-09-21 pcache 工程实测: +4 用例把表尾 timerfd/syscall 用例挤出).
        assert!(
            self.count < MAX_TESTS,
            "测试注册表已满: 用例被丢弃, 需上调 MAX_TESTS"
        );
        self.cases[self.count] = TestCase { module, name, func };
        self.count += 1;
    }
}

pub struct TestRunner {
    registry: IrqSpinLock<TestRegistry>,
    pub passed: AtomicU32,
    pub failed: AtomicU32,
    pub skipped: AtomicU32,
}

impl TestRunner {
    pub const fn new() -> Self {
        Self {
            registry: IrqSpinLock::new(TestRegistry::new()),
            passed: AtomicU32::new(0),
            failed: AtomicU32::new(0),
            skipped: AtomicU32::new(0),
        }
    }

    pub fn register(&self, module: &'static str, name: &'static str, func: TestFn) {
        self.registry.lock().register(module, name, func);
    }

    pub fn run_all(&self) {
        let reg = self.registry.lock();
        let total = reg.count;

        Self::serial_print(b"\n========================================\n");
        Self::serial_print(b"  Edgine Test Suite\n  ");
        Self::serial_print_num(total as u64);
        Self::serial_print(b" test cases registered\n");
        Self::serial_print(b"========================================\n\n");

        // B03-15 修复: interrupt_disable() 返回 usize 保存旧 flags,
        // 测试循环后调 interrupt_restore(flags) 恢复原状态。
        // 之前循环结束后无 re-enable, 测试结束中断永久全关。
        // E-04 (2026-09-06): 中断禁用/恢复改走 sync::disable_interrupts()/
        // restore_interrupts() — host-test 下 B08-14 已桩化为 no-op (直接
        // arch!(interrupt_disable) 执行 cli 特权指令, host 用户态会 SIGSEGV);
        // 裸机 (kernel_test) 下其内部即 arch!(interrupt_disable), 语义完全等价.
        let saved_flags = crate::framework::sync::disable_interrupts();

        for i in 0..total {
            let tc = reg.cases[i];
            let module = tc.module;
            let name = tc.name;
            let func = tc.func;

            Self::serial_print(b"[");
            Self::serial_print_num((i + 1) as u64);
            Self::serial_print(b"/");
            Self::serial_print_num(total as u64);
            Self::serial_print(b"] ");
            Self::serial_print(module.as_bytes());
            Self::serial_print(b"::");
            Self::serial_print(name.as_bytes());
            Self::serial_print(b"...");

            // E-04 (2026-09-06): 测试运行器双端适配 — host-test 下用 catch_unwind
            // 捕获测试内 panic → Fail, 避免单个测试 panic 中断整个 run_all.
            // kernel_test (裸机) 保持直接调用, 行为与改造前完全一致.
            // 注: extern "C" FFI 函数内 panic 为 abort, catch_unwind 无法捕获,
            // 此类依赖裸机初始化的测试在 host-test 下以 Skip 占位注册 (见各注册函数).
            #[cfg(feature = "host-test")]
            let result = match std::panic::catch_unwind(func) {
                Ok(r) => r,
                Err(_) => TestResult::Fail("host: test panicked (裸机环境依赖)"),
            };
            #[cfg(not(feature = "host-test"))]
            let result = func();

            match result {
                TestResult::Pass => {
                    self.passed.fetch_add(1, Ordering::Relaxed);
                    Self::serial_print(b"PASS\n");
                }
                TestResult::Fail(msg) => {
                    self.failed.fetch_add(1, Ordering::Relaxed);
                    Self::serial_print(b"FAIL: ");
                    Self::serial_print(msg.as_bytes());
                    Self::serial_print(b"\n");
                }
                TestResult::Skip(reason) => {
                    self.skipped.fetch_add(1, Ordering::Relaxed);
                    Self::serial_print(b"SKIP: ");
                    Self::serial_print(reason.as_bytes());
                    Self::serial_print(b"\n");
                }
            }
        }

        drop(reg);

        let p = self.passed.load(Ordering::Relaxed);
        let f = self.failed.load(Ordering::Relaxed);
        let s = self.skipped.load(Ordering::Relaxed);

        Self::serial_print(b"\n========================================\n");
        if f > 0 {
            Self::serial_print(b"  RESULT: ");
            Self::serial_print_num(u64::from(p));
            Self::serial_print(b" passed, ");
            Self::serial_print_num(u64::from(f));
            Self::serial_print(b" FAILED, ");
            Self::serial_print_num(u64::from(s));
            Self::serial_print(b" skipped\n");
        } else {
            Self::serial_print(b"  RESULT: ALL ");
            Self::serial_print_num(u64::from(p));
            Self::serial_print(b" TESTS PASSED (");
            Self::serial_print_num(u64::from(s));
            Self::serial_print(b" skipped)\n");
        }
        Self::serial_print(b"========================================\n");

        // B03-15: 恢复中断状态 (而非保持 disable 状态)。
        // SAFETY: saved_flags 由 interrupt_save() 获取, 与 interrupt_disable()
        // 配套使用恢复原状态。
        crate::framework::sync::restore_interrupts(&saved_flags);
    }

    fn serial_print(s: &[u8]) {
        // E-04 (2026-09-06): 测试运行器双端适配 — host-test (std) 下输出走 stdout,
        // 替代裸机串口 (COM1/uart, host 用户态不可用). host-test 分支优先;
        // 原裸机逻辑用 not(host-test) 包裹, kernel_test (QEMU) 行为与改造前完全一致.
        #[cfg(feature = "host-test")]
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(s);
            let _ = out.flush();
        }
        #[cfg(all(not(feature = "host-test"), target_arch = "x86_64"))]
        serial_print(s);
        #[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
        for &b in s {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                crate::framework::arch::aarch64::uart::putc(b);
            }
        }
    }

    fn serial_print_num(n: u64) {
        // E-04 (2026-09-06): 测试运行器双端适配 — host-test (std) 下数字输出走 stdout
        // (std::io::Write::write_fmt), 替代裸机串口. 原裸机逻辑保持 not(host-test).
        #[cfg(feature = "host-test")]
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = write!(out, "{n}");
            let _ = out.flush();
        }
        #[cfg(all(not(feature = "host-test"), target_arch = "x86_64"))]
        serial_print_num(n);
        #[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
        {
            if n == 0 {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    crate::framework::arch::aarch64::uart::putc(b'0');
                }
                return;
            }
            let mut buf = [0u8; 20];
            let mut pos = 0usize;
            let mut val = n;
            while val > 0 {
                buf[pos] = (val % 10) as u8 + b'0';
                pos += 1;
                val /= 10;
            }
            for i in (0..pos).rev() {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    crate::framework::arch::aarch64::uart::putc(buf[i]);
                }
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
unsafe fn port_inb(port: u16) -> u8 {
    crate::arch!(inb(port))
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn port_outb(port: u16, value: u8) {
    crate::arch!(outb(port, value));
}

#[cfg(target_arch = "x86_64")]
pub fn serial_print(s: &[u8]) {
    const COM1: u16 = 0x3F8;
    for &b in s {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            while (port_inb(COM1 + 5) & 0x20) == 0 {
                core::hint::spin_loop();
            }
            port_outb(COM1, b);
        }
        if b == b'\n' {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                port_outb(COM1, b'\r');
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub fn serial_print_num(mut n: u64) {
    if n == 0 {
        serial_print(b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut pos = 0usize;
    while n > 0 {
        buf[pos] = (n % 10) as u8 + b'0';
        pos += 1;
        n /= 10;
    }
    for i in (0..pos).rev() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            const COM1: u16 = 0x3F8;
            while (port_inb(COM1 + 5) & 0x20) == 0 {
                core::hint::spin_loop();
            }
            port_outb(COM1, buf[i]);
        }
    }
}

static TEST_RUNNER: OnceLock<TestRunner> = OnceLock::new();

pub fn runner() -> &'static TestRunner {
    TEST_RUNNER.get_or_init(|slot| {
        slot.write(TestRunner::new());
    })
}

#[macro_export]
macro_rules! check {
    ($cond:expr_2021, $msg:literal $(,)?) => {
        if !($cond) {
            return $crate::framework::tests::TestResult::Fail($msg);
        }
    };
}

#[macro_export]
macro_rules! assert_eq_test {
    ($left:expr_2021, $right:expr_2021, $msg:literal $(,)?) => {
        let l = $left;
        let r = $right;
        if l != r {
            return $crate::framework::tests::TestResult::Fail($msg);
        }
    };
}

#[macro_export]
macro_rules! skip_test {
    ($reason:literal $(,)?) => {
        return $crate::framework::tests::TestResult::Skip($reason);
    };
}

#[macro_export]
macro_rules! register_tests_inner {
    ($r:ident: $($mod:literal: { $($name:literal: $func:ident),* $(,)? }),* $(,)?) => {
        $(
            $(
                $r.register($mod, $name, $func);
            )*
        )*
    };
}

pub use {assert_eq_test, check, skip_test};

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub fn test_runner_init() {
    crate::klog_boot_info!("[TEST] === Edgine Test Framework ===");

    register_all_tests();

    let r = runner();
    let count = r.registry.lock().count;
    crate::klog_boot_info!("[TEST] Registered {} test cases", count);

    // 诊断: test 运行前检查页表
    {
        let read_u64 = |phys: u64, idx: usize| -> u64 {
            let va = phys + crate::framework::mm::KERNEL_BASE + idx as u64 * 8;
            // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
            unsafe { core::ptr::read_volatile(va as *const u64) }
        };
        let pd24 = read_u64(0x109000, 24);
        let pd63 = read_u64(0x109000, 63);
        crate::klog_boot_info!(
            "[PAGETABLE] before run_all: pd[24]=0x{:016X} pd[63]=0x{:016X}",
            pd24,
            pd63
        );
    }

    r.run_all();

    let p = r.passed.load(Ordering::Relaxed);
    let f = r.failed.load(Ordering::Relaxed);
    if f == 0 {
        crate::klog_boot_info!(
            "[TEST] ALL TESTS PASSED ({}/{})",
            p,
            p + r.skipped.load(Ordering::Relaxed)
        );
    } else {
        crate::klog_boot_info!("[TEST] COMPLETE: {} passed, {} FAILED", p, f);
    }
}

// E-04 (2026-09-06): 测试运行器双端适配 — 注册入口抽取.
// kernel_test 启动路径 (test_runner_init) 与 host-test 入口 (host_test_runner_main)
// 共用同一注册逻辑, 保证双端注册同一套纯逻辑测试集 (注册数一致).
// 门控外 15 mod + any(kernel_test, host-test) 5 mod 在双端均注册;
// kernel_test 硬件路径 (driver/idt/timer/signal/...) 保持 kernel_test 专属,
// host-test 下不编译 (这些 mod 在 host 下不存在).
pub fn register_all_tests() {
    // FS 全局单例初始化 — 测试模式下需主动 init, 否则后续
    // 调用 global() 会 panic (e.g. devfs::global() called before init_global()).
    // init_global 是幂等的 (OnceCell::get_or_init), 多次调用安全.
    // 注: ramfs::init_global 需要 mount_point 参数, 在 mount 测试内显式调用.
    crate::services::fs::devfs::init_global();
    crate::services::fs::procfs::init_global();

    test_freg::register_freg_tests();
    test_freg_ext::register_freg_ext_tests();
    test_config::register_config_tests();
    test_pwm::register_pwm_tests();
    test_mm::register_mm_tests();
    test_ipc::register_ipc_tests();
    test_uds::register_uds_tests();
    test_pi_mutex::register_pi_mutex_tests();
    test_proc::register_proc_tests();
    test_new_features::register_new_tests();
    test_smp::register_smp_tests();

    // E-03 (2026-09-06): feature 语义拆分 — 纯逻辑测试注册 (host-test 下同样编译,
    // 供 host-tests 同源引用; kernel_test 下行为与改造前完全一致).
    #[cfg(any(feature = "kernel_test", feature = "host-test"))]
    {
        #[cfg(target_arch = "x86_64")]
        {
            arch::register_tests();
            sys::register_tests();
        }
        sched::register_tests();
        net::register_tests();
        reset::register_tests();
    }

    // E-03 (2026-09-06): 硬件路径测试注册 — 依赖裸机硬件 (驱动/网络/定时器/中断等),
    // 仅 kernel_test (QEMU 裸机测试) 生效; host-test 下不编译.
    // 注: idt 因依赖 kernel_test 门控的框架测试辅助 (见上), 注册同样留在本块.
    #[cfg(feature = "kernel_test")]
    {
        #[cfg(target_arch = "x86_64")]
        {
            idt::register_tests();
        }
        #[cfg(target_arch = "x86_64")]
        {
            crate::framework::timer::calibration::register_timer_calibration_tests();
        }
        crate::framework::timer::tick::register_timer_tick_tests();
        #[cfg(target_arch = "x86_64")]
        crate::framework::timer::irq::register_timer_irq_tests();
        crate::framework::timer::sleep::register_timer_sleep_tests();
        crate::framework::timer::hrtimer::register_hrtimer_tests();
        crate::framework::proc::signal::register_signal_tests();
        crate::framework::config::memory::register_aslr_tests();
        crate::framework::syscall::futex::register_futex_tests();
        crate::framework::proc::robust::register_robust_tests();
        crate::framework::mm::pcache::register_pcache_tests();
        crate::framework::mm::swap::register_swap_tests();
        crate::framework::pci::msi::register_msi_tests();
        crate::framework::syscall::epoll::register_epoll_tests();
        crate::framework::syscall::eventfd::register_eventfd_tests();
        crate::framework::syscall::signalfd::register_signalfd_tests();
        crate::framework::syscall::timerfd::register_timerfd_tests();
    }
}

// E-04 (2026-09-06): 测试运行器双端适配 — host 侧测试运行汇总.
// 供 host-tests 断言 "0 failed". 仅 host-test feature 下编译 (F9 死代码零容忍).
#[cfg(feature = "host-test")]
#[derive(Clone, Copy)]
pub struct TestSummary {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
}

/// E-04 (2026-09-06): 测试运行器双端适配 — host-test 侧测试入口.
///
/// 与 kernel_test 的 `test_runner_init` 等价: 注册全部纯逻辑测试 + 执行 `run_all()`
/// (输出经 `serial_print` host 分支走 stdout), 返回汇总供 host-tests 断言.
/// 仅 host-test feature 下编译 (F9 死代码零容忍); kernel_test/裸机构建不包含本函数.
#[cfg(feature = "host-test")]
pub fn host_test_runner_main() -> TestSummary {
    register_all_tests();
    let r = runner();
    r.run_all();
    TestSummary {
        passed: r.passed.load(Ordering::Relaxed),
        failed: r.failed.load(Ordering::Relaxed),
        skipped: r.skipped.load(Ordering::Relaxed),
    }
}
