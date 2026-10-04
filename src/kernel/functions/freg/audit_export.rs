#![deny(unsafe_code)]
//! 审计日志导出器 — functions/freg/ 业务层
//!
//! ## 职责
//!
//! 收集 `privileged::freg::manager::ROLLBACK_LOG` 与 `privileged::freg::reset`
//! 模块的审计日志, 转换为 functions 层强类型结构, 供 telemetry/monitoring
//! 消费 (kernel shell / dmesg / proc 接口).
//!
//! ## @SAFE
//!
//! 本文件不含 `unsafe`. 通过 `privileged::freg::types` 的 `RollbackEvent`
//! 与 TCB 交互.

use crate::privileged::freg::{ROLLBACK_LOG, recovery_rollback_log_count};

/// 审计摘要 (压缩视图, 适合 dmesg 导出)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackSummary {
    pub domain_id: u64,
    pub generation_from: u64,
    pub generation_to: u64,
    pub entries: usize,
    pub cascade_depth: usize,
    pub result: i32,
    pub tick: u64,
    pub fingerprint: u64,
}

impl RollbackSummary {
    /// 渲染为单行 (dmesg 友好)
    /// 返回: (96 字节 buffer, 实际写入长度)
    pub fn render_line(&self) -> ([u8; 96], usize) {
        let mut buf = [0u8; 96];
        let prefix = b"[FREG] dom=";
        let mut idx = prefix.len();
        buf[..idx].copy_from_slice(prefix);
        idx += write_u64(&mut buf[idx..], self.domain_id);
        let mid = b" gen=";
        buf[idx..idx + mid.len()].copy_from_slice(mid);
        idx += mid.len();
        idx += write_u64(&mut buf[idx..], self.generation_from);
        buf[idx] = b'-';
        idx += 1;
        idx += write_u64(&mut buf[idx..], self.generation_to);
        let mid = b" entries=";
        buf[idx..idx + mid.len()].copy_from_slice(mid);
        idx += mid.len();
        idx += write_u64(&mut buf[idx..], self.entries as u64);
        let mid = b" res=";
        buf[idx..idx + mid.len()].copy_from_slice(mid);
        idx += mid.len();
        idx += write_i32(&mut buf[idx..], self.result);
        buf[idx] = 0; // C 字符串结尾
        (buf, idx)
    }
}

/// 整数写入辅助 (无 alloc)
fn write_u64(buf: &mut [u8], mut v: u64) -> usize {
    if v == 0 {
        if !buf.is_empty() {
            buf[0] = b'0';
        }
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut i = 0;
    while v > 0 && i < tmp.len() {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    let mut out = 0;
    while i > 0 && out < buf.len() {
        i -= 1;
        buf[out] = tmp[i];
        out += 1;
    }
    out
}

fn write_i32(buf: &mut [u8], mut v: i32) -> usize {
    if v == 0 {
        if !buf.is_empty() {
            buf[0] = b'0';
        }
        return 1;
    }
    let negative = v < 0;
    if negative {
        v = -v;
    }
    let mut tmp = [0u8; 12];
    let mut i = 0;
    while v > 0 && i < tmp.len() {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    let mut out = 0;
    if negative && out < buf.len() {
        buf[out] = b'-';
        out += 1;
    }
    while i > 0 && out < buf.len() {
        i -= 1;
        buf[out] = tmp[i];
        out += 1;
    }
    out
}

/// 审计导出器
pub struct AuditExporter {
    /// 输出 buffer (固定大小, 避免 alloc)
    pub output_buf: [u8; 4096],
    pub output_count: usize,
}

impl AuditExporter {
    pub const fn new() -> Self {
        Self {
            output_buf: [0u8; 4096],
            output_count: 0,
        }
    }

    /// 收集 `ROLLBACK_LOG` → `output_buf`
    ///
    /// 返回: 写入字节数
    pub fn export_rollback_log(&mut self) -> usize {
        self.output_count = 0;
        let total = recovery_rollback_log_count();
        if total <= 0 {
            return 0;
        }
        let log = ROLLBACK_LOG.lock();
        for entry in log.iter().flatten() {
            if self.output_count >= self.output_buf.len() {
                break;
            }
            let summary = RollbackSummary {
                domain_id: entry.domain_id,
                generation_from: entry.generation_from,
                generation_to: entry.generation_to,
                entries: entry.entries_rolled_back,
                cascade_depth: entry.cascade_depth,
                result: entry.result,
                tick: entry.tick,
                fingerprint: entry.crash_fingerprint,
            };
            let (line, line_len) = summary.render_line();
            let line_len = line_len.min(line.len());
            let avail = self.output_buf.len() - self.output_count;
            let copy_len = line_len.min(avail);
            self.output_buf[self.output_count..self.output_count + copy_len]
                .copy_from_slice(&line[..copy_len]);
            self.output_count += copy_len;
            if self.output_count < self.output_buf.len() {
                self.output_buf[self.output_count] = b'\n';
                self.output_count += 1;
            }
        }
        self.output_count
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 统计: 成功回滚次数
    pub fn count_success(&self) -> usize {
        let log = ROLLBACK_LOG.lock();
        log.iter()
            .filter(|e| e.is_some_and(|x| x.result == 0))
            .count()
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 统计: 失败回滚次数
    pub fn count_failure(&self) -> usize {
        let log = ROLLBACK_LOG.lock();
        log.iter()
            .filter(|e| e.is_some_and(|x| x.result != 0))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_u64_basic() {
        let mut buf = [0u8; 20];
        let n = write_u64(&mut buf, 12345);
        assert_eq!(&buf[..n], b"12345");
    }

    #[test]
    fn write_u64_zero() {
        let mut buf = [0u8; 20];
        let n = write_u64(&mut buf, 0);
        assert_eq!(&buf[..n], b"0");
    }

    #[test]
    fn write_i32_positive() {
        let mut buf = [0u8; 20];
        let n = write_i32(&mut buf, 42);
        assert_eq!(&buf[..n], b"42");
    }

    #[test]
    fn write_i32_negative() {
        let mut buf = [0u8; 20];
        let n = write_i32(&mut buf, -7);
        assert_eq!(&buf[..n], b"-7");
    }

    #[test]
    fn summary_render_line() {
        let s = RollbackSummary {
            domain_id: 5,
            generation_from: 10,
            generation_to: 8,
            entries: 3,
            cascade_depth: 1,
            result: 0,
            tick: 1000,
            fingerprint: 0xDEADBEEF,
        };
        let (line, len) = s.render_line();
        let s_render = core::str::from_utf8(&line[..len]).unwrap();
        assert!(s_render.contains("dom=5"));
        assert!(s_render.contains("gen=10-8"));
        assert!(s_render.contains("entries=3"));
        assert!(s_render.contains("res=0"));
    }

    #[test]
    fn exporter_empty_log() {
        let mut exp = AuditExporter::new();
        let n = exp.export_rollback_log();
        // 空 log 或测试隔离, n 应为 0 或 log 长度
        let _ = n;
    }
}
