#![deny(unsafe_code)]
//! devpts — 伪终端文件系统 (PTY)
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 services::fs::api。
//!
//! ## 职责
//!
//! - 提供伪终端设备 (/dev/pts/0, /dev/pts/1, ...)
//! - 管理 PTY master/slave 对
//!
//! ## 参考
//!
//! - Linux devpts 文档: Documentation/driver-api/serial/tty.rst

use crate::framework::sync::IrqSpinLock as Mutex;
use crate::framework::sync::OnceLock;
use crate::framework::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// 最大 PTY 设备数
pub const MAX_PTY_DEVICES: usize = 256;

/// 设备名称最大长度
pub const MAX_NAME_LEN: usize = 16;

// ============================================================================
// PTY 设备
// ============================================================================

/// PTY 设备状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyState {
    /// 未分配
    Free,
    /// 已分配
    Allocated,
}

/// PTY 设备
#[derive(Debug, Clone, Copy)]
pub struct PtyDevice {
    /// PTY 编号 (0-255)
    pub id: u32,
    /// 设备状态
    pub state: PtyState,
    /// 设备名称
    pub name: [u8; MAX_NAME_LEN],
    /// 名称长度
    pub name_len: u8,
}

impl PtyDevice {
    pub const fn new(id: u32) -> Self {
        Self {
            id,
            state: PtyState::Free,
            name: [0u8; MAX_NAME_LEN],
            name_len: 0,
        }
    }
}

// ============================================================================
// PTY 设备表
// ============================================================================

/// PTY 设备表 (静态分配)
pub struct PtyTable {
    devices: [PtyDevice; MAX_PTY_DEVICES],
    count: u32,
}

impl PtyTable {
    pub const fn new() -> Self {
        Self {
            devices: [const { PtyDevice::new(0) }; MAX_PTY_DEVICES],
            count: 0,
        }
    }

    /// 分配 PTY
    ///
    /// # Errors
    /// 当所有 PTY 设备均已分配时返回 `ENOMEM`.
    pub fn alloc(&mut self) -> Result<u32, Errno> {
        for i in 0..MAX_PTY_DEVICES {
            if self.devices[i].state == PtyState::Free {
                self.devices[i].state = PtyState::Allocated;
                // 设置名称: "pts/N"
                let prefix = b"pts/";
                let mut j = 0;
                for &b in prefix {
                    if j < MAX_NAME_LEN {
                        self.devices[i].name[j] = b;
                        j += 1;
                    }
                }
                // 格式化 ID
                let id = i as u32;
                if id == 0 {
                    if j < MAX_NAME_LEN {
                        self.devices[i].name[j] = b'0';
                        j += 1;
                    }
                } else {
                    let mut buf = [0u8; 8];
                    let mut n = id;
                    let mut k = 8;
                    while n > 0 && k > 0 {
                        k -= 1;
                        buf[k] = b'0' + (n % 10) as u8;
                        n /= 10;
                    }
                    while k < 8 && j < MAX_NAME_LEN {
                        self.devices[i].name[j] = buf[k];
                        j += 1;
                        k += 1;
                    }
                }
                self.devices[i].name_len = j as u8;
                self.count += 1;
                return Ok(i as u32);
            }
        }
        Err(Errno::ENOMEM)
    }

    /// 释放 PTY
    ///
    /// # Errors
    /// 当 `id` 超出范围或对应设备本为未分配状态时返回 `EINVAL`.
    pub fn free(&mut self, id: u32) -> Result<(), Errno> {
        if (id as usize) >= MAX_PTY_DEVICES {
            return Err(Errno::EINVAL);
        }
        if self.devices[id as usize].state == PtyState::Free {
            return Err(Errno::EINVAL);
        }
        self.devices[id as usize].state = PtyState::Free;
        self.count -= 1;
        Ok(())
    }

    /// 获取设备
    pub fn get(&self, id: u32) -> Option<&PtyDevice> {
        if (id as usize) >= MAX_PTY_DEVICES {
            return None;
        }
        if self.devices[id as usize].state == PtyState::Free {
            return None;
        }
        Some(&self.devices[id as usize])
    }

    /// 检查设备是否存在
    pub fn exists(&self, id: u32) -> bool {
        (id as usize) < MAX_PTY_DEVICES && self.devices[id as usize].state != PtyState::Free
    }

    /// 获取设备数量
    pub fn count(&self) -> u32 {
        self.count
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 PTY 表
static PTY_TABLE: OnceLock<Mutex<PtyTable>> = OnceLock::new();

/// 获取全局 PTY 表
pub fn get_pty_table() -> &'static Mutex<PtyTable> {
    PTY_TABLE.get_or_init(|slot| {
        slot.write(Mutex::new(PtyTable::new()));
    })
}

// ============================================================================
// safe API
// ============================================================================

/// 分配 PTY
///
/// # Errors
/// 当所有 PTY 设备均已分配时返回 `ENOMEM`, 与 [`PtyTable::alloc`] 一致.
pub fn alloc_pty() -> Result<u32, Errno> {
    get_pty_table().lock().alloc()
}

/// 释放 PTY
///
/// # Errors
/// 当 `id` 无效或设备未分配时返回 `EINVAL`, 与 [`PtyTable::free`] 一致.
pub fn free_pty(id: u32) -> Result<(), Errno> {
    get_pty_table().lock().free(id)
}

/// 获取 PTY 信息
pub fn get_pty(id: u32) -> Option<PtyDevice> {
    get_pty_table().lock().get(id).copied()
}

/// 检查 PTY 是否存在
pub fn pty_exists(id: u32) -> bool {
    get_pty_table().lock().exists(id)
}

/// 获取 PTY 数量
pub fn pty_count() -> u32 {
    get_pty_table().lock().count()
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 挂载 devpts
///
/// # Errors
/// 当前实现恒返回 `Ok(())`; 仅初始化 PTY 表, 不返回错误.
pub fn mount_devpts() -> Result<(), Errno> {
    // 初始化 PTY 表
    let mut table = get_pty_table().lock();
    for i in 0..MAX_PTY_DEVICES {
        table.devices[i] = PtyDevice::new(i as u32);
    }
    table.count = 0;
    Ok(())
}

/// 卸载 devpts
///
/// # Errors
/// 错误条件与 [`mount_devpts`] 相同; 当前实现恒返回 `Ok(())`.
pub fn umount_devpts() -> Result<(), Errno> {
    mount_devpts()
}
