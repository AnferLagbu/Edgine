//! I-50 补充验收: 网络时钟用 hrtimer 而非 tick
//!
//! ## B08-20 处置 (2026-09-06): host 不可测, 平行实现已移除
//!
//! 原镜像对象为 `privileged/net/smoltcp_impl.rs::smoltcp_now` (I-50) 的
//! hrtimer/tick 决策 + ns→ms 截断 + 溢出保护. 评估结论: **内核该函数 host 不可测**,
//! 原因:
//!
//! 1. `smoltcp_now` 为**私有** `fn` (非 pub), host-tests 无法引用;
//! 2. 实现直接调用 `hrtimer_clock_read()` (全局 hrtimer 校准状态) 与
//!    `get_uptime_ms()` (tick 全局计数), host 环境无真实时钟状态可注入;
//! 3. 返回值类型为 `smoltcp::time::Instant`, 依赖 smoltcp 类型语义.
//!
//! 按 B08-20/21 消并规则, 依赖全局时钟状态/私有符号的镜像对象不保留平行实现:
//! 本地 `smoltcp_now` / `Instant` 镜像与全部决策用例已删除 (含
//! `#![allow(dead_code)]`, F9 违规已消除).
//! 该契约的真实覆盖由网络子系统集成测试 (host-tests net_*) 与 QEMU 集成测试承担.
//! 待内核将 `smoltcp_now` 提炼为 pub 纯函数 (参数化 hrtimer_ns/tick_ms) 后可
//! 恢复 host 侧验证 (记录待办).
