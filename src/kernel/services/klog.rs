#![deny(unsafe_code)]
//! klog 日志子系统 — services 层安全代理
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 framework::klog (TCB)。
//!
//! ## 职责
//!
//! - 包装 framework::klog 的 LogSink 注册表, 提供 100% safe Rust 的只读视图
//! - 为 `/proc/sys/klog/sinks` (TD-09 V2) 提供文本/JSON 渲染
//! - 维护运行时可见的 sink 名称列表
//!
//! ## 接口契约
//!
//! - `list_sinks()`: 返回当前已注册 sink 的名称列表 (按注册顺序)
//! - `count()`: 已注册 sink 数量
//! - `render_text(buf)`: 文本格式输出 (用于 `cat /proc/sys/klog/sinks`)
//! - `render_json(buf)`: JSON 格式输出 (用于监控/审计消费)
//! - `register_defaults()`: 启动期注册默认 sink (serial)
//!
//! ## 与 framework 的差异
//!
//! framework 提供 register/at/count/broadcast 等 unsafe 入口, services 层
//! 封装为纯安全 API, 仅暴露只读视图 + 启动期一次性注册.

// ============================================================================
// 类型
// ============================================================================

/// 渲染格式 (procfs 入口选择)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkListFormat {
    /// 人类可读纯文本
    Text,
    /// RFC 8259 JSON
    Json,
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// 解析 procfs 入口名 → 格式选择.
///
/// 规则: 文件名以 `.json` 结尾 → Json; 否则 → Text。
pub fn parse_format(name: &str) -> SinkListFormat {
    if name.ends_with(".json") {
        SinkListFormat::Json
    } else {
        SinkListFormat::Text
    }
}

// ============================================================================
// 启动期注册
// ============================================================================

/// 启动期注册默认 sink (serial). 由 kernel 主入口在 klog init 后调用.
///
/// 重复调用幂等: framework 的 register 在容量未满时返回 `Some(idx)`,
/// 已注册 serial 后再调用会拿到下一个 idx. 上层若想保证唯一性, 应
/// 在 init 路径上加 `static INIT` 守卫.
pub fn register_defaults() {
    framework_klog::klog_register_defaults();
}

// ============================================================================
// 只读视图
// ============================================================================

/// 已注册 sink 数量.
pub fn count() -> usize {
    framework_klog::klog_sink_count()
}

/// 取出 idx 处的 sink 名称. idx 越界或槽位空时返回 `None`.
///
/// 名称所有权属于 sink 实现, 调用方持有 `&'static str`, 不可变.
///
/// 本函数是**安全**包装: 内部走 `framework::klog::klog_sink_name_at`, 上层无 unsafe.
pub fn name_at(idx: usize) -> Option<&'static str> {
    framework_klog::klog_sink_name_at(idx)
}

/// 列出所有已注册 sink 的名称 (按注册顺序).
///
/// 返回固定大小的 slice 视图, 长度等于 `count()`. 容量受 `MAX_LOG_SINKS = 4`
/// 约束, 调用方应使用本函数返回的实际长度而非 `MAX_LOG_SINKS`.
pub fn list_names(buf: &mut [&'static str; framework_klog::MAX_LOG_SINKS]) -> usize {
    let n = count();
    let mut i = 0;
    while i < n && i < buf.len() {
        if let Some(name) = name_at(i) {
            buf[i] = name;
        }
        i += 1;
    }
    i
}

// ============================================================================
// 渲染 (procfs `/proc/sys/klog/sinks` 内容生成)
// ============================================================================

/// 文本格式: 一行一 sink, 形如:
///
/// ```text
/// Edgine klog sinks
/// ===============
/// count: 2
/// 0: serial
/// 1: net
/// ```
///
/// 写指针不越界, 写入字节数 = min(内容长度, `buf.len()`).
pub fn render_text(buf: &mut [u8]) -> usize {
    let mut pos = 0usize;
    let mut names = [core::str::from_utf8(b"").unwrap_or(""); framework_klog::MAX_LOG_SINKS];
    let n = list_names(&mut names);

    let push_str = |dst: &mut [u8], p: &mut usize, src: &str| {
        let b = src.as_bytes();
        let end = (*p + b.len()).min(dst.len());
        let len = end.saturating_sub(*p);
        if len > 0 {
            dst[*p..end].copy_from_slice(&b[..len]);
        }
        *p += len;
    };
    let push_usize = |dst: &mut [u8], p: &mut usize, v: usize| {
        if v == 0 {
            if *p < dst.len() {
                dst[*p] = b'0';
                *p += 1;
            }
            return;
        }
        let mut tmp = [0u8; 20];
        let mut i = 20;
        let mut x = v;
        while x > 0 && i > 0 {
            i -= 1;
            tmp[i] = (x % 10) as u8 + b'0';
            x /= 10;
        }
        let end = (*p + (20 - i)).min(dst.len());
        let len = end.saturating_sub(*p);
        if len > 0 {
            dst[*p..end].copy_from_slice(&tmp[i..i + len]);
        }
        *p += len;
    };

    push_str(buf, &mut pos, "Edgine klog sinks\n");
    push_str(buf, &mut pos, "===============\n");
    push_str(buf, &mut pos, "count: ");
    push_usize(buf, &mut pos, n);
    push_str(buf, &mut pos, "\n");
    for (i, name) in names.iter().take(n).enumerate() {
        push_usize(buf, &mut pos, i);
        push_str(buf, &mut pos, ": ");
        push_str(buf, &mut pos, name);
        push_str(buf, &mut pos, "\n");
    }

    pos
}

/// JSON 格式: 字段顺序固定, 形如:
///
/// ```json
/// {"format_version":"1","count":2,"sinks":["serial","net"]}
pub fn render_json(buf: &mut [u8]) -> usize {
    let mut pos = 0usize;
    let mut names = [core::str::from_utf8(b"").unwrap_or(""); framework_klog::MAX_LOG_SINKS];
    let n = list_names(&mut names);

    let push_str = |dst: &mut [u8], p: &mut usize, src: &str| {
        let b = src.as_bytes();
        let end = (*p + b.len()).min(dst.len());
        let len = end.saturating_sub(*p);
        if len > 0 {
            dst[*p..end].copy_from_slice(&b[..len]);
        }
        *p += len;
    };
    let push_usize = |dst: &mut [u8], p: &mut usize, v: usize| {
        if v == 0 {
            if *p < dst.len() {
                dst[*p] = b'0';
                *p += 1;
            }
            return;
        }
        let mut tmp = [0u8; 20];
        let mut i = 20;
        let mut x = v;
        while x > 0 && i > 0 {
            i -= 1;
            tmp[i] = (x % 10) as u8 + b'0';
            x /= 10;
        }
        let end = (*p + (20 - i)).min(dst.len());
        let len = end.saturating_sub(*p);
        if len > 0 {
            dst[*p..end].copy_from_slice(&tmp[i..i + len]);
        }
        *p += len;
    };

    push_str(buf, &mut pos, "{\"format_version\":\"1\",\"count\":");
    push_usize(buf, &mut pos, n);
    push_str(buf, &mut pos, ",\"sinks\":[");
    for (i, name) in names.iter().take(n).enumerate() {
        if i > 0 {
            push_str(buf, &mut pos, ",");
        }
        push_str(buf, &mut pos, "\"");
        push_str(buf, &mut pos, name);
        push_str(buf, &mut pos, "\"");
    }
    push_str(buf, &mut pos, "]}");

    pos
}

// ============================================================================
// Framework 重新导出 (仅本模块内部使用, 避免外部直接走 unsafe 入口)
// ============================================================================
use crate::framework::klog as framework_klog;
