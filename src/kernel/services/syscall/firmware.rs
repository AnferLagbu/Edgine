#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 设备固件加载 系统调用 — services 层实现 (从 framework/syscall/firmware.rs 下沉, §6.2)
//!
//! ## 编号 (730-733: 设备固件)
//!
//! - `QX_FW_LOAD`     (730): 从用户态路径读取文件并附着到设备树节点
//! - `QX_FW_GET_INFO` (731): 拷贝 `FirmwareInfo` 到用户态
//! - `QX_FW_GET`      (732): 按 offset 拷贝固件内容到用户态缓冲
//! - `QX_FW_DETACH`   (733): 移除节点上的固件
//!
//! ## 安全
//!
//! - 用户态指针经 services `check_user_ptr` / `check_user_buf` 校验后, 交由
//!   framework `mm::copy_user` (SMAP + 异常表兜底) 完成实际拷贝, 不假设用户内存对齐
//! - 路径最长 4096 字节, 超过返回 `-EINVAL`
//! - 读取走 framework `fs::vfs::api::{vfs_open_safe, vfs_read_safe}`

use crate::framework::chitin::{
    FW_ERR_IO, FW_ERR_NOT_FOUND, FW_ERR_TOO_LARGE, FirmwareInfo, MAX_FIRMWARE_SIZE,
    devtree_attach_firmware, devtree_detach_firmware, devtree_get_firmware, fnv1a_32,
};
use crate::framework::fs::vfs::api::{vfs_open_safe, vfs_read_safe};
use crate::framework::mm::copy_user::{copy_from_user, copy_to_user};
use alloc::vec::Vec;

const MAX_PATH_LEN: usize = 4096;
const MAX_FW_GET_SIZE: usize = 8 * 1024 * 1024;
const FW_BUF_SIZE: usize = 4096;
const FW_INFO_SIZE: usize = core::mem::size_of::<FirmwareInfo>();

// POSIX errno (与 QX_* 错误语义一致)
const EFAULT: i64 = -14;
const EINVAL: i64 = -22;
const ENOENT: i64 = -2;

/// 从用户态拷贝字节切片 (经 framework `copy_from_user`)
fn copy_user_bytes(ptr: u64, len: usize) -> Option<Vec<u8>> {
    if !super::check_user_ptr(ptr) || len == 0 {
        return None;
    }
    let mut buf = alloc::vec![0u8; len];
    match copy_from_user(&mut buf, ptr, len) {
        Ok(n) if n == len => Some(buf),
        _ => None,
    }
}

/// 向用户态写入字节切片 (经 framework `copy_to_user`)
fn write_user_bytes(ptr: u64, data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if !super::check_user_buf(ptr, data.len() as u64) {
        return false;
    }
    copy_to_user(ptr, data, data.len()).is_ok()
}

/// 将 `FirmwareInfo` 序列化为 native-endian 字节数组
///
/// 不依赖结构体内存对齐, 逐字段 `to_ne_bytes` 拼接, 与用户态 `#[repr(C)]` 布局一致
/// (4 × u32 = 16 字节)。
fn fw_info_bytes(size: u32, name_hash: u32, version: u32, reserved: u32) -> [u8; FW_INFO_SIZE] {
    let mut b = [0u8; FW_INFO_SIZE];
    b[0..4].copy_from_slice(&size.to_ne_bytes());
    b[4..8].copy_from_slice(&name_hash.to_ne_bytes());
    b[8..12].copy_from_slice(&version.to_ne_bytes());
    b[12..16].copy_from_slice(&reserved.to_ne_bytes());
    b
}

/// 打开文件并读取全部内容 (上限 `MAX_FIRMWARE_SIZE`)
fn read_path_data(path: &[u8]) -> Result<Vec<u8>, i64> {
    // 路径必须为 C 字符串 (NUL 结尾); 检查并补齐 NUL
    let mut path_z: Vec<u8> = path.to_vec();
    if path_z.last() != Some(&0) {
        path_z.push(0);
    }
    let Ok(path_str) = core::str::from_utf8(&path_z[..path_z.len() - 1]) else {
        return Err(EINVAL);
    };
    let pwm = 0u64; // 内核侧加载固件使用全权 PWM
    let fd = vfs_open_safe(path_str, 0 /* O_RDONLY */, pwm);
    if fd < 0 {
        return Err(ENOENT);
    }

    let mut out = Vec::new();
    let mut tmp = alloc::vec![0u8; FW_BUF_SIZE];
    loop {
        if out.len() + tmp.len() > MAX_FIRMWARE_SIZE {
            return Err(i64::from(FW_ERR_TOO_LARGE));
        }
        let n = vfs_read_safe(fd as u32, &mut tmp);
        if n < 0 {
            return Err(i64::from(FW_ERR_IO));
        }
        if n == 0 {
            break;
        }
        let n = n as usize;
        out.extend_from_slice(&tmp[..n]);
    }
    Ok(out)
}

/// `sys_fw_load`: 从用户态路径读取并附着固件到 node
///
/// `a0=node_id`, `a1=path_ptr`, `a2=path_len`, `a3=version`
pub fn sys_fw_load(a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let node_id = a0 as u32;
    let path_len = a2 as usize;
    let version = a3 as u32;

    if path_len == 0 || path_len > MAX_PATH_LEN {
        return EINVAL;
    }

    let Some(path_bytes) = copy_user_bytes(a1, path_len) else {
        return EFAULT;
    };

    let data = match read_path_data(&path_bytes) {
        Ok(d) => d,
        Err(e) => return e,
    };

    if data.len() > MAX_FIRMWARE_SIZE {
        return i64::from(FW_ERR_TOO_LARGE);
    }

    // 计算 name hash (使用路径最后一个分量为 "name")
    let Ok(path_str) = core::str::from_utf8(&path_bytes) else {
        return EINVAL;
    };
    let path_str = path_str.trim_end_matches('\0');
    let name = path_str.rsplit('/').next().unwrap_or(path_str);
    let name_hash = fnv1a_32(name);

    if devtree_attach_firmware(node_id, data, name_hash, version) {
        0
    } else {
        i64::from(FW_ERR_NOT_FOUND)
    }
}

/// `sys_fw_get_info`: 将 `FirmwareInfo` 写入用户态 info 指针
///
/// `a0=node_id`, `a1=info_ptr`
pub fn sys_fw_get_info(a0: u64, a1: u64) -> i64 {
    let node_id = a0 as u32;
    if !super::check_user_buf(a1, FW_INFO_SIZE as u64) {
        return EFAULT;
    }
    let Some(blob) = devtree_get_firmware(node_id) else {
        return i64::from(FW_ERR_NOT_FOUND);
    };
    let bytes = fw_info_bytes(blob.size() as u32, blob.name_hash, blob.version, 0);
    if write_user_bytes(a1, &bytes) {
        0
    } else {
        EFAULT
    }
}

/// `sys_fw_get`: 按 offset 拷贝固件到用户态 buf
///
/// `a0=node_id`, `a1=buf_ptr`, `a2=buf_len`, `a3=offset`
///
/// 返回值: 实际拷贝字节数; 失败: 负 errno / `FW_ERR_*`
pub fn sys_fw_get(a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let node_id = a0 as u32;
    let buf_len = a2 as usize;
    let offset = a3 as usize;

    if buf_len > MAX_FW_GET_SIZE {
        return i64::from(FW_ERR_TOO_LARGE);
    }
    if buf_len == 0 {
        return 0;
    }
    if !super::check_user_buf(a1, buf_len as u64) {
        return EFAULT;
    }

    let Some(blob) = devtree_get_firmware(node_id) else {
        return i64::from(FW_ERR_NOT_FOUND);
    };

    if offset >= blob.size() {
        return 0;
    }
    let avail = core::cmp::min(buf_len, blob.size() - offset);
    let slice = &blob.data[offset..offset + avail];

    if write_user_bytes(a1, slice) {
        avail as i64
    } else {
        EFAULT
    }
}

/// `sys_fw_detach`: 移除节点上的固件
///
/// `a0=node_id`
pub fn sys_fw_detach(a0: u64) -> i64 {
    let node_id = a0 as u32;
    if devtree_detach_firmware(node_id) {
        0
    } else {
        i64::from(FW_ERR_NOT_FOUND)
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fw_info_size_matches_struct() {
        // 用户态 ABI 布局: 4 × u32 = 16 字节
        assert_eq!(FW_INFO_SIZE, 16);
    }

    #[test]
    fn fw_info_bytes_layout() {
        let b = fw_info_bytes(0x1122_3344, 0x5566_7788, 0x99AA_BBCC, 0);
        assert_eq!(&b[0..4], &0x1122_3344u32.to_ne_bytes());
        assert_eq!(&b[4..8], &0x5566_7788u32.to_ne_bytes());
        assert_eq!(&b[8..12], &0x99AA_BBCCu32.to_ne_bytes());
        assert_eq!(&b[12..16], &0u32.to_ne_bytes());
    }
}
