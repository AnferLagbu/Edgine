//! 缺页策略 trait — 策略-机制分离接口
//!
//! 本模块落实「framekernel 范式落实」工程 §6.3: `mm/page_fault.rs`
//! 的用户栈扩展参数/阈值属于策略, 应由 functions 实现, privileged
//! 仅保留 demand paging / COW / swap-in 机制。
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackPageFaultPolicy`), 与历史
//!   硬编码值一致, 保证早期启动阶段行为不变
//! - functions 在 `init()` 中通过 `register_page_fault_policy()` 注册策略
//!
//! ## 调用上下文
//!
//! #PF 中断上下文: 本 trait 方法必须是纯决策逻辑 (无阻塞 / 无分配),
//! 仅返回阈值参数, 实际缺页机制由 privileged 执行。

/// 缺页策略接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait PageFaultPolicy: Send + Sync {
    /// 用户栈顶地址 (等同用户地址空间上限)
    ///
    /// 语义与 `constants::limits::USER_ADDR_MAX` 一致 (栈顶 = 地址空间上限).
    fn stack_top(&self) -> u64;

    /// 用户栈默认大小 (字节)
    ///
    /// 栈顶向下 `stack_default_size()` 区间内的缺页视为栈扩展候选.
    fn stack_default_size(&self) -> u64;

    /// 栈保护页数量
    ///
    /// 位于栈底与可扩展区间之间的不可访问页数 (guard page).
    fn stack_guard_pages(&self) -> u64;
}

// ============================================================================
// 默认回退策略 (functions 尚未注册时使用, 与历史硬编码值一致)
// ============================================================================

/// 框架内建回退策略 — 与 `page_fault.rs` 历史硬编码值一致
///
/// 在 functions 注册策略之前, 缺页路径使用此策略.
pub struct FallbackPageFaultPolicy;

impl PageFaultPolicy for FallbackPageFaultPolicy {
    fn stack_top(&self) -> u64 {
        crate::privileged::constants::limits::USER_ADDR_MAX
    }

    fn stack_default_size(&self) -> u64 {
        0x0080_0000 // 8MB
    }

    fn stack_guard_pages(&self) -> u64 {
        1
    }
}

static FALLBACK_PAGE_FAULT_POLICY: FallbackPageFaultPolicy = FallbackPageFaultPolicy;

/// 全局策略注册表 — functions 通过 `register_page_fault_policy` 注册
static PAGE_FAULT_POLICY: crate::privileged::sync::OnceLock<&'static dyn PageFaultPolicy> =
    crate::privileged::sync::OnceLock::new();

/// 注册缺页策略 (由 `functions::mm::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_page_fault_policy(
    policy: &'static dyn PageFaultPolicy,
) -> Result<(), &'static dyn PageFaultPolicy> {
    match PAGE_FAULT_POLICY.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的缺页策略 (未注册时返回内建回退)
#[inline]
pub fn current_page_fault_policy() -> &'static dyn PageFaultPolicy {
    match PAGE_FAULT_POLICY.get() {
        Some(&p) => p,
        None => &FALLBACK_PAGE_FAULT_POLICY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_values() {
        let p = FallbackPageFaultPolicy;
        assert_eq!(p.stack_guard_pages(), 1);
        assert_eq!(p.stack_default_size(), 0x0080_0000);
        assert_eq!(
            p.stack_top(),
            crate::privileged::constants::limits::USER_ADDR_MAX
        );
    }

    #[test]
    fn test_current_policy_falls_back() {
        // 未注册时返回内建回退, 不 panic
        let p = current_page_fault_policy();
        assert_eq!(p.stack_guard_pages(), 1);
    }
}
