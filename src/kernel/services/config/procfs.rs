#![deny(unsafe_code)]
//! /proc/sys/config 接口: 用户态可读取内核编译期/启动期配置
//!
//! ## 迁移记录
//!
//! 策略代码于 2026-06-17 从 framework::config::procfs 迁移至此。
//! framework 层仅保留 re-export 保持调用方兼容。

use crate::framework::config::get_config_summary;

/// `/proc/sys/config` 读取时的文本格式选择器.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// 人类可读纯文本 (klog 风格).
    Text,
    /// 严格 RFC 8259 JSON, 字段顺序固定以保证可读性.
    Json,
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// 将 procfs 条目名解析为 `ConfigFormat`.
///
/// 规则: 文件名以 `.json` 结尾 → JSON; 否则 → Text. **调用方在 `mount`
/// 时按需注册两个条目**.
pub fn parse_format(name: &str) -> ConfigFormat {
    if name.ends_with(".json") {
        ConfigFormat::Json
    } else {
        ConfigFormat::Text
    }
}

/// 生成 `/proc/sys/config` 的文本内容.
///
/// 返回写入 `buf` 的字节数.
pub fn read_sys_config(buf: &mut [u8]) -> usize {
    write_text(buf)
}

/// 生成 `/proc/sys/config.json` 的 JSON 内容.
///
/// 输出与 text 模式字段**一一对应** (除 `format_version` 标识), 便于监控
/// 系统 `jq` 抽取.
pub fn read_sys_config_json(buf: &mut [u8]) -> usize {
    write_json(buf)
}

// ============================================================================
// Text 输出
// ============================================================================

fn write_text(buf: &mut [u8]) -> usize {
    let s = get_config_summary();
    let caps = s.capabilities;

    let mut pos = 0usize;
    let push_str = |dst: &mut [u8], p: &mut usize, src: &str| {
        let b = src.as_bytes();
        let end = (*p + b.len()).min(dst.len());
        let len = end - *p;
        dst[*p..end].copy_from_slice(&b[..len]);
        *p += len;
    };
    let push_u64 = |dst: &mut [u8], p: &mut usize, val: u64| {
        if val == 0 && *p < dst.len() {
            dst[*p] = b'0';
            *p += 1;
            return;
        }
        let mut tmp = [0u8; 20];
        let mut i = 20;
        let mut v = val;
        while v > 0 && i > 0 {
            i -= 1;
            tmp[i] = (v % 10) as u8 + b'0';
            v /= 10;
        }
        let end = (*p + (20 - i)).min(dst.len());
        let len = end - *p;
        dst[*p..end].copy_from_slice(&tmp[i..i + len]);
        *p += len;
    };
    let push_usize = |dst: &mut [u8], p: &mut usize, val: usize| {
        push_u64(dst, p, val as u64);
    };
    let push_bool = |dst: &mut [u8], p: &mut usize, val: bool| {
        push_str(dst, p, if val { "yes" } else { "no" });
    };

    push_str(buf, &mut pos, "Edgine Configuration\n");
    push_str(buf, &mut pos, "==========================\n");
    push_str(buf, &mut pos, "max_cpus:        ");
    push_usize(buf, &mut pos, s.max_cpus);
    push_str(buf, &mut pos, "\nactual_cpus:     ");
    push_u64(buf, &mut pos, u64::from(s.actual_cpus));
    push_str(buf, &mut pos, "\nmax_irqs:        ");
    push_usize(buf, &mut pos, s.max_irqs);
    push_str(buf, &mut pos, "\nmax_processes:   ");
    push_usize(buf, &mut pos, s.max_processes);
    push_str(buf, &mut pos, "\nmax_threads:     ");
    push_usize(buf, &mut pos, s.max_threads);
    push_str(buf, &mut pos, "\npage_size:       ");
    push_u64(buf, &mut pos, s.page_size);
    push_str(buf, &mut pos, "\nkaslr_offset:    0x");
    push_u64(buf, &mut pos, s.kaslr_offset);
    push_str(buf, &mut pos, "\napic_enabled:    ");
    push_bool(buf, &mut pos, s.apic_enabled);
    push_str(buf, &mut pos, "\nioapic_enabled:  ");
    push_bool(buf, &mut pos, s.ioapic_enabled);
    push_str(buf, &mut pos, "\n-- capabilities --\n");
    push_str(buf, &mut pos, "smp:             ");
    push_bool(buf, &mut pos, caps.smp);
    push_str(buf, &mut pos, "\npreempt:         ");
    push_bool(buf, &mut pos, caps.preempt);
    push_str(buf, &mut pos, "\nkaslr:           ");
    push_bool(buf, &mut pos, caps.kaslr);
    push_str(buf, &mut pos, "\nkpti:            ");
    push_bool(buf, &mut pos, caps.kpti);
    push_str(buf, &mut pos, "\nfreg:         ");
    push_bool(buf, &mut pos, caps.freg);
    push_str(buf, &mut pos, "\n");

    pos
}

// ============================================================================
// JSON 输出
// ============================================================================

fn write_json(buf: &mut [u8]) -> usize {
    let s = get_config_summary();
    let caps = s.capabilities;

    let mut pos = 0usize;

    let push_str = |dst: &mut [u8], p: &mut usize, src: &str| {
        let b = src.as_bytes();
        let end = (*p + b.len()).min(dst.len());
        let len = end - *p;
        dst[*p..end].copy_from_slice(&b[..len]);
        *p += len;
    };
    let push_u64 = |dst: &mut [u8], p: &mut usize, val: u64| {
        if val == 0 && *p < dst.len() {
            dst[*p] = b'0';
            *p += 1;
            return;
        }
        let mut tmp = [0u8; 20];
        let mut i = 20;
        let mut v = val;
        while v > 0 && i > 0 {
            i -= 1;
            tmp[i] = (v % 10) as u8 + b'0';
            v /= 10;
        }
        let end = (*p + (20 - i)).min(dst.len());
        let len = end - *p;
        dst[*p..end].copy_from_slice(&tmp[i..i + len]);
        *p += len;
    };

    let push_field_num = |dst: &mut [u8], p: &mut usize, k: &str, v: u64| {
        push_str(dst, p, "\"");
        push_str(dst, p, k);
        push_str(dst, p, "\":");
        push_u64(dst, p, v);
    };
    let push_field_bool = |dst: &mut [u8], p: &mut usize, k: &str, v: bool| {
        push_str(dst, p, "\"");
        push_str(dst, p, k);
        push_str(dst, p, "\":");
        push_str(dst, p, if v { "true" } else { "false" });
    };
    let push_field_str = |dst: &mut [u8], p: &mut usize, k: &str, v: &str| {
        push_str(dst, p, "\"");
        push_str(dst, p, k);
        push_str(dst, p, "\":\"");
        push_str(dst, p, v);
        push_str(dst, p, "\"");
    };

    push_str(buf, &mut pos, "{");
    push_field_str(buf, &mut pos, "format_version", "1");
    push_str(buf, &mut pos, ",");
    push_field_str(buf, &mut pos, "kernel", "Edgine");
    push_str(buf, &mut pos, ",");
    push_field_num(buf, &mut pos, "max_cpus", s.max_cpus as u64);
    push_str(buf, &mut pos, ",");
    push_field_num(buf, &mut pos, "actual_cpus", u64::from(s.actual_cpus));
    push_str(buf, &mut pos, ",");
    push_field_num(buf, &mut pos, "max_irqs", s.max_irqs as u64);
    push_str(buf, &mut pos, ",");
    push_field_num(buf, &mut pos, "max_processes", s.max_processes as u64);
    push_str(buf, &mut pos, ",");
    push_field_num(buf, &mut pos, "max_threads", s.max_threads as u64);
    push_str(buf, &mut pos, ",");
    push_field_num(buf, &mut pos, "page_size", s.page_size);
    push_str(buf, &mut pos, ",");
    push_field_num(buf, &mut pos, "kaslr_offset", s.kaslr_offset);
    push_str(buf, &mut pos, ",");
    push_field_bool(buf, &mut pos, "apic_enabled", s.apic_enabled);
    push_str(buf, &mut pos, ",");
    push_field_bool(buf, &mut pos, "ioapic_enabled", s.ioapic_enabled);
    push_str(buf, &mut pos, ",");
    push_str(buf, &mut pos, "\"capabilities\":{");
    push_field_bool(buf, &mut pos, "smp", caps.smp);
    push_str(buf, &mut pos, ",");
    push_field_bool(buf, &mut pos, "preempt", caps.preempt);
    push_str(buf, &mut pos, ",");
    push_field_bool(buf, &mut pos, "kaslr", caps.kaslr);
    push_str(buf, &mut pos, ",");
    push_field_bool(buf, &mut pos, "kpti", caps.kpti);
    push_str(buf, &mut pos, ",");
    push_field_bool(buf, &mut pos, "freg", caps.freg);
    push_str(buf, &mut pos, "}");
    push_str(buf, &mut pos, "}");

    pos
}
