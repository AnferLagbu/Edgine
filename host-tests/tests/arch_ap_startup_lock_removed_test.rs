//! x86_64 AP 启动串行化 — 静态契约测试 (无锁 / 单一获取者)
//!
//! 追踪: deadlock 审计 HIGH 根治 (`privileged/arch/x86_64/smp_init.rs` 的
//! `AP_STARTUP_LOCK` 删除) + F-12 (显式 `cli`/`sti` 已移除)
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! `start_ap` 曾在函数体开头获取一把 `spin::mutex::SpinMutex` 类型的
//! `AP_STARTUP_LOCK`. 该锁既不做 IRQ save (故 `audit_deadlock_matrix.py` 只能
//! 保守报 HIGH —— 它无法静态证明"不存在第二个获取者"), 又把最长约 150ms/AP 的
//! INIT-SIPI 忙等轮询圈进持锁区间. 源码调研证明它没有任何竞争者:
//! `start_ap` 的唯一调用点是同文件 `init()` 里对 MADT AP 列表的串行循环, 而
//! `init()` 仅由 BSP 经 `Arch::interrupt_late_init()` 在 `kernel_main` 中调用
//! 一次; AP 走 `ap_entry`, 不进入启动路径. 无竞争者的锁不保护任何东西, 故删除.
//!
//! ## 为何用静态契约 (而非运行期断言)
//!
//! `privileged/arch/x86_64/*` 是裸机架构特有源码, host-tests 无法引用其符号,
//! 也无法在 host 上执行 INIT-SIPI. 与同类测试 `aarch64_smp_contract_test.rs`
//! 同手法, 以源码文本分析固化"调用图与锁形态"契约. 复刻常量做自证式断言被禁止,
//! 本测试不定义任何内核逻辑的平行实现.

use std::fs;
use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // Edgine workspace root
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

/// 截取 `src` 中 `begin` 起点到其后首个 `end` 之间的片段.
fn slice_between<'a>(src: &'a str, begin: &str, end: &str) -> &'a str {
    let start = src
        .find(begin)
        .unwrap_or_else(|| panic!("未找到起点标记: {}", begin));
    let rest = &src[start..];
    let end_off = rest
        .find(end)
        .unwrap_or_else(|| panic!("未找到终点标记: {}", end));
    &rest[..end_off]
}

/// 只保留代码行 (剔除以 `//` 开头的整行注释), 避免注释里的历史提法被当成代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

const SMP_INIT_RS: &str = "src/kernel/privileged/arch/x86_64/smp_init.rs";
const ARCH_MOD_RS: &str = "src/kernel/privileged/arch/x86_64/mod.rs";

/// `start_ap` 函数体 (到下一个函数 `send_init_ipi` 为止).
fn start_ap_body() -> String {
    let src = read(SMP_INIT_RS);
    slice_between(&src, "unsafe fn start_ap(", "unsafe fn send_init_ipi(").to_string()
}

/// `ap_entry` 函数体 (它是本文件最后一个函数, 取到文件尾即其完整体).
fn ap_entry_body() -> String {
    let src = read(SMP_INIT_RS);
    let start = src
        .find("extern \"C\" fn ap_entry(")
        .expect("未找到 ap_entry 定义");
    src[start..].to_string()
}

#[test]
fn ap_startup_lock_is_fully_removed() {
    let code = code_only(&read(SMP_INIT_RS));
    assert!(
        !code.contains("static AP_STARTUP_LOCK"),
        "AP_STARTUP_LOCK 声明不应存在: 无第二个获取者时它是纯冗余 (deadlock HIGH 根治依据)"
    );
    assert!(
        !code.contains("AP_STARTUP_LOCK.lock()"),
        "AP_STARTUP_LOCK 获取点不应存在"
    );
    assert!(
        !code.contains("use spin::mutex::SpinMutex"),
        "smp_init 不应再引入 spin::mutex::SpinMutex"
    );
}

#[test]
fn start_ap_body_has_no_lock_and_no_implicit_irq_masking() {
    let body = code_only(&start_ap_body());
    assert!(
        !body.contains(".lock()"),
        "start_ap 体内不应获取任何锁 (BSP 串行启动路径无竞争者)"
    );
    // F-12: 显式 cli / sti 已删除, 不得回到"靠关中断串行化"的旧形态
    // (注意 ap_entry 入口的 cli 属 AP 侧, 不在本函数体范围)
    assert!(
        !body.contains("asm!(\"cli\")"),
        "start_ap 不应重新引入显式 cli"
    );
    assert!(
        !body.contains("asm!(\"sti\")"),
        "start_ap 不应重新引入显式 sti"
    );
}

#[test]
fn start_ap_has_exactly_one_bsp_call_site() {
    let code = code_only(&read(SMP_INIT_RS));
    assert_eq!(
        count(&code, "unsafe fn start_ap("),
        1,
        "start_ap 定义应唯一"
    );
    // 唯一调用点: init() 内对 MADT AP 列表的串行循环
    assert_eq!(
        count(&code, "start_ap(ap.lapic_id, cpu_index);"),
        1,
        "start_ap 的唯一调用点应是 init() 的串行 AP 循环"
    );
    assert_eq!(
        count(&code, "start_ap(") - count(&code, "unsafe fn start_ap("),
        1,
        "除定义外, 全文件只允许一处 start_ap 调用 (多于 1 处即出现并发获取者, 须改 IRQ 安全锁)"
    );
}

#[test]
fn ap_entry_never_starts_another_ap() {
    // AP 侧路径不得进入启动逻辑 —— 这是"锁无竞争者"结论的另一半.
    let body = code_only(&ap_entry_body());
    assert!(
        !body.contains("start_ap("),
        "ap_entry 不应调用 start_ap (否则存在第二个获取者)"
    );
    assert!(
        !body.contains("smp_init::init("),
        "ap_entry 不应调用 smp_init::init"
    );
}

#[test]
fn ap_startup_entry_is_bsp_interrupt_late_init_only() {
    // init() 的唯一外部入口在 x86_64 Arch::interrupt_late_init() 内 (BSP boot 一次).
    let mod_src = read(ARCH_MOD_RS);
    assert_eq!(
        count(&mod_src, "smp_init::init();"),
        1,
        "smp_init::init() 应只有一个外部调用点"
    );
    let late = mod_src
        .find("fn interrupt_late_init()")
        .expect("未找到 interrupt_late_init");
    let call = mod_src
        .find("smp_init::init();")
        .expect("未找到 smp_init::init 调用点");
    assert!(
        call > late,
        "smp_init::init() 调用点应位于 interrupt_late_init 之内 (BSP 启动路径)"
    );
}

#[test]
fn reintroducing_a_startup_lock_must_be_irq_safe() {
    // 前瞻守卫: 若将来 (CPU hotplug 等) 重新引入启动锁, 必须是 IRQ 安全锁,
    // 不得回到 spin::mutex::SpinMutex —— 本测试用代码行判定, 不受注释影响.
    let code = code_only(&read(SMP_INIT_RS));
    if code.contains("AP_STARTUP_LOCK") {
        assert!(
            code.contains("IrqSpinLock"),
            "启动锁重新出现时必须是 IrqSpinLock (IRQ 安全), 且忙等轮询须移出临界区"
        );
    }
}
