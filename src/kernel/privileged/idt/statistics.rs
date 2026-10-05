//! # 中断统计与 JSON 导出模块
//!
//! 提供结构化的中断统计数据收集和 JSON 格式导出功能，
//! 用于测试框架集成和运行时监控。
//!
//! ## 功能特性
//!
//! - **实时统计**: `AtomicU64` 无锁计数，高性能读取
//! - **分类统计**: 按异常类型/IRQ/严重性分组
//! - **JSON 导出**: 标准格式输出，便于测试框架解析
//! - **历史记录**: 保留最近 N 次异常的详细信息

use core::sync::atomic::{AtomicU64, Ordering};

use super::types::{IRQ_BASE, InterruptFrame};
use crate::privileged::sync::IrqSpinLock;

use crate::privileged::sync::OnceLock;
/// 中断事件记录 (用于历史追踪)
#[derive(Debug, Clone, Copy)]
pub struct InterruptEvent {
    /// 时间戳 (TSC)
    pub timestamp: u64,
    /// 向量号
    pub vector: u8,
    /// RIP 地址
    pub rip: u64,
    /// 是否 user-mode
    pub is_user: bool,
}

/// 详细的中断统计信息
pub struct DetailedStatistics {
    /// 总中断次数
    pub total_count: AtomicU64,

    // 异常统计 (32 个标准异常)
    pub exception_counts: [AtomicU64; 32],

    // IRQ 统计 (16 个 IRQ)
    pub irq_counts: [AtomicU64; 16],

    // 特殊向量统计
    pub syscall_count: AtomicU64,  // int 0x80
    pub recovery_count: AtomicU64, // int 0x82

    // 嵌套中断统计
    pub nested_interrupts: AtomicU64,
    pub max_nesting_depth: AtomicU64,

    // User/Kernel 模式分布
    pub user_mode_interrupts: AtomicU64,
    pub kernel_mode_interrupts: AtomicU64,

    // 恢复动作统计
    pub recoveries: AtomicU64,
    pub process_terminations: AtomicU64,
    pub domain_recoveries: AtomicU64,
    pub panics: AtomicU64,

    // 历史记录 (环形缓冲区, IrqSpinLock 保证中断安全)
    history: IrqSpinLock<InterruptHistory>,
}

/// 历史记录缓冲区
struct InterruptHistory {
    events: [InterruptEvent; 64], // 最近 64 次中断
    index: u64,                   // 当前写入位置
    count: u64,                   // 已记录的事件总数
}

impl Default for InterruptHistory {
    fn default() -> Self {
        Self {
            events: [const {
                InterruptEvent {
                    timestamp: 0,
                    vector: 0,
                    rip: 0,
                    is_user: false,
                }
            }; 64],
            index: 0,
            count: 0,
        }
    }
}

impl DetailedStatistics {
    /// 创建新的统计实例
    pub fn new() -> Self {
        // 手动创建 InterruptHistory (因为 const fn 不能调用 Default)
        let history = InterruptHistory {
            events: [const {
                InterruptEvent {
                    timestamp: 0,
                    vector: 0,
                    rip: 0,
                    is_user: false,
                }
            }; 64],
            index: 0,
            count: 0,
        };

        Self {
            total_count: AtomicU64::new(0),
            exception_counts: [const { AtomicU64::new(0) }; 32],
            irq_counts: [const { AtomicU64::new(0) }; 16],
            syscall_count: AtomicU64::new(0),
            recovery_count: AtomicU64::new(0),
            nested_interrupts: AtomicU64::new(0),
            max_nesting_depth: AtomicU64::new(0),
            user_mode_interrupts: AtomicU64::new(0),
            kernel_mode_interrupts: AtomicU64::new(0),
            recoveries: AtomicU64::new(0),
            process_terminations: AtomicU64::new(0),
            domain_recoveries: AtomicU64::new(0),
            panics: AtomicU64::new(0),
            history: IrqSpinLock::new(history),
        }
    }

    /// 记录一次异常
    pub fn record_exception(&self, vector: u8, frame: &InterruptFrame) {
        self.total_count.fetch_add(1, Ordering::Relaxed);

        if vector < 32 {
            self.exception_counts[vector as usize].fetch_add(1, Ordering::Relaxed);
        }

        let is_user = frame.is_user_mode();
        if is_user {
            self.user_mode_interrupts.fetch_add(1, Ordering::Relaxed);
        } else {
            self.kernel_mode_interrupts.fetch_add(1, Ordering::Relaxed);
        }

        // 记录到历史缓冲区
        self.record_history(vector, frame.rip, is_user);

        // 更新时间戳

        let _ = crate::arch!(timestamp());
    }

    /// 记录一次 IRQ
    pub fn record_irq(&self, irq: u8) {
        self.total_count.fetch_add(1, Ordering::Relaxed);

        if irq < 16 {
            self.irq_counts[irq as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记录嵌套中断
    pub fn record_nested(&self, depth: u64) {
        self.nested_interrupts.fetch_add(1, Ordering::Relaxed);

        // 更新最大嵌套深度
        let mut current_max = self.max_nesting_depth.load(Ordering::Relaxed);
        while depth > current_max {
            match self.max_nesting_depth.compare_exchange_weak(
                current_max,
                depth,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current_max = actual,
            }
        }
    }

    /// 记录恢复动作
    pub fn record_recovery_action(&self, action: &super::handlers::RecoveryAction) {
        use super::handlers::RecoveryAction;

        match action {
            RecoveryAction::Recovered => self.recoveries.fetch_add(1, Ordering::Relaxed),
            RecoveryAction::TerminateProcess(_) => {
                self.process_terminations.fetch_add(1, Ordering::Relaxed)
            }
            RecoveryAction::DomainRecovery => {
                self.domain_recoveries.fetch_add(1, Ordering::Relaxed)
            }
            RecoveryAction::Panic(_) => self.panics.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// 记录到历史缓冲区
    fn record_history(&self, vector: u8, rip: u64, is_user: bool) {
        let mut history = self.history.lock();

        let event = InterruptEvent {
            timestamp: crate::arch!(timestamp()),
            vector,
            rip,
            is_user,
        };

        let idx = (history.index % 64) as usize;
        history.events[idx] = event;
        history.index += 1;

        if history.count < 64 {
            history.count += 1;
        }
    }

    /// 获取指定向量的计数
    pub fn get_vector_count(&self, vector: u8) -> u64 {
        if vector < 32 {
            self.exception_counts[vector as usize].load(Ordering::Relaxed)
        } else if (vector as usize) >= IRQ_BASE as usize
            && (vector as usize) < IRQ_BASE as usize + 16
        {
            self.irq_counts[(vector - IRQ_BASE) as usize].load(Ordering::Relaxed)
        } else if vector == 0x80 {
            self.syscall_count.load(Ordering::Relaxed)
        } else if vector == 0x82 {
            self.recovery_count.load(Ordering::Relaxed)
        } else {
            0
        }
    }

    /// 导出为 JSON 格式
    ///
    /// # Returns
    /// JSON 字符串 (需要 alloc crate 支持)
    #[cfg(feature = "json_export")]
    pub fn export_json(&self) -> alloc::string::String {
        use alloc::format;

        format!(
            r#"{{
  "summary": {{
    "total": {},
    "user_mode": {},
    "kernel_mode": {},
    "nested": {},
    "max_nesting": {}
  }},
  "exceptions": {{{}}
  }},
  "irqs": {{{}}
  }},
  "recovery_actions": {{
    "recoveries": {},
    "process_terminations": {},
    "domain_recoveries": {},
    "panics": {}
  }}
}}"#,
            self.total_count.load(Ordering::Relaxed),
            self.user_mode_interrupts.load(Ordering::Relaxed),
            self.kernel_mode_interrupts.load(Ordering::Relaxed),
            self.nested_interrupts.load(Ordering::Relaxed),
            self.max_nesting_depth.load(Ordering::Relaxed),
            // 异常统计
            self.export_exceptions_json(),
            // IRQ 统计
            self.export_irqs_json(),
            // 恢复动作
            self.recoveries.load(Ordering::Relaxed),
            self.process_terminations.load(Ordering::Relaxed),
            self.domain_recoveries.load(Ordering::Relaxed),
            self.panics.load(Ordering::Relaxed),
        )
    }

    #[cfg(feature = "json_export")]
    fn export_exceptions_json(&self) -> alloc::string::String {
        use alloc::{format, string::String};

        let mut json = String::new();
        for i in 0..32u8 {
            if i > 0 {
                json.push_str(",");
            }
            let name = get_exception_name(i);
            let count = self.exception_counts[i as usize].load(Ordering::Relaxed);
            json.push_str(&format!("\n    \"#{} ({})\": {}", i, name, count));
        }
        json
    }

    #[cfg(feature = "json_export")]
    fn export_irqs_json(&self) -> alloc::string::String {
        use alloc::{format, string::String};

        let mut json = String::new();
        for i in 0..16u8 {
            if i > 0 {
                json.push_str(",");
            }
            let name = get_irq_name(i);
            let count = self.irq_counts[i as usize].load(Ordering::Relaxed);
            json.push_str(&format!("\n    \"IRQ {} ({})\": {}", i, name, count));
        }
        json
    }

    /// 获取最近 N 条历史记录
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn get_recent_events(&self, count: usize) -> Vec<InterruptEvent> {
        let history = self.history.lock();
        let count_u64 = count as u64;
        let actual_count = count_u64.min(history.count).min(64) as usize;
        let start_idx = if history.index >= actual_count as u64 {
            (history.index - actual_count as u64) as usize % 64
        } else {
            0
        };

        let mut events = Vec::with_capacity(actual_count);
        for i in 0..actual_count {
            let idx = (start_idx + i) % 64;
            events.push(history.events[idx]);
        }
        events
    }

    /// 重置所有统计计数器 (仅用于测试)
    #[cfg(any(test, feature = "kernel_test"))]
    pub fn reset(&self) {
        self.total_count.store(0, Ordering::Relaxed);

        for i in 0..32u8 {
            self.exception_counts[i as usize].store(0, Ordering::Relaxed);
        }

        for i in 0..16u8 {
            self.irq_counts[i as usize].store(0, Ordering::Relaxed);
        }

        self.syscall_count.store(0, Ordering::Relaxed);
        self.recovery_count.store(0, Ordering::Relaxed);
        self.nested_interrupts.store(0, Ordering::Relaxed);
        self.max_nesting_depth.store(0, Ordering::Relaxed);
        self.user_mode_interrupts.store(0, Ordering::Relaxed);
        self.kernel_mode_interrupts.store(0, Ordering::Relaxed);
        self.recoveries.store(0, Ordering::Relaxed);
        self.process_terminations.store(0, Ordering::Relaxed);
        self.domain_recoveries.store(0, Ordering::Relaxed);
        self.panics.store(0, Ordering::Relaxed);

        let mut history = self.history.lock();
        history.index = 0;
        history.count = 0;
    }
}

// 简单的 Vec 实现 (用于 no_std 环境)
/// 固定容量 (64) 的栈内简易 Vec, 供 no_std 环境存放中断历史事件.
pub struct Vec<T> {
    data: [Option<T>; 64],
    len: usize,
}

impl<T> Vec<T> {
    fn with_capacity(capacity: usize) -> Self {
        let _ = capacity;
        Self {
            data: [const { None }; 64],
            len: 0,
        }
    }

    fn push(&mut self, item: T) {
        if self.len < 64 {
            self.data[self.len] = Some(item);
            self.len += 1;
        }
    }
}

impl<T: Copy> IntoIterator for Vec<T> {
    type Item = T;
    type IntoIter = VecIntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        VecIntoIter {
            vec: self,
            index: 0,
        }
    }
}

/// `Vec` 的按值迭代器
pub struct VecIntoIter<T> {
    vec: Vec<T>,
    index: usize,
}

impl<T: Copy> Iterator for VecIntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.vec.len {
            let item = self.vec.data[self.index];
            self.index += 1;
            item
        } else {
            None
        }
    }
}

/// 全局统计实例 (使用 lazy 初始化避免 const fn 限制)
static DETAILED_STATS: OnceLock<DetailedStatistics> = OnceLock::new();

/// 获取全局详细统计实例
pub fn get_detailed_statistics() -> &'static DetailedStatistics {
    DETAILED_STATS.get_or_init(|slot| {
        // 手动创建 InterruptHistory
        let history = InterruptHistory {
            events: [const {
                InterruptEvent {
                    timestamp: 0,
                    vector: 0,
                    rip: 0,
                    is_user: false,
                }
            }; 64],
            index: 0,
            count: 0,
        };

        slot.write(DetailedStatistics {
            total_count: AtomicU64::new(0),
            exception_counts: [const { AtomicU64::new(0) }; 32],
            irq_counts: [const { AtomicU64::new(0) }; 16],
            syscall_count: AtomicU64::new(0),
            recovery_count: AtomicU64::new(0),
            nested_interrupts: AtomicU64::new(0),
            max_nesting_depth: AtomicU64::new(0),
            user_mode_interrupts: AtomicU64::new(0),
            kernel_mode_interrupts: AtomicU64::new(0),
            recoveries: AtomicU64::new(0),
            process_terminations: AtomicU64::new(0),
            domain_recoveries: AtomicU64::new(0),
            panics: AtomicU64::new(0),
            history: IrqSpinLock::new(history),
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::idt::handlers::PanicInfo;

    #[test]
    fn test_statistics_initialization() {
        let stats = DetailedStatistics::new();
        assert_eq!(stats.total_count.load(Ordering::Relaxed), 0);
        assert_eq!(stats.nested_interrupts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_record_exception() {
        let stats = DetailedStatistics::new();
        let frame = InterruptFrame::new_test_frame(14, 0x400000, 0x23); // PF, user mode

        stats.record_exception(14, &frame);

        assert_eq!(stats.total_count.load(Ordering::Relaxed), 1);
        assert_eq!(stats.get_vector_count(14), 1);
        assert_eq!(stats.user_mode_interrupts.load(Ordering::Relaxed), 1);
        assert_eq!(stats.kernel_mode_interrupts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_record_irq() {
        let stats = DetailedStatistics::new();

        stats.record_irq(1); // Keyboard
        stats.record_irq(0); // Timer
        stats.record_irq(1); // Keyboard again

        assert_eq!(stats.total_count.load(Ordering::Relaxed), 3);
        assert_eq!(stats.irq_counts[1].load(Ordering::Relaxed), 2);
        assert_eq!(stats.irq_counts[0].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_nested_tracking() {
        let stats = DetailedStatistics::new();

        stats.record_nested(1);
        stats.record_nested(2);
        stats.record_nested(3);
        stats.record_nested(2);

        assert_eq!(stats.nested_interrupts.load(Ordering::Relaxed), 4);
        assert_eq!(stats.max_nesting_depth.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_recovery_action_tracking() {
        use super::super::handlers::RecoveryAction;

        let stats = DetailedStatistics::new();

        stats.record_recovery_action(&RecoveryAction::Recovered);
        stats.record_recovery_action(&RecoveryAction::TerminateProcess(42));
        stats.record_recovery_action(&RecoveryAction::DomainRecovery);
        stats.record_recovery_action(&RecoveryAction::Panic(PanicInfo::new("test", 0, 0)));

        assert_eq!(stats.recoveries.load(Ordering::Relaxed), 1);
        assert_eq!(stats.process_terminations.load(Ordering::Relaxed), 1);
        assert_eq!(stats.domain_recoveries.load(Ordering::Relaxed), 1);
        assert_eq!(stats.panics.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_reset() {
        let stats = DetailedStatistics::new();

        stats.record_exception(0, &InterruptFrame::new_test_frame(0, 0x1000, 0x08));
        stats.record_irq(5);
        stats.record_nested(1);

        stats.reset();

        assert_eq!(stats.total_count.load(Ordering::Relaxed), 0);
        assert_eq!(stats.get_vector_count(0), 0);
        assert_eq!(stats.irq_counts[5].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_invalid_vector_count() {
        let stats = DetailedStatistics::new();

        assert_eq!(stats.get_vector_count(255), 0); // Invalid vector
        assert_eq!(stats.get_vector_count(100), 0); // Not exception or IRQ
    }
}
