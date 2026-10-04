//! Memory Pressure 归属契约测试 (P1-I-01 D9 → DECISION-O ② 反转)
//!
//! 契约随 DECISION-O ② (2026-09-13) 反转：MemoryPressure 类型/压力状态/
//! `update_pressure` 包装归 framework (机制持有, OOMD 是调度器 tick 直接驱动
//! 的机制组件); 分级阈值/算法留 services (策略), 经注册注入。
//!
//! 静态契约:
//! 1. MemoryPressure 枚举 / update_pressure / current_pressure 必在
//!    framework/mm/pressure.rs (机制权威)
//! 2. framework/mm/mod.rs 必声明 `pub mod pressure`
//! 3. framework/proc/oomd.rs 必引用 framework::mm::pressure, 禁止引用 services
//! 4. services/mm/memory_pressure.rs 必 deny unsafe_code, 保留阈值/分级算法/
//!    set_thresholds, 并 re-export framework API (services→framework 合法方向)
//! 5. services/mm/mod.rs init 必注册分级策略 (register_pressure_classifier)
//! 6. 4 级状态机变体 + 双重阈值 (绝对值 + 百分比) 契约保持
//! 7. services 文件不含 klog_ffi (避免 unsafe 边界)

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn framework_pressure_rs() -> String {
    let path = format!(
        "{}/../src/kernel/framework/mm/pressure.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read framework/mm/pressure.rs")
}

fn services_memory_pressure_rs() -> String {
    let path = format!(
        "{}/../src/kernel/services/mm/memory_pressure.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read services/mm/memory_pressure.rs")
}

fn framework_mm_mod_rs() -> String {
    let p = repo_root().join("src/kernel/framework/mm/mod.rs");
    fs::read_to_string(&p).expect("read framework/mm/mod.rs")
}

fn framework_oomd_rs() -> String {
    let p = repo_root().join("src/kernel/framework/proc/oomd.rs");
    fs::read_to_string(&p).expect("read framework/proc/oomd.rs")
}

fn services_mm_mod_rs() -> String {
    let path = format!(
        "{}/../src/kernel/services/mm/mod.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read services/mm/mod.rs")
}

/// 提取 `src` 中 `sig` 起始的顶层函数体 (至首个行首 `}` 结束)
fn fn_at_root<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let rest = &src[start..];
    let end = rest.find("\n}\n").map_or(rest.len(), |i| i + 2);
    &rest[..end]
}

/// 提取 `src` 中 `sig` 起始的**方法体** (按花括号配对定位结束, 不越界).
///
/// 与 `fn_at_root` 不同: 位于 `impl` 块内的方法, 其闭合花括号带缩进而非行首,
/// `fn_at_root` 会越过方法边界吞入后续方法, 故此处按花括号配对精确截取.
fn method_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let rest = &src[start..];
    let open = rest
        .find('{')
        .unwrap_or_else(|| panic!("函数签名无函数体: {sig}"));
    let mut depth = 0usize;
    for (i, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("函数花括号未配对: {sig}")
}

#[test]
fn memory_pressure_mechanism_in_framework() {
    // DECISION-O ② 验收: 类型/状态/update_pressure 包装必在 framework (机制权威)
    let src = framework_pressure_rs();
    assert!(
        src.contains("pub enum MemoryPressure"),
        "DECISION-O ②: MemoryPressure 枚举必在 framework/mm/pressure.rs"
    );
    assert!(
        src.contains("pub fn update_pressure"),
        "DECISION-O ②: update_pressure 包装必在 framework"
    );
    assert!(
        src.contains("pub fn current_pressure") && src.contains("pub fn previous_pressure"),
        "DECISION-O ②: 压力状态读取原语必在 framework"
    );
    assert!(
        src.contains("pub fn register_pressure_classifier"),
        "DECISION-O ②: 分级策略注册口必在 framework"
    );
}

#[test]
fn framework_mm_mod_declares_pressure() {
    // DECISION-O ② 验收: framework/mm/mod.rs 必声明 pressure 模块
    let src = framework_mm_mod_rs();
    assert!(
        src.contains("pub mod pressure"),
        "DECISION-O ②: framework/mm/mod.rs 必声明 pub mod pressure"
    );
}

#[test]
fn framework_oomd_uses_framework_pressure() {
    // DECISION-O ② 验收: OOMD 引用 framework::mm::pressure, 禁止 services 引用
    let src = framework_oomd_rs();
    assert!(
        src.contains("framework::mm::pressure::{MemoryPressure, update_pressure}"),
        "DECISION-O ②: oomd.rs 必从 framework::mm::pressure 引入"
    );
    assert!(
        !src.contains("kernel::services::"),
        "DECISION-O ②: framework/proc/oomd.rs 禁止引用 services (反向依赖清零)"
    );
}

/// C1 (T6 治理): OOMD Emergency 宽限期后必须**真实**发送 SIGKILL 至 RSS 最大进程
///
/// 回归保护: 治理前仅 `terminated_count += 1` + 打日志 (未真正 kill);
/// 若后续任何改动退回到"仅计数", 本用例失败.
#[test]
fn oomd_emergency_actually_sends_sigkill() {
    let src = framework_oomd_rs();
    assert!(
        src.contains("do_signal_send(victim, super::SIGKILL)"),
        "C1: OOMD Emergency 必须真实发送 SIGKILL (不可仅计数)"
    );
    assert!(
        src.contains("count_present_user_pages"),
        "C1: OOMD 必须经页表用户页计数选择 RSS 最大进程"
    );
    assert!(
        !src.contains("SIGKILL 发送至最大 RSS 进程待实现"),
        "C1: 待实现标记必须随治理移除"
    );
}

/// 顺序不变量: `do_signal_send` 不得在 `process_for_each` 闭包内调用
///
/// `do_signal_send` 内部会再次获取进程表锁, 而 `process_for_each` 已在闭包
/// 存续期间持有该锁 — 闭包内调用将自锁死. 该路径运行在 scheduler tick
/// (中断上下文, 关中断), 一旦自锁无法恢复, 属系统级死锁.
#[test]
fn oomd_sigkill_sent_outside_process_table_iteration() {
    let src = framework_oomd_rs();
    let start = src
        .find("process_for_each(")
        .expect("C1: OOMD 必须遍历进程表选择 victim");
    let end = src[start..]
        .find("});")
        .map(|i| start + i)
        .expect("C1: process_for_each 调用必须以 }); 结束");

    let iter_body = &src[start..end];
    assert!(
        !iter_body.contains("do_signal_send"),
        "C1: do_signal_send 不得在 process_for_each 闭包内调用 (会自锁进程表)"
    );
    assert!(
        src[end..].contains("do_signal_send"),
        "C1: do_signal_send 必须在 process_for_each 迭代结束之后调用"
    );
}

#[test]
fn memory_pressure_services_keeps_policy() {
    // DECISION-O ② 验收: 阈值/分级算法/set_thresholds 必留 services (策略)
    let src = services_memory_pressure_rs();
    assert!(
        src.contains("pub fn set_thresholds"),
        "DECISION-O ②: set_thresholds 必在 services"
    );
    assert!(
        src.contains("fn classify_pressure"),
        "DECISION-O ②: 分级算法 classify_pressure 必在 services"
    );
    assert!(
        src.contains("pub use crate::framework::mm::pressure::"),
        "DECISION-O ②: services 必经 re-export 保持 API 兼容 (services→framework 合法方向)"
    );
}

#[test]
fn memory_pressure_services_denies_unsafe() {
    // P1-I-01 D9 验收 (保持): services 文件必 deny unsafe_code
    let src = services_memory_pressure_rs();
    assert!(
        src.contains("#![deny(unsafe_code)]"),
        "DECISION-O ②: services/mm/memory_pressure.rs 必 #![deny(unsafe_code)]"
    );
}

#[test]
fn services_mm_init_registers_classifier() {
    // DECISION-O ② 验收: services::mm::init 必注册分级策略 (机制留注册口)
    let src = services_mm_mod_rs();
    assert!(
        src.contains("pub mod memory_pressure"),
        "DECISION-O ②: services/mm/mod.rs 必 pub mod memory_pressure"
    );
    assert!(
        src.contains("memory_pressure::register_pressure_classifier()"),
        "DECISION-O ②: services::mm::init 必注册分级策略"
    );
}

#[test]
fn memory_pressure_uses_4_level_state_machine() {
    // P1-I-01 D9 验收 (保持): 策略核心是 4 级状态机
    let src = framework_pressure_rs();
    // 必 4 个级别
    for variant in ["Normal", "Warning", "Critical", "Emergency"] {
        assert!(
            src.contains(&format!("    {} =", variant)),
            "DECISION-O ②: MemoryPressure 必含 {} 变体",
            variant
        );
    }
}

#[test]
fn memory_pressure_services_uses_double_threshold() {
    // P1-I-01 D9 验收 (保持): 双重阈值 (绝对值 + 百分比)
    let src = services_memory_pressure_rs();
    // 三个阈值常量
    assert!(
        src.contains("FREE_PAGES_THRESHOLD_WARNING"),
        "DECISION-O ②: 必含 warning 阈值"
    );
    assert!(
        src.contains("FREE_PAGES_THRESHOLD_CRITICAL"),
        "DECISION-O ②: 必含 critical 阈值"
    );
    assert!(
        src.contains("FREE_PAGES_THRESHOLD_EMERGENCY"),
        "DECISION-O ②: 必含 emergency 阈值"
    );
    // 阈值守卫: warning > critical > emergency
    assert!(
        src.contains("warning > critical && critical > emergency"),
        "DECISION-O ②: set_thresholds 必验证 warn > crit > emer 顺序"
    );
}

#[test]
fn memory_pressure_services_no_klog_ffi() {
    // P1-I-01 D9 验收 (保持): services 文件不含 klog_ffi (避免 unsafe 边界)
    let src = services_memory_pressure_rs();
    assert!(
        !src.contains("klog_ffi!"),
        "DECISION-O ②: services 文件不应含 klog_ffi! (触发 unsafe)"
    );
    assert!(
        !src.contains("crate::klog_ffi"),
        "DECISION-O ②: services 文件不应含 crate::klog_ffi"
    );
}

/// 丙批审查 A1 门槛 (非阻塞化): 用户页表遍历必须以**非阻塞**方式获取 `VMM_LOCK`.
///
/// 背景: `count_present_user_pages` 原为**无锁**遍历, 而 `unmap_page_in_table`
/// 在解除最后一个表项后会递归释放变空的中间页表 (`get_pmm().free_page`) —
/// 两者交错即踩野指针. 曾以"全程阻塞持锁"收口, 但 `VMM_LOCK` 的
/// `acquire_lock` 含"已持锁即直接返回"的单核短路, 多核下既不构成跨核互斥,
/// 也会把"他人持锁"误判为获取成功, 阻塞持锁因此不可信. 改为**非阻塞获取**:
/// 走一次 `compare_exchange`, 拿不到锁 (并发 map/unmap 持锁) 立即返回 `None`, 不争锁.
///
/// 本门槛锁定该不变量, 且**必须能拦住"改回阻塞持锁"的回退**:
/// - 正: 遍历函数体必出现 `try_acquire_lock` (非阻塞路径), 且先于 `release_lock(`;
/// - 负: 遍历函数体**不得**出现阻塞式 `acquire_lock()` (剔除 `try_acquire_lock`
///   子串后判定, 避免自匹配); 且文件必定义 `fn try_acquire_lock` 并内含 `compare_exchange`.
///
/// 页表并发行为需裸机 SMP 才能复现, host 无页表物理内存, 故以源码结构门槛
/// 收口; 计数正确性由 QEMU 用例 `mm::vmm::count_present_user_pages` (基线增量恒等)
/// 行为验证, 非阻塞 `None` 分支由本文件
/// `count_present_user_pages_returns_none_when_vmm_lock_held` 行为验证.
#[test]
fn user_page_count_traversal_uses_nonblocking_vmm_lock() {
    for (file, sig) in [
        (
            "src/kernel/framework/mm/vmm_x86_64.rs",
            "pub fn count_present_user_pages(",
        ),
        (
            "src/kernel/framework/mm/vmm_aarch64.rs",
            "pub fn count_present_user_pages(",
        ),
    ] {
        let src = fs::read_to_string(repo_root().join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"));
        let body = fn_at_root(&src, sig);
        // 剔除注释行后再判定, 避免"注释里写着 acquire_lock()"造成假通过
        // (负向验证: 把判据行改成注释后, 本用例必须变红).
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        // 正: 非阻塞获取路径必须先于释放.
        let acquire = code
            .find("try_acquire_lock")
            .unwrap_or_else(|| panic!("A1: {file} 遍历必须以非阻塞方式获取 VMM_LOCK"));
        let release = code
            .find("release_lock(")
            .unwrap_or_else(|| panic!("A1: {file} 遍历必须释放 VMM_LOCK"));
        assert!(
            acquire < release,
            "A1: {file} 必须先获取 VMM_LOCK 再释放 (次序不得颠倒)"
        );
        // 负: 不得回退为阻塞式 acquire_lock() — 剔除 try_acquire_lock 子串后判定.
        let without_try = code.replace("try_acquire_lock", "");
        assert!(
            !without_try.contains("acquire_lock()"),
            "A1: {file} 遍历不得使用阻塞式 acquire_lock() (非阻塞契约, 回退即拒收)"
        );
        assert!(
            !body.contains("不取 VMM 锁"),
            "A1: {file} 文档不得再声明无锁遍历 (锁契约已收紧)"
        );

        // 非阻塞助手本体必须用一次 CAS, 不退化回自旋/阻塞.
        let helper = fn_at_root(&src, "fn try_acquire_lock(");
        assert!(
            helper.contains("compare_exchange"),
            "A1: {file} try_acquire_lock 必须以一次 compare_exchange 实现 (非阻塞)"
        );
    }
}

/// 非阻塞 `None` 分支的**可执行**行为证据.
///
/// `VMM_LOCK` 是模块级全局锁 (与具体 VMM 实例无关), 故可先在 host 侧持锁,
/// 再调用 `count_present_user_pages`: 非阻塞路径必须立即返回 `None`, 且**不做
/// 任何遍历** — 传入非法页表根哨兵 `0xDEAD_BEEF`, 一旦遍历即踩野内存而 panic/
/// 段错误, 以此证明 "锁被占用时返回 None 且不 panic". 持锁与释放成对, 释放后
/// 全局锁状态复原 (不影响同进程其他用例).
#[test]
fn count_present_user_pages_returns_none_when_vmm_lock_held() {
    use edgine::kernel::framework::mm::vmm::{VirtualMemoryManager, count_present_user_pages};

    let vmm = VirtualMemoryManager::new();
    let flags = vmm.acquire_lock();
    // 0xDEAD_BEEF 非有效页表根: 仅在 "未获取锁即放弃 (不遍历)" 时才安全.
    let held = count_present_user_pages(0xDEAD_BEEF);
    vmm.release_lock(&flags);

    assert_eq!(
        held, None,
        "VMM_LOCK 被占用时 count_present_user_pages 必须返回 None 且不做遍历"
    );
}

/// P3-C 回归保护: `VMM_LOCK` 的 `acquire_lock` 必须保持**非重入自旋**语义, 且双架构不得发散.
///
/// 背景 (方案 C): x86_64 `acquire_lock` 曾含"单核可重入短路" — 已持锁时
/// `if VMM_LOCK.load(..) { return flags; }` 直接返回. 该短路在多核下既非跨核互斥
/// (他核持锁时本核被误判为获取成功而直接进临界区), 其配套的 `release_lock` 又会
/// 无条件 `store(false)` 释放他人持有的锁, 故定案移除; 已核实全部临界区区内不访问
/// 用户地址、也不嵌套取锁, 无同核递归路径. 本用例 fail-closed 拦两类回退:
/// - 防"短路回退": 两架构 `acquire_lock` 方法体**不得**再现 `VMM_LOCK.load(` 形式的中途返回;
/// - 防"双架构再次发散": 两者方法体**必须**以 `compare_exchange` 自旋获取, 且
///   `try_acquire_lock` 必须仍在且仍以一次 `compare_exchange` 实现 (非阻塞契约).
#[test]
fn vmm_lock_acquire_stays_non_reentrant_spinlock() {
    for file in [
        "src/kernel/framework/mm/vmm_x86_64.rs",
        "src/kernel/framework/mm/vmm_aarch64.rs",
    ] {
        let src = fs::read_to_string(repo_root().join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"));

        // 方法体内剔除注释行后再判定, 避免注释文本造成假通过.
        let code: String = method_body(&src, "pub fn acquire_lock(")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        // 负: 不得回退"单核可重入短路" (持锁即 VMM_LOCK.load 中途返回).
        assert!(
            !code.contains("VMM_LOCK.load("),
            "P3-C: {file} acquire_lock 不得含单核可重入短路 (VMM_LOCK.load 中途返回)"
        );
        // 正: 必须以 CAS 自旋获取锁.
        assert!(
            code.contains("compare_exchange"),
            "P3-C: {file} acquire_lock 必须以 compare_exchange 自旋获取"
        );

        // 双架构不得再次发散: 非阻塞助手仍在且仍以一次 compare_exchange 实现.
        let helper = method_body(&src, "fn try_acquire_lock(");
        assert!(
            helper.contains("compare_exchange"),
            "P3-C: {file} try_acquire_lock 必须以一次 compare_exchange 实现 (非阻塞)"
        );
    }
}
