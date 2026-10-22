// SPDX-License-Identifier: MPL-2.0
// TD-09: klog 多 sink 抽象 + 注册表契约测试.
//
// 验收:
//   - LogSink trait 存在且含 name/putc/write_str/write_bytes
//   - SerialSink 实现 name() == "serial"
//   - 全局注册表容量 MAX_LOG_SINKS = 4
//   - klog_output 改走 klog_broadcast_bytes 路径 (无 serial 直写)
//   - klog_register_defaults 注册入口存在
//   - 整行日志在跨核锁内一次写完 (串口 + 环形缓冲不受另一核插入)

use std::fs;

const KLOG: &str = "../src/kernel/privileged/klog/mod.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释, 只留代码 —— 谓词不应因为一句解释性注释而误判实现形态.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 取 `fn klog_output_baremetal(` 的函数体 (到下一个行首 `}` 为止).
fn baremetal_body(src: &str) -> String {
    let start = src
        .find("fn klog_output_baremetal(")
        .expect("klog_output_baremetal 必须存在 (B08-14 抽离裸机路径)");
    let end_rel = src[start..].find("\n}").unwrap_or(usize::MAX);
    code_only(&src[start..start + end_rel])
}

// ── 整行原子性谓词 (P-e: 跨核日志互斥) ──────────────────────────────
//
// 不变式: 一条日志 = 一次 sink 写出, 且该写出被跨核锁独占; 环形缓冲与串口同
// 一临界区. 只用 `interrupt_disable()` 包整行是错的 —— 它仅屏蔽本核中断,
// 另一核照旧写串口, 整行会被拆成两半 (曾使 QEMU 启动里程碑 grep 假阴性).

/// 裸机体内 sink 广播调用次数 (整行一次写完 → 恰为 1).
fn broadcast_calls(body: &str) -> usize {
    body.matches("klog_broadcast_bytes(").count()
}

/// 是否持有发射锁后才广播 (锁在广播之前 = 整行在临界区内).
fn holds_emit_lock_before_broadcast(body: &str) -> bool {
    match (
        body.find("LOG_EMIT_LOCK.lock()"),
        body.find("klog_broadcast_bytes("),
    ) {
        (Some(lock), Some(bcast)) => lock < bcast,
        _ => false,
    }
}

/// 环形缓冲写入是否也在同一临界区内 (锁 → RING → 显式释放).
///
/// 不绑定守卫变量名 (只要求 RING 之后存在一次显式 `drop(`), 避免改名即误报;
/// 若释放先于 RING 写入 (即环形缓冲在锁外写), `drop(` 就找不到在 ring 之后.
fn ring_inside_emit_critical_section(body: &str) -> bool {
    let (Some(lock), Some(ring)) = (
        body.find("LOG_EMIT_LOCK.lock()"),
        body.find("RING.inner.get()"),
    ) else {
        return false;
    };
    lock < ring && body[ring..].contains("drop(")
}

/// 是否仍拿裸中断屏蔽当整行保护 (跨核无效, 必须为假).
fn uses_bare_irq_mask_as_guard(body: &str) -> bool {
    body.contains("interrupt_disable(") || body.contains("interrupt_restore(")
}

#[test]
fn test_log_emit_lock_is_a_cross_cpu_spinlock() {
    let src = read(KLOG);
    assert!(
        src.contains("static LOG_EMIT_LOCK: crate::privileged::sync::IrqSpinLock<()>"),
        "整行发射必须由跨核 IrqSpinLock 保护 (本核 cli 不能隔离另一核)"
    );
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
        src.contains("static LOG_SINKS: crate::privileged::sync::IrqSpinLock<[SinkPtr"),
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
    let body = baremetal_body(&src);
    // 不再直接 serial_write_bytes
    assert!(
        !body.contains("serial_write_bytes("),
        "klog_output 必须改走 sink 抽象, 不再直写"
    );
    // 必须改用 klog_broadcast_bytes
    let bc = broadcast_calls(&body);
    assert!(
        bc >= 1,
        "klog_output 必须走 klog_broadcast_bytes, 实际 {bc}"
    );
}

#[test]
fn test_klog_line_is_emitted_atomically_across_cpus() {
    let src = read(KLOG);
    let body = baremetal_body(&src);
    // 整行一次写完: 不允许把一条消息切成多片段广播 (片段之间就是交错窗口)
    assert_eq!(
        broadcast_calls(&body),
        1,
        "整条日志必须一次性交给 sink; 多次广播会在片段间留下跨核交错窗口"
    );
    assert!(
        holds_emit_lock_before_broadcast(&body),
        "必须在持有 LOG_EMIT_LOCK 之后才写出整行"
    );
    assert!(
        ring_inside_emit_critical_section(&body),
        "日志环形缓冲与串口必须共享同一临界区 (否则 /proc/kmsg 同样会交错)"
    );
    assert!(
        !uses_bare_irq_mask_as_guard(&body),
        "不得再用裸 interrupt_disable/restore 充当整行保护 (只屏蔽本核, 跨核无效)"
    );
}

#[test]
fn test_negative_control_log_atomicity_predicates() {
    // 回退样本一: 旧形态 (多片段广播 + 裸 cli, 无跨核锁) —— 谓词都该亮起
    let legacy = r#"fn klog_output_baremetal(level: LogLevel, cat: LogCategory, msg: &[u8]) {
    let saved_if = crate::arch!(interrupt_disable());
    klog_broadcast_bytes(ts);
    klog_broadcast_bytes(level.prefix());
    klog_broadcast_bytes(msg);
    crate::arch!(interrupt_restore(saved_if));
    let ring = unsafe { &mut *RING.inner.get() };
    ring.push_str(msg);
}
"#;
    let legacy = code_only(legacy);
    assert_eq!(broadcast_calls(&legacy), 3, "样本应被识别为多片段广播");
    assert!(
        !holds_emit_lock_before_broadcast(&legacy),
        "无跨核锁必须被谓词判为违反"
    );
    assert!(
        !ring_inside_emit_critical_section(&legacy),
        "锁外写 RING 必须被谓词判为违反"
    );
    assert!(
        uses_bare_irq_mask_as_guard(&legacy),
        "裸 cli 充当整行保护必须被谓词亮起"
    );

    // 回退样本二: 加了锁但锁在写出之后 (交错窗口仍在)
    let late_lock = r#"fn klog_output_baremetal(level: LogLevel, cat: LogCategory, msg: &[u8]) {
    klog_broadcast_bytes(msg);
    let _emit = LOG_EMIT_LOCK.lock();
    let ring = unsafe { &mut *RING.inner.get() };
    ring.push_str(msg);
    drop(_emit);
}
"#;
    let late_lock = code_only(late_lock);
    assert!(
        !holds_emit_lock_before_broadcast(&late_lock),
        "锁获取晚于写出必须被谓词判为违反"
    );
    assert!(
        ring_inside_emit_critical_section(&late_lock),
        "本样本的 RING 写入在锁内 (谓词粒度区分两个不变式)"
    );

    // 回退样本三: 单次广播但仍靠裸 cli (跨核无效)
    let bare_cli = r#"fn klog_output_baremetal(level: LogLevel, cat: LogCategory, msg: &[u8]) {
    let saved_if = crate::arch!(interrupt_disable());
    klog_broadcast_bytes(msg);
    crate::arch!(interrupt_restore(saved_if));
}
"#;
    let bare_cli = code_only(bare_cli);
    assert_eq!(broadcast_calls(&bare_cli), 1, "单片段不再能误判为已修复");
    assert!(
        uses_bare_irq_mask_as_guard(&bare_cli),
        "裸 cli 充当跨核保护必须被谓词亮起"
    );
    assert!(
        !holds_emit_lock_before_broadcast(&bare_cli),
        "无发射锁必须被谓词亮起"
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
