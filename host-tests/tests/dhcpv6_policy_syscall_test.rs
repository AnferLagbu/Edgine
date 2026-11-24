// SPDX-License-Identifier: MPL-2.0
//! `DHCPv6` 运行时策略配置面 (#298) — syscall 面回归锁 (ABI 单源 / dispatch 接线 /
//! `EgDhcpv6Policy` 布局 / 用户态包装接线).
//!
//! 覆盖:
//! - `edgine-abi::dhcpv6` 编号 816/817 与内核 `functions::syscall::types` 同源;
//! - `EG_DHCPV6_SET_POLICY` / `EG_DHCPV6_GET_POLICY` 在 `FunctionsSyscallDispatch`
//!   中已分发 (不回退 `ENOSYS`);
//! - `EgDhcpv6Policy` repr(C) 布局 (104 字节, 11×u64 + 4×u32, 无隐式填充);
//! - userlib `dhcpv6_set_policy` / `dhcpv6_get_policy` 包装接线 (源码契约).
//!
//! 逐字段校验 (`validate_config`) 的行为锁在 functions 侧
//! `net/dhcpv6/policy.rs::tests` 内联; ABI↔配置往返锁在 `net/syscall.rs::
//! dhcpv6_policy_tests` 内联; set/get 真往返 + enabled 开关 + 非法拒收归 QEMU
//! init 探针 (`[dhcpv6-policy]`).
//!
//! 坏缓冲 → `EFAULT` 分支经 `read_struct_from_user` / `write_struct_to_user`
//! 的 `validate_user_buf` 短路 (空指针先判, 不触 SMAP/CR4), 故 host 腿可覆盖;
//! 合法用户缓冲的真搬运需 ring-0 SMAP 护栏, 归 e2e 真机腿.
//!
//! 与 `dhcpv6_policy_test.rs` (P6b Phase 6a 客户端契约+行为套件) 分立: 本文件
//! 只锁 #298 运行时配置 syscall 面, 不覆盖客户端状态机行为.

use std::fs;

use edgine::kernel::functions::syscall::FunctionsSyscallDispatch;
use edgine::kernel::functions::syscall::types as ktypes;
use edgine::kernel::privileged::syscall::{Errno, SyscallDispatch};
use edgine_abi::dhcpv6 as abi;

const USER_SYS: &str = "../src/user/lib/src/sys.rs";

/// ABI 单源: 旧号 816/817 已平移到 `PRIVATE_BASE`, 内核/ABI 同源.
#[test]
fn abi_numbers_are_single_source() {
    let base = edgine_abi::linux_syscall::PRIVATE_BASE;
    assert_eq!(ktypes::EG_DHCPV6_SET_POLICY, base + 816);
    assert_eq!(ktypes::EG_DHCPV6_GET_POLICY, base + 817);
    assert_eq!(abi::EG_DHCPV6_SET_POLICY, ktypes::EG_DHCPV6_SET_POLICY);
    assert_eq!(abi::EG_DHCPV6_GET_POLICY, ktypes::EG_DHCPV6_GET_POLICY);
}

/// `EgDhcpv6Policy` repr(C) 布局: 104 字节, 11×u64 (0..80) + 4×u32 (88/92/96/100).
#[test]
fn eg_dhcpv6_policy_repr_c_layout() {
    assert_eq!(core::mem::size_of::<abi::EgDhcpv6Policy>(), 104);
    assert_eq!(
        core::mem::offset_of!(abi::EgDhcpv6Policy, sol_timeout_ms),
        0
    );
    assert_eq!(core::mem::offset_of!(abi::EgDhcpv6Policy, sol_max_rt_ms), 8);
    assert_eq!(
        core::mem::offset_of!(abi::EgDhcpv6Policy, rel_timeout_ms),
        80
    );
    assert_eq!(core::mem::offset_of!(abi::EgDhcpv6Policy, max_retries), 88);
    assert_eq!(
        core::mem::offset_of!(abi::EgDhcpv6Policy, renew_t1_ratio),
        92
    );
    assert_eq!(
        core::mem::offset_of!(abi::EgDhcpv6Policy, renew_t2_ratio),
        96
    );
    assert_eq!(core::mem::offset_of!(abi::EgDhcpv6Policy, enabled), 100);
}

/// dispatch 接线: `EG_DHCPV6_SET_POLICY` 已被 functions 接管, 不回退 `ENOSYS`.
/// 空指针 (`policy_ptr=0`) 经 `read_struct_from_user` 短路 → `EFAULT` (不触驱动锁).
#[test]
fn dispatch_set_does_not_fall_back_to_enosys() {
    let ret = FunctionsSyscallDispatch.dispatch(abi::EG_DHCPV6_SET_POLICY, [0u64; 6]);
    assert_ne!(
        ret,
        ktypes::ENOSYS_RET,
        "EG_DHCPV6_SET_POLICY 必须已被 functions dispatch 接管"
    );
    assert_eq!(ret, Errno::Efault.as_ret(), "空指针应 EFAULT");
}

/// dispatch 接线: `EG_DHCPV6_GET_POLICY` 已被 functions 接管, 不回退 `ENOSYS`.
/// 空指针经 `write_struct_to_user` 短路 → `EFAULT` (先读驱动配置再回写).
#[test]
fn dispatch_get_does_not_fall_back_to_enosys() {
    let ret = FunctionsSyscallDispatch.dispatch(abi::EG_DHCPV6_GET_POLICY, [0u64; 6]);
    assert_ne!(
        ret,
        ktypes::ENOSYS_RET,
        "EG_DHCPV6_GET_POLICY 必须已被 functions dispatch 接管"
    );
    assert_eq!(ret, Errno::Efault.as_ret(), "空指针应 EFAULT");
}

/// userlib 包装接线 (源码契约): 两个包装经 `sys1` 发对应编号 (#298).
#[test]
fn contract_userlib_wrapper_wiring() {
    let sys = fs::read_to_string(USER_SYS).unwrap_or_else(|e| panic!("read {USER_SYS} 失败: {e}"));
    assert!(
        sys.contains("pub fn dhcpv6_set_policy(policy: &EgDhcpv6Policy) -> i64"),
        "userlib 应有 dhcpv6_set_policy 包装 (#298)"
    );
    assert!(
        sys.contains("pub fn dhcpv6_get_policy(policy: &mut EgDhcpv6Policy) -> i64"),
        "userlib 应有 dhcpv6_get_policy 包装 (#298)"
    );
    assert!(
        sys.contains("sys1(EG_DHCPV6_SET_POLICY,"),
        "set 包装应经 sys1 发 EG_DHCPV6_SET_POLICY (#298)"
    );
    assert!(
        sys.contains("sys1(EG_DHCPV6_GET_POLICY,"),
        "get 包装应经 sys1 发 EG_DHCPV6_GET_POLICY (#298)"
    );
}
