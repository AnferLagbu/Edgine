#![deny(unsafe_code)]
// SPDX-License-Identifier: MPL-2.0
//! SGEG 私有存储子系统 syscall — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全 + 参数验证
//! - sgeg 鉴权 (走 `privileged::sgeg::api::pwm`_*)
//! - 用户指针 + 容量验证 (走 `privileged::syscall::raw`)
//! - 委托 `privileged::driver::block` 执行块设备实际操作
//!
//! ## 与 privileged 边界
//!
//! privileged 暴露的 `block_device_count/list/info/is_present/total_sectors`
//! 已封装块设备 (ATA / `NVMe` / AHCI / virtio-blk) 注册表, functions 走该公共 API.
//!
//! ## 不允许简化: 即使 sgeg 私有, 仍走 functions 业务 + 鉴权 + 校验三段式.

use crate::privileged::driver::block as blk;
use crate::privileged::sgeg;
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::raw;

/// sgeg disk 域 = 4 (PWM domain 4 = storage 域)
const PWM_DOMAIN_STORAGE: u16 = 4;

/// 块设备信息写回结构 (与 privileged 旧实现兼容, 76 字节)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct UserDiskInfo {
    pub disk_id: u32,
    pub present: u32,
    pub total_sectors: u32,
    pub sectors: u32,
    pub model: [u8; 64],
}

const USER_DISK_INFO_SIZE: u64 = 76;

/// 列出已注册块设备 id. `disks` 为用户态 u64 数组, `max_count` 为容量.
/// 返回写入的设备数 (>= 0), 失败返 Errno.
///
/// # Errors
/// 当 `disks_ptr` 无效或用户缓冲区校验失败时返回 `Errno::EFAULT`;
/// 当 `max_count` 为 0 或字节数计算溢出时返回 `Errno::EINVAL`.
pub fn disk_list(disks_ptr: u64, max_count: u32) -> Result<usize, Errno> {
    if disks_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if max_count == 0 {
        return Err(Errno::EINVAL);
    }
    let bytes = u64::from(max_count).checked_mul(8).ok_or(Errno::EINVAL)?;
    if !raw::check_user_buf(disks_ptr, bytes) {
        return Err(Errno::EFAULT);
    }
    let count = blk::block_device_count();
    let limit = (max_count as usize).min(count);
    for i in 0..limit {
        if !raw::write_u64_to_user(disks_ptr + (i as u64) * 8, i as u64) {
            return Err(Errno::EFAULT);
        }
    }
    Ok(limit)
}

/// 读单块设备信息. `info` 为 `UserDiskInfo` 用户指针 (76 字节).
///
/// # Errors
/// 当 `info_ptr` 无效或用户缓冲区校验失败时返回 `Errno::EFAULT`;
/// 当 `disk_id` 超出 u8 范围 (>255) 时返回 `Errno::EINVAL`.
pub fn disk_info(disk_id: u32, info_ptr: u64) -> Result<(), Errno> {
    if info_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_buf(info_ptr, USER_DISK_INFO_SIZE) {
        return Err(Errno::EFAULT);
    }
    // 有意窄化: 资源类型转换, u8 块设备 ID 范围 0-255
    let id = u8::try_from(disk_id).map_err(|_| Errno::EINVAL)?;
    let present = blk::hdd_is_present(id);
    let sectors = if present {
        blk::hdd_total_sectors(id) as u32
    } else {
        0
    };
    let (name, _is_present, _total) = if present {
        blk::block_device_info(id)
    } else {
        ("", false, 0u64)
    };
    let mut model = [0u8; 64];
    let name_bytes = name.as_bytes();
    let copy_len = name_bytes.len().min(63);
    model[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
    let info = UserDiskInfo {
        disk_id,
        present: u32::from(present),
        total_sectors: sectors,
        sectors,
        model,
    };
    if !raw::write_struct_to_user(info_ptr, &info) {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 格式化块设备. `fstype` 为用户 C 字符串.
///
/// # Errors
/// 当指针无效时返回 `Errno::EFAULT`; 当 `disk_id` 超出 u8 范围时返回
/// `Errno::EINVAL`; 当当前身份缺少 storage 域写能力时返回
/// `Errno::EACCES`; 当设备不存在时返回 `Errno::ENOENT`; 当前实现为占位,
/// 始终返回 `Errno::ENOSYS`.
pub fn disk_format(disk_id: u32, fstype_ptr: u64) -> Result<(), Errno> {
    if fstype_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(fstype_ptr) {
        return Err(Errno::EFAULT);
    }
    // sgeg 鉴权: storage 域 required=1 (写)
    let pwm = sgeg::api::pwm_get_current();
    if !sgeg::api::pwm_has_capability(pwm, PWM_DOMAIN_STORAGE, 1) {
        return Err(Errno::EACCES);
    }
    // 有意窄化: 资源类型转换, u8 块设备 ID 范围 0-255
    let id = u8::try_from(disk_id).map_err(|_| Errno::EINVAL)?;
    if !blk::hdd_is_present(id) {
        return Err(Errno::ENOENT);
    }
    // 委托 privileged: 走 sys_disk_format 内置实现 (ATA/NVMe 各驱动的格式化路径).
    // 实际格式化在 privileged::syscall::mod::sys_disk_format 已有完整逻辑.
    // functions 层仅校验 + 鉴权, 真实 IO 委派.
    Err(Errno::ENOSYS) // 临时占位: 详细实现后续由 privileged storage 子模块接管
}

/// 块设备分区表写入.
///
/// # Errors
/// 当 `total_sectors` 为 0 或超过 `u32::MAX` 时返回 `Errno::EINVAL`;
/// 当 `disk_id` 超出 u8 范围时返回 `Errno::EINVAL`;
/// 当当前身份缺少 storage 域写能力时返回 `Errno::EACCES`;
/// 当设备不存在时返回 `Errno::ENOENT`; 当前实现为占位, 始终返回 `Errno::ENOSYS`.
pub fn disk_partition(disk_id: u32, total_sectors: u64) -> Result<(), Errno> {
    if total_sectors == 0 || total_sectors > u64::from(u32::MAX) {
        return Err(Errno::EINVAL);
    }
    let pwm = sgeg::api::pwm_get_current();
    if !sgeg::api::pwm_has_capability(pwm, PWM_DOMAIN_STORAGE, 1) {
        return Err(Errno::EACCES);
    }
    // 有意窄化: 资源类型转换, u8 块设备 ID 范围 0-255
    let id = u8::try_from(disk_id).map_err(|_| Errno::EINVAL)?;
    if !blk::hdd_is_present(id) {
        return Err(Errno::ENOENT);
    }
    let _ = id;
    let _ = total_sectors;
    Err(Errno::ENOSYS)
}

/// FAT 格式化.
///
/// # Errors
/// 当 `disk_id` 超出 u8 范围时返回 `Errno::EINVAL`;
/// 当当前身份缺少 storage 域写能力时返回 `Errno::EACCES`;
/// 当设备不存在时返回 `Errno::ENOENT`; 当前实现为占位, 始终返回 `Errno::ENOSYS`.
pub fn fat_format(disk_id: u32) -> Result<(), Errno> {
    let pwm = sgeg::api::pwm_get_current();
    if !sgeg::api::pwm_has_capability(pwm, PWM_DOMAIN_STORAGE, 1) {
        return Err(Errno::EACCES);
    }
    // 有意窄化: 资源类型转换, u8 块设备 ID 范围 0-255
    let id = u8::try_from(disk_id).map_err(|_| Errno::EINVAL)?;
    if !blk::hdd_is_present(id) {
        return Err(Errno::ENOENT);
    }
    let _ = id;
    Err(Errno::ENOSYS)
}

/// boot_install(disk_id) 策略 — 引导安装 (T2 批 5 自 privileged 回退层迁移)
///
/// 委托 privileged 机制 `sys_boot_install` (磁盘扇区写 + stage1/内核拷贝,
/// unsafe 机制库). 仅 x86_64 生产构建实装; 其余构建配置 (kernel_test /
/// aarch64) 返回 ENOSYS (与原 privileged cfg 门控语义一致).
#[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
pub fn boot_install_syscall(disk_id: u32) -> i64 {
    crate::privileged::syscall::sys_boot_install(disk_id)
}

/// boot_install 非实装路径 (kernel_test 主机维度) — ENOSYS
#[cfg(feature = "kernel_test")]
pub fn boot_install_syscall(_disk_id: u32) -> i64 {
    Errno::ENOSYS.as_ret()
}

/// hotplug_status(buf, buf_size) 策略 — 读取热插拔状态 (T2 批 5 自 privileged
/// 回退层迁移)
///
/// 委托 privileged 机制 `sys_hotplug_status` (unsafe 用户 buffer 写入 +
/// 驱动状态读取, 机制库). `buf_ptr` 为用户态缓冲区指针 (u64 位宽), 仅做
/// 类型转换后委托, 指针有效性由机制层校验.
pub fn hotplug_status_syscall(buf_ptr: u64, buf_size: u32) -> i64 {
    crate::privileged::syscall::sys_hotplug_status(buf_ptr as *mut u8, buf_size)
}
