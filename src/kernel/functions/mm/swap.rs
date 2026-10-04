#![deny(unsafe_code)]
//! Swap — functions 层安全代理
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `privileged::mm::swap`。
//!
//! ## 职责
//!
//! - 提供类型安全的 Swap API
//! - 参数验证
//! - Swap 信息查询

use crate::privileged::mm::swap as fw_swap;

// ============================================================================
// Swap Entry 安全封装
// ============================================================================

/// Swap entry 安全封装
///
/// 表示一个换出页面的 swap slot 引用.
/// 内部存储 slot 索引, 不暴露 PTE 编码细节.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapSlot(u64);

impl SwapSlot {
    /// 从 slot 索引创建
    pub fn new(slot: u64) -> Self {
        Self(slot)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取 slot 索引
    pub fn index(&self) -> u64 {
        self.0
    }
}

// ============================================================================
// Swap 信息
// ============================================================================

/// Swap 区使用信息
#[derive(Debug, Clone, Copy)]
pub struct SwapInfo {
    /// 总容量 (字节)
    pub total_bytes: u64,
    /// 空闲容量 (字节)
    pub free_bytes: u64,
}

impl SwapInfo {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 已使用容量 (字节)
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes - self.free_bytes
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 使用率 (千分比, 0..=1000)
    pub fn usage_ratio(&self) -> u64 {
        if self.total_bytes == 0 {
            0
        } else {
            self.used_bytes() * 1000 / self.total_bytes
        }
    }
}

// ============================================================================
// Swap 安全 API
// ============================================================================

/// 初始化 swap 子系统
pub fn swap_init() -> bool {
    fw_swap::swap_init()
}

/// 初始化 kswapd: 注册 Kswapd softirq handler
///
/// 必须在 IRQ 子系统初始化后调用 (lib.rs `kernel_init` 6.5 阶段).
pub fn kswapd_init() {
    fw_swap::kswapd_init();
}

/// 唤醒 kswapd: 立即 raise Kswapd softirq
///
/// 由 scheduler.tick 周期调用, 或 pressure 跃迁调用.
pub fn kswapd_wakeup() {
    fw_swap::kswapd_wakeup();
}

/// 检查 kswapd 是否处于 pending (诊断接口)
pub fn kswapd_is_pending() -> bool {
    fw_swap::kswapd_is_pending()
}

/// 回收页面 (从 LRU inactive 链表选取并换出)
///
/// 返回实际回收的页面数.
pub fn reclaim_pages(max_count: u32) -> u32 {
    fw_swap::reclaim_pages(max_count)
}

/// 获取 swap 区使用信息
pub fn swap_info() -> SwapInfo {
    let (total, free) = fw_swap::swap_info();
    SwapInfo {
        total_bytes: total,
        free_bytes: free,
    }
}

/// 记录页面访问 (添加到 LRU active 链表)
///
/// pml4 为该虚拟地址所属进程的 CR3, 用于 swap-out 时写 PTE 为 swap entry.
/// `locked` 表示该页是否被 mlock 锁定, 由调用方根据 VMA `vm_flags.MLOCKED` 推导.
pub fn lru_touch(pml4: u64, virt_addr: u64, phys_addr: u64, dirty: bool, locked: bool) {
    fw_swap::lru_touch(pml4, virt_addr, phys_addr, dirty, locked);
}

/// 标记某虚拟地址对应 LRU 条目为 mlock 锁定
///
/// 返回 true 表示 LRU 中存在该条目 (并已更新), false 表示该页未在 LRU 跟踪
/// (尚未触达, locked 状态由 VMA `vm_flags` 承载).
pub fn set_page_locked(virt_addr: u64, locked: bool) -> bool {
    fw_swap::set_page_locked(virt_addr, locked)
}

/// 查询某虚拟地址对应 LRU 条目的 mlock 锁定状态
pub fn is_page_locked(virt_addr: u64) -> Option<bool> {
    fw_swap::is_page_locked(virt_addr)
}

/// 检测 PTE 是否为 swap entry
pub fn is_swap_pte(pte: u64) -> bool {
    fw_swap::is_swap_pte(pte)
}
