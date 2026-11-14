// SPDX-License-Identifier: MPL-2.0
//! DNS 双栈 E1 — getaddrinfo syscall 面回归锁 (ABI 单源 / dispatch 接线 /
//! 错误映射 / `EgAddrInfo` 布局 / 用户态包装接线).
//!
//! 覆盖:
//! - `edgine-abi::net` 编号 812 与内核 `functions::syscall::types` 同源;
//! - `EG_GETADDRINFO` 在 `FunctionsSyscallDispatch` 中已分发 (不回退 `ENOSYS`);
//! - `EgAddrInfo` repr(C) 布局 (20 字节, family@0 / addr@4);
//! - 空主机名 → `ENOENT` (网络未就绪短路, `name_len=0` 不触发用户内存搬运);
//! - userlib `getaddrinfo` 包装接线 (源码契约, host 不跑用户态).
//!
//! 双栈映射 (V4 零扩展 / V6 原样) 的行为锁在 privileged 侧
//! `net/syscall.rs::getaddrinfo_tests` 内联; 真网络 AAAA 解析归 QEMU e2e 腿.
//!
//! 非 UTF-8 → `EINVAL` / 坏缓冲 → `EFAULT` 两条错误分支需真实用户内存搬运
//! (`raw_copy_in` / `write_struct_to_user`), 而 `copy_from_user` 的 SMAP 护栏
//! `smap_is_enabled` 读 CR4 (ring-0 特权指令), host x86_64 (ring-3) 必 SIGSEGV,
//! 故无法在 host 腿覆盖; 其映射逻辑由后端源码 + e2e 真机腿保证.

use std::fs;

use edgine::kernel::functions::syscall::FunctionsSyscallDispatch;
use edgine::kernel::functions::syscall::types as ktypes;
use edgine::kernel::privileged::syscall::{Errno, SyscallDispatch};
use edgine_abi::net as abi;

const USER_SYS: &str = "../src/user/lib/src/sys.rs";

/// ABI 单源: 编号 812 内核/ABI 同源, 地址族常量与 Linux `AF_INET`/`AF_INET6` 一致.
#[test]
fn abi_numbers_are_single_source() {
    assert_eq!(ktypes::EG_GETADDRINFO, 812);
    assert_eq!(abi::EG_GETADDRINFO, ktypes::EG_GETADDRINFO);
    assert_eq!(abi::EG_AF_INET, 2);
    assert_eq!(abi::EG_AF_INET6, 10);
}

/// `EgAddrInfo` repr(C) 布局: 20 字节, family@0 (u32) / addr@4 ([u8; 16]).
#[test]
fn eg_addr_info_repr_c_layout() {
    assert_eq!(core::mem::size_of::<abi::EgAddrInfo>(), 20);
    assert_eq!(core::mem::offset_of!(abi::EgAddrInfo, family), 0);
    assert_eq!(core::mem::offset_of!(abi::EgAddrInfo, addr), 4);
}

/// dispatch 接线: `EG_GETADDRINFO` 已被 functions 接管, 不回退 `ENOSYS`.
/// 空主机名 (`name_len=0`) 经 `dns_resolve` 网络未就绪短路 → `ENOENT`.
#[test]
fn dispatch_does_not_fall_back_to_enosys() {
    let ret = FunctionsSyscallDispatch.dispatch(abi::EG_GETADDRINFO, [0u64; 6]);
    assert_ne!(
        ret,
        ktypes::ENOSYS_RET,
        "EG_GETADDRINFO 必须已被 functions dispatch 接管"
    );
    assert_eq!(
        ret,
        Errno::Enoent.as_ret(),
        "空主机名 + 网络未就绪应 ENOENT"
    );
}

/// userlib 包装接线 (源码契约): `getaddrinfo` 经 `sys3` 发 `EG_GETADDRINFO`.
#[test]
fn contract_userlib_wrapper_wiring() {
    let sys = fs::read_to_string(USER_SYS).unwrap_or_else(|e| panic!("read {USER_SYS} 失败: {e}"));
    assert!(
        sys.contains("pub fn getaddrinfo(name: &[u8], out: &mut EgAddrInfo) -> i64"),
        "userlib 应有 getaddrinfo 包装 (E1-c)"
    );
    assert!(
        sys.contains("EG_GETADDRINFO,"),
        "包装应经 sys3 发 EG_GETADDRINFO (E1-c)"
    );
}
