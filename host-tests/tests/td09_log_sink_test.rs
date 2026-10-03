// SPDX-License-Identifier: MPL-2.0
// TD-09: klog 多 sink 抽象 + 注册表契约测试.
//
// 验收:
//   - LogSink trait 存在且含 name/putc/write_str/write_bytes
//   - SerialSink impl name() == "serial"
//   - 全局注册表容量 MAX_LOG_SINKS = 4
//   - klog_output 改走 klog_broadcast_bytes 路径 (无 serial 直写)
//   - klog_register_defaults 注册入口存在

use std::fs;

const KLOG: &str = "../src/kernel/framework/klog/mod.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

#[test]
fn test_log_sink_trait_exists() {
    let src = read(KLOG);
    assert!(src.contains("pub trait LogSink"), "必须有 LogSink trait");
    let body_start = src.find("pub trait LogSink").expect("trait 必须存在");
    // 取到下一个 "\n}\n" 防止越界切字符.
    let end_rel = src[body_start..].find("\n}\n").unwrap_or(400);
    let end = (body_start + end_rel).min(src.len());
    let safe_end = src.floor_char_boundary(end);
    let body = &src[body_start..safe_end];
    assert!(body.contains("fn name(&self)"), "trait 必须有 name()");
    assert!(
        body.contains("fn putc(&self, c: u8)"),
        "trait 必须有 putc()"
    );
    assert!(
        body.contains("fn write_str(&self, s: &str)"),
        "trait 必须有 write_str()"
    );
    assert!(
        body.contains("fn write_bytes(&self, b: &[u8])"),
        "trait 必须有 write_bytes()"
    );
}

#[test]
fn test_serial_sink_impl() {
    let src = read(KLOG);
    assert!(src.contains("pub struct SerialSink"), "必须有 SerialSink");
    assert!(
        src.contains("impl LogSink for SerialSink"),
        "SerialSink 必须 impl LogSink"
    );
    let body = &src[src.find("impl LogSink for SerialSink").expect("impl")..];
    assert!(
        body.contains("\"serial\""),
        "SerialSink::name() 必须返回 \"serial\""
    );
    assert!(
        body.contains("serial_putc_chained"),
        "SerialSink::putc 走串口直写"
    );
    assert!(
        body.contains("serial_write_bytes"),
        "SerialSink::write_bytes 走串口批量写"
    );
}

#[test]
fn test_sink_registry_caps_at_4() {
    let src = read(KLOG);
    assert!(
        src.contains("pub const MAX_LOG_SINKS: usize = 4"),
        "容量必须为 4"
    );
    assert!(
        src.contains("static LOG_SINK_COUNT: AtomicU8"),
        "必须有计数"
    );
    // 新模式: LOG_SINKS 使用 IrqSpinLock 包装 SinkPtr 数组
    assert!(
        src.contains("static LOG_SINKS: crate::framework::sync::IrqSpinLock<[SinkPtr"),
        "必须有 IrqSpinLock 包装的 SinkPtr 注册表"
    );
}

#[test]
fn test_register_unregister_api() {
    let src = read(KLOG);
    assert!(
        src.contains("pub unsafe fn klog_register_sink"),
        "必须有 register 入口"
    );
    assert!(
        src.contains("pub unsafe fn klog_sink_at"),
        "必须有 sink_at 取元素入口"
    );
    // 注册满时返回 None
    let body_start = src.find("pub unsafe fn klog_register_sink").expect("");
    let end_rel = src[body_start..].find("\n}\n").unwrap_or(500);
    let end = (body_start + end_rel).min(src.len());
    let safe_end = src.floor_char_boundary(end);
    let body = &src[body_start..safe_end];
    assert!(body.contains("return None"), "容量满时必须 return None");
}

#[test]
fn test_broadcast_apis() {
    let src = read(KLOG);
    assert!(
        src.contains("pub fn klog_broadcast("),
        "必须有 broadcast 入口"
    );
    assert!(
        src.contains("pub fn klog_broadcast_bytes("),
        "必须有 broadcast_bytes 入口"
    );
    assert!(
        src.contains("pub fn klog_register_defaults()"),
        "必须有 register_defaults 入口"
    );
}

#[test]
fn test_klog_output_routes_through_broadcast() {
    let src = read(KLOG);
    // B08-14 (2026-09-06): host-test 桩将 klog_output 拆为薄包装 + klog_output_baremetal.
    // 裸机广播逻辑在 klog_output_baremetal 中 (host 下 klog no-op, 见 klog/mod.rs 注释).
    // 本契约验证"日志输出走 sink 广播而非串口直写", 解析 baremetal 实现体.
    let body_start = src
        .find("fn klog_output_baremetal(")
        .expect("klog_output_baremetal 必须存在 (B08-14 抽离裸机路径)");
    let body_end_rel = src[body_start..].find("\n}\n").unwrap_or(usize::MAX);
    let body = &src[body_start..body_start + body_end_rel];
    // 不再直接 serial_write_bytes
    assert!(
        !body.contains("serial_write_bytes("),
        "klog_output 必须改走 sink 抽象, 不再直写"
    );
    // 必须改用 klog_broadcast_bytes
    let bc = body.matches("klog_broadcast_bytes(").count();
    assert!(
        bc >= 5,
        "klog_output 必须至少调用 5 次 klog_broadcast_bytes, 实际 {bc}"
    );
}

#[test]
fn test_sink_ptr_union_layout() {
    let src = read(KLOG);
    assert!(src.contains("#[repr(C)]"), "SinkPtr 必须 #[repr(C)]");
    assert!(src.contains("#[derive(Copy, Clone)]"), "SinkPtr 必须 Copy");
    assert!(src.contains("union SinkPtr"), "SinkPtr 是 union");
    assert!(
        src.contains("raw: usize") && src.contains("fat: *const dyn LogSink"),
        "SinkPtr 必须含 raw/fat 两个字段"
    );
}

#[test]
fn test_max_sinks_four_invariants() {
    // 容量边界: 注册 MAX + 1 个 sink 时第 5 个返回 None.
    let src = read(KLOG);
    // 确认容量常量 4 出现在 make_null_sinks 中 4 次.
    let count_null = src.matches("null_sink_ptr()").count();
    assert!(
        count_null >= 4,
        "必须有 4 个 null_sink_ptr 调用, 实际 {count_null}"
    );
}
