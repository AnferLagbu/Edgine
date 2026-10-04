#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! NUMA (Non-Uniform Memory Access) — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原策略代码按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/mm/numa.rs` (`NumaMempolicy` 被 privileged proc/process 机制持有、
//! `numa_init` 被 privileged mm 机制调用, 依赖闭包全在 privileged 内)。
//! privileged/mm 的 numa 为公开子模块 (`pub mod numa`), 可直接 glob re-export,
//! 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::mm::numa::*;

use crate::privileged::syscall::Errno;

// ============================================================================
// mbind — 地址范围 NUMA 策略 (T1 G4 实装)
// ============================================================================

/// `mbind` — 设置地址范围的 NUMA 内存策略
///
/// Linux 签名: `mbind(addr, len, mode, nodemask, maxnode, flags)`.
///
/// ## 策略层职责
///
/// - 参数校验: 页对齐 / `mode` 合法性 (`MPOL_*` → `NumaPolicy` 映射) / `flags` 保留位 /
///   `nodemask` 非空 (Bind/Interleave)
/// - 用户内存读取: `nodemask` 首个 u64 字 (`privileged::syscall::api::read_u64_from_user`)
/// - 委托机制: `MmStruct::set_numa_policy_range` 写入 VMA 级策略 (策略落地, 见下 SIMPLIFIED)
///
/// ## 语义
///
/// - `MPOL_DEFAULT`: 清除范围策略, 回退进程级 (`NumaMempolicy`); 要求 `nodemask == NULL`
/// - 非 `MPOL_DEFAULT`: 要求 `nodemask != NULL` 且 `maxnode > 0`; Bind/Interleave 非空位掩码
///
/// SIMPLIFIED: `flags` 仅校验保留位 (`MPOL_MF_VALID` = 0xF), `MPOL_MF_MOVE` /
/// `MPOL_MF_MOVE_ALL` 的"迁移已触达页"能力不实现 (无页迁移机制) — 不报错也不迁移;
/// `maxnode > 64` 仅取低 64 位 (节点空间上限 `MAX_NUMA_NODES` = 8).
/// 影响面: 策略仅写入 VMA 供查询, 已触达页不发生节点迁移. 何时需扩展: PMM 引入
/// per-node 分区 + 页迁移后, 由分配路径消费策略并实现 MOVE 语义.
pub fn mbind_syscall(
    addr: u64,
    len: u64,
    mode: u32,
    nodemask_ptr: u64,
    maxnode: u64,
    flags: u32,
) -> i64 {
    /// `mbind` flags 合法位 (Linux `MPOL_MF_VALID`)
    const MPOL_MF_VALID: u32 = 0xF;

    let page_size = crate::privileged::mm::PAGE_SIZE as u64;

    if len == 0 || !addr.is_multiple_of(page_size) {
        return Errno::EINVAL.as_ret();
    }
    if flags & !MPOL_MF_VALID != 0 {
        return Errno::EINVAL.as_ret();
    }

    let Some(policy_mode) = NumaPolicy::from_linux_mode(mode) else {
        return Errno::EINVAL.as_ret();
    };

    // 结束地址溢出 → ENOMEM (与 Linux 一致)
    let Some(end) = addr.checked_add(len) else {
        return Errno::ENOMEM.as_ret();
    };

    let policy = if policy_mode == NumaPolicy::Default {
        // Linux: MPOL_DEFAULT 要求 nodemask 为 NULL
        if nodemask_ptr != 0 {
            return Errno::EINVAL.as_ret();
        }
        None
    } else {
        if nodemask_ptr == 0 {
            return Errno::EFAULT.as_ret();
        }
        if maxnode == 0 {
            return Errno::EINVAL.as_ret();
        }
        let Some(raw) = crate::privileged::syscall::api::read_u64_from_user(nodemask_ptr) else {
            return Errno::EFAULT.as_ret();
        };
        // maxnode >= 64: 节点空间上限 8, 高位无意义, 取低 64 位
        let mask = if maxnode >= 64 {
            raw
        } else {
            raw & ((1u64 << maxnode) - 1)
        };
        if mask == 0 && matches!(policy_mode, NumaPolicy::Bind | NumaPolicy::Interleave) {
            return Errno::EINVAL.as_ret();
        }
        Some(NumaRangePolicy {
            mode: policy_mode,
            nodemask: mask,
        })
    };

    let Some(mm) = crate::privileged::mm::vma_get_current_mm() else {
        return Errno::EFAULT.as_ret();
    };

    // 64 位目标下 u64 → usize 无损
    match mm.set_numa_policy_range(addr as usize, end as usize, policy) {
        Ok(_) => 0,
        Err(e) => e.as_ret(),
    }
}
