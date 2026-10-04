// SPDX-License-Identifier: MPL-2.0
// TD-11/12/13: 把 idt/handlers.rs、idt/idt.rs、timer/mod.rs 里的 5 处
// "未接 klog 宏的占位" 全部接上 klog 宏.
//
// 验收:
//   - handlers.rs::print_detailed_gpf_info 不再有 let _ = 占位, 改用 klog_warn!
//   - handlers.rs::print_double_fault_context 不再有 let _ = 占位, 改用 klog_err!
//   - idt.rs::print_stack_trace 已随旧异常路径移除 (handlers.rs 取代)
//   - idt.rs::dump_state 不再有 let _ = 占位, 改用 klog_info!
//   - idt.rs::print_statistics 不再只 let _ = &self.stats, 改为按异常/IRQ 循环 klog
//   - timer/mod.rs::timer_init_ffi 不再有 let _ = msg, 改用 klog_err!
//   - LogCategory 新增 Timer = 14 变体 (供 timer 路径使用)

use std::fs;

const KLOG: &str = "../src/kernel/privileged/klog/mod.rs";
const HANDLERS: &str = "../src/kernel/privileged/idt/handlers.rs";
const IDT: &str = "../src/kernel/privileged/idt/idt.rs";
const TIMER: &str = "../src/kernel/privileged/timer/mod.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 取下一个 `}` 闭合, 避免注释里出现 `}` 干扰.
fn find_block_end(src: &str, start: usize) -> usize {
    let bytes = src.as_bytes();
    let mut depth: i32 = 0;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] as char {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    src.len()
}

#[test]
fn test_td11_gpf_uses_klog_warn() {
    let src = read(HANDLERS);
    let marker = "fn print_detailed_gpf_info";
    let pos = src.find(marker).expect("print_detailed_gpf_info 必须存在");
    let body_end = find_block_end(&src, pos);
    let body = &src[pos..body_end];
    // 必须不再保留 let _ = 占位, 且必须调用 klog_warn!
    assert!(
        !body.contains("let _ = (external,"),
        "GPF 不能保留 let _ = (external, ...) 占位, 应替换为 klog"
    );
    assert!(
        body.contains("klog_warn!") && body.contains("Kernel,"),
        "GPF 必须用 klog_warn!(Kernel, ...) 记录 selector/external/idt_flag/index"
    );
}

#[test]
fn test_td11_double_fault_uses_klog_err() {
    let src = read(HANDLERS);
    let marker = "fn print_double_fault_context";
    let pos = src
        .find(marker)
        .expect("print_double_fault_context 必须存在");
    let body_end = find_block_end(&src, pos);
    let body = &src[pos..body_end];
    assert!(
        !body.contains("let _ = (count, nesting)"),
        "DoubleFault 不能保留 let _ = (count, nesting) 占位"
    );
    assert!(
        body.contains("klog_err!") && body.contains("Kernel,"),
        "DoubleFault 必须用 klog_err!(Kernel, ...) 记录 count/nesting"
    );
}

#[test]
fn test_td12_stack_trace_removed_with_old_exception_path() {
    // print_stack_trace 已随旧异常处理路径一并移除 (被 handlers.rs 的 create_handler 取代)
    let src = read(IDT);
    let marker = "fn print_stack_trace";
    assert!(
        src.find(marker).is_none(),
        "print_stack_trace 应已移除 (旧异常路径已由 handlers.rs 取代)"
    );
}

#[test]
fn test_td12_dump_state_uses_klog_info() {
    let src = read(IDT);
    let marker = "pub fn dump_state";
    let pos = src.find(marker).expect("dump_state 必须存在");
    let body_end = find_block_end(&src, pos);
    let body = &src[pos..body_end];
    assert!(
        !body.contains("let _ = (nesting,"),
        "dump_state 不能保留 let _ = (nesting, current_vec, ...) 占位"
    );
    assert!(
        body.contains("klog_info!") && body.contains("Kernel,"),
        "dump_state 必须用 klog_info!(Kernel, ...) 输出 nesting/current_vec/descriptors"
    );
}

#[test]
fn test_td12_print_statistics_walks_counters() {
    let src = read(IDT);
    let marker = "pub fn print_statistics";
    let pos = src.find(marker).expect("print_statistics 必须存在");
    let body_end = find_block_end(&src, pos);
    let body = &src[pos..body_end];
    // 旧占位只剩 let _ = &self.stats; 新版必须遍历 exception_counts / irq_counts.
    assert!(
        !body.contains("let _ = &self.stats;"),
        "print_statistics 不能保留 let _ = &self.stats; 占位"
    );
    assert!(
        body.contains("exception_counts"),
        "print_statistics 必须遍历 exception_counts"
    );
    assert!(
        body.contains("irq_counts"),
        "print_statistics 必须遍历 irq_counts"
    );
    assert!(
        body.contains("klog_info!") && body.contains("Kernel,"),
        "print_statistics 每条非零计数都应通过 klog_info! 记录"
    );
}

#[test]
fn test_td13_timer_init_ffi_uses_klog_err() {
    let src = read(TIMER);
    let marker = "fn timer_init_ffi";
    let pos = src.find(marker).expect("timer_init_ffi 必须存在");
    let body_end = find_block_end(&src, pos);
    let body = &src[pos..body_end];
    assert!(
        !body.contains("let _ = msg;"),
        "timer_init_ffi 错误分支不能保留 let _ = msg; 占位"
    );
    assert!(
        body.contains("klog_err!") && body.contains("Timer,"),
        "timer_init_ffi 错误分支必须用 klog_err!(Timer, ...) 记录 msg"
    );
}

#[test]
fn test_log_category_has_timer_variant() {
    let src = read(KLOG);
    assert!(
        src.contains("Timer = 14"),
        "LogCategory 必须新增 Timer = 14 变体"
    );
    // use_self fix 改 `LogCategory::Acpi` 为 `Acpi` (impl 块内 fn name 调用)
    let pos = src.find("Acpi => b\"ACPI\"").expect("Acpi 行");
    let window_end = (pos + 200).min(src.len());
    let safe_end = src.floor_char_boundary(window_end);
    let window = &src[pos..safe_end];
    assert!(
        window.contains("Timer => b\"TIMER\""),
        "LogCategory::Timer 必须有对应 name() 输出"
    );
}
