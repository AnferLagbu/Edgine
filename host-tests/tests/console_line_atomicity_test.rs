// SPDX-License-Identifier: MPL-2.0
// ②: 控制台行原子性契约测试.
//
// 验收:
//   - klog 提供 `console_write_bytes` 写出单点: 一次持 `LOG_EMIT_LOCK` 写一整段
//   - 段边界取换行符, 段长上限 `CONSOLE_EMIT_MAX` 封顶 (持锁时长有界)
//   - `SerialSink` 仍走不带锁的 `serial_write_bytes` 原语 (临界区内回环即本核死锁)
//   - functions 层两条控制台旁路 (fd 1/2 与 /dev/console) 改走单点, 不再直写串口
//   - userlib `println` 整行单次 write, 行宽与内核段长上限耦合
//   - 锁序文档登记这把锁 (缺席时 lockdep/人工复核都认不得它)

use std::fs;

const KLOG: &str = "../src/kernel/privileged/klog/mod.rs";
const FS_IO: &str = "../src/kernel/functions/fs/io.rs";
const DEVFS: &str = "../src/kernel/functions/fs/devfs.rs";
const USER_IO: &str = "../src/user/lib/src/io.rs";
const LOCK_ORDER: &str = "../docs/explain/ref-lock-order.md";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释, 只留代码 —— 谓不应因为一句解释性注释而误判实现形态.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 取 `fn name(` 起至下一个行首 `}` 的函数体 (去注释).
fn fn_body(src: &str, name: &str) -> String {
    let anchor = format!("fn {name}(");
    let start = src
        .find(&anchor)
        .unwrap_or_else(|| panic!("{name} 必须存在 (缺即写出单点被拆除)"));
    let end_rel = src[start..].find("\n}").unwrap_or(usize::MAX);
    code_only(&src[start..start + end_rel])
}

#[test]
fn console_write_point_holds_emit_lock_per_segment() {
    let src = read(KLOG);
    let body = fn_body(&src, "console_write_bytes");
    let lock = body
        .find("LOG_EMIT_LOCK.lock()")
        .expect("写出单点必须持 LOG_EMIT_LOCK, 否则与内核日志仍会互插");
    let write = body
        .find("serial_write_bytes(segment)")
        .expect("临界区内必须整段写出");
    assert!(lock < write, "必须持锁后再写: 锁在写出之后等于没保护");
    assert!(
        body.contains("drop(emit)"),
        "每段写完必须释放锁, 不能让后续段搭同一临界区"
    );
    assert!(
        body.contains("let mut rest") && body.contains("while !rest.is_empty()"),
        "超限字节流必须循环切段写出, 不得静默丢弃尾部"
    );
}

#[test]
fn segment_bounds_prefer_newline_and_cap_length() {
    let src = read(KLOG);
    let body = fn_body(&src, "console_emit_segment_len");
    assert!(
        body.contains("position(|&b| b == b'\\n')"),
        "段边界必须优先取换行符 —— 一行的原子性由它所在段保证"
    );
    assert!(
        body.contains(".min(CONSOLE_EMIT_MAX)"),
        "段长必须封顶: 持锁时长 = 本段字节数, 无上限会长时间屏蔽本核中断"
    );
    let cap = src
        .lines()
        .find(|l| l.contains("pub const CONSOLE_EMIT_MAX"))
        .expect("段长上限必须是公开常量 (userlib 行宽与之耦合)")
        .split('=')
        .nth(1)
        .unwrap_or("")
        .trim()
        .trim_end_matches(';')
        .parse::<usize>()
        .expect("CONSOLE_EMIT_MAX 必须是整数字面量");
    assert!((64..=1024).contains(&cap), "段长上限失序: {cap}");
}

#[test]
fn serial_sink_stays_lock_free_primitive() {
    let src = read(KLOG);
    // SerialSink 在 LOG_EMIT_LOCK 临界区内被广播调用: 若它改走 console_write_bytes,
    // 就是在已持锁的本核再取同一把不可重入自旋锁 —— 本核死锁.
    let sink_impl = {
        let start = src
            .find("impl LogSink for SerialSink")
            .expect("SerialSink 实现必须存在");
        let end_rel = src[start..].find("\n}").unwrap_or(usize::MAX);
        code_only(&src[start..start + end_rel])
    };
    assert!(
        sink_impl.contains("serial_write_bytes("),
        "SerialSink 必须继续用不带锁的写出原语 (锁由发射临界区持有)"
    );
    assert!(
        !sink_impl.contains("console_write_bytes"),
        "SerialSink 不得改走写出单点, 否则持锁回环自锁死"
    );
    let primitive = fn_body(&src, "serial_write_bytes");
    assert!(
        !primitive.contains("LOG_EMIT_LOCK"),
        "写出原语保持无锁 (由调用方决定独占), 否则段内套段"
    );
}

#[test]
fn functions_console_paths_use_write_point() {
    for (path, label) in [
        (FS_IO, "write 系统调用 fd 1/2 分支"),
        (DEVFS, "/dev/console 写"),
    ] {
        let src = read(path);
        let code = code_only(&src);
        assert!(
            code.contains("klog::console_write_bytes("),
            "{label} 必须改走写出单点"
        );
        assert!(
            !code.contains("klog::serial_write_bytes("),
            "{label} 仍直写串口 = 旁路未闭合, 内核日志会从用户行中间穿过"
        );
    }
}

#[test]
fn userlib_println_emits_whole_line_in_one_write() {
    let src = read(USER_IO);
    let body = fn_body(&src, "println");
    assert!(
        body.contains("buf[bytes.len()] = b'\\n';"),
        "正文与换行必须拼成同一缓冲区"
    );
    assert!(
        body.contains("fs_write(1, &buf[..=bytes.len()])"),
        "整行必须单次 write: 两次 write 会在段间给别的写者留缝"
    );
    // 行宽与内核段长上限耦合: 正文 + '\n' 必须落进一个持锁段.
    let max = src
        .lines()
        .find(|l| l.contains("const PRINT_LINE_MAX"))
        .expect("行宽上限必须是常量")
        .split('=')
        .nth(1)
        .unwrap_or("")
        .trim()
        .trim_end_matches(';')
        .parse::<usize>()
        .expect("PRINT_LINE_MAX 必须是整数字面量");
    assert!(
        max < 256,
        "行宽 {max} 超出内核段长上限, 整行会跨段失去原子性"
    );
}

#[test]
fn lock_order_doc_registers_emit_lock() {
    let doc = read(LOCK_ORDER);
    assert!(
        doc.contains("LOG_EMIT_LOCK"),
        "锁序文档必须登记这把锁: 未登记时它游离在层级表外, 复核与 lockdep 都认不得"
    );
    assert!(
        doc.contains("CONSOLE_EMIT_MAX"),
        "行原子契约的段长上限必须写进文档 (持锁时长上界是锁纪律的一部分)"
    );
    assert!(
        doc.contains("serial_write_bytes"),
        "有意留在锁外的写出原语必须在文档里点名, 否则下一轮会被误改成取锁"
    );
}
