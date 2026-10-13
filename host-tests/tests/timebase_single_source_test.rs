//! E-I 回归锁: 全内核单一时基 (硬件自由计数器派生), 禁止再出现第二份计数或假频率
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-1 / P-3 / P-7
//! 与 S-3 收益链 (E-I 时基单源).
//!
//! # 被锁死的四条不变式
//!
//! 1. **只有一份 tick 计数**: 调度器/OOM 守护不再自存计数副本, 一律读
//!    `timer::get_ticks()`. 此前 `proc::scheduler::TICK_COUNT` 与 timer 的中断
//!    计数并行存在 (P-1), 两个视图在停 tick / 追赶时会分叉.
//! 2. **只有一个频率来源**: 计数器频率由 `timer::calibration` 单点登记, 越界即
//!    拒. 旧 `cpu::calibrate_tsc` 用 2.5 / 3.0 / 1.0 GHz 厂商硬编码兜底, 且以
//!    `cpu_get_tsc_frequency()` FFI 外溢, 各处再各自折算 (假 2 GHz 的忙等待、
//!    `tsc / 3000` 的日志前缀、`tsc / freq * 1_000_000` 的整数除法).
//! 3. **测定路径不消费被测定量**: 频率测定的时间窗口取自 PIT 倒计数器, 不取
//!    tick 计数 —— 后者正是由待测频率派生的 (循环论证), 且在启动早期
//!    (IF 尚未打开) 永不推进 (死自旋).
//! 4. **周期触发用 carry 不用取模**: 时基改硬件派生后读数可能一次跳过整点
//!    (重新建锚 / E-III 停 tick 后追赶), `is_multiple_of` 会永久漏触发.
//!
//! # 断言方式
//!
//! 源码文本契约 (剥注释 + 去空白), 只锁语义形状, 不受 rustfmt 换行影响. 判定
//! 逻辑收敛成 `has_*` 谓词函数, 由 [`test_negative_control_predicates_reject_regressions`]
//! 用合成回退样本反向验证 —— 谓词对真实文件返 `false` 才有意义, 前提是它确实
//! 能在回归发生时返 `true`.

use std::fs;
use std::path::{Path, PathBuf};

const LIB: &str = "../src/kernel/lib.rs";
const CPU: &str = "../src/kernel/privileged/cpu/mod.rs";
const TICK: &str = "../src/kernel/privileged/timer/tick.rs";
const CALIB: &str = "../src/kernel/privileged/timer/calibration.rs";
const SLEEP: &str = "../src/kernel/privileged/timer/sleep.rs";
const HRTIMER: &str = "../src/kernel/privileged/timer/hrtimer.rs";
const SGEG: &str = "../src/kernel/privileged/sgeg/bootstrap.rs";
const SCHED: &str = "../src/kernel/privileged/proc/scheduler.rs";
const OOMD: &str = "../src/kernel/privileged/proc/oomd.rs";
const TICK_QUERY: &str = "../src/kernel/privileged/tick_query.rs";
const PROCFS: &str = "../src/kernel/functions/fs/procfs_core.rs";

// ============================================================================
// 文本读取与规范化
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 生产代码文本: 剥掉注释行, 并在第一个 `#[cfg(test)]` 处截断.
///
/// 两步都必需:
/// - 本轮修复在多个文件留下大量动机注释, 逐字提到 `TICK_COUNT` /
///   `is_multiple_of` / `cpu_get_tsc_frequency` 等被禁止的符号名. 不剥注释,
///   锁的就成了"注释里不许提旧事".
/// - 单元测试维合法地使用被禁形态 (隔离守卫直接写登记静态量), 不属于回归.
fn prod_code(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && !l.starts_with("/*") && !l.starts_with('*'))
        .take_while(|l| !l.starts_with("#[cfg(test)]"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 去掉全部空白, 使形状断言不受换行与缩排影响.
fn squash(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

fn prod_squashed(path: &str) -> String {
    squash(&prod_code(&read(path)))
}

fn repo_kernel_root() -> PathBuf {
    Path::new("../src/kernel")
        .canonicalize()
        .expect("内核源码根目录")
}

// ============================================================================
// 谓词 (可被负控制反向验证)
// ============================================================================

/// 第二个 tick 计数副本: 自己 `static` 一份计数, 或对外提供私有读数.
fn has_dual_tick_counter(code_squashed: &str) -> bool {
    code_squashed.contains("staticTICK_COUNT") || code_squashed.contains("fncpu_get_tick(")
}

/// 周期任务用取模判定 (时基跳点时会永久漏触发).
///
/// 只锁 kswapd / hotplug 两处周期任务: 调度器里其它 `is_multiple_of` 判定的是
/// 软件队列的清理节奏, 与单一时基的跳点语义无关.
fn has_modulo_periodic_gating(code_squashed: &str) -> bool {
    code_squashed.contains("is_multiple_of(KSWAPD_TICK_INTERVAL)")
        || code_squashed.contains("is_multiple_of(HOTPLUG_TICK_INTERVAL)")
}

/// 编造/硬编码的计数器频率 —— 时基单一来源的反面.
fn has_fabricated_frequency(code_squashed: &str) -> bool {
    const NEEDLES: &[&str] = &[
        // 旧 CpuInfo 字段与它的 FFI 出口
        "tsc_frequency_hz",
        "cpu_get_tsc_frequency",
        // 旧 calibrate_tsc 的厂商兜底档位
        "calibrate_tsc(",
        "2_500_000_000",
        "3_000_000_000",
        // 旧 sleep 的假 2 GHz 折算, 以及直接传字面 MHz 的换算
        "ns*2",
        "cycles_to_nanoseconds(cycles,2000)",
    ];
    NEEDLES.iter().any(|n| code_squashed.contains(n))
}

/// 频率登记静态量的写入者 (用于"唯一写入点"检查).
fn writes_frequency_registry(code_squashed: &str) -> bool {
    code_squashed.contains("COUNTER_FREQ_HZ.store(")
}

/// 测定路径消费 tick 计数 (循环论证 / 启动早期死自旋).
///
/// needle 由片段拼出: 本测试文件不是被扫描对象, 但拼接能让意图显式化 —— 禁的
/// 是"把 tick 读数当时间窗口"这一数据流, 不是某个标识符.
fn measurement_consumes_tick_count(code_squashed: &str) -> bool {
    let needle = ["tick::get_", "ticks()"].concat();
    code_squashed.contains(&needle)
}

/// 时基建立的时序: `timer_init` → `establish_boot_timebase` → `scheduler::init`.
///
/// 返回 `None` 表示三者未全部出现或顺序不符 (建锚需 tick 频率已配置, 调度器的
/// 周期判断必须以单一时基为准).
///
/// 以建锚点为锚做邻近查找, 而不是用首次出现位置: lib.rs 的测试模式启动路径
/// (`kernel_test` / `host-test`) 也调了一次 `scheduler::init()`, 早于真机路径的
/// 定时器步骤, 按首次出现判序会把这条合法分支误判为回退.
fn timebase_wiring_position(lib_squashed: &str) -> Option<(usize, usize, usize)> {
    let established = lib_squashed.find("calibration::establish_boot_timebase()")?;
    let configured = lib_squashed[..established].rfind("timer::timer_init(")?;
    let sched = lib_squashed[established..].find("proc::scheduler::init()")?;
    Some((configured, established, established + sched))
}

// ============================================================================
// 不变式 1: 只有一份 tick 计数
// ============================================================================

#[test]
fn test_scheduler_no_longer_keeps_its_own_tick_count() {
    let sched = prod_squashed(SCHED);
    assert!(
        !has_dual_tick_counter(&sched),
        "P-1: 调度器重新自存 tick 计数副本 (与 timer 视图并行必然分叉)"
    );
    assert!(
        sched.contains("letnew_tick=crate::privileged::timer::get_ticks();"),
        "P-1: 调度器 tick 未改读单一时基权威读数"
    );
}

#[test]
fn test_oomd_and_tick_query_read_the_single_timebase() {
    let oomd = prod_squashed(OOMD);
    assert!(
        !has_dual_tick_counter(&oomd),
        "P-1: OOM 守护重新自存 tick 计数"
    );
    assert!(
        oomd.contains("crate::privileged::timer::get_ticks()"),
        "P-1: OOM 守护未改读单一时基"
    );

    // freg 的 tick 查询回调也必须指向同一来源, 否则又造出第三个视图.
    let tq = prod_squashed(TICK_QUERY);
    assert!(
        tq.contains("register_tick_query(crate::privileged::timer::get_ticks)"),
        "P-1: tick_query 回调源未指向单一时基"
    );
}

#[test]
fn test_tick_falls_back_to_irq_count_until_anchored() {
    let tick = prod_squashed(TICK);
    // 时基未建立 (测定失败) 时 get_ticks 必须回落到中断计数, 不能返回 0 常量.
    assert!(
        tick.contains("ifper_tick==0{returnTICK_IRQ_COUNT.load(Ordering::Acquire);"),
        "E-I: get_ticks 缺未建锚的回落视图 (启动早期时基不可用即读数全 0)"
    );
    // 已建锚时由计数器派生, 且锚点竞态窗口下必须饱和而非回绕 (单调性第一契约).
    assert!(
        tick.contains("counter_read().saturating_sub(anchor_counter)"),
        "E-I: 硬件派生读数缺单调保护 (重复建锚竞态会让 tick 倒退)"
    );
    assert!(
        tick.contains("COUNTERS_PER_TICK.load(Ordering::Acquire)!=0"),
        "E-I: timebase_is_hardware 判据不再是\"已建锚\", tickless 前置失去意义"
    );
}

// ============================================================================
// 不变式 2: 只有一个频率来源
// ============================================================================

#[test]
fn test_cpu_exposes_no_fabricated_frequency() {
    let cpu = prod_squashed(CPU);
    assert!(
        !has_fabricated_frequency(&cpu),
        "P-7: cpu 重新出现厂商硬编码频率或旧的频率 FFI 出口"
    );
    assert!(
        cpu.contains("pubfntsc_nominal_hz()->Option<u64>"),
        "P-7: CPUID 标称频率出口不再是 Option (调用方无法区分\"未报\"与\"报了\")"
    );
    assert!(
        cpu.contains("ifinfo.vendor!=CpuVendor::Intel||info.max_standard_leaf<0x15{returnNone;}"),
        "P-7: Leaf 0x15 不再只对 Intel 采信 (其它厂商该 leaf 内容不保证是 TSC 频率)"
    );
}

#[test]
fn test_frequency_registry_has_a_single_writer() {
    // 递归扫描内核树: 除 calibration.rs 外, 任何生产代码都不得写登记静态量.
    let mut writers: Vec<String> = Vec::new();
    let mut stack = vec![repo_kernel_root()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("遍历内核源码目录") {
            let path = entry.expect("目录项").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = fs::read_to_string(&path).expect("读取源码文件");
            if writes_frequency_registry(&squash(&prod_code(&src))) {
                writers.push(
                    path.canonicalize()
                        .expect("规范化路径")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }

    let canonical = PathBuf::from(CALIB)
        .canonicalize()
        .expect("calibration 路径");
    assert_eq!(
        writers,
        vec![canonical.to_string_lossy().into_owned()],
        "P-7: 频率登记出现第二写入点 (合理性区间校验会被绕过, 时基可被编造值带偏)"
    );
}

#[test]
fn test_all_timebase_consumers_read_registered_hz() {
    // 旧的假频率消费点清单: 逐个确认已改走单一时基.
    let sleep = prod_squashed(SLEEP);
    assert!(
        !has_fabricated_frequency(&sleep),
        "E-I: sleep 重新出现假频率折算 (忙等待可能少等, 硬件时序下限失效)"
    );
    assert!(
        sleep.contains("calibration::counter_hz_for_wait()"),
        "E-I: 忙等待未按登记频率折算"
    );
    assert!(
        sleep.contains("mul_div_saturating(ns,freq_hz,1_000_000_000)"),
        "E-I: 纳秒→周期折算不再走共用乘除底档"
    );
    assert!(
        sleep.contains("tick::get_uptime_ns()"),
        "E-I: measure_time 仍自行由周期数推纳秒 (与全局时基背离)"
    );

    let hrt = prod_squashed(HRTIMER);
    assert!(
        hrt.contains("tick::get_uptime_ns()"),
        "E-I: hrtimer 时钟源未接单一时基 (与 klog/sleep 时基背离)"
    );
    assert!(
        !hrt.contains("fnmul_u64_div("),
        "E-I: hrtimer 重新出现私有乘除换算 (第三份折算实现)"
    );

    let sgeg = prod_squashed(SGEG);
    assert!(
        sgeg.contains("calibration::tsc_to_microseconds(tsc)"),
        "E-I: sgeg 时标未走单一时基换算"
    );
    assert!(
        !has_fabricated_frequency(&sgeg),
        "E-I: sgeg 重新引入频率 FFI 桥 (旧 `tsc/freq*1_000_000` 先除后乘恒 0)"
    );

    let procfs = prod_squashed(PROCFS);
    assert!(
        procfs.contains("timer::hw_counter_frequency_hz()"),
        "E-I: /proc/cpuinfo 的 cpu MHz 未取登记值 (展示层假时基, P-3 同族)"
    );
    assert!(
        !procfs.contains("get_tsc_frequency_hz"),
        "E-I: 展示层重新绕过单一读取入口"
    );
}

#[test]
fn test_busy_wait_fallback_biases_towards_waiting_longer() {
    // 频率未知时按合理性上限折算 ⇒ 周期数偏多 ⇒ 只会多等. 反方向 (取下界或猜
    // 一个中间值) 会在真频更高时少等, 正是旧 `ns * 2` 的失效模式.
    let calib = prod_squashed(CALIB);
    assert!(
        calib.contains("hw_counter_frequency_hz().unwrap_or(COUNTER_HZ_MAX)"),
        "E-I: counter_hz_for_wait 的未知频率回落方向不再是合理性上限"
    );
    assert!(
        calib.contains("pubfncounter_hz_for_wait()->u64{"),
        "E-I: 忙等待折算入口缺失"
    );
}

// ============================================================================
// 不变式 3: 测定路径不消费被测定量
// ============================================================================

#[test]
fn test_measurement_does_not_consume_tick_count() {
    let calib = prod_squashed(CALIB);
    assert!(
        !measurement_consumes_tick_count(&calib),
        "E-I: 频率测定改回以 tick 计数作时间窗口 (循环论证, 且启动早期 IF 未开时永久自旋)"
    );
    // 参考时钟必须显式取自 PIT 倒计数器, 并带看门狗 (PIT 挂死时不得无界自旋).
    assert!(
        calib.contains("super::pit::pit_read_count()"),
        "E-I: 测定路径失去 PIT 参考时钟 (改用别的时基前请先证明它不依赖本测定)"
    );
    assert!(
        calib.contains("guard>PIT_WAIT_GUARD"),
        "E-I: 测定等待失去看门狗"
    );
}

// ============================================================================
// 不变式 4 + 接线: carry 化周期任务, 启动编排建立时基
// ============================================================================

#[test]
fn test_periodic_wakeup_is_carry_based() {
    let sched = prod_squashed(SCHED);
    assert!(
        !has_modulo_periodic_gating(&sched),
        "P-1: 周期任务回到取模判定 (时基跳点会永久漏触发)"
    );
    assert!(
        sched.contains("new_tick.saturating_sub(last_kswapd)>=KSWAPD_TICK_INTERVAL"),
        "E-I: kswapd 周期判断未改为 carry 比较"
    );
    assert!(
        sched.contains("new_tick.saturating_sub(last_hotplug)>=HOTPLUG_TICK_INTERVAL"),
        "E-I: hotplug 周期判断未改为 carry 比较"
    );
    // 多核看到同一个硬件派生读数 ⇒ 必须由单点 CAS 决定谁触发本轮.
    assert!(
        sched.contains("LAST_KSWAPD_TICK.compare_exchange(")
            && sched.contains("LAST_HOTPLUG_TICK.compare_exchange("),
        "E-I: carry 判定缺 CAS 单点化 (多核重复唤醒同一周期任务)"
    );
}

#[test]
fn test_boot_orchestration_establishes_timebase_in_order() {
    let lib = prod_squashed(LIB);
    assert!(
        timebase_wiring_position(&lib).is_some(),
        "P-7: 启动编排未按 timer_init → 建锚 → scheduler init 的顺序接线"
    );
    // 测定失败不得静默: tick 保持中断计数视图, tickless 由 timebase_is_hardware 拦住.
    assert!(
        lib.contains(r#"klog_warn!(Timer,"boottimebasenotestablished:{}"#),
        "P-7: 时基建立失败未记录 (静默丢弃违反 TD-13)"
    );
}

// ============================================================================
// 负控制: 谓词必须在回归真的发生时亮起
// ============================================================================

#[test]
fn test_negative_control_predicates_reject_regressions() {
    // 每个谓词配一段"回退形态"合成样本; 若某次改动让谓词失去判别力, 本用例先红.
    // 抽 `Fixture` 别名避免 `&str` + `fn(&str) -> bool` 内嵌元组达 clippy `type_complexity` 阈值.
    type Fixture<'a> = (&'a str, &'a str, for<'b> fn(&'b str) -> bool);
    let fixtures: &[Fixture<'_>] = &[
        (
            "dual_tick",
            "static TICK_COUNT: AtomicU64 = AtomicU64::new(0);",
            has_dual_tick_counter,
        ),
        (
            "modulo_gating",
            "if new_tick.is_multiple_of(KSWAPD_TICK_INTERVAL) { kswapd_wakeup(); }",
            has_modulo_periodic_gating,
        ),
        (
            "fabricated_hz",
            "let hz = if vendor == Amd { 2_500_000_000 } else { 0 };",
            has_fabricated_frequency,
        ),
        (
            "registry_writer",
            "COUNTER_FREQ_HZ.store(guessed, Ordering::Relaxed);",
            writes_frequency_registry,
        ),
        (
            "tick_window",
            "let t0 = super::tick::get_ticks();",
            measurement_consumes_tick_count,
        ),
    ];

    for (name, fixture, predicate) in fixtures {
        let squashed = squash(&prod_code(fixture));
        assert!(
            predicate(&squashed),
            "负控制失效: 谓词未识别合成回退样本 {name:?} —— 该门禁已形同虚设"
        );
    }

    // 顺序谓词同样要能拒绝错序 (建锚早于 timer_init, 或晚于 scheduler init).
    let early_anchor = squash(
        "calibration::establish_boot_timebase();
         timer::timer_init(DEFAULT_INTERRUPT_FREQ_HZ);
         proc::scheduler::init();",
    );
    assert!(
        timebase_wiring_position(&early_anchor).is_none(),
        "负控制失效: 顺序谓词未识别\"建锚早于 timer_init\""
    );
    let late_anchor = squash(
        "timer::timer_init(DEFAULT_INTERRUPT_FREQ_HZ);
         proc::scheduler::init();
         calibration::establish_boot_timebase();",
    );
    assert!(
        timebase_wiring_position(&late_anchor).is_none(),
        "负控制失效: 顺序谓词未识别\"建锚晚于 scheduler init\""
    );
}
