#![deny(unsafe_code)]
//! 缺页策略 — functions 层权威实现
//!
//! 落实 `docs/plan/framekernel-paradigm-enforcement.md` §6.3: 用户栈扩展的
//! 参数与阈值属策略, demand paging / COW / swap-in 属机制留在 privileged.
//!
//! ## 分层位置
//!
//! - trait [`crate::privileged::mm::PageFaultPolicy`] 定义在 privileged (引用 privileged 类型);
//! - 本模块实现 trait 并在 [`super::init`] 注册;
//! - privileged 保留 `FallbackPageFaultPolicy` 仅作 functions 初始化前的启动兜底.
//!
//! ## 调用上下文
//!
//! 本策略在 #PF 中断上下文被查询: 只返回阈值参数, 无阻塞、无分配.
//! 地址空间上界经 `privileged::mm::user_addr_max` 安全访问器取得, 策略层不重复
//! 架构常量.

use crate::privileged::mm::{PageFaultPolicy, user_addr_max};

/// 用户栈默认大小 (字节)
///
/// 栈顶向下该窗口内的缺页视为栈扩展候选 (与 privileged 历史硬编码值一致).
const STACK_DEFAULT_SIZE: u64 = 8 << 20;

/// 栈底保护页数量
const STACK_GUARD_PAGES: u64 = 1;

/// 用户栈扩展策略 — functions 层权威实现
pub struct StackGrowthPolicy;

impl PageFaultPolicy for StackGrowthPolicy {
    fn stack_top(&self) -> u64 {
        user_addr_max()
    }

    fn stack_default_size(&self) -> u64 {
        STACK_DEFAULT_SIZE
    }

    fn stack_guard_pages(&self) -> u64 {
        STACK_GUARD_PAGES
    }
}

/// 进程内单例 — 策略注册表要求 `&'static dyn PageFaultPolicy`
pub static STACK_GROWTH_POLICY: StackGrowthPolicy = StackGrowthPolicy;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_matches_architecture_limits() {
        let p = StackGrowthPolicy;
        assert_eq!(p.stack_top(), user_addr_max());
        assert_eq!(p.stack_default_size(), 8 << 20);
        assert_eq!(p.stack_guard_pages(), 1);
    }

    #[test]
    fn test_policy_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StackGrowthPolicy>();
    }
}
