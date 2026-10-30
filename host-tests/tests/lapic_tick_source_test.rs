//! E-II-b 回归锁: x86_64 tick 源由 PIT 迁到本核 LAPIC 一次性装填
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` 的 S-3 收益链 (E-II-b)。
//!
//! # 被锁死的六条不变式
//!
//! 1. **一个向量一个源, EOI 归属由向量决定**: tick 占本地向量段 `TICK_IRQ = 16`
//!    (向量 0x30), 不占 legacy 的 irq 0。legacy 段 (0x20-0x2F) 的 EOI 走 8259,
//!    LAPIC 的 ISR 位因此永不清除 —— tick 若仍打在 0x20, 首个 tick 后本核即停摆。
//!    向量号的真源在 `idt/types.rs` (向量空间的拥有者), `isr.asm` 的 stub 必须与
//!    它一致, 由本测试从常量反算后再核对汇编.
//! 2. **PIT 退役**: 不使能 PIC bit0, 且显式屏蔽; 通道 0 只作启动期频率测定的
//!    参考门 (纯端口读, 不依赖中断投递).
//! 3. **APIC 频率只有一个来源**: `apic.rs` 只剩寄存器原语, 标定/换算上收
//!    `timer::clockevent`; 旧实现借 PIT 门 + `timer_sleep_busy` 自旋作窗口,
//!    与 E-I 的单一时基构成第二份频率视图.
//! 4. **deadline 能力双确认**: CPUID 声明与 LAPIC 集成可能不一致 —— 只看 CPUID
//!    会在"报支持但忽略 bit18"的核上从此没有 tick. 必须再写后回读.
//! 5. **每核自装配 + 编排次序**: LAPIC 是每核私有寄存器, 只能本核写; 因此
//!    全局 tick 源参数 (`clockevent::setup`) 必须先于次级核上线发布, 使启动序列
//!    不存在"AP 先上线、参数未定"的窗口 (对齐 Linux `clocksource_init` → `cpu_up`),
//!    也就不需要超时等待与降级分支.
//! 6. **到达率与簿记率绑定**: 恢复周期 tick 时 `hz` 必须等于时基簿记频率
//!    (E-II-a 的 aarch64 教训: 10 ms 周期簿记 1000 Hz ⇒ 记账慢 10 倍).
//!
//! # 断言方式
//!
//! 源码文本契约 (剥注释 + 去空白), 只锁语义形状, 不受 rustfmt 换行影响; 需要
//! 位置关系的断言用未去空白的 `prod_lines` 取函数边界。判定逻辑收敛成 `has_*`
//! / `*_position` 谓词, 由 [`test_negative_control_predicates_reject_regressions`]
//! 用合成回退样本反向验证 —— 谓词对真实文件返"无回退"才有意义, 前提是它确实
//! 能在回归发生时亮起.

use std::fs;

const LIB: &str = "../src/kernel/lib.rs";
const IDT_TYPES: &str = "../src/kernel/privileged/idt/types.rs";
const IDT_MOD: &str = "../src/kernel/privileged/idt/mod.rs";
const ISR_ASM: &str = "../src/kernel/privileged/boot/isr.asm";
const TIMER_IRQ: &str = "../src/kernel/privileged/timer/irq.rs";
const CLOCKEVENT: &str = "../src/kernel/privileged/timer/clockevent.rs";
const TICKLESS: &str = "../src/kernel/functions/timer/tickless.rs";
const APIC: &str = "../src/kernel/privileged/arch/x86_64/apic.rs";
const FEATURE: &str = "../src/kernel/privileged/cpu/feature.rs";
const ARCH_X86: &str = "../src/kernel/privileged/arch/x86_64/mod.rs";
const ARCH_ARM: &str = "../src/kernel/privileged/arch/aarch64/mod.rs";
const SMP_INIT_X86: &str = "../src/kernel/privileged/arch/x86_64/smp_init.rs";

/// 本地向量段 tick stub 在 `isr.asm` 中的序号 (与 MSI 段 16-127 不重叠).
const LOCAL_TICK_STUB_INDEX: u32 = 128;

// ============================================================================
// 文本读取与规范化
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 剥除注释行 (Rust 形态): 生产代码与负控制样本走同一路径.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && !l.starts_with("/*") && !l.starts_with('*'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rust 生产代码文本: 剥掉注释行, 并在测试模块属性处截断.
///
/// 与 E-I 回归锁同一约定: 本轮修复在多处留下动机注释, 逐字提到被禁止的旧形态;
/// 单元测试维也合法地使用被禁形态 (host 下无法装填硬件).
///
/// 截断只在 `#[cfg(test)]` 紧接 `mod` 时生效: 部分文件 (如 idt/types.rs) 在顶部
/// 用 `#[cfg(test)] use ...` 为 host 测试引入类型, 无条件截断会把整个文件剔空.
fn prod_lines(path: &str) -> String {
    truncate_test_module(&strip_comments(&read(path)))
}

/// 在测试模块声明处截断文本 (见 [`prod_lines`]).
fn truncate_test_module(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#[cfg(test)]") || line.starts_with("#[cfg(all(test") {
            let next = lines.get(i + 1).unwrap_or(&"");
            if next.starts_with("mod ") || next.starts_with("pub mod ") {
                return lines[..i].join("\n");
            }
        }
    }
    src.to_string()
}

/// 去掉全部空白, 使形状断言不受换行与缩排影响.
fn squash(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

fn prod_squashed(path: &str) -> String {
    squash(&prod_lines(path))
}

/// 汇编源文本: `isr.asm` 的注释以 `;` 起始, 与 Rust 分支剥法不同.
fn asm_lines(path: &str) -> String {
    read(path)
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with(';'))
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// 谓词 (可被负控制反向验证)
// ============================================================================

/// tick 仍注册在 legacy irq 0 (PIT 通道 0) 上.
fn has_legacy_timer_registration(code_squashed: &str) -> bool {
    code_squashed.contains("register_irq(0,timer_tick_handler")
        || code_squashed.contains("register_irq(0,")
}

/// 解屏蔽 PIC bit0 —— PIT 重新投递 tick 的开关.
fn unmasks_pic_timer_irq(code_squashed: &str) -> bool {
    code_squashed.contains("enable_irq(0)")
}

/// `apic.rs` 里重新长出频率标定/查询面 (与单一时基构成第二份频率视图).
fn has_apic_calibration_surface(code_squashed: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "fncalibratetimer(",
        "fngettimerhz(",
        "fnistimercalibrated(",
        "fninit_timer(",
        "APIC_TIMER_HZ",
        "APIC_TIMER_CALIBRATED",
    ];
    NEEDLES.iter().any(|n| code_squashed.contains(n))
}

/// clockevent 的频率不从单一时基登记取, 或重新借 PIT 门自旋.
fn clockevent_hz_off_single_source(code_squashed: &str) -> bool {
    !code_squashed.contains("calibration::hw_counter_frequency_hz()")
        || code_squashed.contains("pit::")
}

/// deadline 模式只看 CPUID (或只看回读) 而未双确认.
///
/// 不钉定具体写法: 两腿必须都在且由 `&&` 与上 (先取 CPUID 再存中间变量同样成立).
fn deadline_single_check(code_squashed: &str) -> bool {
    !(code_squashed.contains("cpu_supports_deadline()")
        && code_squashed.contains("&&apic::timer_deadline_supported()"))
}

/// 装填后无到期闭环 / 闭环判据取"等真投递"而非 IRR 挂起位 / 在启动期开了中断窗口.
///
/// 启动全程 `IF=0` (`boot.asm` 起即 `cli`, 直到进 Ring 3 才由 iretq 置位): 等真投递
/// 就得在初始化临界区开 `sti` 窗口, 而那会把 tick handler (含 `poll_network` 与其它
/// 锁) 强行插进"设计上单线程无中断"的启动路径 —— 实测 e2e 由 3/3 通过退化为 3/3
/// 失败. IRR 则不依赖 IF: LAPIC 在到期那一刻就把向量记到本核 IRR, 是唯一不破坏并发
/// 前提又能证明"定时器真在跑"的判据.
fn lacks_expiry_closed_loop(code_squashed: &str) -> bool {
    !code_squashed.contains("verify_expiry_observed()")
        || !code_squashed.contains("apic::apic_is_in_irr(TICK_VECTOR)")
        || !code_squashed.contains("TICK_DELIVERED.fetch_add(1")
        || code_squashed.contains("interrupt_enable()")
}

/// tick 装填绕开 clockevent, 各自编程 LAPIC (重复实现 + 向量各写一份).
fn tickless_programs_hw_directly(code_squashed: &str) -> bool {
    code_squashed.contains("apic::set_timer_count(")
        || code_squashed.contains("apic::program_timer(")
        || code_squashed.contains("0x20")
}

/// 恢复周期 tick 时不再校验到达率与簿记率一致.
fn drops_tick_rate_guard(code_squashed: &str) -> bool {
    !code_squashed.contains("ifhz!=bookkeeping{returnErr(")
}

/// ISR 尾部不续期 (one-shot 语义下等价于"只来一个 tick").
fn handler_does_not_rearm(code_squashed: &str) -> bool {
    !code_squashed.contains("clockevent::rearm_local()")
}

/// tick 续期 (hardirq) 路径里打日志.
///
/// 中断里做格式化 + 串口发射既拉长中断时长 (P-3 取证实测一条 60+ 字符日志 ≈ 5 ms
/// IRQ-off 自扰动), 又把 klog 的两级栈缓冲区压在被中断现场的栈上. 取证信号必须留在
/// 启动上下文.
fn isr_path_prints(code_squashed: &str) -> bool {
    fn_body(code_squashed, "fnrearm_local()").is_some_and(|body| body.contains("klog"))
}

/// 取 `sig` 函数体的文本 (花括号配对; 输入须已剔注释并 squash).
///
/// 只用于"体内不得出现 X"类断言 —— 全文 `contains` 会把别处的合法用法误判为违规.
/// sig 写 squash 后的形态 (无空格), 使其只能命中定义而不命中调用点.
fn fn_body<'a>(code: &'a str, sig: &str) -> Option<&'a str> {
    let at = code.find(sig)?;
    let open = code[at..].find('{')? + at;
    let mut depth = 0usize;
    for (i, ch) in code[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&code[open..=(open + i)]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 从 `idt/types.rs` 取一个 `pub const NAME: u8 = N;` 常量值.
fn const_u8(path: &str, name: &str) -> u32 {
    let src = read(path);
    let pat = format!("pub const {name}: u8 = ");
    let at = src
        .find(&pat)
        .unwrap_or_else(|| panic!("{path} 中未找到常量 {name}"));
    src[at + pat.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse::<u32>()
        .unwrap_or_else(|e| panic!("常量 {name} 解析失败: {e}"))
}

/// `isr.asm` 中是否存在"序号 + 向量"与给定值一致的本地 tick stub.
fn has_local_tick_stub(asm_squashed: &str, index: u32, vector: u32) -> bool {
    asm_squashed.contains(&format!("irq_stub{index},{vector}"))
}

/// 启动编排位置关系: 时基登记 → 本核装填 → 次级核上线 → 调度器.
///
/// 返回 `None` 表示四者未全部出现或次序不符。以 `clockevent::setup()` 为锚做
/// 邻近查找而不是取首次出现位置: lib.rs 的测试模式启动路径也调了
/// `proc::scheduler::init()`, 按首次出现判序会把那条合法分支误判为回退.
/// 真机路径现经 `functions::proc::init()` 组合入口调用调度器初始化.
fn orchestration_position(lib_lines: &str) -> Option<(usize, usize, usize, usize)> {
    let lib = squash(lib_lines);
    let setup = lib.find("timer::clockevent::setup()")?;
    let anchored = lib[..setup].rfind("calibration::establish_boot_timebase()")?;
    let bringup = setup + lib[setup..].find("::secondary_cpu_bringup()")?;
    let sched = bringup + lib[bringup..].find("functions::proc::init()")?;
    Some((anchored, setup, bringup, sched))
}

/// `interrupt_late_init()` 体内仍拉起次级核 (与 tick 源参数发布构成竞态窗口).
fn bringup_inside_late_init(arch_lines: &str) -> bool {
    let Some(start) = arch_lines.find("fn interrupt_late_init()") else {
        return false;
    };
    let rest = &arch_lines[start..];
    let end = rest
        .find("fn secondary_cpu_bringup()")
        .unwrap_or(rest.len());
    rest[..end].contains("smp_init::init()")
}

/// AP 自装配本核 tick 的位置: 必在 `register_cpu` 之后 (索引可用), done 握手之前.
fn ap_self_arm_position(smp_lines: &str) -> Option<(usize, usize, usize)> {
    let smp = squash(smp_lines);
    let registered = smp.find("smp::register_cpu(lapic_id)")?;
    let armed = registered + smp[registered..].find("clockevent::arm_local()")?;
    let done = armed + smp[armed..].find("write_volatile(done_ptr")?;
    Some((registered, armed, done))
}

// ============================================================================
// 不变式 1: 一个向量一个源, EOI 归属由向量决定
// ============================================================================

#[test]
fn test_tick_vector_single_source_and_idt_gate() {
    let irq = const_u8(IDT_TYPES, "TICK_IRQ");
    let base = const_u8(IDT_TYPES, "IRQ_BASE");
    assert_eq!(irq, 16, "tick 必须占本地向量段首个槽 (legacy 0-15 归 8259)");

    // 向量号只有一个定义处: TICK_VECTOR 由 IRQ_BASE + TICK_IRQ 算出, 不另写字面量.
    let types = prod_squashed(IDT_TYPES);
    assert!(
        types.contains("pubconstTICK_VECTOR:u8=IRQ_BASE+TICK_IRQ;"),
        "TICK_VECTOR 未由 IRQ_BASE + TICK_IRQ 派生"
    );

    // ISR stub 的向量号必须与常量一致 (跨文件一致性由常量反算).
    let asm = squash(&asm_lines(ISR_ASM));
    let vector = base + irq;
    assert!(
        has_local_tick_stub(&asm, LOCAL_TICK_STUB_INDEX, vector),
        "isr.asm 缺少与 TICK_VECTOR ({vector}) 一致的本地 tick stub"
    );

    // 门编程在 idt_init 路径内, 且用的是同一常量对.
    // stub 地址统一由 `stub_addr!` 换算 (与 `stub_addrs()` 同一套 fn 指针→高半区地址语义).
    let idt_mod = prod_squashed(IDT_MOD);
    assert!(
        idt_mod.contains("init_local_vector(TICK_VECTOR,stub_addr!(irq128))"),
        "IDT 未为 TICK_VECTOR 编程门 (首个 tick 无门 → #GP → #DF)"
    );
}

#[test]
fn test_tick_registered_on_local_vector_not_legacy_irq0() {
    let irq = prod_squashed(TIMER_IRQ);
    assert!(
        !has_legacy_timer_registration(&irq),
        "tick 重新注册到 legacy irq 0: EOI 走 8259, LAPIC ISR 永不清除"
    );
    assert!(
        irq.contains("register_irq(TICK_IRQ,timer_tick_handler"),
        "tick 未注册到 TICK_IRQ 槽"
    );
    // ISR 尾部必须续期 (one-shot 到期即停).
    assert!(
        !handler_does_not_rearm(&irq),
        "tick handler 尾部缺少 clockevent::rearm_local()"
    );
}

// ============================================================================
// 不变式 2: PIT 退役
// ============================================================================

#[test]
fn test_pit_retired_from_tick_delivery() {
    let irq = prod_squashed(TIMER_IRQ);
    assert!(
        !unmasks_pic_timer_irq(&irq),
        "重新 enable_irq(0): PIT 又会向 8259 投递 tick"
    );
    assert!(
        irq.contains("IdtManager::disable_irq(0)"),
        "PIT 退役未显式屏蔽 PIC bit0 (不得依赖上游 boot 的 mask 状态)"
    );
}

// ============================================================================
// 不变式 3: APIC 频率只有一个来源
// ============================================================================

#[test]
fn test_apic_keeps_only_register_primitives() {
    let apic = prod_squashed(APIC);
    assert!(
        !has_apic_calibration_surface(&apic),
        "apic.rs 重新长出标定/频率查询面 (与单一时基构成第二份频率视图)"
    );
    // 机制层只保留的原语 (被 clockevent 消费).
    for primitive in [
        "pubfnprogram_timer(mode:TimerMode,vector:u8)",
        "pubfnset_deadline(deadline:u64)",
        "pubfntimer_deadline_supported()->bool",
        "pubfnmask_timer()",
    ] {
        assert!(
            apic.contains(primitive),
            "apic.rs 缺少 tick 源所需原语: {primitive}"
        );
    }
}

#[test]
fn test_clockevent_hz_comes_from_single_timebase() {
    let ce = prod_squashed(CLOCKEVENT);
    assert!(
        !clockevent_hz_off_single_source(&ce),
        "clockevent 的频率来源偏离单一时基登记 (或重新借 PIT 门自旋)"
    );
    // 每 tick 计数增量也只认 tick 模块的单源.
    assert!(
        ce.contains("tick::counters_per_tick()"),
        "clockevent 未从单一时基取每 tick 计数增量"
    );
    // 标定属策略层: 换算不得回落到 arch 层.
    assert!(
        !apic_has_conversion(&ce),
        "clockevent 与 apic 之间换算职责倒置"
    );
}

/// clockevent 若在 `apic.rs` 里再实现一遍换算/测量, 会出现对 apic 私有静态的引用.
fn apic_has_conversion(_ce: &str) -> bool {
    prod_squashed(APIC).contains("mul_div_saturating(")
        || prod_squashed(APIC).contains("timer_sleep_busy")
}

// ============================================================================
// 不变式 4: deadline 能力双确认
// ============================================================================

#[test]
fn test_deadline_requires_cpuid_and_readback() {
    let ce = prod_squashed(CLOCKEVENT);
    assert!(
        !deadline_single_check(&ce),
        "deadline 模式未同时要求 CPUID 声明与 LAPIC 写后回读"
    );
    assert!(
        !lacks_expiry_closed_loop(&ce),
        "装填后缺少 IRR 到期闭环 (AMD 本机无 TSC-deadline 能力, deadline 分支只能靠闭环在 Intel 真机取证)"
    );
    let feat = prod_squashed(FEATURE);
    assert!(
        feat.contains("constTSCDEADLINE=1<<56;"),
        "CpuFeatures 未声明 TSCDEADLINE 位 (Leaf1 ECX bit24 → 1<<56)"
    );
    assert!(
        feat.contains("ifecx&(1<<24)!=0{") && feat.contains("insert(CpuFeatures::TSCDEADLINE)"),
        "feature.rs 未从 Leaf1 ECX bit24 解析 TSCDeadline"
    );
    // 回读探测本身必须保持屏蔽且不带向量号 (探测期间的误投递不可接受).
    let apic = prod_squashed(APIC);
    assert!(
        apic.contains("apic_write(APIC_LVT_TIMER,LVT_MASK|LVT_TIMER_TSC_DEADLINE)")
            && apic.contains("letaccepted=apic_read(APIC_LVT_TIMER)&LVT_TIMER_TSC_DEADLINE!=0"),
        "timer_deadline_supported 未采用\"屏蔽写入 + 回读判定\"序列"
    );
}

// ============================================================================
// 不变式 5: 每核自装配 + 编排次序
// ============================================================================

#[test]
fn test_orchestration_publishes_tick_source_before_cpu_up() {
    let pos = orchestration_position(&prod_lines(LIB)).expect(
        "启动编排缺少 \"时基登记 → clockevent::setup → secondary_cpu_bringup → scheduler\" 链条",
    );
    assert!(
        pos.0 < pos.1 && pos.1 < pos.2 && pos.2 < pos.3,
        "启动编排次序不符: {pos:?} (次级核上线必须晚于全局 tick 源参数发布)"
    );

    // 次级核上线已从两架构的 interrupt_late_init 移出, 且各有实现.
    for (path, name) in [(ARCH_X86, "x86_64"), (ARCH_ARM, "aarch64")] {
        let lines = prod_lines(path);
        assert!(
            !bringup_inside_late_init(&lines),
            "{name}: interrupt_late_init 仍拉起次级核 (与 tick 源参数发布构成竞态窗口)"
        );
        assert!(
            lines.contains("fn secondary_cpu_bringup()"),
            "{name}: 未实现 InterruptArch::secondary_cpu_bringup"
        );
    }
}

#[test]
fn test_ap_arms_own_tick_after_cpu_registration() {
    let pos = ap_self_arm_position(&prod_lines(SMP_INIT_X86))
        .expect("AP 路径缺少 clockevent::arm_local() 自装配");
    assert!(
        pos.0 < pos.1 && pos.1 < pos.2,
        "AP 自装配位置不符: {pos:?} (必在 register_cpu 之后、done 握手之前)"
    );

    // 装填状态不得反查 tickless 策略 (续期处在 hardirq, 少一次跨模块加锁就少一条锁序边).
    let ce = prod_squashed(CLOCKEVENT);
    assert!(
        !ce.contains("tickless::"),
        "clockevent 反向查询 tickless 策略状态 (在 hardirq 续期路径引入跨模块锁)"
    );
}

// ============================================================================
// 不变式 6: 装填职责单一 + 到达率与簿记率绑定
// ============================================================================

#[test]
fn test_tickless_delegates_programming_to_clockevent() {
    let tk = prod_squashed(TICKLESS);
    assert!(
        !tickless_programs_hw_directly(&tk),
        "tickless 重新直接编程 LAPIC 或写死向量 0x20 (装填职责必须单一)"
    );
    assert!(
        tk.contains("clockevent::program_oneshot_ns(delta_ns)")
            && tk.contains("clockevent::arm_periodic(hz)"),
        "tickless 未把单次/周期装填收口到 clockevent"
    );
    // 频率调整同样要先与簿记频率对齐, 否则名义 hz 与实际错位.
    assert!(
        tk.contains("ifhz!=get_frequency(){returnfalse;}"),
        "set_hz 未前置校验频率一致性"
    );
}

#[test]
fn test_periodic_resume_guards_tick_rate() {
    let ce = prod_squashed(CLOCKEVENT);
    assert!(
        !drops_tick_rate_guard(&ce),
        "恢复周期 tick 时不再校验 hz == 簿记频率 (E-II-a 同类缺陷)"
    );
}

#[test]
fn test_tick_isr_path_stays_silent() {
    let ce = prod_squashed(CLOCKEVENT);
    assert!(
        !isr_path_prints(&ce),
        "tick 续期路径 (hardirq) 里打日志: 取证信号应留在启动上下文"
    );
    // 反面保证本用例不是在扫空体: 闭环自检必须在启动上下文里打正面信号.
    let verify = fn_body(&ce, "fnverify_expiry_observed()").expect("verify_expiry_observed 不存在");
    assert!(
        verify.contains("klog_boot_info!"),
        "到期闭环无启动上下文的正面信号 (Intel 真机 deadline 分支失去取证口)"
    );
}

// ============================================================================
// 负控制: 谓词必须在回归真的发生时亮起
// ============================================================================

#[test]
fn test_negative_control_predicates_reject_regressions() {
    assert_fixture_predicates_accept_regressions();
    assert_predicates_reject_correct_shapes();
    assert_isr_stub_vector_mismatch_rejected();
}

fn assert_fixture_predicates_accept_regressions() {
    // 抽 `Fixture` 别名避免 `&str` + `fn(&str) -> bool` 内嵌元组达 clippy `type_complexity` 阈值.
    type Fixture<'a> = (&'a str, &'a str, for<'b> fn(&'b str) -> bool);
    let fixtures: &[Fixture<'_>] = &[
        (
            "legacy_irq0",
            "manager.register_irq(0, timer_tick_handler, \"PIT\", 0)?;",
            has_legacy_timer_registration,
        ),
        (
            "pic_unmask",
            "manager.enable_irq(0);",
            unmasks_pic_timer_irq,
        ),
        (
            "apic_calibration",
            "pub fn calibrate_timer() { APIC_TIMER_HZ.store(hz, Ordering::Relaxed); }",
            has_apic_calibration_surface,
        ),
        (
            "deadline_cpuid_only",
            "if cpu_supports_deadline() { MODE.store(MODE_DEADLINE, Ordering::Release); }",
            deadline_single_check,
        ),
        (
            "no_closed_loop",
            "pub fn setup() { arm_local(); } fn rearm_local() { write_expiry(next); }",
            lacks_expiry_closed_loop,
        ),
        (
            "sti_window_breaks_boot",
            "fn verify_delivery() { TICK_DELIVERED.fetch_add(1, Ordering::AcqRel); \
             interrupt_enable(); spin_until_tick(); }",
            lacks_expiry_closed_loop,
        ),
        (
            "poll_delivery_only",
            "fn verify_expiry_observed() { while tick::get_ticks() == base {} }",
            lacks_expiry_closed_loop,
        ),
        (
            "tickless_direct_hw",
            "apic::set_timer_count(counts); apic::program_timer(TimerMode::OneShot, 0x20);",
            tickless_programs_hw_directly,
        ),
        (
            "no_rearm",
            "pub extern \"C\" fn timer_tick_handler(_f: *mut InterruptFrame) { on_timer_interrupt(); }",
            handler_does_not_rearm,
        ),
        (
            "isr_print",
            "pub fn rearm_local() { tick::counter_now(); klog_boot_info!(\"first tick\"); }",
            isr_path_prints,
        ),
        (
            "no_rate_guard",
            "pub fn arm_periodic(hz: u32) -> Result<(), &'static str> { arm_local(); Ok(()) }",
            drops_tick_rate_guard,
        ),
    ];

    for (name, fixture, predicate) in fixtures {
        let squashed = squash(&strip_comments(fixture));
        assert!(
            predicate(&squashed),
            "负控制失效: 谓词未识别合成回退样本 {name:?} —— 该门禁已形同虚设"
        );
    }
}

fn assert_predicates_reject_correct_shapes() {
    // 反向: 正确形态不得亮起.
    let good = squash(
        "manager.register_irq(TICK_IRQ, timer_tick_handler, \"LAPIC Timer\", 0)?;
         manager.disable_irq(0);
         crate::privileged::timer::clockevent::rearm_local();",
    );
    assert!(
        !has_legacy_timer_registration(&good) && !unmasks_pic_timer_irq(&good),
        "负控制失效: 正确形态被误判为回退"
    );

    // 时钟源频率偏离单一时基 (借 PIT 门) 也要能被识别.
    let pit_based = squash(
        "let counter_hz = crate::privileged::timer::pit::channel0_hz();
         let _ = calibration::hw_counter_frequency_hz();",
    );
    assert!(
        clockevent_hz_off_single_source(&pit_based),
        "负控制失效: 未识别 clockevent 重新借 PIT 取频率"
    );

    // 顺序谓词: 次级核上线早于参数发布必须被拒 (上线后无参数可取).
    let early_cpu_up = "calibration::establish_boot_timebase();
         <Arch>::secondary_cpu_bringup();
         timer::clockevent::setup();
         functions::proc::init();";
    assert!(
        orchestration_position(early_cpu_up).is_none(),
        "负控制失效: 顺序谓词未识别\"次级核上线早于 tick 源参数发布\""
    );

    // bringup 谓词: 仍在 interrupt_late_init 内拉起次级核必须被拒.
    let late_init_with_bringup = "fn interrupt_late_init() {\n    apic_init();\n    smp_init::init();\n}\nfn secondary_cpu_bringup() {\n}";
    assert!(
        bringup_inside_late_init(late_init_with_bringup),
        "负控制失效: 未识别 interrupt_late_init 仍拉起次级核"
    );

    // AP 自装配位置: 早于 register_cpu (索引未就绪) 必须被拒.
    let arm_too_early = squash(
        "clockevent::arm_local();
         smp::register_cpu(lapic_id);
         write_volatile(done_ptr, 1);",
    );
    assert!(
        ap_self_arm_position(&arm_too_early).is_none(),
        "负控制失效: 未识别 AP 在 register_cpu 之前自装配"
    );
}

fn assert_isr_stub_vector_mismatch_rejected() {
    // isr.asm 一致性: 向量号与常量不符必须被拒.
    let asm_wrong_vector = squash("irq_stub 128, 32");
    assert!(
        !has_local_tick_stub(&asm_wrong_vector, LOCAL_TICK_STUB_INDEX, 48),
        "负控制失效: stub 谓词未识别向量号与常量不一致"
    );
}
